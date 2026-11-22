//! Field parts traced on the CPU into progressively refined triangle tiles.
//! The desktop draws depth and normals into aux, then material and paint in the lit pass.
//! The shared layer tracks edits, sculpt strokes, sections and picking, and draws XR tiles.

use crate::api::{AppState, LiveSdf, PaneId, PartSdf, PlatePart, ShellAction};
use fab_sdf_tiles::draw::{draw_tiles, draw_tiles_lit, draw_tiles_shadow, highlight_texture, DrawSdfTile, DrawSdfTileLit, DrawSdfTileShadow, TileHighlight, TileLook, TileLight, TileSun, material_texture};
use fab_sdf_tiles::surface::{Chunk, QuadBox};
use fab_sdf_tiles::trace::{scan::ScanQueue, Cam, Tiles};
use makepad_render::shadow_csm::{cascade_holds_shadow, cascade_overlaps, cascade_sees, cascade_shadow_in_view};
use makepad_render::{CascadeCasters, CascadeTile};
use makepad_widgets_core::*;
use std::sync::Arc;

use crate::model::ElementId;
use super::view_planes;

/// Fab's scene units per printer millimetre.
const MM: f32 = 0.001;
/// A headset that drew the primary pane's tiles this recently holds their camera.
const XR_TILED: std::time::Duration = std::time::Duration::from_millis(500);
/// The headset draws its eyes from Plate 0's tile slot, which Plate 0 keeps for good.
const XR_PANE: PaneId = PaneId::PLATE_ZERO;

/// What [`SdfLayer::sync`] found.
pub struct SdfSync {
    /// The set of parts drawn by the engine changed: the element lookup hides or shows meshes.
    pub changed: bool,
    /// The view records now: a part's shape is new (the tracer hears of it in that frame).
    pub record: bool,
    /// The tracer is working or owes the view a look, or the paint is baking: the still view's
    /// average waits.
    pub busy: bool,
}

/// One pane's tracers, grown on its first draw and dropped when it closes.
#[derive(Default)]
struct PaneTiles {
    /// The tracer and the field it follows.
    tiles: Option<Tiles>,
    /// A test print's ghost: the whole part, while `tiles` follows its piece.
    ghost_tiles: Option<Tiles>,
    /// Its features lit up ([`tint_of`]), as resolved against what `tiles` draws.
    highlight: Option<Highlight>,
}

/// A part's lit-up features as one pane draws them: for the shape drawn and the tint asked, the
/// mask of a weight per ordinal of its tiles ([`Tiles::drawn_ids`]). Written when either changes,
/// never per frame.
struct Highlight {
    key: [u8; 32],
    tint: Vec<(u32, f32)>,
    mask: Texture,
    count: usize,
}

/// How strongly a feature mixes toward the accent: picked, picked with its chat chip under the
/// pointer, and what a click under the pointer would take.
const TINT_PICKED: f32 = 0.42;
const TINT_LIT: f32 = 0.7;
const TINT_UNDER: f32 = 0.28;

/// The features of printed object `object` lit up ([`csg_sdf::SdfNode::Id`] ids), each at the
/// most any pick or the pointer asks: O(picks).
fn tint_of(state: &AppState, object: &str) -> Vec<(u32, f32)> {
    let picks = &state.picks;
    let mut out: Vec<(u32, f32)> = Vec::new();
    let mut add = |tint: &[(u32, f32)], k: f32| {
        for (id, w) in tint {
            match out.iter_mut().find(|e| e.0 == *id) {
                Some(e) => e.1 = e.1.max(w * k),
                None => out.push((*id, w * k)),
            }
        }
    };
    for p in picks.list.iter().filter(|p| p.object == object) {
        add(&p.tint, if picks.lit.as_deref() == Some(p.id.as_str()) { TINT_LIT } else { TINT_PICKED });
    }
    if let Some(u) = picks.under.as_ref().filter(|u| u.object == object && picks.other.is_none()) {
        add(&u.tint, TINT_UNDER);
    }
    out
}

/// What every part's [`tint_of`] is made of: equal keys, equal tints. The scene passes draw the
/// tint, so a pick, the lit one or the pointer changing a field part's records them again; a
/// pick with no tint (an exact part's) changes nothing. O(picks) a frame.
pub(super) fn tint_key(state: &AppState) -> u64 {
    use std::hash::{Hash, Hasher};
    let picks = &state.picks;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut add = |object: &str, tint: &[(u32, f32)], k: f32| {
        if tint.is_empty() {
            return;
        }
        object.hash(&mut h);
        for (id, w) in tint {
            id.hash(&mut h);
            (w * k).to_bits().hash(&mut h);
        }
    };
    for p in picks.list.iter() {
        add(&p.object, &p.tint, if picks.lit.as_deref() == Some(p.id.as_str()) { TINT_LIT } else { TINT_PICKED });
    }
    if let Some(u) = picks.under.as_ref().filter(|_| picks.other.is_none()) {
        add(&u.object, &u.tint, TINT_UNDER);
    }
    h.finish()
}

impl PaneTiles {
    /// The mask for `tint` against what its tiles draw: kept while both stay, none for no tint
    /// or a table not known yet.
    fn sync_highlight(&mut self, cx: &mut Cx, tint: &[(u32, f32)]) {
        let Some((tiles, key)) = self.tiles.as_ref().and_then(|t| Some((t, t.drawn_key()?))).filter(|_| !tint.is_empty()) else {
            self.highlight = None;
            return;
        };
        if self.highlight.as_ref().is_some_and(|h| h.key == key && h.tint == tint) {
            return;
        }
        let Some(table) = tiles.drawn_ids() else {
            self.highlight = None;
            return;
        };
        let weights: Vec<f32> = std::iter::once(0.0).chain(table.iter().map(|id| tint.iter().filter(|t| t.0 == *id).map(|t| t.1).fold(0.0, f32::max))).collect();
        self.highlight = Some(Highlight { key, tint: tint.to_vec(), count: weights.len(), mask: highlight_texture(cx, &weights) });
    }

    fn highlight(&self, color: Vec4f) -> Option<TileHighlight<'_>> {
        self.highlight.as_ref().map(|h| TileHighlight { mask: &h.mask, count: h.count, color: [color.x, color.y, color.z, 1.0] })
    }

    fn iter(&self) -> impl Iterator<Item = &Tiles> {
        self.tiles.iter().chain(self.ghost_tiles.iter())
    }

    fn iter_mut(&mut self) -> impl Iterator<Item = &mut Tiles> {
        self.tiles.iter_mut().chain(self.ghost_tiles.iter_mut())
    }
}

struct PartView {
    object: String,
    /// The shape and its current placement.
    sdf: PartSdf,
    /// A shape key the engine cannot lower: the part is not drawn (the host says why).
    failed: Option<[u8; 32]>,
    /// The shape whose field's stopped loop the part has been told of.
    loop_stop: Option<[u8; 32]>,
    materials: Option<Texture>,
    materials_key: Option<[u8; 32]>,
    /// The field can be traced: its mesh is hidden.
    ready: bool,
    element: Option<ElementId>,
    color: [f32; 4],
    /// Indexed by `PaneId.0`; the primary pane also supplies the headset's tiles.
    panes: Vec<PaneTiles>,
    newest: Option<PartSdf>,
    /// The primary tile view: the shape it last started on, whether tiles have reached the glass, the
    /// newest dragged shape and the dragged shapes asked for and not drawn yet, oldest first.
    tile_shape: Option<[u8; 32]>,
    /// The section leaves nothing of it.
    cut_away: bool,
    /// The part as the section leaves it: what it was cut from, by which planes, and that.
    cut: Option<([u8; 32], Vec<[f64; 4]>, Option<PartSdf>)>,
    tiles_shown: bool,
    live_shape: Option<[u8; 32]>,
    live_asked: Vec<([u8; 32], std::time::Instant, f64)>,
}

/// A part's pane tiles as the sun's cascades draw them this frame.
struct Caster {
    part: usize,
    pane: PaneId,
    world_from_obj: Mat4f,
    /// Its bounds in the world.
    lo: Vec3f,
    hi: Vec3f,
}

pub struct SdfLayer {
    parts: Vec<PartView>,
    /// The parts in the cascades this frame, the section's planes in the world, what that comes to
    /// ([`CascadeCasters::revision`]) and what draws them.
    casters: Vec<Caster>,
    caster_planes: Vec<[f32; 4]>,
    casters_rev: u64,
    tile_shadow: Option<DrawSdfTileShadow>,
    /// The parts' scans over the hemisphere above the bed, when the user is at rest.
    pub scan: ScanQueue,
    /// View › Shading › Mesh: the parts draw as their meshes.
    as_mesh: bool,
    /// Why a part's field cannot be drawn, for the host (once).
    unavailable: Option<String>,
    /// When a headset last drew the primary pane's tiles: that pane leaves their camera to it,
    /// and looks again itself once it stops. Other panes keep their own cameras.
    xr_tiled: Option<std::time::Instant>,
    /// A headset's look found tiles the desktop has not drawn: it records ([`SdfLayer::take_woke`]).
    xr_found: bool,
    /// The plate's paint (`super::paint`): every lane's lookup.
    pub paint: super::paint::PaintLayer,
    /// Sculpt strokes being drawn or not in the plate yet (`tools::sculpt`), per script
    /// instance: its field parts drawn with them, ahead of the plate's fields.
    sculpts: Vec<SculptLive>,
    /// Parts a placement preview scales by axis ([`StretchLive`]).
    stretches: Vec<StretchLive>,
}

