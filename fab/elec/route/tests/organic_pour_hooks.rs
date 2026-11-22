use fab_elec_route::organic::{
    copper,
    engine::{guard::LocalModel, Engine, Ob, ObKind, PairClass, PairEval, Prim},
    geom::Vec2,
    model::*,
    relaxer::OrganicRelaxer,
    settings::OrganicSettings,
};

fn board() -> OBoard {
    OBoard {
        layers: vec![LayerInfo { index: 0, plane_net: NONE, ..LayerInfo::default() }],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        clr: ClearanceTable { rows: 2, layers: 1, mm: vec![0.2; 4], max: 0.2 },
        ..OBoard::default()
    }
}

fn trace(id: i32, net: i32, positions: &[Vec2]) -> Trace {
    Trace {
        id, net, layer: 0, width: 0.25, is_routed: true,
        points: positions.iter().enumerate().map(|(i, &pos)| TracePoint {
            pos, width: 0.25, is_fixed: i == 0 || i + 1 == positions.len(), ..TracePoint::default()
        }).collect(),
        ..Trace::default()
    }
}

fn bits(p: Vec2) -> (u32, u32) { (p.x.to_bits(), p.y.to_bits()) }

fn moving_prims(b: &OBoard, pos: Vec2) -> Vec<Prim> {
    let tr = &b.traces[0];
    let p = Prim {
        a: tr.points[0].pos, b: pos, h: tr.width as f64 * 0.5, layer: tr.layer, net: tr.net,
        trace: 0, ex_lo: 0, ex_hi: 1, owner: 0, obj: copper::trace_key(0), rule: tr.rule,
        ..Prim::default()
    };
    vec![p, Prim { a: tr.points[2].pos, ..p }]
}

// Drive the middle node through the engine's real move guard; the two terminals stay fixed.
fn pull(b: &mut OBoard, target_y: f32) -> Vec<(u32, u32)> {
    let mut e = Engine::new(b, &OrganicSettings::base());
    e.build_static(b, true);
    e.build_dynamic(b, None, 0.0, None);
    let mut history = Vec::new();
    for _ in 0..40 {
        let pos = b.traces[0].points[1].pos;
        let mut prims = moving_prims(b, pos);
        let m = LocalModel {
            gy: 1000.0 * (pos.y as f64 - target_y as f64), hxx: 1000.0, hyy: 1000.0,
            ..LocalModel::default()
        };
        let result = e.try_move_x0(b, pos, &mut prims, 1, m, &[], 0.0, false, 1.0);
        b.traces[0].points[1].pos = result.pos;
        history.push(bits(result.pos));
        e.build_dynamic(b, None, 0.0, None);
    }
    history
}

#[test]
fn only_net_keepout_holds_only_its_net_off_the_edge() {
    let v = Vec2::new;
    let mut input = board();
    input.traces.push(trace(0, 0, &[v(-1.0, 0.75), v(0.0, 0.25), v(1.0, 0.75)]));
    input.keepouts.push(Keepout {
        id: 0, poly: vec![v(-2.0, -2.0), v(2.0, -2.0), v(2.0, 0.0), v(-2.0, 0.0)],
        layer_mask: 1, clearance: 0.0, no_tracks: true, no_vias: false, only_net: 0,
    });
    assert_eq!(Keepout::default().only_net, NONE, "ordinary keepouts retain the every-net default");
    let mut affected = input.clone();
    pull(&mut affected, -0.5);
    let y = affected.traces[0].points[1].pos.y;
    let limit = 0.125 + OrganicSettings::base().guard_clearance_slack as f64;
    assert!(y as f64 >= limit - 1e-9, "only_net copper stays outside the keepout edge threshold");
    assert!(y < 0.25, "only_net copper approaches the edge before the guard holds it off");
    for i in [0, 2] {
        assert_eq!(bits(affected.traces[0].points[i].pos), bits(input.traces[0].points[i].pos),
            "only_net guard leaves fixed terminal {i} unchanged");
    }

    let mut other = input.clone();
    other.traces[0].net = 1;
    let mut without = other.clone();
    without.keepouts.clear();
    assert_eq!(pull(&mut other, -0.5), pull(&mut without, -0.5),
        "another net's entire move history equals the run without the keepout");
    assert!(other.traces[0].points[1].pos.y < 0.0, "another net can move through the filtered edge");
}

