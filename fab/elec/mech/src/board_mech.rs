//! A board's mechanical summary for cases, fit checks, slicer inserts and the scene (C8).
//!
//! Two inputs, two hashes, so a case rebuilds per feature (ARCHITECTURE 2.3): `board("x")` is the
//! outline, thickness, holes, ports, keepouts and the height envelope (the tallest body on each
//! face), hashed as `hash`; the body list is `board_bodies("x")`, hashed as `bodies_hash`, read
//! only by scripts that shape round single parts. Dragging a capacitor that is neither the tallest
//! on its face nor has a port leaves `hash`, so a case reading only `board("x")` stays as it is.
//!
//! `board_mech` is O(parts + drills + keepouts + port vertices); it runs on the pool when a board
//! changes, never per frame; the hash streams.
//!
//! Items a panel part derived onto the board (`Origin::Derived { from: FeatureRef::Panel(..) }`,
//! a light pipe's leg holes) are left out: the case that made them reads this summary, so they must
//! not rebuild it. The scene and the slice insert read the board itself and show them.

use crate::keys::feature_key;
use crate::scene::placement_pose;
use fab_elec_board::{Board, BoardKeepout, ConnItem, DrillHole, Origin};
use fab_elec_circuit::Circuit;
use fab_elec_core::{canon_struct, mm, BBox, BoardId, Canon, CompId, CompRef, ContentHash, Contour, FeatureRef, ItemId, LayerId, Nm, PolySet, Side, Writer, Xform3, P};
use fab_elec_core::geom::NM_PER_MM;
use fab_elec_parts::{BodyPortKind, BodySource, Footprint, PartDef, PartKind};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::sync::{Arc, Mutex};

/// A board's mechanical summary (C8): what a case, a fit check, a slicer insert and the scene read.
/// Board frame as `SceneBoard`. `hash` is the content hash of every field below but `unit`, `name`
/// and `hash`: routing, vias, zones, nets and texts never change it (but for a part's legend's
/// words, which a case marks); moving a part does.
#[derive(Clone, PartialEq, Debug)]
pub struct BoardMech {
    pub unit: BoardId,
    pub name: String,
    /// Even-odd contours, as `SceneBoard.outline`: Board.outline, then its cutout items but those a
    /// panel part derived.
    pub outline: Vec<Contour>,
    /// Board.stackup.thickness.
    pub thickness: Nm,
    pub holes: Vec<MechHole>,
    pub ports: Vec<PlacedPort>,
    /// Board.keepouts but those a panel part derived.
    pub keepouts: Vec<BoardKeepout>,
    /// The tallest body off each face, top then bottom: what a case clears.
    pub envelope: [Nm; 2],
    /// The tallest top body that stays under a lid: a part whose @plug port points up (a pin
    /// header plugged from above) passes through its opening in the lid and does not count. A
    /// case that cuts those openings clears this; one that leaves them closed clears the top.
    pub covered: Nm,
    /// The hash of `board("x")`: outline, thickness, holes, ports, keepouts and envelope.
    pub hash: ContentHash,
    pub bodies: Vec<BodyBox>,
    /// The hash of `board_bodies("x")`: the body list.
    pub bodies_hash: ContentHash,
}

/// A mounting or tooling hole: a `Board::drills()` entry that is a board hole, any drill of a
/// mounting-hole part (plated and grounded by a pin or not), or a footprint drill no electrical pin
/// uses that lies outside the body outline of a part with a body (inside, it is the part's own post
/// or lug). Vias and other pin pads are not mechanical.
/// `id` is the hole's item, or the placement's for a footprint drill; `name` the hole's, or
/// `<component path>.<pad>`.
#[derive(Clone, PartialEq, Debug)]
pub struct MechHole {
    pub id: ItemId,
    pub feature: FeatureRef,
    pub name: String,
    pub at: P,
    /// The drill's width (`DrillHole::width`: a slot's narrow side, what a standoff's pin fits).
    pub d: Nm,
    pub plated: bool,
}
canon_struct!(MechHole { id, name, at, d, plated });

