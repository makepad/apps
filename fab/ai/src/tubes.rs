//! `fab.tubes` and `fab.orient`: a print breaks along its layers, so a tube standing up is at its
//! weakest and a tube lying flat at its strongest. `fab.tubes` lists a part's tube-like limbs from
//! its layers, named or not, and how each lies; `fab.orient` answers which way up serves them, or
//! serves least support, in one table with the trade in it, and applies a row as the part's normal
//! move, one undo step.
//!
//! Everything's read from the part's layers (a slice's, or sections of the model with none
//! current), so a plain mesh with nothing named takes the same path as a named figure. Gains come
//! from the filament's own strength model and say they're estimates; angles after a move are
//! measured again from the new slice by the next `fab.tubes`.

use crate::out::{arr, flag, int, mm3, n, obj, s, S};
use crate::supports::{ColoredMesh, LookRequest};
use crate::volumes::{on_layers, pick_frame, read_cell, solid_box, PartLayers};
use crate::{str_arg, PartMove, RenderRequest, Reply, Session};
use fab_force::material::Filament;
use fab_model::eval::Evaluated;
use fab_model::paint::Feature;
use fab_model::align::limb_of;
use fab_model::params::{rotate, turned_about, xf_dir, xf_point};
use fab_model::program::Compiled;
use fab_model::{Change, Head};
use fab_model::program::{part_id, split_id};
use fab_model::volumes::{fit_points, Fit, VolumeShape};
use fab_printability::{angle_to_bed, fit_limb, flattest_up, least_support_ups, score_up, solid_samples, turn_to_up, weak_spots, HeldTube, LimbEnd, LimbFit, LimbStation, OrientMesh, SpotKind, UpScore, WeakSpot};
use makepad_fab_slicer::geom::ExPolygon;
use makepad_model::json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const TUBES_TOOL: &str = "tubes";
pub const ORIENT_TOOL: &str = "orient";

/// How thick a limb can be and still be looked for when nobody says: this share of the part's
/// longest side.
const TUBE_SHARE: f64 = 0.15;
/// And of the two, the thinner: a figure's neck and legs stand out at this.
const THIN_SHARE: f64 = 0.06;
/// Shorter than this share of the part, what follows from a weak spot is a detail, not a tube.
const SMALL_SHARE: f64 = 0.05;
/// Length over width: at least this and thin for the part, or at least [`LONG`] whatever its width.
const SLENDER: f64 = 1.5;
const LONG: f64 = 2.5;
/// A radius under this share of the part's longest side is thin for it.
const THIN_FOR_PART: f64 = 0.1;
/// A section reaching further than this over a round one of its area is a plate, not a tube.
const ROUND: f64 = 1.6;
/// A tube's reach over the radius its volume gives: a round section is near 1, a ring or a plate's past this.
const NOT_ROUND: f64 = 1.6;
/// A limb's fit off a feature's long axis by more than this (its cosine, 25 degrees) is another limb.
const SAME_AXIS: f64 = 0.906;
/// A feature this much of the part's longest side is the part, with no limb to follow from it.
const WHOLE_PART: f64 = 0.8;
/// Past this angle to the bed a tube is said to stay standing.
pub(crate) const STAYS_DEG: f64 = 15.0;
/// How far Align a limb goes when nobody says, and what's too little to offer.
pub const MAX_ALIGN_DEG: f64 = 15.0;
const ALIGN_MIN_DEG: f64 = 1.0;
/// How many ups a search brings back, and how many turns about up it tries to fit the bed.
const TOP_UPS: usize = 3;
const SPINS: usize = 12;

type Box3 = ([f64; 3], [f64; 3]);

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] - b[k])
}

fn along(a: [f64; 3], d: [f64; 3], t: f64) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + d[k] * t)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| a[k] * b[k]).sum()
}

fn apply(r: &[[f64; 3]; 3], p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| dot(r[i], p))
}

fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn spin_z(deg: f64) -> [[f64; 3]; 3] {
    let (sin, cos) = deg.to_radians().sin_cos();
    [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]]
}

pub(crate) fn off_segment(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(b, a);
    let dd = dot(d, d);
    let t = if dd > 0.0 { (dot(sub(p, a), d) / dd).clamp(0.0, 1.0) } else { 0.0 };
    let q = sub(p, along(a, d, t));
    dot(q, q).sqrt()
}

/// The filament and bead a part prints with: what a change of angle gains.
#[derive(Clone, Copy)]
pub struct Bead {
    pub filament: &'static Filament,
    pub height: f64,
    pub width: f64,
}

impl Bead {
    /// How many times stronger a line pulled at `to` degrees to the bed is than at `from`.
    pub fn gain(&self, from: f64, to: f64) -> f64 {
        self.filament.pull_strength(to.to_radians(), self.height, self.width) / self.filament.pull_strength(from.to_radians(), self.height, self.width)
    }

    pub fn layer_factor(&self) -> f64 {
        self.filament.layer_factor(self.height, self.width)
    }
}

/// One tube-like limb of a part. Its points and axis are on the plate.
#[derive(Clone, Debug)]
pub struct Tube {
    pub id: String,
    pub part: usize,
    pub object: String,
    pub feature: Option<String>,
    /// What the list calls it: the feature, or "limb near [x y z]" in model mm.
    pub called: String,
    pub axis: [f64; 3],
    pub start: [f64; 3],
    pub end: [f64; 3],
    pub length: f64,
    /// The thinnest section's radius, a disc of its area.
    pub thinnest: f64,
    /// How far its sides reach from its line.
    pub reach: f64,
    /// What it carries beyond it (mm^3).
    pub carries: f64,
    /// What a person would break first, however it lies: what orientation's weighted by.
    pub risk: f64,
    /// From the plate to the model, as the plate stood when it was measured.
    pub to_model: [[f64; 4]; 3],
}

impl Tube {
    pub fn angle(&self) -> f64 {
        angle_to_bed(self.axis, [0.0, 0.0, 1.0])
    }
}

/// A part's layers and what names what in them: a find off the model worker reads only this.
pub struct TubeScan {
    part: usize,
    object: String,
    read: PartLayers,
    to_model: [[f64; 4]; 3],
    features: Arc<Vec<Feature>>,
    /// Each feature's box on the layers.
    boxes: Vec<Option<Box3>>,
    max_thickness: Option<f64>,
    layer_factor: f64,
}

/// A tube's numbers from its limb fit, on the layers' frame; `None` if it isn't tube-like.
fn limb_numbers(limb: &LimbFit) -> Option<(f64, f64)> {
    let count = limb.stations.len();
    if count == 0 {
        return None;
    }
    let round = limb.stations.iter().filter(|st| st.reach <= ROUND * st.radius).count();
    if 2 * round < count {
        return None;
    }
    let mean = limb.stations.iter().map(|st| st.radius).sum::<f64>() / count as f64;
    // a rounded end narrows to nothing: that's its shape, not how thin the limb is
    let past_tip = |st: &&LimbStation| {
        let clear = |end: [f64; 3]| {
            let d = sub(st.centre, end);
            dot(d, d).sqrt() >= mean
        };
        (limb.ends[0] != LimbEnd::Tip || clear(limb.start)) && (limb.ends[1] != LimbEnd::Tip || clear(limb.end))
    };
    let thinnest = limb.stations.iter().filter(past_tip).map(|st| st.radius).fold(f64::MAX, f64::min);
    Some((mean, if thinnest == f64::MAX { mean } else { thinnest }))
}

