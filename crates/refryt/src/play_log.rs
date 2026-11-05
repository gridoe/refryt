//! The songs you listened to, kept on this machine for the Stats page.
//!
//! One JSON object a line, only ever appended to, so a play costs one small write however long
//! the log has grown. A line cut short by a crash is skipped on reading, and the next append
//! starts on a line of its own, so it costs that one play and nothing else.
//!
//! [`Listening`] decides what counts: a song is logged once you have listened to 30 seconds of
//! it, or half of it if that is shorter. Pauses add nothing and neither does seeking forward.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{ArtistLink, ChartTrend, LikeStatus, Track};

/// How long a song has to be listened to before it counts, at most.
const COUNTS_AFTER_SECS: f64 = 30.0;
/// The largest step in position still taken for listening rather than a seek. Generous, because
/// a hidden window is only looked at about once a second and can fall behind.
const MAX_STEP_SECS: f64 = 10.0;
/// How close to its end a song has to be for going back to its start to be another listen.
const NEAR_END_SECS: f64 = 5.0;
/// How close to the start a song has to be, coming from its end, to have looped.
const RESTART_SECS: f64 = 5.0;

/// Where a play was seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Played here, so the seconds listened are measured.
    #[default]
    App,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedArtist {
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "i", default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// One listen. Short field names, since they repeat on every line of a file that can get long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Play {
    #[serde(rename = "v")]
    pub video_id: String,
    #[serde(rename = "t")]
    pub title: String,
    #[serde(rename = "a", default)]
    pub artists: Vec<LoggedArtist>,
    #[serde(rename = "al", default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(rename = "ai", default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(rename = "d", default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
    /// A music video, whose still is 16:9.
    #[serde(rename = "w", default, skip_serializing_if = "std::ops::Not::not")]
    pub wide: bool,
    /// A file on disk, whose `video_id` is its path.
    #[serde(rename = "f", default, skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// When it started, in seconds since the Unix epoch.
    pub at: i64,
    /// How much of it was listened to, in seconds.
    #[serde(rename = "l", default)]
    pub listened: u64,
    #[serde(rename = "s", default)]
    pub source: Source,
}

impl Play {
    fn new(track: &Track, at: i64, listened: f64) -> Self {
        Self {
            video_id: track.video_id.clone(),
            title: track.title.clone(),
            artists: track
                .artists
                .iter()
                .map(|a| LoggedArtist {
                    name: a.name.clone(),
                    id: a.id.clone(),
                })
                .collect(),
            album: track.album.clone(),
            album_id: track.album_id.clone(),
            duration: track.duration,
            thumbnail: track.thumbnail.as_deref().map(str::to_owned),
            wide: track.wide,
            local: track.local_path.is_some(),
            at,
            listened: listened.round() as u64,
            source: Source::App,
        }
    }

    /// The song again, for a row that can play it.
    pub fn track(&self) -> Track {
        Track {
            video_id: self.video_id.clone(),
            title: self.title.clone(),
            artists: self
                .artists
                .iter()
                .map(|a| ArtistLink {
                    name: a.name.clone(),
                    id: a.id.clone(),
                })
                .collect(),
            album: self.album.clone(),
            album_id: self.album_id.clone(),
            duration: self.duration,
            plays: None,
            explicit: false,
            like: LikeStatus::default(),
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: self.thumbnail.as_deref().map(Arc::from),
            wide: self.wide,
            rank: None,
            trend: ChartTrend::Unknown,
            local_path: self
                .local
                .then(|| Arc::from(Path::new(&self.video_id).to_owned().into_boxed_path())),
        }
    }
}

fn path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "refryt")
        .context("could not find the config directory")?;
    Ok(dirs.config_dir().join("plays.jsonl"))
}

/// Add a play to the log.
pub fn append(play: &Play) -> Result<()> {
    append_to(&path()?, play)
}

fn append_to(path: &Path, play: &Play) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let mut line = String::new();
    if ends_mid_line(&mut file)? {
        line.push('\n');
    }
    line.push_str(&serde_json::to_string(play)?);
    line.push('\n');
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Whether the file was left with a line cut short, which the next one must not run on from.
fn ends_mid_line(file: &mut File) -> io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Hand over every readable play, oldest first, without keeping them in memory.
pub fn for_each(visit: impl FnMut(Play)) -> Result<()> {
    for_each_in(&path()?, visit)
}

