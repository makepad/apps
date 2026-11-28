//! Process-local receipts for the human watching their agent. Only a returned page starts work.
use makepad_town_core::{event::Id, queue::Credential};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase { Sent, NotPickedUp, GotIt, Working, Quiet, NoSign, Done, Away }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ink { Action, Status, Warning }

impl Phase {
    pub fn ticking(self) -> bool { matches!(self, Self::NotPickedUp | Self::Quiet | Self::NoSign) }
    pub fn finished(self) -> bool { matches!(self, Self::Done | Self::Away) }
}

pub struct Output {
    pub text: String,
    pub ink: Ink,
    pub at: u64,
}

pub struct Trace {
    pub agent: Id,
    pub name: Arc<str>,
    binding: Credential,
    sent: u64,
    offered: u64,
    pub pickup: Option<u64>,
    last_signal: u64,
    ended: Option<Phase>,
    pub output: Vec<Arc<Output>>,
}

impl Trace {
    pub fn phase(&self, now: u64) -> Phase {
        if let Some(end) = self.ended { return end }
        if self.pickup.is_none() {
            return if now.saturating_sub(self.offered) >= 2_000 { Phase::NotPickedUp } else { Phase::Sent }
        }
        match now.saturating_sub(self.last_signal) {
            120_000.. => Phase::NoSign,
            8_000.. => Phase::Quiet,
            _ if self.output.is_empty() => Phase::GotIt,
            _ => Phase::Working,
        }
    }

    pub fn spin_until(&self, now: u64) -> Option<u64> {
        (self.phase(now) == Phase::Working).then(|| self.last_signal.saturating_add(8_000))
    }

    fn next_change(&self, now: u64) -> Option<u64> {
        // All elapsed labels share one wall-second tick, independent of their send times.
        let second = now.saturating_add(1000 - now % 1000);
        match self.phase(now) {
            Phase::Sent => Some(self.offered.saturating_add(2_000)),
            Phase::Working | Phase::GotIt => Some(self.last_signal.saturating_add(8_000)),
            Phase::Quiet => Some(second.min(self.last_signal.saturating_add(120_000))),
            Phase::NotPickedUp | Phase::NoSign => Some(second),
            Phase::Done | Phase::Away => None,
        }
    }

    pub fn headline(&self, now: u64, clock: &crate::view::LocalClock) -> String {
        let since = |at| elapsed(now.saturating_sub(at) / 1000);
        match self.phase(now) {
            Phase::Sent => "sent".into(),
            Phase::NotPickedUp => format!("{} hasn't picked this up · {}", self.name, since(self.sent)),
            Phase::GotIt => format!("{} got it · {}", self.name, clock.hms(self.pickup.unwrap())),
            Phase::Working => "working".into(),
            Phase::Quiet => {
                let mut text = format!("working · {} since it got it", since(self.pickup.unwrap()));
                if let Some(last) = self.output.last() {
                    text.push_str(&format!(" · last: {}, {} ago", single_line(&last.text), since(last.at)));
                }
                text
            }
            Phase::NoSign => format!("No sign from {} for {}", self.name, since(self.last_signal)),
            Phase::Done => "done".into(),
            Phase::Away => format!("{} left before answering", self.name),
        }
    }
}

pub(crate) fn single_line(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\r', '\n', '\u{2028}', '\u{2029}']) {
        text.replace("\r\n", " ").replace(['\r', '\n', '\u{2028}', '\u{2029}'], " ").into()
    } else { text.into() }
}

