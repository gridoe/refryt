//! Audio playback through libmpv.
//!
//! mpv runs the whole network and decode path: ranged chunked reads, reconnection, seeking inside
//! a remote stream, Opus decode and gapless transitions. We only hand it URLs and read back state.
//!
//! State is *polled* rather than delivered through mpv's event context. We already repaint a few
//! times a second while playing, so a handful of property reads on those frames is cheaper and far
//! simpler than running an event thread and a second channel. The same frames drain mpv's event
//! queue without waiting, for one thing only: its own error messages, which go to the log.

use std::sync::Arc;

use anyhow::{Context, Result};
use libmpv2::Mpv;

use crate::model::Track;

/// A snapshot of what mpv is doing, read once per frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerState {
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// True when mpv has nothing loaded — used to advance the queue.
    pub idle: bool,
}

pub struct Player {
    /// Built on the first `handle()`, not at startup. An idle mpv context still maps libav*
    /// and runs its own playloop/demuxer/ao threads (~10–15 MB); a session that only browses
    /// never needs any of it. `None` also means "nothing has ever been loaded", which is why
    /// `state()` reports idle in that case — the same as a fresh eager context did.
    mpv: Option<Arc<Mpv>>,
    volume: f64,
    /// Set if the last libmpv init attempt failed. Surfaced exactly where an eager failure was —
    /// `App::player_error` and the sidebar's "audio unavailable" note — just at first play
    /// instead of at startup. Not latched: a later play tries again, so a transient failure
    /// (an OOM, a locale another library moved) does not kill playback for the whole session.
    init_error: Option<String>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Player {
    /// One line for the exit log: destroying the mpv core waits for its playloop, and a stream
    /// stuck on the network is the one thing that can make that wait long. If a quit drags and
    /// the log stops here, this is the drop to bound.
    fn drop(&mut self) {
        tracing::info!("player: dropping");
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            mpv: None,
            volume: 70.0,
            init_error: None,
        }
    }