/// How a break here reads with its layers taken out: weak spots rank a standing limb up by the
/// layer factor, orientation's what would change that.
fn risk_of(spot: &WeakSpot, factor: f64) -> f64 {
    spot.score / if spot.axis[2].abs() >= std::f64::consts::FRAC_1_SQRT_2 { factor } else { 1.0 }
}

/// A part's tubes, its most at risk first; `None` when cancelled. Named features that are long and
/// thin come first, the smallest first so a feature holding one isn't one more; then what the
/// weak spots find past them, followed by a fit.
pub fn tubes_of(scan: &TubeScan, stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<Tube>> {
    let layers = &scan.read.layers;
    let (lo, hi) = layers_box(layers);
    let longest = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
    // with no features to name its limbs, what's thin depends on the part: at one share a figure's
    // neck and legs read as one mass, at the other its thick limbs read as bulk
    let shares: &[f64] = if scan.features.is_empty() { &[THIN_SHARE, TUBE_SHARE] } else { &[TUBE_SHARE] };
    let mut spots = Vec::new();
    for thick in scan.max_thickness.map_or_else(|| shares.iter().map(|share| share * longest).collect(), |thick| vec![thick]) {
        spots.extend(weak_spots(layers, thick, scan.layer_factor, stop)?);
    }
    let stopping = || stop();
    let at = scan.read.at;
    let plate = |p: [f64; 3]| [p[0] + at[0], p[1] + at[1], p[2]];
    let model = |p: [f64; 3]| xf_point(&scan.to_model, plate(p));
    let mut tubes: Vec<Tube> = Vec::new();
    let mut order: Vec<usize> = (0..scan.features.len()).collect();
    order.sort_by(|a, b| scan.features[*a].volume.total_cmp(&scan.features[*b].volume).then(a.cmp(b)));
    for i in order {
        let feature = &scan.features[i];
        let Some(bounds) = scan.boxes[i] else { continue };
        if stop() {
            return None;
        }
        let cell = read_cell(&bounds);
        let mut points = Vec::new();
        solid_samples(layers, bounds, cell, &stopping, &mut |p| {
            if feature.own_distance(model(p)) <= 0.0 {
                points.push(p);
            }
        })
        .ok()?;
        // end to end, a short neck as well as a long arm: a capsule's caps would meet in the middle
        let Some(Fit::Cylinder { a, b, radius }) = fit_points(VolumeShape::Cylinder, &points, false) else { continue };
        let line = sub(b, a);
        let length = dot(line, line).sqrt();
        if length <= 0.0 {
            continue;
        }
        let axis = line.map(|x| x / length);
        let volume = points.len() as f64 * cell * cell * cell;
        let round = (volume / (std::f64::consts::PI * length)).sqrt();
        let slender = length / (2.0 * round);
        if !(slender >= LONG || (slender >= SLENDER && round <= THIN_FOR_PART * longest)) {
            continue;
        }
        // a ring or a flat plate is long too, but fills little of the tube round it
        if radius > NOT_ROUND * round {
            continue;
        }
        let middle = along(a, axis, 0.5 * length);
        // what holds a smaller tube end to end isn't one more; what only crosses it is
        let holds = |p: [f64; 3]| feature.own_distance(xf_point(&scan.to_model, p)) <= 0.0;
        if tubes.iter().any(|t| holds(along(t.start, t.axis, t.reach)) && holds(along(t.end, t.axis, -t.reach))) {
            continue;
        }
        let nearest = points.iter().copied().min_by(|p, q| {
            let (dp, dq) = (sub(*p, middle), sub(*q, middle));
            dot(dp, dp).total_cmp(&dot(dq, dq))
        })?;
        let limb = fit_limb(layers, &[nearest], &stopping).ok().filter(|l| dot(l.axis, axis).abs() >= SAME_AXIS);
        // no limb follows from its middle: it's in the body's bulk, the way a chest or a shoulder bar is
        if limb.is_none() && length < WHOLE_PART * longest {
            continue;
        }
        let thinnest = limb.as_ref().and_then(limb_numbers).map_or(round, |(_, thinnest)| thinnest);
        let spot = spots.iter().filter(|spot| feature.own_distance(model(spot.at)) <= 0.0).max_by(|p, q| p.score.total_cmp(&q.score));
        let own_risk = volume * 0.5 * length / (2.0 * thinnest).powi(3);
        let risk = spot.map_or(own_risk, |spot| risk_of(spot, scan.layer_factor)).max(own_risk);
        tubes.push(Tube {
            id: String::new(),
            part: scan.part,
            object: scan.object.clone(),
            feature: Some(feature.name.clone()),
            called: feature.name.clone(),
            axis,
            start: plate(a),
            end: plate(b),
            length,
            thinnest,
            reach: radius,
            carries: spot.map_or(volume, |spot| spot.carries),
            risk,
            to_model: scan.to_model,
        });
    }
    for spot in &spots {
        if stop() {
            return None;
        }
        if tubes.iter().any(|t| off_segment(plate(spot.at), t.start, t.end) <= t.reach + 0.5 * spot.thickness) {
            continue;
        }
        let Ok(limb) = fit_limb(layers, &[spot.middle], &stopping) else { continue };
        let Some((mean, thinnest)) = limb_numbers(&limb) else { continue };
        let line = sub(limb.end, limb.start);
        let length = dot(line, line).sqrt();
        // a neck is short and stays a tube: it's what snaps, joining two masses
        let neck = matches!(spot.kind, SpotKind::Neck);
        if !neck && (length < SLENDER * 2.0 * mean || length < SMALL_SHARE * longest) {
            continue;
        }
        let own_risk = limb.volume * 0.5 * length / (2.0 * thinnest).powi(3);
        let q = model(spot.at);
        let feature = fab_model::paint::feature_at(&scan.features, q, 0.0).map(|f| f.name.clone());
        let mid = model(along(limb.start, limb.axis, 0.5 * length));
        tubes.push(Tube {
            id: String::new(),
            part: scan.part,
            object: scan.object.clone(),
            called: feature.clone().unwrap_or_else(|| format!("{} near [{:.0} {:.0} {:.0}]", if neck { "neck" } else { "limb" }, mid[0], mid[1], mid[2])),
            feature,
            axis: limb.axis,
            start: plate(limb.start),
            end: plate(limb.end),
            length,
            thinnest,
            reach: limb.radius,
            carries: spot.carries,
            risk: risk_of(spot, scan.layer_factor).max(own_risk),
            to_model: scan.to_model,
        });
    }
    if tubes.is_empty() {
        tubes.extend(whole_part_tube(scan, (lo, hi), &stopping)?);
    }
    tubes.sort_by(|a, b| b.risk.total_cmp(&a.risk));
    Some(tubes)
}

/// A part with nothing thin on it can still be one long rod from end to end: it's its own tube.
fn whole_part_tube(scan: &TubeScan, bounds: Box3, stop: &dyn Fn() -> bool) -> Option<Option<Tube>> {
    let layers = &scan.read.layers;
    let cell = read_cell(&bounds);
    let mut points = Vec::new();
    solid_samples(layers, bounds, cell, &stop, &mut |p| points.push(p)).ok()?;
    let Some(Fit::Cylinder { a, b, radius }) = fit_points(VolumeShape::Cylinder, &points, false) else { return Some(None) };
    let line = sub(b, a);
    let length = dot(line, line).sqrt();
    if length <= 0.0 {
        return Some(None);
    }
    let axis = line.map(|x| x / length);
    let volume = points.len() as f64 * cell * cell * cell;
    let round = (volume / (std::f64::consts::PI * length)).sqrt();
    if length / (2.0 * round) < LONG {
        return Some(None);
    }
    let middle = along(a, axis, 0.5 * length);
    let nearest = points.iter().copied().min_by(|p, q| {
        let (dp, dq) = (sub(*p, middle), sub(*q, middle));
        dot(dp, dp).total_cmp(&dot(dq, dq))
    })?;
    // a plank's long too: its sections say it's not round
    let Some((_, thinnest)) = fit_limb(layers, &[nearest], stop).ok().as_ref().and_then(limb_numbers) else { return Some(None) };
    let at = scan.read.at;
    let plate = |p: [f64; 3]| [p[0] + at[0], p[1] + at[1], p[2]];
    let called = scan.object.rsplit('/').next().unwrap_or(&scan.object).to_string();
    Some(Some(Tube {
        id: String::new(),
        part: scan.part,
        object: scan.object.clone(),
        feature: None,
        called,
        axis,
        start: plate(a),
        end: plate(b),
        length,
        thinnest,
        reach: radius,
        carries: volume,
        risk: volume * 0.5 * length / (2.0 * thinnest).powi(3),
        to_model: scan.to_model,
    }))
}

fn layers_box<R: AsRef<[ExPolygon]>>(layers: &[(f64, f64, R)]) -> Box3 {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for (z, h, regions) in layers {
        for e in regions.as_ref() {
            for p in &e.contour.pts {
                lo[0] = lo[0].min(p.x_mm());
                hi[0] = hi[0].max(p.x_mm());
                lo[1] = lo[1].min(p.y_mm());
                hi[1] = hi[1].max(p.y_mm());
                lo[2] = lo[2].min(z - h);
                hi[2] = hi[2].max(*z);
            }
        }
    }
    (lo, hi)
}

fn number_list(p: [f64; 3]) -> S {
    arr(p.iter().map(|x| n((x * 1000.0).round() / 1000.0)))
}

/// A tube's row: how it lies, what it carries, and what lying flat would gain.
fn tube_row(t: &Tube, bead: &Bead, to_model: &[[f64; 4]; 3]) -> S {
    let angle = t.angle();
    let mut row = vec![
        ("id", s(&t.id)),
        ("object", s(&t.object)),
        ("name", s(&t.called)),
        ("angle_to_bed", n(angle)),
        ("axis", number_list(t.axis)),
        ("length", n(t.length)),
        ("thinnest_radius", n(t.thinnest)),
        ("carries_mm3", n(t.carries)),
        ("risk", n(t.risk)),
        ("flat_gain_estimate", n(bead.gain(angle, 0.0))),
        ("start", mm3(t.start)),
        ("end", mm3(t.end)),
        // what the panel draws, plain numbers
        ("band", obj(vec![("a", number_list(t.start)), ("b", number_list(t.end)), ("radius", n(t.reach))])),
        // what fab.settings_volume takes, in the model frame
        ("target", obj(vec![("shape", s("capsule")), ("a", mm3(xf_point(to_model, along(t.start, t.axis, t.reach)))), ("b", mm3(xf_point(to_model, along(t.end, t.axis, -t.reach)))), ("radius", n(t.reach))])),
    ];
    if let Some(feature) = &t.feature {
        row.push(("feature", s(feature)));
    }
    obj(row)
}

/// The tubes of the last list, for a picture, `fab.orient`'s words and `fab.strengthen`'s picks; what
/// plate they were read from.
pub struct TubesMemo {
    /// The plate it's of.
    pub ev: Arc<Evaluated>,
    pub tubes: Vec<Tube>,
    pub head: Head,
}

/// Align a limb: the turn about its joint that lays it flat under a way up.
#[derive(Clone, Debug)]
pub struct Alignment {
    pub called: String,
    pub instance: String,
    /// The solids that turn, the limb's own first, and which of them it carries.
    pub solids: Vec<String>,
    pub carried: Vec<String>,
    pub held_by: String,
    /// Model frame.
    pub pivot: [f64; 3],
    pub turn: [[f64; 3]; 3],
    /// The angle it stands at under that way up: what the turn takes out.
    pub degrees: f64,
}

/// Why a limb isn't offered a turn: `degrees` when it could turn but that's past the cap.
pub struct NoAlignment {
    pub degrees: Option<f64>,
    pub why: String,
}

impl NoAlignment {
    fn because(why: impl Into<String>) -> NoAlignment {
        NoAlignment { degrees: None, why: why.into() }
    }
}

/// The turn that lays `tube` flat under `up` (on the plate as it stood when measured), no more than
/// `cap` degrees. The smallest turn: about the line across the limb and the way up.
pub fn alignment(c: &Compiled, tube: &Tube, up: [f64; 3], cap: f64) -> Result<Alignment, NoAlignment> {
    let Some(feature) = &tube.feature else {
        return Err(NoAlignment::because("nothing in the program names it, so nothing there can turn it. Solid infill or thicken still can"));
    };
    let (instance, name) = split_id(&tube.object).ok_or_else(|| NoAlignment::because("it isn't a script's part"))?;
    let inst = c.instance(instance).ok_or_else(|| NoAlignment::because("its script is gone"))?;
    // a part's id names the solid it prints, which a texture on top may have renamed from its object's
    let solid = inst.script.program.solids.iter().position(|s| s.id == name);
    let object = solid.and_then(|i| inst.script.object_of(i)).ok_or_else(|| NoAlignment::because("its object is gone"))?;
    let (a, b) = (xf_point(&tube.to_model, tube.start), xf_point(&tube.to_model, tube.end));
    let limb = limb_of(&inst.script.program, object, feature, a, b).map_err(NoAlignment::because)?;
    let up_model = xf_dir(&tube.to_model, up);
    let size = dot(up_model, up_model).sqrt();
    if !(size > 0.0) {
        return Err(NoAlignment::because("that way up has no direction"));
    }
    let up_model = up_model.map(|x| x / size);
    let lifted = dot(limb.out, up_model);
    let degrees = lifted.abs().min(1.0).asin().to_degrees();
    if degrees > cap {
        return Err(NoAlignment { degrees: Some(degrees), why: format!("{degrees:.0} deg off flat, past the {cap:.0} deg cap: left alone") });
    }
    let flat = along(limb.out, up_model, -lifted);
    let axis = [limb.out[1] * flat[2] - limb.out[2] * flat[1], limb.out[2] * flat[0] - limb.out[0] * flat[2], limb.out[0] * flat[1] - limb.out[1] * flat[0]];
    let radians = degrees.to_radians();
    let basis = |d: [f64; 3]| rotate(d, axis, radians);
    let (x, y, z) = (basis([1.0, 0.0, 0.0]), basis([0.0, 1.0, 0.0]), basis([0.0, 0.0, 1.0]));
    let turn = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
    Ok(Alignment { called: tube.called.clone(), instance: instance.to_string(), solids: limb.solids, carried: limb.carried, held_by: limb.held_by, pivot: limb.pivot, turn, degrees })
}

impl Alignment {
    /// The program's edit: every solid that turns.
    pub fn changes(&self, c: &Compiled) -> Result<Vec<Change>, String> {
        let mut all = Vec::new();
        for id in &self.solids {
            all.extend(turned_about(c, &self.instance, id, self.pivot, &self.turn)?);
        }
        Ok(all)
    }

    /// Where a point on the limb goes (model frame).
    pub fn moved(&self, p: [f64; 3]) -> [f64; 3] {
        let d = sub(p, self.pivot);
        std::array::from_fn(|r| self.pivot[r] + dot(self.turn[r], d))
    }
}

/// A move `fab.orient` made, which the next `fab.tubes` measures: the tubes as they lay and the turn.
pub struct OrientWas {
    tubes: Vec<Tube>,
    turn: [[f64; 3]; 3],
}

/// A `fab.tubes` to answer off the model worker.
pub struct TubesJob {
    ev: Arc<Evaluated>,
    head: Head,
    scans: Vec<TubeScan>,
    frames: Vec<[[f64; 4]; 3]>,
    bead: Bead,
    from: &'static str,
    memo: Arc<Mutex<Option<Arc<TubesMemo>>>>,
    was: Option<OrientWas>,
}

impl TubesJob {
    pub fn answer(&self, stop: &(dyn Fn() -> bool + Sync)) -> Result<S, String> {
        let t = std::time::Instant::now();
        let mut all: Vec<Tube> = Vec::new();
        for scan in &self.scans {
            all.extend(tubes_of(scan, stop).ok_or("cancelled")?);
        }
        all.sort_by(|a, b| b.risk.total_cmp(&a.risk));
        for (i, tube) in all.iter_mut().enumerate() {
            tube.id = format!("tube{}", i + 1);
        }
        let row = |tube: &Tube| {
            let to_model = self.scans.iter().position(|scan| scan.part == tube.part).map(|i| self.frames[i]);
            let row = tube_row(tube, &self.bead, &to_model.map_or_else(|| fab_model::weave::invert(&IDENTITY), |f| fab_model::weave::invert(&f)));
            match self.was.as_ref().and_then(|was| was.angle_before(tube)) {
                Some(before) => with_field(row, "angle_before", n(before)),
                None => row,
            }
        };
        let rows: Vec<S> = all.iter().map(row).map(crate::out::line).collect();
        makepad_error_log::flight::mark_ab("fab.tubes", self.from, all.len() as u64, t.elapsed().as_millis() as u64);
        *self.memo.lock().unwrap() = Some(Arc::new(TubesMemo { ev: self.ev.clone(), tubes: all, head: self.head }));
        let mut fields = vec![
            ("from", s(self.from)),
            ("layers", s("flat on the bed: a tube's angle_to_bed is 0 lying along them, 90 standing across them")),
            ("measured_ms", int(t.elapsed().as_millis() as usize)),
            ("tubes", arr(rows)),
        ];
        if self.from != "slice" {
            fields.push(("note", s("no current slice: measured from sections of the model. fab.weak_spots waits for one")));
        }
        Ok(obj(fields))
    }
}

const IDENTITY: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];

