//! Fallback resolver that shells out to `yt-dlp`.
//!
//! Slower (~1-2s, dominated by Python startup) but maintained by people who fight YouTube's
//! arms race full time, and it carries support for PO token providers and the SABR
//! downloader. This is what keeps the application alive on the day the native path breaks,
//! without us having to ship a release.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use url::Url;

use crate::{
    AudioStream, Codec, StreamResolver, VideoId, VideoResolver, VideoStream, parse_expiry,
};

/// Prefer Opus, falling back to whatever the best audio-only stream is.
const DEFAULT_FORMAT: &str = "bestaudio[acodec=opus]/bestaudio";

/// The in-app video view's selector: 720p at most, H.264 before anything else.
///
/// The view renders in software, so this is the ceiling that keeps decoding and colour
/// conversion affordable — H.264 is far cheaper to software-decode than VP9 or AV1, and 720p
/// is what the 1280px-wide frame buffer can actually show. The later branches only matter when
/// a video has no AVC rendition at all.
pub const VIDEO_FORMAT: &str = "bestvideo[height<=720][vcodec^=avc1]+bestaudio[acodec^=opus]\
/bestvideo[height<=720]+bestaudio/best[height<=720]";

/// `CREATE_NO_WINDOW`: without it, every spawn of a console-subsystem binary (yt-dlp is one)
/// from this GUI-subsystem app makes Windows allocate it a brand new console, which flashes on
/// screen for the length of the call — once per track resolved. Linux/macOS have no such
/// concept, so this is a no-op there.
#[cfg(windows)]
fn no_console(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd.creation_flags(0x0800_0000)
}
#[cfg(not(windows))]
fn no_console(cmd: &mut tokio::process::Command) -> &mut tokio::process::Command {
    cmd
}

/// The signed-in session's `Cookie` header, shared between whoever owns the session and every
/// yt-dlp run. `None` while signed out: yt-dlp then goes to YouTube anonymously, which is what
/// its "Sign in to confirm you're not a bot" and age gates refuse.
pub type SessionCookie = std::sync::Arc<std::sync::RwLock<Option<String>>>;

/// A Netscape cookie file holding one session, for one yt-dlp run, removed on drop.
///
/// One file per run rather than one shared file: yt-dlp writes its cookie jar back when it
/// exits, and two concurrent runs rewriting the same file could leave a third reading it half
/// written. Created `0600` — it is the account's credential.
pub struct CookieFile(std::path::PathBuf);

impl CookieFile {
    /// Write the current session, or `None` when signed out (or the file cannot be written, in
    /// which case the run goes anonymous rather than failing).
    pub fn for_session(session: &SessionCookie) -> Option<Self> {
        let header = session.read().ok()?.clone()?;
        let text = netscape_cookies(&header)?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("refryt-cookies-{}-{n}.txt", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).ok()?;
        let file_path = Self(path);
        std::io::Write::write_all(&mut file, text.as_bytes()).ok()?;
        Some(file_path)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for CookieFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Whether yt-dlp failed because YouTube wants a signed-in client — its bot check or its age
/// gate — rather than for a reason a session cannot fix.
pub fn is_sign_in_gate(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("Sign in to confirm")
}

/// The words both YouTube's player and yt-dlp use for a song only Music Premium may play.
const PREMIUM_REFUSAL: &str = "only available to Music Premium members";

/// Whether YouTube refused one song for not being a Premium listener. Unlike the bot check this
/// is about the song, not the address: a Premium account gets it, and the next song is unaffected.
pub fn is_premium_gate(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains(PREMIUM_REFUSAL)
}

/// How long a sign-in refusal is believed: while it holds, resolves go straight to the signed-in
/// run instead of paying for an anonymous one that would fail. When YouTube gates anonymous
/// clients it gates all of them from that address, not one video.
const GATE_MEMORY: Duration = Duration::from_secs(15 * 60);

/// How many yt-dlp processes may run at once, across playback, video and downloads. One run
/// peaks near 380 MB with its Deno child (measured 2026-09-23), so this is the memory ceiling.
/// Two, not one, so a play never queues behind a slow prefetch of another track.
const MAX_CONCURRENT_RUNS: usize = 2;

/// A slot to run yt-dlp in. Hold it for as long as the process lives.
pub async fn ytdlp_permit() -> tokio::sync::SemaphorePermit<'static> {
    static RUNS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_RUNS))
        .acquire()
        .await
        .expect("the yt-dlp semaphore is never closed")
}

