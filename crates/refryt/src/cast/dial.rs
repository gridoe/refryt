//! DIAL: how a phone on the same network finds this computer and asks it to pair.
//!
//! The Cast button in YouTube and YouTube Music lists two kinds of device: Chromecasts, and
//! anything that answers [DIAL](http://www.dial-multiscreen.org) for the app named `YouTube` —
//! which is how smart TVs get there, and how this does. Two small servers make that up:
//!
//! - **SSDP**, on UDP 1900. The phone multicasts an `M-SEARCH` for the DIAL service; the answer
//!   is one datagram pointing at the device description below.
//! - **HTTP**, on [`PORT`]. `GET /dd.xml` names the device and, in an `Application-URL`
//!   header, where its apps live; `GET /apps/YouTube` says the app is running; `POST
//!   /apps/YouTube` is the launch, whose body carries a pairing code the phone made up.
//!
//! The launch is the only part that does anything: the code goes to the receiver, which
//! registers it with YouTube against its lounge screen (`lounge.rs`). The phone then asks
//! YouTube which screen claimed its code, and everything after that goes through the lounge.
//!
//! Both servers are written out by hand, like `refryt-resolve`'s stream proxy: a dozen lines
//! of HTTP is less to carry than a server crate. Anyone on the local network can pair, exactly
//! as with a Chromecast or a TV; a web page cannot, because a launch that names a web origin is
//! refused (DIAL 2.1, section 6.4.2).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};

/// The HTTP port. Fixed, so a firewall rule can name it; when something else holds it, any free
/// port is used instead, and discovery still points the phone at it.
pub const PORT: u16 = 47_023;

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const DIAL_SERVICE: &str = "urn:dial-multiscreen-org:service:dial:1";
const DIAL_DEVICE: &str = "urn:dial-multiscreen-org:device:dial:1";
/// How long a phone may keep an answer before asking again, in seconds.
const MAX_AGE: u32 = 1800;
/// How often the device announces itself unasked, well inside [`MAX_AGE`].
const ANNOUNCE_EVERY: Duration = Duration::from_secs(600);
/// How long a launch waits for the receiver to register its pairing code.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
/// The most a request may carry. A launch body is a pairing code and a few flags.
const MAX_REQUEST: usize = 8 * 1024;
/// The most connections served at once. A phone opens one at a time; past this a connection is
/// closed unread, so a flood of them cannot use up the app's file handles, which mpv and the
/// network need too.
const MAX_CONNECTIONS: usize = 16;
/// The least time between two pairings. A phone pairs once per Cast tap; each pairing is a
/// request to YouTube from this computer, which a flood must not turn into a stream of them.
const LAUNCH_GAP: Duration = Duration::from_secs(1);
/// The pause after the listener fails to accept, as it does with no file handles left: without
/// it the loop would retry at once, and keep a core busy until one frees up.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(250);

/// A phone asking to pair, handed to the receiver.
pub(crate) struct Launch {
    pub(crate) pairing_code: String,
    /// The `theme` the launch named: which app is asking (see `lounge::Theme`).
    pub(crate) theme: Option<String>,
    /// Answered `true` once the code is registered; the phone is told to give up otherwise.
    pub(crate) reply: oneshot::Sender<bool>,
}