#[test]
fn pour_rows_slide_over_distinct_same_net_tracks() {
    let v = Vec2::new;
    let mut input = board();
    // Copper overlaps already; the 0.02 mm centre gap starts above the 0.005 mm self threshold.
    input.traces.push(trace(0, 0, &[v(-1.0, 0.5), v(0.0, 0.02), v(1.0, 0.5)]));
    let mut fixed = trace(1, 0, &[v(-2.0, 0.0), v(2.0, 0.0)]);
    fixed.locked = true;
    input.traces.push(fixed);
    let mut ordinary = input.clone();
    pull(&mut ordinary, 0.0);
    let threshold = Engine::new(&input, &OrganicSettings::base()).self_thr;
    let y = ordinary.traces[0].points[1].pos.y as f64;
    assert!(y >= threshold && y < threshold + 0.002,
        "ordinary same-net copper is held at the self threshold, got {y}");

    for (moving_rule, fixed_rule) in [(1, 0), (0, 1), (1, 1)] {
        let mut pour = input.clone();
        pour.pour_rules = vec![1];
        pour.traces[0].rule = moving_rule;
        pour.traces[1].rule = fixed_rule;
        pull(&mut pour, 0.0);
        assert!(pour.traces[0].points[1].pos.y.abs() < 1e-7,
            "either pour row permits same-net centreline overlap ({moving_rule}, {fixed_rule})");
        let at_overlap = bits(pour.traces[0].points[1].pos);
        pull(&mut pour, 0.0);
        assert_eq!(bits(pour.traces[0].points[1].pos), at_overlap,
            "overlapping pour copper has no force moving it away ({moving_rule}, {fixed_rule})");
        assert_eq!(bits(pour.traces[1].points[0].pos), bits(input.traces[1].points[0].pos),
            "sliding a pour path preserves the fixed track");
    }

    input.pour_rules = vec![1];
    input.traces[0].rule = 1;
    let e = Engine::new(&input, &OrganicSettings::base());
    let p = moving_prims(&input, input.traces[0].points[1].pos)[0];
    let o = Ob {
        kind: ObKind::Seg, trace: 1, seg: 0, net: 0, layer: 0,
        a: v(-2.0, 0.0), b: v(2.0, 0.0), x0: -2.0, x1: 2.0, h: 0.125,
        ..Ob::default()
    };
    let mut pair = PairEval::default();
    assert_eq!(e.eval_pair(&input, &p, &o, &mut pair), PairClass::Na,
        "a moving pour row exempts a distinct same-net segment");
    assert_eq!(e.eval_pair(&input, &p, &Ob { trace: 0, seg: 4, ..o }, &mut pair), PairClass::Eval,
        "a pour row retains its own nonadjacent-segment self guard");
    for index in [NONE, input.traces.len() as i32] {
        assert_eq!(e.eval_pair(&input, &Prim { trace: index, ..p }, &o, &mut pair), PairClass::Eval,
            "a missing moving trace index {index} does not masquerade as a pour row");
        assert_eq!(e.eval_pair(&input, &p, &Ob { trace: index, ..o }, &mut pair), PairClass::Na,
            "a valid moving pour row tolerates a missing obstacle trace index {index}");
    }
    assert_eq!(e.eval_pair(&input, &p, &Ob { net: 1, ..o }, &mut pair), PairClass::Eval,
        "a pour row still guards foreign-net copper");
}

