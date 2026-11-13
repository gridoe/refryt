#!/usr/bin/env python3
"""Drive refryt's cast receiver the way a phone does, and check what it says back.

A stand-in for the Cast button of the YouTube Music app: it finds the computer over SSDP, reads
its DIAL description, launches the YouTube app with a pairing code, asks YouTube which screen
claimed the code, binds to the lounge as a remote, and runs a scripted session (cast a queue,
pause, seek, volume, skip, edit the queue), printing every message the receiver sends and
whether it is the one a phone would expect. See crates/refryt/src/cast.rs.

Run refryt with "Cast from YouTube Music" on, on this network, then:

    scripts/cast-check.py                     # the session a phone has
    scripts/cast-check.py --scenario extras   # ...and what is done on the computer reaching the phone
    scripts/cast-check.py --scenario lists    # lists YouTube expands, unknown ids, a cleared queue
    scripts/cast-check.py --theme cl          # as the YouTube app rather than YouTube Music
    scripts/cast-check.py --scenario stay     # stay connected: quit refryt and it should say goodbye

It really casts: the app plays the songs, and its queue is replaced. The checks that look at the
app itself, or drive it, go through `playerctl` and are skipped without it. Standard library
only; it signs in to nothing and sends nothing but video ids.
"""
import json
import shutil
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

BASE = "https://www.youtube.com/api/lounge"
BIND = BASE + "/bc/bind"
T0 = time.time()
SEEN = []          # (t, name, payload) of everything the screen sent
SEEN_LOCK = threading.Lock()


def log(*a):
    print(f"[{time.time()-T0:6.2f}]", *a, flush=True)


def post(url, data=None, headers=None, timeout=30):
    body = urllib.parse.urlencode(data or {}).encode()
    req = urllib.request.Request(url, data=body, headers=headers or {}, method="POST")
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.status, dict(r.headers), r.read().decode()


def discover(timeout=4.0, want=None):
    """M-SEARCH for DIAL devices; return {location: headers}."""
    msg = ("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\n"
           "MX: 1\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n").encode()
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 2)
    s.settimeout(0.5)
    found = {}
    end = time.time() + timeout
    s.sendto(msg, ("239.255.255.250", 1900))
    resent = False
    while time.time() < end:
        try:
            data, addr = s.recvfrom(4096)
        except socket.timeout:
            if not resent:
                s.sendto(msg, ("239.255.255.250", 1900))
                resent = True
            continue
        text = data.decode(errors="replace")
        headers = {}
        for line in text.split("\r\n")[1:]:
            if ":" in line:
                k, v = line.split(":", 1)
                headers[k.strip().upper()] = v.strip()
        loc = headers.get("LOCATION")
        if loc and loc not in found:
            found[loc] = headers
            log("ssdp  answer from", addr, "->", loc, "| ST:", headers.get("ST"), "| USN:", headers.get("USN"))
    return found


def dial_pair(location, theme="m"):
    """Read the device description, launch YouTube with a pairing code; return the code."""
    with urllib.request.urlopen(location, timeout=10) as r:
        app_url = r.headers.get("Application-URL")
        xml = r.read().decode()
    name = xml.split("<friendlyName>")[1].split("</friendlyName>")[0]
    log("dial  device:", repr(name), "| Application-URL:", app_url)
    app = app_url.rstrip("/") + "/YouTube"
    with urllib.request.urlopen(app, timeout=10) as r:
        state = r.read().decode().split("<state>")[1].split("</state>")[0]
    log("dial  app state:", state)
    code = str(uuid.uuid4())
    body = f"pairingCode={code}&theme={theme}".encode()
    req = urllib.request.Request(app, data=body, method="POST",
                                 headers={"Content-Type": "text/plain; charset=utf-8",
                                          "Origin": "package:com.google.android.apps.youtube.music"})
    with urllib.request.urlopen(req, timeout=30) as r:
        log("dial  launch ->", r.status, "| LOCATION:", r.headers.get("LOCATION"))
    return code, name


def get_screen(code):
    for _ in range(20):
        try:
            st, _, body = post(BASE + "/pairing/get_screen", {"pairing_code": code})
            screen = json.loads(body)["screen"]
            log("pair  screen:", repr(screen.get("name")), "| id", screen.get("screenId"),
                "| keys", sorted(screen.keys()))
            return screen
        except urllib.error.HTTPError as e:
            log("pair  get_screen ->", e.code, "(retrying)")
            time.sleep(1)
    raise SystemExit("the screen never claimed the pairing code")


