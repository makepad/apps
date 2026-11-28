use super::*;
use fab_elec_parts::{PartDef, PartKind};

#[test]
fn standard_library_row_symbols_and_details() {
    use fab_elec_lang::{defs_catalog, std_libs, ProjectDefs, Sources};
    use makepad_widgets_core::makepad_draw::svg::{parse_svg, SvgNode};

    let libs = std_libs();
    let catalog = defs_catalog(&libs, &ProjectDefs::new(&Sources::new(), &libs));
    let entry = |name: &str| catalog.entries.iter().find(|e| e.lib == "std" && e.name == name)
        .unwrap_or_else(|| panic!("the standard catalog offers {name}"));
    for (name, detail) in [
        ("r 10kohm 0402", "0402, 10k"),
        ("c 100nF 0402", "0402, 100nF"),
        ("led @green 0603", "0603, green"),
        ("USB_C_POWER", "5 pins"),
        ("usb_c_power", "2 pins"),
    ] {
        assert_eq!(entry_detail(entry(name)), detail, "{name} shows its package and default or its pin count");
    }
    for (name, shapes) in [("r 10kohm 0402", 3), ("USB_C_POWER", 6), ("L_0805", 6)] {
        let part = entry(name).part.as_ref().expect("part rows carry their symbol definition");
        let svg = symbol::symbol_svg(part).expect("a standard part has a drawable symbol");
        let doc = parse_svg(&svg);
        let view = doc.viewbox.as_ref().expect("the icon declares its viewBox");
        let (l, t, r, b) = doc.compute_bounds().expect("the parser finds symbol geometry");
        assert_eq!(doc.root.len(), shapes, "{name} draws every body primitive and pin stub");
        assert!(view.x < l && view.y < t && view.x + view.width > r && view.y + view.height > b,
            "{name} leaves space for strokes around every primitive");
        for pin in &part.symbol.units[0].pins {
            let (x, y) = (pin.at.x as f32, -pin.at.y as f32);
            assert!(l <= x && x <= r && t <= y && y <= b, "{name} reaches each connection point with sheet Y up");
        }
        if name == "L_0805" {
            assert!(t < -0.29 && b < 0.01, "the inductor's four arcs curve above its leads");
        } else {
            assert!(doc.root.iter().any(|node| matches!(node, SvgNode::Rect(_))), "{name} has its rectangular body");
        }
    }
    let mut bare = (**entry("r 10kohm 0402").part.as_ref().unwrap()).clone();
    bare.symbol.units.clear();
    assert!(symbol::symbol_svg(&bare).is_none(), "even a familiar family needs a symbol unit");

    let ctx = ShortlistContext { pinned: &[], used_parts: &BTreeSet::new(), imported_ids: &BTreeSet::new() };
    let mut e = entry("r 10kohm 0402").clone();
    use fab_elec_parts::Lifecycle::{Active, Nrnd, Obsolete};
    for (approved, lifecycle, badges) in [
        (false, None, ""), (false, Some(Active), ""), (true, None, "approved"),
        (true, Some(Active), "approved"), (false, Some(Nrnd), "NRND"),
        (true, Some(Obsolete), "approved · obsolete"),
    ] {
        let part = Arc::make_mut(e.part.as_mut().unwrap());
        part.approved = approved;
        part.lifecycle = lifecycle;
        let ranked = rank::rank_with_context(std::slice::from_ref(&e), "", &ctx, &Prices::default());
        let text = row_text(&e, &ranked[0], true);
        assert_eq!(text.badges, badges, "the first line must keep approval and lifecycle together");
        assert_eq!(text.meta, "std · unpriced · 0402, 10k · +0 alt", "the second line must keep provenance, price, package and alternatives");
    }
    e.lib = "imported".into();
    let ranked = rank::rank_with_context(std::slice::from_ref(&e), "", &ctx, &Prices::default());
    let placeable = can_place(ranked[0].index, 0);
    assert!(!placeable && row_text(&e, &ranked[0], placeable).detail.contains(IMPORT_NOTE),
        "a cache-only part must stay unarmed and explain in its detail and preview how it can be used");
}

fn build_rows(entries: &[CatalogEntry], query: &str, pinned: &[EntryKey], recent: &[EntryKey],
    used: &HashSet<String>, folded: &HashSet<Section>) -> (Vec<Row>, [String; Section::ALL.len()]) {
    let keys = pinned.iter().map(|key| (key.lib.clone(), key.name.clone())).collect::<Vec<_>>();
    let ctx = ShortlistContext { pinned: &keys, used_parts: &used.iter().cloned().collect(), imported_ids: &BTreeSet::new() };
    let matches = rank::rank_with_context(entries, query, &ctx, &Prices::default());
    rows_from_matches(entries, &matches, query, pinned, recent, used, folded)
}

