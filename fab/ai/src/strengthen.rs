//! `fab.weak_spots` and `fab.strengthen`: what would break first on this, read from the current
//! slice, and the fixes for it as one undo step. The Slicer tab's Strength section calls the same
//! two tools, so a person and the chat get the same list and write the same program.
//!
//! The find runs off the model worker ([`WeakSpotJob`]) and never re-traces: it reads the layers the
//! slice made. A spot's fixes are real ones only: solid infill is offered where there's sparse infill
//! to fill, thicken on a part whose shape is a field ([`fab_model::thicken`]). Numbers that come from
//! the slice's own paths are measures; a fix's gain and cost before it's applied are estimates, and
//! say so.

use crate::out::{arr, flag, int, mm3, n, obj, s, S};
use crate::volumes::{plan_regions, slice_frame};
use crate::{str_arg, RenderRequest, Reply, Session};
use fab_model::eval::Evaluated;
use fab_model::params::xf_point;
use fab_model::program::{part_id, settings_solids, split_id, Compiled};
use fab_model::thicken::{thicken_change, Thicken};
use fab_model::{Change, Edit};
use fab_printability::{fit_limb, printed_inside, weak_spots, Printed, SpotKind, WeakSpot};
use makepad_fab_slice::live::PlatePaths;
use makepad_fab_slicer::geom::ExPolygon;
use makepad_model::json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub const FIND_TOOL: &str = "weak_spots";
pub const TOOL: &str = "strengthen";

/// How thin counts as thin when nobody says: 6% of the part's longest side, what makes a figure's
/// limbs thin and a block not, within what a hand could snap.
pub fn default_min_thickness(longest: f64) -> f64 {
    (0.06 * longest).clamp(3.0, 15.0)
}

/// A fix that's offered, as a tool's `fix` says it.
const SOLID: &str = "solid_infill";
const THICKEN: &str = "thicken";
/// Under this share of what prints at a spot, sparse infill is nothing to fill.
const SPARSE_SHARE: f64 = 0.02;
/// How far a test print's box reaches past the place it's round, mm: enough of what it joins to
/// print and to break the way the part would.
const TEST_PRINT_MARGIN: f64 = 6.0;
/// The fixes that turn a limb or the part, picked by tube and by way up.
pub const ALIGN: &str = "align";
pub const ORIENT: &str = "orient";
/// With walls this much of a section, solid infill adds little: thickening is the default.
const WALLS_FILL: f64 = 0.75;

/// One weak spot of the list.
#[derive(Clone, Debug)]
pub struct Spot {
    pub id: String,
    pub part: usize,
    pub object: String,
    /// The named feature it's in, if the part has one there.
    pub feature: Option<String>,
    /// What the list calls it.
    pub called: String,
    /// Plate mm.
    pub found: WeakSpot,
    /// Where it is on the model, model mm: what a turn of the part leaves.
    pub model_at: [f64; 3],
    /// The line down its middle, model mm, and its radius in model mm over plate mm.
    pub line: Option<([f64; 3], [f64; 3])>,
    pub scale: f64,
    /// What the panel draws round it: its line and radius, plate mm.
    pub band: ([f64; 3], [f64; 3], f64),
    /// What prints in its box now.
    pub printed: Printed,
    /// The settings that hold at its place: walls, infill density as written, pattern.
    pub walls: String,
    pub infill: String,
    pub pattern: String,
    /// Its shape's a field, so it can be thickened.
    pub field: bool,
}

/// The last find: of which slice, under which minimum.
pub struct WeakMemo {
    pub paths: Arc<PlatePaths>,
    pub min_thickness: f64,
    pub spots: Vec<Spot>,
    pub plate: Printed,
}

/// A find to run off the model worker; [`WeakSpotJob::answer`] is the tool's answer.
pub struct WeakSpotJob {
    pub ev: Arc<Evaluated>,
    c: Arc<Compiled>,
    paths: Arc<PlatePaths>,
    min_thickness: Option<f64>,
    layer_factor: f64,
    density: Option<f64>,
    /// The plate's own settings, for what a part and its volumes don't set.
    values: BTreeMap<&'static str, String>,
    memo: Arc<Mutex<Option<Arc<WeakMemo>>>>,
    before: Option<(Arc<WeakMemo>, Vec<(String, String)>)>,
}

