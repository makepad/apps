//! `fab.weave` and the weave scenes of `fab.simulate`: chains and chainmail (m.links, m.mail, m.drape) checked
//! from their links' exact geometry and run as free links in box3d, answered first with the verdicts.

use crate::out::{arr, flag, int, mm, mm3, n, obj, opt, s, S};
use fab_model::eval::Evaluated;
use fab_model::weave::{WeavePart, DENSITY};
use makepad_csg_mech::weave::{self, GapClass, WeaveProfile, WeaveScene};
use makepad_csg_mech::Profile;

fn class(c: GapClass) -> &'static str {
    match c {
        GapClass::Ok => "ok",
        GapClass::Tight => "tight",
        GapClass::Fused => "fused",
    }
}

fn part_value(ev: &Evaluated, w: &WeavePart, profile: &Profile) -> (bool, S) {
    let wp = WeaveProfile { r: w.links.radius, ..WeaveProfile::new(profile, Some(w.links.radius)) };
    let (c, (pieces, loose)) = w.check(&wp);
    let ok = c.verdict == GapClass::Ok && pieces == 1 && loose == 0;
    let mut fix = Vec::new();
    if let Some((i, j, g, k)) = c.close.first() {
        let at = w.weave(&wp).links[*i as usize].center();
        fix.push(format!("links {i} and {j} come within {g:.2} mm near [{:.1}, {:.1}, {:.1}] ({}): clearance: {:.2}mm or a larger size", at[0], at[1], at[2], class(*k), wp.c_xy + 0.1));
    }
    if pieces > 1 {
        fix.push(format!("the links make {pieces} separate pieces: the region narrows to less than two links somewhere; widen it or use a smaller size"));
    }
    if loose > 0 {
        fix.push(format!("{loose} link(s) interlock with nothing and print loose"));
    }
    if c.bridge > wp.bridge_max {
        fix.push(format!("bridges of {:.1} mm (at most {:.0}): a smaller size or a steeper leg", c.bridge, wp.bridge_max));
    }
    let name = ev.plate.parts.get(w.part).map(|p| p.object.clone()).unwrap_or_default();
    let painted = w.instances.slots.iter().filter(|s| **s != 0).count();
    (
        ok,
        obj(vec![
            ("part", s(name)),
            crate::checks::Check::of(ok).fields().remove(0),
            ("links", int(c.links)),
            ("interlocks", int(c.pairs)),
            ("pieces", int(pieces)),
            ("loose", int(loose)),
            ("bar_radius", mm(w.links.radius)),
            ("min_gap_in_layer", mm(c.min_gap_xy.0)),
            ("min_gap_in_layer_at", mm3(c.min_gap_xy.1)),
            // none: no link prints over another (every link stands on the bed beside the others)
            ("min_gap_across_layers", opt(c.min_gap_z.0.is_finite().then(|| mm(c.min_gap_z.0)))),
            ("min_gap_across_layers_at", opt(c.min_gap_z.0.is_finite().then(|| mm3(c.min_gap_z.1)))),
            ("clearance_used", obj(vec![("in_layer", mm(wp.c_xy)), ("across_layers", mm(wp.c_z))])),
            ("close_pairs", arr(c.close.iter().take(5).map(|(i, j, g, k)| obj(vec![("links", arr([int(*i), int(*j)])), ("gap", mm(*g)), ("prints", s(class(*k)))])))),
            ("longest_bridge", mm(c.bridge)),
            ("leg_angle_deg", n((c.leg_deg * 10.0).round() / 10.0)),
            ("painted_links", int(painted)),
            ("fix", arr(fix.into_iter().map(s))),
        ]),
    )
}

/// The job an as-printed check slices with: the app's own print setup when the host gave one, else Fab's
/// default process (one filament). The setup, its paint streams and what it is.
pub fn sliced_setup(ev: &Evaluated, print_setup: Option<&crate::PrintSetupFn>) -> Result<(makepad_fab_settings::profiles::JobSetup, Vec<Vec<String>>, String), String> {
    if let Some(f) = print_setup {
        return f(ev);
    }
    let mut setup = default_setup(&ev.machine, ev.plate.parts.len());
    for (k, v) in &ev.plate.settings {
        if makepad_fab_settings::options::options().defs.get(k.as_str()).is_some_and(|d| d.preset == makepad_fab_settings::options::PresetKind::Filament) {
            setup.filaments[0].values.set_str(k, v);
        } else {
            setup.process.values.set_str(k, v);
        }
    }
    Ok((setup, Vec::new(), "Fab's default process".into()))
}

