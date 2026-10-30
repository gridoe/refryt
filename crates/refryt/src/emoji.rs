//! Colour emoji, drawn as images.
//!
//! egui cannot draw a colour emoji. epaint rasterises glyph *outlines* through skrifa and reads
//! neither `COLR` nor the `CBDT`/`sbix` bitmap strikes that every colour emoji font is made of, so
//! a colour face in the font stack renders nothing at all and the bundled monochrome Noto Emoji is
//! the ceiling of what text painting can do. Playlist names are full of emoji and they came out
//! looking like text.
//!
//! So the emoji are painted as images instead: find a colour emoji font that stores PNG strikes,
//! pull the PNG for that glyph, and blit it where the glyph would have gone. The *advance* still
//! comes from the monochrome face, so spacing and truncation are unchanged — only the pixels
//! differ. On Linux and macOS that font is the system's own; on Windows it is Noto Color Emoji,
//! which the packaging ships next to the exe (`font_path`), because Segoe UI Emoji is COLR and
//! has no PNG strike to read.
//!
//! What this deliberately does not do is shape. A ZWJ sequence (👨‍👩‍👧) needs `GSUB` ligature
//! lookups to reach its single glyph; here its components are drawn side by side, which is wrong
//! but legible. Single code points — which is every emoji in a playlist name so far — are exact.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, FontId, Pos2, Rect, Ui, pos2, vec2};

/// How large the cached image is, in pixels. Emoji are mostly drawn at 13–15pt; 48px covers that on a 2×
/// display with room to spare, and costs 9 KB a glyph instead of the 70 KB the font's own 136×128
/// strike would.
const CACHE_PX: u32 = 48;

/// Emoji are drawn slightly larger than the nominal font size, as fonts draw them.
const SIZE_FACTOR: f32 = 1.15;

/// Inter's capitals, cap line to baseline, as a fraction of the size: 0.35 above the line's
/// centre and 0.364 below it.
const CAP_SPAN: f32 = 0.714;

/// How much taller than the capitals an emoji may be, split over both ends. `SIZE_FACTOR`
/// alone grows the overhang with the size: a 56pt title's emoji rose 13pt past its capitals,
/// into the type line above it.
const CAP_OVERHANG: f32 = 6.0;

/// The side of an emoji image drawn at `font_size` — what the text measurer sizes an emoji
/// written with VARIATION SELECTOR-16 to, so the two agree. Up to about 14pt that is
/// `SIZE_FACTOR`; above it the capitals plus a fixed overhang.
pub(crate) fn emoji_side(font_size: f32) -> f32 {
    (font_size * SIZE_FACTOR).min(font_size * CAP_SPAN + CAP_OVERHANG)
}

/// VARIATION SELECTOR-16: "draw the previous character as an emoji". It carries no glyph of its
/// own and is consumed with the character it follows.
const VS16: char = '\u{FE0F}';
/// VARIATION SELECTOR-15, the same in reverse: "draw it as text".
const VS15: char = '\u{FE0E}';

/// A piece of a line: either text for egui to lay out, or one emoji to blit.
#[derive(Debug, PartialEq, Eq)]
enum Run<'a> {
    Text(&'a str),
    /// The character to draw, and the full source slice it stands for (base plus any selector or
    /// skin-tone modifier), which is what the monochrome face would have measured.
    Emoji {
        ch: char,
        source: &'a str,
    },
}

/// Whether `c` should be drawn as a picture rather than as a letter.
///
/// Two cases, and the distinction matters: above U+1F000 everything is emoji by default, while
/// below it a character is only emoji when a VARIATION SELECTOR-16 asks for it. Without that
/// second condition an arrow or a bullet in an ordinary title would silently become a picture.
fn is_emoji(c: char, next: Option<char>) -> bool {
    let cp = c as u32;
    if cp >= 0x1F000 {
        return true;
    }
    cp >= 0x2000 && next == Some(VS16)
}

/// The cheap test that lets a line skip run splitting entirely. Everything [`is_emoji`] accepts
/// is above this, so a line with no character above it cannot contain an emoji.
fn may_be_emoji(c: char) -> bool {
    c as u32 >= 0x2000
}

/// Skin-tone modifiers follow their base character and have no meaning alone — swallowed with it,
/// since we cannot apply the tone and drawing the swatch beside the face would be worse.
fn is_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}' | VS16 | VS15)
}

