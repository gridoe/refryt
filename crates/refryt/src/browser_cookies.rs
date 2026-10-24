//! Import YouTube cookies from a browser you are already signed into.
//!
//! # Why this exists
//!
//! Pasting a cookie from the DevTools console cannot work. `document.cookie` is specified not to
//! expose `HttpOnly` cookies, and `HSID`, `SSID`, `LOGIN_INFO`, `__Secure-1PSID` and
//! `__Secure-3PSID` — the ones that actually authenticate a YouTube session — are all HttpOnly.
//! A cookie pasted that way carries `SAPISID`, which is enough to *build* the `SAPISIDHASH`
//! header and therefore enough for `ytmapi-rs` to accept it, but every request made with it comes
//! back anonymous. That is exactly how this app once reported "Signed in" while showing an empty
//! library.
//!
//! The browser has the whole jar. Reading it needs no web engine and no browser process: on
//! Chromium the store is SQLite plus AES-GCM with a key from the OS keyring, and on Firefox it is
//! plain SQLite. `yt-dlp` already implements all of that, and this project already requires
//! `yt-dlp` for stream resolution — so importing costs one subprocess and no new dependency.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// How a browser keeps its cookies, which decides where its cookie file sits and whether it can
/// be read at all on this OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Engine {
    /// Firefox and its forks (Gecko): plain SQLite, `cookies.sqlite` in each profile.
    Firefox,
    /// Chrome and everything built on Chromium: SQLite with values sealed by a key from the OS
    /// keyring, `Cookies` in each profile.
    Chromium,
    /// Safari, on macOS: one `Cookies.binarycookies` in its container.
    Safari,
}

/// A browser found on this machine that `yt-dlp` can read cookies from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    /// What the app calls it.
    pub label: &'static str,
    /// What `--cookies-from-browser` is given: yt-dlp's name for the browser, and for a profile
    /// folder of its own, that folder (`firefox:/home/me/.zen`) — the forks and the Flatpak and
    /// Snap installs live where yt-dlp would not look.
    pub arg: String,
    /// When its cookie file last changed: the browser used most recently comes first in the
    /// menu.
    pub used: Option<SystemTime>,
}

/// One browser install yt-dlp can read: its label, yt-dlp's name for it, its engine, and the
/// profile folders that prove it is installed, relative to the home folder, for every OS at
/// once (a path that is not there is simply not found).
struct Known {
    label: &'static str,
    name: &'static str,
    engine: Engine,
    roots: &'static [&'static str],
}