/// Fab's default process with one filament for a plate of `parts` parts, on `machine`'s bed.
pub fn default_setup(machine: &fab_plate::machine::Machine, parts: usize) -> makepad_fab_settings::profiles::JobSetup {
    use makepad_fab_settings::profiles::{JobSetup, Preset};
    let mut bed = Preset::default();
    bed.values.set_vec("printable_area", machine.bed.corners());
    JobSetup {
        machine: bed,
        process: Preset::default(),
        filaments: vec![Preset::default()],
        filament_map: vec![1; parts],
        nozzle_volume_type: vec!["Standard".into()],
        bed_type: "Textured PEI Plate".into(),
        filament_map_mode: "Manual".into(),
        filament_colours: vec![None],
    }
}

/// A weave part judged from one slice: its beads, how its islands stack link by link, its steep facets
/// (the field-gradient check), and what moves.
pub struct Printed {
    pub beads: weave::BeadCheck,
    pub islands: fab_model::weave::IslandCheck,
    pub steep: fab_model::weave::SteepFacets,
    pub layers: usize,
    pub ms: f64,
    /// The process's overhang limit (deg from vertical) and longest bridge, mm.
    pub overhang_deg: f64,
    pub bridge_max: f64,
    /// Links, and the rigid bodies they print as (links that fuse or share an island print as one).
    pub links: usize,
    pub bodies: usize,
    /// Links that print fused to another.
    pub stuck: usize,
}

impl Printed {
    pub fn support_free(&self) -> bool {
        self.islands.support_free() && self.steep.support < 1e-6
    }

    /// Every link prints as its own free body.
    pub fn moves(&self) -> bool {
        self.stuck == 0 && self.beads.fused.is_empty() && self.beads.bridged.is_empty()
    }
}

/// Slices the plate once and judges every weave part.
pub fn check_sliced(ev: &Evaluated, setup: &makepad_fab_settings::profiles::JobSetup, paint: &[Vec<String>], fused: f64) -> Result<Vec<Printed>, String> {
    judge(&ev.machine, &ev.plate, &ev.weaves, setup, paint, fused)
}