impl WeakSpotJob {
    /// The list the last answer left.
    pub(crate) fn memo(&self) -> Option<Arc<WeakMemo>> {
        self.memo.lock().unwrap().clone()
    }

    /// The slice it reads.
    pub(crate) fn paths(&self) -> &Arc<PlatePaths> {
        &self.paths
    }
}

impl std::fmt::Debug for WeakSpotJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WeakSpotJob")
    }
}

impl PartialEq for WeakSpotJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|k| a[k] + b[k])
}

fn length(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt()
}

fn angle_to_bed(axis: [f64; 3]) -> f64 {
    axis[2].abs().clamp(0.0, 1.0).asin().to_degrees()
}

/// A value of `key` where the part's own settings and its volumes speak last.
fn setting_here(job: &WeakSpotJob, k: usize, q: [f64; 3], key: &str, plate: &str) -> String {
    let ev = &job.ev;
    let part = &ev.plate.parts[k];
    let mut value = plate.to_string();
    if let Some((_, v)) = part.print.iter().rev().find(|(name, _)| name == key) {
        value = v.clone();
    }
    let Some(inst) = split_id(&part.object).and_then(|(i, _)| job.c.instance(i)) else { return value };
    let p = &inst.script.program;
    for i in settings_solids(p) {
        let holds = fab_model::paint::volume_region(&inst.script, &p.solids[i].id).is_some_and(|region| region.distance(q) <= 0.0);
        if holds {
            if let Some((_, v)) = fab_model::paint::volume_settings(p, i).into_iter().rev().find(|(name, _)| name == key) {
                value = v;
            }
        }
    }
    value
}