/// A placed component's body as a box (E1 5): what fit checks and the simple case use.
#[derive(Clone, PartialEq, Debug)]
pub struct BodyBox {
    pub comp: CompId,
    pub path: String,
    pub side: Side,
    /// Plan box in the board frame: the placed courtyard's bbox (`footprint.courtyard`
    /// transformed by the placement); when the courtyard is empty, `Placed::bbox()`.
    pub min: P,
    pub max: P,
    /// Height off its face (`Body3d.height`); 0 when the part has no body. A body of no stated
    /// height that names no mesh (a KiCad model not here yet, or a part switched to its plain body
    /// whose own body is a model) stands at [`NO_BODY_HEIGHT`](crate::scene::NO_BODY_HEIGHT), the
    /// scene's plain body of a part without one.
    pub height: Nm,
    /// The part has a body (false: `height` is unknown, the checks say so).
    pub known: bool,
    pub source: Option<BodySource>,
    /// Body3d.offset, then the footprint's placement: top at z = thickness, bottom at z = 0
    /// with x and z flipped before rotation. Identity offset when the part has no body.
    pub pose: Xform3,
}
canon_struct!(BodyBox { comp, path, side, min, max, height, known, source, pose });

/// A body port placed on the board (`FeatureRef::Port(CompRef { board, comp }, index)`).
#[derive(Clone, PartialEq, Debug)]
pub struct PlacedPort {
    pub comp: CompId,
    pub path: String,
    /// The component's reference designator (`D1`): what a person calls it.
    pub refdes: String,
    /// Index into the part's `Body3d.ports`.
    pub index: u16,
    pub kind: BodyPortKind,
    pub side: Side,
    /// The port's origin, board frame (nm).
    pub at: [Nm; 3],
    /// Unit direction out of the part, board frame.
    pub dir: [f64; 3],
    /// Profile in its own plane (u, v): v is board +z made perpendicular to `dir` and
    /// normalised (board +y when `dir` is along z), u = v x dir. nm.
    pub profile: PolySet,
    pub reach: Nm,
    pub margin: Nm,
    /// A press port's actuation travel (`BodyPort::travel`), 0 where the part states none.
    pub travel: Nm,
    /// How far a plunger or a shaft stands out of its housing (`BodyPort::stand`): the housing's
    /// face is `stand` behind `at`. 0 where the part states none.
    pub stand: Nm,
    /// The hole a case leaves for it, in the profile's plane: the profile grown by `margin`. A
    /// plug from above (a pin header through the lid) opens round its whole body's plan too,
    /// grown by `margin`, so its mating plug's housing goes on with the case closed and no part
    /// of the body meets the lid.
    pub opening: PolySet,
    /// The part's legend (`Placed::legend`, its `label`), what a case marks beside the opening;
    /// empty when it has none.
    pub legend: String,
    /// The legend's width per unit of its capitals' height in the sans font of a model's text
    /// (`makepad_csg_graph::text_width`), so a case places it without building it; 0 without one.
    pub legend_aspect: f64,
}
canon_struct!(PlacedPort { comp, path, refdes, index, kind, side, at, dir, profile, reach, margin, travel, stand, opening, legend, legend_aspect });

/// A mounting-hole part (`kind: @mounting`, as std's `mounting_hole` makes): a screw goes through
/// each of its drills.
fn mounting(part: &PartDef) -> bool {
    matches!(&part.kind, PartKind::Other(kind) if kind == "mounting")
}

/// A footprint's body outline in its own frame: the bounds of its fab-layer drawing (every std
/// footprint draws its body there, and most imported ones do), or None where it draws none.
fn body_outline(fp: &Footprint) -> Option<BBox> {
    fp.graphics.iter().filter(|(layer, _)| *layer == LayerId::F_FAB)
        .flat_map(|(_, g)| g.shapes()).map(|s| s.bbox()).reduce(BBox::union)
}

