//! Manufacturing actions shared by the command palette, UI and AI tools.

use fab_elec_design::action::{ActionDef, FieldDef, FieldKind, Risk, SelKind};

pub static ACTIONS: &[ActionDef] = &[
    ActionDef {
        name: "elec.export",
        label: "Export board",
        group: "Fabricate",
        help: "Plot the board's fabrication files (Gerber copper, mask, paste, silk and edge layers, Excellon drills, BOM and placement tables, a readme and the zip) through its factory profile or the named one, write them to a folder, read every file back and check it against the board. Waits for the person to approve it.",
        returns: "folder: text  where the files are\nfiles: [text]  the exported file names\nzip: text  the package to upload, when the profile makes one\nprofile: text\nvalidation: {..}  every file read back and compared with the board\n  valid: bool  every check passed\n  passed: int  checks that passed\n  failed: [{..}]  each check that failed\n    file: text\n    check: text\n    cause: text  what differs\n    hint: text  what to change\n    at: point  where on the board, in mm, when it is one place",
        fields: &[
            FieldDef {
                name: "board",
                kind: FieldKind::Sel(SelKind::Unit),
                help: "The board unit's name; needed only when the design has more than one board.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "profile",
                kind: FieldKind::Text,
                help: "A built-in or project factory profile. Defaults to the board's factory, then standard 2-layer.",
                required: false,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::External,
        keys: "",
    },
    ActionDef {
        name: "elec.export.validate",
        label: "Validate export",
        group: "Fabricate",
        help: "Read back the last export, or the files in the named folder, and check them against the board as it is now.",
        returns: "folder: text\nprofile: text\nvalid: bool  every check passed\npassed: int  checks that passed\nfailed: [{..}]  each check that failed\n  file: text\n  check: text\n  cause: text  what differs\n  hint: text  what to change\n  at: point  where on the board, in mm, when it is one place",
        fields: &[FieldDef {
            name: "folder",
            kind: FieldKind::Text,
            help: "A folder of exported files, relative to the project's fabrication folder (its board's name, as an export writes it); the last export's when omitted.",
            required: false,
            default: "",
        }],
        inputs: &[],
        risk: Risk::Read,
        keys: "",
    },
    ActionDef {
        name: "elec.factory.set",
        label: "Set factory profile",
        group: "Fabricate",
        help: "Choose the board's factory profile; its capability rules become the board's manufacturing rules.",
        returns: "seq: int  the step it committed\nlabel: text  its name in the undo history, with the profile (Factory of main: JLCPCB 2-layer)",
        fields: &[
            FieldDef {
                name: "board",
                kind: FieldKind::Sel(SelKind::Unit),
                help: "The board unit's name; needed only when the design has more than one board.",
                required: false,
                default: "",
            },
            FieldDef {
                name: "profile",
                kind: FieldKind::Text,
                help: "The name of a built-in factory or a profile in a project .export.splash document.",
                required: true,
                default: "",
            },
        ],
        inputs: &[],
        risk: Risk::Edit,
        keys: "",
    },
];

pub fn register_actions() {
    fab_elec_design::action::register(ACTIONS);
}
