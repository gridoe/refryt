//! The cast receiver: this computer as a device in YouTube Music's Cast menu.
//!
//! With it on, a phone on the same network lists refryt next to the TVs and speakers, and what
//! it casts plays here: the phone's queue becomes this queue, and its play, pause, seek, skip
//! and volume drive this player. It goes both ways — what is played or paused here shows on the
//! phone — so the two are one player with two remotes, which is the point: there is no second
//! "cast mode" beside the app's own.
//!
//! Three parts, each in its own file or half of this one:
//!
//! - [`dial`] makes the computer findable on the local network and takes the phone's request to
//!   pair.
//! - [`lounge`] is the channel to the phone, relayed by YouTube.
//! - The receiver, below, sits between that channel and the app. It is a translator and keeps
//!   no player state of its own: the app tells it what is playing ([`Snapshot`]), it says so to
//!   the phones; a phone says what to do, it tells the app ([`CastEvent`]). The one thing it
//!   does itself is turn the video ids a phone sends into tracks the queue can show.
//!
//! Like Discord's presence, the whole thing runs off the UI thread behind a handle ([`Cast`])
//! and exists only while its setting is on. Unlike it, this listens on the network, which is
//! why the setting is off until someone turns it on.

mod dial;
mod lounge;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::api::Api;
use crate::innertube::QueueFilter;
use crate::model::{ArtistLink, ChartTrend, LikeStatus, Track, WatchTarget};
use lounge::{Incoming, Outgoing, Session, SessionEvent, Theme};

/// How far the reported position may drift from where the phone thinks the song is before it is
/// told again. The phone counts on by itself while a song plays, so only a jump — a seek, a
/// stall — needs saying.
const DRIFT: f64 = 1.5;
/// How many songs the phone named but YouTube's queue did not describe are looked up one by
/// one, nearest the playing one first. Past this they sit in the queue untitled and still play.
const LOOKUPS: usize = 24;
/// How many of those lookups run at once.
const LOOKUPS_AT_ONCE: usize = 4;
/// How many tracks' details are kept between queue changes, so reordering a queue or picking
/// another song from it asks YouTube nothing.
const KNOWN_TRACKS: usize = 4000;
/// How long shutting down waits to tell the phones, at most.
const GOODBYE: Duration = Duration::from_millis(1500);

/// What the player is doing, as the lounge numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    /// Nothing is loaded.
    Idle,
    Playing,
    Paused,
    /// A song is on its way: resolving, or loading into the player.
    Loading,
}

impl PlayState {
    fn code(self) -> &'static str {
        match self {
            PlayState::Idle => "-1",
            PlayState::Playing => "1",
            PlayState::Paused => "2",
            PlayState::Loading => "3",
        }
    }
}

/// What the app is playing, as much of it as a phone shows. The app hands one over whenever it
/// [differs](Self::differs_from) from the last.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// `None` when nothing is loaded, or for a local file, which a phone could not show.
    pub video_id: Option<String>,
    pub state: PlayState,
    /// Seconds.
    pub position: f64,
    pub duration: f64,
    /// What is heard, 0 to 100: zero while muted.
    pub volume: u8,
    pub muted: bool,
    pub has_previous: bool,
    pub has_next: bool,
    /// Where the song is in the app's queue.
    pub index: Option<usize>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            video_id: None,
            state: PlayState::Idle,
            position: 0.0,
            duration: 0.0,
            volume: 0,
            muted: false,
            has_previous: false,
            has_next: false,
            index: None,
        }
    }
}

impl Snapshot {
    /// Whether a phone shown `last`, `elapsed` ago, would now be showing something wrong.
    /// Anything but the position counts at once; the position only once it is not where a clock
    /// started at `last` would have put it.
    pub fn differs_from(&self, last: &Snapshot, elapsed: Duration) -> bool {
        if self.video_id != last.video_id
            || self.state != last.state
            || self.volume != last.volume
            || self.muted != last.muted
            || self.has_previous != last.has_previous
            || self.has_next != last.has_next
            || self.index != last.index
        {
            return true;
        }
        self.jumped_from(last, elapsed)
    }

    /// Whether the song is somewhere other than a clock started at `last`, `elapsed` ago, would
    /// have it: a seek, a stall, or a length that was not known yet.
    fn jumped_from(&self, last: &Snapshot, elapsed: Duration) -> bool {
        let expected = if last.state == PlayState::Playing {
            last.position + elapsed.as_secs_f64()
        } else {
            last.position
        };
        (self.position - expected).abs() > DRIFT || (self.duration - last.duration).abs() >= 1.0
    }
}

/// A queue a phone asked for, as tracks.
#[derive(Debug)]
pub struct CastQueue {
    pub tracks: Vec<Track>,
    /// The track to play.
    pub index: usize,
    /// Where in it to start, in seconds: a phone handing over a song it was already playing.
    pub position: f64,
    /// The token that continues the list, when the phone named one of YouTube's radios rather
    /// than its own songs. The queue then flows on as a mix started here would.
    pub more: Option<String>,
    pub filters: Vec<QueueFilter>,
    /// The phone changed its queue under the song playing (added one, removed one, moved one)
    /// rather than choosing a song: nothing restarts.
    pub update: bool,
}

/// What the receiver tells the app.
#[derive(Debug)]
pub enum CastEvent {
    Status(Status),
    /// The phones connected changed. `names` is all of them now.
    Senders {
        names: Vec<String>,
        joined: Vec<String>,
        left: Vec<String>,
        /// The ones that left did so by their own Disconnect, rather than by falling asleep or
        /// off the network: the phone carries on playing by itself, so this should stop.
        explicit: bool,
    },
    Queue(Box<CastQueue>),
    Play,
    Pause,
    Stop,
    SeekTo(f64),
    Next,
    Previous,
    /// `volume` is 0 to 100. `muted` only from a sender that has a mute of its own; the phone
    /// apps turn the volume to zero instead.
    SetVolume {
        volume: f64,
        muted: Option<bool>,
    },
}

/// Where the receiver stands, for Settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Off,
    /// Binding its screen with YouTube.
    Connecting,
    /// Listed in the Cast menu.
    Ready,
    /// Not listed, and why. It keeps trying.
    Failed(String),
}

enum Command {
    Publish(Snapshot),
    /// Say goodbye and stop. The sender, when there is one, hears when that is done.
    Shutdown(Option<std::sync::mpsc::Sender<()>>),
}

/// The running receiver. Dropping it stops it.
pub struct Cast {
    commands: mpsc::UnboundedSender<Command>,
    name: String,
}

impl Cast {
    /// Start the receiver on `runtime`. `emit` is called from there with everything it has to
    /// say, and must wake the app.
    pub(crate) fn spawn(
        runtime: &tokio::runtime::Handle,
        api: Arc<Api>,
        emit: impl Fn(CastEvent) + Send + Sync + 'static,
    ) -> Self {
        let name = device_name();
        let (commands, inbox) = mpsc::unbounded_channel();
        let receiver = Receiver::new(api, name.clone(), Store::load(), Box::new(emit));
        runtime.spawn(receiver.run(inbox));
        Self { commands, name }
    }