class Remote:
    def __init__(self, token, theme="m", name="cast-check"):
        self.headers = {"X-YouTube-LoungeId-Token": token}
        self.theme, self.name = theme, name
        self.id = str(uuid.uuid4())
        self.rid = 0
        self.ofs = 0
        self.aid = 0
        self.sid = self.gsid = None
        self.alive = True

    def bind(self):
        data = {"device": "REMOTE_CONTROL", "id": self.id, "name": self.name,
                "mdx-version": 3, "pairing_type": "cast", "app": "android-phone-13.14.55",
                "theme": self.theme, "capabilities": "que,dsdtr,atp"}
        q = {"RID": self.rid, "VER": 8, "CVER": 1}
        self.rid += 1
        st, _, body = post(BIND + "?" + urllib.parse.urlencode(q), data, self.headers)
        for aid, msg in parse(body):
            self.handle(aid, msg)
        log("remote bound")

    def handle(self, aid, msg):
        self.aid = max(self.aid, aid)
        name = msg[0]
        payload = msg[1] if len(msg) > 1 else None
        if name == "c":
            self.sid = payload
            return
        if name == "S":
            self.gsid = payload
            return
        if name == "noop":
            return
        with SEEN_LOCK:
            SEEN.append((time.time() - T0, name, payload))
        if name == "loungeStatus":
            devices = json.loads(payload.get("devices", "[]"))
            log("  <- loungeStatus:", [(d.get("type"), d.get("name")) for d in devices])
        else:
            log("  <-", name, json.dumps(payload) if payload is not None else "")

    def poll(self):
        while self.alive:
            q = {"RID": "rpc", "SID": self.sid, "CI": 0, "AID": self.aid,
                 "gsessionid": self.gsid, "TYPE": "xmlhttp", "VER": 8, "CVER": 1}
            try:
                req = urllib.request.Request(BIND + "?" + urllib.parse.urlencode(q), headers=self.headers)
                with urllib.request.urlopen(req, timeout=300) as r:
                    buf = b""
                    while self.alive:
                        piece = r.read1(65536)
                        if not piece:
                            break
                        buf += piece
                        while True:
                            nl = buf.find(b"\n")
                            if nl < 0:
                                break
                            try:
                                n = int(buf[:nl])
                            except ValueError:
                                buf = buf[nl + 1:]
                                continue
                            text = buf[nl + 1:].decode(errors="ignore")
                            if len(text) < n:
                                break
                            chunk = text[:n]
                            buf = text[n:].encode()
                            for aid, msg in json.loads(chunk):
                                self.handle(aid, msg)
            except Exception as e:
                if self.alive:
                    log("remote poll error", repr(e))
                    time.sleep(1)

    def send(self, name, payload=None):
        q = {"SID": self.sid, "gsessionid": self.gsid, "RID": self.rid, "VER": 8, "CVER": 1}
        self.rid += 1
        data = {"count": 1, "ofs": self.ofs, "req0__sc": name}
        for k, v in (payload or {}).items():
            data[f"req0_{k}"] = v
        self.ofs += 1
        st, _, _ = post(BIND + "?" + urllib.parse.urlencode(q), data, self.headers)
        log("-> ", name, payload or "", "" if st == 200 else f"status {st}")

    def disconnect(self, explicit=True):
        """What the app's own Disconnect does: terminate the channel, flagged as the user's."""
        q = {"SID": self.sid, "gsessionid": self.gsid, "RID": self.rid, "VER": 8, "CVER": 1}
        data = {"TYPE": "terminate", "ui": "true" if explicit else "",
                "clientDisconnectReason": "MDX_SESSION_DISCONNECT_REASON_DISCONNECTED_BY_USER"}
        self.rid += 1
        self.alive = False
        try:
            st, _, _ = post(BIND + "?" + urllib.parse.urlencode(q), data, self.headers)
            log("->  (disconnect, explicit=%s) status %s" % (explicit, st))
        except Exception as e:
            log("->  (disconnect) failed", repr(e))


def parse(text):
    out = []
    i = 0
    while i < len(text):
        nl = text.find("\n", i)
        if nl < 0:
            break
        try:
            n = int(text[i:nl])
        except ValueError:
            break
        out.extend(json.loads(text[nl + 1: nl + 1 + n]))
        i = nl + 1 + n
    return out


def last(name, since=0.0):
    with SEEN_LOCK:
        hits = [p for (t, n, p) in SEEN if n == name and t >= since]
    return hits[-1] if hits else None


