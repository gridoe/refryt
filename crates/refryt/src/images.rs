//! Cover art: fetch off-thread, decode, upload as a texture, remember a bounded number.
//!
//! The UI thread only ever does a hashmap lookup. Everything slow — the HTTP request, the JPEG
//! decode, the dominant-colour pass — happens on a tokio worker, and the finished pixels arrive
//! as an event. Texture upload is the one step that must happen on the UI thread, because it
//! needs the egui context.
//!
//! Two design points are load-bearing and were both wrong before:
//!
//! * **The key is `(url, px)`, not `url`.** Keying on the URL alone means the first caller wins:
//!   an album page's 96pt header would claim a URL and every 40pt track row below it would then
//!   draw that oversized texture, or the reverse and the header would look soft.
//! * **The budget is bytes, not entries.** A 320px card cover is 400 KB and a 96px row cover is
//!   36 KB; capping at "200 entries" meant the cache was worth either 7 MB or 80 MB depending on
//!   what you happened to browse, against a whole-app memory gate of 85 MB.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureOptions};

/// How much decoded cover art to keep resident. Sized so a heavily browsed session stays well
/// inside the app's memory gates; `--stats` reports the live figure.
const MAX_TEXTURE_BYTES: usize = 24 * 1024 * 1024;

/// Backstop for entries that cost no texture memory (failures), so they cannot accumulate.
const MAX_ENTRIES: usize = 600;

/// Ceiling on concurrent fetches, so flinging through a list does not open hundreds of sockets.
const MAX_INFLIGHT: usize = 8;

/// Dominant colours are three bytes each and independent of the size the art was fetched at, so
/// they live in their own map and are pruned against the slots.
const MAX_DOMINANTS: usize = 256;

/// A cover at one particular size and aspect. Two sizes of the same URL are two entries,
/// deliberately — and so are the square and 16:9 variants, which the server crops differently.
type Key = (Arc<str>, u32, bool);

/// The pixels and colour a worker produces. Everything here is cheap to send across a channel.
///
/// `ColorImage` holds the whole bitmap, so `Debug` prints the key and colour only.
pub struct Loaded {
    pub url: Arc<str>,
    pub px: u32,
    /// Whether this was fetched through [`Images::texture_wide`], so `insert` can address the
    /// same slot the lookup used.
    pub wide: bool,
    pub image: ColorImage,
    pub dominant: [u8; 3],
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("url", &self.url)
            .field("px", &self.px)
            .field("wide", &self.wide)
            .field("size", &self.image.size)
            .field("dominant", &self.dominant)
            .finish()
    }
}

enum State {
    Loading,
    Ready(TextureHandle),
    /// Remembered so a broken URL is not retried on every single frame.
    Failed,
}

struct Slot {
    state: State,
    /// Texture bytes this slot holds, 0 while loading or failed.
    bytes: usize,
    /// Logical clock value of the last lookup, for LRU eviction.
    used: u64,
}

#[derive(Default)]
pub struct Images {
    slots: HashMap<Key, Slot>,
    dominants: HashMap<Arc<str>, [u8; 3]>,
    tick: u64,
    inflight: usize,
    bytes: usize,
}

impl Images {
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries held, at any size. Reported by the `--stats` overlay.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Paired with `len` so `clippy::len_without_is_empty` stays quiet.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Texture bytes currently resident.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn inflight(&self) -> usize {
        self.inflight
    }

    /// Look up a cover, returning `None` while it is still loading or if it failed.
    ///
    /// Takes `&Arc<str>` rather than `&str` on purpose: the model already stores thumbnails as
    /// `Arc<str>`, so building the lookup key is an atomic increment. Taking `&str` forced a heap
    /// allocation and copy on *every row, every frame*.
    ///
    /// `want` receives the URL to fetch when one is needed; the caller wires that to the backend.
    /// Doing it this way keeps `Images` free of any dependency on the backend or tokio, which is
    /// what makes it testable.
    pub fn texture(
        &mut self,
        url: &Arc<str>,
        px: u32,
        mut want: impl FnMut(Arc<str>, String, u32, bool),
    ) -> Option<&TextureHandle> {
        self.texture_inner(url, px, false, sized_url, &mut want)
    }

    /// The 16:9 variant: `px` is the *width*, and YouTube's image server returns the crop —
    /// a video still that arrives square is cropped server-side rather than squashed client-side.
    pub fn texture_wide(
        &mut self,
        url: &Arc<str>,
        px: u32,
        mut want: impl FnMut(Arc<str>, String, u32, bool),
    ) -> Option<&TextureHandle> {
        self.texture_inner(url, px, true, sized_url_wide, &mut want)
    }

