//! The model → slicer path: Fab's printable plate (`fab-plate`, parts in
//! printer coordinates) as the slicer's input.
//!
//! - [`PlateContours`] is the plate's [`fab_plate::contour::ContourSource`]:
//!   each part's mesh cut at a height, as integer loops on the plate's grid.
//! - [`PartContours`] reads one part of any plate contour source (the mesh
//!   one, or an exact SDF/CSG one) as the slicer's own contour source.
//! - [`slice_plate`] slices a plate: object `i` of the job (and of the
//!   preview records' `object`) is the plate's part `i`.

use crate::{slice_inputs_with, Progress, SliceResult};
use fab_plate::contour::{self as pc, ContourSource as PlateSource, Region};
use fab_plate::{Bounds, Part, Plate};
use makepad_fab_settings::profiles::JobSetup;
use makepad_fab_slicer::clip::{union, FillRule};
use makepad_fab_slicer::geom::{ExPolygon, Polygon, Pt, SCALE};
use makepad_fab_slicer::mesh::Mesh;
use makepad_fab_slicer::slice::{slice_mesh, SliceParams};
use makepad_fab_slicer::source::ContourSource;
use makepad_fab_toolpath::ObjectInput;
use std::sync::Arc;

/// Slicer grid units (nm) per plate grid unit (0.1 µm).
const SLICER_PER_PLATE: i64 = SCALE as i64 / pc::UNITS_PER_MM;

pub(crate) fn part_mesh(p: &Part) -> Mesh {
    // An exact part slices from its print-tolerance mesh; if that fails, the display mesh prints
    // within the display's chord, and the log says so.
    let fine = p.print_mesh.as_ref().and_then(|f| match (f.0)() {
        Ok(mesh) => Some(mesh),
        Err(e) => {
            eprintln!("[fab] {}: no print-tolerance mesh, slicing its display mesh: {e}", p.name);
            None
        }
    });
    let source = fine.as_ref().unwrap_or(&p.mesh);
    let mut m = Mesh { verts: source.positions.clone(), tris: source.triangles.clone(), paint: Vec::new() };
    m.fix_orientation();
    m
}

fn to_plate_loops(ex: &[ExPolygon]) -> Vec<pc::Loop> {
    let conv = |poly: &Polygon| pc::Loop { points: poly.pts.iter().map(|p| [(p.x + SLICER_PER_PLATE / 2).div_euclid(SLICER_PER_PLATE), (p.y + SLICER_PER_PLATE / 2).div_euclid(SLICER_PER_PLATE)]).collect() };
    let mut out = Vec::new();
    for e in ex {
        for r in e.rings() {
            let l = conv(r);
            if l.points.len() >= 3 {
                out.push(l);
            }
        }
    }
    out
}

fn from_plate_loops(loops: &[pc::Loop]) -> Vec<ExPolygon> {
    let rings: Vec<Polygon> = loops.iter().filter(|l| l.points.len() >= 3).map(|l| Polygon::new(l.points.iter().map(|p| Pt::new(p[0] * SLICER_PER_PLATE, p[1] * SLICER_PER_PLATE)).collect())).collect();
    union(&rings, FillRule::NonZero)
}

/// The plate's parts cut by the slicer's mesh slicer.
pub struct PlateContours {
    parts: Vec<(Mesh, u32, Option<usize>)>,
    bounds: Bounds,
    params: SliceParams,
}

impl PlateContours {
    pub fn new(plate: &Plate) -> Self {
        Self { parts: plate.parts.iter().map(|p| (part_mesh(p), p.filament, p.nozzle)).collect(), bounds: plate.bounds(), params: SliceParams::default() }
    }

    /// Every part's regions at each height (batched: one sweep per part).
    pub fn layers(&self, zs: &[f64]) -> Vec<Vec<Region>> {
        let mut out: Vec<Vec<Region>> = vec![Vec::new(); zs.len()];
        for (i, (mesh, filament, nozzle)) in self.parts.iter().enumerate() {
            for (k, ex) in slice_mesh(mesh, zs, &self.params).into_iter().enumerate() {
                if !ex.is_empty() {
                    out[k].push(Region { part: i, filament: *filament, nozzle: *nozzle, loops: to_plate_loops(&ex) });
                }
            }
        }
        out
    }
}

impl PlateSource for PlateContours {
    fn bounds(&self) -> Bounds {
        self.bounds
    }
    fn layer(&self, z: f64) -> Vec<Region> {
        self.layers(&[z]).pop().unwrap_or_default()
    }
}

/// Part `part` of a plate contour source, as the slicer's contour source.
pub struct PartContours<S: PlateSource + Send + Sync> {
    pub source: Arc<S>,
    pub part: usize,
    pub bounds: ([f64; 3], [f64; 3]),
}

impl<S: PlateSource + Send + Sync> ContourSource for PartContours<S> {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.bounds
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        zs.iter()
            .map(|&z| {
                let loops: Vec<pc::Loop> = self.source.layer(z).into_iter().filter(|r| r.part == self.part).flat_map(|r| r.loops).collect();
                from_plate_loops(&loops)
            })
            .collect()
    }
}

/// The filament slot (0-based) and nozzle checks of a part against the job.
pub(crate) fn part_filament(setup: &JobSetup, i: usize, p: &Part) -> Result<usize, String> {
    let nf = setup.filaments.len();
    if p.filament == 0 || p.filament as usize > nf {
        return Err(format!("part {} ({}) uses filament {} but the job has {nf}", i + 1, p.name, p.filament));
    }
    let f = p.filament as usize - 1;
    if let Some(n) = p.nozzle {
        // The job's filament map is 1-based in the machine's nozzle order
        // (1 = left), the same order as the plate's nozzle index.
        let mapped = setup.filament_map.get(f).copied().unwrap_or(1).max(1) as usize - 1;
        if mapped != n {
            return Err(format!("part {} ({}) must print on nozzle {} but filament {} is mapped to nozzle {}", i + 1, p.name, n + 1, p.filament, mapped + 1));
        }
    }
    Ok(f)
}

/// Slice the plate: the parts' meshes through the mesh slicer.
pub fn slice_plate(setup: &JobSetup, plate: &Plate) -> Result<SliceResult, String> {
    slice_plate_with(setup, plate, &|_, _| {})
}

/// [`slice_plate`], reporting its stages to `progress`.
pub fn slice_plate_with(setup: &JobSetup, plate: &Plate, progress: Progress) -> Result<SliceResult, String> {
    slice_plate_painted_with(setup, plate, &[], progress)
}

/// [`slice_plate_with`] for parts with colour paint: `paint[i]` is part
/// `i`'s per-triangle paint stream (the 3MF `paint_color` hex, "" for an
/// unpainted triangle); missing or mismatched entries slice unpainted.
pub fn slice_plate_painted_with(setup: &JobSetup, plate: &Plate, paint: &[Vec<String>], progress: Progress) -> Result<SliceResult, String> {
    // Every part in printer coordinates: one frame.
    let regions = PaintRegions::default();
    let mut srcs = Vec::with_capacity(plate.parts.len());
    let mut filaments = Vec::with_capacity(plate.parts.len());
    for (i, p) in plate.parts.iter().enumerate() {
        let filament = part_filament(setup, i, p)?;
        let mut mesh = part_mesh(p);
        if let Some(pt) = paint.get(i).filter(|pt| pt.len() == mesh.tris.len()) {
            // `fix_orientation` keeps the triangle order, so the streams still line up.
            mesh.paint = pt.clone();
        }
        let source: Arc<dyn ContourSource + Send + Sync> = Arc::new(makepad_fab_slicer::source::MeshSource::new(mesh));
        srcs.push(painted(p, source, filament as u32 + 1, |q| q, |q| q, 1.0, Some((&regions, &[]))));
        filaments.push(filament);
    }
    slice_glued(setup, plate, srcs, filaments, progress)
}

