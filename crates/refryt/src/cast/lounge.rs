//! The Lounge: YouTube's relay between a cast receiver (a "screen") and the phones casting to
//! it ("remotes").
//!
//! Nothing travels between the phone and this computer directly once they are paired. Both hold
//! a long-lived HTTP channel to `youtube.com/api/lounge/bc/bind` (Google's BrowserChannel): the
//! screen long-polls it for what the remotes say (`setPlaylist`, `pause`, `seekTo`…) and posts
//! what it is doing (`nowPlaying`, `onStateChange`…) back. That is why a paired phone keeps
//! working from another network, and why none of this needs an account: a screen is whoever
//! holds its lounge token.
//!
//! Undocumented, like the rest of what refryt talks to. The shapes here were read off the wire
//! and checked against [yt-cast-receiver](https://github.com/patrickkfkan/yt-cast-receiver),
//! the Node receiver this follows.
//!
//! One [`Session`] is one screen. It owns two tasks: a reader that keeps the long-poll open and
//! re-establishes the session when YouTube forgets it, and a writer that posts outgoing messages
//! in order. Both report to the receiver through one channel, so the receiver never awaits the
//! network.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, mpsc};

const BASE: &str = "https://www.youtube.com/api/lounge";
const BIND: &str = "https://www.youtube.com/api/lounge/bc/bind";

/// How long a request that is not the long-poll may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// The long-poll's own watchdog: YouTube sends a `noop` well inside this while the channel is
/// alive, so a silence this long is a dead connection that nothing closed (a suspend, a NAT that
/// forgot it).
const POLL_SILENCE: Duration = Duration::from_secs(150);
/// A lounge token lasts two weeks; the session is rebuilt with a fresh one this often when
/// YouTube names no interval of its own.
const TOKEN_REFRESH: Duration = Duration::from_secs(13 * 24 * 3600);
/// The waits between attempts to (re)establish a session, the last repeating. A launch with no
/// network keeps trying quietly rather than giving up for the run.
const RETRY: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// Which app's senders a screen serves. YouTube and YouTube Music cast to different screens,
/// told apart by the `theme` both sides bind with; the DIAL launch names the one it wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Theme {
    /// YouTube Music: what this app is a client for, and the screen that is always up.
    Music,
    /// The YouTube app. Its screen is only bound once that app asks for it.
    Video,
}

impl Theme {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Theme::Music => "m",
            Theme::Video => "cl",
        }
    }

    pub(crate) fn parse(code: &str) -> Option<Self> {
        match code {
            "m" => Some(Theme::Music),
            "cl" => Some(Theme::Video),
            _ => None,
        }
    }
}

/// A message from the lounge: `[aid, [name, payload]]` on the wire.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Incoming {
    /// The channel's own running number for it, echoed back so the server knows what arrived.
    pub(crate) aid: i64,
    pub(crate) name: String,
    /// An object for most messages; `Null` for the ones that carry nothing (`pause`, `next`).
    pub(crate) payload: Value,
}

impl Incoming {
    /// A string field of the payload; empty when it is missing or not a string.
    pub(crate) fn text(&self, key: &str) -> &str {
        self.payload
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// A message to the lounge: a name and its form fields.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Outgoing {
    pub(crate) name: &'static str,
    pub(crate) fields: Vec<(&'static str, String)>,
}

impl Outgoing {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            fields: Vec::new(),
        }
    }

    pub(crate) fn with(mut self, key: &'static str, value: impl Into<String>) -> Self {
        self.fields.push((key, value.into()));
        self
    }
}

/// What a session tells the receiver.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// The screen is bound and listening. Carries its id, to be kept for the next launch: a
    /// phone that paired once finds the same screen again.
    Up {
        theme: Theme,
        screen_id: String,
    },
    /// The session is gone and is being rebuilt; nothing sent meanwhile arrives.
    Down {
        theme: Theme,
        error: String,
    },
    Message {
        theme: Theme,
        message: Incoming,
    },
}

/// Who this screen is, to YouTube and to the phones that list it.
#[derive(Clone, Debug)]
pub(crate) struct Identity {
    /// The name a paired phone shows for this screen.
    pub(crate) name: String,
    /// This install's own id, the same for every screen it binds.
    pub(crate) device_id: String,
}

