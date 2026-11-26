//! Plan authored board-header edits against the selected board's current settings.
use crate::edit_actions::{arg, board_unit, error, Args};
use crate::{Docs, Plan};
use fab_elec_board::{Board, BoardOp, Finish};
use fab_elec_core::{geom::NM_PER_MM, Diag, Nm};
use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk};
use fab_elec_design::{Design, DesignOp, Resolved};

const fn field(name: &'static str, kind: FieldKind, help: &'static str) -> FieldDef {
    FieldDef { name, kind, help, required: false, default: "" }
}
const BOARD: FieldDef = field("board", FieldKind::Text, "Board unit name. Omit when the design has one board.");
const MM: FieldKind = FieldKind::Num { unit: "mm" };
const RETURNS: &str = "seq: int  the step it committed\nlabel: text  its name in the undo history";

const STACKUP: ActionDef = ActionDef {
    name: "elec.stackup.set", label: "Set board stackup", group: "Board",
    help: "Edit the board's stackup statement. Supply one or more settings to change together in one undo step.",
    fields: &[
        BOARD, field("thickness", MM, "Finished board thickness in mm."),
        field("finish", FieldKind::Text, "Surface finish: none, hasl, lead_free_hasl, enig, osp, immersion_silver or immersion_tin. Names are case-insensitive."),
        field("mask", FieldKind::Text, "Solder mask colour as #rrggbb or #rrggbbaa."),
        field("silk", FieldKind::Text, "Silkscreen colour as #rrggbb or #rrggbbaa."),
    ],
    returns: RETURNS, inputs: &[], risk: Risk::Edit, keys: "",
};

const RULES: ActionDef = ActionDef {
    name: "elec.rules.set", label: "Set base design rules", group: "Board",
    help: "Edit one or more minima in the board's rules statement as one undo step. The board can specify a size below its factory's capability.",
    fields: &[
        BOARD, field("track", MM, "Minimum track width in mm."),
        field("clearance", MM, "Minimum copper clearance in mm."),
        field("drill", MM, "Minimum drill diameter in mm."),
        field("annular", MM, "Minimum annular ring in mm."),
    ],
    returns: RETURNS, inputs: &[], risk: Risk::Edit, keys: "",
};
pub(crate) static ACTIONS: &[ActionDef] = &[STACKUP, RULES];

fn positive_mm(key: &str, text: &str) -> Result<Nm, Diag> {
    let text = text.trim();
    let number = text.strip_suffix("mm").unwrap_or(text).trim_end();
    let mm: f64 = number.parse().map_err(|_| error(key, "expected a positive length in mm"))?;
    let nm = (mm * NM_PER_MM).round();
    if !(1.0..=f64::from(Nm::MAX)).contains(&nm) {
        return Err(error(key, "expected a positive length representable in board nanometres"));
    }
    Ok(nm as Nm)
}

fn finish(text: &str) -> Result<Finish, Diag> {
    // Accept the board language's tag spellings and names such as "Lead-free HASL".
    let text = text.trim();
    let name = text.strip_prefix('@').unwrap_or(text).to_ascii_lowercase().replace([' ', '-'], "_");
    match name.as_str() {
        "none" => Ok(Finish::None),
        "hasl" => Ok(Finish::Hasl),
        "lead_free_hasl" | "leadfreehasl" => Ok(Finish::LeadFreeHasl),
        "enig" => Ok(Finish::Enig),
        "osp" => Ok(Finish::Osp),
        "immersion_silver" | "immersionsilver" => Ok(Finish::ImmersionSilver),
        "immersion_tin" | "immersiontin" => Ok(Finish::ImmersionTin),
        _ => Err(error("finish", "expected none, hasl, lead_free_hasl, enig, osp, immersion_silver or immersion_tin")),
    }
}