struct Shared {
    /// The device's UUID, the same across launches so a phone lists it once.
    uuid: String,
    /// The name the Cast menu shows.
    name: String,
    port: u16,
    launches: mpsc::UnboundedSender<Launch>,
    /// Held for the length of a pairing, so only one runs at a time.
    pairing: tokio::sync::Semaphore,
    /// When the last pairing began.
    last_launch: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Shared {
    fn new(uuid: String, name: String, port: u16, launches: mpsc::UnboundedSender<Launch>) -> Self {
        Self {
            uuid,
            name,
            port,
            launches,
            pairing: tokio::sync::Semaphore::new(1),
            last_launch: std::sync::Mutex::new(None),
        }
    }
}

/// The two servers. Dropping this stops them; [`Dial::goodbye`] tells the network first.
pub(crate) struct Dial {
    shared: Arc<Shared>,
    http: tokio::task::JoinHandle<()>,
    ssdp: tokio::task::JoinHandle<()>,
}

impl Drop for Dial {
    fn drop(&mut self) {
        self.http.abort();
        self.ssdp.abort();
    }
}

impl Dial {
    /// Open both servers. Fails when discovery's port cannot be shared, since a device nothing
    /// can find is not worth half-starting.
    pub(crate) async fn start(
        uuid: String,
        name: String,
        launches: mpsc::UnboundedSender<Launch>,
    ) -> Result<Self> {
        let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT)).await {
            Ok(listener) => listener,
            Err(e) => {
                tracing::debug!(error = %e, "cast: the usual port is taken, using a free one");
                TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
                    .await
                    .context("could not open a port for pairing")?
            }
        };
        let port = listener.local_addr()?.port();
        let socket = ssdp_socket().context("could not listen for discovery on UDP 1900")?;
        let shared = Arc::new(Shared::new(uuid, name, port, launches));
        let http = tokio::spawn(serve_http(listener, Arc::clone(&shared)));
        let ssdp = tokio::spawn(serve_ssdp(socket, Arc::clone(&shared)));
        tracing::info!(port, "cast: discoverable on the local network");
        Ok(Self { shared, http, ssdp })
    }

    /// The TCP port pairings come in on.
    pub(crate) fn port(&self) -> u16 {
        self.shared.port
    }

    /// Tell the network the device is leaving, so a phone drops it from its list now rather
    /// than when its answer goes stale.
    pub(crate) async fn goodbye(&self) {
        notify(&self.shared, "ssdp:byebye").await;
    }
}

/// The discovery socket: UDP 1900, shared with whatever else on this machine speaks SSDP (a
/// browser looking for Chromecasts, a media server), and joined to the multicast group.
fn ssdp_socket() -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    // Linux and the BSDs deliver a multicast datagram to every socket on the port only when
    // each asked for this as well.
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, SSDP_PORT)).into())?;
    // On the interface the routing table picks for the group, which is the one the default
    // route leaves by: the network the phone is on, for a machine with one.
    socket.join_multicast_v4(&SSDP_ADDR, &Ipv4Addr::UNSPECIFIED)?;
    Ok(UdpSocket::from_std(socket.into())?)
}

async fn serve_ssdp(socket: UdpSocket, shared: Arc<Shared>) {
    // Unasked announcements run beside the answers: a phone already listening picks the device
    // up without searching.
    let announcing = Arc::clone(&shared);
    let announce = tokio::spawn(async move {
        loop {
            notify(&announcing, "ssdp:alive").await;
            tokio::time::sleep(ANNOUNCE_EVERY).await;
        }
    });
    let mut buf = [0u8; 2048];
    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            Err(e) => {
                tracing::debug!(error = %e, "cast: discovery socket failed");
                break;
            }
        };
        let Some(target) = search_target(&String::from_utf8_lossy(&buf[..len]), &shared.uuid)
        else {
            continue;
        };
        if let Err(e) = answer_search(&shared, from, &target).await {
            tracing::debug!(error = %e, "cast: could not answer a search");
        }
    }
    announce.abort();
}

