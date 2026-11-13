# Privacy policy

refryt has no telemetry and no accounts of its own. It sends nothing to the project or its
maintainers. Everything it stores stays on your computer, and it connects only to the services
below.

## What it stores

Settings, themes, the queue, the sign-in session, the log and crash reports, in
`~/.config/refryt` on Linux, `%APPDATA%\refryt\config` on Windows,
`~/Library/Application Support/refryt` on macOS and
`~/.var/app/io.github.gridoe.refryt/config/refryt` in the Flatpak. The session is stored with
owner-only permissions. Songs downloaded for offline play go to a `refryt` folder in your Music
folder. Uninstalling the app leaves both; delete them to remove the data.

## What it connects to

**YouTube** (music.youtube.com, youtube.com, googlevideo.com, ytimg.com, googleusercontent.com,
accounts.google.com). This is the service refryt is a client for: searches, pages, playback,
covers, likes and playlists, and the sign-in window. Signed in, requests carry your Google
session, and plays are added to your YouTube history unless "Save listening history" is off
in Settings. The bundled yt-dlp contacts YouTube the same way when it is used for playback.
Google's privacy policy applies: <https://policies.google.com/privacy>.

**LRCLIB** (lrclib.net), for synced lyrics. While the lyrics panel is open, each song's title,
artist, album and length are sent to look its lyrics up. Turn it off with "Synced lyrics from
LRCLIB" in Settings, Integrations; lyrics then come from YouTube alone.

**GitHub** (api.github.com, github.com), to check for a new release when refryt starts and to
download one when you choose to update. The check sends no information about you or what you
play. Turn it off with "Check for updates" in Settings, About.

**Discord**, only if "Discord Rich Presence" is on in Settings, Integrations. It is off by
default. refryt talks to the Discord app on your computer, which then shows what you are
playing to people who can see your profile.

**Phones on your network**, only if "Cast from YouTube Music" is on in Settings, Integrations,
and only on networks you allowed. It is off by default. To tell networks apart, refryt reads
your router's hardware address from the system every 20 seconds while running, and on Linux
asks NetworkManager for the connection's name. The settings file keeps a hash of the address
and that name for each allowed network; nothing about it leaves the computer. refryt then answers device discovery on the local network with its name
("Refryt" and the computer's hostname) and accepts pairing requests from devices on it. The
connection to a paired phone runs through YouTube (youtube.com), which is told the same name
and what is playing so the phone can show it. A phone that is signed in sends YouTube its own
account as any cast does; refryt is not given it.

## Crash reports

A crash report is written to the folder above and stays there. Nothing is sent unless you copy
it, or press "Open issue", which opens a prefilled GitHub issue in your browser to review
before posting.

## Contact

Questions go to the [issue tracker](https://github.com/gridoe/refryt/issues).
