//! One instance per profile.
//!
//! A plain second launch — the desktop entry again, the AppImage file, `refryt` on `PATH` —
//! should not give a second window and a second player over the same account. The first process
//! to start takes a lock file; a later one asks it over a socket to show its window and exits.
//! Bringing the window forward is the same best-effort request the tray's Show makes
//! (`App::show_window`): real focus on X11 and Windows, an activation request that KWin may
//! honor on Wayland.
//!
//! The claim is deliberately not global: the socket is keyed by the config directory, so
//! isolated profiles — the perf script, the headless harness, a second account — run beside the
//! real app instead of colliding with it. A launch that only exists to be driven by a script
//! (`--page`, `--search`, the test aids) is not a user clicking the app again; those bypass the
//! claim entirely, which `main` decides, not this module.
//!
//! On macOS the lock and socket live in the per-user `$TMPDIR`, since macOS has no
//! `$XDG_RUNTIME_DIR`. `$TMPDIR` is private to the user and short enough for a socket path, which
//! has a length limit of about 100 bytes.
//!
//! On Windows a named mutex is the claim and a named event the request: the second launch raises
//! the window itself, then sets the event so the running instance knows it is shown.
//!
//! Failure to set any of this up is never fatal: the app then runs without a guard rather than
//! refusing to start.

/// Whether this process should run, or a running instance was told to show its window.
pub enum Claim {
    /// No other instance: run.
    Primary,
    /// Another instance is already up, and has been asked to show itself. Exit.
    Secondary,
}

