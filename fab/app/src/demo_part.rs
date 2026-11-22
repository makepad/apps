//! The demo plates ([`DEMOS`]), and the plate Fab opens with when no document is given:
//! `demo.fab.splash`, a project that places three object scripts (a parametric shelf bracket, a
//! bud vase of distance-field nodes and a gyroid lattice cube) on two filaments. They are
//! resources: real Splash, edited like any other. A new demo is a folder
//! `resources/demos/<key>/` holding `<key>.fab.splash` and the files it reads, and one
//! `demo!` line in [`DEMOS`] (a demo in a group of the Demos list names it first), its key in
//! the tests' `demo_tests!`. A demo's electronics documents (the names `DocKind::of_name` knows:
//! circuits, parts, firmware, benches, boards) are the plate's electronics documents, as a saved
//! plate's `elec/` folder is; everything else is a script.

use std::collections::BTreeMap;
use std::sync::Arc;

pub const PROJECT: &str = include_str!("../resources/demo.fab.splash");
pub const SCRIPTS: [(&str, &str); 3] = [
    ("bracket.cad.splash", include_str!("../resources/bracket.cad.splash")),
    ("vase.cad.splash", include_str!("../resources/vase.cad.splash")),
    ("lattice_cube.cad.splash", include_str!("../resources/lattice_cube.cad.splash")),
];

/// The demo objects of the Parts panel's Add menu: its id, its script file, the script and its footprint
/// on the bed (mm) at the script's defaults.
pub const ADDABLE: [(&str, &str, &str, [f64; 2]); 4] = [
    ("chain", "chain.cad.splash", include_str!("../resources/chain.cad.splash"), [170.0, 20.0]),
    ("chainmail", "chainmail.cad.splash", include_str!("../resources/chainmail.cad.splash"), [90.0, 70.0]),
    ("knurled_knob", "knurled_knob.cad.splash", include_str!("../resources/knurled_knob.cad.splash"), [30.0, 30.0]),
    ("voronoi_vase", "voronoi_vase.cad.splash", include_str!("../resources/voronoi_vase.cad.splash"), [50.0, 50.0]),
];

/// A demo plate: its project (`<key>.fab.splash`) and every file the project reads, all code.
/// Opening one builds it from these; nothing about it is stored.
pub struct Demo {
    /// Its name, the project's stem and the key its picture goes by (snake_case).
    pub key: &'static str,
    /// What the Demos list calls it.
    pub title: &'static str,
    /// Its group in the Demos list, under a head of its own ("Studio"); empty: ungrouped, listed
    /// before the groups.
    pub group: &'static str,
    /// File name and text: the project and the scripts and paint files it reads.
    pub files: &'static [(&'static str, &'static str)],
}

/// One plate of the analog drum machine: `$plate` from its demo folder, with the electronics
/// fixtures' documents its case script reads and its Electronics workspace plays.
macro_rules! drum16 {
    ($key:literal, $title:literal, $plate:literal) => {
        Demo {
            key: $key,
            title: $title,
            group: "Electronics",
            files: &[
                ($plate, include_str!(concat!("../resources/demos/drum16/", $plate))),
                ("drum16.cad.splash", include_str!("../../elec/fixtures/drum16.cad.splash")),
                ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash")),
                ("drum16.models.part.splash", include_str!("../../elec/fixtures/drum16.models.part.splash")),
                ("drum16.blocks.splash", include_str!("../../elec/fixtures/drum16.blocks.splash")),
                ("drum16.ckt.splash", include_str!("../../elec/fixtures/drum16.ckt.splash")),
                ("drum16.pcb.splash", include_str!("../../elec/fixtures/drum16.pcb.splash")),
                ("kick.ckt.splash", include_str!("../../elec/fixtures/kick.ckt.splash")),
                ("toms.ckt.splash", include_str!("../../elec/fixtures/toms.ckt.splash")),
                ("snare.ckt.splash", include_str!("../../elec/fixtures/snare.ckt.splash")),
                ("hats.ckt.splash", include_str!("../../elec/fixtures/hats.ckt.splash")),
                ("drum16.fw.splash", include_str!("../../elec/fixtures/drum16.fw.splash")),
                ("drum16.bench.splash", include_str!("../../elec/fixtures/drum16.bench.splash")),
                ("voices.bench.splash", include_str!("../../elec/fixtures/voices.bench.splash")),
            ],
        }
    };
}

/// A demo kept as a folder of its own, `resources/demos/<key>/`: `<key>.fab.splash` and the
/// files it reads, flat beside it. One line in [`DEMOS`] lists it; a group's name before its key
/// puts it in that group, and demos that share a script share a folder (`in "<folder>"`).
macro_rules! demo {
    ($key:literal, $title:literal, [$($file:literal),* $(,)?]) => {
        demo!("", $key, $title, [$($file),*])
    };
    ($group:literal, $key:literal, $title:literal, [$($file:literal),* $(,)?]) => {
        demo!($group, $key, $title, in $key, [$($file),*])
    };
    ($group:literal, $key:literal, $title:literal, in $dir:literal, [$($file:literal),* $(,)?]) => {
        Demo { key: $key, title: $title, group: $group, files: &[$(($file, include_str!(concat!("../resources/demos/", $dir, "/", $file)))),*] }
    };
}

/// One of the Parts panel's addable objects ([`ADDABLE`]) on a plate of its own:
/// `resources/<key>.fab.splash` placing the object of the same script the Add menu places.
macro_rules! part_demo {
    ($key:literal, $title:literal) => {
        Demo {
            key: $key,
            title: $title,
            group: "",
            files: &[
                (concat!($key, ".fab.splash"), include_str!(concat!("../resources/", $key, ".fab.splash"))),
                (concat!($key, ".cad.splash"), include_str!(concat!("../resources/", $key, ".cad.splash"))),
            ],
        }
    };
}