impl WeakSpotJob {
    /// Each part's spots, worst first over the plate, kept for [`Session::strengthen_tool`].
    pub fn answer(&self, stop: &(dyn Fn() -> bool + Sync)) -> Result<S, String> {
        let t = std::time::Instant::now();
        let ev = &self.ev;
        let mut found: Vec<Spot> = Vec::new();
        let mut plate = Printed::default();
        let mut used = None;
        for (k, part) in ev.plate.parts.iter().enumerate() {
            let Some((plan, at)) = self.paths.plans().get(k) else { continue };
            let layers = plan_regions(plan);
            plate.add(&printed_inside(&plan.layers, &|_| true, stop).ok_or("cancelled")?);
            let (lo, hi) = part_box(&layers);
            let thin = self.min_thickness.unwrap_or_else(|| default_min_thickness((hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2])));
            used = Some(used.map_or(thin, |u: f64| u.min(thin)));
            let spots = weak_spots(&layers, thin, self.layer_factor, stop).ok_or("cancelled")?;
            let frame = slice_frame(ev, k);
            let to_model = fab_model::weave::invert(&frame);
            let scale = (frame[0][0].powi(2) + frame[1][0].powi(2) + frame[2][0].powi(2)).sqrt().max(1e-9);
            let shift = [at.x_mm(), at.y_mm(), 0.0];
            for spot in spots {
                let mut plated = spot.clone();
                plated.at = add(spot.at, shift);
                plated.middle = add(spot.middle, shift);
                plated.bounds = (add(spot.bounds.0, shift), add(spot.bounds.1, shift));
                let q = xf_point(&to_model, plated.at);
                let feature = ev.features.get(k).and_then(|f| fab_model::paint::feature_at(f, q, 0.5 * plated.thickness / scale)).map(|f| f.name.clone());
                // the line down its middle, when one follows from its place
                let stopping = || stop();
                let line = fit_limb(&layers, &[spot.middle], &stopping).ok().map(|limb| limb_line(&limb)).map(|(a, b)| (xf_point(&to_model, add(a, shift)), xf_point(&to_model, add(b, shift))));
                let round = 0.5 * plated.thickness + fab_model::volumes::FIT_MARGIN;
                let band = match line {
                    Some((a, b)) => (xf_point(&frame, a), xf_point(&frame, b), round),
                    None => (std::array::from_fn(|i| plated.at[i] - plated.axis[i] * round), std::array::from_fn(|i| plated.at[i] + plated.axis[i] * round), round),
                };
                let (blo, bhi) = spot.bounds;
                let printed = printed_inside(&plan.layers, &|p| (0..3).all(|i| p[i] >= blo[i] - 1e-6 && p[i] <= bhi[i] + 1e-6), stop).ok_or("cancelled")?;
                let setting = |key: &'static str| setting_here(self, k, q, key, self.values.get(key).map_or("", String::as_str));
                let called = feature.clone().unwrap_or_else(|| {
                    let word = match spot.kind {
                        SpotKind::Neck => "neck",
                        SpotKind::Limb => "limb",
                        SpotKind::Thin => "thin part",
                    };
                    format!("{word} near [{:.0} {:.0} {:.0}]", q[0], q[1], q[2])
                });
                found.push(Spot {
                    id: String::new(),
                    part: k,
                    object: part.object.clone(),
                    feature,
                    called,
                    found: plated,
                    model_at: q,
                    line,
                    scale,
                    band,
                    printed,
                    walls: setting("wall_loops"),
                    infill: setting("sparse_infill_density"),
                    pattern: setting("sparse_infill_pattern"),
                    field: ev.fields.get(k).is_some_and(|f| f.is_some()),
                });
            }
        }
        found.sort_by(|a, b| b.found.score.total_cmp(&a.found.score));
        // a feature's own worst place speaks for it: a fix on it is a fix on all of it
        let mut named = std::collections::BTreeSet::new();
        found.retain(|spot| spot.feature.as_ref().is_none_or(|feature| named.insert((spot.part, feature.clone()))));
        for (i, spot) in found.iter_mut().enumerate() {
            spot.id = format!("spot{}", i + 1);
        }
        let min_thickness = self.min_thickness.or(used).unwrap_or_else(|| default_min_thickness(0.0));
        let memo = Arc::new(WeakMemo { paths: self.paths.clone(), min_thickness, spots: found, plate });
        *self.memo.lock().unwrap() = Some(memo.clone());
        makepad_error_log::flight::mark_ab("fab.strength.find", "weak spots", memo.spots.len() as u64, t.elapsed().as_millis() as u64);
        let mut fields = vec![
            ("current", flag(true)),
            ("min_thickness", n(min_thickness)),
            ("measured_ms", int(t.elapsed().as_millis() as usize)),
            ("plate_mm3", n(memo.plate.mm3())),
        ];
        if let Some((was, fixes)) = &self.before {
            makepad_error_log::flight::mark_ab("fab.strength.resliced", "measured after", fixes.len() as u64, t.elapsed().as_millis() as u64);
            fields.push(("fixed", self.after(was, fixes, &memo)));
        }
        // last: a reply is bounded, and what's cut is the end of the list
        fields.push(("spots", arr(memo.spots.iter().map(|spot| crate::out::line(self.row(spot))))));
        Ok(obj(fields))
    }

    fn grams(&self, mm3: f64) -> S {
        self.density.map_or(S::Nil, |d| n(mm3 * d / 1000.0))
    }

    /// What solid infill would add at a spot, an estimate from the density it has now: the sparse
    /// lines' volume is that share of the space they run in.
    fn solid_adds(&self, spot: &Spot) -> f64 {
        let density = fab_plate::setting_number(&spot.infill).map_or(0.15, |d| if spot.infill.ends_with('%') { d / 100.0 } else { d }).clamp(0.01, 1.0);
        spot.printed.sparse_mm3 * (1.0 / density - 1.0)
    }

    fn fixes(&self, spot: &Spot, target: f64) -> Vec<S> {
        let t = spot.found.thickness;
        let rate = spot.printed.mm3() / spot.printed.seconds.max(1e-6);
        let mut out = Vec::new();
        let walls_share = spot.printed.walls_mm3 / spot.printed.mm3().max(1e-6);
        let thicken_first = spot.field && walls_share >= WALLS_FILL;
        if spot.printed.sparse_mm3 > SPARSE_SHARE * spot.printed.mm3() {
            let adds = self.solid_adds(spot);
            out.push(obj(vec![
                ("fix", s(SOLID)),
                ("default", flag(!thicken_first)),
                ("estimate_gain", n((spot.printed.mm3() + adds) / spot.printed.mm3().max(1e-6))),
                ("gain_of", s("material in its section")),
                ("adds_mm3", n(adds)),
                ("adds_grams", self.grams(adds)),
                ("adds_s", n(adds / rate.max(1e-6))),
            ]));
        }
        if spot.field && t < target {
            // along its line, and its shoulder runs on a little past that
            let long = spot.line.map_or(0.0, |(a, b)| length(a, b) * spot.scale) + target;
            let adds = std::f64::consts::FRAC_PI_4 * (target * target - t * t) * long;
            out.push(obj(vec![
                ("fix", s(THICKEN)),
                ("default", flag(thicken_first || out.is_empty())),
                ("to", n(target)),
                ("estimate_gain", n((target / t.max(1e-6)).powi(3))),
                ("gain_of", s("its section's bending strength")),
                ("adds_mm3", n(adds)),
                ("adds_grams", self.grams(adds)),
                ("adds_s", n(adds / rate.max(1e-6))),
            ]));
        }
        out
    }

    fn row(&self, spot: &Spot) -> S {
        let f = &spot.found;
        let target = self.memo.lock().unwrap().as_ref().map_or(f.thickness, |m| m.min_thickness);
        let kind = match f.kind {
            SpotKind::Neck => "neck",
            SpotKind::Limb => "limb",
            SpotKind::Thin => "thin",
        };
        let mut row = vec![
            ("id", s(&spot.id)),
            ("object", s(&spot.object)),
            ("where", s(&spot.called)),
            ("kind", s(kind)),
            ("at", mm3(f.at)),
            ("thickness", n(f.thickness)),
            ("angle_to_bed", n(angle_to_bed(f.axis))),
            ("axis", arr(f.axis.iter().map(|x| n(*x)))),
            ("bond_mm2", if f.bond.is_finite() { n(f.bond) } else { S::Nil }),
            ("bond_z", n(f.bond_z)),
            ("carries_mm3", n(f.carries)),
            ("score", n(f.score)),
            ("there", obj(vec![
                ("walls", s(&spot.walls)),
                ("infill", s(&spot.infill)),
                ("pattern", s(&spot.pattern)),
                ("walls_mm3", n(spot.printed.walls_mm3)),
                ("sparse_mm3", n(spot.printed.sparse_mm3)),
                ("solid_fill_mm3", n(spot.printed.solid_mm3)),
            ])),
            ("fixes", arr(self.fixes(spot, target))),
            ("band", obj(vec![
                ("a", arr(spot.band.0.iter().map(|x| n(*x)))),
                ("b", arr(spot.band.1.iter().map(|x| n(*x)))),
                ("radius", n(spot.band.2)),
            ])),
            // a box round it, as `fab.test_print` takes one
            ("test_print", {
                let (p, q, round) = spot.band;
                let middle: [f64; 3] = std::array::from_fn(|k| 0.5 * (p[k] + q[k]));
                let size: [f64; 3] = std::array::from_fn(|k| (q[k] - p[k]).abs() + 2.0 * (round + TEST_PRINT_MARGIN));
                obj(vec![
                    ("object", s(&spot.object)),
                    ("name", s(format!("{} test", spot.called))),
                    ("at", arr(middle.iter().map(|x| n(*x)))),
                    ("size", arr(size.iter().map(|x| n(*x)))),
                ])
            }),
        ];
        if let Some(feature) = &spot.feature {
            row.push(("feature", s(feature)));
        }
        obj(row)
    }

    /// Before and after, measured from the new slice: each fix's spot, and the plate.
    fn after(&self, was: &WeakMemo, fixes: &[(String, String)], now: &WeakMemo) -> S {
        let rows = fixes.iter().filter_map(|(id, fix)| {
            let then = was.spots.iter().find(|spot| spot.id == *id)?;
            // solid infill leaves the thickness as it was: `fab.inspect {read_volumes: true}` reads what it did
            if fix == SOLID {
                return Some(obj(vec![("spot", s(id)), ("where", s(&then.called)), ("fix", s(fix)), ("thickness_before", n(then.found.thickness)), ("after", s("solid infill placed"))]));
            }
            let still = now.spots.iter().find(|spot| same_place(then, spot));
            Some(obj(vec![
                ("spot", s(id)),
                ("where", s(&then.called)),
                ("fix", s(fix)),
                ("thickness_before", n(then.found.thickness)),
                ("thickness_after", still.map_or(S::Nil, |spot| n(spot.found.thickness))),
                ("after", s(if still.is_some() { "still under the minimum" } else { "no longer under the minimum" })),
            ]))
        });
        obj(vec![
            ("spots", arr(rows)),
            ("plate_mm3_before", n(was.plate.mm3())),
            ("plate_mm3_after", n(now.plate.mm3())),
            ("plate_grams_before", self.grams(was.plate.mm3())),
            ("plate_grams_after", self.grams(now.plate.mm3())),
            ("plate_print_s_before", n(was.plate.seconds)),
            ("plate_print_s_after", n(now.plate.seconds)),
        ])
    }
}

