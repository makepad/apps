//! The tree's settings and the numbers derived from them (R14 §1).
//!
//! [`Keys`] holds what the user sets, with the reference's defaults.
//! [`Settings`] derives every number the stages use from them, once: the tip ramp, the moves
//! per layer, the side gaps, the foot, the tip and roof line spacings, the element radius
//! (§1.3) and the ladder of rounded radii volumes exist for.

use makepad_fab_slicer::geom::SCALE;

/// The bed foot's radius (mm): a fixed number, no key.
pub const FOOT_RADIUS: f64 = 3.75;
/// The least height (mm) a branch stands on the model: a fixed number, no key.
pub const MIN_HEIGHT_ON_MODEL: f64 = 1.0;
/// How much (mm, as a diameter) a branch on the model may grow by merges with branches to the bed:
/// a fixed number, no key.
pub const MODEL_MERGE_GROWTH: f64 = 1.0;
/// The foot's flare angle (radians): its radius grows by at most tan of it a layer.
const FOOT_ANGLE: f64 = 0.7;
/// What counts as empty everywhere (mm²): 1 µm².
pub const TINY_AREA: f64 = 1e-6;
/// The rung ladder's fixed threshold (mm): 1 mm times its growth factor.
const RUNG_THRESHOLD: f64 = 1.5;
/// Each rung past the linear steps is this times the one before.
const RUNG_RATIO: f64 = 1.5;
/// The linear step past the tip's rung when the threshold is that near it (mm).
const RUNG_STEP: f64 = 0.5;
/// The ladder goes up to at least this radius (mm).
const RUNG_TOP: f64 = 100.0;
/// How much less than the move (mm) an avoidance moves a layer: 2 nm.
const AVOIDANCE_SLACK: f64 = 2e-6;
/// Slack (in units of the quotient) when a count is rounded down, so a quotient that is whole in
/// the reference's integer units does not come out one short in floating point.
const WHOLE: f64 = 1e-9;

/// What the user sets (R14 §1.1), with the reference's defaults ([`Keys::new`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Keys {
    /// Layer height (mm).
    pub layer_height: f64,
    /// Support line width (mm): everything that says "line".
    pub line_width: f64,
    /// The narrowest the walls print (mm): what §2.1 keeps of the slice. The reference keeps
    /// what is a line wide; Fab's walls print down to their smallest feature, and what prints
    /// is held like the rest.
    pub narrowest: f64,
    /// Outer wall line width (mm): what xy_min comes from.
    pub wall_width: f64,
    /// The tip's diameter (mm).
    pub tip_diameter: f64,
    /// The branch's diameter (mm).
    pub branch_diameter: f64,
    /// How fast a branch widens (degrees): the taper below the tip ramp.
    pub branch_diameter_angle_deg: f64,
    /// The most a branch leans (degrees): the fast move.
    pub branch_angle_deg: f64,
    /// The lean a branch prefers (degrees): the slow move, capped by the fast one.
    pub angle_slow_deg: f64,
    /// How densely tips sit along their lines (%).
    pub top_rate: f64,
    /// The line gap (mm) for tips without a roof.
    pub branch_distance: f64,
    /// The walls round a base island: 0 is the automatic second wall on big islands.
    pub wall_count: usize,
    /// The side gap (mm).
    pub xy_distance: f64,
    /// The gap (mm) the first layer keeps from the part.
    pub first_layer_gap: f64,
    /// The top gap (mm).
    pub top_distance: f64,
    /// The bottom gap (mm) on the part's tops.
    pub bottom_distance: f64,
    /// The roof's layers.
    pub roof_layers: usize,
    /// The floor's layers, on the part.
    pub floor_layers: usize,
    /// The roof's line gap (mm).
    pub interface_spacing: f64,
    /// The smallest roof layer (mm²).
    pub interface_min_area: f64,
    /// The overhang test's angle (degrees).
    pub threshold_deg: f64,
    /// Whether supports stand on the bed only.
    pub bed_only: bool,
    /// Whether small overhangs go (§2.7, on in R14's profile).
    pub remove_small: bool,
    /// The first support layer's fill (%).
    pub first_layer_density: f64,
    /// How finely outlines are simplified (mm).
    pub resolution: f64,
}

