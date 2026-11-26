//! Screen coverage of a Scene3D layer's nodes: how much of the scene's
//! picture each node covers and where (its box), from the 3D renderer's
//! coverage id map (`SharedRenderer::set_coverage`): every mesh drawn in
//! its id over one depth, so a pixel names the surface seen there, with
//! what is in front taken into account. An inspect grab asks for it with
//! [`crate::MotionDraw::set_scene_coverage`]; an export never draws it.
//! Boxes are in document units, where the layer sits (the map is drawn at
//! the scene's own render scale).

use makepad_motion::frame::Affine;
use makepad_widgets::Texture;

/// A node of a scene that draws surfaces (a mesh, model, 3D text or
/// instances): its number in the scene, its name and path, and the line
/// of the document that writes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverNode {
    pub id: u32,
    pub name: Option<String>,
    /// `children[2].children[0]`.
    pub path: String,
    pub line: u32,
}

impl CoverNode {
    /// `"hero"`, or its path when it has no name.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) => format!("\"{name}\""),
            None => self.path.clone(),
        }
    }
}

/// A Scene3D layer's coverage map from the last recorded frame, for the
/// host to read back and [`SceneCoverage::measure`].
#[derive(Clone, Debug)]
pub struct SceneCoverage {
    /// The layer's path in the document.
    pub scene: String,
    /// 8-bit RGBA: a pixel's red + 256 × green is the index + 1 of the
    /// frame's mesh seen there, 0 for none.
    pub map: Texture,
    /// The node each of the frame's meshes draws.
    pub mesh_nodes: Vec<u32>,
    /// The scene's surface nodes, depth first.
    pub nodes: Vec<CoverNode>,
    /// Where the layer sits: its box (of `size`, in document units) to
    /// the document.
    pub place: Affine,
    pub size: [f64; 2],
}

impl SceneCoverage {
    /// Measure the map as read back (tight RGBA rows of `width` × `height`).
    pub fn measure(&self, rgba: &[u8], width: usize, height: usize) -> FrameCover {
        let mut cover = FrameCover::measure(rgba, width, height, &self.mesh_nodes);
        cover.place = self.place;
        cover.size = self.size;
        cover
    }
}

/// What one node covers in one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeCover {
    pub node: u32,
    pub pixels: u64,
    /// `[x0, y0, x1, y1)` in the map's pixels, top-left origin.
    pub bounds: [u32; 4],
}

/// One frame's coverage of one scene: the map's size, each node seen, and
/// where the layer sits (see [`SceneCoverage`]).
#[derive(Clone, Debug, PartialEq)]
pub struct FrameCover {
    pub width: usize,
    pub height: usize,
    pub nodes: Vec<NodeCover>,
    pub place: Affine,
    pub size: [f64; 2],
}

impl FrameCover {
    /// Read a coverage map as read back (tight RGBA rows); `mesh_nodes`
    /// gives the node each mesh draws. Placed as the map's own pixels.
    pub fn measure(rgba: &[u8], width: usize, height: usize, mesh_nodes: &[u32]) -> FrameCover {
        let mut nodes: Vec<NodeCover> = Vec::new();
        for (i, px) in rgba.chunks_exact(4).take(width * height).enumerate() {
            let id = px[0] as usize | (px[1] as usize) << 8;
            let Some(&node) = id.checked_sub(1).and_then(|mesh| mesh_nodes.get(mesh)) else { continue };
            let (x, y) = ((i % width) as u32, (i / width) as u32);
            match nodes.iter_mut().find(|n| n.node == node) {
                Some(n) => {
                    n.pixels += 1;
                    n.bounds = [n.bounds[0].min(x), n.bounds[1].min(y), n.bounds[2].max(x + 1), n.bounds[3].max(y + 1)];
                }
                None => nodes.push(NodeCover { node, pixels: 1, bounds: [x, y, x + 1, y + 1] }),
            }
        }
        nodes.sort_by_key(|n| n.node);
        FrameCover { width, height, nodes, place: Affine::IDENTITY, size: [width as f64, height as f64] }
    }

    /// The share of the picture node `node` covers, 0..1.
    pub fn share(&self, node: u32) -> f64 {
        let total = (self.width * self.height).max(1) as f64;
        self.nodes.iter().find(|n| n.node == node).map_or(0.0, |n| n.pixels as f64 / total)
    }

    /// The share no node covers (the background).
    pub fn empty(&self) -> f64 {
        let total = (self.width * self.height).max(1) as f64;
        1.0 - self.nodes.iter().map(|n| n.pixels).sum::<u64>() as f64 / total
    }

    /// Node `node`'s box `[x, y, w, h]` in document units (the bounds of
    /// its pixels' box where the layer sits), when seen.
    pub fn bounds(&self, node: u32) -> Option<[f64; 4]> {
        let [x0, y0, x1, y1] = self.nodes.iter().find(|n| n.node == node)?.bounds;
        let (kx, ky) = (self.size[0] / self.width.max(1) as f64, self.size[1] / self.height.max(1) as f64);
        let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]].map(|[x, y]| self.place.apply([x as f64 * kx, y as f64 * ky]));
        let (lo, hi) = corners.iter().fold(([f64::MAX; 2], [f64::MIN; 2]), |(lo, hi), p| ([lo[0].min(p[0]), lo[1].min(p[1])], [hi[0].max(p[0]), hi[1].max(p[1])]));
        Some([lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]])
    }
}

