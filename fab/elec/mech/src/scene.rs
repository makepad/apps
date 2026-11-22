//! The electronics contribution to Fab's one scene (contract C14, ARCHITECTURE 2.5 and 5.15).
//!
//! Boards, component bodies and (later) cables as Fab parts: EMECH owns the geometry (outline,
//! thickness, holes, poses, body shapes), E3D owns how it is drawn (Fab's viewport: the layered
//! board material, body materials, view sets). The scene is plain data and a pure function of a
//! design snapshot and a pose: no GPU handles and no textures. A board's look comes from its layer
//! art, which EBOARD rasterises into per-layer coverage tiles shared by the 2D board view and the
//! 3D view; the scene names them by unit and content hash ([`LayerArtSource`]) and carries the
//! stackup the layered material composes them with.
//!
//! Performance: `scene_of` runs on the pool when the design or the pose changes. It is
//! O(units + placements + pads + vias + holes), plus one content hash per board (its art key) and
//! one per circuit (the scene key). Nothing here runs per frame. A pose-only change (an assembly
//! slider, a mechanism) changes `SceneBoard::pose` and nothing else, and leaves `ElecScene::hash`
//! as it was: the viewport moves instances and uploads nothing.

use crate::Pose;
use fab_elec_board::{Board, ConnItem, Placed, Stackup};
pub use fab_elec_board::DrillHole as SceneDrill;
use fab_elec_core::{
    Angle, BBox, BoardId, Canon, CompRef, ContentHash, Contour, FeatureRef, ItemId, LayerId, Nm, PadIx, Path, Sel, Side, Writer, Xform3, P,
};
use fab_elec_design::{Design, Frame, LinkEnd, Unit, UnitKind};
use fab_elec_parts::{footprint::PAD_POLY_ERR, BodyPort, BodySource, Emitter, Footprint, Gfx, PackageKind, PadKind, PadNo, PartKind};
use fab_materials::part_look::PartClass;
use fab_materials::{Colour, FinishId, MaterialId, Surface};

/// The height of the box a part with no 3D body is drawn as: it shows where the part sits, never
/// what it is. A part library states a body (`Body3d`) for the real height.
pub const NO_BODY_HEIGHT: Nm = 500_000;

/// Everything electronic in the scene for one design snapshot at one pose.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ElecScene {
    pub boards: Vec<SceneBoard>,
    /// The content of every board and its circuit, poses left out: equal hashes, the viewport
    /// keeps its geometry and textures and only moves instances.
    pub hash: ContentHash,
}

/// One board, as a Fab part that is not printed.
#[derive(Clone, PartialEq, Debug)]
pub struct SceneBoard {
    pub unit: BoardId,
    /// The unit's name: what a case part linked `board:<name>` names (its host draws this board
    /// in that part's frame).
    pub name: String,
    /// The board's frame in the world (nm). X right and Y up on the board, Z out of the top side;
    /// the bottom face lies at z = 0 and the top face at z = `thickness`.
    pub pose: Xform3,
    /// Even-odd contours in the board frame: the outer edge, cutouts inside it, islands inside
    /// those (the board's outline followed by its cutout items).
    pub outline: Vec<Contour>,
    pub thickness: Nm,
    pub art: LayerArtSource,
    /// Every drilled hole (board holes, vias, through-hole pads), board frame: the barrels the 3D
    /// view draws. Board holes, slots and unplated drills are cut out of the face mesh; vias and
    /// plated round pad drills are cut by the drill coverage of the art.
    pub holes: Vec<SceneHole>,
    pub bodies: Vec<SceneBody>,
    pub pads: Vec<ScenePad>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SolderPadKind {
    Smd,
    Thru,
}

/// One outer copper face of a fitted part's pad, in the board frame.
#[derive(Clone, PartialEq, Debug)]
pub struct ScenePad {
    pub sel: Sel,
    pub no: PadNo,
    pub kind: SolderPadKind,
    pub side: Side,
    /// A through pad has a different joint on the face away from the placement.
    pub component_side: Side,
    pub outline: Contour,
    /// Further rings of custom copper, each island's outer followed by its clockwise holes.
    pub other_rings: Vec<Contour>,
    pub centre: P,
    pub rot: Angle,
    pub drill: Option<SceneDrill>,
    /// Stencil aperture on this face, in square nm.
    pub paste_area: f64,
}

/// What drilled a hole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoleKind {
    /// A footprint pad's drill.
    Pad,
    Via,
    /// A board hole item: a mounting or tooling hole.
    Board,
}