impl Keys {
    /// The reference's defaults at a layer height and line widths (mm).
    pub fn new(layer_height: f64, line_width: f64, wall_width: f64) -> Keys {
        Keys {
            layer_height,
            line_width,
            narrowest: line_width,
            wall_width,
            tip_diameter: 0.8,
            branch_diameter: 2.0,
            branch_diameter_angle_deg: 5.0,
            branch_angle_deg: 40.0,
            angle_slow_deg: 25.0,
            top_rate: 30.0,
            branch_distance: 1.0,
            wall_count: 0,
            xy_distance: 0.35,
            first_layer_gap: 0.2,
            top_distance: 0.2,
            bottom_distance: 0.2,
            roof_layers: 3,
            floor_layers: 0,
            interface_spacing: 0.5,
            interface_min_area: 0.25,
            threshold_deg: 30.0,
            bed_only: false,
            remove_small: true,
            first_layer_density: 90.0,
            resolution: 0.01,
        }
    }
}

/// `x` rounded down, as a count.
fn whole(x: f64) -> usize {
    (x + WHOLE).floor().max(0.0) as usize
}

/// `mm` truncated to whole nanometres.
fn nm(mm: f64) -> f64 {
    (mm * SCALE).trunc() / SCALE
}

/// Every number the stages use (R14 §1.2), from the keys.
#[derive(Clone, Debug)]
pub struct Settings {
    pub keys: Keys,
    /// The tip radius r_tip (mm).
    pub r_tip: f64,
    /// The branch radius r_br (mm).
    pub r_br: f64,
    /// Layers from r_tip to r_br: the larger of (r_br − r_tip) over a third of a line and r_br
    /// over the layer height, each rounded down.
    pub tip_layers: usize,
    /// How much the radius grows a layer below the ramp (mm).
    pub taper: f64,
    /// The fast move a layer (mm), in whole nm.
    pub fast: f64,
    /// The slow move a layer (mm), in whole nm.
    pub slow: f64,
    /// How far the fast and fast hole-free avoidances move a layer (mm): the fast move less 2 nm.
    pub avoid_fast: f64,
    /// How far the slow avoidance moves a layer (mm): the slow move less 2 nm.
    pub avoid_slow: f64,
    /// xy_min (mm): the smaller of the side gap and half the outer wall.
    pub xy_min: f64,
    /// What a branch keeping the full side gap adds to its radius (mm).
    pub xy_delta: f64,
    /// D28, Fab's: what the first layer's collision keeps from the part besides xy_min (mm), the
    /// first layer's gap where it is wider than xy_min, else nothing: what stands there keeps
    /// the gap the pad does ([`super::draw::pad`]), so the pad is under it.
    pub first_gap: f64,
    /// The top gap in layers, rounded.
    pub top_gap_layers: usize,
    /// The bottom gap in layers, rounded.
    pub bottom_gap_layers: usize,
    /// The least layers a branch stands on the model.
    pub min_height_on_model: usize,
    /// How much radius (mm) a branch on the model may gain in all by merges with branches to the
    /// bed.
    pub model_merge_cap: f64,
    /// How much the foot's radius grows a layer (mm).
    pub foot_growth: f64,
    /// The layer above the bed the foot starts growing under.
    pub foot_start: usize,
    /// What one foot count adds to the radius (mm): the foot's growth less the taper.
    pub foot_step: f64,
    /// Tip spacing along lines (mm).
    pub connect_length: f64,
    /// The support's flow spacing (mm): a line less what the layer's rounded edges take.
    pub flow_spacing: f64,
    /// The roof's line spacing (mm), what tips under a roof are sampled on.
    pub roof_line_spacing: f64,
    /// The line spacing for tips without a roof (mm).
    pub tip_line_spacing: f64,
    /// The grid tips are de-duplicated on per layer (nm): (r_tip + 1 nm) ÷ 10 in whole nm.
    pub dedup_cell: i64,
    /// How far a face hidden by the clearance may be reached for (mm): the outset's cap.
    pub hidden_outset: f64,
    /// The rounded radii (mm), rising from 0; [`Settings::zero_full`] past them.
    ladder: Vec<f64>,
}

