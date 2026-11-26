//! Review sheets for the AI (`fab.render`): dedicated offscreen renderer
//! passes over one prepared plate, never a capture of the window. Ported
//! from Sandbox's model-vision renderer: the same eight fitted views (four
//! perspective volume views, four silhouettes) read back one per frame and
//! packed into two 1024 x 1024 sheets.
//!
//! The model worker prepares the plate (a coloured GLB parsed into a
//! static preview) and encodes the PNG sheets; this widget only draws and
//! reads back. A part made from a distance field has no mesh: each view
//! traces it from its own camera with the plate view's tracer
//! (`fab_sdf_tiles`), to the still view's detail, and draws its tiles with
//! the plate view's field shade, in its own paint baked for the capture (a
//! variant's paint is the variant's, on the variant's shape).

use fab::fab_sdf_tiles::draw::{draw_tiles_lit, DrawSdfTileLit};
use fab::fab_sdf_tiles::surface::Chunk;
use fab::fab_sdf_tiles::trace::{Cam, Tiles};
use fab::viewport::paint::{pack_stills, StillBake};
use fab_ai::camera::{camera_axes, default_views, gltf_box, gltf_point, named_view, project, view_side, Frame, Look, View, ViewCamera, LOOK_FOV};
use makepad_render::{set_pass_camera, DrawSceneAlpha, DrawSceneCube, DrawSceneShadow, DrawSceneSkinned, DrawSceneSky, DrawSceneTerrain, ModelInstance, PreparedStaticPreview, Renderer, SceneDraws};
use makepad_ai_services::wire::MAX_RESULT_IMAGE_BYTES;
use makepad_widgets::*;
use std::time::{Duration, Instant};

const REVIEW_MODEL: &str = "__fab_review";
pub const PAGE_LABELS: [&str; 2] = [
    "Volume: elevated 45deg front three-quarter, elevated 45deg rear three-quarter, lower front three-quarter, opposite lower front",
    "Silhouette: front, side, near-top (83deg), underside; perspective projection",
];

/// A part made from a distance field, as each view traces it: no mesh of it is drawn.
pub struct ReviewField {
    pub object: String,
    pub shape: std::sync::Arc<fab::fab_sdf_engine::makepad_csg_sdf::SdfShape>,
    pub key: [u8; 32],
    /// Its shape's frame onto the plate (printer mm), row-major.
    pub to_printer: [[f64; 4]; 3],
    pub colours: fab_plate::ColourMode,
    /// The owning part’s colour in this render.
    pub color: [f32; 4],
    /// Its own colour paint as the views show it, in the render's colours; None: none. Each
    /// capture bakes it for itself (or takes the plate view's very bake of it): a variant's
    /// paint is its own, on its own shape.
    pub paint: Option<fab::viewport::paint::StillPaint>,
}

/// A prepared plate, made on the model worker.
pub struct PreparedReview {
    /// The plate's meshes; None when every part shown is a field.
    pub model: Option<PreparedStaticPreview>,
    /// The field parts shown.
    pub fields: Vec<ReviewField>,
    /// What the views frame and how their cameras take it.
    pub frame: Frame,
    /// The cameras, in order.
    pub views: Vec<View>,
    /// The sheets' labels (one per four views).
    pub labels: Vec<String>,
    /// What's written on each view; none on a plain review.
    pub overlays: Vec<Overlay>,
    /// Each view's side, pixels ([`view_side`]).
    pub side: usize,
}

/// An orthographic render's scale ([`Frame::orthographic`]): how many mm a view spans, its scale
/// bar (a round length near a third of the view) and its grid's spacing (a quarter or a fifth
/// of the bar; its heavier lines a bar apart), mm.
struct OrthoScale {
    across: f64,
    bar: f64,
    grid: f64,
}

impl OrthoScale {
    fn of(frame: &Frame) -> Option<OrthoScale> {
        let across = 2.0 * (frame.ortho?.half * frame.mm()) as f64;
        let bar = round_below(across * 0.3);
        let grid = if (bar / 10f64.powf(bar.log10().floor()) - 2.0).abs() < 1e-6 { bar / 4.0 } else { bar / 5.0 };
        Some(OrthoScale { across, bar, grid })
    }
}

/// The largest of 1, 2 and 5 times a power of ten that is at most `x` (x > 0).
fn round_below(x: f64) -> f64 {
    let ten = 10f64.powf(x.log10().floor());
    [5.0, 2.0, 1.0].into_iter().map(|m| m * ten).find(|&v| v <= x * (1.0 + 1e-9)).unwrap_or(ten)
}

/// mm as words: no more digits than it has (to 1e-4 mm).
fn mm_words(mm: f64) -> String {
    format!("{}", (mm * 1e4).round() / 1e4)
}

/// One of the person's marks in a view: its outline in the view's pixels, closed for a circle, and
/// the number drawn by it.
#[derive(Clone, Debug, Default)]
pub struct MarkStroke {
    pub points: Vec<(i32, i32)>,
    pub closed: bool,
    pub number: u32,
}

/// What's written on one view, in its pixels.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    /// The person's marks (`fab.annotations`), in the view's pixels.
    pub strokes: Vec<MarkStroke>,
    /// A dot and the words beside it.
    pub marks: Vec<(i32, i32, String)>,
    /// A scale bar's length and what it measures.
    pub bar: Option<(i32, String)>,
}

impl std::fmt::Debug for PreparedReview {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("PreparedReview").field("frame", &self.frame).finish()
    }
}

/// A painted part's triangles longer than this (mm) are split for its paint to show across
/// them (a box's face, a picture tile's); a finer mesh (a field's) takes it at its vertices.
const PAINT_DETAIL: f64 = 3.0;
/// The finest a split triangle's pieces go (mm): a picture's cell is twice this.
const PAINT_PIECE: f64 = 0.2;
/// About how many pieces the plate's long painted triangles split into, all parts together.
const PAINT_PIECES: f64 = 300_000.0;

/// The plate's review mesh in the glTF frame (Y up, metres): positions, a colour per vertex
/// (linear RGB) and triangles.
pub struct PlateMesh {
    pub positions: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// The plate's meshes as the review draws them: printer (x, y, z) mm becomes (x, z, -y) / 1000.
/// Each part shows as the views show it: in its paint where it has one
/// ([`fab_model::eval::PartPaint::shown_at`]), else its print colour. A field part is no mesh
/// here: each view traces it ([`review_fields`]).
pub fn plate_mesh(ev: &fab_model::eval::Evaluated, objects: &[String], extra: Option<&fab_ai::supports::ColoredMesh>) -> PlateMesh {
    let mut m = PlateMesh { positions: Vec::new(), colors: Vec::new(), indices: Vec::new() };
    let mut long = Vec::new();
    for (i, (part, rm)) in ev.plate.parts.iter().zip(&ev.render).enumerate() {
        if (!objects.is_empty() && !objects.iter().any(|o| *o == part.object)) || ev.placed_field(i).is_some() {
            continue;
        }
        add_part(&mut m, part, rm, ev.placed_paint(i), &mut long);
    }
    split_long(&mut m, long);
    if let Some(mesh) = extra {
        let base = m.positions.len() as u32;
        m.positions.extend(mesh.positions.iter().map(|p| gltf_point(*p)));
        m.colors.extend_from_slice(&mesh.colors);
        m.indices.extend(mesh.indices.iter().map(|i| i + base));
    }
    m
}

/// A painted part's triangles too long for its paint to show at their corners, in printer mm.
type Long<'a> = Vec<(&'a fab_model::eval::PartPaint, Vec<[[f64; 3]; 3]>)>;

/// One part's mesh into `m`, in the paint it has (`paint`), else its print colour; the triangles
/// too long to show the paint at their corners wait in `long` for [`split_long`].
fn add_part<'a>(m: &mut PlateMesh, part: &fab_plate::Part, rm: &fab_model::eval::RenderMesh, paint: Option<&'a std::sync::Arc<fab_model::eval::PartPaint>>, long: &mut Long<'a>) {
    let base = m.positions.len() as u32;
    m.positions.extend(rm.positions.iter().map(|p| gltf_point(*p)));
    let Some(paint) = paint.filter(|p| p.program.has(fab_paint_field::Channel::Color)) else {
        m.colors.extend(std::iter::repeat([part.color[0], part.color[1], part.color[2]]).take(rm.positions.len()));
        m.indices.extend(rm.indices.iter().map(|i| i + base));
        return;
    };
    m.colors.extend(paint.shown_at(&rm.positions.iter().map(|p| p.map(f64::from)).collect::<Vec<_>>()));
    let mut split = Vec::new();
    for t in rm.indices.chunks(3) {
        let corners = [0, 1, 2].map(|k| rm.positions[t[k] as usize].map(f64::from));
        if longest(&corners) > PAINT_DETAIL {
            split.push(corners);
        } else {
            m.indices.extend(t.iter().map(|i| i + base));
        }
    }
    long.push((paint.as_ref(), split));
}