/// A part whose placement preview's pose scales it along some axes more than others (a scale
/// handle by axis): its field drawn stretched, as a live field ([`PartSdf::placed`]), and the
/// pose left out of its frame ([`drawn_pose`]), since a tracer's view of a part is a turn, a move
/// and one scale. What it was made from: the part's field key and the pose.
struct StretchLive {
    base: [u8; 32],
    pose: crate::model::state::Pose,
    live: LiveSdf,
}

/// The pose a part's field is drawn under, from the pose source every pane shares (fields are
/// shared; the demo devices draw their parts as meshes), unless it stretches the part (its field
/// is then drawn stretched, [`StretchLive`]).
fn drawn_pose(state: &AppState, element: ElementId) -> Option<crate::model::state::Pose> {
    state.shared_pose(element).filter(crate::model::state::pose_similar)
}

/// `m` with its columns at right angles, each its length kept: a map built through a pose's
/// f32 rows, its noise taken out, so a stretch along a part's axes splits off exactly.
fn square_columns(m: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let len: [f64; 3] = std::array::from_fn(|c| dot(col(c), col(c)).sqrt());
    let mut u: Vec<[f64; 3]> = Vec::new();
    for c in 0..3 {
        let mut v = col(c);
        for w in &u {
            let d = dot(v, *w);
            v = std::array::from_fn(|k| v[k] - d * w[k]);
        }
        let l = dot(v, v).sqrt().max(1e-300);
        u.push(v.map(|x| x / l));
    }
    std::array::from_fn(|r| [u[0][r] * len[0], u[1][r] * len[1], u[2][r] * len[2], m[r][3]])
}

/// One script instance's live sculpt: its field parts' committed fields (object, key), the
/// strokes added to their strokes log (each press's id and the points taken), the log with
/// them, and the parts' fields sculpted by it.
struct SculptLive {
    instance: String,
    bases: Vec<(String, [u8; 32])>,
    added: Vec<(u64, usize)>,
    strokes: Arc<fab_sdf_engine::makepad_csg_sdf::strokes::Strokes>,
    fields: Vec<LiveSdf>,
}

/// The world box round a box in a part's frame.
fn world_box(world_from_obj: &Mat4f, b: &QuadBox) -> (Vec3f, Vec3f) {
    let m = &world_from_obj.v;
    let (mut lo, mut hi) = ([0.0f32; 3], [0.0f32; 3]);
    for r in 0..3 {
        lo[r] = m[12 + r];
        hi[r] = m[12 + r];
        for c in 0..3 {
            let (p, q) = (m[c * 4 + r] * b.lo[c], m[c * 4 + r] * b.hi[c]);
            lo[r] += p.min(q);
            hi[r] += p.max(q);
        }
    }
    (vec3(lo[0], lo[1], lo[2]), vec3(hi[0], hi[1], hi[2]))
}

/// The script instance of a part's object id (`instance/solid`).
pub(crate) fn instance_of(object: &str) -> &str {
    object.split_once('/').map_or(object, |(i, _)| i)
}

impl Default for SdfLayer {
    fn default() -> Self {
        Self { parts: Vec::new(), casters: Vec::new(), caster_planes: Vec::new(), casters_rev: 0, tile_shadow: None, scan: ScanQueue::default(), as_mesh: false, unavailable: None, xr_tiled: None, xr_found: false, paint: Default::default(), sculpts: Vec::new(), stretches: Vec::new() }
    }
}

/// How a field part shades: one material's values, or several's `table` ([`material_texture`]
/// of `materials`). A part without materials of its own prints in its filament, `color`.
pub fn tile_look<'a>(materials: &[fab_sdf_engine::makepad_csg_sdf::SdfMaterial], color: [f32; 4], table: Option<&'a Texture>) -> TileLook<'a> {
    match materials {
        [m] => TileLook { table: None, albedo: m.albedo, rough: m.rough, metal: m.metal, unlit: m.unlit, emit: m.emissive_color.map(|c| c * m.emissive), matte: false, highlight: None },
        [] => TileLook { table: None, albedo: [color[0], color[1], color[2]], rough: crate::api::FILAMENT_ROUGHNESS, metal: 0.0, unlit: false, emit: [0.0; 3], matte: false, highlight: None },
        [m, ..] => TileLook { table, albedo: m.albedo, rough: m.rough, metal: m.metal, unlit: m.unlit, emit: [0.0; 3], matte: false, highlight: None },
    }
}

/// Fab scene units from the shape frame: `MM * to_printer`, plus a drag nudge.
fn fab_from_shape(sdf: &PartSdf, nudge: Vec3f) -> Mat4f {
    let m = &sdf.to_printer;
    let mut v = [0.0f32; 16];
    for r in 0..3 {
        for c in 0..4 {
            v[c * 4 + r] = m[r][c] as f32 * MM;
        }
    }
    v[12] += nudge.x;
    v[13] += nudge.y;
    v[14] += nudge.z;
    v[15] = 1.0;
    Mat4f { v }
}

/// Fab's scene frame from a part's shape frame: Mechanics' rigid pose (rest
/// -> now), else the placement preview's pose, after its placement.
fn fab_from_part(v: &PartView, element: ElementId, state: &AppState) -> Mat4f {
    let moved = match drawn_pose(state, element) {
        Some(a) => Mat4f { v: [a[0][0], a[1][0], a[2][0], 0.0, a[0][1], a[1][1], a[2][1], 0.0, a[0][2], a[1][2], a[2][2], 0.0, a[0][3], a[1][3], a[2][3], 1.0] },
        None => Mat4f::identity(),
    };
    Mat4f::mul(&moved, &fab_from_shape(&v.sdf, vec3(0.0, 0.0, 0.0)))
}

/// A ray's hit on an SDF part's exact field, plate millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlateHit {
    /// Along the ray (plate mm).
    pub t: f64,
    /// Unit, out of the solid (plate frame).
    pub normal: [f64; 3],
    /// The Id node whose surface it is (`csg_sdf::part_id` of its solid).
    pub id: Option<u32>,
}

/// The feature of an SDF part under a ray in plate millimetres (`origin`,
/// unit `dir`), on the part's exact field. `step`: the tracer's kernel scale
/// (a kernel that is no distance steps shorter; 1 otherwise).
pub fn pick_plate(sdf: &PartSdf, origin: [f64; 3], dir: [f64; 3], step: f64) -> Option<PlateHit> {
    // p_plate = M p_shape + t: into the shape frame with M's inverse
    let m = &sdf.to_printer;
    let a = [[m[0][0], m[0][1], m[0][2]], [m[1][0], m[1][1], m[1][2]], [m[2][0], m[2][1], m[2][2]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-18 {
        return None;
    }
    let inv = [
        [(a[1][1] * a[2][2] - a[1][2] * a[2][1]) / det, (a[0][2] * a[2][1] - a[0][1] * a[2][2]) / det, (a[0][1] * a[1][2] - a[0][2] * a[1][1]) / det],
        [(a[1][2] * a[2][0] - a[1][0] * a[2][2]) / det, (a[0][0] * a[2][2] - a[0][2] * a[2][0]) / det, (a[0][2] * a[1][0] - a[0][0] * a[1][2]) / det],
        [(a[1][0] * a[2][1] - a[1][1] * a[2][0]) / det, (a[0][1] * a[2][0] - a[0][0] * a[2][1]) / det, (a[0][0] * a[1][1] - a[0][1] * a[1][0]) / det],
    ];
    let mul = |v: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| inv[i][0] * v[0] + inv[i][1] * v[1] + inv[i][2] * v[2]) };
    let ro = mul([origin[0] - m[0][3], origin[1] - m[1][3], origin[2] - m[2][3]]);
    let rd_s = mul(dir);
    // a uniform scale: shape distances are plate distances / s
    let s = (rd_s[0] * rd_s[0] + rd_s[1] * rd_s[1] + rd_s[2] * rd_s[2]).sqrt();
    if s < 1e-18 {
        return None;
    }
    let rd = rd_s.map(|c| c / s);
    let hit = fab_sdf_tiles::pick::pick(&sdf.shape.root, ro, rd, 0.0, 1.0e7, 1e-4, step)?;
    // a normal turns with the placement's rotation (a uniform scale keeps its direction)
    let n: [f64; 3] = std::array::from_fn(|i| a[i][0] * hit.normal[0] + a[i][1] * hit.normal[1] + a[i][2] * hit.normal[2]);
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-30);
    Some(PlateHit { t: hit.t / s, normal: n.map(|c| c / l), id: hit.id })
}

/// What is left of a part under the section's half-spaces (`planes`, in its shape's frame, unit
/// normals, kept where `n . p + d >= 0`): the field the rays see, so what a cut opens up and the
/// cut's faces are traced as any surface is. Each plane is one more operand of an intersection, a
/// box whose face lies on the plane and whose other faces are out of the part's reach: a true
/// distance around the part, so what proves emptiness holds. A plane that keeps the whole part
/// leaves it as it is; None: nothing is left.
fn cut_part(sdf: &PartSdf, planes: &[[f64; 4]]) -> Option<PartSdf> {
    use fab_sdf_engine::{cut_shape, Cut};
    match cut_shape(&sdf.shape, planes) {
        Cut::Whole => Some(sdf.clone()),
        Cut::Gone => None,
        Cut::Left(shape) => {
            let key = shape.key();
            Some(PartSdf { shape: Arc::new(shape), to_printer: sdf.to_printer, key })
        }
    }
}