impl Settings {
    pub fn new(keys: Keys) -> Settings {
        let k = &keys;
        let (h, w) = (k.layer_height, k.line_width);
        let r_tip = k.tip_diameter / 2.0;
        let r_br = k.branch_diameter / 2.0;
        let tip_layers = whole((r_br - r_tip) / (w / 3.0)).max(whole(r_br / h));
        let taper = k.branch_diameter_angle_deg.to_radians().tan() * h;
        let fast = nm(k.branch_angle_deg.to_radians().tan() * h);
        let slow = nm(k.angle_slow_deg.min(k.branch_angle_deg).to_radians().tan() * h);
        let xy_min = k.xy_distance.min(k.wall_width / 2.0);
        let foot_growth = (FOOT_ANGLE.tan() * h).min(w / 2.0);
        let flow_spacing = w - h * (1.0 - std::f64::consts::FRAC_PI_4);
        let mut out = Settings {
            r_tip,
            r_br,
            tip_layers,
            taper,
            fast,
            slow,
            avoid_fast: fast - AVOIDANCE_SLACK,
            avoid_slow: slow - AVOIDANCE_SLACK,
            xy_min,
            xy_delta: k.xy_distance - xy_min,
            first_gap: if k.first_layer_gap > xy_min { k.first_layer_gap } else { 0.0 },
            top_gap_layers: (k.top_distance / h).round() as usize,
            bottom_gap_layers: (k.bottom_distance / h).round() as usize,
            min_height_on_model: (MIN_HEIGHT_ON_MODEL / h - WHOLE).ceil() as usize,
            model_merge_cap: MODEL_MERGE_GROWTH / 2.0,
            foot_growth,
            foot_start: whole((FOOT_RADIUS - r_br) / foot_growth),
            foot_step: foot_growth - taper,
            connect_length: w * 100.0 / k.top_rate + (2.0 * r_tip - w).max(0.0),
            flow_spacing,
            roof_line_spacing: k.interface_spacing + flow_spacing,
            tip_line_spacing: k.branch_distance + flow_spacing,
            dedup_cell: ((r_tip * SCALE).round() as i64 + 1) / 10,
            hidden_outset: (r_tip - w / 2.0).max(0.0) + if k.xy_distance > xy_min { w / 2.0 } else { 0.0 },
            keys,
            ladder: Vec::new(),
        };
        out.ladder = out.ladder();
        out
    }

    /// The rungs (§1.3): 0, r_tip, then one linear step to the threshold and a second past it
    /// when the threshold is more than [`RUNG_STEP`] above r_tip (else one [`RUNG_STEP`]), then
    /// each [`RUNG_RATIO`] times the last in whole nm; less the rungs at or under r_br that no
    /// ramp radius, nor one at the full side gap, rounds up to.
    fn ladder(&self) -> Vec<f64> {
        let r_tip = self.r_tip;
        let d = RUNG_THRESHOLD - r_tip;
        let mut all = vec![0.0, r_tip];
        if d > RUNG_STEP {
            all.push(r_tip + d);
            all.push(r_tip + 2.0 * d);
        } else {
            all.push(r_tip + RUNG_STEP);
        }
        // In whole nm, as the reference keeps them.
        let mut last = (all[all.len() - 1] * SCALE).round() as i64;
        while (last as f64) < RUNG_TOP * SCALE {
            last = (last as f64 * RUNG_RATIO) as i64;
            all.push(last as f64 / SCALE);
        }
        let up = |r: f64| all.iter().position(|&x| x >= r - 1e-9).unwrap_or(all.len() - 1);
        let asked: Vec<usize> = (0..=self.tip_layers).flat_map(|n| [up(self.radius(n, 0.0)), up(self.radius(n, 0.0) + self.xy_delta)]).collect();
        all.iter().enumerate().filter(|&(i, &r)| i < 2 || r > self.r_br || asked.contains(&i)).map(|(_, &r)| r).collect()
    }

