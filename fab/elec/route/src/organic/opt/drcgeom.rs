// Ported from TiNRS Psychedelic Router 0.1.0: src/core/Drc.h (307-321, 330-347, 353, 379), src/core/Drc.cpp (30-83, 360-376, 393-438, 501-529, 550-559).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in the parent folder.

//! Drc's geometry kernels shared by the finish passes; Fab supplies the rule engine.
//!
//! ADAPTED: OBoard uses u64 copper stack-index masks, not KiCad named bits, and
//! LayerInfo::plane_net stores resolved plane ownership instead of net-name heuristics.
//! C++ optional output pointers are Option<&mut T>; default arguments are explicit.
//!
//! ADAPTED: padOnLayer (Drc.cpp:360-370) is copper::pad_on_layer: a port pad is one copper
//! shape on the layers of its stack mask (model.rs), so the SMD front/back test is the mask's
//! bit, and the K2 locked-anchor pads it also answers do not arise (Fab's locked copper stays
//! tracks and vias).
//!
//! Not ported: copperShapeNet / copperShapeOnLayer need CopperShape, absent from OBoard.
//! viaInPadAllowed is answered by the engine's via_in_pad_ok. lockedAnchor* need
//! the omitted anchor/name fields; is*NetName is replaced by stored plane_net.
//! padsTouch belongs to DRC connectivity, kindName to diagnostic formatting, and
//! the report/run code (including zone/placement checks) is replaced by Fab's DRC.
//!
//! Keep these separate from NeckdownTeardrop's pointInPoly (f32 subtraction,
//! false for short polygons), geom::Segment2 (f32), and copper::seg_seg (reciprocal
//! multiplication and squared-distance minima). Drc divides and takes each root.

use super::super::copper;
use super::super::geom::{std_clamp, std_max, std_min, Vec2};
use super::super::model::{Keepout, LayerIntent, OBoard, Pad, Via};
use super::super::padgeom;

/// Drc.cpp:30-33: double-precision coordinates behind Drc.h:319-321, 340-347.
#[derive(Copy, Clone, Debug, Default)]
pub struct D2 {
    pub x: f64,
    pub y: f64,
}

/// Drc.h:321's coordinate conversion; anonymous Drc.cpp:34 (cast before arithmetic).
pub fn d2(v: Vec2) -> D2 { D2 { x: v.x as f64, y: v.y as f64 } }

/// Drc.h:321's vector difference; anonymous Drc.cpp:35.
pub fn sub(a: D2, b: D2) -> D2 { D2 { x: a.x - b.x, y: a.y - b.y } }

/// Drc.h:321's dot product; anonymous Drc.cpp:36.
pub fn dot(a: D2, b: D2) -> f64 { a.x * b.x + a.y * b.y }

/// Drc.h:321's cross product; anonymous Drc.cpp:37.
pub fn crs(a: D2, b: D2) -> f64 { a.x * b.y - a.y * b.x }

/// Drc.h:321's vector length; anonymous Drc.cpp:38.
pub fn len(a: D2) -> f64 { dot(a, a).sqrt() }

/// Drc.h:321's orientation kernel; anonymous Drc.cpp:39.
pub fn orient(a: D2, b: D2, c: D2) -> f64 { crs(sub(b, a), sub(c, a)) }

/// Drc.h:321's point/segment kernel; anonymous Drc.cpp:41-49, closest defaults to None.
pub fn point_seg(p: D2, a: D2, b: D2, closest: Option<&mut D2>) -> f64 {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    let t = if l2 < 1e-24 { 0.0 } else { std_clamp(dot(sub(p, a), ab) / l2, 0.0, 1.0) };
    let q = D2 { x: a.x + ab.x * t, y: a.y + ab.y * t };
    if let Some(closest) = closest { *closest = q; }
    len(sub(p, q))
}