/// A drilled hole.
#[derive(Clone, PartialEq, Debug)]
pub struct SceneHole {
    /// The hole item, the via, or the pad (`FeatureRef::Pad`).
    pub sel: Sel,
    pub kind: HoleKind,
    pub at: P,
    /// Diameter; a slot's width.
    pub d: Nm,
    /// A slot's other end (`at` is the first).
    pub slot_to: Option<P>,
    pub plated: bool,
}

/// A component's body (later: cables and off-board parts).
#[derive(Clone, PartialEq, Debug)]
pub struct SceneBody {
    /// The component (`Sel::Comp`): hover, selection and cross-probe.
    pub sel: Sel,
    pub mesh: MeshRef,
    /// The part's kind, which the material table reads.
    pub class: PartClass,
    /// What the part states its body is made of (material, finish, colour). `None` while the part
    /// library states nothing: the view draws its default for the mesh kind.
    pub surface: Option<Surface>,
    /// In the board's frame; a bottom-side part's is turned over below the board.
    pub pose: Xform3,
    /// A light source's emitting face (LEDs): the view makes it glow by the part's current.
    pub emitter: Option<Emitter>,
    /// Ports in the mesh's body frame, before the part offset already included in `pose`.
    /// Applying `pose` gives the board-frame ports of `board_mech`.
    pub ports: Vec<BodyPort>,
}

/// Where a body's mesh comes from: the part's `Body3d::source`, resolved.
#[derive(Clone, PartialEq, Debug)]
pub enum MeshRef {
    /// A box (body frame, nm): a part with no body, at the footprint's extent.
    Box { min: [Nm; 3], max: [Nm; 3] },
    /// The courtyard extruded from the board surface to `height` (body frame).
    Extrude { outline: Contour, height: Nm },
    /// An object script of the part library, by its binding (meshed by Fab's evaluator).
    Script(String),
    /// A mesh file of the part library, by its library path.
    Mesh(String),
    /// A package family's generated body (body frame = footprint frame, z up from the board).
    Package(PackageBody),
}

/// What a package family's generator reads: the footprint's body box and pads, and the part's
/// height and class. Two parts of the same family and class with the same footprint and height
/// share one generated mesh.
#[derive(Clone, PartialEq, Debug)]
pub struct PackageBody {
    pub kind: PackageKind,
    /// The part's class: which materials a family's bodies take (a chip resistor's alumina and
    /// film, a capacitor's ceramic).
    pub class: PartClass,
    /// Height of the body's top above the board surface.
    pub height: Nm,
    /// The moulded body's box: the footprint's fabrication outline, else the pads' box.
    pub body: [P; 2],
    /// Every SMD pad's copper box, in footprint order.
    pub pads: Vec<PackagePad>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PackagePad {
    pub no: PadNo,
    pub at: P,
    pub min: P,
    pub max: P,
}

impl PackageBody {
    /// The body of a `kind` package with footprint `fp`, `height` tall.
    pub fn of(kind: PackageKind, class: PartClass, fp: &Footprint, height: Nm) -> PackageBody {
        let pads: Vec<PackagePad> = fp.pads.iter().filter(|pad| pad.kind == PadKind::Smd).map(|pad| {
            let b = pad.stack.front.shape(pad.at, pad.rot).bbox();
            PackagePad { no: pad.no.clone(), at: pad.at, min: b.min, max: b.max }
        }).collect();
        let fab = fp.graphics.iter().filter(|(layer, _)| *layer == LayerId::F_FAB).fold(BBox::EMPTY, |b, (_, g)| match g {
            Gfx::Stroke { path, .. } => b.union(path.bbox()),
            Gfx::Fill(shape) => b.union(shape.bbox()),
        });
        let body = if fab.is_empty() {
            pads.iter().fold(BBox::EMPTY, |b, pad| b.union(BBox::new(pad.min, pad.max)))
        } else {
            fab
        };
        PackageBody { kind, class, height, body: [body.min, body.max], pads }
    }

