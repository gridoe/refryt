//! The play queue: the tracks, the one playing, where a restored song resumes, the shuffle order
//! and the way back.
//!
//! One place owns every index into the queue. An insert, a removal or a trim moves the playing
//! track, the restored position, the shuffle order and the history together, where each edit used
//! to shift (or forget to shift) its own share of them inside `App`. Pure, so the rules are tested
//! without a window or a player.
//!
//! Shuffle is an order fixed when it is turned on, not a random pick at each Next: what plays next
//! is known in advance, so Up next shows it, it is preloaded, and Previous retraces it. Turning
//! shuffle off goes on in list order from the track playing.
//!
//! The radio stays in `App`. Where its rules matter (the songs the listener chose before the
//! radio's, Repeat All looping only those), the caller passes the chosen set as [`Chosen`].

use std::collections::{HashSet, VecDeque};

use crate::app::{Radio, RepeatMode};
use crate::model::Track;

/// How many already-played tracks are kept sitting before the playing one before the oldest are
/// dropped. Autoplay's radio only ever appends, so an unbounded played prefix is the one way the
/// queue, and the state saved every few seconds, grows without limit over a long session.
/// Generous enough that trimming is never something a listener could notice by scrolling up.
pub(crate) const MAX_PLAYED_PREFIX: usize = 200;

/// How many "previous" steps the transport remembers within one queue. Enough to retrace a few
/// songs, not the whole session. The entries are `(index, queue length)`, cheap, because the songs
/// themselves are still in the queue; only the autoplay tail added since needs dropping.
const PLAY_HISTORY: usize = 12;
/// How many replaced queues are kept for Previous to bring back, so starting an album does not
/// erase the playlist you were in.
const QUEUE_HISTORY: usize = 5;

/// The video ids the listener chose, from the radio; `None` when there is no radio, where every
/// track is theirs.
pub(crate) type Chosen<'a> = Option<&'a HashSet<String>>;

fn is_chosen(chosen: Chosen, track: &Track) -> bool {
    chosen.is_none_or(|c| c.contains(&track.video_id))
}

/// Where a track added to the queue goes in the shuffle order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// Straight after the playing track, `n` places further for the n-th of several.
    Next(usize),
    /// After the listener's own songs still to come, before the radio's.
    BeforeRadio,
    /// Anywhere among the radio's songs still to come.
    Radio,
}

/// Where [`PlayQueue::rewind`] says Previous should go.
pub(crate) enum Rewind {
    /// Within the current queue: drop anything past `queue_len`, then play `index`.
    WithinQueue { index: usize, queue_len: usize },
    /// A queue that was replaced: restore it wholesale.
    PreviousQueue {
        queue: Vec<Track>,
        index: usize,
        radio: Option<Radio>,
    },
}

/// A step back within the current queue: where the transport was, and how long the queue was.
/// Restoring truncates any autoplay tail appended after that point.
struct PlayPoint {
    index: usize,
    queue_len: usize,
}

/// The queue in force before another replaced it, for Previous across a queue change.
struct QueuePoint {
    queue: Vec<Track>,
    index: usize,
    radio: Option<Radio>,
}

/// The transport's hidden "previous" history: recent positions in the current queue, and whole
/// queues replaced since. Bounded on both ends so a long session cannot grow it without limit.
#[derive(Default)]
struct PlayHistory {
    played: VecDeque<PlayPoint>,
    queues: VecDeque<QueuePoint>,
}

impl PlayHistory {
    /// Remember the track being left. Called for a forward move only, not for Previous, or the
    /// two would ping-pong.
    fn remember(&mut self, index: usize, queue_len: usize) {
        self.played.push_back(PlayPoint { index, queue_len });
        while self.played.len() > PLAY_HISTORY {
            self.played.pop_front();
        }
    }

    /// The queue's shape changed by hand. Every within-queue point was captured against the old
    /// shape, and shifting each to match is not worth it for a one-off edit: Previous stops
    /// rewinding past the edit rather than risk landing on the wrong track. Whole queues stay.
    fn forget_within_queue(&mut self) {
        self.played.clear();
    }

    /// One queue is being replaced by another: keep the whole old queue, and clear the
    /// within-queue points, which index the queue now gone.
    fn leaving_queue(&mut self, queue: Vec<Track>, index: usize, radio: Option<Radio>) {
        self.queues.push_back(QueuePoint {
            queue,
            index,
            radio,
        });
        while self.queues.len() > QUEUE_HISTORY {
            self.queues.pop_front();
        }
        self.played.clear();
    }

    /// The next restore point, newest first: a position in this queue, else the queue before it.
    fn rewind(&mut self) -> Option<Rewind> {
        if let Some(point) = self.played.pop_back() {
            return Some(Rewind::WithinQueue {
                index: point.index,
                queue_len: point.queue_len,
            });
        }
        self.queues.pop_back().map(|point| Rewind::PreviousQueue {
            queue: point.queue,
            index: point.index,
            radio: point.radio,
        })
    }
}

/// Where Repeat All wraps: at the end of the tracks the listener chose, so a radio tail appended
/// before Repeat All went on is looped past rather than played. Playing inside that tail already,
/// the whole queue is the loop.
pub(crate) fn loop_end(tail_start: Option<usize>, current: usize, len: usize) -> usize {
    tail_start.filter(|&tail| current < tail).unwrap_or(len)
}

/// The next index in a queue when not shuffling: the following track, wrapping to the start only
/// under Repeat All. `None` means the transport stops (or, for Repeat One, replays).
pub(crate) fn sequential_next(current: usize, len: usize, repeat_all: bool) -> Option<usize> {
    if current + 1 < len {
        Some(current + 1)
    } else if repeat_all && len > 0 {
        Some(0)
    } else {
        None
    }
}

/// The shuffle order: a permutation of the queue's indices, and where in it the playing track is.
/// `pos` is `None` before the first entry: a new queue not started yet, or a new round under
/// Repeat All, whose first entry is the next to play.
#[derive(Clone, Debug)]
struct Order {
    order: Vec<usize>,
    pos: Option<usize>,
}

/// The queue and everything that indexes into it. See the module docs.
pub(crate) struct PlayQueue {
    tracks: Vec<Track>,
    current: Option<usize>,
    /// A restored position to start at once its track loads: `(index, seconds)`.
    resume: Option<(usize, f64)>,
    /// The play order while shuffle is on.
    shuffle: Option<Order>,
    history: PlayHistory,
    /// One LCG step per draw, seeded from the clock: enough for a shuffle.
    rng: u64,
}

