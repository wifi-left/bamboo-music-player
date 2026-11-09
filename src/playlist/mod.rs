//! Playback queue with shuffle order, repeat modes and de-duplication.

use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;

use crate::model::{RepeatMode, Track};

pub mod mp3list;

#[derive(Clone, Debug, Default)]
pub struct Queue {
    pub items: Vec<Track>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Indices into `items` in shuffled display order.
    pub shuffle_order: Vec<usize>,
    pub history: Vec<usize>,
    /// O(1) de-duplication for very large queues.
    pub keys: HashSet<String>,
    /// Once a shuffled cycle is underway, newly added tracks go to the end of
    /// the order instead of being inserted before already-played tracks.
    pub shuffle_append: bool,
    /// Next shuffled cycle, prepared ahead of time so the gapless deck can be
    /// pre-opened. It stays separate from `shuffle_order` (the visible list)
    /// until playback actually moves into the new cycle.
    pub(crate) pending_cycle: Option<Vec<usize>>,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
        self.shuffle_order.clear();
        self.shuffle_append = false;
        self.pending_cycle = None;
        self.history.clear();
        self.keys.clear();
    }

    pub fn rebuild_keys(&mut self) {
        self.keys = self
            .items
            .iter()
            .map(|t| t.dedupe_key())
            .collect::<HashSet<_>>();
    }

    /// Returns true when the track was actually added (not a duplicate).
    pub fn add(&mut self, track: Track) -> bool {
        let key = track.dedupe_key();
        if !self.keys.insert(key) {
            return false;
        }
        let idx = self.items.len();
        self.items.push(track);
        if self.shuffle {
            if self.shuffle_append || self.shuffle_order.is_empty() {
                // A restored cycle (or the very first track) keeps insertion
                // order; new songs wait until after the current cycle.
                self.shuffle_order.push(idx);
            } else {
                // Fresh import: distribute new tracks randomly.
                let at = rand_index(self.shuffle_order.len());
                self.shuffle_order.insert(at, idx);
            }
            // A prepared next cycle must include the new track too.
            if let Some(pending) = self.pending_cycle.as_mut() {
                pending.push(idx);
            }
        }
        true
    }

    pub fn add_many(&mut self, tracks: impl IntoIterator<Item = Track>) -> usize {
        let mut added = 0;
        for track in tracks {
            if self.add(track) {
                added += 1;
            }
        }
        added
    }

    pub fn set_current(&mut self, index: usize) {
        if index < self.items.len() {
            // Adopt a pre-built next cycle only when playback actually enters
            // it. Any other selection discards the pending cycle so a stale
            // order can never leak into the visible list.
            if let Some(pending) = self.pending_cycle.take()
                && pending.first() == Some(&index)
            {
                self.shuffle_order = pending;
            }
            if let Some(prev) = self.current {
                if prev != index {
                    self.history.push(prev);
                    if self.history.len() > 256 {
                        self.history.remove(0);
                    }
                }
            }
            self.current = Some(index);
            self.ensure_shuffle_order();
        }
    }

    /// Playback order as indices into `items`.
    pub fn display_order(&self) -> Vec<usize> {
        if self.shuffle {
            self.shuffle_order
                .iter()
                .copied()
                .filter(|i| *i < self.items.len())
                .collect()
        } else {
            (0..self.items.len()).collect()
        }
    }

    pub fn set_shuffle(&mut self, shuffle: bool) {
        if self.shuffle == shuffle {
            return;
        }
        self.shuffle = shuffle;
        self.pending_cycle = None;
        if shuffle {
            self.build_shuffle_order(true);
            self.shuffle_append = true;
        } else {
            self.shuffle_order.clear();
            self.shuffle_append = false;
        }
    }

    fn shuffled_cycle(&self, current_first: bool) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.shuffle(&mut rand::rng());
        if current_first
            && let Some(cur) = self.current
            && let Some(pos) = order.iter().position(|i| *i == cur)
        {
            order.swap(0, pos);
        }
        order
    }

    pub fn build_shuffle_order(&mut self, current_first: bool) {
        self.pending_cycle = None;
        self.shuffle_order = self.shuffled_cycle(current_first);
    }

    /// Explicit "re-randomize" action: start a fresh shuffle cycle while
    /// keeping the currently playing track as its first element.
    pub fn reroll_shuffle(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.shuffle = true;
        self.build_shuffle_order(true);
        self.shuffle_append = true;
        self.history.clear();
    }

    fn ensure_shuffle_order(&mut self) {
        if !self.shuffle {
            return;
        }
        if self.shuffle_order.len() != self.items.len() {
            self.build_shuffle_order(true);
            return;
        }
        // Remove stale indices and append missing ones (e.g. after removal).
        self.pending_cycle = None;
        self.shuffle_order.retain(|i| *i < self.items.len());
        for i in 0..self.items.len() {
            if !self.shuffle_order.contains(&i) {
                self.shuffle_order.push(i);
            }
        }
    }

    /// Rebuild the shuffle order from persisted track keys. Existing tracks
    /// keep their previous relative order, tracks that disappeared are
    /// skipped, and newly added tracks are appended to the end of the cycle.
    /// Returns how many persisted keys matched the queue.
    pub fn restore_shuffle_order(&mut self, keys: &[String]) -> usize {
        if !self.shuffle {
            return 0;
        }
        let mut by_key: HashMap<String, usize> = HashMap::with_capacity(self.items.len());
        for (index, track) in self.items.iter().enumerate() {
            by_key.insert(track.dedupe_key(), index);
        }
        let mut used = HashSet::with_capacity(self.items.len());
        let mut order = Vec::with_capacity(self.items.len());
        let mut matched = 0usize;
        for key in keys {
            if let Some(&index) = by_key.get(key) {
                if used.insert(index) {
                    order.push(index);
                    matched += 1;
                }
            }
        }
        for index in 0..self.items.len() {
            if used.insert(index) {
                order.push(index);
            }
        }
        self.shuffle_order = order;
        self.pending_cycle = None;
        // Everything after this point is "new" and belongs after the cycle.
        self.shuffle_append = true;
        matched
    }

    /// Next track index without changing the visible order. Automatic peeks
    /// at the end of a shuffled cycle prepare the next cycle privately; the
    /// list only switches when playback actually advances into it.
    pub fn peek_next(&mut self, auto: bool) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if auto && self.repeat == RepeatMode::One {
            self.pending_cycle = None;
            return self.current;
        }
        let order = self.display_order();
        if order.is_empty() {
            return None;
        }
        let pos = self
            .current
            .and_then(|cur| order.iter().position(|i| *i == cur))
            .unwrap_or(0);
        if pos + 1 < order.len() {
            return Some(order[pos + 1]);
        }

        // At the end of the current order.
        if self.shuffle {
            let starts_new_cycle = self.repeat == RepeatMode::All || !auto;
            if !starts_new_cycle {
                self.pending_cycle = None;
                return None;
            }
            if auto {
                // Pre-arm the gapless deck with the first track of the next
                // cycle, but keep the visible order until that track actually
                // starts. Otherwise the list would re-randomize while the last
                // song is still playing (and again on every poll).
                if self.pending_cycle.is_none() {
                    self.pending_cycle = Some(self.shuffled_cycle(false));
                }
                self.pending_cycle
                    .as_ref()
                    .and_then(|order| order.first().copied())
            } else {
                // Explicit Next: start the new cycle now.
                self.build_shuffle_order(false);
                self.shuffle_order.first().copied()
            }
        } else if self.repeat == RepeatMode::All || !auto {
            order.first().copied()
        } else {
            None
        }
    }

    pub fn peek_prev(&mut self) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let order = self.display_order();
        if order.is_empty() {
            return None;
        }
        let pos = self
            .current
            .and_then(|cur| order.iter().position(|i| *i == cur))
            .unwrap_or(0);
        if pos > 0 {
            Some(order[pos - 1])
        } else {
            order.last().copied()
        }
    }

    pub fn advance(&mut self, auto: bool) -> Option<usize> {
        let next = self.peek_next(auto)?;
        self.set_current(next);
        Some(next)
    }

    pub fn back(&mut self) -> Option<usize> {
        let prev = self.peek_prev()?;
        self.set_current(prev);
        Some(prev)
    }

    /// Removes an item and keeps the current selection sensible.
    pub fn remove(&mut self, index: usize) -> Option<Track> {
        if index >= self.items.len() {
            return None;
        }
        let removed = self.items.remove(index);
        self.keys.remove(&removed.dedupe_key());
        if let Some(pending) = self.pending_cycle.as_mut() {
            pending.retain(|i| *i != index);
            for i in pending.iter_mut() {
                if *i > index {
                    *i -= 1;
                }
            }
        }
        self.shuffle_order.retain(|i| *i != index);
        for i in self.shuffle_order.iter_mut() {
            if *i > index {
                *i -= 1;
            }
        }
        self.history.retain(|i| *i != index);
        for i in self.history.iter_mut() {
            if *i > index {
                *i -= 1;
            }
        }
        match self.current {
            Some(cur) if cur == index => {
                if self.items.is_empty() {
                    self.current = None;
                } else if cur >= self.items.len() {
                    self.current = Some(self.items.len() - 1);
                }
                // Otherwise the next item shifted into this index.
            }
            Some(cur) if cur > index => self.current = Some(cur - 1),
            _ => {}
        }
        Some(removed)
    }

    pub fn position_of_key(&self, key: &str) -> Option<usize> {
        self.items.iter().position(|t| t.dedupe_key() == key)
    }

    /// Move the track at playback position `from` to `to` (positions in
    /// [`display_order`](Self::display_order)). In shuffle mode only the
    /// playback cycle changes; otherwise the underlying list is reordered and
    /// the current/history indices are remapped so the playing track and the
    /// back-history keep pointing at the same songs.
    pub fn move_in_order(&mut self, from: usize, to: usize) -> bool {
        if self.items.is_empty() {
            return false;
        }
        self.ensure_shuffle_order();
        let mut order = self.display_order();
        if from >= order.len() || to >= order.len() || from == to {
            return false;
        }
        let moved = order.remove(from);
        order.insert(to, moved);
        if self.shuffle {
            // A manual reorder supersedes any prepared next cycle.
            self.pending_cycle = None;
            self.shuffle_order = order;
            return true;
        }

        let mut slots: Vec<Option<Track>> = self.items.drain(..).map(Some).collect();
        let mut reordered = Vec::with_capacity(order.len());
        for old in &order {
            if let Some(track) = slots.get_mut(*old).and_then(Option::take) {
                reordered.push(track);
            }
        }
        self.items = reordered;
        let remap = |index: usize| order.iter().position(|old| *old == index).unwrap_or(index);
        self.current = self.current.map(remap);
        self.history = self
            .history
            .iter()
            .filter_map(|index| order.iter().position(|old| old == index))
            .collect();
        true
    }
}

