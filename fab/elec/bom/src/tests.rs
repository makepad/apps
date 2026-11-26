use super::*;
use fab_elec_cam::{bom_export::build_table, profile::ResolvedProfile, tables::{bom_from_circuit, fill_generic_tables, PlacementRules}};
use fab_elec_circuit::CircuitOp;
use fab_elec_design::DesignOp;
use fab_elec_lang::{evaluate, splice, std_libs, Sources};

#[test]
fn ledctl_sheet_identity_and_fabrication() {
    let mut sources = Sources::new();
    for doc in fab_elec_fixtures::docs("ledctl").unwrap().docs() {
        let text = doc.text.replace("volts: 10V}", "volts: 10V ref: \"Cap.Pair\"}");
        sources.add(&doc.name, text).unwrap();
    }
    let libs = std_libs();
    let evaluated = evaluate(&sources, &libs).unwrap();
    let design = evaluated.design;
    let unit = &design.units[0];
    let id = unit.id;
    let view = SheetView::default();
    let sheet = sheet(&design, id, &view).unwrap();
    let table = build_table(&bom_from_circuit(&unit.circuit, unit.board.as_deref()), &view.options(), "");
    assert_eq!(sheet.header, table.header);
    assert_eq!(sheet.rows.len(), table.rows.len());
    for (row, cam) in sheet.rows.iter().zip(table.rows) {
        assert_eq!((&row.cells, &row.refs), (&cam.cells, &cam.refs));
        let refs: Vec<_> = row.comps.iter().map(|c| &unit.circuit.comps[c.ix()].refdes).collect();
        assert_eq!(refs, row.refs.iter().collect::<Vec<_>>(), "component identities match CAM membership");
    }
    assert!(sheet.rows.iter().any(|r| r.checks.contains("no MPN")));
    let facts = BoardFacts::from_board(unit.board.as_ref().unwrap());
    assert_eq!((facts.bounds.max.x - facts.bounds.min.x, facts.bounds.max.y - facts.bounds.min.y, facts.layers),
        (50_000_000, 30_000_000, 2));
    assert_eq!(facts.area2_nm2, 3000 * 1_000_000_000_000);
    let rates = Rates::example();
    assert_eq!(Rates::from_json(&rates.to_json()).unwrap(), rates);
    let estimate = estimate_board(&facts, &rates, 10);
    let run = estimate.selected.unwrap();
    assert_eq!(run.surcharges.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["Small drill"]);
    assert_eq!((run.totals[0].micros, run.per_board[0].micros), (23_000_000, 2_300_000));
    assert_eq!(estimate.comparisons.len(), 2);
    let mut mixed = rates.clone();
    mixed.setup.currency = "EUR".into();
    let amounts = estimate_board(&facts, &mixed, 10).selected.unwrap().totals;
    assert_eq!(amounts.iter().map(|v| (v.currency.as_str(), v.micros)).collect::<Vec<_>>(),
        [("EUR", 10_000_000), ("USD", 13_000_000)], "currencies are subtotalled separately");
    mixed.prices.clear();
    assert!(estimate_board(&facts, &mixed, 10).selected.is_err(), "no matching amount stays unavailable");

    let circuit = &design.units[0].circuit;
    let a = circuit.comp("reg.c_in").unwrap().id;
    let b = circuit.comp("reg.c_out").unwrap().id;
    assert_eq!(circuit.comps[a.ix()].refdes, circuit.comps[b.ix()].refdes);
    let reference = circuit.comps[a.ix()].refdes.clone();
    let (ap, bp) = (circuit.comps[a.ix()].path.clone(), circuit.comps[b.ix()].path.clone());
    let mut view = SheetView::default();
    view.cam.group_symbols = false;
    let duplicate = super::sheet(&design, id, &view).unwrap();
    let rows: Vec<_> = duplicate.rows.iter().filter(|r| r.refs == [reference.as_str()]).collect();
    assert_eq!(rows.len(), 2, "duplicate display refs survive grouping");
    assert_ne!(rows[0].key, rows[1].key);
    for row in rows {
        assert_eq!(row.paths, [design.units[0].circuit.comps[row.comps[0].ix()].path.as_str()]);
    }
    let mut profile = ResolvedProfile::jlc_2_layer("ledctl", "A");
    let board = unit.board.as_deref().unwrap();
    fill_generic_tables(&mut profile, board, circuit, &PlacementRules::jlc());
    let pair = profile.bom.rows.iter().find(|r| r.refs == [reference.as_str(), reference.as_str()]).unwrap();
    assert_eq!(pair.cells[1], format!("{reference},{reference}"), "factory CSV enumerates both physical parts");

    // The factory's legacy supplier field also separates the live sheet's order lines.
    let mut sourced = design.clone();
    let components = std::sync::Arc::make_mut(&mut sourced.units[0].circuit);
    components.comps[a.ix()].fields.0.push(("lcsc".into(), "C123".into()));
    fill_generic_tables(&mut profile, board, components, &PlacementRules::jlc());
    let fitted = super::sheet(&sourced, id, &SheetView::default()).unwrap();
    assert_eq!(fitted.rows.iter().filter(|r| r.refs == [reference.as_str()]).count(), 2);
    assert_eq!(fitted.rows.iter().map(|r| &r.refs).collect::<Vec<_>>(),
        profile.bom.rows.iter().map(|r| &r.refs).collect::<Vec<_>>(), "tab and factory agree on every group's membership");

    // Supplier part numbers are local to a supplier. Use distinct refs so both tables identify each member.
    let mut offers = design.clone();
    let components = std::sync::Arc::make_mut(&mut offers.units[0].circuit);
    for (id, refdes) in [(a, "C1"), (b, "C2")] {
        let comp = &mut components.comps[id.ix()];
        comp.refdes = refdes.into();
        comp.fields.0.push(("supplier_part".into(), "SKU-1".into()));
    }
    fill_generic_tables(&mut profile, board, components, &PlacementRules::jlc());
    let original_bom = profile.bom.clone();
    for (first_supplier, second_supplier, together) in [
        ("Supplier A", "Supplier A", true), ("Supplier A", "Supplier B", false),
        ("LcSc", "LCSC", true), (" LcSc ", "LCSC", true),
    ] {
        let mut ordered = offers.clone();
        let components = std::sync::Arc::make_mut(&mut ordered.units[0].circuit);
        for (id, supplier) in [(a, first_supplier), (b, second_supplier)] {
            components.comps[id.ix()].fields.0.push(("supplier".into(), supplier.into()));
        }
        fill_generic_tables(&mut profile, board, components, &PlacementRules::jlc());
        let tab = super::sheet(&ordered, id, &SheetView::default()).unwrap();
        assert_eq!(tab.rows.iter().map(|r| &r.refs).collect::<Vec<_>>(),
            profile.bom.rows.iter().map(|r| &r.refs).collect::<Vec<_>>());
        if together {
            let pair = tab.rows.iter().find(|r| r.refs == ["C1", "C2"]).expect("one purchasing line for one supplier");
            if first_supplier == second_supplier {
                assert_eq!(profile.bom, original_bom, "one shared supplier preserves every exported cell and row");
            } else {
                assert_eq!(pair.cells[5], format!("{first_supplier},{second_supplier}"), "visible supplier spellings are preserved");
                let mut exact = SheetView::default();
                for field in &mut exact.cam.fields { field.group_by = field.name == "supplier"; }
                let raw = super::sheet(&ordered, id, &exact).unwrap();
                assert!(raw.rows.iter().any(|r| r.refs == ["C1"]) && raw.rows.iter().any(|r| r.refs == ["C2"]),
                    "grouping on the authored supplier field still compares exact strings");
            }
        } else {
            for reference in ["C1", "C2"] {
                assert!(tab.rows.iter().any(|r| r.refs == [reference]), "{reference} has its own order line");
            }
        }
    }

    // A fitted part excluded from ordering still has a placement row.
    let components = std::sync::Arc::make_mut(&mut sourced.units[0].circuit);
    let header = components.comps.iter_mut().find(|c| c.refdes == "J1").unwrap();
    header.fields.0.push(("exclude_from_bom".into(), "true".into()));
    profile.files.retain(|file| file.kind == fab_elec_cam::profile::FileKind::Bom);
    fill_generic_tables(&mut profile, board, components, &PlacementRules::jlc());
    assert!(!profile.bom.rows.iter().flat_map(|r| &r.refs).any(|r| r == "J1"));
    assert!(profile.placement.rows.iter().any(|r| r.refdes == "J1"));
    let export = fab_elec_cam::export::export(board, components, &[], &profile).unwrap();
    assert!(export.validation.ok(), "excluded BOM member must validate: {:?}", export.validation.failures().collect::<Vec<_>>());

    let resolve = |selector| sheet::component(&design.units[0].circuit, selector).map(|c| c.id);
    let ambiguity = resolve(&reference).unwrap_err();
    assert!(ambiguity.contains(&ap) && ambiguity.contains(&bp), "{ambiguity}");
    let target = resolve(&bp).unwrap();
    let before = design.units[0].circuit.comps[a.ix()].fields.clone();
    let change = splice(&sources, &evaluated.map, &design, &[DesignOp::Circuit { unit: id, op: CircuitOp::SetField {
        comp: target, key: "mpn".into(), text: Some("sample".into()),
    }}]).unwrap();
    sources.apply(&change.edits).unwrap();
    let design = evaluate(&sources, &libs).unwrap().design;
    assert_eq!(design.units[0].circuit.comps[b.ix()].fields.get("mpn"), Some("sample"));
    assert_eq!(design.units[0].circuit.comps[a.ix()].fields, before);
}