/// The `ST` of an `M-SEARCH` this device should answer, or `None` for anything else on the
/// group: other devices' announcements, searches for other things.
fn search_target(datagram: &str, uuid: &str) -> Option<String> {
    let mut lines = datagram.lines();
    if !lines.next()?.starts_with("M-SEARCH") {
        return None;
    }
    let header = |name: &str| {
        datagram.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    if !header("man")?.contains("ssdp:discover") {
        return None;
    }
    let target = header("st")?;
    let ours = target == DIAL_SERVICE
        || target == DIAL_DEVICE
        || target == "ssdp:all"
        || target == "upnp:rootdevice"
        || target.strip_prefix("uuid:") == Some(uuid);
    ours.then_some(target)
}

/// Answer one search, from a socket of its own connected to the asker: the address the system
/// gives that socket is the one of ours the asker can reach, which is what `LOCATION` has to
/// name on a machine with more than one network.
async fn answer_search(shared: &Shared, to: SocketAddr, target: &str) -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.connect(to).await?;
    let local = socket.local_addr()?.ip();
    // `ssdp:all` is answered as the service: one answer is enough to be found.
    let target = if target == "ssdp:all" {
        DIAL_SERVICE
    } else {
        target
    };
    let answer = format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age={MAX_AGE}\r\n\
         DATE: {date}\r\n\
         EXT:\r\n\
         LOCATION: http://{local}:{port}/dd.xml\r\n\
         SERVER: {server}\r\n\
         ST: {target}\r\n\
         USN: {usn}\r\n\
         BOOTID.UPNP.ORG: 1\r\n\
         CONFIGID.UPNP.ORG: 1\r\n\
         \r\n",
        date = http_date(),
        port = shared.port,
        server = server(),
        usn = usn(&shared.uuid, target),
    );
    socket.send(answer.as_bytes()).await?;
    Ok(())
}

/// Multicast an `ssdp:alive` or `ssdp:byebye` for the device and its service.
async fn notify(shared: &Shared, kind: &str) {
    let group = SocketAddr::V4(SocketAddrV4::new(SSDP_ADDR, SSDP_PORT));
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        return;
    };
    // As in `answer_search`: connected to the group, the socket's own address is the one on
    // the network the announcement leaves by.
    if socket.connect(group).await.is_err() {
        return;
    }
    let Ok(local) = socket.local_addr() else {
        return;
    };
    for target in ["upnp:rootdevice", DIAL_DEVICE, DIAL_SERVICE] {
        let message = format!(
            "NOTIFY * HTTP/1.1\r\n\
             HOST: {SSDP_ADDR}:{SSDP_PORT}\r\n\
             CACHE-CONTROL: max-age={MAX_AGE}\r\n\
             LOCATION: http://{ip}:{port}/dd.xml\r\n\
             NT: {target}\r\n\
             NTS: {kind}\r\n\
             SERVER: {server}\r\n\
             USN: {usn}\r\n\
             BOOTID.UPNP.ORG: 1\r\n\
             CONFIGID.UPNP.ORG: 1\r\n\
             \r\n",
            ip = local.ip(),
            port = shared.port,
            server = server(),
            usn = usn(&shared.uuid, target),
        );
        let _ = socket.send(message.as_bytes()).await;
    }
}

fn usn(uuid: &str, target: &str) -> String {
    if target.starts_with("uuid:") {
        target.to_owned()
    } else {
        format!("uuid:{uuid}::{target}")
    }
}

fn server() -> String {
    format!(
        "{}/1.0 UPnP/1.1 refryt/{}",
        std::env::consts::OS,
        env!("CARGO_PKG_VERSION")
    )
}

fn http_date() -> String {
    chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

async fn serve_http(listener: TcpListener, shared: Arc<Shared>) {
    let open = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let socket = match listener.accept().await {
            Ok((socket, _)) => socket,
            Err(e) => {
                tracing::debug!(error = %e, "cast: could not accept a connection");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let Ok(slot) = Arc::clone(&open).try_acquire_owned() else {
            // Dropped unread: closed at once.
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            if let Err(e) = serve(socket, &shared).await {
                tracing::debug!(error = %format!("{e:#}"), "cast: a pairing request failed");
            }
            drop(slot);
        });
    }
}

/// Whether a pairing may start now: none is under way, and the last began long enough ago.
/// Returns the permit that marks this one as under way.
fn may_pair(shared: &Shared) -> Option<tokio::sync::SemaphorePermit<'_>> {
    let permit = shared.pairing.try_acquire().ok()?;
    let mut last = shared.last_launch.lock().unwrap();
    let now = std::time::Instant::now();
    if last.is_some_and(|at| now.duration_since(at) < LAUNCH_GAP) {
        return None;
    }
    *last = Some(now);
    Some(permit)
}