/// Split a line into text and emoji runs.
fn runs(text: &str) -> Vec<Run<'_>> {
    let mut out = Vec::new();
    let mut text_start = 0;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if !is_emoji(c, it.peek().map(|(_, c)| *c)) {
            continue;
        }
        if text_start < i {
            out.push(Run::Text(&text[text_start..i]));
        }
        let mut end = i + c.len_utf8();
        while let Some((j, m)) = it.peek().copied() {
            if !is_modifier(m) {
                break;
            }
            end = j + m.len_utf8();
            it.next();
        }
        out.push(Run::Emoji {
            ch: c,
            source: &text[i..end],
        });
        text_start = end;
    }
    if text_start < text.len() {
        out.push(Run::Text(&text[text_start..]));
    }
    out
}

/// Paint one line of text, left-aligned and vertically centred on `pos`, with colour emoji.
///
/// A drop-in replacement for `painter.text(pos, Align2::LEFT_CENTER, …)`. Returns the width
/// painted. Where no colour font is installed, or a character is not in it, the run falls through
/// to ordinary text painting — which still draws the bundled monochrome emoji, so the line is
/// never worse than it was.
pub fn paint_line(ui: &Ui, pos: Pos2, text: &str, font: FontId, color: Color32) -> f32 {
    paint_line_with(ui.painter(), ui, pos, text, &font, color)
}

/// [`paint_line`], clipped to `clip` — for marquee viewports, where the line is wider than
/// the room it may occupy. The advance is still measured unclipped, so truncation and hit
/// rects cannot disagree with paint.
pub fn paint_clipped_line(
    ui: &Ui,
    clip: Rect,
    pos: Pos2,
    text: &str,
    font: FontId,
    color: Color32,
) -> f32 {
    paint_line_with(
        &ui.painter().with_clip_rect(clip),
        ui,
        pos,
        text,
        &font,
        color,
    )
}

fn paint_line_with(
    painter: &egui::Painter,
    ui: &Ui,
    pos: Pos2,
    text: &str,
    font: &FontId,
    color: Color32,
) -> f32 {
    // Almost every line is a title with no emoji in it, and this is the render path: taking the
    // ordinary route without splitting keeps the per-frame allocation at zero for those.
    if !text.chars().any(may_be_emoji) {
        return paint_text_with(painter, ui, pos, text, font, color);
    }

    let mut x = pos.x;
    for run in runs(text) {
        match run {
            Run::Text(s) => {
                x += paint_text_with(painter, ui, pos2(x, pos.y), s, font, color);
            }
            Run::Emoji { ch, source } => {
                // The advance is the monochrome face's, so a line is the same width whether or not
                // a colour font was found — truncation and hit rects cannot disagree with paint.
                let advance = crate::widgets::text_width(ui, source, font);
                match texture(ui.ctx(), ch) {
                    Some(tex) => {
                        let side = emoji_side(font.size);
                        let rect = Rect::from_center_size(
                            pos2(x + advance / 2.0, pos.y),
                            vec2(side, side),
                        );
                        painter.image(
                            tex.id(),
                            rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                        x += advance;
                    }
                    None => x += paint_text_with(painter, ui, pos2(x, pos.y), source, font, color),
                }
            }
        }
    }
    x - pos.x
}

fn paint_text_with(
    painter: &egui::Painter,
    ui: &Ui,
    pos: Pos2,
    s: &str,
    font: &FontId,
    color: Color32,
) -> f32 {
    painter.text(pos, Align2::LEFT_CENTER, s, font.clone(), color);
    crate::widgets::text_width(ui, s, font)
}

// ---------------------------------------------------------------------------------------------
// The font, and the texture cache
// ---------------------------------------------------------------------------------------------

/// Cached per `egui::Context` rather than threaded through every widget.
///
/// Emoji are painted from `nav_item`, `art_card` and `track_row`, none of which is given the
/// app's image store, and adding a parameter to each for a lookup that is a hash hit after the
/// first frame would be churn for its own sake. `Context::data` is egui's own place for exactly
/// this, and the handle is cheap to clone.
#[derive(Clone, Default)]
struct Cache(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// `None` until looked for, then the mapped font or `None` if there is not one.
    font: Option<Option<Arc<memmap2::Mmap>>>,
    /// Set the moment a background lookup starts, so a second caller on the same frame (or the
    /// next one, before it lands) does not spawn a second one.
    font_loading: bool,
    glyphs: HashMap<char, Option<egui::TextureHandle>>,
}

fn texture(ctx: &egui::Context, ch: char) -> Option<egui::TextureHandle> {
    let cache = ctx.data_mut(|d| {
        d.get_temp_mut_or_insert_with(egui::Id::new("ytm-emoji"), Cache::default)
            .clone()
    });
    let mut inner = cache.0.lock().ok()?;
    if let Some(hit) = inner.glyphs.get(&ch) {
        return hit.clone();
    }

    let font = match &inner.font {
        Some(font) => font.clone(),
        None => {
            // `load_font` can fall through to shelling out to `fc-match` as a last resort — a
            // slow or stuck fontconfig (a stale cache it decides to rebuild, disk contention,
            // anything) must never be able to freeze the whole window with no way to recover,
            // which is exactly what calling it inline here used to risk. Looked up once, off
            // this thread; every frame before it lands just draws the monochrome glyph, same as
            // when there turns out to be no colour font at all.
            if !inner.font_loading {
                inner.font_loading = true;
                let cache = cache.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let found = load_font();
                    if found.is_none() {
                        tracing::info!("no colour emoji font found; emoji will be monochrome");
                    }
                    if let Ok(mut inner) = cache.0.lock() {
                        inner.font = Some(found);
                    }
                    ctx.request_repaint();
                });
            }
            // Not cached below: once the background lookup lands, the next frame tries again
            // for real instead of being stuck with whatever this one frame's guess was.
            return None;
        }
    };

    let handle = font
        .as_ref()
        .and_then(|map| png_for(map, ch))
        .and_then(|png| decode(&png[..]))
        .map(|image| {
            ctx.load_texture(
                format!("emoji-{:x}", ch as u32),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
    inner.glyphs.insert(ch, handle.clone());
    handle
}

/// The PNG the font stores for `ch`, if it has one.
fn png_for(map: &memmap2::Mmap, ch: char) -> Option<Vec<u8>> {
    use skrifa::{MetadataProvider as _, prelude::Size};

    // macOS's `Apple Color Emoji.ttc` is a collection (a single face inside); `FontRef::new`
    // only accepts a bare font, so go through `FileRef` and take the first face either way.
    let font = match skrifa::raw::FileRef::new(map).ok()? {
        skrifa::raw::FileRef::Font(font) => font,
        skrifa::raw::FileRef::Collection(collection) => collection.get(0).ok()?,
    };
    let glyph = font.charmap().map(ch)?;
    let strikes = skrifa::bitmap::BitmapStrikes::new(&font);
    let bitmap = strikes.glyph_for_size(Size::unscaled(), glyph)?;
    match bitmap.data {
        skrifa::bitmap::BitmapData::Png(bytes) => Some(bytes.to_vec()),
        // Only PNG strikes are read. `Bgra` and `Mask` exist in the format but no shipping colour
        // emoji font uses them, and guessing at their layout untested would be worse than falling
        // back to the monochrome glyph.
        _ => None,
    }
}

/// Decode and shrink one emoji bitmap.
fn decode(png: &[u8]) -> Option<egui::ColorImage> {
    let image = image::load_from_memory(png).ok()?;
    let image = image.thumbnail(CACHE_PX, CACHE_PX).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        image.as_raw(),
    ))
}

