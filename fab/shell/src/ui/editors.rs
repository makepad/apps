//! The editors an area can show (design FABWS: Blender's editor types): each one's dock tab, its
//! body's kind, its names and whether two areas can show it at once, in one table.
//!
//! An editor shows in the dock as a tab whose body the shell's `Dock` template of the same kind
//! makes. A repeatable editor has one body per instance ([`crate::ui::areas::Instance`]); any
//! other has one body for the window.

use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::*;

/// An editor type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, SerRon, DeRon)]
pub enum Editor {
    Viewport,
    Device,
    /// Every plate and the demos: the first tab on the left.
    Plates,
    Parts,
    Part,
    Slicer,
    Settings,
    Printer,
    Calibrate,
    Camera,
    Features,
    Code,
    Mechanics,
    Slice,
    /// The Arm workspace's run controls, figures and checks, and its view of the cell.
    Arm,
    ArmCell,
    /// The Arm workspace's comparison of every option's time against the H2D's.
    ArmCompare,
    /// Design's Force render mode: the study's material, fixtures and loads, and results.
    Force,
    /// The Scan workspace's controls and figures, and its view of the scan.
    Scan,
    ScanView,
    /// The board canvas (the shared drawing of the design's board).
    Board,
    /// The circuit explorer: the schematic that draws itself.
    Circuit,
    /// The evaluated bench's drive cards and scope cards.
    Bench,
    /// The logic analyser shows each probe on a shared time axis.
    Scope,
    /// The Electronics frame's top row (the layouts).
    Transport,
    /// The bench's firmware source and build status.
    Firmware,
    /// The plate's print settings overrides: every override, the print settings volumes, and the
    /// picked volume's size and place.
    Overrides,
    /// Test prints: the small prints that try a setting before the whole plate.
    TestPrints,
    /// What would break first, from the slice, and the fixes that help, as one undo step.
    Strength,
    /// The selected part's shape parameters.
    Parameters,
    /// The electronics library: search, pin and place parts (the Schematic's float is its quick
    /// open while no area shows it).
    Library,
    /// The shown unit's checks: the circuit's findings over its board's.
    Checks,
    /// The run board's keys and knobs (the Transport's controls header brings it forward).
    BoardControls,
    /// The paint brush's colour and settings (the plate's rail keeps the paint tools).
    Paint,
    /// Saved print jobs and the printer's progress.
    Jobs,
    /// The frame profiler graph.
    Performance,
    /// Where the 3D models of electronics parts may come from, and every external resource's
    /// licence.
    ModelSources,
}

/// The kinds of editor the app's gates ask about: a sim, a host or a park follows whether an
/// editor of the family is on screen, not which workspace is chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// The Plate editor: the plate mode ([`crate::api::PlateMode`]) acts while it shows.
    Plate,
    /// The arm cell, its run's controls and the comparison.
    Arm,
    /// The scan's view and its controls.
    Scan,
    /// The schematic, the board, the bench, the scope, the firmware and the recorder.
    Electronics,
}

/// The area menu's groups (Blender's editor-type menu): a heading and its editors, in order.
/// Experimental's editors are offered only while View › Experimental has their family on.
pub const MENU: [(&str, &[Editor]); 7] = [
    ("3D views", &[Editor::Viewport, Editor::Device]),
    ("Plate data", &[Editor::Plates, Editor::Parts, Editor::Part, Editor::Parameters, Editor::Paint, Editor::Features, Editor::Code]),
    ("Print", &[Editor::Slicer, Editor::Overrides, Editor::TestPrints, Editor::Settings, Editor::Printer, Editor::Calibrate, Editor::Camera, Editor::Jobs]),
    ("Analysis", &[Editor::Slice, Editor::Strength, Editor::Force, Editor::Mechanics, Editor::Performance]),
    ("Electronics", &[Editor::Circuit, Editor::Board, Editor::Library, Editor::Checks, Editor::Bench, Editor::BoardControls, Editor::Scope, Editor::Transport, Editor::Firmware]),
    ("Experimental", &[Editor::Arm, Editor::ArmCell, Editor::ArmCompare, Editor::Scan, Editor::ScanView]),
    ("Settings", &[Editor::ModelSources]),
];

