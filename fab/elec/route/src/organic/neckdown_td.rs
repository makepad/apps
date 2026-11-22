// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/NeckdownTeardrop.cpp (1539-2458).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Teardrops and the serial neckdown/teardrop export pass. Companion interfaces are
//! in `neckdown` and `teardrop_sdf`; this file does not provide stand-ins for them.
//! Board clearances and fixed-width flags use the model's pre-resolved rule data.

use std::cell::RefCell;
use std::collections::BTreeMap;

use super::copper::{pad_on_layer, seg_seg, K_BIG};
use super::geom::{std_clamp, std_max, std_min, Box2, Vec2};
use super::model::{NeckdownIssue, OBoard};
use super::neckdown::{
    circle_ring, exported_box, exported_path, extent, finite_v, neck_trace, output_stamp,
    pad_cross, pad_ring, path_near_edges, simple_polygon, teardrop_box, Cands, Ctx,
    FilletJob, FilletSide, NeckdownCache, PathView, Poly, Ring, TrackFrame,
};
use super::padgeom;
use super::settings::{relax_immutable, OrganicSettings};
use super::teardrop_sdf::{build, Conn, Input, Result, Scale};

/// NeckdownTeardrop.cpp:1539-2052. Output rings close implicitly (last to first).
pub(crate) fn teardrops_of_trace(c: &mut Ctx<'_>, t: i32, trace_index: &BTreeMap<i32, i32>) {
    let s = c.s;
    // Rust ownership: snapshot this trace's read-only geometry. Publish each end at
    // the reference's write point; C's board and obstacle index stay authoritative.
    let tr = c.b.traces[t as usize].clone();
    if !s.enable_teardrops || tr.points.len() < 2 || !tr.is_routed {
        return;
    }
    // ADAPTED: ClearanceTable queries need this trace's rule row, as in neck_trace.
    c.cur_rule = tr.rule;
    let smooth = tr.smoothed.len() >= 2;
    let var_w = smooth && tr.point_widths.len() == tr.smoothed.len();
    let np = if smooth { tr.smoothed.len() } else { tr.points.len() };
    if np < 2 {
        return;
    }
    let pt_at = |i: usize| if smooth { tr.smoothed[i] } else { tr.points[i].pos };
    let w_at = |i: usize| if var_w { tr.point_widths[i] } else { tr.width };
    let mut total_len = 0.0f64;
    for i in 0..np - 1 {
        total_len += pt_at(i).distance_to(pt_at(i + 1)) as f64;
    }

    let mut q: Vec<Vec2> = Vec::new();
    let mut qw = Vec::new();
    for side in 0..2 {
        if side == 0 {
            c.b.traces[t as usize].teardrop_front.clear();
        } else {
            c.b.traces[t as usize].teardrop_back.clear();
        }
        let tp = if side == 0 { &tr.points[0] } else { tr.points.last().unwrap() };
        // The frame may borrow a polygon pad: own that pad locally so queries can
        // mutably borrow C without aliasing the board through the frame.
        let pad = if tp.terminal_pad >= 0 { c.b.pads.get(tp.terminal_pad as usize).cloned() } else { None };
        let via = if tp.attached_via >= 0 { c.b.vias.get(tp.attached_via as usize).cloned() } else { None };
        let mut j_key = u64::MAX;
        if pad.is_some() {
            j_key = ((tr.layer as u32 as u64) << 32) | tp.terminal_pad as u32 as u64;
        }
        let conns_list = if j_key != u64::MAX && s.teardrop_style == 2 {
            c.junction_conns.and_then(|m| m.get(&j_key)).filter(|v| v.len() > 1).cloned()
        } else {
            None
        };

        let mut junction = 3.0 * tr.width as f64;
        if let Some(pad) = &pad {
            junction = std_max(pad.size.x, pad.size.y) as f64;
        } else if let Some(via) = &via {
            junction = via.diameter as f64;
        }
        let prefix = 1.5 * junction + s.teardrop_max_length as f64 + 1.0;
        q.clear();
        qw.clear();
        let mut acc = 0.0f64;
        for k in 0..np {
            let i = if side == 0 { k } else { np - 1 - k };
            if k > 0 {
                acc += q.last().unwrap().distance_to(pt_at(i)) as f64;
            }
            q.push(pt_at(i));
            qw.push(w_at(i));
            if acc > prefix {
                break;
            }
        }
        if q.len() < 2 {
            continue;
        }
        let pv = PathView::new(&q, &qw);

        let mut cross = 0.0f64;
        let mut tj = false;
        let mut pf = padgeom::Frame::default();
        let mut host_p = Vec::new();
        let mut host_w = Vec::new();
        let inside: Box<dyn Fn(Vec2) -> bool + '_>;
        if let Some(pad) = &pad {
            if !pad_on_layer(pad, tr.layer, c.n_l) {
                continue;
            }
            pf = padgeom::frame(pad);
            inside = Box::new(|q: Vec2| padgeom::point_distance(&pf, q.x as f64, q.y as f64) <= 1e-9);
        } else if let Some(via) = &via {
            let centre = via.pos;
            let r = via.diameter * 0.5f32;
            inside = Box::new(move |q: Vec2| q.distance_to(centre) <= r);
            cross = via.diameter as f64;
        } else if tp.branch_trace >= 0 && s.teardrop_t_junctions {
            let Some(&host_index) = trace_index.get(&tp.branch_trace) else { continue };
            let host = &c.b.traces[host_index as usize];
            if host.layer != tr.layer {
                continue;
            }
            exported_path(host, &mut host_p, &mut host_w);
            if host_p.len() < 2 {
                continue;
            }
            inside = Box::new(|q: Vec2| {
                for i in 0..host_p.len() - 1 {
                    if seg_seg(host_p[i], host_p[i + 1], q, q).d
                        <= 0.5 * std_min(host_w[i], host_w[i + 1]) as f64 + 1e-9
                    {
                        return true;
                    }
                }
                false
            });
            tj = true;
            cross = 3.0 * tr.width as f64;
        } else {
            continue;
        }
        if !inside(q[0]) {
            continue;
        }
        let mut s_x = -1.0f64;
        for k in 1..q.len() {
            if inside(q[k]) {
                continue;
            }
            let mut lo = pv.cum[k - 1];
            let mut hi = pv.cum[k];
            for _ in 0..24 {
                let m = 0.5 * (lo + hi);
                if inside(pv.at(m)) { lo = m; } else { hi = m; }
            }
            s_x = hi;
            break;
        }
        if s_x < 0.0 {
            continue;
        }
        if cross <= 0.0 {
            let mut d = pv.tangent(std_min(s_x, 0.3));
            if s_x > 1e-3 {
                d = (pv.at(s_x) - q[0]).normalized();
            }
            cross = pad_cross(pad.as_ref().unwrap(), q[0], d);
        }
        // NeckdownTeardrop.cpp:1661-1697, the same length rule for all connections.
        let td_length = |q: &[Vec2], pv: &PathView<'_>, s_x: f64, cross: f64, total: f64, trace_id: i32, tr_width: f64| {
            let wt = std_min(s.teardrop_width_ratio as f64 * cross, s.teardrop_max_width as f64);
            let mut l = std_min(s.teardrop_length_ratio as f64 * cross, s.teardrop_max_length as f64);
            l = std_min(l, 0.45 * (total - s_x));
            if s.teardrop_style >= 1 && !tj {
                let room = 0.45 * (total - s_x);
                let w = pv.width_at(std_min(s_x, pv.length())) as f64;
                let mut lf = std_max(l, std_min(2.4 * (0.5 * wt - 0.5 * w), 1.5 * s.teardrop_max_length as f64));
                let mut turn_sign = 0.0f64;
                let mut settled = s_x;
                let reach = std_min(s_x + 4.0 * l, pv.length() - 0.1);
                let mut station = s_x + 0.1;
                while station <= reach {
                    let a = pv.tangent(station - 0.1);
                    let b = pv.tangent(station);
                    let turn = (a.x as f64 * b.y as f64 - a.y as f64 * b.x as f64)
                        .atan2(a.x as f64 * b.x as f64 + a.y as f64 * b.y as f64);
                    if !(turn.abs() < 0.15) {
                        if turn * turn_sign < 0.0 { settled = station; }
                        turn_sign = turn;
                    }
                    station += 0.1;
                }
                if settled > s_x { lf = std_max(lf, settled - s_x + 0.6); }
                l = std_min(std_min(lf, room), prefix - s_x);
            }
            if s.teardrop_style >= 1 && !tj {
                if let Some(ends) = c.branch_ends.and_then(|ends| ends.get(&trace_id)) {
                    for &point in ends {
                        let mut best = K_BIG;
                        let mut s_b = 0.0;
                        for k in 0..q.len() - 1 {
                            let r = seg_seg(q[k], q[k + 1], point, point);
                            if r.d < best {
                                best = r.d;
                                s_b = pv.cum[k] + q[k].distance_to(point) as f64 * 1.0;
                            }
                        }
                        if best < 0.5 * tr_width + 1e-3 && s_b > s_x {
                            l = std_min(l, 0.8 * (s_b - s_x));
                        }
                    }
                }
            }
            l
        };
        let wt = std_min(s.teardrop_width_ratio as f64 * cross, s.teardrop_max_width as f64);
        let l = td_length(&q, &pv, s_x, cross, total_len, tr.id, tr.width as f64);
        if l < 0.05 || wt <= 0.0 {
            continue;
        }
        if s.teardrop_style == 2 && !tj {
            let s_root = std_max(0.0, s_x - 0.5 * cross);
            let w_x = pv.width_at(std_min(s_x, pv.length())) as f64;
            let sp = pv.at(s_root);
            let ns = pv.tangent(s_root).perpendicular();
            if std_min(0.5 * wt, std_max(extent(sp, ns, 0.5 * wt, &*inside), extent(sp, ns * -1.0f32, 0.5 * wt, &*inside)))
                < s.teardrop_flare_factor as f64 * 0.5 * w_x
            {
                continue;
            }
            let mut all_q = Vec::new();
            let mut all_qw = Vec::new();
            let mut input = Input::default();
            if pad.is_some() {
                input.pad = Some(&pf);
            } else {
                let via = via.as_ref().unwrap();
                input.via_centre = via.pos;
                input.via_radius = 0.5 * via.diameter as f64;
            }
            input.path = Some(&q);
            input.width = Some(&qw);
            input.s_x = s_x;
            input.s_root = s_root;
            input.length = std_min(l, pv.length() - s_x);
            input.width2 = wt;

            if let Some(conns_list) = conns_list.as_ref().filter(|v| v.len() >= 2) {
                let mut this_found = false;
                all_q.resize(conns_list.len(), Vec::<Vec2>::new());
                all_qw.resize(conns_list.len(), Vec::<f32>::new());
                // Disjoint mutable slots keep earlier Conn borrows valid. Each slot
                // is filled once, in the reference's ci order, then only read.
                for (ci, (cq, cqw)) in all_q.iter_mut().zip(&mut all_qw).enumerate() {
                    let (ct, cs) = conns_list[ci];
                    let ctr = &c.b.traces[ct as usize];
                    let c_smooth = ctr.smoothed.len() >= 2;
                    let c_var_w = c_smooth && ctr.point_widths.len() == ctr.smoothed.len();
                    let c_np = if c_smooth { ctr.smoothed.len() } else { ctr.points.len() };
                    let c_pt_at = |i: usize| if c_smooth { ctr.smoothed[i] } else { ctr.points[i].pos };
                    let c_w_at = |i: usize| if c_var_w { ctr.point_widths[i] } else { ctr.width };
                    let mut c_acc = 0.0f64;
                    for k in 0..c_np {
                        let i = if cs == 0 { k } else { c_np - 1 - k };
                        if k > 0 { c_acc += cq.last().unwrap().distance_to(c_pt_at(i)) as f64; }
                        cq.push(c_pt_at(i));
                        cqw.push(c_w_at(i));
                        if c_acc > prefix { break; }
                    }
                    if cq.len() < 2 || !inside(cq[0]) { continue; }
                    let cpv = PathView::new(cq, cqw);
                    let mut cs_x = -1.0;
                    for k in 1..cq.len() {
                        if inside(cq[k]) { continue; }
                        let mut lo = cpv.cum[k - 1];
                        let mut hi = cpv.cum[k];
                        for _ in 0..24 {
                            let m = 0.5 * (lo + hi);
                            if inside(cpv.at(m)) { lo = m; } else { hi = m; }
                        }
                        cs_x = hi;
                        break;
                    }
                    if cs_x < 0.0 { continue; }
                    let mut c_cross = cross;
                    if let Some(pad) = &pad {
                        let mut cd = cpv.tangent(std_min(cs_x, 0.3));
                        if cs_x > 1e-3 { cd = (cpv.at(cs_x) - cq[0]).normalized(); }
                        c_cross = pad_cross(pad, cq[0], cd);
                    }
                    let c_wt = std_min(s.teardrop_width_ratio as f64 * c_cross, s.teardrop_max_width as f64);
                    let mut c_total = 0.0f64;
                    for i in 0..c_np - 1 { c_total += c_pt_at(i).distance_to(c_pt_at(i + 1)) as f64; }
                    let c_l = td_length(cq, &cpv, cs_x, c_cross, c_total, ctr.id, ctr.width as f64);
                    let cs_root = std_max(0.0, cs_x - 0.5 * c_cross);
                    let conn = Conn {
                        path: Some(cq),
                        width: Some(cqw),
                        s_x: cs_x,
                        s_root: cs_root,
                        length: std_min(c_l, cpv.length() - cs_x),
                        bridge_length: std_min(std_min(std_min(s.teardrop_length_ratio as f64 * c_cross,
                            s.teardrop_max_length as f64), 0.45 * (c_total - cs_x)), cpv.length() - cs_x),
                        width2: c_wt,
                    };
                    if ct == t && cs == side {
                        this_found = true;
                        input.this_conn = input.conns.len() as i32;
                    }
                    input.conns.push(conn);
                }
                if !this_found {
                    input.conns.clear();
                    input.this_conn = 0;
                }
            }

            let mut result = Result::default();
            let ok = {
                // C++'s collectNear/good/attempt closures share C and near. RefCell
                // scopes those serial borrows: the field reads C only during build,
                // then poly_clear mutates its query scratch. No lock or worker exists.
                let ctx = RefCell::new(&mut *c);
                let near = RefCell::new(Cands::default());
                let foreign = |q: Vec2| ctx.borrow().avail(&near.borrow(), q, q, false, 1.0);
                let mut input = input;
                let mut cands = Cands::default();
                let mut good = |input: &Input<'_>, left: &Scale, right: &Scale| {
                    if !build(input, left, right, &mut result) { return false; }
                    if !simple_polygon(&result.poly) { return false; }
                    ctx.borrow_mut().poly_clear(&result.poly, tr.layer, tr.net, &mut cands)
                };
                // First attempt, then FA17's single-connection retry if needed.
                loop {
                    let mut win = Box2::default();
                    let mut w_max = wt;
                    if input.conns.len() >= 2 {
                        w_max = 0.0;
                        for conn in &input.conns {
                            w_max = std_max(w_max, conn.width2);
                            let cpv = PathView::new(conn.path.unwrap(), conn.width.unwrap());
                            let mut sv = conn.s_root;
                            loop {
                                win.expand(cpv.at(std_min(sv, conn.s_x + conn.length)));
                                if sv >= conn.s_x + conn.length { break; }
                                sv += 0.1;
                            }
                        }
                    } else {
                        let mut sv = s_root;
                        loop {
                            win.expand(pv.at(std_min(sv, s_x + input.length)));
                            if sv >= s_x + input.length { break; }
                            sv += 0.1;
                        }
                    }
                    near.borrow_mut().ids.clear();
                    let reach = (ctx.borrow().clr + 0.5 * w_max + 0.15) as f32;
                    ctx.borrow_mut().collect(win, reach, tr.layer, tr.net, &mut near.borrow_mut());
                    input.foreign = None;
                    if !near.borrow().ids.is_empty() { input.foreign = Some(&foreign); }
                    input.deep_root = true;
                    input.bridge = true;
                    let mut ok = good(&input, &Scale::default(), &Scale::default());
                    if !ok {
                        input.deep_root = false;
                        ok = good(&input, &Scale::default(), &Scale::default());
                    }
                    if !ok {
                        input.bridge = false;
                        const K_SHRINK: [Scale; 11] = [
                            Scale { height: 0.8, length: 0.9 }, Scale { height: 0.8, length: 0.7 },
                            Scale { height: 0.6, length: 0.8 }, Scale { height: 0.6, length: 0.6 },
                            Scale { height: 0.45, length: 0.65 }, Scale { height: 0.45, length: 0.45 },
                            Scale { height: 0.3, length: 0.5 }, Scale { height: 0.3, length: 0.35 },
                            Scale { height: 0.2, length: 0.4 }, Scale { height: 0.2, length: 0.28 },
                            Scale { height: 0.12, length: 0.28 },
                        ];
                        let flat = Scale { height: 0.0, length: 1.0 };
                        let mut left = &flat;
                        let mut right = &flat;
                        for k in &K_SHRINK {
                            if good(&input, k, &flat) { left = k; break; }
                        }
                        for k in &K_SHRINK {
                            if good(&input, &flat, k) { right = k; break; }
                        }
                        if left.height > 0.0 || right.height > 0.0 { ok = good(&input, left, right); }
                        if !ok && left.height > 0.0 && right.height > 0.0 {
                            for k in &K_SHRINK {
                                ok = good(&input, k, k);
                                if ok { break; }
                            }
                            if !ok { ok = good(&input, left, &flat) || good(&input, &flat, right); }
                        }
                        if !ok && input.conns.len() >= 2 { ok = good(&input, &flat, &flat); }
                    }
                    if ok || input.conns.len() < 2 { break ok; }
                    input.conns.clear();
                    input.this_conn = 0;
                }
            };
            if !ok || result.flare < (1.0 + 0.4 * (s.teardrop_flare_factor as f64 - 1.0)) * 0.5 * w_x {
                continue;
            }
            if side == 0 {
                c.b.traces[t as usize].teardrop_front = result.poly.clone();
            } else {
                c.b.traces[t as usize].teardrop_back = result.poly.clone();
            }
            let mut pp = Poly { pts: result.poly, bb: Box2::default(), net: tr.net, layer: tr.layer, trace: t };
            for &p in &pp.pts { pp.bb.expand(p); }
            c.add_poly(pp);
            continue;
        }

        if s.teardrop_style == 1 && !tj {
            let mut ring = Ring::default();
            if pad.is_some() { pad_ring(&pf, &mut ring); }
            else {
                let via = via.as_ref().unwrap();
                circle_ring(via.pos, 0.5 * via.diameter as f64, &mut ring);
            }
            if ring.p.len() < 3 || ring.len < 1e-6 { continue; }
            let s_root = std_max(0.0, s_x - 0.5 * cross);
            let w_x = pv.width_at(std_min(s_x, pv.length())) as f64;
            let sp = pv.at(s_root);
            let ns = pv.tangent(s_root).perpendicular();
            if std_min(0.5 * wt, std_max(extent(sp, ns, 0.5 * wt, &*inside), extent(sp, ns * -1.0f32, 0.5 * wt, &*inside)))
                < s.teardrop_flare_factor as f64 * 0.5 * w_x
            {
                continue;
            }
            let mut fr = TrackFrame::default();
            fr.build(&pv, s_root, std_min(pv.length(), s_x + l), std_clamp(0.3 * l, 0.05, 0.5));
            if fr.max_off > 0.03 {
                fr.build(&pv, s_root, std_min(pv.length(), s_x + l), std_clamp(0.6 * l, 0.1, 1.0));
            }
            let j = FilletJob { pv: &pv, fr: &fr, ring: &ring, inside: &*inside, s_x, s_root,
                l: std_min(l, pv.length() - s_x), wt, curvature: s.teardrop_curvature as f64 };
            const K_PHI: [[f64; 2]; 6] = [[0.92, 1.0], [0.75, 1.0], [0.6, 0.85], [0.45, 0.7], [0.32, 0.55], [0.2, 0.4]];
            const K_SHRINK: [[f64; 2]; 31] = [
                [0.92, 1.0], [0.92, 0.85], [0.75, 1.0], [0.92, 0.7], [0.75, 0.85], [0.6, 1.0], [0.92, 0.55],
                [0.75, 0.7], [0.6, 0.85], [0.45, 1.0], [0.92, 0.4], [0.75, 0.55], [0.6, 0.7], [0.45, 0.85],
                [0.75, 0.4], [0.6, 0.55], [0.45, 0.7], [0.32, 0.85], [0.92, 0.28], [0.6, 0.4], [0.45, 0.55],
                [0.75, 0.28], [0.32, 0.55], [0.6, 0.28], [0.45, 0.4], [0.32, 0.4], [0.2, 0.55], [0.45, 0.28],
                [0.2, 0.4], [0.32, 0.28], [0.2, 0.28],
            ];
            let mut cands = Cands::default();
            let mut left = FilletSide::default();
            let mut right = FilletSide::default();
            let mut clean = Vec::new();
            let mut part = Vec::new();
            let mut has_l = false;
            let mut has_r = false;
            for ph in K_PHI {
                has_l = j.build(1, ph[0], ph[1], &mut left);
                if has_l { break; }
            }
            for ph in K_PHI {
                has_r = j.build(-1, ph[0], ph[1], &mut right);
                if has_r { break; }
            }
            if !has_l && !has_r { continue; }
            if !has_l { j.flat(right.s_tip, &mut left); }
            if !has_r { j.flat(left.s_tip, &mut right); }
            j.assemble(&left, &right, &mut clean);
            let ok = clean.len() >= 3 && simple_polygon(&clean) && c.poly_clear(&clean, tr.layer, tr.net, &mut cands);
            if !ok {
                let mut shrink = |side: i32, f: &mut FilletSide| {
                    for ph in K_SHRINK {
                        if !j.build(side, ph[0], ph[1], f) { continue; }
                        j.half_poly(side, f, &mut part);
                        if simple_polygon(&part) && c.poly_clear(&part, tr.layer, tr.net, &mut cands) { return true; }
                    }
                    false
                };
                has_l = shrink(1, &mut left);
                has_r = shrink(-1, &mut right);
                if !has_l && !has_r { continue; }
                if !has_l { j.flat(right.s_tip, &mut left); }
                if !has_r { j.flat(left.s_tip, &mut right); }
                j.assemble(&left, &right, &mut clean);
                if clean.len() < 3 || !simple_polygon(&clean) || !c.poly_clear(&clean, tr.layer, tr.net, &mut cands) { continue; }
            }
            if std_max(left.flare, right.flare) < (1.0 + 0.4 * (s.teardrop_flare_factor as f64 - 1.0)) * 0.5 * w_x { continue; }
            if side == 0 { c.b.traces[t as usize].teardrop_front = clean.clone(); }
            else { c.b.traces[t as usize].teardrop_back = clean.clone(); }
            let mut pp = Poly { pts: clean, bb: Box2::default(), net: tr.net, layer: tr.layer, trace: t };
            for &p in &pp.pts { pp.bb.expand(p); }
            c.add_poly(pp);
            continue;
        }

        let s_s = if tj { 0.0 } else { std_max(0.0, s_x - 0.5 * cross) };
        let s_t = s_x + l;
        let sp = pv.at(s_s);
        let ns = pv.tangent(s_s).perpendicular();
        let ext_l = extent(sp, ns, 0.5 * wt, &*inside);
        let ext_r = extent(sp, ns * -1.0f32, 0.5 * wt, &*inside);
        let w0 = pv.width_at(std_min(s_x, total_len)) as f64;
        let h = [std_min(0.5 * wt, ext_l), std_min(0.5 * wt, ext_r)];
        if std_max(h[0], h[1]) < s.teardrop_flare_factor as f64 * 0.5 * w0 { continue; }
        const M: usize = 14;
        let mut cands = Cands::default();
        let k = 2.0 * std_clamp(s.teardrop_curvature as f64, 0.0, 1.0);
        let mut axis = vec![Vec2::default(); M + 1];
        let mut nrm = vec![Vec2::default(); M + 1];
        let mut half = vec![0.0f64; M + 1];
        let mut fu = vec![0.0f64; M + 1];
        for i in 0..=M {
            let u = i as f64 / M as f64;
            let sv = s_s + (s_t - s_s) * u;
            axis[i] = pv.at(sv);
            nrm[i] = pv.tangent(sv).perpendicular();
            half[i] = 0.5 * pv.width_at(sv) as f64;
            fu[i] = (1.0 - u) * (1.0 - u) * (1.0 + (2.0 - k) * u);
        }
        let offsets = |side_idx: usize, hs: f64, pts: &mut Vec<Vec2>| {
            pts.clear();
            let sg = if side_idx == 0 { 1.0f32 } else { -1.0f32 };
            for i in 0..=M {
                let h = half[i] + std_max(0.0, hs - half[i]) * fu[i];
                pts.push(axis[i] + nrm[i] * (sg as f64 * h) as f32);
            }
        };
        let assemble = |hl: f64, hr: f64, poly: &mut Vec<Vec2>| {
            let mut right = Vec::new();
            let mut left = Vec::new();
            offsets(1, hr, &mut right);
            offsets(0, hl, &mut left);
            poly.clear();
            for i in 0..=M {
                if poly.is_empty() || poly.last().unwrap().distance_squared_to(right[i]) > 1e-10f32 { poly.push(right[i]); }
            }
            for i in (0..=M).rev() {
                if poly.last().unwrap().distance_squared_to(left[i]) > 1e-10f32 { poly.push(left[i]); }
            }
            if poly.len() >= 2 && poly[0].distance_squared_to(*poly.last().unwrap()) < 1e-10f32 { poly.pop(); }
        };
        let mut clean = Vec::new();
        assemble(h[0], h[1], &mut clean);
        let ok = clean.len() >= 3 && simple_polygon(&clean) && c.poly_clear(&clean, tr.layer, tr.net, &mut cands);
        if !ok {
            const K_SHRINK: [f64; 7] = [1.0, 0.8, 0.64, 0.5, 0.38, 0.28, 0.2];
            let mut hfin = [0.0f64; 2];
            let mut off = Vec::new();
            for sd in 0..2 {
                for phi in K_SHRINK {
                    let hs = half[0] + (h[sd] - half[0]) * phi;
                    if hs <= half[0] * 1.05 { break; }
                    offsets(sd, hs, &mut off);
                    let mut half_poly = axis.clone();
                    for i in (0..=M).rev() { half_poly.push(off[i]); }
                    if c.poly_clear(&half_poly, tr.layer, tr.net, &mut cands) { hfin[sd] = hs; break; }
                }
            }
            if std_max(hfin[0], hfin[1]) < s.teardrop_flare_factor as f64 * 0.5 * w0 { continue; }
            assemble(hfin[0], hfin[1], &mut clean);
            if clean.len() < 3 || !simple_polygon(&clean) { continue; }
            if !c.poly_clear(&clean, tr.layer, tr.net, &mut cands) { continue; }
        }
        if side == 0 { c.b.traces[t as usize].teardrop_front = clean.clone(); }
        else { c.b.traces[t as usize].teardrop_back = clean.clone(); }
        let mut pp = Poly { pts: clean, bb: Box2::default(), net: tr.net, layer: tr.layer, trace: t };
        for &p in &pp.pts { pp.bb.expand(p); }
        c.add_poly(pp);
    }
}

