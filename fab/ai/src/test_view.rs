//! What the views and the checks read of test prints: the pieces as the parts of a plate of their
//! own, cut and closed, where they print or where they are. Derived from the plate each time, and
//! nothing of it is edited or sliced.

use std::sync::Arc;

use fab_model::eval::{affine_mul, Evaluated, PartField, RenderMesh, ShownPiece};
use fab_model::project::TestEdit;
use fab_plate::PartMesh;
use fab_sdf_engine::{cut_shape, Cut};
use makepad_fab_slice::live::LiveInput;
use makepad_csg_graph::TriMesh;
use makepad_csg_math::dvec3;
use makepad_csg_sdf::SdfShape;
use makepad_fab_slice::piece;

use makepad_model::json::Value;

use crate::surface::{Surface, Tracer};
use crate::{Camera, Reply, RenderRequest, Session};

/// How much a cut mesh's corners round: as the view's meshes do.
const CREASE: f64 = 40.0;

/// The tint of what a test print cuts, on the plate it is cut from.
const PIECE_TINT: [f32; 3] = [0.95, 0.55, 0.1];

/// The plate of these test prints' pieces: standing where they print, or `on_bed` false, in place.
/// One part for each part the plan reaches, in its order: what the test plate slices.
pub fn plate(ev: &Evaluated, tests: &[piece::TestPrint], plan: &piece::TestPlan, on_bed: bool) -> Evaluated {
    let pieces = piece::shown_parts(&ev.plate, tests, plan, on_bed).into_iter().map(|s| {
        let from = ev.fields.get(s.of).and_then(Option::as_ref);
        let field = from.and_then(|f| cut_field(f, &s.cube));
        // A mesh part shows its piece's mesh; a field part draws from its field, or in the Mesh
        // look shows its display mesh, cut.
        let mesh = match (from, ev.view.meshes, ev.render.get(s.of)) {
            (None, _, _) => Some(s.part.mesh.clone()),
            (Some(_), true, Some(shown)) => Some(piece::shown_mesh(&PartMesh {
                positions: shown.positions.iter().map(|q| q.map(f64::from)).collect(),
                triangles: shown.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
            }, &s.cube, &s.to_bed)),
            _ => None,
        };
        let render = mesh.map_or_else(RenderMesh::default, |m| {
            let m = fab_loose::mesh::creased(&m.positions, &m.triangles, CREASE);
            RenderMesh { positions: m.positions, normals: m.normals, indices: m.indices }
        });
        ShownPiece { of: s.of, part: s.part, render, field, to_bed: s.to_bed }
    }).collect();
    ev.of_pieces(pieces)
}

/// What `cube` leaves of field `f`, in the field's frame: none when it cuts all of it away.
fn cut_field(f: &PartField, cube: &piece::Cube) -> Option<SdfShape> {
    let (shape, _, to_printer, _) = f.distance();
    match cut_shape(shape, &cube.planes_in(to_printer)) {
        Cut::Whole => Some((**shape).clone()),
        Cut::Gone => None,
        Cut::Left(cut) => Some(cut),
    }
}

/// The sides of a test print's box by word, as [`piece::Cube::sides`] lists them.
const FACES: [&str; 6] = ["right", "left", "back", "front", "top", "bottom"];

/// How far from its piece's surface a point given on it may be (plate mm).
const ON_PIECE: f64 = 0.5;

/// These test prints' pieces as surfaces to trace, each named after its test print: standing
/// where they print, or `on_bed` false, in place. A field part's piece is its cut field.
fn piece_tracer(ev: &Evaluated, shown: Vec<piece::ShownPart>) -> Tracer {
    const SAME: [[f64; 4]; 3] = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
    Tracer::of(shown.into_iter().filter_map(|s| match (ev.fields.get(s.of).and_then(Option::as_ref), s.part.field_bounds) {
        (Some(f), _) => Some((s.part.name, affine_mul(&s.to_bed, f.distance().2), Surface::Field(cut_field(f, &s.cube)?))),
        (None, Some(_)) => None,
        (None, None) => {
            let vertices = s.part.mesh.positions.iter().map(|p| dvec3(p[0], p[1], p[2])).collect();
            Some((s.part.name, SAME, Surface::Mesh(TriMesh { vertices, triangles: s.part.mesh.triangles, tags: Vec::new(), corner_normals: Vec::new() })))
        }
    }).collect())
}

