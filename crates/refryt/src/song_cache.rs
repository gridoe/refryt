//! Songs kept on disk as they stream, so they replay instantly and still play if the connection
//! drops.
//!
//! Off by default. The stream proxy writes each song as it plays (no second download); this keeps
//! track of the finished files, up to a size, and plays them before asking YouTube. Unless the
//! cache is set to outlive the app, the folder is emptied when it closes, and at startup after a
//! crash.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// What the cache remembers about its files, saved beside them when it outlives the app.
const INDEX: &str = "index.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Kept {
    file: String,
    bytes: u64,
    /// When it was last played or kept, as a count, for dropping the oldest.
    used: u64,
    #[serde(default)]
    loudness: Option<f32>,
}

#[derive(Default, Serialize, Deserialize)]
struct Inner {
    songs: HashMap<String, Kept>,
    /// A music video's id → the audio track played for it.
    aliases: HashMap<String, String>,
    #[serde(skip)]
    loudness: HashMap<String, Option<f32>>,
    #[serde(skip)]
    bytes: u64,
    clock: u64,
}

pub struct SongCache {
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
    limit: Mutex<u64>,
}

impl SongCache {
    /// The cache in the app's cache folder. Nothing on disk is touched until [`Self::open`].
    pub fn new() -> Self {
        let dir = directories::ProjectDirs::from("", "", "refryt")
            .map(|dirs| dirs.cache_dir().join("song-cache"));
        Self::with(dir, u64::MAX)
    }

