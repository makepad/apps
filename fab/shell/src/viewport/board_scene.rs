//! Pure preparation of every board in an electronics scene for the 3D board lane.

use super::board::BoardLook;
use super::board_geom::{board_mesh, BoardMesh, BOARD_VERTEX_FLOATS};
use super::bodies::xform_mat;
use super::*;
use fab_elec_board::{Finish, StackKind, Stackup};
use fab_elec_core::{BBox, BoardId, P};
use fab_elec_mech::scene::ElecScene;
use fab_elec_view::board::tiles::{TileId, ART_MAX_LOD};
use fab_materials::{Colour, MaterialId};

/// Pool-prepared geometry and appearance for one board, without GPU resources.
pub struct SceneBoardPrep {
    pub unit: BoardId,
    /// Board millimetres to scene millimetres.
    pub scene_from_board: Mat4f,
    pub faces: BoardMesh,
    pub edges: BoardMesh,
    /// Bounds in the board's own millimetres.
    pub bounds: Aabb,
    pub thickness_mm: f32,
    pub roots: Vec<TileId>,
    pub look: BoardLook,
}

/// Separate board faces from walls and barrels, retaining bounds in board millimetres.
pub fn split_board_mesh(mesh: BoardMesh) -> (BoardMesh, BoardMesh, Aabb) {
    let mut faces = BoardMesh::default();
    let mut edges = BoardMesh::default();
    let mut remap = Vec::with_capacity(mesh.vertices.len() / BOARD_VERTEX_FLOATS);
    let mut bounds = aabb_empty();
    for v in mesh.vertices.chunks_exact(BOARD_VERTEX_FLOATS) {
        bounds.min.x = bounds.min.x.min(v[0]);
        bounds.min.y = bounds.min.y.min(v[1]);
        bounds.min.z = bounds.min.z.min(v[2]);
        bounds.max.x = bounds.max.x.max(v[0]);
        bounds.max.y = bounds.max.y.max(v[1]);
        bounds.max.z = bounds.max.z.max(v[2]);
        let target = if v[3] < 1.5 { &mut faces } else { &mut edges };
        remap.push((target.vertices.len() / BOARD_VERTEX_FLOATS) as u32);
        target.vertices.extend_from_slice(v);
    }
    for triangle in mesh.indices.chunks_exact(3) {
        let kind = mesh.vertices[triangle[0] as usize * BOARD_VERTEX_FLOATS + 3];
        let target = if kind < 1.5 { &mut faces } else { &mut edges };
        target.indices.extend(triangle.iter().map(|i| remap[*i as usize]));
    }
    (faces, edges, bounds)
}

/// Board-space bounds (mm) as the artwork grid's box: outward on the nm grid, with an exclusive
/// maximum. The board's shared art cache and its root tiles both use it.
pub fn art_box(bounds: &Aabb) -> BBox {
    let min = P::new((bounds.min.x as f64 * 1e6).floor() as i32, (bounds.min.y as f64 * 1e6).floor() as i32);
    let max = P::new((bounds.max.x as f64 * 1e6).ceil() as i32, (bounds.max.y as f64 * 1e6).ceil() as i32);
    BBox::new(min, P::new(max.x.saturating_sub(1).max(min.x), max.y.saturating_sub(1).max(min.y)))
}

/// Cover board-space bounds with the root tiles of the fixed artwork grid.
pub fn root_tiles(bounds: &Aabb) -> Vec<TileId> {
    let tile_bounds = art_box(bounds);
    let mut lod = 0;
    while lod < ART_MAX_LOD {
        let span = TileId::span(lod);
        let cells = |a: i32, b: i32| (b as i64).div_euclid(span) - (a as i64).div_euclid(span) + 1;
        if cells(tile_bounds.min.x, tile_bounds.max.x) <= 2 && cells(tile_bounds.min.y, tile_bounds.max.y) <= 2 {
            break;
        }
        lod += 1;
    }
    TileId::covering(lod, tile_bounds)
}