/// A `Cookie` header (`a=1; b=2`) as the Netscape cookie file yt-dlp's `--cookies` reads, every
/// cookie scoped to `.youtube.com`. `None` when the header holds no cookie at all.
fn netscape_cookies(header: &str) -> Option<String> {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    let mut any = false;
    for (name, value) in header
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, _)| !name.is_empty())
    {
        // domain, include subdomains, path, secure, expiry (far future: the session's own
        // lifetime is YouTube's business, not this file's), name, value.
        out.push_str(&format!(
            ".youtube.com\tTRUE\t/\tTRUE\t2147483647\t{name}\t{value}\n"
        ));
        any = true;
    }
    any.then_some(out)
}

pub struct YtDlpResolver {
    program: String,
    /// Extra arguments, e.g. a PO token provider or a cookies file.
    extra_args: Vec<String>,
    /// The `-f` selector. Changeable at runtime so an audio-quality preference is a real change
    /// to what gets fetched rather than a label; read per resolve, so no rebuild is needed.
    format: std::sync::Mutex<String>,
    timeout: Duration,
    /// The signed-in session, read per run so signing in or out takes effect on the next track.
    session: Option<SessionCookie>,
    /// Until when YouTube is taken to be refusing anonymous clients; see [`GATE_MEMORY`].
    gated_until: std::sync::Mutex<Option<std::time::Instant>>,
    /// Songs an earlier resolver already found Premium-only, each taken once by the next resolve:
    /// asking anonymously again would only repeat the refusal, at the cost of a whole run.
    premium_hint: std::sync::Mutex<std::collections::HashSet<VideoId>>,
}