/// The app name the screen binds as. A label, not a credential: the lounge accepts any.
const SCREEN_APP: &str = "refryt";

/// The bound channel: what every request after the bind has to carry.
struct Channel {
    token: String,
    sid: String,
    gsessionid: String,
    /// The highest message number received, so a reconnecting poll resumes after it.
    aid: i64,
    /// The channel's request counter.
    rid: u32,
    /// How many messages have been posted on this channel.
    ofs: u32,
}

struct Shared {
    theme: Theme,
    identity: Identity,
    http: reqwest::Client,
    /// The screen id, once YouTube issued one (or the stored one was accepted).
    screen_id: std::sync::Mutex<Option<String>>,
    channel: Mutex<Option<Channel>>,
    /// Rung by the writer when a post was refused in a way a new session fixes.
    broken: Notify,
    events: mpsc::UnboundedSender<SessionEvent>,
}

/// One screen and the two tasks that keep it bound. Dropping it ends both.
pub(crate) struct Session {
    shared: Arc<Shared>,
    out: mpsc::UnboundedSender<Vec<Outgoing>>,
    reader: tokio::task::JoinHandle<()>,
    writer: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
        self.writer.abort();
    }
}

impl Session {
    /// Start binding a screen for `theme`, reusing `screen_id` when one was kept. Returns at
    /// once; [`SessionEvent::Up`] says when it is listening.
    pub(crate) fn start(
        theme: Theme,
        identity: Identity,
        screen_id: Option<String>,
        http: reqwest::Client,
        events: mpsc::UnboundedSender<SessionEvent>,
    ) -> Self {
        let shared = Arc::new(Shared {
            theme,
            identity,
            http,
            screen_id: std::sync::Mutex::new(screen_id),
            channel: Mutex::new(None),
            broken: Notify::new(),
            events,
        });
        let (out, queue) = mpsc::unbounded_channel();
        let reader = tokio::spawn(Arc::clone(&shared).read());
        let writer = tokio::spawn(Arc::clone(&shared).write(queue));
        Self {
            shared,
            out,
            reader,
            writer,
        }
    }

    /// Queue messages for the lounge. Posted in order; dropped while the session is down, where
    /// the receiver says everything again once it is back.
    pub(crate) fn send(&self, messages: Vec<Outgoing>) {
        if !messages.is_empty() {
            let _ = self.out.send(messages);
        }
    }

    /// Post `messages` now and wait for it, for the goodbye at shutdown: the queue's task may
    /// never run again once the app is closing.
    pub(crate) async fn send_now(&self, messages: &[Outgoing]) -> Result<()> {
        self.shared.post(messages).await
    }

    /// Tie a pairing code to this screen, which is how a DIAL launch finds it: the phone made
    /// the code up, handed it over on the local network, and now asks YouTube which screen
    /// claimed it. The future owns what it needs, to be run on a task of its own.
    pub(crate) fn register_pairing_code(
        &self,
        code: String,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        let shared = Arc::clone(&self.shared);
        async move { shared.register_pairing_code(&code).await }
    }
}

const FORM: &str = "application/x-www-form-urlencoded";

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Why a long-poll ended.
enum PollEnd {
    /// The server closed it, as it does every few minutes: open the next one.
    Closed,
    /// The session is gone (an unknown SID, an expired token): bind again.
    Session(String),
    /// The network failed: the session may still be there.
    Network(String),
}