/// [`check_sliced`] for any plate and the weave parts on it.
pub fn judge(machine: &fab_plate::machine::Machine, plate: &fab_plate::Plate, weaves: &[fab_model::weave::WeavePart], setup: &makepad_fab_settings::profiles::JobSetup, paint: &[Vec<String>], fused: f64) -> Result<Vec<Printed>, String> {
    let t = std::time::Instant::now();
    let sliced = makepad_fab_slice::plate::slice_plate_painted_with(setup, plate, paint, &|_, _| {})?;
    let ms = t.elapsed().as_secs_f64() * 1e3;
    let leg = makepad_csg_mech::weave::WeaveProfile::new(&Profile::default(), None).leg;
    // judged at the stricter of the process's overhang limit and the weave's own (its legs' 45 degrees)
    let mut materials = fab_printability::Materials::from_job(&sliced.job);
    materials.overhang_angle_deg = materials.overhang_angle_deg.min(90.0 - leg.to_degrees());
    Ok(weaves
        .iter()
        .map(|w| {
            let beads = w.check_printed(sliced.layers(), fused);
            let islands = w.check_islands(sliced.layers(), materials.overhang_angle_deg, materials.max_bridge_length, materials.line_width);
            // the field-gradient printability check of this part alone, judged per facet
            let to_model = fab_model::weave::invert(&w.to_plate);
            let links = w.links.clone();
            let field = move |x: f64, y: f64, z: f64| {
                let p = fab_model::weave::affine(&to_model, [x, y, z]);
                links.distance(makepad_csg_math::Vec3d::new(p[0], p[1], p[2]))
            };
            let report = fab_printability::analyse_plate_with(machine, plate, &materials, &|i| (i == w.part).then_some(&field as fab_printability::PartField), &|| false).expect("nothing stops it");
            let steep = match (plate.parts.get(w.part), report.overhang.get(w.part)) {
                (Some(part), Some(deg)) => w.steep_facets(&part.mesh, deg, leg, materials.first_layer_height),
                _ => Default::default(),
            };
            // rigid bodies: links joined by a fused or bridged bead, or sharing an island
            let n = w.links.loops.len();
            let mut parent: Vec<usize> = (0..n).collect();
            fn root(p: &mut [usize], mut i: usize) -> usize {
                while p[i] != i {
                    p[i] = p[p[i]];
                    i = p[i];
                }
                i
            }
            let mut stuck = vec![false; n];
            let joins = beads.fused.iter().map(|f| (f.0, f.1)).chain(beads.bridged.iter().map(|b| (b.0, b.1))).chain(islands.merged.iter().map(|m| (m.0, m.1)));
            for (a, b) in joins {
                let (a, b) = (a as usize, b as usize);
                if a < n && b < n {
                    stuck[a] = true;
                    stuck[b] = true;
                    let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                    parent[ra] = rb;
                }
            }
            let mut roots: Vec<usize> = (0..n).map(|i| root(&mut parent, i)).collect();
            roots.sort_unstable();
            roots.dedup();
            Printed {
                beads,
                islands,
                steep,
                layers: sliced.layers().len(),
                ms,
                overhang_deg: materials.overhang_angle_deg,
                bridge_max: materials.max_bridge_length,
                links: n,
                bodies: roots.len(),
                stuck: stuck.iter().filter(|s| **s).count(),
            }
        })
        .collect())
}

/// The sliced beads of a weave part against its links, how its islands stack, and what moves.
fn printed_value(p: &Printed) -> (bool, S) {
    let c = &p.beads;
    let i = &p.islands;
    let ok = p.moves() && p.support_free();
    (
        ok,
        obj(vec![
            crate::checks::Check::of(ok).fields().remove(0),
            ("layers", int(p.layers)),
            ("beads", int(c.beads)),
            ("min_bead_gap", opt(c.min_gap.map(|g| mm(g.0)))),
            ("min_bead_gap_at", opt(c.min_gap.map(|g| mm3(g.1)))),
            ("fused_pairs", arr(c.fused.iter().take(5).map(|(a, b, g, at)| obj(vec![("links", arr([int(*a), int(*b)])), ("gap", mm(*g)), ("at", mm3(*at))])))),
            ("bridged_pairs", arr(c.bridged.iter().take(5).map(|(a, b, at)| obj(vec![("links", arr([int(*a), int(*b)])), ("at", mm3(*at))])))),
            (
                "support",
                obj(vec![
                    ("free", flag(p.support_free())),
                    ("islands", int(i.islands)),
                    ("floating", int(i.floating.len())),
                    ("floating_at", arr(i.floating.iter().take(3).map(|(l, at)| obj(vec![("link", int(*l)), ("at", mm3(*at))])))),
                    ("unanchored", arr(i.unanchored.iter().take(3).map(|(l, d, at)| obj(vec![("link", int(*l)), ("reach", mm(*d)), ("at", mm3(*at))])))),
                    ("longest_bridge", mm(i.longest_bridge.0)),
                    ("bridge_limit", mm(p.bridge_max)),
                    ("steep_mm2", obj(vec![("on_bed", n((p.steep.on_bed * 10.0).round() / 10.0)), ("bridges", n((p.steep.bridge * 10.0).round() / 10.0)), ("needs_support", n((p.steep.support * 10.0).round() / 10.0))])),
                    ("steep_at", opt(p.steep.worst.map(mm3))),
                    ("overhang_limit_deg", n(p.overhang_deg)),
                ]),
            ),
            (
                "mobility",
                obj(vec![("moves", flag(p.moves())), ("links", int(p.links)), ("bodies", int(p.bodies)), ("stuck_links", int(p.stuck)), ("merged_islands", int(i.merged.len()))]),
            ),
            ("slice_ms", n(p.ms.round())),
        ]),
    )
}