/// The section's half-spaces (Fab's scene frame) in the frame `fab_from` takes to it, unit normals.
fn planes_in_part(planes: &[[f32; 4]], fab_from: &Mat4f) -> Vec<[f64; 4]> {
    let m = &fab_from.v;
    planes
        .iter()
        .map(|p| {
            let n: [f64; 3] = std::array::from_fn(|j| (p[0] * m[j * 4] + p[1] * m[j * 4 + 1] + p[2] * m[j * 4 + 2]) as f64);
            let k = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-30);
            [n[0] / k, n[1] / k, n[2] / k, (p[0] * m[12] + p[1] * m[13] + p[2] * m[14] + p[3]) as f64 / k]
        })
        .collect()
}

/// The nearest point of an SDF part's surface to `p` (plate millimetres) and how far it is, on the
/// part's exact field: a few steps down its gradient. For a kernel that is no distance, an estimate.
pub fn nearest_on_field(sdf: &PartSdf, p: [f64; 3]) -> (f64, [f64; 3]) {
    use fab_sdf_engine::makepad_csg_sdf::distance;
    let m = &sdf.to_printer;
    let a = [[m[0][0], m[0][1], m[0][2]], [m[1][0], m[1][1], m[1][2]], [m[2][0], m[2][1], m[2][2]]];
    // a turn and a uniform scale: its inverse is its transpose over the scale squared
    let k2 = a[0][0] * a[0][0] + a[1][0] * a[1][0] + a[2][0] * a[2][0];
    if k2 < 1e-18 {
        return (f64::INFINITY, p);
    }
    let to_shape = |q: [f64; 3]| -> [f64; 3] {
        let d = [q[0] - m[0][3], q[1] - m[1][3], q[2] - m[2][3]];
        std::array::from_fn(|i| (a[0][i] * d[0] + a[1][i] * d[1] + a[2][i] * d[2]) / k2)
    };
    let to_plate = |q: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| a[i][0] * q[0] + a[i][1] * q[1] + a[i][2] * q[2] + m[i][3]) };
    let f = |q: [f64; 3]| distance(&sdf.shape.root, makepad_csg_math::Vec3d::new(q[0], q[1], q[2]));
    let mut q = to_shape(p);
    let d0 = f(q);
    let h = 1e-3;
    for _ in 0..8 {
        let d = f(q);
        let g: [f64; 3] = std::array::from_fn(|i| {
            let (mut a, mut b) = (q, q);
            a[i] += h;
            b[i] -= h;
            (f(a) - f(b)) / (2.0 * h)
        });
        let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
        if l < 1e-12 || d.abs() < 1e-5 {
            break;
        }
        q = std::array::from_fn(|i| q[i] - g[i] / l * d);
    }
    (d0.abs() * k2.sqrt(), to_plate(q))
}

/// A part's field: a drag's live one ahead of the plate's.
fn effective<'a>(p: &'a PlatePart, live: &'a [LiveSdf]) -> Option<&'a PartSdf> {
    live.iter().find(|l| l.object == p.object).map(|l| &l.sdf).or(p.sdf.as_ref())
}

impl PartView {
    /// The table of a field of several materials.
    fn sync_tile_table(&mut self, cx: &mut Cx) {
        let Some(sdf) = self.newest.as_ref().filter(|s| s.shape.materials.len() > 1) else { return };
        if self.materials_key != Some(sdf.key) {
            self.materials = Some(material_texture(cx, &sdf.shape.materials));
            self.materials_key = Some(sdf.key);
        }
    }

    fn tile_look(&self) -> TileLook<'_> {
        tile_look(self.newest.as_ref().map_or(&[][..], |s| &s.shape.materials[..]), self.color, self.materials.as_ref())
    }

}

impl SdfLayer {
    /// The sculpt strokes being drawn (`tools::sculpt::with_live`): each one's script
    /// instance's field parts are drawn sculpted by their strokes log with the strokes added,
    /// the field the model makes of the same records once they land. A stroke growing adds its
    /// new points to the log (only its new capsules' bricks change); anything else, or a
    /// committed field that changed under it, starts from the plate's log again.
    pub fn set_sculpt(&mut self, state: &AppState) {
        let old = std::mem::take(&mut self.sculpts);
        self.sculpts = crate::tools::sculpt::with_live(|strokes, sampled| {
            let mut out: Vec<SculptLive> = Vec::new();
            for s in strokes.iter().filter(|s| !s.model.is_empty()) {
                let instance = instance_of(s.object());
                if out.iter().any(|l| l.instance == instance) {
                    continue;
                }
                let group: Vec<_> = strokes.iter().filter(|g| !g.model.is_empty() && instance_of(g.object()) == instance).collect();
                let parts: Vec<&PlatePart> = state.plate.parts.iter().filter(|p| p.sdf.is_some() && instance_of(&p.object) == instance).collect();
                if parts.is_empty() {
                    continue;
                }
                let t = std::time::Instant::now();
                let bases: Vec<(String, [u8; 32])> = parts.iter().map(|p| (p.object.clone(), p.sdf.as_ref().unwrap().key)).collect();
                // the strokes added before: the same presses, all but the last one whole
                let grows = |l: &SculptLive| {
                    l.bases == bases
                        && l.added.len() <= group.len()
                        && l.added.iter().zip(&group).enumerate().all(|(k, ((id, n), g))| *id == g.id && if k + 1 == l.added.len() { *n <= g.model.len() } else { *n == g.model.len() })
                };
                let mut live = match old.iter().position(|l| l.instance == instance && grows(l)) {
                    Some(k) => SculptLive { instance: instance.to_string(), bases, added: old[k].added.clone(), strokes: old[k].strokes.clone(), fields: old[k].fields.clone() },
                    None => {
                        // the plate's log (every field part of the instance reads the same file)
                        let log = parts.iter().find_map(|p| match &p.sdf.as_ref().unwrap().shape.root {
                            fab_sdf_engine::makepad_csg_sdf::SdfNode::Sculpt { strokes, .. } => Some(strokes.clone()),
                            _ => None,
                        });
                        SculptLive { instance: instance.to_string(), bases, added: Vec::new(), strokes: log.unwrap_or_default(), fields: Vec::new() }
                    }
                };
                let mut moved = live.fields.is_empty();
                for (k, g) in group.iter().enumerate() {
                    match live.added.get_mut(k) {
                        Some((_, n)) if *n < g.model.len() => {
                            Arc::make_mut(&mut live.strokes).extend(&g.model[*n..]);
                            *n = g.model.len();
                            moved = true;
                        }
                        Some(_) => {}
                        None => {
                            Arc::make_mut(&mut live.strokes).push(g.op, g.radius, g.strength, &g.model);
                            live.added.push((g.id, g.model.len()));
                            moved = true;
                        }
                    }
                }
                if moved {
                    let asked = sampled.unwrap_or(t);
                    live.fields = parts
                        .iter()
                        .map(|p| {
                            let sdf = p.sdf.as_ref().unwrap();
                            let shape = Arc::new((*sdf.shape).clone().sculpted(live.strokes.clone()));
                            let key = shape.key();
                            LiveSdf { object: p.object.clone(), sdf: PartSdf { shape, to_printer: sdf.to_printer, key }, asked, compile_ms: 0.0 }
                        })
                        .collect();
                    let ms = t.elapsed().as_secs_f64() * 1e3;
                    live.fields.iter_mut().for_each(|f| f.compile_ms = ms);
                }
                out.push(live);
            }
            out
        });
        // The last strokes landed: the part's committed field is the one drawn live (nothing
        // retraces); a cancel leaves the committed field as it was.
        for l in old.iter().filter(|l| !self.sculpts.iter().any(|n| n.instance == l.instance)) {
            for f in &l.fields {
                let now = state.plate.parts.iter().find(|p| p.object == f.object).and_then(|p| p.sdf.as_ref());
                if now.map(|s| s.key) != l.bases.iter().find(|(o, _)| *o == f.object).map(|(_, k)| *k) {
                    let why = match now {
                        Some(s) if s.key != f.sdf.key => "landed with a field other than the one drawn".to_string(),
                        Some(s) if s.to_printer != f.sdf.to_printer => {
                            let by: [f64; 3] = std::array::from_fn(|r| s.to_printer[r][3] - f.sdf.to_printer[r][3]);
                            format!("landed as drawn, placed elsewhere (moved {by:?} mm)")
                        }
                        Some(_) => "landed as drawn".to_string(),
                        None => "landed without a field".to_string(),
                    };
                    log!("fab vp sdf sculpt: {} {why}", f.object);
                }
            }
        }
    }