const KNOWN: &[Known] = &[
    Known {
        label: "Firefox",
        name: "firefox",
        engine: Engine::Firefox,
        roots: &[
            ".mozilla/firefox",
            // Some distros (Arch/CachyOS among them) patch Firefox to follow the XDG base
            // directory spec instead of the historical `~/.mozilla` path.
            ".config/mozilla/firefox",
            "snap/firefox/common/.mozilla/firefox",
            ".var/app/org.mozilla.firefox/.mozilla/firefox",
            "Library/Application Support/Firefox",
            "AppData/Roaming/Mozilla/Firefox",
        ],
    },
    Known {
        label: "Zen",
        name: "firefox",
        engine: Engine::Firefox,
        roots: &[
            ".zen",
            ".var/app/app.zen_browser.zen/.zen",
            "Library/Application Support/zen",
            "AppData/Roaming/zen",
        ],
    },
    Known {
        label: "LibreWolf",
        name: "firefox",
        engine: Engine::Firefox,
        roots: &[
            ".librewolf",
            ".var/app/io.gitlab.librewolf-community/.librewolf",
            "Library/Application Support/librewolf",
            "AppData/Roaming/librewolf",
        ],
    },
    Known {
        label: "Floorp",
        name: "firefox",
        engine: Engine::Firefox,
        roots: &[
            ".floorp",
            ".var/app/one.ablaze.floorp/.floorp",
            "Library/Application Support/Floorp",
            "AppData/Roaming/Floorp",
        ],
    },
    Known {
        label: "Waterfox",
        name: "firefox",
        engine: Engine::Firefox,
        roots: &[
            ".waterfox",
            ".var/app/net.waterfox.waterfox/.waterfox",
            "Library/Application Support/Waterfox",
            "AppData/Roaming/Waterfox",
        ],
    },
    Known {
        label: "Chrome",
        name: "chrome",
        engine: Engine::Chromium,
        roots: &[
            ".config/google-chrome",
            ".var/app/com.google.Chrome/config/google-chrome",
            "Library/Application Support/Google/Chrome",
            "AppData/Local/Google/Chrome/User Data",
        ],
    },
    Known {
        label: "Chromium",
        name: "chromium",
        engine: Engine::Chromium,
        roots: &[
            ".config/chromium",
            ".var/app/org.chromium.Chromium/config/chromium",
            "snap/chromium/common/chromium",
            "Library/Application Support/Chromium",
            "AppData/Local/Chromium/User Data",
        ],
    },
    Known {
        label: "Brave",
        name: "brave",
        engine: Engine::Chromium,
        roots: &[
            ".config/BraveSoftware/Brave-Browser",
            ".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser",
            "snap/brave/current/.config/BraveSoftware/Brave-Browser",
            "Library/Application Support/BraveSoftware/Brave-Browser",
            "AppData/Local/BraveSoftware/Brave-Browser/User Data",
        ],
    },
    Known {
        label: "Vivaldi",
        name: "vivaldi",
        engine: Engine::Chromium,
        roots: &[
            ".config/vivaldi",
            ".var/app/com.vivaldi.Vivaldi/config/vivaldi",
            "Library/Application Support/Vivaldi",
            "AppData/Local/Vivaldi/User Data",
        ],
    },
    Known {
        label: "Edge",
        name: "edge",
        engine: Engine::Chromium,
        roots: &[
            ".config/microsoft-edge",
            ".var/app/com.microsoft.Edge/config/microsoft-edge",
            "Library/Application Support/Microsoft Edge",
            "AppData/Local/Microsoft/Edge/User Data",
        ],
    },
    Known {
        label: "Opera",
        name: "opera",
        engine: Engine::Chromium,
        roots: &[
            ".config/opera",
            ".var/app/com.opera.Opera/config/opera",
            "snap/opera/current/.config/opera",
            "Library/Application Support/com.operasoftware.Opera",
            "AppData/Roaming/Opera Software/Opera Stable",
        ],
    },
    Known {
        label: "Whale",
        name: "whale",
        engine: Engine::Chromium,
        roots: &[
            ".config/naver-whale",
            "Library/Application Support/Naver/Whale",
            "AppData/Local/Naver/Naver Whale/User Data",
        ],
    },
    // yt-dlp has no name of its own for Helium, so it is read as Chromium from its folder. Not
    // on macOS: Helium there keeps its key in the Keychain as "Helium Storage Key", while yt-dlp
    // reading it as Chromium asks for "Chromium Safe Storage" — a password prompt, then cookies
    // it cannot decrypt.
    Known {
        label: "Helium",
        name: "chromium",
        engine: Engine::Chromium,
        roots: &[
            ".config/net.imput.helium",
            "AppData/Local/imput/Helium/User Data",
        ],
    },
    Known {
        label: "Safari",
        name: "safari",
        engine: Engine::Safari,
        roots: &["Library/Containers/com.apple.Safari"],
    },
];

fn home() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_owned())
}