fn rand_index(upper: usize) -> usize {
    if upper == 0 {
        0
    } else {
        rand::random_range(0..=upper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(name: &str) -> Track {
        Track::local(format!("C:\\music\\{name}.mp3"))
    }

    #[test]
    fn restored_shuffle_keeps_progress_and_appends_new_tracks() {
        let mut old = Queue::default();
        old.shuffle = true;
        for name in ["a", "b", "c"] {
            old.add(track(name));
        }
        old.shuffle_order = vec![1, 2, 0];
        let keys: Vec<String> = old
            .shuffle_order
            .iter()
            .map(|&index| old.items[index].dedupe_key())
            .collect();

        let mut restored = Queue::default();
        restored.shuffle = true;
        restored.shuffle_append = true;
        for name in ["b", "c"] {
            restored.add(track(name));
        }
        assert_eq!(restored.restore_shuffle_order(&keys), 2);
        assert_eq!(restored.display_order(), vec![0, 1]);

        restored.add(track("d"));
        assert_eq!(restored.display_order(), vec![0, 1, 2]);
    }

    #[test]
    fn shuffle_cycle_restarts_from_a_new_order() {
        let mut queue = Queue::default();
        queue.shuffle = true;
        queue.repeat = RepeatMode::All;
        for name in ["a", "b", "c"] {
            queue.add(track(name));
        }
        queue.current = Some(queue.shuffle_order[2]);
        assert!(queue.advance(true).is_some());
        assert_eq!(queue.shuffle_order.len(), 3);
    }

    #[test]
    fn shuffle_next_cycle_is_published_only_when_playback_advances() {
        let mut queue = Queue::default();
        queue.shuffle = true;
        queue.repeat = RepeatMode::All;
        for name in ["a", "b", "c", "d"] {
            queue.add(track(name));
        }
        let visible = queue.shuffle_order.clone();
        queue.current = Some(visible[3]);

        let first = queue.peek_next(true).expect("next cycle is prepared");
        assert_eq!(
            queue.shuffle_order, visible,
            "peeking must not re-randomize the visible list"
        );
        assert_eq!(
            queue.peek_next(true),
            Some(first),
            "repeated polls keep the same prepared track"
        );
        assert_eq!(queue.shuffle_order, visible);

        assert_eq!(queue.advance(true), Some(first));
        assert_eq!(
            queue.shuffle_order.first(),
            Some(&first),
            "the new order becomes visible when the next track starts"
        );
    }

    #[test]
    fn shuffle_last_track_without_repeat_does_not_build_a_cycle() {
        let mut queue = Queue::default();
        queue.shuffle = true;
        for name in ["a", "b", "c"] {
            queue.add(track(name));
        }
        let visible = queue.shuffle_order.clone();
        queue.current = Some(visible[2]);
        assert_eq!(queue.peek_next(true), None);
        assert_eq!(queue.shuffle_order, visible);
        assert!(queue.pending_cycle.is_none());
    }

    fn order_titles(queue: &Queue) -> Vec<String> {
        queue
            .display_order()
            .iter()
            .map(|&index| queue.items[index].display_title())
            .collect()
    }

    #[test]
    fn move_in_order_reorders_and_remaps_current_track() {
        let mut queue = Queue::default();
        for name in ["a", "b", "c", "d"] {
            queue.add(track(name));
        }
        queue.current = Some(2);
        queue.history = vec![0, 1];
        assert!(queue.move_in_order(1, 3));
        assert_eq!(order_titles(&queue), ["a", "c", "d", "b"]);
        assert_eq!(queue.current, Some(1), "current track follows its item");
        assert_eq!(queue.history, vec![0, 3]);
        assert!(!queue.move_in_order(0, 9), "out-of-range moves are ignored");
        assert!(!queue.move_in_order(1, 1), "no-op moves are ignored");
    }

    #[test]
    fn move_in_order_only_changes_the_shuffle_cycle() {
        let mut queue = Queue::default();
        for name in ["a", "b", "c", "d"] {
            queue.add(track(name));
        }
        queue.shuffle = true;
        queue.shuffle_order = vec![2, 0, 3, 1];
        queue.current = Some(0);
        assert!(queue.move_in_order(0, 2));
        assert_eq!(queue.shuffle_order, vec![0, 3, 2, 1]);
        assert_eq!(queue.current, Some(0), "underlying item indices are stable");
        assert_eq!(queue.items.len(), 4);
    }

    #[test]
    fn reroll_keeps_current_track_first() {
        let mut queue = Queue::default();
        for name in ["a", "b", "c", "d"] {
            queue.add(track(name));
        }
        queue.current = Some(2);
        queue.reroll_shuffle();
        assert!(queue.shuffle);
        assert_eq!(queue.shuffle_order.first(), Some(&2));
        let mut sorted = queue.shuffle_order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3]);
        assert!(queue.shuffle_append);
    }
}