/// The plate's parts as `srcs` cut them (in printer coordinates), with the parts that print as
/// one body glued ([`crate::glue`]).
fn slice_glued(setup: &JobSetup, plate: &Plate, mut srcs: Vec<crate::glue::Source>, filaments: Vec<usize>, progress: Progress) -> Result<SliceResult, String> {
    let gluing = crate::glue::groups(&plate.groups(), srcs.len(), |g| crate::glue::apart(g, |i| &*srcs[i], |i| plate.parts[i].object.as_str()));
    let at = vec![Pt::default(); srcs.len()];
    crate::glue::apply(&gluing.glued, &mut srcs, &at, &filaments);
    if !plate.embeds().is_empty() {
        embed_all(&crate::prepare_job(setup)?, plate, &mut srcs, &filaments, |_| [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]]);
    }
    let boxes = srcs.iter().map(|s| s.bounds()).collect();
    let inputs = plate.parts.iter().zip(srcs).zip(filaments).map(|((p, source), filament)| ObjectInput { overrides: p.print.clone(), name: p.name.clone(), source: Box::new(Shared(source)), filament }).collect();
    let mut sliced = slice_inputs_with(setup, inputs, boxes, progress)?;
    // what prints apart though its object prints its bodies as one says so
    sliced.warnings.extend(gluing.apart.iter().map(|(g, why)| fab_printability::bodies_apart(plate, g, why).message));
    Ok(sliced)
}

/// Cut the items printed into the plate's parts ([`Plate::embeds`]) from their hosts: `srcs[i]`
/// is part `i`'s source, after gluing (a glued group's first part's is the group's), and `to_src(i)`
/// the map from the evaluated plate into its frame.
pub(crate) fn embed_all(job: &makepad_fab_settings::machine::Job, plate: &Plate, srcs: &mut [crate::glue::Source], filaments: &[usize], to_src: impl Fn(usize) -> crate::live::Affine) {
    for (host, items) in plate.embeds() {
        let own = (!plate.parts[host].print.is_empty()).then(|| job.with_overrides(&plate.parts[host].print));
        let thickest = makepad_fab_toolpath::layers::thickest(&makepad_fab_toolpath::Params::from_job(own.as_ref().unwrap_or(job), filaments[host]));
        srcs[host] = crate::live::embedded(srcs[host].clone(), 0, &to_src(host), &items, thickest).0;
    }
}