/// Every demo plate, in the order the Demos list shows them.
pub const DEMOS: &[Demo] = &[
    Demo { key: "demo", title: "Demo plate", group: "", files: &[("demo.fab.splash", PROJECT), SCRIPTS[0], SCRIPTS[1], SCRIPTS[2]] },
    Demo {
        key: "motor_mount",
        title: "Motor mount (exact B-rep)",
        group: "",
        files: &[("motor_mount.fab.splash", include_str!("../resources/motor_mount.fab.splash")), ("motor_mount.cad.splash", include_str!("../resources/motor_mount.cad.splash"))],
    },
    // Procedural paint on the bud vase: the region is a plain Splash field (`m.sdf`), evaluated
    // by the same field evaluator as any shape and baked into the same paint bricks.
    Demo {
        key: "noise_vase",
        title: "Noise vase",
        group: "",
        files: &[
            ("noise_vase.fab.splash", include_str!("../resources/noise_vase.fab.splash")),
            ("noise_vase.cad.splash", concat!(include_str!("../resources/vase.cad.splash"), "\n", include_str!("../resources/noise_paint.splash"))),
        ],
    },
    // Materials by a field of the part's script (`Mix`): the lattice cube by Voronoi cells, the
    // vase blending up its height (dithered), a marble block.
    Demo {
        key: "voronoi_cube",
        title: "Voronoi cube (two filaments)",
        group: "",
        files: &[
            ("voronoi_cube.fab.splash", include_str!("../resources/voronoi_cube.fab.splash")),
            ("voronoi_cube.cad.splash", concat!(include_str!("../resources/lattice_cube.cad.splash"), "\n", include_str!("../resources/voronoi_paint.splash"))),
        ],
    },
    Demo {
        key: "gradient_vase",
        title: "Gradient vase (dithered)",
        group: "",
        files: &[
            ("gradient_vase.fab.splash", include_str!("../resources/gradient_vase.fab.splash")),
            ("gradient_vase.cad.splash", concat!(include_str!("../resources/vase.cad.splash"), "\n", include_str!("../resources/gradient_paint.splash"))),
        ],
    },
    demo!("marble_block", "Marble block", ["marble_block.fab.splash", "marble_block.cad.splash"]),
    // Mechanisms printed in place, in two filaments: they come off the bed moving.
    demo!("jewellery_box", "Jewellery box", ["jewellery_box.fab.splash", "jewellery_box.cad.splash"]),
    demo!("roller_bearing", "Roller bearing", ["roller_bearing.fab.splash", "roller_bearing.cad.splash"]),
    demo!("ball_cage", "Ball in a cage", ["ball_cage.fab.splash", "ball_cage.cad.splash"]),
    demo!("bistable_switch", "Bistable switch", ["bistable_switch.fab.splash", "bistable_switch.cad.splash"]),
    demo!("travel_cup", "Collapsible travel cup", ["travel_cup.fab.splash", "travel_cup.cad.splash"]),
    demo!("articulated_dragon", "Articulated dragon", ["articulated_dragon.fab.splash", "articulated_dragon.cad.splash"]),
    demo!("poseable_robot", "Poseable robot", ["poseable_robot.fab.splash", "poseable_robot.cad.splash"]),
    demo!("gear_heart", "Gear heart", ["gear_heart.fab.splash", "gear_heart.cad.splash"]),
    demo!("little_car", "Little car", ["little_car.fab.splash", "little_car.cad.splash"]),
    // Servos on its hinges, set from the Mechanics panel.
    demo!("servo_arm", "Servo arm", ["servo_arm.fab.splash", "servo_arm.cad.splash"]),
    demo!("exact_bracket", "Exact angle bracket", ["exact_bracket.fab.splash", "exact_bracket.cad.splash"]),
    demo!("surface_textures", "Surface textures", ["surface_textures.fab.splash", "surface_textures.cad.splash"]),
    // The Add menu's objects, each on a plate of its own.
    part_demo!("chain", "Chain"),
    part_demo!("chainmail", "Chainmail"),
    part_demo!("knurled_knob", "Knurled knob"),
    part_demo!("voronoi_vase", "Voronoi vase"),
    // Links printed linked, each a body of its own in Mechanics; a fill paints whole links.
    demo!("two_colour_chain", "Two-colour chain", ["two_colour_chain.fab.splash", "two_colour_chain.cad.splash"]),
    demo!("chainmail_sheet", "Chainmail sheet", ["chainmail_sheet.fab.splash", "chainmail_sheet.cad.splash"]),
    // A glider printed flat, one blended field: fly it in the wind tunnel.
    demo!("glider", "Hand-launch glider", ["glider.fab.splash", "glider.cad.splash"]),
    // Planetary herringbone gears printed meshed: turn the hub and the cups ride round, spinning.
    demo!("cup_carousel", "Cup carousel", ["cup_carousel.fab.splash", "cup_carousel.cad.splash"]),
    // A puzzle printed apart: six notched sticks that lock into a cross.
    demo!("burr_puzzle", "Six-piece burr", ["burr_puzzle.fab.splash", "burr_puzzle.cad.splash"]),
    // Colour as a volume: inlaid pips, stripes up twisted flutes, mosaic grout, leaded glass, a
    // planet's continents, a landscape's heights, agate bands, wood grain and a fade along a knot.
    demo!("Colour fields", "inlaid_dice", "Inlaid dice", ["inlaid_dice.fab.splash", "inlaid_dice.cad.splash"]),
    demo!("Colour fields", "twisted_vase", "Twisted stripe vase", ["twisted_vase.fab.splash", "twisted_vase.cad.splash"]),
    demo!("Colour fields", "mosaic_coasters", "Mosaic coasters", ["mosaic_coasters.fab.splash", "mosaic_coasters.cad.splash"]),
    demo!("Colour fields", "stained_glass", "Stained-glass panel", ["stained_glass.fab.splash", "stained_glass.cad.splash"]),
    demo!("Colour fields", "planet_globe", "Planet globe", ["planet_globe.fab.splash", "planet_globe.cad.splash"]),
    demo!("Colour fields", "terrain_tile", "Terrain tile", ["terrain_tile.fab.splash", "terrain_tile.cad.splash"]),
    demo!("Colour fields", "geode_bookends", "Geode bookends", ["geode_bookends.fab.splash", "geode_bookends.cad.splash"]),
    demo!("Colour fields", "wood_box", "Wood-grain box", ["wood_box.fab.splash", "wood_box.cad.splash"]),
    demo!("Colour fields", "trefoil_knot", "Trefoil knot", ["trefoil_knot.fab.splash", "trefoil_knot.cad.splash"]),
    demo!("Colour fields", "gyroid_lamp", "Gyroid lamp", ["gyroid_lamp.fab.splash", "gyroid_lamp.cad.splash"]),
    demo!("Colour fields", "damascus_cup", "Damascus pen holder", ["damascus_cup.fab.splash", "damascus_cup.cad.splash"]),
    demo!("Colour fields", "reef_dome", "Reef dome", ["reef_dome.fab.splash", "reef_dome.cad.splash"]),
    demo!("Colour fields", "fractal_sponge", "Fractal sponge", ["fractal_sponge.fab.splash", "fractal_sponge.cad.splash"]),
    demo!("Colour fields", "braid_bracelet", "Braid bracelet", ["braid_bracelet.fab.splash", "braid_bracelet.cad.splash"]),
    demo!("Colour fields", "chess_set", "Marble chess set", ["chess_set.fab.splash", "chess_set.cad.splash"]),
    demo!("Colour fields", "soft_grip", "Soft-grip handle", ["soft_grip.fab.splash", "soft_grip.cad.splash"]),
    demo!("Colour fields", "lattice_sole", "Lattice sole", ["lattice_sole.fab.splash", "lattice_sole.cad.splash"]),
    demo!("Colour fields", "voronoi_shade", "Voronoi shade", ["voronoi_shade.fab.splash", "voronoi_shade.cad.splash"]),
    // Studio: models that mix the engines (exact steering, field bodies, print-in-place joints) with
    // painted and volumetric colour, in up to nine filaments.
    demo!("Studio", "rally_car", "Rally car", in "studio_cars", ["rally_car.fab.splash", "studio_car.cad.splash"]),
    demo!("Studio", "marionette", "Jester marionette", ["marionette.fab.splash", "studio_marionette.cad.splash"]),
    demo!("Studio", "dune_buggy", "Dune buggy", in "studio_cars", ["dune_buggy.fab.splash", "studio_car.cad.splash"]),
    demo!("Studio", "roadster", "Boat-tail roadster", in "studio_cars", ["roadster.fab.splash", "studio_car.cad.splash"]),
    demo!("Studio", "concept_car", "Concept coupe", in "studio_cars", ["concept_car.fab.splash", "studio_car.cad.splash"]),
    demo!("Studio", "lizard_puppet", "Lizard hand puppet", ["lizard_puppet.fab.splash", "lizard_puppet.cad.splash"]),
    // Grown patterns: Turing patterns, Lichtenberg figures, scales, a slime mould's network and
    // flow lines grown on a part's own skin (`Turing{..}`, `Lichtenberg{..}`, `Scales{..}`,
    // `Physarum{..}`, `Flow{..}` in a mix).
    demo!("Grown patterns", "leopard_vase", "Leopard vase", ["leopard_vase.fab.splash", "leopard_vase.cad.splash"]),
    demo!("Grown patterns", "turing_lizard", "Turing lizard", in "lizard_puppet", ["turing_lizard.fab.splash", "lizard_puppet.cad.splash"]),
    demo!("Grown patterns", "coral_bowl", "Coral bowl", ["coral_bowl.fab.splash", "coral_bowl.cad.splash"]),
    demo!("Grown patterns", "lichtenberg_vase", "Lichtenberg vase", ["lichtenberg_vase.fab.splash", "lichtenberg_vase.cad.splash"]),
    demo!("Grown patterns", "scaled_egg", "Scaled egg", ["scaled_egg.fab.splash", "scaled_egg.cad.splash"]),
    demo!("Grown patterns", "mould_dish", "Slime mould: trunks", ["mould_dish.fab.splash", "mould_dish.cad.splash"]),
    demo!("Grown patterns", "mould_lace", "Slime mould: lace", ["mould_lace.fab.splash", "mould_lace.cad.splash"]),
    demo!("Grown patterns", "mould_pebble", "Slime mould pebble", ["mould_pebble.fab.splash", "mould_pebble.cad.splash"]),
    demo!("Grown patterns", "mould_relief", "Slime mould relief", ["mould_relief.fab.splash", "mould_relief.cad.splash"]),
    demo!("Grown patterns", "flow_pebble", "Flow pebble", ["flow_pebble.fab.splash", "flow_pebble.cad.splash"]),
    // Showpieces: rigid parts fused to a TPU spine or cord, printed in place, that flex and spring
    // back; figures whose hinges hold a pose on TPU brakes.
    demo!("Showpieces", "flex_dragon", "Flex dragon", ["flex_dragon.fab.splash", "flex_dragon.cad.splash"]),
    demo!("Showpieces", "flex_snake", "Flex snake", ["flex_snake.fab.splash", "flex_snake.cad.splash"]),
    demo!("Showpieces", "flex_acrobat", "Flex acrobat", ["flex_acrobat.fab.splash", "flex_acrobat.cad.splash"]),
    demo!("Showpieces", "poseable_athlete", "Poseable athlete", ["poseable_athlete.fab.splash", "poseable_athlete.cad.splash"]),
    demo!("Showpieces", "flex_jellyfish", "Flex jellyfish", ["flex_jellyfish.fab.splash", "flex_jellyfish.cad.splash"]),
    demo!("Showpieces", "flex_beetle", "Flex beetle", ["flex_beetle.fab.splash", "flex_beetle.cad.splash"]),
    demo!("Showpieces", "flex_flower", "Flex flower", ["flex_flower.fab.splash", "flex_flower.cad.splash"]),
    // Electronics: the plate is its documents, the electronics fixtures' own files (one copy).
    // The LED chaser runs its firmware on a simulated ATmega328P when the Electronics workspace plays;
    // its board document shows the placed board in 3D, and its case script prints the case round it
    // (the electronics fixtures' own file, as for its documents).
    Demo {
        key: "ledavr",
        title: "LED chaser (ATmega328P)",
        group: "Electronics",
        files: &[
            ("ledavr.fab.splash", include_str!("../resources/demos/ledavr/ledavr.fab.splash")),
            ("ledavr.cad.splash", include_str!("../../elec/fixtures/ledavr.cad.splash")),
            ("parts/avr/ATMEGA328P_TQFP32.part.splash", include_str!("../../elec/fixtures/parts/avr/ATMEGA328P_TQFP32.part.splash")),
            ("ledavr.models.part.splash", include_str!("../../elec/fixtures/ledavr.models.part.splash")),
            ("ledavr.ckt.splash", include_str!("../../elec/fixtures/ledavr.ckt.splash")),
            ("ledavr.pcb.splash", include_str!("../../elec/fixtures/ledavr.pcb.splash")),
            ("ledavr.fw.splash", include_str!("../../elec/fixtures/ledavr.fw.splash")),
            ("ledavr.bench.splash", include_str!("../../elec/fixtures/ledavr.bench.splash")),
        ],
    },
    // The analog drum machine: the whole board playing its sequencer firmware, and each voice a
    // circuit of its own on its bench, its pots turned live on the Bench (the electronics
    // fixtures' own files). Its case prints on two plates, the base and the lid with the keycaps
    // and knobs, each inside the part of the bed both nozzles reach.
    drum16!("drum16", "Analog drum machine", "drum16.fab.splash"),
    drum16!("drum16_lid", "Analog drum machine: lid and keys", "drum16_lid.fab.splash"),
    // One key, one knob, one LED and one audio jack: holding the key lights the LED and plays an
    // RC oscillator's tone that the knob tunes, closed in its case too (the electronics fixtures'
    // own files).
    Demo {
        key: "tonebox",
        title: "Tone box",
        group: "Electronics",
        files: &[
            ("tonebox.fab.splash", include_str!("../resources/demos/tonebox/tonebox.fab.splash")),
            ("tonebox.cad.splash", include_str!("../../elec/fixtures/tonebox.cad.splash")),
            ("tonebox.models.part.splash", include_str!("../../elec/fixtures/tonebox.models.part.splash")),
            ("tonebox.ckt.splash", include_str!("../../elec/fixtures/tonebox.ckt.splash")),
            ("tonebox.pcb.splash", include_str!("../../elec/fixtures/tonebox.pcb.splash")),
            ("tonebox.bench.splash", include_str!("../../elec/fixtures/tonebox.bench.splash")),
        ],
    },
    // A sand toy on an ESP32-S3 with an AMOLED panel, touch, an IMU, a codec and a speaker amplifier: the
    // firmware runs on the simulated S3 and draws on the panel (the electronics fixtures' own files). The board stands
    // bare, with its parts' bodies.
    Demo {
        key: "handheld",
        title: "ESP32 handheld: sand",
        group: "Electronics",
        files: &[
            ("handheld.fab.splash", include_str!("../resources/demos/handheld/handheld.fab.splash")),
            ("handheld.cad.splash", include_str!("../resources/demos/handheld/handheld.cad.splash")),
            ("parts/s3/ESP32S3_QFN56.part.splash", include_str!("../../elec/fixtures/parts/s3/ESP32S3_QFN56.part.splash")),
            ("parts/audio/ES8311_QFN20.part.splash", include_str!("../../elec/fixtures/parts/audio/ES8311_QFN20.part.splash")),
            ("parts/audio/NS4150B_MSOP8.part.splash", include_str!("../../elec/fixtures/parts/audio/NS4150B_MSOP8.part.splash")),
            ("handheld.models.part.splash", include_str!("../../elec/fixtures/handheld.models.part.splash")),
            ("handheld.ckt.splash", include_str!("../../elec/fixtures/handheld.ckt.splash")),
            ("handheld.pcb.splash", include_str!("../../elec/fixtures/handheld.pcb.splash")),
            ("handheld.fw.splash", include_str!("../../elec/fixtures/handheld.fw.splash")),
            ("handheld.bench.splash", include_str!("../../elec/fixtures/handheld.bench.splash")),
        ],
    },
];