/// What the app knows of an editor type.
pub struct EditorDef {
    pub editor: Editor,
    /// The dock tab of its first instance.
    pub tab: LiveId,
    /// The kind of its body: the name of its template in the shell's `Dock`.
    pub kind: LiveId,
    /// Its tab's name.
    pub name: &'static str,
    /// Its name in an area's menu.
    pub title: &'static str,
    /// Whether two areas can show it at once, each its own instance (two Scopes at two zooms).
    pub repeatable: bool,
}

const fn def(editor: Editor, tab: LiveId, kind: LiveId, name: &'static str, title: &'static str, repeatable: bool) -> EditorDef {
    EditorDef { editor, tab, kind, name, title, repeatable }
}

/// Every editor, in [`Editor`]'s order.
pub const EDITORS: [EditorDef; 37] = [
    def(Editor::Viewport, live_id!(tab_viewport), live_id!(Viewport), "Plate", "Plate", true),
    def(Editor::Device, live_id!(tab_device), live_id!(Device), "3D view", "3D view", false),
    def(Editor::Plates, live_id!(tab_plates), live_id!(Plates), "Plates", "Plates", false),
    def(Editor::Parts, live_id!(tab_parts), live_id!(Parts), "Parts", "Parts", false),
    def(Editor::Part, live_id!(tab_part), live_id!(Part), "Part", "Part", false),
    def(Editor::Slicer, live_id!(tab_slicer), live_id!(Slicer), "Slicer", "Slicer", false),
    def(Editor::Settings, live_id!(tab_settings), live_id!(Settings), "Job", "Job", false),
    def(Editor::Printer, live_id!(tab_printer), live_id!(Printer), "Printer", "Printer", false),
    def(Editor::Calibrate, live_id!(tab_calibrate), live_id!(Calibrate), "Calibrate", "Calibrate", false),
    def(Editor::Camera, live_id!(tab_camera), live_id!(Camera), "Camera", "Camera", false),
    def(Editor::Features, live_id!(tab_features), live_id!(Features), "Features", "Features", false),
    def(Editor::Code, live_id!(tab_code), live_id!(Code), "Program", "Program", false),
    def(Editor::Mechanics, live_id!(tab_mechanics), live_id!(Mechanics), "Mechanics", "Mechanics", false),
    def(Editor::Slice, live_id!(tab_slice), live_id!(Slice), "Slice", "Slice", false),
    def(Editor::Arm, live_id!(tab_arm), live_id!(Arm), "Arm", "Arm", false),
    def(Editor::ArmCell, live_id!(tab_arm_cell), live_id!(ArmCell), "Cell", "Arm cell", false),
    def(Editor::ArmCompare, live_id!(tab_arm_compare), live_id!(ArmCompare), "Compare", "Compare", false),
    def(Editor::Force, live_id!(tab_force), live_id!(Force), "Force", "Force", false),
    def(Editor::Scan, live_id!(tab_scan), live_id!(Scan), "Scan", "Scan", false),
    def(Editor::ScanView, live_id!(tab_scan_view), live_id!(ScanView), "Scan view", "Scan view", false),
    def(Editor::Board, live_id!(tab_board), live_id!(Board), "Board", "Board", false),
    def(Editor::Circuit, live_id!(tab_circuit), live_id!(Circuit), "Schematic", "Schematic", false),
    def(Editor::Bench, live_id!(tab_bench), live_id!(Bench), "Bench", "Bench", false),
    def(Editor::Scope, live_id!(tab_scope), live_id!(Scope), "Scope", "Scope", true),
    def(Editor::Transport, live_id!(tab_transport), live_id!(Transport), "Transport", "Transport", false),
    def(Editor::Firmware, live_id!(tab_firmware), live_id!(Firmware), "Firmware", "Firmware", false),
    def(Editor::Overrides, live_id!(tab_overrides), live_id!(Overrides), "Overrides", "Overrides", false),
    def(Editor::TestPrints, live_id!(tab_test_prints), live_id!(TestPrints), "Test prints", "Test prints", false),
    def(Editor::Strength, live_id!(tab_strength), live_id!(Strength), "Strength", "Strength", false),
    def(Editor::Parameters, live_id!(tab_parameters), live_id!(Parameters), "Parameters", "Parameters", false),
    def(Editor::Library, live_id!(tab_library), live_id!(Library), "Library", "Library", false),
    def(Editor::Checks, live_id!(tab_checks), live_id!(Checks), "Checks", "Checks", false),
    def(Editor::BoardControls, live_id!(tab_board_controls), live_id!(BoardControls), "Board controls", "Board controls", false),
    def(Editor::Paint, live_id!(tab_paint), live_id!(Paint), "Paint", "Paint", false),
    def(Editor::Jobs, live_id!(tab_jobs), live_id!(Jobs), "Jobs", "Jobs", false),
    def(Editor::Performance, live_id!(tab_performance), live_id!(Performance), "Performance", "Performance", false),
    def(Editor::ModelSources, live_id!(tab_model_sources), live_id!(ModelSources), "Model sources", "Model sources", false),
];

