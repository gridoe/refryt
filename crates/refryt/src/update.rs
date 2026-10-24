//! Updates: whether a newer release is out, and — for the packages that can — installing it.
//!
//! The check is one request to GitHub's "latest release" endpoint, compared with this build's own
//! version. What an update can do next depends on how this copy was installed:
//!
//! - The Windows setup and MSI download the matching installer, check it against the release's
//!   `SHA256SUMS`, and run it once the app has closed; the installer is followed by the new
//!   version starting.
//! - The AppImage downloads its successor beside itself, checks it, swaps it in and restarts.
//! - Everything else only says that a new version exists and links the release: a Flatpak should
//!   not replace itself from inside its sandbox, an unnotarised macOS bundle that rewrites itself
//!   trips Gatekeeper, and the Windows zip and a build from source have no installer to run.
//!
//! Nothing is installed without a click, and nothing is sent beyond the request itself.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const LATEST: &str = "https://api.github.com/repos/gridoe/refryt/releases/latest";
/// Where the project's releases live. A download or page URL outside it is never followed, even
/// if the API's answer names one.
const RELEASES: &str = "https://github.com/gridoe/refryt/releases/";
const USER_AGENT: &str = concat!("refryt/", env!("CARGO_PKG_VERSION"));

/// How this copy was installed, which decides whether it can update itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// The Inno Setup installer: its uninstaller sits next to the exe.
    WindowsSetup,
    /// The MSI: under Program Files, without Inno's uninstaller.
    WindowsMsi,
    /// An AppImage, which the runtime names in `$APPIMAGE`.
    AppImage,
    /// Anything that cannot update itself: say so and link the release.
    Manual,
}

impl Install {
    pub fn current() -> Self {
        detect()
    }

    /// The release file this install updates from. `None` for a manual install, and for a CPU a
    /// package is not built for (there is no ARM64 setup.exe or AppImage).
    fn asset_name(self, version: &str) -> Option<String> {
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x86_64",
            "aarch64" => "arm64",
            _ => return None,
        };
        match self {
            Install::WindowsSetup if arch == "x86_64" => {
                Some(format!("refryt-{version}-windows-x86_64-setup.exe"))
            }
            Install::WindowsMsi => Some(format!("refryt-{version}-windows-{arch}.msi")),
            Install::AppImage if arch == "x86_64" => {
                Some(format!("refryt-{version}-x86_64.AppImage"))
            }
            _ => None,
        }
    }

    /// What "Update and restart" will do, or, for a manual install, how to update by hand.
    pub fn how(self) -> &'static str {
        match self {
            Install::WindowsSetup | Install::WindowsMsi => {
                "refryt downloads the installer, checks it, then closes and runs it. Windows may \
                 ask for permission. The new version starts once it is installed."
            }
            Install::AppImage => {
                "refryt downloads the new AppImage, checks it, puts it in place of this one and \
                 restarts."
            }
            Install::Manual => manual_hint(),
        }
    }
}

fn manual_hint() -> &'static str {
    if crate::flatpak::sandboxed() {
        "Run `flatpak update`, or update refryt from your software centre."
    } else if cfg!(target_os = "macos") {
        "Download the new disk image from the release page and drag Refryt to Applications again."
    } else if cfg!(target_os = "windows") {
        "Download the new zip from the release page and unpack it over this copy."
    } else {
        "Get it from the release page, or update refryt the way you installed it."
    }
}

#[cfg(target_os = "windows")]
fn detect() -> Install {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return Install::Manual;
    };
    if dir.join("unins000.exe").is_file() {
        return Install::WindowsSetup;
    }
    // The MSI installs to Program Files and nowhere else; a zip unpacked anywhere else, or a
    // `cargo run`, is not something msiexec should be pointed at.
    let installed = ["ProgramW6432", "ProgramFiles"]
        .iter()
        .filter_map(std::env::var_os)
        .any(|program_files| dir.starts_with(program_files));
    if installed {
        Install::WindowsMsi
    } else {
        Install::Manual
    }
}

