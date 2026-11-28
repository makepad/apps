//! Numbering and ownership of the media sessions on one authenticated link.
use super::signal::MediaSignal;

#[cfg_attr(test, derive(Hash))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Want { on: bool, last: u32, running: bool }

const IDLE_WANT: Want = Want { on: false, last: 0, running: false };

#[cfg_attr(test, derive(Clone, PartialEq, Eq, Hash))]
pub(super) struct MediaSession {
    pub(super) session_last: u32,
    peer_want: Want,
    want_sent: Option<Want>,
}

impl Default for MediaSession {
    fn default() -> Self {
        // Both seats already know the follower is idle at link-up, so it sends no frame for that level.
        Self { session_last: 0, peer_want: IDLE_WANT, want_sent: Some(IDLE_WANT) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    #[derive(Clone, Default, PartialEq, Eq, Hash)]
    struct Pair {
        machine: [MediaSession; 2],
        held: [Option<u32>; 2],
        toggles: [u8; 2],
        inbox: [VecDeque<MediaSignal>; 2],
    }

    impl Pair {
        fn need(&self, side: usize) -> bool { self.toggles[side] % 2 == 1 }

        fn apply(&mut self, side: usize, before: u32, changes: Changes) {
            if changes.retire { assert!(self.held[side].take().is_some()); }
            if let Some(n) = changes.announce_end {
                self.inbox[1-side].push_back(MediaSignal::Media { n, on: false });
            }
            if let Some(n) = changes.start {
                assert!(n > before && self.held[side].replace(n).is_none(), "each start takes a fresh number");
                assert!(n <= self.machine[0].session_last, "the keeper has issued every accepted number");
            }
            if let Some(msg) = changes.send { self.inbox[1-side].push_back(msg); }
        }

        fn settle(&mut self, side: usize, ended: bool) {
            if ended { self.held[side] = None; }
            let own = self.need(side);
            let before = self.machine[side].session_last;
            let changes = self.machine[side].settle(side == 0, own, self.held[side].is_some(), ended, true);
            if side == 1 { assert!(changes.start.is_none() && !changes.retire); }
            self.apply(side, before, changes);
        }
    }

    #[test]
    fn need_changes_and_fifo_deliveries_converge_in_every_order() {
        for ends_on_lapse in [false, true] {
            let mut initial = Pair::default();
            initial.settle(0, false);
            initial.settle(1, false);
            assert!(initial.inbox.iter().all(VecDeque::is_empty), "an idle key exchange sends no Want");
            let mut todo = vec![initial];
            let mut visited = HashSet::new();
            while let Some(pair) = todo.pop() {
                if !visited.insert(pair.clone()) { continue; }
                let starts = pair.machine[0].session_last;
                assert!(starts <= u32::from(pair.toggles[0] + pair.toggles[1]) + 1, "spurious restart");
                if pair.inbox.iter().all(VecDeque::is_empty) {
                    if pair.need(1) {
                        assert_eq!(pair.held, [Some(starts); 2], "the follower needs a shared session");
                    } else if pair.need(0) {
                        assert_eq!(pair.held[0], Some(starts));
                        assert!(pair.held[1].is_none() || pair.held[1] == pair.held[0]);
                    } else { assert_eq!(pair.held, [None; 2]); }
                }
                for side in 0..2 {
                    if pair.toggles[side] < 3 {
                        let mut next = pair.clone();
                        next.toggles[side] += 1;
                        next.settle(side, ends_on_lapse && !next.need(side) && next.held[side].is_some());
                        todo.push(next);
                    }
                    if let Some(&msg) = pair.inbox[side].front() {
                        let mut next = pair.clone();
                        next.inbox[side].pop_front();
                        let before = next.machine[side].session_last;
                        let changes = next.machine[side].receive(side == 0, next.held[side].is_some(), true, msg).unwrap();
                        if side == 1 {
                            if let Some(n) = changes.start { assert_eq!(msg, MediaSignal::Media { n, on: true }); }
                        }
                        next.apply(side, before, changes);
                        next.settle(side, false);
                        todo.push(next);
                    }
                }
            }
        }
    }
}

/// In order: retire and announce the old session, start the new one, then report it.
#[derive(Default)]
pub(super) struct Changes {
    pub retire: bool,
    pub announce_end: Option<u32>,
    pub start: Option<u32>,
    pub send: Option<MediaSignal>,
    pub exhausted: bool,
}

impl MediaSession {
    pub(super) fn receive(&mut self, keeper: bool, held: bool, keys: bool, msg: MediaSignal) -> Result<Changes, &'static str> {
        use MediaSignal::*;
        let mut changes = Changes::default();
        match (keeper, msg) {
            (true, Want { last, .. }) if last > self.session_last => return Err("Want for a future session"),
            (true, Want { on, last, running }) => self.peer_want = self::Want { on, last, running },
            (false, Media { n, on: true }) if n > self.session_last => {
                changes.retire = held;
                self.session_last = n;
                // An honest keeper's MediaKey always precedes its Media on the reliable link.
                if keys { changes.start = Some(n); }
            }
            (false, Media { on: true, .. }) => return Err("repeated or older Media start"),
            (false, Media { n, on: false }) => changes.retire = held && n == self.session_last,
            (true, Media { .. }) => return Err("Media from the follower"),
            (false, Want { .. }) => return Err("Want from the keeper"),
        }
        Ok(changes)
    }

    /// `held` is sampled after retiring a driver that ended itself.
    pub(super) fn settle(&mut self, keeper: bool, own: bool, held: bool, ended: bool, keys: bool) -> Changes {
        let mut changes = Changes::default();
        let n = self.session_last;
        if !keeper {
            let want = Want { on: own, last: n, running: held };
            if self.want_sent != Some(want) {
                self.want_sent = Some(want);
                changes.send = Some(MediaSignal::Want { on: own, last: n, running: held });
            }
            return changes;
        }
        let w = self.peer_want;
        let peer_needs_start = w.last == n && w.on && !w.running;
        let idle = !own && w.last == n && !w.on;
        changes.retire = held && (peer_needs_start || idle);
        if ended || changes.retire { changes.announce_end = Some(n); }
        if (!held || changes.retire) && keys && (own || peer_needs_start) {
            if let Some(next) = n.checked_add(1) {
                self.session_last = next;
                changes.start = Some(next);
                changes.send = Some(MediaSignal::Media { n: next, on: true });
            } else {
                changes.exhausted = true;
            }
        }
        changes
    }
}
