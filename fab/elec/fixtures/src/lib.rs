//! Document-backed electronics fixtures and the stock board catalogue.
//! The controller and pair have circuit, board, bench and system documents; the pair
//! also retains its Rust probe builder for comparison.
//!
//! Cost: one evaluation of the ledctl documents per process on first use, then a clone per
//! call. A `Design` clone shares each unit's circuit and board through Arcs; `ledctl_eval`
//! also clones the source map, bench specifications and evaluation metadata.

mod grid;
mod neck;
mod pair;
pub mod stock;

use fab_elec_core::{Diag, Sel};
use fab_elec_design::Design;
use fab_elec_lang::{evaluate, std_libs, Evaluated, Libs, SourceRange, Sources, UnitState};
use std::sync::OnceLock;

pub use grid::{ledctl_grid, ledctl_panel};
pub use neck::pour_neck;
pub use pair::ledctl_pair;

/// ledctl's requirements document, kept out of `docs("ledctl")`: the tests that read
/// requirements add it.
pub const LEDCTL_REQ: &str = include_str!("../ledctl.req.splash");

/// The committed document sets at each stage of the brief-to-board controller.
pub mod brief_ledctl {
    pub const BRIEF: &str = include_str!("../brief_ledctl/stage0_brief.req.splash");
    pub const REQUIREMENTS: &[(&str, &str)] = &[("brief_ledctl.req.splash", include_str!("../brief_ledctl/stage1_req.req.splash"))];
    pub const ARCHITECTURE: &[(&str, &str)] = &[
        ("brief_ledctl.blocks.splash", include_str!("../brief_ledctl/stage2_arch.blocks.splash")),
        ("brief_ledctl.ckt.splash", include_str!("../brief_ledctl/stage2_arch.ckt.splash")),
    ];
    pub const PARTS: &[(&str, &str)] = &[("brief_ledctl.part.splash", include_str!("../brief_ledctl/stage3_parts.part.splash"))];
    pub const PRICES: &str = include_str!("../brief_ledctl/stage3_prices.json");
    pub const SCHEMATIC: &[(&str, &str)] = &[
        ("brief_ledctl.blocks.splash", include_str!("../brief_ledctl/stage4_schematic.blocks.splash")),
        ("brief_ledctl.ckt.splash", include_str!("../brief_ledctl/stage4_schematic.ckt.splash")),
    ];
}

/// ledctl's documents.
const LEDCTL: [(&str, &str); 3] = [
    ("ledctl.ckt.splash", include_str!("../ledctl.ckt.splash")),
    ("ledctl.pcb.splash", include_str!("../ledctl.pcb.splash")),
    ("ledctl.bench.splash", include_str!("../ledctl.bench.splash")),
];

/// ledavr's documents: ledctl's LED board on an ATmega328P, its MCU part, its parts' 3D models
/// (named, not shipped: each waits for the project to hold it), its firmware and the bench that
/// runs it.
const LEDAVR: [(&str, &str); 5] = [
    ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../parts/avr/ATMEGA328P_TQFP32.part.splash")),
    ("ledavr.models.part.splash", include_str!("../ledavr.models.part.splash")),
    ("ledavr.ckt.splash", include_str!("../ledavr.ckt.splash")),
    ("ledavr.fw.splash", include_str!("../ledavr.fw.splash")),
    ("ledavr.bench.splash", include_str!("../ledavr.bench.splash")),
];

/// handheld's documents: an MCU, panel, touch, IMU, expander, codec and speaker amplifier behind
/// USB-C and a 3.3 V regulator, their footprints and named (not shipped) 3D models, the placed and
/// routed board, the app firmware (Sand and Voice Toy pages) and its benches. The case follows later.
const HANDHELD: [(&str, &str); 8] = [
    ("parts/s3/ESP32S3_QFN56.part.splash", include_str!("../parts/s3/ESP32S3_QFN56.part.splash")),
    ("parts/audio/ES8311_QFN20.part.splash", include_str!("../parts/audio/ES8311_QFN20.part.splash")),
    ("parts/audio/NS4150B_MSOP8.part.splash", include_str!("../parts/audio/NS4150B_MSOP8.part.splash")),
    ("handheld.models.part.splash", include_str!("../handheld.models.part.splash")),
    ("handheld.ckt.splash", include_str!("../handheld.ckt.splash")),
    ("handheld.pcb.splash", include_str!("../handheld.pcb.splash")),
    ("handheld.fw.splash", include_str!("../handheld.fw.splash")),
    ("handheld.bench.splash", include_str!("../handheld.bench.splash")),
];