#[test]
fn pour_net_is_not_rebuilt_by_the_step_loop_simplifier() {
    let v = Vec2::new;
    let mut input = board();
    input.pads = [v(-0.75, 1.0), v(0.75, 1.0)].into_iter().enumerate().map(|(i, center)| Pad {
        id: i as i32, net: 0, center, size: v(0.25, 0.25), local_size: v(0.25, 0.25),
        layer_mask: 1, is_smd: true, drill_diameter: 0.0, ..Pad::default()
    }).collect();
    input.traces = vec![
        trace(0, 0, &[v(-0.75, 1.0), v(0.75, 1.0)]),
        trace(1, 0, &[v(-0.75, 1.0), v(0.0, 2.3), v(0.75, 1.0)]),
    ];
    for tr in &mut input.traces {
        tr.points[0].terminal_pad = 0;
        tr.points.last_mut().expect("test terminal").terminal_pad = 1;
    }
    input.traces[1].rule = 1;
    input.pour_rules = vec![1];
    let mut settings = OrganicSettings::base();
    settings.enable_loop_simplification = true;
    // Neither trace splits or merges. An ordinary row splits above 1.6 mm, and the 1.5 mm chord
    // exceeds its merge bound (0.9 * 1.6). A pour row resamples at twice the spacing (hook 7): it
    // splits above 3.2 mm, and the detour's 1.50 mm sides are not under its merge length (0.45 * 3.2).
    settings.node_spacing = 1.6;
    let mut ordinary = input.clone();
    ordinary.pour_rules.clear();
    let mut control = OrganicRelaxer::new(settings.clone());
    assert_eq!(control.step(&mut ordinary, 1000), Ok(()), "ordinary loop control runs the full step");
    assert_eq!(ordinary.traces.len(), 1, "ordinary loop control proves the simplifier removes the detour");

    let mut kept = input.clone();
    let mut relaxer = OrganicRelaxer::new(settings.clone());
    assert_eq!(relaxer.step(&mut kept, 1000), Ok(()), "pour loop protection runs the full step");
    assert_eq!(kept.traces.len(), 2, "the only moving trace's pour row keeps both traces out of rebuilding");
    for (before, after) in input.traces.iter().zip(&kept.traces) {
        assert_eq!((after.id, after.net, after.rule, after.points.len()),
            (before.id, before.net, before.rule, before.points.len()),
            "pour net retains each trace's identity, rule and nodes");
        for i in [0, before.points.len() - 1] {
            assert_eq!(bits(after.points[i].pos), bits(before.points[i].pos),
                "pour net retains trace {} terminal {i}", before.id);
            assert_eq!(after.points[i].terminal_pad, before.points[i].terminal_pad,
                "pour net retains trace {} terminal {i}'s attachment", before.id);
        }
    }
    assert!(!kept.traces[1].points[1].is_fixed, "keeping a pour net does not pin its moving node");
    assert_eq!(bits(kept.traces[1].points[1].pos), bits(input.traces[1].points[1].pos),
        "a pour path follows: with nothing near it, no spring or bend pulls its node");

    // A pad of another net over the middle node, inside its clearance: the path gives way.
    let mut pushed = input.clone();
    pushed.pads.push(Pad {
        id: 2, net: 1, center: v(0.0, 2.55), size: v(0.25, 0.25), local_size: v(0.25, 0.25),
        layer_mask: 1, is_smd: true, drill_diameter: 0.0, ..Pad::default()
    });
    let mut relaxer = OrganicRelaxer::new(settings);
    assert_eq!(relaxer.step(&mut pushed, 1000), Ok(()), "the pushed pour path runs the full step");
    assert_eq!(pushed.traces.len(), 2, "the pushed pour net keeps both traces out of rebuilding");
    assert!(pushed.traces[1].points[1].pos.y < input.traces[1].points[1].pos.y,
        "a pour path still moves out of a foreign neighbour's clearance while its net is protected from rebuilding");
}