/// Whether a spot of a new list is an old one's place, whichever way up the part now is: by the
/// feature it's in, or by model mm off the old one's line.
fn same_place(then: &Spot, spot: &Spot) -> bool {
    if spot.part != then.part {
        return false;
    }
    if let (Some(was), Some(is)) = (&then.feature, &spot.feature) {
        return was == is;
    }
    let (a, b) = then.line.unwrap_or((then.model_at, then.model_at));
    segment_distance(spot.model_at, a, b) <= then.found.thickness / then.scale
}

fn part_box<R: AsRef<[ExPolygon]>>(layers: &[(f64, f64, R)]) -> ([f64; 3], [f64; 3]) {
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

/// Two splices of one script as one: `first` before `second`. The range of each is of the text as
/// it was.
fn one_splice(src: &str, first: (std::ops::Range<usize>, String), second: (std::ops::Range<usize>, String)) -> (std::ops::Range<usize>, String) {
    let text = format!("{}{}{}", first.1, &src[first.0.end..second.0.start], second.1);
    (first.0.start..second.0.end, text)
}

impl Session {
    /// The surfaces at weak spots `ids` of the last list, tinted: what a picture aimed at a spot shows.
    pub(crate) fn tint_spots(&mut self, ev: &Evaluated, ids: &[String], objects: &[String], mesh: &mut crate::supports::ColoredMesh) -> Result<(), String> {
        if ids.is_empty() {
            return Ok(());
        }
        let memo = self.weak_memo.lock().unwrap().clone().ok_or("no weak spots yet: fab.weak_spots first")?;
        for id in ids {
            let Some(spot) = memo.spots.iter().find(|spot| spot.id == *id) else {
                let all: Vec<&str> = memo.spots.iter().map(|spot| spot.id.as_str()).collect();
                return Err(format!("no weak spot '{id}': the list has [{}]", all.join(", ")));
            };
            if !objects.is_empty() && !objects.contains(&spot.object) {
                continue;
            }
            let to_model = fab_model::weave::invert(&slice_frame(ev, spot.part));
            let at = xf_point(&to_model, spot.found.at);
            let (a, b) = spot.line.unwrap_or((at, at));
            let radius = spot.found.thickness.max(memo.min_thickness) / spot.scale;
            self.tint_surface(ev, spot.part, &|q| segment_distance(q, a, b) - radius, crate::volumes::VOLUME_TINT, mesh)?;
        }
        Ok(())
    }

    /// `fab.weak_spots`: the find off the model worker, on the plate's slice: asked for with none
    /// made, this is what waits for it, as `fab.supports` does.
    pub(crate) fn weak_spots_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let Some(ev) = self.last.clone().filter(|ev| !ev.plate.parts.is_empty()) else { return Reply::err("nothing on the plate") };
        let Some(c) = self.compiled.clone() else { return Reply::err("nothing is built yet") };
        let min_thickness = match args.get("min_thickness") {
            None | Some(Value::Null) => None,
            Some(v) => match crate::number(v) {
                Some(t) if t > 0.0 => Some(t),
                _ => return Reply::err("'min_thickness' is a number in mm, more than 0"),
            },
        };
        let Some(source) = self.slices.clone() else { return Reply::err("this session has no slicer to read a slice from") };
        let paths = match source.made(&ev) {
            Some(paths) => paths,
            None => match source.input(&ev).and_then(|(input, _)| source.sliced(&input, cancelled)) {
                Ok(paths) => paths,
                Err(why) => return Reply::err(why),
            },
        };
        let setup = self.slices.as_ref().and_then(|src| src.input(&ev).ok()).and_then(|(input, _)| (input.setup)(None).ok());
        let density = setup.as_ref().and_then(|setup| setup.filaments.first().map(|f| f.values.f64_at("filament_density", 0)).filter(|d| *d > 0.0));
        let layer_factor = setup.as_ref().map_or(fab_force::material::PLA.layer_factor(0.2, 0.45), |setup| {
            let kind = setup.filaments.first().map_or("", |f| f.values.raw("filament_type", 0));
            let filament = fab_force::material::filament_of_type(kind).unwrap_or(&fab_force::material::PLA);
            filament.layer_factor(setup.process.values.f64("layer_height").max(0.05), setup.process.values.f64("inner_wall_line_width").max(0.1))
        });
        let mut values = BTreeMap::new();
        for key in ["wall_loops", "sparse_infill_density", "sparse_infill_pattern"] {
            let profile = setup.as_ref().and_then(|setup| setup.process.values.get(key)).map(|value| value.at(0).to_string());
            let default = fab_plate::print_setting(key).map(|row| row.default.to_string());
            if let Some(value) = profile.or(default) {
                values.insert(key, value);
            }
        }
        for (k, v) in &ev.plate.settings {
            if let Some(key) = ["wall_loops", "sparse_infill_density", "sparse_infill_pattern"].into_iter().find(|key| key == k) {
                values.insert(key, v.clone());
            }
        }
        // what the last fixes did, for as long as the plate is as they left it: a read that's cancelled
        // and asked again, or asked twice, says it as well
        let before = self.strength_before.clone().filter(|_| self.strength_head == Some(self.model.head()));
        let job = WeakSpotJob { ev, c, paths, min_thickness, layer_factor, density, values, memo: self.weak_memo.clone(), before };
        Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::WeakSpots(Arc::new(job))) }
    }

    /// `fab.strengthen`: the fixes picked from the last find, as one undo step.
    pub(crate) fn strengthen_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let expected = match self.head_arg(args) {
            Ok(h) => h,
            Err(r) => return r,
        };
        let (Some(c), Some(ev)) = (self.compiled.clone(), self.last.clone()) else { return Reply::err("nothing is built yet") };
        let Some(picks) = args.get("fixes").and_then(Value::as_arr).filter(|a| !a.is_empty()) else {
            return Reply::err("'fixes' is a list of {spot, fix}: fix is solid_infill or thicken (thicken takes 'to', mm), or {tube, fix: align} or {fix: orient, up, spin}");
        };
        // the spots picked read the last find, the tubes the last list of them
        let memo = if picks.iter().any(|pick| pick.get("spot").is_some()) {
            let Some(memo) = self.weak_memo.lock().unwrap().clone() else { return Reply::err("no weak spots to fix yet: fab.weak_spots first") };
            let current = self.slices.as_ref().and_then(|src| src.made(&ev)).is_some_and(|paths| Arc::ptr_eq(&paths, &memo.paths));
            if !current {
                return Reply::err("the list is of an older slice: fab.weak_spots again, and pick again");
            }
            Some(memo)
        } else {
            None
        };
        let shaped = match self.shape_picks(&c, &ev, picks) {
            Ok(shaped) => shaped,
            Err(why) => return Reply::err(why),
        };
        let mut volumes: Vec<Value> = Vec::new();
        let mut thick: BTreeMap<usize, Vec<Thicken>> = BTreeMap::new();
        let mut done: Vec<(String, String)> = Vec::new();
        let mut said: Vec<String> = shaped.said.clone();
        for pick in picks {
            if matches!(str_arg(pick, "fix"), Some(ALIGN | ORIENT)) {
                continue;
            }
            let (Some(id), Some(memo)) = (str_arg(pick, "spot"), &memo) else { return Reply::err("each fix names its 'spot'") };
            let Some(spot) = memo.spots.iter().find(|spot| spot.id == id) else {
                let ids: Vec<&str> = memo.spots.iter().map(|spot| spot.id.as_str()).collect();
                return Reply::err(format!("no weak spot '{id}': the list has [{}]", ids.join(", ")));
            };
            match str_arg(pick, "fix") {
                Some(SOLID) => {
                    if spot.printed.sparse_mm3 <= SPARSE_SHARE * spot.printed.mm3() {
                        return Reply::err(format!("{id} ({}): solid infill changes nothing, it has no sparse infill: thicken it instead", spot.called));
                    }
                    match spot_target(spot, shaped.line_of(spot)) {
                        Ok(target) => volumes.push(target),
                        Err(why) => return Reply::err(format!("{id} ({}): {why}", spot.called)),
                    }
                    said.push(format!("{}: solid infill", spot.called));
                }
                Some(THICKEN) => {
                    if !spot.field {
                        return Reply::err(format!("{id} ({}): this part's shape is a mesh: thicken needs a field. Solid infill still can", spot.called));
                    }
                    let to = pick.get("to").and_then(crate::number).unwrap_or(memo.min_thickness);
                    if to <= spot.found.thickness {
                        return Reply::err(format!("{id} ({}) is {:.1} mm already: 'to' is more than that", spot.called, spot.found.thickness));
                    }
                    let (a, b) = shaped.line_of(spot).unwrap_or_else(|| {
                        let at = shaped.moved(spot, xf_point(&fab_model::weave::invert(&slice_frame(&ev, spot.part)), spot.found.at));
                        (at, at)
                    });
                    thick.entry(spot.part).or_default().push(Thicken { a, b, radius: 0.5 * to / spot.scale });
                    said.push(format!("{}: thickened to {:.1} mm", spot.called, to));
                }
                _ => return Reply::err(format!("{id}: 'fix' is solid_infill or thicken, or align for a tube, or orient for the part")),
            }
            done.push((id.to_string(), str_arg(pick, "fix").unwrap_or("").to_string()));
        }
        // the volumes are one tool's plan; its splices go on the end of each script
        let mut changes: Vec<Change> = Vec::new();
        let mut solids = Vec::new();
        if !volumes.is_empty() {
            let plan_args = Value::Obj(vec![
                ("targets".into(), Value::Arr(volumes)),
                ("settings".into(), Value::Obj(vec![("sparse_infill_density".into(), Value::Str("100%".into()))])),
            ]);
            match self.volume_plan(&plan_args) {
                Ok((plan, ids, _)) => {
                    changes = plan;
                    solids = ids;
                }
                Err(why) => return Reply::err(why),
            }
        }
        for (k, spots) in thick {
            let part = &ev.plate.parts[k];
            let Some(inst) = split_id(&part.object).and_then(|(i, _)| c.instance(i)) else { return Reply::err(format!("{} has no script", part.object)) };
            let Some(object) = split_id(&part.object).and_then(|(_, name)| inst.script.objects.iter().find(|o| o.name == name)) else { return Reply::err(format!("{} has no body in its script", part.object)) };
            if object.bodies.len() != 1 {
                return Reply::err(format!("{} prints several bodies: thicken works on a single body so far", part.object));
            }
            let change = match thicken_change(inst, object.solid, &object.name, &spots) {
                Ok(change) => change,
                Err(why) => return Reply::err(why),
            };
            let Change::Script { file, edit: Edit::Splice { range, text } } = change else { return Reply::err("thicken writes a splice") };
            // one splice per script
            let same = changes.iter().position(|c| matches!(c, Change::Script { file: f, .. } if *f == file));
            let merged = match same.map(|i| changes.remove(i)) {
                Some(Change::Script { edit: Edit::Splice { range: r2, text: t2 }, .. }) => one_splice(inst.script.source(), (range, text), (r2, t2)),
                _ => (range, text),
            };
            changes.push(Change::Script { file, edit: Edit::Splice { range: merged.0, text: merged.1 } });
        }
        changes.extend(shaped.edits);
        if !shaped.moves.is_empty() {
            match self.place_changes(&shaped.moves, false) {
                Ok(moved) => changes.extend(moved),
                Err(reply) => return reply,
            }
        }
        let words = said.join(", ");
        if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
            return match self.model.preview(&mut self.eval, &changes, cancelled) {
                Ok(_) => Reply::ok(obj(vec![("dry_run", flag(true)), ("committed", flag(false)), ("done", s(&words))])),
                Err(e) => self.apply_error(e),
            };
        }
        let request_id = str_arg(args, "request_id").map_or_else(|| self.next_request_id(TOOL), str::to_string);
        let base = self.slices.as_ref().and_then(|src| src.made(&ev)).map(|paths| Arc::new(crate::volumes::VolumeBase::of(paths, &ev)));
        match self.commit_changes(&request_id, expected, &changes, cancelled, true) {
            Ok((applied, built)) => {
                makepad_error_log::flight::mark_ab("fab.strength.apply", &words, done.len() as u64, 0);
                if let Some(base) = base {
                    self.volume_bases.extend(solids.iter().map(|id: &String| (id.clone(), base.clone())));
                }
                self.strength_head = Some(applied.head);
                self.strength_before = memo.map(|memo| (memo, done));
                self.orient_was = shaped.was;
                self.commit_reply(vec![("head", s(applied.head.text())), ("done", s(&words)), ("solids", arr(solids.iter().map(s)))], built)
            }
            Err(e) => self.apply_error(e),
        }
    }
}