impl Shared {
    async fn register_pairing_code(&self, code: &str) -> Result<()> {
        let screen_id = self
            .screen_id
            .lock()
            .unwrap()
            .clone()
            .context("the screen is not bound yet")?;
        let body = form(&[
            ("access_type", "permanent"),
            ("app", SCREEN_APP),
            ("pairing_code", code),
            ("screen_id", &screen_id),
            ("screen_name", &self.identity.name),
            ("device_id", &self.identity.device_id),
        ]);
        let response = self
            .http
            .post(format!("{BASE}/pairing/register_pairing_code"))
            .header(reqwest::header::CONTENT_TYPE, FORM)
            .body(body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("could not reach YouTube to register the pairing code")?;
        if !response.status().is_success() {
            bail!("YouTube refused the pairing code ({})", response.status());
        }
        Ok(())
    }

    /// The reader's whole life: bind, poll until the session is lost, bind again.
    async fn read(self: Arc<Self>) {
        let mut failures = 0usize;
        loop {
            let refresh = match self.establish().await {
                Ok(refresh) => refresh,
                Err(e) => {
                    self.down(format!("{e:#}"));
                    tokio::time::sleep(RETRY[failures.min(RETRY.len() - 1)]).await;
                    failures += 1;
                    continue;
                }
            };
            let established = Instant::now();
            let mut network_failures = 0usize;
            loop {
                let end = tokio::select! {
                    end = self.poll() => end,
                    () = self.broken.notified() => PollEnd::Session("a post was refused".to_owned()),
                    () = tokio::time::sleep(refresh.saturating_sub(established.elapsed())) => {
                        tracing::debug!(theme = self.theme.code(), "cast: refreshing the lounge token");
                        break;
                    }
                };
                match end {
                    PollEnd::Closed => network_failures = 0,
                    PollEnd::Session(error) => {
                        tracing::debug!(theme = self.theme.code(), %error, "cast: the lounge session ended");
                        self.down(error);
                        break;
                    }
                    PollEnd::Network(error) => {
                        tracing::debug!(theme = self.theme.code(), %error, "cast: the lounge poll failed");
                        // A few quick retries on the same session, then a new one: after a long
                        // outage the old SID is gone anyway, and binding again says so sooner.
                        network_failures += 1;
                        if network_failures > 3 {
                            self.down(error);
                            break;
                        }
                        tokio::time::sleep(RETRY[network_failures - 1]).await;
                    }
                }
            }
            *self.channel.lock().await = None;
            // A session that was refused straight after it was bound will be refused again:
            // wait as for a bind that failed, instead of asking in a loop.
            if established.elapsed() < Duration::from_secs(20) {
                tokio::time::sleep(RETRY[failures.min(RETRY.len() - 1)]).await;
                failures += 1;
            } else {
                failures = 0;
            }
        }
    }

    fn down(&self, error: String) {
        let _ = self.events.send(SessionEvent::Down {
            theme: self.theme,
            error,
        });
    }

    /// Get a screen id and a token, bind the channel, and hand the receiver what the bind
    /// answered with. Returns how long the token is good for.
    async fn establish(&self) -> Result<Duration> {
        let stored = self.screen_id.lock().unwrap().clone();
        let (screen_id, token) = match stored {
            Some(id) => match self.lounge_token(&id).await {
                Ok(token) => (id, token),
                // The kept id may have been retired: a fresh one costs a request, and only a
                // phone that paired by TV code would notice the difference.
                Err(e) => {
                    tracing::debug!(error = %format!("{e:#}"), "cast: the kept screen id was refused");
                    let id = self.generate_screen_id().await?;
                    let token = self.lounge_token(&id).await?;
                    (id, token)
                }
            },
            None => {
                let id = self.generate_screen_id().await?;
                let token = self.lounge_token(&id).await?;
                (id, token)
            }
        };
        *self.screen_id.lock().unwrap() = Some(screen_id.clone());

        let mut query = self.common(&token.token);
        let rid = first_rid();
        query.push(("deviceInfo", device_info()));
        query.push(("RID", rid.to_string()));
        query.push(("CVER", "1".to_owned()));
        let url = url::Url::parse_with_params(BIND, &query).context("bad bind URL")?;
        let response = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, FORM)
            .body(form(&[("count", "0")]))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("could not reach the lounge")?;
        let status = response.status();
        if !status.is_success() {
            bail!("the lounge refused the screen ({status})");
        }
        let mut body = response
            .bytes()
            .await
            .context("the lounge stopped mid-answer")?
            .to_vec();
        let messages = drain_messages(&mut body);

        let mut sid = None;
        let mut gsessionid = None;
        let mut aid = 0;
        for message in &messages {
            aid = aid.max(message.aid);
            match message.name.as_str() {
                "c" => sid = message.payload.as_str().map(str::to_owned),
                "S" => gsessionid = message.payload.as_str().map(str::to_owned),
                _ => {}
            }
        }
        let (Some(sid), Some(gsessionid)) = (sid, gsessionid) else {
            bail!("the lounge's answer named no session");
        };
        *self.channel.lock().await = Some(Channel {
            token: token.token,
            sid,
            gsessionid,
            aid,
            rid: rid + 1,
            ofs: 0,
        });
        tracing::info!(theme = self.theme.code(), "cast: the screen is bound");
        let _ = self.events.send(SessionEvent::Up {
            theme: self.theme,
            screen_id,
        });
        self.forward(messages);
        Ok(token.refresh)
    }