fn for_each_in(path: &Path, mut visit: impl FnMut(Play)) -> Result<()> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("could not open the play log"),
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if let Ok(play) = serde_json::from_str(&line) {
            visit(play);
        }
    }
}

/// Forget every play.
pub fn delete() -> Result<()> {
    match fs::remove_file(path()?) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(e).context("could not delete the play log")
        }
        _ => Ok(()),
    }
}

/// How long a song of `duration` seconds has to be listened to. A length not known is 0.
fn counts_after(duration: f64) -> f64 {
    if duration > 0.0 {
        COUNTS_AFTER_SECS.min(duration / 2.0)
    } else {
        COUNTS_AFTER_SECS
    }
}

fn near_end(position: f64, duration: f64) -> bool {
    duration > 0.0 && position >= duration - NEAR_END_SECS
}

/// One song being listened to.
struct Pass {
    track: Track,
    started: i64,
    listened: f64,
    last: Option<f64>,
    duration: f64,
}

impl Pass {
    fn new(track: Track, now: i64) -> Self {
        let duration = track.duration.unwrap_or(0) as f64;
        Self {
            track,
            started: now,
            listened: 0.0,
            last: None,
            duration,
        }
    }

    fn play(&self) -> Option<Play> {
        (self.listened >= counts_after(self.duration))
            .then(|| Play::new(&self.track, self.started, self.listened))
    }

    fn at_end(&self) -> bool {
        self.last.is_some_and(|at| near_end(at, self.duration))
    }
}

/// Watches the player and says when a listen has ended that should be logged.
#[derive(Default)]
pub struct Listening {
    pass: Option<Pass>,
}

impl Listening {
    /// A track has loaded. The same one again partway through is the player reloading it after a
    /// stall, which carries on the listen; from its end it is another listen.
    pub fn started(&mut self, track: &Track, now: i64) -> Option<Play> {
        if let Some(pass) = &mut self.pass
            && pass.track.video_id == track.video_id
            && !pass.at_end()
        {
            pass.last = None;
            return None;
        }
        let done = self.finish();
        self.pass = Some(Pass::new(track.clone(), now));
        done
    }

    /// What the player reports this pass. Cheap: a little arithmetic, and a logged play only when
    /// Repeat One goes round or a queue runs out.
    pub fn observe(
        &mut self,
        position: f64,
        duration: f64,
        paused: bool,
        idle: bool,
        now: i64,
    ) -> Option<Play> {
        let pass = self.pass.as_mut()?;
        if idle {
            // Mid-song this is a stalled load, which comes back; at the end, the queue ran out.
            return if pass.at_end() { self.finish() } else { None };
        }
        if duration > 0.0 {
            pass.duration = duration;
        }
        let previous = pass.last.replace(position);
        if paused {
            return None;
        }
        let previous = previous?;
        let step = position - previous;
        if step > 0.0 && step <= MAX_STEP_SECS {
            pass.listened += step;
        } else if position <= RESTART_SECS && near_end(previous, pass.duration) {
            let done = pass.play();
            pass.started = now;
            pass.listened = 0.0;
            return done;
        }
        None
    }

    /// The listen is over, whatever comes next.
    pub fn finish(&mut self) -> Option<Play> {
        self.pass.take().and_then(|pass| pass.play())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, duration: Option<u64>) -> Track {
        let mut track = Play {
            video_id: id.to_owned(),
            title: id.to_owned(),
            artists: vec![LoggedArtist {
                name: "Artist".to_owned(),
                id: Some("UC1".to_owned()),
            }],
            album: None,
            album_id: None,
            duration,
            thumbnail: None,
            wide: false,
            local: false,
            at: 0,
            listened: 0,
            source: Source::App,
        }
        .track();
        track.duration = duration;
        track
    }

    /// Plays `from..to` a second at a time.
    fn listen(l: &mut Listening, from: u32, to: u32, duration: f64) -> Option<Play> {
        (from..=to).find_map(|s| l.observe(s as f64, duration, false, false, 0))
    }

    #[test]
    fn a_song_counts_after_thirty_seconds_or_half_if_shorter() {
        assert_eq!(counts_after(200.0), 30.0);
        assert_eq!(counts_after(40.0), 20.0);
        assert_eq!(counts_after(0.0), 30.0);
    }