/// `0.4%`, `12%`.
pub fn percent(share: f64) -> String {
    let p = share * 100.0;
    if p > 0.0 && p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.0}%")
    }
}

/// A scene's coverage over sampled times as text: per surface node, its
/// share of the scene's picture at each time and its box where it is
/// largest, or that it was never seen; then what no node covers.
pub fn report(scene: &str, nodes: &[CoverNode], frames: &[(f64, FrameCover)]) -> String {
    let Some((_, first)) = frames.first() else { return String::new() };
    let times: Vec<String> = frames.iter().map(|(t, _)| format!("{t:.2}")).collect();
    let mut out = format!("coverage of {scene} ({:.0}x{:.0}) at {} s: each node's share of the scene's picture, and its box (x,y w×h in the document) where largest", first.size[0], first.size[1], times.join(" "));
    for node in nodes {
        let largest = frames.iter().filter(|(_, f)| f.share(node.id) > 0.0).max_by(|a, b| a.1.share(node.id).total_cmp(&b.1.share(node.id)));
        let line = match largest.and_then(|(t, f)| Some((t, f.bounds(node.id)?))) {
            Some((t, [x, y, w, h])) => {
                let shares: Vec<String> = frames.iter().map(|(_, f)| percent(f.share(node.id))).collect();
                format!("{}; box {x:.0},{y:.0} {w:.0}x{h:.0} at {t:.2} s", shares.join(" "))
            }
            None => "not seen at any sampled time".to_string(),
        };
        out.push_str(&format!("\n  {} (line {}): {line}", node.label(), node.line));
    }
    let empty: Vec<String> = frames.iter().map(|(_, f)| percent(f.empty())).collect();
    out.push_str(&format!("\n  no node (the background): {}", empty.join(" ")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4×2 map: mesh 1 (node 7) on two pixels of the top row, mesh 2
    /// (node 9) on three, one pixel of a mesh the frame does not have.
    fn map() -> Vec<u8> {
        let px = |id: u16| [(id & 255) as u8, (id >> 8) as u8, 0, 255];
        [1u16, 1, 0, 2, 0, 2, 2, 300].iter().flat_map(|id| px(*id)).collect()
    }

    #[test]
    fn a_map_gives_each_node_its_share_and_box() {
        let cover = FrameCover::measure(&map(), 4, 2, &[7, 9]);
        assert_eq!(cover.nodes, vec![NodeCover { node: 7, pixels: 2, bounds: [0, 0, 2, 1] }, NodeCover { node: 9, pixels: 3, bounds: [1, 0, 4, 2] }]);
        assert_eq!((cover.share(7), cover.share(9), cover.share(8)), (0.25, 0.375, 0.0));
        assert_eq!(cover.empty(), 0.375, "the empty pixels and the one naming no mesh");
        assert_eq!(cover.bounds(9), Some([1.0, 0.0, 3.0, 2.0]));
        // A map drawn at twice the layer's size, the layer moved to 100, 50:
        // boxes in document units.
        let placed = FrameCover { place: Affine::translate(100.0, 50.0), size: [2.0, 1.0], ..cover.clone() };
        assert_eq!(placed.bounds(9), Some([100.5, 50.0, 1.5, 1.0]));
        // Two meshes of one node add up.
        let both = FrameCover::measure(&map(), 4, 2, &[7, 7]);
        assert_eq!(both.nodes, vec![NodeCover { node: 7, pixels: 5, bounds: [0, 0, 4, 2] }]);
    }

    #[test]
    fn the_report_names_each_node_and_where_it_is_largest() {
        let node = |id: u32, name: Option<&str>, path: &str, line: u32| CoverNode { id, name: name.map(str::to_string), path: path.into(), line };
        let nodes = [node(7, Some("hero"), "children[0]", 4), node(9, None, "children[1]", 6), node(11, Some("prop"), "children[2]", 9)];
        let frames = [(0.0, FrameCover::measure(&map(), 4, 2, &[7, 9])), (1.5, FrameCover::measure(&map(), 4, 2, &[9, 7]))];
        assert_eq!(
            report("layers[0]", &nodes, &frames),
            "coverage of layers[0] (4x2) at 0.00 1.50 s: each node's share of the scene's picture, and its box (x,y w×h in the document) where largest\n  \
             \"hero\" (line 4): 25% 38%; box 1,0 3x2 at 1.50 s\n  \
             children[1] (line 6): 38% 25%; box 1,0 3x2 at 0.00 s\n  \
             \"prop\" (line 9): not seen at any sampled time\n  \
             no node (the background): 38% 38%"
        );
    }
}