    fn forward(&self, messages: Vec<Incoming>) {
        for message in messages {
            // The channel's own bookkeeping, already read.
            if matches!(message.name.as_str(), "c" | "S" | "noop") {
                continue;
            }
            let _ = self.events.send(SessionEvent::Message {
                theme: self.theme,
                message,
            });
        }
    }

    async fn generate_screen_id(&self) -> Result<String> {
        let response = self
            .http
            .get(format!("{BASE}/pairing/generate_screen_id"))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("could not reach YouTube for a screen id")?;
        if !response.status().is_success() {
            bail!("YouTube issued no screen id ({})", response.status());
        }
        let id = response.text().await.context("unreadable screen id")?;
        let id = id.trim();
        if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("YouTube's screen id does not look like one");
        }
        Ok(id.to_owned())
    }

    async fn lounge_token(&self, screen_id: &str) -> Result<Token> {
        let response = self
            .http
            .post(format!("{BASE}/pairing/get_lounge_token_batch"))
            .header(reqwest::header::CONTENT_TYPE, FORM)
            .body(form(&[("screen_ids", screen_id)]))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("could not reach YouTube for a lounge token")?;
        if !response.status().is_success() {
            bail!("YouTube issued no lounge token ({})", response.status());
        }
        let body = response.bytes().await.context("unreadable lounge token")?;
        let json: Value = serde_json::from_slice(&body).context("unreadable lounge token")?;
        parse_token(&json).context("the answer carried no lounge token")
    }

    /// The fields every bind request carries.
    fn common(&self, token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("device", "LOUNGE_SCREEN".to_owned()),
            ("id", self.identity.device_id.clone()),
            ("obfuscatedGaiaId", String::new()),
            ("name", self.identity.name.clone()),
            ("app", SCREEN_APP.to_owned()),
            ("theme", self.theme.code().to_owned()),
            ("capabilities", "dsp,mic,dpa,ntb".to_owned()),
            ("cst", "m".to_owned()),
            ("mdxVersion", "2".to_owned()),
            ("loungeIdToken", token.to_owned()),
            ("VER", "8".to_owned()),
            ("v", "2".to_owned()),
            ("zx", zx()),
            ("t", "1".to_owned()),
        ]
    }

    /// One long-poll: open it, forward what arrives, return why it ended.
    async fn poll(&self) -> PollEnd {
        let url = {
            let guard = self.channel.lock().await;
            let Some(channel) = guard.as_ref() else {
                return PollEnd::Session("no channel".to_owned());
            };
            let mut query = self.common(&channel.token);
            query.push(("RID", "rpc".to_owned()));
            query.push(("SID", channel.sid.clone()));
            query.push(("CI", "0".to_owned()));
            query.push(("AID", channel.aid.to_string()));
            query.push(("gsessionid", channel.gsessionid.clone()));
            query.push(("TYPE", "xmlhttp".to_owned()));
            match url::Url::parse_with_params(BIND, &query) {
                Ok(url) => url,
                Err(e) => return PollEnd::Session(format!("bad poll URL: {e}")),
            }
        };
        let mut response = match self.http.get(url).send().await {
            Ok(response) => response,
            Err(e) => return PollEnd::Network(describe(&e)),
        };
        let status = response.status();
        if !status.is_success() {
            // 400 is an unknown SID, 401/403 a token that expired, 410 a session closed from
            // the other side. Every one of them means the same thing here.
            return if status.is_server_error() {
                PollEnd::Network(format!("the lounge answered {status}"))
            } else {
                PollEnd::Session(format!("the lounge answered {status}"))
            };
        }
        let mut buf = Vec::new();
        loop {
            let chunk = match tokio::time::timeout(POLL_SILENCE, response.chunk()).await {
                Ok(Ok(Some(chunk))) => chunk,
                Ok(Ok(None)) => return PollEnd::Closed,
                Ok(Err(e)) => return PollEnd::Network(describe(&e)),
                Err(_) => return PollEnd::Network("the lounge went silent".to_owned()),
            };
            buf.extend_from_slice(&chunk);
            let messages = drain_messages(&mut buf);
            if messages.is_empty() {
                continue;
            }
            if let Some(channel) = self.channel.lock().await.as_mut() {
                for message in &messages {
                    channel.aid = channel.aid.max(message.aid);
                }
            }
            self.forward(messages);
        }
    }

    /// The writer's whole life: post each batch in turn, folding in whatever queued up behind it
    /// while the last post was on the wire.
    async fn write(self: Arc<Self>, mut queue: mpsc::UnboundedReceiver<Vec<Outgoing>>) {
        while let Some(mut batch) = queue.recv().await {
            while let Ok(more) = queue.try_recv() {
                batch.extend(more);
            }
            if let Err(e) = self.post(&batch).await {
                tracing::debug!(
                    theme = self.theme.code(),
                    error = %format!("{e:#}"),
                    "cast: could not post to the lounge"
                );
            }
        }
    }

    /// Post `messages` on the bound channel.
    ///
    /// The channel numbers its requests, and the server takes them in order: one that never
    /// arrived holds up every one after it. So the numbers only move on once a post has landed,
    /// a post the network dropped is sent again under the same number, and one that still does
    /// not land ends the session rather than leave a gap in it.
    async fn post(&self, messages: &[Outgoing]) -> Result<()> {
        let (url, body, sid) = {
            let guard = self.channel.lock().await;
            let Some(channel) = guard.as_ref() else {
                bail!("the session is down");
            };
            let mut query = self.common(&channel.token);
            query.push(("deviceInfo", device_info()));
            query.push(("SID", channel.sid.clone()));
            query.push(("RID", channel.rid.to_string()));
            query.push(("AID", channel.aid.to_string()));
            query.push(("gsessionid", channel.gsessionid.clone()));
            let url = url::Url::parse_with_params(BIND, &query).context("bad post URL")?;
            (url, encode(messages, channel.ofs), channel.sid.clone())
        };
        let mut failure = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            let sent = self
                .http
                .post(url.clone())
                .header(reqwest::header::CONTENT_TYPE, FORM)
                .body(body.clone())
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await;
            match sent {
                Ok(response) if response.status().is_success() => {
                    // Only if the session is still the one this was posted on.
                    if let Some(channel) = self.channel.lock().await.as_mut()
                        && channel.sid == sid
                    {
                        channel.rid += 1;
                        channel.ofs += messages.len() as u32;
                    }
                    return Ok(());
                }
                // Refused: the session is gone, and sending it again changes nothing.
                Ok(response) if response.status().is_client_error() => {
                    failure = format!("the lounge answered {}", response.status());
                    break;
                }
                Ok(response) => failure = format!("the lounge answered {}", response.status()),
                Err(e) => failure = describe(&e),
            }
        }
        // The poll may sit on a dead session for minutes before it notices. Only a reader that
        // is polling hears this: one already rebinding needs no telling.
        self.broken.notify_waiters();
        bail!("{failure}");
    }
}