/// NeckdownTeardrop.cpp:2053-2110. Cache entries follow unique trace ids.
pub(crate) fn remap_by_id(k: &mut NeckdownCache, board: &OBoard) {
    let t_count = board.traces.len();
    let mut same = k.ids.len() == t_count;
    for t in 0..t_count {
        if !same { break; }
        same = k.ids[t] == board.traces[t].id;
    }
    let n_old = k.ids.len();
    let entries_ok = k.path_box.len() == n_old && k.td_box.len() == n_old
        && k.stamp.len() == n_old && k.nominal.len() == n_old;
    if same && entries_ok && k.index.len() <= t_count && (t_count == 0 || !k.index.is_empty()) {
        return;
    }
    let mut pb = vec![Box2::default(); t_count];
    let mut tb = vec![Box2::default(); t_count];
    let mut sp = vec![0u64; t_count];
    let mut nm = vec![Vec::new(); t_count];
    if entries_ok {
        if same {
            std::mem::swap(&mut pb, &mut k.path_box);
            std::mem::swap(&mut tb, &mut k.td_box);
            std::mem::swap(&mut sp, &mut k.stamp);
            std::mem::swap(&mut nm, &mut k.nominal);
        } else {
            // C++ unordered_map -> BTreeMap. These maps are only looked up;
            // entries are transferred in board-index order, never map order.
            let mut old_pos = BTreeMap::new();
            for i in 0..n_old {
                old_pos.entry(k.ids[i]).and_modify(|p| *p = -1).or_insert(i as i32);
            }
            let mut seen = BTreeMap::<i32, i32>::new();
            for t in 0..t_count { *seen.entry(board.traces[t].id).or_default() += 1; }
            for t in 0..t_count {
                let id = board.traces[t].id;
                if seen[&id] != 1 { continue; }
                let Some(&old) = old_pos.get(&id) else { continue };
                if old < 0 { continue; }
                let o = old as usize;
                pb[t] = k.path_box[o];
                tb[t] = k.td_box[o];
                sp[t] = k.stamp[o];
                std::mem::swap(&mut nm[t], &mut k.nominal[o]);
            }
        }
    }
    std::mem::swap(&mut k.path_box, &mut pb);
    std::mem::swap(&mut k.td_box, &mut tb);
    std::mem::swap(&mut k.stamp, &mut sp);
    std::mem::swap(&mut k.nominal, &mut nm);
    k.ids.resize(t_count, 0);
    k.index.clear();
    for t in 0..t_count {
        k.ids[t] = board.traces[t].id;
        k.index.insert(board.traces[t].id, t as i32);
    }
}

