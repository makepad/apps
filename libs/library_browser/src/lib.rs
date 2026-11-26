//! The library browser the product windows share (Story's CUT and DECK,
//! Stage's SCORE): the card list and its draw primitives ([`list`],
//! [`draw`]), the lanes and their rows read from the asset store
//! ([`lanes`]), the catalogue worker of a window with no documents
//! ([`catalog`]), and the local drafts of the open documents ([`drafts`]).

pub mod catalog;
pub mod drafts;
pub mod draw;
pub mod lanes;
pub mod list;

use makepad_widgets::ScriptVm;

/// The list's widgets and the draw shaders, after the theme (`mod.stage`).
pub fn script_mod(vm: &mut ScriptVm) {
    draw::script_mod(vm);
    list::script_mod(vm);
}