    /// The artist hero variant: `px` is the *width* and the server crops to [`HERO_ASPECT`], so
    /// the page gets a band shaped like the slot it draws rather than a square it would have to
    /// cover-crop (and, at hero sizes, upscale) client-side. The `wide` key bit keeps it distinct
    /// from the square texture the same URL produces as a circle avatar.
    pub fn texture_hero(
        &mut self,
        url: &Arc<str>,
        px: u32,
        mut want: impl FnMut(Arc<str>, String, u32, bool),
    ) -> Option<&TextureHandle> {
        self.texture_inner(url, px, true, sized_url_hero, &mut want)
    }

    /// The now-playing view's blurred backdrop for a cover. Fetched small, as there is no detail
    /// left to keep, and its own cache entry through [`BACKDROP_PX`].
    pub fn texture_backdrop(
        &mut self,
        url: &Arc<str>,
        mut want: impl FnMut(Arc<str>, String, u32, bool),
    ) -> Option<&TextureHandle> {
        self.texture_inner(url, BACKDROP_PX, false, sized_backdrop_url, &mut want)
    }

    fn texture_inner(
        &mut self,
        url: &Arc<str>,
        px: u32,
        wide: bool,
        // Called only on a miss: rewriting the URL allocates, and a hit is every visible cover
        // on every frame.
        sized: fn(&str, u32) -> String,
        want: &mut impl FnMut(Arc<str>, String, u32, bool),
    ) -> Option<&TextureHandle> {
        self.tick += 1;
        let key: Key = (Arc::clone(url), px, wide);

        match self.slots.get_mut(&key) {
            Some(slot) => {
                slot.used = self.tick;
            }
            None => {
                if self.inflight >= MAX_INFLIGHT {
                    // Try again on a later frame; the row is still visible so it will ask again.
                    return None;
                }
                self.inflight += 1;
                self.slots.insert(
                    key.clone(),
                    Slot {
                        state: State::Loading,
                        bytes: 0,
                        used: self.tick,
                    },
                );
                want(Arc::clone(url), sized(url, px), px, wide);
            }
        }

        match &self.slots.get(&key)?.state {
            State::Ready(texture) => Some(texture),
            _ => None,
        }
    }

    /// The dominant colour of an already-loaded cover, for the page header wash.
    /// Size-independent.
    pub fn dominant(&self, url: &str) -> Option<[u8; 3]> {
        self.dominants.get(url).copied()
    }

