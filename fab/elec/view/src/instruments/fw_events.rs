//! Read-only firmware events in simulated seconds, independent of the UI's acquisition.

use fab_elec_design::FwAt;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FwSource { Image, Trace }

impl FwSource {
    fn index(self) -> usize { match self { Self::Image => 0, Self::Trace => 1 } }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FwKind {
    // Image watch frames. Hello and Time are clock bookkeeping, not events.
    Write { pin: u8, level: bool },
    Duty { pin: u8, level: u8 },
    Read { pin: u8, level: bool },
    Fault { code: u8, arg: i32 },
    Lost { count: u32 },
    Probe { id: u8, value: u16 },
    Rx { byte: u8 },
    // The emulator's observation, never the image's.
    Enter { handler: u16 },
    Exit { handler: u16 },
    Statement { statement: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FwEvent {
    pub seq: u64,
    pub t: f64,
    pub source: FwSource,
    pub kind: FwKind,
    pub site: Option<u16>,
}

/// A read-only view of a firmware event store, ordered by (t, seq). Simulated seconds.
/// Real stores (the simulator's firmware event store) implement the same trait.
/// A view's queries describe one coherent retained snapshot. Time and handler searches are
/// O(log n); indexed reads are O(1) or O(log n), so drawing never needs to scan a window.
pub trait FwEvents: Send + Sync {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> Option<FwEvent>;
    /// The first index whose t is at or after `t` (a partition point; len when none).
    fn first_at(&self, t: f64) -> usize;
    /// As `get`, in just this source's (t, seq) order. O(1) or O(log n).
    fn get_source(&self, source: FwSource, index: usize) -> Option<FwEvent>;
    /// As `first_at`, in just this source's index space. O(log n).
    /// Two partition points give a column's count without scanning mixed-source events.
    fn first_at_source(&self, source: FwSource, t: f64) -> usize;
    /// Events rolled off before the oldest retained one.
    fn lost(&self) -> u64;
    /// The handler active at `t` (the innermost Enter without its Exit at or before t) and its Enter time.
    fn handler_at(&self, t: f64) -> Option<(u16, f64)>;
    fn pin_name(&self, pin: u8) -> Option<&str>;
    fn handler_name(&self, handler: u16) -> Option<&str>;
}

/// The last recorded event and active handler at `t`, each found in O(log n).
pub fn fw_at(events: &dyn FwEvents, t: f64) -> FwAt {
    let event = events.first_at(t.next_up()).checked_sub(1).and_then(|index| events.get(index)).map(|event| event.seq);
    FwAt { t, event, handler: events.handler_at(t).map(|(handler, _)| handler) }
}

/// The source identity shared by lane labels and capture replies.
pub fn source_name(source: FwSource) -> &'static str {
    match source { FwSource::Image => "image", FwSource::Trace => "trace" }
}

/// The lane and capture's common event label, using the store's pin and handler names.
pub fn label(events: &dyn FwEvents, event: FwEvent) -> String {
    let pin = |pin| events.pin_name(pin).map(str::to_owned).unwrap_or_else(|| format!("P{pin}"));
    let handler = |handler| events.handler_name(handler).map(str::to_owned).unwrap_or_else(|| handler.to_string());
    match event.kind {
        FwKind::Write { pin: id, level } => format!("write {} {}", pin(id), u8::from(level)),
        FwKind::Read { pin: id, level } => format!("read {} {}", pin(id), u8::from(level)),
        FwKind::Duty { pin: id, level } => format!("duty {} {level}", pin(id)),
        FwKind::Fault { code, .. } => format!("fault {code}"),
        FwKind::Lost { count } => format!("lost {count}"),
        FwKind::Probe { id, value } => format!("probe {id} {value}"),
        FwKind::Rx { byte } => format!("rx {byte}"),
        FwKind::Enter { handler: id } => format!("enter {}", handler(id)),
        FwKind::Exit { handler: id } => format!("exit {}", handler(id)),
        FwKind::Statement { statement } => format!("stmt {statement}"),
    }
}

/// A fixed script, sorted once at construction. Names and the rolled-off count are fixture data.
/// Handler transitions retain the innermost active Enter, including restoration of its parent
/// after a nested Exit. Queries binary-search those transitions instead of replaying the script.
#[derive(Clone, Debug, Default)]
pub struct ScriptedFwEvents {
    events: Vec<FwEvent>,
    rows: [Vec<usize>; 2],
    handlers: Vec<(f64, Option<(u16, f64)>)>,
    pub pin_names: HashMap<u8, String>,
    pub handler_names: HashMap<u16, String>,
    pub lost: u64,
}

impl ScriptedFwEvents {
    /// Times must be finite. An unmatched Exit is ignored (its Enter may have rolled off).
    pub fn new(mut events: Vec<FwEvent>) -> Self {
        assert!(events.iter().all(|event| event.t.is_finite()), "firmware event times must be finite");
        events.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap().then(a.seq.cmp(&b.seq)));
        Self::ordered(events)
    }

    /// Copy the inclusive window and its referenced names with two searches and k indexed
    /// reads: O(log n + k) for stores with O(1) indexed reads.
    /// Handler spans begun before `from` are not carried.
    pub fn window(store: &dyn FwEvents, from: f64, to: f64) -> ScriptedFwEvents {
        let first = store.first_at(from); let end = store.first_at(to.next_up());
        let events = (first..end).filter_map(|index| store.get(index)).collect();
        let mut window = Self::ordered(events);
        for event in &window.events {
            match event.kind {
                FwKind::Write { pin, .. } | FwKind::Read { pin, .. } | FwKind::Duty { pin, .. } => {
                    if let Some(name) = store.pin_name(pin) { window.pin_names.entry(pin).or_insert_with(|| name.to_owned()); }
                }
                FwKind::Enter { handler } | FwKind::Exit { handler } => {
                    if let Some(name) = store.handler_name(handler) { window.handler_names.entry(handler).or_insert_with(|| name.to_owned()); }
                }
                _ => (),
            }
        }
        if store.get(0).is_none_or(|oldest| oldest.t > from) { window.lost = store.lost(); }
        window
    }

    fn ordered(events: Vec<FwEvent>) -> Self {
        let mut handlers = Vec::new();
        let mut rows: [Vec<usize>; 2] = Default::default();
        let mut stack: Vec<(u16, f64)> = Vec::new();
        for (index, event) in events.iter().enumerate() {
            rows[event.source.index()].push(index);
            if event.source != FwSource::Trace { continue; }
            match event.kind {
                FwKind::Enter { handler } => stack.push((handler, event.t)),
                FwKind::Exit { handler } => {
                    if let Some(index) = stack.iter().rposition(|&(id, _)| id == handler) { stack.remove(index); }
                }
                _ => continue,
            }
            handlers.push((event.t, stack.last().copied()));
        }
        Self { events, rows, handlers, ..Self::default() }
    }
}

impl FwEvents for ScriptedFwEvents {
    fn len(&self) -> usize { self.events.len() }
    fn get(&self, index: usize) -> Option<FwEvent> { self.events.get(index).copied() }
    fn first_at(&self, t: f64) -> usize { self.events.partition_point(|event| event.t < t) }
    fn get_source(&self, source: FwSource, index: usize) -> Option<FwEvent> {
        self.rows[source.index()].get(index).map(|&index| self.events[index])
    }
    fn first_at_source(&self, source: FwSource, t: f64) -> usize {
        self.rows[source.index()].partition_point(|&index| self.events[index].t < t)
    }
    fn lost(&self) -> u64 { self.lost }
    fn handler_at(&self, t: f64) -> Option<(u16, f64)> {
        let end = self.handlers.partition_point(|(at, _)| *at <= t);
        end.checked_sub(1).and_then(|index| self.handlers[index].1)
    }
    fn pin_name(&self, pin: u8) -> Option<&str> { self.pin_names.get(&pin).map(String::as_str) }
    fn handler_name(&self, handler: u16) -> Option<&str> { self.handler_names.get(&handler).map(String::as_str) }
}