/// Whether side `side` of `cube` cuts any of what it reaches (`shown`, in place): the piece has a
/// cut face there.
fn cuts(ev: &Evaluated, shown: &[piece::ShownPart], cube: &piece::Cube, side: usize) -> bool {
    const GRID: usize = 33;
    let (n, d) = cube.sides()[side];
    let (k, axes) = (side / 2, cube.axes());
    let along = |j: usize, by: f64| -> [f64; 3] { std::array::from_fn(|r| axes[r][j] * by) };
    let (u, w) = ((k + 1) % 3, (k + 2) % 3);
    let on_side = |g: usize| -> [f64; 3] {
        let at = |i: usize, j: usize| ((i as f64 + 0.5) / GRID as f64 - 0.5) * cube.size[j];
        let (a, b) = (along(u, at(g % GRID, u)), along(w, at(g / GRID, w)));
        std::array::from_fn(|r| cube.center[r] + n[r] * cube.size[k] / 2.0 + a[r] + b[r])
    };
    shown.iter().any(|s| match (ev.fields.get(s.of).and_then(Option::as_ref), s.part.field_bounds) {
        (Some(f), _) => {
            let (shape, _, to_printer, _) = f.distance();
            let back = fab_model::weave::invert(to_printer);
            (0..GRID * GRID).any(|g| {
                let q = fab_model::params::xf_point(&back, on_side(g));
                makepad_csg_sdf::distance(&shape.root, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) < 0.0
            })
        }
        (None, Some(_)) => false,
        (None, None) => s.part.mesh.positions.iter().any(|p| (n[0] * p[0] + n[1] * p[1] + n[2] * p[2] - d).abs() < 1e-6),
    })
}

/// Sheets of a plate of pieces: what `fab.render` asks the app to draw.
pub struct PiecesJob {
    pub ev: Arc<Evaluated>,
    pub views: Vec<String>,
    pub extra: Option<Arc<crate::supports::ColoredMesh>>,
    pub camera: Camera,
}

impl PartialEq for PiecesJob {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl std::fmt::Debug for PiecesJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PiecesJob({} parts, views {:?})", self.ev.plate.parts.len(), self.views)
    }
}

/// The ticked test prints' pieces on the bed: what the checks read while the Slicer slices them.
pub(crate) struct TestBed {
    /// The plate they are cut from ([`Evaluated::content`]).
    content: [u8; 32],
    tests: Vec<piece::TestPrint>,
    plan: piece::TestPlan,
    ev: Arc<Evaluated>,
}

impl Session {
    /// The project's test prints that `wanted` picks, where the plate `ev` has them.
    fn cut_tests(&self, ev: &Evaluated, wanted: &dyn Fn(&fab_model::project::TestPrint) -> bool) -> Vec<piece::TestPrint> {
        self.model.project().test_prints.iter().filter(|t| wanted(t)).filter_map(|t| {
            let placed = ev.test_print_box(t)?;
            Some(piece::TestPrint { name: t.name.clone(), cube: piece::Cube { center: placed.center, size: placed.size, turn: placed.turn }, pose: ev.test_print_lay(t), brim: t.brim })
        }).collect()
    }

