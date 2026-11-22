use fab_elec_parts::PartKind;
use fab_elec_view::instruments::panel_model::{panel_model, ControlKind};

#[test]
fn controls_follow_the_circuit_and_the_physical_board() {
    let libs = fab_elec_lang::std_libs();
    for (name, switches, pots) in [("drum16", 20, 8), ("handheld", 2, 0)] {
        let sources = fab_elec_fixtures::docs(name).expect("the fixture supplies its board sources");
        let ev = fab_elec_lang::evaluate(&sources, &libs).unwrap_or_else(|d| panic!("{name} evaluates: {d:?}"));
        let unit = ev.design.unit_by_name(name).expect("the fixture has its named board unit");
        let panel = panel_model(unit);
        let controls: Vec<_> = panel.groups.iter().flat_map(|group| &group.controls).collect();
        let mut ids: Vec<_> = controls.iter().map(|c| c.comp).collect();
        ids.sort_unstable();
        let expected: Vec<_> = unit.circuit.comps.iter()
            .filter(|c| !c.dnp && matches!(c.part.kind, PartKind::Switch | PartKind::Potentiometer))
            .map(|c| c.id).collect();
        assert_eq!(ids, expected, "{name} has exactly one control per populated switch or pot");
        assert_eq!(controls.iter().filter(|c| matches!(c.kind, ControlKind::Key | ControlKind::Switch)).count(),
            switches, "{name} has its full set of switch controls");
        assert_eq!(controls.iter().filter(|c| matches!(c.kind, ControlKind::Knob | ControlKind::Slider)).count(),
            pots, "{name} has its full set of pot controls");
        assert!(panel.groups.iter().all(|g| !g.controls.is_empty()), "{name} has no empty panel groups");
        for c in &controls {
            let part = &unit.circuit.comps[c.comp.ix()];
            assert!(!c.label.trim().is_empty(), "{name} gives {} a visible label", part.refdes);
            let label = c.label.to_ascii_lowercase();
            assert!(["drum16", "handheld"].iter().all(|board| !label.contains(*board)),
                "{name} labels {} without a fixture name", part.refdes);
        }

        if name == "handheld" {
            for path in ["boot_button", "reset_button"] {
                let switch = unit.circuit.comp(path).expect("the handheld has its named button");
                let button = controls.iter().find(|c| c.comp == switch.id).expect("the handheld button appears in the panel");
                assert!(button.kind == ControlKind::Key && button.label == switch.refdes,
                    "the handheld shows {path} as a momentary key labelled by its reference");
                assert_eq!(button.glow, None, "the handheld has no LED paired with {path}");
            }
            assert!(panel.indicators.is_empty(), "the handheld has no standalone LEDs");
            continue;
        }

        let key = unit.circuit.comp("keys.keys[0][0].sw").expect("the key matrix has its first switch");
        let grid = panel.groups.iter().find(|g| g.controls.iter().any(|c| c.comp == key.id))
            .expect("the first switch identifies the physical key group");
        assert_eq!((grid.rows, grid.cols), (2, 10), "the physical key field has two rows of ten");
        assert_eq!(grid.controls.len(), switches, "all twenty key cells share the same group");
        let board = unit.board.as_ref().expect("the drum fixture has a PCB");
        let keys: Vec<_> = board.placed.iter().filter(|p| unit.circuit.comps[p.comp.ix()].part.kind == PartKind::Switch).collect();
        let mut xs: Vec<_> = keys.iter().map(|p| p.at.x).collect();
        xs.sort_unstable();
        xs.dedup();
        let mut ys: Vec<_> = keys.iter().map(|p| p.at.y).collect();
        ys.sort_unstable_by(|a, b| b.cmp(a));
        ys.dedup();
        assert_eq!(keys.len(), switches, "the board places every switch");
        for i in 0..switches {
            let key_path = format!("keys.keys[{}][{}].sw", i / 4, i % 4);
            let p = keys.iter().find(|p| p.path == key_path).expect("the board places this matrix key");
            let c = grid.controls.iter().find(|c| c.comp == p.comp).expect("each placed switch has a grid cell");
            assert_eq!((ys[c.row as usize], xs[c.col as usize]), (p.at.y, p.at.x),
                "{} occupies its PCB position viewed from above", p.refdes);
            let led_path = if i < 16 { format!("chain.steps[{i}].d") } else { format!("chain.funcs[{}].d", i - 16) };
            let led = unit.circuit.comp(&led_path).expect("the chain has this key's LED");
            assert_eq!(c.glow, Some(led.id), "{key_path} glows with its own LED {led_path}");
        }
        assert!(panel.indicators.is_empty(), "all twenty drum LEDs belong to the key grid");
    }
}
