//! The organic port's copper kernels against Fab's own distances (L0 of the port): on ledctl's
//! pads, drills, tracks and vias, the port's gaps for generated segments and discs equal
//! `core::dist::clearance` within the float model's tolerance (exact pad shapes) or stay below it
//! (pads the port models by their box), and the port's pad groups per net agree with Fab's
//! connectivity.

use fab_elec_board::{connect, resolve_rules, ConnItem, CustomRule, NetPair, RuleCode, RuleItem, RuleKey, RuleProps, RuleSet, RuleSource, ViaKind};
use fab_elec_circuit::{ClassRules, NetClass, ViaSize};
use fab_elec_core::{clearance, BBox, ClassId, ContentHash, LayerId, NetId, Nm, Severity, Shape, P};
use fab_elec_fixtures::ledctl;
use fab_elec_route::organic::adapt::{adapt, mm, to_ref, PadSrc};
use fab_elec_route::organic::{copper, diffpairs};
use fab_elec_route::organic::engine::Engine;
use fab_elec_route::organic::model::{PadShape, ViaType};
use fab_elec_route::organic::settings::OrganicSettings;
use fab_elec_route::RStack;
use std::collections::{BTreeMap, BTreeSet};

/// f32 mm resolves ~4 nm at ledctl's size; Fab rounds gaps down to whole nm.
const TOL_NM: f64 = 10.0;