/// The line down a limb's middle: end to end where it meets something thicker, so a short neck has
/// one, and short of a tip by the limb's radius, so what's round the line stops at the tip.
fn limb_line(limb: &fab_printability::LimbFit) -> ([f64; 3], [f64; 3]) {
    let at = |from: [f64; 3], t: f64| -> [f64; 3] { std::array::from_fn(|k| from[k] + limb.axis[k] * t) };
    let length = length(limb.start, limb.end);
    let short = |end: &fab_printability::LimbEnd| if matches!(end, fab_printability::LimbEnd::Tip) { limb.radius } else { 0.0 };
    let (first, last) = (short(&limb.ends[0]), short(&limb.ends[1]));
    if first + last >= length {
        let middle = at(limb.start, 0.5 * length);
        return (middle, middle);
    }
    (at(limb.start, first), at(limb.end, -last))
}

fn segment_distance(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let (d, q): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| b[k] - a[k]), std::array::from_fn(|k| p[k] - a[k]));
    let dd: f64 = d.iter().map(|x| x * x).sum();
    let t = if dd > 0.0 { ((0..3).map(|k| q[k] * d[k]).sum::<f64>() / dd).clamp(0.0, 1.0) } else { 0.0 };
    (0..3).map(|k| (q[k] - d[k] * t).powi(2)).sum::<f64>().sqrt()
}

