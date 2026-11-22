//! The electronics workspaces' arrangements (V9, user 11-18: "its more about what i wanna see at
//! the same time"): areas, each showing an editor (Scope, the 3D view, Schematic, Board, then
//! Firmware and Bench), named by what they show. Seven defaults; the workspaces themselves are
//! [`crate::workspaces`]' (each default sits over the recorder there), the areas and their edits
//! [`crate::ui::areas`]'.
//!
//! Costs: each function is O(areas), on a click or a key; nothing here runs per frame.

use super::areas::AreaTree;
use super::editors::Editor;
pub use super::areas::layout_map;

/// The 3D editor: the Device view, the device on its own camera (FabDevice3d).
pub const ELEC_3D: Editor = Editor::Device;

/// The editors an area offers, in its menu's order: the four core ones, then the other two.
pub const EDITORS: [Editor; 6] = [Editor::Scope, ELEC_3D, Editor::Circuit, Editor::Board, Editor::Firmware, Editor::Bench];

/// The seven arrangements (V9.2), by name, in the caption's order.
pub fn defaults() -> Vec<(&'static str, AreaTree)> {
    use AreaTree as T;
    let area = T::area;
    vec![
        ("Schematic", T::over(area(Editor::Circuit), area(Editor::Scope), 0.64)),
        ("Schematic | Board", T::over(T::beside(area(Editor::Circuit), area(Editor::Board), 0.5), area(Editor::Scope), 0.66)),
        ("Board + 3D", T::beside(area(Editor::Board), area(ELEC_3D), 0.58)),
        ("Device + Scope", T::over(area(ELEC_3D), area(Editor::Scope), 0.55)),
        ("Firmware", T::beside(area(Editor::Firmware), T::over(area(ELEC_3D), area(Editor::Scope), 0.5), 0.5)),
        ("All four", T::over(T::beside(area(Editor::Circuit), area(Editor::Board), 0.5), T::beside(area(Editor::Scope), area(ELEC_3D), 0.66), 0.58)),
        ("Bench", T::beside(area(Editor::Bench), area(Editor::Scope), 0.5)),
    ]
    .into_iter()
    .map(|(name, tree)| (name, tree.numbered()))
    .collect()
}

/// The first editor of the area menu's list that `tree` does not show, else another Scope: what a
/// split shows.
pub fn next_editor(tree: &AreaTree) -> Editor {
    EDITORS.into_iter().find(|e| tree.showing(*e).is_none()).unwrap_or(Editor::Scope)
}