/// Each long one's edges in k steps, k² flat pieces in the paint at their middles, the pieces
/// about as fine as the budget allows.
fn split_long(m: &mut PlateMesh, long: Long) {
    let piece = (long.iter().flat_map(|(_, ts)| ts).map(|t| longest(t).powi(2)).sum::<f64>() / PAINT_PIECES).sqrt().max(PAINT_PIECE);
    for (paint, ts) in long {
        let mut pieces: Vec<[[f64; 3]; 3]> = Vec::new();
        for [a, b, c] in ts {
            let k = (longest(&[a, b, c]) / piece).ceil() as usize;
            let p = |i: usize, j: usize| -> [f64; 3] { std::array::from_fn(|d| a[d] + (b[d] - a[d]) * i as f64 / k as f64 + (c[d] - a[d]) * j as f64 / k as f64) };
            for i in 0..k {
                for j in 0..k - i {
                    pieces.push([p(i, j), p(i + 1, j), p(i, j + 1)]);
                    if i + j + 1 < k {
                        pieces.push([p(i + 1, j), p(i + 1, j + 1), p(i, j + 1)]);
                    }
                }
            }
        }
        let middles: Vec<[f64; 3]> = pieces.iter().map(|q| std::array::from_fn(|d| (q[0][d] + q[1][d] + q[2][d]) / 3.0)).collect();
        for (q, c) in pieces.iter().zip(paint.shown_at(&middles)) {
            let at = m.positions.len() as u32;
            m.positions.extend(q.iter().map(|v| gltf_point(v.map(|x| x as f32))));
            m.colors.extend([c; 3]);
            m.indices.extend([at, at + 1, at + 2]);
        }
    }
}

/// The field parts of `objects` (empty: all) and the glTF box their solids' boxes span.
pub fn review_fields(ev: &fab_model::eval::Evaluated, objects: &[String]) -> (Vec<ReviewField>, [[f32; 3]; 2]) {
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    let mut fields = Vec::new();
    for (i, part) in ev.plate.parts.iter().enumerate() {
        if !objects.is_empty() && !objects.iter().any(|o| *o == part.object) {
            continue;
        }
        let Some(f) = ev.placed_field(i) else { continue };
        if let Some(b) = part.field_bounds {
            for q in [gltf_point(b.min.map(|v| v as f32)), gltf_point(b.max.map(|v| v as f32))] {
                for d in 0..3 {
                    bounds[0][d] = bounds[0][d].min(q[d]);
                    bounds[1][d] = bounds[1][d].max(q[d]);
                }
            }
        }
        let b = part.bounds();
        // the field as a distance (a part scaled by axis has its stretch in its shape)
        let (shape, key, to_printer, _) = f.distance();
        let paint = ev.placed_paint(i).filter(|p| p.shown().has(fab_paint_field::Channel::Color)).map(|p| fab::viewport::paint::StillPaint::new(p.shown().clone(), p.palette(), &p.from_printer, b.min, b.max, *key, to_printer));
        fields.push(ReviewField { object: part.object.clone(), shape: shape.clone(), key: *key, to_printer: *to_printer, color: part.color, colours: ev.colour_mode, paint });
    }
    (fields, bounds)
}

/// A triangle's longest edge.
fn longest(t: &[[f64; 3]; 3]) -> f64 {
    (0..3).map(|k| { let (a, b) = (t[k], t[(k + 1) % 3]); ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt() }).fold(0.0, f64::max)
}

/// The plate's meshes as one vertex-coloured GLB ([`plate_mesh`]) and their box; None: no mesh.
pub fn plate_glb(ev: &fab_model::eval::Evaluated, objects: &[String], extra: Option<&fab_ai::supports::ColoredMesh>) -> Option<(Vec<u8>, [[f32; 3]; 2])> {
    let m = plate_mesh(ev, objects, extra);
    if m.indices.is_empty() {
        return None;
    }
    let mut bounds = [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]];
    for q in &m.positions {
        for d in 0..3 {
            bounds[0][d] = bounds[0][d].min(q[d]);
            bounds[1][d] = bounds[1][d].max(q[d]);
        }
    }
    Some((makepad_gltf::write_glb_mesh_colored(&m.positions, &m.indices, Some(&m.colors)), bounds))
}


/// On the model worker: one plate part as its own vertex-coloured preview (the headset moves
/// parts one by one), in the same glTF frame as [`plate_glb`].
pub fn prepare_part(ev: &fab_model::eval::Evaluated, i: usize, part: &fab_plate::Part, rm: &fab_model::eval::RenderMesh) -> Result<PreparedStaticPreview, String> {
    let mut m = PlateMesh { positions: Vec::new(), colors: Vec::new(), indices: Vec::new() };
    let mut long = Vec::new();
    add_part(&mut m, part, rm, ev.placed_paint(i), &mut long);
    split_long(&mut m, long);
    let glb = makepad_gltf::write_glb_mesh_colored(&m.positions, &m.indices, Some(&m.colors));
    let model = makepad_render::StaticModel::parse_glb(&glb).map_err(|e| format!("part model: {e}"))?;
    PreparedStaticPreview::prepare(model)
}

/// On the model worker: the mechanics sim's white link mesh, rest poses and colours.
/// The ordinary plate view draws the part's field.
pub fn prepare_links(part: &fab_plate::Part, w: &fab_model::weave::WeavePart, slots: &[[f32; 4]]) -> Result<crate::xr::PreparedLinks, String> {
    let links = &w.instances;
    let mesh = &links.mesh;
    let colour = |k: usize| {
        let slot = links.slots.get(k).copied().unwrap_or(0) as usize;
        let c = slots.get(slot).filter(|_| slot > 0).copied().unwrap_or(part.color);
        [c[0], c[1], c[2]]
    };
    let yup = |p: [f64; 3]| [(p[0] * 0.001) as f32, (p[2] * 0.001) as f32, (-p[1] * 0.001) as f32];
    let one: Vec<[f32; 3]> = mesh.vertices.iter().map(|v| yup([v.x, v.y, v.z])).collect();
    let white = vec![[1.0f32; 3]; one.len()];
    let one_indices: Vec<u32> = mesh.triangles.iter().flatten().copied().collect();
    let glb = makepad_gltf::write_glb_mesh_colored(&one, &one_indices, Some(&white));
    let model = PreparedStaticPreview::prepare(makepad_render::StaticModel::parse_glb(&glb).map_err(|e| format!("link model: {e}"))?)?;
    let rest = links.poses.iter().map(|pose| Mat4f { v: crate::plate_scene::rigid_m(&fab_model::weave::compose(&w.to_plate, pose)) }).collect();
    let tints = (0..links.poses.len()).map(|k| { let c = colour(k); vec4(c[0], c[1], c[2], 1.0) }).collect();
    Ok(crate::xr::PreparedLinks { model, rest, tints })
}