/// Slice the plate's parts as `source` cuts them (an exact source, or
/// [`PlateContours`]); `plate` gives the parts' filaments, nozzles and names.
pub fn slice_source<S: PlateSource + Send + Sync + 'static>(setup: &JobSetup, plate: &Plate, source: Arc<S>) -> Result<SliceResult, String> {
    let mut srcs: Vec<crate::glue::Source> = Vec::with_capacity(plate.parts.len());
    let mut filaments = Vec::with_capacity(plate.parts.len());
    for (i, p) in plate.parts.iter().enumerate() {
        filaments.push(part_filament(setup, i, p)?);
        let b = p.mesh.bounds();
        srcs.push(Arc::new(PartContours { source: source.clone(), part: i, bounds: (b.min, b.max) }));
    }
    slice_glued(setup, plate, srcs, filaments, &|_, _| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_plate::PartMesh;
    use makepad_fab_gcode::record::MoveKind;
    use makepad_fab_settings::profiles::Preset;

    fn cube(min: [f64; 3], size: f64) -> PartMesh {
        let positions = (0..8u32).map(|i| [min[0] + size * (i & 1) as f64, min[1] + size * ((i >> 1) & 1) as f64, min[2] + size * ((i >> 2) & 1) as f64]).collect();
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        PartMesh { positions, triangles }
    }

    fn part(name: &str, filament: u32, nozzle: Option<usize>, mesh: PartMesh) -> Part {
        Part { body_of: None, print: Vec::new(), print_mesh: None, paint: None, object: name.into(), name: name.into(), filament, color: [1.0; 4], nozzle, mesh, field_bounds: None, field_floor: None, ..Default::default() }
    }

    /// A two-nozzle job from the option defaults, on the app's default printer's bed: no vendor
    /// data.
    fn setup(map: Vec<u32>) -> JobSetup {
        let mut machine = Preset::default();
        machine.values.set_vec("printable_area", fab_plate::machine::Machine::dual_nozzle_350().bed.corners());
        machine.values.set_vec("nozzle_diameter", vec!["0.4".into(), "0.4".into()]);
        machine.values.set_vec("extruder_type", vec!["Direct Drive".into(), "Direct Drive".into()]);
        machine.values.set_vec("physical_extruder_map", vec!["1".into(), "0".into()]);
        let nf = map.len();
        JobSetup {
            machine,
            process: Preset::default(),
            filaments: (0..nf).map(|_| Preset::default()).collect(),
            filament_map: map,
            nozzle_volume_type: vec!["Standard".into(), "Standard".into()],
            bed_type: "Textured PEI Plate".into(),
            filament_map_mode: "Manual".into(),
            filament_colours: vec![None; nf],
        }
    }

    fn plate() -> Plate {
        Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("a", 1, Some(0), cube([100.0, 100.0, 0.0], 10.0)), part("b", 2, None, cube([200.0, 100.0, 0.0], 10.0))] }
    }

    /// An item printed into a part: no bead enters it, the print pauses once its cavity is
    /// printed, and the first layer after the pause closes it in, bridging over it. Supports on,
    /// the item holds that layer: nothing stands where it goes.
    #[test]
    fn an_embedded_item_is_cut_from_its_host_and_paused_for() {
        use makepad_fab_toolpath::Role;
        let host = part("host", 1, None, cube([100.0, 100.0, 0.0], 10.0));
        let nut = part("nut", 1, None, cube([103.0, 103.0, 3.0], 4.0));
        let insert = fab_plate::Insert { into: "host".into(), clearance: 0.2, embed: true, melt: None };
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let excluded = fab_plate::ExcludedPart { part: nut.clone(), link: None, frame: None, insert: Some(insert.clone()), render: Default::default(), handles: Default::default(), model_to_plate: identity, paint: None };
        let plate = Plate { parts: vec![host], excluded: vec![excluded], settings: Vec::new() };
        for support in [None, Some("normal(auto)"), Some("tree(auto)")] {
            let mut s = setup(vec![1]);
            if let Some(kind) = support {
                s.process.values.set_str("enable_support", "1");
                s.process.values.set_str("support_type", kind);
            }
            let res = slice_plate(&s, &plate.printable()).unwrap();
            let hits = fab_printability::insert::bead_issues(&[(&nut, &insert)], &mut res.timeline.moves().copied());
            let near: Vec<String> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && m.z_layer > 3.1 && m.z_layer < 6.9).filter_map(|m| {
                // The bead's centre from the nut's box (103..107 square), sampled along the move.
                let d = (0..=20).map(|k| {
                    let t = k as f32 / 20.0;
                    let (x, y) = (m.p0[0] + t * (m.p1[0] - m.p0[0]), m.p0[1] + t * (m.p1[1] - m.p0[1]));
                    let (dx, dy) = ((103.0 - x).max(x - 107.0).max(0.0), (103.0 - y).max(y - 107.0).max(0.0));
                    dx.hypot(dy)
                }).fold(f32::INFINITY, f32::min);
                (d < 0.2 + 0.5 * m.width).then(|| format!("{:?} z {} w {} d {d:.4} {:?}->{:?} arc {}", m.role, m.z_layer, m.width, m.p0, m.p1, m.flags & makepad_fab_gcode::record::flags::ARC != 0))
            }).take(8).collect();
            assert!(hits.is_empty(), "{support:?}: no bead enters the item: {:?} near: {near:#?}", hits.iter().map(|h| &h.message).collect::<Vec<_>>());
            let pauses = res.toolpaths.pauses();
            assert_eq!(pauses.len(), 1, "{support:?}: one pause");
            let (layers, at) = (&res.toolpaths.layers, pauses[0].layer);
            assert_eq!(res.toolpaths.objects[0].embeds[0].name, "nut");
            assert!(layers[at].print_z - layers[at].height >= 7.0 - 1e-9, "{support:?}: the first layer after the pause starts at or over the item's top (7 mm): {}", layers[at].print_z - layers[at].height);
            let centre = Pt::from_mm(105.0, 105.0);
            let holds = |l: usize| layers[l].objects.iter().filter(|o| !o.support_only).any(|o| o.slices.iter().any(|e| e.contains(centre)));
            assert!(!holds(at - 1) && holds(at), "{support:?}: the cavity is open up to the pause and closed by the layer after it");
            assert!((0..at).filter(|&l| layers[l].print_z > 3.0).all(|l| !holds(l)), "{support:?}: open from the item's bottom up");
            // The beads of that layer that pass over the item's middle.
            let over: Vec<_> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && (m.z_layer as f64 - layers[at].print_z).abs() < 1e-3).filter(|m| (0..=20).any(|k| {
                let t = k as f32 / 20.0;
                (m.p0[0] + t * (m.p1[0] - m.p0[0]) - 105.0).hypot(m.p0[1] + t * (m.p1[1] - m.p0[1]) - 105.0) < 1.5
            })).collect();
            assert!(!over.is_empty() && over.iter().all(|m| m.role == Role::Bridge), "{support:?}: the layer over the item bridges it: {:?}", over.iter().map(|m| m.role).collect::<std::collections::BTreeSet<_>>());
            let supports = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && matches!(m.role, Role::Support | Role::SupportInterface | Role::SupportTransition)).count();
            assert_eq!(supports, 0, "{support:?}: the item holds the layer over it, and nothing else overhangs");
            assert_eq!(res.gcode.matches("; FAB_PAUSE: ").count(), 1);
            assert!(res.gcode.contains(&format!("; FAB_PAUSE: layer {} ", at + 1)), "{support:?}: the G-code pauses before that layer");
            let mut report = fab_printability::Report::default();
            fab_printability::analyse_slice(&fab_plate::machine::Machine::dual_nozzle_350(), &plate, &fab_printability::Materials::from_job(&res.job), res.layers(), &res.timeline, &mut report);
            assert!(report.issues.iter().all(|i| i.kind != "embed_uncovered" && i.kind != "insert_hit"), "{support:?}: {:?}", report.issues.iter().map(|i| &i.message).collect::<Vec<_>>());
        }
        // An item up to the part's top: nothing prints over it, so nothing pauses, and the report
        // says so.
        let mut proud = plate.clone();
        proud.excluded[0].part = part("nut", 1, None, cube([103.0, 103.0, 7.0], 3.0));
        let res = slice_plate(&setup(vec![1]), &proud.printable()).unwrap();
        assert!(res.toolpaths.pauses().is_empty() && !res.gcode.contains("FAB_PAUSE"), "no layer over it, no pause");
        let mut report = fab_printability::Report::default();
        fab_printability::analyse_slice(&fab_plate::machine::Machine::dual_nozzle_350(), &proud, &fab_printability::Materials::from_job(&res.job), res.layers(), &res.timeline, &mut report);
        assert_eq!(report.issues.iter().filter(|i| i.kind == "embed_uncovered").map(|i| i.message.as_str()).collect::<Vec<_>>(), ["Nothing prints over nut: the print never pauses to place it, and nothing closes it in"]);
    }

    /// A part's own print modes reach its toolpaths only: a spiral vase on
    /// part "a" climbs; part "b" next to it prints flat.
    #[test]
    fn per_part_modes_stay_with_their_part() {
        let mut p = plate();
        p.parts[0].print = vec![("spiral_mode".into(), "1".into())];
        let res = slice_plate(&setup(vec![1, 1]), &p).unwrap();
        let sloped = |part: u16| res.timeline.moves().filter(|m| m.object == part && m.flags & makepad_fab_gcode::record::flags::SLOPED != 0).count();
        assert!(sloped(0) > 100, "part a climbs ({} sloped moves)", sloped(0));
        assert_eq!(sloped(1), 0, "part b stays planar");
    }

    /// Extruded mm of a part per role (what a setting's effect shows up in, whatever order
    /// the paths print in).
    fn lengths(res: &crate::SliceResult, part: u16) -> std::collections::BTreeMap<String, f64> {
        let mut out = std::collections::BTreeMap::new();
        for m in res.timeline.moves().filter(|m| m.object == part && m.kind == MoveKind::Extrude) {
            let d = (0..3).map(|i| (m.p1[i] - m.p0[i]) as f64).map(|v| v * v).sum::<f64>().sqrt();
            *out.entry(format!("{:?}", m.role)).or_insert(0.0) += d;
        }
        out
    }

    fn same_lengths(a: &std::collections::BTreeMap<String, f64>, b: &std::collections::BTreeMap<String, f64>) {
        assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
        for (role, l) in a {
            assert!((l - b[role]).abs() <= 0.002 * l.max(1.0), "{role}: {l} against {}", b[role]);
        }
    }

    fn twins() -> Plate {
        // The same mesh twice: a copy with its own settings must not share its twin's plan.
        Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("a", 1, None, cube([100.0, 100.0, 0.0], 25.0)), part("b", 1, None, cube([200.0, 100.0, 0.0], 25.0))] }
    }

    /// An object's infill and walls change its own toolpath; its neighbour keeps its own.
    #[test]
    fn an_objects_infill_and_walls_change_its_toolpath_and_not_its_neighbours() {
        let s = setup(vec![1]);
        let base = slice_plate(&s, &twins()).unwrap();
        let mut p = twins();
        p.parts[0].print = vec![("sparse_infill_density".into(), "60%".into()), ("wall_loops".into(), "5".into())];
        let res = slice_plate(&s, &p).unwrap();
        let (a0, a1) = (lengths(&base, 0), lengths(&res, 0));
        assert!(a1["InnerWall"] > 1.5 * a0["InnerWall"], "more walls: {} against {}", a1["InnerWall"], a0["InnerWall"]);
        assert!(a1["SparseInfill"] > 1.5 * a0["SparseInfill"], "denser infill: {} against {}", a1["SparseInfill"], a0["SparseInfill"]);
        same_lengths(&lengths(&base, 1), &lengths(&res, 1));
        // Without overrides the twins print alike.
        same_lengths(&lengths(&base, 0), &lengths(&base, 1));
    }

    /// The plate's global set reaches every object, and an object's own value wins over it.
    #[test]
    fn global_settings_reach_every_object_and_an_override_wins() {
        let mut s = setup(vec![1]);
        let base = slice_plate(&s, &twins()).unwrap();
        s.process.values.set_str("wall_loops", "4");
        let global = slice_plate(&s, &twins()).unwrap();
        assert!(lengths(&global, 0)["InnerWall"] > 1.5 * lengths(&base, 0)["InnerWall"]);
        assert!(lengths(&global, 1)["InnerWall"] > 1.5 * lengths(&base, 1)["InnerWall"]);
        let mut p = twins();
        p.parts[1].print = vec![("wall_loops".into(), "2".into())];
        let own = slice_plate(&s, &p).unwrap();
        same_lengths(&lengths(&global, 0), &lengths(&own, 0));
        same_lengths(&lengths(&base, 1), &lengths(&own, 1));
    }

    /// Speeds and accelerations are the object's own.
    #[test]
    fn an_objects_speed_and_acceleration_stay_with_it() {
        let mut s = setup(vec![1]);
        s.process.values.set_str("outer_wall_speed", "100");
        s.process.values.set_str("default_acceleration", "5000");
        // Neither the hot end's flow nor layer cooling caps the speeds compared here.
        for f in &mut s.filaments {
            f.values.set_str("filament_max_volumetric_speed", "40");
            f.values.set_str("slow_down_for_layer_cooling", "0");
        }
        // Under the machine's default extruding limit (1500 mm/s²), which caps every acceleration.
        s.process.values.set_str("outer_wall_acceleration", "1400");
        let mut p = twins();
        p.parts[0].print = vec![("outer_wall_speed".into(), "20".into()), ("outer_wall_acceleration".into(), "600".into())];
        let res = slice_plate(&s, &p).unwrap();
        let outer = |part: u16| -> Vec<_> { res.timeline.moves().filter(|m| m.object == part && m.kind == MoveKind::Extrude && m.p1[2] > 0.5 && m.e > 0.0 && format!("{:?}", m.role) == "OuterWall").copied().collect() };
        let (a, b) = (outer(0), outer(1));
        assert!(!a.is_empty() && !b.is_empty());
        let max = |v: &[makepad_fab_gcode::record::MoveRecord]| v.iter().map(|m| m.f_cmd).fold(0.0f32, f32::max);
        let accel = |v: &[makepad_fab_gcode::record::MoveRecord]| v.iter().map(|m| m.accel).fold(0.0f32, f32::max);
        assert!(max(&a) <= 20.5, "its own speed: {}", max(&a));
        assert!(max(&b) > 50.0, "the neighbour's: {}", max(&b));
        assert!(accel(&a) <= 600.5, "its own acceleration: {}", accel(&a));
        assert!(accel(&b) > 1200.0, "the neighbour's: {}", accel(&b));
    }

    /// Layer heights differ per object: each stack keeps its own heights on the shared Z.
    #[test]
    fn an_object_prints_at_its_own_layer_height() {
        let s = setup(vec![1]);
        let mut p = twins();
        p.parts[0].print = vec![("layer_height".into(), "0.1".into())];
        let res = slice_plate(&s, &p).unwrap();
        let zs = |part: u16| -> Vec<f32> {
            let mut z: Vec<f32> = res.timeline.moves().filter(|m| m.object == part && m.kind == MoveKind::Extrude).map(|m| m.p1[2]).collect();
            z.sort_by(|a, b| a.partial_cmp(b).unwrap());
            z.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
            z
        };
        let (a, b) = (zs(0), zs(1));
        assert!(a.len() > (b.len() as f32 * 1.8) as usize, "{} layers against {}", a.len(), b.len());
        assert!((b[1] - b[0] - 0.2).abs() < 1e-3 && (a[1] - a[0] - 0.1).abs() < 1e-3, "{:?} {:?}", &a[..2], &b[..2]);
        // Z only climbs, so no nozzle pass meets a taller neighbour.
        let moves: Vec<f32> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && m.object != u16::MAX).map(|m| m.p1[2]).collect();
        assert!(moves.windows(2).all(|w| w[1] >= w[0] - 1e-3), "Z climbs through the print");
    }

    /// On two filaments the shared tower stands on every merged step: its beads are as high as
    /// the step below them, not as the thickest object's layer.
    #[test]
    fn the_tower_prints_the_real_step_between_mixed_layer_heights() {
        // Two filaments on one nozzle: every change primes on the tower.
        let mut s = setup(vec![1, 1]);
        s.process.values.set_str("enable_prime_tower", "1");
        let mut p = plate();
        p.parts[0].nozzle = None;
        p.parts[0].print = vec![("layer_height".into(), "0.1".into())];
        let res = slice_plate(&s, &p).unwrap();
        let tower: Vec<_> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && format!("{:?}", m.role) == "PrimeTower").copied().collect();
        assert!(!tower.is_empty(), "two filaments print a tower");
        let above_first: Vec<_> = tower.iter().filter(|m| m.p1[2] > 0.25).collect();
        let highest = above_first.iter().map(|m| m.height).fold(0.0f32, f32::max);
        assert!(highest <= 0.2 + 1e-3, "beads fit their step: {highest} at z {:?}", above_first.iter().find(|m| m.height == highest).map(|m| m.p1[2]));
        assert!(above_first.iter().any(|m| (m.height - 0.1).abs() < 1e-3), "the 0.1 step prints at 0.1");
    }

    /// Two parts a rounding apart in height print their tops as one layer, so the tower holds
    /// beads as high as the layers and not as high as the rounding.
    #[test]
    fn parts_a_rounding_apart_in_height_share_their_top_layer() {
        let mut s = setup(vec![1, 1]);
        s.process.values.set_str("enable_prime_tower", "1");
        let tower_box = |p: &Plate| {
            let res = slice_plate(&s, p).unwrap();
            assert!(res.timeline.filament_swaps > 0 && res.timeline.head_changes == 0, "one nozzle only swaps");
            let tower: Vec<_> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && format!("{:?}", m.role) == "PrimeTower" && m.layer > 0).copied().collect();
            let range = |f: &dyn Fn(&[f32; 3]) -> f32| {
                let v = tower.iter().map(|m| f(&m.p1));
                v.clone().fold(f32::MIN, f32::max) - v.fold(f32::MAX, f32::min)
            };
            (range(&|p| p[0]), range(&|p| p[1]), res.timeline.moves().map(|m| m.layer).max().unwrap())
        };
        let mut p = plate();
        p.parts[0].nozzle = None;
        let even = tower_box(&p);
        // Both tops land past the grid, a quarter micron apart.
        p.parts[0].mesh = cube([100.0, 100.0, 0.0], 10.00875);
        p.parts[1].mesh = cube([200.0, 100.0, 0.0], 10.0085);
        let apart = tower_box(&p);
        assert_eq!(apart.2, even.2, "no extra layer: {apart:?} {even:?}");
        assert!((apart.0 - even.0).abs() < 1.0 && (apart.1 - even.1).abs() < 1.0, "the same tower: {apart:?} {even:?}");
    }

    /// A job on `nozzles` nozzles (`map`: each filament's), its filaments in `colours`, weighed.
    fn shaped(nozzles: usize, map: Vec<u32>, colours: &[&str]) -> JobSetup {
        let mut s = setup(map);
        s.machine.values.set_vec("nozzle_diameter", vec!["0.4".into(); nozzles]);
        s.machine.values.set_vec("extruder_type", vec!["Direct Drive".into(); nozzles]);
        s.machine.values.set_vec("physical_extruder_map", (0..nozzles).map(|e| e.to_string()).collect());
        s.nozzle_volume_type = vec!["Standard".into(); nozzles];
        s.filament_colours = colours.iter().take(s.filaments.len()).map(|c| Some(c.to_string())).collect();
        for f in &mut s.filaments {
            f.values.set_vec("filament_density", vec!["1.24".into()]);
        }
        s
    }

    /// Every change is counted at what the printer's change model says it costs, in the
    /// G-code's time, the slot's filament and Printability: same-nozzle swaps on a two-nozzle
    /// head and on one nozzle fed from four slots purge; a tool changer, whose tools carry a
    /// filament each, and a profile whose swaps cost nothing purge nothing.
    #[test]
    fn every_change_purges_what_the_printer_says() {
        let shaped = |nozzles: usize, map: Vec<u32>| shaped(nozzles, map, &["#FFFFFF", "#000000", "#FF0000", "#0000FF"]);
        let mut p = plate();
        p.parts = (0..4).map(|i| part(&format!("p{i}"), i as u32 + 1, None, cube([100.0 + 15.0 * (i % 2) as f64, 100.0 + 15.0 * (i / 2) as f64, 0.0], 3.0))).collect();
        let check = |what: &str, s: &JobSetup| {
            let res = slice_plate(s, &p).unwrap();
            let tl = &res.timeline;
            let materials = fab_printability::Materials::from_job(&res.job);
            let machine = fab_plate::machine::Machine::dual_nozzle_350();
            let mut report = fab_printability::Report::default();
            fab_printability::analyse_slice(&machine, &p, &materials, res.layers(), tl, &mut report);
            let purge_g: f64 = (0..4).map(|f| tl.purge_mm[f] * res.job.filament_area(f) * res.job.fil("filament_density", f) / 1000.0).sum();
            let figures = report.plate.as_ref().expect("the sliced figures");
            assert!((figures.flush_mass_g - purge_g).abs() < 1e-6, "{what}: Printability's purge is the time model's: {} g, {purge_g} g", figures.flush_mass_g);
            assert!((res.stats.filament_g.iter().sum::<f64>() - figures.mass_g).abs() < 1e-6, "{what}: the slot grams carry the purge");
            let issue = report.issues.iter().find(|i| i.kind == "purge");
            assert_eq!(issue.is_some(), purge_g > 0.0, "{what}: Printability says so when it purges");
            (res, purge_g)
        };
        // Two nozzles, slots 1 and 3 on the left, 2 and 4 on the right.
        let (two, two_g) = check("two nozzles", &shaped(2, vec![1, 2, 1, 2]));
        let tl = &two.timeline;
        assert!(tl.filament_swaps > 0 && tl.purges == tl.filament_swaps && tl.head_changes > 0, "{} swaps, {} purges, {} head changes", tl.filament_swaps, tl.purges, tl.head_changes);
        assert!(two_g > 0.0 && tl.role_times[makepad_fab_gcode::process::PURGE] > 0.0);
        // The same swaps where the profile says they cost nothing: the purge is all the difference.
        let mut free = shaped(2, vec![1, 2, 1, 2]);
        free.process.values.set_vec("flush_multiplier", vec!["0".into(), "0".into()]);
        let (none, none_g) = check("swaps that cost nothing", &free);
        assert_eq!((none_g, none.timeline.purges, none.timeline.filament_swaps), (0.0, 0, tl.filament_swaps));
        let dt = two.stats.total_time - none.stats.total_time;
        assert!((dt - tl.role_times[makepad_fab_gcode::process::PURGE]).abs() < 1.0, "the print time carries the purge: {dt} s");
        // One nozzle fed from four slots: every change is a swap.
        let (one, one_g) = check("one nozzle, four slots", &shaped(1, vec![1, 1, 1, 1]));
        assert!(one.timeline.purges > tl.purges && one_g > 0.0 && one.timeline.head_changes == 0, "{} purges, {one_g} g, {} head changes; two nozzles {} purges", one.timeline.purges, one.timeline.head_changes, tl.purges);
        // A tool changer: four tools, a filament each.
        let (tools, tools_g) = check("tool changer", &shaped(4, vec![1, 2, 3, 4]));
        assert!(tools.timeline.head_changes > 0 && tools.timeline.filament_swaps == 0 && tools_g == 0.0);
    }

    /// The changes a choice of nozzles is weighed by are the ones the writer prints: on each
    /// nozzle map the plan over the plate's layer sets swaps, switches heads and takes as long
    /// at its changes as the G-code's time model says.
    #[test]
    fn a_nozzle_choice_is_weighed_by_the_changes_the_writer_prints() {
        let mut p = plate();
        p.parts = (0..4).map(|i| part(&format!("p{i}"), i as u32 + 1, None, cube([100.0 + 15.0 * (i % 2) as f64, 100.0 + 15.0 * (i / 2) as f64, 0.0], 3.0 + i as f64))).collect();
        for map in [vec![1, 2, 1, 2], vec![1, 1, 2, 2], vec![2, 1, 1, 1]] {
            let mut s = shaped(2, map.clone(), &["#FFFFFF", "#000000", "#FF0000", "#0000FF"]);
            for (k, v) in [("machine_switch_extruder_time", "5"), ("machine_load_filament_time", "20"), ("machine_unload_filament_time", "15")] {
                s.machine.values.set_str(k, v);
            }
            let res = slice_plate(&s, &p).unwrap();
            let changes = makepad_fab_settings::change::ChangeModel::new(&res.job);
            let plan = makepad_fab_gcode::writer::plan_changes(&makepad_fab_gcode::writer::layer_sets(&res.toolpaths), &changes);
            let tl = &res.timeline;
            assert_eq!((plan.swaps, plan.head_switches), (tl.filament_swaps as usize, tl.head_changes as usize), "{map:?}: the swaps and head switches");
            let charged: f64 = [makepad_fab_gcode::process::HEAD_SWITCH, makepad_fab_gcode::process::FILAMENT_CHANGE, makepad_fab_gcode::process::PURGE].iter().filter_map(|k| tl.role_times.get(*k)).sum();
            assert!((plan.seconds - charged).abs() < 1e-6 * charged.max(1.0), "{map:?}: the changes take {} s planned, {charged} s in the G-code", plan.seconds);
            assert!(plan.swaps > 0 && plan.purge_mm3 > 0.0, "{map:?}: a nozzle swaps");
        }
    }

    /// The flush options print a change's purge into what doesn't show before the printer
    /// purges the rest: the sparse infill, a marked part's inside or the support the new
    /// filament prints first after the change take it, as much as they hold, out of the
    /// printer's purge; every outer wall and skin stays its part's colour.
    #[test]
    fn the_purge_prints_into_what_does_not_show() {
        use makepad_fab_toolpath::Role;
        // One nozzle fed black, white and red, a block of each: every layer swaps.
        let blocks = || Plate { excluded: Vec::new(), settings: Vec::new(), parts: (0..3).map(|i| part(&format!("p{i}"), i as u32 + 1, None, boxed([100.0 + 25.0 * i as f64, 100.0, 0.0], [20.0, 20.0, 4.0]))).collect() };
        let job = |into: [&str; 3]| {
            let mut s = shaped(1, vec![1, 1, 1], &["#000000", "#FFFFFF", "#FF0000"]);
            for (k, v) in ["flush_into_infill", "flush_into_objects", "flush_into_support"].iter().zip(into) {
                s.process.values.set_str(k, v);
            }
            s
        };
        struct Run { purge_mm3: f64, purge_s: f64, laid_mm3: f64, after_change_mm3: f64, other_colour: Vec<Role> }
        let run = |what: &str, s: &JobSetup, p: &Plate, sinks: &[Role]| {
            let res = slice_plate(s, p).unwrap();
            let tl = &res.timeline;
            let area = res.job.filament_area(0);
            let materials = fab_printability::Materials::from_job(&res.job);
            let mut report = fab_printability::Report::default();
            fab_printability::analyse_slice(&fab_plate::machine::Machine::dual_nozzle_350(), p, &materials, res.layers(), tl, &mut report);
            let purge_mm3: f64 = tl.purge_mm.iter().sum::<f64>() * area;
            assert!((report.plate.as_ref().expect("the sliced figures").flush_mass_g - purge_mm3 * 1.24e-3).abs() < 1e-6, "{what}: Printability purges what the time model does");
            // What the new filament lays down after each change before anything that may not
            // take a purge, and what prints in another part's filament.
            let (mut last, mut window, mut after_change_mm3, mut laid_mm3) = (0u8, false, 0.0, 0.0);
            let mut other_colour = Vec::new();
            for m in tl.moves().filter(|m| m.kind == MoveKind::Extrude && m.e > 0.0 && m.object != u16::MAX) {
                let v = m.e as f64 * area;
                laid_mm3 += v;
                if m.filament != last {
                    window = last != 0;
                    last = m.filament;
                }
                window &= sinks.contains(&m.role);
                if window {
                    after_change_mm3 += v;
                }
                let own = p.parts[m.object as usize].filament as u8;
                if m.filament != own {
                    assert!(sinks.contains(&m.role), "{what}: part {} prints its {:?} in filament {}", m.object, m.role, m.filament);
                    other_colour.push(m.role);
                }
            }
            Run { purge_mm3, purge_s: tl.role_times.get(makepad_fab_gcode::process::PURGE).copied().unwrap_or(0.0), laid_mm3, after_change_mm3, other_colour }
        };
        let plate = blocks();
        let none = run("no flush options", &job(["0", "0", "0"]), &plate, &[]);
        assert!(none.purge_mm3 > 0.0 && none.other_colour.is_empty());
        // Into the sparse infill: the parts' own after their change, then the later parts'.
        let infill = run("into infill", &job(["1", "0", "0"]), &plate, &[Role::SparseInfill]);
        let moved = none.purge_mm3 - infill.purge_mm3;
        assert!(moved > 0.0 && infill.purge_s < none.purge_s, "the purge shrinks: {:.0} mm³ of {:.0}", infill.purge_mm3, none.purge_mm3);
        assert!(infill.after_change_mm3 >= 0.9 * moved, "the infill after the changes takes it: {:.0} mm³ printed, {moved:.0} mm³ moved", infill.after_change_mm3);
        assert!((infill.laid_mm3 - none.laid_mm3).abs() < 1e-3 * none.laid_mm3, "the parts print the same: {:.0} mm³, {:.0} mm³", infill.laid_mm3, none.laid_mm3);
        assert!(infill.other_colour.contains(&Role::SparseInfill), "a later part's infill takes the purge");
        // Into a marked part: the red one's inside, and nobody else's infill.
        let mut marked = blocks();
        marked.parts[2].print = vec![("flush_into_objects".into(), "1".into())];
        let inside = [Role::InnerWall, Role::SparseInfill, Role::InternalSolidInfill, Role::InternalBridge];
        let object = run("into a marked part", &job(["0", "0", "0"]), &marked, &inside);
        let moved = none.purge_mm3 - object.purge_mm3;
        assert!(moved > 0.0 && object.after_change_mm3 >= 0.9 * moved, "{moved:.0} mm³ moved, {:.0} mm³ after the changes", object.after_change_mm3);
        assert!(!object.other_colour.is_empty(), "the red part's inside prints in the others' filaments");
        // Into support: a white table over a black block.
        let mut table = boxed([150.0, 100.0, 0.0], [4.0, 4.0, 3.0]);
        let top = boxed([142.0, 92.0, 3.0], [20.0, 20.0, 2.0]);
        let b = table.positions.len() as u32;
        table.positions.extend(top.positions);
        table.triangles.extend(top.triangles.iter().map(|t| [t[0] + b, t[1] + b, t[2] + b]));
        let supported = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("block", 1, None, boxed([100.0, 100.0, 0.0], [20.0, 20.0, 6.0])), part("table", 2, None, table)] };
        let with_support = |into: &str| {
            let mut s = job(["0", "0", into]);
            s.process.values.set_str("enable_support", "1");
            s
        };
        let support = [Role::Support, Role::SupportTransition];
        let kept = run("support kept", &with_support("0"), &supported, &[]);
        let into = run("into support", &with_support("1"), &supported, &support);
        let moved = kept.purge_mm3 - into.purge_mm3;
        assert!(moved > 0.0 && into.after_change_mm3 >= 0.9 * moved, "{moved:.0} mm³ moved, {:.0} mm³ of support after the changes", into.after_change_mm3);
    }

    /// The tower is optional: without it the colour changes stay and no tower move is left. An
    /// extruder coming back pushes out what it drew back when it left, and the printer is told
    /// how much that is; an extruder that has not printed yet has drawn nothing back.
    #[test]
    fn without_a_tower_an_extruder_coming_back_pushes_its_filament_out() {
        let mut s = setup(vec![1, 2]);
        s.machine.values.set_vec("retract_length_toolchange", vec!["2".into(), "2".into()]);
        s.machine.values.set_str("change_filament_gcode", "; drawn back {new_extruder_retracted_length}\nT[next_filament_id]\n");
        s.process.values.set_str("enable_prime_tower", "1");
        let on = slice_plate(&s, &plate()).unwrap();
        s.process.values.set_str("enable_prime_tower", "0");
        let off = slice_plate(&s, &plate()).unwrap();
        let tower = |r: &SliceResult| r.timeline.moves().filter(|m| format!("{:?}", m.role) == "PrimeTower").count();
        assert!(tower(&on) > 0, "two nozzles print a tower when it is on");
        assert_eq!(tower(&off), 0, "and none when it is off");
        assert!(!off.gcode.contains("; WIPE_TOWER") && !off.gcode.contains("Prime tower"));
        // The writer follows each tool command with the changes left.
        let changes = |r: &SliceResult| r.gcode.lines().filter(|l| l.starts_with("M73 E")).count();
        assert_eq!(changes(&off), changes(&on), "the same colour changes");
        assert!(changes(&off) > 2, "{}", changes(&off));
        assert_eq!((off.timeline.head_changes as usize, off.timeline.filament_swaps), (changes(&off), 0), "a nozzle each swaps nothing");
        let lines: Vec<&str> = off.gcode.lines().collect();
        let told: Vec<&str> = lines.iter().filter_map(|l| l.strip_prefix("; drawn back ")).collect();
        assert_eq!(told.len(), changes(&off));
        assert_eq!(told[0], "0", "the other extruder has not printed yet");
        assert!(told[1..].iter().all(|r| *r == "2"), "{told:?}");
        for (k, (i, _)) in lines.iter().enumerate().filter(|(_, l)| l.starts_with("M73 E")).enumerate().skip(1) {
            let first = lines[i..].iter().find(|l| l.starts_with("G1") && l.contains(" E")).unwrap_or_else(|| panic!("line {i}: nothing printed after change {k}"));
            assert!(first.starts_with("G1 E2 "), "change {k}, line {i}: the push back comes first, not {first}");
        }
    }

    #[test]
    fn plate_contours_cut_each_part() {
        let src = PlateContours::new(&plate());
        let regions = src.layer(5.0);
        assert_eq!(regions.len(), 2);
        assert_eq!((regions[0].part, regions[0].filament, regions[0].nozzle), (0, 1, Some(0)));
        assert_eq!((regions[1].part, regions[1].filament), (1, 2));
        let a: i128 = regions[0].loops.iter().map(|l| l.area2()).sum();
        let mm2 = a as f64 / 2.0 / (pc::UNITS_PER_MM * pc::UNITS_PER_MM) as f64;
        assert!((mm2 - 100.0).abs() < 1e-3, "{mm2}");
        assert!(src.layer(10.5).is_empty());
    }

    #[test]
    fn plate_slices_with_part_indices_and_nozzles() {
        let res = slice_plate(&setup(vec![1, 2]), &plate()).unwrap();
        let ext: Vec<_> = res.timeline.moves().filter(|m| m.kind == MoveKind::Extrude && m.object != u16::MAX && m.layer > 0).collect();
        assert!(!ext.is_empty());
        for m in &ext {
            // Part 0 is on the left (x 100..110), part 1 on the right.
            let x = m.p1[0];
            let part = if x < 150.0 { 0 } else { 1 };
            if m.object != part {
                let l = m.gcode_line as usize;
                let lines: Vec<&str> = res.gcode.lines().collect();
                panic!("record at x {x} line {l}:\n{}", lines[l.saturating_sub(30)..(l + 3).min(lines.len())].join("\n"));
            }
            assert_eq!(m.nozzle as u16, part, "record at x {x}");
            assert_eq!(m.tool as u16, 1 - part, "firmware tool at x {x}");
        }
        // The same plate through the plate contour source.
        let via = slice_source(&setup(vec![1, 2]), &plate(), Arc::new(PlateContours::new(&plate()))).unwrap();
        assert_eq!(via.stats.layers, res.stats.layers);
        let n = |r: &SliceResult| r.timeline.moves().filter(|m| m.kind == MoveKind::Extrude).count();
        assert_eq!(n(&via), n(&res));
        // Without machine limits in the profile the estimate stays sane.
        assert!(res.stats.total_time > 10.0 && res.stats.total_time < 24.0 * 3600.0, "{} {:?}", res.stats.total_time, res.timeline.role_times);
    }

    fn boxed(min: [f64; 3], size: [f64; 3]) -> PartMesh {
        let positions = (0..8u32).map(|i| [min[0] + size[0] * (i & 1) as f64, min[1] + size[1] * ((i >> 1) & 1) as f64, min[2] + size[2] * ((i >> 2) & 1) as f64]).collect();
        let triangles = vec![[0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4], [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5]];
        PartMesh { positions, triangles }
    }

    /// Every arc the writer emits for tree supports sweeps outside the
    /// model: an arc written the wrong way round would drag the nozzle
    /// through the part (a cap on a stem, arcs on, tree supports).
    #[test]
    fn tree_support_arcs_stay_outside_the_model() {
        let mut stem = boxed([96.0, 96.0, 0.0], [8.0, 8.0, 8.0]);
        let cap = boxed([88.0, 88.0, 8.0], [24.0, 24.0, 3.0]);
        let b = stem.positions.len() as u32;
        stem.positions.extend(cap.positions);
        stem.triangles.extend(cap.triangles.iter().map(|t| [t[0] + b, t[1] + b, t[2] + b]));
        let mut s = setup(vec![1]);
        s.process.values.set_str("enable_support", "1");
        s.process.values.set_str("support_type", "tree(auto)");
        s.process.values.set_str("enable_arc_fitting", "1");
        let res = slice_plate(&s, &Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("t", 1, None, stem)] }).unwrap();
        // The model's regions per print z, a little inset (a support bead
        // touching the XY gap's edge is not inside).
        let model: Vec<(f64, Vec<ExPolygon>)> = res
            .layers()
            .iter()
            .map(|l| {
                let r: Vec<ExPolygon> = l.objects.iter().filter(|o| !o.support_only).flat_map(|o| o.slices.iter().cloned()).collect();
                (l.print_z, makepad_fab_slicer::offset::offset_ex(&r, -0.05 * SCALE, makepad_fab_slicer::offset::Join::Miter(3.0)))
            })
            .collect();
        let word = |l: &str, c: char| l.split_whitespace().find_map(|w| w.strip_prefix(c).and_then(|v| v.parse::<f64>().ok()));
        let (mut x, mut y, mut z) = (0.0, 0.0, 0.0);
        let mut support = false;
        let mut arcs = 0;
        for (n, l) in res.gcode.lines().enumerate() {
            if let Some(f) = l.strip_prefix("; FEATURE: ") {
                support = f.starts_with("Support");
            }
            let g = l.split_whitespace().next().unwrap_or("");
            if !matches!(g, "G0" | "G1" | "G2" | "G3") {
                continue;
            }
            let (nx, ny) = (word(l, 'X').unwrap_or(x), word(l, 'Y').unwrap_or(y));
            z = word(l, 'Z').unwrap_or(z);
            if support && (g == "G2" || g == "G3") && word(l, 'E').is_some_and(|e| e > 0.0) {
                arcs += 1;
                let (cx, cy) = (x + word(l, 'I').unwrap_or(0.0), y + word(l, 'J').unwrap_or(0.0));
                let a0 = (y - cy).atan2(x - cx);
                let mut a1 = (ny - cy).atan2(nx - cx);
                let r = (x - cx).hypot(y - cy);
                // G3 counter-clockwise, G2 clockwise.
                if g == "G3" {
                    while a1 <= a0 { a1 += std::f64::consts::TAU; }
                } else {
                    while a1 >= a0 { a1 -= std::f64::consts::TAU; }
                }
                let layer = model.iter().min_by(|p, q| (p.0 - z).abs().total_cmp(&(q.0 - z).abs())).map(|m| &m.1);
                for k in 0..=32 {
                    let a = a0 + (a1 - a0) * k as f64 / 32.0;
                    let q = Pt::from_mm(cx + r * a.cos(), cy + r * a.sin());
                    assert!(!layer.is_some_and(|m| m.iter().any(|e| e.contains(q))), "line {}: `{l}` sweeps through the model at z {z} ({:.3}, {:.3})", n + 1, q.x_mm(), q.y_mm());
                }
            }
            (x, y) = (nx, ny);
        }
        assert!(arcs > 0, "the fixture's tree supports print arcs");
    }

    /// A small round post against a wall, far out on the bed, with every bead asked to run
    /// faster than the filament's flow limit allows (so each runs at the limit): the live
    /// check, which reads the toolpaths' own records before any G-code, finds no move over the
    /// limit. Offsetting the round post leaves segments a few tens of nm long; out there an f32
    /// position resolves only about 15 nm, so a record whose filament followed the toolpath's
    /// exact length, not the length its endpoints keep, read up to half as much flow again.
    #[test]
    fn a_post_against_a_wall_runs_within_the_flow_limit_in_the_live_check() {
        let (centre, r, h, sides) = ([246.0, 184.0], 2.5, 4.0, 48);
        let mut mesh = boxed([236.0, 180.0, 0.0], [20.0, 2.0, h]);
        let b = mesh.positions.len() as u32;
        mesh.positions.extend([[centre[0], centre[1], 0.0], [centre[0], centre[1], h]]);
        for i in 0..sides {
            let a = std::f64::consts::TAU * i as f64 / sides as f64;
            let (x, y) = (centre[0] + r * a.cos(), centre[1] + r * a.sin());
            mesh.positions.extend([[x, y, 0.0], [x, y, h]]);
        }
        for i in 0..sides as u32 {
            let j = (i + 1) % sides as u32;
            let (b0, t0, b1, t1) = (b + 2 + 2 * i, b + 3 + 2 * i, b + 2 + 2 * j, b + 3 + 2 * j);
            mesh.triangles.extend([[b, b1, b0], [b + 1, t0, t1], [b0, b1, t1], [b0, t1, t0]]);
        }
        let mut s = setup(vec![1]);
        for key in ["outer_wall_speed", "inner_wall_speed", "sparse_infill_speed", "internal_solid_infill_speed", "top_surface_speed", "gap_infill_speed"] {
            s.process.values.set_str(key, "1000");
        }
        s.filaments[0].values.set_str("filament_max_volumetric_speed", "18");
        let plate = Plate { excluded: Vec::new(), settings: Vec::new(), parts: vec![part("post", 1, None, mesh)] };
        let res = slice_plate(&s, &plate).unwrap();
        let paths = || res.layers().iter().flat_map(|l| &l.objects).flat_map(|o| &o.islands).flat_map(|i| i.walls.iter().chain(&i.fills));
        assert!(paths().any(|p| p.area * p.speed > 18.0 * 0.999), "the beads run at the flow limit");
        let job = &res.job;
        let mut records = res.layers().iter().enumerate().flat_map(|(i, l)| l.objects.iter().flat_map(move |o| crate::live::layer_records(i, l.print_z, o, job)));
        let mut report = fab_printability::Report::default();
        let materials = fab_printability::Materials::from_job(&res.job);
        fab_printability::analyse_slice_with(&fab_plate::machine::Machine::dual_nozzle_350(), &plate, &materials, res.layers(), &makepad_fab_gcode::process::Timeline::default(), &mut records, &mut report, &mut |_, _, _| true);
        let over: Vec<&str> = report.issues.iter().filter(|i| i.kind == "flow").map(|i| i.message.as_str()).collect();
        assert!(over.is_empty(), "the live check reads a move over the flow limit: {over:?}");
    }

    #[test]
    fn nozzle_conflict_is_an_error() {
        let e = slice_plate(&setup(vec![2, 2]), &plate()).err().unwrap();
        assert!(e.contains("must print on nozzle 1"), "{e}");
    }
}