def wait_for(name, pred=lambda p: True, timeout=25.0, since=None):
    """Wait until the screen sends `name` with a payload `pred` accepts (after `since`)."""
    since = time.time() - T0 if since is None else since
    end = time.time() + timeout
    while time.time() < end:
        with SEEN_LOCK:
            for (t, n, p) in SEEN:
                if n == name and t >= since and pred(p or {}):
                    return p
        time.sleep(0.1)
    return None


RESULTS = []


def check(what, ok, detail=""):
    RESULTS.append((what, bool(ok)))
    log(("PASS " if ok else "FAIL ") + what + (f" — {detail}" if detail else ""))


def main():
    theme = "m"
    args = sys.argv[1:]
    name_filter = None
    scenario = "full"
    while args:
        a = args.pop(0)
        if a == "--theme":
            theme = args.pop(0)
        elif a == "--name":
            name_filter = args.pop(0)
        elif a == "--scenario":
            scenario = args.pop(0)

    found = discover()
    check("the device answers an SSDP search", bool(found))
    if not found:
        return 1
    target = None
    for loc in found:
        try:
            with urllib.request.urlopen(loc, timeout=5) as r:
                xml = r.read().decode()
            fname = xml.split("<friendlyName>")[1].split("</friendlyName>")[0]
        except Exception:
            continue
        if "efryt" in fname and (name_filter is None or name_filter in fname):
            target = loc
            break
    check("its description is served", target is not None)
    if target is None:
        return 1

    code, device_name = dial_pair(target, theme)
    screen = get_screen(code)
    check("YouTube knows the screen by the pairing code", screen.get("loungeToken"))
    check("the screen carries the device's name", screen.get("name") == device_name,
          f"{screen.get('name')!r} vs {device_name!r}")

    remote = Remote(screen["loungeToken"], theme)
    remote.bind()
    threading.Thread(target=remote.poll, daemon=True).start()
    time.sleep(2.5)
    check("the screen greets a new phone with its state", last("onHasPreviousNextChanged") is not None
          and last("nowPlaying") is not None and last("onVolumeChanged") is not None)

    if scenario == "connect":
        remote.disconnect()
        time.sleep(1)
        return report()

    A, B, C, D = "dQw4w9WgXcQ", "9bZkp7q19f0", "kJQP7kiw5Fk", "JGwWNGJdvx8"

    if scenario == "lists":
        return lists(remote, A, B)
    if scenario == "one":
        # A single song, as the YouTube app casts a video.
        t = time.time() - T0
        remote.send("setPlaylist", {"videoId": A, "listId": "", "currentTime": "0", "currentIndex": "-1",
                                    "audioOnly": "false"})
        p = wait_for("nowPlaying", lambda p: p.get("videoId") == A and p.get("state") == "1", since=t, timeout=40)
        check("a single cast video plays", p is not None, json.dumps(p))
        remote.disconnect(explicit=True)
        time.sleep(2)
        return report()

    # 1. Cast a queue of three, starting on the second, 30 s in.
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": B, "listId": "", "currentTime": "30", "currentIndex": "1",
                                "audioOnly": "true", "videoIds": f"{A},{B},{C}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == B and p.get("state") == "1", since=t, timeout=40)
    check("a cast queue starts playing the chosen song", p is not None, json.dumps(p))
    if p:
        check("…from where the phone had it", 28 <= float(p.get("currentTime", 0)) <= 45, p.get("currentTime"))
        check("…placed in the phone's list", p.get("currentIndex") == "1" and p.get("listId", "").startswith("RQ"),
              f"index {p.get('currentIndex')} list {p.get('listId')}")
        check("…with its length", float(p.get("duration", 0)) > 100, p.get("duration"))
    nav = last("onHasPreviousNextChanged", since=t)
    check("next and previous are offered", nav and nav.get("hasNext") == "true" and nav.get("hasPrevious") == "true", json.dumps(nav))

    # 2. Pause, play.
    t = time.time() - T0
    remote.send("pause")
    p = wait_for("onStateChange", lambda p: p.get("state") == "2", since=t)
    check("pause pauses", p is not None, json.dumps(p))
    t = time.time() - T0
    remote.send("play")
    p = wait_for("onStateChange", lambda p: p.get("state") == "1", since=t)
    check("play resumes", p is not None, json.dumps(p))

    # 3. Seek.
    t = time.time() - T0
    remote.send("seekTo", {"newTime": "120"})
    p = wait_for("onStateChange", lambda p: 118 <= float(p.get("currentTime", 0)) <= 126, since=t)
    check("a seek is reported where it landed", p is not None, json.dumps(p))

    # 4. Volume: to something it is not already at, since a volume that does not change is
    #    not reported.
    was = (last("onVolumeChanged") or {}).get("volume")
    want = "35" if was != "35" else "55"
    t = time.time() - T0
    remote.send("setVolume", {"volume": want})
    p = wait_for("onVolumeChanged", lambda p: p.get("volume") == want, since=t)
    check("the volume follows the phone", p is not None, f"{was} -> {json.dumps(p)}")
    t = time.time() - T0
    remote.send("getVolume")
    p = wait_for("onVolumeChanged", lambda p: p.get("volume") == want, since=t)
    check("getVolume is answered", p is not None, json.dumps(p))

    # 5. Next, then previous (which restarts a song under way, then steps back).
    t = time.time() - T0
    remote.send("next")
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == C and p.get("state") == "1", since=t, timeout=40)
    check("next plays the next song of the list", p is not None and p.get("currentIndex") == "2", json.dumps(p))
    t = time.time() - T0
    remote.send("previous")
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == B, since=t, timeout=40) or \
        wait_for("onStateChange", lambda p: float(p.get("currentTime", 99)) < 3, since=t, timeout=5)
    check("previous goes back (or restarts the song)", p is not None, json.dumps(p))

    # 6. The phone adds a song to its queue: nothing restarts.
    time.sleep(3)
    t = time.time() - T0
    before = last("nowPlaying")
    remote.send("addVideo", {"videoId": D})
    time.sleep(6)
    restarted = wait_for("nowPlaying", lambda p: p.get("state") == "3", since=t, timeout=0.5)
    check("adding a song to the queue restarts nothing", restarted is None, json.dumps(restarted))

    # 7. Pick another song of the same list.
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": D, "listId": (before or {}).get("listId", ""), "currentTime": "0",
                                "currentIndex": "3", "audioOnly": "true", "videoIds": f"{A},{B},{C},{D}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == D and p.get("state") == "1", since=t, timeout=40)
    check("picking the added song plays it", p is not None and p.get("currentIndex") == "3", json.dumps(p))

    # 8. getNowPlaying is answered.
    t = time.time() - T0
    remote.send("getNowPlaying")
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == D, since=t)
    check("getNowPlaying is answered", p is not None, json.dumps(p))

    if scenario == "stay":
        log("staying connected for 40 s (watch for the screen's goodbye)…")
        t = time.time() - T0
        end = time.time() + 40
        heard = False
        while time.time() < end and not heard:
            with SEEN_LOCK:
                heard = any(n == "loungeScreenDisconnected" and at >= t for (at, n, _) in SEEN)
            time.sleep(0.1)
        check("the screen says goodbye when it leaves", heard)
        return report()

    if scenario == "extras":
        if mpris("status") is None:
            log("SKIP the checks that drive the app: playerctl is not installed")
        else:
            extras(remote, A, B, C, D)

    # The phone's own Disconnect pauses the computer.
    if mpris("status") is None:
        remote.disconnect(explicit=True)
        time.sleep(2)
        return report()
    check("the computer is playing before the phone leaves", mpris("status") == "Playing", mpris("status"))
    remote.disconnect(explicit=True)
    for _ in range(40):
        if mpris("status") == "Paused":
            break
        time.sleep(0.25)
    check("the phone's Disconnect pauses the computer", mpris("status") == "Paused", mpris("status"))
    return report()