    /// Upload finished pixels. Must run on the UI thread — `load_texture` needs the context.
    pub fn insert(&mut self, ctx: &egui::Context, loaded: Loaded) {
        self.inflight = self.inflight.saturating_sub(1);
        let [w, h] = loaded.image.size;
        let bytes = w * h * 4;
        let texture = ctx.load_texture(
            format!("cover-{}-{}-{}", loaded.px, loaded.wide, loaded.url),
            loaded.image,
            TextureOptions::LINEAR,
        );
        self.dominants
            .insert(Arc::clone(&loaded.url), loaded.dominant);

        let key: Key = (loaded.url, loaded.px, loaded.wide);
        let used = self.tick;
        if let Some(old) = self.slots.insert(
            key,
            Slot {
                state: State::Ready(texture),
                bytes,
                used,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        self.evict();
    }

    pub fn mark_failed(&mut self, url: Arc<str>, px: u32, wide: bool) {
        self.inflight = self.inflight.saturating_sub(1);
        let used = self.tick;
        if let Some(old) = self.slots.insert(
            (url, px, wide),
            Slot {
                state: State::Failed,
                bytes: 0,
                used,
            },
        ) {
            self.bytes -= old.bytes;
        }
    }

    /// Drop the least recently asked-for entries until the cache is back inside both budgets.
    fn evict(&mut self) {
        while self.bytes > MAX_TEXTURE_BYTES || self.slots.len() > MAX_ENTRIES {
            let Some(oldest) = self
                .slots
                .iter()
                .min_by_key(|(_, slot)| slot.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(slot) = self.slots.remove(&oldest) {
                self.bytes -= slot.bytes;
            }
        }

        if self.dominants.len() > MAX_DOMINANTS {
            // Keep only colours we still hold art for; the rest can be recomputed on refetch.
            let live: std::collections::HashSet<Arc<str>> = self
                .slots
                .keys()
                .map(|(url, _, _)| Arc::clone(url))
                .collect();
            self.dominants.retain(|url, _| live.contains(url));
        }
    }
}

/// Ask YouTube for the size we are actually going to draw.
///
/// Thumbnail URLs end in a size suffix like `=w60-h60-l90-rj`. Left alone we would upscale a
/// 60px image into a 40px slot on a HiDPI screen and it would look soft. Rewriting `w` and `h`
/// costs nothing and returns a correctly sized image.
pub fn sized_url(url: &str, px: u32) -> String {
    let Some(eq) = url.rfind('=') else {
        return url.to_owned();
    };
    let (base, params) = url.split_at(eq);
    let rewritten: Vec<String> = params[1..]
        .split('-')
        .map(|part| {
            let (prefix, rest) = part.split_at(1.min(part.len()));
            match prefix {
                "w" if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
                    format!("w{px}")
                }
                "h" if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
                    format!("h{px}")
                }
                _ => part.to_owned(),
            }
        })
        .collect();
    format!("{base}={}", rewritten.join("-"))
}

/// The artist hero's height per unit width. YouTube's channel art is ~2.4:1; the artist page
/// draws slightly shorter than that (see `ui::artist`'s `HERO_MIN_H`/`HERO_MAX_H`), and the
/// server crop this ratio asks for is the one the page usually shows, so nothing is fetched that
/// is never drawn. Both the fetch and the layout read this one number, which is what keeps the
/// cached texture keyed by width alone honest.
pub const HERO_ASPECT: f32 = 0.36;

/// Ask YouTube for a crop of the given width at the hero's aspect.
pub fn sized_url_hero(url: &str, w: u32) -> String {
    sized_url_aspect(url, w, HERO_ASPECT)
}

/// Ask YouTube for a 16:9 crop at `w` wide. Same suffix rewrite as [`sized_url`], but the
/// height follows the width — the server crops, so a square-arriving still is never squashed
/// into a wide slot client-side.
pub fn sized_url_wide(url: &str, w: u32) -> String {
    sized_url_aspect(url, w, 9.0 / 16.0)
}

/// Rewrite the `w`/`h` size suffix to the given width and aspect. The one place the two
/// wide-shaped variants differ.
fn sized_url_aspect(url: &str, w: u32, aspect: f32) -> String {
    let h = (w as f32 * aspect).round().max(1.0) as u32;
    let Some(eq) = url.rfind('=') else {
        return url.to_owned();
    };
    let (base, params) = url.split_at(eq);
    let rewritten: Vec<String> = params[1..]
        .split('-')
        .map(|part| {
            let (prefix, rest) = part.split_at(1.min(part.len()));
            match prefix {
                "w" if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
                    format!("w{w}")
                }
                "h" if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() => {
                    format!("h{h}")
                }
                _ => part.to_owned(),
            }
        })
        .collect();
    format!("{base}={}", rewritten.join("-"))
}

/// The cache's size key for a backdrop. Cover sizes are multiples of 32, so this one is never
/// a real cover's and the two never share an entry.
pub const BACKDROP_PX: u32 = 97;
/// The side a backdrop is blurred and kept at. Stretched across a screen it stays smooth, as the
/// blur leaves nothing finer than a few of its pixels.
const BACKDROP_SIDE: u32 = 96;
/// The blur's sigma, in the backdrop's own pixels.
const BACKDROP_BLUR: f32 = 11.0;
/// How far the colours are pushed from grey: blurring averages them toward mud.
const BACKDROP_SATURATION: f32 = 1.3;
/// The brightness every backdrop is brought to on average, 0 to 255, so a nearly black sleeve
/// still gives a colour and a nearly white one doesn't blind the words on it.
const BACKDROP_LEVEL: f32 = 105.0;
/// The most a dark cover is lifted. A black sleeve stays dark rather than turning its noise up.
const BACKDROP_MAX_GAIN: f32 = 5.0;
/// The brightest any part of a backdrop may be, 0 to 255, for a cover with a hot spot.
const BACKDROP_PEAK: f32 = 160.0;

fn sized_backdrop_url(url: &str, _px: u32) -> String {
    sized_url(url, 128)
}