impl Demo {
    /// The demo as a document (not evaluated yet): its electronics files are the plate's
    /// electronics documents, the rest its scripts. O(files).
    pub fn model(&self) -> Result<fab_model::FabModel, String> {
        let project = format!("{}.{}", self.key, fab_model::project::EXTENSION);
        let text = self.files.iter().find(|(f, _)| *f == project).ok_or_else(|| format!("the demo {} has no {project}", self.key))?.1;
        let elec = |f: &str| fab_elec_lang::DocKind::of_name(f).is_some();
        let scripts: BTreeMap<String, String> = self.files.iter().filter(|(f, _)| *f != project && !elec(f)).map(|(f, s)| (f.to_string(), s.to_string())).collect();
        let docs: fab_model::Docs = self.files.iter().filter(|(f, _)| elec(f)).map(|(f, s)| (f.to_string(), Arc::from(*s))).collect();
        let mut model = fab_model::FabModel::load(self.title, text, scripts).map_err(|f| format!("the demo {}: {f}", self.key))?;
        if !docs.is_empty() {
            model.set_docs(Arc::new(docs));
        }
        Ok(model)
    }
}

/// The demos the Plates panel lists and paints, in [`DEMOS`]' order. `FAB_DEMOS` (a comma list
/// of demo keys) limits the bundled demos to those keys, for teaching and kiosk setups; unset, or
/// empty, lists them all. A key that is no demo is logged once and ignored.
pub fn listed() -> &'static [&'static Demo] {
    static LISTED: std::sync::OnceLock<Vec<&'static Demo>> = std::sync::OnceLock::new();
    LISTED.get_or_init(|| {
        let keys: Vec<String> = std::env::var("FAB_DEMOS").unwrap_or_default().split(',').map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
        if keys.is_empty() {
            return DEMOS.iter().collect();
        }
        for key in keys.iter().filter(|k| !DEMOS.iter().any(|d| d.key == k.as_str())) {
            makepad_widgets::log!("fab: FAB_DEMOS names no demo '{key}'; ignored");
        }
        DEMOS.iter().filter(|d| keys.iter().any(|k| k == d.key)).collect()
    })
}

/// The demo `key`.
pub fn demo(key: &str) -> Option<&'static Demo> {
    DEMOS.iter().find(|d| d.key == key)
}

