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

/// The mirror nodes a googlevideo URL offers instead of the one its host points at, in the
/// order `mn=` lists them. The node the host is on is the list's first entry — `mvi` is the
/// `rrN` host index, *not* a position in this list (measured 2026-11-16: a URL on
/// `rr1---sn-8pxuuxahv1-h2hz` with `mvi=1` and `mn=sn-8pxuuxahv1-h2hz,sn-8xgp1vo-ab5s`) —
/// so the mirrors are the entries the host's own node does not name. Empty when the URL
/// names no mirrors: localhost test servers, non-googlevideo hosts, a list of one.
fn mn_mirrors(url: &Url) -> Vec<String> {
    let Some(nodes) = url.query_pairs().find(|(k, _)| k == "mn").map(|(_, v)| v) else {
        return Vec::new();
    };
    let on = url
        .host_str()
        .unwrap_or_default()
        .split_once("---")
        .map(|(_, node)| node.trim_end_matches(".googlevideo.com"));
    nodes
        .split(',')
        .map(str::to_owned)
        .filter(|node| Some(node.as_str()) != on)
        .collect()
}

/// Rebuild a googlevideo URL on `node`, one of the cache nodes its `mn=` names: same path,
/// query and `rrN` prefix, with only the node between the host's dashes swapped. The
/// signature travels in the query and is not tied to the host, so nothing else changes.
/// `None` when the host is not the `rrN---<node>.googlevideo.com` shape.
fn mirror_url(url: &Url, node: &str) -> Option<Url> {
    let host = url.host_str()?;
    if !host.ends_with(".googlevideo.com") {
        return None;
    }
    let (prefix, _) = host.split_once("---")?;
    let mut mirrored = url.clone();
    mirrored
        .set_host(Some(&format!("{prefix}---{node}.googlevideo.com")))
        .ok()?;
    Some(mirrored)
}

/// How long a stream host gets to send its response headers, connection included, before
/// it counts as unreachable and a mirror is tried: for [`probe_first_byte`] and for each
/// range the proxy fetches. A reachable host answers in well under a second (0.06–0.75 s
/// measured 2026-11-17, cold), and a dead one should cost little more than this.
pub(crate) const STREAM_DEADLINE: Duration = Duration::from_secs(6);