    fn with(dir: Option<PathBuf>, limit: u64) -> Self {
        Self {
            dir,
            inner: Mutex::new(Inner::default()),
            limit: Mutex::new(limit),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// At startup: pick up what the last run kept when the cache outlives the app, and otherwise
    /// empty whatever is there (left by a crash, or by a run that kept it before this was off).
    pub fn open(&self, keep: bool) {
        let Some(dir) = &self.dir else { return };
        if !keep {
            empty_later(dir);
            return;
        }
        let saved: Inner = std::fs::read(dir.join(INDEX))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut inner = Inner {
            aliases: saved.aliases,
            clock: saved.clock,
            ..Inner::default()
        };
        let files = std::fs::read_dir(dir).into_iter().flatten().flatten();
        for entry in files {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                // Half written when the app stopped.
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
            let Some((id, _)) = name.split_once('.').filter(|_| name != INDEX) else {
                continue;
            };
            let Ok(bytes) = entry.metadata().map(|m| m.len()) else {
                continue;
            };
            let kept = saved.songs.get(id).cloned().map_or(
                Kept {
                    file: name.clone(),
                    bytes,
                    used: 0,
                    loudness: None,
                },
                |old| Kept { bytes, ..old },
            );
            inner.loudness.insert(id.to_owned(), kept.loudness);
            inner.bytes += bytes;
            inner.songs.insert(id.to_owned(), kept);
        }
        *self.lock() = inner;
        self.trim();
    }

    /// The most to keep, in bytes. The songs played longest ago go first.
    pub fn set_limit(&self, bytes: u64) {
        *self.limit.lock().unwrap_or_else(|e| e.into_inner()) = bytes;
        self.trim();
    }

    /// The folder to write into, made if needed.
    pub fn folder(&self) -> Option<PathBuf> {
        let dir = self.dir.clone()?;
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    pub fn note_loudness(&self, id: &str, db: Option<f32>) {
        self.lock().loudness.insert(id.to_owned(), db);
    }

    /// `track_id` played as the audio of `audio_id`.
    pub fn note_alias(&self, track_id: &str, audio_id: &str) {
        if track_id != audio_id {
            self.lock()
                .aliases
                .insert(track_id.to_owned(), audio_id.to_owned());
        }
    }

    /// A song has been written in full.
    pub fn add(&self, id: &str, path: &Path, bytes: u64) {
        let Some(file) = path.file_name().map(|f| f.to_string_lossy().into_owned()) else {
            return;
        };
        {
            let mut inner = self.lock();
            inner.clock += 1;
            let kept = Kept {
                file,
                bytes,
                used: inner.clock,
                loudness: inner.loudness.get(id).copied().flatten(),
            };
            if let Some(old) = inner.songs.insert(id.to_owned(), kept) {
                inner.bytes -= old.bytes;
            }
            inner.bytes += bytes;
        }
        self.trim();
    }

    fn trim(&self) {
        let limit = *self.limit.lock().unwrap_or_else(|e| e.into_inner());
        let mut inner = self.lock();
        while inner.bytes > limit {
            let Some(oldest) = inner
                .songs
                .iter()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            if let Some(gone) = inner.songs.remove(&oldest) {
                inner.bytes -= gone.bytes;
                if let Some(dir) = &self.dir {
                    let _ = std::fs::remove_file(dir.join(gone.file));
                }
            }
        }
    }

    /// The kept file for a song and its loudness, if it is here.
    pub fn get(&self, id: &str) -> Option<(PathBuf, Option<f32>)> {
        let dir = self.dir.as_ref()?;
        let mut inner = self.lock();
        let id = inner
            .aliases
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_owned());
        inner.clock += 1;
        let clock = inner.clock;
        let kept = inner.songs.get_mut(&id)?;
        let path = dir.join(&kept.file);
        if !path.is_file() {
            // Deleted by hand: forget it, so it stops counting towards the size.
            if let Some(gone) = inner.songs.remove(&id) {
                inner.bytes = inner.bytes.saturating_sub(gone.bytes);
            }
            return None;
        }
        kept.used = clock;
        Some((path, kept.loudness))
    }

    /// Forget every song and delete the files, in the background.
    pub fn clear(&self) {
        *self.lock() = Inner::default();
        if let Some(dir) = &self.dir {
            empty_later(dir);
        }
    }

    /// The app is closing: save what is kept for next time, or delete it now.
    pub fn close(&self, keep: bool) {
        let Some(dir) = &self.dir else { return };
        if keep {
            let inner = self.lock();
            if dir.is_dir()
                && let Ok(bytes) = serde_json::to_vec(&*inner)
            {
                let _ = crate::persist::write_json_atomic(&dir.join(INDEX), &bytes);
            }
        } else {
            *self.lock() = Inner::default();
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Default for SongCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Move the folder aside at once, so new files can start, and delete it off the calling thread.
fn empty_later(dir: &Path) {
    let aside = dir.with_file_name(format!(
        "song-cache.old-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let moved = dir.exists() && std::fs::rename(dir, &aside).is_ok();
    let parent = dir.parent().map(Path::to_owned);
    std::thread::spawn(move || {
        if moved {
            let _ = std::fs::remove_dir_all(&aside);
        }
        // Earlier runs' leftovers that were moved aside but never deleted.
        let Some(parent) = parent else { return };
        for entry in std::fs::read_dir(parent).into_iter().flatten().flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("song-cache.old-")
            {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("refryt-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_kept_song_plays_back_with_its_loudness() {
        let dir = scratch("get");
        let cache = SongCache::with(Some(dir.clone()), 1000);
        cache.note_loudness("a", Some(-3.0));
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        assert_eq!(cache.get("a"), Some((dir.join("a.webm"), Some(-3.0))));
        assert_eq!(cache.get("b"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_song_played_longest_ago_goes_first_past_the_limit() {
        let dir = scratch("limit");
        let cache = SongCache::with(Some(dir.clone()), 25);
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        cache.add("b", &file(&dir, "b.webm", 10), 10);
        cache.get("a");
        cache.add("c", &file(&dir, "c.webm", 10), 10);
        assert!(cache.get("b").is_none(), "b was played longest ago");
        assert!(!dir.join("b.webm").exists());
        assert!(cache.get("a").is_some() && cache.get("c").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_song_deleted_by_hand_stops_taking_room() {
        let dir = scratch("deleted");
        let cache = SongCache::with(Some(dir.clone()), 20);
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        cache.add("b", &file(&dir, "b.webm", 10), 10);
        std::fs::remove_file(dir.join("a.webm")).unwrap();
        assert_eq!(cache.get("a"), None);
        cache.add("c", &file(&dir, "c.webm", 10), 10);
        assert!(cache.get("b").is_some(), "a's room was free for c");
        assert!(cache.get("c").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_smaller_limit_trims_at_once() {
        let dir = scratch("shrink");
        let cache = SongCache::with(Some(dir.clone()), 100);
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        cache.add("b", &file(&dir, "b.webm", 10), 10);
        cache.set_limit(10);
        assert!(cache.get("a").is_none() && cache.get("b").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_music_video_finds_the_audio_kept_for_it() {
        let dir = scratch("alias");
        let cache = SongCache::with(Some(dir.clone()), 1000);
        cache.note_alias("video", "audio");
        cache.add("audio", &file(&dir, "audio.webm", 10), 10);
        assert!(cache.get("video").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn closing_without_keeping_removes_the_folder() {
        let dir = scratch("close");
        let cache = SongCache::with(Some(dir.clone()), 1000);
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        cache.close(false);
        assert!(!dir.exists());
        assert!(cache.get("a").is_none());
    }

    /// Kept after closing, the next run finds the songs, their loudness and the aliases, and
    /// drops a file left half written.
    #[test]
    fn a_kept_cache_comes_back_at_the_next_start() {
        let dir = scratch("keep");
        let cache = SongCache::with(Some(dir.clone()), 1000);
        cache.note_loudness("a", Some(-2.0));
        cache.note_alias("video", "a");
        cache.add("a", &file(&dir, "a.webm", 10), 10);
        file(&dir, ".b.part", 5);
        cache.close(true);

        let next = SongCache::with(Some(dir.clone()), 1000);
        next.open(true);
        assert_eq!(next.get("a"), Some((dir.join("a.webm"), Some(-2.0))));
        assert!(next.get("video").is_some());
        assert!(!dir.join(".b.part").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file the index never heard of (the app crashed before saving it) is picked up too.
    #[test]
    fn a_song_kept_before_a_crash_is_found_without_the_index() {
        let dir = scratch("crash");
        file(&dir, "a.webm", 10);
        let next = SongCache::with(Some(dir.clone()), 1000);
        next.open(true);
        assert_eq!(next.get("a"), Some((dir.join("a.webm"), None)));
        let _ = std::fs::remove_dir_all(dir);
    }
}