/// Says once per footprint (per process) that a part with a body and a pinless drill draws no
/// body outline: its drills stay case mounting holes, though one may be the part's own post.
fn unoutlined(footprint: &str) {
    static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert(footprint.to_owned()) {
        eprintln!("fab.elec.mech: footprint {footprint} has a pinless drill and no body outline (F.Fab): its drills count as case mounting holes");
    }
}

/// Summarise every placement, mechanical hole and keepout in the board's own frame.
pub fn board_mech(board: &Board, circuit: &Circuit) -> BoardMech {
    board_mech_with(board, circuit, &board.drills())
}

/// [`board_mech`] with the board's `drills()` as the host already has them.
pub fn board_mech_with(board: &Board, circuit: &Circuit, drills: &[DrillHole]) -> BoardMech {
    let mut bodies = Vec::with_capacity(board.placed.len());
    let mut ports = Vec::new();
    for placed in &board.placed {
        let comp = &circuit.comps[placed.comp.ix()];
        let body = comp.part.body.as_ref();
        let bounds = if placed.footprint.courtyard.is_empty() {
            placed.bbox()
        } else {
            BBox::of_points(placed.footprint.courtyard.rings().flatten().map(|p| placed.to_board(*p)))
        };
        let pose = placement_pose(placed, board.stackup.thickness).then(&body.map_or(Xform3::IDENTITY, |b| b.offset));
        bodies.push(BodyBox {
            comp: placed.comp,
            path: comp.path.clone(),
            side: placed.side,
            min: bounds.min,
            max: bounds.max,
            height: body.map_or(0, |b| if b.height <= 0 && b.source == BodySource::Extrude { crate::scene::NO_BODY_HEIGHT } else { b.height }),
            known: body.is_some(),
            source: body.map(|b| b.source.clone()),
            pose,
        });
        let legend = placed.legend.as_ref().map(|t| t.text.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_default();
        let legend_aspect = if legend.is_empty() || body.is_none_or(|b| b.ports.is_empty()) {
            0.0
        } else {
            makepad_csg_graph::text_width(&legend, 1.0, 0.0, makepad_csg_graph::Font::Sans).unwrap_or(0.0)
        };
        if let Some(body) = body {
            for (index, port) in body.ports.iter().enumerate() {
                let dir = normalised(pose.rotate(port.dir));
                let (u_body, v_body) = port_axes(port.dir);
                let (u, v) = port_axes(dir);
                let rings: Vec<Vec<P>> = port.profile.rings().map(|ring| {
                    ring.iter().map(|p| {
                        let offset = std::array::from_fn(|i| p.x as f64 * u_body[i] + p.y as f64 * v_body[i]);
                        let offset = pose.rotate(offset);
                        P::from_f(dot(offset, u), dot(offset, v))
                    }).collect()
                }).collect();
                let profile = PolySet::from_rings_even_odd(&rings);
                let at = pose.apply(port.at);
                let mut opening = profile.offset(port.margin, mm(0.02));
                if port.kind == BodyPortKind::Plug && placed.side == Side::Top && dir[2] > 0.7 {
                    // The body's plan box in the port's (u, v), about its mouth.
                    let plane = |p: P| {
                        let d = [(p.x - at[0]) as f64, (p.y - at[1]) as f64, 0.0];
                        P::from_f(dot(d, u), dot(d, v))
                    };
                    let body = PolySet::rect(BBox::new(plane(bounds.min), plane(bounds.max))).offset(port.margin, mm(0.02));
                    opening = opening.union(&body);
                }
                ports.push(PlacedPort {
                    comp: placed.comp,
                    path: comp.path.clone(),
                    refdes: comp.refdes.clone(),
                    index: u16::try_from(index).expect("body port index fits FeatureRef::Port"),
                    kind: port.kind,
                    side: placed.side,
                    at,
                    dir,
                    profile,
                    opening,
                    reach: port.reach,
                    margin: port.margin,
                    travel: port.travel,
                    stand: port.stand,
                    legend: legend.clone(),
                    legend_aspect,
                });
            }
        }
    }
    // drills() is the one source of drill shapes (the scene's holes read it too)
    let holes: Vec<MechHole> = drills.iter().filter_map(|h| match h.item {
        ConnItem::Hole(id) => {
            let hole = board.holes.iter().find(|x| x.id == id).filter(|x| !from_panel(&x.origin))?;
            Some(MechHole { id, feature: FeatureRef::Hole(board.unit, id), name: hole.name.clone(), at: h.c, d: h.width(), plated: h.plated })
        }
        ConnItem::Pad { placed, pad } => {
            let pl = board.placed.iter().find(|p| p.id == placed)?;
            let comp = circuit.comps.get(pl.comp.ix())?;
            let fp_pad = pl.footprint.pads.get(pad.ix())?;
            let no = &fp_pad.no;
            // Every drill of a mounting-hole part is a mount, a plated one a pin ties to ground
            // included: the screw goes through it.
            if !mounting(&comp.part) {
                if comp.part.pins.iter().any(|pin| pin.pads.contains(no)) {
                    return None;
                }
                // A pinless drill under the body of a part that stands one on the board is the
                // part's post, peg or lug: it holds the part, and no screw or lid post reaches it
                // past the body. A part with no body keeps its pinless drills as mounts.
                if comp.part.body.is_some() {
                    match body_outline(&pl.footprint) {
                        Some(body) if body.contains(fp_pad.at) => return None,
                        Some(_) => {}
                        None => unoutlined(&pl.footprint.name),
                    }
                }
            }
            Some(MechHole { id: placed, feature: FeatureRef::Pad(fab_elec_core::CompRef { board: board.unit, comp: pl.comp }, pad), name: format!("{}.{}", comp.path, no), at: h.c, d: h.width(), plated: h.plated })
        }
        _ => None,
    }).collect();
    let keepouts: Vec<BoardKeepout> = board.keepouts.iter().filter(|k| !from_panel(&k.origin)).cloned().collect();
    let outline: Vec<Contour> = board.outline.iter().cloned().chain(board.cutouts.iter().filter(|c| !from_panel(&c.origin)).map(|c| c.contour.clone())).collect();
    let tallest = |side: Side| bodies.iter().filter(|b| b.side == side).map(|b| b.height).max().unwrap_or(0);
    let envelope = [tallest(Side::Top), tallest(Side::Bottom)];
    let through_lid = |comp: CompId| ports.iter().any(|p| p.comp == comp && p.kind == BodyPortKind::Plug && p.side == Side::Top && p.dir[2] > 0.7);
    let covered = bodies.iter().filter(|b| b.side == Side::Top && !through_lid(b.comp)).map(|b| b.height).max().unwrap_or(0);
    let mut key = Writer::hasher();
    outline.canon(&mut key);
    board.stackup.thickness.canon(&mut key);
    holes.canon(&mut key);
    ports.canon(&mut key);
    keepouts.canon(&mut key);
    envelope.canon(&mut key);
    covered.canon(&mut key);
    let mut body_key = Writer::hasher();
    bodies.canon(&mut body_key);
    BoardMech {
        unit: board.unit,
        name: board.name.clone(),
        outline,
        thickness: board.stackup.thickness,
        holes,
        ports,
        keepouts,
        envelope,
        covered,
        hash: key.finish(),
        bodies,
        bodies_hash: body_key.finish(),
    }
}

/// Derived onto the board by a panel part (E7 1.2): the case's own output, not its input.
fn from_panel(origin: &Origin) -> bool {
    matches!(origin, Origin::Derived { from: FeatureRef::Panel(_) })
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn normalised(v: [f64; 3]) -> [f64; 3] {
    let len = dot(v, v).sqrt();
    v.map(|x| x / len)
}

pub(crate) use fab_elec_parts::port_axes;

impl BoardMech {
    /// The Splash value `board("name")` gives a case script: the shape in enclosure.splash's header.
    /// Contours are polygonised within 0.05 mm (arcs as chords on the arc).
    pub fn splash(&self) -> String {
        let rings: Vec<Vec<P>> = self.outline.iter().map(|c| c.to_ring(mm(0.05))).collect();
        let outline = rings.first().cloned().unwrap_or_default();
        let mut out = format!("{{name: \"{}\" outline: {} cutouts: [", escaped(&self.name), ring(&outline));
        for cut in rings.iter().skip(1) {
            write!(out, "{} ", ring(cut)).unwrap();
        }
        write!(out, "] thickness: {} envelope: {{top: {} bottom: {} covered: {}}} holes: [", length(self.thickness), length(self.envelope[0]), length(self.envelope[1]), length(self.covered)).unwrap();
        for hole in &self.holes {
            let feature = feature_key(&hole.feature).unwrap_or_default();
            write!(out, "{{name: \"{}\" at: {} d: {} plated: {} feature: \"{}\"}} ", escaped(&hole.name), point(hole.at), length(hole.d), hole.plated, feature).unwrap();
        }
        out.push_str("] keepouts: [");
        for k in &self.keepouts {
            let area = k.area.polygons().first().map_or(&[][..], |p| p.outer.as_slice());
            write!(out, "{{area: {} reason: \"{}\"}} ", ring(area), escaped(&k.reason)).unwrap();
        }
        out.push_str("] ports: [");
        for port in &self.ports {
            let points = port.opening.polygons().first().map_or(&[][..], |p| p.outer.as_slice());
            let section = port.profile.polygons().first().map_or(&[][..], |p| p.outer.as_slice());
            let kind = match port.kind {
                BodyPortKind::Plug => "plug",
                BodyPortKind::Light => "light",
                BodyPortKind::Press => "press",
                BodyPortKind::Air => "air",
                BodyPortKind::Cable => "cable",
                BodyPortKind::Shaft => "shaft",
            };
            let feature = feature_key(&FeatureRef::Port(CompRef { board: self.unit, comp: port.comp }, port.index)).unwrap_or_default();
            write!(out, "{{ref: \"{}\" refdes: \"{}\" kind: @{} side: @{} at: [{}, {}, {}] dir: [{}, {}, {}] opening: {} profile: {} reach: {} margin: {} travel: {} stand: {} feature: \"{}\"",
                escaped(&port.path), escaped(&port.refdes), kind, side(port.side), length(port.at[0]), length(port.at[1]), length(port.at[2]),
                decimal(port.dir[0], 6), decimal(port.dir[1], 6), decimal(port.dir[2], 6), ring(points), ring(section), length(port.reach), length(port.margin),
                length(port.travel), length(port.stand), feature).unwrap();
            if !port.legend.is_empty() {
                write!(out, " legend: {{text: \"{}\" aspect: {}}}", escaped(&port.legend), decimal(port.legend_aspect, 4)).unwrap();
            }
            out.push_str("} ");
        }
        out.push_str("]}");
        out
    }

    /// The Splash value `board_bodies("name")` gives: each placed part's plan box and optional
    /// mesh content key, in the shape of enclosure.splash's header.
    pub fn bodies_splash(&self) -> String {
        self.bodies_splash_with_mesh_keys().0
    }

    /// Write the bodies and collect their content keys with one cache read per mesh file.
    pub(crate) fn bodies_splash_with_mesh_keys(&self) -> (String, Vec<Option<Arc<str>>>) {
        let mut out = String::from("[");
        let mut files = BTreeMap::new();
        let mut keys = Vec::with_capacity(self.bodies.len());
        for body in &self.bodies {
            let key = match &body.source {
                Some(BodySource::Mesh(path)) => files.entry(path.as_str()).or_insert_with(|| {
                    fab_elec_import::cache::MeshCache::shared().get(std::path::Path::new(path)).ok().map(|mesh| mesh.sha256)
                }).clone(),
                _ => None,
            };
            let mesh = key.as_ref().map_or_else(|| "nil".to_string(), |key| format!("\"{key}\""));
            write!(out, "{{ref: \"{}\" side: @{} min: {} max: {} height: {} known: {} mesh: {}}} ",
                escaped(&body.path), side(body.side), point(body.min), point(body.max), length(body.height), body.known, mesh).unwrap();
            keys.push(key);
        }
        out.push(']');
        (out, keys)
    }
}

pub(crate) fn side(side: Side) -> &'static str {
    match side {
        Side::Top => "top",
        Side::Bottom => "bottom",
    }
}

pub(crate) fn escaped(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn decimal(value: f64, places: usize) -> String {
    let s = format!("{value:.places$}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Write a millimetre quantity, including bounding spans wider than a single coordinate.
pub(crate) fn length(nm: impl Into<i64>) -> String {
    format!("{}mm", decimal(nm.into() as f64 / NM_PER_MM, 4))
}

pub(crate) fn point(p: P) -> String {
    format!("[{}, {}]", length(p.x), length(p.y))
}

pub(crate) fn ring(points: &[P]) -> String {
    format!("[{}]", points.iter().map(|p| point(*p)).collect::<Vec<_>>().join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_board::Hole;
    use fab_elec_core::Seg;
    use fab_elec_fixtures::ledctl;

    #[test]
    fn a_track_edit_keeps_the_mech_hash_and_a_part_move_changes_it() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("fixture has a board");
        let circuit = &*unit.circuit;
        let comp = circuit.comp("usb.j").expect("fixture has usb.j").id;
        let mech = board_mech(board, circuit);
        assert_eq!(mech.bodies.len(), board.placed.len(), "every placed component has a body box");
        for (body, placed) in mech.bodies.iter().zip(&board.placed) {
            assert_eq!(body.comp, placed.comp, "body boxes retain placement order");
            assert!(body.known && body.height > 0, "every fixture part states its body height: {}", body.path);
        }
        assert_eq!(mech.bodies.iter().find(|b| b.comp == comp).map(|b| b.height), Some(mm(3.16)), "the receptacle keeps its stated height");
        let kinds = |k: BodyPortKind| mech.ports.iter().filter(|p| p.kind == k).count();
        assert_eq!((kinds(BodyPortKind::Plug), kinds(BodyPortKind::Light), kinds(BodyPortKind::Press)), (1, 8, 1), "the receptacle, eight LEDs and the button give their ports");
        assert_eq!(mech.holes.iter().map(|h| (h.name.as_str(), h.d)).collect::<Vec<_>>(), vec![("mount1", mm(3.2)), ("mount2", mm(3.2))], "the board's holes, not the pads' drills or the vias, are the mechanical holes");

        let mut piped = board.clone();
        piped.holes.push(Hole { id: ItemId(9000), name: "pipe.leg".into(), at: P::mm(30.0, 20.0), drill: mm(1.0), pad: None, net: None, plated: false, origin: Origin::Derived { from: FeatureRef::Panel("lid".into()) } });
        assert_eq!(board_mech(&piped, circuit).hash, mech.hash, "a hole a panel part derived does not rebuild the case that made it");

        let mut routed = board.clone();
        match routed.tracks[0].path.segs.last_mut().expect("fixture track has an end") {
            Seg::Line(end) | Seg::Arc { end, .. } => end.x += mm(1.0),
        }
        routed.vias[0].at.x += mm(1.0);
        routed.zones.clear();
        let routed = board_mech(&routed, circuit);
        assert_eq!((routed.hash, routed.bodies_hash), (mech.hash, mech.bodies_hash), "track, via and zone edits leave both hashes unchanged");
        assert_eq!(mech.envelope, [mm(3.16), mm(2.5)], "the envelope is the receptacle on top and the header underneath");
        assert_eq!(mech.covered, mm(3.16), "no top part plugs from above: a lid clears the receptacle");
        let press = mech.ports.iter().find(|p| p.kind == BodyPortKind::Press).expect("the button gives its press port");
        assert_eq!(press.travel, mm(0.25), "the button's port carries its plunger's travel");
        assert!(mech.splash().contains("kind: @press side: @top") && mech.splash().contains("travel: 0.25mm"), "board() gives the case the button's travel");

        // a small part moved: the bodies change, the case's input does not
        let cap = circuit.comp("reg.c_in").expect("fixture has reg.c_in").id;
        let mut nudged = board.clone();
        nudged.placed.iter_mut().find(|p| p.comp == cap).expect("fixture places reg.c_in").at.x += mm(1.0);
        let nudged = board_mech(&nudged, circuit);
        assert_eq!(nudged.hash, mech.hash, "moving a capacitor that is not the tallest and has no port keeps board()'s hash");
        assert_ne!(nudged.bodies_hash, mech.bodies_hash, "moving it changes board_bodies()'s hash");

        // a drill no pin uses is a mounting hole; one a pin uses is not, nor one under the part's
        // own body (its post or lug: the receptacle's body outline ends at y 2.175 mm)
        let mut drilled = board.clone();
        let usb = drilled.placed.iter_mut().find(|p| p.comp == comp).expect("fixture places usb.j");
        let fp = std::sync::Arc::make_mut(&mut usb.footprint);
        let mut mh = fp.pads.iter().find(|p| p.drill.is_some()).expect("the receptacle has drilled shield pads").clone();
        mh.no = "MH".into();
        mh.at = P::mm(0.0, 2.5);
        let mut post = mh.clone();
        post.no = "POST".into();
        post.at = P::mm(0.0, -1.0);
        fp.pads.push(mh);
        fp.pads.push(post);
        let drilled_mech = board_mech(&drilled, circuit);
        let names: Vec<&str> = drilled_mech.holes.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["usb.j.MH", "mount1", "mount2"], "a pinless drill outside its part's body is a mechanical hole; the shield's pinned drills and a post under the body are not");
        assert_ne!(drilled_mech.hash, mech.hash, "a new mechanical hole changes board()'s hash");
        // a part that stands no body on the board (a mounting-hole part) keeps every pinless drill a mount
        let mut bodiless = circuit.clone();
        std::sync::Arc::make_mut(&mut bodiless.comps[comp.ix()].part).body = None;
        let names: Vec<String> = board_mech(&drilled, &bodiless).holes.into_iter().map(|h| h.name).collect();
        assert_eq!(names, vec!["usb.j.MH", "usb.j.POST", "mount1", "mount2"], "a part with no body has no post: its drills stay mounts");
        // a mounting-hole part's drills are all mounts, those a pin grounds included
        let mut mounting_part = circuit.clone();
        std::sync::Arc::make_mut(&mut mounting_part.comps[comp.ix()].part).kind = PartKind::Other("mounting".into());
        let names: Vec<String> = board_mech(&drilled, &mounting_part).holes.into_iter().map(|h| h.name).collect();
        assert_eq!(names.iter().filter(|n| *n == "usb.j.S1").count(), 4, "a mounting-hole part's pinned drills are mounts: {names:?}");
        assert!(names.contains(&"usb.j.MH".into()) && names.contains(&"usb.j.POST".into()), "a mounting-hole part's pinless drills are mounts: {names:?}");
        // a footprint that draws no body outline keeps today's rule: every pinless drill is a mount
        let usb = drilled.placed.iter_mut().find(|p| p.comp == comp).expect("fixture places usb.j");
        std::sync::Arc::make_mut(&mut usb.footprint).graphics.retain(|(layer, _)| *layer != LayerId::F_FAB);
        let outline_less = board_mech(&drilled, circuit);
        let names: Vec<&str> = outline_less.holes.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["usb.j.MH", "usb.j.POST", "mount1", "mount2"], "without a body outline no pinless drill is taken for the part's own post");

        let body = mech.bodies.iter().find(|b| b.comp == comp).expect("declared body is placed");
        let port = mech.ports.iter().find(|p| p.comp == comp).expect("declared port is placed");
        let mut moved = board.clone();
        moved.placed.iter_mut().find(|p| p.comp == comp).expect("fixture has placement").at.y += mm(2.0);
        let moved = board_mech(&moved, circuit);
        let moved_body = moved.bodies.iter().find(|b| b.comp == comp).expect("moved body is placed");
        let moved_port = moved.ports.iter().find(|p| p.comp == comp).expect("moved port is placed");
        assert_ne!(mech.hash, moved.hash, "moving a part changes the mechanical hash");
        assert_eq!(moved_body.min.y, body.min.y + mm(2.0), "body minimum follows the placement in y");
        assert_eq!(moved_body.max.y, body.max.y + mm(2.0), "body maximum follows the placement in y");
        assert_eq!(moved_port.at[1], port.at[1] + mm(2.0), "port origin follows the placement in y");

        assert!((port.dir[0] + 1.0).abs() < 1e-9 && port.dir[1].abs() < 1e-9 && port.dir[2].abs() < 1e-9, "left-edge port points along board -x");
        assert_eq!(port.at[2], board.stackup.thickness + mm(1.58), "top port origin is above the top face");
        let bounds = port.profile.bbox();
        assert!((bounds.width() - mm(8.94) as i64).abs() <= 1, "placed profile keeps its width along u within one nm");
        assert!((bounds.height() - mm(3.16) as i64).abs() <= 1, "placed profile keeps its height along v within one nm");

        let mut bottom = board.clone();
        bottom.placed.iter_mut().find(|p| p.comp == comp).expect("fixture has placement").side = Side::Bottom;
        let bottom = board_mech(&bottom, circuit);
        let bottom_body = bottom.bodies.iter().find(|b| b.comp == comp).expect("bottom body is placed");
        let bottom_port = bottom.ports.iter().find(|p| p.comp == comp).expect("bottom port is placed");
        assert_eq!(bottom_body.side, Side::Bottom, "body records its bottom-side placement");
        assert_eq!(bottom_port.at[2], mm(-1.58), "bottom port origin is below the bottom face");
        assert!(bottom_port.dir[2].abs() < 1e-9, "bottom port direction stays in the board plane");
        assert!((bottom_port.profile.bbox().height() - mm(3.16) as i64).abs() <= 1, "bottom profile keeps its height along board up within one nm");

        let splash = mech.splash();
        let usb_refdes = &circuit.comp("usb.j").expect("fixture has usb.j").refdes;
        assert!(!usb_refdes.is_empty() && splash.contains(&format!("ref: \"usb.j\" refdes: \"{usb_refdes}\"")), "Splash identifies the component by path and by its designator");
        assert!(splash.contains("kind: @plug"), "Splash identifies the plug port kind");
        assert!(splash.contains("thickness: 1.6mm"), "Splash writes board thickness in trimmed millimetres");
        assert!(splash.contains("envelope: {top: 3.16mm bottom: 2.5mm covered: 3.16mm}") && !splash.contains("bodies:"), "board() carries the envelope, not the body list");
        assert!(mech.bodies_splash().contains("ref: \"usb.j\" side: @top"), "board_bodies() lists each part's box");
        assert_eq!(mech.bodies_splash().matches("known: true mesh: nil").count(), mech.bodies.len(), "every procedural body explicitly has no mesh key");
    }
}