/// Find the system's colour emoji font.
///
/// Mapped rather than read: these fonts are around 10 MB, which is a quarter of this app's whole
/// idle memory figure, and none of it needs to be resident — a map is file-backed and only the
/// pages holding the emoji actually drawn are ever touched.
fn load_font() -> Option<Arc<memmap2::Mmap>> {
    let path = font_path()?;
    let file = std::fs::File::open(&path).ok()?;
    // SAFETY: a font file replaced underneath a running process would be seen as changed bytes.
    // Everything read from it is bounds-checked by skrifa and a bad parse yields `None`, so the
    // worst case is an emoji that fails to draw.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    tracing::debug!(path = %path.display(), "colour emoji font");
    Some(Arc::new(map))
}

fn font_path() -> Option<PathBuf> {
    // Windows' own colour font, Segoe UI Emoji, is COLR — nothing for `png_for` to read — so the
    // app carries one that has PNG strikes: Noto Color Emoji, next to the exe in a packaged
    // build. `CARGO_MANIFEST_DIR` is the repository's copy for a dev run; it is a compile-time
    // path that only resolves on the machine that produced the binary, and the shipped copy is
    // found first anyway.
    #[cfg(target_os = "windows")]
    {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let repository =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/NotoColorEmoji.ttf");
        exe_dir
            .into_iter()
            .map(|dir| dir.join("NotoColorEmoji.ttf"))
            .chain(std::iter::once(repository))
            .find(|path| path.is_file())
    }

    #[cfg(not(target_os = "windows"))]
    {
        const CANDIDATES: &[&str] = &[
            // Arch, Fedora
            "/usr/share/fonts/noto/NotoColorEmoji.ttf",
            "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
            // Debian, Ubuntu
            "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
            "/usr/share/fonts/truetype/noto-color-emoji/NotoColorEmoji.ttf",
            "/usr/share/fonts/NotoColorEmoji.ttf",
            // macOS ships an `sbix` font, which skrifa reads the same way.
            "/System/Library/Fonts/Apple Color Emoji.ttc",
        ];
        for path in CANDIDATES {
            if Path::new(path).is_file() {
                return Some(PathBuf::from(path));
            }
        }
        if let Some(home) = directories::BaseDirs::new() {
            let local = home
                .home_dir()
                .join(".local/share/fonts/NotoColorEmoji.ttf");
            if local.is_file() {
                return Some(local);
            }
        }
        // Distributions move fonts around; fontconfig is the one component that always knows
        // where they went. Only reached when every well-known path missed.
        fontconfig_path()
    }
}

