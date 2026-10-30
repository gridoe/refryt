//! The first resolver in the chain: YouTube's own `player` endpoint, asked as a guest.
//!
//! Borrowed from Sonora's `ytmusic-rs` (2026-09): as the VISIONOS client, `player` answers with
//! direct stream URLs — no signature cipher, no `n` challenge, no PO token — so there is no
//! JavaScript to run and nothing to spawn. One HTTPS round trip, ~0.4 s warm, against yt-dlp's
//! ~8 s and ~380 MB (Python plus the Deno it starts for the player challenges).
//!
//! YouTube's "Sign in to confirm you're not a bot" is tied to the *visitor* as much as to the
//! address: the refusal itself carries a fresh `visitorData`, and asking again as that visitor
//! goes through (measured 2026-09-23 on an address where every anonymous yt-dlp run was
//! refused). The visitor is kept, and saved when a path is given, so the handshake is paid once
//! rather than on every launch.
//!
//! These URLs are throttled when fetched in one unranged GET, which is how mpv asks; the streams
//! this returns are marked [`AudioStream::needs_ranged`] and played through
//! [`crate::StreamProxy`], which fetches them in ranges.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use url::Url;

use crate::{AudioStream, Codec, StreamResolver, VideoId, parse_expiry, score};

const CLIENT_NAME: &str = "VISIONOS";
const CLIENT_ID: &str = "101";
const CLIENT_VERSION: &str = "1.02";
/// Sent with the `player` request and, by the proxy, with every range of the stream itself.
pub const GUEST_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) \
AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const API_BASE: &str = "https://www.youtube.com";

/// Which of the offered audio streams to take, mirroring the Audio Quality setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuestQuality {
    /// The best stream: Opus over AAC, then bitrate.
    #[default]
    Best,
    /// The best stream at or under this many kbps, else the smallest.
    AtMost(u32),
    /// The smallest stream of at least 48 kbps, else the smallest.
    Smallest,
}

impl GuestQuality {
    /// Read the yt-dlp selector the app already keeps for the setting, so one value drives both
    /// resolvers.
    pub fn from_ytdlp_format(format: &str) -> Self {
        if format.starts_with("worstaudio") {
            Self::Smallest
        } else if let Some(rest) = format.split("abr<=").nth(1) {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
                .map_or(Self::Best, Self::AtMost)
        } else {
            Self::Best
        }
    }
}

pub struct GuestResolver {
    http: reqwest::Client,
    base: String,
    visitor: Mutex<Option<String>>,
    visitor_file: Option<PathBuf>,
    quality: Mutex<GuestQuality>,
    /// Ask the stream for its first byte before handing it on; see [`Self::probe`].
    probe: bool,
}

