//! Persisted playback: the queue, the current track, and the transport, so a relaunch picks up
//! where it left off.
//!
//! `Track` holds `Arc<str>`/`Arc<Path>`, so rather than put serde on the wire model this keeps
//! its own small plain-data shape and maps in and out. The file is best-effort: a missing or
//! unreadable `playback.json` simply means "nothing to restore".

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::app::RepeatMode;
use crate::model::{ArtistLink, ChartTrend, LikeStatus, Track};

pub fn path() -> Result<PathBuf> {
    let dirs =
        ProjectDirs::from("", "", "refryt").context("could not find the config directory")?;
    Ok(dirs.config_dir().join("playback.json"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatKind {
    Off,
    All,
    One,
}

impl From<RepeatMode> for RepeatKind {
    fn from(mode: RepeatMode) -> Self {
        match mode {
            RepeatMode::Off => Self::Off,
            RepeatMode::All => Self::All,
            RepeatMode::One => Self::One,
        }
    }
}

impl From<RepeatKind> for RepeatMode {
    fn from(kind: RepeatKind) -> Self {
        match kind {
            RepeatKind::Off => Self::Off,
            RepeatKind::All => Self::All,
            RepeatKind::One => Self::One,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedArtist {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedTrack {
    pub video_id: String,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<PersistedArtist>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub wide: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

impl PersistedTrack {
    pub fn from_track(t: &Track) -> Self {
        Self {
            video_id: t.video_id.clone(),
            title: t.title.clone(),
            artists: t
                .artists
                .iter()
                .map(|a| PersistedArtist {
                    name: a.name.clone(),
                    id: a.id.clone(),
                })
                .collect(),
            album: t.album.clone(),
            album_id: t.album_id.clone(),
            duration: t.duration,
            thumbnail: t.thumbnail.as_deref().map(str::to_owned),
            wide: t.wide,
            local_path: t
                .local_path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }

    pub fn into_track(self) -> Track {
        Track {
            video_id: self.video_id,
            title: self.title,
            artists: self
                .artists
                .into_iter()
                .map(|a| ArtistLink {
                    name: a.name,
                    id: a.id,
                })
                .collect(),
            album: self.album,
            album_id: self.album_id,
            duration: self.duration,
            plays: None,
            explicit: false,
            // Ratings and library membership are not restored: they belong to the account and
            // are re-read from YouTube the moment a page loads, so a stale copy here would only
            // be overwritten (or, worse, briefly disagree).
            like: LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            wide: self.wide,
            rank: None,
            trend: ChartTrend::Unknown,
            thumbnail: self.thumbnail.map(Arc::from),
            local_path: self
                .local_path
                .map(|p| Arc::<std::path::Path>::from(PathBuf::from(p))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub queue: Vec<PersistedTrack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<usize>,
    #[serde(default)]
    pub position: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<RepeatKind>,
    #[serde(default)]
    pub shuffle: bool,
    /// The last audible volume: muting leaves it as it was, to come back to.
    #[serde(default)]
    pub volume: f64,
    /// Whether the volume was muted. An older file without it reads as not.
    #[serde(default)]
    pub muted: bool,
    /// Whether the in-app video view was open. Only ever restored for a track that has a video.
    #[serde(default)]
    pub video_open: bool,
    /// The queue's radio: which songs were the listener's, how it continues, and its filters.
    /// An older file without it restores the whole queue as chosen, with a fresh radio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio: Option<PersistedRadio>,
}

/// [`crate::app::Radio`] as written to disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PersistedRadio {
    pub seed: String,
    pub chosen: Vec<String>,
    /// Not fetched yet: seeded from the queue's end once it plays.
    #[serde(default)]
    pub pending: bool,
    /// The next page's token while it flows. A stale one is no loss: following it falls back to
    /// a fresh radio from the song playing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<PersistedFilter>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedFilter {
    pub label: String,
    pub playlist_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(default)]
    pub selected: bool,
}

/// Write the store, creating the config dir if needed.
pub fn save(store: &Store) -> Result<()> {
    let path = path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    // Compact: nobody reads it by hand, and it is rewritten on every skip.
    let bytes = serde_json::to_vec(store).context("could not encode the playback state")?;
    crate::persist::write_json_atomic(&path, &bytes)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Read the store. Anything unreadable reads as "nothing to restore".
pub fn load() -> Option<Store> {
    let path = path().ok()?;
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Track {
        Track {
            video_id: "abc12345678".into(),
            title: "Everything In Its Right Place".into(),
            artists: vec![
                ArtistLink {
                    name: "Radiohead".into(),
                    id: Some("UCr_iyUANcn9OX_yy9piYoLw".into()),
                },
                ArtistLink::plain("Guest"),
            ],
            album: Some("Kid A".into()),
            album_id: Some("MPREb_xsmdKhqhQrG".into()),
            duration: Some(251),
            plays: Some("100M plays".into()),
            explicit: true,
            like: LikeStatus::Like,
            in_library: true,
            library_add: Some(Arc::from("add-token")),
            library_remove: None,
            set_video_id: Some(Arc::from("set-id")),
            wide: true,
            rank: Some("3".into()),
            trend: ChartTrend::Up,
            thumbnail: Some(Arc::from("https://example/cover.jpg")),
            local_path: Some(Arc::<std::path::Path>::from(PathBuf::from("/music/a.opus"))),
        }
    }

    /// The fields worth keeping survive; the account state that is deliberately dropped does not.
    #[test]
    fn a_track_survives_the_round_trip() {
        let t = sample();
        let back = PersistedTrack::from_track(&t).into_track();
        assert_eq!(back.video_id, t.video_id);
        assert_eq!(back.title, t.title);
        assert_eq!(back.album, t.album);
        assert_eq!(back.album_id, t.album_id);
        assert_eq!(back.duration, t.duration);
        assert_eq!(back.thumbnail.as_deref(), t.thumbnail.as_deref());
        assert_eq!(back.wide, t.wide);
        assert_eq!(back.artists.len(), 2);
        assert_eq!(back.artists[0].name, "Radiohead");
        assert_eq!(
            back.artists[0].id.as_deref(),
            Some("UCr_iyUANcn9OX_yy9piYoLw")
        );
        assert_eq!(back.artists[1].id, None);
        assert_eq!(back.local_path.as_deref(), t.local_path.as_deref());
        // Not restored: this is account state YouTube is the authority on.
        assert_eq!(back.like, LikeStatus::Indifferent);
        assert!(!back.in_library);
        assert_eq!(back.plays, None);
    }

    #[test]
    fn repeat_modes_map_both_ways() {
        for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            assert_eq!(RepeatMode::from(RepeatKind::from(mode)), mode);
        }
    }

    /// `video_open` rides the store so a relaunch comes back in the video view, and an older
    /// file without the field still reads.
    #[test]
    fn the_video_flag_round_trips_and_defaults_to_closed() {
        let mut store: Store = serde_json::from_str(
            r#"{"version":1,"queue":[],"position":0.0,"shuffle":false,"volume":70.0}"#,
        )
        .unwrap();
        assert!(
            !store.video_open,
            "an older store must not restore the view"
        );

        store.video_open = true;
        let json = serde_json::to_string(&store).unwrap();
        let back: Store = serde_json::from_str(&json).unwrap();
        assert!(back.video_open);
    }

    /// A mute rides the store beside the volume it will come back to, and an older file
    /// without the field reads as not muted.
    #[test]
    fn a_mute_round_trips_and_defaults_to_off() {
        let mut store: Store = serde_json::from_str(
            r#"{"version":1,"queue":[],"position":0.0,"shuffle":false,"volume":40.0}"#,
        )
        .unwrap();
        assert!(!store.muted);
        store.muted = true;
        let back: Store = serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        assert!(back.muted);
        assert_eq!(back.volume, 40.0);
    }
}