/// NeckdownTeardrop.cpp:2114-2456. `None` mask processes every trace.
/// `teardrops == false` preserves teardrops only when no cache is supplied.
pub fn neckdowns_and_teardrops(
    board: &mut OBoard,
    st: &OrganicSettings,
    mask: Option<&[u8]>,
    issues: &mut Vec<Vec<NeckdownIssue>>,
    chord_gap: Option<&[f64]>,
    keep_teardrops: Option<&[u8]>,
    cache: Option<&mut NeckdownCache>,
    mut teardrops: bool,
) {
    let has_cache = cache.is_some();
    if !teardrops && has_cache { teardrops = true; }
    let keep_td = |t: usize| keep_teardrops.is_some_and(|keep| t < keep.len() && keep[t] != 0);
    let t_count = board.traces.len();
    issues.resize_with(t_count, Vec::new);
    let masked = |t: usize| mask.is_none_or(|mask| t < mask.len() && mask[t] != 0);
    let mut local = NeckdownCache::default();
    let k = cache.unwrap_or(&mut local);
    if k.scratch.is_none() { k.scratch = Some(Box::default()); }
    if !k.ncl_valid {
        // ADAPTED: NetClearances::build is the board's already-resolved table.
        k.ncl.clone_from(&board.clr);
        k.ncl_valid = true;
    }
    let mut entry_ok = Vec::new();
    if has_cache {
        remap_by_id(k, board);
        entry_ok.resize(t_count, 0u8);
        if mask.is_some() {
            for t in 0..t_count {
                entry_ok[t] = (!masked(t) && k.stamp[t] != 0 && k.stamp[t] == output_stamp(&board.traces[t])) as u8;
            }
        }
    } else {
        k.index.clear();
        for t in 0..t_count { k.index.insert(board.traces[t].id, t as i32); }
    }
    let boxes_ok = has_cache && mask.is_some();
    let mut pbox = Vec::new();
    let mut tbox = Vec::new();
    if boxes_ok {
        pbox = k.path_box.clone();
        tbox = k.td_box.clone();
        for t in 0..t_count {
            if entry_ok[t] == 0 {
                pbox[t] = exported_box(&board.traces[t]);
                tbox[t] = teardrop_box(&board.traces[t]);
            }
        }
    }
    if k.fixed_width.len() != board.nets.len() {
        k.fixed_width = vec![0; board.nets.len()];
        // ADAPTED: trackWidthFixed reads the model's resolved per-net flags.
        for n in 0..board.nets.len() {
            k.fixed_width[n] = (board.track_width_fixed.get(n).copied().unwrap_or(0) != 0) as u8;
        }
    }
    let mut branch_ends = BTreeMap::<i32, Vec<Vec2>>::new();
    if st.teardrop_style >= 1 {
        for tr in &board.traces {
            if tr.points.is_empty() { continue; }
            for e in [&tr.points[0], tr.points.last().unwrap()] {
                if e.branch_trace >= 0 { branch_ends.entry(e.branch_trace).or_default().push(e.pos); }
            }
        }
    }
    // Rust ownership: copy only the cached paths actually borrowed by C. They
    // belong to unprocessed traces and never change in this pass. This leaves
    // K.nominal's processed slots independently writable by neck_trace.
    let nominal: Vec<Option<Vec<Vec2>>> = if has_cache && mask.is_some() {
        (0..t_count).map(|t| {
            if entry_ok[t] != 0 && !k.nominal[t].is_empty() { Some(k.nominal[t].clone()) } else { None }
        }).collect()
    } else { Vec::new() };
    let nom_ptr: Vec<Option<&[Vec2]>> = nominal.iter().map(|p| p.as_deref()).collect();
    let mut junction_conns = BTreeMap::<u64, Vec<(i32, i32)>>::new();
    let mut c = Ctx::new(board, st, k.scratch.as_deref_mut().unwrap(), Some(&k.ncl));
    c.fixed_width = Some(&k.fixed_width);
    c.all = mask.is_none();
    if st.teardrop_style >= 1 { c.branch_ends = Some(&branch_ends); }
    if boxes_ok { c.path_box = Some(&pbox); }
    if has_cache && mask.is_some() { c.nominal = Some(&nom_ptr); }
    if !c.all {
        let mut max_w = 0.0f32;
        for tr in &c.b.traces { max_w = std_max(max_w, tr.width); }
        // ADAPTED: routingHoleClearance uses rules.hole_clearance, clamped as in Ctx::new.
        let r = 0.5f32 * max_w + 0.5f32 * std_max(0.0f32, st.teardrop_max_width)
            + std_max(std_max(c.b.rules.min_clearance, std_max(c.b.rules.hole_clearance, 0.0f32)), c.b.rules.board_edge_clearance) + 0.5f32;
        let mut any_masked = false;
        let mut boxes = Vec::new();
        // The reference adds these f32 values before widening to double.
        let edge_clr = (c.b.rules.board_edge_clearance + std_max(0.0f32, st.guard_clearance_slack)) as f64;
        let clr = (c.b.rules.min_clearance + std_max(0.0f32, st.guard_clearance_slack)) as f64;
        // ADAPTED: padRuleReach (C++:2205) uses b.clr.max, as engine/mod.rs:453;
        // NetClearances::base is rules.min_clearance in the rule-row adaptation.
        let rs = r + std_max(0.0f32, c.b.clr.max - c.b.rules.min_clearance);
        for t in 0..t_count {
            if !masked(t) { continue; }
            any_masked = true;
            let tr = &c.b.traces[t];
            let sp = &tr.smoothed;
            let mut full = !chord_gap.is_some_and(|gap| t < gap.len() && gap[t] >= clr + 1e-4) || sp.len() < 2;
            if !full && c.b.outline.len() >= 3 {
                full = path_near_edges(sp, c.b.outline.len(),
                    |e| (c.b.outline[e], c.b.outline[(e + 1) % c.b.outline.len()]),
                    0.5 * tr.width as f64, edge_clr + 0.05);
            }
            if full {
                let mut bad = false;
                // Return whether the segment was finite instead of capturing bad
                // mutably across the loop condition; the same first bad segment stops.
                let mut seg_boxes = |a: Vec2, point: Vec2| {
                    if !finite_v(a) || !finite_v(point) { return false; }
                    let mut b = Box2::default();
                    b.expand(a);
                    b.expand(point);
                    b.pad(rs);
                    boxes.push(b);
                    c.region.expand_box(&b);
                    true
                };
                for i in 0..sp.len().saturating_sub(1) {
                    if bad { break; }
                    bad = !seg_boxes(sp[i], sp[i + 1]);
                }
                for i in 0..tr.points.len().saturating_sub(1) {
                    if bad { break; }
                    bad = !seg_boxes(tr.points[i].pos, tr.points[i + 1].pos);
                }
                if bad || (sp.len() < 2 && tr.points.len() < 2) { c.all = true; continue; }
            }
            if tr.points.len() >= 2 {
                for side in 0..2 {
                    let tp = if side == 0 { &tr.points[0] } else { tr.points.last().unwrap() };
                    let mut j = 3.0f32 * tr.width;
                    if tp.terminal_pad >= 0 && tp.terminal_pad < c.b.pads.len() as i32 {
                        let pad = &c.b.pads[tp.terminal_pad as usize];
                        j = std_max(pad.size.x, pad.size.y);
                    } else if tp.attached_via >= 0 && tp.attached_via < c.b.vias.len() as i32 {
                        j = c.b.vias[tp.attached_via as usize].diameter;
                    }
                    let r = 1.0f32 * j + st.teardrop_max_length + std_max(st.neckdown_taper_length, 0.0f32) + r;
                    let e = if sp.len() >= 2 { if side == 0 { sp[0] } else { *sp.last().unwrap() } } else { tp.pos };
                    if !finite_v(e) { c.all = true; continue; }
                    let eb = Box2::new(e - Vec2::new(r, r), e + Vec2::new(r, r));
                    boxes.push(eb);
                    c.region.expand_box(&eb);
                }
            }
        }
        if !any_masked { return; }
        if !c.all { c.set_regions(&boxes); }
    }

    // NeckdownTeardrop.cpp:2263-2296, parallelFor at 2286: worker index 0,
    // serial todo-index order. Each iteration resets its own node widths and
    // issues[t]/K.nominal[t], then neck_trace writes only that trace's exports.
    // C.build(false) snapshots nominal obstacles before any such writes, just
    // as in the parallel reference; one C replaces all WorkerQ/worker copies.
    c.build(false);
    let mut todo = Vec::new();
    for t in 0..t_count { if masked(t) { todo.push(t as i32); } }
    for i in 0..todo.len() {
        let t = todo[i] as usize;
        issues[t].clear();
        if has_cache { k.nominal[t].clear(); }
        let width = c.b.traces[t].width;
        for p in &mut c.b.traces[t].points { p.width = width; }
        if relax_immutable(&c.b.traces[t], st) { continue; }
        let chords_clear = chord_gap.is_some_and(|gap| t < gap.len() && gap[t] >= c.clr + 1e-4);
        neck_trace(&mut c, t as i32, &mut issues[t], chords_clear, if has_cache { Some(&mut k.nominal[t]) } else { None });
    }
    if !teardrops { return; }

    let trace_index = &k.index;
    for t in 0..t_count {
        if masked(t) && !keep_td(t) {
            c.b.traces[t].teardrop_front.clear();
            c.b.traces[t].teardrop_back.clear();
            continue;
        }
        if boxes_ok && !c.in_region(&tbox[t], 0.5f32) { continue; }
        for side in 0..2 {
            let tr = &c.b.traces[t];
            let poly = if side == 0 { &tr.teardrop_front } else { &tr.teardrop_back };
            if poly.len() < 3 { continue; }
            let mut p = Poly { pts: poly.clone(), bb: Box2::default(), net: tr.net, layer: tr.layer, trace: t as i32 };
            for &q in poly { p.bb.expand(q); }
            if !c.in_region(&p.bb, 0.5f32) { continue; }
            c.add_poly(p);
        }
    }
    c.index_polys();
    let mut td_todo = Vec::new();
    for t in 0..t_count {
        if masked(t) && !keep_td(t) && !relax_immutable(&c.b.traces[t], st) { td_todo.push(t as i32); }
    }
    // ADAPTED: ties broken by board index (the reference's unstable std::sort leaves them to the library)
    td_todo.sort_by_key(|&t| (c.b.traces[t as usize].id, t));

    let junction_key = |tr: &super::model::Trace, sd: i32| {
        if !tr.is_routed || tr.points.len() < 2 { return u64::MAX; }
        let p = if sd == 0 { &tr.points[0] } else { tr.points.last().unwrap() };
        if p.terminal_pad < 0 || p.terminal_pad >= c.b.pads.len() as i32 { return u64::MAX; }
        ((tr.layer as u32 as u64) << 32) | p.terminal_pad as u32 as u64
    };
    if st.teardrop_style == 2 && !td_todo.is_empty() {
        let mut want = Vec::new();
        for &t in &td_todo {
            for sd in 0..2 {
                let key = junction_key(&c.b.traces[t as usize], sd);
                if key != u64::MAX { want.push(key); }
            }
        }
        want.sort_unstable();
        want.dedup();
        for ti in 0..t_count {
            if want.is_empty() { break; }
            for sd in 0..2 {
                let key = junction_key(&c.b.traces[ti], sd);
                if key != u64::MAX && want.binary_search(&key).is_ok() {
                    junction_conns.entry(key).or_default().push((ti as i32, sd));
                }
            }
        }
        // NeckdownTeardrop.cpp:2351: unordered_map iteration becomes ascending
        // junction-key order. Only the vector at that key is sorted/written.
        for list in junction_conns.values_mut() {
            // ADAPTED: ties broken by board index (the reference's unstable std::sort leaves them to the library)
            list.sort_by_key(|&(t, side)| (c.b.traces[t as usize].id, side, t));
        }
    }
    c.junction_conns = Some(&junction_conns);
    // NeckdownTeardrop.cpp:2362-2440, parallelFor at 2380: choose the
    // reference's nwTd <= 1 branch, worker index 0. Each todo-index writes its
    // two teardrop rings and adds polygons immediately to C, so the next trace
    // sees all earlier commits. The speculative branch only delays those writes
    // and redoes conflicting queries; Spec/QLog/cell commit bookkeeping vanishes.
    for i in 0..td_todo.len() { teardrops_of_trace(&mut c, td_todo[i], trace_index); }

    if has_cache {
        for t in 0..t_count {
            if mask.is_none() || entry_ok[t] == 0 {
                let tr = &c.b.traces[t];
                if mask.is_some() && !masked(t) { k.nominal[t].clear(); }
                k.path_box[t] = exported_box(tr);
                k.td_box[t] = teardrop_box(tr);
                k.stamp[t] = output_stamp(tr);
            }
        }
    }
}
