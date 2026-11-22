//! Derived features (C4, E2.17): a feature of one unit projected through the assembly into
//! another unit's frame, flattened along the target board's normal.
//!
//! First version: poses of units mounted on the world or on another unit (spacer stacks)
//! compose here; a mount on a mate needs the connectors' mate frames (`PartDef.mate`, additive
//! after the freeze) and a mount on a mechanism body needs the mechanics port's poses, so those
//! answer with a located error naming what is missing. Body silhouettes are the courtyard until
//! bodies have their own outline (EMECH).
//!
//! Cost: O(units) for the pose chain, O(vertices) for the feature.

use crate::{Design, Frame};
use fab_elec_core::{Angle, BoardId, CompRef, Contour, Diag, FeatureRef, Nm, PolySet, Sel, Xform3, P};

/// Where a projected feature goes.
#[derive(Clone, PartialEq, Debug)]
pub enum ProjectInto {
    /// Into a board's frame, flattened onto its plane.
    Unit(BoardId),
    /// Into a 3D frame (cases and fixtures).
    Frame(Frame),
}

/// A projected feature, in the target unit's board frame.
#[derive(Clone, PartialEq, Debug)]
pub enum Feature {
    /// A point with a direction (a part origin; height above the target's top surface).
    Point { at: P, rot: Angle, z: Nm },
    /// A region (a pad, a body silhouette, a port profile).
    Region(PolySet),
    /// Hole centres and diameters.
    Holes(Vec<(P, Nm)>),
    /// Closed contours (an outline, a sketch).
    Contours(Vec<Contour>),
}

/// A unit's pose in the world, composing mounts on the world and on other units.
pub fn world_pose(d: &Design, unit: BoardId) -> Result<Xform3, Diag> {
    let mut chain = Vec::new();
    let mut cur = unit;
    loop {
        if chain.contains(&cur) {
            return Err(Diag::error("the mounts form a loop", chain.iter().map(|u| Sel::Unit(*u)).collect()));
        }
        chain.push(cur);
        let m = d.mount(cur).ok_or_else(|| Diag::error("the unit has no mount", vec![Sel::Unit(cur)]))?;
        match &m.parent {
            Frame::World => break,
            Frame::Unit(parent) => cur = *parent,
            Frame::Mate(l) => return Err(Diag::error("a mate's pose needs the connectors' mate frames (not in the parts yet)", vec![Sel::Link(*l)])),
            Frame::Body { mechanism, body } => {
                return Err(Diag::error(format!("a pose on {mechanism}.{body} needs the mechanism's poses"), vec![Sel::Unit(cur)]));
            }
        }
    }
    let mut x = Xform3::IDENTITY;
    for u in chain.iter().rev() {
        x = x.then(&d.mount(*u).unwrap().xform);
    }
    Ok(x)
}

/// Project feature `f` into `into`.
pub fn project(d: &Design, f: &FeatureRef, into: ProjectInto) -> Result<Feature, Diag> {
    let target = match into {
        ProjectInto::Unit(u) => u,
        ProjectInto::Frame(_) => return Err(Diag::error("3D projection into a frame is the mechanics lane's (C8)", vec![Sel::Feature(f.clone())])),
    };
    let src_unit = Sel::Feature(f.clone()).unit().ok_or_else(|| Diag::error("CAD features project through the object script", vec![Sel::Feature(f.clone())]))?;
    let to = world_pose(d, target)?.inverse().then(&world_pose(d, src_unit)?);
    let map = |p: P| {
        let q = to.apply([p.x, p.y, 0]);
        P::new(q[0], q[1])
    };
    let map_rot = |a: Angle| {
        let (s, c) = a.sin_cos();
        let v = to.rotate([c, s, 0.0]);
        Angle::rad(v[1].atan2(v[0]))
    };
    let unit = d.unit(src_unit).ok_or_else(|| Diag::error("no such unit", vec![Sel::Unit(src_unit)]))?;
    let board = unit.board.as_ref().ok_or_else(|| Diag::error("the unit has no board", vec![Sel::Unit(src_unit)]))?;
    let placed = |c: &CompRef| board.placed_of(c.comp).ok_or_else(|| Diag::error("the part is not placed", vec![Sel::Comp(*c)]));
    let region = |set: &PolySet, f: &dyn Fn(P) -> P| PolySet::from_rings_even_odd(&set.rings().map(|r| r.iter().map(|p| map(f(*p))).collect()).collect::<Vec<Vec<P>>>());
    Ok(match f {
        FeatureRef::Part(c) => {
            let p = placed(c)?;
            let q = to.apply([p.at.x, p.at.y, 0]);
            Feature::Point { at: P::new(q[0], q[1]), rot: map_rot(p.rot), z: q[2] }
        }
        FeatureRef::Pad(c, pad) => {
            let p = placed(c)?;
            let l = fab_elec_core::LayerId::F_CU.on(p.side);
            let s = p.pad_shape(*pad, l, board.copper_count()).ok_or_else(|| Diag::error("no such pad", vec![Sel::Feature(f.clone())]))?;
            Feature::Region(region(&s.to_polyset(fab_elec_board::ARC_ERR), &|q| q))
        }
        FeatureRef::Body(c) => {
            let p = placed(c)?;
            Feature::Region(region(&p.footprint.courtyard, &|q| p.to_board(q)))
        }
        FeatureRef::Port(..) => return Err(Diag::error("port profiles project with the mechanics lane's bodies (C8)", vec![Sel::Feature(f.clone())])),
        FeatureRef::Hole(_, id) => {
            let h = board.holes.iter().find(|h| h.id == *id).ok_or_else(|| Diag::error("no such hole", vec![Sel::Feature(f.clone())]))?;
            Feature::Holes(vec![(map(h.at), h.drill)])
        }
        FeatureRef::HoleSet(_, prefix) => Feature::Holes(board.holes.iter().filter(|h| h.name.starts_with(prefix.as_str())).map(|h| (map(h.at), h.drill)).collect()),
        FeatureRef::Outline(_) => Feature::Contours(board.outline.iter().map(|c| Contour::new(c.path.mapped(map))).collect()),
        FeatureRef::Sketch(_) | FeatureRef::Face { .. } | FeatureRef::Panel(_) | FeatureRef::Generated(_) => unreachable!("CAD features have no unit"),
    })
}