#[derive(Debug, PartialEq)]
struct Request {
    method: String,
    path: String,
    origin: Option<String>,
    body: String,
}

async fn serve(mut socket: TcpStream, shared: &Shared) -> Result<()> {
    // The address the phone reached, which is the one its next request should go to.
    let base = match socket.local_addr()? {
        SocketAddr::V4(local) => format!("http://{}:{}", local.ip(), local.port()),
        SocketAddr::V6(local) => format!("http://[{}]:{}", local.ip(), local.port()),
    };
    let request = tokio::time::timeout(Duration::from_secs(10), read_request(&mut socket))
        .await
        .context("the request never finished")??;
    let response = respond(&request, &base, shared).await;
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await?;
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> Result<Request> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(request) = parse_request(&buf)? {
            return Ok(request);
        }
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            bail!("the client closed before finishing its request");
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST {
            bail!("request too large");
        }
    }
}

/// A whole request out of what has arrived so far, or `None` while its headers or body are
/// still on the way.
fn parse_request(buf: &[u8]) -> Result<Option<Request>> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&buf[..end]);
    let mut lines = head.lines();
    let mut first = lines.next().context("empty request")?.split_whitespace();
    let method = first.next().context("no method")?.to_owned();
    let target = first.next().context("no path")?;
    let path = target.split(['?', '#']).next().unwrap_or(target).to_owned();
    let mut origin = None;
    let mut length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().context("bad Content-Length")?;
        }
    }
    if length > MAX_REQUEST {
        bail!("request body too large");
    }
    let body = &buf[end + 4..];
    if body.len() < length {
        return Ok(None);
    }
    Ok(Some(Request {
        method,
        path,
        origin,
        body: String::from_utf8_lossy(&body[..length]).into_owned(),
    }))
}

/// Whether a request's `Origin` is one a launch may come from. A native app sends none, or a
/// `package:` one; a web page always names itself with a web scheme, and a page the user merely
/// visited must not be able to start playback on their speakers.
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let scheme = origin
        .split_once(':')
        .map_or(origin, |(scheme, _)| scheme)
        .to_ascii_lowercase();
    !matches!(scheme.as_str(), "http" | "https" | "file" | "null")
}

async fn respond(request: &Request, base: &str, shared: &Shared) -> String {
    if !origin_allowed(request.origin.as_deref()) {
        return plain(403, "Forbidden");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/dd.xml") => {
            let headers = format!("Application-URL: {base}/apps\r\n");
            xml(200, &headers, &device_description(base, shared))
        }
        ("GET", "/apps/YouTube") => xml(200, "", APP_DESCRIPTION),
        ("POST", "/apps/YouTube") => {
            let Some((pairing_code, theme)) = launch_data(&request.body) else {
                // A launch with no code is a phone that only wants the app in front, which it
                // already is.
                return launched(base);
            };
            let Some(_pairing) = may_pair(shared) else {
                tracing::debug!("cast: a pairing came too soon after another, refused");
                return plain(503, "Service Unavailable");
            };
            let (reply, registered) = oneshot::channel();
            let launch = Launch {
                pairing_code,
                theme,
                reply,
            };
            if shared.launches.send(launch).is_err() {
                return plain(503, "Service Unavailable");
            }
            match tokio::time::timeout(LAUNCH_TIMEOUT, registered).await {
                Ok(Ok(true)) => launched(base),
                _ => plain(503, "Service Unavailable"),
            }
        }
        // The app is the player itself: there is nothing a phone may stop.
        ("DELETE", path) if path.starts_with("/apps/YouTube/") => plain(405, "Method Not Allowed"),
        ("GET", "/apps") => plain(204, "No Content"),
        _ => plain(404, "Not Found"),
    }
}