    /// The parts a placement preview scales by axis, each drawn by its field stretched (made once
    /// per pose: a drag's step re-traces it as a dragged value's does). Gives the parts whose
    /// preview ended (committed, or called off): each keeps it drawn till its field is whole, no
    /// frame of the field half traced between.
    fn sync_stretches(&mut self, state: &AppState) -> Vec<String> {
        let mut keep = Vec::new();
        for p in &state.plate.parts {
            let (Some(sdf), Some(element)) = (p.sdf.as_ref(), p.element) else { continue };
            let Some(pose) = state.shared_pose(element).filter(|a| !crate::model::state::pose_similar(a)) else { continue };
            if let Some(i) = self.stretches.iter().position(|s| s.live.object == p.object && s.base == sdf.key && s.pose == pose) {
                keep.push(self.stretches.swap_remove(i));
                continue;
            }
            // the pose (Fab's frame, metres) after the placement (printer mm)
            let m: [[f64; 4]; 3] = std::array::from_fn(|r| {
                std::array::from_fn(|c| (0..3).map(|k| pose[r][k] as f64 * sdf.to_printer[k][c]).sum::<f64>() + if c == 3 { pose[r][3] as f64 * 1000.0 } else { 0.0 })
            });
            let Some(drawn) = PartSdf::placed(sdf.shape.clone(), &square_columns(&m), sdf.key) else { continue };
            keep.push(StretchLive { base: sdf.key, pose, live: LiveSdf { object: p.object.clone(), sdf: drawn, asked: std::time::Instant::now(), compile_ms: 0.0 } });
        }
        // ended with the preview, not by a drag back to an even scale (its part drawn under it)
        let previewed = |o: &str| state.plate.parts.iter().find(|p| p.object == o).and_then(|p| p.element).is_some_and(|e| state.shared_pose(e).is_some());
        let ended = self.stretches.drain(..).map(|s| s.live.object).filter(|o| !keep.iter().any(|k| k.live.object == *o) && !previewed(o)).collect();
        self.stretches = keep;
        ended
    }

    /// The fields the sculpt strokes draw live, in place of their parts' committed ones.
    pub fn sculpt_fields(&self) -> impl Iterator<Item = &LiveSdf> {
        self.sculpts.iter().flat_map(|l| &l.fields)
    }

    /// Follows the plate: new parts, edited shapes, moves, removals; takes
    /// finished builds.
    pub fn sync(&mut self, cx: &mut Cx, state: &AppState) -> SdfSync {
        let mut changed = false;
        let as_mesh = state.view.look == crate::api::PartLook::Mesh;
        if as_mesh != self.as_mesh {
            self.as_mesh = as_mesh;
            log!("fab vp sdf look: {}", if as_mesh { "mesh" } else { "field" });
            changed = true;
        }
        let plate = &state.plate.parts;
        let stretch_ended = self.sync_stretches(state);
        // a sculpt's fields ahead of a parameter drag's, then a scale by axis's (one part is not
        // two at once)
        let live: Vec<LiveSdf> = self.sculpt_fields().cloned().chain(state.params.sdf_live.iter().cloned()).chain(self.stretches.iter().map(|s| s.live.clone())).collect();
        let live = &live[..];
        // what the engine draws: the plate's field parts, and the pieces of field parts a Mechanics
        // sim moves (in its own scene) as (object, field, element, colour)
        let drawn: Vec<(&str, &PartSdf, Option<ElementId>, [f32; 4])> = plate
            .iter()
            .filter_map(|p| Some((p.object.as_str(), effective(p, live)?, p.element, p.color)))
            .chain(state.mech.fields.iter().map(|f| (f.object.as_str(), &f.sdf, state.scene.elements.iter().find(|e| e.name == f.object).map(|e| e.id), f.color)))
            .collect();
        let before = self.parts.len();
        self.parts.retain(|v| drawn.iter().any(|d| d.0 == v.object));
        changed |= self.parts.len() != before;
        for &(object, sdf, element, color) in &drawn {
            let i = match self.parts.iter().position(|v| v.object == object) {
                Some(i) => i,
                None => {
                    self.parts.push(PartView {
                        object: object.to_string(),
                        sdf: sdf.clone(),
                        failed: None,
                        loop_stop: None,
                        materials: None,
                        materials_key: None,
                        ready: false,
                        element: None,
                        color,
                        panes: Vec::new(),
                        tile_shape: None,
                        cut_away: false,
                        cut: None,
                        tiles_shown: false,
                        live_shape: None,
                        live_asked: Vec::new(),
                        newest: None,
                    });
                    self.parts.len() - 1
                }
            };
            let v = &mut self.parts[i];
            if v.element != element {
                v.element = element;
                changed = true;
            }
            v.color = color;
            if v.sdf.key == sdf.key {
                // A move preserves the shape.
                v.sdf.to_printer = sdf.to_printer;
            }
        }
        let mut record = false;
        if !state.test_cut_shown() {
            self.parts.iter_mut().flat_map(|v| &mut v.panes).for_each(|p| p.ghost_tiles = None);
        }
        let poses = state.shared_poses();
        for (v, sdf) in self.parts.iter_mut().filter_map(|v| {
            let d = drawn.iter().find(|d| d.0 == v.object)?;
            Some((v, d.1))
        }) {
            v.sdf = sdf.clone();
            let planes = view_planes(state);
            let planes = v.element.map_or(Vec::new(), |e| planes_in_part(planes.as_slice(), &fab_from_part(v, e, state)));
            if planes.is_empty() {
                v.cut = None;
            } else if v.cut.as_ref().is_none_or(|c| c.0 != sdf.key || c.1 != planes) {
                let left = cut_part(sdf, &planes);
                v.cut = Some((sdf.key, planes, left));
            }
            // cut away whole: nothing to trace, nothing to draw
            let sdf = match &v.cut {
                Some((_, _, left)) => left.as_ref(),
                None => Some(sdf),
            };
            let cut_away = sdf.is_none();
            if cut_away != v.cut_away {
                v.cut_away = cut_away;
                changed = true;
            }
            v.newest = sdf.cloned();
            let Some(sdf) = v.newest.clone() else {
                // No pane will frame this piece again while it is cut away. Cancel its work;
                // a test print's whole-part ghosts still draw from their separate tracers.
                v.panes.iter_mut().for_each(|p| p.tiles = None);
                continue;
            };
            if v.failed != Some(sdf.key) && v.panes.iter().flat_map(PaneTiles::iter).any(|t| t.failed_on(&sdf.key)) {
                log!("fab vp sdf: {} cannot be traced", v.object);
                self.unavailable.get_or_insert_with(|| format!("{}'s field cannot be traced", v.object));
                v.failed = Some(sdf.key);
            }
            if v.loop_stop != Some(sdf.key) && (sdf.shape.root.loop_stopped() || v.panes.iter().flat_map(PaneTiles::iter).any(|t| t.loop_stopped_on(&sdf.key))) {
                v.loop_stop = Some(sdf.key);
                Cx::post_action(ShellAction::FieldLoopStopped { key: sdf.key });
            }
            let ready = v.failed != Some(sdf.key);
            if ready != v.ready {
                v.ready = ready;
                changed = true;
            }
            if stretch_ended.contains(&v.object) {
                for t in v.panes.iter_mut().filter_map(|p| p.tiles.as_mut()) {
                    t.hold_until_whole(sdf.key);
                }
            }
            if v.tile_shape != Some(sdf.key) {
                v.tile_shape = Some(sdf.key);
                let key: String = sdf.key[..8].iter().map(|b| format!("{b:02x}")).collect();
                log!("fab vp sdf tiles: {} new shape {key}", v.object);
                // nothing else may ask the view to record: the tracer hears of it in that frame
                record = true;
            }
            if let Some(l) = live.iter().find(|l| l.object == v.object && v.live_shape != Some(l.sdf.key)) {
                v.live_shape = Some(l.sdf.key);
                v.live_asked.push((sdf.key, l.asked, l.compile_ms));
            }
            if let Some(t) = v.panes.get_mut(state.view.lead_plate().0 as usize).and_then(|p| p.tiles.as_mut()) {
                if !v.tiles_shown && t.drawn_key() == Some(sdf.key) && t.count() > 0 {
                    v.tiles_shown = true;
                    let n = t.count();
                    log!("fab vp sdf: {} drawn by the engine: {n} tiles, {} materials", v.object, sdf.shape.materials.len());
                }
                if let Some(n) = t.take_settled() {
                    log!("fab vp sdf tiles: {} settled at {n} tiles", v.object);
                }
            }
            if let Some(element) = v.element {
                let up = fab_from_part(v, element, state).invert();
                let selected = state.scene_state.selection.set.iter().any(|e| *e == element);
                let up = [up.v[8] as f64, up.v[9] as f64, up.v[10] as f64];
                for p in &v.panes {
                    if let Some(t) = &p.tiles {
                        t.set_scan(up, !v.cut_away && state.is_drawn(element), selected);
                    }
                    if let Some(t) = &p.ghost_tiles {
                        t.set_scan(up, state.is_ghost_drawn_under(poses, element), selected);
                    }
                }
            }
        }
        let busy = self.parts.iter().flat_map(|v| &v.panes).flat_map(PaneTiles::iter).any(|t| t.busy());
        // Update the paint alongside the field parts.
        let baking = self.paint.sync(cx, state);
        SdfSync { changed, record, busy: record || busy || baking }
    }

    /// A part any pane drew in its last record has more to come from its tracer: a look under way
    /// or owed, tiles not drawn yet, or views of its scan left. Read as it is now, not as of the
    /// last draw.
    pub fn tracing(&self) -> bool {
        self.parts.iter().flat_map(|v| &v.panes).flat_map(PaneTiles::iter).any(|t| t.more_to_come())
    }

    /// A pane's tracer has something for the view (tiles to draw, or work that cannot go on without a
    /// look), or a paint bake ended: the view records and looks again. Each comes with a signal.
    pub fn take_woke(&mut self) -> bool {
        let mut woke = std::mem::take(&mut self.xr_found) | self.paint.take_woke();
        for t in self.parts.iter().flat_map(|v| &v.panes).flat_map(PaneTiles::iter) {
            woke |= t.take_woke();
        }
        woke
    }