/// A button plays a tone through an S3, an I2S codec and a speaker amplifier.
const BEEP: [(&str, &str); 6] = [
    ("parts/s3/ESP32S3_QFN56.part.splash", include_str!("../parts/s3/ESP32S3_QFN56.part.splash")),
    ("parts/audio/ES8311_QFN20.part.splash", include_str!("../parts/audio/ES8311_QFN20.part.splash")),
    ("parts/audio/NS4150B_MSOP8.part.splash", include_str!("../parts/audio/NS4150B_MSOP8.part.splash")),
    ("beep.ckt.splash", include_str!("../beep.ckt.splash")),
    ("beep.fw.splash", include_str!("../beep.fw.splash")),
    ("beep.bench.splash", include_str!("../beep.bench.splash")),
];

/// tonebox's documents: one key, one knob, one LED and one audio jack on USB 5 V; holding the key
/// lights the LED and plays an RC oscillator's tone on the jack, and the PITCH knob tunes it. Its
/// parts' 3D models (named, not shipped), its circuit, its placed board and its bench.
const TONEBOX: [(&str, &str); 4] = [
    ("tonebox.models.part.splash", include_str!("../tonebox.models.part.splash")),
    ("tonebox.ckt.splash", include_str!("../tonebox.ckt.splash")),
    ("tonebox.pcb.splash", include_str!("../tonebox.pcb.splash")),
    ("tonebox.bench.splash", include_str!("../tonebox.bench.splash")),
];

/// organ8's documents: an eight-voice organ on USB 5 V, eight copies of one block (a free-running
/// Schmitt RC oscillator its key lets through) summed to a line out; its blocks, its circuit and its
/// bench. The live simulator's rounds are measured on it beside drum16.
const ORGAN8: [(&str, &str); 3] = [
    ("organ8.blocks.splash", include_str!("../organ8.blocks.splash")),
    ("organ8.ckt.splash", include_str!("../organ8.ckt.splash")),
    ("organ8.bench.splash", include_str!("../organ8.bench.splash")),
];

/// drum16's documents: an analog drum machine on one 5 V supply. Its MCU part, its parts' 3D models
/// (named, not shipped: each waits for the project or the part library to hold it), its blocks, the
/// whole board's circuit and placement, each voice as a unit of its own, the sequencer firmware, the
/// board's bench and the benches that strike each voice.
const DRUM16: [(&str, &str); 12] = [
    ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../parts/avr/ATMEGA328P_TQFP32.part.splash")),
    ("drum16.models.part.splash", include_str!("../drum16.models.part.splash")),
    ("drum16.blocks.splash", include_str!("../drum16.blocks.splash")),
    ("drum16.ckt.splash", include_str!("../drum16.ckt.splash")),
    ("drum16.pcb.splash", include_str!("../drum16.pcb.splash")),
    ("kick.ckt.splash", include_str!("../kick.ckt.splash")),
    ("toms.ckt.splash", include_str!("../toms.ckt.splash")),
    ("snare.ckt.splash", include_str!("../snare.ckt.splash")),
    ("hats.ckt.splash", include_str!("../hats.ckt.splash")),
    ("drum16.fw.splash", include_str!("../drum16.fw.splash")),
    ("drum16.bench.splash", include_str!("../drum16.bench.splash")),
    ("voices.bench.splash", include_str!("../voices.bench.splash")),
];