/// The pairing code and theme out of a launch's form body.
fn launch_data(body: &str) -> Option<(String, Option<String>)> {
    let mut pairing_code = None;
    let mut theme = None;
    for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
        match key.as_ref() {
            "pairingCode" => pairing_code = Some(value.into_owned()),
            "theme" => theme = Some(value.into_owned()),
            _ => {}
        }
    }
    let pairing_code = pairing_code.filter(|code| !code.is_empty() && code.len() <= 128)?;
    Some((pairing_code, theme))
}

fn launched(base: &str) -> String {
    format!(
        "HTTP/1.1 201 Created\r\n\
         LOCATION: {base}/apps/YouTube/run\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n"
    )
}

fn plain(status: u16, reason: &str) -> String {
    format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

fn xml(status: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} OK\r\n\
         Content-Type: application/xml; charset=utf-8\r\n\
         {headers}\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    )
}

/// The app is always running, and not the phone's to stop: it is this player.
const APP_DESCRIPTION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<service xmlns=\"urn:dial-multiscreen-org:schemas:dial\" dialVer=\"1.7\">\n\
  <name>YouTube</name>\n\
  <options allowStop=\"false\"/>\n\
  <state>running</state>\n\
  <link rel=\"run\" href=\"run\"/>\n\
</service>\n";