    /// The radius (mm) at counter `n` (layers below the tip) with foot count `foot` (§1.3): linear
    /// from r_tip to r_br over the ramp, then the taper, plus the foot.
    pub fn radius(&self, n: usize, foot: f64) -> f64 {
        let ramp = if n <= self.tip_layers {
            self.r_tip + (self.r_br - self.r_tip) * n as f64 / self.tip_layers.max(1) as f64
        } else {
            self.r_br + (n - self.tip_layers) as f64 * self.taper
        };
        ramp + foot * self.foot_step
    }

    /// The radius (mm) routing keeps clear of the part: it follows the ramp counter.
    pub fn collision_radius(&self, ramp: usize, foot: f64) -> f64 {
        self.radius(ramp, foot)
    }

    /// The radius (mm) drawn: by the depth while both counters are inside the ramp, else by the
    /// ramp counter.
    pub fn drawn_radius(&self, depth: usize, ramp: usize, foot: f64) -> f64 {
        let inside = depth < self.tip_layers && ramp < self.tip_layers;
        self.radius(if inside { depth } else { ramp }, foot)
    }

    /// The `i`th rung's radius (mm); [`Settings::zero_full`]'s is the xy delta.
    pub fn rung(&self, i: usize) -> f64 {
        if i == self.zero_full() { self.xy_delta } else { self.ladder[i] }
    }

    /// How many rungs there are, [`Settings::zero_full`] included.
    pub fn rungs(&self) -> usize {
        self.ladder.len() + 1
    }

    /// The rung radius 0 at the full side gap is kept at: exactly the xy delta, not rounded.
    pub fn zero_full(&self) -> usize {
        self.ladder.len()
    }

    /// The rung a radius (mm) rounds up to.
    pub fn rung_of(&self, radius: f64) -> usize {
        self.ladder.iter().position(|&x| x >= radius - 1e-9).unwrap_or(self.ladder.len() - 1)
    }

    /// The rung a branch of `radius` (mm) asks volumes for, at xy_min or keeping the full side
    /// gap.
    pub fn rung_for(&self, radius: f64, full_xy: bool) -> usize {
        match (full_xy, radius <= 0.0) {
            (true, true) => self.zero_full(),
            (true, false) => self.rung_of(radius + self.xy_delta),
            (false, _) => self.rung_of(radius),
        }
    }

    /// The foot's target radius (mm) on layer `l` for a branch at or past r_br: r_br, plus a
    /// layer's growth for each layer it is under the foot's start.
    pub fn foot_target(&self, l: usize) -> f64 {
        self.r_br + self.foot_start.saturating_sub(l) as f64 * self.foot_growth
    }