    /// The name phones list this computer under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tell the phones what is playing. Cheap: a send on a channel.
    pub fn publish(&self, snapshot: Snapshot) {
        let _ = self.commands.send(Command::Publish(snapshot));
    }

    /// Stop, waiting up to `wait` for the phones and the network to be told. For the app's own
    /// exit, where the runtime is about to go and a plain drop would leave the phone showing a
    /// device that is no longer there.
    pub fn close(self, wait: Duration) {
        let (done, finished) = std::sync::mpsc::channel();
        if self.commands.send(Command::Shutdown(Some(done))).is_ok() {
            let _ = finished.recv_timeout(wait);
        }
    }
}

impl Drop for Cast {
    fn drop(&mut self) {
        // Not waited for: turning the setting off must not hold the UI thread. The task says
        // its goodbyes on the runtime, which is still there.
        let _ = self.commands.send(Command::Shutdown(None));
    }
}

/// "Refryt (hostname)", so two computers on one network can be told apart in the menu.
fn device_name() -> String {
    match host_name() {
        Some(host) => format!("Refryt ({host})"),
        None => "Refryt".to_owned(),
    }
}

/// The machine's name, without a dependency for it: each platform has one place to ask.
fn host_name() -> Option<String> {
    #[cfg(target_os = "linux")]
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
    #[cfg(target_os = "windows")]
    let name = std::env::var("COMPUTERNAME").ok();
    #[cfg(target_os = "macos")]
    let name = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    let name: Option<String> = None;
    name.map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty() && n.len() <= 64)
}

// ---------------------------------------------------------------------------------------------
// What is kept between launches
// ---------------------------------------------------------------------------------------------

/// The receiver's identity, in `cast.json`. Kept so the computer is the same device every
/// launch: a phone lists it once, and one that paired before finds the screen it knows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Store {
    /// The DIAL device's UUID.
    #[serde(default)]
    uuid: String,
    /// The lounge device id.
    #[serde(default)]
    device_id: String,
    /// The screen id YouTube issued for each theme, by its code.
    #[serde(default)]
    screens: BTreeMap<String, String>,
}