impl YtDlpResolver {
    pub fn new() -> Self {
        Self {
            program: "yt-dlp".to_owned(),
            extra_args: Vec::new(),
            format: std::sync::Mutex::new(DEFAULT_FORMAT.to_owned()),
            timeout: Duration::from_secs(30),
            session: None,
            gated_until: std::sync::Mutex::new(None),
            premium_hint: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Note that YouTube refused `id` as Premium-only, so the next resolve of it goes straight to
    /// the signed-in run, or fails at once when there is no session to run as.
    pub fn note_premium(&self, id: &VideoId) {
        if let Ok(mut hint) = self.premium_hint.lock() {
            hint.insert(id.clone());
        }
    }

    fn take_premium_hint(&self, id: &VideoId) -> bool {
        self.premium_hint
            .lock()
            .map(|mut hint| hint.remove(id))
            .unwrap_or(false)
    }

    /// Run yt-dlp as the signed-in account whenever there is one.
    pub fn with_session(mut self, session: SessionCookie) -> Self {
        self.session = Some(session);
        self
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// Start out asking for something other than the default audio-only selector — the video
    /// view's own resolver is built with [`VIDEO_FORMAT`].
    pub fn with_format(self, format: &str) -> Self {
        self.set_format(format);
        self
    }

    /// Replace the format selector. Takes `&self`: the resolver lives behind an `Arc` in the
    /// chain, and a preference change must not require rebuilding it.
    pub fn set_format(&self, format: &str) {
        if let Ok(mut f) = self.format.lock() {
            format.clone_into(&mut f);
        }
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Whether the configured yt-dlp is actually present, so the UI can warn early rather
    /// than at the moment playback is wanted.
    pub async fn is_available(&self) -> bool {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        no_console(&mut cmd)
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Default for YtDlpResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// The subset of yt-dlp's `-j` output we care about.
#[derive(Debug, Deserialize)]
struct YtDlpFormat {
    /// Absent entirely when the selection was `bestvideo+bestaudio`: yt-dlp then prints the
    /// merged selection with only `requested_formats` carrying URLs. Audio-only payloads always
    /// have it, and `resolve` still rejects an empty one.
    #[serde(default)]
    url: String,
    #[serde(default)]
    acodec: Option<String>,
    #[serde(default)]
    vcodec: Option<String>,
    #[serde(default)]
    abr: Option<f64>,
    #[serde(default)]
    tbr: Option<f64>,
    #[serde(default)]
    format_id: Option<String>,
    #[serde(default)]
    filesize: Option<u64>,
    #[serde(default)]
    filesize_approx: Option<u64>,
    #[serde(default)]
    duration: Option<f64>,
    /// With a `bestvideo+bestaudio` selector, `-j` prints the merged selection's top-level
    /// fields and lists the streams it picked here. The top-level `url` is empty in that case,
    /// so this is the only place the playable URLs exist.
    #[serde(default)]
    requested_formats: Option<Vec<YtDlpFormat>>,
}

impl YtDlpResolver {
    /// Ask yt-dlp for `format`, anonymously first. Only when YouTube refuses an anonymous client
    /// ("Sign in to confirm you're not a bot", "… your age") is it asked again as the signed-in
    /// account, with `/best` appended.
    ///
    /// Not signed in every time: signed in, yt-dlp takes YouTube's web clients, which need a PO
    /// token it cannot mint, so every audio-only stream is withheld and only the combined 360p
    /// file is offered (measured 2026-09-23). Anonymous keeps Opus for everything YouTube lets
    /// through; the fallback trades quality for playing at all, and mpv runs with `vid=no`, so
    /// the combined file plays as its audio alone.
    ///
    /// Each yt-dlp run costs seconds, so a refusal is remembered for [`GATE_MEMORY`]: in that
    /// window the anonymous attempt is skipped, and after it the next resolve tries anonymously
    /// again, so Opus comes back by itself once YouTube relents.
    ///
    /// A Premium-only refusal also retries signed in, but is not remembered: it is about one
    /// song, and the next one still starts anonymously.
    async fn run_gated(&self, id: &VideoId, format: &str) -> Result<YtDlpFormat> {
        let signed_in = |cookies: CookieFile| async move {
            self.run_json(id, &format!("{format}/best"), Some(&cookies))
                .await
        };
        let cookies = || self.session.as_ref().and_then(CookieFile::for_session);
        if self.take_premium_hint(id) {
            return match cookies() {
                Some(cookies) => signed_in(cookies)
                    .await
                    .with_context(|| format!("retried signed in after: {PREMIUM_REFUSAL}")),
                None => bail!("{id} is {PREMIUM_REFUSAL}, and there is no session to run as"),
            };
        }
        if self.gated()
            && let Some(cookies) = cookies()
        {
            return signed_in(cookies).await;
        }
        let anonymous = self.run_json(id, format, None).await;
        let Err(refusal) = &anonymous else {
            return anonymous;
        };
        let premium = is_premium_gate(refusal);
        if !premium && !is_sign_in_gate(refusal) {
            return anonymous;
        }
        let Some(cookies) = cookies() else {
            return anonymous;
        };
        if !premium {
            self.remember_gate();
        }
        signed_in(cookies)
            .await
            .with_context(|| format!("retried signed in after: {refusal:#}"))
    }

    /// Whether a recent refusal still stands. Clears (and says so) once it has lapsed.
    fn gated(&self) -> bool {
        let Ok(mut until) = self.gated_until.lock() else {
            return false;
        };
        match *until {
            Some(at) if std::time::Instant::now() < at => true,
            Some(_) => {
                *until = None;
                tracing::info!("yt-dlp: trying anonymously again");
                false
            }
            None => false,
        }
    }

    fn remember_gate(&self) {
        if let Ok(mut until) = self.gated_until.lock() {
            if until.is_none() {
                tracing::info!(
                    minutes = GATE_MEMORY.as_secs() / 60,
                    "yt-dlp: YouTube wants a sign-in; resolving as the account for now"
                );
            }
            *until = Some(std::time::Instant::now() + GATE_MEMORY);
        }
    }

    /// Ask yt-dlp for one format and parse its `-j` output.
    async fn run_json(
        &self,
        id: &VideoId,
        format: &str,
        cookies: Option<&CookieFile>,
    ) -> Result<YtDlpFormat> {
        // Held until the process is done; see `MAX_CONCURRENT_RUNS`.
        let _permit = ytdlp_permit().await;
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(["-j", "--no-warnings", "--no-playlist", "-f", format])
            .args(&self.extra_args);
        if let Some(file) = cookies {
            cmd.arg("--cookies").arg(file.path());
        }
        cmd.arg(id.watch_url())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        no_console(&mut cmd);

        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .with_context(|| format!("yt-dlp timed out resolving {id}"))?
            .with_context(|| format!("failed to run {}", self.program))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("yt-dlp failed for {id}: {}", stderr.trim());
        }

        serde_json::from_slice(&output.stdout).context("could not parse yt-dlp JSON output")
    }
}

#[async_trait::async_trait]
impl StreamResolver for YtDlpResolver {
    fn name(&self) -> &'static str {
        "yt-dlp"
    }

    async fn resolve(&self, id: &VideoId) -> Result<AudioStream> {
        let format = self
            .format
            .lock()
            .map(|f| f.clone())
            .unwrap_or_else(|_| DEFAULT_FORMAT.to_owned());
        let parsed = self.run_gated(id, &format).await?;

        if parsed.url.is_empty() {
            bail!("yt-dlp returned no URL for {id}");
        }

        let codec = parsed
            .acodec
            .as_deref()
            .map(Codec::from_codec_str)
            .unwrap_or(Codec::Other);

        let url = Url::parse(&parsed.url)
            .map_err(|e| anyhow!("yt-dlp returned an unparseable URL for {id}: {e}"))?;

        Ok(AudioStream {
            itag: parsed
                .format_id
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            codec,
            bitrate_kbps: parsed.abr.or(parsed.tbr).unwrap_or(0.0).round() as u32,
            content_length: parsed.filesize.or(parsed.filesize_approx),
            duration: parsed.duration.map(Duration::from_secs_f64),
            expires_at: parse_expiry(&url),
            // yt-dlp does not surface YouTube's loudness measurement in this shape; mpv
            // falls back to its own ReplayGain handling when this is absent.
            loudness_db: None,
            url,
            resolved_by: "yt-dlp",
            mime: None,
            needs_ranged: false,
        })
    }
}

/// Turn one parsed `-j` payload into a [`VideoStream`].
///
/// Split out from the command so the parsing — which is the fragile half — is testable without
/// a network call. `requested_formats` is the normal shape for a `+` selector: the video and
/// audio URLs live there, each marked by which codec it has. A single entry (a progressive
/// format, or a video with no separate audio) falls back to the top-level `url`.
fn video_stream_from(parsed: &YtDlpFormat, id: &VideoId) -> Result<VideoStream> {
    let has_video = |f: &YtDlpFormat| f.vcodec.as_deref().is_some_and(|c| c != "none");
    let has_audio = |f: &YtDlpFormat| f.acodec.as_deref().is_some_and(|c| c != "none");

    let mut video = None;
    let mut audio = None;
    if let Some(formats) = &parsed.requested_formats {
        for f in formats {
            let url = Url::parse(&f.url)
                .map_err(|e| anyhow!("yt-dlp returned an unparseable video URL for {id}: {e}"))?;
            if has_video(f) {
                video = Some(url);
            } else if has_audio(f) {
                audio = Some(url);
            }
        }
    }
    let video = match video {
        Some(url) => url,
        None => {
            if parsed.url.is_empty() || !has_video(parsed) {
                bail!("yt-dlp returned no video stream for {id}");
            }
            Url::parse(&parsed.url)
                .map_err(|e| anyhow!("yt-dlp returned an unparseable video URL for {id}: {e}"))?
        }
    };
    Ok(VideoStream {
        expires_at: parse_expiry(&video),
        video,
        audio,
        duration: parsed.duration.map(Duration::from_secs_f64),
        resolved_by: "yt-dlp",
    })
}

#[async_trait::async_trait]
impl VideoResolver for YtDlpResolver {
    fn name(&self) -> &'static str {
        "yt-dlp"
    }

    async fn resolve_video(&self, id: &VideoId) -> Result<VideoStream> {
        let parsed = self.run_gated(id, VIDEO_FORMAT).await?;
        video_stream_from(&parsed, id)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The bot check and the age gate retry signed in; anything else (a removed video, a
    /// network failure) does not, since a session changes nothing there.
    #[test]
    fn only_youtubes_sign_in_refusals_count_as_a_gate() {
        let bot = anyhow!(
            "yt-dlp failed for x: ERROR: [youtube] x: Sign in to confirm you’re not a bot."
        );
        let age = anyhow!("yt-dlp failed for x: ERROR: [youtube] x: Sign in to confirm your age.");
        let gone = anyhow!("yt-dlp failed for x: ERROR: [youtube] x: Video unavailable");
        assert!(is_sign_in_gate(&bot));
        assert!(is_sign_in_gate(&age));
        assert!(!is_sign_in_gate(&gone));
    }

    /// The Premium refusal is its own kind: it retries signed in, but is not the bot check.
    #[test]
    fn a_premium_refusal_is_not_a_sign_in_gate() {
        let premium = anyhow!(
            "yt-dlp failed for x: ERROR: [youtube] x: This video is only available to Music Premium members"
        );
        let guest = anyhow!(
            "guest player for x: UNPLAYABLE: This video is only available to Music Premium members"
        );
        assert!(is_premium_gate(&premium));
        assert!(is_premium_gate(&guest));
        assert!(!is_sign_in_gate(&premium));
    }

    /// Each pair becomes one `.youtube.com` line; stray separators and empty names are
    /// dropped, and `=` inside a value survives.
    #[test]
    fn a_cookie_header_becomes_a_netscape_file() {
        let text = netscape_cookies("SID=abc; ; __Secure-3PSID=x=y;=nope").unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "# Netscape HTTP Cookie File");
        assert_eq!(
            lines[1],
            ".youtube.com\tTRUE\t/\tTRUE\t2147483647\tSID\tabc"
        );
        assert_eq!(
            lines[2],
            ".youtube.com\tTRUE\t/\tTRUE\t2147483647\t__Secure-3PSID\tx=y"
        );
        assert_eq!(lines.len(), 3);
        assert_eq!(netscape_cookies(" ; "), None);
    }

    /// Signed out writes nothing; signed in writes a private file that is gone once dropped.
    #[test]
    fn the_cookie_file_is_private_and_removed_after_the_run() {
        let session: SessionCookie = std::sync::Arc::new(std::sync::RwLock::new(None));
        assert!(CookieFile::for_session(&session).is_none());

        *session.write().unwrap() = Some("SID=abc".to_owned());
        let file = CookieFile::for_session(&session).unwrap();
        let path = file.path().to_owned();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\tSID\tabc")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn parses_a_representative_yt_dlp_payload() {
        let json = r#"{
            "url": "https://r5.googlevideo.com/videoplayback?expire=1757200000&itag=251",
            "acodec": "opus",
            "abr": 159.99,
            "format_id": "251",
            "filesize": 3456789,
            "duration": 213.5
        }"#;
        let f: YtDlpFormat = serde_json::from_str(json).unwrap();
        assert_eq!(f.format_id.as_deref(), Some("251"));
        assert_eq!(
            Codec::from_codec_str(f.acodec.as_deref().unwrap()),
            Codec::Opus
        );
        assert_eq!(f.abr.unwrap().round() as u32, 160);
    }

    #[test]
    fn tolerates_a_payload_missing_optional_fields() {
        let json = r#"{"url": "https://example.com/x"}"#;
        let f: YtDlpFormat = serde_json::from_str(json).unwrap();
        assert!(f.acodec.is_none());
        assert!(f.filesize.is_none());
    }

    fn id() -> VideoId {
        VideoId::new("dQw4w9WgXcQ").unwrap()
    }

    /// The shape a `bestvideo+bestaudio` selection actually prints: the top-level `url` is
    /// empty and the playable streams are the two entries in `requested_formats`, marked by
    /// which codec each one has. This is the payload the video view depends on.
    #[test]
    fn a_requested_formats_payload_splits_video_and_audio() {
        // No top-level `url` at all — the real shape of a `+` selection.
        let json = r#"{
            "duration": 213.5,
            "requested_formats": [
                {"url": "https://r1.googlevideo.com/videoplayback?expire=1757200000&itag=136",
                 "vcodec": "avc1.4d401f", "acodec": "none", "format_id": "136", "height": 720},
                {"url": "https://r1.googlevideo.com/videoplayback?expire=1757200000&itag=251",
                 "vcodec": "none", "acodec": "opus", "format_id": "251"}
            ]
        }"#;
        let parsed: YtDlpFormat = serde_json::from_str(json).unwrap();
        let stream = video_stream_from(&parsed, &id()).unwrap();
        assert!(stream.video.as_str().contains("itag=136"), "{stream:?}");
        assert!(
            stream
                .audio
                .as_ref()
                .is_some_and(|a| a.as_str().contains("itag=251")),
            "{stream:?}"
        );
        assert_eq!(stream.duration, Some(Duration::from_secs_f64(213.5)));
        assert!(stream.expires_at.is_some());
    }

    /// A muxed fallback has both codecs on the one top-level URL, so there is no external audio.
    #[test]
    fn a_progressive_payload_needs_no_external_audio() {
        let json = r#"{
            "url": "https://r1.googlevideo.com/videoplayback?expire=1757200000&itag=18",
            "vcodec": "avc1.42001E", "acodec": "mp4a.40.2"
        }"#;
        let parsed: YtDlpFormat = serde_json::from_str(json).unwrap();
        let stream = video_stream_from(&parsed, &id()).unwrap();
        assert!(stream.audio.is_none());
    }

    /// If the selector somehow lands on an audio-only stream, that is not a video and must be
    /// an error rather than a "video" that is really sound.
    #[test]
    fn an_audio_only_payload_is_not_a_video() {
        let json = r#"{
            "url": "https://r1.googlevideo.com/videoplayback?itag=251",
            "vcodec": "none", "acodec": "opus"
        }"#;
        let parsed: YtDlpFormat = serde_json::from_str(json).unwrap();
        assert!(video_stream_from(&parsed, &id()).is_err());
    }

