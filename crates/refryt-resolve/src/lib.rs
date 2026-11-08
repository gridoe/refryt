//! Stream URL resolution for YouTube Music.
//!
//! This crate is deliberately separate from the application. It is the only part of the
//! project that depends on YouTube's undocumented, actively-changing stream protection
//! (signature cipher, `n` parameter, PO tokens, SABR). Isolating it means it can be tested,
//! versioned and replaced without touching the UI, and that when it breaks the blast radius
//! is one trait implementation.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use url::Url;

mod cache;
mod chain;
mod guest;
#[cfg(feature = "native-resolver")]
mod native;
mod proxy;
mod ytdlp;

pub use cache::{StreamCache, VideoStreamCache};
pub use chain::ChainResolver;
pub use guest::{GUEST_USER_AGENT, GuestQuality, GuestResolver};
#[cfg(feature = "native-resolver")]
pub use native::{NativeConfig, NativeResolver};
pub use proxy::{Recorded, Refresh, StreamProxy};
pub use ytdlp::{
    CookieFile, SessionCookie, VIDEO_FORMAT, YtDlpResolver, is_premium_gate, is_sign_in_gate,
    ytdlp_permit,
};

/// Audio codec of a resolved stream.
///
/// Opus is what we want: itag 251 is ~160kbps VBR and is the best audio the free tier serves.
/// AAC (itag 140, 128kbps) is the fallback when Opus is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Opus,
    Aac,
    Other,
}

impl Codec {
    fn from_codec_str(s: &str) -> Self {
        let s = s.to_ascii_lowercase();
        if s.contains("opus") {
            Codec::Opus
        } else if s.contains("mp4a") || s.contains("aac") {
            Codec::Aac
        } else {
            Codec::Other
        }
    }
}

impl std::fmt::Display for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Codec::Opus => write!(f, "opus"),
            Codec::Aac => write!(f, "aac"),
            Codec::Other => write!(f, "other"),
        }
    }
}

/// A YouTube video id, validated on construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoId(String);

impl VideoId {
    pub fn new(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        if id.len() != 11
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("not a valid YouTube video id: {id:?}");
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn watch_url(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.0)
    }
}

impl std::fmt::Display for VideoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A resolved, directly-playable audio stream.
#[derive(Debug, Clone)]
pub struct AudioStream {
    pub url: Url,
    pub itag: u32,
    pub codec: Codec,
    pub bitrate_kbps: u32,
    pub content_length: Option<u64>,
    pub duration: Option<Duration>,
    /// From the URL's `expire` parameter. Typically ~6h out.
    pub expires_at: Option<SystemTime>,
    /// YouTube's own loudness measurement, fed to mpv for normalisation.
    pub loudness_db: Option<f32>,
    /// Which resolver produced this. Recorded so rot in the fast path is visible in logs.
    pub resolved_by: &'static str,
    /// The container's MIME type (`audio/webm`), when the resolver knows it. The stream proxy
    /// answers with it.
    pub mime: Option<String>,
    /// Throttled unless fetched in ranges: play it through [`StreamProxy`], not by URL.
    pub needs_ranged: bool,
}

impl AudioStream {
    /// Whether this stream is still safe to hand to the player.
    ///
    /// A margin is applied because a URL that expires mid-track is worse than one we
    /// re-resolve slightly early.
    pub fn is_fresh(&self, margin: Duration) -> bool {
        match self.expires_at {
            Some(exp) => SystemTime::now() + margin < exp,
            None => true,
        }
    }
}

/// A resolved music video: a video stream, plus its audio when YouTube served the two as
/// separate adaptive streams — which is what `bestvideo+bestaudio` selects, and what mpv then
/// plays as one file with the audio attached as an external track.
#[derive(Debug, Clone)]
pub struct VideoStream {
    pub video: Url,
    pub audio: Option<Url>,
    pub duration: Option<Duration>,
    /// From the video URL's `expire` parameter, like [`AudioStream::expires_at`].
    pub expires_at: Option<SystemTime>,
    pub resolved_by: &'static str,
}

impl VideoStream {
    /// Whether this stream is still safe to hand to the player. Same margin rule as
    /// [`AudioStream::is_fresh`]: expiring mid-video is worse than re-resolving early.
    pub fn is_fresh(&self, margin: Duration) -> bool {
        match self.expires_at {
            Some(exp) => SystemTime::now() + margin < exp,
            None => true,
        }
    }
}

/// Resolves a video id to a playable video stream.
///
/// Deliberately not part of [`StreamResolver`]: the shape is different (two URLs, not one), and
/// the native path cannot do video at all — `yt-dlp` is the only resolver that can.
#[async_trait::async_trait]
pub trait VideoResolver: Send + Sync {
    async fn resolve_video(&self, id: &VideoId) -> Result<VideoStream>;
    /// Stable short name, used in logs.
    fn name(&self) -> &'static str;
}

