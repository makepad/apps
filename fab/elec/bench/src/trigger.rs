//! Edge detection over a sampled stream.

pub use crate::measure::Slope;
use crate::edges::{EdgeTracker, Threshold};
use crate::source::TriggerKind;

/// What an edge or excursion must satisfy. Interval bounds are seconds, `min` inclusive, `max`
/// exclusive; `min = 0` and `max = INFINITY` leave that side open.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Qualifier {
    #[default]
    None,
    /// From the previous confirmed edge of the opposite direction to this edge (a pulse width).
    Width { min: f64, max: f64 },
    /// From the previous confirmed edge of the same direction to this edge (a period).
    Period { min: f64, max: f64 },
    /// A pulse returning through the trigger level without reaching this second level in volts.
    Runt { other: f64 },
}

impl Qualifier {
    pub fn kind(&self) -> TriggerKind {
        match self { Self::None => TriggerKind::Edge, Self::Width { .. } | Self::Period { .. } => TriggerKind::Width, Self::Runt { .. } => TriggerKind::Runt }
    }
}

pub fn validate_qualifier(qualifier: Qualifier) -> bool {
    match qualifier {
        Qualifier::None => true,
        Qualifier::Width { min, max } | Qualifier::Period { min, max } => min.is_finite() && min >= 0.0 && max > min,
        Qualifier::Runt { other } => other.is_finite(),
    }
}

/// For a person's edit: the second level more than a full hysteresis band away on the slope's
/// side. The detector itself takes any finite level; its side picks the pulse direction, and a
/// level inside the band (a scale change can widen it) simply finds no runts.
pub fn validate_runt(trigger: &EdgeTrigger, other: f64) -> bool {
    other.is_finite() && trigger.level.is_finite() && trigger.hyst.is_finite() && trigger.hyst >= 0.0 && match trigger.slope {
        Slope::Rising => other > trigger.level + trigger.hyst,
        Slope::Falling => other < trigger.level - trigger.hyst,
        Slope::Either => (other - trigger.level).abs() > trigger.hyst,
    }
}

/// An edge timed at its last level crossing toward the confirming band edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeTrigger {
    pub level: f64,
    /// Band width in volts. Each slope arms beyond the opposite band edge.
    pub hyst: f64,
    pub slope: Slope,
    /// Minimum seconds between accepted events.
    pub holdoff: f64,
}

/// Seconds to retain before and after an event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub pre: f64,
    pub post: f64,
}

/// The latest crossing's state after a sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    None,
    Pending(f64),
    Accepted(f64),
    Dropped,
}

#[derive(Clone, Copy)]
struct Sample {
    t: f64,
    y: f32,
}

/// Shared threshold edges, with holdoff measured from accepted crossings.
pub struct EdgeDetector {
    trigger: EdgeTrigger,
    qualifier: Qualifier,
    previous: Option<Sample>,
    line: EdgeTracker,
    last_event: Option<f64>,
    events: u64,
    held_off: u64,
    unqualified: u64,
    confirmed: [Option<f64>; 2],
    excursion: Option<f64>,
}

impl EdgeDetector {
    /// Start with no samples or armed edges.
    pub fn new(trigger: EdgeTrigger) -> Self {
        Self::with_qualifier(trigger, Qualifier::None)
    }

    pub fn with_qualifier(trigger: EdgeTrigger, qualifier: Qualifier) -> Self {
        validate(trigger);
        assert!(validate_qualifier(qualifier), "invalid trigger qualifier bounds");
        Self {
            trigger, qualifier,
            previous: None,
            line: EdgeTracker::new(Threshold { level: trigger.level, hysteresis: trigger.hyst }),
            last_event: None,
            events: 0,
            held_off: 0,
            unqualified: 0,
            confirmed: [None; 2],
            excursion: None,
        }
    }

    /// Keep counters and holdoff; re-arm from the latest sample.
    pub fn set(&mut self, trigger: EdgeTrigger) {
        validate(trigger);
        assert!(validate_qualifier(self.qualifier), "invalid trigger qualifier bounds");
        if self.trigger.level != trigger.level || self.trigger.hyst != trigger.hyst { self.confirmed = [None; 2]; }
        self.excursion = None;
        self.trigger = trigger;
        self.line = EdgeTracker::new(Threshold { level: trigger.level, hysteresis: trigger.hyst });
        if let Some(sample) = self.previous {
            self.line.push(sample.t, sample.y as f64);
        }
    }

    /// Keep confirmed references, counters and holdoff; pending crossings are checked afresh.
    pub fn set_qualifier(&mut self, qualifier: Qualifier) {
        assert!(validate_qualifier(qualifier), "invalid trigger qualifier bounds");
        if self.qualifier != qualifier { self.excursion = None; }
        self.qualifier = qualifier;
    }

    /// Missing input breaks adjacency and arming, preserving event counts and holdoff.
    pub fn break_stream(&mut self) {
        self.previous = None;
        self.line.reset();
        self.confirmed = [None; 2];
        self.excursion = None;
    }