/// The support channels and regions' print settings of the parts one slice reads, made once per
/// paint and frame: an assembly's parts carry their object's paint in one frame, so they share
/// its regions, and with them each outline a region is read as
/// ([`makepad_fab_slicer::source::RegionOutlines`]): the car's support blocks were contoured on
/// every layer of each of its eight parts, seven of them the same outlines.
#[derive(Default)]
pub struct PaintRegions(std::sync::Mutex<std::collections::HashMap<(u64, Vec<u64>), PartRegions>>);

type PartRegions = (Option<makepad_fab_slicer::source::SupportPaint>, Vec<makepad_fab_slicer::source::SettingsZone>);

/// A part's source as its paint by position (`Part::paint`) has it: split by the paint when a
/// colour prints in another tool than `own` (1-based; the job set each colour's tool), and
/// carrying its support channels and regions' print settings. `to_part` takes the source's points to the plate's, stretching
/// a length by at most `stretch`; `from_part` is its inverse. `shared`: the regions of the parts
/// sliced with it, and the numbers `to_part` is made of (`from_part` and `stretch` follow from
/// them): a part whose paint has the same content in the same frame takes the regions made for the
/// first.
pub fn painted(p: &Part, source: Arc<dyn ContourSource + Send + Sync>, own: u32, to_part: impl Fn([f64; 3]) -> [f64; 3] + Send + Sync + 'static, from_part: impl Fn([f64; 3]) -> [f64; 3], stretch: f64, shared: Option<(&PaintRegions, &[f64])>) -> Arc<dyn ContourSource + Send + Sync> {
    let Some(sp) = p.paint.as_ref() else { return source };
    let to_part = Arc::new(to_part);
    let states = sp.states(own);
    let source: Arc<dyn ContourSource + Send + Sync> = if states.iter().all(|&s| s == 0) {
        source
    } else {
        let (sample_at, to_part) = (sp.sample_at.clone(), to_part.clone());
        let paint: makepad_fab_slicer::source::PaintAt = Arc::new(move |x, y, z| {
            let s = sample_at(to_part([x, y, z]));
            let state = |slot: u8| states.get(slot as usize).copied().unwrap_or(0);
            let here = state(s.slot);
            // a blend of two colours that print in one tool prints in it
            let mix = s.mix.map(|(o, share)| (state(o), share)).filter(|(o, _)| *o != here);
            makepad_fab_slicer::source::PaintHere { state: here, dist: s.dist / stretch, mix }
        });
        Arc::new(makepad_fab_slicer::source::PaintedSource { inner: source, paint })
    };
    if sp.support_block.is_none() && sp.support_enforce.is_none() && sp.no_trees.is_none() && sp.settings.is_empty() {
        return source;
    }
    let region = |r: &fab_plate::SupportRegion| {
        let (at, to_part) = (r.at.clone(), to_part.clone());
        let bounds = r.bounds.map(|(lo, hi)| {
            let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
            for corner in 0..8 {
                let q = from_part(std::array::from_fn(|k| if corner & (1 << k) == 0 { lo[k] } else { hi[k] }));
                for k in 0..3 {
                    min[k] = min[k].min(q[k]);
                    max[k] = max[k].max(q[k]);
                }
            }
            (min, max)
        });
        makepad_fab_slicer::source::SupportRegion::new(Arc::new(move |x, y, z| at(to_part([x, y, z])) / stretch), bounds)
    };
    let made = || -> PartRegions {
        let channels = sp.support_block.is_some() || sp.support_enforce.is_some() || sp.no_trees.is_some();
        let paint = channels.then(|| makepad_fab_slicer::source::SupportPaint { block: sp.support_block.as_ref().map(region), enforce: sp.support_enforce.as_ref().map(region), no_trees: sp.no_trees.as_ref().map(region) });
        let zones = sp.settings.iter().map(|z| makepad_fab_slicer::source::SettingsZone { region: region(&z.region), values: z.values.clone() }).collect();
        (paint, zones)
    };
    // The regions' key holds their paint, settings and frame on the printer, the part's colours
    // aside (0: none, nothing to share).
    let (paint, zones) = match shared.filter(|_| sp.regions_key != 0) {
        Some((regions, frame)) => regions.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry((sp.regions_key, frame.iter().map(|v| v.to_bits()).collect())).or_insert_with(made).clone(),
        None => made(),
    };
    Arc::new(SupportPainted { inner: source, paint, zones })
}