/// Parse the `expire` query parameter (unix seconds) out of a googlevideo URL.
pub(crate) fn parse_expiry(url: &Url) -> Option<SystemTime> {
    let secs: u64 = url
        .query_pairs()
        .find(|(k, _)| k == "expire")
        .and_then(|(_, v)| v.parse().ok())?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

/// Rank an audio format. Higher is better.
///
/// Opus is preferred over AAC at any bitrate, so a 128k Opus beats a 128k AAC; within a
/// codec, higher bitrate wins.
/// The guest and native resolvers rank formats themselves; yt-dlp does its own selection.
pub(crate) fn score(codec: Codec, bitrate_kbps: u32) -> u64 {
    let codec_weight: u64 = match codec {
        Codec::Opus => 2_000_000,
        Codec::Aac => 1_000_000,
        Codec::Other => 0,
    };
    codec_weight + u64::from(bitrate_kbps)
}

/// Resolves a video id to a playable audio stream URL.
/// So a resolver can be shared — the chain holds one and the app retunes it.
#[async_trait::async_trait]
impl<T: StreamResolver + ?Sized> StreamResolver for std::sync::Arc<T> {
    async fn resolve(&self, id: &VideoId) -> Result<AudioStream> {
        (**self).resolve(id).await
    }

    fn name(&self) -> &'static str {
        (**self).name()
    }
}

#[async_trait::async_trait]
pub trait StreamResolver: Send + Sync {
    async fn resolve(&self, id: &VideoId) -> Result<AudioStream>;
    /// Stable short name, used in logs and by the circuit breaker.
    fn name(&self) -> &'static str;
}

/// Largest single range request googlevideo will serve.
///
/// Discovered empirically at the M1 gate: on a URL without `ratebypass`, a 512 KiB range is
/// served but a 1 MiB range is refused with 403, as is a request with no Range header at all.
/// YouTube uses this to force clients into chunked reads. mpv already behaves this way; our
/// throughput probe has to as well, or it measures nothing but a rejection.
const MAX_RANGE_CHUNK: u64 = 512 * 1024;

/// Measure achievable throughput on a resolved URL by reading sequential chunks.
///
/// This exists because a *successful* resolve is not proof of a working resolver: if the
/// `n` parameter was deciphered wrongly YouTube still serves the stream, just throttled to
/// a trickle. Speed is the only reliable signal, so it is asserted in the M1 gate and in
/// the live tests.
pub async fn probe_throughput(stream: &AudioStream, want_bytes: u64) -> Result<(u64, Duration)> {
    let client = reqwest::Client::builder()
        .build()
        .context("building probe client")?;

    let want = match stream.content_length {
        Some(len) => want_bytes.min(len),
        None => want_bytes,
    };

    let started = std::time::Instant::now();
    let mut total = 0u64;

    while total < want {
        let end = (total + MAX_RANGE_CHUNK - 1).min(want - 1);
        let resp = client
            .get(stream.url.as_str())
            .header("Range", format!("bytes={total}-{end}"))
            .send()
            .await
            .context("probe request failed")?
            .error_for_status()
            .context("probe request returned an error status")?;

        let chunk = resp.bytes().await.context("probe stream interrupted")?;
        if chunk.is_empty() {
            break;
        }
        total += chunk.len() as u64;
    }

    Ok((total, started.elapsed()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_id_validates() {
        assert!(VideoId::new("dQw4w9WgXcQ").is_ok());
        assert!(VideoId::new("short").is_err());
        assert!(VideoId::new("has spaces!").is_err());
        assert!(VideoId::new("waytoolongvideoid").is_err());
    }

    #[test]
    fn opus_outranks_aac_even_at_lower_bitrate() {
        assert!(score(Codec::Opus, 128) > score(Codec::Aac, 128));
        assert!(score(Codec::Aac, 128) > score(Codec::Other, 999));
    }

    #[test]
    fn higher_bitrate_wins_within_a_codec() {
        assert!(score(Codec::Opus, 160) > score(Codec::Opus, 128));
    }

    #[test]
    fn codec_detected_from_mime_codec_string() {
        assert_eq!(Codec::from_codec_str("opus"), Codec::Opus);
        assert_eq!(Codec::from_codec_str("mp4a.40.2"), Codec::Aac);
        assert_eq!(Codec::from_codec_str("vp9"), Codec::Other);
    }

    #[test]
    fn expiry_parsed_from_url() {
        let u =
            Url::parse("https://r1.googlevideo.com/videoplayback?expire=1757200000&id=x").unwrap();
        let exp = parse_expiry(&u).unwrap();
        assert_eq!(exp, UNIX_EPOCH + Duration::from_secs(1_757_200_000));
    }

    #[test]
    fn missing_expiry_is_treated_as_fresh() {
        let s = AudioStream {
            url: Url::parse("https://example.com/a").unwrap(),
            itag: 251,
            codec: Codec::Opus,
            bitrate_kbps: 160,
            content_length: None,
            duration: None,
            expires_at: None,
            loudness_db: None,
            resolved_by: "test",
            mime: None,
            needs_ranged: false,
        };
        assert!(s.is_fresh(Duration::from_secs(600)));
    }

    #[test]
    fn expiry_margin_is_respected() {
        let mut s = AudioStream {
            url: Url::parse("https://example.com/a").unwrap(),
            itag: 251,
            codec: Codec::Opus,
            bitrate_kbps: 160,
            content_length: None,
            duration: None,
            expires_at: Some(SystemTime::now() + Duration::from_secs(300)),
            loudness_db: None,
            resolved_by: "test",
            mime: None,
            needs_ranged: false,
        };
        // Expires in 5 minutes; with a 10 minute margin it is already stale.
        assert!(!s.is_fresh(Duration::from_secs(600)));
        assert!(s.is_fresh(Duration::from_secs(60)));

        s.expires_at = Some(SystemTime::now() - Duration::from_secs(1));
        assert!(!s.is_fresh(Duration::ZERO));
    }
}