/// Whether `yt-dlp` can read the cookies of this install, at `root`, on this OS at all.
///
/// On Windows, Chrome, Edge and other Chromium browsers that ship an elevation service seal
/// their cookie key to their own process (App-Bound Encryption, Chromium 127+), so the import
/// fails with "Failed to decrypt with DPAPI" no matter what the user does — `yt-dlp` #10927.
/// `webview_login` is the path that works there, and offering these browsers anyway would only
/// produce the failure that window exists to avoid. Which ones do is a fact of each build, not of
/// Chromium (Helium does not, for one), so it is read from the install: a sealed one keeps an
/// `app_bound_encrypted_key` beside the plain `encrypted_key` in `Local State`. No `Local State`
/// means no key yt-dlp could use either. Firefox-based browsers have no such scheme, and are
/// offered everywhere. Safari is macOS only.
///
/// On macOS Chromium browsers keep their cookie key in the login Keychain. The first import makes
/// macOS ask for permission to read it, so a password prompt there is expected, not a fault.
fn readable_on(engine: Engine, root: &Path, windows: bool, macos: bool) -> bool {
    match engine {
        Engine::Firefox => true,
        Engine::Chromium => {
            !windows
                || std::fs::read_to_string(root.join("Local State"))
                    .is_ok_and(|state| !state.contains("\"app_bound_encrypted_key\""))
        }
        Engine::Safari => macos,
    }
}

/// When `root`'s cookie store last changed: the newest of its profiles' cookie files. Checked by
/// name in the few places each engine keeps it, so nothing is opened — no database, no keyring.
/// `None` when there is no cookie file at all: a profile folder left behind by an uninstall.
fn cookies_used(root: &Path, engine: Engine) -> Option<SystemTime> {
    let mtime = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let profiles = |dir: PathBuf| {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect::<Vec<_>>()
    };
    match engine {
        Engine::Firefox => profiles(root.to_owned())
            .into_iter()
            // Firefox on Windows and macOS keeps its profiles one level down, in `Profiles`.
            .chain(profiles(root.join("Profiles")))
            .filter_map(|p| mtime(p.join("cookies.sqlite")))
            .max(),
        Engine::Chromium => profiles(root.to_owned())
            .into_iter()
            .flat_map(|p| [p.join("Network").join("Cookies"), p.join("Cookies")])
            // Opera keeps its one profile in the root itself.
            .chain([root.join("Network").join("Cookies"), root.join("Cookies")])
            .filter_map(mtime)
            .max(),
        // Without Full Disk Access the container cannot be read, which says nothing about
        // whether Safari is signed in: found, with no time to sort it by.
        Engine::Safari => mtime(root.join("Data/Library/Cookies/Cookies.binarycookies"))
            .or(Some(SystemTime::UNIX_EPOCH)),
    }
}

/// The browsers on this machine with a cookie store `yt-dlp` can read, the most recently used
/// first. A browser installed more than one way (natively and as a Flatpak) is listed once, by
/// the install used last.
///
/// Presence of a cookie file is not proof that `yt-dlp` can read it — a locked keyring or a
/// sandboxed profile will still fail — so the import surfaces its error rather than this
/// pretending to know.
pub fn detected() -> Vec<Browser> {
    match home() {
        Some(home) => detected_in(&home, cfg!(windows), cfg!(target_os = "macos")),
        None => Vec::new(),
    }
}

fn detected_in(home: &Path, windows: bool, macos: bool) -> Vec<Browser> {
    let mut found: Vec<Browser> = Vec::new();
    for known in KNOWN {
        for root in known.roots {
            let root = home.join(root);
            if !root.is_dir() || !readable_on(known.engine, &root, windows, macos) {
                continue;
            }
            let Some(used) = cookies_used(&root, known.engine) else {
                continue;
            };
            let arg = match known.engine {
                Engine::Safari => known.name.to_owned(),
                _ => format!("{}:{}", known.name, root.display()),
            };
            let browser = Browser {
                label: known.label,
                arg,
                used: Some(used),
            };
            match found.iter_mut().find(|b| b.label == known.label) {
                Some(same) if same.used < browser.used => *same = browser,
                Some(_) => {}
                None => found.push(browser),
            }
        }
    }
    // Newest first; stable, so equal times keep `KNOWN`'s order.
    found.sort_by_key(|b| std::cmp::Reverse(b.used));
    found
}

