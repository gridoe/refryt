//! Sign-in via an imported browser cookie, and credential persistence.
//!
//! OAuth device-flow Bearer tokens were tried and dropped: InnerTube rejects them with
//! `400 ... Request contains an invalid argument` — a widespread, upstream issue
//! (sigma67/ytmusicapi#676, yt-dlp/yt-dlp#11462) affecting every third-party OAuth client —
//! while browser-cookie (SAPISIDHASH) auth keeps working, so that's the only method left.
//! Sign-in means importing the `Cookie` from a logged-in music.youtube.com session in a local
//! browser; the parsing here (`extract_cookie` et al.) also accepts a manually pasted header
//! block, which `install_cookie` reuses either way. The cookie is stored at
//! `~/.config/refryt/browser.json` with mode 0600 — it is a credential.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use directories::ProjectDirs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub fn cookie_path() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("", "", "refryt").context("could not find the config directory")?;
    Ok(dirs.config_dir().join("browser.json"))
}

/// Where an OAuth token is kept. Also a credential, also `0600`.
///
/// OAuth sign-in itself is gone (browser import is the only method now), but a build from
/// before that removal may have left one of these behind — `delete_oauth` below exists purely
/// to clean that up quietly rather than leave an orphaned credential file unread.
fn oauth_path() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("", "", "refryt").context("could not find the config directory")?;
    Ok(dirs.config_dir().join("oauth.json"))
}

pub fn delete_oauth() -> Result<()> {
    let path = oauth_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("could not delete {}", path.display())),
    }
}

/// What we persist. `saved_at` is informational only.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCookie {
    cookie: String,
    saved_at: SystemTime,
    /// Which browser this was imported from, when it was. Recorded so a session that has gone
    /// stale — Google rotates `__Secure-1PSIDTS` — can be refreshed from the same place without
    /// asking again. `None` for a pasted cookie, which cannot be refreshed.
    #[serde(default)]
    source: Option<String>,
}

/// A stored session: the cookie and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub cookie: String,
    pub source: Option<String>,
}

pub fn load_session() -> Result<Option<Session>> {
    let path = cookie_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
    let stored: StoredCookie = serde_json::from_slice(&bytes)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(Some(Session {
        cookie: stored.cookie,
        source: stored.source,
    }))
}

pub fn save_session(cookie: &str, source: Option<&str>) -> Result<()> {
    let path = cookie_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let stored = StoredCookie {
        cookie: cookie.trim().to_owned(),
        saved_at: SystemTime::now(),
        source: source.map(str::to_owned),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    // 0600: this file is a credential.
    #[cfg(unix)]
    let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    serde_json::to_writer_pretty(&mut file, &stored).context("could not write the cookie")
}

pub fn delete_cookie() -> Result<()> {
    let path = cookie_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("could not delete {}", path.display())),
    }
}

/// Pull the cookie out of whatever the user pasted.
///
/// Asking someone to extract just the cookie value is the fiddly part of signing in, so this
/// accepts every shape a browser will hand you and finds it:
///
/// * the bare cookie string;
/// * a `Cookie: …` or `cookie: …` line;
/// * a `Copy as cURL` blob, where it appears as `-H 'cookie: …'` or `-b '…'`;
/// * a **Request Headers** block copied from DevTools — including Chrome's newer layout, which
///   puts each header's name and value on separate lines.
///
/// Idea taken from `gridoe/ytm-tracker`, which accepts a pasted header block for the same reason.
pub fn extract_cookie(raw: &str) -> String {
    let lines: Vec<&str> = raw.lines().map(str::trim).collect();

    // `Cookie: …` on one line, or inside a cURL blob.
    for line in &lines {
        if let Some(rest) = strip_header_prefix(line, "cookie") {
            return sanitize_cookie(rest);
        }
        if let Some(value) = curl_cookie(line) {
            return sanitize_cookie(&value);
        }
    }

    // Chrome's two-line layout: a bare `cookie` line, with the value on the next.
    for (i, line) in lines.iter().enumerate() {
        if (line.eq_ignore_ascii_case("cookie") || line.eq_ignore_ascii_case(":cookie"))
            && let Some(value) = lines.get(i + 1)
        {
            return sanitize_cookie(value);
        }
    }

    // Nothing header-shaped: treat the whole paste as the cookie itself.
    sanitize_cookie(raw)
}