/// On the model worker: the unit ball of a draped cloth's scene, in the same frame.
pub fn prepare_ball() -> Result<PreparedStaticPreview, String> {
    let (rings, segs) = (24u32, 48u32);
    let (mut positions, mut indices) = (Vec::new(), Vec::new());
    for i in 0..=rings {
        let th = std::f32::consts::PI * i as f32 / rings as f32;
        for j in 0..=segs {
            let ph = std::f32::consts::TAU * j as f32 / segs as f32;
            positions.push([th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin()]);
        }
    }
    for i in 0..rings {
        for j in 0..segs {
            let a = i * (segs + 1) + j;
            let b = a + segs + 1;
            indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    let white = vec![[1.0f32; 3]; positions.len()];
    let glb = makepad_gltf::write_glb_mesh_colored(&positions, &indices, Some(&white));
    PreparedStaticPreview::prepare(makepad_render::StaticModel::parse_glb(&glb).map_err(|e| format!("ball model: {e}"))?)
}

/// The plate in the mode requested for this render.
pub fn shown_plate(ev: &fab_model::eval::Evaluated, colours: fab_ai::Colours) -> std::borrow::Cow<'_, fab_model::eval::Evaluated> {
    let mode = match colours {
        fab_ai::Colours::Design => fab_plate::ColourMode::Model,
        fab_ai::Colours::Spools => fab_plate::ColourMode::Material,
    };
    if mode == ev.colour_mode {
        std::borrow::Cow::Borrowed(ev)
    } else {
        std::borrow::Cow::Owned(ev.in_colours(mode))
    }
}

/// On the model worker: the review product of the current plate.
/// `views`: names (empty: the standard eight); `objects`: the printed objects to frame (empty: all).
/// Each view as large as its sheet lets it be ([`view_side`]).
pub fn prepare(ev: &fab_model::eval::Evaluated, views: &[String], objects: &[String], extra: Option<&fab_ai::supports::ColoredMesh>, camera: &fab_ai::Camera, marks: &[fab_ai::annotate::RenderMark]) -> Result<PreparedReview, String> {
    prepare_sized(ev, views, objects, extra, camera, None, marks)
}

/// On the model worker: views of the plate each drawn as one cell of a sheet of four, whatever
/// their count: what `fab.variants` lays out four cells to a sheet.
pub fn prepare_cells(ev: &fab_model::eval::Evaluated, views: &[String], objects: &[String], camera: &fab_ai::Camera) -> Result<PreparedReview, String> {
    prepare_sized(ev, views, objects, None, camera, Some(view_side(4)), &[])
}

/// [`prepare`], each view `side` pixels square (None: as large as its sheet lets it be).
fn prepare_sized(ev: &fab_model::eval::Evaluated, views: &[String], objects: &[String], extra: Option<&fab_ai::supports::ColoredMesh>, camera: &fab_ai::Camera, side: Option<usize>, marks: &[fab_ai::annotate::RenderMark]) -> Result<PreparedReview, String> {
    if ev.plate.parts.is_empty() {
        return Err("nothing on the plate to render".into());
    }
    let ev = shown_plate(ev, camera.colours);
    let ev = &*ev;
    let glb = plate_glb(ev, objects, extra);
    let (fields, field_box) = review_fields(ev, objects);
    let mut shown = field_box;
    if let Some((_, b)) = &glb {
        for d in 0..3 {
            shown[0][d] = shown[0][d].min(b[0][d]);
            shown[1][d] = shown[1][d].max(b[1][d]);
        }
    }
    if !shown[0][0].is_finite() {
        return Err("none of those objects is on the plate".into());
    }
    let model = match glb {
        Some((glb, _)) => Some(PreparedStaticPreview::prepare(makepad_render::StaticModel::parse_glb(&glb).map_err(|e| format!("review model: {e}"))?)?),
        None => None,
    };
    let what = if objects.is_empty() { String::new() } else { format!(" of {}", objects.join(", ")) };
    let (views, labels): (Vec<View>, Vec<String>) = if views.is_empty() {
        (default_views(), PAGE_LABELS.iter().map(|l| format!("{l}{what}")).collect())
    } else {
        let list: Vec<View> = views.iter().map(|v| named_view(v).ok_or_else(|| format!("unknown view '{v}'"))).collect::<Result<_, _>>()?;
        let labels = list.chunks(4).map(|c| format!("Views{what}, row-major: {}", c.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", "))).collect();
        (list, labels)
    };
    let side = side.unwrap_or_else(|| view_side(views.len()));
    // what the views frame: what's shown, or the focus (plate mm, Z up) in the glTF frame
    let frame = Frame::of_render(shown, &views, camera);
    let (overlays, labels) = match OrthoScale::of(&frame) {
        Some(OrthoScale { across, bar, grid }) => {
            let overlay = Overlay { bar: Some(((bar / across * side as f64).round() as i32, format!("{}mm grid {}mm", mm_words(bar), mm_words(grid)))), ..Default::default() };
            let words = format!("orthographic, every view {} mm across, grid lines every {} mm, heavier every {} mm", mm_words((across * 10.0).round() / 10.0), mm_words(grid), mm_words(bar));
            (vec![overlay; views.len()], labels.into_iter().map(|l| format!("{}; {words}", l.replace("; perspective projection", ""))).collect())
        }
        None => (Vec::new(), labels),
    };
    // the person's marks, drawn in each view as the picture shows them
    let overlays = if marks.is_empty() {
        overlays
    } else {
        let mut overlays = overlays;
        overlays.resize(views.len(), Overlay::default());
        for (overlay, view) in overlays.iter_mut().zip(&views) {
            let camera = frame.look(view);
            overlay.strokes = marks
                .iter()
                .map(|m| MarkStroke { points: m.points.iter().filter_map(|p| project(&camera, frame.placed.at(gltf_point(p.map(|c| c as f32))), side)).collect(), closed: m.closed, number: m.number })
                .filter(|s| !s.points.is_empty())
                .collect();
        }
        overlays
    };
    Ok(PreparedReview { model, fields, frame, views, labels, overlays, side })
}

/// On the model worker: a picture aimed at a place on the plate. The frame fills the view, whatever
/// else is in the mesh.
pub fn prepare_look(look: &fab_ai::supports::LookRequest) -> Result<PreparedReview, String> {
    let positions: Vec<[f32; 3]> = look.mesh.positions.iter().map(|p| gltf_point(*p)).collect();
    let glb = makepad_gltf::write_glb_mesh_colored(&positions, &look.mesh.indices, Some(&look.mesh.colors));
    let model = makepad_render::StaticModel::parse_glb(&glb).map_err(|e| format!("look model: {e}"))?;
    let model = Some(PreparedStaticPreview::prepare(model)?);
    let (lo, hi) = look.frame;
    let frame = Frame::new(gltf_box(lo, hi), false, None, LOOK_FOV);
    let views: Vec<View> = look.views.iter().map(|v| named_view(v).ok_or_else(|| format!("unknown view '{v}'"))).collect::<Result<_, _>>()?;
    let side = view_side(views.len());
    let overlays = views.iter().map(|v| {
        let camera = frame.look(v);
        let marks = look.marks.iter().filter_map(|(p, words)| {
            let (x, y) = project(&camera, frame.placed.at(gltf_point(p.map(|c| c as f32))), side)?;
            Some((x, y, words.clone()))
        }).collect();
        Overlay { marks, bar: scale_bar(&camera, frame.placed.scale, side), ..Default::default() }
    }).collect();
    let labels = views.chunks(4).map(|c| format!("Views, row-major: {}", c.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(", "))).collect();
    Ok(PreparedReview { model, fields: Vec::new(), frame, views, labels, overlays, side })
}

/// A round number of mm near a quarter of a view `side` square, at the view's middle.
fn scale_bar(look: &Look, scale: f32, side: usize) -> Option<(i32, String)> {
    let mm_per_pixel = 2.0 * (look.fov.to_radians() * 0.5).tan() * look.distance / side as f32 / (scale * 0.001);
    let most = mm_per_pixel * side as f32 * 0.3;
    let mm = [100.0f32, 50.0, 20.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1].into_iter().find(|&m| m <= most)?;
    Some(((mm / mm_per_pixel) as i32, format!("{mm}mm")))
}