/// A request error without its URL, which carries the lounge token in its query: whoever holds
/// that can drive the screen, so it stays out of a log people paste into bug reports.
fn describe(error: &reqwest::Error) -> String {
    let mut text = if error.is_connect() {
        "could not connect".to_owned()
    } else if error.is_timeout() {
        "timed out".to_owned()
    } else if error.is_body() || error.is_decode() {
        "the connection broke mid-answer".to_owned()
    } else {
        "the request failed".to_owned()
    };
    // The cause underneath says which it was (a reset, a DNS failure) and names no URL.
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        if inner.source().is_none() {
            text = format!("{text}: {inner}");
        }
        source = inner.source();
    }
    text
}

struct Token {
    token: String,
    refresh: Duration,
}

/// `{"screens":[{"loungeToken":…,"refreshIntervalInMillis":…}]}`.
fn parse_token(json: &Value) -> Option<Token> {
    let screen = json.get("screens")?.get(0)?;
    let token = screen.get("loungeToken")?.as_str()?.to_owned();
    if token.is_empty() {
        return None;
    }
    let refresh = screen
        .get("refreshIntervalInMillis")
        .and_then(Value::as_u64)
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
        // Never past the default: a token outlives it by a day, and a wrong figure from the
        // server must not leave the screen on an expired one.
        .map_or(TOKEN_REFRESH, |d| d.min(TOKEN_REFRESH));
    Some(Token { token, refresh })
}