impl GuestResolver {
    /// `http` is shared with the stream proxy, so the connections a resolve opens are the ones
    /// the first ranges of the stream then reuse.
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            base: API_BASE.to_owned(),
            visitor: Mutex::new(None),
            visitor_file: None,
            quality: Mutex::new(GuestQuality::default()),
            probe: true,
        }
    }

    /// Keep the visitor across launches in `path`, loading whatever is already there.
    pub fn with_visitor_file(mut self, path: PathBuf) -> Self {
        if let Ok(saved) = std::fs::read_to_string(&path) {
            let saved = saved.trim();
            if !saved.is_empty() {
                *self.visitor.lock().unwrap() = Some(saved.to_owned());
            }
        }
        self.visitor_file = Some(path);
        self
    }

    /// Tests point the requests at a local server whose canned streams live on googlevideo, so
    /// the first-byte check is off unless a test turns it back on.
    #[cfg(test)]
    fn with_base(mut self, base: &str) -> Self {
        self.probe = false;
        base.clone_into(&mut self.base);
        self
    }

    pub fn set_quality(&self, quality: GuestQuality) {
        if let Ok(mut q) = self.quality.lock() {
            *q = quality;
        }
    }

    fn visitor(&self) -> Option<String> {
        self.visitor.lock().ok()?.clone()
    }

    fn adopt_visitor(&self, visitor: &str) {
        if let Ok(mut held) = self.visitor.lock() {
            *held = Some(visitor.to_owned());
        }
        if let Some(path) = &self.visitor_file
            && let Err(e) = std::fs::write(path, visitor)
        {
            tracing::debug!(error = %e, "guest: could not save the visitor");
        }
    }

    /// Ask the stream for its first byte, as the proxy will ask for its ranges. The media server
    /// now and then refuses a URL `player` has just handed out; found here, the chain still
    /// falls through to yt-dlp, where found in mpv the song would simply fail. The connection it
    /// opens is the one the proxy's first range then reuses, as for the `player` request.
    async fn probe(&self, url: &Url) -> Result<()> {
        let response = self
            .http
            .get(url.as_str())
            .header("Range", "bytes=0-0")
            .header("User-Agent", GUEST_USER_AGENT)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .context("the stream did not answer")?;
        match response.status().as_u16() {
            200 | 206 => Ok(()),
            status => bail!("the stream answered {status}"),
        }
    }

    async fn player(&self, id: &VideoId, visitor: Option<&str>) -> Result<Value> {
        let mut request = self
            .http
            .post(format!(
                "{}/youtubei/v1/player?prettyPrint=false&alt=json",
                self.base
            ))
            .header("Content-Type", "application/json")
            .header("Origin", "https://www.youtube.com")
            .header("User-Agent", GUEST_USER_AGENT)
            .header("X-Youtube-Client-Name", CLIENT_ID)
            .header("X-Youtube-Client-Version", CLIENT_VERSION)
            .timeout(Duration::from_secs(15))
            .body(player_body(id.as_str(), visitor).to_string());
        if let Some(visitor) = visitor {
            request = request.header("X-Goog-Visitor-Id", visitor);
        }
        let response = request.send().await.context("cannot reach YouTube")?;
        let status = response.status();
        if !status.is_success() {
            bail!("the guest player request failed with {status}");
        }
        let body = response
            .bytes()
            .await
            .context("the guest player response was cut short")?;
        serde_json::from_slice(&body).context("the guest player response is not JSON")
    }
}

#[async_trait::async_trait]
impl StreamResolver for GuestResolver {
    fn name(&self) -> &'static str {
        "guest"
    }

    async fn resolve(&self, id: &VideoId) -> Result<AudioStream> {
        let held = self.visitor();
        let mut response = self.player(id, held.as_deref()).await?;
        if status(&response).0 == "LOGIN_REQUIRED"
            && let Some(issued) = response
                .pointer("/responseContext/visitorData")
                .and_then(Value::as_str)
                .filter(|issued| Some(*issued) != held.as_deref())
                .map(str::to_owned)
        {
            let retried = self.player(id, Some(&issued)).await?;
            // Kept only when it worked: an age gate also answers LOGIN_REQUIRED, and no visitor
            // fixes that, so a good visitor must not be traded away for it.
            if status(&retried).0 == "OK" {
                tracing::info!("guest: YouTube refused the visitor; now using the one it issued");
                self.adopt_visitor(&issued);
            }
            response = retried;
        }
        let (status, reason) = status(&response);
        if status != "OK" {
            // Worded like yt-dlp's refusals, so the chain's breaker treats a gated or missing
            // video as that video's problem rather than this resolver's.
            bail!(
                "guest player for {id}: {status}: {}",
                reason.unwrap_or_default()
            );
        }
        let quality = self.quality.lock().map(|q| *q).unwrap_or_default();
        let stream =
            pick(&response, quality).with_context(|| format!("no direct audio stream for {id}"))?;
        if self.probe {
            self.probe(&stream.url)
                .await
                .with_context(|| format!("guest stream for {id}"))?;
        }
        Ok(stream)
    }
}