def lists(remote, A, B):
    # A list the phone names and leaves to YouTube: a song's radio, with no ids.
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": A, "listId": "RDAMVM" + A, "currentTime": "0", "currentIndex": "0",
                                "audioOnly": "true", "params": "wAEB"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == A and p.get("state") == "1", since=t, timeout=40)
    check("a radio named by its id plays its song", p is not None, json.dumps(p))
    log("     list as the lounge has it:", json.dumps(last("playlistModified", since=t)))
    t = time.time() - T0
    remote.send("next")
    p = wait_for("nowPlaying", lambda p: p.get("videoId") not in (A, None) and p.get("state") == "1", since=t, timeout=40)
    check("…and next goes on into the radio", p is not None, json.dumps(p))

    # A queue with a video id YouTube knows nothing about.
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": B, "listId": "", "currentTime": "0", "currentIndex": "0",
                                "audioOnly": "true", "videoIds": f"{B},zzzzzzzzzzz,{A}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == B and p.get("state") == "1", since=t, timeout=40)
    check("a queue holding an unknown id still plays", p is not None, json.dumps(p))
    log("     list as the lounge has it:", json.dumps(last("playlistModified", since=t)))

    # The phone clears its queue.
    t = time.time() - T0
    remote.send("clearPlaylist")
    p = wait_for("onStateChange", lambda p: p.get("state") == "2", since=t, timeout=5)
    check("a cleared queue pauses the computer", p is not None, json.dumps(p))
    remote.disconnect(explicit=True)
    time.sleep(2)
    return report()