    /// What the Slicer slices now, as a plate: the committed one, or the ticked test prints' pieces
    /// on the bed.
    pub(crate) fn slicer_plate(&mut self) -> Result<Arc<Evaluated>, String> {
        let whole = self.last.clone().ok_or("nothing is built yet")?;
        if !self.test_mode {
            return Ok(whole);
        }
        let tests = self.cut_tests(&whole, &|t| t.on);
        if tests.is_empty() {
            return Err("the Slicer slices test prints and none is ticked: tick one with fab.test_print, or {action: \"mode\", tests: false} for the whole plate".into());
        }
        if let Some(bed) = self.test_bed.as_ref().filter(|b| b.content == whole.content && b.tests == tests && b.ev.machine.same_volume(&whole.machine) && b.ev.view == whole.view) {
            return Ok(bed.ev.clone());
        }
        let plan = piece::test_plan(&whole.plate, &whole.machine, &tests)?;
        let ev = Arc::new(plate(&whole, &tests, &plan, true));
        self.test_bed = Some(TestBed { content: whole.content, tests, plan, ev: ev.clone() });
        Ok(ev)
    }

    /// The live input that slices `ev`: a plate's own, or for the plate of [`Self::slicer_plate`]
    /// its test prints', cut from the committed plate.
    pub(crate) fn slice_input(&self, source: &dyn crate::supports::SliceSource, ev: &Evaluated) -> Result<(LiveInput, String), String> {
        match (self.test_bed.as_ref().filter(|b| std::ptr::eq(&*b.ev, ev)), &self.last) {
            (Some(bed), Some(whole)) => {
                let (input, name) = source.input(whole)?;
                Ok((piece::planned_input(&input, &bed.tests, &bed.plan), name))
            }
            _ => source.input(ev),
        }
    }

    /// The lay of test print `name` that fab.test_print's lay asks for, on the plate `ev`: one of
    /// `normal`, `face` (a side of its box that cuts) or `point` (on its piece), as its part stands
    /// on the plate.
    pub(crate) fn lay_edit(&self, ev: &Evaluated, args: &Value) -> Result<TestEdit, String> {
        let name = crate::str_arg(args, "name").ok_or("missing 'name': the test print whose piece to lay")?;
        let tests = self.cut_tests(ev, &|t| t.name == name);
        let Some(t) = tests.first() else { return Err(self.no_test(name)) };
        let given: Vec<&str> = ["normal", "face", "point"].into_iter().filter(|k| args.get(k).is_some()).collect();
        let shown = || -> Result<Vec<piece::ShownPart>, String> { Ok(piece::shown_parts(&ev.plate, &tests, &piece::test_plan(&ev.plate, &ev.machine, &tests)?, false)) };
        let normal = match given[..] {
            ["normal"] => crate::point_arg::<3>(args.get("normal").unwrap_or(&Value::Null)).map_err(|_| "'normal' is a plate direction [x, y, z] on the piece as its part stands, as printed".to_string())?,
            ["face"] => {
                let word = crate::str_arg(args, "face").unwrap_or_default();
                let side = FACES.iter().position(|f| *f == word).ok_or_else(|| format!("'face' is a side of the box: one of {}", FACES.join(", ")))?;
                if !cuts(ev, &shown()?, &t.cube, side) {
                    return Err(format!("test print '{name}': its box's {word} side cuts nothing, so the piece has no cut face there: give another side, a normal, or a point on the piece"));
                }
                t.cube.sides()[side].0
            }
            ["point"] => {
                let p = crate::point_arg::<3>(args.get("point").unwrap_or(&Value::Null)).map_err(|_| "'point' is a plate point [x, y, z] mm on the piece as its part stands".to_string())?;
                let found = piece_tracer(ev, shown()?).nearest(p);
                let Some((_, _, normal, off)) = found else { return Err(format!("test print '{name}': its box cuts nothing of the plate")) };
                if off > ON_PIECE {
                    return Err(format!("test print '{name}': {p:?} is {off:.1} mm from its piece: give a point on it (fab.render {{test_print: [{name:?}]}} shows it, fab.pick reads a point), or a face"));
                }
                normal
            }
            _ => return Err("lay takes one of normal (a plate direction on the piece), face (bottom, top, front, back, left, right: a side of its box) or point (on the piece, plate mm)".into()),
        };
        Ok(TestEdit::LayFlat { name: name.to_string(), normal })
    }

