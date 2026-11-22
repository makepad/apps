//! The 2D canvas base every electronics 2D view uses (ARCHITECTURE 6.10): the camera and its
//! navigation, the level-of-detail ladder, picking and the virtual semantic nodes, the state
//! looks of ARCHITECTURE 6.6 and the tokens of 6.7. The explorer, the board, the Part and the
//! proof views build on it; nothing in it knows circuits or boards.
//!
//! - `camera`: `Camera2d`, `Ladder`, and the `CanvasItems` / `CanvasNode` seam.
//! - `looks`: the state looks, colour roles (dark and light), line weights, motion timings.
//! - `nav`: wheel glides, pinch, pans with fling, hop flights (over `makepad_explore_kit`).
//! - `pane`: a pane's input as the modeless grammar, hover, semantic nodes, frame requests.
//! - `chips`: small screen-space labels beside the pointer or an item that say what a gesture does.
//! - `emitter`: an LED's own colour from its part's emitter data, for the glow and the 3D view.
//! - `tools`: a pane's tool strip: the armed-tool model (one gesture, or kept) and its place.

pub mod camera;
pub mod chips;
pub mod draw;
pub mod emitter;
pub mod looks;
pub mod nav;
pub mod pane;
pub mod tools;

pub use camera::{Camera2d, CanvasItems, CanvasNode, Ladder, LadderAt, Rung, MAX_RUNGS};
pub use looks::{Look, Tokens};
pub use nav::Nav;
pub use pane::{CanvasInput, CanvasPane};