/// The demo project as a document (not evaluated yet).
#[cfg(test)]
pub fn model() -> fab_model::FabModel {
    demo("demo").and_then(|d| d.model().ok()).expect("the demo project loads")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A demo built as the app builds a plate it opens (its case scripts read its electronics
    /// documents), on Fab's default printer.
    pub(crate) struct DemoBuild {
        pub model: fab_model::FabModel,
        pub evaluator: fab_model::eval::Evaluator,
        pub compiled: fab_model::program::Compiled,
        pub ev: fab_model::eval::Evaluated,
        /// What the build filed in the error log ([`crate::model_host::filed`]).
        pub filed: Vec<String>,
    }

    fn build(key: &str) -> DemoBuild {
        let d = DEMOS.iter().find(|d| d.key == key).unwrap_or_else(|| panic!("{key}: not a demo"));
        crate::model_host::filed::listen();
        let mut model = d.model().unwrap_or_else(|e| panic!("{e}"));
        // A plate's case scripts read its own boards, as the app gives each plate (`doc_env`).
        if !model.docs().is_empty() {
            model.set_env(crate::elec_host::doc_env(model.docs(), model.elec_assets()));
        }
        let mut evaluator = fab_model::eval::Evaluator::new(crate::print::CheckSetup::fab_defaults().machine().expect("Fab's default printer"));
        let (compiled, ev) = model.evaluate(&mut evaluator, None).unwrap_or_else(|f| panic!("{key}: {f}"));
        DemoBuild { model, evaluator, compiled, ev, filed: crate::model_host::filed::take() }
    }

    /// The demo plate builds, and a vase edit rebuilds only the vase.
    #[test]
    fn the_demo_builds_and_a_vase_edit_rebuilds_only_the_vase() {
        let mut m = model();
        let mut e = fab_model::eval::Evaluator::new(fab_plate::machine::Machine::dual_nozzle_350());
        let (_, ev) = m.evaluate(&mut e, None).expect("the demo builds");
        assert_eq!(ev.plate.parts.len(), 3);
        let head = m.head();
        let change = fab_model::Change::Param { instance: "vase".into(), name: "vase_height".into(), value: Some(0.08) };
        let (_, built) = m.apply(&mut e, "b", head, &[change], None).expect("the vase rebuilds");
        // Only the vase was rebuilt: the bracket and the lattice came from the cache.
        let (_, after) = built.unwrap();
        assert!(after.changes.iter().zip(&after.plate.parts).all(|(c, p)| p.object.starts_with("vase/") || *c != fab_model::eval::PartChange::New));
    }

    /// A demo builds on the default printer with no script error filed (nothing reaches the error
    /// log: [`crate::model_host::filed`]), a plate with parts (the material demos are one part
    /// whose last paint is its script's `Mix` of its two print colours, printing in the first),
    /// and slices on the live engine as the Slice view does, with the plate's own settings and as
    /// many filaments as it prints with (on the two nozzles in turn, up to the nine a printer may
    /// hold), to a plan its sliced checks find no error in. Then Print's path: the same build
    /// settled on the engine that sliced it, as Print settles the plate the Slice view shows. The
    /// job's tools are the colours that print (a demo whose job loses tools says so), and one
    /// slice makes it. A settle on an engine that sliced nothing before is checked by the
    /// two-material jellyfish below and by the settles of `print.rs`.
    ///
    /// The checks of what the demo is for run on this same build, before it slices: its jointed
    /// piece's in Mechanics ([`crate::mechanics::tests::grabbed_released_comes_to_rest`] and the
    /// marionette's own), a showpiece's ([`showpiece_checks_free_and_moves`]) and the athlete's.
    /// One build a demo: they were tests of their own, each building the demo again in another
    /// process of the land gate's tests (the marionette's 25 s build four times).
    fn builds_and_slices(key: &str) {
        use makepad_fab_slice::live::LiveEvent;
        let DemoBuild { model: m, evaluator: mut e, compiled: c, ev, filed } = build(key);
        let ev = Arc::new(ev);
        assert!(filed.is_empty(), "{key}: script errors filed: {filed:?}");
        if let Some(&(_, firmware)) = ELECTRONICS.iter().find(|e| e.0 == key) {
            electronics(key, &m, &ev, firmware);
            // a case's parts sit where Fab's default printer prints their colours, as the app maps
            // them onto its heads: none shows red
            let mapped = crate::print::Printing::default().refresh_printer(&mut Default::default(), Some(&ev)).unwrap_or_default();
            e.mapped = mapped.clone();
            let (_, mapped_ev) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{key}: {f}"));
            let tools: Vec<_> = mapped.tools.iter().collect();
            let blocking: Vec<_> = mapped_ev.reports.iter().filter(|r| r.blocking())
                .map(|r| (mapped_ev.plate.parts.get(r.part).map(|p| p.name.as_str()), r.issues.iter().map(|i| i.describe(&mapped_ev.machine)).collect::<Vec<_>>())).collect();
            assert!(blocking.is_empty(), "{key}: every part sits where Fab's default printer reaches it, colours on {tools:?}: {blocking:?}");
            // and inside the part of the bed every nozzle reaches, so it prints whichever nozzle a
            // loadout puts its colours on
            let machine = &mapped_ev.machine;
            let shared = machine.reach(&(0..machine.nozzles.len()).collect::<Vec<_>>());
            let outside: Vec<_> = mapped_ev.plate.parts.iter().filter(|p| !shared.contains_footprint(&p.bounds(), fab_plate::machine::EDGE_SLACK))
                .map(|p| p.name.as_str()).collect();
            assert!(outside.is_empty(), "{key}: parts outside the bed's part every nozzle reaches: {outside:?}");
        }
        // a bare board is bought, not printed: its placement shows it (and Mechanics carries it)
        // while the plate prints nothing, so there is nothing to slice
        let shown_board = ev.plate.excluded.iter().any(|p| !p.handles.hidden && p.link.as_deref().is_some_and(|l| l.starts_with("board:")));
        assert!(!ev.plate.parts.is_empty() || shown_board, "{key}: no parts");
        let rows: Vec<&str> = TOO_BIG_FOR_RESIN.iter().filter_map(|r| r.strip_prefix(key)?.strip_prefix(' ')).collect();
        assert_eq!(too_big_for_resin(&c, &ev), rows, "{key}: the placements too big for the resin printer's bed");
        assert_eq!(!ev.mech.is_empty(), crate::mechanics::tests::JOINTED_DEMOS.contains(&key), "{key}: the demos with a jointed piece, and only they, are Mechanics' jointed demos");
        if let Some(mech) = ev.mech.first() {
            crate::mechanics::tests::grabbed_released_comes_to_rest(key, mech);
            match key {
                "marionette" => {
                    crate::mechanics::tests::the_marionette_hangs_on_its_strings_and_follows_the_hand(mech);
                    crate::mechanics::tests::the_marionettes_arms_turn_out_of_their_print_overlap(mech);
                }
                "poseable_athlete" => {
                    the_poseable_athletes_parts_are_each_one_piece(&ev, &mut e);
                    the_poseable_athlete_holds_a_pose(mech);
                }
                _ => {}
            }
        }
        if demo(key).is_some_and(|d| d.group == "Showpieces") {
            showpiece_checks_free_and_moves(key, &ev);
        }
        if ev.plate.parts.is_empty() {
            return;
        }
        let colours = match key {
            "voronoi_cube" | "gradient_vase" => Some(["Blue", "Yellow"]),
            "marble_block" => Some(["Black", "White"]),
            _ => None,
        };
        if let Some(colours) = colours {
            assert_eq!(ev.plate.parts.len(), 1, "{key}");
            let paint = ev.paint[0].as_ref().unwrap_or_else(|| panic!("{key}: painted"));
            let Some(fab_paint_field::Op::Mix { mix, .. }) = paint.program.layers.last().map(|l| &l.op) else { panic!("{key}: no mix") };
            let names: Vec<_> = mix.slots.iter().map(|&s| paint.colors[s as usize].color.as_deref()).collect();
            assert_eq!(names, colours.map(Some).to_vec(), "{key}");
            assert_eq!(ev.part_color(0), Some(colours[0]), "{key}");
        }
        // the job's tools are numbered from 1 in the order the plate uses its colours
        let tools = ev.plate.parts.iter().flat_map(|p| std::iter::once(p.filament).chain(p.paint.iter().flat_map(|sp| sp.colors.iter().filter_map(|c| c.filament)))).max().unwrap_or(1) as usize;
        assert!(tools <= 9, "{key}: prints with {tools} filaments");
        let setup = crate::print::slice_bench::setup(&ev);
        let (engine, rx) = crate::live_slice::cpu_engine();
        engine.set_input(crate::live_slice::input_for(crate::live_slice::plate_id(&ev), &setup, Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)));
        let report = rx.iter().find_map(|e| match e {
            LiveEvent::Issues { report, done, total, .. } if done == total && total > 0 => Some(report),
            LiveEvent::Failed { error, .. } => panic!("{key}: does not slice: {error}"),
            _ => None,
        }).expect("the engine answers");
        let errors: Vec<String> = report.issues.iter().filter(|i| i.severity == fab_printability::Severity::Error).map(|i| format!("{} {:?}: {}", i.kind, i.object, i.message)).collect();
        assert!(errors.is_empty(), "{key}: {errors:?}");
        let (r, slices) = settled_on(&engine, &rx, &ev).unwrap_or_else(|e| panic!("{key}: {e}"));
        let mut used = r.stats.used.clone();
        used.sort_unstable();
        let job = r.setup.filaments.len();
        if r.tools.len() < tools {
            println!("{key}: {tools} job tools, prints {} ({:?})", r.tools.len(), r.tools);
        }
        assert!(slices == 1 && r.tools.len() == job && used == (0..job).collect::<Vec<_>>(), "{key}: tools {:?}, used {used:?}, job {job}, slices {slices}", r.tools);
    }

    /// The placements too big for the resin printer's 120 x 80 mm bed (a demo and its instance's
    /// object): a 120 x 120 mm mount, a 64 x 57 mm box beside its lid, an 85 mm cup, a 204 mm
    /// dragon, a 97 x 102 mm gear heart, a 170 mm chain and a 160 mm two-colour one, a 150 mm
    /// glider, a 190 mm square of coasters, a 100 x 140 mm stained-glass panel, a 173 mm pair of
    /// geode bookends, a 138 mm box beside its lid, a 149 mm chess set, a 243 mm sole, a 140 mm
    /// lamp shade, a 126 mm car, a 190 mm marionette beside its 134 mm control, a 120 mm buggy, a
    /// 141 mm roadster, a 130 mm coupe, a 148 mm lizard puppet (and its Turing-skinned twin), a 150
    /// mm coral bowl, an 88 mm Lichtenberg vase, two 100 mm mould dishes, a 179 mm flex dragon, a
    /// 177 mm flex snake, a 209 mm flex acrobat, a 196 mm poseable athlete, a 137 mm flex
    /// jellyfish, a 138 mm flex beetle, a 107 mm flex flower, and the drum machine's two
    /// 219.8 x 164.8 mm case shells. The resin census
    /// ([`crate::sla`]) prints demos that fit.
    const TOO_BIG_FOR_RESIN: [&str; 38] = ["motor_mount motor_mount/mount", "jewellery_box jewellery_box/Jewellery box", "travel_cup travel_cup/Travel cup", "articulated_dragon articulated_dragon/Articulated dragon", "gear_heart gear_heart/Gear heart", "servo_arm servo_arm/Servo arm", "chain chain/Chain", "two_colour_chain two_colour_chain/Chain", "glider glider/Glider", "mosaic_coasters mosaic_coasters/Mosaic coasters", "stained_glass stained_glass/Stained-glass panel", "geode_bookends geode_bookends/Geode bookends", "wood_box wood_box/Wood-grain box", "chess_set chess_set/Chess set", "lattice_sole lattice_sole/Lattice sole", "voronoi_shade voronoi_shade/Voronoi shade", "rally_car studio_car/Rally car", "marionette studio_marionette/Marionette", "marionette studio_marionette/Control", "dune_buggy studio_car/Dune buggy", "roadster studio_car/Roadster", "concept_car studio_car/Concept coupe", "lizard_puppet lizard_puppet/Lizard puppet", "turing_lizard lizard_puppet/Lizard puppet", "coral_bowl coral_bowl/bowl", "lichtenberg_vase lichtenberg_vase/vase", "mould_dish mould_dish/dish", "mould_lace mould_lace/dish", "mould_relief mould_relief/relief", "flex_dragon flex_dragon/Flex dragon", "flex_snake flex_snake/Flex snake", "flex_acrobat flex_acrobat/Flex acrobat", "poseable_athlete poseable_athlete/Athlete", "flex_jellyfish flex_jellyfish/Flex jellyfish", "flex_beetle flex_beetle/Flex beetle", "flex_flower flex_flower/Flex flower", "drum16 drum16/Base", "drum16_lid drum16/Lid"];

    /// The placements of the plate `ev` (its changes `c`) too big for the resin printer's bed
    /// (`instance/object`, an assembly's parts as one, in the plate's order): their footprint, as
    /// the plate turns them, is wider or deeper than the bed by more than the slack the bed check
    /// allows at its two edges.
    fn too_big_for_resin(c: &fab_model::program::Compiled, ev: &fab_model::eval::Evaluated) -> Vec<String> {
        use fab_model::{Change, PlaceArg};
        let bed = fab_sla::printer::simulated().machine().bed;
        let slack = 2.0 * fab_plate::machine::EDGE_SLACK;
        let mut placed: Vec<((String, String), ([f64; 2], [f64; 2]))> = Vec::new();
        for p in &ev.plate.parts {
            let Some(Change::Place { instance, object, .. }) = Change::place(c, &p.object, PlaceArg::At(None)) else { continue };
            let b = p.bounds();
            let (lo, hi) = ([b.min[0], b.min[1]], [b.max[0], b.max[1]]);
            match placed.iter_mut().find(|g| g.0 .0 == instance && g.0 .1 == object) {
                Some(g) => g.1 = ([g.1 .0[0].min(lo[0]), g.1 .0[1].min(lo[1])], [g.1 .1[0].max(hi[0]), g.1 .1[1].max(hi[1])]),
                None => placed.push(((instance, object), (lo, hi))),
            }
        }
        placed.into_iter().filter(|(_, (lo, hi))| hi[0] - lo[0] > bed.width() + slack || hi[1] - lo[1] > bed.depth() + slack).map(|((i, o), _)| format!("{i}/{o}")).collect()
    }

    /// The demos whose plate carries electronics documents beside the parts it prints, and whether
    /// their board runs firmware (the tone box is all analog and logic).
    const ELECTRONICS: &[(&str, bool)] = &[("ledavr", true), ("drum16", true), ("drum16_lid", true), ("tonebox", false), ("handheld", true)];

    /// The documents an electronics demo's plate holds evaluate as the Electronics workspace loads
    /// them: every unit and bench built, a bench running the firmware where the board has one.
    fn electronics(key: &str, m: &fab_model::FabModel, ev: &fab_model::eval::Evaluated, firmware: bool) {
        assert!(m.docs().keys().all(|name| fab_elec_lang::DocKind::of_name(name).is_some()),
            "{key}: only electronics files become the plate's documents");
        let mut sources = fab_elec_lang::Sources::new();
        for (name, text) in m.docs().iter() {
            sources.add(name.clone(), text.clone()).unwrap_or_else(|d| panic!("{key}: {name}: {}", d.message));
        }
        sources.ensure_system().unwrap_or_else(|d| panic!("{key}: {}", d.message));
        let evaluated = fab_elec_lang::evaluate(&sources, &fab_elec_lang::std_libs())
            .unwrap_or_else(|diags| panic!("{key}: its documents do not evaluate: {diags:?}"));
        assert!(!evaluated.units.is_empty(), "{key}: the plate has a circuit");
        for state in evaluated.units.iter().map(|u| &u.state).chain(evaluated.benches.iter().map(|b| &b.state)) {
            assert!(matches!(state, fab_elec_lang::UnitState::Built), "{key}: {:?}", state.diags());
        }
        assert_eq!(evaluated.benches.iter().any(|b| !b.spec.firmware.is_empty()), firmware, "{key}: a bench runs the firmware");
        assert!(!evaluated.benches.is_empty(), "{key}: the plate has a bench to play");
        assert!(evaluated.design.units.iter().any(|u| u.board.is_some()), "{key}: the plate carries its board, so the 3D view shows it");
        // each motion sensor rides the part its board prints as, or its case's base, so the
        // Mechanics sim tilts it (`motion_link`); a board lying flat reads 1 g along Z at rest, its sign
        // the part's (the handheld's IMU Z points into its board, as on the real board)
        let mounts = fab_elec_mech::motion_mounts(&evaluated.design, ev, &mut |why| panic!("{key}: {why}"));
        let sensors: usize = evaluated.design.units.iter()
            .filter_map(|u| Some(fab_elec_mech::board_sensors(u.board.as_deref()?, &u.circuit, &mut |why| panic!("{key}: {why}")).len()))
            .sum();
        assert_eq!(mounts.len(), sensors, "{key}: every motion sensor rides a printed part");
        assert!(key != "handheld" || mounts.len() == 1, "the handheld's IMU rides its bare board");
        for mount in &mounts {
            let board = |k: usize| ev.plate.excluded.get(k).is_some_and(|p| !p.handles.hidden && p.link.as_deref().is_some_and(|l| l.starts_with("board:")));
            assert!(ev.plate.parts.get(mount.part).is_some() || board(mount.part - ev.plate.parts.len()), "{key}: a motion sensor rides a printed part or its shown board, which Mechanics moves");
            assert!(mount.axes[2][2].abs() > 0.999, "{key}: the sensor's Z is along its board's normal, as the board lies: {:?}", mount.axes);
            assert!(key != "handheld" || mount.axes[2][2] < -0.999, "the handheld's IMU Z points into its board, as the real board's does: {:?}", mount.axes);
        }
    }

    /// One test a demo, so the runner builds and slices them side by side.
    macro_rules! demo_tests {
        ($($key:ident),* $(,)?) => {
            const TESTED: &[&str] = &[$(stringify!($key)),*];
            mod builds_and_slices {
                $(#[test]
                fn $key() {
                    super::builds_and_slices(stringify!($key))
                })*
            }
        };
    }
    demo_tests!(demo, motor_mount, noise_vase, voronoi_cube, gradient_vase, marble_block, jewellery_box, roller_bearing, ball_cage, bistable_switch, travel_cup, articulated_dragon, poseable_robot, gear_heart, little_car, servo_arm, exact_bracket, surface_textures, chain, chainmail, knurled_knob, voronoi_vase, two_colour_chain, chainmail_sheet, glider, cup_carousel, burr_puzzle, inlaid_dice, twisted_vase, mosaic_coasters, stained_glass, planet_globe, terrain_tile, geode_bookends, wood_box, trefoil_knot, gyroid_lamp, damascus_cup, reef_dome, fractal_sponge, braid_bracelet, chess_set, soft_grip, lattice_sole, voronoi_shade, rally_car, marionette, dune_buggy, roadster, concept_car, lizard_puppet, leopard_vase, turing_lizard, coral_bowl, lichtenberg_vase, scaled_egg, mould_dish, mould_lace, mould_pebble, mould_relief, flow_pebble, flex_dragon, flex_snake, flex_acrobat, poseable_athlete, flex_jellyfish, flex_beetle, flex_flower, ledavr, drum16, drum16_lid, tonebox, handheld);

    /// The slime mould relief: before the host raises its veins it is the bare dish; once they rise
    /// (as the app raises them: the mould grown on the dish, the tubes that print without support
    /// where the dish stands) there are tubes on the floor down to the thinnest, and with supports
    /// on the slicer asks no more support of the dish with them than without: no tube hangs. The
    /// thinnest thread is in the slice of its top layer, and a line prints along it there.
    #[test]
    fn mould_relief_tubes_add_no_support() {
        use makepad_fab_slice::live::{plan_frames_outlines, slice_frames, Frame, PartField};
        use makepad_fab_slicer::geom::{total_area_mm2, Pt};
        use makepad_fab_toolpath::{par, support};
        let d = DEMOS.iter().find(|d| d.key == "mould_relief").expect("the relief demo");
        let mut m = d.model().unwrap_or_else(|e| panic!("{e}"));
        let mut e = fab_model::eval::Evaluator::new(crate::print::CheckSetup::fab_defaults().machine().expect("Fab's default printer"));
        let (c, bare) = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        assert_eq!(bare.plate.parts.len(), 1);
        let (inst, _) = c.object(&bare.plate.parts[0].object).expect("the dish's object");
        let script = inst.script.clone();
        let field = bare.fields[0].clone().expect("the dish is a field");
        let asks = crate::model_host::veins_asks(&script.program.veins_requests, &field, &bare.plate.settings);
        assert_eq!(asks.len(), 1, "the relief asks for its veins");
        let (request, up, lowest) = asks[0].clone();
        assert!((up[1] - 1.0).abs() < 1e-9, "the dish stands on its foot: {up:?}");
        let asked = fab_model::veins::asked(&request, up, lowest);
        assert!(m.ask_solved(&script.file, &request.part, Some(asked)));
        let grid = fab_model::veins::answer(&request, up, lowest, &|| false).expect("the veins rise").expect("the dish has a skin");
        let tubes: Vec<&[f32]> = grid.table.chunks(12).collect();
        assert!(m.set_solved(&script.file, &request.part, Some(asked), Some(grid.clone())));
        let raised = m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}")).1;
        // a demo opens with no warning: the tubes' box is theirs, so the dish keeps its bottom;
        // before they come it says once that they will
        assert!(bare.warnings.len() == 1 && bare.warnings[0].contains("veins rise") && raised.warnings.is_empty(), "before the tubes {:?}, with them {:?}", bare.warnings, raised.warnings);
        // on the floor (2.4 mm up, its tubes' cuts under it): the thinnest, a 0.5 mm half-width
        // and 0.4 mm high, its round 0.5125 mm about a middle 0.1125 mm under the floor
        let floor: Vec<&[f32]> = tubes.iter().copied().filter(|t| t[9] > 0.99 && (t[11] as f64 + fab_paint_field::pattern::veins::BELOW - 2.4).abs() < 0.05).collect();
        assert!(floor.len() > 100, "{} tubes on the floor of {}", floor.len(), tubes.len());
        let thin = (0.25f64 + 0.16) / 0.8;
        let thinnest = floor.iter().copied().filter(|t| (t[3] as f64 - thin).abs() < 1e-3 && (t[7] as f64 - thin).abs() < 1e-3).max_by(|a, b| {
            let l = |t: &[f32]| (0..3).map(|c| (t[4 + c] - t[c]).powi(2)).sum::<f32>();
            l(a).total_cmp(&l(b))
        });
        let thinnest = thinnest.unwrap_or_else(|| panic!("no thread at the thinnest among {} on the floor", floor.len()));
        // what the slicer asks held, with supports on at the plate's overhang angle
        let frames = |ev: &fab_model::eval::Evaluated| -> Vec<Frame> {
            ev.frames.iter().enumerate().map(|(i, f)| {
                let field = ev.fields.get(i).and_then(Option::as_ref).and_then(|pf| PartField::placed(pf.shape.clone(), pf.to_part).map(|f| f.with_bounds(pf.bounds.map(|b| (b.min, b.max)))));
                Frame { local: f.local.clone(), offset: f.offset, field }
            }).collect()
        };
        let setup = |ev: &fab_model::eval::Evaluated| {
            let mut setup = crate::print::slice_bench::setup(ev).job_setup(&ev.plate, &ev.machine).expect("a job");
            for (k, v) in &ev.plate.settings {
                setup.process.values.set_str(k, v);
            }
            setup.process.values.set_str("enable_support", "1");
            setup.process.values.set_str("support_type", "tree(auto)");
            setup
        };
        let ctl = par::Ctl::default();
        let held = |ev: &fab_model::eval::Evaluated| -> f64 {
            let outlines = plan_frames_outlines(ctl, &setup(ev), &ev.plate, &frames(ev), &[]).expect("outlines");
            outlines.iter().map(|(slices, heights, sp)| support::demand(ctl, slices, heights, sp.as_ref().expect("tree supports' settings")).expect("demand").iter().map(|f| total_area_mm2(f)).sum::<f64>()).sum()
        };
        let (without, with) = (held(&bare), held(&raised));
        assert!(with <= without + 1.0, "the tubes ask {with:.1} mm2 held, the bare dish {without:.1}");
        // the thinnest thread's middle on the floor, and 0.3 mm over it (its top layer's middle)
        let r = thin;
        let mid: [f64; 3] = std::array::from_fn(|k| 0.5 * (thinnest[k] + thinnest[4 + k]) as f64 + (r - 0.4) * up[k]);
        let pf = raised.fields[0].as_ref().expect("the relief is a field");
        let at = |m: &[[f64; 4]; 3], p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|k| m[k][0] * p[0] + m[k][1] * p[1] + m[k][2] * p[2] + m[k][3]) };
        let over = |h: f64| std::array::from_fn::<f64, 3, _>(|k| mid[k] + h * up[k]);
        let (part, printer) = (at(&pf.to_part, over(0.3)), at(&pf.to_printer, over(0.3)));
        let outlines = plan_frames_outlines(ctl, &setup(&raised), &raised.plate, &frames(&raised), &[]).expect("outlines");
        let (slices, heights, _) = &outlines[0];
        let layer = heights.iter().position(|&(z, h)| z - h < part[2] && part[2] <= z).unwrap_or_else(|| panic!("no layer at {:.2} mm", part[2]));
        assert!(slices[layer].iter().any(|e| e.contains(Pt::from_mm(part[0], part[1]))), "the thinnest thread at {mid:?} is in its top layer's slice ({:.2} mm)", heights[layer].0);
        let sliced = slice_frames(&setup(&raised), &raised.plate, &frames(&raised), &[], None).expect("it slices");
        let printed = sliced.layers().iter().find(|l| (l.print_z - heights[layer].0).abs() < 1e-6).expect("the layer prints");
        let near = |a: Pt, b: Pt, w: f64| -> bool {
            let (ax, ay, bx, by) = (a.x_mm(), a.y_mm(), b.x_mm(), b.y_mm());
            let (dx, dy) = (bx - ax, by - ay);
            let l = dx * dx + dy * dy;
            let t = if l > 0.0 { (((printer[0] - ax) * dx + (printer[1] - ay) * dy) / l).clamp(0.0, 1.0) } else { 0.0 };
            ((ax + t * dx - printer[0]).powi(2) + (ay + t * dy - printer[1]).powi(2)).sqrt() <= 0.5 * w + 0.05
        };
        let lined = printed.objects.iter().flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills)).any(|p| p.pts.windows(2).any(|w| near(w[0], w[1], p.width)));
        assert!(lined, "a line prints along the thinnest thread at {printer:?} on its top layer ({:.2} mm)", printed.print_z);
    }

    /// `ev` settled on a live engine that sliced nothing before: the slice, and how many slices
    /// (toolpaths) the settle made.
    fn settled(ev: &std::sync::Arc<fab_model::eval::Evaluated>) -> Result<(std::sync::Arc<makepad_fab_slice::live::Settled>, usize), String> {
        let (engine, rx) = crate::live_slice::cpu_engine();
        settled_on(&engine, &rx, ev)
    }

    /// `ev` settled on `engine` (its events on `rx`) as Print settles it: the slice, and how many
    /// slices (toolpaths) the settle made.
    fn settled_on(engine: &makepad_fab_slice::live::LiveEngine, rx: &std::sync::mpsc::Receiver<makepad_fab_slice::live::LiveEvent>, ev: &std::sync::Arc<fab_model::eval::Evaluated>) -> Result<(std::sync::Arc<makepad_fab_slice::live::Settled>, usize), String> {
        use makepad_fab_slice::live::LiveEvent;
        use std::sync::Arc;
        engine.settle(1, crate::live_slice::input_for(crate::live_slice::plate_id(ev), &crate::print::slice_bench::setup(ev), Arc::new(ev.plate.printable()), ev.machine.clone(), Some(&ev.frames), Some(&ev.fields)));
        let mut slices = 0;
        for e in rx.iter() {
            match e {
                LiveEvent::Toolpaths { .. } => slices += 1,
                LiveEvent::Failed { error, .. } => return Err(error),
                LiveEvent::Settled { result, .. } => return result.map(|r| (r, slices)),
                _ => {}
            }
        }
        Err("the engine stopped".into())
    }

    /// The job's tools are the colours that print: a job tool whose colour no path prints in
    /// (a part's own colour its paint covers all of) leaves the job, and one slice makes it.
    fn binds_its_tools(key: &str, r: &makepad_fab_slice::live::Settled, slices: usize) {
        assert_eq!(slices, 1, "{key}: one slice");
        assert_eq!(r.tools.len(), r.setup.filaments.len(), "{key}: a job tool each");
        assert!(r.tools.windows(2).all(|w| w[0] < w[1]), "{key}: {:?}", r.tools);
        let mut used = r.stats.used.clone();
        used.sort_unstable();
        assert_eq!(used, (0..r.tools.len()).collect::<Vec<_>>(), "{key}: every tool of the job prints, job tools {:?}", r.tools);
    }

    /// The jellyfish in two materials, a magenta bell and its aqua cords, is placed in indigo: the
    /// paint covers the indigo all over, so the job is the two that print, one on each nozzle,
    /// with no tower and nothing purged.
    #[test]
    fn the_two_material_jellyfish_prints_two() {
        let mut m = demo("flex_jellyfish").and_then(|d| d.model().ok()).expect("the jellyfish loads");
        let mut e = fab_model::eval::Evaluator::new(crate::print::CheckSetup::fab_defaults().machine().expect("Fab's default printer"));
        m.evaluate(&mut e, None).unwrap_or_else(|f| panic!("{f}"));
        let head = m.head();
        let change = fab_model::Change::Param { instance: "flex_jellyfish".into(), name: "materials".into(), value: Some(2.0) };
        let (_, built) = m.apply(&mut e, "two", head, &[change], None).unwrap_or_else(|f| panic!("{f}"));
        let ev = std::sync::Arc::new(built.expect("rebuilt").1);
        assert_eq!(ev.part_color(0), Some("Indigo"));
        let (r, slices) = settled(&ev).unwrap_or_else(|e| panic!("{e}"));
        binds_its_tools("flex_jellyfish", &r, slices);
        assert_eq!(r.tools, [1, 2], "Magenta and Aqua, not the place colour");
        assert_eq!(r.setup.filament_map, [2, 1], "each on its own nozzle");
        assert_eq!(r.setup.process.values.str("enable_prime_tower"), "0");
        assert_eq!((r.timeline.filament_swaps, r.timeline.purges), (0, 0));
    }

    /// A showpiece (each one's build test runs this on its build), built on Fab's default printer,
    /// fits it (no printability error before it slices) and its joints check free. A flexible
    /// part bonded to a row of parts bends between every two of them (a part may sit in several
    /// rows: a chest the arms branch from), no paint lies over it, and bent each way in turn and
    /// let go, every bend springs back. Held where it is anchored (else by its first part) and
    /// shaken, every other joint moves.
    fn showpiece_checks_free_and_moves(key: &str, ev: &fab_model::eval::Evaluated) {
        use makepad_csg_mech::sim::{AnchorKind, AnchorSpec, Friction, JointKind, Scenario, SimOptions};
        let bend = 10f64.to_radians();
        let errors: Vec<String> = ev.printability.issues.iter().filter(|i| i.severity == fab_printability::Severity::Error).map(|i| format!("{} {:?}: {}", i.kind, i.object, i.message)).collect();
        assert!(errors.is_empty(), "{key}: {errors:?}");
        let mech = ev.mech.first().unwrap_or_else(|| panic!("{key}: no joints or bonds"));
        let bent: Vec<(usize, usize)> = mech.measure_flexes().as_ref().unwrap_or_else(|f| panic!("{key}: {f}")).iter().map(|f| (f.a, f.b)).collect();
        let rows: Vec<(usize, usize)> = mech.plan.bonds.iter().filter(|b| b.flexible).flat_map(|b| b.parts.windows(2).map(|w| (w[0], w[1]))).collect();
        assert_eq!(bent, rows, "{key}: a flex between every two parts on each flexible part, in its row's order");
        for b in mech.plan.bonds.iter().filter(|b| b.flexible) {
            // it prints in its own material through and through: paint over it (any after its
            // whole fill) would print rigid plastic in it
            let spine = &mech.names[b.to];
            let i = ev.plate.parts.iter().position(|p| &p.object == spine).unwrap_or_else(|| panic!("{key}: {spine} is not on the plate"));
            assert!(ev.plate.parts[i].paint.is_none() && ev.paint[i].is_none(), "{key}: paint over the flexible {spine}");
        }
        let checks = mech.check(&|| false);
        let stuck: Vec<String> = checks.iter().filter(|c| !c.ok()).map(|c| format!("joint {}: {c:?}", c.joint)).collect();
        assert!(stuck.is_empty(), "{key}: {stuck:?}");
        let mut asm = mech.assembly(&checks, 1.0).unwrap_or_else(|f| panic!("{key}: {f}"));
        if asm.anchors.is_empty() {
            let first = mech.plan.bonds.first().map_or(0, |b| b.parts[0]);
            asm.anchors.push(AnchorSpec { body: first, pivot: asm.bodies[first].mass.com, kind: AnchorKind::Fixed, friction: Friction::default() });
        }
        let flex: Vec<bool> = asm.joints.iter().map(|j| matches!(j.kind, JointKind::Flex { .. })).collect();
        if flex.iter().any(|f| *f) {
            // every other bend one way, the rest the other, let go on the bed (it bears the
            // weight: no gravity)
            let angles: Vec<f64> = flex.iter().enumerate().map(|(j, f)| if !f { 0.0 } else if j % 2 == 0 { bend } else { -bend }).collect();
            let opts = SimOptions { scenario: Scenario::Pose(angles), seconds: 8.0, gravity: Some([0.0; 3]), ..SimOptions::default() };
            let r = mech.simulate(&asm, &opts, &|| false).unwrap_or_else(|f| panic!("{key}: {f}"));
            let held: Vec<String> = r.joints.iter().zip(&asm.joints).zip(&flex).filter(|((j, _), f)| **f && j.last.abs() > 2f64.to_radians()).map(|((j, s), _)| format!("{} at {:.1}° (about {:.2?})", j.name, j.last.to_degrees(), s.axis)).collect();
            assert!(held.is_empty(), "{key}: bent {:.0}° and let go, these did not spring back: {held:?}", bend.to_degrees());
        }
        if flex.iter().zip(&asm.joints).any(|(f, j)| !f && j.kind != JointKind::Weld) {
            let r = mech.simulate(&asm, &SimOptions { scenario: Scenario::Shake, seconds: 4.0, ..SimOptions::default() }, &|| false).unwrap_or_else(|f| panic!("{key}: {f}"));
            let still: Vec<&str> = r.joints.iter().zip(&flex).filter(|(j, f)| !**f && !j.holds && !j.moved).map(|(j, _)| j.name.as_str()).collect();
            assert!(still.is_empty(), "{key}: shaken, these never moved: {still:?}");
        }
    }

    /// Each of the poseable athlete's parts prints in one piece (its build test runs this on its
    /// build, `e` the evaluator that made `ev`): a fork's plates hold on to their limb (cut loose
    /// from it, the joint comes apart in the hand), and a brake's shoe part is its two shoes.
    fn the_poseable_athletes_parts_are_each_one_piece(ev: &fab_model::eval::Evaluated, e: &mut fab_model::eval::Evaluator) {
        let mut wrong = Vec::new();
        for p in &ev.plate.parts {
            let mesh = e.object_mesh(&p.object).unwrap_or_else(|f| panic!("{f}"));
            let pieces = makepad_csg_cad::measure::shells(&mesh).iter().filter(|s| s.volume.abs() >= 1.0).count();
            let want = if p.object.contains("_shoe") { 2 } else { 1 };
            if pieces != want {
                wrong.push(format!("{}: {pieces} pieces", p.object));
            }
        }
        assert_eq!(ev.plate.parts.len(), 23);
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The poseable athlete holds a pose (its build test runs this on its build's jointed piece):
    /// held by its chest under gravity, every joint turned 40 degrees (past where its brake grips)
    /// and let go stays within two degrees; with the brakes taken off the same pose falls.
    fn the_poseable_athlete_holds_a_pose(mech: &fab_model::mech::Mech) {
        use makepad_csg_mech::sim::{Scenario, SimOptions};
        let checks = mech.check(&|| false);
        let asm = mech.assembly(&checks, 1.0).unwrap_or_else(|f| panic!("{f}"));
        let braked: Vec<usize> = (0..asm.joints.len()).filter(|j| asm.joints[*j].brake.is_some()).collect();
        assert_eq!(braked.len(), 11, "neck, shoulders, elbows, hips, knees and ankles");
        let turn = 40f64.to_radians();
        // each way it turns far enough
        let pose: Vec<f64> = (0..asm.joints.len())
            .map(|j| match checks.iter().find(|c| c.joint == j) {
                Some(c) if braked.contains(&j) => if c.effective.1 > turn + 0.1 { turn } else { assert!(-c.effective.0 > turn + 0.1, "{}: turns {:?}", asm.joints[j].name, c.effective); -turn },
                _ => 0.0,
            })
            .collect();
        let posed = |asm: &makepad_csg_mech::sim::Assembly| mech.simulate(asm, &SimOptions { scenario: Scenario::Pose(pose.clone()), seconds: 4.0, ..SimOptions::default() }, &|| false).unwrap_or_else(|f| panic!("{f}"));
        let r = posed(&asm);
        let moved: Vec<String> = braked.iter().filter(|j| (r.joints[**j].last - pose[**j]).abs() > 2f64.to_radians()).map(|j| format!("{} {:.1}° from {:.0}°", r.joints[*j].name, r.joints[*j].last.to_degrees(), pose[*j].to_degrees())).collect();
        assert!(moved.is_empty(), "posed and let go, these did not hold: {moved:?}");
        let mut free = asm.clone();
        for j in &mut free.joints {
            j.brake = None;
        }
        let f = posed(&free);
        let fell = braked.iter().filter(|j| (f.joints[**j].last - pose[**j]).abs() > 10f64.to_radians()).count();
        assert!(fell >= 4, "without the brakes the pose falls: only {fell} joints moved");
    }

    /// Every demo has its test, once (a key listed twice is a test defined twice), in the list's
    /// order.
    #[test]
    fn every_demo_has_its_test() {
        assert_eq!(DEMOS.iter().map(|d| d.key).collect::<Vec<_>>(), TESTED, "a demo's key goes in `demo_tests!` too");
    }

    /// Why: a demo opened by its path did not build (11-18) where its folder held none of its
    /// electronics documents, which its case script's `board(...)` reads. A saved demo carries
    /// them (its `.elec` folder): each electronics demo shown as the Demos list shows it, saved as
    /// File > Save does and opened by that path in a fresh session as File > Open does, builds
    /// with the demo's parts.
    #[test]
    fn every_electronics_demo_saved_and_opened_by_its_path_builds() {
        let root = std::env::temp_dir().join(format!("fab-demo-reopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = || {
            let mut s = fab_ai::Session::new(crate::print::CheckSetup::fab_defaults().machine().expect("Fab's default printer"), root.join("Documents"));
            s.model.set_env(crate::elec_host::shared_env());
            s.env_for = Some(Arc::new(crate::elec_host::doc_env));
            s
        };
        for &(key, _) in ELECTRONICS {
            let d = demo(key).unwrap_or_else(|| panic!("{key}: not a demo"));
            let mut shown = session();
            let parts = shown.open_demo(d.model().unwrap_or_else(|e| panic!("{e}")), key).unwrap_or_else(|e| panic!("{key}: the demo shows: {e}")).plate.parts.len();
            let path = shown.save(Some(root.join(key).join(format!("{key}.{}", fab_model::project::EXTENSION)))).unwrap_or_else(|e| panic!("{key}: save: {e}"));
            let ev = session().open(&path).unwrap_or_else(|e| panic!("{key}: the saved demo opened by its path: {e}"));
            assert_eq!(ev.plate.parts.len(), parts, "{key}: the reopened plate has the demo's parts");
        }
        std::fs::remove_dir_all(&root).expect("remove the saved demos");
    }
}