impl Store {
    fn path() -> Option<std::path::PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "refryt")?;
        Some(dirs.config_dir().join("cast.json"))
    }

    /// The kept identity, made up where there is none yet.
    fn load() -> Self {
        let mut store: Store = Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if store.uuid.is_empty() {
            store.uuid = lounge::uuid();
        }
        if store.device_id.is_empty() {
            store.device_id = lounge::uuid();
        }
        store
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = crate::persist::write_json_atomic(&path, &bytes) {
            tracing::debug!(error = %e, "cast: could not keep the device's identity");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The receiver
// ---------------------------------------------------------------------------------------------

/// A phone connected to one of the screens.
#[derive(Clone, Debug, PartialEq)]
struct Sender {
    id: String,
    name: String,
    theme: Theme,
}

impl Sender {
    /// One entry of `loungeStatus`'s device list, when it is a remote.
    fn parse(device: &Value, theme: Theme) -> Option<Self> {
        if device.get("type")?.as_str()? != "REMOTE_CONTROL" {
            return None;
        }
        let id = device.get("id")?.as_str()?.to_owned();
        let name = ["name", "clientName"]
            .iter()
            .filter_map(|key| device.get(*key)?.as_str())
            .find(|name| !name.trim().is_empty())
            .unwrap_or("A phone")
            .trim()
            .to_owned();
        Some(Self { id, name, theme })
    }
}

/// The phone's queue as it last sent it: what `nowPlaying` places the song in.
#[derive(Clone, Debug, PartialEq)]
struct List {
    id: String,
    ids: Vec<String>,
}

/// What came with the song a phone chose, to hand back with it: `params` shapes what YouTube
/// plays after it, `ctt` is the phone's leave to play a private upload.
#[derive(Clone, Debug, Default, PartialEq)]
struct Context {
    video_id: String,
    params: Option<String>,
    ctt: Option<String>,
}

/// A `setPlaylist` or `updatePlaylist`, read.
#[derive(Clone, Debug, PartialEq)]
struct ListRequest {
    update: bool,
    list_id: Option<String>,
    /// The song to play. `None` for an update, which names none.
    video_id: Option<String>,
    /// The queue, in order. Empty when the phone named a list and left its contents to YouTube.
    ids: Vec<String>,
    index: Option<usize>,
    position: f64,
    params: Option<String>,
    ctt: Option<String>,
}

impl ListRequest {
    /// Read a playlist message. `None` when it names nothing at all: the phone cleared its queue.
    fn parse(message: &Incoming, update: bool) -> Option<Self> {
        let text = |key: &str| {
            let value = message.text(key).trim();
            (!value.is_empty()).then(|| value.to_owned())
        };
        let ids: Vec<String> = message
            .text("videoIds")
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect();
        let (list_id, video_id) = (text("listId"), text("videoId"));
        if list_id.is_none() && video_id.is_none() && ids.is_empty() {
            return None;
        }
        Some(Self {
            update,
            list_id,
            video_id,
            ids,
            // "-1" from a phone that leaves the place to the lounge.
            index: message.text("currentIndex").trim().parse().ok(),
            position: message
                .text("currentTime")
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|t| t.is_finite() && *t > 0.0)
                .unwrap_or(0.0),
            params: text("params"),
            ctt: text("ctt"),
        })
    }

    /// This update folded into a song choice still being looked up, so the choice is not lost
    /// to it: the phone's newer list, the same song.
    fn onto(self, chosen: &ListRequest) -> ListRequest {
        ListRequest {
            update: false,
            list_id: self.list_id.or_else(|| chosen.list_id.clone()),
            ids: self.ids,
            index: None,
            ..chosen.clone()
        }
    }
}

/// A [`ListRequest`] with its tracks found.
struct Resolved {
    generation: u64,
    request: ListRequest,
    ids: Vec<String>,
    tracks: Vec<Track>,
    index: usize,
    more: Option<String>,
    filters: Vec<QueueFilter>,
    /// The second word on the same request: the details that took a lookup each, for a queue
    /// the app already has.
    late: bool,
}

type Known = Arc<Mutex<HashMap<String, Track>>>;

/// Who is connected and what they have been told: the half of the receiver that is plain data,
/// so the rules for what gets said, and when, are tested without a network.
#[derive(Default)]
struct Room {
    senders: Vec<Sender>,
    list: Option<List>,
    context: Context,
    /// What the app last said it is playing, and so what the phones were last told.
    latest: Snapshot,
    /// When it said so.
    told: Option<Instant>,
    /// The playback's id in YouTube's own statistics, which `nowPlaying` carries.
    cpn: String,
}

/// What a roll call changed.
#[derive(Debug, PartialEq)]
struct RollCall {
    joined: Vec<String>,
    left: Vec<String>,
    explicit: bool,
}

struct Receiver {
    api: Arc<Api>,
    http: reqwest::Client,
    emit: Box<dyn Fn(CastEvent) + Send + Sync>,
    name: String,
    store: Store,
    sessions: HashMap<Theme, Session>,
    /// The screens bound right now.
    up: HashSet<Theme>,
    dial: Option<dial::Dial>,
    status: Status,
    room: Room,
    /// Tracks already described, by video id.
    known: Known,
    /// Counts playlist messages, so a lookup that a newer one overtook is dropped.
    generation: u64,
    /// A song choice whose tracks are still being found.
    choosing: Option<ListRequest>,
    /// Launches waiting for their screen to be bound.
    launches: Vec<(Theme, String, oneshot::Sender<bool>)>,
    session_events: mpsc::UnboundedSender<SessionEvent>,
    resolved: mpsc::UnboundedSender<Resolved>,
    inbox: Option<Inbox>,
}

/// The receiving halves, taken by [`Receiver::run`].
struct Inbox {
    session_events: mpsc::UnboundedReceiver<SessionEvent>,
    resolved: mpsc::UnboundedReceiver<Resolved>,
}

impl Receiver {
    fn new(
        api: Arc<Api>,
        name: String,
        store: Store,
        emit: Box<dyn Fn(CastEvent) + Send + Sync>,
    ) -> Self {
        let (session_events, session_inbox) = mpsc::unbounded_channel();
        let (resolved, resolved_inbox) = mpsc::unbounded_channel();
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .connect_timeout(Duration::from_secs(10))
            // The long-poll is one quiet stream: without a ping, a connection that died in a
            // suspend or behind a router that forgot it looks alive until the watchdog.
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(15))
            .http2_keep_alive_while_idle(true)
            .build()
            .unwrap_or_default();
        Self {
            api,
            http,
            emit,
            name,
            store,
            sessions: HashMap::new(),
            up: HashSet::new(),
            dial: None,
            status: Status::Off,
            room: Room {
                cpn: format!("{:016x}", lounge::random_u64()),
                ..Room::default()
            },
            known: Known::default(),
            generation: 0,
            choosing: None,
            launches: Vec::new(),
            session_events,
            resolved,
            inbox: Some(Inbox {
                session_events: session_inbox,
                resolved: resolved_inbox,
            }),
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let Some(mut inbox) = self.inbox.take() else {
            return;
        };
        let (launches, mut launched) = mpsc::unbounded_channel();
        self.set_status(Status::Connecting);
        self.ensure_session(Theme::Music);
        // Housekeeping: discovery that could not start is tried again, and launches nobody is
        // waiting on any more are forgotten.
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::Publish(snapshot)) => self.on_snapshot(snapshot),
                    Some(Command::Shutdown(done)) => {
                        self.goodbye().await;
                        if let Some(done) = done {
                            let _ = done.send(());
                        }
                        return;
                    }
                    None => {
                        self.goodbye().await;
                        return;
                    }
                },
                Some(event) = inbox.session_events.recv() => {
                    self.on_session_event(event, &launches).await;
                }
                Some(launch) = launched.recv() => self.on_launch(launch),
                Some(resolved) = inbox.resolved.recv() => self.on_resolved(resolved),
                _ = tick.tick() => {
                    self.launches.retain(|(_, _, reply)| !reply.is_closed());
                    if self.dial.is_none() && self.up.contains(&Theme::Music) {
                        self.start_dial(&launches).await;
                    }
                }
            }
        }
    }

    fn say(&self, event: CastEvent) {
        (self.emit)(event);
    }

    fn set_status(&mut self, status: Status) {
        if self.status != status {
            self.status = status.clone();
            self.say(CastEvent::Status(status));
        }
    }

    /// Bind the screen for `theme`, if it is not already bound or on its way.
    fn ensure_session(&mut self, theme: Theme) {
        if self.sessions.contains_key(&theme) {
            return;
        }
        let identity = lounge::Identity {
            name: self.name.clone(),
            device_id: self.store.device_id.clone(),
        };
        let session = Session::start(
            theme,
            identity,
            self.store.screens.get(theme.code()).cloned(),
            self.http.clone(),
            self.session_events.clone(),
        );
        self.sessions.insert(theme, session);
    }

    async fn start_dial(&mut self, launches: &mpsc::UnboundedSender<dial::Launch>) {
        match dial::Dial::start(self.store.uuid.clone(), self.name.clone(), launches.clone()).await
        {
            Ok(dial) => {
                self.dial = Some(dial);
                self.set_status(Status::Ready);
            }
            Err(e) => {
                let error = format!("{e:#}");
                tracing::warn!(%error, "cast: could not become discoverable");
                self.set_status(Status::Failed(error));
            }
        }
    }

    async fn on_session_event(
        &mut self,
        event: SessionEvent,
        launches: &mpsc::UnboundedSender<dial::Launch>,
    ) {
        match event {
            SessionEvent::Up { theme, screen_id } => {
                self.up.insert(theme);
                if self.store.screens.get(theme.code()) != Some(&screen_id) {
                    self.store
                        .screens
                        .insert(theme.code().to_owned(), screen_id);
                    let store = self.store.clone();
                    tokio::task::spawn_blocking(move || store.save());
                }
                if theme == Theme::Music {
                    if self.dial.is_none() {
                        self.start_dial(launches).await;
                    } else {
                        self.set_status(Status::Ready);
                    }
                }
                // Launches that arrived while the screen was binding.
                let waiting = std::mem::take(&mut self.launches);
                for (wanted, code, reply) in waiting {
                    if wanted == theme {
                        self.register(theme, code, reply);
                    } else {
                        self.launches.push((wanted, code, reply));
                    }
                }
                // A rebound screen is a new channel: phones still on it missed whatever was
                // said while it was down.
                if self.room.themes().contains(&theme) {
                    self.send(theme, self.room.everything());
                }
            }
            SessionEvent::Down { theme, error } => {
                self.up.remove(&theme);
                if theme == Theme::Music {
                    tracing::warn!(%error, "cast: lost the connection to YouTube; retrying");
                    self.set_status(Status::Failed(error));
                }
            }
            SessionEvent::Message { theme, message } => self.on_message(theme, message),
        }
    }

    /// A phone asked to pair over the local network.
    fn on_launch(&mut self, launch: dial::Launch) {
        // A launch that names no app is taken for YouTube Music's: it is what this plays.
        let theme = launch
            .theme
            .as_deref()
            .and_then(Theme::parse)
            .unwrap_or(Theme::Music);
        tracing::info!(theme = theme.code(), "cast: a phone asked to pair");
        self.ensure_session(theme);
        if self.up.contains(&theme) {
            self.register(theme, launch.pairing_code, launch.reply);
        } else {
            self.launches
                .push((theme, launch.pairing_code, launch.reply));
        }
    }

    fn register(&self, theme: Theme, code: String, reply: oneshot::Sender<bool>) {
        let Some(session) = self.sessions.get(&theme) else {
            let _ = reply.send(false);
            return;
        };
        let registering = session.register_pairing_code(code);
        tokio::spawn(async move {
            let registered = registering.await;
            if let Err(e) = &registered {
                tracing::warn!(error = %format!("{e:#}"), "cast: could not pair");
            }
            let _ = reply.send(registered.is_ok());
        });
    }

    fn send(&self, theme: Theme, messages: Vec<Outgoing>) {
        if let Some(session) = self.sessions.get(&theme) {
            if tracing::enabled!(tracing::Level::DEBUG) {
                let names: Vec<&str> = messages.iter().map(|m| m.name).collect();
                tracing::debug!(theme = theme.code(), ?names, "cast: to the phone");
            }
            session.send(messages);
        }
    }

    /// Send to every screen that has a phone on it.
    fn broadcast(&self, messages: Vec<Outgoing>) {
        if messages.is_empty() {
            return;
        }
        for theme in self.room.themes() {
            self.send(theme, messages.clone());
        }
    }

    fn on_message(&mut self, theme: Theme, message: Incoming) {
        // The name only: a playlist message carries the phone's leave to play private uploads.
        tracing::debug!(theme = theme.code(), name = %message.name, "cast: from the phone");
        match message.name.as_str() {
            "loungeStatus" => self.on_lounge_status(theme, &message),
            "getNowPlaying" => self.send(theme, vec![self.room.now_playing()]),
            "getVolume" => self.send(theme, vec![self.room.volume_changed()]),
            "setPlaylist" => self.on_playlist(&message, false),
            "updatePlaylist" => self.on_playlist(&message, true),
            "play" => self.say(CastEvent::Play),
            "pause" => self.say(CastEvent::Pause),
            "stopVideo" => self.say(CastEvent::Stop),
            "next" => self.say(CastEvent::Next),
            "previous" => self.say(CastEvent::Previous),
            "seekTo" => {
                if let Some(to) = seconds(message.text("newTime")) {
                    self.say(CastEvent::SeekTo(to));
                }
            }
            "setVolume" => {
                if let Some(volume) = seconds(message.text("volume")) {
                    self.say(CastEvent::SetVolume {
                        volume: volume.clamp(0.0, 100.0),
                        muted: flag(message.payload.get("muted")),
                    });
                }
            }
            // Subtitles, the D-pad, party games and the rest of what a TV answers.
            _ => {}
        }
    }

    /// The lounge's roll call: who is on the screen now, and what just changed.
    fn on_lounge_status(&mut self, theme: Theme, message: &Incoming) {
        let Some(RollCall {
            joined,
            left,
            explicit,
        }) = self.room.roll_call(theme, message)
        else {
            return;
        };
        if !joined.is_empty() {
            tracing::info!(theme = theme.code(), "cast: a phone connected");
            self.send(theme, self.room.everything());
        }
        if !left.is_empty() {
            tracing::info!(theme = theme.code(), explicit, "cast: a phone disconnected");
        }
        self.say(CastEvent::Senders {
            names: self.room.senders.iter().map(|s| s.name.clone()).collect(),
            joined,
            left,
            explicit,
        });
    }

    /// A phone set its queue, or changed it.
    fn on_playlist(&mut self, message: &Incoming, update: bool) {
        self.generation += 1;
        let Some(request) = ListRequest::parse(message, update) else {
            // The queue was cleared: nothing of the phone's is left to play.
            self.room.list = None;
            self.choosing = None;
            self.say(CastEvent::Stop);
            return;
        };
        let request = match (&self.choosing, update) {
            // An update overtaking a choice must not swallow it.
            (Some(chosen), true) => request.onto(chosen),
            _ => request,
        };
        if !request.update {
            self.choosing = Some(request.clone());
        }
        // An update names no song: YouTube is asked about the list around the one playing.
        let anchor = self.room.latest.video_id.clone();
        tokio::spawn(resolve(
            Arc::clone(&self.api),
            Arc::clone(&self.known),
            request,
            anchor,
            self.generation,
            self.resolved.clone(),
        ));
    }

    fn on_resolved(&mut self, resolved: Resolved) {
        // A newer playlist message overtook this one, and answers for it.
        if resolved.generation != self.generation {
            return;
        }
        let request = &resolved.request;
        if !resolved.late {
            self.choosing = None;
        }
        if resolved.tracks.is_empty() {
            return;
        }
        if !resolved.late {
            self.room.list = request.list_id.clone().map(|id| List {
                id,
                ids: resolved.ids.clone(),
            });
            if let Some(video_id) = &request.video_id {
                self.room.context = Context {
                    video_id: video_id.clone(),
                    params: request.params.clone(),
                    ctt: request.ctt.clone(),
                };
            }
        }
        // The song the phone chose is the one already playing: the app leaves it be, so
        // nothing it says next would tell the phone where the song stands. Said here.
        let same_song = !resolved.late
            && !request.update
            && request.video_id.is_some()
            && request.video_id == self.room.latest.video_id;
        self.say(CastEvent::Queue(Box::new(CastQueue {
            tracks: resolved.tracks,
            index: resolved.index,
            position: request.position,
            more: resolved.more,
            filters: resolved.filters,
            update: request.update || resolved.late,
        })));
        if same_song {
            self.broadcast(vec![self.room.now_playing(), self.room.state_changed()]);
        }
    }

    /// The app says what it is playing: tell the phones whatever of it changed.
    fn on_snapshot(&mut self, snapshot: Snapshot) {
        let elapsed = self.room.told.map_or(Duration::ZERO, |at| at.elapsed());
        let previous = std::mem::replace(&mut self.room.latest, snapshot);
        self.room.told = Some(Instant::now());
        if self.room.senders.is_empty() {
            return;
        }
        self.broadcast(self.room.changes(&previous, elapsed));
    }

    /// Leave the network and the phones, briefly: the app may be closing.
    async fn goodbye(&mut self) {
        let leaving = async {
            if let Some(dial) = &self.dial {
                dial.goodbye().await;
            }
            for theme in self.room.themes() {
                if let Some(session) = self.sessions.get(&theme) {
                    let _ = session
                        .send_now(&[Outgoing::new("loungeScreenDisconnected")])
                        .await;
                }
            }
        };
        let _ = tokio::time::timeout(GOODBYE, leaving).await;
        self.dial = None;
        self.sessions.clear();
    }
}