/// Whether a paste is a block of HTTP headers that simply has no cookie in it.
///
/// The common way to get this wrong is to copy a `googlevideo.com` media request: it looks like
/// the right thing — dozens of headers, obviously from YouTube — but media requests carry no
/// session cookie, so there is no credential in it at all. Saying that beats listing the cookie
/// names it is "missing".
pub fn looks_like_headers_without_a_cookie(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    if lower.contains("cookie") {
        return false;
    }
    [
        ":authority",
        ":path",
        "user-agent",
        "sec-fetch-mode",
        "accept-encoding",
    ]
    .iter()
    .filter(|marker| lower.contains(*marker))
    .count()
        >= 2
}

/// `Name: value`, case-insensitively, allowing the `:name` form HTTP/2 tooling prints.
fn strip_header_prefix<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(':').unwrap_or(line);
    let (found, value) = rest.split_once(':')?;
    found
        .trim()
        .eq_ignore_ascii_case(name)
        .then(|| value.trim())
}

/// The cookie out of a `Copy as cURL` line: `-H 'cookie: …'` or `-b '…'`.
fn curl_cookie(line: &str) -> Option<String> {
    let quoted = |after: &str| -> Option<String> {
        let rest = line.split_once(after)?.1.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let body = rest[quote.len_utf8()..].split(quote).next()?;
        Some(body.to_owned())
    };
    for marker in ["-H 'cookie:", "-H \"cookie:", "-H 'Cookie:", "-H \"Cookie:"] {
        if let Some(at) = line.find(marker) {
            let rest = &line[at + marker.len()..];
            let quote = if marker.contains('\'') { '\'' } else { '"' };
            return Some(rest.split(quote).next()?.trim().to_owned());
        }
    }
    if line.contains(" -b ") {
        return quoted(" -b ");
    }
    None
}

/// The cookie as `ytmapi-rs`'s `BrowserToken::from_str` must see it: SAPISID first, empty and
/// broken pairs dropped. It reads SAPISID up to the next `;`, so with SAPISID the last cookie —
/// how an import can order it, since a jar keeps the browser's own storage order — it fails with
/// a bare "Error parsing header" though the session is good. What is stored is left as captured.
pub fn for_token(cookie: &str) -> String {
    let pairs: Vec<&str> = cookie
        .split(';')
        .map(str::trim)
        .filter(|p| {
            p.split_once('=')
                .is_some_and(|(name, _)| !name.trim().is_empty())
        })
        .collect();
    let sapisid = |p: &str| {
        p.split_once('=')
            .is_some_and(|(n, _)| n.trim() == "SAPISID")
    };
    let (first, rest): (Vec<&str>, Vec<&str>) = pairs.into_iter().partition(|p| sapisid(p));
    first.into_iter().chain(rest).collect::<Vec<_>>().join("; ")
}

/// Strip bytes that are illegal in an HTTP header value. Pasted cookies routinely pick up
/// stray line breaks (console wrapping, cURL blobs, trailing newlines); without this the
/// request builder fails with a cryptic "builder error" and the cookie is never tried.
pub fn sanitize_cookie(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\0'))
        .collect()
}

/// The cookies a YouTube session actually needs.
///
/// `SAPISID` alone is enough to build the `SAPISIDHASH` header, which is why an incomplete cookie
/// is accepted by `ytmapi-rs` and then answered anonymously by YouTube. The rest of these are
/// `HttpOnly`, so `document.cookie` cannot see them — which is exactly how a paste from the
/// DevTools console produces a session that reports itself signed in and is not.
const REQUIRED: &[&str] = &["SAPISID", "HSID", "SSID"];

/// One of these is required too; which one varies with how the browser stored the session.
const REQUIRED_ANY: &[&str] = &["__Secure-1PSID", "__Secure-3PSID"];