    /// Pad 1, the package's index corner.
    pub fn pin1(&self) -> Option<&PackagePad> {
        self.pads.iter().find(|pad| pad.no == "1")
    }
}

/// A board's layer art: EBOARD's coverage tiles for this unit (copper, mask opening, silk, drill,
/// paste, copper height; `layer_art(layer, lod)`), and the stackup the layered material composes
/// them with (mask and silk colours, the copper finish, the layer thicknesses).
#[derive(Clone, PartialEq, Debug)]
pub struct LayerArtSource {
    pub unit: BoardId,
    /// Content hash of the board the art is drawn from: a new hash, the view re-samples the tiles
    /// EBOARD's tile generations report as dirty.
    pub hash: ContentHash,
    pub stackup: Stackup,
}

/// The scene of `d` at `pose` (C14): every board unit with its outline, holes and part bodies,
/// placed in the world.
pub fn scene_of(d: &Design, pose: &Pose) -> ElecScene {
    let mut key = Writer::hasher();
    let mut boards = Vec::new();
    for unit in &d.units {
        let (UnitKind::Board, Some(board)) = (&unit.kind, &unit.board) else { continue };
        let art_hash = board.content_hash();
        art_hash.canon(&mut key);
        unit.circuit.content_hash().canon(&mut key);
        boards.push(SceneBoard {
            unit: unit.id,
            name: unit.name.clone(),
            pose: world_pose(d, pose, unit.id),
            outline: board.outline.iter().cloned().chain(board.cutouts.iter().map(|c| c.contour.clone())).collect(),
            thickness: board.stackup.thickness,
            art: LayerArtSource { unit: unit.id, hash: art_hash, stackup: board.stackup.clone() },
            holes: holes_of(board),
            bodies: bodies_of(unit, board),
            pads: pads_of(unit, board),
        });
    }
    ElecScene { boards, hash: key.finish() }
}

/// A unit's frame in the world: its mount's transform after its parent's frame. No mount: the
/// world origin. A loop of mounts is a located error of the assembly checks; here it stops at the
/// world rather than recursing forever.
pub fn world_pose(d: &Design, pose: &Pose, unit: BoardId) -> Xform3 {
    let mut chain = Vec::new();
    let mut at = Some(unit);
    let mut root = Xform3::IDENTITY;
    while let Some(u) = at.take() {
        let Some(m) = d.mount(u) else { break };
        if chain.len() > d.units.len() {
            break;
        }
        chain.push(m.xform);
        match &m.parent {
            Frame::World => {}
            Frame::Unit(parent) => at = Some(*parent),
            // Until mate frames are specified (E2.18 7) the mount's transform is taken from the
            // unit at the link's other end.
            Frame::Mate(link) => {
                at = d.link(*link).and_then(|l| {
                    let units = [end_unit(&l.a), end_unit(&l.b)];
                    units.into_iter().find(|v| *v != u)
                })
            }
            Frame::Body { mechanism, body } => root = pose.body(mechanism, body),
        }
    }
    chain.iter().rev().fold(root, |parent, x| parent.then(x))
}

fn end_unit(e: &LinkEnd) -> BoardId {
    match e {
        LinkEnd::Comp(c) => c.board,
        LinkEnd::Pin(p) => p.board,
    }
}

fn holes_of(board: &Board) -> Vec<SceneHole> {
    let unit = board.unit;
    // `Board::drills` is the one drill function: pads, vias and holes, in board item order.
    let comps: std::collections::HashMap<_, _> = board.placed.iter().map(|pl| (pl.id, pl.comp)).collect();
    board
        .drills()
        .into_iter()
        .filter_map(|h| {
            let (sel, kind) = match h.item {
                ConnItem::Pad { placed, pad } => (Sel::Feature(FeatureRef::Pad(CompRef { board: unit, comp: *comps.get(&placed)? }, pad)), HoleKind::Pad),
                ConnItem::Via(id) => (Sel::Item(unit, id), HoleKind::Via),
                ConnItem::Hole(id) => (Sel::Item(unit, id), HoleKind::Board),
                ConnItem::Track { .. } | ConnItem::Zone { .. } => return None,
            };
            // a slot along its long axis (`size.x` along `rot` when it is the longer)
            let (w, l) = (h.width(), h.size.x.max(h.size.y));
            let half = (l - w) / 2;
            let (at, slot_to) = if half > 0 {
                let axis = if h.size.x >= h.size.y { P::new(half, 0) } else { P::new(0, half) };
                let axis = axis.rotated(h.rot);
                (h.c.sub(axis), Some(h.c.add(axis)))
            } else {
                (h.c, None)
            };
            Some(SceneHole { sel, kind, at, d: w, slot_to, plated: h.plated })
        })
        .collect()
}

/// Where a placement puts its footprint's frame in the board's (nm): on the top face, or turned
/// over about the board's Y axis onto the bottom face (the footprint's X mirrored, as
/// `Placed::to_board` maps it, and its up pointing down out of the board).
pub fn placement_pose(pl: &Placed, thickness: Nm) -> Xform3 {
    placement_pose_at(pl, pl.at, thickness)
}

/// [`placement_pose`] with the placement's origin at `at`.
fn placement_pose_at(pl: &Placed, at: P, thickness: Nm) -> Xform3 {
    match pl.side {
        Side::Top => Xform3::axis_angle([at.x, at.y, thickness], [0.0, 0.0, 1.0], pl.rot),
        Side::Bottom => Xform3::axis_angle([at.x, at.y, 0], [0.0, 0.0, 1.0], pl.rot)
            .then(&Xform3::axis_angle([0; 3], [0.0, 1.0, 0.0], Angle::HALF)),
    }
}

/// The material table's part class for a library part's kind.
pub fn part_class(kind: &PartKind) -> PartClass {
    match kind {
        PartKind::Ic | PartKind::Analog | PartKind::Mcu | PartKind::Regulator => PartClass::Ic,
        PartKind::Transistor => PartClass::Transistor,
        PartKind::Diode => PartClass::Diode,
        PartKind::Led => PartClass::Led,
        PartKind::Resistor | PartKind::Potentiometer => PartClass::Resistor,
        PartKind::Capacitor => PartClass::Capacitor,
        PartKind::Inductor => PartClass::Inductor,
        PartKind::Crystal => PartClass::Crystal,
        PartKind::Connector => PartClass::Connector,
        PartKind::Switch => PartClass::Switch,
        PartKind::Fuse => PartClass::Fuse,
        PartKind::PowerFlag | PartKind::Other(_) => PartClass::Other,
    }
}

/// The bodies of the placements `moved` (board, item, by: `BoardOp::Move`'s effect, summed per
/// item), each by its component, posed in its board's frame after the move: exactly the
/// `SceneBody::pose` that [`scene_of`] gives for the moved design, with no meshes, outlines or
/// hashes. A view poses a dragged part by it straight from the gesture, on every step, while the
/// design's scene is still on its way. O(moves + units): each placement through the board's item
/// index.
pub fn moved_poses(d: &Design, moved: &[(BoardId, ItemId, P)]) -> Vec<(Sel, Xform3)> {
    let mut sums: std::collections::HashMap<(BoardId, ItemId), P> = std::collections::HashMap::with_capacity(moved.len());
    let mut order = Vec::with_capacity(moved.len());
    for &(board, item, by) in moved {
        sums.entry((board, item)).and_modify(|sum| *sum = sum.add(by)).or_insert_with(|| {
            order.push((board, item));
            by
        });
    }
    let units: std::collections::HashMap<BoardId, &Unit> =
        d.units.iter().filter(|unit| unit.kind == UnitKind::Board).map(|unit| (unit.id, unit)).collect();
    let mut out = Vec::with_capacity(order.len());
    for key in order {
        let Some(unit) = units.get(&key.0) else { continue };
        let Some(board) = unit.board.as_ref() else { continue };
        let Some(pl) = board.placed_item(key.1) else { continue };
        let Some(comp) = shown_comp(unit, pl) else { continue };
        let offset = comp.part.body.as_ref().map_or(Xform3::IDENTITY, |body| body.offset);
        let at = placement_pose_at(pl, pl.at.add(sums[&key]), board.stackup.thickness);
        out.push((Sel::Comp(CompRef { board: unit.id, comp: pl.comp }), at.then(&offset)));
    }
    out
}

/// The placement's component when it has a body in the scene (it exists and is fitted).
fn shown_comp<'a>(unit: &'a Unit, pl: &Placed) -> Option<&'a fab_elec_circuit::Component> {
    unit.circuit.comps.get(pl.comp.ix()).filter(|c| c.id == pl.comp && !c.dnp)
}