/// Ask a stream host for its first byte, and fail over to a mirror node when it will not
/// take the connection.
///
/// YouTube's cache picker now and then assigns a node a client cannot reach at all — the
/// TCP connection neither answers nor refuses, for tens of seconds (measured 2026-11-16:
/// both `player` and yt-dlp kept handing out URLs on a node that did this from one
/// address). The URL itself names the nodes YouTube had in mind, so the fix needs no
/// second resolve: see [`mn_mirrors`] and [`mirror_url`]. The probe walks that list and
/// rewrites `url` in place onto the first node that answers, which the caller then plays;
/// the connection it opens is pooled in `http`, so the stream's first real range reuses it.
/// A host that *answers* is another matter — a 403 or 410 is the URL expiring, which no
/// node fixes — so an HTTP status is an error, never a failover.
pub async fn probe_first_byte(http: &reqwest::Client, url: &mut Url) -> Result<()> {
    let get = |url: &Url| {
        http.get(url.as_str())
            .header("Range", "bytes=0-0")
            .header("User-Agent", guest::GUEST_USER_AGENT)
            // Short, so a dead node costs seconds: the mirrors are tried while the caller
            // is still waiting for a URL worth playing.
            .timeout(STREAM_DEADLINE)
            .send()
    };
    let response = match get(url).await {
        Ok(response) => response,
        Err(lost) => {
            for node in mn_mirrors(url) {
                let Some(mirrored) = mirror_url(url, &node) else {
                    continue;
                };
                match get(&mirrored).await {
                    Ok(answered) if matches!(answered.status().as_u16(), 200 | 206) => {
                        tracing::info!(
                            host = mirrored.host_str().unwrap_or_default(),
                            "stream host unreachable; playing from a mirror"
                        );
                        *url = mirrored;
                        return Ok(());
                    }
                    // A mirror that refuses the URL or will not answer is no better than
                    // the node we came from; try the next.
                    Ok(_) | Err(_) => continue,
                }
            }
            return Err(lost).context("the stream did not answer");
        }
    };
    match response.status().as_u16() {
        200 | 206 => Ok(()),
        status => bail!("the stream answered {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local upstreams keep googlevideo hostnames without changing process-wide DNS or proxies.
    pub(crate) mod mirror_fixture {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use url::Url;

        pub(crate) const PRIMARY: &str = "rr1---sn-primary.googlevideo.com";
        pub(crate) const FAILED: &str = "rr1---sn-failed.googlevideo.com";
        pub(crate) const REFUSED: &str = "rr1---sn-refused.googlevideo.com";
        pub(crate) const LIVE: &str = "rr1---sn-live.googlevideo.com";
        pub(crate) const FRESH: &str = "rr1---sn-fresh.googlevideo.com";

        #[derive(Debug)]
        pub(crate) struct Request {
            pub(crate) host: String,
            pub(crate) target: String,
        }

        pub(crate) struct MirrorHost {
            addr: std::net::SocketAddr,
            pub(crate) http: reqwest::Client,
            held: Arc<Mutex<Vec<tokio::net::TcpStream>>>,
            stop: Option<tokio::sync::oneshot::Sender<()>>,
            task: tokio::task::JoinHandle<Vec<Request>>,
        }

        impl MirrorHost {
            /// `None` closes after reading the request, before sending any HTTP response.
            pub(crate) async fn start(
                routes: &[(&'static str, Option<u16>)],
                data: Arc<Vec<u8>>,
            ) -> Self {
                Self::start_inner(routes, data, false).await
            }

            /// Keep primary sockets open without headers while serving the live mirror normally.
            pub(crate) async fn start_hanging_primary(data: Arc<Vec<u8>>) -> Self {
                Self::start_inner(&[(PRIMARY, None), (LIVE, Some(206))], data, true).await
            }

            async fn start_inner(
                routes: &[(&'static str, Option<u16>)],
                data: Arc<Vec<u8>>,
                hold_primary: bool,
            ) -> Self {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                let mut client = reqwest::Client::builder().no_proxy();
                for (host, _) in routes {
                    client = client.resolve(host, addr);
                }
                let http = client.build().unwrap();
                let routes: HashMap<_, _> = routes.iter().copied().collect();
                let (stop, mut stopped) = tokio::sync::oneshot::channel();
                let held = Arc::new(Mutex::new(Vec::new()));
                let held_by_task = Arc::clone(&held);
                let task = tokio::spawn(async move {
                    let mut seen = Vec::new();
                    loop {
                        let (mut socket, _) = tokio::select! {
                            _ = &mut stopped => break,
                            accepted = listener.accept() => accepted.unwrap(),
                        };
                        let mut bytes = Vec::new();
                        let mut buf = [0u8; 1024];
                        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = socket.read(&mut buf).await.unwrap();
                            assert_ne!(n, 0, "upstream request ended before its headers");
                            bytes.extend_from_slice(&buf[..n]);
                            assert!(bytes.len() <= 16 * 1024, "upstream headers too large");
                        }
                        let text = String::from_utf8(bytes).unwrap();
                        let header = |name: &str| {
                            text.lines()
                                .find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case(name)
                                        .then(|| value.trim().to_owned())
                                })
                                .unwrap()
                        };
                        let host = header("host").split(':').next().unwrap().to_owned();
                        let range = header("range");
                        let target = text.split_whitespace().nth(1).unwrap().to_owned();
                        let status = *routes.get(host.as_str()).expect("unexpected upstream host");
                        let hold = hold_primary && host == PRIMARY;
                        seen.push(Request { host, target });
                        if hold {
                            // Do not await this socket: mirrors must be accepted while it hangs.
                            held_by_task.lock().unwrap().push(socket);
                            continue;
                        }
                        let Some(status) = status else {
                            continue;
                        };
                        let body = if status == 206 {
                            let (start, end) = range
                                .strip_prefix("bytes=")
                                .unwrap()
                                .split_once('-')
                                .unwrap();
                            &data[start.parse::<usize>().unwrap()..=end.parse::<usize>().unwrap()]
                        } else {
                            &[][..]
                        };
                        let mut reply = format!(
                            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        ).into_bytes();
                        reply.extend_from_slice(body);
                        socket.write_all(&reply).await.unwrap();
                    }
                    seen
                });
                Self {
                    addr,
                    http,
                    held,
                    stop: Some(stop),
                    task,
                }
            }

            pub(crate) fn url(&self, host: &str, query: &str) -> Url {
                Url::parse(&format!(
                    "http://{host}:{}/videoplayback?{query}",
                    self.addr.port()
                ))
                .unwrap()
            }

            pub(crate) fn held_connections(&self) -> usize {
                self.held.lock().unwrap().len()
            }

            pub(crate) async fn finish(mut self) -> Vec<Request> {
                self.stop.take().unwrap().send(()).unwrap();
                (&mut self.task).await.unwrap()
            }
        }

        impl Drop for MirrorHost {
            fn drop(&mut self) {
                // Also clean up when a test panics or its outer timeout cancels finish().
                self.task.abort();
                self.held.lock().unwrap().clear();
            }
        }
    }

    #[tokio::test]
    async fn probe_recovers_on_a_live_mirror_without_changing_the_signed_query() {
        use mirror_fixture::{LIVE, MirrorHost, PRIMARY, REFUSED};
        tokio::time::timeout(Duration::from_secs(10), async {
            let host = MirrorHost::start(
                &[(PRIMARY, None), (REFUSED, Some(403)), (LIVE, Some(206))],
                std::sync::Arc::new(vec![42]),
            ).await;
            let query = "itag=251&mn=sn-primary%2Csn-refused%2Csn-live&mvi=1&sig=A%2fb%2BC%3D&n=a+b&dup=1&dup=2";
            let mut url = host.url(PRIMARY, query);
            probe_first_byte(&host.http, &mut url).await.unwrap();
            assert_eq!(url, host.url(LIVE, query));

            let seen = host.finish().await;
            assert_eq!(
                seen.iter().map(|r| r.host.as_str()).collect::<Vec<_>>(),
                [PRIMARY, REFUSED, LIVE]
            );
            for request in seen {
                assert_eq!(request.target, format!("/videoplayback?{query}"));
            }
        }).await.expect("mirror probe did not finish");
    }

    #[tokio::test]
    async fn probe_does_not_hide_primary_expiry_by_trying_a_mirror() {
        use mirror_fixture::{LIVE, MirrorHost, PRIMARY};
        tokio::time::timeout(Duration::from_secs(10), async {
            for status in [403, 410] {
                let host = MirrorHost::start(
                    &[(PRIMARY, Some(status)), (LIVE, Some(206))],
                    std::sync::Arc::new(vec![42]),
                )
                .await;
                let original = host.url(PRIMARY, "mn=sn-primary,sn-live&sig=expired");
                let mut url = original.clone();
                let error = probe_first_byte(&host.http, &mut url).await.unwrap_err();
                assert!(error.to_string().contains(&status.to_string()), "{error:#}");
                assert_eq!(url, original);
                let seen = host.finish().await;
                assert_eq!(seen.len(), 1, "{seen:?}");
                assert_eq!(seen[0].host, PRIMARY);
            }
        })
        .await
        .expect("expiry probe did not finish");
    }

    #[test]
    fn video_id_validates() {
        assert!(VideoId::new("dQw4w9WgXcQ").is_ok());
        assert!(VideoId::new("short").is_err());
        assert!(VideoId::new("has spaces!").is_err());
        assert!(VideoId::new("waytoolongvideoid").is_err());
    }

    /// A mirror is the same URL on the next node in `mn=` — host rebuilt, everything else
    /// (path, query, signature) intact — and a URL with no `mn=` has no mirror to rebuild on.
    #[test]
    fn a_mirror_is_the_same_url_on_the_next_node() {
        let primary = Url::parse(
            "https://rr1---sn-8pxuuxahv1-h2hz.googlevideo.com/videoplayback?\
             itag=251&mn=sn-8pxuuxahv1-h2hz,sn-8xgp1vo-ab56&mvi=0&sig=ABCD",
        )
        .unwrap();
        let mirrored = mirror_url(&primary, "sn-8xgp1vo-ab56").unwrap();
        assert_eq!(
            mirrored.host_str(),
            Some("rr1---sn-8xgp1vo-ab56.googlevideo.com")
        );
        assert_eq!(mirrored.path(), "/videoplayback");
        assert_eq!(mirrored.query(), primary.query());
        assert_eq!(mn_mirrors(&primary), vec!["sn-8xgp1vo-ab56".to_owned()]);
        // A URL already playing from the second node mirrors back to the first, whichever
        // way `mvi` points.
        let on_second = mirror_url(&primary, "sn-8xgp1vo-ab56").unwrap();
        assert_eq!(
            mn_mirrors(&on_second),
            vec!["sn-8pxuuxahv1-h2hz".to_owned()]
        );

        let plain = Url::parse("https://rr1.googlevideo.com/videoplayback?itag=251").unwrap();
        assert_eq!(mn_mirrors(&plain), Vec::<String>::new());
        assert!(mirror_url(&plain, "sn-8xgp1vo-ab56").is_none());
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