fn device_description(base: &str, shared: &Shared) -> String {
    format!(
        "<?xml version=\"1.0\"?>\n\
<root xmlns=\"urn:schemas-upnp-org:device-1-0\">\n\
  <specVersion><major>1</major><minor>0</minor></specVersion>\n\
  <URLBase>{base}</URLBase>\n\
  <device>\n\
    <deviceType>{DIAL_DEVICE}</deviceType>\n\
    <friendlyName>{name}</friendlyName>\n\
    <manufacturer>refryt</manufacturer>\n\
    <modelName>refryt</modelName>\n\
    <UDN>uuid:{uuid}</UDN>\n\
    <serviceList>\n\
      <service>\n\
        <serviceType>{DIAL_SERVICE}</serviceType>\n\
        <serviceId>urn:dial-multiscreen-org:serviceId:dial</serviceId>\n\
        <controlURL>/ssdp/notfound</controlURL>\n\
        <eventSubURL>/ssdp/notfound</eventSubURL>\n\
        <SCPDURL>/ssdp/notfound</SCPDURL>\n\
      </service>\n\
    </serviceList>\n\
  </device>\n\
</root>\n",
        name = xml_escape(&shared.name),
        uuid = shared.uuid,
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "0f6c1a52-7d0e-4c1b-9a53-6f3d2e1b0a99";

    fn search(st: &str) -> String {
        format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\n\
             MX: 1\r\nST: {st}\r\n\r\n"
        )
    }

    #[test]
    fn a_search_for_dial_is_answered_and_others_are_not() {
        assert_eq!(
            search_target(&search(DIAL_SERVICE), UUID).as_deref(),
            Some(DIAL_SERVICE)
        );
        assert_eq!(
            search_target(&search("ssdp:all"), UUID).as_deref(),
            Some("ssdp:all")
        );
        assert!(search_target(&search(&format!("uuid:{UUID}")), UUID).is_some());
        // Another device's id, a media renderer, and an announcement rather than a search.
        assert!(search_target(&search("uuid:someone-else"), UUID).is_none());
        assert!(
            search_target(&search("urn:schemas-upnp-org:device:MediaRenderer:1"), UUID).is_none()
        );
        assert!(
            search_target(
                "NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\nNTS: ssdp:alive\r\n\r\n",
                UUID
            )
            .is_none()
        );
    }

    #[test]
    fn a_search_reads_its_headers_in_any_case() {
        let datagram = "M-SEARCH * HTTP/1.1\r\nHost: 239.255.255.250:1900\r\n\
                        Man: \"ssdp:discover\"\r\nSt: urn:dial-multiscreen-org:service:dial:1\r\n\r\n";
        assert_eq!(search_target(datagram, UUID).as_deref(), Some(DIAL_SERVICE));
    }

    #[test]
    fn a_launch_is_read_once_its_whole_body_has_arrived() {
        let head = "POST /apps/YouTube?x=1 HTTP/1.1\r\nHost: 192.168.1.5:47023\r\n\
                    Content-Type: text/plain\r\nContent-Length: 28\r\n\r\n";
        let body = "pairingCode=abc-123&theme=m&";
        assert_eq!(body.len(), 28);
        let whole = format!("{head}{body}");
        // Short of the headers' end, then short of the body's.
        assert_eq!(parse_request(&whole.as_bytes()[..40]).unwrap(), None);
        assert_eq!(
            parse_request(&whole.as_bytes()[..whole.len() - 3]).unwrap(),
            None
        );
        let request = parse_request(whole.as_bytes()).unwrap().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/apps/YouTube");
        assert_eq!(request.origin, None);
        assert_eq!(
            launch_data(&request.body),
            Some(("abc-123".to_owned(), Some("m".to_owned())))
        );
    }

    #[test]
    fn a_launch_without_a_code_names_none() {
        assert_eq!(launch_data("v=dQw4w9WgXcQ&t=0"), None);
        assert_eq!(launch_data("pairingCode="), None);
        assert_eq!(
            launch_data("pairingCode=abc"),
            Some(("abc".to_owned(), None))
        );
    }

    #[test]
    fn a_body_past_the_limit_is_refused() {
        let request = format!(
            "POST /apps/YouTube HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_REQUEST + 1
        );
        assert!(parse_request(request.as_bytes()).is_err());
    }

    /// An app sends no origin or a `package:` one; a page in a browser always sends its own.
    #[test]
    fn only_a_web_origin_is_turned_away() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("package:com.google.android.youtube")));
        assert!(origin_allowed(Some(
            "package:com.google.android.apps.youtube.music"
        )));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("http://192.168.1.5:47023")));
        assert!(!origin_allowed(Some("HTTPS://EVIL.EXAMPLE")));
        assert!(!origin_allowed(Some("file://")));
        assert!(!origin_allowed(Some("null")));
    }

    #[test]
    fn pairings_go_one_at_a_time_and_not_too_close_together() {
        let (launches, _keep) = mpsc::unbounded_channel();
        let shared = Shared::new(UUID.to_owned(), "x".to_owned(), PORT, launches);
        let first = may_pair(&shared);
        assert!(first.is_some());
        assert!(may_pair(&shared).is_none(), "one is under way");
        drop(first);
        assert!(
            may_pair(&shared).is_none(),
            "the last began under a second ago"
        );
        *shared.last_launch.lock().unwrap() =
            Some(std::time::Instant::now() - LAUNCH_GAP - Duration::from_millis(10));
        assert!(may_pair(&shared).is_some());
    }

    #[test]
    fn a_device_name_cannot_break_its_description() {
        let (launches, _keep) = mpsc::unbounded_channel();
        let shared = Shared::new(UUID.to_owned(), "Rock & <Roll>".to_owned(), PORT, launches);
        let xml = device_description("http://192.168.1.5:47023", &shared);
        assert!(xml.contains("<friendlyName>Rock &amp; &lt;Roll&gt;</friendlyName>"));
        assert!(xml.contains(&format!("<UDN>uuid:{UUID}</UDN>")));
    }

    #[test]
    fn a_usn_names_the_device_and_what_was_asked_for() {
        assert_eq!(
            usn(UUID, DIAL_SERVICE),
            format!("uuid:{UUID}::{DIAL_SERVICE}")
        );
        assert_eq!(usn(UUID, &format!("uuid:{UUID}")), format!("uuid:{UUID}"));
    }
}