/// A source with support channels or regions' print settings: it cuts as its `inner` does, and
/// plans on its own (its copies' channels are their own).
struct SupportPainted {
    inner: Arc<dyn ContourSource + Send + Sync>,
    paint: Option<makepad_fab_slicer::source::SupportPaint>,
    zones: Vec<makepad_fab_slicer::source::SettingsZone>,
}

impl ContourSource for SupportPainted {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.inner.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.inner.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.inner.contours(zs)
    }
    fn painted(&self) -> bool {
        self.inner.painted()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &makepad_fab_slicer::paint::PaintParams) -> Vec<makepad_fab_slicer::paint::LayerPaint> {
        self.inner.paint_regions(zs, slices, p)
    }
    fn paint_at(&self) -> Option<makepad_fab_slicer::source::PaintAt> {
        self.inner.paint_at()
    }
    fn support_paint(&self) -> Option<makepad_fab_slicer::source::SupportPaint> {
        self.paint.clone()
    }
    fn settings_zones(&self) -> Vec<makepad_fab_slicer::source::SettingsZone> {
        self.zones.clone()
    }
    fn np_plan(&self) -> Option<Arc<makepad_fab_slicer::nonplanar::NpPlan>> {
        self.inner.np_plan()
    }
    fn slope_profile(&self, step: f64) -> Option<makepad_fab_slicer::source::SlopeProfile> {
        self.inner.slope_profile(step)
    }
    fn embeds(&self) -> Vec<makepad_fab_slicer::embed::Embed> {
        self.inner.embeds()
    }
    fn cavity(&self, z: f64) -> Vec<ExPolygon> {
        self.inner.cavity(z)
    }
}