    #[test]
    fn a_skipped_song_is_not_logged_and_a_listened_one_is() {
        let mut l = Listening::default();
        assert_eq!(l.started(&track("a", Some(200)), 10), None);
        listen(&mut l, 0, 10, 200.0);
        assert_eq!(l.started(&track("b", Some(200)), 20), None, "skipped");
        listen(&mut l, 0, 40, 200.0);
        let play = l.started(&track("c", Some(200)), 60).expect("listened");
        assert_eq!(play.video_id, "b");
        assert_eq!((play.at, play.listened), (20, 40));
    }

    #[test]
    fn paused_time_adds_nothing() {
        let mut l = Listening::default();
        l.started(&track("a", Some(200)), 0);
        listen(&mut l, 0, 5, 200.0);
        for _ in 0..100 {
            l.observe(5.0, 200.0, true, false, 0);
        }
        assert_eq!(l.finish(), None);
    }

    #[test]
    fn seeking_forward_adds_nothing() {
        let mut l = Listening::default();
        l.started(&track("a", Some(200)), 0);
        listen(&mut l, 0, 5, 200.0);
        l.observe(150.0, 200.0, false, false, 0);
        assert_eq!(l.finish(), None);
    }

    #[test]
    fn repeat_one_logs_each_time_round() {
        let mut l = Listening::default();
        l.started(&track("a", Some(40)), 0);
        listen(&mut l, 0, 39, 40.0);
        let first = l.observe(0.2, 40.0, false, false, 100).expect("looped");
        assert_eq!(first.at, 0);
        listen(&mut l, 1, 39, 40.0);
        assert_eq!(l.finish().map(|p| p.at), Some(100));
    }

    /// Going back to the start mid-song is one listen carrying on, not two.
    #[test]
    fn restarting_a_song_midway_is_the_same_listen() {
        let mut l = Listening::default();
        l.started(&track("a", Some(200)), 0);
        listen(&mut l, 0, 35, 200.0);
        assert_eq!(l.observe(0.0, 200.0, false, false, 0), None);
        assert!(l.finish().is_some());
    }

    #[test]
    fn a_reload_after_a_stall_carries_on_the_listen() {
        let mut l = Listening::default();
        l.started(&track("a", Some(200)), 0);
        listen(&mut l, 0, 35, 200.0);
        assert_eq!(l.observe(0.0, 0.0, false, true, 0), None, "stalled");
        assert_eq!(l.started(&track("a", Some(200)), 50), None, "reloaded");
        listen(&mut l, 35, 60, 200.0);
        let play = l.finish().expect("one listen");
        assert_eq!((play.at, play.listened), (0, 60));
    }

    #[test]
    fn replaying_a_song_from_its_end_is_another_listen() {
        let mut l = Listening::default();
        l.started(&track("a", Some(40)), 0);
        listen(&mut l, 0, 39, 40.0);
        assert!(l.started(&track("a", Some(40)), 50).is_some());
    }

    #[test]
    fn a_queue_running_out_ends_the_listen() {
        let mut l = Listening::default();
        l.started(&track("a", Some(40)), 0);
        listen(&mut l, 0, 39, 40.0);
        assert!(l.observe(0.0, 0.0, false, true, 0).is_some());
        assert_eq!(l.finish(), None, "only once");
    }

    #[test]
    fn plays_survive_the_file_and_a_cut_line_costs_only_itself() {
        let dir = std::env::temp_dir().join(format!("refryt-play-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("plays.jsonl");
        let _ = fs::remove_file(&file);
        let read = || {
            let mut plays = Vec::new();
            for_each_in(&file, |p| plays.push(p.video_id)).unwrap();
            plays
        };
        assert!(read().is_empty(), "no file yet");

        let play = |id: &str| Play::new(&track(id, Some(100)), 5, 50.0);
        append_to(&file, &play("a")).unwrap();
        let mut f = OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(br#"{"v":"cut"#).unwrap();
        drop(f);
        append_to(&file, &play("b")).unwrap();
        assert_eq!(read(), ["a", "b"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_logged_song_can_be_played_again() {
        let local = Track {
            local_path: Some(Arc::from(
                Path::new("/m/a.flac").to_owned().into_boxed_path(),
            )),
            ..track("/m/a.flac", None)
        };
        let play = Play::new(&local, 1, 40.0);
        let line = serde_json::to_string(&play).unwrap();
        let back: Play = serde_json::from_str(&line).unwrap();
        assert_eq!(back, play);
        assert_eq!(back.track().local_path, local.local_path);
        assert_eq!(back.track().artists, local.artists);
    }
}