#[test]
fn library_tree_shortcuts_categories_and_ranked_search() {
    let part = |name: &str, kind| Arc::new(PartDef {
        name: name.into(), kind, symbol: Default::default(), footprint: None, body: None,
        pins: Vec::new(), fields: Default::default(), emitter: None, gpio: None, lifecycle: None, approved: false,
    });
    let entry = |name: &str, kind, part| CatalogEntry {
        lib: "test".into(), name: name.into(), kind, title: name.into(), summary: String::new(),
        pins: Vec::new(), stmt: String::new(), stem: String::new(), defaults: Vec::new(), part, hash: 0,
    };
    let mut entries = vec![
        entry("preclock", CatalogKind::Passive, Some(part("resistor", PartKind::Resistor))),
        entry("clockwork", CatalogKind::Part, Some(part("oscillator", PartKind::Ic))),
        entry("clock", CatalogKind::Block, None),
        entry("clock flag", CatalogKind::Part, Some(part("flag", PartKind::PowerFlag))),
    ];
    entries.extend((0..9).map(|i| entry(&format!("part{i}"), CatalogKind::Part, None)));
    let missing = EntryKey { lib: "absent".into(), name: "clock".into() };
    let pinned = vec![missing.clone(), entry_key(&entries[1]), entry_key(&entries[3])];
    let recent_keys: Vec<_> = entries[4..].iter().map(entry_key).collect();
    let used = HashSet::from(["resistor".into(), "flag".into()]);
    let rows = |query: &str, pinned: &[EntryKey], recent: &[EntryKey], used: &HashSet<String>, folded: &[Section]| {
        build_rows(&entries, query, pinned, recent, used, &folded.iter().copied().collect())
    };
    let headers = |rows: &[Row]| rows.iter().filter_map(|row| match row {
        Row::Header(section) => Some(*section), _ => None,
    }).collect::<Vec<_>>();
    let indices = |rows: &[Row], section| rows.iter().filter_map(|row| match row {
        Row::Entry { index, section: here, .. } if *here == section => Some(*index), _ => None,
    }).collect::<Vec<_>>();

    let (tree, counts) = rows("", &pinned, &recent_keys, &used, &[]);
    assert_eq!(headers(&tree), [
        Section::Pinned, Section::Recent, Section::Board, Section::Passives, Section::Parts, Section::Blocks,
    ], "shortcuts precede the three catalog categories");
    assert_eq!(indices(&tree, Some(Section::Pinned)), [1], "unknown pins and power flags stay hidden");
    assert_eq!(indices(&tree, Some(Section::Board)), [0], "board membership uses the underlying part definition");
    assert_eq!(indices(&tree, Some(Section::Recent)), (4..12).collect::<Vec<_>>(), "only the first eight recent keys appear, in order");
    assert_eq!(counts, ["1", "8", "1", "1", "10", "1"], "headers count just the entries each section offers");
    assert!(!tree.iter().any(|row| matches!(row, Row::Entry { index: 3, .. })), "no section offers power flags");
    assert!(tree.contains(&Row::Entry { index: 1, section: Some(Section::Parts), pinned: true }),
        "a pinned entry still appears starred in its category");

    let (folded, counts) = rows("", &[missing], &[], &HashSet::new(), &[Section::Parts]);
    assert_eq!(headers(&folded), [Section::Passives, Section::Parts, Section::Blocks], "empty shortcut sections are hidden");
    assert!(folded.windows(2).any(|pair| pair == [Row::Header(Section::Parts), Row::Header(Section::Blocks)]),
        "a folded section shows only its header");
    assert_eq!(counts[Section::Parts as usize], "10", "folding preserves the full section count");
    assert_eq!(headers(&build_rows(&entries[..1], "", &[], &[], &used, &HashSet::new()).0),
        [Section::Board, Section::Passives], "empty catalog categories are hidden too");

    let (search, _) = rows("clock", &pinned, &recent_keys, &used, &Section::ALL);
    assert_eq!(search, [
        Row::Entry { index: 2, section: None, pinned: false },
        Row::Entry { index: 1, section: None, pinned: true },
        Row::Entry { index: 0, section: None, pinned: false },
    ], "search is flat and ranked across categories, ignores folds, keeps pins and excludes power flags");

    let mut recent = Vec::new();
    for key in &recent_keys {
        touch_recent(&mut recent, key.clone());
    }
    assert_eq!(recent, recent_keys[1..].iter().rev().cloned().collect::<Vec<_>>(),
        "arming a ninth part evicts the oldest shortcut");
    touch_recent(&mut recent, recent_keys[1].clone());
    assert_eq!(recent, std::iter::once(&recent_keys[1]).chain(recent_keys[2..].iter().rev()).cloned().collect::<Vec<_>>(),
        "using a previous part moves it to the front without duplicating it");
}