#[cfg(not(target_os = "windows"))]
fn fontconfig_path() -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "Noto Color Emoji"])
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_owned());
    // fc-match always answers with *something*; only a genuinely colour font is any use here.
    (path.is_file() && path.file_name()?.to_string_lossy().contains("Emoji")).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text-sized emoji keep the font's usual size; a header title's stay within 3pt of its
    /// capitals at either end, so they clear the type line above the name.
    #[test]
    fn a_title_emoji_stays_near_its_capitals() {
        assert_eq!(emoji_side(13.0), 13.0 * SIZE_FACTOR);
        for size in [26.0, 40.0, 56.0] {
            let overhang = (emoji_side(size) - size * CAP_SPAN) / 2.0;
            assert!(overhang <= 3.0 + f32::EPSILON, "{size}: {overhang}");
        }
    }

    #[test]
    fn a_plain_title_is_one_text_run() {
        assert_eq!(runs("actually good rap playlist").len(), 1);
        assert_eq!(runs("").len(), 0);
    }

    /// The real playlist names this exists for.
    #[test]
    fn emoji_are_split_out_of_the_line() {
        assert_eq!(
            runs("Χάουζ 🫧"),
            vec![
                Run::Text("Χάουζ "),
                Run::Emoji {
                    ch: '🫧',
                    source: "🫧"
                }
            ]
        );
        assert_eq!(
            runs("Playlist 🗿"),
            vec![
                Run::Text("Playlist "),
                Run::Emoji {
                    ch: '🗿',
                    source: "🗿"
                }
            ]
        );
    }

    /// A variation selector belongs to the emoji before it, not to the text after it — otherwise
    /// it is painted on its own and, since no face covers it, becomes a replacement box.
    #[test]
    fn a_variation_selector_stays_with_its_emoji() {
        assert_eq!(
            runs("🕶️"),
            vec![Run::Emoji {
                ch: '🕶',
                source: "🕶\u{FE0F}"
            }]
        );
        assert_eq!(runs("↖️↗️").len(), 2);
    }

    /// The fast path skips run splitting on any line with no character above U+2000. If a single
    /// code point could be an emoji without passing that test, such a line would silently lose its
    /// picture — so the implication is checked over every code point there is.
    #[test]
    fn the_fast_path_cannot_skip_an_emoji() {
        for cp in 0..=0x10FFFF_u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            for next in [None, Some(VS16)] {
                assert!(!is_emoji(c, next) || may_be_emoji(c), "U+{cp:04X}");
            }
        }
    }

    /// An arrow that has not asked to be an emoji is text, and must not turn into a picture.
    #[test]
    fn a_bare_symbol_is_left_as_text() {
        assert_eq!(runs("a → b"), vec![Run::Text("a → b")]);
        assert_eq!(runs("2 • 3"), vec![Run::Text("2 • 3")]);
        assert_eq!(runs("#1 hits"), vec![Run::Text("#1 hits")]);
    }

    /// A skin tone cannot be applied without shaping, and drawn on its own it is a colour swatch
    /// beside the face. It goes with its base character.
    #[test]
    fn a_skin_tone_goes_with_its_base() {
        assert_eq!(
            runs("👋🏽"),
            vec![Run::Emoji {
                ch: '👋',
                source: "👋🏽"
            }]
        );
    }

    /// Whatever this machine has, it must be a colour emoji font and it must yield a PNG for a
    /// modern emoji — the whole point being that the bundled monochrome face has no 🫧.
    #[test]
    fn the_system_font_yields_a_picture() {
        let Some(map) = load_font() else {
            eprintln!("no colour emoji font on this machine; skipping");
            return;
        };
        let png = png_for(&map, '🫧').expect("a PNG for 🫧");
        assert_eq!(&png[1..4], b"PNG");
        let image = decode(&png).expect("decodes");
        assert!(image.size[0] <= CACHE_PX as usize && image.size[1] <= CACHE_PX as usize);
        assert!(image.size[0] > 8);
    }
}
