//! Fab electronics views (contract C6): the design snapshot every view reads, the actions views
//! emit (ops, previews, focus, hover, the time cursor), and the views themselves (explorer,
//! schematic, board, 3D, instruments). Views never touch source text.

pub mod board;
pub mod canvas;
pub mod explorer;
pub mod firmware;
pub mod fw_rows;
pub mod fw_state;
pub mod instruments;
pub mod library;
pub mod live;
pub mod proof;
pub mod snapshot;
pub mod states;
pub mod trace;

pub use snapshot::{DesignSnapshot, DocText, DocsView, JobEnd, Refusal, RouteJobState, StageFlags, StateOf, Tagged, ViewAction};
pub use snapshot::{ElecLive, ElecPanel};
pub use states::{board_rows, board_texels, part_rows, sheet_rows, sheet_texels, with_flash, ItemStates, SheetIndex, StatesOf, ViewState};

#[cfg(test)]
mod states_tests;