pub(crate) fn with_field(row: S, key: &'static str, value: S) -> S {
    match row {
        S::Object(name, mut fields) => {
            fields.push((key.to_string(), value));
            S::Object(name, fields)
        }
        other => other,
    }
}

impl OrientWas {
    /// The angle a tube lay at before the move: the one by the same feature, or whose axis the
    /// turn brings closest to it.
    fn angle_before(&self, now: &Tube) -> Option<f64> {
        let same = self.tubes.iter().find(|t| t.part == now.part && t.feature.is_some() && t.feature == now.feature);
        let turned = |t: &&Tube| dot(apply(&self.turn, t.axis), now.axis).abs();
        same.or_else(|| self.tubes.iter().filter(|t| t.part == now.part).max_by(|a, b| turned(a).total_cmp(&turned(b))).filter(|t| turned(t) >= SAME_AXIS)).map(Tube::angle)
    }
}

/// Runs a search `cancelled` can stop on a thread of its own, polling `cancelled` here: a search's
/// stop is shared across its workers, and the model worker's isn't made to be.
pub(crate) fn with_stop<T: Send>(cancelled: Option<&dyn Fn() -> bool>, work: impl FnOnce(&(dyn Fn() -> bool + Sync)) -> T + Send) -> T {
    let Some(cancelled) = cancelled else { return work(&|| false) };
    let flag = AtomicBool::new(false);
    let waiting = std::thread::current();
    std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            let out = work(&|| flag.load(Ordering::Relaxed));
            waiting.unpark();
            out
        });
        while !running.is_finished() {
            if cancelled() {
                flag.store(true, Ordering::Relaxed);
            }
            std::thread::park_timeout(std::time::Duration::from_millis(20));
        }
        running.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// A tube's colour in a picture: green lying flat to red standing.