#[test]
fn adapter_appends_zone_rows_and_preserves_all_mask_rings() {
    use fab_elec_board::{
        resolve_rules, zone::{static_masks, zone_gap}, Anchor, Area, Board, BoardKeepout, BoardNet,
        CustomRule, Origin, RuleCode, RuleItem, RuleKey, RuleProps, RuleSet, RuleSource, Track,
        Zone as BoardZone, ZoneSettings,
    };
    use fab_elec_circuit::{ClassRules, NetClass};
    use fab_elec_core::{BBox, BoardId, ClassId, ContentHash, Contour, LayerId, LayerSet, NetId, Path, PolySet, Severity, P};
    use fab_elec_parts::Forbid;
    use fab_elec_route::{organic::adapt::{adapt, add_pour_rows, mm, pour_avoid_areas, to_ref}, RStack};
    use std::collections::BTreeSet;

    struct ZoneRules;
    impl RuleSource for ZoneRules {
        fn when(&self, _: &RuleCode, a: &RuleProps, b: &RuleProps) -> bool {
            a.item == RuleItem::Zone || b.item == RuleItem::Zone
        }
    }
    let mut board = Board::new(BoardId(0), "pour adapter");
    board.outline.push(Contour::rect(P::ZERO, P::new(12_000_000, 8_000_000), 0));
    board.classes = vec![
        NetClass { id: ClassId(0), name: "ordinary".into(), rules: ClassRules::default() },
        NetClass { id: ClassId(1), name: "wide".into(), rules: ClassRules {
            clearance: Some(400_000), ..ClassRules::default()
        } },
    ];
    board.nets = vec![
        BoardNet { name: "pour".into(), class: ClassId(0) },
        BoardNet { name: "other".into(), class: ClassId(1) },
    ];
    board.rules.custom.push(CustomRule {
        name: "front zone clearance".into(), when: RuleCode { id: 0, hash: ContentHash::default(), at: (0, 0) },
        layers: Some(LayerSet::one(LayerId::F_CU)), set: RuleSet { clearance: Some(600_000), ..RuleSet::default() },
        severity: Severity::Error,
    });
    for (net, area) in [
        (NetId(1), Area::Board),
        (NetId(0), Area::Poly(PolySet::rect(BBox::new(P::new(1_000_000, 1_000_000), P::new(11_000_000, 7_000_000))))),
    ] {
        let id = board.alloc_id();
        board.zones.push(BoardZone {
            id, net: Some(net), name: "pour".into(), area, layers: board.copper_set(), priority: 0,
            settings: ZoneSettings { clearance: Some(250_000), ..ZoneSettings::default() },
            locked: false, origin: Origin::Authored,
        });
    }
    let id = board.alloc_id();
    board.tracks.push(Track {
        id, net: NetId(0), layer: LayerId::F_CU, w: 250_000, widths: None,
        path: Path::new(P::new(2_000_000, 2_000_000)).line(P::new(3_000_000, 2_000_000)),
        ends: [Anchor::Free; 2], locked: true, origin: Origin::Authored,
    });
    let centre = P::new(6_000_000, 4_000_000);
    let ring = PolySet::rect(BBox::point(centre).inflate(800_000))
        .difference(&PolySet::rect(BBox::point(centre).inflate(300_000)));
    for (area, forbid) in [
        (ring, Forbid::ZONES),
        (PolySet::rect(BBox::point(P::new(10_000_000, 2_000_000)).inflate(200_000)), Forbid::TRACKS),
    ] {
        let id = board.alloc_id();
        board.keepouts.push(BoardKeepout {
            id, area, layers: LayerSet::one(LayerId::F_CU), forbid,
            reason: String::new(), origin: Origin::Authored, z: None,
        });
    }
    board.reindex();
    let rules = resolve_rules(&board, Some(&ZoneRules));
    let a = adapt(&board, &rules, &RStack::of(&board));
    assert!(a.board.pour_rules.is_empty(), "adapt alone adds no pour rows");
    assert_eq!(a.board.keepouts.len(), 1, "adapt alone adds only the existing track keepout");
    assert_eq!(a.board.keepouts[0].only_net, NONE, "existing track keepouts still affect every net");
    let margin = 20_000;
    let mut ob = a.board.clone();
    let floor = ob.rules.min_clearance.to_bits();
    let before = ob.clr.clone();
    // Deliberately differ from board order: the returned rows follow the caller's zone order.
    let zones = [&board.zones[1], &board.zones[0]];
    let zone_ids = zones.map(|z| z.id);
    let zone_keys = zones.map(|z| RuleKey {
        class: board.nets[z.net.expect("zone net").ix()].class, item: RuleItem::Zone,
    });
    let added = add_pour_rows(&mut ob, &a, &board, &rules, &zone_ids, margin);
    assert_eq!(added.len(), zones.len(), "one batch appends exactly one row per requested zone");
    assert_eq!(ob.clr.rows, before.rows + zones.len(), "the batch expands the clearance table once for all zones");
    for (i, &z) in zones.iter().enumerate() {
        let row = added[i];
        assert_eq!(row as usize, before.rows + i, "pour rows append in caller zone order without renumbering");
        let zkey = zone_keys[i];
        let keys = a.rules.iter().copied().chain(zone_keys.iter().copied());
        for (k, key) in keys.enumerate() {
            for (li, &layer) in a.layers.iter().enumerate() {
                let expected = mm(zone_gap(z, rules.clearance(zkey, key, layer))) + mm(margin);
                assert_eq!(ob.clr.get(row, k as u16, li as i32).to_bits(), expected.to_bits(),
                    "zone row {row} uses row {k}'s key and layer {layer:?}'s clearance plus margin");
                assert_eq!(ob.clr.get(k as u16, row, li as i32).to_bits(), expected.to_bits(),
                    "zone row {row} remains symmetric against row {k} on {layer:?}");
            }
        }
    }
    for i in 0..before.rows {
        for j in 0..before.rows {
            for li in 0..before.layers {
                assert_eq!(ob.clr.get(i as u16, j as u16, li as i32).to_bits(),
                    before.get(i as u16, j as u16, li as i32).to_bits(),
                    "appending pour rows preserves old pair ({i}, {j}, {li}) bit for bit");
            }
        }
    }
    assert_eq!(ob.pour_rules, added, "pour-row membership stays sorted across the whole batch");
    assert_eq!(ob.rules.min_clearance.to_bits(), floor, "pour rows preserve the established clearance floor");
    assert_eq!(ob.clr.max, ob.clr.mm.iter().copied().fold(0.0f32, f32::max),
        "pour rows update the table's maximum query reach");
    let unchanged = format!("{ob:?}");
    assert!(add_pour_rows(&mut ob, &a, &board, &rules, &[], margin).is_empty(),
        "an empty batch returns no new rows");
    assert_eq!(format!("{ob:?}"), unchanged, "an empty batch leaves the entire board unchanged");

    let mut collapsed = a.board.clone();
    let rows = collapsed.clr.rows;
    let mut values = Vec::new();
    for i in 0..rows {
        for j in 0..rows { values.push(collapsed.clr.get(i as u16, j as u16, -1)); }
    }
    collapsed.clr = ClearanceTable { rows, layers: 1, mm: values, max: collapsed.clr.max };
    let added = add_pour_rows(&mut collapsed, &a, &board, &rules, &zone_ids, margin);
    for &row in &added {
        assert_eq!(collapsed.clr.get(row, row, 0), mm(600_000) + mm(margin),
            "a layer-independent table takes the maximum zone clearance over adapter layers");
    }

    // Exercise adapter numbering explicitly, rather than assuming the source NetId is its slot.
    let mut permuted = a.clone();
    permuted.nets.reverse();
    let mut ids: BTreeSet<i32> = a.board.keepouts.iter().map(|k| k.id).collect();
    let mut next = a.board.keepouts.iter().map(|k| k.id).max().unwrap_or(NONE) + 1;
    for z in &board.zones {
        let areas = pour_avoid_areas(&board, &rules, z.id, &permuted, next);
        let masks = static_masks(&board, &rules, z.id);
        let expected: Vec<_> = masks.iter().flat_map(|(layer, set)| set.rings().map(move |ring| (*layer, ring))).collect();
        assert_eq!(areas.len(), expected.len(), "avoid areas retain every outer and hole ring for zone {:?}", z.id);
        assert!(masks.iter().any(|(_, set)| set.polygons().iter().any(|p| !p.holes.is_empty())),
            "avoid-area fixture actually exercises mask holes");
        let net = permuted.nets.iter().position(|n| Some(*n) == z.net).expect("adapter net") as i32;
        for (i, (area, (layer, ring))) in areas.iter().zip(expected).enumerate() {
            assert_eq!(area.id, next + i as i32, "avoid-area ids start at the caller's id and follow ring order");
            assert_eq!(area.only_net, net, "avoid areas use adapter net numbering");
            assert_eq!(area.layer_mask, 1u64 << a.layer_of(layer), "each avoid area affects exactly its mask layer");
            assert_eq!((area.clearance, area.no_tracks, area.no_vias), (0.0, true, false),
                "avoid areas guard tracks at the mask edge without adding clearance or guarding vias");
            assert_eq!(area.poly.iter().copied().map(bits).collect::<Vec<_>>(),
                ring.iter().copied().map(to_ref).map(bits).collect::<Vec<_>>(),
                "avoid-area ring vertices preserve the mask in adapter coordinates");
            assert!(ids.insert(area.id), "threaded avoid-area ids are fresh across the adapter and zone calls");
        }
        next += areas.len() as i32;
    }
    let zone = board.zones[1].id;
    let back_rings: usize = static_masks(&board, &rules, zone).iter()
        .filter(|(layer, _)| *layer == LayerId::B_CU).map(|(_, set)| set.rings().count()).sum();
    assert!(back_rings > 0, "skipped-layer fixture retains back-layer rings after front-layer rings");
    let mut too_many = vec![LayerId::B_CU; 64];
    too_many.push(LayerId::F_CU);
    for layers in [vec![LayerId::B_CU], too_many] {
        let mut filtered = permuted.clone();
        filtered.layers = layers;
        let areas = pour_avoid_areas(&board, &rules, zone, &filtered, next);
        assert_eq!(areas.len(), back_rings, "missing or out-of-range front layers emit no avoid areas");
        for (i, area) in areas.iter().enumerate() {
            assert_eq!(area.layer_mask, 1, "only the adapter's back-layer slot remains");
            assert_eq!(area.id, next + i as i32, "skipped front layers consume no avoid-area ids");
        }
    }
}