// The table is in the enum's order, so an editor finds its row by its discriminant; each editor
// has a bit of `UiState::screen`.
const _: () = {
    assert!(EDITORS.len() <= 64);
    let mut i = 0;
    while i < EDITORS.len() {
        assert!(EDITORS[i].editor as usize == i);
        i += 1;
    }
};

impl Editor {
    pub fn def(self) -> &'static EditorDef {
        &EDITORS[self as usize]
    }

    pub fn tab(self) -> LiveId {
        self.def().tab
    }

    pub fn kind(self) -> LiveId {
        self.def().kind
    }

    pub fn name(self) -> &'static str {
        self.def().name
    }

    pub fn title(self) -> &'static str {
        self.def().title
    }

    pub fn repeatable(self) -> bool {
        self.def().repeatable
    }

    pub fn has_tools(self) -> bool {
        matches!(self, Editor::Viewport | Editor::Circuit)
    }

    pub fn tools_move(self) -> bool {
        self == Editor::Circuit
    }

    /// Its body keeps the top-left corner clear for its area's nub: alone in an area it shows no
    /// tab bar, only the nub. Every other editor alone keeps a tab bar of one, the nub at its
    /// left end, so the nub covers none of it.
    pub fn keeps_corner(self) -> bool {
        matches!(self, Editor::Viewport | Editor::Code | Editor::Device | Editor::Board | Editor::Circuit | Editor::Bench | Editor::Scope | Editor::Transport | Editor::Firmware)
    }

    /// The editor whose bodies are of `kind`.
    /// The family whose work it shows: the Plate view makes a plate on screen ([`Family::Plate`]);
    /// the plate's panels belong to none.
    pub fn family(self) -> Option<Family> {
        match self {
            Editor::Viewport => Some(Family::Plate),
            Editor::Arm | Editor::ArmCell | Editor::ArmCompare => Some(Family::Arm),
            Editor::Scan | Editor::ScanView => Some(Family::Scan),
            Editor::Device | Editor::Board | Editor::Circuit | Editor::Bench | Editor::Scope | Editor::Transport | Editor::Firmware
            | Editor::Library | Editor::Checks | Editor::BoardControls => Some(Family::Electronics),
            _ => None,
        }
    }

    pub fn of_kind(kind: LiveId) -> Option<Editor> {
        EDITORS.iter().find(|d| d.kind == kind).map(|d| d.editor)
    }

    /// The [`crate::api::UiState::screen`] bits of the editors whose bodies are of `kinds` (each
    /// instance's body has its editor's kind). O(kinds).
    pub fn screen_bits(kinds: impl Iterator<Item = LiveId>) -> u64 {
        kinds.filter_map(Editor::of_kind).fold(0, |bits, editor| bits | 1 << editor as u64)
    }

    /// Every editor, in the table's order.
    pub fn all() -> impl Iterator<Item = Editor> {
        EDITORS.iter().map(|d| d.editor)
    }
}