    /// How long until any pane is to record and look again though its tracer has nothing for it:
    /// a camera that rests takes the finer tolerance, a shape left alone its one mesh, and a
    /// headset that stops drawing the tiles leaves their camera to the primary pane again. None:
    /// nothing is owed.
    pub fn look_due_in(&self) -> Option<std::time::Duration> {
        let xr_due = self.xr_tiled.map(|t| XR_TILED.saturating_sub(t.elapsed()));
        // XR drives the primary's looks until it lets go; the other panes still owe theirs.
        self.parts.iter()
            .flat_map(|v| v.panes.iter().enumerate())
            .filter(|(pane, _)| xr_due.is_none() || *pane != XR_PANE.0 as usize)
            .flat_map(|(_, p)| p.iter())
            .filter_map(|t| t.look_due_in())
            .chain(xr_due)
            .min()
    }

    /// The view set starts to record its panes' passes: a part a pane does not look at in them
    /// owes it nothing. Call once before recording the set, not again between its panes.
    pub fn begin_record(&mut self) {
        for t in self.parts.iter_mut().flat_map(|v| &mut v.panes).flat_map(PaneTiles::iter_mut) {
            t.unframe();
        }
    }

    /// Parking a pane removes its casters and drops its tracers, cancelling their work.
    /// The primary tile slot is shared with the headset's eyes. While the headset is drawing,
    /// parking that pane keeps the slot and the headset's session state.
    pub fn drop_pane(&mut self, pane: PaneId) {
        let before = self.casters.len();
        self.casters.retain(|c| c.pane != pane);
        if self.casters.len() != before {
            self.casters_rev = self.casters_rev.wrapping_add(1);
        }
        if pane == XR_PANE {
            if self.xr_tiled.is_some_and(|t| t.elapsed() < XR_TILED) {
                return;
            }
            self.xr_tiled = None;
            self.xr_found = false;
        }
        for v in &mut self.parts {
            if let Some(p) = v.panes.get_mut(pane.0 as usize) {
                *p = PaneTiles::default();
            }
        }
    }

    /// Why a part's field could not be drawn, once (the host shows meshes instead).
    pub fn take_unavailable(&mut self) -> Option<String> {
        self.unavailable.take()
    }

    /// The elements whose meshes the engine replaces.
    pub fn drawn_elements(&self) -> impl Iterator<Item = ElementId> + '_ {
        self.parts.iter().filter(|v| v.ready && !self.as_mesh).filter_map(|v| v.element)
    }

    /// The nearest field part under a ray (plate mm, unit `dir`) among the parts the engine
    /// draws, `pickable` ones only: its element and hit; a part Mechanics or a drag moves is
    /// picked where it shows.
    pub fn pick(&self, state: &AppState, origin: [f64; 3], dir: [f64; 3], pickable: &dyn Fn(ElementId) -> bool) -> Option<(ElementId, PlateHit)> {
        if self.as_mesh {
            return None;
        }
        let mut best: Option<(ElementId, PlateHit)> = None;
        for v in self.parts.iter().filter(|v| v.ready) {
            let Some(element) = v.element else { continue };
            if !pickable(element) {
                continue;
            }
            // (a test print's piece shows; a click goes to the part, through its ghost too)
            let Some(sdf) = (if state.test_cut_shown() { Some(&v.sdf) } else { v.newest.as_ref() }) else { continue };
            let step = v.panes.iter().flat_map(PaneTiles::iter).next().map_or(1.0, |t| t.step());
            // a part Mechanics moves is picked where it shows: the ray goes back into its rest
            // frame (a rigid move keeps `t`), and the normal comes out turned by the pose
            let pose = drawn_pose(state, element);
            let (o, d) = match pose {
                Some(a) => {
                    let back = |p: [f64; 3], translate: bool| -> [f64; 3] {
                        let q = if translate { [p[0] - a[0][3] as f64 * 1000.0, p[1] - a[1][3] as f64 * 1000.0, p[2] - a[2][3] as f64 * 1000.0] } else { p };
                        std::array::from_fn(|i| (0..3).map(|k| a[k][i] as f64 * q[k]).sum())
                    };
                    (back(origin, true), back(dir, false))
                }
                None => (origin, dir),
            };
            if let Some(mut h) = pick_plate(sdf, o, d, step) {
                if let Some(a) = pose {
                    let n = h.normal;
                    h.normal = std::array::from_fn(|k| (0..3).map(|i| a[k][i] as f64 * n[i]).sum());
                }
                if best.is_none_or(|(_, b)| h.t < b.t) {
                    best = Some((element, h));
                }
            }
        }
        best
    }

    /// The tracer's kernel scale (1 without a tracer): a pick's step. The field's scale is
    /// independent of the pane camera; use the first pane with a tracer, primary first.
    pub fn kernel_scale(&self, object: &str) -> f64 {
        let Some(v) = self.parts.iter().find(|v| v.object == object) else { return 1.0 };
        v.panes.iter().flat_map(PaneTiles::iter).next().map_or(1.0, |t| t.step())
    }

    /// The elements this layer picks on their fields ([`SdfLayer::pick`]).
    pub fn picks(&self, _state: &AppState, element: ElementId) -> bool {
        !self.as_mesh && self.parts.iter().any(|v| v.ready && v.element == Some(element))
    }

    /// The objects drawn as fields: a headset hides
    /// their meshes.
    pub fn field_objects(&self) -> Vec<String> {
        if self.as_mesh {
            return Vec::new();
        }
        self.parts.iter().filter(|v| v.ready && v.element.is_some()).map(|v| v.object.clone()).collect()
    }

    /// Whether `object` shows as its mesh instead of its field: the mesh look, or a field the
    /// tracer cannot draw.
    pub fn shows_mesh(&self, object: &str) -> bool {
        self.as_mesh || self.parts.iter().any(|v| v.object == object && v.failed.is_some() && !v.ready)
    }

    /// Draws the ready parts for `pane` into the current pass. `world_from_fab` takes
    /// Fab's scene frame to the pane's pass camera world; `planes` are the
    /// section planes in Fab's frame. Aux writes the G-buffer and depth; lit shades the tiles.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, cx: &mut Cx2d, pane: PaneId, pass: SdfPass, view: &Mat4f, proj: &Mat4f, size: DVec2, world_from_fab: Mat4f, planes: &[[f32; 4]], state: &AppState) -> u32 {
        let mut calls = 0;
        let mut pass = pass;
        let as_mesh = self.as_mesh;
        let ghosts = matches!(pass, SdfPass::GhostAux(..));
        let whole_ghosts = ghosts && state.test_cut_shown();
        let xr_active = self.xr_tiled.is_some_and(|t| t.elapsed() < XR_TILED);
        if !xr_active {
            self.xr_tiled = None;
        }
        let headset = pane == XR_PANE && xr_active;
        let pane_index = pane.0 as usize;
        // The shaders take at most six section planes. Transform once for the pane's pass,
        // on the stack, rather than allocating a vector for every part it draws.
        let mut world_planes = [[0.0; 4]; 6];
        let plane_count = planes.len().min(world_planes.len());
        for (out, plane) in world_planes.iter_mut().zip(world_plane_iter(planes, &world_from_fab)) {
            *out = plane;
        }
        let world_planes = &world_planes[..plane_count];
        let poses = state.pane_poses(pane);
        for v in &mut self.parts {
            let Some(element) = v.element else { continue };
            if !v.ready || as_mesh || !(if ghosts { state.is_ghost_drawn_under(poses, element) } else { state.is_drawn_in(element, pane) }) {
                continue;
            }
            let fab_from = fab_from_part(v, element, state);
            let world_from_obj = Mat4f::mul(&world_from_fab, &fab_from);
            let id = (element.index() + 1) as f32;

            if v.cut_away && !whole_ghosts {
                continue;
            }
            match &mut pass {
                SdfPass::Aux(tile) | SdfPass::GhostAux(tile) => {
                    let Some(newest) = (if whole_ghosts { Some(&v.sdf) } else { v.newest.as_ref() }) else { continue };
                    let pool = cx.cx.task_pool();
                    let cam = Cam::new(view, proj, &world_from_obj, size.x, size.y);
                    if v.panes.len() <= pane_index {
                        v.panes.resize_with(pane_index + 1, PaneTiles::default);
                    }
                    let p = &mut v.panes[pane_index];
                    let tiles = if whole_ghosts { &mut p.ghost_tiles } else { &mut p.tiles }.get_or_insert_with(|| Tiles::in_queue(&self.scan));
                    tiles.set_drag_detail(state.view.drag_detail);
                    let one_sided = tiles.one_sided();
                    let inst = if headset { tiles.latest() } else { tiles.frame(&pool, cam, newest.key, &newest.shape) };
                    // paint is a material through the volume: a cut through a painted part shows it
                    let cut_painted = self.paint.binding().and_then(|b| b.sdf_header(&v.object)).is_some();
                    draw_tiles(cx, tile, inst, (!headset).then_some(&cam), one_sided, proj, size.y as f32, &world_from_obj, world_planes, id, cut_painted);
                    // in this frame, as the primary pane's tiles draw it
                    if pane == state.view.lead_plate() {
                        if let Some(at) = v.live_asked.iter().position(|w| tiles.drawn_key() == Some(w.0)) {
                            let (_, asked, compile_ms) = v.live_asked[at];
                            log!("fab vp sdf live: {} edit-to-frame {:.1} ms (compile {compile_ms:.1}), {at} older versions never drawn", v.object, asked.elapsed().as_secs_f64() * 1e3);
                            v.live_asked.drain(..=at);
                        }
                        if tiles.drawn_key() == Some(newest.key) {
                            v.live_asked.clear();
                        }
                    }
                }
                SdfPass::TileLit { draw, light, accent } => {
                    v.sync_tile_table(cx.cx);
                    let tint = tint_of(state, &v.object);
                    let Some(p) = v.panes.get_mut(pane_index).filter(|p| p.tiles.is_some()) else { continue };
                    p.sync_highlight(cx.cx, &tint);
                    let p = &v.panes[pane_index];
                    let Some(t) = p.tiles.as_ref() else { continue };
                    let (inst, one_sided) = (t.drawn(), t.one_sided());
                    let cam = Cam::new(view, proj, &world_from_obj, size.x, size.y);
                    let paint = self.paint.binding().and_then(|b| b.sdf_header(&v.object).map(|h| (&b.texture, b.ctl, h)));
                    let look = TileLook { highlight: p.highlight(*accent), ..v.tile_look() };
                    draw_tiles_lit(cx, draw, inst, (!headset).then_some(&cam), one_sided, proj, size.y as f32, &world_from_obj, world_planes, look, paint, light);
                }
            }
            calls += 1;
        }
        calls
    }
}


