//! The Board pane: a board's layers drawn from retained per-layer batches of instanced analytic
//! shapes, composited layer by layer, with the dynamic overlay of a gesture on top.
//!
//! - `scene`: a board as drawable shapes per layer, tagged with their items (pure, worker side).
//! - `checks`: live findings as markers and lit scene shapes (pure, worker side).
//! - `gen`: a generated board of any size, for the 100 000 segment proof.
//! - `inst`: the shaders' instance rows (the canvas common block, then the shape).
//! - `polys`: polygon fills triangulated once on the worker, with rings for hairline outlines.
//! - `batch`: a scene as instance rows per (layer, kind), cut per tile (pure, worker side).
//! - `held`: a dragged item's copies (at the cursor, and its ghost), drawn over the layers.
//! - `rats`: the ratsnest hairlines; ends on a dragged item follow it in the frame.
//! - `draw`: the shaders (strokes and arcs, rounded pads with holes, the layer composite).
//! - `flow`: stable wire graphs per net for live segment currents (pure, worker side).
//! - `shared`: one board's drawing built once and shared by every view of it (panes, 3D, art).
//! - `lists`: drawing board publications as retained draw lists (the one drawing entry point).
//! - `art`: GPU layer-art tiles drawn from the board's shared publications for the 3D material.
//! - `liveix`: the simulation host's net, segment and part rows for the board instances.
//! - `marks`: check markers and labels over the board.
//! - `pick`: what is under the pointer, topmost first.
//! - `nodes`: the board's items as canvas nodes (semantic boxes for remote control and tests).
//! - `rail`: the layer rail (activate, show or hide, solo).
//! - `status`: the board counts, active layer, copper dimensions and snap grid along the bottom.
//! - `tools`: the tool strip along the pane's left edge (the modes, routing, the board's actions).
//! - `tray`: unplaced circuit parts and the rows that start their placement gestures.
//! - `findings`: the Checks list and its shared focus and hover actions.
//! - `theme`: the layer stack order and the inks of the dark and light canvas.
//! - `view`: the Board pane widget (layer targets, composite, holes, hover, pan and zoom).
//! - `tiles`: layer art, each layer's coverage rasterised into board-space tiles (what the 3D
//!   board material samples).

use makepad_widgets_core::{ScriptValue, ScriptVm};

pub mod art;
pub mod batch;
pub mod checks;
pub mod draw;
pub mod flow;
pub mod focus;
pub mod gen;
pub mod gesture;
pub mod heatkey;
pub mod held;
pub mod inst;
pub mod lists;
pub mod liveix;
pub mod marks;
pub mod nodes;
pub mod pick;
pub mod plan;
pub mod polys;
pub mod rats;
pub mod rail;
pub mod routeline;
pub mod scene;
pub mod shared;
pub mod status;
pub mod theme;
pub mod tiles;
pub mod tools;
pub mod tray;
pub mod findings;
pub mod view;

pub use view::{BoardPane, BoardStats};
pub use shared::{BoardDraw, BoardDraws};
pub use tiles::{ArtPlane, BoardArt, LayerArt, TileId, ART_APRON, ART_BASE_NM, ART_MAX_LOD, ART_TILE_STORED, ART_TILE_TEXELS};

/// Registers the board shaders, the canvas shapes the pane's refusal chip draws with
/// (`canvas::draw`), `mod.widgets.BoardLayerRail`, `mod.widgets.BoardTray`,
/// `mod.widgets.BoardChecks` and `mod.widgets.BoardPane` (after the widgets' own script_mod
/// and `live::script_mod`, whose `mod.elec_live` the board and canvas shaders use), and the probe
/// flags both canvas panes draw (`live::flags`, with the probe tag they wear).
pub fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
    crate::canvas::draw::script_mod(vm);
    crate::live::tag::script_mod(vm);
    crate::live::flags::script_mod(vm);
    draw::script_mod(vm);
    art::script_mod(vm);
    marks::script_mod(vm);
    heatkey::script_mod(vm);
    routeline::script_mod(vm);
    rail::script_mod(vm);
    tray::script_mod(vm);
    findings::script_mod(vm);
    crate::req_list::script_mod(vm);
    crate::bom::script_mod(vm);
    view::script_mod(vm)
}
