use fab_elec_route::organic::{
    copper,
    geom::Vec2,
    model::{DesignRules, Keepout, LayerInfo, LayerIntent, Net, OBoard, Pad, PadShape, Via, ViaType},
    opt::drcgeom::{self as drc, D2},
};

#[test]
fn organic_drc_geometry_and_layers() {
    let v = Vec2::new;
    // Drc.cpp:53-61: opposite orientations on both segments give distance 0.
    // For the other cases, cpp:43-48 projects with t=clamp(dot/l2, 0, 1),
    // and cpp:61 takes the first minimum of the four endpoint distances:
    // parallel: sqrt(0^2+3^2)=3; collinear: sqrt((6-4)^2)=2;
    // endpoint/interior: t=2/4, q=(2,0), sqrt(0^2+2^2)=2.
    for (property, a, b, c, d, expected) in [
        ("crossing segments", v(0.0, 0.0), v(4.0, 0.0), v(2.0, -1.0), v(2.0, 1.0), 0.0),
        ("parallel segments", v(0.0, 0.0), v(4.0, 0.0), v(0.0, 3.0), v(4.0, 3.0), 3.0),
        ("collinear apart", v(0.0, 0.0), v(4.0, 0.0), v(6.0, 0.0), v(8.0, 0.0), 2.0),
        ("endpoint to interior", v(0.0, 0.0), v(4.0, 0.0), v(2.0, 2.0), v(2.0, 4.0), 2.0),
    ] {
        assert_eq!(drc::segment_segment_distance(a, b, c, d), expected, "segment distance: {property}");
    }
    // Drc.cpp:43-48: ab=(4,0), l2=16, t=8/16; q=(2,0), distance=3.
    let mut closest = D2::default();
    assert_eq!(drc::point_seg(drc::d2(v(2.0, 3.0)), drc::d2(v(0.0, 0.0)), drc::d2(v(4.0, 0.0)), Some(&mut closest)),
        3.0, "point segment: optional closest distance");
    assert_eq!((closest.x, closest.y), (2.0, 0.0), "point segment: optional closest coordinates");
    // Drc.cpp:45-48: l2=0 selects t=0, q=a=(0,0); sqrt(3^2+4^2)=5.
    assert_eq!(drc::point_seg(drc::d2(v(3.0, 4.0)), D2::default(), D2::default(), None),
        5.0, "point segment: zero-length segment");

    let rect = Pad { shape: PadShape::Rect, size: v(4.0, 2.0), local_size: v(4.0, 2.0), ..Pad::default() };
    let circle = Pad { shape: PadShape::Circle, size: v(2.0, 2.0), ..Pad::default() };
    let rotated = Pad { center: v(2.0, -1.0), rotation: 90.0, size: v(2.0, 4.0), ..rect.clone() };
    // Drc.cpp:65-67,426-427 delegate to PadGeometry.h:209-230, then cast to f32.
    // Rect: h=(2,1), r=0 (PadGeometry.h:54-55,68). For local p=(5,5),
    // pointBoxLocal (h:139-142) gives sqrt((5-2)^2+(5-1)^2)=5.
    // Circle: h=(1,1), r=1 (h:67), inner box=(0,0): sqrt(3^2+4^2)-1=4.
    // Rotated: h:58-63 gives c=0,s=1; h:102-104 maps board (7,-6)
    // about (2,-1) to local (5,5), so the same rectangle distance is 5.
    for (property, pad, point, expected) in [
        ("rectangular pad", &rect, v(5.0, 5.0), 5.0),
        ("circular pad", &circle, v(3.0, 4.0), 4.0),
        ("rotated pad", &rotated, v(7.0, -6.0), 5.0),
    ] {
        assert_eq!(drc::point_pad_distance(point, pad), expected, "point pad distance: {property}");
    }
    // PadGeometry.h:145-157,225: the local segment (-3,3)..(3,3) is 3-1=2
    // from the rectangle's top edge. For the circle, h:149 projects its centre
    // to (0,3) and h:225 subtracts r=1, again 3-1=2. Rotated board endpoints
    // (5,2)..(5,-4) map to (-3,3)..(3,3) by h:102-104.
    for (property, pad, a, b) in [
        ("rectangular pad", &rect, v(-3.0, 3.0), v(3.0, 3.0)),
        ("circular pad", &circle, v(-3.0, 3.0), v(3.0, 3.0)),
        ("rotated pad", &rotated, v(5.0, 2.0), v(5.0, -4.0)),
    ] {
        assert_eq!(drc::segment_pad_distance(a, b, pad), 2.0, "segment pad distance: {property}");
    }
    // PadGeometry.h:214,225: centre/core distance 0 minus r=1 is -1 for a
    // circle; r=0 gives 0 inside a rectangle (not its true signed depth).
    assert_eq!(drc::point_pad_distance(v(0.0, 0.0), &circle), -1.0, "point pad distance: circle interior sign");
    assert_eq!(drc::segment_pad_distance(v(-2.0, 0.0), v(2.0, 0.0), &circle), -1.0,
        "segment pad distance: circle crossing sign");
    assert_eq!(drc::point_pad_distance(v(0.0, 0.0), &rect), 0.0, "point pad distance: rectangle interior");

    let keepout = Keepout {
        poly: vec![v(0.0, 0.0), v(2.0, 0.0), v(2.0, 2.0), v(0.0, 2.0)],
        layer_mask: 1_u64 << 2, clearance: 0.25, ..Keepout::default()
    };
    // Drc.cpp:75-79: at (1,1) only x=2 toggles the ray, so inside=true;
    // at (5,1) neither x=0 nor x=2 does, so inside=false.
    assert!(drc::point_in_keepout(&keepout, v(1.0, 1.0)), "keepout containment: interior");
    assert!(!drc::point_in_keepout(&keepout, v(5.0, 1.0)), "keepout containment: exterior");
    // Drc.cpp:515-528: an inside endpoint returns 0; outside (5,1) projects
    // to (2,1) on the nearest edge, sqrt((5-2)^2)=3. A crossing segment hits
    // x=0 and x=2 properly, so segSeg returns 0 at cpp:60.
    assert_eq!(drc::point_keepout_distance(&keepout, v(1.0, 1.0)), 0.0, "keepout distance: inside is zero");
    assert_eq!(drc::point_keepout_distance(&keepout, v(5.0, 1.0)), 3.0, "keepout distance: outside to edge");
    assert_eq!(drc::segment_keepout_distance(&keepout, v(-1.0, 1.0), v(3.0, 1.0)), 0.0,
        "keepout distance: segment crosses polygon");
    // Drc.h:347 uses the same zero-length segment: (5,1) to x=2 is 3.
    assert_eq!(drc::point_polygon_distance(&keepout.poly, v(5.0, 1.0)), 3.0, "polygon distance: point wrapper");
    // Drc.cpp:550-558: pad centre (4,1) is outside, and its left copper edge
    // is x=3, one unit from the keepout's x=2 edge. Moving its centre inside
    // triggers cpp:554's zero; enclosing the polygon triggers cpp:558's zero.
    let outside_pad = Pad { center: v(4.0, 1.0), size: v(2.0, 2.0), local_size: v(2.0, 2.0), ..rect.clone() };
    assert_eq!(drc::pad_keepout_distance(&keepout, &outside_pad), 1.0, "pad keepout distance: separate copper");
    let inside_pad = Pad { center: v(1.0, 1.0), ..outside_pad.clone() };
    assert_eq!(drc::pad_keepout_distance(&keepout, &inside_pad), 0.0, "pad keepout distance: centre inside");
    let enclosing_pad = Pad { size: v(10.0, 4.0), local_size: v(10.0, 4.0), ..outside_pad };
    assert_eq!(drc::pad_keepout_distance(&keepout, &enclosing_pad), 0.0, "pad keepout distance: polygon inside pad");
    // Drc.h:337: positive 0.25 is retained; negative, -0 and NaN fail >0
    // and return the literal +0 (bits=0), not std_max(clearance, 0).
    assert_eq!(drc::keepout_margin(&keepout), 0.25, "keepout margin: positive clearance");
    for clearance in [-0.25, -0.0, f32::NAN] {
        let k = Keepout { clearance, ..Keepout::default() };
        assert_eq!(drc::keepout_margin(&k).to_bits(), 0, "keepout margin: nonpositive or NaN gives positive zero");
    }
    // Drc.cpp:71 returns true for an empty ring, while cpp:511 returns false
    // and cpp:521,553 return the 1e30f sentinel before any geometry access.
    let empty = Keepout { poly: Vec::new(), ..Keepout::default() };
    assert!(drc::point_in_poly(&empty.poly, D2::default()), "short polygon: anonymous containment convention");
    assert!(!drc::point_in_keepout(&empty, v(0.0, 0.0)), "short polygon: keepout containment rejects");
    assert_eq!(drc::segment_keepout_distance(&empty, v(0.0, 0.0), v(1.0, 0.0)), 1e30f32,
        "short polygon: segment distance sentinel");
    assert_eq!(drc::pad_keepout_distance(&empty, &rect), 1e30f32, "short polygon: pad distance sentinel");

    let board = OBoard {
        layers: vec![
            LayerInfo { index: 0, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 1, intent: LayerIntent::GroundPlane, plane_net: 0 },
            LayerInfo { index: 2, intent: LayerIntent::Signal, plane_net: -1 },
            LayerInfo { index: 3, intent: LayerIntent::Signal, plane_net: -1 },
        ],
        nets: vec![Net { id: 0, ..Net::default() }, Net { id: 1, ..Net::default() }],
        rules: DesignRules { min_clearance: 0.375, ..DesignRules::default() },
        ..OBoard::default()
    };
    // Drc.cpp:503-506: usable ring and mask & bit != 0. In OBoard the named
    // layer becomes stack bit 1<<layer, so mask 1<<2 selects only index 2.
    // Drc.cpp:435-437 translates front/inner/back to stack bits 1,2,4,8.
    for layer in 0..4 {
        assert_eq!(drc::keepout_on_layer(&board, &keepout, layer), layer == 2,
            "keepout layer mask: only the named stack layer {layer}");
        assert_eq!(drc::layer_mask_for_index(layer, 4), 1_u64 << layer, "layer mask: stack bit {layer}");
    }
    // Drc.cpp:503: fewer than three vertices returns false regardless of mask.
    assert!(!drc::keepout_on_layer(&board, &empty, 0), "keepout layer mask: short ring rejected");

    // Drc.cpp:362,368-369: a front SMD pad is copper on layer 0 only and a through pad on
    // every layer; the port's pads carry those layers as stack bits (model.rs).
    let smd = Pad { is_smd: true, layer_mask: 1, ..rect.clone() };
    let through = Pad { is_smd: false, layer_mask: 0b1111, ..rect.clone() };
    for layer in 0..4 {
        assert_eq!(drc::pad_on_layer(&smd, layer, 4), layer == 0, "pad layers: front SMD pad on stack layer {layer}");
        assert!(drc::pad_on_layer(&through, layer, 4), "pad layers: through pad on stack layer {layer}");
    }

    let blind = Via { kind: ViaType::Blind, layer_start: 1, layer_end: 0, ..Via::default() };
    // Drc.cpp:374-375: min(1,0)=0, max(1,0)=1; inclusive 0<=layer<=1.
    for layer in 0..4 {
        assert_eq!(drc::via_on_layer(&blind, layer), layer <= 1, "blind via span: stack layer {layer}");
    }
    // Drc.cpp:395-397: only index 1 has ground/power intent. cpp:402 rejects
    // other layers; adapted cpp:403-415 reads its resolved owner 0. cpp:420-423
    // returns the first such layer (1), and -1 for negative or unowned nets.
    for layer in 0..4 {
        assert_eq!(drc::is_plane_layer(&board, layer), layer == 1, "plane intent: stack layer {layer}");
        assert_eq!(drc::plane_net(&board, layer), if layer == 1 { 0 } else { -1 }, "plane owner: stack layer {layer}");
    }
    assert!(!drc::is_plane_layer(&board, -1) && !drc::is_plane_layer(&board, 4), "plane intent: invalid layer rejected");
    assert_eq!(drc::plane_net(&board, -1), -1, "plane owner: invalid layer has no net");
    assert_eq!(drc::plane_layer_of_net(&board, 0), 1, "plane lookup: ground net owns layer one");
    assert_eq!(drc::plane_layer_of_net(&board, 1), -1, "plane lookup: signal net has no plane");
    assert_eq!(drc::plane_layer_of_net(&board, -1), -1, "plane lookup: negative net has no plane");
    // copper.rs:236-239 uses the same intent/owner test and includes layer 1.
    let mut planes = Vec::new();
    copper::plane_layers_of(&board, 0, &mut planes);
    assert_eq!(planes, vec![1], "plane lookup: agrees with existing copper membership");
    // Drc.h:379 returns rules.minClearance unchanged, here the exact 3/8 mm.
    assert_eq!(drc::via_pad_clearance(&board), 0.375, "via pad clearance: board rule unchanged");
}