fn colour(key: &str, text: &str) -> Result<u32, Diag> {
    let invalid = || error(key, "expected #rrggbb or #rrggbbaa");
    let digits = text.trim().strip_prefix('#').ok_or_else(invalid)?;
    if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let value = u32::from_str_radix(digits, 16).map_err(|_| invalid())?;
    Ok(if digits.len() == 6 { (value << 8) | 0xff } else { value })
}

fn update<T: PartialEq>(slot: &mut T, value: T, key: &str, text: &str, changed: &mut Vec<String>) {
    if *slot != value {
        *slot = value;
        changed.push(format!("{key} {}", text.trim()));
    }
}

fn selected<'a>(design: &'a Design, def: &ActionDef, args: &Args) -> Result<&'a Board, Diag> {
    def.check_args(args)?;
    let unit = board_unit(design, "board", arg(args, "board"))?;
    if args.iter().all(|(key, _)| key == "board") {
        return Err(error(def.name, "supply at least one setting to change"));
    }
    Ok(unit.board.as_deref().expect("board_unit selected a board"))
}

fn edit(board: &Board, settings: &str, changes: Vec<String>, op: BoardOp) -> Result<Resolved, Diag> {
    if changes.is_empty() {
        return Err(error(settings, format_args!("{} already has the requested settings", board.name)));
    }
    Ok(Resolved {
        ops: vec![DesignOp::Board { unit: board.unit, op }], place: vec![],
        label: format!("Set {} {settings} ({})", board.name, changes.join(", ")),
    })
}

/// Plan one stackup edit from the fields supplied by the form or catalog.
pub fn plan_stackup(design: &Design, args: &[(String, String)]) -> Result<Resolved, Diag> {
    let board = selected(design, &STACKUP, args)?;
    let mut stack = board.stackup.clone();
    let mut changes = Vec::new();
    if let Some(text) = arg(args, "thickness") {
        update(&mut stack.thickness, positive_mm("thickness", text)?, "thickness", text, &mut changes);
    }
    if let Some(text) = arg(args, "finish") {
        update(&mut stack.finish, finish(text)?, "finish", text, &mut changes);
    }
    for (key, slot) in [("mask", &mut stack.mask_color), ("silk", &mut stack.silk_color)] {
        if let Some(text) = arg(args, key) {
            update(slot, colour(key, text)?, key, text, &mut changes);
        }
    }
    edit(board, "stackup", changes, BoardOp::SetStackup(stack))
}

/// Plan one base-rule edit. Start with the authored rules, before factory minima are applied.
pub fn plan_rules(design: &Design, args: &[(String, String)]) -> Result<Resolved, Diag> {
    let board = selected(design, &RULES, args)?;
    let mut rules = board.rules.clone();
    let mut changes = Vec::new();
    let base = &mut rules.base;
    for (key, slot) in [("track", &mut base.track), ("clearance", &mut base.clearance),
                        ("drill", &mut base.drill), ("annular", &mut base.annular)] {
        if let Some(text) = arg(args, key) {
            update(slot, Some(positive_mm(key, text)?), key, text, &mut changes);
        }
    }
    edit(board, "base rules", changes, BoardOp::SetRules(rules))
}