/// The name to show for an import source: a session stored before the profile folder was part
/// of it holds yt-dlp's bare name ("firefox"); a newer one holds `name:folder`.
pub fn label_for(arg: &str) -> String {
    let (name, folder) = arg.split_once(':').unwrap_or((arg, ""));
    KNOWN
        .iter()
        .filter(|k| k.name == name)
        .find(|k| folder.is_empty() || k.roots.iter().any(|r| folder.ends_with(r)))
        .map_or_else(|| name.to_owned(), |k| k.label.to_owned())
}

/// Import the YouTube cookies from `browser` and return them as a `Cookie` header value.
///
/// Runs `yt-dlp --cookies-from-browser <browser> --cookies <file>` with **no URL**: yt-dlp writes
/// the jar and then exits 2 because it has nothing to download, so this makes no network request
/// and opens no browser. The exit status is therefore not a failure signal — the file is.
pub async fn import(browser: &str) -> Result<String> {
    // The name, never the argument: that can be a whole profile path.
    let label = label_for(browser);
    let path = temp_jar()?;
    // Best effort cleanup even on the error paths below.
    let _guard = Cleanup(path.clone());

    let mut cmd = Command::new(crate::backend::yt_dlp_program());
    cmd.arg("--cookies-from-browser")
        .arg(browser)
        .arg("--cookies")
        .arg(&path);
    // See `ytdlp::no_console`'s doc comment: without this, this GUI-subsystem app spawning a
    // console-subsystem binary makes Windows flash a new console window for it.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = cmd.output().await.context(
        "could not run yt-dlp; it is required for playback too, so install it and try again",
    )?;

    let jar = match std::fs::read_to_string(&path) {
        Ok(jar) => jar,
        Err(_) => {
            // yt-dlp wrote nothing, so its stderr is the real explanation: a locked keyring, an
            // unsupported browser, a profile it cannot see.
            let why = String::from_utf8_lossy(&output.stderr);
            let why = why
                .lines()
                .find(|l| l.contains("ERROR") || l.contains("could not"))
                .unwrap_or_else(|| why.trim())
                .trim();
            // Chromium on Windows holds its cookie database open while it runs (yt-dlp #7271),
            // and the browsers offered there are exactly those whose key is not the problem.
            if why.contains("Could not copy Chrome cookie database") {
                bail!("{label} is open and keeps its cookies locked; close it and try again");
            }
            bail!("yt-dlp could not read {label}'s cookies: {why}");
        }
    };

    let cookie = cookie_header(&jar);
    if cookie.is_empty() {
        bail!("no youtube.com cookies in {label}; sign in to YouTube Music there first");
    }
    Ok(cookie)
}

/// Delete the exported jar as soon as we are done with it: it is a credential.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn temp_jar() -> Result<PathBuf> {
    let dir = std::env::temp_dir();
    let unique = format!(
        "refryt-cookies-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    Ok(dir.join(unique))
}