fn angle_colour(angle: f64) -> [f32; 3] {
    let t = (angle / 90.0).clamp(0.0, 1.0) as f32;
    [0.15 + 0.8 * t, 0.75 - 0.6 * t, 0.25 - 0.1 * t]
}

const PART_GREY: [f32; 3] = [0.62, 0.62, 0.65];
const ROD_SIDES: usize = 8;
const ROD_RADIUS: f64 = 0.5;
const RING_SIDES: usize = 32;
/// How far past a tube's ends its axis shows, of its length.
const AXIS_PAST: f64 = 0.2;

/// Two directions square to unit `d` and to each other.
fn square_to(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let seed = if d[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let x = cross(d, seed);
    let l = dot(x, x).sqrt();
    let x = x.map(|v| v / l);
    (x, cross(d, x))
}

/// A thin bar from `a` to `b`.
fn rod(mesh: &mut ColoredMesh, a: [f64; 3], b: [f64; 3], color: [f32; 3]) {
    let d = sub(b, a);
    let l = dot(d, d).sqrt().max(1e-9);
    let (x, y) = square_to(d.map(|v| v / l));
    let round = |p: [f64; 3], i: usize| {
        let (sin, cos) = (i as f64 * std::f64::consts::TAU / ROD_SIDES as f64).sin_cos();
        along(along(p, x, cos * ROD_RADIUS), y, sin * ROD_RADIUS)
    };
    for i in 0..ROD_SIDES {
        let (p, q, r, t) = (round(a, i), round(a, i + 1), round(b, i), round(b, i + 1));
        mesh.triangle([p, q, t], color);
        mesh.triangle([p, t, r], color);
    }
}

/// A flat ring round `at` in the plane of the plate: the layers' direction there.
fn ring(mesh: &mut ColoredMesh, at: [f64; 3], radius: f64, color: [f32; 3]) {
    let round = |r: f64, i: usize| {
        let (sin, cos) = (i as f64 * std::f64::consts::TAU / RING_SIDES as f64).sin_cos();
        [at[0] + r * cos, at[1] + r * sin, at[2]]
    };
    let inner = (radius - 2.0 * ROD_RADIUS).max(0.1);
    for i in 0..RING_SIDES {
        let (p, q, r, t) = (round(inner, i), round(inner, i + 1), round(radius, i), round(radius, i + 1));
        // both sides show
        for tri in [[p, q, t], [p, t, r], [t, q, p], [r, t, p]] {
            mesh.triangle(tri, color);
        }
    }
}

/// What `fab.orient` answers: a move it made, or the table, whose fields are its reply.
pub(crate) enum Oriented {
    Moved(Reply),
    Table(OrientTable),
}

pub(crate) struct OrientTable {
    pub fields: Vec<(String, S)>,
    pub tubes: Vec<Tube>,
    /// The first choice's turn, none with nothing to choose.
    pub best: Option<[[f64; 3]; 3]>,
    pub says: Option<String>,
}

/// A tube's angle to the bed after a turn of the part.
pub(crate) fn angle_after(tube: &Tube, turn: &[[f64; 3]; 3]) -> f64 {
    angle_to_bed(apply(turn, tube.axis), [0.0, 0.0, 1.0])
}

/// One row of `fab.orient`'s table: a way up and what it does.
struct OrientRow {
    /// The way up on the plate as it stands, and the turn about it that fits the bed.
    up: [f64; 3],
    spin: f64,
    turn: [[f64; 3]; 3],
    score: UpScore,
    fits: bool,
    size: [f64; 3],
    /// What the real support planner says for the part, when it was asked: mm^3 and seconds.
    real: Option<(f64, f64)>,
    said: &'static str,
}

fn size_after(verts: &[[f64; 3]], turn: &[[f64; 3]; 3]) -> [f64; 3] {
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for v in verts {
        let p = apply(turn, *v);
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    std::array::from_fn(|k| hi[k] - lo[k])
}

/// The turn taking `up` to the plate's up, about it the first that fits the bed.
fn fit_turn(verts: &[[f64; 3]], up: [f64; 3], bed: [f64; 2], height: f64) -> (f64, [[f64; 3]; 3], [f64; 3], bool) {
    let base = turn_to_up(up);
    let mut first = None;
    for i in 0..SPINS {
        let spin = i as f64 * 180.0 / SPINS as f64;
        let turn = mat_mul(&spin_z(spin), &base);
        let size = size_after(verts, &turn);
        let fits = size[0] <= bed[0] && size[1] <= bed[1] && size[2] <= height;
        if fits {
            return (spin, turn, size, true);
        }
        first.get_or_insert((spin, turn, size, false));
    }
    first.unwrap_or((0.0, base, [0.0; 3], false))
}

impl Session {
    /// The filament and bead the plate prints with: what `fab.weak_spots` and `fab.orient` weigh by.
    pub(crate) fn bead(&self, ev: &Evaluated) -> Bead {
        let setup = self.slices.as_ref().and_then(|src| src.input(ev).ok()).and_then(|(input, _)| (input.setup)(None).ok());
        let kind = setup.as_ref().and_then(|setup| setup.filaments.first().map(|f| f.values.raw("filament_type", 0).to_string())).unwrap_or_default();
        let filament = fab_force::material::filament_of_type(&kind).unwrap_or(&fab_force::material::PLA);
        let (height, width) = setup.as_ref().map_or((0.2, 0.45), |setup| (setup.process.values.f64("layer_height").max(0.05), setup.process.values.f64("inner_wall_line_width").max(0.1)));
        Bead { filament, height, width }
    }

    /// The steepest a face can overhang unsupported on this plate, from vertical (degrees).
    fn overhang_limit(&self, ev: &Evaluated) -> f64 {
        let setup = self.slices.as_ref().and_then(|src| src.input(ev).ok()).and_then(|(input, _)| (input.setup)(None).ok());
        let threshold = setup.map_or(0.0, |setup| setup.process.values.f64("support_threshold_angle"));
        90.0 - (if threshold > 0.0 { threshold } else { 45.0 }).clamp(1.0, 89.0)
    }

    /// What a find of plate part `k` reads.
    fn tube_scan(&mut self, ev: &Evaluated, k: usize, bead: &Bead, max_thickness: Option<f64>) -> Result<TubeScan, String> {
        let read = self.part_layers(ev, k)?;
        let object = ev.plate.parts[k].object.clone();
        let frame = pick_frame(ev, k);
        let features = ev.features.get(k).cloned().unwrap_or_default();
        let instance = split_id(&object).map(|(i, _)| i.to_string()).unwrap_or_default();
        let boxes = features.iter().map(|f| solid_box(ev, &part_id(&instance, &f.name)).map(|b| on_layers(&frame, &read, b))).collect();
        Ok(TubeScan { part: k, object, read, to_model: fab_model::weave::invert(&frame), features, boxes, max_thickness, layer_factor: bead.layer_factor() })
    }

    /// `fab.render {tubes: true}`: the last list on the part, its names and angles written on it.
    pub(crate) fn tubes_look(&mut self, ev: &Arc<Evaluated>, objects: &[String], views: Vec<String>) -> Result<LookRequest, String> {
        let memo = self.tubes_memo.lock().unwrap().clone().ok_or("no tubes yet: fab.tubes first")?;
        if !Arc::ptr_eq(&memo.ev, ev) {
            return Err("that list is of an older plate: fab.tubes again".into());
        }
        let mut mesh = ColoredMesh::default();
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for (k, part) in ev.plate.parts.iter().enumerate() {
            if !objects.is_empty() && !objects.contains(&part.object) {
                continue;
            }
            let frame = pick_frame(ev, k);
            let surface = self.eval.object_mesh(&part.object)?;
            for t in &surface.triangles {
                let corners = t.map(|i| {
                    let v = &surface.vertices[i as usize];
                    xf_point(&frame, [v.x, v.y, v.z])
                });
                for c in corners {
                    for i in 0..3 {
                        lo[i] = lo[i].min(c[i]);
                        hi[i] = hi[i].max(c[i]);
                    }
                }
                mesh.triangle(corners, PART_GREY);
            }
        }
        let mut marks = Vec::new();
        let mut said = Vec::new();
        for t in memo.tubes.iter().filter(|t| objects.is_empty() || objects.contains(&t.object)) {
            let (angle, color) = (t.angle(), angle_colour(t.angle()));
            let to_model = fab_model::weave::invert(&pick_frame(ev, t.part));
            let (a, b) = (xf_point(&to_model, t.start), xf_point(&to_model, t.end));
            let frame = pick_frame(ev, t.part);
            let scale = (frame[0][0].powi(2) + frame[1][0].powi(2) + frame[2][0].powi(2)).sqrt().max(1e-9);
            let reach = t.reach / scale;
            self.tint_surface(ev, t.part, &|q| off_segment(q, a, b) - reach, color, &mut mesh)?;
            let past = AXIS_PAST * t.length;
            let (from, to) = (along(t.start, t.axis, -past), along(t.end, t.axis, past));
            rod(&mut mesh, from, to, color);
            ring(&mut mesh, along(t.start, t.axis, 0.5 * t.length), t.reach + 3.0, color);
            for i in 0..3 {
                for p in [from, to] {
                    lo[i] = lo[i].min(p[i]);
                    hi[i] = hi[i].max(p[i]);
                }
            }
            marks.push((to, format!("{} {angle:.0} deg", t.called)));
            said.push(obj(vec![("tube", s(&t.id)), ("name", s(&t.called)), ("angle_to_bed", n(angle))]));
        }
        let views = if views.is_empty() { ["front", "right", "back", "left"].map(str::to_string).to_vec() } else { views };
        let answer = crate::out::text(&obj(vec![
            ("tubes", arr(said)),
            ("views", arr(views.iter().map(s))),
            ("reading", s("Each tube's surface and axis green lying flat to red standing, a ring across it in the layers' plane, its name and angle to the bed written beside it.")),
        ]));
        Ok(LookRequest { mesh, frame: (lo, hi), views, answer, marks })
    }

    /// `fab.tubes`: asks for the find off the model worker.
    pub(crate) fn tubes_tool(&mut self, args: &Value) -> Reply {
        let Some(ev) = self.last.clone().filter(|ev| !ev.plate.parts.is_empty()) else { return Reply::err("nothing on the plate") };
        let max_thickness = match args.get("max_thickness") {
            None | Some(Value::Null) => None,
            Some(v) => match crate::number(v) {
                Some(t) if t > 0.0 => Some(t),
                _ => return Reply::err("'max_thickness' is a number in mm, more than 0"),
            },
        };
        let parts: Vec<usize> = match str_arg(args, "object") {
            Some(object) => match ev.plate.parts.iter().position(|p| p.object == object) {
                Some(k) => vec![k],
                None => return Reply::err(format!("no part '{object}' on the plate")),
            },
            None => (0..ev.plate.parts.len()).collect(),
        };
        let bead = self.bead(&ev);
        let mut scans = Vec::with_capacity(parts.len());
        let mut frames = Vec::with_capacity(parts.len());
        let mut from = "slice";
        for k in parts {
            match self.tube_scan(&ev, k, &bead, max_thickness) {
                Ok(scan) => {
                    if scan.read.from != "slice" {
                        from = scan.read.from;
                    }
                    frames.push(pick_frame(&ev, k));
                    scans.push(scan);
                }
                Err(why) => return Reply::err(why),
            }
        }
        // what a slice of the move measures, and only that slice
        let was = if from == "slice" { self.orient_was.take() } else { None };
        let job = TubesJob { ev, head: self.model.head(), scans, frames, bead, from, memo: self.tubes_memo.clone(), was };
        Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::Tubes(Arc::new(job))) }
    }

    /// `fab.orient`: which way up serves the tubes or least support, as a table; with `apply`, a
    /// row's way up as the part's move, one undo step.
    pub(crate) fn orient_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        match self.orient_answer(args, cancelled) {
            Ok(Oriented::Moved(reply)) => reply,
            Ok(Oriented::Table(table)) => Reply::ok(S::Object(None, table.fields)),
            Err(why) => Reply::err(why),
        }
    }

    pub(crate) fn orient_answer(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Result<Oriented, String> {
        let ev = self.last.clone().filter(|ev| !ev.plate.parts.is_empty()).ok_or("nothing on the plate")?;
        let k = match str_arg(args, "object") {
            Some(object) => ev.plate.parts.iter().position(|p| p.object == object).ok_or_else(|| format!("no part '{object}' on the plate"))?,
            None if ev.plate.parts.len() == 1 => 0,
            None => return Err("the plate has several parts: say which with 'object'".into()),
        };
        let bead = self.bead(&ev);
        let scan = self.tube_scan(&ev, k, &bead, None)?;
        let frame = pick_frame(&ev, k);
        let found = with_stop(cancelled, |stop| tubes_of(&scan, stop)).ok_or("cancelled")?;
        let mut tubes = found;
        for (i, tube) in tubes.iter_mut().enumerate() {
            tube.id = format!("tube{}", i + 1);
        }
        *self.tubes_memo.lock().unwrap() = Some(Arc::new(TubesMemo { ev: ev.clone(), tubes: tubes.clone(), head: self.model.head() }));
        let mesh = self.eval.object_mesh(&ev.plate.parts[k].object)?;
        let verts: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| xf_point(&frame, [v.x, v.y, v.z])).collect();
        let tris: Vec<[u32; 3]> = mesh.triangles.clone();
        let surface = OrientMesh { verts: &verts, tris: &tris };
        let bed = ev.machine.bed;
        let bed_size = [bed.max[0] - bed.min[0], bed.max[1] - bed.min[1]];
        let height = ev.machine.max_height;
        let limit = self.overhang_limit(&ev);
        // which tubes, by name, id or feature, and how they weigh
        let names = |key: &str| -> Result<Vec<&Tube>, String> {
            let Some(list) = args.get(key).and_then(Value::as_arr) else { return Ok(Vec::new()) };
            list.iter().map(|item| item.as_str().or_else(|| str_arg(item, "tube")).ok_or_else(|| format!("'{key}' lists tubes by name")).and_then(|word| {
                tubes.iter().find(|t| t.id == word || t.called == word || t.feature.as_deref() == Some(word)).ok_or_else(|| {
                    let all: Vec<&str> = tubes.iter().map(|t| t.called.as_str()).collect();
                    format!("no tube '{word}' on this part: it has [{}]", all.join(", "))
                })
            })).collect()
        };
        let picked = names("tubes")?;
        let mut held = Vec::new();
        if let Some(list) = args.get("hold").and_then(Value::as_arr) {
            let tube_of = names("hold")?;
            for (item, tube) in list.iter().zip(tube_of) {
                held.push((tube, item.get("within").and_then(crate::number).unwrap_or(STAYS_DEG)));
            }
        }
        let weight_of = |t: &Tube| args.get("weights").and_then(|w| [t.id.as_str(), t.called.as_str()].into_iter().find_map(|key| w.get(key))).and_then(crate::number).unwrap_or(t.risk);
        let chosen: Vec<&Tube> = if picked.is_empty() { tubes.iter().collect() } else { picked };
        let goal = str_arg(args, "goal").unwrap_or("strength");
        let row_of = |up: [f64; 3], said: &'static str, stop: &(dyn Fn() -> bool + Sync)| -> Option<OrientRow> {
            let score = score_up(&surface, up, limit, stop)?;
            let (spin, turn, size, fits) = fit_turn(&verts, up, bed_size, height);
            Some(OrientRow { up, spin, turn, score, fits, size, real: None, said })
        };
        // a way up asked for by number: this is what a row applies
        if args.get("apply").and_then(Value::as_bool) == Some(true) {
            let up = args.get("up").and_then(Value::as_arr).filter(|a| a.len() == 3).and_then(|a| {
                let v: Vec<f64> = a.iter().filter_map(crate::number).collect();
                (v.len() == 3).then(|| [v[0], v[1], v[2]])
            }).ok_or("'apply' takes the 'up' of a row, and its 'spin'")?;
            let length = dot(up, up).sqrt();
            if !(length > 0.0) {
                return Err("'up' has no direction".into());
            }
            let up = up.map(|x| x / length);
            let spin = args.get("spin").and_then(crate::number).unwrap_or(0.0);
            let expected = self.head_arg(args).map_err(|reply| reply.text)?;
            let turn = mat_mul(&spin_z(spin), &turn_to_up(up));
            let object = ev.plate.parts[k].object.clone();
            let changes = self.place_changes(&[orient_move(&ev.plate.parts[k], &turn)], false).map_err(|reply| reply.text)?;
            let request_id = str_arg(args, "request_id").map_or_else(|| self.next_request_id(ORIENT_TOOL), str::to_string);
            return match self.model.apply(&mut self.eval, &request_id, expected, &changes, cancelled) {
                Ok((applied, built)) => {
                    makepad_error_log::flight::mark_ab("fab.orient.apply", &object, tubes.len() as u64, 0);
                    self.orient_was = Some(OrientWas { tubes: tubes.clone(), turn });
                    let words: Vec<String> = chosen.iter().map(|t| format!("{} {:.0} -> {:.0} deg", t.called, t.angle(), angle_to_bed(apply(&turn, t.axis), [0.0, 0.0, 1.0]))).collect();
                    Ok(Oriented::Moved(self.commit_reply(vec![("head", s(applied.head.text())), ("done", s(words.join(", "))), ("next", s("fab.weak_spots waits for the new slice, then fab.tubes measures the angles from its layers"))], built)))
                }
                Err(e) => Ok(Oriented::Moved(self.apply_error(e))),
            };
        }
        // the plate as it stands is always the table's first row
        let mut rows: Vec<OrientRow> = Vec::new();
        let searched = with_stop(cancelled, |stop| -> Option<Vec<OrientRow>> {
            let mut rows = vec![row_of([0.0, 0.0, 1.0], "as it stands", stop)?];
            rows[0].spin = 0.0;
            rows[0].turn = spin_z(0.0);
            match goal {
                "supports" => {
                    let holds: Vec<HeldTube> = held.iter().map(|(tube, within)| HeldTube { axis: tube.axis, within_deg: *within }).collect();
                    for score in least_support_ups(&surface, limit, &holds, TOP_UPS, stop)? {
                        rows.push(row_of(score.up, "least support", stop)?);
                    }
                }
                _ => {
                    let axes: Vec<fab_printability::Tube> = chosen.iter().map(|t| fab_printability::Tube { axis: t.axis, weight: weight_of(t) }).collect();
                    let up = flattest_up(&axes);
                    // the same angles either way up: the one that needs less support comes first
                    let mut sides = [row_of(up, "tubes flattest", stop)?, row_of(up.map(|x| -x), "tubes flattest", stop)?];
                    sides.sort_by(|a, b| a.score.support_mm3.total_cmp(&b.score.support_mm3));
                    sides[1].said = "tubes flattest, other side up";
                    rows.extend(sides);
                }
            }
            Some(rows)
        }).ok_or("cancelled")?;
        rows.extend(searched);
        let mut notes: Vec<String> = Vec::new();
        if goal == "supports" {
            if rows.len() == 1 {
                let words: Vec<String> = held.iter().map(|(tube, within)| format!("{} within {within:.0} deg", tube.called)).collect();
                return Err(format!("no way up holds {}: ask for less", words.join(" and ")));
            }
            // the top few through the real support planner, its paths only: what they extrude and
            // take ranks them, planned at once; what they tell is the applied plate's slice's
            let mut tried = Vec::with_capacity(rows.len());
            for row in &rows {
                let changes = self.place_changes(&[orient_move(&ev.plate.parts[k], &row.turn)], false).map_err(|reply| reply.text)?;
                tried.push(self.tried_plate(&changes, cancelled)?);
            }
            for (row, figures) in rows.iter_mut().zip(self.tried_figures(&tried)?) {
                row.real = figures.get(k).copied();
            }
            let (first, rest) = rows.split_at_mut(1);
            rest.sort_by(|a, b| a.real.map_or(f64::MAX, |r| r.0).total_cmp(&b.real.map_or(f64::MAX, |r| r.0)));
            if first[0].real.is_some_and(|r| r.0 == 0.0) && rest.iter().all(|r| r.real.is_some_and(|r| r.0 == 0.0)) {
                notes.push("the plan says no supports anywhere: the plate's supports may be off, or it needs none. The estimates still rank them".to_string());
            }
        }
        let density = self.slices.as_ref().and_then(|src| src.input(&ev).ok()).and_then(|(input, _)| (input.setup)(None).ok()).and_then(|setup| setup.filaments.first().map(|f| f.values.f64_at("filament_density", 0))).filter(|d| *d > 0.0);
        // the table
        let table = rows.iter().map(|row| {
            let tubes_after = tubes.iter().map(|t| {
                let turned = angle_to_bed(apply(&row.turn, t.axis), [0.0, 0.0, 1.0]);
                crate::out::line(obj(vec![("tube", s(&t.id)), ("name", s(&t.called)), ("before", n(t.angle())), ("after", n(turned)), ("gain_estimate", n(bead.gain(t.angle(), turned)))]))
            });
            let mut fields = vec![
                ("is", s(row.said)),
                ("up", number_list(row.up)),
                ("spin", n(row.spin)),
                ("height", n(row.size[2])),
                ("size", number_list(row.size)),
                ("fits_bed", flag(row.fits)),
                ("overhang_mm2", n(row.score.overhang_mm2)),
                ("support_estimate_mm3", n(row.score.support_mm3)),
                ("on_bed_mm2", n(row.score.bed_mm2)),
                ("tubes", arr(tubes_after)),
            ];
            if let Some((mm3_real, seconds)) = row.real {
                fields.push(("support_planned_mm3", n(mm3_real)));
                fields.push(("support_planned_s", n(seconds)));
                if let Some(density) = density {
                    fields.push(("support_planned_grams", n(mm3_real * density / 1000.0)));
                }
            }
            crate::out::line(obj(fields))
        });
        let table: Vec<S> = table.collect();
        // the plain words: what lies flat in the first choice, what stays
        if let Some(best) = rows.get(1) {
            let (flat, stays): (Vec<&&Tube>, Vec<&&Tube>) = chosen.iter().partition(|t| angle_to_bed(apply(&best.turn, t.axis), [0.0, 0.0, 1.0]) <= STAYS_DEG);
            let say = |list: &[&&Tube]| list.iter().map(|t| format!("{} {:.0} deg", t.called, angle_to_bed(apply(&best.turn, t.axis), [0.0, 0.0, 1.0]))).collect::<Vec<_>>().join(", ");
            let mut words = say(&flat);
            if !stays.is_empty() {
                if !words.is_empty() {
                    words.push_str("; ");
                }
                words.push_str(&format!("{} stays", say(&stays)));
            }
            notes.push(words);
        }
        makepad_error_log::flight::mark_ab("fab.orient", goal, rows.len() as u64, tubes.len() as u64);
        // what a small turn of a limb would add to the first choice, or to the plate as it stands
        let cap = args.get("max_angle").and_then(crate::number).unwrap_or(MAX_ALIGN_DEG);
        let aligned = rows.get(1).map_or([0.0, 0.0, 1.0], |row| row.up);
        let (align, align_here) = self.compiled.clone().map_or_else(Default::default, |c| (self.align_rows(&c, &chosen, aligned, cap, &bead), self.align_rows(&c, &chosen, [0.0, 0.0, 1.0], cap, &bead)));
        let mut fields = vec![
            ("dry_run", flag(true)),
            ("committed", flag(false)),
            ("goal", s(goal)),
            ("object", s(&ev.plate.parts[k].object)),
            ("from", s(scan.read.from)),
            ("estimate", s("gain is pulled along a tube, from the filament's model; overhang and support_estimate from the mesh. Estimates: the new slice measures them after a move")),
            ("rows", arr(table)),
            ("align", arr(align)),
            ("align_as_it_stands", arr(align_here)),
            ("apply", s("fab.orient {apply: true, up: <a row's up>, spin: <its spin>, expected: <head>}")),
            ("head", s(self.model.head().text())),
        ];
        let says = (!notes.is_empty()).then(|| notes.join(". "));
        if let Some(says) = &says {
            fields.push(("says", s(says)));
        }
        let best = rows.get(1).map(|row| row.turn);
        let fields = fields.into_iter().map(|(key, value)| (key.to_string(), value)).collect();
        Ok(Oriented::Table(OrientTable { fields, tubes, best, says }))
    }
}

