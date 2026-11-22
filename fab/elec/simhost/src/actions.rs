//! The simulation's actions (the live bench and the counted bench run) shared by the command
//! palette, the UI and the AI tools.

use fab_elec_design::action::{
    ActionDef, FieldDef, FieldKind, InputDef, InputKind, Risk, SelKind,
};

pub const PLAY: ActionDef = ActionDef {
    name: "elec.sim.play",
    label: "Play simulation",
    group: "Simulate",
    help: "Run the live bench of the committed design at the given rate (simulated seconds per real second).",
    returns: "time: num s  the simulated time it starts from",
    fields: &[FieldDef {
        name: "rate",
        kind: FieldKind::Num { unit: "" },
        help: "Simulated seconds per real second; `inf` runs as fast as the solver can.",
        required: false,
        default: "1",
    }],
    inputs: &[],
    risk: Risk::Run,
    keys: "Space",
};

pub const PAUSE: ActionDef = ActionDef {
    name: "elec.sim.pause",
    label: "Pause simulation",
    group: "Simulate",
    help: "Hold the live bench at its current simulated time; the values shown stay.",
    returns: "time: num s  the simulated time it holds at",
    fields: &[],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub const STEP: ActionDef = ActionDef {
    name: "elec.sim.step",
    label: "Step simulation",
    group: "Simulate",
    help: "Advance the live bench by the given simulated time, then pause.",
    returns: "time: num s  the simulated time reached",
    fields: &[FieldDef {
        name: "dt",
        kind: FieldKind::Num { unit: "s" },
        help: "Simulated time to advance.",
        required: true,
        default: "1ms",
    }],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub const STATUS: ActionDef = ActionDef {
    name: "elec.sim.status",
    label: "Simulation status",
    group: "Simulate",
    help: "What the transport shows: the simulated time, whether the bench plays, steps firmware or has settled, the asked and reached rates and what limits the rate.",
    returns: "t: num s  the newest values' simulated time\nplaying: bool  play was asked last\nstepping: bool  a firmware step runs\nrate_asked: num  simulated seconds per real second asked (inf: as fast as it goes)\nrate_achieved: num  simulated seconds per real second reached over the last half second\nlimited_by: text  paced, solver, scope (the scopes' acquisition), emulator or events (the device's event feed is full) while running; empty at rest\nsteady: bool  the bench has settled\nreflects: int  the newest committed step the run was made from (0 before any)\nrerender: num s  where the take renders again from after an edit it heard; empty while it does not",
    fields: &[FieldDef {
        name: "wait",
        kind: FieldKind::Int { min: 0, max: i64::MAX },
        help: "An edit's seq: the answer waits until the run reflects that step and has re-rendered past it (a paused run answers once it has the step).",
        required: false,
        default: "",
    }],
    inputs: &[],
    risk: Risk::Read,
    keys: "",
};

pub const RATE: ActionDef = ActionDef {
    name: "elec.sim.rate",
    label: "Simulation rate",
    group: "Simulate",
    help: "Set the live bench's rate (simulated seconds per real second) without playing or pausing it; any positive rate, or `inf` for as fast as the solver goes.",
    returns: "rate_asked: num  the rate now asked\nrate_achieved: num  the rate reached over the last half second",
    fields: &[FieldDef {
        name: "rate",
        kind: FieldKind::Num { unit: "" },
        help: "Simulated seconds per real second; `inf` runs as fast as the solver can.",
        required: true,
        default: "1",
    }],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub const LEDS: ActionDef = ActionDef {
    name: "elec.sim.leds",
    label: "LED states",
    group: "Simulate",
    help: "Each LED of the live bench: its current now and over the last publication, the light output drawn now and its colour.",
    returns: "leds: [{..}]  one per LED\n  part: text  the LED's component path\n  i: num A  its current at the simulated time\n  i_mean: num A  its mean current over the eye\n  brightness: num  its light as drawn, 0 to 1\n  rgb: [num]  its colour as drawn; empty when the part has none",
    fields: &[],
    inputs: &[],
    risk: Risk::Read,
    keys: "",
};

pub const MIC: ActionDef = ActionDef {
    name: "elec.sim.mic",
    label: "Play microphone WAV",
    group: "Simulate",
    help: "Play a WAV into the running firmware's microphone at the Firmware pane's level.",
    returns: "playing: bool  the board accepted the converted WAV",
    fields: &[FieldDef { name: "file", kind: FieldKind::Text, help: "WAV path relative to the saved project's folder.",
        required: true, default: "" }],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub const NET: ActionDef = ActionDef {
    name: "elec.sim.net",
    label: "Read board network",
    group: "Simulate",
    help: "Read the running board's network status and arrivals from its real network input log, in board time. Pass next as from for the following page.",
    returns: "arrivals: [{..}]  ms, c, gen and kind; messages show text or length and hex prefix, taken shows bytes, ended shows code and detail\nnext: int  first unread arrival\nconnections: [{..}]  one per connection\n  c: int  the connection\n  state: choice ready|connecting|offline\n  code: int  how it last ended, 0 if it has not\n  detail: int  the end's detail\nup: int  host messages up\ndown: int  host messages down\nbytes_up: int  host data bytes up\nbytes_down: int  host data bytes down\ncuts: int  times replay changed the log; read from 0 if it changes",
    fields: &[FieldDef { name: "from", kind: FieldKind::Int { min: 0, max: i64::MAX }, help: "Zero-based index of the first arrival to read.",
        required: false, default: "0" }],
    inputs: &[],
    risk: Risk::Read,
    keys: "",
};

pub const STEP_FW: ActionDef = ActionDef {
    name: "elec.sim.step_fw",
    label: "Step firmware",
    group: "Simulate",
    help: "Run the bench's firmware to its next statement, handler entry or the running handler's exit, then pause; the circuit runs with it.",
    returns: "time: num s  the simulated time it stopped at",
    fields: &[FieldDef {
        name: "to",
        kind: FieldKind::Choice(&["statement", "handler_entry", "handler_exit"]),
        help: "Where to stop.",
        required: false,
        default: "statement",
    }],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub const SET: ActionDef = ActionDef {
    name: "elec.sim.set",
    label: "Set value live",
    group: "Simulate",
    help: "Change a component's value in the running simulation only (the design is not edited; commit the value as an edit to keep it).",
    returns: "effect: bool  whether the change has a simulation effect",
    fields: &[
        FieldDef {
            name: "comp",
            kind: FieldKind::Sel(SelKind::Comp),
            help: "The component.",
            required: true,
            default: "",
        },
        FieldDef {
            name: "value",
            kind: FieldKind::Num { unit: "" },
            help: "The new value in the component's unit (ohm, F, H, V).",
            required: true,
            default: "",
        },
    ],
    inputs: &[InputDef {
        name: "comp",
        kind: InputKind::Selection {
            kind: SelKind::Comp,
            many: false,
        },
        help: "The selected component.",
    }],
    risk: Risk::Preview,
    keys: "",
};

pub const PROBE: ActionDef = ActionDef {
    name: "elec.sim.probe",
    label: "Probe",
    group: "Simulate",
    help: "Read a live value of the running bench: a net's voltage, a pin's current or a component's dissipation at the newest simulated time (now), or a net's mean, min or max over the newest published interval (since to t, about a frame of real time). Live values are indications; elec.sim.step first to read at a chosen time.",
    returns: "value: num  the live value\nunit: text  V, A or W\nt: num s  the simulated time it was read at\nsince: num s  where a mean, min or max interval starts",
    fields: &[
        FieldDef {
            name: "target",
            kind: FieldKind::Sel(SelKind::Any),
            help: "A net (GND), a pin (R1.2) or a component (R1, mcu.boot_r).",
            required: true,
            default: "",
        },
        FieldDef {
            name: "measure",
            kind: FieldKind::Choice(&["now", "mean", "min", "max"]),
            help: "now for any target; mean, min and max over the newest interval for a net.",
            required: false,
            default: "now",
        },
    ],
    inputs: &[InputDef {
        name: "target",
        kind: InputKind::Selection {
            kind: SelKind::Any,
            many: false,
        },
        help: "The selected net, pin or component.",
    }],
    risk: Risk::Read,
    keys: "",
};

pub const BENCH_RUN: ActionDef = ActionDef {
    name: "elec.bench.run",
    label: "Check bench",
    group: "Simulate",
    help: "Run the running bench once more on its own, from power-on over the given time, judge every point against its expectations and return each one's outcome (pass, fail, not judged) with its value, first failure and worst excursion: the counted check, as the Bench editor's Check bench (the live verdicts stay the take's).",
    returns: "",
    fields: &[
        FieldDef {
            name: "board",
            kind: FieldKind::Sel(SelKind::Unit),
            help: "The board unit; defaults to the active board.",
            required: false,
            default: "",
        },
        FieldDef {
            name: "tstop",
            kind: FieldKind::Num { unit: "s" },
            help: "Simulated time to run; defaults to the bench's stop, else 20 ms.",
            required: false,
            default: "",
        },
    ],
    inputs: &[],
    risk: Risk::Run,
    keys: "",
};

pub static ACTIONS: &[ActionDef] = &[PLAY, PAUSE, STEP, STATUS, RATE, LEDS, MIC, NET, STEP_FW, SET, PROBE, BENCH_RUN];

pub fn register_actions() {
    fab_elec_design::action::register(ACTIONS);
}
