//! Screen coverage of the 3D scenes in grabbed frames: per scene node, its
//! share of the scene's picture at each grabbed time and its box in the
//! document where it is largest, from the coverage id map the grabber has each
//! Scene3D draw beside its frame (what covers a node counted: a node behind
//! another shows less). A node never seen at any grabbed time is named, so
//! an agent can tell "the product is off screen" or "too small" without
//! looking.

use crate::inspect::num;
use makepad_story_edit_render::coverage::{report, CoverNode, FrameCover};
use makepad_strict_json::{obj, Value};

/// One scene's coverage in one grabbed frame.
#[derive(Clone, Debug)]
pub struct SceneCover {
    /// The graphic node the scene is in, and its layer path there.
    pub graphic: String,
    pub scene: String,
    pub nodes: Vec<CoverNode>,
    pub cover: FrameCover,
}

/// The scenes of a run of frames (`(t, scenes)` per frame), each with the
/// times it was drawn at: its report as JSON and as text, in the order
/// they first appear.
pub fn summary(frames: &[(f64, &[SceneCover])]) -> Vec<(Value, String)> {
    let mut scenes: Vec<(&SceneCover, Vec<(f64, FrameCover)>)> = Vec::new();
    for (t, covers) in frames {
        for c in covers.iter() {
            match scenes.iter_mut().find(|(first, _)| first.graphic == c.graphic && first.scene == c.scene) {
                Some((_, seen)) => seen.push((*t, c.cover.clone())),
                None => scenes.push((c, vec![(*t, c.cover.clone())])),
            }
        }
    }
    scenes
        .into_iter()
        .map(|(c, seen)| {
            let nodes = Value::Arr(
                c.nodes
                    .iter()
                    .map(|n| {
                        let largest = seen.iter().filter(|(_, f)| f.share(n.id) > 0.0).max_by(|a, b| a.1.share(n.id).total_cmp(&b.1.share(n.id)));
                        let (bounds, at) = match largest.and_then(|(t, f)| Some((*t, f.bounds(n.id)?))) {
                            Some((t, b)) => (Value::Arr(b.iter().map(|v| Value::Int(v.round() as i64)).collect()), num(t)),
                            None => (Value::Null, Value::Null),
                        };
                        obj(vec![
                            ("name", n.name.clone().map_or(Value::Null, Value::Str)),
                            ("path", Value::Str(n.path.clone())),
                            ("line", Value::Int(n.line as i64)),
                            ("share", Value::Arr(seen.iter().map(|(_, f)| num(f.share(n.id))).collect())),
                            ("box", bounds),
                            ("box_at", at),
                        ])
                    })
                    .collect(),
            );
            let first = &seen[0].1;
            let json = obj(vec![
                ("graphic", Value::Str(c.graphic.clone())),
                ("scene", Value::Str(c.scene.clone())),
                ("width", Value::Int(first.size[0].round() as i64)),
                ("height", Value::Int(first.size[1].round() as i64)),
                ("times", Value::Arr(seen.iter().map(|(t, _)| num(*t)).collect())),
                ("nodes", nodes),
                ("background", Value::Arr(seen.iter().map(|(_, f)| num(f.empty())).collect())),
            ]);
            (json, report(&format!("{} in {}", c.scene, c.graphic), &c.nodes, &seen))
        })
        .collect()
}