/// `fab.weave`: every weave part's check (design geometry; with `as_printed`, also the sliced beads).
pub fn check_value(ev: &Evaluated, profile: &Profile, print_setup: Option<&crate::PrintSetupFn>, as_printed: bool) -> S {
    if ev.weaves.is_empty() {
        let mut fields = crate::checks::Check::NotChecked("the program has no chains or chainmail: m.links, m.mail, m.drape".into()).fields();
        fields.push(("weaves", arr(Vec::new())));
        return obj(fields);
    }
    // as printed: one slice of the plate with the app's own setup, judged per weave part
    let mut setup_name = String::new();
    let sliced = as_printed.then(|| {
        sliced_setup(ev, print_setup).and_then(|(setup, paint, name)| {
            setup_name = name;
            check_sliced(ev, &setup, &paint, 0.15)
        })
    });
    let parts: Vec<(bool, S)> = ev
        .weaves
        .iter()
        .enumerate()
        .map(|(k, w)| {
            let (ok, v) = part_value(ev, w, profile);
            let (pok, p) = match &sliced {
                None => return (ok, v),
                Some(Ok(checks)) => printed_value(&checks[k]),
                Some(Err(e)) => (false, obj(crate::checks::Check::NotChecked(format!("the plate could not be sliced: {e}")).fields())),
            };
            let v = match v {
                S::Object(name, mut fields) => {
                    fields.push(("as_printed".into(), p));
                    S::Object(name, fields)
                }
                other => other,
            };
            (ok && pok, v)
        })
        .collect();
    let mut fields = vec![crate::checks::Check::of(parts.iter().all(|p| p.0)).fields().remove(0)];
    if !as_printed {
        fields.push(("not_checked", crate::checks::not_checked(&[("the sliced beads".into(), "fab.weave {as_printed: true} slices the plate and checks them".into())])));
    }
    fields.extend([
        ("geometry", s(if as_printed { format!("design + as_printed ({setup_name})") } else { "design".into() })),
        ("profile", obj(vec![("nozzle", mm(profile.nozzle)), ("layer", mm(profile.layer)), ("material", s(&profile.material.name)), ("calibrated", flag(profile.measured.c_xy.is_some() || profile.measured.c_z_layers.is_some()))])),
        ("weaves", arr(parts.into_iter().map(|p| p.1))),
    ]);
    obj(fields)
}

pub fn scene_of(name: &str) -> Result<WeaveScene, String> {
    match name {
        "drop" => Ok(WeaveScene::Drop),
        "drape" => Ok(WeaveScene::Drape),
        "hang" => Ok(WeaveScene::Hang),
        other => Err(format!("a chain or chainmail runs in scenario drop, drape or hang, not {other}")),
    }
}

/// `fab.simulate` on the first weave part.
pub fn sim_value(ev: &Evaluated, profile: &Profile, scene: WeaveScene, seconds: f64, cancelled: &dyn Fn() -> bool) -> Result<S, String> {
    let w = &ev.weaves[0];
    let wp = WeaveProfile::new(profile, Some(w.links.radius));
    let woven = w.weave(&wp);
    let r = weave::simulate(&woven, DENSITY, scene, seconds, cancelled)?;
    let name = ev.plate.parts.get(w.part).map(|p| p.object.clone()).unwrap_or_default();
    Ok(obj(vec![
        ("scenario", s(r.scene)),
        ("part", s(name)),
        ("holds_together", flag(r.unlinked == 0)),
        ("unlinked_pairs", int(r.unlinked)),
        ("links", int(r.links)),
        ("deepest_overlap", mm((-r.deepest).max(0.0))),
        ("settled_at_s", opt(r.settled.map(n))),
        ("fell", mm(r.drop)),
        ("pairs_moved", mm(r.pair_motion)),
        ("size_at_end", mm3(r.size)),
        ("simulated_s", n(r.seconds)),
        ("step_ms", n((r.step_ms * 100.0).round() / 100.0)),
        ("cancelled", flag(r.cancelled)),
        ("ms", n(r.wall_ms.round())),
        ("others", opt((ev.weaves.len() > 1).then(|| s(format!("{} more chain parts not simulated", ev.weaves.len() - 1))))),
    ]))
}