/// The request body: the video and the VISIONOS context, as Sonora's `context.rs` sends it.
fn player_body(video_id: &str, visitor: Option<&str>) -> Value {
    let mut client = json!({
        "hl": "en",
        "gl": "US",
        "clientName": CLIENT_NAME,
        "clientVersion": CLIENT_VERSION,
        "userAgent": GUEST_USER_AGENT,
        "deviceMake": "Apple",
        "deviceModel": "RealityDevice17,1",
        "osName": "visionOS",
        "osVersion": "26.5.23O471",
        "utcOffsetMinutes": 0,
    });
    if let Some(visitor) = visitor {
        client["visitorData"] = json!(visitor);
    }
    json!({
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
        "playbackContext": {
            "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" }
        },
        "context": {
            "client": client,
            "user": { "lockedSafetyMode": false },
            "request": { "useSsl": true },
        },
    })
}

fn status(response: &Value) -> (&str, Option<&str>) {
    let playability = response.get("playabilityStatus");
    (
        playability
            .and_then(|p| p.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN"),
        playability
            .and_then(|p| p.get("reason"))
            .and_then(Value::as_str),
    )
}

/// One offered audio stream with a direct URL and a known length (the proxy needs the length to
/// answer ranges; a stream without one is skipped rather than played unranged and throttled).
struct Offer {
    stream: AudioStream,
}

fn offers(response: &Value) -> Vec<Offer> {
    let loudness_db = loudness_above_target(response);
    let Some(formats) = response
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    formats
        .iter()
        .filter_map(|format| {
            let mime = format.get("mimeType")?.as_str()?;
            if !mime.starts_with("audio/") {
                return None;
            }
            // A `signatureCipher` stream needs the player JS; this path takes only direct URLs.
            let url = Url::parse(format.get("url")?.as_str()?).ok()?;
            let content_length = format.get("contentLength")?.as_str()?.parse::<u64>().ok()?;
            let codec = Codec::from_codec_str(mime);
            let bitrate = format.get("bitrate").and_then(Value::as_u64).unwrap_or(0);
            Some(Offer {
                stream: AudioStream {
                    itag: format.get("itag").and_then(Value::as_u64).unwrap_or(0) as u32,
                    codec,
                    bitrate_kbps: (bitrate / 1000) as u32,
                    content_length: Some(content_length),
                    duration: format
                        .get("approxDurationMs")
                        .and_then(Value::as_str)
                        .and_then(|ms| ms.parse().ok())
                        .map(Duration::from_millis),
                    expires_at: parse_expiry(&url),
                    loudness_db,
                    mime: Some(mime.split(';').next().unwrap_or(mime).trim().to_owned()),
                    needs_ranged: true,
                    url,
                    resolved_by: "guest",
                },
            })
        })
        .collect()
}

/// How many dB the track sits above YouTube's loudness target — what `loudness_db` means to the
/// player, which turns it down (or up) by that much. The web clients send it as `loudnessDb`;
/// VISIONOS sends the track's absolute loudness and the target instead (2026-09-23).
fn loudness_above_target(response: &Value) -> Option<f32> {
    let audio = response.pointer("/playerConfig/audioConfig")?;
    let number = |key: &str| audio.get(key).and_then(Value::as_f64);
    let db = number("loudnessDb")
        .or_else(|| Some(number("trackAbsoluteLoudnessLkfs")? - number("loudnessTargetLkfs")?))?;
    Some(db as f32)
}

fn pick(response: &Value, quality: GuestQuality) -> Option<AudioStream> {
    let offers = offers(response);
    let best = |candidates: &mut dyn Iterator<Item = &Offer>| {
        candidates
            .max_by_key(|o| score(o.stream.codec, o.stream.bitrate_kbps))
            .map(|o| o.stream.clone())
    };
    let smallest = |floor: u32| {
        offers
            .iter()
            .filter(|o| o.stream.bitrate_kbps >= floor)
            .min_by_key(|o| o.stream.bitrate_kbps)
            .or_else(|| offers.iter().min_by_key(|o| o.stream.bitrate_kbps))
            .map(|o| o.stream.clone())
    };
    match quality {
        GuestQuality::Best => best(&mut offers.iter()),
        GuestQuality::AtMost(kbps) => {
            best(&mut offers.iter().filter(|o| o.stream.bitrate_kbps <= kbps))
                .or_else(|| smallest(0))
        }
        GuestQuality::Smallest => smallest(48),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a real VISIONOS answer (2026-09-23), trimmed: two Opus and one AAC stream
    /// with direct URLs, one ciphered stream, and a video stream.
    fn response() -> Value {
        let format = |itag: u64, mime: &str, bitrate: u64, url: bool| {
            let mut f = json!({
                "itag": itag,
                "mimeType": mime,
                "bitrate": bitrate,
                "contentLength": "2900000",
                "approxDurationMs": "180000",
            });
            if url {
                f["url"] = json!(format!(
                    "https://rr1.googlevideo.com/videoplayback?itag={itag}&expire=4102444800"
                ));
            } else {
                f["signatureCipher"] = json!("s=abc&url=https%3A%2F%2Fx");
            }
            f
        };
        json!({
            "playabilityStatus": { "status": "OK" },
            "playerConfig": { "audioConfig": {
                "trackAbsoluteLoudnessLkfs": -7.97,
                "loudnessTargetLkfs": -14.0,
            } },
            "streamingData": { "adaptiveFormats": [
                format(137, "video/mp4; codecs=\"avc1\"", 4_000_000, true),
                format(140, "audio/mp4; codecs=\"mp4a.40.2\"", 131_081, true),
                format(250, "audio/webm; codecs=\"opus\"", 85_257, true),
                format(251, "audio/webm; codecs=\"opus\"", 164_789, true),
                format(999, "audio/webm; codecs=\"opus\"", 400_000, false),
            ]}
        })
    }

    #[test]
    fn picks_the_best_direct_opus_and_carries_what_the_player_needs() {
        let s = pick(&response(), GuestQuality::Best).unwrap();
        assert_eq!(s.itag, 251, "the ciphered 400 kbps stream is not reachable");
        assert_eq!(s.codec, Codec::Opus);
        assert_eq!(s.content_length, Some(2_900_000));
        // 6 dB above the -14 LKFS target, as VISIONOS reports it.
        assert!(
            (s.loudness_db.unwrap() - 6.03).abs() < 0.01,
            "{:?}",
            s.loudness_db
        );
        assert_eq!(
            loudness_above_target(
                &json!({ "playerConfig": { "audioConfig": { "loudnessDb": -3.5 } } })
            ),
            Some(-3.5),
            "the web clients' own figure wins"
        );
        assert_eq!(s.mime.as_deref(), Some("audio/webm"));
        assert!(s.needs_ranged);
        assert!(s.expires_at.is_some());
    }

    #[test]
    fn quality_follows_the_setting() {
        assert_eq!(
            pick(&response(), GuestQuality::AtMost(128)).unwrap().itag,
            250
        );
        assert_eq!(pick(&response(), GuestQuality::Smallest).unwrap().itag, 250);
        assert_eq!(
            GuestQuality::from_ytdlp_format("bestaudio[acodec=opus]/bestaudio"),
            GuestQuality::Best
        );
        assert_eq!(
            GuestQuality::from_ytdlp_format("bestaudio[abr<=128]/bestaudio"),
            GuestQuality::AtMost(128)
        );
        assert_eq!(
            GuestQuality::from_ytdlp_format("worstaudio[abr>=48]/worstaudio/bestaudio"),
            GuestQuality::Smallest
        );
    }

    #[test]
    fn the_body_is_the_visionos_context_with_the_visitor() {
        let body = player_body("dQw4w9WgXcQ", Some("CgtWSVM"));
        assert_eq!(body["videoId"], "dQw4w9WgXcQ");
        assert_eq!(body["context"]["client"]["clientName"], "VISIONOS");
        assert_eq!(body["context"]["client"]["clientVersion"], "1.02");
        assert_eq!(body["context"]["client"]["visitorData"], "CgtWSVM");
        assert!(
            player_body("dQw4w9WgXcQ", None)["context"]["client"]
                .get("visitorData")
                .is_none()
        );
    }

    /// A minimal HTTP server answering each connection with the next canned body, recording
    /// the requests it saw.
    async fn fake_youtube(bodies: Vec<Value>) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        fake_server(|_| bodies.into_iter().map(|b| (200, b)).collect()).await
    }

    /// [`fake_youtube`] with a status per reply, the replies built from the server's own address
    /// so a canned stream URL can point back at it.
    async fn fake_server(
        replies: impl FnOnce(&str) -> Vec<(u16, Value)>,
    ) -> (String, std::sync::Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let replies = replies(&base);
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        tokio::spawn(async move {
            for (code, body) in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 8192];
                // Headers, then the body by its Content-Length.
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).to_string());
                let body = body.to_string();
                let reply = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    /// YouTube's bot check on a fresh visitor: the refusal's own `visitorData` is adopted, saved,
    /// and the request goes through as that visitor.
    #[tokio::test]
    async fn a_refused_visitor_is_replaced_by_the_one_youtube_issues() {
        let refused = json!({
            "playabilityStatus": { "status": "LOGIN_REQUIRED", "reason": "Sign in to confirm you’re not a bot" },
            "responseContext": { "visitorData": "ISSUED" },
        });
        let (base, seen) = fake_youtube(vec![refused, response()]).await;
        let file = std::env::temp_dir().join(format!("refryt-visitor-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let guest = GuestResolver::new(reqwest::Client::new())
            .with_base(&base)
            .with_visitor_file(file.clone());

        let stream = guest
            .resolve(&VideoId::new("dQw4w9WgXcQ").unwrap())
            .await
            .unwrap();
        assert_eq!(stream.itag, 251);
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert!(
            !requests[0]
                .to_ascii_lowercase()
                .contains("x-goog-visitor-id")
        );
        assert!(requests[1].contains("ISSUED"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "ISSUED");

        // A new resolver, as after a restart, starts as the saved visitor.
        let restarted = GuestResolver::new(reqwest::Client::new()).with_visitor_file(file.clone());
        assert_eq!(restarted.visitor().as_deref(), Some("ISSUED"));
        let _ = std::fs::remove_file(&file);
    }

    /// A stream the media server refuses is an error the chain falls through on; one that hands
    /// out its first byte goes through, asked for as a range.
    #[tokio::test]
    async fn a_refused_stream_is_found_before_it_is_handed_on() {
        for (code, plays) in [(403, false), (206, true)] {
            let (base, seen) = fake_server(|base| {
                let mut answer = response();
                for f in answer["streamingData"]["adaptiveFormats"]
                    .as_array_mut()
                    .unwrap()
                {
                    if let Some(url) = f["url"].as_str() {
                        f["url"] = json!(url.replace("https://rr1.googlevideo.com", base));
                    }
                }
                vec![(200, answer), (code, json!({}))]
            })
            .await;
            let mut guest = GuestResolver::new(reqwest::Client::new()).with_base(&base);
            guest.probe = true;
            let stream = guest.resolve(&VideoId::new("dQw4w9WgXcQ").unwrap()).await;
            assert_eq!(stream.is_ok(), plays, "{code}: {stream:?}");
            let requests = seen.lock().unwrap().clone();
            assert!(
                requests[1]
                    .to_ascii_lowercase()
                    .contains("range: bytes=0-0"),
                "{code}"
            );
        }
    }

    /// Anything but OK is an error the chain can fall through on, worded so a gate stays the
    /// video's problem.
    #[tokio::test]
    async fn an_unplayable_answer_is_an_error() {
        let gone =
            json!({ "playabilityStatus": { "status": "ERROR", "reason": "Video unavailable" } });
        let (base, _) = fake_youtube(vec![gone]).await;
        let guest = GuestResolver::new(reqwest::Client::new()).with_base(&base);
        let err = guest
            .resolve(&VideoId::new("dQw4w9WgXcQ").unwrap())
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("Video unavailable"), "{err:#}");
    }
}