impl Room {
    /// The screens with a phone on them.
    fn themes(&self) -> Vec<Theme> {
        let mut themes: Vec<Theme> = self.senders.iter().map(|s| s.theme).collect();
        themes.sort_by_key(|t| t.code());
        themes.dedup();
        themes
    }

    /// Take the lounge's roll call for `theme`'s screen: who is on it now. `None` when nobody
    /// came or went.
    fn roll_call(&mut self, theme: Theme, message: &Incoming) -> Option<RollCall> {
        let devices: Vec<Value> = serde_json::from_str(message.text("devices")).unwrap_or_default();
        let remotes: Vec<Sender> = devices
            .iter()
            .filter_map(|device| Sender::parse(device, theme))
            .collect();
        let details: Value =
            serde_json::from_str(message.text("connectionEventDetails")).unwrap_or(Value::Null);
        let explicit = details.get("ui").and_then(Value::as_str) == Some("true");

        let joined: Vec<String> = remotes
            .iter()
            .filter(|r| !self.senders.iter().any(|s| s.id == r.id))
            .map(|r| r.name.clone())
            .collect();
        let left: Vec<String> = self
            .senders
            .iter()
            .filter(|s| s.theme == theme && !remotes.iter().any(|r| r.id == s.id))
            .map(|s| s.name.clone())
            .collect();
        self.senders.retain(|s| s.theme != theme);
        self.senders.extend(remotes);
        (!joined.is_empty() || !left.is_empty()).then_some(RollCall {
            joined,
            left,
            explicit,
        })
    }