fn elapsed(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

#[derive(Default)]
pub struct Traces {
    // Two agents can be seeded with the same unanswered line. Keep each receipt separately.
    rows: HashMap<Id, Vec<Trace>>,
    open: BTreeMap<Credential, Open>,
    working: BTreeSet<Credential>,
}

struct Open {
    agent: Id,
    lines: Vec<(Id, usize)>,
}

impl Traces {
    pub fn rows(&self, line: &Id) -> &[Trace] { self.rows.get(line).map_or(&[], Vec::as_slice) }
    pub fn working(&self, binding: Credential) -> bool { self.working.contains(&binding) }

    /// Refresh the town's receipts, keeping the last known name when an agent has left.
    pub fn sync_names(&mut self, town: Id, mut name_of: impl FnMut(Id) -> Option<String>) {
        if self.rows.is_empty() { return }
        let mut names = HashMap::new();
        for row in self.rows.values_mut().flatten().filter(|row| row.binding.town == town) {
            let name = names.entry(row.agent).or_insert_with(|| name_of(row.agent).map(|name| {
                if name == row.name.as_ref() { row.name.clone() } else { Arc::<str>::from(name) }
            }));
            if let Some(name) = name {
                if row.name != *name { row.name = name.clone(); }
            }
        }
    }

    pub(crate) fn offer(&mut self, binding: Credential, agent: Id, name: &str, line: Id, sent: u64, now: u64) {
        let rows = self.rows.entry(line).or_default();
        if rows.iter().any(|r| r.binding == binding) { return }
        self.open.entry(binding).or_insert_with(|| Open { agent, lines: vec![] }).lines.push((line, rows.len()));
        rows.push(Trace { binding, agent, name: name.into(), sent, offered: now, pickup: None,
            last_signal: now, ended: None, output: vec![] });
    }

    pub(crate) fn pickup(&mut self, binding: Credential, agent: Id, name: &str, lines: &[Id], now: u64) {
        let mut active = false;
        for &line in lines {
            // This can be the first local observation after restart or approval of a peer request.
            self.offer(binding, agent, name, line, now, now);
            let row = self.rows.get_mut(&line).unwrap().iter_mut().find(|r| r.binding == binding).unwrap();
            if row.ended.is_some() { continue }
            row.pickup.get_or_insert(now);
            row.last_signal = now;
            active = true;
        }
        if active {
            self.working.insert(binding);
        }
    }

    pub(crate) fn output(&mut self, binding: Credential, text: String, ink: Ink, at: u64) -> bool {
        if !self.working(binding) { return false }
        let output = Arc::new(Output { text, ink, at });
        for &(line, index) in &self.open[&binding].lines {
            let row = &mut self.rows.get_mut(&line).unwrap()[index];
            if row.pickup.is_some() {
                row.last_signal = at;
                row.output.push(output.clone());
            }
        }
        true
    }

    /// Attach a tool signal to the picked-up request that started it.
    pub(crate) fn output_on(&mut self, binding: Credential, line: Id, text: String, ink: Ink, at: u64) -> bool {
        let Some(open) = self.open.get(&binding) else { return false };
        let Some(&(_, index)) = open.lines.iter().find(|(id, _)| *id == line) else { return false };
        let row = &mut self.rows.get_mut(&line).unwrap()[index];
        if row.pickup.is_none() { return false }
        row.last_signal = at;
        row.output.push(Arc::new(Output { text, ink, at }));
        true
    }

    /// Done ends the picked-up lines and leaves undelivered ones waiting; Away ends them all.
    pub(crate) fn finish(&mut self, binding: Credential, end: Phase) {
        let Some(open) = self.open.get_mut(&binding) else { return };
        open.lines.retain(|&(line, index)| {
            let row = &mut self.rows.get_mut(&line).unwrap()[index];
            if end == Phase::Done && row.pickup.is_none() { return true }
            row.ended = Some(end);
            false
        });
        if open.lines.is_empty() { self.open.remove(&binding); }
        self.working.remove(&binding);
    }

    pub(crate) fn close_ended(&mut self, ended: impl Fn(Credential, Id) -> bool) {
        let ended: Vec<_> = self.open.iter().filter(|(binding, o)| ended(**binding, o.agent)).map(|(b, _)| *b).collect();
        for binding in ended { self.finish(binding, Phase::Away); }
    }

    pub fn open_lines(&self, binding: Credential) -> impl Iterator<Item = Id> + '_ {
        self.open.get(&binding).into_iter().flat_map(|open| open.lines.iter().map(|(id, _)| *id))
    }

    pub fn bindings(&self, town: Id) -> impl Iterator<Item = Credential> + '_ {
        self.open.keys().filter(move |b| b.town == town).copied()
    }

    fn in_flight(&self, town: Id) -> impl Iterator<Item = &Trace> {
        self.open.iter().filter(move |(b, _)| b.town == town).flat_map(|(_, open)| {
            open.lines.iter().map(|(line, index)| &self.rows[line][*index])
        })
    }

    /// Absolute wall time for the next change in the town the window is showing.
    pub fn next_change(&self, town: Id, now: u64) -> Option<u64> {
        self.in_flight(town).filter_map(|row| row.next_change(now)).min()
    }

    pub fn animating(&self, town: Id, now: u64) -> bool {
        self.in_flight(town).any(|row| row.spin_until(now).is_some())
    }

    pub fn has_open(&self) -> bool { !self.open.is_empty() }

    pub fn current(&self, binding: Credential) -> Option<&Trace> {
        self.open.get(&binding)?.lines.iter().filter_map(|(line, index)| {
            let row = &self.rows[line][*index];
            row.pickup.map(|at| (at, row))
        }).max_by_key(|(at, _)| *at).map(|(_, row)| row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_and_motion_end_with_the_last_open_trace() {
        let town = [1; 32];
        let binding = Credential { town, check: [2; 32] };
        let (agent, line) = ([3; 32], [4; 32]);
        let mut traces = Traces::default();
        let schedule = |traces: &Traces, now| (traces.next_change(town, now), traces.animating(town, now));
        assert_eq!(schedule(&traces, 0), (None, false), "an idle town needs no wake or frame");
        traces.offer(binding, agent, "AI", line, 100, 100);
        assert_eq!(schedule(&traces, 100), (Some(2_100), false), "sent wakes precisely at the offer boundary");
        assert_eq!(schedule(&traces, 2_100), (Some(3_000), false), "only elapsed text ticks while waiting");
        traces.pickup(binding, agent, "AI", &[line], 2_347);
        assert_eq!(schedule(&traces, 2_347), (Some(10_347), false), "receipt alone stays still until the quiet boundary");
        traces.output(binding, "read board".into(), Ink::Action, 3_150);
        assert_eq!(schedule(&traces, 3_151), (Some(11_150), true), "fresh output moves until eight seconds after its signal");
        assert_eq!(schedule(&traces, 11_150), (Some(12_000), false), "a quiet trace ticks without requesting frames");
        assert_eq!(schedule(&traces, 123_000), (Some(123_150), false), "the two-minute boundary can precede the next whole second");
        assert_eq!(schedule(&traces, 123_150), (Some(124_000), false), "no sign keeps ticking once a second");
        assert_eq!(traces.next_change([0; 32], 123_151), None, "background towns do not wake this window");
        traces.finish(binding, Phase::Done);
        assert_eq!(schedule(&traces, 123_151), (None, false), "finishing the last task cancels all trace wakes and frames");
    }
}