    /// The lay a click on a test print's piece asks for: the plate ray `from + t dir` (mm) on the
    /// pieces as the view shows them, test print `name`'s in place or, with none, the ticked ones
    /// where they print.
    pub fn lay_by_ray(&self, name: Option<&str>, from: [f64; 3], dir: [f64; 3]) -> Result<TestEdit, String> {
        let ev = self.last.clone().ok_or("nothing is built yet")?;
        let tests = self.cut_tests(&ev, &|t| name.map_or(t.on, |n| t.name == n));
        if tests.is_empty() {
            return Err(name.map_or_else(|| "Lay flat: no test print is ticked".to_string(), |n| self.no_test(n)));
        }
        let plan = piece::test_plan(&ev.plate, &ev.machine, &tests)?;
        let tracer = piece_tracer(&ev, piece::shown_parts(&ev.plate, &tests, &plan, name.is_none()));
        let hit = tracer.hit(from, dir).ok_or("Lay flat: click the piece's surface or a cut face")?;
        let normal = tracer.plate_normal(hit.part, hit.normal).ok_or("Lay flat: the piece has no face there: click inside a face")?;
        let t = tests.iter().find(|t| t.name == hit.object).ok_or("Lay flat: click the piece's surface or a cut face")?;
        // Where it prints it is turned by its pose: back to as its part stands.
        let normal = match (name, t.pose) {
            (None, Some(r)) => std::array::from_fn(|c| (0..3).map(|k| r[k][c] * normal[k]).sum()),
            _ => normal,
        };
        Ok(TestEdit::LayFlat { name: hit.object, normal })
    }

    fn no_test(&self, name: &str) -> String {
        let all: Vec<&str> = self.model.project().test_prints.iter().map(|t| t.name.as_str()).collect();
        format!("no test print '{name}': the test prints are [{}]", all.join(", "))
    }

    /// `fab.render` of test prints: `names`' pieces alone, cut and closed; with none, what the
    /// Slicer slices, on the bed.
    pub(crate) fn render_pieces(&mut self, names: &[String], args: &Value, views: Vec<String>, camera: Camera) -> Result<Reply, String> {
        let flag = |k: &str| args.get(k).and_then(Value::as_bool) == Some(true);
        let whole = self.last.clone().ok_or("nothing is built yet")?;
        let render = if names.is_empty() {
            let ev = self.slicer_plate()?;
            let extra = crate::supports::render_extra(self, &ev, &[], flag("supports"), &[], &[])?;
            RenderRequest::Pieces(Arc::new(PiecesJob { ev, views, extra, camera }))
        } else {
            let tests = self.cut_tests(&whole, &|t| names.contains(&t.name));
            if let Some(absent) = names.iter().find(|n| !tests.iter().any(|t| t.name == **n)) {
                return Err(self.no_test(absent));
            }
            let plan = piece::test_plan(&whole.plate, &whole.machine, &tests)?;
            if flag("with_plate") {
                let mut mesh = crate::supports::ColoredMesh::default();
                for (t, of) in tests.iter().zip(&plan.reached) {
                    let sides = t.cube.sides();
                    for &k in of {
                        let frame = crate::volumes::pick_frame(&whole, k);
                        let inside = |q: [f64; 3]| {
                            let at = fab_model::params::xf_point(&frame, q);
                            sides.iter().map(|(n, d)| n[0] * at[0] + n[1] * at[1] + n[2] * at[2] - d).fold(f64::NEG_INFINITY, f64::max)
                        };
                        self.tint_surface(&whole, k, &inside, PIECE_TINT, &mut mesh)?;
                    }
                }
                RenderRequest::Sheets { views, objects: Vec::new(), extra: Some(Arc::new(mesh)), camera, marks: Vec::new() }
            } else {
                RenderRequest::Pieces(Arc::new(PiecesJob { ev: Arc::new(plate(&whole, &tests, &plan, flag("on_bed"))), views, extra: None, camera }))
            }
        };
        Ok(Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(render) })
    }
}