    fn init() -> Result<Mpv> {
        // GTK — which the tray brings up on its own thread (`tray.rs`) — calls
        // `setlocale(LC_ALL, "")` when it initialises, moving `LC_NUMERIC` off the "C" locale.
        // libmpv refuses to create a context unless it is "C": `mpv_create` returns NULL, mpv
        // prints "Non-C locale detected" to stderr, and playback can never start. The tray comes
        // up during startup, before any play, so this is the `setlocale(LC_NUMERIC, "C")` mpv
        // asks its embedders for, put back before the context is created.
        //
        // macOS makes the same call. mpv has the same requirement there, and the call is harmless
        // when the locale is already "C", so it is made unconditionally rather than depending on
        // which framework last touched the locale.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        restore_c_locale();
        let mpv = Mpv::with_initializer(|init| {
            // Audio only: never spend bandwidth or decode time on video.
            init.set_property("vid", "no")?;
            init.set_property("audio-display", "no")?;
            // Gapless needs the next file appended before the current one ends.
            init.set_property("gapless-audio", "yes")?;
            // Let mpv buffer ahead. It reads in ranged chunks by default, which YouTube
            // requires — a whole-file request is refused outright, and 1MiB ranges are too.
            init.set_property("cache", "yes")?;
            init.set_property("demuxer-max-bytes", "32MiB")?;
            // Already-played audio mpv keeps around for a backward seek. The default is
            // ~47 MiB held for the life of the file; a music player seeks back rarely and a
            // miss just re-reads from the network, which the forward cache already does past
            // its own edge. 4 MiB is a few minutes of Opus — plenty for a scrub-back.
            init.set_property("demuxer-max-back-bytes", "4MiB")?;
            // Bound connection establishment only; body reads keep their normal tolerance
            // for buffered proxy streams. Older mpv builds without curl lack this option.
            let _ = init.set_property("curl-connect-timeout", "5");
            // We resolve stream URLs ourselves via refryt-resolve, so mpv must not invoke
            // yt-dlp for them — direct `googlevideo.com` URLs never match ytdl_hook's
            // `youtube.com` patterns, so enabling the hook costs nothing there. The in-app
            // music video *does* need it, and mpv reads `ytdl` and the hook's own path only
            // here, at init: setting them later is silently ignored, which is exactly why the
            // video view never started until the hook was armed once, up front.
            init.set_property("ytdl", "yes")?;
            init.set_property(
                "script-opts",
                format!("ytdl_hook-ytdl_path={}", crate::backend::yt_dlp_program()),
            )?;
            init.set_property("terminal", "no")?;
            // mpv's PipeWire output asks for the graph's own step, 1024 samples (21 ms), and is
            // woken for every one: about 47 times a second per thread for as long as a song
            // plays, and a window the sound server cannot make bigger on its own. Asking for
            // 45 ms halves those wakeups (118 to 70 a second for the whole process, measured) and
            // costs 24 ms of latency, which a pause, a seek or the volume slider never shows.
            // The step is ours to bound: a bigger ask (250) is granted in full on a stock
            // PipeWire and would make those controls lag. mpv built without PipeWire, or
            // playing through PulseAudio, has no such option, and the failure is the answer.
            #[cfg(target_os = "linux")]
            let _ = init.set_property("pipewire-buffer", "45");
            // Diagnostic hatch for the video path, whose failures mpv reports only in its own
            // log: point `REFRYT_MPV_LOG` at a file to capture it.
            if let Ok(path) = std::env::var("REFRYT_MPV_LOG") {
                let _ = init.set_property("log-file", path);
                let _ = init.set_property("msg-level", "all=v");
            }
            // A test aid for debug builds: `REFRYT_MPV_OPTS="pipewire-buffer=200,audio-buffer=0.4"`
            // sets those mpv options, to try an audio setting without a rebuild.
            #[cfg(debug_assertions)]
            for pair in std::env::var("REFRYT_MPV_OPTS")
                .unwrap_or_default()
                .split(',')
                .filter(|p| !p.is_empty())
            {
                if let Some((name, value)) = pair.split_once('=') {
                    let set = init.set_property(name, value);
                    tracing::info!(name, value, ok = set.is_ok(), "REFRYT_MPV_OPTS");
                }
            }
            Ok(())
        })
        .context("could not initialise libmpv")?;
        // A stream that loads and then goes idle without a sound leaves nothing in our own log;
        // mpv's error-level messages say why. libmpv2 does not wrap this call.
        // SAFETY: `ctx` is the live handle `mpv` owns, and the level is a static C string.
        unsafe {
            libmpv2_sys::mpv_request_log_messages(mpv.ctx.as_ptr(), c"error".as_ptr());
        }
        Ok(mpv)
    }

    /// A handle safe to move into a background task, initialising libmpv on the first call.
    /// `Mpv` is `Send + Sync`, so playback can be started from whichever thread finished
    /// resolving. `None` (with [`Self::init_error`] set) if libmpv could not be brought up;
    /// a later call retries rather than latching that first failure.
    pub fn handle(&mut self) -> Option<Arc<Mpv>> {
        if self.mpv.is_none() {
            match Self::init() {
                Ok(mpv) => {
                    self.mpv = Some(Arc::new(mpv));
                    self.init_error = None;
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    tracing::error!(error = %msg, "libmpv initialisation failed");
                    self.init_error = Some(msg);
                }
            }
        }
        self.mpv.clone()
    }

    /// The message from a failed one-time init, if any.
    pub fn init_error(&self) -> Option<&str> {
        self.init_error.as_deref()
    }

    pub fn state(&self) -> PlayerState {
        if let Some(mpv) = &self.mpv {
            drain_events(mpv);
        }
        let Some(mpv) = &self.mpv else {
            // Nothing loaded — idle, so the queue-advance check behaves as it did before a
            // context was created eagerly.
            return PlayerState {
                idle: true,
                ..PlayerState::default()
            };
        };
        PlayerState {
            position: mpv.get_property("time-pos").unwrap_or(0.0),
            duration: mpv.get_property("duration").unwrap_or(0.0),
            paused: mpv.get_property("pause").unwrap_or(false),
            idle: mpv.get_property("idle-active").unwrap_or(true),
        }
    }

    /// The loaded video's display size, in pixels. `None` for audio, or before the first frame
    /// has reported one. The software renderer draws at this size so mpv does no scaling.
    pub fn video_size(&self) -> Option<(usize, usize)> {
        let mpv = self.mpv.as_ref()?;
        let w: i64 = mpv.get_property("dwidth").ok()?;
        let h: i64 = mpv.get_property("dheight").ok()?;
        (w > 0 && h > 0).then_some((w as usize, h as usize))
    }