/// Drc.h:321's proper-crossing kernel; anonymous Drc.cpp:51-56.
pub fn seg_cross(a: D2, b: D2, c: D2, d: D2) -> bool {
    let d1 = orient(c, d, a);
    let d2v = orient(c, d, b);
    let d3 = orient(a, b, c);
    let d4 = orient(a, b, d);
    ((d1 > 0.0 && d2v < 0.0) || (d1 < 0.0 && d2v > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

/// Drc.h:321's segment/segment kernel; anonymous Drc.cpp:58-62.
pub fn seg_seg(a: D2, b: D2, c: D2, d: D2) -> f64 {
    if seg_cross(a, b, c, d) { return 0.0; }
    // std::min(initializer_list) retains the first minimum, including NaN/zero ties.
    std_min(std_min(std_min(point_seg(a, c, d, None), point_seg(b, c, d, None)),
        point_seg(c, a, b, None)), point_seg(d, a, b, None))
}

/// Drc.h:319's double-precision pad kernel; anonymous Drc.cpp:65.
pub fn point_pad(p: D2, pad: &Pad) -> f64 {
    // Checked line by line: PadGeometry.h:209-216 == padgeom.rs:206-214,
    // including frame/transform/box/polygon callees (h:31-204, rs:10-203).
    // Same casts, branches, arithmetic order and first-minimum semantics.
    padgeom::point_distance_pad(pad, p.x, p.y)
}

/// Drc.h:320's double-precision pad kernel; anonymous Drc.cpp:67.
pub fn seg_pad(a: D2, b: D2, pad: &Pad) -> f64 {
    // Checked line by line: PadGeometry.h:219-230 == padgeom.rs:217-226;
    // its supporting kernels also match, as recorded at point_pad. One definition.
    padgeom::segment_distance_pad(pad, a.x, a.y, b.x, b.y)
}

/// Drc.h:353's even-odd kernel; anonymous Drc.cpp:69-83 (true for fewer than 3 vertices).
pub fn point_in_poly(poly: &[Vec2], p: D2) -> bool {
    if poly.len() < 3 { return true; }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let a = d2(poly[i]);
        let b = d2(poly[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let xi = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
            if p.x < xi { inside = !inside; }
        }
        j = i;
    }
    inside
}

/// Drc.h:307; Drc.cpp:360-370, ADAPTED (see the module notes): the pad's copper is on `layer`.
pub fn pad_on_layer(pad: &Pad, layer: i32, n_layers: i32) -> bool {
    copper::pad_on_layer(pad, layer, n_layers)
}

/// Drc.h:308; Drc.cpp:372-376: inclusive span, independent of endpoint order.
pub fn via_on_layer(v: &Via, layer: i32) -> bool {
    // Line-by-line match: cpp:374 min/max == copper.rs:226-227;
    // cpp:375 inclusive comparisons == copper.rs:228. Reuse the one definition.
    copper::via_on_layer(v, layer)
}

/// Drc.h:309; Drc.cpp:393-398: ground/power plane intent at a valid stack index.
pub fn is_plane_layer(board: &OBoard, layer: i32) -> bool {
    if layer < 0 || layer >= board.layers.len() as i32 { return false; }
    let li = board.layers[layer as usize].intent;
    li == LayerIntent::GroundPlane || li == LayerIntent::PowerPlane
}

/// Drc.h:313; Drc.cpp:400-416: resolved plane owner, or -1 for non-plane layers.
pub fn plane_net(board: &OBoard, layer: i32) -> i32 {
    if !is_plane_layer(board, layer) { return -1; }
    // ADAPTED: cpp:403-415's net-name/pad-count selection is resolved by the adapter.
    // Same intent/owner predicate as copper::plane_layers_of (copper.rs:237-238).
    board.layers[layer as usize].plane_net
}

/// Drc.h:315; Drc.cpp:418-424: first plane of net in stack order, or -1.
pub fn plane_layer_of_net(board: &OBoard, net_id: i32) -> i32 {
    if net_id < 0 { return -1; }
    // Unlike copper::plane_layers_of, return only the first match and preserve
    // Drc's negative-only net guard (copper also rejects net >= board.nets.len()).
    for l in 0..board.layers.len() as i32 {
        if plane_net(board, l) == net_id { return l; }
    }
    -1
}

/// Drc.h:319; Drc.cpp:426: point to copper, casting the double result back to float.
pub fn point_pad_distance(p: Vec2, pad: &Pad) -> f32 { point_pad(d2(p), pad) as f32 }

/// Drc.h:320; Drc.cpp:427: segment to copper, with the reference's sign convention.
pub fn segment_pad_distance(a: Vec2, b: Vec2, pad: &Pad) -> f32 { seg_pad(d2(a), d2(b), pad) as f32 }

/// Drc.h:321; Drc.cpp:428-431: segment distance, casting only the final double result.
pub fn segment_segment_distance(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f32 {
    seg_seg(d2(a), d2(b), d2(c), d2(d)) as f32
}

/// Drc.h:330; Drc.cpp:433-438: front/back fallback and the inner-layer bit.
pub fn layer_mask_for_index(i: i32, n: i32) -> u64 {
    // ADAPTED: named F.Cu/B.Cu become stack bits 0/(n-1), with u64 masks.
    // Preserve both reference guards; an unrepresentable stack bit is zero.
    if i <= 0 { return 1_u64; }
    if i >= n - 1 { return 1_u64.checked_shl((n - 1) as u32).unwrap_or(0); }
    1_u64.checked_shl(i as u32).unwrap_or(0)
}

/// Drc.h:335; Drc.cpp:501-507: a usable keepout ring with the queried layer bit.
pub fn keepout_on_layer(board: &OBoard, k: &Keepout, layer_index: i32) -> bool {
    if k.poly.len() < 3 { return false; }
    // ADAPTED: Board::layerMaskForIndex resolves named layer bits; OBoard already
    // uses stack bits. Keep the valid-index query and zero-bit fallback order.
    let mut bit = if layer_index < 0 || layer_index >= board.layers.len() as i32 {
        0
    } else {
        1_u64.checked_shl(layer_index as u32).unwrap_or(0)
    };
    if bit == 0 { bit = layer_mask_for_index(layer_index, std_max(1, board.layers.len() as i32)); }
    k.layer_mask & bit != 0
}

/// Drc.h:353; Drc.cpp:509-513: even-odd containment, false for an unusable ring.
pub fn point_in_keepout(k: &Keepout, p: Vec2) -> bool {
    if k.poly.len() < 3 { return false; }
    point_in_poly(&k.poly, d2(p))
}

/// Drc.h:340; Drc.cpp:515: point to filled keepout area, zero inside.
pub fn point_keepout_distance(k: &Keepout, p: Vec2) -> f32 { segment_keepout_distance(k, p, p) }

/// Drc.h:341; Drc.cpp:517: segment centre line to filled keepout area.
pub fn segment_keepout_distance(k: &Keepout, a: Vec2, b: Vec2) -> f32 {
    segment_polygon_distance(&k.poly, a, b)
}

/// Drc.h:346; Drc.cpp:519-529: filled polygon distance, 1e30f for an unusable ring.
pub fn segment_polygon_distance(poly: &[Vec2], a: Vec2, b: Vec2) -> f32 {
    if poly.len() < 3 { return 1e30f32; }
    if point_in_poly(poly, d2(a)) || point_in_poly(poly, d2(b)) { return 0.0f32; }
    let mut best = 1e300f64;
    let a = d2(a);
    let b = d2(b);
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        best = std_min(best, seg_seg(a, b, d2(poly[j]), d2(poly[i])));
        j = i;
    }
    best as f32
}

/// Drc.h:347: point to filled polygon via a zero-length segment.
pub fn point_polygon_distance(poly: &[Vec2], p: Vec2) -> f32 { segment_polygon_distance(poly, p, p) }

/// Drc.h:337: positive keepout margin, otherwise positive zero (including NaN).
pub fn keepout_margin(k: &Keepout) -> f32 { if k.clearance > 0.0f32 { k.clearance } else { 0.0f32 } }

/// Drc.h:343; Drc.cpp:550-559: true pad copper to keepout area, zero on overlap.
pub fn pad_keepout_distance(k: &Keepout, pad: &Pad) -> f32 {
    let poly = &k.poly;
    if poly.len() < 3 { return 1e30f32; }
    if point_in_poly(poly, d2(pad.center)) { return 0.0f32; }
    let mut best = 1e300f64;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        best = std_min(best, seg_pad(d2(poly[j]), d2(poly[i]), pad));
        j = i;
    }
    if best <= 0.0 { 0.0f32 } else { best as f32 }
}

/// Drc.h:379: own-net via/pad gap when via-in-pad is disallowed.
pub fn via_pad_clearance(board: &OBoard) -> f32 { board.rules.min_clearance }