def mpris(*args):
    """Drive or read the app the way the desktop does (a media key, the shell's widget).
    None without playerctl."""
    if shutil.which("playerctl") is None:
        return None
    out = subprocess.run(["playerctl", "-p", "refryt", *args], capture_output=True, text=True)
    return out.stdout.strip()


def extras(remote, A, B, C, D):
    # What is done on the computer shows on the phone.
    t = time.time() - T0
    mpris("pause")
    p = wait_for("onStateChange", lambda p: p.get("state") == "2", since=t)
    check("a pause on the computer reaches the phone", p is not None, json.dumps(p))
    t = time.time() - T0
    mpris("play")
    p = wait_for("onStateChange", lambda p: p.get("state") == "1", since=t)
    check("a play on the computer reaches the phone", p is not None, json.dumps(p))
    t = time.time() - T0
    mpris("volume", "0.6")
    p = wait_for("onVolumeChanged", lambda p: p.get("volume") == "60", since=t)
    check("a volume change on the computer reaches the phone", p is not None, json.dumps(p))

    # Start over at the top of the list, then skip from the computer.
    t = time.time() - T0
    listId = (last("nowPlaying") or {}).get("listId", "")
    remote.send("setPlaylist", {"videoId": A, "listId": listId, "currentTime": "0", "currentIndex": "0",
                                "audioOnly": "true", "videoIds": f"{A},{B},{C},{D}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == A and p.get("state") == "1", since=t, timeout=40)
    check("the first song of the list plays", p is not None and p.get("currentIndex") == "0", json.dumps(p))
    t = time.time() - T0
    mpris("next")
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == B and p.get("state") == "1", since=t, timeout=40)
    check("a skip on the computer reaches the phone, placed in its list",
          p is not None and p.get("currentIndex") == "1", json.dumps(p))

    # The end of a song moves on to the next of the phone's list by itself.
    dur = float((p or {}).get("duration", 219))
    t = time.time() - T0
    remote.send("seekTo", {"newTime": str(int(dur - 6))})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == C and p.get("state") == "1", since=t, timeout=45)
    check("the song ending plays the next one", p is not None and p.get("currentIndex") == "2", json.dumps(p))

    # A song picked on the computer that is not the phone's is told without a place.
    # (Past the phone's four songs is the autoplay tail the app added.)
    t = time.time() - T0
    for _ in range(2):
        mpris("next")
        time.sleep(4)
    p = wait_for("nowPlaying", lambda p: p.get("videoId") not in (A, B, C, D, None) and p.get("state") == "1",
                 since=t, timeout=45)
    check("a song from the app's own autoplay is told to the phone", p is not None, json.dumps(p))

    # The phone picks from its list again: back in the list, nothing lost.
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": C, "listId": listId, "currentTime": "0", "currentIndex": "2",
                                "audioOnly": "true", "videoIds": f"{A},{B},{C},{D}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == C and p.get("state") == "1", since=t, timeout=40)
    check("the phone takes it back to its list", p is not None and p.get("currentIndex") == "2", json.dumps(p))

    # The phone names the song that is already playing (it reconnected, or sent its queue
    # again): nothing restarts, and the screen says where the song really is.
    time.sleep(6)
    t = time.time() - T0
    remote.send("setPlaylist", {"videoId": C, "listId": listId, "currentTime": "0", "currentIndex": "2",
                                "audioOnly": "true", "videoIds": f"{A},{B},{C},{D}"})
    p = wait_for("nowPlaying", lambda p: p.get("videoId") == C, since=t, timeout=15)
    reloaded = wait_for("onStateChange", lambda p: p.get("state") == "3", since=t, timeout=3)
    check("naming the song already playing restarts nothing",
          p is not None and float(p.get("currentTime", 0)) > 4 and p.get("state") == "1" and reloaded is None,
          f"{json.dumps(p)} reloaded={json.dumps(reloaded)}")


def report():
    bad = [w for (w, ok) in RESULTS if not ok]
    log(f"{len(RESULTS) - len(bad)}/{len(RESULTS)} checks passed" + (": FAILED " + "; ".join(bad) if bad else ""))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