#[cfg(target_os = "linux")]
fn detect() -> Install {
    if crate::flatpak::sandboxed() {
        return Install::Manual;
    }
    match std::env::var_os("APPIMAGE").map(PathBuf::from) {
        // An AppImage under /usr or /opt belongs to a package manager (the AUR package puts
        // it there): replacing it would fail, and would be wrong if it did not.
        Some(path) if path.is_file() && !path.starts_with("/usr") && !path.starts_with("/opt") => {
            Install::AppImage
        }
        _ => Install::Manual,
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn detect() -> Install {
    Install::Manual
}

/// A release newer than this build.
#[derive(Clone, Debug)]
pub struct Release {
    /// Without the tag's `v`.
    pub version: String,
    /// The release's page on GitHub: its notes, and every file.
    pub page: String,
    /// What this install updates from. `None` when it cannot update itself, or the release lacks
    /// the file or its checksums — which it does for a few minutes after it is published.
    download: Option<Download>,
}

#[derive(Clone, Debug)]
struct Download {
    name: String,
    url: String,
    size: u64,
    sums_url: String,
}

impl Release {
    /// Whether "Update and restart" can install it.
    pub fn installable(&self) -> bool {
        self.download.is_some()
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// The newest release, if it is newer than this build.
pub async fn check(http: &reqwest::Client) -> Result<Option<Release>> {
    let response = http
        .get(LATEST)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .context("could not reach GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub answered {}", response.status());
    }
    let bytes = response
        .bytes()
        .await
        .context("could not read GitHub's answer")?;
    let release: GhRelease =
        serde_json::from_slice(&bytes).context("could not read GitHub's answer")?;
    Ok(pick(release, &current_version(), Install::current()))
}

/// A test aid: `REFRYT_UPDATE_AS=0.1.11` makes the check treat this build as that version, so the
/// latest release is offered and the whole update can be tried against it. It can only make an
/// update appear; installing one still takes the click.
const UPDATE_AS: &str = "REFRYT_UPDATE_AS";

/// This build's version, or the one `REFRYT_UPDATE_AS` stands in for it.
pub fn current_version() -> String {
    std::env::var(UPDATE_AS)
        .ok()
        .filter(|v| parse_version(v).is_some())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

fn pick(release: GhRelease, current: &str, install: Install) -> Option<Release> {
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if !newer(&version, current) {
        return None;
    }
    let find = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name && a.browser_download_url.starts_with(RELEASES))
    };
    let download = install.asset_name(&version).and_then(|name| {
        let asset = find(&name)?;
        let sums = find(&format!("refryt-{version}-SHA256SUMS.txt"))?;
        Some(Download {
            url: asset.browser_download_url.clone(),
            size: asset.size,
            sums_url: sums.browser_download_url.clone(),
            name,
        })
    });
    Some(Release {
        version,
        page: if release.html_url.starts_with(RELEASES) {
            release.html_url
        } else {
            format!("{RELEASES}latest")
        },
        download,
    })
}

/// `major.minor.patch`, the only shape `release.sh` makes.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a later version than `current`. Anything unparseable is not.
pub fn newer(candidate: &str, current: &str) -> bool {
    matches!(
        (parse_version(candidate), parse_version(current)),
        (Some(a), Some(b)) if a > b
    )
}

/// The checksum `sums` (`sha256sum`'s output) lists for `name`.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        // `sha256sum` marks a file it read in binary mode with a `*`.
        (file.trim_start().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Download the release's file for this install and check it against the release's checksums,
/// reporting progress as 0–100. Returns where it was saved.
pub async fn download(
    http: &reqwest::Client,
    release: &Release,
    progress: impl Fn(f32),
) -> Result<PathBuf> {
    let d = release
        .download
        .as_ref()
        .ok_or_else(|| anyhow!("this release has nothing this copy can install"))?;
    let sums = http
        .get(&d.sums_url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("could not download the checksums")?
        .text()
        .await
        .context("could not download the checksums")?;
    let expected = expected_sum(&sums, &d.name)
        .ok_or_else(|| anyhow!("the checksums do not list {}", d.name))?;

    let target = staging_path(&d.name)?;
    let mut response = http
        .get(&d.url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("could not download {}", d.name))?;
    let total = response.content_length().unwrap_or(d.size).max(1);
    let mut file = tokio::fs::File::create(&target)
        .await
        .with_context(|| format!("could not write {}", target.display()))?;
    let (mut hasher, mut done, mut shown) = (Sha256::new(), 0_u64, -1.0_f32);
    let written: Result<()> = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("could not download {}", d.name))?
        {
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            done += chunk.len() as u64;
            // Whole percents: a repaint per chunk would be hundreds a second for nothing.
            let percent = (done as f32 / total as f32 * 100.0).min(100.0).floor();
            if percent > shown {
                shown = percent;
                progress(percent);
            }
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    drop(file);
    let checked = written.and_then(|()| {
        let actual = format!("{:x}", hasher.finalize());
        if actual == expected {
            Ok(())
        } else {
            Err(anyhow!("the download does not match its checksum"))
        }
    });
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&target);
        return Err(e);
    }
    Ok(target)
}

/// Where a download goes. The AppImage's goes beside it, so the swap is a rename on one file
/// system; an installer goes to the temp directory, where it can stay once it has run.
fn staging_path(name: &str) -> Result<PathBuf> {
    match Install::current() {
        Install::AppImage => {
            let appimage = std::env::var_os("APPIMAGE")
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("$APPIMAGE is not set"))?;
            let dir = appimage
                .parent()
                .ok_or_else(|| anyhow!("the AppImage has no folder"))?;
            Ok(dir.join(format!(".{name}.part")))
        }
        _ => {
            let dir = std::env::temp_dir().join("refryt-update");
            // An earlier update's installer is of no further use.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
            Ok(dir.join(name))
        }
    }
}

/// What to do once the window has closed: run the installer, or start the new AppImage.
enum Pending {
    #[cfg(target_os = "windows")]
    Installer { file: PathBuf, msi: bool },
    #[cfg(target_os = "linux")]
    Relaunch(PathBuf),
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Ready a downloaded and checked file to install: the AppImage is swapped in now, an installer
/// is left for [`finish`]. Blocking file work; the app closes once this returns `Ok`.
pub fn stage(file: &Path) -> Result<()> {
    let pending = prepare(file).inspect_err(|_| {
        let _ = std::fs::remove_file(file);
    })?;
    if let Ok(mut slot) = PENDING.lock() {
        *slot = Some(pending);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn prepare(file: &Path) -> Result<Pending> {
    match Install::current() {
        kind @ (Install::WindowsSetup | Install::WindowsMsi) => Ok(Pending::Installer {
            file: file.to_path_buf(),
            msi: kind == Install::WindowsMsi,
        }),
        _ => bail!("this copy of refryt cannot update itself"),
    }
}

#[cfg(target_os = "linux")]
fn prepare(file: &Path) -> Result<Pending> {
    use std::os::unix::fs::PermissionsExt;
    if Install::current() != Install::AppImage {
        bail!("this copy of refryt cannot update itself");
    }
    let appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("$APPIMAGE is not set"))?;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("could not make {} executable", file.display()))?;
    // A rename over a running AppImage is safe: the mounted image keeps the old file's inode
    // open until this process exits.
    std::fs::rename(file, &appimage)
        .with_context(|| format!("could not replace {}", appimage.display()))?;
    Ok(Pending::Relaunch(appimage))
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn prepare(_file: &Path) -> Result<Pending> {
    bail!("this copy of refryt cannot update itself")
}

/// Run what [`stage`] left, if anything. Called by `main` after the event loop has exited, so
/// the installer finds no running refryt holding its files.
pub fn finish() {
    let Some(pending) = PENDING.lock().ok().and_then(|mut slot| slot.take()) else {
        return;
    };
    if let Err(e) = spawn(pending) {
        tracing::warn!(error = %format!("{e:#}"), "could not start the update");
    }
}

#[cfg(target_os = "windows")]
fn spawn(pending: Pending) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Pending::Installer { file, msi } = pending;
    let exe = std::env::current_exe().context("could not find refryt.exe")?;
    // `start /wait`, not a plain call: it goes through ShellExecute, which is what lets the
    // installer raise its UAC prompt, and it waits for the install before the relaunch. The
    // relaunch comes from this unelevated cmd, so the new refryt is not elevated either (see the
    // `runasoriginaluser` note in `windows-installer.iss` for why that matters). A UAC prompt
    // that is refused just starts the old version again.
    let install = if msi {
        format!(
            r#"start "" /wait msiexec.exe /i "{}" /passive /norestart"#,
            file.display()
        )
    } else {
        format!(
            r#"start "" /wait "{}" /SILENT /SUPPRESSMSGBOXES /NORESTART"#,
            file.display()
        )
    };
    // `/s` strips exactly the outer pair of quotes, so the inner ones survive as written.
    let line = format!(r#"/d /s /c "{install} & start "" "{}"""#, exe.display());
    std::process::Command::new("cmd.exe")
        .raw_arg(line)
        // The updated refryt should be itself, not the version a test told this one to be.
        .env_remove(UPDATE_AS)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .context("could not start the installer")?;
    tracing::info!(installer = %file.display(), "update: installer started");
    Ok(())
}

#[cfg(target_os = "linux")]
fn spawn(pending: Pending) -> Result<()> {
    let Pending::Relaunch(appimage) = pending;
    // Wait (up to ten seconds) for this process to go, or the new one would find the instance
    // lock still held and only ask this one to show its window. The AppImage runtime's
    // variables belong to the old mount, which is about to disappear; the new runtime sets its
    // own.
    std::process::Command::new("sh")
        .arg("-c")
        .arg(
            r#"i=0; while kill -0 "$1" 2>/dev/null && [ "$i" -lt 50 ]; do sleep 0.2; i=$((i + 1)); done; exec "$2""#,
        )
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(&appimage)
        .env_remove(UPDATE_AS)
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("ARGV0")
        .env_remove("OWD")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("could not restart refryt")?;
    tracing::info!(appimage = %appimage.display(), "update: restarting");
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn spawn(pending: Pending) -> Result<()> {
    match pending {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> GhRelease {
        GhRelease {
            tag_name: tag.to_owned(),
            html_url: format!("https://github.com/gridoe/refryt/releases/tag/{tag}"),
            assets: assets
                .iter()
                .map(|name| GhAsset {
                    name: (*name).to_owned(),
                    browser_download_url: format!("{RELEASES}download/{tag}/{name}"),
                    size: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("0.1.13", "0.1.12"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.12", "0.1.12"));
        assert!(!newer("0.1.11", "0.1.12"));
        assert!(!newer("0.1", "0.0.1"));
        assert!(!newer("0.1.13-rc1", "0.1.12"));
        assert!(!newer("0.1.13.1", "0.1.12"));
    }

    #[test]
    fn an_older_or_equal_release_is_no_update() {
        assert!(pick(release("v0.1.12", &[]), "0.1.12", Install::Manual).is_none());
        assert!(pick(release("v0.1.11", &[]), "0.1.12", Install::Manual).is_none());
    }

    #[test]
    fn a_manual_install_gets_the_notice_only() {
        let r = pick(release("v0.1.13", &[]), "0.1.12", Install::Manual).unwrap();
        assert_eq!(r.version, "0.1.13");
        assert!(!r.installable());
    }

    #[test]
    fn an_update_needs_its_file_and_the_checksums() {
        if std::env::consts::ARCH != "x86_64" {
            return;
        }
        let setup = "refryt-0.1.13-windows-x86_64-setup.exe";
        let sums = "refryt-0.1.13-SHA256SUMS.txt";
        let without_sums = pick(
            release("v0.1.13", &[setup]),
            "0.1.12",
            Install::WindowsSetup,
        );
        assert!(!without_sums.unwrap().installable());

        let both = pick(
            release("v0.1.13", &[setup, sums]),
            "0.1.12",
            Install::WindowsSetup,
        )
        .unwrap();
        assert!(both.installable());
        assert_eq!(both.download.unwrap().name, setup);

        let msi = pick(
            release("v0.1.13", &[setup, sums]),
            "0.1.12",
            Install::WindowsMsi,
        )
        .unwrap();
        assert!(!msi.installable(), "the MSI must not pick the setup.exe");
    }

    #[test]
    fn files_outside_the_projects_releases_are_not_downloaded() {
        if std::env::consts::ARCH != "x86_64" {
            return;
        }
        let setup = "refryt-0.1.13-windows-x86_64-setup.exe";
        let sums = "refryt-0.1.13-SHA256SUMS.txt";
        let mut foreign = release("v0.1.13", &[setup, sums]);
        foreign.assets[0].browser_download_url = format!("https://example.invalid/{setup}");
        foreign.html_url = "https://example.invalid/release".to_owned();
        let r = pick(foreign, "0.1.12", Install::WindowsSetup).unwrap();
        assert!(!r.installable());
        assert_eq!(r.page, "https://github.com/gridoe/refryt/releases/latest");
    }

    #[test]
    fn checksums_are_found_by_file_name() {
        let sums = "AAAA  refryt-0.1.13-x86_64.AppImage\n\
                    bbbb *refryt-0.1.13-windows-x86_64.msi\n";
        assert_eq!(
            expected_sum(sums, "refryt-0.1.13-x86_64.AppImage").as_deref(),
            Some("aaaa")
        );
        assert_eq!(
            expected_sum(sums, "refryt-0.1.13-windows-x86_64.msi").as_deref(),
            Some("bbbb")
        );
        assert_eq!(expected_sum(sums, "refryt-0.1.13-x86_64.flatpak"), None);
    }
}