    /// The rungs a branch on the ramp asks for (§1.3): each ramp radius's, at xy_min and keeping
    /// the full side gap, in order.
    pub fn ramp_rungs(&self) -> Vec<usize> {
        let mut out: Vec<usize> = (0..=self.tip_layers).flat_map(|n| [false, true].map(|full| self.rung_for(self.radius(n, 0.0), full))).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Each rung past the ramp a branch under tips no higher than layer `top` can ask for, with
    /// the highest layer it can ask for it on: where the widest such branch's collision radius
    /// rounds up to it, keeping the full side gap. Its ramp counter is at most its depth, at most
    /// `top` less its layer; its foot grows at most a step a layer in the foot zone; a merge takes
    /// the larger counters and radius. A rung no branch reaches is left out.
    pub fn rung_tops(&self, top: usize) -> Vec<(usize, usize)> {
        let ramp = self.ramp_rungs();
        let (mut out, mut reached) = (Vec::new(), 0);
        for l in (0..=top).rev() {
            let rung = self.rung_for(self.radius(top - l, self.foot_start.saturating_sub(l) as f64), true);
            out.extend((reached + 1..=rung).filter(|r| !ramp.contains(r)).map(|r| (r, l)));
            reached = reached.max(rung);
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The profile R14 works its numbers on (§1.1): 0.2 mm layers, 0.42 mm lines, 45°, 2 roof and
    /// 2 floor layers, 0.012 mm resolution, at side gap `xy`.
    pub(crate) fn r14(xy: f64) -> Settings {
        Settings::new(Keys { branch_angle_deg: 45.0, roof_layers: 2, floor_layers: 2, resolution: 0.012, xy_distance: xy, ..Keys::new(0.2, 0.42, 0.42) })
    }

    fn near(what: &str, got: f64, want: f64, tol: f64) {
        assert!((got - want).abs() <= tol, "§1.2 {what}: {got} mm, R14 says {want}");
    }

    /// §1.2, every row at R14's profile.
    #[test]
    fn the_derived_numbers_are_r14s() {
        let s = r14(0.35);
        assert_eq!(s.tip_layers, 5, "§1.2 tip layers: max(4, 5)");
        near("taper per layer", s.taper, 0.0175, 5e-5);
        near("fast move, whole nm (tan 45° is just under 1)", s.fast, 0.199_999, 1e-12);
        near("slow move, whole nm", s.slow, 0.093_261, 1e-12);
        near("avoidance move, fast and fast hole-free", s.avoid_fast, 0.199_997, 1e-12);
        near("avoidance move, slow", s.avoid_slow, 0.093_259, 1e-12);
        for (xy, delta) in [(0.35, 0.14), (0.8, 0.59), (2.0, 1.79)] {
            let s = r14(xy);
            near(&format!("xy_min at xy {xy}"), s.xy_min, 0.21, 1e-9);
            near(&format!("xy delta at xy {xy}"), s.xy_delta, delta, 1e-9);
        }
        assert_eq!((s.top_gap_layers, s.bottom_gap_layers), (1, 1), "§1.2 top and bottom gap in layers");
        assert_eq!(s.min_height_on_model, 5, "§1.2 min height on model");
        near("model merge cap", s.model_merge_cap, 0.5, 1e-9);
        near("radius-catch-up limit", s.radius(s.tip_layers, 0.0), s.r_br, 1e-9);
        assert!(s.radius(s.tip_layers - 1, 0.0) < s.r_br, "§1.2 the catch-up limit r_br is reached at layer 5 of the ramp, not before");
        near("bed-foot radius", FOOT_RADIUS, 3.75, 0.0);
        near("foot growth per layer", s.foot_growth, 0.168, 5e-4);
        assert_eq!(s.foot_start, 16, "§1.2 foot start");
        near("foot step", s.foot_step, 0.151, 5e-4);
        near("connect length", s.connect_length, 1.78, 1e-9);
        near("roof line spacing", s.roof_line_spacing, 0.877, 5e-4);
        near("tip line spacing without a roof", s.tip_line_spacing, 1.377, 5e-4);
        assert_eq!(s.dedup_cell, 40_000, "§1.2 tip de-dup cell: (r_tip + 1 nm) ÷ 10 in whole nm");
        near("outset for hidden faces", s.hidden_outset, 0.40, 1e-9);
        near("outset for hidden faces with xy at xy_min", r14(0.21).hidden_outset, 0.19, 1e-9);
        near("tiny area", TINY_AREA, 1e-6, 0.0);
        // A gap that is not whole layers rounds, not up.
        let k = Keys { top_distance: 0.25, ..Keys::new(0.2, 0.42, 0.42) };
        assert_eq!(Settings::new(k).top_gap_layers, 1, "§1.2 a 0.25 mm gap at 0.2 mm layers is 1 layer, rounded");
        // The slow angle is capped by the fast one.
        let k = Keys { branch_angle_deg: 20.0, ..Keys::new(0.2, 0.42, 0.42) };
        let capped = Settings::new(k);
        near("slow move capped by the fast", capped.slow, capped.fast, 1e-12);
    }

    /// §1.3: the radius ramps linearly to r_br, then tapers, plus the foot; the drawn radius
    /// follows the depth only inside the ramp.
    #[test]
    fn an_elements_radius_ramps_tapers_and_feet() {
        let s = r14(2.0);
        near("radius at the tip", s.radius(0, 0.0), 0.4, 1e-12);
        near("radius half way up the ramp", s.radius(2, 0.0), 0.4 + 0.6 * 2.0 / 5.0, 1e-12);
        near("radius 10 layers below the ramp", s.radius(15, 0.0), 1.0 + 10.0 * s.taper, 1e-12);
        near("radius with 2.5 foot counts", s.radius(15, 2.5), 1.0 + 10.0 * s.taper + 2.5 * s.foot_step, 1e-12);
        near("collision radius follows the ramp counter", s.collision_radius(3, 0.0), s.radius(3, 0.0), 0.0);
        near("drawn radius in the ramp follows the depth", s.drawn_radius(4, 2, 0.0), s.radius(4, 0.0), 0.0);
        near("drawn radius past the ramp follows the ramp counter", s.drawn_radius(9, 4, 0.0), s.radius(4, 0.0), 0.0);
        near("drawn radius with the ramp counter past the ramp", s.drawn_radius(3, 6, 0.0), s.radius(6, 0.0), 0.0);
        // The foot target: r_br at the foot's start, 16 layers of growth on the bed.
        near("foot target at layer 16", s.foot_target(16), 1.0, 1e-12);
        near("foot target on the bed", s.foot_target(0), 1.0 + 16.0 * s.foot_growth, 1e-12);
        assert!(s.foot_target(0) <= FOOT_RADIUS, "the foot's growth stops at the bed-foot radius");
    }

    /// §1.3: the ladder 0, 0.4 → 1.5 → 2.6 → 3.9 → 5.85 → 8.775 → 13.1625, every radius rounded
    /// up, full xy asking for the rung at the radius plus the xy delta, radius 0 at full xy kept at
    /// exactly the delta; the table of tip and r_br rungs per variant.
    #[test]
    fn radii_round_up_to_the_ladder() {
        let s = r14(2.0);
        for (i, want) in [(0, 0.0), (1, 0.4), (2, 1.5), (3, 2.6), (4, 3.9), (5, 5.85), (6, 8.775), (7, 13.1625)] {
            near(&format!("rung {i}"), s.rung(i), want, 1e-9);
        }
        assert_eq!(s.rung_of(0.0), 0, "radius 0 is its own rung");
        assert_eq!(s.rung_of(0.4), 1, "the tip's radius is a rung");
        assert_eq!(s.rung_of(0.41), 2, "just past the tip rounds up to 1.5");
        assert_eq!(s.rung_of(3.75), 4, "the bed foot rounds up to 3.9");
        let zero = s.rung_for(0.0, true);
        assert_eq!((zero, s.rung(zero)), (s.zero_full(), s.xy_delta), "radius 0 at full xy is keyed at exactly the xy delta");
        for (xy, tip_full, br_min, br_full) in [(0.35, 1.5, 1.5, 1.5), (0.8, 1.5, 1.5, 2.6), (2.0, 2.6, 1.5, 3.9)] {
            let s = r14(xy);
            near(&format!("tip rung, min kind, xy {xy}"), s.rung(s.rung_for(0.4, false)), 0.4, 1e-9);
            near(&format!("tip rung, full kind, xy {xy}"), s.rung(s.rung_for(0.4, true)), tip_full, 1e-9);
            near(&format!("r_br rung, min kind, xy {xy}"), s.rung(s.rung_for(1.0, false)), br_min, 1e-9);
            near(&format!("r_br rung, full kind, xy {xy}"), s.rung(s.rung_for(1.0, true)), br_full, 1e-9);
        }
        // A 2.4 mm tip: the threshold is within a step of it, so one 0.5 mm step, then × 1.5.
        let wide = Settings::new(Keys { tip_diameter: 2.4, branch_diameter: 4.0, ..r14(0.35).keys });
        for (i, want) in [(1, 1.2), (2, 1.7), (3, 2.55)] {
            near(&format!("rung {i} for a 1.2 mm tip"), wide.rung(i), want, 1e-9);
        }
    }
}