    #[cfg(unix)]
    const BOT_CHECK: &str = "ERROR: [youtube] x: Sign in to confirm you are not a bot.";
    #[cfg(unix)]
    pub(crate) const PREMIUM_ONLY: &str =
        "ERROR: [youtube] x: This video is only available to Music Premium members";

    /// A stub yt-dlp that refuses anonymous runs with `refusal` and answers signed-in ones,
    /// recording every run in `log`.
    ///
    /// The script is written beside its final name and put in place by `install`, so this
    /// process never holds it open for writing. A test on another thread that spawns while
    /// `fs::write` has the file open hands that descriptor to its child until the child's exec,
    /// and running the script inside that window fails with ETXTBSY ("Text file busy").
    #[cfg(unix)]
    pub(crate) fn gated_stub(dir: &std::path::Path, refusal: &str) -> (String, std::path::PathBuf) {
        let log = dir.join("runs");
        let script = dir.join("yt-dlp");
        let staged = dir.join("yt-dlp.sh");
        std::fs::write(
            &staged,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *--cookies*) echo signed >> '{log}'; \
                 echo '{{\"url\":\"https://example.com/a\",\"acodec\":\"mp4a.40.2\",\"format_id\":\"18\"}}' ;;\n\
                 *) echo anonymous >> '{log}'; \
                 echo '{refusal}' >&2; exit 1 ;;\n\
                 esac\n",
                log = log.display()
            ),
        )
        .unwrap();
        let installed = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&staged)
            .arg(&script)
            .status()
            .unwrap();
        assert!(
            installed.success(),
            "install could not place the stub yt-dlp"
        );
        (script.to_string_lossy().into_owned(), log)
    }

    /// Once refused, the next track skips the anonymous attempt: one yt-dlp run, not two.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refusal_is_remembered_so_the_next_track_runs_yt_dlp_once() {
        let dir = std::env::temp_dir().join(format!("refryt-gate-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (program, log) = gated_stub(&dir, BOT_CHECK);
        let resolver = YtDlpResolver::new()
            .with_program(program)
            .with_session(signed_in_session());

        let first = resolver
            .resolve(&VideoId::new("dQw4w9WgXcQ").unwrap())
            .await;
        assert_eq!(first.unwrap().itag, 18);
        let second = resolver
            .resolve(&VideoId::new("TiebZllW8As").unwrap())
            .await;
        assert_eq!(second.unwrap().itag, 18);

        let runs = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            runs.lines().collect::<Vec<_>>(),
            ["anonymous", "signed", "signed"]
        );
    }

    #[cfg(unix)]
    pub(crate) fn signed_in_session() -> SessionCookie {
        std::sync::Arc::new(std::sync::RwLock::new(Some("SID=abc".to_owned())))
    }

    /// A Premium-only song retries as the account, but the next song still starts anonymously:
    /// the refusal was about that one song.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_premium_refusal_retries_signed_in_but_is_not_remembered() {
        let dir = std::env::temp_dir().join(format!("refryt-premium-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (program, log) = gated_stub(&dir, PREMIUM_ONLY);
        let resolver = YtDlpResolver::new()
            .with_program(program)
            .with_session(signed_in_session());

        for id in ["dQw4w9WgXcQ", "TiebZllW8As"] {
            let stream = resolver.resolve(&VideoId::new(id).unwrap()).await;
            assert_eq!(stream.unwrap().itag, 18);
        }

        let runs = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            runs.lines().collect::<Vec<_>>(),
            ["anonymous", "signed", "anonymous", "signed"]
        );
    }

    /// Signed out, a song already known to be Premium-only fails without running yt-dlp at all.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_premium_hint_without_a_session_fails_without_a_run() {
        let dir = std::env::temp_dir().join(format!("refryt-hint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (program, log) = gated_stub(&dir, PREMIUM_ONLY);
        let resolver = YtDlpResolver::new().with_program(program);
        let id = VideoId::new("dQw4w9WgXcQ").unwrap();

        resolver.note_premium(&id);
        let error = resolver.resolve(&id).await.unwrap_err();

        let ran = log.exists();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(is_premium_gate(&error));
        assert!(!ran);
    }
}
