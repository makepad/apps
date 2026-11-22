// Ported from TiNRS Psychedelic Router 0.1.0: src/organic/OrganicRelaxer.cpp (Engine).
// Copyright (c) 2026 Stijn Kuipers. MIT License: see LICENSE in this folder.

//! Evaluation of one moving copper primitive against one obstacle.

use super::{Engine, Ob, ObKind, PairClass, PairEval, Prim};
use crate::organic::copper::{pad_on_layer, seg_hole, seg_pad, seg_seg, Closest, PadModel, K_BIG};
use crate::organic::geom::{std_clamp, std_max, std_min, Vec2};
use crate::organic::model::OBoard;

/// ADAPTED: pour hook 5. The share of its margin other copper keeps from a free (moving) pour
/// path, in the pair's targets and its reach. The path feels the other copper's full margin and
/// moves off by it, so the two need not each keep all of it; against a locked (pinned) stretch,
/// which does not move, the full margin stays.
pub const POUR_SHARE: f64 = 0.5;

impl Engine {
    /// Squared distance between the bounding boxes of a primitive and an obstacle.
    #[inline]
    pub fn bbox_gap2(p: &Prim, o: &Ob) -> f64 {
        let px0 = if p.a.x < p.b.x { p.a.x } else { p.b.x };
        let px1 = if p.a.x < p.b.x { p.b.x } else { p.a.x };
        let py0 = if p.a.y < p.b.y { p.a.y } else { p.b.y };
        let py1 = if p.a.y < p.b.y { p.b.y } else { p.a.y };
        let mut dx = o.x0 - px1;
        if px0 - o.x1 > dx {
            dx = px0 - o.x1;
        }
        let mut dy = o.y0 - py1;
        if py0 - o.y1 > dy {
            dy = py0 - o.y1;
        }
        if dx < 0.0_f32 {
            dx = 0.0_f32;
        }
        if dy < 0.0_f32 {
            dy = 0.0_f32;
        }
        (dx * dx + dy * dy) as f64
    }

    pub const K_PERP_MAX_TURN_COS: f64 = 0.8660254037844387; // cos 30 deg

    /// Cosine between a -> x and o, and its gradient with respect to x.
    #[inline]
    pub fn perp_cos(a: Vec2, x: Vec2, o: &Ob) -> Option<(f64, f64, f64)> {
        let d1x = x.x as f64 - a.x as f64;
        let d1y = x.y as f64 - a.y as f64;
        let d2x = o.b.x as f64 - o.a.x as f64;
        let d2y = o.b.y as f64 - o.a.y as f64;
        let l1 = (d1x * d1x + d1y * d1y).sqrt();
        let l2 = (d2x * d2x + d2y * d2y).sqrt();
        if l1 < 1e-4 || l2 < 1e-4 {
            return None;
        }
        let u1x = d1x / l1;
        let u1y = d1y / l1;
        let u2x = d2x / l2;
        let u2y = d2y / l2;
        let c = u1x * u2x + u1y * u2y;
        let gx = (u2x - c * u1x) / l1;
        let gy = (u2y - c * u1y) / l1;
        Some((c, gx, gy))
    }

    pub const K_PERP_TAPER: f64 = 0.1;

    /// Crossing weight, ramped over [-taper, +taper] around each segment end.
    #[inline]
    pub fn perp_weight(a: Vec2, x: Vec2, o: &Ob) -> f64 {
        let d1x = x.x as f64 - a.x as f64;
        let d1y = x.y as f64 - a.y as f64;
        let d2x = o.b.x as f64 - o.a.x as f64;
        let d2y = o.b.y as f64 - o.a.y as f64;
        let l1 = (d1x * d1x + d1y * d1y).sqrt();
        let l2 = (d2x * d2x + d2y * d2y).sqrt();
        if l1 < 1e-4 || l2 < 1e-4 {
            return 0.0;
        }
        let den = d1x * d2y - d1y * d2x;
        if den.abs() < 1e-6 * l1 * l2 {
            return 0.0;
        }
        let ex = o.a.x as f64 - a.x as f64;
        let ey = o.a.y as f64 - a.y as f64;
        let t = (ex * d2y - ey * d2x) / den;
        let u = (ex * d1y - ey * d1x) / den;
        let win = |tt: f64, l: f64| {
            let s0 = std_clamp((tt * l + Self::K_PERP_TAPER) / (2.0 * Self::K_PERP_TAPER), 0.0, 1.0);
            let s1 = std_clamp(((1.0 - tt) * l + Self::K_PERP_TAPER) / (2.0 * Self::K_PERP_TAPER), 0.0, 1.0);
            std_min(s0, s1)
        };
        win(t, l1) * win(u, l2)
    }