/// Decode a cover into the now-playing view's backdrop: small, heavily blurred, a little more
/// saturated and brought to a common brightness. Runs on a worker thread. The dominant colour is read before the blur, so it is the
/// same colour the cover's other sizes report.
pub fn decode_backdrop(bytes: &[u8]) -> anyhow::Result<(ColorImage, [u8; 3])> {
    let rgba = image::load_from_memory(bytes)?
        .resize_to_fill(
            BACKDROP_SIDE,
            BACKDROP_SIDE,
            image::imageops::FilterType::Triangle,
        )
        .to_rgba8();
    let dominant = dominant_from_pixels(
        rgba.as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, _]| [r, g, b]),
    );
    let mut blurred = image::imageops::blur(&rgba, BACKDROP_BLUR);
    let count = blurred.pixels().len().max(1) as f32;
    let mean = blurred.pixels().map(|p| luma(p.0)).sum::<f32>() / count;
    let gain = (BACKDROP_LEVEL / mean.max(1.0)).min(BACKDROP_MAX_GAIN);
    for pixel in blurred.pixels_mut() {
        pixel.0 = backdrop_pixel(pixel.0, gain);
    }
    let image = ColorImage::from_rgba_unmultiplied(
        [blurred.width() as usize, blurred.height() as usize],
        blurred.as_raw(),
    );
    Ok((image, dominant))
}

/// A pixel's brightness, 0 to 255.
fn luma([r, g, b, _]: [u8; 4]) -> f32 {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
}

/// One blurred pixel brought to the backdrop's level and saturation, and held under its peak.
fn backdrop_pixel(pixel: [u8; 4], gain: f32) -> [u8; 4] {
    let grey = luma(pixel) * gain;
    let channel = |v: u8| grey + (f32::from(v) * gain - grey) * BACKDROP_SATURATION;
    let [r, g, b] = [channel(pixel[0]), channel(pixel[1]), channel(pixel[2])];
    let hold = (BACKDROP_PEAK / grey.max(1.0)).min(1.0);
    let out = |v: f32| (v * hold).round().clamp(0.0, 255.0) as u8;
    [out(r), out(g), out(b), 255]
}

/// Decode image bytes into an egui image plus a dominant colour, both sized for our use.
///
/// Runs on a worker thread. The returned `ColorImage` fits inside a `px` box with its aspect
/// kept — a square cover lands square exactly as before, and a server-cropped 16:9 still
/// stays 16:9 instead of being forced square. The dominant-colour sample is taken from
/// that same scaled copy rather than resampling the source a second time.
pub fn decode(bytes: &[u8], px: u32) -> anyhow::Result<(ColorImage, [u8; 3])> {
    let decoded = image::load_from_memory(bytes)?;
    let scaled = decoded.resize(px, px, image::imageops::FilterType::Lanczos3);

    // One RGBA copy, not two: the dominant colour is read straight from the buffer we upload,
    // dropping the alpha byte inline, instead of also materialising a whole `RgbImage`.
    let rgba = scaled.to_rgba8();
    let dominant = dominant_from_pixels(
        rgba.as_raw()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[r, g, b, _]| [r, g, b]),
    );
    let image = ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    );
    Ok((image, dominant))
}

/// Pick a representative colour from RGB bytes.
///
/// A plain average of an album cover trends towards mud, so this ignores near-black, near-white
/// and washed-out pixels, buckets what is left into a coarse 4×4×4 histogram, and averages the
/// heaviest bucket. That reliably finds the colour a person would name when looking at the
/// sleeve. `ui::header_wash` then blends it into the page's own background.
pub fn dominant_colour(rgb: &[u8]) -> [u8; 3] {
    dominant_from_pixels(rgb.as_chunks::<3>().0.iter().copied())
}