impl SdfLayer {
    /// The parts in a headset's XR pass, drawn with its cameras, once per eye or multiview: `eyes`
    /// as its uniforms carry them, `eye_px` each one's size. Shares the primary pane's tiles;
    /// other panes keep tracing from their own cameras. How many drew.
    pub fn stereo_tiles(&mut self, cx: &mut Cx2d, draw: &mut DrawSdfTileLit, light: &TileLight, eyes: &[(Mat4f, Mat4f); 2], eye_px: DVec2, world_from: &dyn Fn(&str) -> Option<Mat4f>, state: &AppState) -> usize {
        self.xr_tiled = Some(std::time::Instant::now());
        let planes = view_planes(state);
        let pool = cx.cx.task_pool();
        let mut drawn = 0;
        for v in &mut self.parts {
            let Some(element) = v.element else { continue };
            if !v.ready || v.cut_away || !state.is_drawn(element) {
                continue;
            }
            let (Some(world_from_fab), Some(newest)) = (world_from(&v.object), v.newest.clone()) else { continue };
            let world_from_obj = Mat4f::mul(&world_from_fab, &fab_from_part(v, element, state));
            let cams = [0, 1].map(|e| Cam::of_eye(&eyes[e].0, &eyes[e].1, &world_from_obj, eye_px.x, eye_px.y));
            let primary = XR_PANE.0 as usize;
            if v.panes.len() <= primary {
                v.panes.resize_with(primary + 1, PaneTiles::default);
            }
            let tiles = v.panes[primary].tiles.get_or_insert_with(|| Tiles::in_queue(&self.scan));
            tiles.set_drag_detail(state.view.drag_detail);
            let rev = tiles.drawn_rev();
            tiles.frame_eyes(&pool, cams, newest.key, &newest.shape);
            if tiles.drawn_rev() != rev && !self.xr_found {
                // the desktop's primary pane shows them too
                self.xr_found = true;
                makepad_widgets_core::makepad_platform::SignalToUI::set_ui_signal();
            }
            v.sync_tile_table(cx.cx);
            let Some(t) = v.panes[primary].tiles.as_ref() else { continue };
            let paint = self.paint.binding().and_then(|b| b.sdf_header(&v.object).map(|h| (&b.texture, b.ctl, h)));
            let world_planes = planes_in_world(planes.as_slice(), &world_from_fab);
            draw_tiles_lit(cx, draw, t.drawn(), None, t.one_sided(), &eyes[0].1, eye_px.y as f32, &world_from_obj, &world_planes, v.tile_look(), paint, light);
            drawn += 1;
        }
        drawn
    }
}

impl SdfLayer {
    /// Who casts into the sun's cascades this frame: the parts the panes' lit passes draw, as
    /// they show, from each pane's traced mesh, which its scan fills from `to_sun` first.
    pub fn set_casters(&mut self, state: &AppState, world_from_fab: &Mat4f, to_sun: [f32; 3]) {
        use std::hash::{Hash, Hasher};
        let mut rev = std::collections::hash_map::DefaultHasher::new();
        self.casters.clear();
        self.caster_planes = planes_in_world(view_planes(state).as_slice(), world_from_fab);
        self.caster_planes.iter().for_each(|p| p.map(f32::to_bits).hash(&mut rev));
        let poses = state.shared_poses();
        for (part, v) in self.parts.iter().enumerate() {
            let Some(element) = v.element else { continue };
            // (a test print's cut: its whole part is a ghost, and a ghost casts nothing)
            if self.as_mesh || !v.ready || v.cut_away || !state.is_drawn(element) || state.is_ghost_drawn_under(poses, element) {
                continue;
            }
            let Some(newest) = v.newest.as_ref() else { continue };
            if !v.panes.iter().any(|p| p.tiles.is_some()) {
                continue;
            }
            let world_from_obj = Mat4f::mul(world_from_fab, &fab_from_part(v, element, state));
            let (mut lo, mut hi) = (vec3(f32::MAX, f32::MAX, f32::MAX), vec3(f32::MIN, f32::MIN, f32::MIN));
            let m = &world_from_obj.v;
            for bits in 0..8 {
                let c = [0, 1, 2].map(|i| if bits >> i & 1 == 0 { newest.shape.min[i] } else { newest.shape.max[i] } as f32);
                let at = [0, 1, 2].map(|r| m[r] * c[0] + m[4 + r] * c[1] + m[8 + r] * c[2] + m[12 + r]);
                lo = vec3(lo.x.min(at[0]), lo.y.min(at[1]), lo.z.min(at[2]));
                hi = vec3(hi.x.max(at[0]), hi.y.max(at[1]), hi.z.max(at[2]));
            }
            let o = world_from_obj.invert().v;
            let to_light = [0, 1, 2].map(|r| (o[r] * to_sun[0] + o[4 + r] * to_sun[1] + o[8 + r] * to_sun[2]) as f64);
            for (pane, p) in v.panes.iter().enumerate() {
                let Some(tiles) = &p.tiles else { continue };
                let pane = PaneId(pane as u16);
                tiles.set_to_light(to_light);
                (part, pane, tiles.drawn_rev(), world_from_obj.v.map(f32::to_bits)).hash(&mut rev);
                self.casters.push(Caster { part, pane, world_from_obj, lo, hi });
            }
        }
        self.casters_rev = rev.finish();
    }
}

impl CascadeCasters for SdfLayer {
    fn revision(&self) -> u64 {
        self.casters_rev
    }

    fn draw_cascade(&mut self, cx: &mut CxDraw, at: &CascadeTile) -> usize {
        if self.casters.is_empty() {
            return 0;
        }
        if self.tile_shadow.is_none() {
            self.tile_shadow = cx.cx.try_with_vm(|vm| DrawSdfTileShadow::script_new_with_default(vm));
        }
        let Some(draw) = self.tile_shadow.as_mut() else { return 0 };
        let c = at.cascade;
        let row = |r: Vec4f| [r.x, r.y, r.z, r.w];
        let sun = TileSun { rx: row(c.rx), ry: row(c.ry), rz: row(c.rz), tile: row(at.tile), depth: [at.depth.0, at.depth.1], scissor: at.scissor };
        let mut draws = 0;
        for caster in &self.casters {
            if !cascade_overlaps(c, caster.lo, caster.hi) || !cascade_sees(c, caster.lo, caster.hi) || !cascade_shadow_in_view(c, caster.lo, caster.hi) {
                continue;
            }
            // its shadow ends on the plate, at height 0
            if at.finer.is_some_and(|f| cascade_holds_shadow(f, caster.lo, caster.hi, caster.lo.y.max(0.0))) {
                continue;
            }
            let Some(tiles) = self.parts[caster.part].panes.get(caster.pane.0 as usize).and_then(|p| p.tiles.as_ref()) else { continue };
            let keep = |chunk: &Chunk| {
                let (lo, hi) = world_box(&caster.world_from_obj, &chunk.quads);
                cascade_overlaps(c, lo, hi) && cascade_sees(c, lo, hi) && cascade_shadow_in_view(c, lo, hi) && !at.finer.is_some_and(|f| cascade_holds_shadow(f, lo, hi, lo.y.max(0.0)))
            };
            draws += draw_tiles_shadow(cx, draw, tiles.drawn(), keep, &sun, &caster.world_from_obj, &self.caster_planes);
        }
        draws
    }
}

/// Section planes in Fab's frame, into the pass world with `world_from_fab`.
pub(super) fn planes_in_world(planes: &[[f32; 4]], world_from_fab: &Mat4f) -> Vec<[f32; 4]> {
    world_plane_iter(planes, world_from_fab).collect()
}

/// The same transform without storage, for a pane's stack-backed pass planes.
fn world_plane_iter<'a>(planes: &'a [[f32; 4]], world_from_fab: &Mat4f) -> impl Iterator<Item = [f32; 4]> + 'a {
    let inv = world_from_fab.invert();
    planes
        .iter()
        .map(move |p| {
            let m = &inv.v;
            let mut o = [0.0f32; 4];
            for j in 0..3 {
                o[j] = p[0] * m[j * 4] + p[1] * m[j * 4 + 1] + p[2] * m[j * 4 + 2];
            }
            o[3] = p[0] * m[12] + p[1] * m[13] + p[2] * m[14] + p[3];
            o
        })
}