fn pads_of(unit: &Unit, board: &Board) -> Vec<ScenePad> {
    let mut out = Vec::new();
    let (copper, n) = (board.copper_set(), board.copper_count());
    for pl in &board.placed {
        if shown_comp(unit, pl).is_none() { continue; }
        let pose = placement_pose(pl, board.stackup.thickness);
        let sel = Sel::Comp(CompRef { board: unit.id, comp: pl.comp });
        for (i, pad) in pl.footprint.pads.iter().enumerate() {
            let kind = match pad.kind {
                PadKind::Smd => SolderPadKind::Smd,
                PadKind::Thru => SolderPadKind::Thru,
                PadKind::Edge | PadKind::Npth => continue,
            };
            let ix = PadIx(i as u16);
            let layers = pl.pad_layers(ix, copper, n);
            let (s, c) = pad.rot.sin_cos();
            let x = pose.rotate([c, s, 0.0]);
            let rot = Angle::rad(x[1].atan2(x[0]));
            for side in [pl.side.flipped(), pl.side] {
                if kind == SolderPadKind::Smd && side != pl.side { continue; }
                let layer = LayerId::F_CU.on(side);
                if !layers.has(layer) { continue; }
                let Some(shape) = pl.pad_shape(ix, layer, n) else { continue };
                let polygon = shape.to_polyset(PAD_POLY_ERR);
                let mut rings = polygon.rings().map(|r| {
                    Contour::new(r[1..].iter().fold(Path::new(r[0]), |path, &p| path.line(p)))
                });
                let Some(outline) = rings.next() else { continue };
                let paste_area = if kind == SolderPadKind::Smd {
                    board.paste_opening(pl.id, ix, side).map_or(0.0, |s| s.to_polyset(PAD_POLY_ERR).area())
                } else {
                    0.0
                };
                let drill = if kind == SolderPadKind::Thru { pl.drill_hole(ix) } else { None };
                out.push(ScenePad {
                    sel: sel.clone(), no: pad.no.clone(), kind, side, component_side: pl.side,
                    outline, other_rings: rings.collect(), centre: pl.to_board(pad.at), rot,
                    drill, paste_area,
                });
            }
        }
    }
    out
}