/// The picks of `fab.strengthen` that turn something: what they change and what they said.
pub(crate) struct ShapePicks {
    pub aligns: Vec<Alignment>,
    pub edits: Vec<Change>,
    pub moves: Vec<PartMove>,
    pub was: Option<OrientWas>,
    pub said: Vec<String>,
}

impl ShapePicks {
    /// Where a point of `spot` goes with the limb that turns.
    pub fn moved(&self, spot: &crate::strengthen::Spot, p: [f64; 3]) -> [f64; 3] {
        let turning = self.aligns.iter().find(|align| spot.feature.as_ref().is_some_and(|feature| align.solids.contains(feature)));
        turning.map_or(p, |align| align.moved(p))
    }

    pub fn line_of(&self, spot: &crate::strengthen::Spot) -> Option<([f64; 3], [f64; 3])> {
        spot.line.map(|(a, b)| (self.moved(spot, a), self.moved(spot, b)))
    }
}

fn up_arg(value: &Value) -> Result<Option<[f64; 3]>, String> {
    let Some(list) = value.get("up") else { return Ok(None) };
    let numbers: Vec<f64> = list.as_arr().map_or_else(Vec::new, |a| a.iter().filter_map(crate::number).collect());
    let [x, y, z] = numbers[..] else { return Err("'up' is three numbers".to_string()) };
    let length = (x * x + y * y + z * z).sqrt();
    if !(length > 0.0) {
        return Err("'up' has no direction".to_string());
    }
    Ok(Some([x / length, y / length, z / length]))
}