/// Turn a Netscape cookie jar into a `Cookie` header, keeping only youtube.com.
///
/// The format is one tab-separated record per line: domain, include-subdomains, path, secure,
/// expiry, name, value. Comment and blank lines are skipped. A name that appears for several
/// domains is kept once — the last wins, which is the more specific host in yt-dlp's output.
pub fn cookie_header(jar: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut values: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for line in jar.lines() {
        let line = line.trim_end();
        // yt-dlp writes `#HttpOnly_` before the domain of an HttpOnly cookie — which is precisely
        // the set a console paste is missing, so it must not be treated as a comment.
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let [domain, _, _, _, _, name, value] = fields[..] else {
            continue;
        };
        if !domain.contains("youtube.com") || name.is_empty() {
            continue;
        }
        if values.insert(name.to_owned(), value.to_owned()).is_none() {
            names.push(name.to_owned());
        }
    }

    names
        .iter()
        .map(|n| format!("{n}={}", values[n]))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAR: &str = "\
# Netscape HTTP Cookie File
# This file is generated by yt-dlp.

.youtube.com\tTRUE\t/\tTRUE\t1799999999\tSAPISID\tsapisid-value
#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1799999999\t__Secure-3PSID\tsecret-3psid
#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1799999999\tHSID\tsecret-hsid
.google.com\tTRUE\t/\tTRUE\t1799999999\tNID\tnot-ours
music.youtube.com\tFALSE\t/\tTRUE\t1799999999\twide\t1
";

    /// The whole point: yt-dlp marks HttpOnly cookies with a `#HttpOnly_` prefix, and those are
    /// exactly the ones a console paste cannot see. Treating the line as a comment would import a
    /// jar just as useless as the paste it replaces.
    #[test]
    fn httponly_cookies_are_imported_not_skipped() {
        let header = cookie_header(JAR);
        assert!(header.contains("__Secure-3PSID=secret-3psid"), "{header}");
        assert!(header.contains("HSID=secret-hsid"), "{header}");
    }

    #[test]
    fn only_youtube_cookies_are_kept() {
        let header = cookie_header(JAR);
        assert!(header.contains("SAPISID=sapisid-value"));
        assert!(header.contains("wide=1"));
        assert!(
            !header.contains("not-ours"),
            "a google.com cookie leaked in"
        );
    }

    #[test]
    fn the_header_is_well_formed() {
        let header = cookie_header(JAR);
        assert_eq!(header.matches("; ").count(), 3, "{header}");
        assert!(!header.ends_with(';'));
        assert!(header.split("; ").all(|p| p.contains('=')));
    }

    /// A fake home with cookie files at the given paths, each touched `age` seconds before now.
    fn home_with(name: &str, files: &[(&str, u64)]) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "refryt-browsers-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        for (file, age) in files {
            let path = home.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let f = std::fs::File::create(&path).unwrap();
            f.set_modified(SystemTime::now() - std::time::Duration::from_secs(*age))
                .unwrap();
        }
        home
    }

    fn labels(found: &[Browser]) -> Vec<&'static str> {
        found.iter().map(|b| b.label).collect()
    }

    /// The browser used last comes first, whatever its place in `KNOWN`; a profile folder with
    /// no cookie file is not a browser anyone signs in with.
    #[test]
    fn the_browser_used_last_comes_first() {
        let home = home_with(
            "order",
            &[
                (".mozilla/firefox/a.default/cookies.sqlite", 500),
                (".zen/x.Default (release)/cookies.sqlite", 10),
                (".config/google-chrome/Default/Network/Cookies", 100),
                (".config/vivaldi/Default/Preferences", 1),
            ],
        );
        let found = detected_in(&home, false, false);
        assert_eq!(labels(&found), ["Zen", "Chrome", "Firefox"]);
        assert_eq!(
            found[0].arg,
            format!("firefox:{}", home.join(".zen").display())
        );
        assert_eq!(
            found[1].arg,
            format!("chrome:{}", home.join(".config/google-chrome").display())
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Installed twice — natively and as a Flatpak — is one row, pointing at the one used last.
    #[test]
    fn two_installs_of_one_browser_are_listed_once() {
        let home = home_with(
            "twice",
            &[
                (".config/google-chrome/Default/Cookies", 300),
                (
                    ".var/app/com.google.Chrome/config/google-chrome/Default/Network/Cookies",
                    5,
                ),
            ],
        );
        let found = detected_in(&home, false, false);
        assert_eq!(labels(&found), ["Chrome"]);
        assert!(
            found[0].arg.contains(".var/app/com.google.Chrome"),
            "{}",
            found[0].arg
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Windows: a Chromium browser only when its `Local State` has a plain key and no app-bound
    /// one. Helium is read as Chromium, from its own folder, and named Helium.
    #[test]
    fn windows_offers_chromium_only_without_app_bound_encryption() {
        let home = home_with(
            "app-bound",
            &[
                (
                    "AppData/Local/imput/Helium/User Data/Default/Network/Cookies",
                    5,
                ),
                (
                    "AppData/Local/Microsoft/Edge/User Data/Default/Network/Cookies",
                    10,
                ),
                (
                    "AppData/Local/Vivaldi/User Data/Default/Network/Cookies",
                    20,
                ),
            ],
        );
        let state = |dir: &str, json: &str| {
            std::fs::write(home.join(dir).join("Local State"), json).unwrap();
        };
        state(
            "AppData/Local/imput/Helium/User Data",
            r#"{"os_crypt":{"audit_enabled":true,"encrypted_key":"x"}}"#,
        );
        state(
            "AppData/Local/Microsoft/Edge/User Data",
            r#"{"os_crypt":{"app_bound_encrypted_key":"x","encrypted_key":"y"}}"#,
        );
        // Vivaldi has no `Local State` at all: no key, nothing yt-dlp could decrypt with.
        let found = detected_in(&home, true, false);
        assert_eq!(labels(&found), ["Helium"]);
        assert_eq!(
            found[0].arg,
            format!(
                "chromium:{}",
                home.join("AppData/Local/imput/Helium/User Data").display()
            )
        );
        assert_eq!(label_for(&found[0].arg), "Helium");
        // Elsewhere the key comes from the keyring, so `Local State` decides nothing.
        assert_eq!(
            labels(&detected_in(&home, false, false)),
            ["Helium", "Edge", "Vivaldi"]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Windows: no Chromium browser that seals its store (this Chrome has no `Local State`).
    /// Safari only on macOS.
    #[test]
    fn each_os_offers_only_what_it_can_read() {
        let home = home_with(
            "os",
            &[
                (
                    "AppData/Roaming/Mozilla/Firefox/Profiles/a/cookies.sqlite",
                    50,
                ),
                (
                    "AppData/Local/Google/Chrome/User Data/Default/Network/Cookies",
                    5,
                ),
                (
                    "Library/Containers/com.apple.Safari/Data/Library/Cookies/x",
                    5,
                ),
            ],
        );
        assert_eq!(labels(&detected_in(&home, true, false)), ["Firefox"]);
        assert_eq!(
            labels(&detected_in(&home, false, false)),
            ["Chrome", "Firefox"]
        );
        assert!(labels(&detected_in(&home, false, true)).contains(&"Safari"));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// macOS: no Helium, whose Keychain item yt-dlp does not know.
    #[test]
    fn macos_does_not_offer_helium() {
        let home = home_with(
            "helium-macos",
            &[(
                "Library/Application Support/net.imput.helium/Default/Cookies",
                5,
            )],
        );
        assert!(labels(&detected_in(&home, false, true)).is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Errors and the account rows name the browser, never the path the import was given.
    #[test]
    fn a_source_is_named_by_its_browser() {
        assert_eq!(label_for("firefox"), "Firefox");
        assert_eq!(label_for("firefox:/home/me/.zen"), "Zen");
        assert_eq!(label_for("firefox:/home/me/.mozilla/firefox"), "Firefox");
        assert_eq!(
            label_for("chrome:/home/me/.var/app/com.google.Chrome/config/google-chrome"),
            "Chrome"
        );
        assert_eq!(label_for("brave"), "Brave");
        assert_eq!(label_for("netscape"), "netscape");
    }

    #[test]
    fn junk_does_not_panic() {
        assert_eq!(cookie_header(""), "");
        assert_eq!(cookie_header("# only a comment\n\n"), "");
        assert_eq!(cookie_header("not\ttab\tseparated\n"), "");
    }
}