    /// The messages that bring a phone shown `previous`, `elapsed` ago, up to date.
    fn changes(&self, previous: &Snapshot, elapsed: Duration) -> Vec<Outgoing> {
        let now = &self.latest;
        let track = previous.video_id != now.video_id || previous.index != now.index;
        let state = previous.state != now.state;
        let nav = previous.has_previous != now.has_previous || previous.has_next != now.has_next;
        let volume = previous.volume != now.volume || previous.muted != now.muted;

        let mut out = Vec::new();
        // The position rides `onStateChange`, and is only said for a jump: a volume step while
        // a song plays is not one.
        if track || state || now.jumped_from(previous, elapsed) {
            out.push(self.state_changed());
        }
        // `nowPlaying` is for the song, and for a song starting or coming back. Not for a
        // pause: the lounge delivers it a beat behind `onStateChange`, and one sent for every
        // change of state lands, stale, on top of the change after it.
        if track || (state && previous.state != PlayState::Playing) {
            out.push(self.now_playing());
        }
        if track || nav {
            out.push(self.nav_changed());
        }
        if volume {
            out.push(self.volume_changed());
        }
        out
    }

    /// Where the song is now. The app says where it is only when that changes in a way a phone
    /// could not work out, so between two words a playing song has moved on by the clock: a
    /// phone that arrives in the middle of it, or asks, is told where it has got to.
    fn position(&self) -> f64 {
        let now = &self.latest;
        let since = match (now.state, self.told) {
            (PlayState::Playing, Some(told)) => told.elapsed().as_secs_f64(),
            _ => 0.0,
        };
        let position = now.position + since;
        if now.duration > 0.0 {
            position.min(now.duration)
        } else {
            position
        }
    }

    /// The whole picture, for a phone that just arrived.
    fn everything(&self) -> Vec<Outgoing> {
        vec![
            self.nav_changed(),
            self.now_playing(),
            self.state_changed(),
            self.volume_changed(),
        ]
    }

    fn now_playing(&self) -> Outgoing {
        let now = &self.latest;
        let Some(video_id) = &now.video_id else {
            return Outgoing::new("nowPlaying");
        };
        let mut message = Outgoing::new("nowPlaying")
            .with("videoId", video_id.as_str())
            .with("currentTime", time(self.position()))
            .with("duration", time(now.duration))
            .with("state", now.state.code())
            .with("loadedTime", time(now.duration))
            .with("seekableStartTime", "0")
            .with("seekableEndTime", time(now.duration))
            .with("cpn", self.cpn.as_str());
        if let Some((list_id, index)) = place(self.list.as_ref(), video_id, now.index) {
            message = message
                .with("listId", list_id)
                .with("currentIndex", index.to_string());
        }
        if self.context.video_id == *video_id {
            if let Some(params) = &self.context.params {
                message = message.with("params", params.as_str());
            }
            if let Some(ctt) = &self.context.ctt {
                message = message.with("ctt", ctt.as_str());
            }
        }
        message
    }

    fn state_changed(&self) -> Outgoing {
        let now = &self.latest;
        Outgoing::new("onStateChange")
            .with("state", now.state.code())
            .with("currentTime", time(self.position()))
            .with("duration", time(now.duration))
            .with(
                "loadedTime",
                if now.state == PlayState::Idle {
                    "0".to_owned()
                } else {
                    time(now.duration)
                },
            )
            .with("seekableStartTime", "0")
            .with("seekableEndTime", time(now.duration))
            .with("cpn", self.cpn.as_str())
    }

    fn nav_changed(&self) -> Outgoing {
        Outgoing::new("onHasPreviousNextChanged")
            .with("hasPrevious", self.latest.has_previous.to_string())
            .with("hasNext", self.latest.has_next.to_string())
    }

    fn volume_changed(&self) -> Outgoing {
        Outgoing::new("onVolumeChanged")
            .with("volume", self.latest.volume.to_string())
            .with("muted", self.latest.muted.to_string())
    }
}

/// Where `video_id` sits in the phone's list: at the app's own `index` when the two queues
/// still line up there, which is the right answer for a song the list holds twice; else
/// wherever the list first has it. `None` when the song is not the phone's: one the app's own
/// autoplay added, or one picked here.
fn place<'a>(
    list: Option<&'a List>,
    video_id: &str,
    index: Option<usize>,
) -> Option<(&'a str, usize)> {
    let list = list?;
    let at = index
        .filter(|&i| list.ids.get(i).map(String::as_str) == Some(video_id))
        .or_else(|| list.ids.iter().position(|id| id == video_id))?;
    Some((&list.id, at))
}