    pub fn set_paused(&self, paused: bool) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.set_property("pause", paused);
        }
    }

    pub fn toggle_pause(&self) {
        self.set_paused(!self.state().paused);
    }

    /// Unload whatever is playing. mpv keeps the outgoing file until the next one loads, so a
    /// track that never loads would otherwise leave the previous song playing under it.
    pub fn stop(&self) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command("stop", &[]);
        }
    }

    pub fn seek(&self, seconds: f64) {
        if let Some(mpv) = &self.mpv {
            let _ = mpv.command("seek", &[&format!("{seconds:.3}"), "absolute"]);
        }
    }

    pub fn volume(&self) -> f64 {
        self.volume
    }

    pub fn set_volume(&mut self, volume: f64) {
        self.volume = volume.clamp(0.0, 100.0);
        if let Some(mpv) = &self.mpv {
            let _ = mpv.set_property("volume", self.volume);
        }
    }
}

/// Log mpv's error messages and empty its event queue, without waiting. Only the UI thread calls
/// this, through [`Player::state`]: mpv allows one reader per handle, and nothing else reads it.
fn drain_events(mpv: &Mpv) {
    use libmpv2::events::Event;
    let home = directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_string_lossy().into_owned())
        .unwrap_or_default();
    while let Some(event) = mpv.wait_event(0.0) {
        match event {
            Ok(Event::LogMessage { prefix, text, .. }) => {
                tracing::warn!(source = prefix, "mpv: {}", redact(text.trim_end(), &home));
            }
            // A file that ended with an error comes back this way, as mpv's bare error code.
            Err(libmpv2::Error::Raw(code)) => {
                // SAFETY: mpv returns a static string for every code, known or not.
                let reason =
                    unsafe { std::ffi::CStr::from_ptr(libmpv2_sys::mpv_error_string(code)) };
                tracing::warn!(reason = %reason.to_string_lossy(), "mpv: playback ended with an error");
            }
            Err(e) => tracing::warn!(error = %e, "mpv: playback ended with an error"),
            Ok(_) => {}
        }
    }
}