pub(crate) fn resolve(design: &Design, _docs: Option<&Docs>, tool: &str, args: &Args) -> Result<Plan, Diag> {
    match tool {
        "elec.stackup.set" => plan_stackup(design, args),
        "elec.rules.set" => plan_rules(design, args),
        _ => Err(error("tool", format_args!("unknown {tool}"))),
    }.map(Plan::Edit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cmd, Engine, Mailbox, Outcome, Ticket};
    use fab_elec_design::By;
    use fab_elec_lang::{evaluate, std_libs, Sources};
    use std::sync::{atomic::AtomicU64, Arc};

    fn args(fields: &[(&str, &str)]) -> Vec<(String, String)> {
        fields.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect()
    }

    #[test]
    fn catalog_settings_validate_the_entire_edit() {
        crate::edit_actions::register_actions();
        let mut design = fab_elec_fixtures::ledctl();
        for action in ACTIONS {
            assert_eq!(fab_elec_design::action::find(action.name), Some(action));
            for fields in [vec![], vec![("board", "ledctl")]] {
                let Err(why) = resolve(&design, None, action.name, &args(&fields)) else { panic!("empty edit") };
                assert!(why.message.contains("at least one"));
            }
        }
        for (tool, fields, reason) in [
            (STACKUP, vec![("thickness", "1.6mm")], "already"),
            (STACKUP, vec![("thickness", "0")], "positive"),
            (STACKUP, vec![("thickness", "-1mm")], "positive"),
            (STACKUP, vec![("thickness", "3000mm")], "representable"),
            (STACKUP, vec![("thickness", "1.6oz")], "thickness"),
            (STACKUP, vec![("finish", "bronze")], "none, hasl, lead_free_hasl, enig, osp, immersion_silver or immersion_tin"),
            (STACKUP, vec![("mask", "#00gg00")], "#rrggbb"),
            (STACKUP, vec![("silk", "aabbccdd")], "#rrggbbaa"),
            (RULES, vec![("track", "0")], "positive"),
            (RULES, vec![("clearance", "-.01")], "positive"),
            (RULES, vec![("track", ".02"), ("drill", "1oz")], "drill"),
            (RULES, vec![("annular", "0.0000001")], "positive"),
            (RULES, vec![("board", "missing"), ("drill", ".1")], "no unit"),
        ] {
            let Err(why) = resolve(&design, None, tool.name, &args(&fields)) else { panic!("accepted {fields:?}") };
            assert!(why.message.contains(reason), "{fields:?}: {}", why.message);
        }
        // An omitted minimum inherits the standard 0.15 mm; writing 0.15 is a real edit that pins it.
        assert!(resolve(&design, None, RULES.name, &args(&[("track", "0.15")])).is_ok(), "a written value equal to the standard is an edit");
        let original = design.units[0].board.as_deref().unwrap();
        let plan = plan_stackup(&design, &args(&[("finish", "@Immersion_Silver"), ("thickness", "1.6")])).unwrap();
        let mut expected = original.stackup.clone();
        expected.finish = Finish::ImmersionSilver;
        assert!(matches!(&plan.ops[..], [DesignOp::Board { op: BoardOp::SetStackup(s), .. }] if *s == expected));
        assert!(plan.label.contains("finish") && !plan.label.contains("thickness"));
        // Imported boards can already carry compiled rule and check identities.
        let rules = &mut Arc::make_mut(design.units[0].board.as_mut().unwrap()).rules;
        let code = fab_elec_board::RuleCode { id: 1, hash: fab_elec_core::ContentHash([1; 16]), at: (0, 15) };
        rules.custom.push(fab_elec_board::CustomRule {
            name: "spacing".into(), when: code.clone(), set: Default::default(), layers: None, severity: Default::default(),
        });
        rules.checks.push(fab_elec_board::CustomCheck { name: "shape".into(), code, severity: Default::default() });
        let mut expected = rules.clone();
        expected.base.annular = Some(1);
        let plan = plan_rules(&design, &args(&[("annular", "0.000001mm")])).unwrap();
        assert!(matches!(&plan.ops[..], [DesignOp::Board { op: BoardOp::SetRules(r), .. }] if *r == expected));
    }

    #[test]
    fn ledctl_settings_splice_one_header_and_undo_one_step() {
        let pcb = "ledctl.pcb.splash";
        let stock = fab_elec_fixtures::docs("ledctl").unwrap();
        let mut sources = Sources::new();
        for doc in stock.docs() {
            let text = if doc.name == pcb {
                doc.text.replacen("board(\"ledctl\")", concat!(
                    "board(\"ledctl\")\nfactory(\"standard 2-layer\")\n",
                    "rules({edge: 0.4mm})\n",
                    "rule(\"spacing\", {when: fn(a,b) { true } set: {clearance: 0.4mm}})\n",
                    "check(\"shape\",  fn(b) { true })\n",
                    "waive(\"0000000000000011\",   \"reviewed\")"
                ), 1)
            } else { doc.text.to_string() };
            sources.add(&doc.name, text).unwrap();
        }
        let doc = sources.find(pcb).unwrap();
        let original = sources.doc(doc).unwrap().text.clone();
        let evaluated = evaluate(&sources, &std_libs()).unwrap();
        let (design, docs) = Docs::from_evaluated(sources, evaluated);
        let before = design.units[0].board.clone().unwrap();
        assert_eq!(before.rules.waivers.len(), 1);
        let factories = fab_elec_cam::profile::builtin_factories();
        let factory = design.units[0].factory.as_deref().unwrap();
        assert!(factories[factory].min_track > 50_000);
        let mut engine = Engine::new(design, Arc::new(factories), Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)), "settings".into()).with_docs(docs).with_app_tools();
        let mailbox = Mailbox::default();
        for (action, fields, header) in [
            (STACKUP, vec![("thickness", "0.812345mm"), ("finish", "eNiG"), ("mask", "#12ACef"), ("silk", "#A0b1c280")], "stackup("),
            (RULES, vec![("board", "ledctl"), ("track", ".05"), ("clearance", ".08mm"), ("drill", ".2"), ("annular", ".06")], "rules("),
        ] {
            mailbox.push(Cmd::Invoke { tool: action.name.into(), args: args(&fields), preview: false, by: By::Person, ticket: Ticket(1) });
            let step = engine.step(mailbox.take(), &mut |_| {});
            match &step.outcomes[..] {
                [(Ticket(1), Outcome::Done { .. })] => {}
                [(_, Outcome::Refused(why))] => panic!("{}: {}", action.name, why.text),
                _ => panic!("expected one completed {}", action.name),
            }
            assert_eq!(engine.log().head, 1);
            let entry = &engine.log().entries[0];
            assert_eq!(entry.ops.len(), 1);
            let [edit] = &entry.text.edits[..] else { panic!("one header splice expected") };
            assert_eq!(edit.doc, doc);
            for (key, _) in &fields {
                if *key != "board" { assert!(entry.label.contains(key), "missing {key} in {}", entry.label); }
            }
            let board = engine.design().units[0].board.as_deref().unwrap();
            let mut expected_stack = before.stackup.clone();
            let mut expected_rules = before.rules.clone();
            if action.name == STACKUP.name {
                expected_stack.thickness = 812_345;
                expected_stack.finish = Finish::Enig;
                expected_stack.mask_color = 0x12ac_efff;
                expected_stack.silk_color = 0xa0b1_c280;
            } else {
                expected_rules.base.track = Some(50_000);
                expected_rules.base.clearance = Some(80_000);
                expected_rules.base.drill = Some(200_000);
                expected_rules.base.annular = Some(60_000);
            }
            assert_eq!(board.stackup, expected_stack);
            assert_eq!(board.rules, expected_rules);
            let docs = engine.docs().unwrap();
            let written = docs.text(doc).unwrap().clone();
            assert_eq!(written.lines().filter(|line| line.starts_with(header)).count(), 1);
            let without_header = |text: &str| text.split_inclusive('\n').filter(|line| !line.starts_with(header)).collect::<String>();
            assert_eq!(without_header(&written), without_header(&original));
            let reread = evaluate(docs.sources(), docs.libs()).unwrap();
            let read_board = reread.design.units[0].board.as_deref().unwrap();
            assert_eq!(read_board.stackup, expected_stack);
            assert_eq!(read_board.rules.base, expected_rules.base);
            assert_eq!(read_board.rules, expected_rules);

            mailbox.push(Cmd::Undo);
            engine.step(mailbox.take(), &mut |_| {});
            assert_eq!(engine.log().head, 0);
            assert_eq!(engine.docs().unwrap().text(doc).unwrap(), &original);
            let restored = engine.design().units[0].board.as_deref().unwrap();
            assert_eq!(restored.stackup, before.stackup);
            assert_eq!(restored.rules, before.rules);
        }
    }
}