/// Printable ASCII, 5 x 7, top row first (bit 4 the leftmost column); anything else a hollow box.
fn glyph(c: char) -> [u8; 7] {
    match c {
        ' ' => [0; 7],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '"' => [0b01010, 0b01010, 0b01010, 0, 0, 0, 0],
        '#' => [0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010],
        '$' => [0b00100, 0b01111, 0b10100, 0b01110, 0b00101, 0b11110, 0b00100],
        '%' => [0b11000, 0b11001, 0b00010, 0b00100, 0b01000, 0b10011, 0b00011],
        '&' => [0b01100, 0b10010, 0b10100, 0b01000, 0b10101, 0b10010, 0b01101],
        '\'' => [0b01100, 0b00100, 0b01000, 0, 0, 0, 0],
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        '*' => [0, 0b00100, 0b10101, 0b01110, 0b10101, 0b00100, 0],
        '+' => [0, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0],
        ',' => [0, 0, 0, 0, 0b01100, 0b00100, 0b01000],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        '/' => [0, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        ':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        ';' => [0, 0b01100, 0b01100, 0, 0b01100, 0b00100, 0b01000],
        '<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        '=' => [0, 0, 0b11111, 0, 0b11111, 0, 0],
        '>' => [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '@' => [0b01110, 0b10001, 0b00001, 0b01101, 0b10101, 0b10101, 0b01110],
        'A' => [0b01110, 0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
        '\\' => [0, 0b10000, 0b01000, 0b00100, 0b00010, 0b00001, 0],
        ']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
        '^' => [0b00100, 0b01010, 0b10001, 0, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '`' => [0b01000, 0b00100, 0b00010, 0, 0, 0, 0],
        'a' => [0, 0, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110],
        'c' => [0, 0, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110],
        'd' => [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10011, 0b01101],
        'e' => [0, 0, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'g' => [0, 0, 0b01111, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'j' => [0b00010, 0, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0, 0, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001],
        'n' => [0, 0, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0, 0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0, 0, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000],
        'q' => [0, 0, 0b01101, 0b10011, 0b01111, 0b00001, 0b00001],
        'r' => [0, 0, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0, 0, 0b01110, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0, 0, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0, 0, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0, 0, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'x' => [0, 0, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0, 0, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'z' => [0, 0, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
        '{' => [0b00010, 0b00100, 0b00100, 0b01000, 0b00100, 0b00100, 0b00010],
        '|' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        '}' => [0b01000, 0b00100, 0b00100, 0b00010, 0b00100, 0b00100, 0b01000],
        '~' => [0, 0, 0b01000, 0b10101, 0b00010, 0, 0],
        _ => [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111],
    }
}

/// Paints inside one view of a sheet, `side` pixels square: the view's points (in the pixels it
/// was rendered at, `from` square) scaled to it, its words at their own size, so they stay as
/// legible on a sheet that was shrunk to fit the answer's picture budget.
struct ViewPen<'a> {
    sheet: &'a mut makepad_model::RgbaImage,
    at: (i32, i32),
    side: i32,
    from: i32,
}

impl ViewPen<'_> {
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) {
        let width = self.sheet.width as i32;
        for py in y.max(0)..(y + h).min(self.side) {
            for px in x.max(0)..(x + w).min(self.side) {
                let dst = ((self.at.1 + py) * width + self.at.0 + px) as usize * 4;
                self.sheet.pixels[dst..dst + 4].copy_from_slice(&color);
            }
        }
    }

    /// A length in the pixels the view was rendered at, in this sheet's.
    fn scaled(&self, v: i32) -> i32 {
        v * self.side / self.from
    }

    /// Words on a dark plate, twice the font's size.
    fn words(&mut self, x: i32, y: i32, words: &str) {
        let n = words.chars().count() as i32;
        // inside the view: words too long for it lose their end, not their start
        let x = x.min(self.side - n * 12 - 3).max(2);
        self.rect(x - 2, y - 2, n * 12 + 2, 18, [10, 12, 15, 255]);
        for (i, c) in words.chars().enumerate() {
            for (row, bits) in glyph(c).into_iter().enumerate() {
                for col in 0..5 {
                    if bits & (0b10000 >> col) != 0 {
                        self.rect(x + i as i32 * 12 + col * 2, y + row as i32 * 2, 2, 2, [255; 4]);
                    }
                }
            }
        }
    }

    /// The person's marks: a dark halo under a bright line, the number by the highest point.
    fn stroke(&mut self, stroke: &MarkStroke) {
        let pts: Vec<(i32, i32)> = stroke.points.iter().map(|(x, y)| (self.scaled(*x), self.scaled(*y))).collect();
        let mut segs: Vec<((i32, i32), (i32, i32))> = pts.windows(2).map(|w| (w[0], w[1])).collect();
        if stroke.closed && pts.len() > 2 {
            segs.push((pts[pts.len() - 1], pts[0]));
        }
        if segs.is_empty() {
            segs.push((pts[0], pts[0]));
        }
        for (width, color) in [(7, [10, 12, 15, 255]), (4, [255, 41, 140, 255])] {
            for &((x0, y0), (x1, y1)) in &segs {
                let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
                for i in 0..=steps {
                    let (x, y) = (x0 + (x1 - x0) * i / steps, y0 + (y1 - y0) * i / steps);
                    self.rect(x - width / 2, y - width / 2, width, width, color);
                }
            }
        }
        if let Some(&(x, y)) = pts.iter().min_by_key(|p| p.1) {
            self.words(x + 8, y - 22, &stroke.number.to_string());
        }
    }

    fn overlay(&mut self, overlay: &Overlay) {
        for stroke in &overlay.strokes {
            self.stroke(stroke);
        }
        for (x, y, words) in &overlay.marks {
            let (x, y) = (self.scaled(*x), self.scaled(*y));
            self.rect(x - 3, y - 3, 7, 7, [10, 12, 15, 255]);
            self.rect(x - 2, y - 2, 5, 5, [255; 4]);
            self.words(x + 7, y - 7, words);
        }
        if let Some((length, words)) = &overlay.bar {
            let (y, length) = (self.side - 14, self.scaled(*length));
            self.rect(10, y - 1, length + 2, 6, [10, 12, 15, 255]);
            self.rect(11, y, length, 4, [255; 4]);
            self.words(13 + length, y - 7, words);
        }
    }
}

/// A render's answer: its sheets and its views' cameras.
pub struct Sheets {
    /// Each sheet's label and PNG.
    pub pictures: Vec<(String, Vec<u8>)>,
    /// Each view's camera as the answer tells it ([`ViewCamera::value`]), a list, written out.
    pub cameras: String,
}

impl std::fmt::Debug for Sheets {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Sheets").field("labels", &self.pictures.iter().map(|(l, _)| l).collect::<Vec<_>>()).finish()
    }
}

/// On the model worker: BGRA (RGBA on the web) frames, up to four to a PNG sheet, row-major, with
/// what's written on them. The sheets share one answer's picture budget: all as large as it lets
/// them be, alike, so no sheet takes another's room. Each sheet's label ends with its views' size;
/// each view's camera is told in the pixels it comes back at.
pub fn encode_sheets(frames: &Frames) -> Result<Sheets, String> {
    let Frames { side, frames, labels, overlays, cameras, .. } = frames;
    let side = *side;
    if frames.is_empty() || frames.len().div_ceil(4) != labels.len() || frames.iter().any(|f| f.len() != side * side * 4) {
        return Err("invalid review frame count or dimensions".into());
    }
    let limits = makepad_model::Limits::default();
    let mut sheets = Vec::new();
    for frames in frames.chunks(4) {
        let (columns, rows) = (frames.len().min(2), frames.len().div_ceil(2));
        let mut sheet = makepad_model::RgbaImage::new((side * columns) as u32, (side * rows) as u32, [20, 23, 28, 255], &limits).map_err(|e| format!("{e:?}"))?;
        for (index, bytes) in frames.iter().enumerate() {
            for y in 0..side {
                for x in 0..side {
                    let src = (y * side + x) * 4;
                    let dst = (((index / 2 * side + y) * side * columns) + (index % 2 * side + x)) * 4;
                    let rgba = if cfg!(target_arch = "wasm32") {
                        [bytes[src], bytes[src + 1], bytes[src + 2], bytes[src + 3]]
                    } else {
                        [bytes[src + 2], bytes[src + 1], bytes[src], bytes[src + 3]]
                    };
                    sheet.pixels[dst..dst + 4].copy_from_slice(&rgba);
                }
            }
        }
        sheets.push((sheet, [columns as u32, rows as u32]));
    }
    let pngs = crate::chat::fit_pictures(&sheets, MAX_RESULT_IMAGE_BYTES, |page, sheet, [cell, _]| {
        let cell = cell as i32;
        for index in 0..(frames.len() - page * 4).min(4) {
            if let Some(overlay) = overlays.get(page * 4 + index) {
                ViewPen { sheet: &mut *sheet, at: (index as i32 % 2 * cell, index as i32 / 2 * cell), side: cell, from: side as i32 }.overlay(overlay);
            }
        }
    })?;
    let told = cameras.iter().enumerate().map(|(i, (name, camera))| camera.value(name, i / 4, [i % 4 % 2, i % 4 / 2], pngs.get(i / 4).map_or(0, |(_, [w, _])| *w)));
    let cameras = fab_ai::out::text(&fab_ai::out::arr(told));
    let pictures = labels.iter().zip(pngs).map(|(label, (png, [w, h]))| (format!("{label}; each view {w} x {h} px"), png)).collect();
    Ok(Sheets { pictures, cameras })
}

/// A view's camera as its pass draws it, `side` pixels square: `look` as asked (the field of view
/// and distance unclamped), the near plane in front of all it frames ([`Look::projection`]).
fn review_scene(look: &Look, side: f64) -> SceneState3D {
    SceneState3D {
        time: 0.0,
        camera_pos: look.eye(),
        view: look.view_matrix(),
        projection: look.projection(),
        viewport_rect: Rect { pos: dvec2(0., 0.), size: dvec2(side, side) },
    }
}

/// A field part's shape frame in the scene: its frame onto the plate (printer mm, row-major),
/// into glTF metres ([`gltf_point`]), then where `transform` places the plate.
fn field_world(transform: &Mat4f, to_printer: &[[f64; 4]; 3]) -> Mat4f {
    let r = to_printer;
    let gltf = [r[0], r[2], r[1].map(|v| -v)];
    let mut m = [0.0f32; 16];
    for (row, g) in gltf.iter().enumerate() {
        for c in 0..4 {
            m[c * 4 + row] = (g[c] * 0.001) as f32;
        }
    }
    m[15] = 1.0;
    Mat4f::mul(transform, &Mat4f { v: m })
}

/// How far a view's key light hangs off its camera's axis: it lights all the view shows but a
/// sliver along the outline on its far side, the value falling off toward the outline, so a
/// round part reads round from every side. The plate view hangs its key wider (beside the
/// camera), where its studio's fill lights the far side; a review has only a flat ambient there,
/// so a wider key leaves what it does not reach one flat colour (a third of a ball's face, in a
/// side view lit by the plate view's key).
const KEY_OFF_AXIS_DEG: f32 = 30.0;
/// Which way off the axis: round from the view's up toward its left, as light falls from the
/// upper left in a drawing.
const KEY_TOWARD_DEG: f32 = 45.0;

/// The direction toward a view's key light (glTF frame, Y up): above and to the left of its
/// camera ([`KEY_OFF_AXIS_DEG`], [`KEY_TOWARD_DEG`]), turning with the camera, so a side, back
/// or underside view is lit as a front one is.
fn key_light(yaw: f32, pitch: f32) -> Vec3f {
    let (forward, right, up) = camera_axes(yaw, pitch);
    let (off_sin, off_cos) = KEY_OFF_AXIS_DEG.to_radians().sin_cos();
    let (toward_sin, toward_cos) = KEY_TOWARD_DEG.to_radians().sin_cos();
    let aside = up * toward_cos - right * toward_sin;
    (aside * off_sin - forward * off_cos).normalize()
}

/// One view's world: the stock sky and nothing else on the stage, the key light from beside
/// this view's camera ([`key_light`]).
fn review_world(look: &Look) -> makepad_scene::World {
    let mut world = makepad_scene::World::new();
    world.entities.clear();
    world.terrain = None;
    world.sky = Some(makepad_scene::SkyConfig::default());
    world.camera.target = look.target;
    world.camera.distance = look.distance;
    world.camera.fov = look.fov;
    world.sun.dir = Some(key_light(look.yaw, look.pitch));
    world.mark_render_dirty();
    world
}

struct CaptureTask {
    job: u64,
    started: Instant,
    view: usize,
    drawn: bool,
    settled: u8,
    requested: bool,
    skips: u64,
    frames: Vec<Vec<u8>>,
    /// Each view read back so far: the field parts' tiles it was drawn with.
    tiles: Vec<Vec<ViewTiles>>,
    /// What the views frame, and how their cameras take it.
    frame: Frame,
    views: Vec<View>,
    labels: Vec<String>,
    overlays: Vec<Overlay>,
    side: usize,
    /// An orthographic render's scale, for its grid.
    ortho: Option<OrthoScale>,
    /// Each painted field part's paint bake (its index in `fields`), once started.
    bakes: Option<Vec<(usize, StillBake)>>,
    /// The field parts' paint in the render's colours, once their bakes are in; Some(None): no
    /// field part is painted.
    paint: Option<Option<FieldPaint>>,
    /// The plate in the scene: glTF metres scaled and moved to the frame ([`Placed`]).
    transform: Mat4f,
    fields: Vec<TracedField>,
    /// Why the capture cannot go on.
    failed: Option<String>,
}

/// Field parts' paint for a capture: each part's own paint baked for it, packed alone in the
/// render's own palettes (its design colours, or the trays'), and each painted part's header texel.
struct FieldPaint {
    texture: Texture,
    ctl: [f32; 4],
    headers: Vec<(String, f32)>,
}

/// The width of a capture's field paint atlas, texels.
const PAINT_ATLAS_WIDTH: usize = 256;

impl FieldPaint {
    fn new(cx: &mut Cx, pack: fab::viewport::paint::PaintPack) -> Option<FieldPaint> {
        let height = pack.data.len() / (PAINT_ATLAS_WIDTH * 4);
        (height > 0).then(|| FieldPaint {
            texture: Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: PAINT_ATLAS_WIDTH, height, data: Some(pack.data), updated: TextureUpdated::Full }),
            ctl: [PAINT_ATLAS_WIDTH as f32, height as f32, 0.0, 1.0],
            headers: pack.sdf,
        })
    }
}

/// A field part being traced for a capture: its tracer keeps what it found across the views.
struct TracedField {
    of: ReviewField,
    tiles: Tiles,
    /// Its materials' table, for a part of several.
    table: Option<Texture>,
    /// Its tiles reached the still view's detail from this view's camera.
    settled: bool,
}

/// A finished capture's views: BGRA (RGBA on the web) frames, `side` square, the sheets' labels
/// and what to write on each view.
pub struct Frames {
    pub side: usize,
    pub frames: Vec<Vec<u8>>,
    pub labels: Vec<String>,
    pub overlays: Vec<Overlay>,
    /// Each view's name and camera.
    pub cameras: Vec<(String, ViewCamera)>,
    /// Each view's field parts, as their tiles were drawn in it.
    pub tiles: Vec<Vec<ViewTiles>>,
}

/// A field part's tiles as one view drew them: the drawn chunks themselves (shared, not copied),
/// in its shape's frame, and that frame onto the plate.
pub struct ViewTiles {
    /// The shape's frame onto the plate (printer mm), row-major.
    pub to_printer: [[f64; 4]; 3],
    pub chunks: Vec<Option<Chunk>>,
}

/// What a finished capture hands back: the job and its frames.
pub struct Captured {
    pub job: u64,
    pub result: Result<Frames, String>,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    // An orthographic view's mm grid: drawn over the whole view at the far plane, as the sky is,
    // so it shows only where nothing nearer was drawn. An orthographic view draws its plane at
    // one scale, so the grid is the view's own: lines every `step` mm from where the plate's
    // origin falls (`origin`, the view's pixels), heavier every `major` mm.
    mod.draw.DrawReviewGrid = mod.std.set_type_default() do #(DrawReviewGrid::script_shader(vm)){
        ..mod.draw.DrawQuad
        depth_write: false
        origin: vec2(0.0, 0.0)
        mm_px: 1.0
        step: 1.0
        major: 5.0
        vertex: fn() {
            self.pos = self.geom.pos
            self.vertex_pos = vec4(self.geom.pos.x * 2.0 - 1.0, 1.0 - self.geom.pos.y * 2.0, 1.0, 1.0)
        }
        // how much of the pixel `at` mm along a line every `apart` mm covers
        line: fn(at: float, apart: float) -> float {
            let off = abs(at / apart - floor(at / apart + 0.5)) * apart / self.mm_px
            return clamp(1.0 - off, 0.0, 1.0)
        }
        pixel: fn() {
            let px = self.pos * self.rect_size
            let u = (px.x - self.origin.x) * self.mm_px
            let v = (self.origin.y - px.y) * self.mm_px
            let minor = max(self.line(u, self.step), self.line(v, self.step))
            let major = max(self.line(u, self.major), self.line(v, self.major))
            let a = max(minor * 0.3, major * 0.5)
            return vec4(a, a, a, a)
        }
    }
    mod.widgets.FabReviewRendererBase = #(FabReviewRenderer::register_widget(vm))
    mod.widgets.FabReviewRenderer = set_type_default() do mod.widgets.FabReviewRendererBase{
        width: 1 height: 1
        draw_tick +: {color: #0000}
        draw_tile_lit: mod.draw.SdfTileLit{ alpha_blend: false }
        draw_grid: mod.draw.DrawReviewGrid{}
    }
}

/// An orthographic view's grid ([`OrthoScale`]): where the plate's origin falls in the view, and
/// its lines' spacing.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawReviewGrid {
    #[deref]
    draw_super: DrawQuad,
    /// The plate's origin in the view, pixels from its top left corner.
    #[live]
    origin: Vec2f,
    /// mm a pixel spans.
    #[live]
    mm_px: f32,
    /// The lines' spacing and the heavier lines', mm.
    #[live]
    step: f32,
    #[live]
    major: f32,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabReviewRenderer {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[live]
    draw_cube: DrawSceneCube,
    #[live]
    draw_alpha: DrawSceneAlpha,
    #[live]
    draw_sky: DrawSceneSky,
    #[live]
    draw_terrain: DrawSceneTerrain,
    #[live]
    draw_shadow: DrawSceneShadow,
    #[live]
    draw_models: DrawSceneSkinned,
    /// A 1x1 transparent quad: gives the widget an area to redraw every
    /// frame while a capture runs.
    #[redraw]
    #[live]
    draw_tick: DrawColor,
    /// The field parts' tiles, shaded as the plate view shades them.
    #[live]
    draw_tile_lit: DrawSdfTileLit,
    /// An orthographic view's mm grid.
    #[live]
    draw_grid: DrawReviewGrid,
    #[new]
    pass: DrawPass,
    #[new]
    draw_list: DrawList,
    /// The pass's own list: the scene's inside it, then the field parts' tiles.
    #[new]
    root_list: DrawList,
    #[new]
    color: Texture,
    #[new]
    depth: Texture,
    #[rust]
    initialized: bool,
    #[rust]
    renderer: Renderer,
    #[rust]
    task: Option<CaptureTask>,
}

/// How far a capture is: the view being drawn, of how many, and how far its field parts are traced
/// at the still view's detail (the least of them; None: it has none, its meshes draw at once).
pub struct CaptureProgress {
    pub job: u64,
    pub view: usize,
    pub views: usize,
    pub traced: Option<f32>,
}

impl FabReviewRenderer {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }

    /// The capture being drawn, and how far it is.
    pub fn progress(&self) -> Option<CaptureProgress> {
        let task = self.task.as_ref()?;
        let traced = task.fields.iter().map(|f| if f.settled { 1.0 } else { f.tiles.traced() }).reduce(f32::min);
        Some(CaptureProgress { job: task.job, view: task.view.min(task.views.len().saturating_sub(1)), views: task.views.len(), traced })
    }

    pub fn start(&mut self, cx: &mut Cx, job: u64, prepared: PreparedReview) -> bool {
        if self.busy() {
            return false;
        }
        // A fresh texture identity also rejects a late readback from a
        // cancelled previous review; that image must never attest a new head.
        let side = prepared.side;
        self.color = Texture::new_with_format(cx, TextureFormat::RenderBGRAu8 { size: TextureSize::Fixed { width: side, height: side }, initial: true });
        self.depth = Texture::new_with_format(cx, TextureFormat::DepthD32 { size: TextureSize::Fixed { width: side, height: side }, initial: true });
        self.pass.set_color_texture(cx, &self.color, DrawPassClearColor::ClearWith(vec4(0.08, 0.09, 0.11, 1.)));
        self.pass.set_depth_texture(cx, &self.depth, DrawPassClearDepth::ClearWith(1.));
        if !self.initialized {
            self.pass.set_keep_camera_matrix(cx, true);
            self.renderer.set_gpu_lightmap_mode(makepad_render::gpu_lightmap::GpuLightmapMode::Realtime);
            self.renderer.set_clustered_lighting(true);
            self.initialized = true;
        }
        let PreparedReview { model, fields, frame, views, labels, overlays, side } = prepared;
        self.renderer.enter_realm();
        let transform = frame.placed.transform();
        let models = match model {
            Some(model) => {
                self.renderer.install_static_preview(cx, REVIEW_MODEL, model, false);
                vec![ModelInstance {
                    model: REVIEW_MODEL.into(),
                    custom_material: None,
                    layer_materials: Vec::new(),
                    transform,
                    tint: vec4(1., 1., 1., 1.),
                    color_adjust: vec4(0., 1., 1., 0.),
                    dynamic: true,
                    depth_order: 0.,
                    part_poses: Vec::new(),
                    lamps: Default::default(),
                }]
            }
            None => Vec::new(),
        };
        self.renderer.set_models(models);
        let fields = fields
            .into_iter()
            .map(|of| {
                let table = (of.shape.materials.len() > 1).then(|| fab::viewport::sdf::tile_materials(cx, &of.shape.materials, of.color, of.colours));
                TracedField { of, tiles: Tiles::new(), table, settled: false }
            })
            .collect();
        self.task = Some(CaptureTask { job, started: Instant::now(), view: 0, drawn: false, settled: 0, requested: false, skips: 0, frames: Vec::new(), tiles: Vec::new(), frame, views, labels, overlays, side, ortho: OrthoScale::of(&frame), bakes: None, paint: None, transform, fields, failed: None });
        self.draw_tick.redraw(cx);
        true
    }

    /// Call on every event while busy; returns the frames once all are read back.
    pub fn poll(&mut self, cx: &mut Cx) -> Option<Captured> {
        let task = self.task.as_mut()?;
        if let Some(why) = task.failed.take() {
            let task = self.task.take().unwrap();
            return Some(Captured { job: task.job, result: Err(why) });
        }
        if task.started.elapsed() > Duration::from_secs(120) {
            let task = self.task.take().unwrap();
            return Some(Captured { job: task.job, result: Err("render readback timed out".into()) });
        }
        // Read back only after the view has been drawn on two frames and no
        // pipeline is still compiling (a view captured then is empty).
        if task.drawn && !task.requested {
            if task.settled < 2 || cx.pipelines_pending() > 0 {
                task.settled = task.settled.saturating_add(1);
                task.drawn = false;
                cx.repaint_pass(self.pass.draw_pass_id());
            } else if cx.request_render_texture_capture(&self.color) {
                task.requested = true;
                task.skips = cx.pipeline_skips();
                cx.repaint_pass(self.pass.draw_pass_id());
            }
        }
        if task.requested {
            for (tid, width, height, bytes) in cx.take_render_texture_captures() {
                if tid != self.color.texture_id() {
                    continue;
                }
                if width != task.side || height != task.side || bytes.len() != task.side * task.side * 4 {
                    let task = self.task.take().unwrap();
                    return Some(Captured { job: task.job, result: Err("render returned invalid dimensions".into()) });
                }
                if cx.pipeline_skips() != task.skips {
                    task.requested = false;
                    task.drawn = false;
                    task.settled = 0;
                    continue;
                }
                task.frames.push(bytes);
                task.tiles.push(task.fields.iter().map(|f| ViewTiles { to_printer: f.of.to_printer, chunks: f.tiles.drawn().to_vec() }).collect());
                task.view += 1;
                task.requested = false;
                task.drawn = false;
                task.settled = 0;
                for f in &mut task.fields {
                    f.settled = false;
                }
                if task.view == task.views.len() {
                    let task = self.task.take().unwrap();
                    let cameras = task.views.iter().map(|v| (v.name.clone(), task.frame.camera(v, task.side))).collect();
                    return Some(Captured { job: task.job, result: Ok(Frames { side: task.side, frames: task.frames, labels: task.labels, overlays: task.overlays, cameras, tiles: task.tiles }) });
                }
            }
        }
        self.draw_tick.redraw(cx);
        None
    }

    /// One frame of the current view: the meshes, then each field part traced from this view's
    /// camera. The view is drawn (to be read back) once every field part's tiles are at the still
    /// view's detail and its paint is baked (`state`: the plate view, whose bake of the very same
    /// paint is taken rather than baked again).
    fn draw_view(&mut self, cx: &mut Cx2d, state: Option<&fab::AppState>) {
        let Some(task) = self.task.as_mut() else { return };
        let look = task.frame.look(&task.views[task.view.min(task.views.len() - 1)]);
        let side = task.side as f64;
        let scene = review_scene(&look, side);
        cx.make_child_pass(&self.pass);
        cx.begin_pass(&self.pass, None);
        self.pass.set_size(cx, scene.viewport_rect.size);
        self.pass.set_dpi_factor(cx, 1.);
        set_pass_camera(cx.cx, &self.pass, &scene);
        let pass_size = cx.current_pass_size();
        self.root_list.begin_always(cx);
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        let mut draws = SceneDraws {
            cube: Some(&mut self.draw_cube),
            alpha: Some(&mut self.draw_alpha),
            sky: &mut self.draw_sky,
            sky_analytic: None,
            terrain: Some(&mut self.draw_terrain),
            shadow: Some(&mut self.draw_shadow),
            shadow_sdf: None,
            firework: None,
            flare: None,
            water: None,
            screen: None,
            screen_instances: &[],
            view_model: None,
        };
        let world = review_world(&look);
        self.renderer.draw_scene_full(&mut Cx3d::new(cx.cx), &mut self.draw_list, &mut draws, &world, scene, None, Some(&mut self.draw_models));
        // the field parts after, depth-tested with the meshes, under the meshes' light
        let light = fab::viewport::tile_light(&self.renderer, &world, self.renderer.hdr_output());
        let pool = cx.cx.task_pool();
        // the field parts' own paint, baked for this capture, packed in the render's palettes
        if task.paint.is_none() {
            if task.bakes.is_none() {
                let layer = state.and_then(|s| s.sdf.as_ref()).map(|l| l.borrow());
                let started = task.fields.iter().enumerate().filter_map(|(k, f)| Some((k, f, f.of.paint.as_ref()?))).map(|(k, f, still)| StillBake::start(&pool, layer.as_ref().map(|l| &l.paint), still, f.of.shape.clone()).map(|bake| (k, bake))).collect();
                match started {
                    Ok(bakes) => task.bakes = Some(bakes),
                    Err(e) => task.failed = Some(e),
                }
            }
            let mut baked = task.bakes.is_some();
            for (k, bake) in task.bakes.iter_mut().flatten() {
                match bake.volume() {
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        task.failed = Some(format!("'{}' cannot be drawn: {e}", task.fields[*k].of.object));
                        baked = false;
                    }
                    None => baked = false,
                }
            }
            if baked {
                let bakes = task.bakes.take().unwrap_or_default();
                let parts: Vec<(&str, &fab_paint_field::Volume, &fab::viewport::paint::StillPaint)> = bakes
                    .iter()
                    .filter_map(|(k, bake)| match (bake, &task.fields[*k].of) {
                        (StillBake::Baked(vol), ReviewField { object, paint: Some(still), .. }) => Some((object.as_str(), &**vol, still)),
                        _ => None,
                    })
                    .collect();
                task.paint = Some(if parts.is_empty() { None } else { FieldPaint::new(cx.cx, pack_stills(&parts, PAINT_ATLAS_WIDTH)) });
            }
        }
        let mut ready = task.paint.is_some();
        self.renderer.bind_output_dither(cx.cx, &mut self.draw_tile_lit.draw_vars);
        self.renderer.bind_ibl(cx.cx, &mut self.draw_tile_lit.draw_vars);
        for f in &mut task.fields {
            let world_from_obj = field_world(&task.transform, &f.of.to_printer);
            let cam = Cam::new(&scene.view, &scene.projection, &world_from_obj, side, side);
            f.tiles.frame(&pool, cam, f.of.key, &f.of.shape);
            f.settled |= f.tiles.take_settled().is_some();
            if f.tiles.failed_on(&f.of.key) {
                task.failed = Some(format!("'{}' cannot be drawn: its field does not compile for the tracer", f.of.object));
            }
            ready &= f.settled;
            let paint = task.paint.as_ref().and_then(|p| p.as_ref()).and_then(|p| Some((&p.texture, p.ctl, p.headers.iter().find(|(o, _)| *o == f.of.object)?.1)));
            // matte, as the review's meshes are (they carry no material of their own): a field part
            // and an exact one of the same shape look the same
            let look = fab::fab_sdf_tiles::draw::TileLook { matte: true, ..fab::viewport::sdf::tile_look(&f.of.shape.materials, f.of.color, f.of.colours, f.table.as_ref()) };
            draw_tiles_lit(cx, &mut self.draw_tile_lit, f.tiles.drawn(), Some(&cam), f.tiles.one_sided(), &scene.projection, side as f32, &world_from_obj, &[], look, paint, &light);
        }
        if let (Some(scale), Some(half)) = (&task.ortho, look.ortho) {
            // the grid from where the plate's origin falls in the view
            let (_, right, up) = camera_axes(look.yaw, look.pitch);
            let origin = task.frame.placed.at([0.0; 3]);
            let g = &mut self.draw_grid;
            g.origin = vec2((0.5 + 0.5 * origin.dot(right) / half) * side as f32, (0.5 - 0.5 * origin.dot(up) / half) * side as f32);
            g.mm_px = (scale.across / side) as f32;
            (g.step, g.major) = (scale.grid as f32, scale.bar as f32);
            g.draw_abs(cx, Rect { pos: dvec2(0., 0.), size: dvec2(side, side) });
        }
        cx.end_pass_sized_turtle();
        self.root_list.end(cx);
        cx.end_pass(&self.pass);
        task.drawn = ready;
    }
}

impl Widget for FabReviewRenderer {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.draw_view(cx, scope.data.get_mut::<fab::AppState>().map(|s| &*s));
        self.draw_tick.draw_walk(cx, walk);
        DrawStep::done()
    }
}