impl PlayQueue {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            tracks: Vec::new(),
            current: None,
            resume: None,
            shuffle: None,
            history: PlayHistory::default(),
            rng: seed,
        }
    }

    // --- Reading -------------------------------------------------------------------------------

    pub(crate) fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// The tracks themselves, to update a rating in place; the length cannot change through it.
    pub(crate) fn tracks_mut(&mut self) -> &mut [Track] {
        &mut self.tracks
    }

    pub(crate) fn get(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.tracks.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.current
    }

    pub(crate) fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.tracks.get(i))
    }

    pub(crate) fn is_shuffled(&self) -> bool {
        self.shuffle.is_some()
    }

    pub(crate) fn resume(&self) -> Option<(usize, f64)> {
        self.resume
    }

    /// The restored position of the track playing, if it has one.
    pub(crate) fn resume_here(&self) -> Option<f64> {
        self.resume
            .filter(|(i, _)| Some(*i) == self.current)
            .map(|(_, p)| p)
    }

    pub(crate) fn set_resume(&mut self, resume: Option<(usize, f64)>) {
        self.resume = resume;
    }

    /// The restored position for `index`, taken so it is applied once.
    pub(crate) fn take_resume_for(&mut self, index: usize) -> Option<f64> {
        let at = self.resume.filter(|(i, _)| *i == index).map(|(_, p)| p);
        if at.is_some() {
            self.resume = None;
        }
        at
    }

    /// Where the radio's tail begins: the first track the listener did not choose.
    pub(crate) fn tail_start(&self, chosen: Chosen) -> Option<usize> {
        chosen?;
        self.tracks.iter().position(|t| !is_chosen(chosen, t))
    }

    /// Where the listener's own songs still to come end: the first radio track after the playing
    /// one, or the end of the list. "Add to queue" lands here. Unlike [`Self::tail_start`], radio
    /// songs already played do not count, or the addition would land behind the playing song.
    pub(crate) fn own_end(&self, chosen: Chosen) -> usize {
        let start = self.current.map_or(0, |i| i + 1).min(self.tracks.len());
        let Some(c) = chosen else {
            return self.tracks.len();
        };
        self.tracks[start..]
            .iter()
            .position(|t| !c.contains(&t.video_id))
            .map_or(self.tracks.len(), |p| start + p)
    }

    /// What plays after the playing track, in order: the rest of the list, or of the shuffle
    /// order. Up next lists these.
    pub(crate) fn upcoming(&self, repeat: RepeatMode, chosen: Chosen) -> Vec<usize> {
        match &self.shuffle {
            Some(o) => {
                let skip = self.round_skips(repeat, chosen);
                let from = o.pos.map_or(0, |p| p + 1);
                o.order[from.min(o.order.len())..]
                    .iter()
                    .copied()
                    .filter(|&i| !skip(i))
                    .collect()
            }
            None => {
                let start = self.current.map_or(0, |i| i + 1);
                (start.min(self.tracks.len())..self.tracks.len()).collect()
            }
        }
    }

    /// How many tracks [`Self::upcoming`] lists, without building the list: what autoplay's
    /// low-water mark counts, since under shuffle the playing track's place in the list says
    /// nothing about how much is left.
    pub(crate) fn remaining(&self, repeat: RepeatMode, chosen: Chosen) -> usize {
        match &self.shuffle {
            Some(o) => {
                let skip = self.round_skips(repeat, chosen);
                let from = o.pos.map_or(0, |p| p + 1);
                o.order[from.min(o.order.len())..]
                    .iter()
                    .filter(|&&i| !skip(i))
                    .count()
            }
            None => self.tracks.len() - self.current.map_or(0, |i| i + 1).min(self.tracks.len()),
        }
    }

    /// The track to start when nothing has played yet: the first of the list, or of the shuffle.
    pub(crate) fn first(&self) -> usize {
        self.shuffle
            .as_ref()
            .and_then(|o| o.order.first().copied())
            .unwrap_or(0)
    }

    // --- Deciding -------------------------------------------------------------------------------

    /// The track Next plays, or `None` where the transport stops. Under shuffle with Repeat All the
    /// end of a round draws the next round's order here, so this takes `&mut self`.
    pub(crate) fn next(&mut self, repeat: RepeatMode, chosen: Chosen) -> Option<usize> {
        let current = self.current?;
        if self.shuffle.is_none() {
            return self.next_in_list(current, repeat, chosen);
        }
        if let Some(next) = self.next_in_order(repeat, chosen) {
            return Some(next);
        }
        if repeat != RepeatMode::All {
            return None;
        }
        let order = self.new_round(current, repeat, chosen);
        let first = order.order.first().copied().unwrap_or(current);
        self.shuffle = Some(order);
        Some(first)
    }

    /// [`Self::next`] without drawing a new round: what to preload. `None` at the end of a
    /// shuffled round, where the next track is not known until it is drawn.
    pub(crate) fn peek_next(&self, repeat: RepeatMode, chosen: Chosen) -> Option<usize> {
        let current = self.current?;
        if self.shuffle.is_some() {
            self.next_in_order(repeat, chosen)
        } else {
            self.next_in_list(current, repeat, chosen)
        }
    }

    /// The track before the playing one in the shuffle order. `None` without shuffle, or at the
    /// start of the order, where Previous falls back to the history.
    pub(crate) fn previous_in_order(&self) -> Option<usize> {
        let o = self.shuffle.as_ref()?;
        let pos = o.pos?;
        pos.checked_sub(1).and_then(|p| o.order.get(p).copied())
    }

    fn next_in_list(&self, current: usize, repeat: RepeatMode, chosen: Chosen) -> Option<usize> {
        let repeat_all = repeat == RepeatMode::All;
        let len = if repeat_all {
            loop_end(self.tail_start(chosen), current, self.tracks.len())
        } else {
            self.tracks.len()
        };
        sequential_next(current, len, repeat_all)
    }

    fn next_in_order(&self, repeat: RepeatMode, chosen: Chosen) -> Option<usize> {
        let o = self.shuffle.as_ref()?;
        let skip = self.round_skips(repeat, chosen);
        let from = o.pos.map_or(0, |p| p + 1);
        o.order.get(from..)?.iter().copied().find(|&i| !skip(i))
    }

    /// Which tracks a shuffled round passes over: under Repeat All the loop is the listener's own
    /// songs, so a radio tail appended before it went on is not played; otherwise none.
    fn round_skips(&self, repeat: RepeatMode, chosen: Chosen) -> impl Fn(usize) -> bool + '_ {
        let loops_chosen =
            repeat == RepeatMode::All && self.tracks.iter().any(|t| is_chosen(chosen, t));
        let chosen: Option<HashSet<String>> = chosen.cloned();
        move |i| {
            loops_chosen
                && self
                    .tracks
                    .get(i)
                    .is_some_and(|t| !is_chosen(chosen.as_ref(), t))
        }
    }

    // --- Moving ---------------------------------------------------------------------------------

    /// Make `index` the playing track. Under shuffle the order moves to it: jumping ahead passes
    /// over the songs in between, stepping back makes them upcoming again.
    pub(crate) fn set_current(&mut self, index: usize) {
        self.current = Some(index);
        if let Some(o) = &mut self.shuffle {
            o.pos = o.order.iter().position(|&i| i == index);
        }
    }

    /// Move the track at `index` to play straight after the playing one, everything else keeping
    /// its order, and return where it now is: what "play from the queue without skipping" plays.
    /// The songs between the playing track and the chosen one stay ahead, after it, instead of
    /// being passed over. In the list without shuffle, in the order under shuffle (the list then
    /// says nothing about what plays next). `None` with nothing playing, for the playing track
    /// itself, or for an index out of range.
    pub(crate) fn bring_up_next(&mut self, index: usize) -> Option<usize> {
        let current = self.current?;
        if index == current || index >= self.tracks.len() {
            return None;
        }
        if let Some(o) = &mut self.shuffle {
            let from = o.order.iter().position(|&i| i == index)?;
            let at = o.pos.map_or(0, |p| p + 1);
            if from >= at {
                let entry = o.order.remove(from);
                o.order.insert(at, entry);
            } else {
                // Already played this round: it comes out from before the playing track, which
                // moves back one place.
                let entry = o.order.remove(from);
                o.pos = o.pos.map(|p| p - 1);
                o.order.insert(at - 1, entry);
            }
            return Some(index);
        }
        // Where it goes, counted after it has been taken out: straight after the playing track,
        // which itself moves back one place when the track came from before it.
        let to = if index > current {
            current + 1
        } else {
            current
        };
        if to == index {
            return Some(index);
        }
        let track = self.tracks.remove(index);
        self.tracks.insert(to, track);
        self.history.forget_within_queue();
        self.reindex(|i| {
            Some(if i == index {
                to
            } else if index > current && (to..index).contains(&i) {
                i + 1
            } else if index < current && (index + 1..=to).contains(&i) {
                i - 1
            } else {
                i
            })
        });
        Some(to)
    }

    /// Remember the track being left, for Previous.
    pub(crate) fn remember_current(&mut self) {
        if let Some(current) = self.current {
            self.history.remember(current, self.tracks.len());
        }
    }

    /// Where Previous goes when the track is not underway and there is nothing earlier in the
    /// shuffle order. Under shuffle the within-queue points are passed over: they index list order,
    /// which is not the order things played in.
    pub(crate) fn rewind(&mut self) -> Option<Rewind> {
        loop {
            match self.history.rewind()? {
                Rewind::WithinQueue { .. } if self.shuffle.is_some() => {}
                step => return Some(step),
            }
        }
    }

    // --- Replacing ------------------------------------------------------------------------------

    /// Make `tracks` the queue, keeping the queue it replaces (and its `radio`) for Previous.
    /// Nothing plays in it until [`Self::set_current`]; under shuffle `start` is drawn first, so
    /// the order begins with the track about to play.
    pub(crate) fn replace(
        &mut self,
        tracks: Vec<Track>,
        radio: Option<Radio>,
        start: usize,
        chosen: Chosen,
    ) {
        if let Some(current) = self.current {
            let old = std::mem::take(&mut self.tracks);
            self.history.leaving_queue(old, current, radio);
        }
        self.load(tracks, start, chosen);
    }

    /// Bring back a queue Previous rewound to; it is not remembered again.
    pub(crate) fn restore_queue(&mut self, tracks: Vec<Track>, start: usize, chosen: Chosen) {
        self.load(tracks, start, chosen);
    }

    fn load(&mut self, tracks: Vec<Track>, start: usize, chosen: Chosen) {
        self.tracks = tracks;
        self.current = None;
        // Keyed by index: the new queue's song at that index would open where the old one left off.
        self.resume = None;
        if self.shuffle.is_some() {
            self.shuffle = Some(self.draw(Some(start), chosen));
        }
    }

    /// The queue saved last session, its track, its restored position and its shuffle.
    pub(crate) fn restore_session(
        &mut self,
        tracks: Vec<Track>,
        current: Option<usize>,
        resume: Option<f64>,
        shuffle: bool,
        chosen: Chosen,
    ) {
        self.tracks = tracks;
        self.current = current.filter(|&i| i < self.tracks.len());
        self.resume = self.current.zip(resume);
        // Not `set_shuffle`: a shuffled session's list order says nothing of what played, so
        // the whole queue is drawn again, as it was.
        self.shuffle = shuffle.then(|| {
            let mut order = self.draw(self.current, chosen);
            order.pos = self.current.map(|_| 0);
            order
        });
    }

    /// Turn shuffle on or off. On, the songs before the playing one have played and stay behind
    /// it, where Previous finds them in list order; the playing track stays and the rest are
    /// drawn into an order, the listener's own songs before the radio's. Off, the list order goes
    /// on from the track playing.
    pub(crate) fn set_shuffle(&mut self, on: bool, chosen: Chosen) {
        if on == self.shuffle.is_some() {
            return;
        }
        self.shuffle = on.then(|| {
            let mut order = self.draw(self.current, chosen);
            if let Some(current) = self.current {
                order.order.retain(|&i| i >= current);
                order.order.splice(0..0, 0..current);
                order.pos = Some(current);
            }
            order
        });
    }

    /// A fresh order: `first` (if any) leads, then the listener's songs, then the radio's, each
    /// shuffled within itself. `pos` is left before the start.
    fn draw(&mut self, first: Option<usize>, chosen: Chosen) -> Order {
        let first = first.filter(|&i| i < self.tracks.len());
        let (mut own, mut radio): (Vec<usize>, Vec<usize>) = (0..self.tracks.len())
            .filter(|&i| Some(i) != first)
            .partition(|&i| is_chosen(chosen, &self.tracks[i]));
        self.shuffle_in_place(&mut own);
        self.shuffle_in_place(&mut radio);
        let mut order = Vec::with_capacity(self.tracks.len());
        order.extend(first);
        order.extend(own);
        order.extend(radio);
        Order { order, pos: None }
    }

    /// The next round under Repeat All: every track of the loop again, in a new order, with the one
    /// that just played not first so it does not play twice in a row.
    fn new_round(&mut self, just_played: usize, repeat: RepeatMode, chosen: Chosen) -> Order {
        let (mut round, mut rest): (Vec<usize>, Vec<usize>) = {
            let skip = self.round_skips(repeat, chosen);
            (0..self.tracks.len()).partition(|&i| !skip(i))
        };
        self.shuffle_in_place(&mut round);
        self.shuffle_in_place(&mut rest);
        if round.len() > 1 && round[0] == just_played {
            let swap_with = 1 + self.draw_below(round.len() - 1);
            round.swap(0, swap_with);
        }
        round.extend(rest);
        Order {
            order: round,
            pos: None,
        }
    }

    fn draw_below(&mut self, n: usize) -> usize {
        self.rng = self
            .rng
            .wrapping_mul(0x5851f42d4c957f2d)
            .wrapping_add(0x14057b7ef767814f);
        ((self.rng >> 33) as usize) % n.max(1)
    }

    fn shuffle_in_place(&mut self, items: &mut [usize]) {
        for i in (1..items.len()).rev() {
            let j = self.draw_below(i + 1);
            items.swap(i, j);
        }
    }

    // --- Editing --------------------------------------------------------------------------------

    /// Insert `track` at list index `at`; `slot` says where it goes in the shuffle order. The
    /// playing track, the restored position, the order and the history all follow.
    pub(crate) fn insert(&mut self, at: usize, track: Track, slot: Slot, chosen: Chosen) {
        let at = at.min(self.tracks.len());
        self.tracks.insert(at, track);
        if self.current.is_some_and(|c| c >= at) {
            self.history.forget_within_queue();
        }
        self.reindex(|i| Some(if i >= at { i + 1 } else { i }));
        let place = self.slot_position(slot, chosen);
        if let Some(o) = &mut self.shuffle {
            let place = place.min(o.order.len());
            o.order.insert(place, at);
            if o.pos.is_some_and(|p| p >= place) {
                o.pos = o.pos.map(|p| p + 1);
            }
        }
    }

    /// Where in the shuffle order a new track goes (the order before it is added).
    fn slot_position(&mut self, slot: Slot, chosen: Chosen) -> usize {
        let Some(o) = &self.shuffle else { return 0 };
        let after = o.pos.map_or(0, |p| p + 1);
        // The first radio song still to come, where the listener's own songs end.
        let radio_from = o.order[after.min(o.order.len())..]
            .iter()
            .position(|&i| !is_chosen(chosen, &self.tracks[i]))
            .map_or(o.order.len(), |p| after + p);
        match slot {
            Slot::Next(n) => after + n,
            Slot::BeforeRadio => radio_from,
            Slot::Radio => {
                let span = o.order.len() - radio_from + 1;
                radio_from + self.draw_below(span)
            }
        }
    }

    /// Append the radio's `fresh` tracks the queue does not already hold, by video id. A reseed
    /// returns the seed itself first, and continuations may overlap the tail; either way the
    /// queue must not repeat itself. Returns how many were added.
    pub(crate) fn append_radio(&mut self, fresh: &[Track], chosen: Chosen) -> usize {
        let mut added = 0;
        for t in fresh {
            if !self.tracks.iter().any(|q| q.video_id == t.video_id) {
                let at = self.tracks.len();
                self.insert(at, t.clone(), Slot::Radio, chosen);
                added += 1;
            }
        }
        added
    }

    /// How many of the listener's own songs play before the radio's first, in play order: all
    /// of [`Self::upcoming`] while no radio song is in it.
    pub(crate) fn chosen_before_radio(&self, repeat: RepeatMode, chosen: Chosen) -> usize {
        let own = |&i: &usize| is_chosen(chosen, &self.tracks[i]);
        if self.shuffle.is_some() {
            return self
                .upcoming(repeat, chosen)
                .into_iter()
                .take_while(own)
                .count();
        }
        let start = self.current.map_or(0, |i| i + 1).min(self.tracks.len());
        (start..self.tracks.len()).take_while(own).count()
    }

    /// Drop the radio's songs still to come — what [`Self::upcoming`] lists that the listener did
    /// not choose — so a radio filter can put its own in their place. The playing track, the
    /// ones already played and every chosen one stay. Returns how many went.
    pub(crate) fn drop_upcoming_radio(&mut self, repeat: RepeatMode, chosen: Chosen) -> usize {
        let mut gone = vec![false; self.tracks.len()];
        for i in self.upcoming(repeat, chosen) {
            if self.current != Some(i) && !is_chosen(chosen, &self.tracks[i]) {
                gone[i] = true;
            }
        }
        self.drop_marked(&gone)
    }

    /// Clear Up next's own part: the listener's songs still to come before the radio's first,
    /// as [`Self::chosen_before_radio`] counts them. The playing track, the ones already played
    /// and the radio stay. Returns how many went.
    pub(crate) fn clear_up_next(&mut self, repeat: RepeatMode, chosen: Chosen) -> usize {
        let mut gone = vec![false; self.tracks.len()];
        for i in self
            .upcoming(repeat, chosen)
            .into_iter()
            .take_while(|&i| is_chosen(chosen, &self.tracks[i]))
        {
            if self.current != Some(i) {
                gone[i] = true;
            }
        }
        self.drop_marked(&gone)
    }

    /// Take out every track `gone` marks, keeping the rest's order, the playing track and the
    /// shuffle order on them.
    fn drop_marked(&mut self, gone: &[bool]) -> usize {
        let dropped = gone.iter().filter(|&&g| g).count();
        if dropped == 0 {
            return 0;
        }
        let mut kept = 0;
        let map: Vec<Option<usize>> = gone
            .iter()
            .map(|&g| {
                (!g).then(|| {
                    kept += 1;
                    kept - 1
                })
            })
            .collect();
        let mut i = 0;
        self.tracks.retain(|_| {
            i += 1;
            !gone[i - 1]
        });
        self.history.forget_within_queue();
        self.reindex(|i| map.get(i).copied().flatten());
        dropped
    }

    /// Remove `video_id` from the radio's songs still to come, if it is one. Returns whether it
    /// was. A song the listener chose, or the playing one, stays.
    pub(crate) fn take_upcoming_radio(
        &mut self,
        video_id: &str,
        repeat: RepeatMode,
        chosen: Chosen,
    ) -> bool {
        if chosen.is_none() || chosen.is_some_and(|c| c.contains(video_id)) {
            return false;
        }
        self.upcoming(repeat, chosen)
            .into_iter()
            .find(|&i| self.tracks[i].video_id == video_id)
            .is_some_and(|i| self.remove(i))
    }

    /// Remove the track at `index`. The playing track is never removed; returns whether one was.
    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if self.current == Some(index) || index >= self.tracks.len() {
            return false;
        }
        self.tracks.remove(index);
        self.history.forget_within_queue();
        self.reindex(|i| match i.cmp(&index) {
            std::cmp::Ordering::Less => Some(i),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(i - 1),
        });
        true
    }

    /// Drop the oldest played tracks once more than [`MAX_PLAYED_PREFIX`] sit before the playing
    /// one. Not under shuffle, where a track before the playing one in the list may not have
    /// played yet.
    pub(crate) fn trim_played_prefix(&mut self) {
        if self.shuffle.is_some() {
            return;
        }
        let Some(drop) = self
            .current
            .and_then(|c| c.checked_sub(MAX_PLAYED_PREFIX))
            .filter(|&d| d > 0)
        else {
            return;
        };
        self.tracks.drain(..drop);
        self.history.forget_within_queue();
        self.reindex(|i| i.checked_sub(drop));
    }

    /// Cut the queue back to `len`, as Previous does when it rewinds past a radio page that
    /// arrived later.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.tracks.truncate(len);
        self.reindex(|i| (i < len).then_some(i));
    }

    /// Carry every index through `map` (`None`: the track is gone): the playing track, the
    /// restored position, and the shuffle order, whose place stays on the same entry.
    fn reindex(&mut self, map: impl Fn(usize) -> Option<usize>) {
        self.current = self.current.and_then(&map);
        self.resume = self.resume.and_then(|(i, p)| map(i).map(|i| (i, p)));
        if let Some(o) = &mut self.shuffle {
            let at_pos = o.pos.and_then(|p| o.order.get(p).copied()).and_then(&map);
            o.order = o.order.iter().filter_map(|&i| map(i)).collect();
            o.pos = at_pos.and_then(|e| o.order.iter().position(|&i| i == e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::River;

    fn track(id: &str) -> Track {
        Track {
            video_id: id.to_owned(),
            title: id.to_owned(),
            artists: vec![crate::model::ArtistLink::plain("Someone")],
            album: None,
            album_id: None,
            duration: None,
            plays: None,
            explicit: false,
            like: crate::model::LikeStatus::Indifferent,
            in_library: false,
            library_add: None,
            library_remove: None,
            set_video_id: None,
            thumbnail: None,
            rank: None,
            trend: crate::model::ChartTrend::Unknown,
            wide: false,
            local_path: None,
        }
    }

    fn tracks(ids: &[&str]) -> Vec<Track> {
        ids.iter().map(|s| track(s)).collect()
    }

    fn ids(q: &PlayQueue, idx: &[usize]) -> Vec<String> {
        idx.iter().map(|&i| q.tracks[i].video_id.clone()).collect()
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    fn radio(chosen: &[&str]) -> Radio {
        Radio {
            state: River::Dead,
            seed: String::new(),
            chosen: set(chosen),
            filters: Vec::new(),
            filtering: None,
        }
    }

    fn queue(list: &[&str], current: usize) -> PlayQueue {
        let mut q = PlayQueue::new(7);
        q.restore_session(tracks(list), Some(current), None, false, None);
        q
    }

    /// Play every track by Next from the current one until the transport stops or `cap` plays.
    fn play_out(q: &mut PlayQueue, repeat: RepeatMode, chosen: Chosen, cap: usize) -> Vec<String> {
        let mut played = Vec::new();
        while played.len() < cap {
            let Some(next) = q.next(repeat, chosen) else {
                break;
            };
            q.remember_current();
            q.set_current(next);
            played.push(q.tracks[next].video_id.clone());
        }
        played
    }

    /// A radio filter swaps only the radio's songs still to come: the played radio song, the
    /// playing one and the listener's own picks stay where they are.
    #[test]
    fn dropping_the_upcoming_radio_keeps_played_playing_and_chosen_songs() {
        let chosen = set(&["a", "b", "mine"]);
        // a b are the listener's; r1 played; r2 plays; mine was queued by hand among the radio.
        let mut q = queue(&["a", "b", "r1", "r2", "r3", "mine", "r4"], 3);
        assert_eq!(q.drop_upcoming_radio(RepeatMode::Off, Some(&chosen)), 2);
        assert_eq!(ids(&q, &[0, 1, 2, 3, 4]), ["a", "b", "r1", "r2", "mine"]);
        assert_eq!(q.len(), 5);
        assert_eq!(q.current(), Some(3));
        let up = q.upcoming(RepeatMode::Off, Some(&chosen));
        assert_eq!(ids(&q, &up), ["mine"]);
    }

    /// Up next's filter waits for the radio to come near: counted in play order, from the song
    /// after the playing one, up to the radio's first.
    #[test]
    fn the_own_songs_before_the_radio_are_counted_in_play_order() {
        let chosen = set(&["a", "b", "c"]);
        let q = queue(&["a", "b", "c", "r1", "r2"], 0);
        assert_eq!(q.chosen_before_radio(RepeatMode::Off, Some(&chosen)), 2);
        let q = queue(&["a", "b", "c", "r1", "r2"], 2);
        assert_eq!(q.chosen_before_radio(RepeatMode::Off, Some(&chosen)), 0);
        // A mix: only its seed is chosen, so the radio is next from the start.
        let q = queue(&["r0", "r1", "r2"], 0);
        assert_eq!(
            q.chosen_before_radio(RepeatMode::Off, Some(&set(&["mix"]))),
            0
        );
        // No radio yet: every song still to come is the listener's.
        let q = queue(&["a", "b", "c"], 0);
        assert_eq!(q.chosen_before_radio(RepeatMode::Off, Some(&chosen)), 2);
        let mut q = queue(&["a", "b", "c", "r1"], 0);
        q.set_shuffle(true, Some(&chosen));
        let own = q
            .upcoming(RepeatMode::Off, Some(&chosen))
            .iter()
            .take_while(|&&i| q.tracks[i].video_id != "r1")
            .count();
        assert_eq!(q.chosen_before_radio(RepeatMode::Off, Some(&chosen)), own);
    }

    /// Up next's ✕ clears the listener's songs still to come, and only those: the radio after
    /// them, the playing song and the played ones stay.
    #[test]
    fn clearing_up_next_keeps_the_radio_and_the_playing_song() {
        let chosen = set(&["a", "b", "c", "d"]);
        let mut q = queue(&["a", "b", "c", "d", "r1", "r2"], 1);
        assert_eq!(q.clear_up_next(RepeatMode::Off, Some(&chosen)), 2);
        assert_eq!(ids(&q, &[0, 1, 2, 3]), ["a", "b", "r1", "r2"]);
        assert_eq!(q.current(), Some(1));
        // No radio: the whole of Up next goes.
        let mut q = queue(&["a", "b", "c"], 0);
        assert_eq!(q.clear_up_next(RepeatMode::Off, None), 2);
        assert_eq!(q.len(), 1);
        let mut q = queue(&["a", "b", "c", "d", "r1"], 0);
        q.set_shuffle(true, Some(&chosen));
        let playing = q.current_track().unwrap().video_id.clone();
        q.clear_up_next(RepeatMode::Off, Some(&chosen));
        assert_eq!(q.current_track().unwrap().video_id, playing);
        let up = q.upcoming(RepeatMode::Off, Some(&chosen));
        assert_eq!(ids(&q, &up), ["r1"]);
    }

    #[test]
    fn dropping_the_upcoming_radio_under_shuffle_keeps_the_order_on_its_songs() {
        let chosen = set(&["a", "b", "c"]);
        let mut q = queue(&["a", "b", "c", "r1", "r2", "r3"], 0);
        q.set_shuffle(true, Some(&chosen));
        let before: Vec<String> = {
            let up = q.upcoming(RepeatMode::Off, Some(&chosen));
            ids(&q, &up)
        };
        let playing = q.current_track().unwrap().video_id.clone();
        q.drop_upcoming_radio(RepeatMode::Off, Some(&chosen));
        assert_eq!(q.current_track().unwrap().video_id, playing);
        let after = {
            let up = q.upcoming(RepeatMode::Off, Some(&chosen));
            ids(&q, &up)
        };
        let expected: Vec<String> = before
            .into_iter()
            .filter(|id| !id.starts_with('r'))
            .collect();
        assert_eq!(after, expected);
        assert!(
            q.tracks()
                .iter()
                .all(|t| !t.video_id.starts_with('r') || t.video_id == playing)
        );
    }

    /// The listener jumps to a song further down: it plays next, and the songs they jumped over
    /// stay ahead of it rather than being passed over.
    #[test]
    fn bringing_a_song_up_next_keeps_the_ones_in_between() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 0);
        let to = q.bring_up_next(3).unwrap();
        assert_eq!(to, 1);
        assert_eq!(q.current(), Some(0), "the playing track stays put");
        let up = q.upcoming(RepeatMode::Off, None);
        assert_eq!(ids(&q, &up), ["d", "b", "c", "e"]);
    }

    #[test]
    fn bringing_up_next_keeps_the_resume_point_on_its_track() {
        let mut q = queue(&["a", "b", "c", "d"], 1);
        q.set_resume(Some((2, 30.0)));
        q.bring_up_next(3);
        assert_eq!(q.current(), Some(1));
        assert_eq!(
            q.resume().map(|(i, _)| q.tracks[i].video_id.clone()),
            Some("c".into())
        );
    }

    #[test]
    fn bringing_up_the_next_song_or_the_playing_one_changes_nothing() {
        let mut q = queue(&["a", "b", "c"], 0);
        assert_eq!(q.bring_up_next(1), Some(1));
        assert_eq!(q.bring_up_next(0), None);
        assert_eq!(q.bring_up_next(9), None);
        assert_eq!(ids(&q, &[0, 1, 2]), ["a", "b", "c"]);
        let mut idle = PlayQueue::new(1);
        idle.restore_session(tracks(&["a", "b"]), None, None, false, None);
        assert_eq!(idle.bring_up_next(1), None);
    }

    /// From before the playing track (not something Up next shows, but the queue allows it): the
    /// playing track moves back a place and the song lands right after it.
    #[test]
    fn bringing_up_a_song_from_behind_lands_after_the_playing_one() {
        let mut q = queue(&["a", "b", "c", "d"], 2);
        let to = q.bring_up_next(0).unwrap();
        assert_eq!(q.tracks[q.current().unwrap()].video_id, "c");
        assert_eq!(q.tracks[to].video_id, "a");
        assert_eq!(ids(&q, &[0, 1, 2, 3]), ["b", "c", "a", "d"]);
    }

    /// Under shuffle the order moves, not the list: the song plays next and the rest of the
    /// round keeps its order.
    #[test]
    fn bringing_up_next_under_shuffle_moves_the_order() {
        let mut q = queue(&["a", "b", "c", "d", "e", "f"], 0);
        q.set_shuffle(true, None);
        let before = q.upcoming(RepeatMode::Off, None);
        let pick = before[3];
        let list: Vec<String> = q.tracks().iter().map(|t| t.video_id.clone()).collect();
        assert_eq!(q.bring_up_next(pick), Some(pick));
        let after = q.upcoming(RepeatMode::Off, None);
        let mut expected = vec![pick];
        expected.extend(before.iter().copied().filter(|&i| i != pick));
        assert_eq!(after, expected);
        let list_after: Vec<String> = q.tracks().iter().map(|t| t.video_id.clone()).collect();
        assert_eq!(list, list_after, "the list itself is left alone");
    }

    #[test]
    fn sequential_next_wraps_only_for_repeat_all() {
        assert_eq!(sequential_next(0, 3, false), Some(1));
        assert_eq!(
            sequential_next(2, 3, false),
            None,
            "end without repeat stops"
        );
        assert_eq!(sequential_next(2, 3, true), Some(0), "Repeat All wraps");
        assert_eq!(sequential_next(0, 1, false), None);
        assert_eq!(
            sequential_next(0, 1, true),
            Some(0),
            "a single track repeats"
        );
        assert_eq!(
            sequential_next(0, 0, true),
            None,
            "an empty queue has nothing"
        );
    }

    /// Repeat All loops the chosen tracks, not the radio tail after them, unless the listener is
    /// already inside that tail, where the whole queue is the loop.
    #[test]
    fn repeat_all_wraps_at_the_radio_tail() {
        let mut q = queue(&["a", "b", "t1", "t2"], 1);
        let chosen = set(&["a", "b"]);
        assert_eq!(q.tail_start(Some(&chosen)), Some(2));
        assert_eq!(loop_end(Some(2), 1, 4), 2);
        assert_eq!(q.next(RepeatMode::All, Some(&chosen)), Some(0));
        assert_eq!(loop_end(Some(2), 2, 4), 4, "inside the tail");
        assert_eq!(q.tail_start(None), None, "no radio");
        assert_eq!(q.tail_start(Some(&set(&["a", "b", "t1", "t2"]))), None);
    }

    #[test]
    fn history_pops_the_newest_position_first() {
        let mut h = PlayHistory::default();
        h.remember(0, 5);
        h.remember(1, 5);
        assert!(matches!(
            h.rewind(),
            Some(Rewind::WithinQueue {
                index: 1,
                queue_len: 5
            })
        ));
        assert!(matches!(
            h.rewind(),
            Some(Rewind::WithinQueue { index: 0, .. })
        ));
        assert!(h.rewind().is_none(), "emptied history must report nothing");
    }

    #[test]
    fn removing_a_queue_entry_forgets_only_the_within_queue_points() {
        let mut h = PlayHistory::default();
        h.leaving_queue(tracks(&["a", "b"]), 0, None);
        h.remember(1, 5);
        h.remember(2, 5);
        h.forget_within_queue();
        match h.rewind() {
            Some(Rewind::PreviousQueue { index, .. }) => assert_eq!(index, 0),
            _ => panic!("expected the earlier whole queue to survive"),
        }
    }

    #[test]
    fn a_replaced_queue_comes_back_after_the_current_ones() {
        let mut h = PlayHistory::default();
        h.remember(3, 9);
        h.leaving_queue(tracks(&["a", "b"]), 1, None);
        h.remember(2, 5);
        assert!(matches!(
            h.rewind(),
            Some(Rewind::WithinQueue { index: 2, .. })
        ));
        match h.rewind() {
            Some(Rewind::PreviousQueue { queue, index, .. }) => {
                assert_eq!(index, 1);
                assert_eq!(queue.len(), 2);
            }
            _ => panic!("expected the replaced queue"),
        }
        assert!(h.rewind().is_none());
    }

    #[test]
    fn history_is_bounded() {
        let mut h = PlayHistory::default();
        for i in 0..(PLAY_HISTORY + 5) {
            h.remember(i, 100);
        }
        let mut seen = Vec::new();
        while let Some(Rewind::WithinQueue { index, .. }) = h.rewind() {
            seen.push(index);
        }
        assert_eq!(seen.len(), PLAY_HISTORY);
        assert_eq!(seen.first(), Some(&16), "the newest point is first");
        assert_eq!(seen.last(), Some(&5), "the oldest points fell off");
        for i in 0..(QUEUE_HISTORY + 3) {
            h.leaving_queue(tracks(&[&i.to_string()]), 0, None);
        }
        let mut queues = 0;
        while let Some(Rewind::PreviousQueue { .. }) = h.rewind() {
            queues += 1;
        }
        assert_eq!(queues, QUEUE_HISTORY);
    }

    /// Inserts before, at and after the playing track keep it, and a restored position, on the
    /// same song.
    #[test]
    fn an_insert_keeps_the_playing_track_and_the_restored_position() {
        for at in [0, 2, 3, 5] {
            let mut q = queue(&["a", "b", "c", "d", "e"], 2);
            q.set_resume(Some((2, 40.0)));
            q.insert(at, track("x"), Slot::BeforeRadio, None);
            assert_eq!(q.current_track().unwrap().video_id, "c", "insert at {at}");
            assert_eq!(q.resume_here(), Some(40.0), "insert at {at}");
            assert_eq!(q.tracks[at].video_id, "x");
        }
    }

    /// Add to Queue while a radio song plays lands straight after it, not among the radio songs
    /// already played: Next plays the addition, then goes on with the radio.
    #[test]
    fn add_to_queue_inside_the_radio_tail_plays_next() {
        let mut q = queue(&["a", "r1", "r2", "r3"], 2);
        let mut chosen = set(&["a"]);
        chosen.insert("x".into());
        let at = q.own_end(Some(&chosen));
        q.insert(at, track("x"), Slot::BeforeRadio, Some(&chosen));
        assert_eq!(q.current_track().unwrap().video_id, "r2");
        let next = q.next(RepeatMode::Off, Some(&chosen)).unwrap();
        assert_eq!(q.tracks[next].video_id, "x");
        q.set_current(next);
        let next = q.next(RepeatMode::Off, Some(&chosen)).unwrap();
        assert_eq!(q.tracks[next].video_id, "r3");
    }

    /// The listener's own songs after the playing one stay ahead of an addition; the radio
    /// comes after it.
    #[test]
    fn add_to_queue_goes_after_the_listeners_songs_and_before_the_radio() {
        let q = queue(&["r0", "a", "b", "r1"], 0);
        let chosen = set(&["a", "b", "x"]);
        assert_eq!(q.own_end(Some(&chosen)), 3);
        assert_eq!(q.own_end(None), 4, "no radio: the end");
        let q = queue(&["a", "b"], 1);
        assert_eq!(q.own_end(Some(&chosen)), 2);
    }

    /// Queuing a song the radio already holds moves it out of the radio rather than copying it.
    #[test]
    fn queuing_a_radio_song_moves_it() {
        let mut q = queue(&["a", "r1", "r2", "r3"], 1);
        let mut chosen = set(&["a"]);
        assert!(
            !q.take_upcoming_radio("r1", RepeatMode::Off, Some(&chosen)),
            "it is playing"
        );
        assert!(
            !q.take_upcoming_radio("a", RepeatMode::Off, Some(&chosen)),
            "it was chosen"
        );
        assert!(
            !q.take_upcoming_radio("r3", RepeatMode::Off, None),
            "no radio"
        );
        assert!(q.take_upcoming_radio("r3", RepeatMode::Off, Some(&chosen)));
        chosen.insert("r3".into());
        let at = q.own_end(Some(&chosen));
        q.insert(at, track("r3"), Slot::BeforeRadio, Some(&chosen));
        assert_eq!(
            ids(&q, &(0..q.tracks.len()).collect::<Vec<_>>()),
            ["a", "r1", "r3", "r2"]
        );
        assert_eq!(q.current_track().unwrap().video_id, "r1");
    }

    /// Shuffle turned on part way through: the songs already played, an added one among them,
    /// do not come round again, and Previous still reaches them.
    #[test]
    fn shuffle_on_leaves_the_played_songs_behind() {
        let mut q = queue(&["a", "x", "r1", "r2", "r3", "r4"], 2);
        let chosen = set(&["a", "x"]);
        q.set_shuffle(true, Some(&chosen));
        let mut up = ids(&q, &q.upcoming(RepeatMode::Off, Some(&chosen)));
        up.sort();
        assert_eq!(up, ["r2", "r3", "r4"]);
        assert_eq!(q.current_track().unwrap().video_id, "r1");
        assert_eq!(
            q.previous_in_order()
                .map(|i| q.tracks[i].video_id.clone())
                .as_deref(),
            Some("x")
        );
        assert_eq!(q.chosen_before_radio(RepeatMode::Off, Some(&chosen)), 0);
    }

    #[test]
    fn removals_before_and_after_keep_the_playing_track() {
        let mut q = queue(&["a", "b", "c", "d"], 2);
        assert!(q.remove(0));
        assert_eq!(q.current_track().unwrap().video_id, "c");
        assert!(q.remove(2));
        assert_eq!(q.current_track().unwrap().video_id, "c");
        assert!(!q.remove(q.current().unwrap()), "the playing track stays");
    }

    #[test]
    fn the_prefix_trim_keeps_the_playing_track() {
        let list: Vec<String> = (0..MAX_PLAYED_PREFIX + 10).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = list.iter().map(String::as_str).collect();
        let current = MAX_PLAYED_PREFIX + 5;
        let mut q = queue(&refs, current);
        q.set_resume(Some((current, 9.0)));
        q.trim_played_prefix();
        assert_eq!(q.current(), Some(MAX_PLAYED_PREFIX));
        assert_eq!(q.current_track().unwrap().video_id, current.to_string());
        assert_eq!(q.resume_here(), Some(9.0));
    }

    /// A replaced queue drops the restored position, which belonged to the old queue's song.
    #[test]
    fn a_replaced_queue_drops_the_restored_position() {
        let mut q = queue(&["a", "b"], 0);
        q.set_resume(Some((0, 120.0)));
        q.replace(tracks(&["x"]), None, 0, None);
        q.set_current(0);
        assert_eq!(q.take_resume_for(0), None);
        match q.rewind() {
            Some(Rewind::PreviousQueue { index, queue, .. }) => {
                assert_eq!(index, 0);
                assert_eq!(queue.len(), 2);
            }
            _ => panic!("the old queue should come back"),
        }
    }

    /// Shuffle plays every track once, starting from the one playing, then stops under Repeat Off.
    #[test]
    fn a_shuffle_plays_each_track_once_then_stops() {
        let mut q = queue(&["a", "b", "c", "d", "e", "f"], 0);
        q.set_shuffle(true, None);
        let played = play_out(&mut q, RepeatMode::Off, None, 20);
        assert_eq!(played.len(), 5);
        let mut all: Vec<_> = played.clone();
        all.push("a".into());
        all.sort();
        assert_eq!(all, ["a", "b", "c", "d", "e", "f"]);
    }

    /// What Up next shows under shuffle is what Next then plays, in order.
    #[test]
    fn upcoming_is_what_next_plays() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 1);
        q.set_shuffle(true, None);
        let shown = ids(&q, &q.upcoming(RepeatMode::Off, None));
        let played = play_out(&mut q, RepeatMode::Off, None, 20);
        assert_eq!(shown, played);
        assert_eq!(q.peek_next(RepeatMode::Off, None), None);
    }

    /// Under Repeat All every round plays each track once, and a new round never starts with the
    /// track that ended the last one.
    #[test]
    fn shuffle_rounds_cover_every_track_without_a_repeat_at_the_seam() {
        for seed in 0..40 {
            let mut q = PlayQueue::new(seed);
            q.restore_session(tracks(&["a", "b", "c", "d"]), Some(0), None, true, None);
            let mut played = vec!["a".to_owned()];
            played.extend(play_out(&mut q, RepeatMode::All, None, 15));
            for round in played.chunks(4).filter(|r| r.len() == 4) {
                let mut r = round.to_vec();
                r.sort();
                assert_eq!(r, ["a", "b", "c", "d"], "seed {seed}: {played:?}");
            }
            for pair in played.windows(2) {
                assert_ne!(pair[0], pair[1], "seed {seed}: {played:?}");
            }
        }
    }

    /// A one-track queue under Repeat All replays it, shuffled or not.
    #[test]
    fn a_single_track_repeats_under_repeat_all() {
        for shuffle in [false, true] {
            for (repeat, expect) in [(RepeatMode::All, Some(0)), (RepeatMode::Off, None)] {
                let mut q = PlayQueue::new(3);
                q.restore_session(tracks(&["a"]), Some(0), None, shuffle, None);
                assert_eq!(
                    q.next(repeat, None),
                    expect,
                    "shuffle {shuffle}, {repeat:?}"
                );
            }
        }
    }

    /// Previous under shuffle retraces the order, even after a track is removed from the queue.
    #[test]
    fn previous_under_shuffle_retraces_the_order() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 0);
        q.set_shuffle(true, None);
        let played = play_out(&mut q, RepeatMode::Off, None, 3);
        let gone = q.upcoming(RepeatMode::Off, None)[0];
        q.remove(gone);
        let mut back = Vec::new();
        while let Some(i) = q.previous_in_order() {
            q.set_current(i);
            back.push(q.tracks[i].video_id.clone());
        }
        let mut expected: Vec<String> = vec!["a".into()];
        expected.extend(played[..2].iter().cloned());
        expected.reverse();
        assert_eq!(back, expected);
    }

    /// Turning shuffle on keeps the listener's songs before the radio's; off, the list order goes
    /// on from the track playing.
    #[test]
    fn shuffle_keeps_own_songs_before_the_radio_and_off_resumes_list_order() {
        let mut q = queue(&["a", "b", "c", "r1", "r2"], 0);
        let chosen = set(&["a", "b", "c"]);
        q.set_shuffle(true, Some(&chosen));
        let up = ids(&q, &q.upcoming(RepeatMode::Off, Some(&chosen)));
        let mut own = up[..2].to_vec();
        own.sort();
        assert_eq!(own, ["b", "c"]);
        let mut rad = up[2..].to_vec();
        rad.sort();
        assert_eq!(rad, ["r1", "r2"]);
        let next = q.next(RepeatMode::Off, Some(&chosen)).unwrap();
        q.set_current(next);
        q.set_shuffle(false, Some(&chosen));
        let expect: Vec<usize> = (next + 1..5).collect();
        assert_eq!(q.upcoming(RepeatMode::Off, Some(&chosen)), expect);
    }

    /// Play Next goes straight after the playing track in the order, Add to Queue after the
    /// listener's songs still to come, and the radio's songs among the radio's.
    #[test]
    fn added_songs_land_where_the_listener_expects() {
        let mut q = queue(&["a", "b", "c", "r1", "r2"], 0);
        let mut chosen = set(&["a", "b", "c"]);
        q.set_shuffle(true, Some(&chosen));
        q.insert(1, track("n"), Slot::Next(0), Some(&chosen));
        chosen.insert("n".into());
        assert_eq!(ids(&q, &q.upcoming(RepeatMode::Off, Some(&chosen)))[0], "n");
        chosen.insert("q".into());
        let at = q.own_end(Some(&chosen));
        q.insert(at, track("q"), Slot::BeforeRadio, Some(&chosen));
        let up = ids(&q, &q.upcoming(RepeatMode::Off, Some(&chosen)));
        assert_eq!(up[3], "q", "after n, b, c: {up:?}");
        let added = q.append_radio(&tracks(&["r1", "r3", "r3"]), Some(&chosen));
        assert_eq!(added, 1, "r1 is already queued, and r3 comes once");
        let up = ids(&q, &q.upcoming(RepeatMode::Off, Some(&chosen)));
        assert!(up[4..].contains(&"r3".to_owned()), "{up:?}");
        assert_eq!(up.len(), 7);
    }

    /// Repeat All under shuffle loops the listener's songs, passing over a radio tail.
    #[test]
    fn repeat_all_shuffle_loops_only_the_listeners_songs() {
        let mut q = queue(&["a", "b", "r1"], 0);
        let chosen = set(&["a", "b"]);
        q.set_shuffle(true, Some(&chosen));
        let played = play_out(&mut q, RepeatMode::All, Some(&chosen), 8);
        assert!(!played.contains(&"r1".to_owned()), "{played:?}");
        assert_eq!(played.len(), 8);
    }

    /// The history passed over under shuffle is the list-order one; a replaced queue still comes
    /// back.
    #[test]
    fn under_shuffle_previous_still_reaches_a_replaced_queue() {
        let mut q = queue(&["a", "b"], 0);
        q.replace(tracks(&["x", "y"]), Some(radio(&["a", "b"])), 0, None);
        q.set_current(0);
        q.remember_current();
        q.set_shuffle(true, None);
        assert!(matches!(q.rewind(), Some(Rewind::PreviousQueue { .. })));
    }
}