/// What the screen says about itself in the bind. The lounge passes some of it on to the
/// phone; none of it is checked.
fn device_info() -> String {
    serde_json::json!({
        "brand": "refryt",
        "model": "Desktop",
        "year": 0,
        "os": std::env::consts::OS,
        "osVersion": "",
        "chipset": "",
        "clientName": "TVHTML5",
        "dialAdditionalDataSupportLevel": "unsupported",
        "mdxDialServerType": "MDX_DIAL_SERVER_TYPE_UNKNOWN",
    })
    .to_string()
}

/// The form body of a post: `count`, `ofs`, then each message as `req<i>__sc=<name>` and
/// `req<i>_<field>=<value>`.
fn encode(messages: &[Outgoing], ofs: u32) -> String {
    let mut body = url::form_urlencoded::Serializer::new(String::new());
    body.append_pair("count", &messages.len().to_string());
    body.append_pair("ofs", &ofs.to_string());
    for (i, message) in messages.iter().enumerate() {
        body.append_pair(&format!("req{i}__sc"), message.name);
        for (key, value) in &message.fields {
            body.append_pair(&format!("req{i}_{key}"), value);
        }
    }
    body.finish()
}

/// Take every complete frame out of `buf`, leaving a partial one for the next read.
///
/// The channel frames its stream as a decimal length on a line of its own, then a JSON array of
/// `[aid, [name, payload]]` messages. The length is not used: it counts characters where the
/// socket hands over bytes, and the two differ as soon as a phone's name has an accent in it.
/// The JSON says where it ends on its own.
pub(crate) fn drain_messages(buf: &mut Vec<u8>) -> Vec<Incoming> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        while buf.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let Some(line) = buf[at..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let body = at + line + 1;
        if line == 0 || !buf[at..at + line].iter().all(u8::is_ascii_digit) {
            // Not a length: skip the line rather than stall on it forever.
            at = body;
            continue;
        }
        let mut frames = serde_json::Deserializer::from_slice(&buf[body..]).into_iter::<Value>();
        match frames.next() {
            Some(Ok(frame)) => {
                at = body + frames.byte_offset();
                out.extend(messages_of(&frame));
            }
            // The rest of the frame has not arrived yet.
            Some(Err(e)) if e.is_eof() => break,
            Some(Err(_)) => at = body,
            None => break,
        }
    }
    buf.drain(..at);
    out
}

fn messages_of(frame: &Value) -> Vec<Incoming> {
    let Some(items) = frame.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let aid = item.get(0)?.as_i64()?;
            let message = item.get(1)?.as_array()?;
            let name = message.first()?.as_str()?.to_owned();
            let payload = message.get(1).cloned().unwrap_or(Value::Null);
            Some(Incoming { aid, name, payload })
        })
        .collect()
}

/// A random value from the standard library's own hashing keys, as `refryt-resolve` does for
/// its proxy's secret: no dependency for a handful of ids.
pub(crate) fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

/// A random version-4 UUID.
pub(crate) fn uuid() -> String {
    let (a, b) = (random_u64(), random_u64());
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        0x8000 | ((b >> 48) & 0x3fff),
        b & 0xffff_ffff_ffff,
    )
}

/// The cache-busting value every request carries.
fn zx() -> String {
    format!("{:012x}", random_u64() & 0xffff_ffff_ffff)
}