/// The `n` characters a view's pen wrote at `(x, y)` of the view whose top left is `at` on
/// `sheet`, read back glyph by glyph; one that is no printable character's reads as U+FFFD.
#[cfg(test)]
pub(crate) fn read_words(sheet: &makepad_model::RgbaImage, at: (i32, i32), x: i32, y: i32, n: usize) -> String {
    (0..n as i32)
        .map(|i| {
            let bits: [u8; 7] = std::array::from_fn(|row| {
                (0..5).fold(0u8, |bits, col| {
                    let (px, py) = (at.0 + x + i * 12 + col * 2, at.1 + y + row as i32 * 2);
                    let k = (py as usize * sheet.width as usize + px as usize) * 4;
                    bits | if sheet.pixels[k..k + 3] == [255, 255, 255] { 0b10000 >> col } else { 0 }
                })
            });
            (' '..='~').find(|&c| glyph(c) == bits).unwrap_or('\u{fffd}')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_plate::machine::Machine;

    /// Every printable character writes as a glyph of its own and any other as a box none of
    /// them is, so what a view's words say reads back from its pixels as written.
    #[test]
    fn every_printable_character_writes_as_its_own_glyph() {
        let all: Vec<char> = (' '..='~').collect();
        for (i, &a) in all.iter().enumerate() {
            assert!(a == ' ' || glyph(a) != [0; 7], "{a:?} writes nothing");
            assert_ne!(glyph(a), glyph('\u{b0}'), "{a:?} writes as a character it is not");
            for &b in &all[i + 1..] {
                assert_ne!(glyph(a), glyph(b), "{a:?} and {b:?} write alike");
            }
        }
        let side = 512;
        let mut sheet = makepad_model::RgbaImage::new(side, side, [20, 23, 28, 255], &makepad_model::Limits::default()).unwrap();
        let text: String = all.iter().collect();
        let lines: Vec<&str> = text.as_bytes().chunks(32).map(|line| std::str::from_utf8(line).unwrap()).collect();
        let mut pen = ViewPen { sheet: &mut sheet, at: (0, 0), side: side as i32, from: side as i32 };
        for (k, line) in lines.iter().enumerate() {
            pen.words(10, 10 + 24 * k as i32, line);
        }
        for (k, line) in lines.iter().enumerate() {
            assert_eq!(read_words(&sheet, (0, 0), 10, 10 + 24 * k as i32, line.len()), *line);
        }
    }

    /// A box painted red where a small ball meets its top: what `fab.render` draws of it is its
    /// surface once, red there (across its two top triangles) and in its own colour away from
    /// it. A ball made from a field, painted alike, is no mesh there: each view traces it, in its
    /// own paint (`tests/review_closeup.rs`).
    #[test]
    fn a_red_painted_region_renders_red() {
        let red = "let red = Material{albedo: [0.8 0.05 0.05]}\n";
        let ball = "let body = m.sdf({field: fn(p) { return sdf_sphere(p, 20mm) } bounds: [[-20mm, -20mm, -20mm] [20mm, 20mm, 20mm]] cell: 0.5mm})\nobject(body)\nm.fill(m.sphere({radius: 8mm at: [0 20mm 0]}), red)\n";
        for body in [ball, "let body = m.box({size: [30mm 20mm 30mm]})\nobject(body)\nm.fill(m.sphere({radius: 6mm at: [0 10mm 0]}), red)\n"] {
            let mut session = fab_ai::Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
            session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
            let (reply, _) = session.set_source("t.cad.splash", &format!("{}{red}{body}", fab_model::program::EMPTY));
            assert!(reply.ok, "{}", reply.text);
            // as the model worker draws the tool's answer
            let reply = session.call("render", &makepad_model::json::Value::Obj(Vec::new()), None);
            assert!(reply.ok, "{}", reply.text);
            let Some(fab_ai::RenderRequest::Sheets { objects, extra, .. }) = reply.render else { panic!("fab.render asks for sheets") };
            let ev = session.review_plate().expect("the plate meshes for review");
            let own = ev.plate.parts[0].color;
            let m = plate_mesh(&ev, &objects, extra.as_deref());
            if body == ball {
                let (fields, shown) = review_fields(&ev, &objects);
                assert!(m.indices.is_empty() && fields.len() == 1 && fields[0].object == ev.plate.parts[0].object, "{} triangles, {} fields", m.indices.len() / 3, fields.len());
                // its box, 40 mm across, in glTF metres
                assert!((0..3).all(|d| ((shown[1][d] - shown[0][d]) - 0.040).abs() < 0.002), "{shown:?}");
                continue;
            }
            assert!(m.indices.len() > 300, "{body}: {} triangles", m.indices.len() / 3);
            // the top's middle (glTF y is up), and each drawn vertex's distance (mm) from it
            let drawn: Vec<usize> = m.indices.iter().map(|&i| i as usize).collect();
            let (lo, hi) = drawn.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), &i| (std::array::from_fn(|d| lo[d].min(m.positions[i][d])), std::array::from_fn(|d| hi[d].max(m.positions[i][d]))));
            let top = [(lo[0] + hi[0]) / 2.0, hi[1], (lo[2] + hi[2]) / 2.0];
            let from_top = |i: usize| (0..3).map(|d| (m.positions[i][d] - top[d]).powi(2)).sum::<f32>().sqrt() * 1000.0;
            let is_red = |c: [f32; 3]| c[0] > 0.6 && c[1] < 0.1 && c[2] < 0.1;
            let near: Vec<[f32; 3]> = drawn.iter().filter(|&&i| from_top(i) < 3.0).map(|&i| m.colors[i]).collect();
            let far: Vec<[f32; 3]> = drawn.iter().filter(|&&i| from_top(i) > 12.0).map(|&i| m.colors[i]).collect();
            assert!(near.len() >= 6 && near.iter().all(|c| is_red(*c)), "{body}: by the top {near:?}");
            assert!(!far.is_empty() && far.iter().all(|c| *c == [own[0], own[1], own[2]]), "{body}: away from it, not its own colour {own:?}");
        }
    }

    /// A render's sheets as the chat's provider takes them: one view (a close-up) and two views,
    /// each drawn as large as such a render draws it, come back no more than it takes a side.
    #[test]
    fn a_render_of_one_or_two_views_comes_back_within_the_chat_s_picture_side() {
        for names in [&["front"][..], &["front", "35,-15"][..]] {
            let views: Vec<View> = names.iter().map(|v| named_view(v).unwrap()).collect();
            let side = view_side(views.len());
            let frame = Frame::of_render([[0.0; 3], [0.1; 3]], &views, &fab_ai::Camera::default());
            // a render-like view: a gradient under a dither, which a PNG hardly compresses
            let view = |k: usize| -> Vec<u8> { (0..side * side).flat_map(|i| { let v = ((i % side + i / side) * 160 / (2 * side) + (i * 7 + k * 13) % 5) as u8 + 40; [v, v, v, 255] }).collect() };
            let frames = Frames {
                side,
                frames: (0..views.len()).map(view).collect(),
                labels: vec![format!("{names:?}")],
                overlays: views.iter().map(|_| Overlay::default()).collect(),
                cameras: views.iter().map(|v| (v.name.clone(), frame.camera(v, side))).collect(),
                tiles: views.iter().map(|_| Vec::new()).collect(),
            };
            let sheets = encode_sheets(&frames).unwrap();
            let sizes = crate::chat::as_the_provider_takes(sheets.pictures).unwrap();
            println!("{names:?} at {side} px a view: {sizes:?}");
            assert!(sizes.iter().all(|s| s[0].max(s[1]) <= makepad_ai_hub::providers::provider::MAX_TOOL_IMAGE_SIDE), "{names:?}: {sizes:?}");
        }
    }

    /// A part painted pink, on a plate whose pink maps to a grey tray: `fab.render` draws the
    /// paint pink, as designed, and grey when asked for the trays' colours; a field part's paint
    /// (its palette) alike.
    #[test]
    fn a_render_defaults_to_the_live_colour_mode_and_can_override_it() {
        let (pink, grey) = ([0.9f32, 0.3, 0.55], [0.53f32, 0.53, 0.53]);
        let material = format!("let pink = Material{{albedo: [{} {} {}] name: \"Pink\"}}\n", pink[0], pink[1], pink[2]);
        let ball = "let body = m.sdf({field: fn(p) { return sdf_sphere(p, 20mm) } bounds: [[-20mm, -20mm, -20mm] [20mm, 20mm, 20mm]] cell: 0.5mm})\nobject(body)\nm.fill(m.sphere({radius: 8mm at: [0 20mm 0]}), pink)\n";
        let block = "let body = m.box({size: [30mm 20mm 30mm]})\nobject(body)\nm.fill(m.sphere({radius: 6mm at: [0 10mm 0]}), pink)\n";
        let near = |a: [f32; 3], b: [f32; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 0.01);
        for body in [ball, block] {
            let mut session = fab_ai::Session::new(Machine::dual_nozzle_350(), std::env::temp_dir());
            session.model = fab_model::FabModel::with_script("t", "t.cad.splash", fab_model::program::EMPTY);
            let (reply, _) = session.set_source("t.cad.splash", &format!("{}{material}{body}", fab_model::program::EMPTY));
            assert!(reply.ok, "{}", reply.text);
            // the plate's pink on a grey spool
            let inks = [("Pink".to_string(), grey)].into_iter().collect();
            assert!(session.map_colours(std::sync::Arc::new(fab_model::eval::Mapped { inks, tools: Default::default() })).is_some(), "the mapping takes");
            session.set_colour_view(fab_plate::ColourMode::Material, true);
            for (args, want) in [("{}", grey), (r#"{"colours":"model"}"#, pink), (r#"{"colours":"material"}"#, grey)] {
                let reply = session.call("render", &makepad_model::json::parse(args.as_bytes()).unwrap(), None);
                assert!(reply.ok, "{}", reply.text);
                let Some(fab_ai::RenderRequest::Sheets { objects, extra, camera, .. }) = reply.render else { panic!("fab.render asks for sheets") };
                let ev = session.review_plate().expect("the plate for review");
                let ev = shown_plate(&ev, camera.colours);
                let shown: Vec<[f32; 3]> = if body == ball {
                    let (fields, _) = review_fields(&ev, &objects);
                    fields[0].paint.as_ref().expect("the ball's paint").palette.clone()
                } else {
                    plate_mesh(&ev, &objects, extra.as_deref()).colors
                };
                let (pinks, greys) = (shown.iter().filter(|c| near(**c, pink)).count(), shown.iter().filter(|c| near(**c, grey)).count());
                let (is, not) = if want == pink { (pinks, greys) } else { (greys, pinks) };
                assert!(is > 0 && not == 0, "{args} on the {}: {pinks} pink and {greys} grey colours shown", if body == ball { "field ball" } else { "box" });
            }
        }
    }
}