/// A number of seconds (or a volume) out of a message's text.
fn seconds(text: &str) -> Option<f64> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// A boolean a sender wrote as one, or as the word.
fn flag(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Seconds as the lounge reads them: a plain number, to the millisecond.
fn time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "0".to_owned();
    }
    let text = format!("{seconds:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

// ---------------------------------------------------------------------------------------------
// From video ids to tracks
// ---------------------------------------------------------------------------------------------

/// Find the tracks a phone asked for and report them, in up to two words: the queue as soon as
/// the song to play is known, and — when some of the rest took a lookup each — once more with
/// those filled in.
///
/// The phone sends video ids. One `next` call on the list it names describes them all, since the
/// phone's queue is a playlist of its own on YouTube's side (`RQ…`); the songs that call missed
/// are looked up singly, the playing one first.
async fn resolve(
    api: Arc<Api>,
    known: Known,
    request: ListRequest,
    anchor: Option<String>,
    generation: u64,
    out: mpsc::UnboundedSender<Resolved>,
) {
    let mut ids = request.ids.clone();
    let mut more = None;
    let mut filters = Vec::new();
    let all_known = |ids: &[String]| {
        let known = known.lock().unwrap();
        !ids.is_empty() && ids.iter().all(|id| known.contains_key(id))
    };
    if !all_known(&ids)
        && let Some(list_id) = &request.list_id
    {
        let target = WatchTarget {
            video_id: request
                .video_id
                .clone()
                .or_else(|| anchor.filter(|a| ids.is_empty() || ids.contains(a))),
            playlist_id: list_id.clone(),
            params: request.params.clone(),
        };
        match api.watch(&target).await {
            Ok(queue) => {
                if ids.is_empty() {
                    // The list is YouTube's to expand: an album, a playlist, a radio.
                    ids = queue.tracks.iter().map(|t| t.video_id.clone()).collect();
                    more = queue.next;
                    filters = queue.filters;
                }
                remember(&known, queue.tracks);
            }
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), "cast: YouTube did not describe the phone's queue");
            }
        }
    }
    // The song to play leads a list that somehow does not hold it.
    if let Some(video_id) = &request.video_id
        && !ids.contains(video_id)
    {
        ids.insert(0, video_id.clone());
    }
    if ids.is_empty() {
        // Still an answer: the receiver is waiting to hear what became of the request.
        let _ = out.send(Resolved {
            generation,
            request,
            ids,
            tracks: Vec::new(),
            index: 0,
            more: None,
            filters: Vec::new(),
            late: false,
        });
        return;
    }
    let index = request
        .index
        .filter(|&i| ids.get(i) == request.video_id.as_ref())
        .or_else(|| {
            let video_id = request.video_id.as_ref()?;
            ids.iter().position(|id| id == video_id)
        })
        .unwrap_or(0);

    // The playing song first and alone: it is what the phone is waiting to hear.
    if !request.update && !known.lock().unwrap().contains_key(&ids[index]) {
        look_up(&api, &known, vec![ids[index].clone()]).await;
    }
    let missing = missing_nearest(&ids, index, &known);
    let report = |late: bool| Resolved {
        generation,
        request: request.clone(),
        tracks: tracks_for(&ids, &known),
        ids: ids.clone(),
        index,
        more: more.clone(),
        filters: filters.clone(),
        late,
    };
    // An update waits for its lookups instead: nothing is waiting to start, and one word
    // moves the queue once.
    if !request.update || missing.is_empty() {
        if out.send(report(false)).is_err() {
            return;
        }
        if missing.is_empty() {
            return;
        }
    }
    look_up(&api, &known, missing).await;
    // For a song choice this is the second word, the details catching up.
    let _ = out.send(report(!request.update));
}

/// The ids nothing is known about, nearest `index` first, as many as will be looked up.
fn missing_nearest(ids: &[String], index: usize, known: &Known) -> Vec<String> {
    let known = known.lock().unwrap();
    let mut missing: Vec<(usize, &String)> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| !known.contains_key(*id))
        .map(|(i, id)| (i.abs_diff(index), id))
        .collect();
    missing.sort_by_key(|(distance, _)| *distance);
    let mut seen = HashSet::new();
    missing
        .into_iter()
        .filter(|(_, id)| seen.insert(*id))
        .take(LOOKUPS)
        .map(|(_, id)| id.clone())
        .collect()
}

/// Ask YouTube about each of `ids` on its own: the song's radio opens with the song itself,
/// described as any queue row is. The same lookup an imported history uses.
async fn look_up(api: &Arc<Api>, known: &Known, ids: Vec<String>) {
    for batch in ids.chunks(LOOKUPS_AT_ONCE) {
        let mut lookups = tokio::task::JoinSet::new();
        for id in batch {
            let (api, id) = (Arc::clone(api), id.clone());
            lookups.spawn(async move {
                let queue = api.mix(&id).await.ok()?;
                queue.tracks.into_iter().find(|t| t.video_id == id)
            });
        }
        let mut found = Vec::new();
        while let Some(joined) = lookups.join_next().await {
            found.extend(joined.ok().flatten());
        }
        remember(known, found);
    }
}

fn remember(known: &Known, tracks: Vec<Track>) {
    let mut known = known.lock().unwrap();
    // Dropped whole rather than trimmed: what is playing is asked about again in one call.
    if known.len() + tracks.len() > KNOWN_TRACKS {
        known.clear();
    }
    for track in tracks {
        known.insert(track.video_id.clone(), track);
    }
}

fn tracks_for(ids: &[String], known: &Known) -> Vec<Track> {
    let known = known.lock().unwrap();
    ids.iter()
        .map(|id| known.get(id).cloned().unwrap_or_else(|| untitled(id)))
        .collect()
}