fn copper_of(finish: Finish) -> MaterialId {
    let name = match finish {
        Finish::None => "cu_bare",
        Finish::Hasl => "hasl",
        Finish::LeadFreeHasl => "hasl_lf",
        Finish::Enig => "enig",
        Finish::Osp => "osp",
        Finish::ImmersionSilver => "imm_silver",
        Finish::ImmersionTin => "imm_tin",
    };
    MaterialId::lookup(name).expect("board finish rows are in the material table")
}

/// Resolve the stackup's materials, colours and thicknesses into board shader uniforms.
pub fn look_of(stackup: &Stackup) -> BoardLook {
    let copper = copper_of(stackup.finish);
    let laminate = stackup.layers.iter()
        .find(|layer| matches!(layer.kind, StackKind::Dielectric { core: true }))
        .and_then(|layer| MaterialId::lookup(&layer.material.to_ascii_lowercase()))
        .unwrap_or_else(|| MaterialId::lookup("fr4").expect("FR-4 is in the material table"));
    // Stackup colours are 0xRRGGBBAA; Colour::hex takes 0xRRGGBB.
    let mask = Colour::hex(stackup.mask_color >> 8).srgb;
    let silk = Colour::hex(stackup.silk_color >> 8).srgb;
    let thickness_mm = stackup.thickness as f32 * 1e-6;
    // Layers are top to bottom, so the first copper layer is the outermost top copper.
    let copper_mm = stackup.layers.iter()
        .find(|layer| matches!(layer.kind, StackKind::Copper(_)))
        .map_or(0.035, |layer| layer.thickness as f32 * 1e-6);
    let look = fab_materials::shade::board_look(copper, laminate, mask, silk);
    BoardLook::resolve(&look, thickness_mm, copper_mm)
}

/// Prepare every board on the pool when scene content changes, never per frame.
/// Cost is O(outline vertices + holes) per board at the fixed meshing tolerance.
pub fn prepare_scene(scene: &ElecScene) -> Vec<SceneBoardPrep> {
    scene.boards.iter().map(|board| {
        let (faces, edges, bounds) = split_board_mesh(board_mesh(board, 2_000));
        let roots = root_tiles(&bounds);
        SceneBoardPrep {
            unit: board.unit,
            scene_from_board: xform_mat(&board.pose),
            faces,
            edges,
            bounds,
            thickness_mm: board.thickness as f32 * 1e-6,
            roots,
            look: look_of(&board.art.stackup),
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_mech::scene::scene_of;

    #[test]
    fn prepares_ledctl_assembly() {
        let scene = scene_of(&fab_elec_fixtures::ledctl_pair(), &fab_elec_mech::Pose::default());
        let prepared = prepare_scene(&scene);
        assert_eq!(prepared.len(), 2, "the assembly prepares two boards");
        for (index, (board, prep)) in scene.boards.iter().zip(&prepared).enumerate() {
            assert!(
                !prep.faces.vertices.is_empty() && !prep.faces.indices.is_empty(),
                "board {index} has non-empty faces",
            );
            assert!(
                !prep.edges.vertices.is_empty() && !prep.edges.indices.is_empty(),
                "board {index} has non-empty edges",
            );
            assert!(!prep.roots.is_empty(), "board {index} has at least one root tile");
            assert!(prep.thickness_mm > 0.0, "board {index} has positive thickness");
            let copper = fab_materials::shade::shade(&fab_materials::Surface::new(copper_of(board.art.stackup.finish)));
            assert_eq!(copper.metal, 1.0, "board {index} uses a metal copper finish");
            assert_eq!(
                prep.look.finish,
                [copper.base[0], copper.base[1], copper.base[2], copper.roughness],
                "board {index} finish carries the metal copper shading values",
            );
            assert!((0.0..1.0).contains(&prep.look.finish[3]), "board {index} copper roughness is in 0..1");
        }
        assert_ne!(
            prepared[0].scene_from_board.v[12..15],
            prepared[1].scene_from_board.v[12..15],
            "the boards have distinct scene translations",
        );
    }
}
