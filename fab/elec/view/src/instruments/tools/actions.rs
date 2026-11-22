//! Scope commands shared by the palette and tool callers.

use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};

const CAPTURE_FIELDS: &[FieldDef] = &[
    FieldDef {
        name: "source",
        kind: FieldKind::Text,
        help: "Source id.",
        required: false,
        default: "\"sim\"",
    },
    FieldDef {
        name: "from",
        kind: FieldKind::Num { unit: "s" },
        help: "Window start relative to the trigger.",
        required: false,
        default: "0s",
    },
    FieldDef {
        name: "to",
        kind: FieldKind::Num { unit: "s" },
        help: "Window end relative to the trigger.",
        required: false,
        default: "10ms",
    },
    FieldDef {
        name: "rate",
        kind: FieldKind::Num { unit: "Hz" },
        help: "Requested rate; otherwise resolution determines it.",
        required: false,
        default: "",
    },
    FieldDef {
        name: "resolution",
        kind: FieldKind::Int { min: 1, max: u32::MAX as i64 },
        help: "Points across the window.",
        required: false,
        default: "2000",
    },
    FieldDef {
        name: "depth",
        kind: FieldKind::Int { min: 1, max: i64::MAX },
        help: "Requested maximum samples per channel.",
        required: false,
        default: "",
    },
    FieldDef {
        name: "trigger",
        kind: FieldKind::Choice(&["edge", "level", "none", "width", "period", "runt"]),
        help: "Trigger kind.",
        required: false,
        default: "@edge",
    },
    FieldDef {
        name: "runt_level",
        kind: FieldKind::Num { unit: "V" },
        help: "Second level in volts, required for a runt trigger.",
        required: false,
        default: "",
    },
    FieldDef {
        name: "min_interval",
        kind: FieldKind::Num { unit: "s" },
        help: "Qualifier bounds for a width or period trigger",
        required: false,
        default: "",
    },
    FieldDef {
        name: "max_interval",
        kind: FieldKind::Num { unit: "s" },
        help: "Qualifier bounds for a width or period trigger",
        required: false,
        default: "",
    },
    FieldDef {
        name: "on",
        kind: FieldKind::Text,
        help: "Trigger probe id or label; absent selects the first probe.",
        required: false,
        default: "",
    },
    FieldDef {
        name: "level",
        kind: FieldKind::Num { unit: "V" },
        help: "Trigger level; absent uses known mid-swing, otherwise zero.",
        required: false,
        default: "",
    },
    FieldDef {
        name: "slope",
        kind: FieldKind::Choice(&["rise", "fall", "either"]),
        help: "Trigger direction.",
        required: false,
        default: "@rise",
    },
    FieldDef {
        name: "holdoff",
        kind: FieldKind::Num { unit: "s" },
        help: "Minimum time between triggers.",
        required: false,
        default: "0s",
    },
    FieldDef {
        name: "timeout",
        kind: FieldKind::Num { unit: "s" },
        help: "Seconds to wait; the capture continues afterwards.",
        required: false,
        default: "10s",
    },
    FieldDef {
        name: "name",
        kind: FieldKind::Text,
        help: "Capture name.",
        required: false,
        default: "",
    },
];

