//! Playback queue with shuffle order, repeat modes and de-duplication.

use std::collections::HashSet;

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
            // Insert at a random position after the current one.
            let at = if self.shuffle_order.is_empty() {
                0
            } else {
                rand_index(self.shuffle_order.len())
            };
            self.shuffle_order.insert(at, idx);
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

    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn current_track_mut(&mut self) -> Option<&mut Track> {
        self.current.and_then(|i| self.items.get_mut(i))
    }

    pub fn set_current(&mut self, index: usize) {
        if index < self.items.len() {
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
        if shuffle {
            self.build_shuffle_order(true);
        } else {
            self.shuffle_order.clear();
        }
    }

    pub fn build_shuffle_order(&mut self, current_first: bool) {
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.shuffle(&mut rand::rng());
        if current_first {
            if let Some(cur) = self.current {
                if let Some(pos) = order.iter().position(|i| *i == cur) {
                    order.swap(0, pos);
                }
            }
        }
        self.shuffle_order = order;
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
        self.shuffle_order.retain(|i| *i < self.items.len());
        for i in 0..self.items.len() {
            if !self.shuffle_order.contains(&i) {
                self.shuffle_order.push(i);
            }
        }
    }

    fn position_in_order(&self, index: usize) -> Option<usize> {
        let order = self.display_order();
        order.iter().position(|i| *i == index)
    }

    /// Next track index without changing the queue.
    pub fn peek_next(&mut self, auto: bool) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if auto && self.repeat == RepeatMode::One {
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
            Some(order[pos + 1])
        } else if self.repeat == RepeatMode::All || !auto {
            if self.shuffle {
                self.build_shuffle_order(false);
                self.shuffle_order.first().copied()
            } else {
                order.first().copied()
            }
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

    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn position_of_key(&self, key: &str) -> Option<usize> {
        self.items.iter().position(|t| t.dedupe_key() == key)
    }

    /// Moves an item in the underlying list (used by drag-reorder).
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return;
        }
        let track = self.items.remove(from);
        self.items.insert(to, track);
        // Rebuild indices after reorder; this happens rarely.
        self.shuffle_order.clear();
        self.current = None;
        self.history.clear();
        if self.shuffle {
            self.build_shuffle_order(false);
        }
    }
}

fn rand_index(upper: usize) -> usize {
    if upper == 0 {
        0
    } else {
        rand::random_range(0..=upper)
    }
}