/// The histogram pass itself, over any iterator of RGB triples — so `decode` can feed it the
/// RGBA upload buffer directly (alpha dropped) without allocating a packed RGB copy.
fn dominant_from_pixels(pixels: impl Iterator<Item = [u8; 3]>) -> [u8; 3] {
    let mut buckets = [[0u64; 4]; 64]; // [count, r_sum, g_sum, b_sum]
    let mut fallback = [0u64; 4];

    for [r, g, b] in pixels {
        let (r, g, b) = (r as u64, g as u64, b as u64);
        fallback[0] += 1;
        fallback[1] += r;
        fallback[2] += g;
        fallback[3] += b;

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        // Skip pixels too dark, too bright, or too grey to say anything about the artwork.
        if max < 30 || min > 225 || max - min < 24 {
            continue;
        }
        let idx = ((r / 64) * 16 + (g / 64) * 4 + (b / 64)) as usize;
        buckets[idx][0] += 1;
        buckets[idx][1] += r;
        buckets[idx][2] += g;
        buckets[idx][3] += b;
    }

    let best = buckets
        .iter()
        .max_by_key(|b| b[0])
        .copied()
        .unwrap_or(fallback);
    let chosen = if best[0] > 0 { best } else { fallback };
    if chosen[0] == 0 {
        return [128, 128, 128];
    }
    [
        (chosen[1] / chosen[0]) as u8,
        (chosen[2] / chosen[0]) as u8,
        (chosen[3] / chosen[0]) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backdrop is the blurred size whatever comes in, opaque, and has lost the cover's edges:
    /// the two halves of a sharp cover meet in a blend.
    #[test]
    fn a_backdrop_is_small_and_blurred() {
        let (w, h) = (300u32, 200u32);
        let cover = image::RgbaImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                image::Rgba([220, 40, 40, 255])
            } else {
                image::Rgba([40, 40, 220, 255])
            }
        });
        let mut bytes = std::io::Cursor::new(Vec::new());
        cover
            .write_to(&mut bytes, image::ImageFormat::Png)
            .expect("encode");
        let (image, _) = decode_backdrop(bytes.get_ref()).expect("decode");
        assert_eq!(image.size, [96, 96]);
        let middle = image.pixels[48 * 96 + 48];
        assert_eq!(middle.a(), 255);
        assert!(middle.r() > 60 && middle.b() > 60, "{middle:?}");
    }

    /// Dark and bright covers come out in the same range: a dark one lifted, keeping its colour,
    /// and a white one held down.
    #[test]
    fn backdrops_are_brought_to_a_common_brightness() {
        let lifted = backdrop_pixel([40, 8, 8, 255], 4.0);
        assert!(lifted[0] > 120 && lifted[0] > lifted[1] * 2, "{lifted:?}");
        let white = backdrop_pixel([255, 255, 255, 255], 1.0);
        assert!(luma(white) <= BACKDROP_PEAK + 1.0, "{white:?}");
    }

    #[test]
    fn rewrites_the_youtube_size_suffix() {
        assert_eq!(
            sized_url("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj", 120),
            "https://lh3.googleusercontent.com/abc=w120-h120-l90-rj"
        );
        assert_eq!(
            sized_url(
                "https://lh3.googleusercontent.com/x=w544-h544-l90-rj-flcro",
                60
            ),
            "https://lh3.googleusercontent.com/x=w60-h60-l90-rj-flcro"
        );
    }

    #[test]
    fn leaves_urls_without_a_size_suffix_alone() {
        let plain = "https://example.com/cover.jpg";
        assert_eq!(sized_url(plain, 120), plain);
    }

    #[test]
    fn wide_urls_get_a_16x9_crop() {
        assert_eq!(
            sized_url_wide("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj", 256),
            "https://lh3.googleusercontent.com/abc=w256-h144-l90-rj"
        );
    }

    #[test]
    fn wide_urls_without_a_size_suffix_are_left_alone() {
        let plain = "https://example.com/still.jpg";
        assert_eq!(sized_url_wide(plain, 256), plain);
    }

    /// The hero asks for a band, not a 16:9 still: 1600 wide comes back 576 tall (36%).
    #[test]
    fn hero_urls_get_the_hero_band() {
        assert_eq!(
            sized_url_hero("https://yt3.googleusercontent.com/abc=w60-h60-l90-rj", 1600),
            "https://yt3.googleusercontent.com/abc=w1600-h576-l90-rj"
        );
        assert_eq!(
            sized_url_hero("https://example.com/banner.jpg", 1600),
            "https://example.com/banner.jpg"
        );
    }

    /// The squash this exists to prevent: a 16:9 still forced through the square path used
    /// to arrive distorted in the texture, not just in the slot.
    #[test]
    fn decode_keeps_a_wide_source_wide() {
        use std::io::Cursor;
        let wide = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            320,
            180,
            image::Rgb([200, 30, 40]),
        ));
        let mut bytes = Vec::new();
        wide.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encodes");
        let (image, _) = decode(&bytes, 256).expect("decodes");
        assert_eq!(image.size, [256, 144]);
    }

    #[test]
    fn decode_leaves_a_square_source_square() {
        use std::io::Cursor;
        let square = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            544,
            544,
            image::Rgb([30, 60, 200]),
        ));
        let mut bytes = Vec::new();
        square
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .expect("encodes");
        let (image, _) = decode(&bytes, 256).expect("decodes");
        assert_eq!(image.size, [256, 256]);
    }

    #[test]
    fn does_not_mangle_other_parameters() {
        // `l90` and `rj` must survive; only w/h are ours to change.
        let out = sized_url("https://x/y=w60-h60-l90-rj", 256);
        assert!(out.ends_with("-l90-rj"), "{out}");
    }

    /// The bug this cache key exists to prevent: a 40pt row and a 96pt header sharing a URL must
    /// each get their own art, not whichever one asked first.
    #[test]
    fn the_same_url_at_two_sizes_is_two_requests() {
        let mut images = Images::new();
        let url: Arc<str> = Arc::from("https://x/y=w60-h60-l90-rj");
        let mut asked: Vec<(String, u32)> = Vec::new();

        images.texture(&url, 64, |_, sized, px, _| asked.push((sized, px)));
        images.texture(&url, 256, |_, sized, px, _| asked.push((sized, px)));
        // Asking again for a size already in flight must not re-request it.
        images.texture(&url, 64, |_, sized, px, _| asked.push((sized, px)));

        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0].1, 64);
        assert_eq!(asked[1].1, 256);
        assert!(asked[0].0.contains("w64-h64"), "{}", asked[0].0);
        assert!(asked[1].0.contains("w256-h256"), "{}", asked[1].0);
    }

    /// The square and 16:9 variants of one URL are different server crops, so they must not
    /// share a slot even at the same `px` — the same rows and the player bar can both ask.
    #[test]
    fn the_same_url_square_and_wide_is_two_requests() {
        let mut images = Images::new();
        let url: Arc<str> = Arc::from("https://x/y=w60-h60-l90-rj");
        let mut asked: Vec<(bool, String)> = Vec::new();

        images.texture(&url, 64, |_, sized, _, wide| asked.push((wide, sized)));
        images.texture_wide(&url, 64, |_, sized, _, wide| asked.push((wide, sized)));

        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked.iter().any(|(wide, _)| !*wide), "square missing");
        assert!(asked.iter().any(|(wide, _)| *wide), "wide missing");
        assert!(
            asked.iter().any(|(_, sized)| sized.contains("w64-h64")),
            "no square crop: {asked:?}"
        );
        assert!(
            asked.iter().any(|(_, sized)| sized.contains("w64-h36")),
            "no 16:9 crop: {asked:?}"
        );
    }

    #[test]
    fn concurrent_fetches_are_capped() {
        let mut images = Images::new();
        let mut asked = 0;
        for i in 0..40 {
            let url: Arc<str> = Arc::from(format!("https://x/{i}=w60-h60").as_str());
            images.texture(&url, 64, |_, _, _, _| asked += 1);
        }
        assert_eq!(asked, MAX_INFLIGHT);
    }

    #[test]
    fn a_failure_is_remembered_rather_than_retried_every_frame() {
        let mut images = Images::new();
        let url: Arc<str> = Arc::from("https://x/broken=w60-h60");
        let mut asked = 0;
        images.texture(&url, 64, |_, _, _, _| asked += 1);
        images.mark_failed(Arc::clone(&url), 64, false);
        assert_eq!(images.inflight(), 0);
        for _ in 0..10 {
            images.texture(&url, 64, |_, _, _, _| asked += 1);
        }
        assert_eq!(asked, 1);
    }

    fn solid(r: u8, g: u8, b: u8, n: usize) -> Vec<u8> {
        [r, g, b].repeat(n)
    }

    #[test]
    fn finds_the_saturated_colour_not_the_background() {
        // A cover that is mostly near-black with a strong red block.
        let mut px = solid(4, 4, 4, 900);
        px.extend(solid(220, 20, 30, 100));
        let [r, g, b] = dominant_colour(&px);
        assert!(r > 150 && g < 90 && b < 90, "got {r},{g},{b}");
    }

    #[test]
    fn ignores_greys_when_a_real_colour_exists() {
        let mut px = solid(130, 130, 130, 800);
        px.extend(solid(30, 60, 200, 200));
        let [r, g, b] = dominant_colour(&px);
        assert!(
            b > r && b > g,
            "expected a blue-ish result, got {r},{g},{b}"
        );
    }

    #[test]
    fn a_fully_grey_cover_still_returns_something_usable() {
        let px = solid(128, 128, 128, 500);
        let [r, g, b] = dominant_colour(&px);
        assert!(
            (120..=136).contains(&r) && r == g && g == b,
            "got {r},{g},{b}"
        );
    }

    #[test]
    fn empty_input_does_not_panic() {
        assert_eq!(dominant_colour(&[]), [128, 128, 128]);
    }
}