#[test]
fn order_estimate_uses_actual_offers_and_counts_assembly_once() {
    use crate::cost::{estimate, Options, Placements};
    use crate::prices::{Entry, Prices};
    use crate::rates::{Attrition, Billing};
    use fab_elec_core::Side;
    use std::{collections::BTreeSet, sync::Arc};

    let mut design = fab_elec_fixtures::ledctl_eval().design;
    let unit = &mut design.units[0];
    let facts = BoardFacts::from_board(unit.board.as_ref().unwrap());
    let ckt = Arc::make_mut(&mut unit.circuit);
    let ids: Vec<_> = ["reg.c_in","reg.c_out"].map(|path| ckt.comp(path).unwrap().id).into();
    for (i,id) in ids.iter().enumerate() {
        ckt.comps[id.ix()].fields.0.extend([("mpn".into(),"CAP".into()),("supplier".into(),"Source A".into()),
            ("supplier_part".into(),format!("A-{i}")),("assembly_class".into(),"basic".into())]);
    }
    let board = unit.id;
    let mut view = SheetView::default();
    view.only = Some(BTreeSet::from(["reg.c_in".into(),"reg.c_out".into()]));
    let grouped = super::sheet(&design,board,&view).unwrap();
    assert_eq!(grouped.rows.len(),2);

    let entry = |key:&str,currency:&str,offer:&str| {
        let json = format!(r#"{{{key},"currency":"{currency}","source":"local receipt","date":"2026-11-25",{offer}}}"#);
        Entry::from_json(&makepad_strict_json::parse(json.as_bytes()).unwrap()).unwrap()
    };
    let mut prices = Prices::default();
    for e in [
        entry(r#""mpn":"CAP""#,"USD",r#""breaks":[{"min":1,"micros":1000000}]"#),
        entry(r#""supplier":"sOuRcE A","supplier_part":"A-0""#,"USD",r#""breaks":[{"min":1,"micros":800000}]"#),
    ] {
        assert_eq!(Entry::from_json(&e.json()).unwrap(),e); prices.0.insert(e.key.clone(),e);
    }
    let mut lookup = crate::prices::Sourcing { supplier: "  SOURCE A  ".into(), supplier_part: "A-0".into(), ..Default::default() };
    let offer = prices.resolve(&lookup).unwrap();
    assert_eq!(offer.key.label(), "sOuRcE A · A-0");
    assert_eq!(offer.json().get("supplier").unwrap().as_str(), Some("sOuRcE A"));
    lookup.supplier_part = "a-0".into();
    assert!(prices.resolve(&lookup).is_none(), "the SKU remains case-sensitive");
    let mut rates = Rates::example();
    let places:Placements = [("reg.c_in".into(),(Side::Top,2)),("reg.c_out".into(),(Side::Bottom,2))].into();
    let hand = Options { hand:true,..Default::default() };
    let hand_run = estimate(&grouped,&facts,&places,&prices,&rates,10,&hand).selected.unwrap();
    let part = |r:&crate::cost::CostRun,n:usize| r.lines[n].fitted.as_ref().unwrap().unit.as_ref().unwrap().micros;
    assert_eq!((part(&hand_run,0),part(&hand_run,1)),(800_000,1_000_000));
    assert_eq!(hand_run.parts[0].micros,18_000_000,"ten at USD 0.80 from the selected supplier and ten at USD 1.00 by MPN");
    let euro = entry(r#""mpn":"CAP""#,"EUR",r#""breaks":[{"min":1,"micros":1000000}]"#);
    prices.0.insert(euro.key.clone(),euro);
    let hand_run = estimate(&grouped,&facts,&places,&prices,&rates,10,&hand).selected.unwrap();
    assert_eq!(hand_run.parts.iter().map(|a| (&*a.currency,a.micros)).collect::<Vec<_>>(),[("EUR",10_000_000),("USD",8_000_000)]);

    // Optional user inputs carry their own dates through conversion and per-board division.
    let settings = crate::prices::Settings {
        hand:true, spares:Default::default(),
        flat:crate::prices::Setting {value:"USD 2.50".into(),date:"2026-11-23".into()},
        exchange:crate::prices::Setting {value:"EUR 1.10 USD".into(),date:"2026-11-24".into()},
    };
    let options = settings.options().unwrap();
    let with_user = estimate(&grouped,&facts,&places,&prices,&rates,10,&options).selected.unwrap();
    let usd_before = hand_run.totals.iter().find(|a| a.currency == "USD").unwrap();
    assert_eq!(with_user.totals.len(),1);
    assert_eq!(with_user.totals[0].micros,usd_before.micros + 11_000_000 + 2_500_000);
    assert_eq!(with_user.per_board[0].micros,with_user.totals[0].micros / 10);
    assert!(with_user.totals[0].sources.iter().any(|s| s.name == "User entry" && s.date == "2026-11-24"));
    let book = crate::prices::Library { settings, ..Default::default() };
    assert_eq!(crate::prices::Library::from_text(&book.to_text()).unwrap().settings,book.settings);
    let mut alt_design = design.clone();
    Arc::make_mut(&mut alt_design.units[0].circuit).comps[ids[0].ix()].fields.0.push(("alt.1.mpn".into(),"CAP".into()));
    let alt_sheet = super::sheet(&alt_design,board,&view).unwrap();
    let alt_run = estimate(&alt_sheet,&facts,&places,&prices,&rates,10,&hand).selected.unwrap();
    let alt = &alt_run.lines[0].alternates[0].2;
    assert_eq!((alt.bought.as_ref().unwrap().currency.as_str(),alt.bought.as_ref().unwrap().micros),("EUR",10_000_000));
    assert_eq!(alt_run.parts.iter().map(|a| a.micros).collect::<Vec<_>>(),[10_000_000,8_000_000]);
    let mut legacy = rates.clone();
    legacy.assembly = None;
    let without_schedule = estimate(&grouped,&facts,&places,&prices,&legacy,10,&Options::default()).selected.unwrap();
    assert!(without_schedule.assembly.unwrap_err().contains("Assembly not estimated") && without_schedule.lower_bound);

    view.cam.fields.iter_mut().for_each(|f| f.group_by = f.name == "Value");
    let merged = super::sheet(&design,board,&view).unwrap();
    assert_eq!(merged.rows.len(),1);
    let mixed = estimate(&merged,&facts,&places,&prices,&rates,10,&hand).selected.unwrap();
    assert!(mixed.lower_bound && mixed.priced == 0 && mixed.fitted == 1);
    assert!(mixed.lines[0].fitted.as_ref().unwrap().notes.iter().any(|s| s.contains("buy as different parts")));

    let s = rates.assembly.as_mut().unwrap();
    s.default_class=None; s.prefixes.clear();
    s.classes.insert("two".into(),Attrition{ppm:50_000,minimum:2});
    s.prefixes.extend([("C".into(),"standard".into()),("C_0".into(),"two".into())]);
    s.setup=20_000_000; s.stencil=10_000_000; s.billing=Billing::Placement; s.rate=250_000;
    s.per_line=0; s.outside_basic=0; s.second_side=0; s.minimum=25_000_000;
    let newer = entry(r#""supplier":"Source A","supplier_part":"A-0""#,"USD",
        r#""moq":13,"multiple":5,"breaks":[{"min":1,"micros":800000},{"min":18,"micros":500000}]"#);
    prices.0.insert(newer.key.clone(),newer);
    let run = estimate(&grouped,&facts,&places,&prices,&rates,10,&Options::default()).selected.unwrap();
    let cap = run.lines[0].fitted.as_ref().unwrap();
    assert_eq!((cap.needed,cap.spares,cap.ordered),(10,Some(2),Some(20)));
    // 12 required, MOQ 13, pack rounds to 15 × 0.80; the next legal order is 20 × 0.50.
    assert_eq!(cap.bought.as_ref().unwrap().micros,10_000_000);
    assert_eq!(cap.used_per_board.as_ref().unwrap().micros,500_000);
    assert!(cap.notes.iter().any(|s| s.contains("next break")));
    let assembly = run.assembly.unwrap();
    assert_eq!(assembly.total.micros,35_000_000);
    assert!(assembly.charges.iter().any(|(s,_)| s=="Second side"));
    assert!(!assembly.charges.iter().any(|(s,_)| s.contains("Minimum")));
    let s = rates.assembly.as_mut().unwrap(); s.minimum=55_000_000; s.prefixes.clear();
    let unknown = estimate(&grouped,&facts,&places,&prices,&rates,10,&Options::default()).selected.unwrap();
    assert!(unknown.lower_bound && unknown.lines[0].fitted.as_ref().unwrap().notes.iter().any(|s| s.contains("Attrition unknown")));
    assert!(unknown.assembly.unwrap().charges.iter().any(|(s,a)| s.contains("Minimum") && a.micros==20_000_000));

    Arc::make_mut(&mut design.units[0].circuit).comps[ids[0].ix()].dnp=true;
    let revised = super::sheet(&design,board,&view).unwrap();
    let unpriced = estimate(&revised,&facts,&places,&Prices::default(),&rates,10,&hand).selected.unwrap();
    assert_eq!(unpriced.dnp.as_slice(),["C1"]);
    assert!(unpriced.lower_bound && unpriced.priced==0 && unpriced.fitted==1 && unpriced.parts.is_empty());
}