/// drum16's board as it stood placed, before its routed copper: the MCU part, the parts' models,
/// the blocks, the circuits and the placement, frozen so that router and pour measurements keep
/// comparing like with like while drum16 itself moves on. Its documents keep drum16's names, so
/// its board unit is `drum16`. Never edited: a newer reference board is a new frozen set.
const DRUM16_PLACED: [(&str, &str); 9] = [
    ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../frozen/drum16-placed/ATMEGA328P_TQFP32.part.splash")),
    ("drum16.models.part.splash", include_str!("../frozen/drum16-placed/drum16.models.part.splash")),
    ("drum16.blocks.splash", include_str!("../frozen/drum16-placed/drum16.blocks.splash")),
    ("drum16.ckt.splash", include_str!("../frozen/drum16-placed/drum16.ckt.splash")),
    ("drum16.pcb.splash", include_str!("../frozen/drum16-placed/drum16.pcb.splash")),
    ("kick.ckt.splash", include_str!("../frozen/drum16-placed/kick.ckt.splash")),
    ("toms.ckt.splash", include_str!("../frozen/drum16-placed/toms.ckt.splash")),
    ("snare.ckt.splash", include_str!("../frozen/drum16-placed/snare.ckt.splash")),
    ("hats.ckt.splash", include_str!("../frozen/drum16-placed/hats.ckt.splash")),
];

/// A stock project's documents, under their original names; unknown projects return `None`.
pub fn docs(name: &str) -> Option<Sources> {
    let stock: &[(&str, &str)] = match name {
        "ledctl" | "pair" => &LEDCTL,
        "ledavr" => &LEDAVR,
        "handheld" => &HANDHELD,
        "beep" => &BEEP,
        "drum16" => &DRUM16,
        "drum16-placed" => &DRUM16_PLACED,
        "tonebox" => &TONEBOX,
        "organ8" => &ORGAN8,
        _ => return None,
    };
    let mut sources = Sources::new();
    for &(name, text) in stock {
        sources
            .add(name, text)
            .expect("stock documents have distinct electronics names");
    }
    if name == "pair" {
        for (name, text) in [
            ("probe.ckt.splash", include_str!("../probe.ckt.splash")),
            ("pair.sys.splash", include_str!("../pair.sys.splash")),
        ] {
            sources
                .add(name, text)
                .expect("stock documents have distinct electronics names");
        }
    }
    sources
        .ensure_system()
        .expect("the stock project has a system document");
    Some(sources)
}

/// The controller's evaluated documents, cloned from the process-wide cache.
/// Panics with located diagnostics if the checked-in documents do not build.
pub fn ledctl_eval() -> Evaluated {
    cached_ledctl().clone()
}

/// The controller design, sharing the cached evaluation's circuits and boards.
pub fn ledctl() -> Design {
    cached_ledctl().design.clone()
}

fn cached_ledctl() -> &'static Evaluated {
    static LEDCTL: OnceLock<Evaluated> = OnceLock::new();
    LEDCTL.get_or_init(|| {
        let sources = docs("ledctl").expect("the controller has stock documents");
        let libs = std_libs();
        let evaluated = evaluate(&sources, &libs)
            .unwrap_or_else(|diags| document_failure(&sources, &libs, &diags));
        for state in evaluated
            .units
            .iter()
            .map(|unit| &unit.state)
            .chain(evaluated.benches.iter().map(|bench| &bench.state))
        {
            if !matches!(state, UnitState::Built) {
                document_failure(&sources, &libs, state.diags());
            }
        }
        evaluated
    })
}

fn document_failure(sources: &Sources, libs: &Libs, diags: &[Diag]) -> ! {
    let messages = diags
        .iter()
        .map(|diag| {
            let locations = diag
                .sel
                .iter()
                .filter_map(|sel| match sel {
                    Sel::Source { doc, start, end } => Some(sources.describe(
                        libs,
                        &SourceRange {
                            doc: *doc,
                            span: *start..*end,
                        },
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if locations.is_empty() {
                diag.message.clone()
            } else {
                format!("{}: {}", locations.join(", "), diag.message)
            }
        })
        .collect::<Vec<_>>();
    panic!(
        "the checked-in controller documents must build:\n{}",
        messages.join("\n")
    );
}