/// A shared source as an object's own.
pub struct Shared(pub Arc<dyn ContourSource + Send + Sync>);

impl ContourSource for Shared {
    fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        self.0.bounds()
    }
    fn pivot_box(&self) -> ([f64; 2], [f64; 2]) {
        self.0.pivot_box()
    }
    fn contours(&self, zs: &[f64]) -> Vec<Vec<ExPolygon>> {
        self.0.contours(zs)
    }
    fn painted(&self) -> bool {
        self.0.painted()
    }
    fn mesh(&self) -> Option<&Mesh> {
        self.0.mesh()
    }
    fn paint_regions(&self, zs: &[f64], slices: &[Vec<ExPolygon>], p: &makepad_fab_slicer::paint::PaintParams) -> Vec<makepad_fab_slicer::paint::LayerPaint> {
        self.0.paint_regions(zs, slices, p)
    }
    fn paint_at(&self) -> Option<makepad_fab_slicer::source::PaintAt> {
        self.0.paint_at()
    }
    fn support_paint(&self) -> Option<makepad_fab_slicer::source::SupportPaint> {
        self.0.support_paint()
    }
    fn settings_zones(&self) -> Vec<makepad_fab_slicer::source::SettingsZone> {
        self.0.settings_zones()
    }
    fn np_plan(&self) -> Option<Arc<makepad_fab_slicer::nonplanar::NpPlan>> {
        self.0.np_plan()
    }
    fn layer_stack(&self) -> Option<Arc<[(f64, f64)]>> {
        self.0.layer_stack()
    }
    fn slope_profile(&self, step: f64) -> Option<makepad_fab_slicer::source::SlopeProfile> {
        self.0.slope_profile(step)
    }
    fn embeds(&self) -> Vec<makepad_fab_slicer::embed::Embed> {
        self.0.embeds()
    }
    fn cavity(&self, z: f64) -> Vec<ExPolygon> {
        self.0.cavity(z)
    }
}