/// A spot as `fab.settings_volume` takes it: the feature it's in when it has one, else a capsule
/// down its middle.
fn spot_target(spot: &Spot, line: Option<([f64; 3], [f64; 3])>) -> Result<Value, String> {
    let object = Value::Str(spot.object.clone());
    if let Some((feature, (instance, _))) = spot.feature.as_ref().zip(split_id(&spot.object)) {
        return Ok(Value::Obj(vec![("feature".into(), Value::Str(part_id(instance, feature)))]));
    }
    let (a, b) = line.ok_or("no line follows from its place to fit a volume round: place one by numbers")?;
    let radius = 0.5 * spot.found.thickness / spot.scale + fab_model::volumes::FIT_MARGIN;
    let point = |p: [f64; 3]| Value::Arr(p.iter().map(|x| Value::F64(*x)).collect());
    Ok(Value::Obj(vec![
        ("name".into(), Value::Str(format!("{}_{}", if spot.found.kind == SpotKind::Neck { "neck" } else { "limb" }, spot.id))),
        ("object".into(), object),
        ("shape".into(), Value::Str("capsule".into())),
        ("a".into(), point(a)),
        ("b".into(), point(b)),
        ("radius".into(), Value::F64(radius)),
    ]))
}

#[cfg(test)]
#[path = "strengthen_tests.rs"]
pub(crate) mod tests;
