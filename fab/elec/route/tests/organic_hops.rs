use fab_elec_route::organic::{
    geom::Vec2,
    model::{LayerInfo, LayerIntent, Net, OBoard, Pad, Trace, TracePoint, Via},
    opt::hops::{count_hops, CountedHop, HopCount},
};

#[test]
fn organic_hops_counts_real_pseudo_and_reports_deterministically() {
    let v = Vec2::new;
    let line = |id, a: Vec2, b: Vec2| Trace {
        id,
        net: 0,
        layer: 0,
        width: 0.2,
        points: vec![
            TracePoint { pos: a, width: 0.2, ..TracePoint::default() },
            TracePoint { pos: b, width: 0.2, ..TracePoint::default() },
        ],
        smoothed: vec![a, (a + b) * 0.5, b],
        is_routed: true,
        ..Trace::default()
    };
    let base = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        nets: vec![Net { id: 0, ..Net::default() }],
        vias: vec![
            Via { id: 17, net: 0, pos: v(0.0, 0.0), diameter: 0.6, drill: 0.3, ..Via::default() },
            Via { id: 42, net: 0, pos: v(2.0, 0.0), diameter: 0.6, drill: 0.3, ..Via::default() },
        ],
        traces: vec![line(0, v(0.0, 0.0), v(2.0, 0.0))],
        ..OBoard::default()
    };
    let bits = |p: Vec2| (p.x.to_bits(), p.y.to_bits());
    let detail_bits = |detail: &[CountedHop]| {
        detail.iter().map(|h| (h.via_a, h.via_b, bits(h.at), h.pseudo)).collect::<Vec<_>>()
    };
    let where_bits = |where_: &[Vec2]| where_.iter().copied().map(bits).collect::<Vec<_>>();
    let check_outputs = |name: &str, board: &OBoard, count: HopCount, where_: &[Vec2], detail: &[CountedHop]| {
        // ViaHops.h:231-232 sums the two classes; ViaHops.cpp:2061-2070
        // increments one class and appends exactly one item to each output.
        assert_eq!(count.all(), count.real + count.pseudo, "{name}: all sums real and pseudo");
        assert_eq!(where_.len(), count.all() as usize, "{name}: where length agrees with total count");
        assert_eq!(detail.len(), count.all() as usize, "{name}: detail length agrees with total count");
        assert_eq!(detail.iter().filter(|h| !h.pseudo).count(), count.real as usize,
            "{name}: real detail entries agree with real count");
        assert_eq!(detail.iter().filter(|h| h.pseudo).count(), count.pseudo as usize,
            "{name}: pseudo detail entries agree with pseudo count");
        for (at, h) in where_.iter().zip(detail) {
            // ViaHops.cpp:2066-2068 sorts the via indices and records their
            // f32 midpoint, also appended to where at line 2062.
            assert_eq!((h.via_a, h.via_b), (0, 1), "{name}: details use sorted board indices, not via ids");
            let midpoint = (board.vias[0].pos + board.vias[1].pos) * 0.5f32;
            assert_eq!(bits(h.at), bits(midpoint), "{name}: detail records the via midpoint");
            assert_eq!(bits(*at), bits(h.at), "{name}: where and detail midpoints agree bit for bit");
        }
    };

    // ViaHops.cpp:1993 attaches both endpoints to their respective via discs;
    // 2035 accepts 2 < 3 mm; 2050 returns true at each degree-one endpoint;
    // 2060-2061 therefore increments real once, with the reverse edge skipped
    // by seen at 2010-2012. Raw and smoothed polylines are the same straight run
    // (1887-1897), with the smoothed version also containing a midpoint sample.
    let mut where_ = vec![v(99.0, 99.0)];
    let mut detail = vec![CountedHop::default()];
    let real = count_hops(&base, 3.0, false, Some(&mut where_), Some(&mut detail), false);
    assert_eq!((real.real, real.pseudo), (1, 0), "short straight run: exactly one real hop");
    check_outputs("real", &base, real, &where_, &detail);
    let raw_where = where_bits(&where_);
    let raw_detail = detail_bits(&detail);
    let smoothed = count_hops(&base, 3.0, true, Some(&mut where_), Some(&mut detail), false);
    assert_eq!(smoothed, real, "straight run: raw and smoothed counts agree");
    assert_eq!(where_bits(&where_), raw_where, "straight run: raw and smoothed locations agree");
    assert_eq!(detail_bits(&detail), raw_detail, "straight run: raw and smoothed details agree");

    // ViaHops.cpp:2035 uses strict <, so a 4 mm chain at limit 3 and a 2 mm
    // chain at limit 2 each count zero. Lines 1883-1884 clear prior outputs.
    let mut long = base.clone();
    long.vias[1].pos = v(4.0, 0.0);
    long.traces = vec![line(0, long.vias[0].pos, long.vias[1].pos)];
    let none = count_hops(&long, 3.0, false, Some(&mut where_), Some(&mut detail), false);
    assert_eq!((none.real, none.pseudo), (0, 0), "long run: neither real nor pseudo is counted");
    check_outputs("long run clears outputs", &long, none, &where_, &detail);
    let equal = count_hops(&base, 2.0, false, None, None, false);
    assert_eq!(equal.all(), 0, "length boundary: equality with the limit is excluded");

    // ViaHops.cpp:1993 attaches (0, 0.2) and (2, 0.2) since 0.2 <= 0.3 + 0.1.
    // Three separate traces keep these graph vertices after segmentsOf. Each
    // attached vertex has degree two (1953-1957), is 0.2 > 1e-3 from its via
    // centre (2050), and has no further same-via attached node away from the
    // middle chain (2051-2058). Thus endsOn is false and 2060-2061 counts one
    // pseudo, zero real. The two outer chains each have an unattached end and
    // fail 2035; only the 2 mm middle chain qualifies.
    let mut pseudo = base.clone();
    pseudo.traces = vec![
        line(0, v(-1.0, 0.2), v(0.0, 0.2)),
        line(1, v(0.0, 0.2), v(2.0, 0.2)),
        line(2, v(2.0, 0.2), v(3.0, 0.2)),
    ];
    let pass_over = count_hops(&pseudo, 3.0, false, Some(&mut where_), Some(&mut detail), false);
    assert_eq!((pass_over.real, pass_over.pseudo), (0, 1), "pass-over run: exactly one pseudo hop");
    check_outputs("pseudo", &pseudo, pass_over, &where_, &detail);
    let first_where = where_bits(&where_);
    let first_detail = detail_bits(&detail);
    // All loops at ViaHops.cpp:1930-2074 follow board/adjacency order, and
    // 1883-1884 reset both outputs: repeating this input yields identical data.
    let again = count_hops(&pseudo, 3.0, false, Some(&mut where_), Some(&mut detail), false);
    assert_eq!(again, pass_over, "repeatability: identical input gives identical counts");
    assert_eq!(where_bits(&where_), first_where, "repeatability: locations are bit-identical and not appended");
    assert_eq!(detail_bits(&detail), first_detail, "repeatability: details are bit-identical and not appended");
    assert_eq!(count_hops(&pseudo, 3.0, false, None, None, false), pass_over,
        "optional outputs: omitting both vectors preserves counts");

    // ViaHops.cpp:2032-2035 adds 0.2 + 0.2 to the middle chain only when
    // viaCentreLength is true: 2 < 2.2 qualifies, 2.4 < 2.2 does not.
    assert_eq!(count_hops(&pseudo, 2.2, false, None, None, false).pseudo, 1,
        "chain length: pass-over chain is below 2.2 mm");
    assert_eq!(count_hops(&pseudo, 2.2, false, None, None, true).all(), 0,
        "centre length: both via-centre offsets exceed the 2.2 mm limit");

    // ADAPTED at ViaHops.cpp:1992-1993: copper::via_on_layer excludes a via
    // absent from the trace layer, so 2035 no longer has two attached vias.
    let mut span = base.clone();
    span.vias[1].layer_start = 1;
    span.vias[1].layer_end = 1;
    assert_eq!(count_hops(&span, 3.0, false, None, None, false).all(), 0,
        "via layers: a via on another layer cannot form a hop");

    // ViaHops.cpp:1983-1989 gives pads precedence over vias, so 2035 fails
    // when a pad occupies the first endpoint. ADAPTED at 1969: membership
    // follows copper stack mask bits; a back-only pad leaves this front hop.
    let mut padded = base.clone();
    padded.pads.push(Pad {
        id: 0, net: 0, center: v(0.0, 0.0), size: v(0.6, 0.6),
        drill_diameter: 0.0, is_smd: true, layer_mask: 2, ..Pad::default()
    });
    assert_eq!(count_hops(&padded, 3.0, false, None, None, false).real, 1,
        "pad layers: a back-only pad leaves the front-layer hop intact");
    padded.pads[0].layer_mask = 1;
    assert_eq!(count_hops(&padded, 3.0, false, None, None, false).all(), 0,
        "pad precedence: an on-layer pad prevents attachment to the covered via");
}