    #[inline]
    pub fn bbox_gap(p: &Prim, o: &Ob) -> f64 {
        Self::bbox_gap2(p, o).sqrt()
    }

    pub fn fallback_dir(&self, b: &OBoard, p: &Prim, o: &Ob) -> Vec2 {
        let node = p.b;
        match o.kind {
            ObKind::Seg => {
                let pts = &b.traces[self.c.seg_trace[o.id as usize] as usize].points;
                let mut d = pts[(self.c.seg_idx[o.id as usize] + 1) as usize].pos - pts[self.c.seg_idx[o.id as usize] as usize].pos;
                if d.x < 0.0 || (d.x == 0.0 && d.y < 0.0) {
                    d = d * -1.0_f32;
                }
                let mut n = d.perpendicular().normalized();
                if n.length_squared() < 0.5_f32 {
                    n = Vec2::new(0.0, 1.0);
                }
                return if p.trace >= 0 && p.trace < self.c.seg_trace[o.id as usize] { n } else { n * -1.0_f32 };
            }
            ObKind::Pad => {
                let d = (node - b.pads[o.id as usize].center).normalized();
                if d.length_squared() > 0.5_f32 {
                    return d;
                }
            }
            ObKind::Via => {
                let d = (node - b.vias[o.id as usize].pos).normalized();
                if d.length_squared() > 0.5_f32 {
                    return d;
                }
            }
            ObKind::Edge => {
                let d = (self.c.board_center - node).normalized();
                if d.length_squared() > 0.5_f32 {
                    return d;
                }
            }
            ObKind::Ko => {}
        }
        let s = (p.b - p.a).perpendicular().normalized();
        if s.length_squared() > 0.5_f32 { s } else { Vec2::new(1.0, 0.0) }
    }

    /// Effective radius including copper clearance from the drill wall.
    pub fn drilled_radius(&self, r: f64, drill: f64) -> f64 {
        std_max(r, 0.5 * drill + self.hole_clr - self.clr_rule)
    }

    /// Copper, copper-to-hole and hole-to-hole rules folded into a copper-threshold gap.
    pub fn via_via_gap(&self, d: f64, r_a: f64, dr_a: f64, r_b: f64, dr_b: f64) -> f64 {
        let req = std_max(
            std_max(std_max(r_a + r_b + self.clr_rule, 0.5 * dr_a + self.hole_clr + r_b), r_a + 0.5 * dr_b + self.hole_clr),
            0.5 * (dr_a + dr_b) + self.hole_h2h,
        );
        d - req + self.clr_rule
    }

    /// Gap from the primitive to a pad's drill capsule, normalised to copper clearance.
    pub fn pad_hole_gap(&self, pm: &PadModel, d: f64, p: &Prim) -> f64 {
        if !pm.hole {
            return K_BIG;
        }
        let mut req = p.h + pm.hr + self.hole_clr;
        if p.disc {
            req = std_max(req, 0.5 * p.drill + pm.hr + self.hole_h2h);
        }
        d - req + self.clr_rule
    }