/// The cookie names present in a `Cookie` header value.
fn names(cookie: &str) -> Vec<&str> {
    cookie
        .split(';')
        .filter_map(|p| p.split_once('=').map(|(n, _)| n.trim()))
        .filter(|n| !n.is_empty())
        .collect()
}

/// Reject a cookie that cannot authenticate, naming what is missing.
///
/// Checked before the network so the failure is specific and actionable, rather than a library
/// that says "signed in" and a YouTube that disagrees.
pub fn validate_cookie(cookie: &str) -> Result<()> {
    if cookie.is_empty() {
        anyhow::bail!("paste the Cookie header first");
    }
    if looks_like_headers_without_a_cookie(cookie) {
        anyhow::bail!(
            "those headers have no cookie in them. A googlevideo.com media request never \
             carries one; pick a request to music.youtube.com instead (any `youtubei/v1/…` \
             one will do) and copy its headers."
        );
    }
    let present = names(cookie);
    let mut missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|n| !present.contains(n))
        .collect();
    if !REQUIRED_ANY.iter().any(|n| present.contains(n)) {
        missing.push("__Secure-3PSID");
    }
    if missing.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "this cookie is missing {}. Those are HttpOnly, so `document.cookie` cannot show them. \
         Use Import from browser, or copy the cookie from DevTools → Network → right-click a \
         request → Copy as cURL.",
        missing.join(", ")
    )
}

/// What the settings page shows about authentication.
#[derive(Clone, Debug, Default)]
pub enum AuthState {
    #[default]
    SignedOut,
    /// A stored session exists but YouTube has not confirmed it yet. Shown at startup rather than
    /// asserting a sign-in that may have expired since the app last ran.
    Checking,
    /// Signed in, and YouTube confirmed whose session it is. Carrying the account name is what
    /// makes the state honest — the app previously said "Signed in" on a cookie that authenticated
    /// nothing.
    SignedIn {
        account: String,
    },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A good session whose SAPISID came last failed to sign in at all; the token now always
    /// finds a `;` after it, and nothing else in the cookie is lost or changed.
    #[test]
    fn the_token_sees_sapisid_first() {
        let cookie = "HSID=h; SSID=s; __Secure-3PSID=p; SAPISID=a/b";
        let out = for_token(cookie);
        assert_eq!(out, "SAPISID=a/b; HSID=h; SSID=s; __Secure-3PSID=p");
        assert_eq!(for_token(&out), out);
        // `__Secure-3PAPISID` is another cookie, and stays where it was.
        assert_eq!(
            for_token("__Secure-3PAPISID=x; SID=y; SAPISID=z;"),
            "SAPISID=z; __Secure-3PAPISID=x; SID=y"
        );
        assert_eq!(for_token(" ; junk; =v; A=1 "), "A=1");
    }

    const FULL: &str = "SID=a; HSID=b; SSID=c; SAPISID=d; __Secure-3PSID=e; LOGIN_INFO=f";

    /// Chrome's Headers pane puts the name and value on separate lines. Asking someone to
    /// reformat that by hand is the fiddly part of signing in.
    #[test]
    fn a_devtools_header_block_yields_the_cookie() {
        let pasted = format!(
            ":authority\nmusic.youtube.com\n:method\nPOST\naccept\n*/*\ncookie\n{FULL}\nreferer\nhttps://music.youtube.com/"
        );
        assert_eq!(extract_cookie(&pasted), FULL);
        validate_cookie(&extract_cookie(&pasted)).unwrap();
    }

    /// The older, one-line header format.
    #[test]
    fn a_single_line_header_yields_the_cookie() {
        assert_eq!(extract_cookie(&format!("Cookie: {FULL}")), FULL);
        assert_eq!(extract_cookie(&format!("cookie: {FULL}")), FULL);
        assert_eq!(
            extract_cookie(&format!("accept: */*\ncookie: {FULL}\norigin: x")),
            FULL
        );
    }

    #[test]
    fn a_curl_blob_yields_the_cookie() {
        let curl = format!(
            "curl 'https://music.youtube.com/' -H 'accept: */*' -H 'cookie: {FULL}' --compressed"
        );
        assert_eq!(extract_cookie(&curl), FULL);
    }