/// A part's move to a way up: the part's normal move, about its middle.
fn orient_move(part: &fab_plate::Part, turn: &[[f64; 3]; 3]) -> PartMove {
    let c = part.bounds().center();
    let row = |i: usize| [turn[i][0], turn[i][1], turn[i][2], c[i] - dot(turn[i], c)];
    PartMove { object: part.object.clone(), delta: [row(0), row(1), row(2)] }
}

/// What's written for one limb under a way up: the turn to pick, or why there's none.
impl Session {
    /// `fab.strengthen`'s turning picks. A limb aligned with a way up picked alongside lies flat
    /// under that one.
    pub(crate) fn shape_picks(&mut self, c: &Compiled, ev: &Evaluated, picks: &[Value]) -> Result<ShapePicks, String> {
        let mut out = ShapePicks { aligns: Vec::new(), edits: Vec::new(), moves: Vec::new(), was: None, said: Vec::new() };
        let is = |pick: &&Value, fix: &str| str_arg(pick, "fix") == Some(fix);
        if !picks.iter().any(|pick| is(&pick, crate::strengthen::ALIGN) || is(&pick, crate::strengthen::ORIENT)) {
            return Ok(out);
        }
        let memo = self.tubes_memo.lock().unwrap().clone();
        let memo = memo.filter(|memo| memo.head == self.model.head());
        let mut turned: Option<(usize, [[f64; 3]; 3], [f64; 3])> = None;
        for pick in picks.iter().filter(|pick| is(pick, crate::strengthen::ORIENT)) {
            let k = match str_arg(pick, "object") {
                Some(object) => ev.plate.parts.iter().position(|p| p.object == object).ok_or_else(|| format!("no part '{object}' on the plate"))?,
                None if ev.plate.parts.len() == 1 => 0,
                None => return Err("the plate has several parts: an orient pick says which with 'object'".to_string()),
            };
            let up = up_arg(pick)?.ok_or("an orient pick takes the 'up' of a fab.orient row, and its 'spin'")?;
            let turn = mat_mul(&spin_z(pick.get("spin").and_then(crate::number).unwrap_or(0.0)), &turn_to_up(up));
            out.moves.push(orient_move(&ev.plate.parts[k], &turn));
            turned = Some((k, turn, up));
            let flat = memo.as_ref().map_or_else(Vec::new, |memo| memo.tubes.iter().filter(|t| t.part == k).map(|t| format!("{} {:.0} -> {:.0} deg", t.called, t.angle(), angle_to_bed(apply(&turn, t.axis), [0.0, 0.0, 1.0]))).collect());
            out.said.push(if flat.is_empty() { "the part turned".to_string() } else { format!("the part turned: {}", flat.join(", ")) });
        }
        let mut aligned = 0;
        for pick in picks.iter().filter(|pick| is(pick, crate::strengthen::ALIGN)) {
            let Some(word) = str_arg(pick, "tube") else { return Err("an align pick names its 'tube'".to_string()) };
            let Some(memo) = &memo else { return Err("the tubes aren't of this plate: fab.tubes or fab.orient lists them, and pick again".to_string()) };
            let Some(tube) = memo.tubes.iter().find(|t| t.id == word || t.called == word || t.feature.as_deref() == Some(word)) else {
                let names: Vec<&str> = memo.tubes.iter().map(|t| t.called.as_str()).collect();
                return Err(format!("no tube '{word}': the list has [{}]", names.join(", ")));
            };
            let up = match (up_arg(pick)?, &turned) {
                (Some(up), _) => up,
                (None, Some((k, _, up))) if *k == tube.part => *up,
                _ => [0.0, 0.0, 1.0],
            };
            let cap = pick.get("max_angle").and_then(crate::number).unwrap_or(MAX_ALIGN_DEG);
            let align = alignment(c, tube, up, cap).map_err(|no| format!("{}: {}", tube.called, no.why))?;
            if align.degrees < ALIGN_MIN_DEG {
                return Err(format!("{}: it lies flat under that way up already", tube.called));
            }
            out.edits.extend(align.changes(c)?);
            out.said.push(format!("{} turned {:.1} deg at its joint with {}", tube.called, align.degrees, align.held_by));
            out.aligns.push(align);
            aligned += 1;
        }
        // the next list of a slice says what the angles were
        if let Some(memo) = &memo {
            out.was = Some(OrientWas { tubes: memo.tubes.clone(), turn: turned.map_or(spin_z(0.0), |(_, turn, _)| turn) });
        }
        makepad_error_log::flight::mark_ab("fab.strength.turn", "orient + align", out.moves.len() as u64, aligned);
        Ok(out)
    }