    /// Read one increasing time. Pending repeats until confirmed or dropped.
    /// A new crossing takes precedence over a dropped opposite crossing.
    pub fn push(&mut self, t: f64, y: f32) -> Edge {
        let valid = t.is_finite() && self.previous.is_none_or(|previous| t > previous.t);
        debug_assert!(valid, "sample times must be finite and strictly increasing");
        if !valid {
            return Edge::None;
        }
        if !y.is_finite() { self.break_stream(); return Edge::None; }
        let was_pending = self.pending().iter().any(Option::is_some);
        if let (Qualifier::Runt { other }, Some(extreme)) = (self.qualifier, &mut self.excursion) {
            *extreme = if other > self.trigger.level { extreme.max(y as f64) } else { extreme.min(y as f64) };
        }
        let accepted = self.line.push(t, y as f64);
        self.previous = Some(Sample { t, y });
        if let Some(edge) = accepted {
            let selected = self.selects(edge.dir);
            // The interval is read before this edge becomes the next reference.
            let qualified = selected && self.qualifies(edge.t, edge.dir);
            self.confirmed[usize::from(edge.dir == Slope::Falling)] = Some(edge.t);
            if let Qualifier::Runt { other } = self.qualifier {
                let outgoing = if other > self.trigger.level { Slope::Rising } else { Slope::Falling };
                self.excursion = (edge.dir == outgoing).then_some(y as f64);
            }
            if selected && !qualified { self.unqualified += 1; }
            if qualified {
                if self.last_event.is_some_and(|last| edge.t - last < self.trigger.holdoff) {
                    self.held_off += 1;
                    return Edge::Dropped;
                }
                self.last_event = Some(edge.t);
                self.events += 1;
                return Edge::Accepted(edge.t);
            }
        }
        if let Some(time) = self.pending().into_iter().flatten().next() {
            Edge::Pending(time)
        } else if was_pending {
            Edge::Dropped
        } else {
            Edge::None
        }
    }

    /// Accepted events, including windows that have not finished.
    pub fn events(&self) -> u64 {
        self.events
    }

    /// Confirmed edges rejected by holdoff.
    pub fn held_off(&self) -> u64 {
        self.held_off
    }

    /// Selected confirmed edges rejected by their interval or excursion.
    pub fn unqualified(&self) -> u64 { self.unqualified }

    fn selects(&self, dir: Slope) -> bool {
        match self.qualifier {
            Qualifier::Runt { other } => self.excursion.is_some() && dir == if other > self.trigger.level { Slope::Falling } else { Slope::Rising },
            _ => self.trigger.slope == Slope::Either || dir == self.trigger.slope,
        }
    }

    fn qualifies(&self, t: f64, dir: Slope) -> bool {
        let side = usize::from(dir == Slope::Falling);
        let (reference, min, max) = match self.qualifier {
            Qualifier::None => return true,
            Qualifier::Width { min, max } => (self.confirmed[1 - side], min, max),
            Qualifier::Period { min, max } => (self.confirmed[side], min, max),
            Qualifier::Runt { other } => return self.excursion.is_some_and(|extreme| if other > self.trigger.level { extreme < other } else { extreme > other }),
        };
        reference.is_some_and(|reference| { let interval = t - reference; interval >= min && interval < max })
    }

    /// The crossing each slope (rising, falling) holds while it waits for confirmation: an
    /// acquisition needs the cancellations a new opposite crossing hides.
    pub fn pending(&self) -> [Option<f64>; 2] {
        let mut pending = [None; 2];
        if let Some(edge) = self.line.pending().filter(|edge| self.selects(edge.dir) && self.qualifies(edge.t, edge.dir)) {
            pending[usize::from(edge.dir == Slope::Falling)] = Some(edge.t);
        }
        pending
    }
}

/// Find every accepted event using the incremental detector.
pub fn events(t: &[f64], y: &[f32], trigger: &EdgeTrigger) -> Vec<f64> {
    events_qualified(t, y, trigger, Qualifier::None)
}

pub fn events_qualified(t: &[f64], y: &[f32], trigger: &EdgeTrigger, qualifier: Qualifier) -> Vec<f64> {
    assert_eq!(t.len(), y.len(), "sample times and values must have equal lengths");
    let mut detector = EdgeDetector::with_qualifier(*trigger, qualifier);
    let mut events = Vec::new();
    for (&time, &value) in t.iter().zip(y) {
        if let Edge::Accepted(time) = detector.push(time, value) {
            events.push(time);
        }
    }
    events
}

fn validate(trigger: EdgeTrigger) {
    assert!(trigger.level.is_finite(), "trigger level must be finite");
    assert!(trigger.hyst.is_finite() && trigger.hyst >= 0.0, "hysteresis must be finite and nonnegative");
    assert!(trigger.holdoff.is_finite() && trigger.holdoff >= 0.0, "holdoff must be finite and nonnegative");
}