    /// A bare paste is still just the cookie.
    #[test]
    fn a_bare_cookie_is_passed_through() {
        assert_eq!(extract_cookie(FULL), FULL);
        assert_eq!(extract_cookie(&format!("  {FULL}  \n")), FULL);
    }

    /// The mistake this message exists for: copying a `googlevideo.com` media request, which
    /// looks right and contains no credential whatsoever.
    #[test]
    fn a_media_request_is_recognised_as_having_no_cookie() {
        let paste = ":authority\nrr3---sn-4g5e6nzz.googlevideo.com\n:method\nPOST\n\
                     :path\n/videoplayback?expire=1788881179&itag=141\naccept\n*/*\n\
                     accept-encoding\ngzip, deflate, br, zstd\nuser-agent\nMozilla/5.0\n\
                     sec-fetch-mode\ncors";
        assert!(looks_like_headers_without_a_cookie(paste));
        let err = validate_cookie(&extract_cookie(paste))
            .unwrap_err()
            .to_string();
        assert!(err.contains("googlevideo"), "{err}");
        assert!(
            !err.contains("HttpOnly"),
            "the wrong advice for this mistake: {err}"
        );
    }

    /// A real header block must not trip that check.
    #[test]
    fn headers_containing_a_cookie_are_not_flagged() {
        let paste =
            format!(":authority\nmusic.youtube.com\nuser-agent\nMozilla/5.0\ncookie\n{FULL}");
        assert!(!looks_like_headers_without_a_cookie(&paste));
        validate_cookie(&extract_cookie(&paste)).unwrap();
    }

    /// The exact shape of the bug: a cookie copied from `document.cookie` carries SAPISID and
    /// looks plausible, but YouTube answers every request with it anonymously.
    #[test]
    fn a_console_pasted_cookie_is_rejected() {
        let from_console = "SID=a; APISID=b; SAPISID=c; __Secure-1PAPISID=d; \
                            __Secure-3PAPISID=e; SIDCC=f; PREF=g; SOCS=h";
        let err = validate_cookie(from_console)
            .expect_err("an incomplete cookie must not be accepted")
            .to_string();
        assert!(err.contains("HSID"), "{err}");
        assert!(err.contains("SSID"), "{err}");
        assert!(err.contains("__Secure-3PSID"), "{err}");
        assert!(err.contains("HttpOnly"), "the message must say why: {err}");
    }

    #[test]
    fn a_browser_cookie_is_accepted() {
        let from_browser = "SID=a; HSID=b; SSID=c; APISID=d; SAPISID=e; \
                            __Secure-1PSID=f; __Secure-3PSID=g; LOGIN_INFO=h";
        validate_cookie(from_browser).expect("a full browser cookie must be accepted");
    }

    /// Either variant of the session cookie satisfies the requirement.
    #[test]
    fn one_secure_psid_variant_is_enough() {
        validate_cookie("SAPISID=a; HSID=b; SSID=c; __Secure-1PSID=d").unwrap();
        validate_cookie("SAPISID=a; HSID=b; SSID=c; __Secure-3PSID=d").unwrap();
        assert!(validate_cookie("SAPISID=a; HSID=b; SSID=c").is_err());
    }

    #[test]
    fn an_empty_cookie_is_rejected() {
        assert!(validate_cookie("").is_err());
    }

    #[test]
    fn sanitize_strips_paste_artifacts_but_keeps_the_cookie_intact() {
        assert_eq!(
            sanitize_cookie("  GPS=1; SAPISID=abc123; YSC=x  \r\n"),
            "GPS=1; SAPISID=abc123; YSC=x"
        );
        assert_eq!(
            sanitize_cookie("GPS=1;\nSAPISID=abc123;\r\nYSC=x"),
            "GPS=1;SAPISID=abc123;YSC=x"
        );
        assert_eq!(sanitize_cookie(""), "");
        assert_eq!(sanitize_cookie("  \n "), "");
    }
}
