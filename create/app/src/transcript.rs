//! Finished local utterances cross from the transcriber to the UI here.
use makepad_town_core::event::{SaidLine, SessionId};
use std::collections::BTreeMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

pub struct Heard {
    pub session: SessionId,
    pub at_ms: u32,
    pub dur_ms: u32,
    pub text: String,
}

#[derive(Default)]
struct Pending {
    since: Option<u64>,
    bytes: usize,
    lines: Vec<SaidLine>,
}

pub struct TranscriptSink {
    rx: Receiver<Heard>,
    pending: BTreeMap<SessionId, Pending>,
    ready: Vec<(SessionId, Vec<SaidLine>)>,
}

/// The worker retains the utterance returned by a full try_send and retries it.
pub fn channel() -> (SyncSender<Heard>, TranscriptSink) {
    let (tx, rx) = sync_channel(32);
    (tx, TranscriptSink { rx, pending: BTreeMap::new(), ready: Vec::new() })
}

impl TranscriptSink {
    fn receive(&mut self, now_ms: Option<u64>) {
        for heard in self.rx.try_iter() {
            let p = self.pending.entry(heard.session).or_default();
            if p.since.is_none() { p.since = now_ms; }
            p.bytes += heard.text.len();
            p.lines.push(SaidLine { at_ms: heard.at_ms, dur_ms: heard.dur_ms, text: heard.text });
            if p.bytes > 400 {
                self.ready.push((heard.session, self.pending.remove(&heard.session).unwrap().lines));
            }
        }
    }

    pub fn poll(&mut self, now_ms: u64) -> Vec<(SessionId, Vec<SaidLine>)> {
        self.receive(Some(now_ms));
        self.pending.retain(|id, p| {
            if now_ms.saturating_sub(*p.since.get_or_insert(now_ms)) < 8_000 { return true }
            self.ready.push((*id, std::mem::take(&mut p.lines)));
            false
        });
        std::mem::take(&mut self.ready)
    }

    pub fn flush(&mut self, session: SessionId) -> Option<Vec<SaidLine>> {
        self.receive(None);
        let mut lines = Vec::new();
        self.ready.retain_mut(|(id, batch)| {
            if *id != session { return true }
            lines.append(batch);
            false
        });
        if let Some(mut p) = self.pending.remove(&session) { lines.append(&mut p.lines); }
        (!lines.is_empty()).then_some(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finished_lines_flush_by_age_size_or_session_boundary() {
        let (tx, mut sink) = channel();
        let (a, b) = ([1; 16], [2; 16]);
        let send = |session, at_ms, text: &str| tx.try_send(Heard { session, at_ms, dur_ms: 10, text: text.into() }).unwrap();
        send(a, 7, "first");
        assert!(sink.poll(500).is_empty(), "the timer starts on receipt");
        send(b, 1, &"é".repeat(200));
        assert!(sink.poll(8_499).is_empty(), "neither 7,999 ms nor exactly 400 bytes flushes");
        send(a, 4, "second");
        let aged = sink.poll(8_500);
        assert_eq!(aged.len(), 1, "only the oldest session is due");
        assert_eq!((aged[0].0, aged[0].1.iter().map(|l| l.at_ms).collect::<Vec<_>>()), (a, vec![7, 4]), "batch preserves receipt order");
        send(b, 2, "!");
        send(b, 3, &"b".repeat(401));
        let full = sink.poll(8_501);
        assert_eq!(full.iter().map(|(id, l)| (*id, l.len())).collect::<Vec<_>>(), [(b, 2), (b, 1)], "each completed utterance can finish a batch");
        send(b, 4, "stay");
        send(a, 8, "last");
        assert_eq!(sink.flush(a).unwrap()[0].text, "last", "a leave drains queued speech immediately");
        assert!(sink.flush(a).is_none(), "a second flush cannot duplicate words");
        assert_eq!(sink.flush(b).unwrap()[0].text, "stay", "the other session keeps its words");
    }
}