fn xorshift64(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn point_in(state: &mut u64, area: BBox) -> P {
    P::new(
        area.min.x + (xorshift64(state) % (area.width() as u64 + 1)) as Nm,
        area.min.y + (xorshift64(state) % (area.height() as u64 + 1)) as Nm,
    )
}

/// The port's gap in nm, clamped at 0 like Fab's.
fn nm(gap_mm: f64) -> f64 {
    (gap_mm * 1e6).max(0.0)
}

#[test]
fn kernels_match_fab_distances() {
    let design = ledctl();
    let board = design.units[0].board.as_deref().expect("ledctl has a board").clone();
    let rules = resolve_rules(&board, None);
    let stack = RStack::of(&board);
    let a = adapt(&board, &rules, &stack);
    let ob = &a.board;

    // Fab's shape of every pad and plated-hole ring per copper layer
    let mut fab_shape: BTreeMap<(PadSrc, LayerId), Shape> = BTreeMap::new();
    for c in board.all_copper() {
        let src = match c.item {
            ConnItem::Pad { placed, pad } => PadSrc::Pad { placed, pad },
            ConnItem::Hole(h) => PadSrc::Hole(h),
            _ => continue,
        };
        fab_shape.insert((src, c.layer), c.shape);
    }
    // an unplated hole has no copper: the port's pad for it is the hole on every copper layer
    let unplated: Vec<_> = board.drills().into_iter().filter(|d| !d.plated).collect();
    for d in &unplated {
        let src = match d.item {
            ConnItem::Pad { placed, pad } => PadSrc::Pad { placed, pad },
            ConnItem::Hole(h) => PadSrc::Hole(h),
            _ => continue,
        };
        if !fab_shape.keys().any(|k| k.0 == src) {
            for &layer in &a.layers {
                fab_shape.insert((src, layer), d.shape());
            }
        }
    }
    assert!(!unplated.is_empty(), "ledctl has unplated holes");
    assert!(!ob.pads.is_empty() && ob.pads.len() == a.pads.len(), "adapter: one source per pad piece");
    let pads_fab: BTreeSet<PadSrc> = fab_shape.keys().map(|k| k.0).collect();
    let pads_port: BTreeSet<PadSrc> = a.pads.iter().copied().collect();
    assert_eq!(pads_fab, pads_port, "adapter: every Fab pad with copper, and every unplated hole, is in the port's board");
    assert!(ob.pads.iter().all(|p| p.layer_mask != 0), "adapter: every pad piece is on a layer");
    assert_eq!((ob.vias.len(), ob.traces.len()), (board.vias.len(), board.tracks.len()), "adapter: every via and track");

    // Board.cpp:314-370 resolves sizes by net class; Fab's equivalent is rules.via(class),
    // with pad = diameter and nm / 1e6 = mm. zone::rule_item / class_of gives a via
    // RuleKey { class: board.nets[n].class, item: Via }, even for a net with no existing via.
    for (n, net) in ob.nets.iter().enumerate() {
        let class = board.nets[n].class;
        let via = rules.via(class);
        assert_eq!(a.rules[net.via_rule as usize], RuleKey { class, item: RuleItem::Via }, "adapter: net {n}'s via row has its class and Via item kind");
        assert_eq!(net.via_diameter.to_bits(), mm(via.pad).to_bits(), "adapter: net {n}'s via diameter is its resolved class pad in mm");
        assert_eq!(net.via_drill.to_bits(), mm(via.drill).to_bits(), "adapter: net {n}'s via drill is its resolved class drill in mm");
        assert!(!net.via_in_pad, "adapter: net {n}'s class without permission keeps vias out of pads");
    }

    // No input via can supply this new class's Via key. Board.cpp:314-370 selects
    // its explicit class sizes: 1_600_000 / 1e6 = 1.6 mm pad, 800_000 / 1e6 = 0.8 mm
    // drill, both above the base minima. Its new via must still have a table row.
    let mut no_vias = board.clone();
    no_vias.vias.clear();
    let class = ClassId(no_vias.classes.len() as u16);
    no_vias.classes.push(NetClass {
        id: class, name: "large_via".into(),
        rules: ClassRules { via: Some(ViaSize { pad: 1_600_000, drill: 800_000 }), via_in_pad: Some(true), ..ClassRules::default() },
    });
    no_vias.nets[0].class = class;
    let no_via_rules = resolve_rules(&no_vias, None);
    let no_via_adapted = adapt(&no_vias, &no_via_rules, &stack);
    let net = &no_via_adapted.board.nets[0];
    assert_eq!(no_via_adapted.rules[net.via_rule as usize], RuleKey { class, item: RuleItem::Via }, "adapter: a class without input vias still gets its Via row");
    assert_eq!(net.via_diameter.to_bits(), 1.6f32.to_bits(), "adapter: a class without input vias keeps its larger diameter");
    assert_eq!(net.via_drill.to_bits(), 0.8f32.to_bits(), "adapter: a class without input vias keeps its larger drill");
    assert!(net.via_in_pad, "adapter: a net whose class allows vias in pads keeps that permission");
    assert!(!no_via_adapted.board.nets[1].via_in_pad, "adapter: another class without permission still keeps vias out of pads");
    let engine = Engine::new(&no_via_adapted.board, &OrganicSettings::default());
    assert!(engine.via_in_pad_ok(0), "engine: the adapter's via-in-pad permission reaches the net lookup");
    assert!(!engine.via_in_pad_ok(1), "engine: via-in-pad permission stays specific to the allowed net");

    // A rule only a via of that class meets (the board has none) is in its row, for a via a pass
    // adds, but does not lower the clearance floor of the board's own copper.
    struct ViaToVia(ClassId);
    impl RuleSource for ViaToVia {
        fn when(&self, _: &RuleCode, a: &RuleProps, b: &RuleProps) -> bool {
            [a, b].iter().all(|p| p.class == self.0 && p.item == RuleItem::Via)
        }
    }
    let mut close_vias = no_vias.clone();
    close_vias.rules.custom.push(CustomRule {
        name: "close_vias".into(),
        when: RuleCode { id: 0, hash: ContentHash::default(), at: (0, 0) },
        layers: None,
        set: RuleSet { clearance: Some(50_000), ..RuleSet::default() },
        severity: Severity::Error,
    });
    let close = adapt(&close_vias, &resolve_rules(&close_vias, Some(&ViaToVia(class))), &stack);
    let row = close.board.nets[0].via_rule;
    let floor = no_via_adapted.board.rules.min_clearance;
    assert!(close.board.clr.get(row, row, -1) < floor, "adapter: the via-to-via rule of the class is in its via row");
    assert_eq!(close.board.rules.min_clearance.to_bits(), floor.to_bits(), "adapter: a rule no copper of the board meets leaves the clearance floor");

    // the port's via keeps Fab's kind, so a via a pass copies lifts with it
    let mut micro = board.clone();
    micro.vias[0].kind = ViaKind::Micro;
    let micro_adapted = adapt(&micro, &rules, &stack);
    assert_eq!(micro_adapted.board.vias[0].kind, ViaType::Micro, "adapter: a micro via is a micro via in the port");

    // Board::diffPairGap's role (Board.cpp:236-247) is Fab's pair rule: each net of a pair knows its
    // partner and Fab's gap (the pair's own, else its classes'); every other net is in no pair
    let mut paired = board.clone();
    paired.pairs = vec![NetPair { p: NetId(1), n: NetId(2), gap: Some(150_000) }, NetPair { p: NetId(3), n: NetId(4), gap: None }];
    let pair_rules = resolve_rules(&paired, None);
    let pa = adapt(&paired, &pair_rules, &stack);
    for n in 0..pa.board.nets.len() as i32 {
        let fab = pair_rules.partner(NetId(n as u32));
        assert_eq!(diffpairs::partner_of(&pa.board, n), fab.map_or(-1, |(q, _)| q.0 as i32), "adapter: net {n}'s pair partner is Fab's");
        assert_eq!(diffpairs::member_gap(&pa.board, n).to_bits(), fab.map_or(0.0, |(_, g)| mm(g)).to_bits(), "adapter: net {n}'s pair gap is Fab's");
    }
    assert_eq!(diffpairs::member_gap(&pa.board, 2).to_bits(), 0.15f32.to_bits(), "adapter: a pair's own gap is its gap");
    assert!(diffpairs::partner_of(&pa.board, 3) == 4 && diffpairs::member_gap(&pa.board, 4) > 0.0, "adapter: a pair without its own gap takes its classes'");

    // 1. segment and disc gaps to pads: exact shapes within TOL_NM, box-modelled pads never above Fab's
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let (mut exact, mut boxed) = (0usize, 0usize);
    for (i, pad) in ob.pads.iter().enumerate() {
        let layer = a.layers[pad.layer_mask.trailing_zeros() as usize];
        let shape = &fab_shape[&(a.pads[i], layer)];
        let area = shape.bbox().inflate(1_500_000);
        let is_exact = pad.shape != PadShape::Polygon && matches!(shape, Shape::Circle { .. } | Shape::Rect { .. } | Shape::Oval { .. });
        for k in 0..24 {
            let (p, q) = (point_in(&mut seed, area), point_in(&mut seed, area));
            let q = if k % 6 == 0 { p } else { q }; // discs too
            let w: Nm = if k % 2 == 0 { 200_000 } else { 250_000 };
            let fab = clearance(&Shape::Segment { a: p, b: q, w }, shape) as f64;
            let port = nm(copper::seg_pad_gap(to_ref(p), to_ref(q), w as f64 / 2e6, pad));
            if is_exact {
                assert!((port - fab).abs() <= TOL_NM, "pad gap: pad {i} {:?} seg {p:?}-{q:?} w {w}: port {port} nm, fab {fab} nm", pad.shape);
                exact += 1;
            } else {
                assert!(port <= fab + TOL_NM, "pad gap (box model) never more room than Fab: pad {i}: port {port}, fab {fab}");
                boxed += 1;
            }
        }
    }
    assert!(exact > 100, "pad gap: ledctl gives exact cases ({exact} exact, {boxed} boxed)");

    // 2. drills: the port's hole capsule against Fab's drill shape
    let mut drills = 0;
    for d in board.drills() {
        let src = match d.item {
            ConnItem::Pad { placed, pad } => PadSrc::Pad { placed, pad },
            ConnItem::Hole(h) => PadSrc::Hole(h),
            _ => continue,
        };
        let Some(i) = a.pads.iter().position(|s| *s == src) else { continue };
        let m = copper::pad_model(&ob.pads[i]);
        assert!(m.hole, "drill: pad {i} of a drill (plated or not) has a hole");
        let hole = d.shape();
        let area = hole.bbox().inflate(1_000_000);
        for _ in 0..12 {
            let (p, q) = (point_in(&mut seed, area), point_in(&mut seed, area));
            let fab = clearance(&Shape::Segment { a: p, b: q, w: 200_000 }, &hole) as f64;
            let port = nm(copper::seg_hole(to_ref(p), to_ref(q), &m).d - m.hr - 0.1);
            assert!((port - fab).abs() <= TOL_NM, "drill gap: pad {i} seg {p:?}-{q:?}: port {port}, fab {fab}");
            drills += 1;
        }
    }

    // 3. segment-segment and disc-segment gaps: ledctl's own track and via copper, then generated pairs
    let mut segs = 0;
    let mut check_seg = |own_a: P, own_b: P, own_w: Nm, seed: &mut u64, what: &str| {
        let own = Shape::Segment { a: own_a, b: own_b, w: own_w };
        let area = own.bbox().inflate(1_000_000);
        for _ in 0..6 {
            let (p, q) = (point_in(seed, area), point_in(seed, area));
            let fab = clearance(&Shape::Segment { a: p, b: q, w: 200_000 }, &own) as f64;
            let port = nm(copper::seg_seg(to_ref(p), to_ref(q), to_ref(own_a), to_ref(own_b)).d - 0.1 - own_w as f64 / 2e6);
            assert!((port - fab).abs() <= TOL_NM, "{what} gap: {own_a:?}-{own_b:?} w {own_w} vs {p:?}-{q:?}: port {port}, fab {fab}");
            segs += 1;
        }
    };
    for c in board.all_copper() {
        match (c.item, &c.shape) {
            (ConnItem::Track { .. }, Shape::Segment { a: sa, b: sb, w }) => check_seg(*sa, *sb, *w, &mut seed, "track"),
            (ConnItem::Via(_), Shape::Circle { c: vc, r }) => check_seg(*vc, *vc, 2 * *r, &mut seed, "via"),
            _ => {}
        }
    }
    let outline = board.outline_bbox();
    for k in 0..40 {
        let (p, q) = (point_in(&mut seed, outline), point_in(&mut seed, outline));
        let q = if k % 5 == 0 { p } else { q };
        check_seg(p, q, if k % 2 == 0 { 250_000 } else { 600_000 }, &mut seed, "generated");
    }
    assert!(drills > 0 && segs > 0, "drill and track cases exist on ledctl ({drills} drills, {segs} segments)");

    // 4. pad groups per net: the port's net_pad_groups equals Fab's clusters holding the net's pads, minus one
    let conn = connect(&board, &[]);
    for net in 0..ob.nets.len() {
        let pads: BTreeSet<PadSrc> = ob.pads.iter().zip(&a.pads).filter(|(p, _)| p.net == net as i32).map(|(_, s)| *s).collect();
        let fab_groups = conn
            .clusters
            .iter()
            .filter(|c| {
                c.items.iter().any(|it| match *it {
                    ConnItem::Pad { placed, pad } => pads.contains(&PadSrc::Pad { placed, pad }),
                    ConnItem::Hole(h) => pads.contains(&PadSrc::Hole(h)),
                    _ => false,
                })
            })
            .count();
        let expect = if pads.len() < 2 { 0 } else { fab_groups.saturating_sub(1) as i32 };
        let port = copper::net_pad_groups(ob, net as i32, None, 1e-3);
        assert_eq!(port, expect, "pad groups: net {net} ({} pads): port {port}, Fab {expect}", pads.len());
    }
}
