use fab_elec_route::organic::{
    copper,
    engine::{Engine, EngineScratch, Ob, ObKind, Prim},
    geom::{std_max, std_min, Box2, Vec2},
    model::*,
    settings::OrganicSettings,
};
use std::collections::BTreeMap;

#[test]
fn organic_engine_indices_and_essential_contacts() {
    let v = Vec2::new;
    let point = |pos| TracePoint { pos, ..TracePoint::default() };
    let b = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        clr: ClearanceTable { rows: 2, layers: 2, mm: vec![0.2; 8], max: 0.2 },
        outline: vec![v(0.0, 0.0), v(20.0, 0.0), v(20.0, 20.0), v(0.0, 20.0)],
        cutouts: vec![vec![v(14.0, 14.0), v(16.0, 14.0), v(16.0, 16.0), v(14.0, 16.0)]],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        pads: vec![
            Pad {
                id: 10, center: v(3.0, 4.0), size: v(2.0, 2.0), shape: PadShape::Circle,
                is_smd: true, drill_diameter: 0.0, layer_mask: 1, net: 0, rule: 0, ..Pad::default()
            },
            Pad {
                id: 20, center: v(11.0, 4.0), size: v(2.0, 1.0), shape: PadShape::Rect,
                is_smd: true, drill_diameter: 0.0, layer_mask: 2, net: 0, rule: 1, ..Pad::default()
            },
            Pad {
                id: 30, center: v(5.0, 12.0), size: v(3.0, 1.0), shape: PadShape::Oval,
                is_smd: false, drill_diameter: 0.5, layer_mask: 3, net: 1, rule: 1, ..Pad::default()
            },
            Pad {
                id: 40, center: v(12.0, 12.0), size: v(2.0, 2.0), local_size: v(2.0, 1.0),
                rotation: 30.0, shape: PadShape::RoundRect, is_smd: true, drill_diameter: 0.0,
                layer_mask: 1, net: 1, rule: 0, ..Pad::default()
            },
            Pad {
                id: 50, center: v(16.0, 5.0), size: v(2.0, 1.0), shape: PadShape::Polygon,
                polygon: vec![v(0.0, 0.0), v(2.0, 0.0), v(2.0, 1.0), v(0.0, 1.0)],
                is_smd: true, drill_diameter: 0.0, layer_mask: 2, net: 1, rule: 1, ..Pad::default()
            },
        ],
        traces: vec![
            Trace {
                id: 41, net: 0, layer: 0, width: 0.5, rule: 0, is_routed: true,
                points: vec![TracePoint { terminal_pad: 0, ..point(v(3.0, 4.0)) }, point(v(5.0, 4.0)), point(v(7.0, 4.0))],
                ..Trace::default()
            },
            Trace {
                id: 51, net: 0, layer: 1, width: 0.75, rule: 1, is_routed: true,
                points: vec![point(v(7.0, 4.0)), point(v(9.0, 4.0)), TracePoint { terminal_pad: 1, ..point(v(11.0, 4.0)) }],
                ..Trace::default()
            },
            Trace {
                id: 61, net: 1, layer: 0, width: 1.0, rule: 1, is_routed: true,
                points: vec![point(v(7.0, 2.0)), point(v(7.0, 4.0)), point(v(18.0, 10.0))],
                ..Trace::default()
            },
        ],
        vias: vec![
            Via { id: 71, net: 0, pos: v(7.0, 4.0), diameter: 1.0, drill: 0.5, rule: 1, ..Via::default() },
            Via {
                id: 81, net: 1, pos: v(17.0, 17.0), diameter: 1.5, drill: 0.75, rule: 0,
                layer_start: 1, layer_end: 0, ..Via::default()
            },
        ],
        keepouts: vec![Keepout {
            id: 91, poly: vec![v(1.0, 15.0), v(3.0, 15.0), v(3.0, 17.0), v(1.0, 17.0)],
            layer_mask: 2, clearance: 0.25, no_tracks: true, no_vias: true,
            ..Keepout::default()
        }],
        ..OBoard::default()
    };
    let mut engine = Engine::new(&b, &OrganicSettings::base());
    engine.build_static(&b, true);
    engine.build_dynamic(&b, None, 0.0, None);

    assert_eq!((engine.c.bounds.min.x, engine.c.bounds.min.y, engine.c.bounds.max.x, engine.c.bounds.max.y),
        (-2.0, -2.0, 22.0, 22.0), "bounds include board geometry plus the 2 mm border");
    assert_eq!((engine.c.board_center.x, engine.c.board_center.y), (10.0, 10.0), "board centre follows the outline");
    let edge_count = b.outline.len() + b.cutouts.iter().map(Vec::len).sum::<usize>();
    assert_eq!(engine.c.edges.len(), edge_count, "one board edge per outline and cutout side");
    let mut edge_id = 0;
    for ring in std::iter::once(&b.outline).chain(b.cutouts.iter()) {
        for i in 0..ring.len() {
            let (a, c) = engine.c.edges[edge_id];
            assert_eq!((a.x, a.y, c.x, c.y), (ring[i].x, ring[i].y, ring[(i + 1) % ring.len()].x, ring[(i + 1) % ring.len()].y),
                "board edge {edge_id} retains the outline/cutout side and order");
            edge_id += 1;
        }
    }
    assert_eq!(engine.c.ko_edges.len(), 4, "one keepout edge per nondegenerate side");
    for (i, ke) in engine.c.ko_edges.iter().enumerate() {
        assert_eq!((ke.track_layers, ke.via_layers), (2, 2), "keepout edge {i} applies only on the keepout's stack layers");
        assert_eq!(ke.thr, (0.25f32 + engine.s.guard_clearance_slack) as f64, "keepout edge {i} adds slack in f32");
    }
    assert_eq!(engine.c.ko_max_g, engine.c.ko_edges[0].thr, "keepout reach is the maximum threshold");
    assert_eq!(engine.c.seg_base, [0, 2, 4, 6], "segment bases include the final sentinel");
    assert_eq!(engine.c.seg_trace, [0, 0, 1, 1, 2, 2], "every segment maps to its trace slot once");
    assert_eq!(engine.c.seg_idx, [0, 1, 0, 1, 0, 1], "every segment maps to its point index once");
    assert_eq!(engine.c.n_segs, 6, "segment count covers all traces on both layers");
    assert_eq!(engine.c.max_half_w, 0.5, "maximum segment half width");
    assert_eq!(engine.c.max_via_r, 0.75, "maximum via radius");

    // Brute-force source objects, independent of the Engine's indices and cached obstacles.
    let segment_box = |a: Vec2, c: Vec2| Box2::new(v(std_min(a.x, c.x), std_min(a.y, c.y)), v(std_max(a.x, c.x), std_max(a.y, c.y)));
    let mut source = Vec::new();
    for (id, pad) in b.pads.iter().enumerate() {
        let m = copper::pad_model(pad);
        let r = m.ext as f32;
        source.push(Ob {
            kind: ObKind::Pad, id: id as i32, net: pad.net, rule: pad.rule,
            x0: m.center.x - r, y0: m.center.y - r, x1: m.center.x + r, y1: m.center.y + r,
            a: m.center, b: m.center, ..Ob::default()
        });
    }
    let mut id = 0;
    for ring in std::iter::once(&b.outline).chain(b.cutouts.iter()) {
        for i in 0..ring.len() {
            let a = ring[i];
            let c = ring[(i + 1) % ring.len()];
            let q = segment_box(a, c);
            source.push(Ob { kind: ObKind::Edge, id, x0: q.min.x, y0: q.min.y, x1: q.max.x, y1: q.max.y, a, b: c, ..Ob::default() });
            id += 1;
        }
    }
    let mut id = 0;
    for ko in &b.keepouts {
        for i in 0..ko.poly.len() {
            let a = ko.poly[(i + ko.poly.len() - 1) % ko.poly.len()];
            let c = ko.poly[i];
            let q = segment_box(a, c);
            source.push(Ob { kind: ObKind::Ko, id, x0: q.min.x, y0: q.min.y, x1: q.max.x, y1: q.max.y, a, b: c, ..Ob::default() });
            id += 1;
        }
    }
    let mut id = 0;
    for (t, tr) in b.traces.iter().enumerate() {
        for (k, pair) in tr.points.windows(2).enumerate() {
            let a = pair[0].pos;
            let c = pair[1].pos;
            let q = segment_box(a, c);
            source.push(Ob {
                kind: ObKind::Seg, id, net: tr.net, layer: tr.layer, h: tr.width * 0.5,
                trace: t as i32, seg: k as i32, rule: tr.rule, a, b: c,
                x0: q.min.x, y0: q.min.y, x1: q.max.x, y1: q.max.y, ..Ob::default()
            });
            id += 1;
        }
    }
    for (id, via) in b.vias.iter().enumerate() {
        let r = via.diameter * 0.5;
        source.push(Ob {
            kind: ObKind::Via, id: id as i32, net: via.net, h: r, drill: via.drill, rule: via.rule,
            x0: via.pos.x - r, y0: via.pos.y - r, x1: via.pos.x + r, y1: via.pos.y + r,
            a: via.pos, b: via.pos, ..Ob::default()
        });
    }
    let key = |o: &Ob| (o.kind as i32, o.id);
    let fields = |o: &Ob| (
        (o.net, o.layer, o.h, o.trace, o.seg, o.rule, o.drill),
        (o.x0, o.y0, o.x1, o.y1), (o.a.x, o.a.y, o.b.x, o.b.y),
    );
    let check_query = |engine: &Engine, scratch: &mut EngineScratch, q: Box2, layer: i32| {
        engine.gather(&b, scratch, q.min.x, q.min.y, q.max.x, q.max.y, layer, None);
        let mut actual = BTreeMap::new();
        for o in &scratch.obs {
            assert!(actual.insert(key(o), fields(o)).is_none(), "gather deduplicates obstacle {:?} in {q:?}, layer {layer}", key(o));
        }
        let expected: BTreeMap<_, _> = source.iter().filter(|o| {
            (o.kind != ObKind::Seg || layer < 0 || o.layer == layer)
                && Box2::new(v(o.x0, o.y0), v(o.x1, o.y1)).overlaps(&q)
        }).map(|o| (key(o), fields(o))).collect();
        assert_eq!(actual, expected, "gather matches brute-force overlap and all source metadata in {q:?}, layer {layer}");
    };
    let mut scratch = EngineScratch::default();
    let mut seed = 0x83a4_2f19u32;
    let mut random = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / 16_777_216.0f32
    };
    for _ in 0..100 {
        let x = random() * 26.0 - 3.0;
        let y = random() * 26.0 - 3.0;
        let q = Box2::new(v(x, y), v(x + random() * 8.0, y + random() * 8.0));
        for layer in [-1, 0] { check_query(&engine, &mut scratch, q, layer); }
    }
    let whole = Box2::new(v(-3.0, -3.0), v(23.0, 23.0));
    for layer in [-1, 0, 1, 2] { check_query(&engine, &mut scratch, whole, layer); }
    check_query(&engine, &mut scratch, Box2::new(v(7.0, 4.0), v(7.0, 4.0)), -1);
    scratch.stamp = u32::MAX;
    scratch.stamp_s.fill(1);
    scratch.stamp_d.fill(1);
    check_query(&engine, &mut scratch, whole, -1);
    assert_eq!(scratch.stamp, 1, "gather clears stamps when the counter wraps");
    engine.gather_x0(&b, whole.min.x, whole.min.y, whole.max.x, whole.max.y, -1, None);
    assert_eq!(engine.x0.obs.iter().map(fields).collect::<Vec<_>>(), scratch.obs.iter().map(fields).collect::<Vec<_>>(),
        "X0 overload has the same ordered obstacle result");

    // Partial indices must give the full result for queries from each flagged segment.
    let only = [1, 0, 0];
    let boxes: Vec<_> = b.traces.iter().map(|tr| {
        let mut q = Box2::default();
        for p in &tr.points { q.expand(p.pos); }
        q
    }).collect();
    for only_boxes in [None, Some(boxes.as_slice())] {
        engine.build_dynamic(&b, Some(&only), 1.0, only_boxes);
        for pair in b.traces[0].points.windows(2) {
            let mut q = segment_box(pair[0].pos, pair[1].pos);
            q.pad(1.0);
            check_query(&engine, &mut scratch, q, 0);
        }
        assert!(engine.c.region_bits[1].is_empty(), "partial index leaves unflagged layers unmarked");
        assert_eq!(engine.c.seg_base, [0, 2, 4, 6], "partial index retains all segment identities");
    }
    engine.build_dynamic(&b, Some(&[0, 0, 0]), 1.0, Some(&boxes));
    engine.gather(&b, &mut scratch, -3.0, -3.0, 23.0, 23.0, -1, None);
    assert!(!scratch.obs.iter().any(|o| o.kind == ObKind::Seg), "empty region indexes no segments");
    assert_eq!(scratch.obs.iter().filter(|o| o.kind == ObKind::Via).count(), 2, "partial indexing still indexes every via");
    engine.build_dynamic(&b, Some(&[1]), 1.0, None);
    check_query(&engine, &mut scratch, whole, -1);
    engine.build_dynamic(&b, None, 0.0, None);

    engine.compute_essential(&b, None, 0.0);
    assert_eq!(copper::net_pad_groups(&b, 0, None, 1e-3), 0, "two pads are connected by traces through the via");
    let disc = Prim {
        a: b.vias[0].pos, b: b.vias[0].pos, h: 0.5, layer: -1, net: 0, disc: true,
        via_id: 0, obj: copper::via_key(0), rule: b.vias[0].rule, ..Prim::default()
    };
    // compute_essential's last gather need not surround this via: gather its contacts explicitly.
    engine.gather(&b, &mut scratch, 6.0, 3.0, 8.0, 5.0, -1, None);
    let touching: Vec<_> = scratch.obs.iter().filter(|o| o.kind == ObKind::Seg && o.net == 0).copied().collect();
    assert_eq!(touching.len(), 2, "via query sees one touching segment from each layer");
    for o in &touching {
        assert!(engine.is_essential(&disc, o), "same-net via-trace contact on layer {} is essential", o.layer);
        assert!(engine.obj_has_essential(copper::trace_key(o.trace)), "contact marks trace {} as essential", o.trace);
    }
    let foreign = scratch.obs.iter().find(|o| o.kind == ObKind::Seg && o.net == 1).expect("foreign trace touches the via");
    assert!(!engine.is_essential(&disc, foreign), "foreign trace contact is not essential");
    assert!(engine.obj_has_essential(disc.obj), "contact marks the via as essential");
    assert!(!engine.obj_has_essential(-1), "missing object has no essential contact");
    let previous = engine.c.ess_by_net.clone();
    engine.compute_essential(&b, Some(&[1]), 0.0);
    assert_eq!(engine.c.ess_by_net.get(&0), previous.get(&0), "selective essential refresh preserves other nets");
    for o in &touching { assert!(engine.is_essential(&disc, o), "selective refresh retains via contacts of unselected nets"); }

    let p = Prim { a: v(1.0, 7.0), b: v(8.0, 7.0), h: 0.25, layer: 0, ..Prim::default() };
    let pad = Ob { kind: ObKind::Pad, id: 0, ..Ob::default() };
    let via = Ob { kind: ObKind::Via, id: 0, ..Ob::default() };
    assert_eq!(engine.contact_gap(&b, &p, &pad), Some(1.75), "segment-pad copper gap is 3 - 1 - 0.25 mm");
    assert_eq!(engine.contact_gap(&b, &p, &via), Some(2.25), "segment-via copper gap is 3 - 0.5 - 0.25 mm");
    let other_layer = Prim { layer: 1, ..p };
    assert_eq!(engine.contact_gap(&b, &other_layer, &pad), None, "pad on another layer has no contact gap");
    let layer_zero = touching.iter().find(|o| o.layer == 0).expect("contact on layer zero");
    assert_eq!(engine.contact_gap(&b, &other_layer, layer_zero), None, "segment on another layer has no contact gap");
    assert!(engine.disc_on_pad(&b, &disc, &b.pads[1]), "through via reaches the bottom pad");
    assert!(engine.disc_on_pad(&b, &disc, &b.pads[2]), "through via reaches the THT pad");
    let mut span_board = b.clone();
    span_board.vias[0].layer_end = 0;
    assert!(!engine.disc_on_pad(&span_board, &disc, &b.pads[1]), "known via span excludes the bottom pad");

    scratch.obs = touching;
    scratch.c_old = scratch.obs.iter().map(|o| engine.contact_gap(&b, &disc, o).expect("disc has a copper gap")).collect();
    let n_o = scratch.obs.len();
    assert!(engine.contacts_kept(&b, &mut scratch, &[disc], 1, n_o), "unchanged essential contacts are kept");
    let shallow = Prim { a: disc.a + v(0.0, 0.25), b: disc.b + v(0.0, 0.25), ..disc };
    assert!(engine.contacts_kept(&b, &mut scratch, &[shallow], 1, n_o), "deep overlap may relax while contact remains");
    let separated = Prim { a: disc.a + v(0.0, 1.5), b: disc.b + v(0.0, 1.5), ..disc };
    assert!(!engine.contacts_kept(&b, &mut scratch, &[separated], 1, n_o), "opening an essential contact is rejected");
    assert!(engine.contacts_kept(&b, &mut scratch, &[separated, disc], 1, n_o), "contact gap is the minimum across one owner's primitives");
}