    pub fn eval_pair(&self, b: &OBoard, p: &Prim, o: &Ob, e: &mut PairEval) -> PairClass {
        let extra_reach = 2.0 * self.max_step;
        *e = PairEval::default();
        match o.kind {
            ObKind::Seg => {
                let t2 = o.trace;
                let s2 = o.seg;
                let h2 = o.h as f64;
                let mut self_like = false;
                if p.disc {
                    if o.net == p.net {
                        return PairClass::Na;
                    }
                } else {
                    if o.layer != p.layer {
                        return PairClass::Na;
                    }
                    if t2 == p.trace {
                        if s2 >= p.ex_lo && s2 <= p.ex_hi {
                            return PairClass::Na;
                        }
                        self_like = true;
                    } else if o.net == p.net {
                        // ADAPTED: pour hook 2, distinct pour paths may slide over their own net's copper.
                        let pour = |t: i32| t >= 0 && b.traces.get(t as usize)
                            .is_some_and(|tr| b.pour_rules.binary_search(&tr.rule).is_ok());
                        if pour(p.trace) || pour(t2) {
                            return PairClass::Na;
                        }
                        // Junction-connected same-net traces may slide over one another.
                        if p.trace >= 0 && p.trace < self.c.jgroup.len() as i32 && self.c.jgroup[p.trace as usize] >= 0 {
                            return PairClass::Na;
                        }
                        self_like = true;
                    }
                }
                if self_like {
                    let rr = self.self_thr + 2.0 * self.max_step;
                    if Self::bbox_gap2(p, o) > rr * rr {
                        return PairClass::Far;
                    }
                    let c = seg_seg(p.a, p.b, o.a, o.b);
                    e.gap = c.d;
                    e.thr = self.self_thr;
                    e.target = self.self_thr;
                    e.target_wide = self.self_thr;
                    e.t = c.t;
                    e.vx = c.vx;
                    e.vy = c.vy;
                    e.repel = false;
                    return PairClass::Eval;
                }
                e.partner = !p.disc && o.net == self.partner_of(p.net) && o.net >= 0;
                let r_eff = if p.disc { self.drilled_radius(p.h, p.drill) } else { p.h };
                // ADAPTED: pour hook 5, copper that is not a pour path keeps `POUR_SHARE` of its
                // margin from a free pour path.
                let yields = b.pour_rules.binary_search(&p.rule).is_err()
                    && b.pour_rules.binary_search(&o.rule).is_ok()
                    && !b.traces[t2 as usize].locked;
                let margin_wide = if yields { POUR_SHARE * self.margin_wide } else { self.margin_wide };
                let reach = r_eff + h2 + self.clr_g + self.max_extra + margin_wide + extra_reach + if e.partner { 2.0 } else { 0.0 };
                if Self::bbox_gap2(p, o) > reach * reach {
                    return PairClass::Far;
                }
                let c = seg_seg(p.a, p.b, o.a, o.b);
                // ADAPTED: OrganicRelaxer.cpp:1029 uses rule rows on p.layer (segment) or o.layer (disc).
                let mut shift = self.pair_shift(b, p.rule, o.rule, if p.disc { o.layer } else { p.layer });
                // pairShift(p.net, o.net, tracks): a track of the pair partner keeps the pair gap itself,
                // also below the clearance, as Fab's DRC asks of partner tracks on one layer.
                if !p.disc && o.net >= 0 && self.pair_partner_track(p.net, o.net) {
                    shift = self.pair_gap[p.net as usize] as f64 - self.clr_rule;
                }
                e.gap = c.d - r_eff - h2 - shift;
                e.thr = self.clr_g;
                e.target = if e.partner { self.clr_g } else { self.clr_g + self.margin };
                e.target_wide = if e.partner { self.clr_g } else { self.clr_g + self.margin_wide };
                if !p.disc && !self.bundle_net.is_empty() && self.same_bundle(p.net, o.net, p.layer) {
                    e.target = self.clr_g;
                    e.target_wide = self.clr_g;
                }
                if p.disc && self.pass_relief > 0.0 {
                    let sc = self.pass_scale(p, Some(o));
                    e.target = self.clr_g + sc * self.margin;
                    e.target_wide = self.clr_g + sc * self.margin_wide;
                }
                if yields {
                    e.target = self.clr_g + POUR_SHARE * self.margin;
                    e.target_wide = self.clr_g + margin_wide;
                }
                e.t = c.t;
                e.vx = c.vx;
                e.vy = c.vy;
                e.repel = true;
                PairClass::Eval
            }
            ObKind::Pad => {
                let pad = &b.pads[o.id as usize];
                if o.net >= 0 && o.net == p.net {
                    // Same-net via drills and forbidden via-in-pad contacts are hard limits.
                    let pm = &self.c.pad_m[o.id as usize];
                    if !p.disc {
                        return PairClass::Na;
                    }
                    let vip = !self.via_in_pad_ok(p.net) && self.disc_on_pad(b, p, pad);
                    if !pm.hole && !vip {
                        return PairClass::Na;
                    }
                    let mut best = K_BIG;
                    // ADAPTED: the hard level (see `PairEval::hard_extra`)
                    let mut best_h = K_BIG;
                    let mut cb = Closest::default();
                    let mut far = true;
                    if pm.hole {
                        let hg = self.hole_h2h + (self.clr_g - self.clr_rule);
                        let rr = 0.5 * p.drill + pm.hr + hg + 2.0 * self.max_step + 0.5 * pm.ha.distance_to(pm.hb) as f64;
                        let dx0 = (p.a.x - pad.center.x) as f64;
                        let dy0 = (p.a.y - pad.center.y) as f64;
                        if dx0 * dx0 + dy0 * dy0 <= rr * rr {
                            far = false;
                            let ch = seg_hole(p.a, p.a, pm);
                            best = ch.d - 0.5 * p.drill - pm.hr - hg + self.clr_g;
                            best_h = best;
                            cb = ch;
                        }
                    }
                    if vip {
                        let reach = p.h + self.clr_g + extra_reach;
                        if Self::bbox_gap2(p, o) <= reach * reach {
                            far = false;
                            let c = seg_pad(p.a, p.a, pm);
                            let g = c.d - pm.r - p.h;
                            // ADAPTED: the drill wall at the pad's via drill gap, against clr_g
                            let gh = c.d - pm.r - 0.5 * p.drill - pad.via_drill_gap as f64 + self.clr_g;
                            best_h = std_min(best_h, gh);
                            if g < best {
                                best = g;
                                cb = c;
                            }
                        }
                    }
                    if far {
                        return PairClass::Far;
                    }
                    e.gap = best;
                    e.hard_extra = best_h - best;
                    e.thr = self.clr_g;
                    e.target = self.clr_g;
                    e.target_wide = self.clr_g;
                    e.t = 1.0;
                    e.vx = cb.vx;
                    e.vy = cb.vy;
                    e.repel = false;
                    return PairClass::Eval;
                }
                if !p.disc && !pad_on_layer(pad, p.layer, self.n_l) {
                    return PairClass::Na;
                }
                let r_eff = if p.disc { self.drilled_radius(p.h, p.drill) } else { p.h };
                let reach = r_eff + self.clr_g + self.max_extra + self.margin_wide + extra_reach + self.hole_extra;
                if Self::bbox_gap2(p, o) > reach * reach {
                    return PairClass::Far;
                }
                let pm = &self.c.pad_m[o.id as usize];
                let mut c = seg_pad(p.a, p.b, pm);
                // ADAPTED: OrganicRelaxer.cpp:1102 uses the pad's rule row, on p.layer or all disc layers (-1).
                let cls_shift = self.pair_shift(b, p.rule, pad.rule, if p.disc { -1 } else { p.layer });
                // ADAPTED: OrganicRelaxer.cpp:1103 uses the same shift; Fab has no separate pad override.
                e.gap = c.d - pm.r - r_eff - cls_shift;
                if pm.hole {
                    let ch = seg_hole(p.a, p.b, pm);
                    let hg = self.pad_hole_gap(pm, ch.d, p) - cls_shift;
                    if hg < e.gap {
                        e.gap = hg;
                        c = ch;
                    }
                }
                let mut ms = self.partner_margin_scale(p.net, pad.net);
                if p.pass > 0.0_f32 && self.pass_relief > 0.0 {
                    ms *= self.pass_scale(p, None);
                }
                e.thr = self.clr_g;
                e.target = self.clr_g + ms * self.margin;
                e.target_wide = self.clr_g + ms * self.margin_wide;
                e.t = c.t;
                e.vx = c.vx;
                e.vy = c.vy;
                e.repel = true;
                PairClass::Eval
            }
            ObKind::Via => {
                if p.disc && o.id == p.via_id {
                    return PairClass::Na;
                }
                if o.net == p.net {
                    if !p.disc {
                        return PairClass::Na;
                    }
                    let hg = self.hole_h2h + (self.clr_g - self.clr_rule);
                    let rr = p.drill * 0.5 + o.drill as f64 * 0.5 + hg + 2.0 * self.max_step;
                    let dx = (p.a.x - o.a.x) as f64;
                    let dy = (p.a.y - o.a.y) as f64;
                    if dx * dx + dy * dy > rr * rr {
                        return PairClass::Far;
                    }
                    e.gap = (dx * dx + dy * dy).sqrt() - 0.5 * (p.drill + o.drill as f64);
                    e.thr = hg;
                    e.target = hg;
                    e.target_wide = hg;
                    e.t = 1.0;
                    e.vx = dx;
                    e.vy = dy;
                    e.repel = self.via_extra > 0.0 && e.gap < hg;
                    return PairClass::Eval;
                }
                let v_extra = if p.disc { self.via_extra } else { 0.0 };
                let reach = p.h + self.clr_g + self.max_extra + self.margin_wide + extra_reach + self.hole_extra + v_extra;
                if Self::bbox_gap2(p, o) > reach * reach {
                    return PairClass::Far;
                }
                let c = seg_seg(p.a, p.b, o.a, o.a);
                e.gap = if p.disc {
                    self.via_via_gap(c.d, p.h, p.drill, o.h as f64, o.drill as f64)
                } else {
                    c.d - self.drilled_radius(o.h as f64, o.drill as f64) - p.h
                };
                // ADAPTED: OrganicRelaxer.cpp:1141 uses rule rows on p.layer or all disc layers (-1).
                e.gap -= self.pair_shift(b, p.rule, o.rule, if p.disc { -1 } else { p.layer });
                let mut ms = self.partner_margin_scale(p.net, o.net);
                if p.pass > 0.0_f32 && self.pass_relief > 0.0 {
                    ms *= self.pass_scale(p, None);
                }
                e.thr = self.clr_g;
                e.target = self.clr_g + ms * (self.margin + v_extra);
                e.target_wide = self.clr_g + ms * (self.margin_wide + v_extra);
                e.t = c.t;
                e.vx = c.vx;
                e.vy = c.vy;
                e.repel = true;
                PairClass::Eval
            }
            ObKind::Edge => {
                let reach = p.h + self.edge_g + self.margin + extra_reach;
                if Self::bbox_gap2(p, o) > reach * reach {
                    return PairClass::Far;
                }
                let c = seg_seg(p.a, p.b, o.a, o.b);
                e.gap = c.d - p.h;
                e.thr = self.edge_g;
                e.target = self.edge_g + self.margin * 0.5;
                e.target_wide = e.target;
                e.t = c.t;
                e.vx = c.vx;
                e.vy = c.vy;
                e.repel = true;
                PairClass::Eval
            }
            ObKind::Ko => {
                let ke = &self.c.ko_edges[o.id as usize];
                if ke.copper && ke.net >= 0 && p.net == ke.net {
                    return PairClass::Na;
                }
                if ke.only_net >= 0 && p.net != ke.only_net {
                    return PairClass::Na;
                }
                if ke.courtyard >= 0 {
                    if !p.disc || p.via_id < 0 {
                        return PairClass::Na;
                    }
                    let own = &self.c.cy_nets[ke.courtyard as usize];
                    if p.net >= 0 && own.binary_search(&p.net).is_ok() {
                        return PairClass::Na;
                    }
                }
                if p.disc {
                    let mut span = u64::MAX;
                    if p.via_id >= 0 && p.via_id < b.vias.len() as i32 {
                        let v = &b.vias[p.via_id as usize];
                        let lo = std_max(0, std_min(v.layer_start, v.layer_end));
                        let hi = std_min(63, std_max(v.layer_start, v.layer_end));
                        span = 0;
                        for l in lo..=hi {
                            span |= 1_u64 << l;
                        }
                    }
                    if ke.via_layers & span == 0 {
                        return PairClass::Na;
                    }
                } else if p.layer < 0 || p.layer >= 64 || ke.track_layers & (1_u64 << p.layer) == 0 {
                    return PairClass::Na;
                }
                let reach = p.h + ke.thr + if ke.copper { self.max_extra } else { 0.0 } + self.margin + extra_reach;
                if Self::bbox_gap2(p, o) > reach * reach {
                    return PairClass::Far;
                }
                let c = if ke.area < 0 {
                    seg_seg(p.a, p.b, o.a, o.b)
                } else {
                    // ADAPTED: OrganicRelaxer.cpp:1190-1216 (brief:1180-1205) AvoidArea/RoutingIntents are absent; no such edge is built.
                    unreachable!("avoid-area edges are not built for OBoard")
                };
                e.gap = c.d - p.h;
                if ke.copper {
                    // ADAPTED: OrganicRelaxer.cpp:1218 has no copper shape rule in OBoard; use row 0 on the
                    // segment's layer, every layer (-1) for a disc.
                    e.gap -= self.pair_shift(b, p.rule, 0, if p.disc { -1 } else { p.layer });
                }
                // Keepouts have a hard threshold without a soft repulsion zone.
                e.thr = ke.thr;
                e.target = ke.thr;
                e.target_wide = ke.thr;
                e.t = c.t;
                e.vx = c.vx;
                e.vy = c.vy;
                e.repel = true;
                PairClass::Eval
            }
        }
    }

    /// Shortest distance from a via centre to the copper of a pad.
    pub fn point_pad_dist(&self, b: &OBoard, p: Vec2, pad_id: i32) -> f64 {
        let _pad = &b.pads[pad_id as usize];
        let pm = &self.c.pad_m[pad_id as usize];
        seg_pad(p, p, pm).d - pm.r
    }

    pub fn pen_energy(g: f64, t: f64, tw: f64, k: f64) -> f64 {
        let a = std_max(0.0, t - g);
        let b = std_max(0.0, tw - g);
        0.5 * k * a * a + 0.125 * k * b * b
    }

    /// Coupling spring; repel_only keeps only the side closer than the target D.
    pub fn cpl_energy(d: f64, d_target: f64, kc: f64, repel_only: bool) -> f64 {
        let u = std_min(d, if repel_only { d_target } else { 3.0 * d_target }) - d_target;
        0.5 * kc * u * u
    }
}