/// Where a channel's request counter starts.
fn first_rid() -> u32 {
    41_000 + (random_u64() % 9_000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bind's own answer, as captured: three messages on three lines inside one frame.
    #[test]
    fn a_frame_is_read_into_its_messages() {
        let mut buf = br#"118
[[0,["c","71FA2850D2B1A11C","",8]]
,[1,["S","vj8XHacm0xJJ"]]
,[2,["loungeStatus",{"queueId":"RQeH48"}]]
,[3,["getNowPlaying"]]
]
"#
        .to_vec();
        let messages = drain_messages(&mut buf);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].name, "c");
        assert_eq!(messages[0].payload, "71FA2850D2B1A11C");
        assert_eq!(messages[1].payload, "vj8XHacm0xJJ");
        assert_eq!(messages[2].text("queueId"), "RQeH48");
        assert_eq!(messages[3].aid, 3);
        assert_eq!(messages[3].payload, Value::Null);
        assert!(buf.is_empty(), "nothing may be left of a whole frame");
    }

    /// The socket cuts a frame wherever it likes, the length line included.
    #[test]
    fn a_frame_split_across_reads_waits_for_its_end() {
        let whole = b"2\n[[7,[\"pause\"]]]\n33\n[[8,[\"seekTo\",{\"newTime\":\"42\"}]]]\n";
        for cut in 1..whole.len() {
            let mut buf = whole[..cut].to_vec();
            let mut messages = drain_messages(&mut buf);
            buf.extend_from_slice(&whole[cut..]);
            messages.extend(drain_messages(&mut buf));
            let names: Vec<&str> = messages.iter().map(|m| m.name.as_str()).collect();
            assert_eq!(names, ["pause", "seekTo"], "cut at {cut}");
            assert_eq!(messages[1].text("newTime"), "42", "cut at {cut}");
        }
    }

    /// The length counts characters, so a frame with an accent in it is longer in bytes than it
    /// says. Reading the JSON to its own end is what keeps the next frame intact.
    #[test]
    fn a_frame_longer_in_bytes_than_its_length_says_still_ends_where_it_ends() {
        let mut buf =
            "44\n[[4,[\"remoteConnected\",{\"name\":\"Teléfono\"}]]]\n17\n[[5,[\"pause\"]]]\n"
                .as_bytes()
                .to_vec();
        let messages = drain_messages(&mut buf);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text("name"), "Teléfono");
        assert_eq!(messages[1].name, "pause");
    }

    #[test]
    fn a_line_that_is_no_frame_is_skipped() {
        let mut buf = b"Unknown SID\n17\n[[5,[\"pause\"]]]\n".to_vec();
        let messages = drain_messages(&mut buf);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].name, "pause");
    }

    #[test]
    fn a_post_numbers_its_messages_from_zero_and_carries_the_offset() {
        let body = encode(
            &[
                Outgoing::new("nowPlaying").with("videoId", "dQw4w9WgXcQ"),
                Outgoing::new("onStateChange").with("state", "1"),
            ],
            5,
        );
        assert_eq!(
            body,
            "count=2&ofs=5&req0__sc=nowPlaying&req0_videoId=dQw4w9WgXcQ\
             &req1__sc=onStateChange&req1_state=1"
        );
    }

    #[test]
    fn a_token_is_read_with_its_refresh_capped() {
        let json = serde_json::json!({"screens": [{
            "screenId": "abc", "loungeToken": "tok", "refreshIntervalInMillis": 60_000u64,
        }]});
        let token = parse_token(&json).unwrap();
        assert_eq!(token.token, "tok");
        assert_eq!(token.refresh, Duration::from_secs(60));

        let long = serde_json::json!({"screens": [{
            "loungeToken": "tok", "refreshIntervalInMillis": 9_999_999_999_999u64,
        }]});
        assert_eq!(parse_token(&long).unwrap().refresh, TOKEN_REFRESH);
        assert!(parse_token(&serde_json::json!({"screens": []})).is_none());
    }

    #[test]
    fn a_uuid_has_the_shape_of_one() {
        let id = uuid();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(id, uuid());
    }

    #[test]
    fn themes_round_trip_through_their_codes() {
        for theme in [Theme::Music, Theme::Video] {
            assert_eq!(Theme::parse(theme.code()), Some(theme));
        }
        assert_eq!(Theme::parse("x"), None);
    }
}