/// An mpv message made fit for a log people paste into bug reports. A direct stream URL carries
/// the listener's IP address and a signature in its query (`ip=`, `sig=`), and mpv prints the
/// whole URL when it fails to open one, so every query is cut to `?…`; the home folder, which
/// names the user, becomes `~`.
fn redact(text: &str, home: &str) -> String {
    let text = if home.len() > 1 {
        text.replace(home, "~")
    } else {
        text.to_owned()
    };
    text.split(' ')
        .map(|word| match word.split_once('?') {
            Some((before, _)) => format!("{before}?…"),
            None => word.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Put `LC_NUMERIC` back to "C" — see `Player::init` for why. Declared here rather than through
/// `libc` so this stays a single, dependency-free call.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn restore_c_locale() {
    use std::os::raw::{c_char, c_int};
    // glibc and musl number the locale categories identically; `LC_NUMERIC` is 1 in both.
    // Darwin's libc orders them differently: `LC_NUMERIC` is 4.
    #[cfg(target_os = "linux")]
    const LC_NUMERIC: c_int = 1;
    #[cfg(target_os = "macos")]
    const LC_NUMERIC: c_int = 4;
    unsafe extern "C" {
        fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    }
    unsafe {
        setlocale(LC_NUMERIC, c"C".as_ptr());
    }
}

/// The correction, in dB, that brings a song to YouTube's loudness target: its figure is how far
/// above the target it sits, so the gain is the negative of it. Held to −12…+6 dB so a bad figure
/// can never mute a song or blow one out; zero when the normaliser is off or there is no figure.
fn gain_db(normalise: bool, loudness_db: Option<f32>) -> f64 {
    match (normalise, loudness_db) {
        (true, Some(db)) => (-f64::from(db)).clamp(-12.0, 6.0),
        _ => 0.0,
    }
}

/// Hand a resolved URL to mpv and start playing it.
///
/// Separate from `Player` so it can be called from a background task that owns only a handle.
pub fn load(
    mpv: &Mpv,
    url: &str,
    volume: f64,
    loudness_db: Option<f32>,
    normalise: bool,
    start_at: Option<f64>,
) -> Result<()> {
    // Apply YouTube's own loudness measurement so tracks don't jump in level. It rides mpv's
    // `volume-gain` (dB, on top of `volume`), so the slider, mute and media keys can set `volume`
    // later without dropping this song's correction.
    let gain = gain_db(normalise, loudness_db);
    if mpv.set_property("volume-gain", gain).is_ok() {
        let _ = mpv.set_property("volume", volume.clamp(0.0, 130.0));
    } else {
        // An mpv too old to have `volume-gain`: fold the correction into the percentage instead.
        let folded = volume * 10f64.powf(gain / 20.0);
        let _ = mpv.set_property("volume", folded.clamp(0.0, 130.0));
    }
    let _ = mpv.set_property("pause", false);
    // Audio-only again, in case the video view just handed playback back. `vo` stays `libmpv`
    // from the video path, but with `vid=no` there is no video for it to initialise.
    let _ = mpv.set_property("vid", "no");
    // The video path raises this; a song does not need the memory.
    let _ = mpv.set_property("demuxer-max-bytes", "32MiB");
    set_start(mpv, start_at);
    mpv.command("loadfile", &[url, "replace"])
        .context("mpv refused the stream URL")
}

/// Apply a resume position as a per-file `start`, or clear it (`none`) for a fresh play.
///
/// At load time mpv takes the position after the whole file is opened, which is the only point
/// at which an externally attached audio stream exists to be seeked along with the video.
/// Seeking after the load instead moved the video and left the audio at zero — the two then
/// drifted apart for the rest of the track.
fn set_start(mpv: &Mpv, start_at: Option<f64>) {
    let start = start_at.map_or_else(|| "none".to_owned(), |s| format!("+{s:.3}"));
    let _ = mpv.set_property("start", start.as_str());
}

/// Load a music video.
///
/// `video_url` is normally a direct stream the app resolved itself, with `audio_url` its
/// separate adaptive audio track when YouTube served one — mpv plays the pair as a single file.
/// When `video_url` is instead a watch page, mpv's yt-dlp hook fetches it (armed once in
/// [`Player::init`], since mpv ignores `ytdl` set any later); that is the fallback when
/// resolving failed.
pub fn load_video(
    mpv: &Mpv,
    video_url: &str,
    audio_url: Option<&str>,
    volume: f64,
    start_at: Option<f64>,
) -> Result<()> {
    // The software render context was created before this call, and `libmpv` is the vo that
    // hands frames to it. Without this the file plays into nothing and every frame is black.
    let _ = mpv.set_property("vo", "libmpv");
    let _ = mpv.set_property("vid", "auto");
    let _ = mpv.set_property("audio-display", "no");
    // A video carries no loudness figure, and the song before it may have set a gain.
    let _ = mpv.set_property("volume-gain", 0.0);
    let _ = mpv.set_property("volume", volume.clamp(0.0, 130.0));
    let _ = mpv.set_property("pause", false);
    // Decode on the GPU when the platform can, copying frames back into the software surface;
    // mpv falls back to software decoding when it cannot. The buffer is what makes this
    // affordable, not the renderer.
    let _ = mpv.set_property("hwdec", "auto-safe");
    // The hook fallback (a watch page) must ask yt-dlp for the same capped format the direct
    // resolver uses, or it picks whatever is best — a 4K AV1 stream the software path cannot
    // afford. The direct path never consults this.
    let _ = mpv.set_property("ytdl-format", refryt_resolve::VIDEO_FORMAT);
    // A video reads further ahead than a song: a stall in the middle is far more visible.
    let _ = mpv.set_property("demuxer-max-bytes", "64MiB");
    // Insurance for the frames that do get scaled — the native-size path normally avoids the
    // scaler entirely, and bilinear is a fraction of Lanczos' cost when it cannot.
    let _ = mpv.set_property("sws-scaler", "bilinear");
    set_start(mpv, start_at);
    // The fourth `loadfile` argument is a per-file option list, so the external audio applies
    // to this file only and is gone when the next one loads. Commas inside the URL are safe:
    // the option parser takes the value to the end of the entry.
    let options = audio_url.map(|a| format!("audio-files-append={a}"));
    let mut args: Vec<&str> = vec![video_url, "replace", "-1"];
    if let Some(options) = &options {
        args.push(options);
    }
    mpv.command("loadfile", &args)
        .context("mpv refused the video URL")
}

/// What the player bar shows about the track currently loaded.
#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    pub track: Option<Track>,
    /// Set while we are resolving, so the UI can say so instead of looking frozen.
    pub resolving: bool,
    pub error: Option<String>,
    /// Kept in sync every frame from `PlayerState::paused` (see `App::ui`'s per-frame poll) —
    /// carried on `NowPlaying` rather than queried separately so every row/card that already
    /// takes `&NowPlaying` can tell "is my track the current one, and is it actually playing"
    /// without a second parameter threaded everywhere `now` already reaches.
    pub paused: bool,
    /// The playlist a card most recently started playing, and the video ids it fetched to do
    /// so. An album card can tell it's the one playing from `Track::album_id` alone — every
    /// track already carries it — but a playlist has no such back-reference on its tracks (the
    /// same song can sit in many playlists), so this is the only way a playlist card knows.
    /// Self-correcting the same way `App::queue_matches` already is: once `track` is no longer
    /// one of these ids, nothing checks this as a match, so it is never explicitly cleared —
    /// only ever replaced by the next card played this way. Carried forward (not reset) across
    /// every other `NowPlaying` rebuild, since it describes the queue's source, not this one
    /// track.
    pub playlist_playing: Option<(String, Vec<String>)>,
}

#[cfg(test)]
mod tests {
    use super::{gain_db, redact};

    /// The lines mpv really prints for a stream it cannot open and a missing local file: no IP,
    /// no signature and no username may survive into the log.
    #[test]
    fn mpv_messages_lose_the_ip_signature_and_home_folder() {
        let stream = redact(
            "Failed to open https://rr1---sn-x.googlevideo.com/videoplayback?expire=1&ip=203.0.113.7&sig=SECRET.",
            "/home/iv",
        );
        assert_eq!(
            stream,
            "Failed to open https://rr1---sn-x.googlevideo.com/videoplayback?…"
        );
        let hook = redact(
            "ERROR: [generic] videoplayback?expire=1&ip=203.0.113: Unable to download webpage",
            "/home/iv",
        );
        assert!(!hook.contains("203.0.113"), "{hook}");
        let file = redact(
            "Cannot open file '/home/iv/Music/song.flac': No such file or directory",
            "/home/iv",
        );
        assert_eq!(
            file,
            "Cannot open file '~/Music/song.flac': No such file or directory"
        );
        // A message with nothing to hide comes through untouched.
        assert_eq!(
            redact("Failed to recognize file format.", "/home/iv"),
            "Failed to recognize file format."
        );
    }

    /// Loudness normalisation must never mute a track or blow one out, whatever YouTube reports.
    #[test]
    fn loudness_gain_stays_within_sane_bounds() {
        for db in [-40.0f32, -12.0, -3.0, 0.0, 6.0, 40.0] {
            let gain = gain_db(true, Some(db));
            assert!(
                (-12.0..=6.0).contains(&gain),
                "db {db} produced gain {gain}"
            );
        }
    }

    #[test]
    fn a_quiet_track_is_boosted_and_a_loud_one_cut() {
        assert!(gain_db(true, Some(-6.0)) > 0.0);
        assert!(gain_db(true, Some(6.0)) < 0.0);
    }

    #[test]
    fn no_gain_without_the_setting_or_a_figure() {
        assert_eq!(gain_db(false, Some(6.0)), 0.0);
        assert_eq!(gain_db(true, None), 0.0);
    }

    /// The tray's GTK init calls `setlocale(LC_ALL, "")` in-process, which is exactly how
    /// playback once died with "could not initialise libmpv: Null". Creating a context must
    /// survive a non-C locale being in force.
    #[cfg(target_os = "linux")]
    #[test]
    fn libmpv_initialises_with_a_non_c_locale_in_force() {
        use std::os::raw::{c_char, c_int};
        const LC_ALL: c_int = 6;
        unsafe extern "C" {
            fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
        }
        // What GTK does when it comes up: take the environment's locale wholesale.
        unsafe {
            setlocale(LC_ALL, c"".as_ptr());
        }
        assert!(
            super::Player::init().is_ok(),
            "libmpv must create a context after another library moved the locale"
        );
        // Leave the process as the rest of the suite expects it.
        super::restore_c_locale();
    }
}