/// The complete simulator and synthetic-source tool catalog.
pub static ACTIONS: &[ActionDef] = &[
    ActionDef {
        name: "elec.expect.list",
        label: "List bench expectations",
        group: "Scope",
        help: "Read live bench expectations and retained failures. Real expectations are not judged until a real source is open.",
        returns: r#"run: int
t: num s
running: bool
items: [{..}]
  name: text
  probe: text  as written in the bench
  unit: text  unit of the measured value
  measure: text
  when: {..}  exactly one of span or trailing
    span: {..}
      from: num s
      to: num s
    trailing: {..}
      window: num s
      after: num s
  min: num  range lower bound, in unit
  max: num  range upper bound, in unit
  equals: text  text equality bound, instead of min and max
  contains: text  text containment bound, instead of min and max
  site: site  the expect statement
  sim: {..}  present for on sim or both
    outcome: text  pass, fail, or not judged; any retained failure wins
    value: num  latest judgment, in unit; absent while not judged
    at: num s  latest judgment time; absent while not judged
    done: bool
    first_failure: {..}  absent until a failure
      t: num s
      value: num
    worst: num  largest bound violation
  real: {..}  present for on real or both
    outcome: text  not judged
    reason: text  no real source is open
total: int
next: text  cursor for the rest of this snapshot; absent at the end
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "names",
                kind: FieldKind::Text,
                help: "Comma-separated expectation names; absent selects all.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "on",
                kind: FieldKind::Choice(&["sim", "real", "both"]),
                help: "Sides to report.",
                required: false,
                default: "@sim",
            },
            FieldDef {
                name: "limit",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum items per page.",
                required: false,
                default: "100",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page, with the same names and on.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.expect.run",
        label: "Run bench expectations",
        group: "Scope",
        help: "Restart the simulator for each repeat and wait for the named expectations within one timeout. Real or both is refused until a real source is open. Rows that do not all fit the budget come compact, and the rest page: after reads them from the finished answer without running again.",
        returns: r#"state: text  done or waiting
compact: bool  present when the rows were trimmed to fit the budget: probe, measure, when, site and sim's done, first_failure and worst are left out (elec.expect.list has them)
run: int  last observed run; absent if the host has not replied
t: num s  last observed simulation time; absent if the host has not replied
running: bool  last observed transport state; absent if the host has not replied
items: [{..}]
  name: text
  probe: text  as written in the bench
  unit: text  unit of the measured value
  measure: text
  when: {..}  exactly one of span or trailing
    span: {..}
      from: num s
      to: num s
    trailing: {..}
      window: num s
      after: num s
  min: num  range lower bound, in unit
  max: num  range upper bound, in unit
  equals: text  text equality bound, instead of min and max
  contains: text  text containment bound, instead of min and max
  site: site  the expect statement
  sim: {..}
    outcome: text  pass, fail, or not judged; any retained failure wins
    value: num  latest judgment, in unit; absent while not judged
    at: num s  latest judgment time; absent while not judged
    done: bool
    first_failure: {..}  first failure in the displayed run, if any
      t: num s
      value: num
    worst: num
  final: bool  judged-final in the requested run, or that run stopped
  repeats: {..}  counts over finished repeats only
    pass: int
    fail: int
    not_judged: int
  first_failure: {..}  first failure across repeats, if any
    repeat: int  one-based repeat index
    t: num s
    value: num
total: int
next: text  cursor for the rest of this answer; absent at the end
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "names",
                kind: FieldKind::Text,
                help: "Comma-separated expectation names; absent selects all.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "on",
                kind: FieldKind::Choice(&["sim", "real", "both"]),
                help: "Only sim is available; real and both require a real source.",
                required: false,
                default: "@sim",
            },
            FieldDef {
                name: "repeats",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Number of simulator restarts.",
                required: false,
                default: "1",
            },
            FieldDef {
                name: "timeout",
                kind: FieldKind::Num { unit: "s" },
                help: "Whole-call wait bound, including host replies and every repeat.",
                required: false,
                default: "10s",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Cursor from a preceding answer: read its next page; nothing runs again.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Run,
        keys: "",
    },
    ActionDef {
        name: "elec.source.list",
        label: "List scope sources",
        group: "Scope",
        help: "List the simulator and synthetic source with their capabilities.",
        returns: r#"sources: [{..}]  every source, the simulator first
  id: id source
  side: choice sim|real|replay
  name: text
  state: choice closed|open|busy|error
  access: text  `ok`, or why it cannot be opened (permissions, in use, unplugged)
  channels: [{..}]
    id: int
    kind: choice volt|amp|logic
    range: [num V]  the input ranges it offers, smallest first
    bits: int
  rates: [num Hz]  the sample rates it offers; the simulator gives its max step as 1/step
  depth: int  samples per channel per capture; left out for the simulator
  trigger: [text]  the trigger kinds (edge, level, pulse, pattern, serial), each with hw or sw
  outputs: [text]  generator and digital outputs, empty when none
  cannot: [text]  what this source cannot do that the tools could ask for"#,
        fields: &[
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.probe.add",
        label: "Add scope probe",
        group: "Scope",
        help: "Add a bench probe as one undoable edit. Unsupported current and power probes explain why they cannot be added.",
        returns: r#"probe: id probe
lane: int  the scope lane it draws in
unit: text
net: text  the resolved net, or the branch for an i on a source or inductor
site: site  the statement written"#,
        fields: &[
            FieldDef {
                name: "target",
                kind: FieldKind::Text,
                help: "Net, pin or branch to probe.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "quantity",
                kind: FieldKind::Choice(&["v", "i", "p"]),
                help: "Physical quantity.",
                required: false,
                default: "@v",
            },
            FieldDef {
                name: "label",
                kind: FieldKind::Text,
                help: "Channel label; absent uses the target.",
                required: false,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.probe.remove",
        label: "Remove scope probe",
        group: "Scope",
        help: "Remove a bench probe as one undoable edit.",
        returns: r#""#,
        fields: &[
            FieldDef {
                name: "probe",
                kind: FieldKind::Text,
                help: "Probe id or label.",
                required: true,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.plan",
        label: "Plan capture",
        group: "Scope",
        help: "Resolve capture settings against the source capabilities without starting acquisition.",
        returns: r#"source: id source
rate: num Hz  the real rate (the simulator: 1 / max step)
depth: int
span: [num s]  from and to, relative to the trigger
ranges: [num V]  per channel, the real input range
step: [num V]  per channel, the level step (bits over range); left out for the simulator
trigger: {..}
  kind: choice edge|level|pulse|none|width
  on: text
  level: num V
  slope: choice rise|fall|either
  how: choice hw|sw
  blind: num %  the share of time the source cannot see (re-arm, readout); 0 for the simulator
notes: [text]  every place the plan differs from the ask, and why"#,
        fields: CAPTURE_FIELDS,
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.run",
        label: "Run capture",
        group: "Scope",
        help: "Capture simulator or synthetic samples on workers. A wait deadline leaves acquisition running.",
        returns: r#"capture: id capture
state: choice done|waiting|timeout|failed
t_trigger: num s  the trigger instant in the source's time
samples: int  per channel
firmware: int  firmware event count, when a record is present
span: [num s]  what was recorded
gaps: [{..}]  stretches with no samples
  from: num s
  to: num s
  why: choice overrun|rearm|blind|restart
resolved: {..}  as capture.plan: source, rate, depth, span, ranges, step, trigger, notes"#,
        fields: CAPTURE_FIELDS,
        inputs: &[],
        risk: Risk::Run,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.wait",
        label: "Wait for capture",
        group: "Scope",
        help: "Wait for a running capture up to the given deadline.",
        returns: r#"capture: id capture
state: choice done|waiting|timeout|failed
t_trigger: num s  the trigger instant in the source's time
samples: int  per channel
firmware: int  firmware event count, when a record is present
span: [num s]  what was recorded
gaps: [{..}]  stretches with no samples
  from: num s
  to: num s
  why: choice overrun|rearm|blind|restart
resolved: {..}  as capture.plan: source, rate, depth, span, ranges, step, trigger, notes"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "timeout",
                kind: FieldKind::Num { unit: "s" },
                help: "Seconds to wait; the capture continues afterwards.",
                required: false,
                default: "10s",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.list",
        label: "List captures",
        group: "Scope",
        help: "List immutable captures in creation order.",
        returns: r#"captures: [{..}]  oldest first; total, and next when more remain
  id: id capture
  name: text
  side: choice sim|real|replay
  source: id source
  span: [num s]
  channels: int
  samples: int  per channel
  firmware: int  firmware event count, when a record is present
  saved: text  the project-relative path, left out until saved
total: int
next: text  the cursor for the rest
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "side",
                kind: FieldKind::Choice(&["sim", "real", "replay"]),
                help: "Filter by origin.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.drop",
        label: "Drop capture",
        group: "Scope",
        help: "Release a capture and its record.",
        returns: r#""#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Run,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.save",
        label: "Save trace",
        group: "Scope",
        help: "Write a capture, or without one the Scope's take (its lanes over the run, as the recording bar's Save trace does), into the project's captures folder as a .trace file.",
        returns: r#"path: text  project-relative"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id; absent: the Scope's take.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "name",
                kind: FieldKind::Text,
                help: "Project capture filename; absent uses the capture name, or take-N for the take.",
                required: false,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.open",
        label: "Open trace",
        group: "Scope",
        help: "Open a saved .trace file (a path elec.capture.save answered, e.g. captures/take-1.trace) as a capture of its own, to read with elec.measure, elec.capture.summary and the rest, or to compare against the run with elec.capture.compare; the Scope tab shows it in place of the live run, as a saved trace in the Scope menu does. The run goes on, and elec.capture.close shows it again. Opening the same file again replaces its capture.",
        returns: r#"capture: id capture  the trace's capture, side replay
path: text
channels: [text]  each channel's net, as a capture of the run names it
from: num s  the first sample's time
to: num s  the last sample's time
samples: int"#,
        fields: &[FieldDef {
            name: "path",
            kind: FieldKind::Text,
            help: "The trace's project-relative path, captures/<name>.trace.",
            required: true,
            default: "",
        }],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.close",
        label: "Back to live",
        group: "Scope",
        help: "Show the live run in the Scope tab again after elec.capture.open, as the Scope's Back to live does.",
        returns: r#"closed: text  the trace that was shown, or none"#,
        fields: &[],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.summary",
        label: "Summarize capture",
        group: "Scope",
        help: "Read signal statistics and activity from the capture index.",
        returns: r#"side: choice sim|real|replay
channels: [{..}]
  channel: int
  name: text  the channel's net
  label: text  its Scope lane's name; absent when it has none
  class: choice dc|square|pwm|sine|bursts|idle|unknown
  min: num V
  max: num V
  mean: num V
  rms: num V
  top: num V
  base: num V
  edges: int
  frequency: {..}
    value: num Hz
    sigma: num Hz
  duty: {..}
    value: num %
    sigma: num %
  first_edge: num s
  last_edge: num s
  segments: [{..}]  run-length activity; total, and next when more remain
    from: num s
    to: num s
    what: text  idle low, pwm 10kHz 51%, burst 9600 baud
  clipped: [{..}]
    from: num s
    to: num s
  step: {..}  the sample step and level step
    time: num s
    level: num V
gaps: [{..}]
  from: num s
  to: num s
  why: choice overrun|rearm|blind|restart
total: int  segments over the whole query
next: text  the cursor for the rest
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channels",
                kind: FieldKind::Text,
                help: "Channel ids separated by spaces or commas; absent selects all.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.samples",
        label: "Read capture samples",
        group: "Scope",
        help: "Read a bounded page of raw or reduced samples, or firmware events.",
        returns: r#"t0: num s
dt: num s  the step between points; raw on non-uniform steps gives t instead
t: [num s]  relative to the trigger; for firmware or raw on non-uniform steps
reduce: choice minmax|mean|raw
y: [num V]  for mean and raw
min: [num V]  for minmax
max: [num V]  for minmax
real_step: num s  the record's own step over the span (mean, and the largest)
source: [choice image|trace]  for firmware
event: [text]  for firmware
site: [int]  for firmware; -1 when the event has no site
lost: int  for firmware; events rolled off before the record
total: int  samples or firmware events over the whole query
next: text  cursor for the rest, absent on the last page
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channel",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "Channel id.",
                required: false,
                default: "0",
            },
            FieldDef {
                name: "stream",
                kind: FieldKind::Choice(&["analog", "firmware"]),
                help: "analog samples of one channel, or the firmware events of both sources",
                required: false,
                default: "@analog",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "points",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum points across the span, or firmware events per page.",
                required: false,
                default: "200",
            },
            FieldDef {
                name: "reduce",
                kind: FieldKind::Choice(&["minmax", "mean", "raw"]),
                help: "Sample reduction.",
                required: false,
                default: "@minmax",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.measure",
        label: "Measure capture",
        group: "Scope",
        help: "Read the shared bench measurements from an indexed capture.",
        returns: r#"items: [{..}]  total, and next when more remain
  what: text
  value: num  in the measure's unit, written with its suffix
  n: int  cycles or events it is over
  sigma: num  in the measure's unit
  min: {..}
    value: num  in the measure's unit
    t: num s
  max: {..}
    value: num  in the measure's unit
    t: num s
total: int
next: text  the cursor for the rest
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channel",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "Channel id.",
                required: false,
                default: "0",
            },
            FieldDef {
                name: "what",
                kind: FieldKind::Choice(&["now", "mean", "rms", "min", "max", "pp", "frequency", "duty", "width", "rise", "fall", "edges", "delay", "uart"]),
                help: "Bench measurement.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "per",
                kind: FieldKind::Choice(&["record", "cycle", "event"]),
                help: "Aggregation interval.",
                required: false,
                default: "@record",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.edges",
        label: "Read capture edges",
        group: "Scope",
        help: "Read confirmed edges and complete 10-90% transition times, relative to the trigger.",
        returns: r#"items: [{..}]
  t: num s  relative to the trigger
  dir: choice rising|falling
  rise: num s  complete transition duration; absent when incomplete
threshold: num  in the channel's unit, written with its suffix
hysteresis: num  in the channel's unit, written with its suffix
total: int
next: text  the cursor for the rest
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channel",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "Channel id.",
                required: false,
                default: "0",
            },
            FieldDef {
                name: "threshold",
                kind: FieldKind::Text,
                help: "Quantity in the channel's unit; empty uses mid-swing. An explicit threshold scans the span; auto reads the cached edges.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "hysteresis",
                kind: FieldKind::Text,
                help: "Nonnegative quantity in the channel's unit; empty uses 10% of the whole-record swing. A non-auto hysteresis scans the span.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "kind",
                kind: FieldKind::Choice(&["both", "rising", "falling"]),
                help: "Edge direction.",
                required: false,
                default: "@both",
            },
            FieldDef {
                name: "limit",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum items per page.",
                required: false,
                default: "100",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.pulses",
        label: "Read capture pulses",
        group: "Scope",
        help: "Read complete pulses within the span, with filtered medians and optional deviation-first order.",
        returns: r#"items: [{..}]
  t: num s  pulse start relative to the trigger
  width: num s
  level: choice high|low
  period: num s  start to the next complete pulse of the same level, before filtering; absent for the last or across a gap
median: {..}  widths over the filtered set
  high: num s  absent when no high pulses remain
  low: num s  absent when no low pulses remain
total: int
next: text  the cursor for the rest
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channel",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "Channel id.",
                required: false,
                default: "0",
            },
            FieldDef {
                name: "threshold",
                kind: FieldKind::Text,
                help: "Quantity in the channel's unit; empty uses mid-swing. An explicit threshold scans the span; auto reads the cached edges.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "hysteresis",
                kind: FieldKind::Text,
                help: "Nonnegative quantity in the channel's unit; empty uses 10% of the whole-record swing. A non-auto hysteresis scans the span.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "kind",
                kind: FieldKind::Choice(&["both", "rising", "falling"]),
                help: "Pulse start direction; rising selects high pulses and falling selects low pulses.",
                required: false,
                default: "@both",
            },
            FieldDef {
                name: "level",
                kind: FieldKind::Choice(&["both", "high", "low"]),
                help: "Pulse level.",
                required: false,
                default: "@both",
            },
            FieldDef {
                name: "where",
                kind: FieldKind::Text,
                help: "Empty selects all; use width < 80us or width > 1ms and period < 5ms. Operators: < <= > >=. Period is measured before filtering.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "order",
                kind: FieldKind::Choice(&["time", "deviation"]),
                help: "Time order or descending absolute deviation from the filtered median of the same level, with ties by time.",
                required: false,
                default: "@time",
            },
            FieldDef {
                name: "limit",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum items per page.",
                required: false,
                default: "100",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.decode",
        label: "Decode capture",
        group: "Scope",
        help: "Decode UART frames in one continuous voltage or logic span. Auto baud measures the line rate; decoded frames are cached for paging.",
        returns: r#"frames: [{..}]
  t0: num s  start relative to the trigger
  t1: num s  stop centre relative to the trigger
  byte: int  received data, including the ninth bit when selected
  error: choice framing|parity|break  absent for a good frame
text: text  page bytes as printable ASCII or escaped \xNN; nine-bit data uses the low eight bits
baud: {..}
  measured: num  observed rate; absent if an explicit baud was given with too few edges to measure
  nominal: num  nearest standard rate within three percent, absent when none
  error: num %  signed measured rate error against nominal, absent when none
  used: num  explicit baud, otherwise the measured rate
errors: {..}
  framing: int  over the whole decoded span
  parity: int  over the whole decoded span
  break: int  over the whole decoded span
total: int  frames over the whole decoded span
next: text  cursor for the rest, absent on the last page
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "capture",
                kind: FieldKind::Text,
                help: "Capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channel",
                kind: FieldKind::Int { min: 0, max: u32::MAX as i64 },
                help: "UART receive channel id.",
                required: false,
                default: "0",
            },
            FieldDef {
                name: "protocol",
                kind: FieldKind::Choice(&["uart"]),
                help: "Serial protocol.",
                required: false,
                default: "@uart",
            },
            FieldDef {
                name: "baud",
                kind: FieldKind::Text,
                help: "auto, or a positive baud rate in bits per second.",
                required: false,
                default: "\"auto\"",
            },
            FieldDef {
                name: "bits",
                kind: FieldKind::Int { min: 5, max: 9 },
                help: "Data bits per frame.",
                required: false,
                default: "8",
            },
            FieldDef {
                name: "parity",
                kind: FieldKind::Choice(&["none", "even", "odd"]),
                help: "Parity bit.",
                required: false,
                default: "@none",
            },
            FieldDef {
                name: "stop",
                kind: FieldKind::Choice(&["1", "1.5", "2"]),
                help: "Stop interval in bit times.",
                required: false,
                default: "@1",
            },
            FieldDef {
                name: "invert",
                kind: FieldKind::Choice(&["no", "yes"]),
                help: "Invert the receive line around its threshold.",
                required: false,
                default: "@no",
            },
            FieldDef {
                name: "threshold",
                kind: FieldKind::Text,
                help: "Threshold quantity; empty uses the index's mid-swing level. Hysteresis is ten percent of the capture swing.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the trigger, as [from to]; absent means the whole record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "limit",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum frames per page.",
                required: false,
                default: "64",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.capture.compare",
        label: "Compare captures",
        group: "Scope",
        help: "Compare auto-threshold edges in expected trigger-relative time with one shared clock map. Results are cached for paging; gaps prevent a verdict.",
        returns: r#"verdict: choice pass|fail|not_judged
reason: text  only when not_judged; identifies the capture with a gap
alignment: {..}
  method: choice trigger|trigger_fit|event|event_fit|fit  where the clock map came from; _fit: then fitted to the matched edges
  offset: num s
  skew: num %
  residual_rms: num s
  matched: int
  runner_up: int  only for fit; most matches of a candidate that settled on another clock
tolerance_used: {..}
  time: num s
channels: [{..}]
  expected: int  expected channel id
  actual: int  actual channel id
  matched: int
  missing: int
  extra: int
  late: int
  early: int
  first_difference: {..}  absent when the channel has no differences
    t: num s  relative to the expected trigger
    kind: choice missing_edge|extra_edge|late|early
    dir: choice rising|falling
    dt: num s  signed residual; only for late or early
differences: [{..}]  all channels merged in time order
  t: num s  relative to the expected trigger
  kind: choice missing_edge|extra_edge|late|early
  dir: choice rising|falling
  channel: int  expected channel id
  dt: num s  signed residual; only for late or early
total: int  differences over the whole query
next: text  cursor for the rest, absent on the last page
more: text  how many items are left and how to ask for them; absent at the end"#,
        fields: &[
            FieldDef {
                name: "expected",
                kind: FieldKind::Text,
                help: "Expected capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "actual",
                kind: FieldKind::Text,
                help: "Actual capture id.",
                required: true,
                default: "",
            },
            FieldDef {
                name: "channels",
                kind: FieldKind::Text,
                help: "Expected:actual channel ids, such as 0:0 or 0:1,1:0; empty pairs the channels of the same net (a saved take and a capture of the run name a net alike), or by position when no net is in both.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "align",
                kind: FieldKind::Choice(&["trigger", "event", "fit"]),
                help: "Start at the triggers, the first anchor edges, or search up to sixteen same-direction offsets.",
                required: false,
                default: "@trigger",
            },
            FieldDef {
                name: "anchor",
                kind: FieldKind::Choice(&["rising", "falling"]),
                help: "First edge direction on the first channel pair for event alignment.",
                required: false,
                default: "@rising",
            },
            FieldDef {
                name: "skew",
                kind: FieldKind::Choice(&["fit", "none"]),
                help: "Fit offset and clock skew after matching. none keeps trigger/event anchors fixed, or fits only offset with align: fit.",
                required: false,
                default: "@fit",
            },
            FieldDef {
                name: "tolerance",
                kind: FieldKind::Text,
                help: "Nonnegative time quantity; empty uses twice the largest sample step over both channels of every pair.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "span",
                kind: FieldKind::Text,
                help: "Two times relative to the expected trigger, as [from to]; absent means the whole expected record.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "limit",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Maximum differences per page.",
                required: false,
                default: "50",
            },
            FieldDef {
                name: "after",
                kind: FieldKind::Text,
                help: "Opaque cursor returned by the preceding page.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "budget",
                kind: FieldKind::Int { min: 1, max: i64::MAX },
                help: "Reply bytes, also capped by the host: a list answers the items that fit (at least one) and pages the rest.",
                required: false,
                default: "4096",
            },
        ],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
];

/// Register these commands once when the host installs its scope tools.
pub fn register_actions() {
    fab_elec_design::action::register(ACTIONS);
}