/// Linux and macOS share one implementation: both are Unix systems with `flock`-style file locks
/// and Unix domain sockets. Only where the two files live differs (see `paths`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::collections::hash_map::DefaultHasher;
    use std::fs::{File, TryLockError};
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Claim;

    /// A second launch asked for the window. The listener thread sets it; [`poll`] takes it.
    static SHOW: AtomicBool = AtomicBool::new(false);
    /// The UI's context, installed by [`attach`] once the app is built — the listener thread
    /// starts before that, so a request landing early simply waits for the first frame.
    static CTX: Mutex<Option<egui::Context>> = Mutex::new(None);
    /// The claim itself: held for the process lifetime, released by the OS on exit, crash
    /// included. A leftover lock file with no live lock is simply free to take, so nothing has to
    /// detect or clean up a stale one.
    static LOCK: Mutex<Option<File>> = Mutex::new(None);

    /// How many `accept()` errors in a row before the listener stops trying.
    const MAX_ACCEPT_ERRORS: u32 = 5;

    /// The lock and socket paths: `$XDG_RUNTIME_DIR` on Linux, the per-user `$TMPDIR` on macOS
    /// (which has no runtime dir; `TMPDIR` is private to the user and short enough for a socket
    /// path). The profile's config directory is hashed into both names so isolated profiles do
    /// not share one.
    fn paths() -> Option<(PathBuf, PathBuf)> {
        let config = directories::BaseDirs::new()?.config_dir().to_path_buf();
        #[cfg(target_os = "linux")]
        let runtime = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
        #[cfg(target_os = "macos")]
        let runtime = std::env::temp_dir();
        Some((
            runtime.join(lock_name(&config)),
            runtime.join(socket_name(&config)),
        ))
    }

    /// The names for a given config directory. Pure, so the profile separation is testable.
    fn lock_name(config: &Path) -> String {
        format!("refryt-{:x}.lock", profile_hash(config))
    }

    fn socket_name(config: &Path) -> String {
        format!("refryt-{:x}.sock", profile_hash(config))
    }

    fn profile_hash(config: &Path) -> u64 {
        let mut hasher = DefaultHasher::new();
        config.hash(&mut hasher);
        hasher.finish()
    }

    pub fn claim() -> Claim {
        let Some((lock, socket)) = paths() else {
            return Claim::Primary;
        };
        claim_at(&lock, &socket)
    }

    /// [`claim`] with the paths injected, so the profile split and the lock are testable.
    ///
    /// The lock decides who is primary; the socket only carries the "show" message. Deciding on
    /// the socket's existence instead cannot be made race-free: between a failed connect and a
    /// bind, another process may have bound, and unlinking what looks stale then deletes the
    /// winner's live socket. A lock is atomic, and the OS drops it when the holder dies.
    fn claim_at(lock: &Path, socket: &Path) -> Claim {
        let file = match File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock)
        {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(
                    path = %lock.display(),
                    error = %e,
                    "single instance: no lock file; running without a guard"
                );
                return Claim::Primary;
            }
        };
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                show_running(socket);
                return Claim::Secondary;
            }
            Err(TryLockError::Error(e)) => {
                tracing::warn!(
                    path = %lock.display(),
                    error = %e,
                    "single instance: could not lock; running without a guard"
                );
                return Claim::Primary;
            }
        }
        // The lock is ours, so any socket file still sitting there is stale. Only the lock
        // holder ever removes it, which is what keeps the unlink from racing another process.
        let _ = std::fs::remove_file(socket);
        let listener = match UnixListener::bind(socket) {
            Ok(listener) => listener,
            Err(e) => {
                tracing::warn!(
                    path = %socket.display(),
                    error = %e,
                    "single instance: could not listen; the window cannot be raised from outside"
                );
                if let Ok(mut slot) = LOCK.lock() {
                    *slot = Some(file);
                }
                return Claim::Primary;
            }
        };
        if let Ok(mut slot) = LOCK.lock() {
            *slot = Some(file);
        }
        // A blocking thread, not a non-blocking poll: an idle egui app paints no frames, so
        // nothing would look at the socket until some unrelated event scheduled one. This is
        // the same queue-and-wake shape `tray::inject` uses.
        std::thread::spawn(move || serve(listener.incoming()));
        Claim::Primary
    }

    /// Ask the running instance to show its window, best-effort. A connect landing in the
    /// primary's own lock-to-bind window is lost, but that primary is about to show its window
    /// anyway; there is nothing to retry for.
    fn show_running(socket: &Path) {
        if let Ok(mut stream) = UnixStream::connect(socket) {
            let _ = stream.write_all(b"show\n");
        }
    }

    /// The listener loop. One transient `accept()` error (fd exhaustion; `EINTR` is already
    /// retried inside std) must not kill the guard for the rest of the session, but a listener
    /// that keeps failing should stop rather than spin.
    fn serve<I: IntoIterator<Item = std::io::Result<UnixStream>>>(streams: I) {
        let mut errors = 0u32;
        for stream in streams {
            let mut stream = match stream {
                Ok(stream) => {
                    errors = 0;
                    stream
                }
                Err(e) => {
                    errors += 1;
                    if errors >= MAX_ACCEPT_ERRORS {
                        tracing::error!(
                            error = %e,
                            errors,
                            "single instance: accept keeps failing; the listener is stopping"
                        );
                        return;
                    }
                    tracing::warn!(error = %e, errors, "single instance: accept failed; continuing");
                    continue;
                }
            };
            let mut buf = [0u8; 8];
            let n = stream.read(&mut buf).unwrap_or(0);
            if buf[..n].starts_with(b"show") {
                SHOW.store(true, Ordering::SeqCst);
                if let Ok(ctx) = CTX.lock()
                    && let Some(ctx) = ctx.as_ref()
                {
                    ctx.request_repaint();
                }
            }
        }
    }

    /// Hand the listener thread somewhere to send its wake-up. Called once the app exists; a
    /// request that arrived before this is still in [`SHOW`] for the first frame to take.
    pub fn attach(ctx: &egui::Context) {
        if let Ok(mut slot) = CTX.lock() {
            *slot = Some(ctx.clone());
        }
    }

    /// Whether a second launch is asking for the window, drained once a frame.
    pub fn poll() -> bool {
        SHOW.swap(false, Ordering::SeqCst)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A path unique to the test process and the test name: the lock lives in a process-wide
        /// static, so tests must not share one.
        fn temp_path(name: &str) -> PathBuf {
            std::env::temp_dir().join(format!("refryt-test-{}-{name}", std::process::id()))
        }

        /// `SHOW` is process-wide and the listener sets it asynchronously, so every test that
        /// can touch it runs under this lock.
        static SHOW_TESTS: Mutex<()> = Mutex::new(());

        fn serial() -> std::sync::MutexGuard<'static, ()> {
            SHOW_TESTS.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Two profiles get two sockets, so a test run beside the real app cannot trip it.
        #[test]
        fn different_profiles_get_different_sockets() {
            let a = socket_name(Path::new("/home/user/.config/refryt"));
            let b = socket_name(Path::new("/tmp/scratch/refryt"));
            assert_ne!(a, b);
            assert!(a.starts_with("refryt-") && a.ends_with(".sock"));
            assert_ne!(lock_name(Path::new("/home/user/.config/refryt")), a);
        }

        /// The primitive the claim rests on: a second handle cannot take the same lock.
        #[test]
        fn a_second_lock_on_the_same_file_blocks() {
            let path = temp_path("lock-blocking");
            let first = File::options()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            first.try_lock().unwrap();
            let second = File::options().read(true).write(true).open(&path).unwrap();
            assert!(matches!(second.try_lock(), Err(TryLockError::WouldBlock)));
            drop(first);
            assert!(second.try_lock().is_ok());
            let _ = std::fs::remove_file(&path);
        }

        /// The bug this replaces: a second claim used to race the first into a second Primary.
        #[test]
        fn a_second_claim_becomes_secondary() {
            let _serial = serial();
            let lock = temp_path("second-claim.lock");
            let socket = temp_path("second-claim.sock");
            assert!(matches!(claim_at(&lock, &socket), Claim::Primary));
            assert!(matches!(claim_at(&lock, &socket), Claim::Secondary));
            let _ = std::fs::remove_file(&socket);
            let _ = std::fs::remove_file(&lock);
        }

        /// One transient error must not end the loop: the message after it still lands.
        #[test]
        fn a_transient_accept_error_does_not_kill_the_listener() {
            let _serial = serial();
            let (mut client, server) = UnixStream::pair().unwrap();
            let writer = std::thread::spawn(move || {
                let _ = client.write_all(b"show\n");
            });
            SHOW.store(false, Ordering::SeqCst);
            serve([Err(std::io::Error::other("transient")), Ok(server)]);
            writer.join().unwrap();
            assert!(SHOW.swap(false, Ordering::SeqCst));
        }

        /// A listener that keeps failing stops instead of spinning forever.
        #[test]
        fn the_listener_gives_up_after_a_streak_of_errors() {
            let _serial = serial();
            let (_client, server) = UnixStream::pair().unwrap();
            SHOW.store(false, Ordering::SeqCst);
            let errors = std::iter::repeat_with(|| Err(std::io::Error::other("broken")))
                .take(MAX_ACCEPT_ERRORS as usize);
            serve(errors.chain(std::iter::once(Ok(server))));
            assert!(
                !SHOW.swap(false, Ordering::SeqCst),
                "the stream after the streak must not be served"
            );
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt as _;
    use std::ptr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    use super::Claim;

    /// The mutex handle, for the process lifetime. The OS releases it when the process ends.
    static MUTEX: Mutex<Option<isize>> = Mutex::new(None);

    /// The event a second launch sets once it has raised the window, same scope as the mutex.
    const SHOW_EVENT: &str = "Local\\refryt-show";

    /// Set by the event's waiter thread, drained by [`poll`].
    static SHOW: AtomicBool = AtomicBool::new(false);

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    /// A local (per-session) named mutex: one instance per login, the same scope the Run key has.
    pub fn claim() -> Claim {
        let name = wide("Local\\refryt-single-instance");
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle == 0 {
            return Claim::Primary;
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            show_existing();
            return Claim::Secondary;
        }
        if let Ok(mut slot) = MUTEX.lock() {
            *slot = Some(handle);
        }
        Claim::Primary
    }

    /// Ask the running instance's window to come forward. Finding it by its fixed title is how
    /// the window is reached: the window title never changes.
    ///
    /// This process raises the window itself, because the launch the user just made is what
    /// holds the right to take the foreground; the running instance may not get it. Then it sets
    /// [`SHOW_EVENT`] so the running instance takes the show through `App::show_window` as well.
    /// Without that, winit still records the window as hidden and the app still thinks it is in
    /// the tray, so the next titlebar X sends a hide winit sees as no change, and nothing happens.
    fn show_existing() {
        let title = wide("Refryt");
        let hwnd = unsafe { FindWindowW(ptr::null(), title.as_ptr()) };
        if hwnd != 0 {
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                SetForegroundWindow(hwnd);
            }
        }
        let name = wide(SHOW_EVENT);
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, name.as_ptr()) };
        if event != 0 {
            unsafe {
                SetEvent(event);
                CloseHandle(event);
            }
        }
    }

    /// Wait on [`SHOW_EVENT`] in a thread of its own, flag each set for [`poll`] and wake the UI.
    /// Auto-reset, so each second launch is one show. The handle lives as long as the process.
    pub fn attach(ctx: &egui::Context) {
        let name = wide(SHOW_EVENT);
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, name.as_ptr()) };
        if event == 0 {
            return;
        }
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                SHOW.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            }
        });
    }

    /// Whether a second launch showed the window, drained once a frame.
    pub fn poll() -> bool {
        SHOW.swap(false, Ordering::SeqCst)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::{attach, claim, poll};

#[cfg(target_os = "windows")]
pub use windows::{attach, claim, poll};