/// Which pass the parts draw into.
pub enum SdfPass<'a> {
    /// Tile depth and normals in the G-buffer.
    Aux(&'a mut DrawSdfTile),
    /// Ghost parts in their own G-buffer.
    GhostAux(&'a mut DrawSdfTile),
    /// Material and paint under the scene light; the features picked or under the pointer mix
    /// toward `accent`.
    TileLit { draw: &'a mut DrawSdfTileLit, light: &'a TileLight, accent: Vec4f },
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_sdf_engine::makepad_csg_sdf::{distance, part_id, SdfNode, SdfOp, SdfShape};
    use fab_sdf_tiles::trace::under_section;
    use makepad_csg_math::Vec3d;

    /// The field a cut leaves is the part's and the planes' true distances, whatever way a plane
    /// faces: what the rays walk and what proves emptiness stand on that. A plane past the part
    /// leaves it alone, and one with the part behind it leaves nothing.
    #[test]
    fn a_cut_is_the_part_and_its_planes_and_nothing_else() {
        let root = SdfNode::Sphere { radius: 10.0 };
        let shape = SdfShape { root, min: [-10.0; 3], max: [10.0; 3], materials: Vec::new() };
        let key = shape.key();
        let sdf = PartSdf { shape: Arc::new(shape), to_printer: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]], key };
        let k = 1.0 / 3f64.sqrt();
        let (a, b, c) = ([k, -k, k, 3.0], [0.0, 0.0, -1.0, 2.0], [0.0, 1.0, 0.0, 0.5]);
        let cut = cut_part(&sdf, &[a, b, c]).expect("something is left");
        assert_ne!(cut.key, key);
        let plane = |p: &[f64; 4], q: [f64; 3]| -(p[0] * q[0] + p[1] * q[1] + p[2] * q[2] + p[3]);
        for i in 0..2000 {
            let f = |m: u32| (((i * m + 17) % 241) as f64 / 240.0 - 0.5) * 30.0;
            let q = [f(13), f(29), f(53)];
            let want = (Vec3d::new(q[0], q[1], q[2]).length() - 10.0).max(plane(&a, q)).max(plane(&b, q)).max(plane(&c, q));
            let got = distance(&cut.shape.root, Vec3d::new(q[0], q[1], q[2]));
            assert!((got - want).abs() < 1e-9, "at {q:?}: {got} against {want}");
        }
        // the tiles know the section's bodies from the part
        assert!(under_section(&cut.shape.root).is_some_and(|(_, bodies)| bodies.len() == 3) && under_section(&sdf.shape.root).is_none());
        assert_eq!(cut_part(&sdf, &[[1.0, 0.0, 0.0, 10.0]]).expect("the whole part").key, key);
        assert!(cut_part(&sdf, &[[1.0, 0.0, 0.0, -10.0]]).is_none());
    }

    #[test]
    fn a_plate_ray_picks_the_feature_on_a_placed_part() {
        let ball = |r: f64, at: [f64; 3], name: &str| SdfNode::Id { id: part_id(name), of: Box::new(SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Sphere { radius: r }) }) };
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![ball(5.0, [0.0; 3], "body"), ball(2.0, [0.0, 6.0, 0.0], "boss")] };
        let shape = SdfShape { root, min: [-5.0, -5.0, -5.0], max: [5.0, 8.0, 5.0], materials: Vec::new() };
        // shape Y up -> plate Z up, scaled 2, moved to (100, 50, 10)
        let sdf = PartSdf { shape: std::sync::Arc::new(shape), to_printer: [[2.0, 0.0, 0.0, 100.0], [0.0, 0.0, -2.0, 50.0], [0.0, 2.0, 0.0, 10.0]], key: [0; 32] };
        let PlateHit { t, id, normal } = pick_plate(&sdf, [100.0, 50.0, 100.0], [0.0, 0.0, -1.0], 1.0).expect("a hit");
        assert!((normal[2] - 1.0).abs() < 1e-6, "the boss top faces up: {normal:?}");
        assert_eq!(id, Some(part_id("boss")));
        // the boss top is at shape y 8 -> plate z 10 + 16 = 26
        assert!((t - 74.0).abs() < 1e-3, "t {t}");
        let PlateHit { id, .. } = pick_plate(&sdf, [200.0, 50.0, 10.0], [-1.0, 0.0, 0.0], 1.0).expect("a side hit");
        assert_eq!(id, Some(part_id("body")));

        // The same part's primary tiles serve the headset when its desktop pane parks.
        let mut layer = SdfLayer::default();
        layer.parts.push(PartView {
            object: "body".into(), sdf, ready: true, color: [1.0; 4], element: None,
            panes: vec![PaneTiles { tiles: Some(Tiles::new()), ..Default::default() }],
            failed: None, loop_stop: None, materials: None, materials_key: None, newest: None,
            tile_shape: None, cut_away: false, cut: None, tiles_shown: false, live_shape: None, live_asked: Vec::new(),
        });
        let caster = || Caster { part: 0, pane: XR_PANE, world_from_obj: Mat4f::identity(), lo: Vec3f::default(), hi: Vec3f::default() };
        layer.casters.push(caster());
        layer.xr_found = true;
        let last_frame = std::time::Instant::now();
        layer.xr_tiled = Some(last_frame);
        layer.drop_pane(XR_PANE);
        assert!(layer.parts[0].panes[0].tiles.is_some(), "parking the primary pane keeps the headset's tiles");
        assert!(layer.casters.is_empty(), "parking removes the pane's caster while the headset draws");
        assert_eq!(layer.xr_tiled, Some(last_frame), "parking keeps the headset's last frame time");
        assert!(layer.xr_found, "parking preserves the headset's pending wake");

        layer.casters.push(caster());
        layer.xr_tiled = Some(last_frame - XR_TILED * 2);
        layer.drop_pane(XR_PANE);
        assert!(layer.parts[0].panes[0].tiles.is_none() && layer.casters.is_empty(), "after XR expires, parking drops both tiles and caster");
        assert!(layer.xr_tiled.is_none() && !layer.xr_found, "parking clears the expired headset state");
    }

    /// What the view shows: per pixel (rows top down) the nearest of the tiles' triangles over
    /// its centre, interpolated as the GPU does through the camera's matrices, a point of the
    /// plate (mm); None where no tile covers it.
    fn rasterise(tiles: &Tiles, sdf: &PartSdf, to_clip: &Mat4f, w: usize, h: usize) -> Vec<Option<[f64; 3]>> {
        let mut out: Vec<Option<(f64, [f64; 3])>> = vec![None; w * h];
        for chunk in tiles.drawn().iter().flatten() {
            for t in chunk.data().chunks_exact(fab_sdf_tiles::trace::TILE_SLOTS) {
                let at = |k: usize| [t[k * 4] as f64, t[k * 4 + 1] as f64, t[k * 4 + 2] as f64];
                for k in 0..4 {
                    let tri = [at(4), at(k), at((k + 1) % 4)];
                    let clip = tri.map(|p| to_clip.transform_vec4(vec4(p[0] as f32, p[1] as f32, p[2] as f32, 1.0)));
                    if clip.iter().any(|c| c.w <= 0.0) {
                        continue;
                    }
                    // in pixels, rows down
                    let s = clip.map(|c| [((c.x / c.w) as f64 * 0.5 + 0.5) * w as f64, (0.5 - (c.y / c.w) as f64 * 0.5) * h as f64, (c.z / c.w) as f64]);
                    let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
                    if area.abs() < 1e-12 {
                        continue;
                    }
                    let lo = |i: usize| s.iter().map(|v| v[i]).fold(f64::MAX, f64::min).floor().max(0.0) as usize;
                    let hi = |i: usize, n: usize| (s.iter().map(|v| v[i]).fold(f64::MIN, f64::max).ceil().max(0.0) as usize).min(n);
                    for y in lo(1)..hi(1, h) {
                        for x in lo(0)..hi(0, w) {
                            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                            let edge = |a: [f64; 3], b: [f64; 3]| ((b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1])) / area;
                            let b = [edge(s[1], s[2]), edge(s[2], s[0]), edge(s[0], s[1])];
                            if b.iter().any(|v| *v < -1e-9) {
                                continue;
                            }
                            let z = b[0] * s[0][2] + b[1] * s[1][2] + b[2] * s[2][2];
                            if out[y * w + x].is_some_and(|(nearest, _)| nearest <= z) {
                                continue;
                            }
                            // perspective-correct, as the rasteriser interpolates
                            let q: [f64; 3] = std::array::from_fn(|i| b[i] / clip[i].w as f64);
                            let sum = q[0] + q[1] + q[2];
                            let p: [f64; 3] = std::array::from_fn(|j| (q[0] * tri[0][j] + q[1] * tri[1][j] + q[2] * tri[2][j]) / sum);
                            let m = &sdf.to_printer;
                            out[y * w + x] = Some((z, std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])));
                        }
                    }
                }
            }
        }
        out.into_iter().map(|p| p.map(|(_, p)| p)).collect()
    }

    /// The still view's tiles of `sdf` from `cam`, traced to their end.
    fn traced(sdf: &PartSdf, cam: &crate::api::Camera, size: DVec2) -> Tiles {
        let pool = makepad_draw::makepad_platform::Cx::new(Box::new(|_, _| {})).task_pool();
        let aspect = (size.x / size.y) as f32;
        let (view, proj) = (cam.view(), cam.projection(aspect));
        let world_from_obj = fab_from_shape(sdf, vec3(0.0, 0.0, 0.0));
        let mut tiles = Tiles::new();
        let mut frames = 0;
        while tiles.take_settled().is_none() {
            assert!(frames < 20_000, "the still view never settled");
            tiles.frame(&pool, Cam::new(&view, &proj, &world_from_obj, size.x, size.y), sdf.key, &sdf.shape);
            std::thread::sleep(std::time::Duration::from_millis(2));
            frames += 1;
        }
        tiles
    }

    /// A box on the bed (mm, the shape's middle at `at`), and the plate's box round it (m).
    fn placed(root: SdfNode, min: [f64; 3], max: [f64; 3], at: [f64; 3]) -> (PartSdf, crate::api::Aabb) {
        let shape = SdfShape { root, min, max, materials: Vec::new() };
        let key = shape.key();
        let sdf = PartSdf { shape: Arc::new(shape), to_printer: [[1.0, 0.0, 0.0, at[0]], [0.0, 1.0, 0.0, at[1]], [0.0, 0.0, 1.0, at[2]]], key };
        let m = |p: [f64; 3]| vec3(((p[0] + at[0]) * 0.001) as f32, ((p[1] + at[1]) * 0.001) as f32, ((p[2] + at[2]) * 0.001) as f32);
        (sdf, crate::api::Aabb { min: m(min), max: m(max) })
    }

    fn block(size: [f64; 3], at: [f64; 3]) -> SdfNode {
        SdfNode::Transform { at, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) }
    }

    /// A pick at a pixel meets what the tile there shows, in both projections: the exact field's
    /// first hit on the camera's ray through the pixel ([`ViewProjector::ray`], what the tools
    /// cast) and the tiles' triangle under it, traced from the same camera's matrices, are one
    /// point of the surface. The view is the closest a perspective view goes to the block's top,
    /// low over it, with a post standing between the block's middle and the eye's plane: behind
    /// the eye in perspective, in the orthographic view's parallel rays, which start behind the
    /// part (the eye stands back), so the post's front face is what the middle of that view shows.
    #[test]
    fn a_pick_meets_what_the_tile_under_it_shows_in_both_projections() {
        // a 60 mm square block 10 mm thick, a 10 mm square post 30 mm tall on its front half
        let root = SdfNode::Combine { op: SdfOp::Union, blend: 0.0, of: vec![block([60.0, 60.0, 10.0], [0.0, 0.0, 5.0]), block([10.0, 10.0, 30.0], [0.0, -25.0, 25.0])] };
        let (sdf, bounds) = placed(root, [-30.0, -30.0, 0.0], [30.0, 30.0, 40.0], [150.0, 150.0, 0.0]);
        let (w, h) = (640usize, 400usize);
        let size = dvec2(w as f64, h as f64);
        let rect = Rect { pos: dvec2(0.0, 0.0), size };
        let aspect = (size.x / size.y) as f32;
        // the block's top in its middle, from 15 degrees over it, the post toward the eye
        let target = vec3(0.15, 0.15, 0.01);
        let back = vec3(0.0, -(15f32.to_radians().cos()), 15f32.to_radians().sin());
        let to_plate = |ray: &crate::api::Ray| ([ray.origin.x as f64 * 1000.0, ray.origin.y as f64 * 1000.0, ray.origin.z as f64 * 1000.0], [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64]);
        for ortho in [false, true] {
            let mut cam = crate::api::Camera { target, eye: target + back * crate::nav::orbit::MIN_DISTANCE, up: vec3(0.0, 0.0, 1.0), ..Default::default() };
            crate::nav::orbit::set_ortho(&mut cam, ortho);
            cam.fit_clip_planes(&bounds, aspect);
            let tiles = traced(&sdf, &cam, size);
            let shown = rasterise(&tiles, &sdf, &Mat4f::mul(&cam.view_projection(aspect), &fab_from_shape(&sdf, vec3(0.0, 0.0, 0.0))), w, h);
            let projector = crate::api::ViewProjector::new(cam, rect);
            let (mut both, mut apart, mut one) = (0, Vec::new(), 0);
            for j in 0..25 {
                for i in 0..40 {
                    let (x, y) = (8 + 16 * i, 8 + 16 * j);
                    let px = dvec2(x as f64 + 0.5, y as f64 + 0.5);
                    let (o, d) = to_plate(&projector.ray(px));
                    let pick = pick_plate(&sdf, o, d, 1.0).map(|hit| std::array::from_fn::<f64, 3, _>(|k| o[k] + d[k] * hit.t));
                    match (pick, shown[y * w + x]) {
                        (Some(p), Some(t)) => {
                            let on = distance(&sdf.shape.root, Vec3d::new(p[0] - 150.0, p[1] - 150.0, p[2]));
                            assert!(on.abs() < 1e-3, "ortho {ortho}: the pick at {px:?} is {on} mm off the surface at {p:?}");
                            both += 1;
                            apart.push((0..3).map(|k| (p[k] - t[k]).powi(2)).sum::<f64>().sqrt());
                        }
                        (None, None) => {}
                        _ => one += 1,
                    }
                }
            }
            apart.sort_by(f64::total_cmp);
            let (median, p95) = (apart[apart.len() / 2], apart[apart.len() * 95 / 100]);
            eprintln!("ortho {ortho}: {both} pixels both hit, {one} one alone, the pick and the tile apart {median:.4} mm (median), {p95:.4} (95th)");
            assert!(both > 500, "ortho {ortho}: the part fills the view: {both} of 1000");
            assert!(one * 50 <= both, "ortho {ortho}: {one} pixels show the part without a pick there, or the other way");
            assert!(median < 0.01 && p95 < 0.05, "ortho {ortho}: the pick and the tile apart {median} mm (median), {p95} (95th)");
            // the middle pixel: the block's top in perspective, the post's front face orthographic
            let (o, d) = to_plate(&projector.ray(size * 0.5));
            let hit = pick_plate(&sdf, o, d, 1.0).expect("the middle hits");
            let y = o[1] + d[1] * hit.t - 150.0;
            if ortho {
                assert!((y + 30.0).abs() < 1e-3 && hit.normal[1] < -0.999, "orthographic: the middle shows the post's front face, not y {y} mm, {:?}", hit.normal);
            } else {
                assert!(y.abs() < 1e-3 && hit.normal[2] > 0.999, "perspective: the middle shows the block's top, not y {y} mm, {:?}", hit.normal);
            }
        }
    }

    /// A box seen face on, off the middle of the view: orthographic, its tiles show its front face
    /// alone, a rectangle of its size whose edges run straight along the rows and columns (no
    /// perspective taper); in perspective the same view sees its side face too, the side's top and
    /// bottom edges converging.
    #[test]
    fn a_box_face_on_is_a_rectangle_orthographic() {
        let (sdf, bounds) = placed(block([40.0, 30.0, 20.0], [0.0; 3]), [-20.0, -15.0, -10.0], [20.0, 15.0, 10.0], [150.0, 150.0, 10.0]);
        let (w, h) = (640usize, 400usize);
        let size = dvec2(w as f64, h as f64);
        let aspect = (size.x / size.y) as f32;
        // level with the box's middle, 25 mm to its right, 100 mm in front of it
        let target = vec3(0.175, 0.15, 0.01);
        for ortho in [true, false] {
            let mut cam = crate::api::Camera { target, eye: target - vec3(0.0, 0.1, 0.0), up: vec3(0.0, 0.0, 1.0), ..Default::default() };
            crate::nav::orbit::set_ortho(&mut cam, ortho);
            cam.fit_clip_planes(&bounds, aspect);
            let tiles = traced(&sdf, &cam, size);
            let shown = rasterise(&tiles, &sdf, &Mat4f::mul(&cam.view_projection(aspect), &fab_from_shape(&sdf, vec3(0.0, 0.0, 0.0))), w, h);
            let rows: Vec<(usize, usize, usize)> = (0..h).filter_map(|y| {
                let xs: Vec<usize> = (0..w).filter(|&x| shown[y * w + x].is_some()).collect();
                Some((y, *xs.first()?, *xs.last()?))
            }).collect();
            let cols: Vec<(usize, usize, usize)> = (0..w).filter_map(|x| {
                let ys: Vec<usize> = (0..h).filter(|&y| shown[y * w + x].is_some()).collect();
                Some((x, *ys.first()?, *ys.last()?))
            }).collect();
            let spread = |v: &[(usize, usize, usize)], k: usize| {
                let it = || v.iter().map(|r| if k == 1 { r.1 } else { r.2 });
                it().max().unwrap_or(0) - it().min().unwrap_or(0)
            };
            let behind = shown.iter().flatten().filter(|p| p[1] > 135.5).count();
            eprintln!("ortho {ortho}: {} rows, {} columns; left/right edges spread {}/{} px, top/bottom {}/{} px; {behind} pixels behind the front face", rows.len(), cols.len(), spread(&rows, 1), spread(&rows, 2), spread(&cols, 1), spread(&cols, 2));
            if ortho {
                // 40 x 20 mm of a view cam.ortho_height mm high, 400 px
                let px = h as f64 / (cam.ortho_height as f64 * 1000.0);
                assert!((cols.len() as f64 - 40.0 * px).abs() <= 1.5 && (rows.len() as f64 - 20.0 * px).abs() <= 1.5, "orthographic: the face is {} x {} px, not {:.1} x {:.1}", cols.len(), rows.len(), 40.0 * px, 20.0 * px);
                assert!(spread(&rows, 1) <= 1 && spread(&rows, 2) <= 1 && spread(&cols, 1) <= 1 && spread(&cols, 2) <= 1, "orthographic: the face's edges are not straight");
                assert_eq!(behind, 0, "orthographic: a face-on view shows the box's sides");
            } else {
                assert!(behind > 100 && spread(&cols, 1) > 3, "perspective: the side face shows, its edges converging");
            }
        }
    }
}