    pub(crate) fn align_rows(&self, c: &Compiled, tubes: &[&Tube], up: [f64; 3], cap: f64, bead: &Bead) -> Vec<S> {
        let mut rows = Vec::new();
        for tube in tubes {
            let mut row = vec![("tube", s(&tube.id)), ("name", s(&tube.called))];
            match alignment(c, tube, up, cap) {
                Ok(align) if align.degrees < ALIGN_MIN_DEG => continue,
                Ok(align) => {
                    row.push(("can", flag(true)));
                    row.push(("degrees", n(align.degrees)));
                    row.push(("gain_estimate", n(bead.gain(align.degrees, 0.0))));
                    row.push(("pivot", mm3(align.pivot)));
                    row.push(("joint_with", s(&align.held_by)));
                    row.push(("turns", arr(align.solids.iter().map(s))));
                    // what follows the limb's solids in the program turns too, and that's said
                    if let Some(also) = align.changes(c).ok().and_then(|changes| self.also_moves(c, &changes, &align)).filter(|also| !also.is_empty()) {
                        row.push(("also_turns", arr(also.iter().map(s))));
                    }
                }
                Err(no) => {
                    row.push(("can", flag(false)));
                    if let Some(degrees) = no.degrees {
                        row.push(("degrees", n(degrees)));
                    }
                    row.push(("why", s(&no.why)));
                }
            }
            rows.push(crate::out::line(obj(row)));
        }
        rows
    }

    /// The solids an alignment's edit changes beyond what it means to: a mirror of the limb, say.
    fn also_moves(&self, c: &Compiled, changes: &[Change], align: &Alignment) -> Option<Vec<String>> {
        let (_, after) = self.model.edited(changes).ok()?;
        let (was, now) = (&c.instance(&align.instance)?.script.program, &after.instance(&align.instance)?.script.program);
        Some(now.solids.iter().filter(|solid| solid.children.is_empty() && !align.solids.contains(&solid.id)).filter(|solid| {
            was.solids.iter().find(|old| old.id == solid.id).is_some_and(|old| was.graph.node(old.node).key != now.graph.node(solid.node).key)
        }).map(|solid| solid.id.clone()).collect())
    }
}

impl std::fmt::Debug for TubesJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TubesJob")
    }
}

impl PartialEq for TubesJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

#[cfg(test)]
#[path = "tubes_tests.rs"]
mod tests;