/// A track nothing is known about but its id. It plays, under the still every video has.
fn untitled(video_id: &str) -> Track {
    Track {
        video_id: video_id.to_owned(),
        title: "Unknown song".to_owned(),
        artists: vec![ArtistLink::plain("YouTube")],
        album: None,
        album_id: None,
        duration: None,
        plays: None,
        explicit: false,
        like: LikeStatus::Indifferent,
        in_library: false,
        library_add: None,
        library_remove: None,
        set_video_id: None,
        thumbnail: Some(Arc::from(format!(
            "https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"
        ))),
        wide: false,
        rank: None,
        trend: ChartTrend::Unknown,
        local_path: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(video_id: &str, position: f64) -> Snapshot {
        Snapshot {
            video_id: Some(video_id.to_owned()),
            state: PlayState::Playing,
            position,
            duration: 200.0,
            volume: 70,
            muted: false,
            has_previous: true,
            has_next: true,
            index: Some(0),
        }
    }

    fn message(name: &str, payload: Value) -> Incoming {
        Incoming {
            aid: 1,
            name: name.to_owned(),
            payload,
        }
    }

    fn room() -> Room {
        Room {
            cpn: "cpn".to_owned(),
            ..Room::default()
        }
    }

    fn names(messages: &[Outgoing]) -> Vec<&'static str> {
        messages.iter().map(|m| m.name).collect()
    }

    fn field<'a>(message: &'a Outgoing, key: &str) -> Option<&'a str> {
        message
            .fields
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// A song playing on needs no telling: the phone counts the seconds itself.
    #[test]
    fn a_song_playing_on_is_not_told_again() {
        let last = playing("a", 10.0);
        assert!(!playing("a", 15.0).differs_from(&last, Duration::from_secs(5)));
        assert!(!playing("a", 15.9).differs_from(&last, Duration::from_secs(5)));
    }

    #[test]
    fn a_seek_or_a_stall_is_told() {
        let last = playing("a", 10.0);
        // A seek: far from where the clock would be.
        assert!(playing("a", 90.0).differs_from(&last, Duration::from_secs(1)));
        // A stall: the clock ran, the song did not.
        assert!(playing("a", 10.0).differs_from(&last, Duration::from_secs(5)));
    }

    #[test]
    fn a_paused_song_is_expected_to_stay_where_it_is() {
        let mut last = playing("a", 10.0);
        last.state = PlayState::Paused;
        let mut now = last.clone();
        assert!(!now.differs_from(&last, Duration::from_secs(600)));
        now.position = 40.0;
        assert!(now.differs_from(&last, Duration::from_secs(600)));
    }

    #[test]
    fn anything_a_phone_shows_is_told_when_it_changes() {
        let last = playing("a", 10.0);
        let at = Duration::ZERO;
        let mut next = last.clone();
        next.video_id = Some("b".to_owned());
        assert!(next.differs_from(&last, at));
        let mut paused = last.clone();
        paused.state = PlayState::Paused;
        assert!(paused.differs_from(&last, at));
        let mut quieter = last.clone();
        quieter.volume = 40;
        assert!(quieter.differs_from(&last, at));
        let mut end = last.clone();
        end.has_next = false;
        assert!(end.differs_from(&last, at));
        // The length arriving once the song has loaded.
        let mut loaded = last.clone();
        loaded.duration = 214.0;
        assert!(loaded.differs_from(&last, at));
    }

    #[test]
    fn times_are_plain_numbers() {
        assert_eq!(time(0.0), "0");
        assert_eq!(time(-3.0), "0");
        assert_eq!(time(f64::NAN), "0");
        assert_eq!(time(42.0), "42");
        assert_eq!(time(42.5), "42.5");
        assert_eq!(time(212.3456), "212.346");
    }

    /// The message as the lounge relays it from a phone: every value a string.
    #[test]
    fn a_set_playlist_is_read() {
        let request = ListRequest::parse(
            &message(
                "setPlaylist",
                serde_json::json!({
                    "listId": "RQabc", "currentTime": "83", "audioOnly": "true",
                    "videoId": "b", "videoIds": "a,b,c", "currentIndex": "1",
                    "params": "wAEB", "ctt": "token",
                }),
            ),
            false,
        )
        .unwrap();
        assert_eq!(request.list_id.as_deref(), Some("RQabc"));
        assert_eq!(request.video_id.as_deref(), Some("b"));
        assert_eq!(request.ids, ["a", "b", "c"]);
        assert_eq!(request.index, Some(1));
        assert_eq!(request.position, 83.0);
        assert_eq!(request.params.as_deref(), Some("wAEB"));
        assert_eq!(request.ctt.as_deref(), Some("token"));
    }

    #[test]
    fn a_playlist_message_that_names_nothing_is_a_cleared_queue() {
        assert_eq!(
            ListRequest::parse(&message("setPlaylist", serde_json::json!({})), false),
            None
        );
        assert_eq!(
            ListRequest::parse(
                &message("updatePlaylist", serde_json::json!({"listId": ""})),
                true
            ),
            None
        );
    }

    /// A phone that leaves the place to the lounge sends -1, and no start time is zero.
    #[test]
    fn a_place_that_is_not_one_is_left_out() {
        let request = ListRequest::parse(
            &message(
                "setPlaylist",
                serde_json::json!({"videoId": "a", "currentIndex": "-1", "currentTime": "0"}),
            ),
            false,
        )
        .unwrap();
        assert_eq!(request.index, None);
        assert_eq!(request.position, 0.0);
        assert!(request.ids.is_empty());
    }

    /// An update that arrives while a chosen song is still being looked up carries the phone's
    /// newer list, and must still start that song.
    #[test]
    fn an_update_overtaking_a_choice_keeps_the_choice() {
        let chosen = ListRequest::parse(
            &message(
                "setPlaylist",
                serde_json::json!({
                    "listId": "RQabc", "videoId": "b", "videoIds": "a,b",
                    "currentIndex": "1", "currentTime": "30",
                }),
            ),
            false,
        )
        .unwrap();
        let update = ListRequest::parse(
            &message(
                "updatePlaylist",
                serde_json::json!({"listId": "RQabc", "videoIds": "x,a,b,c"}),
            ),
            true,
        )
        .unwrap();
        let merged = update.onto(&chosen);
        assert!(!merged.update);
        assert_eq!(merged.video_id.as_deref(), Some("b"));
        assert_eq!(merged.ids, ["x", "a", "b", "c"]);
        assert_eq!(merged.position, 30.0);
        // The old place no longer holds: it is found again by the song.
        assert_eq!(merged.index, None);
    }

    #[test]
    fn a_song_is_placed_in_the_phones_list() {
        let list = List {
            id: "RQabc".to_owned(),
            ids: ["a", "b", "a", "c"].map(str::to_owned).to_vec(),
        };
        // Where the app says, when the lists agree there: the second "a", not the first.
        assert_eq!(place(Some(&list), "a", Some(2)), Some(("RQabc", 2)));
        // The app's queue has drifted (its own autoplay, a trimmed prefix): by the song.
        assert_eq!(place(Some(&list), "c", Some(7)), Some(("RQabc", 3)));
        assert_eq!(place(Some(&list), "c", None), Some(("RQabc", 3)));
        // Not the phone's song at all.
        assert_eq!(place(Some(&list), "z", Some(0)), None);
        assert_eq!(place(None, "a", Some(0)), None);
    }

    #[test]
    fn a_remote_is_read_out_of_the_roll_call_and_a_screen_is_not() {
        let remote = serde_json::json!({
            "app": "android-phone-13.14.55", "name": "Pixel", "theme": "m",
            "id": "03b7", "type": "REMOTE_CONTROL",
        });
        assert_eq!(
            Sender::parse(&remote, Theme::Music),
            Some(Sender {
                id: "03b7".to_owned(),
                name: "Pixel".to_owned(),
                theme: Theme::Music,
            })
        );
        let screen = serde_json::json!({"name": "Refryt", "id": "b150", "type": "LOUNGE_SCREEN"});
        assert_eq!(Sender::parse(&screen, Theme::Music), None);
        // A remote that gave no name still counts.
        let nameless = serde_json::json!({"id": "9", "type": "REMOTE_CONTROL", "name": " "});
        assert_eq!(
            Sender::parse(&nameless, Theme::Music).unwrap().name,
            "A phone"
        );
    }

    #[test]
    fn flags_are_read_as_booleans_or_words() {
        assert_eq!(flag(Some(&serde_json::json!(true))), Some(true));
        assert_eq!(flag(Some(&serde_json::json!("False"))), Some(false));
        assert_eq!(flag(Some(&serde_json::json!("maybe"))), None);
        assert_eq!(flag(None), None);
    }

    #[test]
    fn a_pause_says_the_state_and_a_seek_only_the_position() {
        let mut room = room();
        let before = playing("a", 10.0);
        let at = Duration::from_secs(1);
        let mut paused = playing("a", 11.0);
        paused.state = PlayState::Paused;
        room.latest = paused.clone();
        assert_eq!(names(&room.changes(&before, at)), ["onStateChange"]);

        // Coming back says the song again: a phone that joined during the pause has it right.
        room.latest = playing("a", 11.0);
        assert_eq!(
            names(&room.changes(&paused, Duration::from_secs(30))),
            ["onStateChange", "nowPlaying"]
        );

        room.latest = playing("a", 90.0);
        let seek = room.changes(&before, at);
        assert_eq!(names(&seek), ["onStateChange"]);
        assert_eq!(field(&seek[0], "currentTime"), Some("90"));
        assert_eq!(field(&seek[0], "state"), Some("1"));
    }

    #[test]
    fn a_new_song_says_everything_about_it_and_a_volume_only_itself() {
        let mut room = room();
        let before = playing("a", 10.0);
        room.latest = playing("b", 0.0);
        room.latest.index = Some(1);
        assert_eq!(
            names(&room.changes(&before, Duration::from_secs(3))),
            ["onStateChange", "nowPlaying", "onHasPreviousNextChanged"]
        );

        // Five seconds on, five seconds further in: the song is where the phone has it.
        room.latest = playing("a", 15.0);
        room.latest.volume = 35;
        let volume = room.changes(&before, Duration::from_secs(5));
        assert_eq!(names(&volume), ["onVolumeChanged"]);
        assert_eq!(field(&volume[0], "volume"), Some("35"));
        assert_eq!(field(&volume[0], "muted"), Some("false"));
    }

    /// `nowPlaying` places the song in the phone's own list, and hands back what came with the
    /// song the phone chose — for that song only.
    #[test]
    fn now_playing_carries_the_place_and_the_context_of_the_phones_song() {
        let mut room = room();
        room.list = Some(List {
            id: "RQabc".to_owned(),
            ids: ["a", "b"].map(str::to_owned).to_vec(),
        });
        room.context = Context {
            video_id: "b".to_owned(),
            params: Some("wAEB".to_owned()),
            ctt: Some("token".to_owned()),
        };
        room.latest = playing("b", 5.0);
        room.latest.index = Some(1);
        let message = room.now_playing();
        assert_eq!(field(&message, "videoId"), Some("b"));
        assert_eq!(field(&message, "listId"), Some("RQabc"));
        assert_eq!(field(&message, "currentIndex"), Some("1"));
        assert_eq!(field(&message, "params"), Some("wAEB"));
        assert_eq!(field(&message, "ctt"), Some("token"));

        // The next song is in the list but is not the one the context came with.
        room.latest = playing("a", 0.0);
        let message = room.now_playing();
        assert_eq!(field(&message, "currentIndex"), Some("0"));
        assert_eq!(field(&message, "ctt"), None);

        // One the app's own autoplay added: the phone is told the song and no place.
        room.latest = playing("z", 0.0);
        room.latest.index = Some(2);
        let message = room.now_playing();
        assert_eq!(field(&message, "videoId"), Some("z"));
        assert_eq!(field(&message, "listId"), None);
    }

    /// The app last spoke a while ago, and the song has played on since.
    #[test]
    fn a_phone_that_asks_mid_song_is_told_where_the_song_has_got_to() {
        let mut room = room();
        room.latest = playing("a", 10.0);
        room.told = Some(Instant::now() - Duration::from_secs(60));
        let at: f64 = field(&room.now_playing(), "currentTime")
            .unwrap()
            .parse()
            .unwrap();
        assert!((70.0..71.0).contains(&at), "{at}");
        let at: f64 = field(&room.state_changed(), "currentTime")
            .unwrap()
            .parse()
            .unwrap();
        assert!((70.0..71.0).contains(&at), "{at}");

        // Paused, it is where it was left; and no song runs past its own end.
        room.latest.state = PlayState::Paused;
        assert_eq!(field(&room.now_playing(), "currentTime"), Some("10"));
        room.latest = playing("a", 190.0);
        assert_eq!(field(&room.now_playing(), "currentTime"), Some("200"));
    }

    #[test]
    fn nothing_loaded_is_an_empty_now_playing() {
        let room = room();
        let message = room.now_playing();
        assert_eq!(message.name, "nowPlaying");
        assert!(message.fields.is_empty());
        assert_eq!(field(&room.state_changed(), "state"), Some("-1"));
    }

    #[test]
    fn the_songs_looked_up_are_the_ones_nearest_the_one_playing() {
        let known = Known::default();
        remember(&known, vec![untitled("c")]);
        let ids: Vec<String> = ["a", "b", "c", "d", "e", "b"].map(str::to_owned).to_vec();
        // "c" is known; "b" is named twice and asked about once.
        assert_eq!(missing_nearest(&ids, 2, &known), ["b", "d", "a", "e"]);
        let many: Vec<String> = (0..100).map(|i| format!("id{i}")).collect();
        assert_eq!(missing_nearest(&many, 50, &known).len(), LOOKUPS);
    }

    #[test]
    fn a_song_nothing_is_known_about_still_has_a_cover_and_plays() {
        let known = Known::default();
        let tracks = tracks_for(&["dQw4w9WgXcQ".to_owned()], &known);
        assert_eq!(tracks[0].video_id, "dQw4w9WgXcQ");
        assert_eq!(
            tracks[0].thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
        );
        assert!(tracks[0].local_path.is_none());
    }

    #[test]
    fn the_roll_call_says_who_joined_and_who_left() {
        let mut room = room();
        let roll = |devices: Value, details: Value| {
            message(
                "loungeStatus",
                serde_json::json!({
                    "devices": devices.to_string(),
                    "connectionEventDetails": details.to_string(),
                }),
            )
        };
        let screen = serde_json::json!({"id": "s", "type": "LOUNGE_SCREEN", "name": "Refryt"});
        let phone = serde_json::json!({"id": "p", "type": "REMOTE_CONTROL", "name": "Pixel"});

        let arrived = room.roll_call(
            Theme::Music,
            &roll(
                serde_json::json!([screen, phone]),
                serde_json::json!({"deviceId": "p"}),
            ),
        );
        assert_eq!(
            arrived,
            Some(RollCall {
                joined: vec!["Pixel".to_owned()],
                left: Vec::new(),
                explicit: false,
            })
        );
        assert_eq!(room.themes(), [Theme::Music]);

        // The same roll again says nothing new.
        assert_eq!(
            room.roll_call(
                Theme::Music,
                &roll(serde_json::json!([screen, phone]), Value::Null)
            ),
            None
        );
        // Nor does the other screen's, which this phone is not on.
        assert_eq!(
            room.roll_call(
                Theme::Video,
                &roll(serde_json::json!([screen]), Value::Null)
            ),
            None
        );
        assert_eq!(room.senders.len(), 1);

        // The phone's own Disconnect.
        let gone = room.roll_call(
            Theme::Music,
            &roll(
                serde_json::json!([screen]),
                serde_json::json!({"deviceId": "p", "ui": "true"}),
            ),
        );
        assert_eq!(
            gone,
            Some(RollCall {
                joined: Vec::new(),
                left: vec!["Pixel".to_owned()],
                explicit: true,
            })
        );
        assert!(room.themes().is_empty());
    }

    #[test]
    fn the_identity_is_made_up_once_and_kept() {
        let mut store = Store::default();
        assert!(store.uuid.is_empty());
        store.uuid = lounge::uuid();
        store.screens.insert("m".to_owned(), "screen".to_owned());
        let back: Store = serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();
        assert_eq!(back, store);
        // A file from before a field existed still reads.
        let old: Store = serde_json::from_str(r#"{"uuid":"u"}"#).unwrap();
        assert_eq!(old.uuid, "u");
        assert!(old.screens.is_empty());
    }
}