fn bodies_of(unit: &Unit, board: &Board) -> Vec<SceneBody> {
    let mut out = Vec::with_capacity(board.placed.len());
    for pl in &board.placed {
        let Some(comp) = shown_comp(unit, pl) else { continue };
        let at = placement_pose(pl, board.stackup.thickness);
        let class = part_class(&comp.part.kind);
        let plain = || {
            let b = pl.footprint.courtyard.bbox();
            let b = if b.is_empty() { pl.footprint.bbox() } else { b };
            (MeshRef::Box { min: [b.min.x, b.min.y, 0], max: [b.max.x, b.max.y, NO_BODY_HEIGHT] }, Xform3::IDENTITY)
        };
        let (mesh, offset) = match &comp.part.body {
            // A body of no stated height (a KiCad model not here yet, or a part switched to its
            // plain body whose own body is a model) is the plain body of a part without one.
            Some(body) if matches!(body.source, BodySource::Extrude) && body.height <= 0 => plain(),
            Some(body) => {
                let mesh = match &body.source {
                    BodySource::Script(s) => MeshRef::Script(s.clone()),
                    BodySource::Mesh(p) => MeshRef::Mesh(p.clone()),
                    BodySource::Extrude => MeshRef::Extrude { outline: courtyard_contour(pl), height: body.height },
                    BodySource::Package(kind) => MeshRef::Package(PackageBody::of(*kind, class, &pl.footprint, body.height)),
                };
                (mesh, body.offset)
            }
            None => plain(),
        };
        out.push(SceneBody {
            sel: Sel::Comp(CompRef { board: unit.id, comp: pl.comp }),
            mesh,
            class,
            // the part parser refused unknown names, so a lookup finds what the part states
            surface: comp.part.body.as_ref().and_then(|body| body.surface.as_ref()).and_then(|stated| {
                let mut surface = Surface::new(MaterialId::lookup(&stated.material)?);
                if let Some(finish) = stated.finish.as_deref().and_then(FinishId::lookup) {
                    surface.finish = finish;
                }
                if let Some(colour) = stated.colour {
                    surface.colour = Colour { srgb: colour.srgb, translucency: colour.translucency, ral: colour.ral };
                }
                Some(surface)
            }),
            pose: at.then(&offset),
            emitter: comp.part.emitter.clone(),
            ports: comp.part.body.as_ref().map_or_else(Vec::new, |body| body.ports.clone()),
        });
    }
    out
}

/// The courtyard's outer ring (footprint frame), the box of the footprint when it has none.
fn courtyard_contour(pl: &Placed) -> Contour {
    match pl.footprint.courtyard.polygons().first().filter(|p| p.outer.len() >= 3) {
        Some(p) => {
            let mut path = Path::new(p.outer[0]);
            for q in &p.outer[1..] {
                path = path.line(*q);
            }
            Contour::new(path)
        }
        None => {
            let b = pl.footprint.bbox();
            Contour::rect(b.min, b.max, 0)
        }
    }
}
