//! The `fab.*` tools: one manifest for the in-app chat, external agents
//! (`makepad-remote mcp <pid>`) and Fab's own buttons, and the headless
//! [`Session`] that answers them.
//!
//! A session owns the model document (a project of object scripts, each a Splash
//! program), its evaluator and where it is saved. It runs on Fab's model worker. Tools
//! take Splash (a script, edits written as Splash statements: [`edits`], or project
//! statements) and answer with Splash values ([`out`]); the hub's request envelope only
//! carries them. Every script change is a structured splice of its source, never a
//! regenerated text.

use fab_model::eval::{free_spot, free_spot_off, no_room, Evaluated, Evaluator};
use fab_model::params::{self, PValue, PartParams};
use fab_model::program::{Compiled, Script, ScriptEnv};
use fab_model::project::write_atomic;
use fab_model::{project, script_file, ApplyError, Change, Edit, FabModel, Head, HistoryAction, PlaceArg};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use fab_plate::machine::{nozzle_word, Machine};
use fab_plate::{stl, threemf};
use fab_elec_design::action::{self, ActionDef, FieldKind, Risk as ActionRisk};
use makepad_ai_services::wire::{Risk, ServiceManifest, ToolDef};
use makepad_model::json::Value;
use out::{arr, flag, int, mm, mm2, mm3, n, name, obj, opt, s, S};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub mod annotate;
mod autosave;
pub mod camera;
pub mod checks;
pub mod compare;
pub mod edits;
pub mod img2mesh;
pub mod import;
pub use import::NewPart;
pub mod fit;
pub mod inspect;
pub mod interference;
pub mod layer_overhangs;
pub mod mass;
pub mod mech;
pub mod out;
pub mod picks;
pub mod hole;
mod hoist;
pub mod pull;
pub mod shell;
pub mod round;
pub mod overrides;
pub mod plates;
pub mod printable;
pub mod printable_tool;
pub mod raster;
pub mod supports;
pub mod test_view;
mod surface;
pub mod variants;
pub mod strengthen;
pub mod tubes;
pub mod strength_plan;
pub mod volumes;
pub mod weave;

pub const SERVICE: &str = "fab";

/// The modelling guide `fab.guide` returns: the core, then its sections, one per call.
pub const GUIDE: &str = include_str!("../GUIDE.md");

const SECTION_MARK: &str = "<!-- section: ";

/// The guide's core (no name), a section, or the vocabulary, which comes from the builders' table.
pub fn guide_section(name: Option<&str>) -> Result<String, String> {
    let Some(name) = name else {
        return Ok(GUIDE.split(SECTION_MARK).next().unwrap_or(GUIDE).trim().to_string());
    };
    // The vocabulary comes from the builders' table, a group at a time.
    if let Some(rest) = name.strip_prefix("vocabulary") {
        return inspect::vocabulary(rest.trim());
    }
    GUIDE
        .split(SECTION_MARK)
        .skip(1)
        .find_map(|part| part.split_once(" -->\n").filter(|(n, _)| *n == name).map(|(_, text)| text.trim().to_string()))
        .ok_or_else(|| format!("no guide section '{name}'"))
}

const BRIEF: &str = "Fab is a 3D modeler for printing. A project places object scripts (name.cad.splash) on the plate; a script is a Splash program that builds parts, one builder call each (m.box, m.extrude, m.subtract, m.blend, SDF fields written as Splash distance functions), each bound by a let in millimetres, Y up, and emits its objects with object(part). Every change rebuilds live in the view. Read fab.guide once, then: fab.inspect for the head, the project and the scripts, fab.script for a new object, fab.apply with edits of a script written as Splash statements, fab.place to move objects and set their print settings and parameter values, fab.render to look, fix, repeat. Fab lays the objects out on a 350 x 320 mm dual-nozzle bed and reports out-of-reach parts. A part that must fit a real product or standard part (a mount, case, adapter, spare): first ask the person which exact one with chat.ask when it comes in versions, then research its mating dimensions online, then build from those numbers with print clearance and pin them with m.expect (fab.guide section fit).";

fn schema(props: &str, required: &[&str]) -> String {
    let req: Vec<String> = required.iter().map(|r| format!("\"{r}\"")).collect();
    format!("{{\"type\":\"object\",\"properties\":{{{props}}},\"required\":[{}]}}", req.join(","))
}

const HEAD_PROP: &str = r#""expected":{"type":"string","description":"The head from fab.inspect or the last fab.apply: generation:hash."}"#;
const REQ_PROP: &str = r#""request_id":{"type":"string","description":"Unique per batch; a retry with the same id returns the first answer."}"#;

/// The service manifest.
/// What a support tool tries, committing nothing.
const TRY_PROPS: &str = r#""place":{"type":"string","description":"Try: project statements, as in fab.place. Nothing is committed."},"edits":{"type":["string","object"],"description":"Try: script edits, as in fab.apply, or an object of them by script file. Nothing is committed."},"script":{"type":"string","description":"The script `edits` are for."}"#;

pub fn manifest() -> ServiceManifest {
    let board = &fab_elec_mech::read::ACTIONS[0];
    let board_props = board.fields.iter().map(|field| format!(
        r#""{}":{{"type":"string","description":{}}}"#, field.name, Value::Str(field.help.into()).to_json(),
    )).collect::<Vec<_>>().join(",");
    let board_required: Vec<_> = board.fields.iter().filter(|field| field.required).map(|field| field.name).collect();
    const SCRIPT_PROP: &str = r#""script":{"type":"string","description":"The object script's file (name.cad.splash); default: the project's only script."}"#;
    let apply_props = format!(
        r#"{REQ_PROP},{HEAD_PROP},{SCRIPT_PROP},"edits":{{"type":"string","description":"Splash statements, each one edit of the script: set(site, value) (a site as fab.params keys it, instance/ optional: a param, part.field, a literal vector whole or one element part.field.i), field(part, name, value), unset(part, name), replace(part, code) (or a top-level let of a plain value: replace(width, param(30mm, min: 20mm, max: 40mm)) makes it a slider), operand(part, cutter), remove(part), rename(part, new_name), rollback(part, to: step) to end the part at that step and keep its name, after(part) followed by let part = code(part, ..) (the code becomes part, right after part's statement, so the part keeps its name, placement and settings; what part was is renamed by its kind, part_union, and the code and later part.face names read that: a fillet or chamfer on a body), otherwise after(name) puts added statements in order after that name's let or fn until the next before or after, unobject(part); any other statement (let lid = m.box({{...}}), object(lid)) is added to the script. All or nothing."}},"dry_run":{{"type":"boolean","description":"Build the edits and report what they would do without committing them."}}"#
    );
    let program_props = format!(r#"{REQ_PROP},{HEAD_PROP},{SCRIPT_PROP},"source":{{"type":"string","description":"The whole object script, Splash (use mod.model.*, use mod.fab.*, let m = mod.model first; object(part) for each object it puts on the plate). A new file is placed on the plate."}}"#);
    let fill_props = format!(r#"{REQ_PROP},{HEAD_PROP},{SCRIPT_PROP},"region":{{"type":"string","description":"Splash: a feature's name (holes), the object's name, or a region solid (m.box({{size: [20mm 20mm 20mm] at: [0 30mm 0]}})). Leave it out with seed for a smooth-region flood."}},"seed":{{"type":"array","items":{{"type":"number"}},"description":"A smooth-region flood instead of a region: a point on the part's surface [x y z] in mm, model frame (fab.measure / fab.inspect give surfaces). The fill spreads over the surface from it while neighbouring normals stay within angle, so it stops at creases and edges; it is recomputed on the current shape after edits."}},"angle":{{"type":"number","description":"The flood's maximum angle between neighbouring normals, degrees (default 30)."}},"albedo":{{"type":"array","items":{{"type":"number"}},"description":"The design colour [r g b], 0..1 linear."}},"color":{{"type":"string","description":"The project print colour it prints as (color(\"Blue\")); default: a colour of its own named after the albedo."}},"dry_run":{{"type":"boolean"}}"#);
    let place_props = format!(
        r#"{REQ_PROP},{HEAD_PROP},"edits":{{"type":"string","description":"Project statements: place(\"instance\", object: \"name\", link: 2, at: [x y], rotate:, scale:, drop:, name:, color: \"Blue\", paint: [[#rrggbb \"Yellow\"]], print:, show:, settings:) sets just the arguments it gives (object: default the instance's only one). Use at for the script origin, center for the footprint centre, or corner for its minimum bed X/Y, e.g. center: [200mm 160mm]. Each writes the origin to at. Use at: @free to return that object to the shelf pack. arrange(gap: 5mm) spaces free parts by their as-built slots through shape edits (before placement turns and scales), and arrange() restores the default. settings({{key: value}}) sets the plate's global print settings (nil: back to the profile); color(\"Blue\", rgb: #x2060d0, at: [[\"H2D\" @left \"A2\"]]) defines a print colour and the tray it prints from; a place's settings: is that object's whole set of overrides; params(\"instance\", {{name: value}}) sets the base and all its linked clones' parameter values (nil: the script's); script(\"file.cad.splash\", name: \"other\") places a script again; remove(\"instance~2\") removes a linked placement; measure(points: [[x y z] ...], kind: @distance, name: \"Gap A\") adds a ruler; rename(color: \"Colour 2\", to: \"Eyes\") renames a print colour and all that names it (placements, paint pairs, the scripts' Material{{name}}s), refused when the name is taken. All or nothing."}},"dry_run":{{"type":"boolean"}}"#
    );
    let manifest = ServiceManifest::new(SERVICE, "Fab", BRIEF)
        .with_tool(ToolDef::new("guide", "The CAD guide: units and frames, the three engines and when to use which, the API by example, error patterns, and a list of sections to ask for by name (project, params, edit, field, workflow, print, find, supports, force, joints, weave, printability, printer, and `vocabulary <group>` for every builder of a group). Read it before the first fab.apply.", schema(r#""section":{"type":"string","description":"A section's name; left out: the core."}"#, &[]), Risk::Read))
        .with_tool(ToolDef::new("import", "Import a mesh file as editable parts on this plate, or on a new plate.", schema(r#""path":{"type":"string"},"new_plate":{"type":"boolean","default":false}"#, &["path"]), Risk::Act))
        .with_tool(ToolDef::new(
            "inspect",
            "The current head (pass it as `expected`), the project (its text: scripts placed and how each object prints), each script (its Splash source, its solids' ids, the objects it emits and its instances), `volumes`: each print settings volume (name, shape, target, grow, box, values), the plate: each placement's print flag, size, bed position, volume, print colour, tool (filament number), nozzle and placement issues, and the palette: each print colour by name with the parts, paint and fill or stroke statements that use it.",
            schema(r#""program":{"type":"boolean","description":"Include the scripts' sources (default true)."},"read_volumes":{"type":"boolean","description":"Also `volumes_read`: what the current slice actually prints inside each print settings volume: its layers, area, walls and infill, grams and seconds, what it added against the slice before it, what's inside it by feature, and `changed_nothing` when it did nothing. With no slice of this plate made, it waits for one."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "settings",
            "The slicer settings that are set: the plate's global ones and each object's overrides. With a group (Quality, Walls, Infill, Top and bottom, Support and brim, Speed, Material) or a search word: every matching setting with its key, unit, range or choices, and whether only the plate can set it. Set them with fab.place: settings({key: value}) for the plate, place(\"instance\", settings: {key: value}) for an object.",
            schema(r#""find":{"type":"string","description":"A group's title or a word to search keys and labels for."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "colors",
            "Each placed part's colours: index, sRGB hex (as the live Model or Material colour mode shows it), the print colour it prints as and its tool. Colour 0 is the part's own print colour.",
            schema("", &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "params",
            "The parameter model: every tunable value of each part (declared param(...) first, then solids' literal fields), keyed instance/site, with its kind, value, range and whether it has a handle in the view. Unfiltered, per part: the declared parameters in full, then each solid's name and how many fields it has. 'solid' lists every key of one solid and the calls inside it (a blend's operands); 'prefix' the keys (or sites) that start so; both together narrow both ways. Unless solid or prefix is given, the palette too: each print colour by name with what uses it (default_name: a 'Colour N' to rename for its role). The user edits these in the part panel and with handles. A declared parameter's value is its instance's: set it with fab.place params(\"instance\", {name: value}); a part field is its script's: set it with fab.apply set(site, value), the key as listed.",
            schema(r#""part":{"type":"string","description":"Only this plate part (instance/solid; default: every part)."},"solid":{"type":"string","description":"The keys of this solid or engine node and the solids inside it: a name from fab.tree (head, or doll/head)."},"prefix":{"type":"string","description":"The keys starting so, with or without the instance (head.at, doll/nose.ellipsoid.radii)."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "check",
            "Check printability of the current plate, or of the test prints while the Slicer slices them. Leads with check: @pass or @fail (an error) and not_checked: what it left out and why (unsliced: the sliced checks). Returns severity counts and up to 40 problems with printer-mm locations and fix hints. With slice=true, use the app's current print setup for thin_wall, cantilever, bridge, mid_air, uncovered_overhang, support_blocked, support_unreached, support_inside, support_locked, support_unremovable, support_low_hold, support_rule, empty_layer and flow checks and estimates.",
            schema(r#""slice":{"type":"boolean","description":"Include sliced analysis using the current Print editor setup (default false)."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "board",
            board.tool_description(),
            schema(&board_props, &board_required),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "align",
            "Fit a board's mounting holes onto a printed part's bosses (round column tops and holes in faces looking the board's way): per hole its boss's axis (plate mm), the residual (mm), the move that puts the hole on the axis (board mm) and the gap under the board there; the mount that seats it in the part's model frame, and `edits` for fab.apply on `script` that write it (board_mount). A hole further than the tolerance from its boss is named under `misses` with the two fixes: move the hole, or move the boss. Reads only.",
            schema(r#""board":{"type":"string","description":"The board unit's name; omit with one board."},"part":{"type":"string","description":"The plate part to seat it on (instance/object or object); default: the part that holds the board."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "slice",
            "The slice's state: Sliced (the shown slice is current), Slicing with its fraction and stage, Failed with the error, Waiting (the Slice view is open and the slice is not there yet) or Off (nothing asks for a slice: the Slice view, Monitor, Print and Export each slice when they need one). `whole` is true once the slice has every layer, no live tiles left in it: a live slice shows its tiles as they come first. `on_screen` is true once the view's last drawn frame shows this state (it follows within a frame).",
            schema(r#""drawn":{"type":"boolean","description":"Also tell what frames drew of the last revisions: each one's frames, how many of its tiles were on screen before its whole slice, and how many tiles (default false). With revision it tells that revision's tiles instead, a page at a time from from; left says how many are left."},"revision":{"type":"integer","minimum":0,"description":"With drawn, tell this revision's tiles: part, layer, revision and whole ms after the edit."},"from":{"type":"integer","minimum":0,"description":"Start at this tile of the revision (default 0)."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "colour_mode",
            "Read the plate viewport’s colour mode and its explanation. Supply a mode to select it for the app.",
            schema(r#""mode":{"type":"string","enum":["model","material"]}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "force",
            "A part's force study as Design's Force view solves it (linear elastic, the part as it will print: walls, skins, infill, weak layers), the same numbers and words the view and the Force panel show. Its fixtures and loads as the script writes them, each with its region (area, centre, normal; model mm, model frame); all act together in one study. Then the safety factor against the printed material (margin included, its readings in `safety_factor_read`; none while the cell sizes disagree: `verdict` apart), the safety factor near the fixtures, thin features the cells cannot resolve, the largest von Mises and principal stress (MPa) and where, the largest movement (mm) and where, and the weakest point and its factor; `buckling`: the factor on all the loads at which the part bows out sideways (said only where the cell sizes agree, `factor` nil otherwise), found after the stresses on a job of its own (`state` running until then: call again), stale after an edit until the next study; `rows` are the panel's lines word for word, `status` its state line. Shows the Force view (it studies the selected part, else the one studied last, else the plate's first; `object` selects one) and answers once the study is up to date with the plate as it stands, so call it after an edit to read the new answer. Only in Design. Fixtures and loads are script statements: guide section force.",
            schema(r#""object":{"type":"string","description":"The plate part to study (its name as fab.inspect lists the placements); it is selected. Default: the part the Force view studies."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "printer",
            "Read the linked printer's status, stage, progress, time left, storage (USB drive present), AMS trays by nozzle side, loaded slots, temperatures, which tray each print colour prints from and Fab's current print job. Serial is limited to its last four characters. Printing itself is only started by the user in the Print editor (Print…, then Start print).",
            schema("", &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "voice",
            "The linked headset's microphone into this chat, the same as the chat's microphone button: listen=true asks the headset's microphone, listen=false stops it and the text of what the wearer said lands in the chat's composer for the person to read and send. Without listen: what the microphone is doing now. Offered only while a headset with a microphone is linked through the XR remote; the sound goes to the voice hub for text and is kept nowhere.",
            schema(r#""listen":{"type":"boolean","description":"true: start listening; false: stop and write the text. Left out: report the state."}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "workspace",
            "Fab's workspaces as the person sees them: the row of workspaces (n, name, which is current, the default each started as and the plate mode it pins), the plate's mode, and the current workspace's areas as a tree: each area by its id with the editors stacked in it (front first named in `front`) and its rectangle in window points, with tools: on|off when the front editor has a shelf (Plate or Schematic) and side: left|right for Schematic, each split by its id with its axis (across: side by side; down: one over the other) and share. While a temporary layout is open, temporary: its name (the note), whether it shows and who opened it (ai or person). With catalog=true, every editor by name with its group (the nub menu's columns) and whether it repeats. Area and split ids are what fab.workspace_edit takes; read this before editing.",
            schema(r#""catalog":{"type":"boolean","description":"Also list every editor with its group and whether one area after another can hold it (default false)."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "workspace_edit",
            "Change Fab's workspaces with the same changes the person's gestures make: Splash statements, one change each, in order, all or nothing (when one cannot apply, nothing changes and the answer names it and why). Workspaces by their n or name; areas and splits by the ids fab.workspace prints (in the current workspace; switch first to edit another); editors by name, a second instance of one in an area as \"Scope 2\"; sides left, right, up, down; a split's size as fab.workspace writes it (share: x between 0 and 1, or first: px / last: px for a part held at that many points). Statements: switch(ws); add(origin) (a default: design, mechanics, monitor, arm, scan, schematic, schematic_board, board_3d, device_scope, firmware, all_four, bench); rename(ws, \"name\"); duplicate(ws); reset(ws); delete(ws); reorder(ws, to); set(area, editor) (in front of the area); select(area, editor) (a tab of its stack to the front); region(area, tools: on|off, side: left|right) (the front editor instance's shelf across workspaces, saved across restarts and independent of layout undo: tools for Plate and Schematic, side for Schematic, at least one); show(editor) (on screen where it fits best, as a menu command shows it); split(area, side, editor, share: 0.5) (the size optional: half); close(area); close_tab(area, editor); swap(a, b); move(area, to: area, at: side|center); move_tab(area, editor, to: area, at: side|center); resize(split, share: 0.3); maximize(area); restore(); mode(design|mechanics|monitor) (the plate's); focus(area or editor, net: \"GND\" | part: \"R1\" | pin: \"U1.PB0\") (that editor to the front of its area, the thing selected and shown in it: the Schematic and the Board show nets, parts and pins, the 3D view parts, a Scope a net's lane; a quoted name alone is whichever of those it names; board: \"name\" where several boards have it); temporary(\"note\", from: ws) (a temporary layout: a copy of the current workspace, or of from, titled by the note and shown; area edits after it change the temporary copy; one at a time, a second one takes its place); dismiss() (back to where the person was); keep(\"name\") (into the row as a workspace of theirs). To show the person something, use a temporary layout unless they asked to arrange their own workspace: temporary(\"Net VCC\") set(3, Schematic) split(3, right, Board). The temporary layout is `temp` as a workspace. While the person holds a pointer or a key down, the change waits and applies when they let go (the answer then has waited). Use undo_layout() undoes the shown workspace's last layout change (one whole fab.workspace_edit call is one change), undo_layout(ws) another workspace's. Answers the workspaces as fab.workspace does, after the change.",
            schema(r#""edits":{"type":"string","description":"Splash statements, one change each, e.g. split(2, right, Schematic)\nselect(2, Parts)."}"#, &["edits"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "show",
            "Show the person electronics things: nets, parts and pins. Each thing is its name (\"VCC\", \"U1\", \"U1.PB0\": a part wins over a net of the same name, a net over a pin), or {net: \"VCC\"}, {part: \"U1\"}, {pin: \"U1.PB0\"}, with board: \"name\" where several boards have it. When the editors on screen can show them all, they are selected and framed there and nothing moves. Otherwise a temporary layout titled by the note (default: what it shows) opens with the Schematic and the Board, each pointed at them; the person keeps it or goes back (fab.workspace_edit keep() and dismiss() do the same). The same change as fab.workspace_edit's temporary, show and focus, in one call: held while the person's pointer or a key is down. Answers what it showed and where, then the workspaces as fab.workspace does.",
            schema(r#""things":{"type":"array","items":{},"description":"What to show: names, or {net|part|pin: name, board: name} objects."},"note":{"type":"string","description":"The temporary layout's title when one opens, e.g. \"Net VCC\"."}"#, &["things"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "print",
            "Ask the user to print the current slice on the linked printer: opens the confirm row in the Print editor and returns the print safety check's verdict. You cannot start a print: only the user's Start print does, and only after the safety check passed.",
            schema("", &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "apply",
            "Edit an object script with Splash statements (see the edits property). Its instances rebuild and the view updates; edits that do not build commit nothing and name the file, line, column and solid. Returns the new head, then `built` (the plate's part and joint counts; each object it changed with its size, and each body it added or changed with its engine, size and volume, model frame: hold it against what was asked first; `expects`: the person's numbers, m.expect), `expects_changed` when the batch changed one (say so), what changed per part and the plate summary. A batch that makes one of the person's m.expect numbers fail is refused at its line. dry_run builds without committing (and reports such a failure).",
            schema(&apply_props, &["request_id", "expected", "edits"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "fill",
            "Paint a feature, the object or a region in a material: appends m.fill(region, Material{albedo name}) to the script (paint changes no shape; every part type takes it). Fill features by their name in the script (holes, bores) so the paint follows parameter edits; a seed point (and angle) floods the smooth surface region round it instead, written m.fill(Flood{seed: [x y z] angle: 30deg}, ..). fab.render shows the result, fab.colors lists the colours.",
            schema(&fill_props, &["request_id", "expected", "albedo"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "script",
            "Write a whole object script (a new object: a new file is placed on the plate; or a new design of an existing script). Prefer fab.apply for changes to an existing one.",
            schema(&program_props, &["request_id", "expected", "source"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "place",
            "Edit the project with Splash statements (see the edits property): where each object prints and with what, the instances' parameter values, scripts placed again or taken away, print colours defined and renamed. Never touches a script, but for a colour's rename (the materials that name it).",
            schema(&place_props, &["request_id", "expected", "edits"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "duplicate",
            "Duplicate the instance containing a part or emitted object. mode linked shares its script and parameters; copy forks a new script file with the same parameters. All object placements and print overrides are copied. at is the first object's centre in plate mm; omitted: a free spot with a 10 mm gap.",
            schema(&format!(r#"{REQ_PROP},{HEAD_PROP},"object":{{"type":"string"}},"mode":{{"type":"string","enum":["linked","copy"]}},"at":{{"type":"array","items":{{"type":"number"}},"minItems":2,"maxItems":2}},"dry_run":{{"type":"boolean"}}"#), &["request_id", "expected", "object", "mode"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "remove",
            "Remove an instance (object accepts an instance, part or emitted object id). scope instance, the default, removes only that placement; removing the base promotes its lowest linked clone. scope script removes its ScriptRef and every linked placement, dropping the source only when no ScriptRef uses it.",
            schema(&format!(r#"{REQ_PROP},{HEAD_PROP},"object":{{"type":"string"}},"scope":{{"type":"string","enum":["instance","script"]}},"dry_run":{{"type":"boolean"}}"#), &["request_id", "expected", "object"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "exact",
            "An exact (m.brep) body's true volume, area, centroid and bounds, and its exact sharp edges and faces with ids, kinds, positions, radii and normals; each face's names as the script writes them (body.top, r.face(m.edge(a, b))) and each edge's selector (m.edge(a, b)) for fillet, chamfer and shell. An error for a solid that went through the mesh.",
            schema(r#""object":{"type":"string","description":"A solid id, instance/solid (fab.inspect lists them)."}"#, &["object"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "edges",
            "A solid's sharp edges (kind line/circle/curve, convex or concave, midpoint, direction, length, radius) in the model frame: pick m.fillet/m.chamfer edge selectors from it.",
            schema(r#""object":{"type":"string","description":"A solid id, instance/solid (fab.inspect lists them)."}"#, &["object"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "section",
            "Cut solids (default: every printed one) with a plane and look at the cut: area, pieces, holes, the narrowest width and where, plus a picture of the section. axis x|y|z (default y: a horizontal cut, like a print layer), at = mm along that axis. `supports`: true cuts the planned supports too, on the plate: plate mm, z up (default axis), each part's surface.",
            schema(r#""axis":{"type":"string","enum":["x","y","z"]},"at":{"type":"number"},"object":{"type":"string"},"supports":{"type":"boolean"}"#, &["at"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "measure",
            "Saved rulers: action add takes points ([x,y,z] mm in plate coordinates), optional kind (default distance) and name; set takes index and optional points/name (null clears name); remove takes index. Mutations require request_id and expected and are undoable; inspect lists rulers. Without action, measure in the model frame: what=size (bounds and volume of solids), clearance (the smallest gap between solids a and b, or overlap), distance (between points or solids a and b), wall (thickness at the surface point nearest 'at' on 'object', optionally along 'dir'), angle (between the faces at points a and b of 'object'), edge (the feature edge nearest 'at' on 'object': length, and radius and centre when it is circular), faces (the faces at points a and b of 'object': their distance when parallel, else the angle), thread_fit (a threaded rod on solid a and a threaded hole on solid b: radial clearance against what this printer needs, a check that passes when it fits, and a verdict), mass (each printed solid, or 'object': its grams as the job prints it from the filament's density, walls, shells and infill, its centre of mass, its inertia about that centre, and whether it stands on its base as modelled, Y up, with the tilt that tips it and which way; 'outline': true adds the base's outline; the person sees the centre of mass marked in the view as you read it). Use fit (a_at on a's seating face, b_at on b's mating face, optional turn in degrees about a's normal) to report b's fitted pose, overlap, side gaps and whether the faces meet. Points are [x, y, z] mm.",
            schema(&format!(r#"{REQ_PROP},{HEAD_PROP},"action":{{"type":"string","enum":["add","set","remove"]}},"index":{{"type":"integer","minimum":0}},"points":{{"type":"array","items":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}}}},"kind":{{"type":"string"}},"name":{{"type":["string","null"]}},"dry_run":{{"type":"boolean"}},"what":{{"type":"string","enum":["size","clearance","distance","wall","angle","edge","faces","fit","thread_fit","mass"]}},"outline":{{"type":"boolean"}},"object":{{"type":"string"}},"a":{{}},"b":{{}},"a_at":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"b_at":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"turn":{{"type":"number"}},"at":{{"type":"array","items":{{"type":"number"}}}},"dir":{{"type":"array","items":{{"type":"number"}}}}"#), &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "lay_flat",
            "Lay a whole part flat using the UI's Lay flat move, then drop it onto the bed, in one undo step. Give exactly one of normal (the face's outward plate normal) or point (on its surface in plate mm); fab.surface and fab.pick return plate and plate_normal. Usually used to put print strength along the layers. Returns the plate with rotate in degrees about X, then Y, then Z, as fab.inspect does. dry_run previews without committing.",
            schema(&format!(r#"{REQ_PROP},{HEAD_PROP},"object":{{"type":"string","description":"A printed part: its solid id (instance/solid) or its name from object(.., name:)."}},"normal":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"point":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"dry_run":{{"type":"boolean"}}"#), &["request_id", "expected", "object"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "test_print",
            "Test prints: a box cut out of the plate, to print only what is inside it (any part, with the plate's paint and settings) on its own supports. Action add takes `boxes`, a list of {feature: a name from fab.tree's features, or a whole part: the box fits round it and takes its name; or object: the part it lives on, at: centre [x,y,z] mm on the plate, size: a side or [x,y,z] mm (with a feature: instead of the fit), model: true when at, size and turn are in the part's model frame, as fab.fit answers them, turn: degrees about Z or [x,y,z] about X, Y, Z, name, on: ticked (default true)}: one undo step. Set takes name and any of at, size, turn, on, pose ([x,y,z] model degrees; null: as printed), brim (true: a brim of its own), to (a new name). Lay takes name and one of normal (plate direction on the piece as printed), face (bottom, top, front, back, left, right: a cut side of its box) or point (plate mm on the piece): that goes face down, on layers of its own. Remove takes name. Mode takes tests: true slices and prints only the ticked boxes, on the bed where they will print, false the whole plate. Read says what's slicing, each piece's figures: where it prints, pose, brim, layers, footprint, time, grams, flags (the checks' issues on it), the plate's settings that reach it, the total by the whole plate's, whether the slice is current, slice_seconds, slice_id (new for a fresh slice) and, once settled, gcode_key (same program, same key). A box moves, turns and scales with its part. Edits require request_id and expected.",
            schema(&format!(r#"{REQ_PROP},{HEAD_PROP},"action":{{"type":"string","enum":["add","set","remove","lay","mode","read"]}},"tests":{{"type":"boolean"}},"boxes":{{"type":"array","items":{{"type":"object"}}}},"name":{{"type":"string"}},"to":{{"type":"string"}},"at":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"size":{{}},"turn":{{}},"pose":{{}},"normal":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"face":{{"type":"string","enum":["bottom","top","front","back","left","right"]}},"point":{{"type":"array","items":{{"type":"number"}},"minItems":3,"maxItems":3}},"on":{{"type":"boolean"}},"brim":{{"type":"boolean"}},"dry_run":{{"type":"boolean"}}"#), &["action"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "check_mesh",
            "Check printed solids' meshes (default: all), leading with check: @pass or @fail (an error finding): closed and manifold, separate shells and specks (boolean slivers), sliver triangles, the thinnest wall, thin features under two line widths and weak necks (a cross-section far smaller than above and below), with model-frame locations and fixes.",
            schema(r#""object":{"type":"string"}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "joints",
            "Check every joint (m.hinge, m.ball, m.slider, m.bearing, m.chain) as printed in place: the clearance it was built with, the smallest gap between its parts and where, the in-layer gap, the area printed over the other part by empty layers (one layer or less fuses), the fused-joint risk, the limits where the parts touch, whether a ball is held, and one-line fixes. check: @pass when every joint is free to move, @fail when one is not, @not_checked (with why) when no object has joints.",
            schema("", &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            interference::TOOL,
            "Whether a jointed object's parts collide anywhere in their travel: every pair of its parts at rest, then with each joint at each end of its travel (its limits, else where fab.joints finds its parts touch; a free turn at quarter turns), the others at rest. Each overlap: the two parts, depth (mm), at (where they overlap deepest, model frame with the pose applied) and pose (\"rest\", \"lid at 90°\"). A joint's own two parts, a part and what it is bonded to are left out (skipped, with why): fab.joints checks a joint's own parts. Leads with check: @pass, @fail on any overlap, @not_checked (with why) when nothing moves. Run it after an edit to an object with joints.",
            schema(r#""object":{"type":"string","description":"The jointed object (its instance); default: every one."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "simulate",
            "Run the assembled piece in a physics sim (rigid parts, joints with their limits and pin friction, deterministic; a chain or chainmail runs as free links that hold by contact: scenario drop (onto a floor), drape (over a ball) or hang (by one end), answering holds_together, unlinked pairs, overlap, how far it fell, its size at the end and the cost per step). scenario hang (held by its m.anchor, released as printed: hangs_right), shake (held and shaken: articulates, binding joints), drop (on the bed), pose (angles: degrees per joint, a slider's mm; pose_drift says whether it holds). Answers articulates, hangs_right, binding, limit hits, self-contacts, settle time and each joint's range and final angle.",
            schema(r#""object":{"type":"string","description":"The jointed object (instance/object; default: the first)."},"scenario":{"type":"string","enum":["hang","shake","drop","pose","drape"]},"seconds":{"type":"number","description":"Simulated seconds (default 15 hang, 8 others; at most 30)."},"angles":{"type":"array","items":{"type":"number"},"description":"pose: degrees per joint (a slider's: mm), in fab.joints order."},"seed":{"type":"number"}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "weave",
            "Check every chain and chainmail (m.links, m.mail, m.drape) as printed in place from its links' exact geometry: link and interlock counts, whether it is one piece (no loose links), the smallest gap between links in a layer and across layers (one empty layer fuses) and where, the longest bridge and the leg angle, painted links, and one-line fixes. check: @pass when every link prints free and interlocked (@not_checked without a weave; not_checked lists the sliced beads unless as_printed). as_printed: true also slices the plate and checks the real beads (no bead reaches another link, nothing bridges two links).",
            schema(r#""as_printed":{"type":"boolean"}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "pick",
            "What a pixel of a render shows: give one of the cameras fab.render answers (its `cameras`, as given) and the pixel x, y in that view's picture (from its top left corner, in the picture's `px`). Answers the first surface the pixel's ray meets: the solid (the printed part) and the node there (a blend's operand), the point and outward normal in that solid's model frame and on the plate, and how far along the ray; or that the ray meets nothing. The point to name a feature you see (the nose's tip) in model mm.",
            schema(r#""camera":{"type":"object","description":"One of fab.render's cameras: {view sheet cell px eye forward right up fov}, or across for an orthographic one."},"x":{"type":"number","description":"Pixels from the picture's left edge."},"y":{"type":"number","description":"Pixels from its top edge."}"#, &["camera", "x", "y"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            picks::TOOL,
            "The person's picks in the 3D view: what they pointed at (Edit shape mode), each also a chip on their chat line; set them to show what you mean (the view lights up whole-part picks). Without arguments: the picks now, each with `write` (the binding to change, for a crease the statement joining the two edits, file:line and the value sites as fab.params keys them, for fab.apply), `what` (builder, size, the side pointed at, a crease's op, blend radius and angle, the wall there) and where (point and normal, model mm of `on`, or `object` for a whole-part pick). Kinds: edit (the binding whose surface wins there), crease (where two edits meet: change the joining statement's blend radius, or blend a hard union/subtract), edges (an edit's own edges: its rounding rounds them all), spot (a point: a new edit goes `at:` it), feature (on a mesh part the smallest solid round the point; on an exact part the step its face comes from), body (the whole printed object); on an exact (m.brep) part face, edge (where two faces meet) and chain (an edge and those continuing it tangent), each with `select`, what a script writes for it (`b.top`, `m.edge(b.top, b.front)`, for fillet edges: and shell open:). `set` replaces the picks, `add` adds: a list of picks, each a solid's name (\"knob\"), two names for a crease ([\"body\" \"knob\"]), {solid at kind} (at: a model point, snapped onto the surface; kind defaults to spot), {solid select} (an exact part's face or edge as fab.exact writes it), or {camera x y} at the top level as fab.pick takes a render's pixel. Inside a named or model-point pick, `on` names an exact step of the part (eased or instance/eased) whose faces and edges you can pick there, including a face a later step removes. `remove`: a pick's id. `clear`: true. A pick follows its names through edits; one whose solid is gone drops, and the next read says so.",
            schema(r#""set":{"description":"Picks replacing the ones there: a list of a name, [name name] (a crease) or {solid at kind} or {solid select}, optionally with on (a step name or instance/solid)."},"add":{"description":"Picks added, as set takes them."},"remove":{"description":"A pick's id (p3), or a list."},"clear":{"type":"boolean"},"camera":{"type":"object","description":"With x, y: pick what a pixel of a fab.render view shows."},"x":{"type":"number"},"y":{"type":"number"},"kind":{"type":"string","enum":["any","edit","crease","edges","spot","feature","body","face","edge","chain"],"description":"With camera: what to pick there (default any: on an exact part the edge near it, else the face; on a field part a crease near where two edits meet, an edit's edges near its own edge, else the edit)."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            round::TOOL,
            "Round (or chamfer) what the person picked, as their Round and Chamfer commands do, by one number. An exact (m.brep) part's picked edges and chains (one part, one step): the step they were picked on (the picks' `on`; the part's body when none is named) becomes its fillet, `let body = m.brep.fillet(body_union, r, {edges: ..})` right after what it was, now named by its kind (body_union), so the step keeps its name and the later steps build on the fillet (chamfer: m.brep.chamfer, the size). A crease on a part drawn from its field: the joining statement's blend radius (a hard union or subtract becomes m.blend or m.carve). An edit's own edges on a field part: a box's radius, which rounds all its edges. Without radius: what it would write and the value now (nothing changes). With radius (mm): committed, one undo step. Once a round is written, change its value with fab.apply set(<step>.radius, v) (<step>.size for a chamfer) rather than rounding again.",
            schema(r#""picks":{"description":"A pick's id (p3), or a list; default all the picks."},"radius":{"description":"The radius in mm (a number or \"2mm\"); a chamfer's size. Leave it out to see the plan."},"chamfer":{"type":"boolean","description":"Bevel the exact edges instead of rounding them."}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            pull::TOOL,
            "Pull (press-pull) the face the person picked along its normal, as their Pull command does, by `by` mm (out; in below 0). It writes the values that make the face, each moved as far: a box's size on that axis and its centre `at` by half, so the opposite face stays (`at` written where there was none); a cylinder's height and centre (top, bottom) or radius (wall, with a cone's top_radius); a sphere's radius; an extrude's height (twice the travel when pulled both ways). A number is set where it is, a declared param as that param; a value worked out from others is refused, naming it. A flat face no written value moves (an extrude's side, a chamfer's face, a face a boolean cut) gets a pull step of its own: the step it was picked on (the pick's `on`; the part's body when none is named) becomes `m.brep.pull(body, {face: .. by: ..})`, keeping its name, and pulled again that step's by moves; a result that is no closed solid is refused; a curved face no value moves (a fillet's) is refused. Without by: what it would write and its range (nothing changes). With by: committed, one undo step.",
            schema(r#""picks":{"description":"A pick's id (p3), or a list; the face picked last is pulled; default all the picks."},"by":{"description":"How far the face moves in mm (a number or \"2mm\"); in below 0. Leave it out to see the plan."}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            hole::TOOL,
            "Drill a hole where the person picked a flat face of an exact part (the spot they pointed at, else the face's own point), as their Hole command does: the step it was picked on (the pick's `on`; the part's body when none is named) becomes `m.brep.hole(body, {on: <face> at: [x y z] diameter: d})`, keeping its name; it starts on the face wherever the face goes, and runs through the part unless `depth` is given. Counterbores and countersinks: fab.apply on that statement (kind, bore_diameter, sink_diameter). Without diameter: what it would write (nothing changes). With diameter: committed, one undo step.",
            schema(r#""picks":{"description":"A pick's id (p3), or a list; the face picked last is drilled; default all the picks."},"diameter":{"description":"Across, in mm (a number or \"3.4mm\"). Leave it out to see the plan."},"depth":{"description":"How deep in mm; leave it out for a hole through."}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            shell::TOOL,
            "Hollow the exact part the person picked faces of, open at those faces, as their Shell command does: the step they were picked on (the picks' `on`; the part's body when none is named) becomes `m.brep.shell(body, thickness, {open: [faces]})`, keeping its name; a step that is a shell already has its wall set. Without thickness: what it would write (nothing changes). With thickness: committed, one undo step.",
            schema(r#""picks":{"description":"A pick's id (p3), or a list; their faces are left open; default all the picks."},"thickness":{"description":"The wall in mm (a number or \"2mm\"). Leave it out to see the plan."}"#, &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "annotations",
            "What the person marked on the 3D view with the marker (the Annotate button in the chat): the marks now and the ones sent with each line. Per mark: its number (drawn beside it) and kind (dot, open stroke, circle), what it covers by solid and node with each one's share of the mark's hits, its bounds and centre on the plate and in each solid's model frame (mm), its outline in the picture's pixels and the camera of that picture, so fab.pick on a pixel inside the outline lands in the mark's solid. A circle means everything visible inside it; an open stroke what is under the line; a dot that point. A mark is pointing, not an instruction: say what you took it to be before a change that cannot be undone cheaply. Frame a mark with fab.render focus {mark: N}. 'camera' reads the outlines against that camera (a render's); 'mark' picks one; 'line' gives one sent line's marks in full.",
            schema(r#""camera":{"type":"object","description":"A fab.render camera (or the chat picture's) to give the outlines in."},"mark":{"type":"integer","minimum":1},"line":{"type":"integer","minimum":1,"description":"The person's line a set of marks went with."},"compact":{"type":"boolean","description":"A short form: the camera once, up to 8 outline points and 3 covers per mark."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "surface",
            "A solid's surface near model points: per point in 'near' ([x y z] model mm, or a list of them), the nearest point of the solid's own surface, its outward unit normal, the signed distance to it (negative inside), the node that makes the surface there (a blend's operand) and the same point and normal on the plate. A field solid answers from its own distance field (an operand alone, without its blend's rounding; the blend for the whole), any other from its mesh. What sets a feature on a surface: an eye at the head's surface point, along its normal.",
            schema(r#""solid":{"type":"string","description":"A solid from fab.tree: head, or doll/head."},"near":{"type":"array","description":"A model point [x y z] in mm, or a list of them.","items":{}}"#, &["solid", "near"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new("tree", "The part tree: every solid's id (instance/binding), builder, line, what it is made of and the object it prints in; `features`: the named features of the printed objects with their kinds and boxes (model mm), to match what a person calls them; then the tunable parameters per part. `steps` lists each plate part's build history in the person's words.", schema("", &[]), Risk::Read))
        .with_tool(ToolDef::new(
            "history",
            "Undo or redo one batch.",
            schema(&format!(r#"{HEAD_PROP},"action":{{"type":"string","enum":["undo","redo"]}}"#), &["expected", "action"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "render",
            "Render the plate as image sheets for you to look at. Without arguments: eight views (four perspective volume views, front/side/near-top/underside). 'views' picks up to 8 by name (front, back, left, right, top, bottom, iso, iso_back, iso_left, iso_right, low, underside, or \"yaw,pitch\" degrees), 'object' shows one or more printed solids only. With one object, 'step' shows it as built at that step of its fab.tree history. 'focus' frames every view on a region: a box [[x y z] [x y z]] or a ball {at: [x y z] radius: r} in plate mm; a solid's name (\"doll/head\", or head) frames that solid wherever its object sits; {mark: 2} frames the person's mark; {object: \"doll/head\" at: [x y z] radius: r} or {object: .. box: [[..] [..]]} is in that solid's model frame. 'distance' (mm, 1 to 5000, camera to the middle of what it frames) and 'fov' (degrees, 5 to 120; default 42) set the camera; by default it fits what it frames. 'ortho': true draws every view in parallel projection at one scale, with a scale bar and a mm grid (the label gives a view's width in mm). 'colours' selects model or material colours for this render. Omit it to use the live viewport mode. The picture budget goes to the views asked: one view comes back about 1000 px square, two or four 500, eight 370; each sheet's label says its views' size. The answer's 'cameras' maps a view's pixel to a ray. Field parts are traced from their fields, so a close-up shows the true surface. 'supports': true draws the planned supports. 'test_print': a piece. Example, a head close-up: {object: \"doll/figure\" views: [\"35,-15\"] focus: {at: [150 160 95] radius: 25} fov: 30}.",
            schema(r#""views":{"type":"array","items":{"type":"string"}},"object":{"description":"A printed part: its solid id (instance/solid) or its name from object(.., name:). Accepts a list of parts."},"step":{"type":"string","description":"A step id from fab.tree for the one object to render."},"test_print":{"description":"A test print's name, or a list: its piece alone, cut and closed, where it is on the model. While the Slicer slices test prints, a render with none of object, volume, spot and tubes shows those pieces on the bed, 'supports' too."},"on_bed":{"type":"boolean","description":"With test_print: where the pieces print."},"with_plate":{"type":"boolean","description":"With test_print: the whole plate, the piece's surface tinted."},"volume":{"description":"A print settings volume's name, or a list: the part's surface inside it comes out tinted."},"spot":{"description":"A row of the last fab.weak_spots, or a list: the part's surface there comes out tinted."},"tubes":{"type":"boolean","description":"The last fab.tubes list drawn on the part: each tube green lying flat to red standing, its axis past its ends, a ring in the layers' plane across it, and its name and angle written on the picture."},"focus":{"description":"What every view frames: [[x y z] [x y z]] or {at: [x y z], radius: mm} in plate mm; a solid's name (\"doll/head\"); or {object: solid, at: [x y z], radius: mm} / {object: solid, box: [[..] [..]]} in that solid's model frame."},"distance":{"type":"number","description":"mm from the camera to the middle of what it frames (1 to 5000); default: what fits."},"fov":{"type":"number","description":"Field of view, degrees (5 to 120); default 42."},"ortho":{"type":"boolean","description":"Parallel projection at one scale for every view, with a scale bar and a mm grid; default false."},"colours":{"type":"string","enum":["model","material"],"description":"Use model or mapped spool colours for this render. Defaults to the live viewport mode."},"supports":{"type":"boolean"},"marks":{"type":"boolean","description":"Draw the person's marks in the views (default true while there are marks); focus {mark: N} frames mark N."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            strength_plan::TOOL,
            "\"Optimize this part's printed strength\", in one read that changes nothing: its tubes with how they lie, its weak spots with their fixes, the best way up with what it costs, the limb turns that would help, and what stays weak after all of it. Waits for a current slice: with none it says so, and fab.slice waits for one. Say it in plain words with a picture, ask before turning the part or a limb, then fab.strengthen does the picks as one undo step.",
            schema(r#""object":{"type":"string"},"min_thickness":{"type":"number"},"max_angle":{"type":"number","description":"The most a limb turn would be, degrees; 15 unless said."},"goal":{"type":"string","enum":["strength","supports"]},"whole":{"type":"boolean","description":"true: every row of every list, however long; a reply past 16 KB is cut at its end. Left out: the lists lose their least-at-risk rows to fit, and say so."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            tubes::TUBES_TOOL,
            "A print breaks along its layers: a tube standing up is at its weakest, lying flat at its strongest. The part's tube-like limbs, named or not, read from its layers, worst to break first: name (a feature, or 'limb near [x y z]'), axis, length, thinnest_radius, angle_to_bed (0 flat, 90 standing), carries_mm3, what lying flat would gain by the filament's model (an estimate), and a capsule target fab.settings_volume takes. After fab.orient and its slice each row says angle_before too. fab.render {tubes: true} draws them.",
            schema(r#""object":{"type":"string","description":"A printed part: its solid id (instance/solid) or its name from object(.., name:)."},"max_thickness":{"type":"number","description":"How thick a limb can be and still be looked for, mm. Left out: 15% of the part's longest side, and on a part with no features 6% too."}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            tubes::ORIENT_TOOL,
            "Which way up: a table, moving nothing unless 'apply'. goal 'strength' (default): lays the tubes as flat as they go together, weighted by break risk or 'weights'; 'tubes' keeps it to those. goal 'supports': the ways up with least support, from the mesh then through the real support planner; 'hold': [{tube, within}] keeps a tube within that many degrees of flat. Its first row is the part as it stands; each row has every tube's angle before and after with the estimated gain, height, size, fits_bed, overhang and support estimates and, for 'supports', support_planned. 'says' puts what lies flat and what stays in plain words. Ask before applying unless told directly. apply: true with a row's 'up' and 'spin' and 'expected' moves the part, one undo step; fab.slice then fab.tubes measures it.",
            schema(&format!(r#"{HEAD_PROP},"object":{{"type":"string"}},"goal":{{"type":"string","enum":["strength","supports"]}},"tubes":{{"type":"array","items":{{"type":"string"}}}},"weights":{{"type":"object"}},"hold":{{"type":"array","items":{{"type":"object","properties":{{"tube":{{"type":"string"}},"within":{{"type":"number"}}}}}}}},"apply":{{"type":"boolean"}},"up":{{"type":"array","items":{{"type":"number"}}}},"spin":{{"type":"number"}}"#), &[]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            strengthen::FIND_TOOL,
            "What would break first on this: the plate's thin places, worst first, measured from the current slice (it never re-traces; with no current slice it waits for one). Each row: its id, where (a feature's name, or \"limb near [x y z]\" in model mm), kind (neck, limb or thin), at (plate mm), thickness in mm, angle_to_bed, bond_mm2 (the smallest layer bond in it) and bond_z, carries_mm3 (what's beyond it), there (its walls, infill and pattern, and what prints in it) and fixes: each fix a real one with an estimated gain and cost. 'min_thickness' is how thin counts as thin, in mm (default: 6% of the part's longest side, 3 to 15 mm). After a fab.strengthen and a new slice, 'fixed' says what each fix did, measured.",
            schema(r#""min_thickness":{"type":"number"}"#, &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            strengthen::TOOL,
            "Fix weak spots from the last fab.weak_spots list and turn the part or a limb from fab.orient's table, all of them as one undo step; the plate reslices, then fab.weak_spots says what they did. {tube: \"neck\", fix: \"align\", max_angle: 15} turns one limb flat about its joint (changes the design's pose: ask first unless told); {fix: \"orient\", up: [..], spin: 0} turns the part as a fab.orient row says. 'fixes': [{spot: \"spot1\", fix: \"solid_infill\"}, {spot: \"spot2\", fix: \"thicken\", to: 8}]: solid_infill places a print settings volume on it (a program edit; the volume is a fab.settings_volume one, read with fab.inspect {read_volumes: true}), thicken takes the part to 'to' mm there (default the list's min_thickness), a capsule round the place blended into the body as a named link of a chain, on parts whose shape is a field. A fix the list doesn't offer for a spot is refused, saying why. 'dry_run': builds it, commits nothing.",
            schema(&format!(r#"{HEAD_PROP},"fixes":{{"type":"array","items":{{"type":"object","properties":{{"spot":{{"type":"string"}},"tube":{{"type":"string"}},"fix":{{"type":"string","enum":["solid_infill","thicken","align","orient"]}},"to":{{"type":"number"}},"up":{{"type":"array","items":{{"type":"number"}}}},"spin":{{"type":"number"}},"object":{{"type":"string"}},"max_angle":{{"type":"number"}}}},"required":["fix"]}}}},"dry_run":{{"type":"boolean"}},"request_id":{{"type":"string"}}"#), &["expected", "fixes"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            variants::TOOL,
            "Two to six versions of the model side by side from one camera, to choose between: each a batch of fab.apply edits (set(w, 30mm), field(...), ...) built as its dry_run builds it, nothing committed. Every variant is drawn from the same views ('views': up to 2, default front; variants across, views down) and camera ('focus', 'distance', 'fov', 'ortho' as fab.render takes them; it frames all the variants together, so a bigger one looks bigger). The answer has each variant's diff against the plate as it is and its ready fab.apply call: apply the one you pick. A variant whose edits do not build is told with why. 'compare': {picture, fit, ortho, fov, background, rect, islands, shadows} reads each variant against a reference picture as fab.compare does, from the first view (or the view 'fit': true searches for once, on the plate as it is), and ranks them best match first, each with its IoU and the bands that differ most. Example: {variants: [{name: \"narrow\" edits: \"set(head_r, 9mm)\"} {name: \"wide\" edits: \"set(head_r, 12mm)\"}] views: [\"front\" \"right\"] compare: {picture: \"front.png\"}}. Guide section: compare.",
            schema(r#""variants":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"edits":{"type":"string","description":"fab.apply's Splash statements: this variant's edits of one script."},"script":{"type":"string","description":"The script the edits are of; default: the one their set() sites name, else the project's only one."}},"required":["name","edits"]},"minItems":2,"maxItems":6},"views":{"type":"array","items":{"type":"string"},"maxItems":2,"description":"fab.render view names or \"yaw,pitch\" degrees; default front."},"object":{"description":"A printed part: its solid id (instance/solid) or its name from object(.., name:). Accepts a list of parts. Default: the whole plate."},"focus":{"description":"As fab.render's; default: all the variants together."},"distance":{"type":"number"},"fov":{"type":"number"},"ortho":{"type":"boolean"},"compare":{"type":"object","description":"A reference picture each variant is read against, as fab.compare takes it: picture, fit, ortho, fov, background, rect, islands, shadows, holes.","properties":{"picture":{"type":"string"},"fit":{"type":"boolean"},"ortho":{"type":"boolean"},"fov":{"type":"number"},"background":{"type":"number"},"rect":{"type":"array","items":{"type":"number"}},"islands":{"type":"boolean"},"shadows":{"type":"boolean"},"holes":{"type":"boolean"}},"required":["picture"]}"#, &["variants"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            compare::TOOL,
            "Compare the model with a reference picture (a drawing or photo of what you are modelling) seen as one of fab.render's views, in numbers. The picture's figure is the largest piece a flood fill from its border over the background colour and its soft shadow leaves, holes open (the notes say what was left out; check the figure picture). The model is scaled and moved onto the figure by height, vertical middle and centre of area across, then: silhouette IoU, 16 height bands (widths in the reference and the model and the model's middle less the reference's, as shares of the figure's height and in mm, most different first) and a sheet: the reference, the model shaded, and the overlay. 'rect' [x0 y0 x1 y1] in picture fractions cuts the picture first (a screenshot's card, or the head alone); 'solid' (a name or list) cuts the model to what the view shows of those solids, to match. 'align' lays the model on a band of the figure ({band: [0 0.3] solid: \"head\"}) or on points ({points: {crown: {picture: [u v] plate: [x y z]} chin: ..}}) instead of its whole height. 'points' {name: [u v]} answers each picture spot in plate mm. 'fit': true searches yaw and pitch for the picture's view (then compare at it). 'background' (0 to 441, default 40) is how far a colour may be from the background's; 'islands': true keeps every piece, 'shadows': false the shadow. 'ortho': true for a drawing; 'fov' (degrees) matches a photo's lens. Example: {picture: \"front.png\" view: \"front\" points: {eye: [0.41 0.22]}}. Guide section: compare.",
            schema(r#""picture":{"type":"string","description":"The name of a picture the person sent in this chat (chat.pictures lists them), or a picture file's path (PNG, JPEG, WebP, GIF or BMP)."},"view":{"type":"string","description":"A fab.render view name or \"yaw,pitch\" in degrees: the view the picture is seen from."},"fit":{"type":"boolean","description":"Search for the view instead."},"object":{"description":"A printed part: its solid id (instance/solid) or its name from object(.., name:). Accepts a list of parts. Default: the whole plate."},"rect":{"type":"array","items":{"type":"number"},"description":"[x0 y0 x1 y1], fractions of the picture from its top left."},"points":{"type":"object","description":"{name: [u v]}, fractions of the picture from its top left."},"background":{"type":"number"},"islands":{"type":"boolean","description":"Keep every piece of the figure, not only its largest."},"solid":{"description":"A solid's name (fab.tree lists them) or a list: the model's silhouette is what the view shows of them."},"align":{"type":"object","description":"{band: [from to] solid: name(s)}: the reference figure's rows from..to of its height (from its top) against what the view shows of the solids; or {points: {name: {picture: [u v] plate: [x y z]} ...}}: two or more picture points (fractions) onto plate points (mm)."},"shadows":{"type":"boolean","description":"false: the background's soft shadow is figure (default true: it is background)."},"holes":{"type":"boolean","description":"false: background the figure encloses is figure (default true: it is seen through, background)."},"ortho":{"type":"boolean"},"fov":{"type":"number"}"#, &["picture"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "overhangs",
            "The plate's overhang patches, read from the layers its slice cuts, plate mm, ranked by area the planned supports leave: id, area as the bed sees it, where, lowest point, what's under it and how far (clear), covered (share of its area with a planned tip, pad or footprint in reach). A patch the plan leaves comes with `enforce`, a ready-made m.support_enforce statement for its `script`; `blocked` and `given_up` say why one is left. Near patches share a row when there are more than `limit`: ask again with its box. Guide section: supports.",
            schema(&format!(r#""object":{{"type":"string"}},"box":{{"type":"array","description":"[[x y z] [x y z]], plate mm: only patches centred in it."}},"limit":{{"type":"integer","description":"Rows (default 12)."}},"angle":{{"type":"number","description":"Count faces flatter than this from the bed (degrees); default the plan's support threshold. Faces between it and the threshold get no support unless enforced."}},{TRY_PROPS}"#), &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            supports::VIEW_TOOL,
            "One small picture aimed at a patch or a box, from below, framed on it, with what stands below it cut away: the parts' layers, overhang faces coloured, planned supports drawn, patch ids and a mm bar written on it.",
            schema(&format!(r#""patch":{{"type":"string","description":"An id from fab.overhangs."}},"box":{{"type":"array","description":"[[x y z] [x y z]], plate mm."}},"views":{{"type":"array","items":{{"type":"string"}},"description":"Up to 4: below (default), a fab.render name, or \"yaw,pitch\" degrees."}},"supports":{{"type":"boolean","default":true}},"angle":{{"type":"number","description":"As in fab.overhangs: patch ids match the call that used the same angle."}},{TRY_PROPS}"#), &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            "supports",
            "The plate's support plan, without slicing: per part its type, pieces, branches, volume, material, extruding time, height and the overhang area it leaves. `verdicts`: true adds whether each piece can leave the part once broken free and whether a tool reaches it; that search takes a while and the person sees its progress. A piece that won't comes with `block`: a ready-made m.support_block statement for its `script`, in the object's own frame. Try them with `edits`: the answer says what the try changed (`was`, `gone`, `new`), committing nothing. Says nothing of how hard a piece is to break off. Guide section: supports.",
            schema(&format!(r#""verdicts":{{"type":"boolean"}},"limit":{{"type":"integer","description":"Piece rows, worst first (default 12)."}},{TRY_PROPS}"#), &[]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            img2mesh::TOOL,
            "Turn a picture into a part on the plate. The AI hub's image-to-3D model (trellis-2, on a fleet GPU node that has it installed) makes a mesh of the object in the picture; Fab welds it, sizes it so its largest dimension is size_mm, stands it on the bed at a free spot as a new object script that imports the mesh (m.import, with a Size parameter), and answers with the mesh check (watertight, open edges, pieces), fab.check's findings and a render of the new part: look at it before you go on. One whole object on a plain background works best; a cut-out PNG skips the hub's own background removal. Geometry only, no colours. It takes a minute or more; the person sees its progress and can cancel it.",
            schema(
                r#""image":{"type":"string","description":"A picture file's path (PNG, JPEG, WebP, GIF or BMP), or the name of a picture the person sent in this chat (chat.pictures lists them)."},"size_mm":{"type":"number","description":"The part's largest dimension in mm (default 60, 5 to 300)."},"detail":{"type":"integer","description":"Surface detail: the remesh grid along the longest side, 64 to 512 (default 256; 512 is finer and slower)."},"name":{"type":"string","description":"The new object's name (default: from the picture's)."},"seed":{"type":"integer","description":"The generation's seed: the same picture and seed give the same shape."},"printable":{"type":"boolean","default":true,"description":"Make the mesh printable before import: repair holes, thicken features and orient its base. Failure keeps the fitted mesh and returns printable.error."}"#,
                &["image"],
            ),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "picture",
            "Print a photo in full colour: a flat tile (m.picture) the width given, face down, dithered from the project's print colours (each a filament the printer loads). The picture is copied into the project beside a new object script, which lands at a free spot on the bed. Guide section: image.",
            schema(r#""image":{"type":"string","description":"A picture file's path (PNG, JPEG, WebP, GIF or BMP), or the name of a picture the person sent in this chat (chat.pictures lists them)."},"width_mm":{"type":"number","description":"The tile's width in mm (default 100, 10 to 300); its depth follows the picture's."},"name":{"type":"string","description":"What the tile and its picture are called (default: from the picture's file)."}"#, &["image"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "colour_read",
            "Read a photo of a printed colour swatch (m.colour_swatch) back as a calibrated filament set: what each stack of its filaments really shows. The photo is of the swatch's face, face up on plain white paper in daylight, the whole swatch and some paper round it in the picture. The set is kept under `name` in the Fab documents' calibration folder; a picture prints with it by `set: \"name\"`, and by default when its filaments are the ones loaded at the same layer height. Answers how well the photo registered and how far the reading moved each stack from the first guess. Guide section: image.",
            schema(r#""image":{"type":"string","description":"The swatch photo: a picture file's path (PNG, JPEG, WebP, GIF or BMP), or the name of a picture the person sent in this chat (chat.pictures lists them)."},"name":{"type":"string","description":"The set's name, e.g. \"Matte CMYW\"; a set of that name is replaced."},"object":{"type":"string","description":"The swatch's script instance, when the plate has more than one swatch."}"#, &["image", "name"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            volumes::FIT_TOOL,
            "The shape of a part of the model you found by looking, from points picked on it (fab.pick's `point`): its axis, ends and radius, measured from the current slice's layers (or from sections of the model when no slice is current; `from` says which), so no radius is guessed from a picture. It follows the part from the points until it dies out (a tip) or its thickness jumps (where a limb meets the body), and says which at each end. More than one point: it runs through them all, so an arm and its hand are one fit. Places nothing: give the answer's `target` to fab.settings_volume with a name. With `volume`: also what share of this part that volume holds, and how much of what's inside it is something else.",
            schema(r#""points":{"type":"array","description":"Points on the part, [x y z] in model mm, from fab.pick."},"object":{"type":"string","description":"The plate part (fab.pick's `solid`, or instance/object); left out, the plate's only part."},"shape":{"type":"string","enum":["capsule","cylinder","box","sphere"],"description":"Default capsule."},"volume":{"type":"string","description":"A print settings volume's name to read against this part."}"#, &["points"]),
            Risk::Read,
        ))
        .with_tool(ToolDef::new(
            volumes::TOOL,
            "Print settings in a volume of a part: solid infill or more walls in the arms, the legs and the neck, say. One call covers several (`targets`), as one undo step. Each target is a feature by name (fab.tree's `features` lists names, kinds and boxes), by default followed in its own shape and grown past its blend, or a box, sphere, cylinder or capsule fitted round it. A model with no names: find the part by looking (fab.render, fab.pick, fab.fit) and give it by numbers in the frame of fab.pick's `point`: {name, shape: \"capsule\", a, b, radius}, the same for a cylinder, {name, shape: \"sphere\", at, radius}, {name, shape: \"box\", at, size, up}. A word that matches no feature changes nothing: the answer lists the features there are. The answer's `done` says what it did by name and value.",
            schema(&format!(r#"{HEAD_PROP},"request_id":{{"type":"string"}},"object":{{"type":"string","description":"The plate part (instance/object) the names are on; or alone, a volume over the whole part."}},"targets":{{"type":"array","description":"Names; {{feature, shape, grow}}; {{name, object, shape, a, b, radius}} (capsule, cylinder); {{name, object, shape, at, radius}} (sphere); {{name, object, shape, at, size, up}} (box); or {{name, object, min, max, shape}} to fit a shape round a box. Numbers are model mm."}},"feature":{{"type":"string","description":"One target."}},"shape":{{"type":"string","enum":["follows","box","sphere","cylinder","capsule"],"description":"Default follows on a feature, box otherwise."}},"grow":{{"type":"number","description":"mm a followed feature grows, default 1."}},"settings":{{"type":"object","description":"sparse_infill_density (\"100%\" is solid infill), sparse_infill_pattern, wall_loops. Default a solid infill."}},"dry_run":{{"type":"boolean"}}"#), &["expected"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new(
            "make_printable",
            "Make an imported mesh part printable at its current Size and placement scale. Saves a new asset, replaces the script's import and applies figure print settings in one undo step. Returns before/after mesh statistics, repairs, orientation, overhangs, notes and a render. The original asset is kept for undo.",
            schema(r#""object":{"type":"string","description":"The imported mesh part, instance/object."},"nozzle_mm":{"type":"number"},"min_feature_mm":{"type":"number"},"cell_mm":{"type":"number"},"max_triangles":{"type":"integer"},"orient":{"type":"boolean"},"height_mm":{"type":"number","description":"Optional repaired height in mm; omitted preserves scale."},"flat_base":{"type":"boolean"}"#, &["object"]),
            Risk::Act,
        ))
        .with_tool(ToolDef::new("new", "Start a new, empty plate (the open one keeps its edits and stays on the plate list).", schema(r#""name":{"type":"string"}"#, &[]), Risk::Request))
        .with_tool(ToolDef::new(
            "plates",
            "The plate list: every plate (id, name, file, last edited, modified: its edits are not in its file yet) and the demos (key, title). `open` puts a plate on the bed as it was left (the plate on the bed keeps its edits), or a demo as it ships (`demo`: its key; its first edit makes a plate), `duplicate` copies one, `rename` names one, `remove` takes one off the list (its files stay). Guide section: plates.",
            schema(r#""action":{"type":"string","enum":["list","open","duplicate","rename","remove"]},"id":{"type":"string","description":"A plate id from the list."},"demo":{"type":"string","description":"open: a demo's key from the list, instead of an id."},"name":{"type":"string","description":"rename: the new name."}"#, &[]),
            Risk::Request,
        ))
        .with_tool(ToolDef::new(
            "save",
            "Save the plate as name.fab.splash with its object scripts (name.cad.splash) beside it (default: where it was opened or last saved, else a folder of its own in the Fab documents folder). Edits are kept without it; saving writes them into the plate's own file.",
            schema(r#""path":{"type":"string","description":"A .fab.splash file path."}"#, &[]),
            Risk::Request,
        ))
        .with_tool(ToolDef::new(
            "export",
            "Export the plate for printing: STL (one mesh) or 3MF (one object per part, with its filament and colour). Default path: next to the project, else the Fab documents folder.",
            schema(r#""format":{"type":"string","enum":["stl","3mf"]},"path":{"type":"string"}"#, &["format"]),
            Risk::Request,
        ));
    with_elec_tools(manifest)
}

/// The electronics tools, one per kind of effect, and the catalog risks each one offers.
/// `elec_read` reads the design the editors show and the live bench; `elec_run` drives the bench
/// as its transport does (never the design, no undo step); `elec_edit` edits the design as a
/// person does in an editor, one undo step a call; `elec_export` writes files outside the
/// project once the person approves the call. `elec_external` writes entries to the person's
/// library with per-entry approval. The board read stays `fab.board`
/// (`fab_elec_mech::read`).
pub const ELEC_TOOLS: [(&str, &[ActionRisk], Risk); 5] = [
    ("elec_read", &[ActionRisk::Read], Risk::Read),
    ("elec_run", &[ActionRisk::Run], Risk::Act),
    ("elec_edit", &[ActionRisk::Edit, ActionRisk::Preview], Risk::Act),
    ("elec_export", &[ActionRisk::External], Risk::Request),
    ("elec_external", &[ActionRisk::Library], Risk::Request),
];

/// The electronics tool that changes a document's text as typing in its code pane does: one
/// call is one undo step. It is offered with the document reads (`elec.doc.read`) it builds on.
pub const ELEC_TEXT_TOOL: &str = "elec_text";

/// The tool that offers an action of this risk; None: no tool offers it yet. O(1).
pub fn elec_tool_of(risk: ActionRisk) -> Option<&'static str> {
    ELEC_TOOLS.iter().find(|(_, risks, _)| risks.contains(&risk)).map(|(tool, _, _)| *tool)
}

/// The catalog actions `tool` offers, by name; the board read is `fab.board`'s. O(catalog).
pub fn elec_actions(tool: &str) -> Vec<&'static ActionDef> {
    let board = fab_elec_mech::read::ACTIONS[0].name;
    action::catalog().iter().filter(|def| def.name != board && elec_tool_of(def.risk) == Some(tool)).collect()
}

/// A field's kind in the words the tools speak (Splash values).
fn field_kind(kind: &FieldKind) -> String {
    match kind {
        FieldKind::Bool => "true|false".into(),
        FieldKind::Int { .. } => "int".into(),
        FieldKind::Num { unit: "" } => "num".into(),
        FieldKind::Num { unit } => format!("num {unit}"),
        FieldKind::Text => "text".into(),
        FieldKind::Choice(choices) => choices.join("|"),
        FieldKind::Sel(of) => format!("{of:?}").to_lowercase(),
        FieldKind::Sels(of) => format!("[{}]", format!("{of:?}").to_lowercase()),
        FieldKind::Point => "point".into(),
        FieldKind::Points => "points".into(),
        FieldKind::Layer => "layer".into(),
        FieldKind::Layers => "layers".into(),
        FieldKind::Splash => "splash".into(),
    }
}

/// One action's line in its tool's description: `name(field: kind, opt?: kind = default)  help`.
fn elec_action_line(def: &ActionDef) -> String {
    let fields: Vec<String> = def.fields.iter().map(|field| {
        let default = if field.default.is_empty() { String::new() } else { format!(" = {}", field.default) };
        format!("{}{}: {}{default}", field.name, if field.required { "" } else { "?" }, field_kind(&field.kind))
    }).collect();
    format!("{}({})  {}", def.name, fields.join(", "), def.help)
}

/// An action's whole entry, for `action: "help"`: what it does, its fields with their help and
/// the shape of its reply.
pub fn elec_action_help(def: &ActionDef) -> String {
    let mut text = def.tool_description();
    if !def.fields.is_empty() {
        text.push_str("\n\nFields:");
        for field in def.fields {
            let default = if field.default.is_empty() { String::new() } else { format!(" (default {})", field.default) };
            text.push_str(&format!("\n{}: {}{}{default}  {}", field.name, field_kind(&field.kind), if field.required { ", required" } else { "" }, field.help));
        }
    }
    text
}

/// The manifest with the electronics tools generated from the action catalog: a lane's
/// registered `ActionDef` shows up in the tool its risk maps to, with no edit here. A tool with
/// no registered action (outside the app, which registers them) is left out. O(catalog).
fn with_elec_tools(mut manifest: ServiceManifest) -> ServiceManifest {
    const INTRO: [(&str, &str); 5] = [
        ("elec_read", "Read Fab Electronics: the design its editors show and the live bench. Read elec.design.read before an edit: the edit actions name parts by reference (R1) or path (mcu.boot_r). elec.check.list says what ERC and DRC find. elec.net.length gives routed track lengths and pair or group differences against their tolerances. After an edit, elec.sim.status with wait: <its seq> answers once the run has it, so a measurement after it reads the new circuit."),
        ("elec_external", "Propose a dated offline price library entry with its source URL. The person reviews this individual entry in the BOM tab before it is written."),
        ("elec_export", "Write Fab Electronics' fabrication files (Gerber, drill, BOM, placement, the zip) and check each one read back against the board. The person approves the call first, and can change its arguments."),
        ("elec_run", "Drive Fab Electronics' bench as its transport does: play, pause and rate. It never changes the design and makes no undo step."),
        ("elec_edit", "Edit Fab Electronics as a person does in its editors: each call is one undo step, the edit shows where it lands, and a playing take re-renders with it. The answer's take says what the edit does to the run: state playing, held or none; effect retune, restart or none. An edit of the board, the circuit or the system answers once every check has run on it, and its checks say what it changed for them: the new findings, errors first, and how many are gone. When the project has requirements, an edit answers once they are judged on it too, with requirements: met as elec.req.list gives it, changed (each row whose status the edit moved: name, was, now, value, bound, why) and waiting (rows whose value waits for its producer to run again, with why). An edit that changes the person's requirements also answers requirements_changed: say each line to the person."),
    ];
    for (tool, _, risk) in ELEC_TOOLS {
        let defs = elec_actions(tool);
        if defs.is_empty() {
            continue;
        }
        let intro = INTRO.iter().find(|(name, _)| *name == tool).map_or("", |(_, intro)| intro);
        let mut description = format!("{intro} Actions (args by field name; `?` is optional):");
        for def in &defs {
            description.push_str("\n");
            description.push_str(&elec_action_line(def));
        }
        description.push_str("\nhelp(of: text)  an action's fields with their help and the shape of its reply.");
        let names: Vec<String> = defs.iter().map(|def| Value::Str(def.name.into()).to_json()).chain([Value::Str("help".into()).to_json()]).collect();
        let props = format!(
            r#""action":{{"type":"string","enum":[{}],"description":"The action to run, or help."}},"args":{{"type":"object","description":"The action's fields by name, each a Splash value as text (22k, [12mm 30mm], top); a JSON number or true/false is taken as its text."}}"#,
            names.join(","),
        );
        manifest = manifest.with_tool(ToolDef::new(tool, &description, schema(&props, &["action"]), risk));
    }
    if action::find("elec.doc.read").is_some() {
        manifest = manifest.with_tool(ToolDef::new(
            ELEC_TEXT_TOOL,
            "Change the text of a Fab Electronics document as typing in its code pane does: one call is one undo step, and the change shows where it lands. Firmware (.fw.splash), circuit (.ckt.splash), block (.blocks.splash), bench (.bench.splash) and part (.part.splash) documents; many design changes in one call are one evaluation and one undo step. Read the document with elec.doc.read first and give its rev; a document that changed since is refused with its new rev. A firmware change is compiled first, as the running bench builds it: text that does not compile is not committed, and the error lists each diagnostic's line and col. When no bench run builds that firmware, the change is committed unchecked and the answer says compiled: false and why. Any other change is evaluated first: text that adds a mistake is not committed, and the error gives its doc, line and col; the answer says evaluated: true and made: the parts and nets it defines. A circuit, block or part change answers once every check has run on it, with checks: the new findings and how many are gone. The answer gives the new rev, each edit's first and last line, and take: what the change does to the run. With requirements in the project it waits until they are judged on the change, with requirements as fab.elec_edit's answers give it; a change of a requirements document (.req.splash) also gives requirements_changed: say each line to the person.",
            schema(r#""doc":{"type":"string","description":"The document's name, as elec.doc.list gives it."},"rev":{"type":"integer","description":"The revision elec.doc.read gave for the text the edits were made against."},"edits":{"type":"array","description":"Replacements, applied together: each old is text found exactly once in that revision (add the text around it to make it so); edits may not overlap.","items":{"type":"object","properties":{"old":{"type":"string"},"new":{"type":"string"}},"required":["old","new"]}},"text":{"type":"string","description":"The document's whole new text, in place of edits."}"#, &["doc", "rev"]),
            Risk::Act,
        ));
    }
    manifest
}

/// Refuses a call whose arguments its tool does not take, in one line naming the first such and
/// the ones the tool takes: a misspelt or unknown argument is never answered as if it were not
/// there. Every AI call (the app's chat and outside agents alike) is checked here first; a tool
/// the manifest does not list is left to its dispatch to refuse.
pub fn check_arguments(tool: &str, args: &Value) -> Result<(), String> {
    let manifest = manifest();
    let Some(def) = manifest.tool(tool) else { return Ok(()) };
    let given = match args {
        Value::Obj(fields) => fields,
        Value::Null => return Ok(()),
        _ => return Err(format!("fab.{tool} takes its arguments as an object")),
    };
    let schema = makepad_model::json::parse(def.parameters.as_bytes()).map_err(|e| format!("fab.{tool}'s own schema does not parse: {e}"))?;
    let takes: Vec<&str> = match schema.get("properties") {
        Some(Value::Obj(props)) => props.iter().map(|(k, _)| k.as_str()).collect(),
        _ => Vec::new(),
    };
    match given.iter().find(|(k, _)| !takes.contains(&k.as_str())) {
        None => Ok(()),
        Some((k, _)) if takes.is_empty() => Err(format!("fab.{tool} takes no arguments: '{k}' is not one")),
        Some((k, _)) => Err(format!("fab.{tool} takes no argument '{k}'; it takes {}", takes.join(", "))),
    }
}

/// What the UI must do for a call besides showing its text.
#[derive(Clone, Debug, PartialEq)]
pub enum RenderRequest {
    /// The review sheets of the current plate: the named views (empty: the
    /// standard eight) of these objects (empty: the whole plate), with what the plate's own
    /// meshes don't show.
    Sheets { views: Vec<String>, objects: Vec<String>, extra: Option<Arc<supports::ColoredMesh>>, camera: Camera, marks: Vec<annotate::RenderMark> },
    /// A picture aimed at a place on the plate; its answer goes back with it.
    Look(Arc<supports::LookRequest>),
    /// Each support piece's way out: a search that runs off the model worker, whose answer the
    /// call waits for.
    Verdicts(Arc<supports::VerdictJob>),
    /// A part's weak spots, read from its slice: a search that runs off the model worker, whose
    /// answer the call waits for.
    WeakSpots(Arc<strengthen::WeakSpotJob>),
    /// A part's tubes, read from its layers: off the model worker the same way.
    Tubes(Arc<tubes::TubesJob>),
    /// `fab.strength_plan`'s weak spots and what it says with them, off the model worker.
    StrengthPlan(Arc<strength_plan::PlanJob>),
    /// Sheets of a supplied plate, such as test print pieces or one part at a build step.
    Pieces(Arc<test_view::PiecesJob>),
    /// Slice the call's plate with the current app print setup.
    CheckSlice,
    /// A reference picture against views of the plate ([`compare::CompareJob::views`]): the app
    /// draws them and answers from what they drew ([`compare::answer`]).
    Compare(Arc<compare::CompareJob>),
    /// Versions of the plate built without committing them, each drawn from the same views and
    /// camera ([`variants::VariantJob::rows`]; with a fit, the survey of the plate as it is first);
    /// the app answers from what they drew ([`variants::answer`]).
    Variants(Arc<variants::VariantJob>),
}

/// What a `fab.render` view frames, plate mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    /// A box: its two opposite corners, lowest first.
    Box([f64; 3], [f64; 3]),
    /// A ball: its middle and radius.
    Ball([f64; 3], f64),
}

/// How `fab.render`'s cameras frame what they show, and in which colours. None of each: as they
/// always have (the whole plate or the objects asked, from the distance that fits them, with the
/// render's field of view, in the design's colours).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Camera {
    pub focus: Option<Focus>,
    /// From the camera to the middle of what it frames, mm.
    pub distance: Option<f64>,
    /// The view's field of view, degrees.
    pub fov: Option<f64>,
    /// Parallel projection, every view at one scale, with a scale bar and a mm grid behind.
    pub ortho: bool,
    /// Which colours the parts and their paint show.
    pub colours: Colours,
}

/// Which colours a render shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Colours {
    /// The design's: each print colour's override, else its design colour.
    #[default]
    Design,
    /// The loaded trays' a print colour maps to: what the printer would print.
    Spools,
}

/// The distances a render's camera takes, mm from it to the middle of what it frames.
pub const RENDER_DISTANCE: (f64, f64) = (1.0, 5000.0);
/// The fields of view a render's camera takes, degrees.
pub const RENDER_FOV: (f64, f64) = (5.0, 120.0);

/// Where a solid is, for a tool that names it: its full name (`instance/solid`), the printed
/// part it is built into (its index on the plate) and that part's frame (model mm to plate mm,
/// row-major), and its box in the model frame.
pub struct Located {
    pub name: String,
    pub part: usize,
    pub to_plate: [[f64; 4]; 3],
    pub bounds: Option<([f64; 3], [f64; 3])>,
}

/// The box (plate mm) that holds the model box `lo..hi` under `m`.
fn plate_box(m: &[[f64; 4]; 3], lo: [f64; 3], hi: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let (mut a, mut b) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for c in 0..8 {
        let q = params::xf_point(m, std::array::from_fn(|k| if c >> k & 1 == 0 { lo[k] } else { hi[k] }));
        for k in 0..3 {
            a[k] = a[k].min(q[k]);
            b[k] = b[k].max(q[k]);
        }
    }
    (a, b)
}

/// How much a frame scales lengths: its longest axis (a placement scales evenly).
pub(crate) fn frame_scale(m: &[[f64; 4]; 3]) -> f64 {
    (0..3).map(|k| (0..3).map(|i| m[i][k] * m[i][k]).sum::<f64>().sqrt()).fold(0.0, f64::max)
}

/// `fab.render`'s `focus`, `distance` and `fov`: each checked, none clamped. A focus naming a
/// solid frames its box on the plate, and one with an `object` is in that solid's model frame:
/// `locate` says where a solid is.
fn camera_arg(args: &Value, locate: &dyn Fn(&str) -> Result<Located, String>, mark_box: &dyn Fn(u32) -> Result<([f64; 3], [f64; 3]), String>) -> Result<Camera, String> {
    camera_in_colours(args, fab_plate::ColourMode::Model, locate, mark_box)
}

fn camera_in_colours(args: &Value, live_colours: fab_plate::ColourMode, locate: &dyn Fn(&str) -> Result<Located, String>, mark_box: &dyn Fn(u32) -> Result<([f64; 3], [f64; 3]), String>) -> Result<Camera, String> {
    let number = |v: &Value| match v {
        Value::F64(f) if f.is_finite() => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    };
    let point = |v: &Value| -> Option<[f64; 3]> {
        let a = v.as_arr().filter(|a| a.len() == 3)?;
        Some([number(&a[0])?, number(&a[1])?, number(&a[2])?])
    };
    let focus = match args.get("focus") {
        None => None,
        Some(Value::Str(name)) => {
            let at = locate(name)?;
            let (lo, hi) = at.bounds.ok_or_else(|| format!("'{name}' has no box to frame yet"))?;
            let (lo, hi) = plate_box(&at.to_plate, lo, hi);
            Some(Focus::Box(lo, hi))
        }
        // a mark the person drew: the box round what it means, as a few mm of context
        Some(f) if f.get("mark").is_some() => {
            let n = f.get("mark").and_then(|m| match m {
                Value::Int(i) if *i >= 1 => Some(*i as u32),
                _ => None,
            });
            let (lo, hi) = mark_box(n.ok_or("'focus' {mark: N} is the number drawn beside a mark (fab.annotations lists them)")?)?;
            Some(Focus::Box(lo, hi))
        }
        Some(f) => {
            let wrong = "'focus' is a box, [[x y z] [x y z]] (two opposite corners), or a ball, {at: [x y z] radius: r}, in plate mm; a solid's name (\"head\") frames that solid; {object: \"head\" at: [x y z] radius: r} or {object: \"head\" box: [[x y z] [x y z]]} is in that solid's model frame";
            let frame = match f.get("object") {
                None => None,
                Some(Value::Str(o)) => Some(locate(o)?.to_plate),
                Some(_) => return Err(wrong.into()),
            };
            if let Some(corners) = f.as_arr().or_else(|| frame.and(f.get("box")).and_then(Value::as_arr)) {
                let [a, b] = corners else { return Err(wrong.into()) };
                let (a, b) = (point(a).ok_or(wrong)?, point(b).ok_or(wrong)?);
                let (lo, hi): ([f64; 3], [f64; 3]) = (std::array::from_fn(|k| a[k].min(b[k])), std::array::from_fn(|k| a[k].max(b[k])));
                if (0..3).all(|k| hi[k] - lo[k] <= 0.0) {
                    return Err("'focus' box has no size: its corners are one point (give a ball, {at: [x y z] radius: r}, to frame round a point)".into());
                }
                let (lo, hi) = match &frame {
                    Some(m) => plate_box(m, lo, hi),
                    None => (lo, hi),
                };
                Some(Focus::Box(lo, hi))
            } else {
                let at = f.get("at").and_then(point).ok_or(wrong)?;
                let radius = f.get("radius").and_then(number).ok_or(wrong)?;
                if radius <= 0.0 {
                    return Err(format!("'focus' radius must be more than 0 mm, not {radius}"));
                }
                match &frame {
                    Some(m) => Some(Focus::Ball(params::xf_point(m, at), radius * frame_scale(m))),
                    None => Some(Focus::Ball(at, radius)),
                }
            }
        }
    };
    let ranged = |k: &str, (lo, hi): (f64, f64), what: &str| -> Result<Option<f64>, String> {
        match args.get(k) {
            None => Ok(None),
            Some(v) => match number(v) {
                Some(x) if (lo..=hi).contains(&x) => Ok(Some(x)),
                Some(x) => Err(format!("'{k}' {x} is out of range: {what} {lo} to {hi}")),
                None => Err(format!("'{k}' is a number: {what} {lo} to {hi}")),
            },
        }
    };
    let distance = ranged("distance", RENDER_DISTANCE, "mm from the camera to the middle of what it frames,")?;
    let fov = ranged("fov", RENDER_FOV, "degrees across the view,")?;
    let ortho = match args.get("ortho") {
        None => false,
        Some(v) => v.as_bool().ok_or("'ortho' is true or false")?,
    };
    if ortho {
        for (k, given) in [("fov", fov.is_some()), ("distance", distance.is_some())] {
            if given {
                return Err(format!("'{k}' does not apply to an orthographic view: its scale is what it frames ('focus', else the objects shown)"));
            }
        }
    }
    let requested = match args.get("colours") {
        None => live_colours,
        // Older saved tool calls used these spellings.
        Some(v) if v.as_str() == Some("spools") => fab_plate::ColourMode::Material,
        Some(v) if v.as_str() == Some("design") => fab_plate::ColourMode::Model,
        Some(v) => v.as_str().and_then(fab_plate::ColourMode::from_key).ok_or(match live_colours {
            fab_plate::ColourMode::Model => "'colours' takes 'model' (the model's own colours, the default) or 'material' (the mapped spools)",
            fab_plate::ColourMode::Material => "'colours' takes 'model' (the model's own colours) or 'material' (the mapped spools, the default)",
        })?,
    };
    let colours = match requested {
        fab_plate::ColourMode::Model => Colours::Design,
        fab_plate::ColourMode::Material => Colours::Spools,
    };
    Ok(Camera { focus, distance, fov, ortho, colours })
}

/// The views `fab.render` knows by name (a custom view is "yaw,pitch" in degrees).
pub const VIEW_NAMES: &[&str] = &["front", "back", "left", "right", "top", "bottom", "iso", "iso_back", "iso_left", "iso_right", "low", "underside"];

/// One answer.
pub struct Reply {
    pub ok: bool,
    /// Pictures that go with the answer (a section drawing): (label, PNG).
    pub images: Vec<(String, Vec<u8>)>,
    /// A Splash value for the caller.
    pub text: String,
    /// A new evaluation to show (the head moved, or a model was opened).
    pub update: Option<(Arc<Compiled>, Arc<Evaluated>)>,
    pub render: Option<RenderRequest>,
}

/// Why an edited program was not taken: where (1-based line and column, when it says)
/// and what.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceProblem {
    /// The script it is in (empty: the project).
    pub file: String,
    pub line: Option<(u32, u32)>,
    pub message: String,
}

/// The code panel's files, in tab order: project, placed scripts, then their paint files.
#[derive(Clone, Debug)]
pub struct SourceFiles {
    pub tabs: Vec<(String, String)>,
    pub texts: Arc<BTreeMap<String, String>>,
}

impl std::fmt::Display for SourceProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file = if self.file.is_empty() { String::new() } else { format!("{}:", self.file) };
        match self.line {
            Some((l, c)) => write!(f, "{file}{l}:{c}: {}", self.message),
            None => write!(f, "{file}{}", self.message),
        }
    }
}

impl Reply {
    fn ok(v: S) -> Reply {
        Reply { images: Vec::new(), ok: true, text: out::text(&v), update: None, render: None }
    }
    fn err(message: impl Into<String>) -> Reply {
        Reply { images: Vec::new(), ok: false, text: out::text(&obj(vec![("error", s(message.into()))])), update: None, render: None }
    }
}

/// One part moved on the plate (see [`Session::place_parts`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PartMove {
    pub object: String,
    /// Row-major affine in printer mm: `p' = delta * p` on the part as it is now.
    pub delta: [[f64; 4]; 3],
}

/// Turn a placed part about its bounds centre so the outward plate normal faces down.
/// `Session::place_parts` applies this move and drops the whole object onto the bed.
pub fn lay_flat_move(ev: &Evaluated, object: &str, normal: [f64; 3]) -> Option<PartMove> {
    if normal.iter().any(|x| !x.is_finite()) || normal.iter().all(|x| *x == 0.0) {
        return None;
    }
    let i = ev.plate.placed_parts().position(|p| p.object == object)?;
    let c = ev.plate.placed_part(i)?.bounds().center();
    // Scale first so every finite nonzero normal can be normalised without overflow.
    let scale = normal.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
    let r = fab_model::eval::turn_onto(normal.map(|x| x / scale), [0.0, 0.0, -1.0]);
    let row = |i: usize| [r[i][0], r[i][1], r[i][2], c[i] - (r[i][0] * c[0] + r[i][1] * c[1] + r[i][2] * c[2])];
    Some(PartMove { object: object.to_string(), delta: [row(0), row(1), row(2)] })
}

/// What a parameter preview's build polls: true stops it. Its argument says whether the plate
/// with a parameter at a value builds no exact body ([`FabModel::bodies_built`]): it compiles the
/// plate at that value, on the thread that asks and a CPU permit of its own ([`on_own_permit`]).
pub type PreviewStop<'a> = &'a dyn Fn(&dyn Fn(&str, &PValue) -> bool) -> bool;

/// Runs `f` on a CPU permit of its own at the thread's class, the thread's own token lent while it
/// does: work a waiting thread does as it polls (a preview's check of a newer value, while the
/// engine builds) is the budget's like any other, never done on a lent token.
fn on_own_permit<T>(label: &str, f: impl FnOnce() -> T) -> T {
    struct Held {
        permit: u32,
        lent: bool,
    }
    impl Drop for Held {
        fn drop(&mut self) {
            makepad_cpu_budget::release(self.permit);
            if self.lent {
                makepad_cpu_budget::take_back();
            }
        }
    }
    let lent = makepad_cpu_budget::lend();
    let class = makepad_cpu_budget::thread_class().unwrap_or(makepad_cpu_budget::VISIBLE);
    let _held = Held { lent, permit: makepad_cpu_budget::admit(class, makepad_cpu_budget::APP, label) };
    f()
}

/// The env a document's scripts build with, from its electronics documents ([`FabModel::docs`])
/// and assets ([`FabModel::elec_assets`]): the boards its case scripts read. The same documents
/// and assets give the same env (`ScriptEnv::same`), so a document switched back to shows its build again.
pub type EnvFor = Arc<dyn Fn(&Arc<fab_model::Docs>, &Arc<fab_model::ElecAssets>) -> ScriptEnv + Send + Sync>;

/// Resolves the app's print setup for an evaluation: the job and its paint streams, and a name for it.
pub type PrintSetupFn = Arc<dyn Fn(&Evaluated) -> Result<(makepad_fab_settings::profiles::JobSetup, Vec<Vec<String>>, String), String> + Send + Sync>;

/// The app's demos: each one's key and title, and how one is built by key.
#[derive(Clone)]
pub struct Demos {
    pub list: Vec<(String, String)>,
    pub build: fn(&str) -> Option<FabModel>,
}

impl Default for Demos {
    fn default() -> Self {
        Demos { list: Vec::new(), build: |_| None }
    }
}

/// The origin of a model edit, carried from the tool or app entry through its commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Author {
    Person,
    Ai,
    /// A remote tool acting outside the conversation.
    Agent,
}

/// The session while an app or tool call holds it. Nested calls inherit its author.
#[must_use]
pub struct AuthoredSession<'a> {
    session: &'a mut Session,
    previous: Option<Author>,
}

impl std::ops::Deref for AuthoredSession<'_> {
    type Target = Session;
    fn deref(&self) -> &Session { self.session }
}

impl std::ops::DerefMut for AuthoredSession<'_> {
    fn deref_mut(&mut self) -> &mut Session { self.session }
}

impl Drop for AuthoredSession<'_> {
    fn drop(&mut self) { self.session.author = self.previous; }
}

/// Shared by the model worker and electronics host until a demo becomes a listed plate.
/// Working ids are never reused across processes. Only the minting session produces
/// records in its plate-less folder.
/// Reserving the identity performs no I/O. Only creating the plate writes its list record.
#[derive(Default, Debug)]
pub struct WorkingId(std::sync::OnceLock<String>);

impl WorkingId {
    pub fn id(&self) -> &str {
        self.0.get_or_init(|| {
            static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("working-{:x}-{:x}-{serial:x}", now.as_nanos(), std::process::id())
        })
    }
}


pub struct Session {
    pub working_id: Arc<WorkingId>,
    pub model: FabModel,
    pub eval: Evaluator,
    /// This session owns the build on the bed. The B-rep engine evicts its accepted builds' bodies last.
    pub holds_bodies: bool,
    /// Where the project is saved, once it is.
    pub path: Option<PathBuf>,
    /// The folder for new projects and exports.
    pub documents: PathBuf,
    /// The last evaluation and the program it is of.
    pub last: Option<Arc<Evaluated>>,
    /// The Slicer's choice as the app says it: the ticked test prints, not the whole plate.
    pub test_mode: bool,
    /// The pieces of the ticked test prints last read ([`Session::slicer_plate`]).
    test_bed: Option<test_view::TestBed>,
    pub compiled: Option<Arc<Compiled>>,
    /// A draft that built with statements skipped (each lost its selector, [`Evaluated::lost`]):
    /// what the view shows until a commit, so picks are taken on it ([`Self::shown_last`]).
    pub draft: Option<(Arc<Compiled>, Arc<Evaluated>)>,
    serial: u64,
    /// The parameter model of `last`.
    params: Option<([u8; 32], Arc<Vec<PartParams>>)>,
    /// The plate's working copy as last committed and the identities of its immutable files.
    autosaved: autosave::Snapshot,
    /// Every plate's record and working copy ([`plates::PlateStore`]). None: a session no
    /// person works in (a test, an automation client) keeps nothing.
    pub store: Option<plates::PlateStore>,
    /// The plate on the bed (its id in the store); none for a demo shown as it ships, or a
    /// document with nothing on it yet. Either becomes a plate at its first edit.
    pub plate: Option<String>,
    /// The demo on the bed as it ships (its key) and the head it opened at.
    demo: Option<(String, Head)>,
    /// The viewport's last settled frame as a small PNG: saved with the plate's working copy
    /// (and beside its file on Save, and with a kept image-to-mesh result). Dropped when the
    /// document is replaced; the next settled frame brings the new one.
    thumb: Option<Arc<[u8]>>,
    /// The working copy has this picture.
    thumb_autosaved: bool,
    /// The document's content as its file (`path`) holds it, read or last saved.
    saved: Option<[u8; 32]>,
    /// The kept copy of the newest image-to-mesh result, waiting for its picture.
    made_waiting: Option<PathBuf>,
    /// The newest image-to-mesh result's kept project, even after its picture is saved.
    pub made_last: Option<PathBuf>,
    /// Moves whenever a plate record changes (made, saved, renamed, edited, its picture): the
    /// app lists the plates again.
    pub plates_rev: u64,
    /// The app's demos: listed for the AI and opened by key.
    pub demos: Demos,
    /// Where a document's env comes from when it goes on the bed ([`EnvFor`]); none: every
    /// document takes the env the bed has.
    pub env_for: Option<EnvFor>,
    /// Called on the model worker when a document takes the bed, before the
    /// calling job can edit it (for example importing onto a new plate).
    pub on_model_open: Option<Arc<dyn Fn(&mut Session) + Send + Sync>>,
    /// Moves whenever a document goes on the bed (a plate switched to, a demo, a file opened, a
    /// new plate): the app then shows it as it was last looked at, or frames it.
    pub switched: u64,
    /// The plates (and demos) last switched away from, the most recent first, each as it was on
    /// the bed with its undo history: switching back takes it from here, so undo goes on where it
    /// was, and the most recent show their build again.
    kept: Vec<Doc>,
    /// The document's head when it was last built: a build put aside with it is its own.
    built_head: Option<[u8; 32]>,
    /// The app's own print setup (machine, process, filaments and paint) for a plate: what as-printed
    /// checks slice with. None: Fab's default process.
    pub print_setup: Option<PrintSetupFn>,
    support_memo: supports::Memo,
    /// Where the support tools' plans come from (the host's slices); none: they can't plan.
    pub slices: Option<Arc<dyn supports::SliceSource>>,
    /// Per print settings volume (`instance/solid`), the slice before it was placed.
    volume_bases: BTreeMap<String, Arc<volumes::VolumeBase>>,
    /// The last list of weak spots, which `fab.strengthen` picks from.
    weak_memo: Arc<Mutex<Option<Arc<strengthen::WeakMemo>>>>,
    /// The list a `fab.strengthen` fixed and what it picked: the next list says what changed.
    strength_before: Option<(Arc<strengthen::WeakMemo>, Vec<(String, String)>)>,
    /// The last list of tubes: a picture's overlay.
    tubes_memo: Arc<Mutex<Option<Arc<tubes::TubesMemo>>>>,
    /// The last `fab.strength_plan`'s answer: asked the same again, it's the plan.
    plan_memo: Arc<Mutex<Option<Arc<strength_plan::PlanMemo>>>>,
    /// The move a `fab.orient` made: the next list of a slice says what its angles were.
    orient_was: Option<tubes::OrientWas>,
    /// The head `fab.strengthen` last left: undoing it takes back what the lists would say of it.
    strength_head: Option<Head>,
    /// The joint checks of `last` (by content hash and instance).
    joint_checks: Option<([u8; 32], String, Arc<Vec<makepad_csg_mech::JointCheck>>)>,
    /// The current call's author. Outside a call, direct edits belong to the person.
    author: Option<Author>,
    /// The files of plates' working copies read back this session that were not as Fab wrote
    /// them, each taken as it reads (by plate id; one line naming each): the plate list says so.
    pub outside_edits: BTreeMap<String, Vec<String>>,
    /// What the person marked on the view ([`annotate`]): not part of the project.
    pub marks: annotate::Marks,
    /// What the person (or the AI) picked in the view ([`picks`]): not part of the project.
    pub picks: picks::Picks,
}

/// One document with what goes with it: what [`Session::take`] swaps in and out.
struct Doc {
    working_id: Arc<WorkingId>,
    model: FabModel,
    path: Option<PathBuf>,
    saved: Option<[u8; 32]>,
    plate: Option<String>,
    demo: Option<(String, Head)>,
    autosaved: autosave::Snapshot,
    thumb: Option<Arc<[u8]>>,
    /// Its plate as last built, with what it was built with ([`Session::build_stamp`]): put
    /// back on the bed while that is unchanged, it shows without being built again.
    built: Option<(Arc<Compiled>, Arc<Evaluated>, u64)>,
}

impl Doc {
    /// The same document: the same plate, or the same demo not yet a plate.
    fn same(&self, other: &Doc) -> bool {
        match (&self.plate, &other.plate) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.demo.as_ref().map(|d| &d.0) == other.demo.as_ref().map(|d| &d.0) && self.demo.is_some(),
            _ => false,
        }
    }
}

/// How many plates switched away from keep their model (and its undo history) in memory.
const KEPT_PLATES: usize = 8;
/// How many of those keep their build too (a build can be large): the most recent ones.
const KEPT_BUILT: usize = 2;

/// The project file a working copy is of (its picture and meshes are named after it).
pub const AUTOSAVE_FILE: &str = "current.fab.splash";
/// A live parameter preview meshes SDF fields at this many times their cell (eight times
/// fewer cells): the drag sees the shape change at once, the commit builds it in full.
pub const PREVIEW_DRAFT: f64 = 2.0;

fn mat3_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|n| a[i][n] * b[n][j]).sum()))
}

/// The turn that lays a mesh turned by `turn` flat on its largest face within 20 degrees of
/// facing down (-Z), if it has one.
fn lay_flat(mesh: &fab_plate::PartMesh, turn: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let cos_limit = 20f64.to_radians().cos();
    let mut best: Option<([f64; 3], f64)> = None;
    for t in &mesh.triangles {
        let [a, b, c] = t.map(|i| mesh.positions[i as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let area2 = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if area2 < 1e-12 {
            continue;
        }
        let n: [f64; 3] = std::array::from_fn(|i| (turn[i][0] * n[0] + turn[i][1] * n[1] + turn[i][2] * n[2]) / area2);
        if -n[2] >= cos_limit && best.is_none_or(|(_, a)| area2 > a) {
            best = Some((n, area2));
        }
    }
    let (n, _) = best?;
    // the shortest turn taking n to -Z: about n x (-Z)
    let axis = [-n[1], n[0], 0.0];
    let s = (axis[0] * axis[0] + axis[1] * axis[1]).sqrt();
    if s < 1e-9 {
        return None;
    }
    let (x, y) = (axis[0] / s, axis[1] / s);
    let (sin, cos) = (s, -n[2]);
    let t = 1.0 - cos;
    Some([
        [cos + x * x * t, x * y * t, y * sin],
        [x * y * t, cos + y * y * t, -x * sin],
        [-y * sin, x * sin, cos],
    ])
}

fn issue_value(i: &fab_printability::Issue) -> S {
    obj(vec![
        ("severity", name(i.severity.name())),
        ("kind", name(i.kind)),
        ("object", opt(i.object.as_ref().map(s))),
        (
            "where",
            obj(vec![
                ("at", mm3(i.at_mm)),
                ("footprint", arr([mm2(i.footprint.min), mm2(i.footprint.max)])),
                ("layer", opt(i.layer.map(int))),
                ("layer_to", opt(i.layer_to.or(i.layer).map(int))),
                ("z_from", mm(i.lowest_z)),
                ("z_to", mm(i.highest_z)),
            ]),
        ),
        ("message", s(&i.message)),
        ("area_mm2", n(i.area_mm2)),
        ("measure", n(i.measure)),
        ("hint", s(&i.hint)),
        ("fix", opt(i.fix.map(s))),
    ])
}

/// Answer-first, bounded tool output; the report itself is never truncated. It leads with its
/// check: pass with no errors, else fail.
pub fn check_value(report: &fab_printability::Report) -> S {
    let [errors, warnings, info] = report.counts();
    let mut issues: Vec<_> = report.issues.iter().collect();
    issues.sort_by_key(|i| i.severity);
    obj(vec![
        checks::Check::of(errors == 0).fields().remove(0),
        ("errors", int(errors)),
        ("warnings", int(warnings)),
        ("info", int(info)),
        ("issues", arr(issues.iter().take(40).map(|i| issue_value(i)))),
        ("more", int(issues.len().saturating_sub(40))),
        (
            "estimates",
            if let Some(p) = report.plate.as_ref() {
                obj(vec![
                    ("time_s", n(p.time_s)),
                    ("mass_g", n(p.mass_g)),
                    ("cost", n(p.cost)),
                    ("flush_mass_g", n(p.flush_mass_g)),
                    ("filament_changes", int(p.filament_changes)),
                    ("nozzle_changes", int(p.nozzle_changes)),
                    ("filament_swaps", int(p.filament_swaps)),
                    (
                        "filaments",
                        arr(p.filaments.iter().map(|f| obj(vec![("filament", int(f.filament)), ("length", mm(f.length_mm)), ("mass_g", n(f.mass_g)), ("cost", n(f.cost)), ("peak_flow_mm3_s", n(f.peak_flow_mm3_s))]))),
                    ),
                ])
            } else {
                S::Nil
            },
        ),
    ])
}

fn printability_summary(report: &fab_printability::Report) -> S {
    let [errors, warnings, _] = report.counts();
    let mut issues: Vec<_> = report.issues.iter().collect();
    issues.sort_by_key(|i| i.severity);
    obj(vec![("errors", int(errors)), ("warnings", int(warnings)), ("top", arr(issues.into_iter().take(3).map(issue_value)))])
}

/// Placed part `i`'s volume (mm³; printed, then excluded): its mesh's, or a field part's (no mesh
/// is built for it) read from its field, None while that read is pending; no volume for a field
/// in a drag's draft.
fn part_volume(ev: &Evaluated, i: usize) -> Option<Option<f64>> {
    match ev.placed_field(i) {
        Some(f) => f.volume.as_ref().map(|v| v.known()),
        None => Some(Some(ev.placed_part(i).mesh.volume())),
    }
}

/// A volume (mm³), or `pending` while a field's is still being read.
fn volume_value(v: Option<f64>) -> S {
    v.map_or_else(|| s("pending"), n)
}

/// The plate's objects that print their bodies as one (`object([..])`,
/// [`fab_plate::Part::body_of`]), in plate order: each with its bodies (plate parts) and the
/// engine that builds each, and its volume with where that comes from: the slice's layers when
/// the plate is sliced (`sliced`: each printed part's as its layers print it, a glued group's
/// union on its first part), where an overlap counts once, else its bodies' own summed, where it
/// counts once per body (pending while a field body's is being read). None: no object of the
/// plate has bodies.
pub fn objects_value(ev: &Evaluated, sliced: Option<&[f64]>) -> Option<S> {
    let mut objects: Vec<(&str, Vec<usize>)> = Vec::new();
    for (i, p) in ev.plate.placed_parts().enumerate() {
        let Some(of) = p.body_of.as_deref() else { continue };
        match objects.iter_mut().find(|o| o.0 == of) {
            Some(o) => o.1.push(i),
            None => objects.push((of, vec![i])),
        }
    }
    if objects.is_empty() {
        return None;
    }
    let printed = ev.plate.parts.len();
    Some(arr(objects.into_iter().map(|(of, bodies)| {
        let mut f = vec![
            ("object", s(of)),
            ("bodies", arr(bodies.iter().map(|&i| obj(vec![("part", s(&ev.placed_part(i).object)), ("engine", name(ev.engine(i).name()))])))),
        ];
        // an object whose eye is off is in no slice
        let layers = sliced.filter(|v| bodies.iter().all(|&i| i < printed && i < v.len())).map(|v| bodies.iter().map(|&i| v[i]).sum::<f64>());
        let volume = match layers {
            Some(v) => Some((Some(v), "slice layers: an overlap counts once")),
            None => bodies.iter().map(|&i| part_volume(ev, i)).collect::<Option<Vec<_>>>().map(|v| (v.into_iter().sum::<Option<f64>>(), "bodies summed: an overlap counts once per body")),
        };
        if let Some((v, from)) = volume {
            f.push(("volume_mm3", volume_value(v)));
            f.push(("volume_from", s(from)));
        }
        obj(f)
    })))
}

/// What a batch changed, part by part: added, removed, and parts whose size, volume,
/// triangle count or field moved (token-lean: unchanged parts are left out). A field part
/// has no mesh: its size, volume and field say what changed.
pub fn plate_diff(before: &Evaluated, after: &Evaluated) -> S {
    let size = |p: &fab_plate::Part| p.bounds().size();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for (i, p) in after.plate.parts.iter().enumerate() {
        match before.plate.parts.iter().position(|q| q.object == p.object) {
            None => added.push(obj([("part", s(&p.object)), ("size", mm3(size(p)))].into_iter().chain(part_volume(after, i).map(|v| ("volume_mm3", volume_value(v)))).collect())),
            Some(j) => {
                let q = &before.plate.parts[j];
                let (sp, sq) = (size(p), size(q));
                let (vp, vq) = (part_volume(after, i), part_volume(before, j));
                if checks::part_moved(before, j, after, i) {
                    let mut fields = vec![("part", s(&p.object)), ("size", mm3(sp)), ("size_delta", mm3(std::array::from_fn(|i| sp[i] - sq[i])))];
                    if let Some(vp) = vp {
                        fields.push(("volume_mm3", volume_value(vp)));
                        if let (Some(vp), Some(Some(vq))) = (vp, vq) {
                            fields.push(("volume_delta_mm3", n(vp - vq)));
                        }
                    }
                    fields.push(("triangles", int(p.mesh.triangles.len())));
                    changed.push(obj(fields));
                }
            }
        }
    }
    let removed: Vec<S> = before.plate.parts.iter().filter(|q| !after.plate.parts.iter().any(|p| p.object == q.object)).map(|q| s(&q.object)).collect();
    obj(vec![("added", arr(added)), ("changed", arr(changed)), ("removed", arr(removed))])
}

/// A linear design colour as six sRGB hex digits (Splash accepts `#x` too).
pub fn color_hex(rgb: [f32; 3]) -> String {
    let byte = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let s = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    format!("#{:02x}{:02x}{:02x}", byte(rgb[0]), byte(rgb[1]), byte(rgb[2]))
}

/// `fab.settings`: what is set; with `find`, the table's matching rows too.
fn settings_text(ev: &Evaluated, find: Option<&str>) -> String {
    let pairs = |v: &[(String, String)]| v.iter().map(|(k, v)| format!("{k}: {}", out::quote(v))).collect::<Vec<_>>().join(" ");
    let mut lines = vec![format!("global: {{{}}}", pairs(&ev.plate.settings))];
    for part in ev.plate.placed_parts() {
        let own: Vec<(String, String)> = fab_plate::own_settings(&part.print).cloned().collect();
        if !own.is_empty() {
            lines.push(format!("{}: {{{}}}", out::quote(&part.object), pairs(&own)));
        }
    }
    let Some(find) = find.map(str::to_lowercase) else {
        lines.push(format!("groups: {}", fab_plate::SettingGroup::ALL.map(|g| g.title()).join(", ")));
        return lines.join("\n");
    };
    for g in fab_plate::SettingGroup::ALL {
        let whole = g.title().to_lowercase() == find;
        let rows: Vec<_> = fab_plate::PRINT_SETTINGS.iter().filter(|s| s.group == g && (whole || s.key.contains(&find) || s.label.to_lowercase().contains(&find))).collect();
        if rows.is_empty() {
            continue;
        }
        lines.push(format!("{}:", g.title()));
        for s in rows {
            let what = match s.kind {
                fab_plate::SettingKind::Bool => "true|false".to_string(),
                fab_plate::SettingKind::Number { min, max, unit, .. } => format!("{min}..{max} {unit}"),
                fab_plate::SettingKind::Choice(c) => c.iter().map(|(v, _)| format!("\"{v}\"")).collect::<Vec<_>>().join("|"),
            };
            let plate = if let fab_plate::SettingScope::Plate(why) = s.scope { format!(" PLATE ONLY: {why}") } else { String::new() };
            lines.push(format!("  {} {what} default {} ({}){plate}", s.key, out::quote(s.default), s.label));
        }
    }
    lines.join("\n")
}

/// The plate's print colours and what uses each: the parts printing in it, the parts whose paint
/// does, and the fill and stroke statements whose material names it (fab.params, fab.inspect).
fn palette_value(c: &Compiled, ev: &Evaluated) -> S {
    arr(params::palette(c, ev).into_iter().map(|k| {
        let mut fields = vec![("name", s(&k.name)), ("rgb", s(color_hex(k.rgb)))];
        if k.implicit {
            fields.push(("implicit", flag(true)));
        }
        if params::default_color_name(&k.name) {
            fields.push(("default_name", flag(true)));
        }
        fields.push(("parts", arr(k.parts.iter().map(s))));
        fields.push(("paint", arr(k.paint.iter().map(s))));
        fields.push(("materials", arr(k.materials.iter().map(s))));
        obj(fields)
    }))
}

fn colors_text(ev: &Evaluated) -> String {
    // A colour's hex is what the live colour mode shows: the design's own colour (Model) or its
    // linked tray's filament (Material). Where the other one differs, say it too.
    let tray = |name: &str| if ev.printer_linked { ev.mapped.inks.get(name).copied().map(color_hex) } else { None };
    let asks = |name: &str, shown: [f32; 3]| {
        let shown = color_hex(shown);
        let asked = ev.colors.iter().find(|c| c.name == name).filter(|c| c.rgb.is_none()).and_then(|c| c.design).map(color_hex);
        match ev.colour_mode {
            fab_plate::ColourMode::Material => match (asked, tray(name)) {
                (Some(a), Some(_)) if a != shown => format!(" (its tray's filament; the design asks {a})"),
                (Some(a), None) => format!(" (no linked tray holds it; the design asks {a})"),
                _ => String::new(),
            },
            fab_plate::ColourMode::Model => tray(name).filter(|t| *t != shown)
                .map_or_else(String::new, |t| format!(" (the design's colour; its tray's filament is {t})")),
        }
    };
    let mut lines = Vec::new();
    for (i, part) in ev.plate.parts.iter().enumerate() {
        let paint = ev.paint.get(i).and_then(Option::as_ref);
        let count = paint.map_or(1, |p| p.colors.len());
        lines.push(format!("{}: {count} colours", out::quote(&part.object)));
        if let Some(paint) = paint {
            for (k, color) in paint.colors.iter().enumerate() {
                let name = color.color.as_deref().or(ev.part_color(i)).unwrap_or("its own");
                let tool = color.filament.map_or_else(|| "unmapped".to_string(), |f| format!("tool {f}"));
                lines.push(format!("  {k} {} {} {tool}{}", color_hex(color.rgb), out::quote(name), asks(name, color.rgb)));
            }
            // what paints it, in order (the last one holding a point wins)
            use fab_paint_field::{Channel, Op, Region};
            let what: Vec<String> = paint.program.layers.iter().map(|l| {
                let region = match &l.region {
                    Region::All => "the whole part".to_string(),
                    Region::Operand(of) if of.len() > 1 => format!("{} features", of.len()),
                    Region::Operand(_) => "a feature".to_string(),
                    Region::Field { .. } => "a region".to_string(),
                    Region::Stroke { points, radius } => format!("a stroke of {} points, {radius:.1} mm", points.len()),
                    Region::Facets { facets, state, .. } => format!("the painted facets of an imported mesh, state {state}, {} leaves", facets.states().iter().filter(|s| *s == state).count()),
                    Region::Flood { seed, angle } => format!("a flood from [{:.1} {:.1} {:.1}] mm at {:.0} deg that reaches nothing on this part", seed[0], seed[1], seed[2], angle.to_degrees()),
                    Region::Patch(p) => {
                        let (n, of) = p.flooded();
                        format!("a smooth-region flood over {n} of {of} surface triangles")
                    }
                    Region::Strokes { file, strokes, .. } => format!("{} hand-drawn strokes ({file}), each in its own colour", strokes.len()),
                };
                if matches!(l.region, Region::Strokes { .. }) {
                    return region;
                }
                match &l.op {
                    Op::Paint { channel: Channel::Color, slot } => format!("{region} in colour {slot}"),
                    Op::Paint { channel: Channel::SupportBlock, .. } => format!("{region}: supports blocked"),
                    Op::Paint { channel: Channel::SupportEnforce, .. } => format!("{region}: supports enforced"),
                    Op::Paint { channel: Channel::NoTrees, .. } => format!("{region}: no trees here"),
                    Op::Paint { channel: Channel::Settings, slot } => match paint.settings.get(usize::from(*slot).wrapping_sub(1)) {
                        Some(set) => format!("{region}: settings {}", set.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")),
                        None => format!("{region}: the part's own settings"),
                    },
                    Op::Erase { .. } => format!("{region} erased"),
                    Op::Mix { mix, .. } => format!("{region} in colours {:?} by its field{}", mix.slots, if mix.sharp >= 1.0 { "" } else { ", blended" }),
                }
            }).collect();
            lines.push(format!("  paint: {}", what.join("; ")));
        } else {
            let shown = [part.color[0], part.color[1], part.color[2]];
            let name = ev.part_color(i).unwrap_or("its own");
            lines.push(format!("  0 {} {} tool {}{}", color_hex(shown), out::quote(name), part.filament, asks(name, shown)));
        }
    }
    if lines.is_empty() { "No placed parts.".into() } else { lines.join("\n") }
}

/// The per-part machine issues that printability reports as `outside_bed` errors, at the
/// center of the same checked bounds. Excluded placements have no printability report.
fn off_bed_parts(ev: &Evaluated) -> impl Iterator<Item = (&fab_plate::Part, &fab_plate::machine::PartReport, &fab_plate::machine::Issue)> {
    use fab_plate::machine::Issue;
    ev.plate.placed_parts().zip(&ev.reports).filter_map(|(part, report)| {
        let issue = report.issues.iter().find(|i| matches!(i, Issue::OffBed | Issue::BelowPlate { .. } | Issue::KeepOut { .. }))?;
        Some((part, report, issue))
    })
}

fn newly_off_bed(before: Option<&Evaluated>, next: &Evaluated) -> Option<fab_model::eval::Failure> {
    let mut outside = off_bed_parts(next).peekable();
    outside.peek()?;
    // Match by placed name. If two placements share a label, their instance/solid IDs
    // distinguish them so an existing error cannot excuse a new one on its namesake.
    let mut ambiguous = HashSet::new();
    for ev in before.into_iter().chain([next]) {
        let mut seen = HashSet::new();
        for part in ev.plate.placed_parts() {
            if !seen.insert(part.name.as_str()) {
                ambiguous.insert(part.name.as_str());
            }
        }
    }
    fn key<'a>(part: &'a fab_plate::Part, ambiguous: &HashSet<&str>) -> (&'a str, Option<&'a str>) {
        (&part.name, ambiguous.contains(part.name.as_str()).then_some(part.object.as_str()))
    }
    let previous: HashSet<_> = before.into_iter().flat_map(off_bed_parts)
        .map(|(part, _, _)| key(part, &ambiguous)).collect();
    let (part, report, issue) = outside.find(|(part, _, _)| !previous.contains(&key(part, &ambiguous)))?;
    let [x, y, z] = report.bounds.center();
    let bed = &next.machine.bed;
    let (width, depth) = (bed.width(), bed.depth());
    let hint = match [(x, width), (y, depth), (z, next.machine.max_height)].into_iter().find(|(at, size)| at.abs() > 10.0 * size) {
        Some((at, _)) => format!("lengths need a unit (for example {}); a bare number is metres", out::text(&mm(at / 1000.0))),
        None => {
            let action = match issue {
                fab_plate::machine::Issue::BelowPlate { .. } => "drop its base onto",
                fab_plate::machine::Issue::KeepOut { .. } => "move it clear of the keep-out zone on",
                _ => "move the whole part inside",
            };
            format!("{action} the {width} × {depth} mm bed")
        }
    };
    Some(fab_model::eval::Failure {
        file: String::new(), line: 1, col: 1, id: part.object.clone(), op: "place".into(), lost: None,
        message: format!("place would put {} off the bed, at [{x:.2} {y:.2} {z:.2}] mm: {hint}", part.name),
    })
}

/// Resolve a printed part once, before a tool uses its solid id or plate index.
fn printed_part_index(parts: &[fab_plate::Part], object: &str) -> Result<usize, String> {
    if let Some(index) = parts.iter().position(|p| p.object == object) {
        return Ok(index);
    }
    let mut named = parts.iter().enumerate().filter(|(_, p)| !p.name.is_empty() && p.name == object);
    if let Some((index, first)) = named.next() {
        if let Some((_, second)) = named.next() {
            let mut ids = vec![first.object.as_str(), second.object.as_str()];
            ids.extend(named.map(|(_, p)| p.object.as_str()));
            return Err(format!("'{object}' names several printed parts: {}", ids.join(", ")));
        }
        return Ok(index);
    }
    let labels: Vec<String> = parts.iter().map(|p| {
        let solid = fab_model::program::split_id(&p.object).map_or(p.object.as_str(), |(_, solid)| solid);
        if p.name.is_empty() || p.name == solid || p.name == p.object {
            p.object.clone()
        } else {
            format!("{} ({})", p.object, p.name)
        }
    }).collect();
    Err(format!("'{object}' is not on the plate (printed objects: {})", labels.join(", ")))
}

/// The plate as the AI reads it.
pub fn plate_value(ev: &Evaluated) -> S {
    let parts = ev.plate.placed_parts().enumerate().map(|(i, p)| {
        let r = ev.reports.get(i);
        let b = p.bounds();
        // a resin printer has no nozzles: its parts print with none
        let nozzle = match (p.nozzle, ev.machine.nozzles.is_empty()) {
            (_, true) => name("none"),
            (Some(k), false) => name(&ev.machine.nozzles.get(k).map(|z| nozzle_word(&z.name)).unwrap_or_default()),
            (None, false) => name("any"),
        };
        let mut fields = vec![
            ("name", s(&p.name)),
            ("solid", s(&p.object)),
            ("print", flag(i < ev.plate.parts.len())),
            ("color", s(ev.part_color(i).unwrap_or_default())),
            ("filament", int(p.filament)),
            ("nozzle", nozzle),
            ("size", mm3(b.size())),
        ];
        // where its model origin sits on the bed: what `place`'s `at` puts there (and the Part
        // panel's Position shows), so a move reads and writes the same point
        if let Some(f) = ev.placed_frame(i) {
            fields.push(("at", mm2([f[0][3], f[1][3]])));
        }
        if let Some(h) = ev.placed_handles(i) {
            fields.push(("rotate", arr(h.rotate.into_iter().map(out::deg))));
        }
        // A field part is its distance field (no mesh is built for it).
        if p.field_bounds.is_some() {
            fields.push(("field", flag(true)));
            if let Some(volume) = ev.fields.get(i).and_then(Option::as_ref).and_then(|f| f.volume.as_ref()) {
                fields.push(("volume_mm3", volume_value(volume.known())));
            }
            // how far its box reaches under the solid point it rests on (it may sit that far into
            // the bed): what its field's proof left open
            if let Some(gap) = ev.fields.get(i).and_then(Option::as_ref).and_then(|f| f.bottom_gap) {
                fields.push(("bottom_gap_mm", n(gap)));
            }
        } else {
            fields.push(("volume_mm3", n(p.mesh.volume())));
            fields.push(("triangles", int(p.mesh.triangles.len())));
            fields.push(("closed", flag(p.mesh.open_edges() == 0)));
        }
        fields.push(("issues", arr(r.into_iter().flat_map(|r| &r.issues).map(|i| s(i.describe(&ev.machine))))));
        // An insert: the part it rides in. Printed into it (`embed: true`), the room the slice
        // leaves for it, on the bed (mm): its box grown by its clearance sideways and over it, open
        // above so it drops in. `fab.slice` says the layer the print pauses before for it.
        let excluded = i.checked_sub(ev.plate.parts.len()).and_then(|k| ev.plate.excluded.get(k));
        if let Some((e, insert)) = excluded.and_then(|e| Some((e, e.insert.as_ref()?))) {
            fields.push(("into", s(&insert.into)));
            if insert.embed {
                let c = insert.clearance;
                fields.push(("embed", obj(vec![
                    ("clearance", mm(c)),
                    ("cavity_min", mm3([b.min[0] - c, b.min[1] - c, b.min[2]])),
                    ("cavity_max", mm3([b.max[0] + c, b.max[1] + c, b.max[2] + c])),
                ])));
            }
            // A heat-set insert (`melt: heat_insert(..)`): its hole on the bed (mm), what it
            // wants of its part round the hole and under it, and what the part has there,
            // measured (41 rays through the part, on this call).
            if let Some(m) = e.melt() {
                let mut melt = vec![
                    ("size", s(&m.size)),
                    ("mouth", mm3(m.mouth)),
                    ("axis", arr(m.axis.map(n))),
                    ("hole", mm(m.hole)),
                    ("depth", mm(m.depth)),
                    ("taper", mm(m.taper)),
                    ("wants_wall", mm(m.wall)),
                    ("wants_floor", mm(m.floor)),
                ];
                match ev.plate.parts.iter().find(|p| p.object == insert.into).map(|h| m.seat(&h.mesh)) {
                    Some(fab_plate::melt::Seat::Held { wall, floor }) => melt.extend([("wall", mm(wall)), ("floor", mm(floor))]),
                    Some(fab_plate::melt::Seat::NoHole) => melt.push(("no_hole", flag(true))),
                    None => {}
                }
                fields.push(("melt", obj(melt)));
            }
        }
        obj(fields)
    });
    let mut fields = vec![
        ("printability", printability_summary(&ev.printability)),
        ("machine", s(&ev.machine.name)),
        ("parts", arr(parts)),
        ("blocking_issues", int(ev.blocking_issues())),
    ];
    if !ev.warnings.is_empty() {
        fields.push(("warnings", arr(ev.warnings.iter().map(s))));
    }
    fields.push(("build_ms", n(ev.millis)));
    obj(fields)
}

/// One script's solids as the AI reads them: id (the script's own), type, where it is
/// written, the solid it is part of and the object it prints in.
pub fn script_solids(sc: &Script) -> S {
    let p = &sc.program;
    arr(p.solids.iter().enumerate().map(|(i, sd)| {
        let mut f = vec![("id", s(&sd.id)), ("type", name(sd.proto))];
        if let Some((line, col)) = sd.loc {
            f.push(("at", s(format!("{line}:{col}"))));
        }
        if let Some(q) = sd.parent {
            f.push(("in", s(&p.solids[q].id)));
        }
        if let Some(o) = sc.object_of(i) {
            f.push(("object", s(&o.name)));
        }
        obj(f)
    }))
}

/// The project's solids as the AI reads them: per instance, `instance/solid` ids.
pub fn solids_value(c: &Compiled) -> S {
    arr(c.instances.iter().map(|i| obj(vec![("instance", s(&i.name)), ("script", s(&i.script.file)), ("solids", script_solids(&i.script))])))
}

fn step_value(step: fab_model::steps::Step) -> S {
    let mut fields = vec![("n", int(step.n)), ("id", s(step.id)), ("verb", s(step.verb))];
    if let Some(value) = step.value {
        fields.push(("value", s(value)));
    }
    fields.push(("line", int(step.line)));
    if !step.uses.is_empty() {
        fields.push(("uses", arr(step.uses.iter().map(|b| s(b.to_string())))));
    }
    if step.off {
        fields.push(("off", flag(true)));
    }
    if let Some(function) = step.made_in {
        fields.push(("made_in", s(function)));
    }
    if !step.shared_with.is_empty() {
        fields.push(("shared_with", arr(step.shared_with.into_iter().map(s))));
    }
    out::line(obj(fields))
}

fn steps_value(c: &Compiled, ev: &Evaluated) -> S {
    let mut parts: HashMap<_, _> = ev.plate.placed_parts().map(|p| (p.object.as_str(), Vec::new())).collect();
    for inst in &c.instances {
        let part = |i: usize| fab_model::program::part_id(&inst.name, &inst.script.program.solids[i].id);
        let bodies = inst.placed.iter().filter(|b| parts.contains_key(part(b.solid).as_str())).map(|b| (b.solid, b.object.as_str()));
        for (i, steps) in fab_model::steps::body_steps(&inst.script, bodies) {
            if let Some(list) = parts.get_mut(part(i).as_str()) {
                *list = steps;
            }
        }
    }
    arr(ev.plate.placed_parts().map(|p| {
        let steps = parts.remove(p.object.as_str()).unwrap_or_default();
        obj(vec![("part", s(&p.object)), ("steps", arr(steps.into_iter().map(step_value)))])
    }))
}

/// The scripts as the AI reads them: file, the objects each emits, its base instances,
/// their linked clone ids and (`source`) its text and solids.
pub fn scripts_value(model: &FabModel, source: bool) -> S {
    arr(model.state().scripts.iter().map(|(file, text)| {
        let instances: Vec<S> = model.project().scripts.iter().filter(|r| r.file == *file).map(|r| {
            let links: BTreeSet<u32> = model.project().places.iter().filter(|p| p.instance == r.name && p.link > 0).map(|p| p.link).collect();
            obj(vec![("base", s(&r.name)), ("links", arr(links.into_iter().map(|link| s(fab_model::program::link_name(&r.name, link)))))])
        }).collect();
        let mut f = vec![("file", s(file)), ("instances", arr(instances))];
        match model.compile_script(file) {
            Ok(sc) => {
                f.push(("objects", arr(sc.objects.iter().map(|o| s(&o.name)))));
                if source {
                    f.push(("source", s(text)));
                    f.push(("solids", script_solids(&sc)));
                }
            }
            Err(e) => {
                f.push(("error", s(e.to_string())));
                if source {
                    f.push(("source", s(text)));
                }
            }
        }
        obj(f)
    }))
}

/// A plate's name: its file name without `.fab.splash` (or `.3mf`).
pub fn project_name(path: &Path) -> String {
    let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("plate");
    file.strip_suffix(&format!(".{}", project::EXTENSION)).or_else(|| file.strip_suffix(".3mf")).unwrap_or(file).to_string()
}

/// A photo tile's width when none is given (mm).
pub const PICTURE_WIDTH_MM: f64 = 100.0;

fn safe_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "model".into() } else { s }
}

fn str_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

/// Report surviving bare copies using the instance names on the resulting plate.
fn place_again(before: &project::Project, edits: &[Change], after: &project::Project) -> Vec<S> {
    let mut known_names: HashSet<_> = before.scripts.iter().map(|script| script.name.as_str()).collect();
    let mut files: BTreeMap<&str, (Option<&str>, Vec<&str>)> = BTreeMap::new();
    for change in edits {
        match change {
            Change::AddInstance { file, source: None, name: None, .. } => { files.entry(file.as_str()).or_default(); }
            // A bare addition that reuses a name added and removed in this batch can hide a note.
            Change::AddInstance { name: Some(name), .. } => { known_names.insert(name.as_str()); }
            _ => {}
        }
    }
    for script in &after.scripts {
        let Some((existing, added)) = files.get_mut(script.file.as_str()) else { continue };
        if known_names.contains(script.name.as_str()) {
            existing.get_or_insert(script.name.as_str());
        } else {
            added.push(script.name.as_str());
        }
    }
    let mut notes = Vec::new();
    for (file, (existing, added)) in files {
        let mut added = added.into_iter();
        let Some(existing) = existing.or_else(|| added.next()) else { continue };
        let existing = out::quote(existing);
        for new in added {
            let new = out::quote(new);
            notes.push(s(format!(
                "With {file} already on the plate as {existing}, script({}) adds another instance, {new} (use remove({new}) to take it off or place({existing}, at: ..) alone to move the existing one).",
                out::quote(file),
            )));
        }
    }
    notes
}

fn point_arg<const N: usize>(value: &Value) -> Result<[f64; N], String> {
    let a = value.as_arr().filter(|a| a.len() == N).ok_or_else(|| format!("expected {N} coordinates in mm"))?;
    let mut point = [0.0; N];
    for (to, value) in point.iter_mut().zip(a) {
        *to = match value {
            Value::F64(n) if n.is_finite() => *n,
            Value::Int(n) => *n as f64,
            _ => return Err("coordinates must be finite numbers in mm".into()),
        };
    }
    Ok(point)
}

fn measure_change(args: &Value) -> Result<Change, String> {
    let points = args.get("points").map(|v| {
        v.as_arr().ok_or_else(|| "'points' must be an array of [x,y,z] in mm".to_string())?
            .iter().map(point_arg::<3>).collect::<Result<Vec<_>, _>>()
    }).transpose()?;
    let name = match args.get("name") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::Str(n)) => Some(Some(n.clone())),
        Some(_) => return Err("'name' must be text or null".into()),
    };
    let index = || args.get("index").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or_else(|| "'index' must be a ruler's nonnegative integer index".to_string());
    match str_arg(args, "action") {
        Some("add") => {
            let kind = match args.get("kind") {
                None => "distance".into(),
                Some(Value::Str(k)) => k.clone(),
                Some(_) => return Err("'kind' must be an identifier string".into()),
            };
            Ok(Change::MeasureAdd { measure: project::Measure { points: points.ok_or("missing 'points'")?, kind, name: name.flatten() } })
        }
        Some("set") => Ok(Change::MeasureSet { index: index()?, points, name }),
        Some("remove") => Ok(Change::MeasureRemove { index: index()? }),
        _ => Err("'action' must be add, set, remove, lay, mode or read".into()),
    }
}

/// A box's sides from `size`: one number is a true cube, three are per axis (mm).
fn side_arg(value: &Value) -> Result<[f64; 3], String> {
    match value {
        Value::F64(n) if n.is_finite() => Ok([*n; 3]),
        Value::Int(n) => Ok([*n as f64; 3]),
        _ => point_arg::<3>(value).map_err(|_| "'size' is a side, or [x, y, z], in mm".to_string()),
    }
}

/// Angles from a tool's argument: one number is a turn about Z, three are about X, then Y, then Z.
/// A number argument, as JSON writes a whole or a fraction.
pub(crate) fn number(v: &Value) -> Option<f64> {
    match v {
        Value::F64(x) if x.is_finite() => Some(*x),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn turn_arg(value: &Value) -> Result<[f64; 3], String> {
    match value {
        Value::F64(n) if n.is_finite() => Ok([0.0, 0.0, *n]),
        Value::Int(n) => Ok([0.0, 0.0, *n as f64]),
        _ => point_arg::<3>(value).map_err(|_| "'turn' is degrees about Z, or [x, y, z] degrees about X, then Y, then Z".to_string()),
    }
}

/// The edits a `fab.test_print` call asks for: the same ones the list, the fields and the gizmo
/// make ([`project::TestEdit`]).
fn test_print_edits(args: &Value, fit: &mut dyn FnMut(&str, Option<&str>) -> Result<volumes::FeatureBox, String>) -> Result<Vec<project::TestEdit>, String> {
    let on_arg = |args: &Value| match args.get("on") {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err("'on' must be true or false".to_string()),
    };
    let name = |args: &Value| str_arg(args, "name").map(str::to_string).ok_or_else(|| "missing 'name': the test print's, as fab.inspect lists them".to_string());
    match str_arg(args, "action") {
        Some("add") => {
            let Some(boxes) = args.get("boxes").and_then(Value::as_arr) else { return Err("missing 'boxes': a list of {feature} or {object, at, size}".into()) };
            let mut one = |b: &Value| -> Result<project::TestEdit, String> {
                let fitted = str_arg(b, "feature").map(|word| fit(word, str_arg(b, "object"))).transpose()?;
                let Some(object) = fitted.as_ref().map(|f| f.object.as_str()).or(str_arg(b, "object")) else { return Err("each box needs 'feature', or 'object': the part it lives on (instance/object, as fab.inspect lists them)".to_string()) };
                let center = match (b.get("at"), &fitted) {
                    (Some(at), _) => point_arg::<3>(at)?,
                    (None, Some(f)) => f.center(),
                    (None, None) => return Err("each box needs 'at': its centre, [x, y, z] mm on the plate".to_string()),
                };
                let size = match (b.get("size"), &fitted) {
                    (Some(size), _) => side_arg(size)?,
                    (None, Some(f)) => f.size(),
                    (None, None) => return Err("each box needs 'size': a side or [x, y, z] mm".to_string()),
                };
                let name = str_arg(b, "name").map(str::to_string).or_else(|| fitted.as_ref().map(|f| f.name.clone()));
                Ok(project::TestEdit::Add { object: object.to_string(), center, size, turn: b.get("turn").map(turn_arg).transpose()?.unwrap_or([0.0; 3]), name, on: on_arg(b)?.unwrap_or(true), in_model: b.get("model").and_then(Value::as_bool) == Some(true) })
            };
            let (mut edits, mut problems) = (Vec::new(), Vec::new());
            for b in boxes {
                match one(b) {
                    Ok(edit) => edits.push(edit),
                    Err(why) => problems.push(why),
                }
            }
            if !problems.is_empty() {
                return Err(format!("{}. Nothing was changed. Name features that exist; or find it by looking (fab.render, fab.pick, fab.fit) and give {{object, at, size}}, or ask what was meant", problems.join("; ")));
            }
            Ok(edits)
        }
        Some("set") => Ok(vec![project::TestEdit::Set {
            name: name(args)?,
            center: args.get("at").map(point_arg::<3>).transpose()?,
            size: args.get("size").map(side_arg).transpose()?,
            turn: args.get("turn").map(turn_arg).transpose()?,
            pose: match args.get("pose") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(p) => Some(Some(point_arg::<3>(p).map_err(|_| "'pose' is [x, y, z] degrees about the model's X, then Y, then Z, or null for as printed".to_string())?)),
            },
            on: on_arg(args)?,
            brim: match args.get("brim") {
                None => None,
                Some(Value::Bool(b)) => Some(*b),
                Some(_) => return Err("'brim' is true for a brim of the piece's own, false for the profile's".into()),
            },
            to: str_arg(args, "to").map(str::to_string),
        }]),
        Some("remove") => Ok(vec![project::TestEdit::Remove { name: name(args)? }]),
        _ => Err("'action' must be add, set, remove, lay, mode or read".into()),
    }
}

impl Session {
    pub fn new(machine: Machine, documents: PathBuf) -> Session {
        let mut eval = Evaluator::new(machine);
        // the calibrated filament sets kept (one that cannot be read, fab.colour_read names)
        eval.calibrated = Arc::new(fab_model::colour_sets::load_all(&documents).0);
        Session { holds_bodies: false, test_mode: false, working_id: Default::default(), test_bed: None, marks: Default::default(), picks: Default::default(), volume_bases: BTreeMap::new(), weak_memo: Arc::default(), strength_before: None, tubes_memo: Arc::default(), plan_memo: Arc::default(), orient_was: None, strength_head: None, model: FabModel::new("Untitled"), eval, path: None, documents, last: None, compiled: None, draft: None, serial: 0, params: None, autosaved: autosave::Snapshot::default(), store: None, plate: None, demo: None, thumb: None, thumb_autosaved: false, saved: None, made_waiting: None, made_last: None, plates_rev: 0, demos: Demos::default(), env_for: None, on_model_open: None, switched: 0, kept: Vec::new(), built_head: None, print_setup: None, support_memo: Default::default(), slices: None, joint_checks: None, author: None, outside_edits: BTreeMap::new() }
    }

    /// The accepted build's meshes stay in this session's evaluator; the bed's also tells the
    /// process's B-rep engine, which every session shares.
    fn keep_bodies(&mut self) {
        self.eval.keep_meshes(self.compiled.as_deref());
        if self.holds_bodies {
            fab_model::eval::keep_exact_bodies(self.compiled.as_deref());
        }
    }

    fn keep(&mut self, c: Compiled, ev: Evaluated) -> (Arc<Compiled>, Arc<Evaluated>) {
        let (c, ev) = (Arc::new(c), Arc::new(ev));
        self.compiled = Some(c.clone());
        self.last = Some(ev.clone());
        self.draft = None;
        self.built_head = Some(self.model.head().content);
        self.keep_bodies();
        (c, ev)
    }

    /// The build the view shows: a draft whose statements lost their selector while one shows,
    /// else the last committed one. Picks are taken on it.
    pub(crate) fn shown_last(&self) -> Option<Arc<Evaluated>> {
        self.draft.as_ref().map(|(_, ev)| ev.clone()).or_else(|| self.last.clone())
    }

    /// The program of [`Self::shown_last`].
    pub(crate) fn shown_compiled(&self) -> Option<Arc<Compiled>> {
        self.draft.as_ref().map(|(c, _)| c.clone()).or_else(|| self.compiled.clone())
    }

    /// A preview built: a draft with lost selectors is what picks are taken on until the next.
    fn previewed(&mut self, c: Compiled, ev: Evaluated) -> (Arc<Compiled>, Arc<Evaluated>) {
        let (c, ev) = (Arc::new(c), Arc::new(ev));
        self.draft = (!ev.lost.is_empty()).then(|| (c.clone(), ev.clone()));
        (c, ev)
    }

    /// What a build depends on besides its document: the printer, its mechanisms profile,
    /// whether fields are meshed (an export's) and the tolerance exact parts slice at. Not the
    /// print colours: the app maps them for each plate on the bed, and a build takes them without
    /// being built again. Not the view's: a build put back shows as the view shows it now.
    fn build_stamp(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}|{:?}|{}|{}", self.eval.machine, self.eval.profile, self.eval.mesh_fields, self.eval.print_chord).hash(&mut h);
        h.finish()
    }

    /// The objects of `ev`'s plate that print their bodies as one ([`objects_value`]): their
    /// volumes from the host's slice of the plate when it has made one (nothing waits for it).
    fn objects(&self, ev: &Evaluated) -> Option<S> {
        if !ev.plate.placed_parts().any(|p| p.body_of.is_some()) {
            return None;
        }
        let sliced = self.slices.as_ref().and_then(|s| s.made(ev)).map(|p| p.volumes());
        objects_value(ev, sliced.as_deref())
    }

    fn render_step(&mut self, ev: &Evaluated, objects: &[String], id: &str, views: Vec<String>, camera: Camera) -> Result<RenderRequest, String> {
        let [object] = objects else { return Err("render with a step needs exactly one object".into()) };
        let part = printed_part_index(&ev.plate.parts, object)?;
        let compiled = self.compiled.as_ref().ok_or("the plate is not built yet")?;
        let steps = fab_model::steps::part_steps(compiled, object);
        let step = steps.iter().find(|s| s.id == id).ok_or_else(|| {
            let ids: Vec<_> = steps.iter().map(|s| s.id.as_str()).collect();
            format!("no step '{id}' on {object}: its steps are [{}]", ids.join(", "))
        })?;
        let inst = fab_model::program::split_id(object).and_then(|(i, _)| compiled.instance(i)).ok_or("the part has no script instance")?;
        let frame = ev.placed_frame(part).ok_or("the part has no placed frame")?;
        let render = self.eval.step_render(&inst.script, step.solid, frame, None)?;
        // A Pieces request draws its entire supplied plate. Keep just this part, in place,
        // then give it the strip's mesh and clear its whole-part field and paint.
        let one = ev.of_pieces(vec![fab_model::eval::ShownPiece {
            of: part, part: ev.plate.parts[part].clone(), render: Default::default(), field: None,
            to_bed: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
        }]);
        Ok(RenderRequest::Pieces(Arc::new(test_view::PiecesJob { ev: Arc::new(one.with_step(0, render)), views, camera, extra: None })))
    }

    /// The parameter model of the current plate: every tunable value of every part, with its
    /// kind, range and handle (see `fab_model::params`).
    pub fn params(&mut self) -> Result<Arc<Vec<PartParams>>, String> {
        if self.last.is_none() || self.compiled.is_none() {
            self.rebuild()?;
        }
        let (c, ev) = (self.compiled.clone().unwrap(), self.last.clone().unwrap());
        if let Some((content, p)) = &self.params {
            if *content == ev.content {
                return Ok(p.clone());
            }
        }
        let p = Arc::new(params::model(&c, &ev));
        self.params = Some((ev.content, p.clone()));
        Ok(p)
    }

    /// The change that sets parameter `key` (`instance/site`) to `value`.
    pub fn param_edit(&mut self, key: &str, value: &PValue) -> Result<Change, String> {
        let model = self.params()?;
        params::edit(self.compiled.as_ref().unwrap(), &model, key, value)
    }

    /// The plate with parameter `key` at `value`, evaluated but not committed (a drag): only
    /// its instance rebuilds (the others come from the cache), its fields meshed in draft.
    /// `stop` stops the build at its next check (a commit took over, or a newer value builds
    /// nothing).
    pub fn preview_param(&mut self, key: &str, value: &PValue, stop: Option<PreviewStop>) -> Result<(Arc<Compiled>, Arc<Evaluated>, Arc<Vec<PartParams>>), String> {
        let change = self.param_edit(key, value)?;
        let (compiled, parts) = (self.compiled.clone().ok_or("nothing is built yet")?, self.params()?);
        let model = &self.model;
        let known = |key: &str, value: &PValue| on_own_permit("fab preview check", || params::edit(&compiled, &parts, key, value).is_ok_and(|c| model.bodies_built(&[c], &compiled)));
        let cancelled = stop.map(|stop| move || stop(&known));
        self.eval.draft = key.split_once('/').map(|(instance, _)| (instance.to_string(), PREVIEW_DRAFT));
        let built = model.preview(&mut self.eval, &[change], cancelled.as_ref().map(|c| c as &dyn Fn() -> bool));
        self.eval.draft = None;
        let (_, c, ev) = built.map_err(|e| e.to_string())?;
        let model = Arc::new(params::model(&c, &ev));
        let (c, ev) = self.previewed(c, ev);
        Ok((c, ev, model))
    }

    /// Set one parameter as one undo step: the part panel, the view's handles and the headset
    /// all commit through here (the same change path the AI uses).
    pub fn set_param(&mut self, key: &str, value: &PValue) -> Reply {
        self.set_param_at_step(key, value, None)
    }

    /// A step editor supplies the feature's name. Use the value being committed here
    /// so a capped drag records its held value in history.
    pub fn set_param_at_step(&mut self, key: &str, value: &PValue, step: Option<&str>) -> Reply {
        let change = match self.param_edit(key, value) {
            Ok(change) => change,
            Err(e) => return Reply::err(e),
        };
        // param_edit resolved the key against this committed parameter model.
        let label = step.and_then(|step| {
            let p = self.params.as_ref()?.1.iter().flat_map(|part| &part.params).find(|p| p.key == key)?;
            let old = fab_model::steps::value_text(p, &p.value, false);
            let new = fab_model::steps::value_text(p, value, true);
            Some(format!("{step}: {old} → {new}"))
        });
        self.commit_labeled("param", vec![change], label)
    }

    /// Link a field to declared parameters as one undo step.
    pub fn link_param(&mut self, key: &str, code: &str) -> Reply {
        let change = self.params().and_then(|parts| params::link(self.compiled.as_ref().unwrap(), &parts, key, code));
        match change {
            Ok(c) => self.commit("param", vec![c]),
            Err(e) => Reply::err(e),
        }
    }

    /// The object scripts' sources (file -> text; what the code panel shows).
    pub fn scripts(&self) -> BTreeMap<String, String> {
        self.model.state().scripts.clone()
    }

    pub fn project_file(&self) -> String {
        format!("{}.fab.splash", self.model.name)
    }

    pub fn source_files(&self) -> SourceFiles {
        let file = self.project_file();
        let mut texts = self.scripts();
        texts.insert(file.clone(), self.model.project_text());
        let mut tabs = vec![(file, self.model.name.clone())];
        let mut seen = std::collections::BTreeSet::new();
        for part in &self.model.project().scripts {
            if seen.insert(part.file.clone()) {
                tabs.push((part.file.clone(), part.name.clone()));
            }
        }
        let script_tabs = tabs.len();
        for index in 1..script_tabs {
            let (script, name) = tabs[index].clone();
            if let Some(source) = texts.get(&script) {
                for file in fab_model::program::paint_files(source) {
                    if texts.contains_key(&file) && seen.insert(file.clone()) {
                        tabs.push((file, format!("{name} paint")));
                    }
                }
            }
        }
        SourceFiles { tabs, texts: Arc::new(texts) }
    }

    /// A whole project replaces its placements and overrides. Reuse the AI's project
    /// statements, retaining the script sources while the old instances are removed.
    fn source_changes(&self, file: &str, text: &str) -> Result<Vec<Change>, SourceProblem> {
        if file != self.project_file() {
            return Ok(vec![Change::Script { file: file.to_string(), edit: Edit::Program { source: text.to_string() } }]);
        }
        let problem = |diags: Vec<makepad_model_script::lang::Diag>| {
            let d = diags.first();
            SourceProblem { file: file.to_string(), line: d.map(|d| (d.line, d.col)), message: d.map_or_else(|| "The project does not evaluate".into(), |d| d.message.clone()) }
        };
        project::parse(text).map_err(&problem)?;
        let mut changes = project::changes(text).map_err(problem)?;
        for change in &mut changes {
            if let Change::AddInstance { file, source, .. } = change {
                *source = self.model.source(file).map(str::to_string);
            }
        }
        let mut reset: Vec<_> = self.model.project().scripts.iter().map(|s| Change::RemoveInstance { instance: s.name.clone() }).collect();
        if self.model.project().arrange_gap.is_some() { reset.push(Change::ArrangeGap(None)); }
        reset.push(Change::PlateSettings { set: Vec::new(), reset: self.model.project().settings.iter().map(|(k, _)| k.clone()).collect() });
        reset.extend(changes);
        Ok(reset)
    }

    /// One script's source.
    pub fn source(&self, file: &str) -> Option<String> {
        self.model.source(file).map(str::to_string)
    }

    fn source_problem(file: &str, e: &ApplyError) -> SourceProblem {
        let mut p = match e {
            ApplyError::Build(f) => SourceProblem { file: f.file.clone(), line: Some((f.line, f.col)), message: if f.id.is_empty() { f.message.clone() } else { format!("{} ({}): {}", f.id, f.op, f.message) } },
            other => SourceProblem { file: String::new(), line: None, message: other.to_string() },
        };
        if !p.file.is_empty() && p.file != file {
            // Paint evaluation reports through its caller's m.paint_file line. Recover the
            // nested location so the paint tab marks the actual failing line.
            let nested = p.message.split_once(&format!("{file}:")).and_then(|(_, rest)| {
                let mut fields = rest.splitn(3, ':');
                Some(((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?), fields.next()?.trim().to_string()))
            });
            if let Some((line, message)) = nested {
                p.line = Some(line);
                p.message = message;
            } else {
                p.message = p.to_string();
                p.line = None;
            }
        }
        p.file = file.to_string();
        p
    }

    /// The project, script or paint `file` at `text`, evaluated but not committed, and its
    /// parameter model. `Ok(None)`: the text is the file as it is. `cancelled` stops the build
    /// at its next check.
    pub fn preview_source(&mut self, file: &str, text: &str, cancelled: Option<&dyn Fn() -> bool>) -> Result<Option<(Arc<Compiled>, Arc<Evaluated>, Arc<Vec<PartParams>>)>, SourceProblem> {
        if self.model.source(file) == Some(text) || (file == self.project_file() && self.model.project_text() == text) {
            // the committed plate shows again
            self.draft = None;
            return Ok(None);
        }
        let changes = self.source_changes(file, text)?;
        let (_, c, ev) = self.model.preview(&mut self.eval, &changes, cancelled).map_err(|e| Self::source_problem(file, &e))?;
        let model = Arc::new(params::model(&c, &ev));
        let (c, ev) = self.previewed(c, ev);
        Ok(Some((c, ev, model)))
    }

    /// Commit `file` at `text` as one undo step (the code panel). A text that does not
    /// build commits nothing: the reply says where it fails.
    pub fn set_source(&mut self, file: &str, text: &str) -> (Reply, Option<SourceProblem>) {
        self.set_source_with(file, text, None)
    }

    /// [`Self::set_source`] with parameter `key` set to `value` in the same undo step: a
    /// selector a parameter's draft lost, named again for that value.
    pub fn set_source_with(&mut self, file: &str, text: &str, param: Option<(&str, &PValue)>) -> (Reply, Option<SourceProblem>) {
        if param.is_none() && (self.model.source(file) == Some(text) || (file == self.project_file() && self.model.project_text() == text)) {
            return (Reply::ok(obj(vec![("head", s(self.model.head().text()))])), None);
        }
        let request_id = self.next_request_id("source");
        let head = self.model.head();
        let mut changes = match self.source_changes(file, text) {
            Ok(changes) => changes,
            Err(p) => return (Reply::err(p.to_string()), Some(p)),
        };
        if let Some((key, value)) = param {
            match self.param_edit(key, value) {
                Ok(c) => changes.push(c),
                Err(e) => return (Reply::err(e), None),
            }
        }
        match self.commit_changes(&request_id, head, &changes, None, true) {
            Ok((applied, built)) => (self.commit_reply(vec![("head", s(applied.head.text())), ("changed", arr(applied.changed.iter().map(s)))], built), None),
            Err(e) => {
                let p = Self::source_problem(file, &e);
                (self.apply_error(e), Some(p))
            }
        }
    }

    /// Place object script files on the plate (dropped `.cad.splash` files) as one undo step: a
    /// new instance of each, its objects where the plate has room ([`Self::script_files_changes`]).
    pub fn add_script_files(&mut self, paths: &[PathBuf]) -> Reply {
        match self.script_files_changes(paths, None) {
            Ok(changes) => self.commit("add", changes),
            Err(e) => Reply::err(e),
        }
    }

    /// The plate as [`Self::add_script_files`] would leave it, evaluated but not committed (files
    /// dragged over the plate): the same changes, so the same arrangement. `cancelled` stops the
    /// build at its next check.
    pub fn preview_script_files(&mut self, paths: &[PathBuf], cancelled: Option<&dyn Fn() -> bool>) -> Result<(Arc<Compiled>, Arc<Evaluated>, Arc<Vec<PartParams>>), String> {
        let changes = self.script_files_changes(paths, cancelled)?;
        let (_, c, ev) = self.model.preview(&mut self.eval, &changes, cancelled).map_err(|e| e.to_string())?;
        let model = Arc::new(params::model(&c, &ev));
        Ok((Arc::new(c), Arc::new(ev), model))
    }

    /// A new instance of each object script file, its objects in free spots ([`Self::place_dropped`]).
    /// A file inside the project's folder keeps its relative path; any other gets a file name
    /// inside the project that neither the project nor an earlier file of the batch has.
    fn script_files_changes(&mut self, paths: &[PathBuf], cancelled: Option<&dyn Fn() -> bool>) -> Result<Vec<Change>, String> {
        let dir = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        let mut changes = Vec::new();
        let mut added: BTreeMap<String, PathBuf> = BTreeMap::new();
        for path in paths {
            if !project::is_script(path) {
                return Err(format!("{} is not an object script (.{})", path.display(), fab_model::program::SCRIPT_EXTENSION));
            }
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let file = match dir.as_ref().and_then(|d| path.strip_prefix(d).ok()) {
                Some(rel) => rel.to_string_lossy().replace('\\', "/"),
                // The same file dragged twice is one script with two instances.
                None => match added.iter().find(|(_, p)| *p == path) {
                    Some((file, _)) => file.clone(),
                    None => {
                        let stem = safe_stem(&project::stem(&path.to_string_lossy()));
                        let taken = |file: &str| added.contains_key(file) || self.model.state().scripts.contains_key(file) || dir.as_ref().is_some_and(|dir| dir.join(file).exists());
                        let file = script_file(&stem);
                        if taken(&file) {
                            (2u64..).map(|n| script_file(&format!("{stem}_{n}"))).find(|file| !taken(file)).unwrap()
                        } else { file }
                    }
                },
            };
            project::script_file_name(&file)?;
            let source = (self.model.source(&file).is_none() && !added.contains_key(&file)).then_some(text);
            added.insert(file.clone(), path.clone());
            changes.push(Change::AddInstance { file, source, name: None, at: None });
        }
        self.place_dropped(&mut changes, cancelled)?;
        Ok(changes)
    }

    /// Put the objects the new instances of `changes` emit where Add would put a new object
    /// ([`free_spot_off`]): off the plate's parts and off the objects placed before them, where
    /// the nozzles their colours print with reach. One build with them arranged gives their
    /// footprints and where the other parts stand once they are on the plate (the shelf pack
    /// keeps a slot for a placed object too, so placing them moves nothing else); the placed
    /// build reuses its bodies. An object with no such spot stays arranged, so the plate says
    /// there is no room for it.
    fn place_dropped(&mut self, changes: &mut Vec<Change>, cancelled: Option<&dyn Fn() -> bool>) -> Result<(), String> {
        let (state, c, ev) = self.model.preview(&mut self.eval, changes, cancelled).map_err(|e| e.to_string())?;
        let had = self.model.state().project.scripts.len();
        let names: Vec<String> = state.project.scripts.iter().skip(had).map(|r| r.name.clone()).collect();
        if names.len() != changes.len() {
            return Ok(());
        }
        let mut taken: Vec<fab_plate::Bounds> = ev.plate.placed_parts().filter(|p| fab_model::program::split_id(&p.object).is_none_or(|(i, _)| !names.iter().any(|n| n == i))).map(|p| p.bounds()).collect();
        let mut places = Vec::new();
        for (change, instance) in changes.iter_mut().zip(&names) {
            let Change::AddInstance { name, at, .. } = change else { continue };
            // Named as the build named it, so the placements below find it.
            *name = Some(instance.clone());
            let Some(inst) = c.instance(instance) else { continue };
            for (k, object) in inst.script.objects.iter().enumerate() {
                let ids: Vec<String> = object.bodies.iter().map(|b| fab_model::program::part_id(instance, &inst.script.program.solids[*b].id)).collect();
                let parts: Vec<usize> = ev.plate.placed_parts().enumerate().filter(|(_, p)| ids.contains(&p.object)).map(|(i, _)| i).collect();
                let Some(bounds) = parts.iter().filter_map(|&i| ev.plate.placed_part(i)).map(|p| p.bounds()).reduce(|a, b| a.union(&b)) else { continue };
                let size = bounds.size();
                let colours: Vec<&str> = parts.iter().flat_map(|&i| ev.part_colours(i)).collect();
                let Some(spot) = free_spot_off(&ev, &taken, [size[0], size[1]], size[2], &ev.nozzles_for(&colours)) else { continue };
                // `at` is where the object's model origin sits: it moves from its centre to the spot.
                let centre = [(bounds.min[0] + bounds.max[0]) * 0.5, (bounds.min[1] + bounds.max[1]) * 0.5];
                let anchor = ev.placed_frame(parts[0]).map(|f| [f[0][3], f[1][3]]).unwrap_or(centre);
                let here = [spot[0] + anchor[0] - centre[0], spot[1] + anchor[1] - centre[1]];
                let (rx, ry) = (size[0] * 0.5, size[1] * 0.5);
                taken.push(fab_plate::Bounds { min: [spot[0] - rx, spot[1] - ry, 0.0], max: [spot[0] + rx, spot[1] + ry, size[2]] });
                if k == 0 {
                    *at = Some(here);
                } else {
                    places.push(Change::Place { instance: instance.clone(), object: object.name.clone(), arg: PlaceArg::At(Some(here)) });
                }
            }
        }
        changes.extend(places);
        Ok(())
    }

    /// Read the photo of the plate's colour swatch back as the calibrated set `name`
    /// ([`fab_model::picture::read_swatch`]), keep it in the documents' calibration folder and
    /// build again, so the pictures that print with it take it.
    pub fn read_colour_swatch(&mut self, photo: &Path, name: &str, object: Option<&str>) -> Reply {
        let (Some(compiled), Some(ev)) = (self.compiled.clone(), self.last.clone()) else { return Reply::err("nothing is built yet: put the colour swatch on the plate first") };
        let swatches: Vec<(&str, &fab_model::picture::Spec)> = compiled
            .instances
            .iter()
            .filter(|i| object.map_or(true, |o| o == i.name))
            .flat_map(|i| fab_model::picture::specs(&i.script).into_iter().filter(|s| s.arg.swatch).map(move |s| (i.name.as_str(), s)))
            .collect();
        let spec = match swatches.as_slice() {
            [(_, spec)] => *spec,
            [] => return Reply::err(match object {
                Some(o) => format!("'{o}' has no colour swatch (m.colour_swatch)"),
                None => "no colour swatch (m.colour_swatch) on the plate: the photo is read against the swatch as it printed".into(),
            }),
            more => return Reply::err(format!("more than one swatch: say which with object ({})", more.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", "))),
        };
        let bytes = match std::fs::read(photo) {
            Ok(b) => b,
            Err(e) => return Reply::err(format!("{}: {e}", photo.display())),
        };
        // the filaments as the build gave the swatch them: the print colours as they show
        let loaded: Vec<(String, [f32; 3])> = ev.colors.iter().filter(|c| !c.implicit).map(|c| (c.name.clone(), fab_model::eval::shown_color(&ev.colors, &ev.mapped.inks, &c.name))).collect();
        let (set, reading) = match fab_model::picture::read_swatch(spec, &loaded, self.eval.profile.nozzle, &bytes, name) {
            Ok(r) => r,
            Err(e) => return Reply::err(e),
        };
        let file = match fab_model::colour_sets::save(&self.documents, &set) {
            Ok(f) => f,
            Err(e) => return Reply::err(e),
        };
        let (sets, unread) = fab_model::colour_sets::load_all(&self.documents);
        self.eval.calibrated = Arc::new(sets);
        if let Err(e) = self.rebuild() {
            return Reply::err(format!("the set is kept ({}), but the plate does not build with it: {e}", file.display()));
        }
        // how far the reading moved each stack from the first guess (OKLab)
        let guess = fab_colour::FilamentSet { measured: Vec::new(), ..set.clone() };
        let moved: Vec<f32> = set.measured.iter().map(|m| fab_colour::oklab::delta_e(fab_colour::oklab::oklab(m.colour), fab_colour::oklab::oklab(guess.colour_of(&m.stack, m.background).0))).collect();
        let mean = moved.iter().sum::<f32>() / moved.len().max(1) as f32;
        let most = moved.iter().cloned().fold(0.0, f32::max);
        let mut fields = vec![
            ("set", out::s(name)),
            ("file", out::s(file.display().to_string())),
            ("filaments", out::s(set.filaments.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", "))),
            ("stacks", out::int(set.measured.len())),
            ("registration", out::mm(reading.registration)),
            ("ramp_off", out::n(reading.ramp_error as f64)),
            ("third_layer_shows", out::n(reading.backing_shows as f64)),
            ("moved_from_guess", out::obj(vec![("mean", out::n(mean as f64)), ("most", out::n(most as f64)), ("seen_at", out::n(0.02))])),
        ];
        if !unread.is_empty() {
            fields.push(("unreadable_sets", out::s(unread.join("; "))));
        }
        Reply { images: Vec::new(), ok: true, text: out::text(&out::obj(fields)), update: None, render: None }
    }

    /// Put a photo on the plate printed in full colour (`fab.picture`): the picture becomes the
    /// project's (its file name made free among them), a new object script prints it as a tile
    /// `width_mm` wide (`m.picture`), its object at `at` (none: arranged), as one undo step.
    /// `name`: what the tile and its picture are called (default: from the file's name).
    pub fn add_picture(&mut self, path: &Path, width_mm: f64, at: Option<[f64; 2]>, name: Option<&str>) -> Reply {
        if !(10.0..=300.0).contains(&width_mm) {
            return Reply::err(format!("width_mm is 10 to 300, not {width_mm}"));
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return Reply::err(format!("{}: {e}", path.display())),
        };
        let picture = match fab_model::picture::Picture::new(bytes) {
            Ok(p) => Arc::new(p),
            Err(e) => return Reply::err(format!("{}: {e}", path.display())),
        };
        let stem = safe_stem(&project::stem(name.or(path.file_name().and_then(|f| f.to_str())).unwrap_or("photo")));
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_else(|| "png".into());
        let taken = |n: &str| self.model.pictures.contains_key(n) || self.model.state().scripts.contains_key(n);
        let image = (1u64..).map(|n| if n == 1 { format!("{stem}.{ext}") } else { format!("{stem}_{n}.{ext}") }).find(|n| !taken(n)).unwrap();
        let name = self.import_name(&stem);
        let file = script_file(&name);
        let source = format!("// {name}: the picture {image} printed in full colour, face down (m.picture).\nuse mod.model.*\nuse mod.fab.*\nlet m = mod.model\nlet {name} = m.picture({{image: \"{image}\" width: {width_mm}mm}})\nobject({name})\n");
        let mut pictures = (*self.model.pictures).clone();
        pictures.insert(image.clone(), picture);
        let previous = std::mem::replace(&mut self.model.pictures, Arc::new(pictures));
        let reply = self.commit("picture", vec![Change::AddInstance { file: file.clone(), source: Some(source), name: Some(name.clone()), at }]);
        if !reply.ok {
            self.model.pictures = previous;
        }
        reply
    }

    /// Put a mesh the AI hub made of a picture on the plate (`fab.image_to_mesh`, the Parts
    /// panel's From image): the fitted mesh becomes the project's asset `name` (made free
    /// among its meshes and scripts), a new object script imports it with a Size parameter,
    /// and the object lands at a free spot on the bed, as one undo step. The answer carries
    /// the mesh's own check, `fab.check_mesh` of the new object and `fab.check` of the plate,
    /// `about` (where it was made, how long each stage took), and asks for a render of it.
    pub fn import_mesh(&mut self, name: &str, fitted: &img2mesh::Fitted, from: &img2mesh::Provenance, printable: Option<&Result<printable::Report, String>>, about: Vec<(&str, S)>) -> Reply {
        let wanted = name.to_string();
        let name = self.import_name(&wanted);
        let file = script_file(&name);
        let image = from.image.replace(['\n', '\r'], " ");
        let header = format!("// {{name}}: made from the picture {image} by the AI hub's {} image-to-3D model.\n// The mesh is this project's `{{name}}` (in its .meshes folder); Size scales it.", from.model);
        let settings = match self.printable_settings(&[]) {
            Ok(settings) => PlaceArg::Settings(settings),
            Err(error) => return Reply::err(error),
        };
        let reply = self.add_imported("image", vec![import::NewPart {
            wanted, mesh: fitted.mesh.clone(), size_mm: fitted.size.iter().copied().fold(0.0, f64::max),
            facets: None, header, size_bounds: (img2mesh::SIZE_MIN_MM, img2mesh::SIZE_MAX_MM), place: vec![settings.clone()], colors: Vec::new(),
        }], Vec::new());
        if !reply.ok { return reply; }
        let changes = vec![
            Change::AddInstance { file: file.clone(), source: self.model.source(&file).map(str::to_string), name: Some(name.clone()), at: None },
            Change::Place { instance: name.clone(), object: name.clone(), arg: settings },
        ];
        if let Err(e) = self.keep_made(&name, changes) {
            eprintln!("fab: the made mesh '{name}' could not be kept as a plate of its own: {e}");
        }
        let Some(ev) = self.last.clone() else { return reply };
        let objects: Vec<String> = ev.plate.parts.iter().filter(|p| p.object.split('/').next() == Some(name.as_str())).map(|p| p.object.clone()).collect();
        let lw = self.eval.materials.line_width;
        let mesh_check = match objects.first() {
            Some(o) => inspect::check_mesh(&Value::Obj(vec![("object".into(), Value::Str(o.clone()))]), &ev, &mut self.eval, lw).unwrap_or_else(|e| obj(vec![("error", s(e))])),
            None => S::Nil,
        };
        let report = fab_printability::analyse_plate(&ev.machine, &ev.plate, &self.eval.materials);
        let f = fitted;
        let mut fields = vec![
            ("imported", s(&name)),
            ("script", s(&file)),
            ("objects", arr(objects.iter().map(s))),
            (
                "mesh",
                obj(vec![
                    ("triangles", int(f.mesh.triangles.len())),
                    ("size", obj(vec![("x", mm(f.size[0])), ("y", mm(f.size[1])), ("z", mm(f.size[2]))])),
                    ("watertight", flag(f.watertight())),
                    ("open_edges", int(f.open_edges)),
                    ("nonmanifold_edges", int(f.nonmanifold_edges)),
                    ("pieces", int(f.shells)),
                    ("background_sheets_left_out", int(f.dropped_sheets)),
                    ("turned_outward", flag(f.flipped)),
                ]),
            ),
        ];
        fields.extend(about);
        if let Some(result) = printable {
            fields.push(("printable", match result {
                Ok(report) => printable_tool::report_value(report),
                Err(error) => obj(vec![("error", s(error))]),
            }));
        }
        fields.push(("check_mesh", mesh_check));
        fields.push(("check", check_value(&report)));
        fields.push(("head", s(self.model.head().text())));
        fields.push(("next", s("A render of the new part follows: look at it. Size is its parameter (fab.params); move it with fab.place; m.blend or m.subtract it with other parts in its script.")));
        Reply { images: Vec::new(), ok: true, text: out::text(&obj(fields)), update: reply.update, render: Some(RenderRequest::Sheets { views: Vec::new(), objects, extra: None, camera: Camera::default(), marks: Vec::new() }) }
    }

    /// Keep an image-to-mesh result as a plate of its own, saved in a folder of its own in the
    /// plates home: the plate it came to may be replaced, the picture it was made from may be
    /// gone, and the hub keeps nothing, so this is the only copy. The viewport's next settled
    /// frame is its picture.
    fn keep_made(&mut self, name: &str, changes: Vec<Change>) -> Result<(), String> {
        let Some(store) = self.store.clone() else { return Ok(()) };
        let machine = self.last.as_ref().map(|e| e.machine.clone()).ok_or("nothing is built yet")?;
        let mut own = Session::new(machine, PathBuf::new());
        own.model.name = name.to_string();
        let mut assets = (*own.model.assets).clone();
        assets.insert(name.to_string(), self.model.assets.get(name).ok_or("the made mesh is missing")?.clone());
        own.model.assets = Arc::new(assets);
        if let Some(facets) = self.model.facets.get(name) {
            own.model.facets = Arc::new(HashMap::from([(name.to_string(), facets.clone())]));
        }
        let reply = own.as_author(self.commit_author()).commit("image", changes);
        if !reply.ok {
            return Err(reply.text);
        }
        let file = store.new_file(name);
        project::save(&own.model, &file)?;
        store.create(name, Some(file.clone()), plates::content_text(&own.model.content()), plates::Origin::Made)?;
        self.plates_rev += 1;
        self.made_last = Some(file.clone());
        self.made_waiting = Some(file);
        Ok(())
    }

    /// Bind the originating author to every commit made while the job holds the session.
    pub fn as_author(&mut self, author: Author) -> AuthoredSession<'_> {
        let previous = self.author.replace(author);
        AuthoredSession { session: self, previous }
    }

    fn commit_author(&self) -> Author {
        self.author.unwrap_or(Author::Person)
    }

    /// Commit `changes` as one undo step with the current author.
    pub fn commit(&mut self, what: &str, changes: Vec<Change>) -> Reply {
        self.commit_labeled(what, changes, None)
    }

    fn commit_labeled(&mut self, what: &str, changes: Vec<Change>, label: Option<String>) -> Reply {
        let request_id = self.next_request_id(what);
        let head = self.model.head();
        match self.commit_changes_labeled(&request_id, head, &changes, None, true, label) {
            Ok((applied, built)) => self.commit_reply(vec![("head", s(applied.head.text())), ("changed", arr(applied.changed.iter().map(s)))], built),
            Err(e) => self.apply_error(e),
        }
    }

    /// Undo or redo one step of the document's history, at whatever head the document is at
    /// now: the app's one history (caption, keys, menu, headset) never races its own edits.
    pub fn undo_redo(&mut self, action: HistoryAction) -> Reply {
        let head = self.model.head();
        match self.model.history(&mut self.eval, head, action, None) {
            Ok((head, c, ev)) => self.commit_reply(vec![("head", s(head.text()))], Some((c, ev))),
            Err(e) => self.apply_error(e),
        }
    }

    /// A UI request id (the AI brings its own).
    pub fn next_request_id(&mut self, what: &str) -> String {
        self.serial += 1;
        format!("ui-{what}-{}", self.serial)
    }

    /// The printer's mechanisms profile (clearances, measured fits): joints and the weave builders size
    /// from it. True when it changed (the project then needs a rebuild).
    pub fn set_profile(&mut self, profile: makepad_csg_mech::Profile) -> bool {
        if self.eval.profile == profile {
            return false;
        }
        self.model.set_context(fab_model::program::CompileContext::from_profile(&profile));
        self.eval.profile = profile;
        true
    }

    /// The plate as the review sheets draw it: the last one with its exact parts' display meshes
    /// at the view's detail. Its field parts show from their fields, as in the view (each sheet's
    /// view traces them), never from meshes: none is built for them.
    pub fn review_plate(&mut self) -> Result<Arc<Evaluated>, String> {
        let ev = self.last.clone().ok_or("nothing to render")?;
        self.for_review(ev)
    }

    /// A plate as the review sheets draw it ([`Self::review_plate`]).
    pub(crate) fn for_review(&mut self, ev: Arc<Evaluated>) -> Result<Arc<Evaluated>, String> {
        let turned = self.eval.set_view_meshes(false);
        let shown = self.eval.shown_at_view(&ev);
        if turned {
            self.eval.set_view_meshes(true);
        }
        Ok(shown?.map_or(ev, Arc::new))
    }

    /// Evaluate the current project (after opening one).
    /// `ev` with its field parts meshed, for what reads triangles (an export, the weave's
    /// mesh slice): evaluated once more, this plate only.
    fn meshed(&mut self, ev: Arc<Evaluated>, cancelled: Option<&dyn Fn() -> bool>) -> Result<Arc<Evaluated>, String> {
        if !ev.plate.parts.iter().any(|p| p.field_bounds.is_some() && p.mesh.triangles.is_empty()) {
            return Ok(ev);
        }
        let was = std::mem::replace(&mut self.eval.mesh_fields, true);
        let meshed = self.model.evaluate(&mut self.eval, cancelled);
        self.eval.mesh_fields = was;
        meshed.map(|(_, ev)| Arc::new(ev)).map_err(|f| f.to_string())
    }

    /// The chord exact parts are shown at (mm): the view's zoom picks it. Their display meshes
    /// only, no build: the model, its checks and its slice keep their own tolerances. `None`:
    /// nothing shows differently.
    pub fn set_exact_detail(&mut self, chord: f64) -> Option<Result<Arc<Evaluated>, String>> {
        if !self.eval.set_view_chord(chord) {
            return None;
        }
        self.show_view()
    }

    /// The builds the session holds: the plate on the bed's, then those put aside with the
    /// plates (and demos) switched away from, which show again without a rebuild. What the app
    /// makes of a build (its scene) is worth keeping while the build is here.
    pub fn builds(&self) -> impl Iterator<Item = &Arc<Evaluated>> {
        self.last.iter().chain(self.kept.iter().filter_map(|d| d.built.as_ref().map(|b| &b.1)))
    }

    /// Set the display mode for future builds and recolour the one on the bed.
    pub fn set_colour_view(&mut self, mode: fab_plate::ColourMode, linked: bool) -> Option<Arc<Evaluated>> {
        self.eval.colour_mode = mode;
        self.eval.printer_linked = linked;
        let ev = self.last.as_mut()?;
        if ev.colour_mode == mode && ev.printer_linked == linked { return None; }
        Arc::make_mut(ev).set_colour_view(mode, linked);
        Some(ev.clone())
    }

    /// The app's colour mapping for the plate on the bed: its parts and paint take it without a
    /// rebuild, and its printability is checked again. None for the mapping it already shows (or
    /// with nothing built).
    pub fn map_colours(&mut self, mapped: Arc<fab_model::eval::Mapped>) -> Option<Arc<Evaluated>> {
        if *self.eval.mapped == *mapped {
            return None;
        }
        self.eval.mapped = mapped.clone();
        let mut ev = (*self.last.clone()?).clone();
        ev.set_mapped(mapped);
        ev.printability = Arc::new(fab_printability::analyse_plate(&ev.machine, &ev.plate, &self.eval.materials));
        ev.add_bend_issues();
        let ev = Arc::new(ev);
        self.last = Some(ev.clone());
        Some(ev)
    }

    /// The plate's grown patterns have grown to the steps they print at: the plate with those
    /// states, which a slice may now take. `None` while one still grows, or none was.
    pub fn patterns_grown(&mut self) -> Option<Arc<Evaluated>> {
        let ev = Arc::new(self.last.as_ref()?.with_patterns_grown()?);
        self.last = Some(ev.clone());
        Some(ev)
    }

    /// The bends of the plate's flexing pieces were measured, after its build: a bond that could
    /// not be is an error on the plate's report. The plate with it; none when none failed or the
    /// plate moved on.
    pub fn bends_measured(&mut self, content: [u8; 32]) -> Option<Arc<Evaluated>> {
        let mut ev = (**self.last.as_ref().filter(|ev| ev.content == content)?).clone();
        if !ev.add_bend_issues() {
            return None;
        }
        let ev = Arc::new(ev);
        self.last = Some(ev.clone());
        Some(ev)
    }

    /// The plate with its display meshes as the view now shows them; `None` when they show so
    /// already.
    fn show_view(&mut self) -> Option<Result<Arc<Evaluated>, String>> {
        let ev = self.last.clone()?;
        match self.eval.shown_at_view(&ev) {
            Ok(Some(shown)) => {
                let shown = Arc::new(shown);
                self.last = Some(shown.clone());
                Some(Ok(shown))
            }
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// The tolerance a slice keeps to an exact part's surface (mm): the job's `resolution`.
    /// True when it changed (the plate's print meshes are of the old one until it builds again).
    pub fn set_print_chord(&mut self, chord: f64) -> bool {
        std::mem::replace(&mut self.eval.print_chord, chord) != chord
    }

    /// What the job prints round the parts ([`fab_model::eval::Rim`]): the layout keeps it where
    /// their nozzles reach. True when it changed (the plate lays out again).
    pub fn set_rim(&mut self, rim: fab_model::eval::Rim) -> bool {
        std::mem::replace(&mut self.eval.rim, rim) != rim
    }

    /// Field parts shown as meshes (the Mesh look) or from their fields: their display meshes
    /// only, no build (they are laid out, checked and sliced from their fields either way).
    /// `None`: nothing shows differently.
    pub fn set_mesh_fields(&mut self, on: bool) -> Option<Result<Arc<Evaluated>, String>> {
        if !self.eval.set_view_meshes(on) {
            return None;
        }
        self.show_view()
    }

    /// Evaluations run so far, and fields meshed in this process so far.
    pub fn counts(&self) -> (u64, u64) {
        (self.eval.evaluations, fab_model::eval::fields_meshed())
    }

    pub fn rebuild(&mut self) -> Result<Arc<Evaluated>, String> {
        let (c, ev) = self.model.evaluate(&mut self.eval, None).map_err(|f| f.to_string())?;
        Ok(self.keep(c, ev).1)
    }

    /// The document differs from its file: never saved with something on it, or edited since
    /// it was read or saved. The plate list and the title show it with a `*`.
    pub fn modified(&self) -> bool {
        match (&self.path, &self.saved) {
            (Some(_), Some(saved)) => *saved != self.model.content(),
            (Some(_), None) => true,
            (None, _) => !self.model.is_empty(),
        }
    }

    /// The demo on the bed as it ships, by key.
    pub fn demo_key(&self) -> Option<&str> {
        self.demo.as_ref().map(|(k, _)| k.as_str())
    }

    /// What is on the bed: a plate's id, or a demo's key (true); none for a session without plates.
    pub fn bed(&self) -> Option<(String, bool)> {
        self.plate.clone().map(|id| (id, false)).or_else(|| self.demo_key().map(|k| (k.to_string(), true)))
    }

    /// A document that is no plate yet becomes one at its next autosave: a demo once edited,
    /// anything else once something is on it.
    fn forks(&self) -> bool {
        match &self.demo {
            Some((_, head)) => head.content != self.model.content(),
            None => !self.model.is_empty(),
        }
    }

    /// Whether the worker has document changes or a picture to save after its quiet period.
    pub fn needs_autosave(&self) -> bool {
        if self.store.is_none() {
            return false;
        }
        match &self.plate {
            Some(_) => !self.autosaved.is_current(&self.model, self.path.as_deref())
                || (!self.model.project().scripts.is_empty() && self.thumb.is_some() && !self.thumb_autosaved),
            None => self.forks(),
        }
    }

    /// Keep electronics documents that reached no plate on disk: in `plate`'s folder when the
    /// store has it, else in the store's root. Returns where they are.
    pub fn keep_docs_aside(&self, plate: Option<&str>, docs: &fab_model::Docs, why: &str) -> Result<PathBuf, String> {
        let store = self.store.as_ref().ok_or("no plate store")?;
        let dir = plate.map(|id| store.dir(id)).filter(|dir| dir.is_dir()).unwrap_or_else(|| store.root.clone());
        autosave::keep_texts_aside(&dir, docs, why)
    }

    fn adopt_working_id(&self, store: &plates::PlateStore, file: Option<PathBuf>, origin: plates::Origin) -> Result<plates::Plate, String> {
        let id = self.working_id.id().to_owned();
        std::fs::create_dir_all(store.dir(&id)).map_err(|e| e.to_string())?;
        let plate = plates::Plate { id, name: self.model.name.clone(), file, saved: String::new(), origin,
            edited: makepad_civil_time::now_secs(), modified: false, printer: String::new(), removed: false };
        store.write(&plate)?;
        Ok(plate)
    }

    /// Commit the plate's working copy, then its settled picture and its record. A document
    /// that is no plate yet becomes one here (see [`Self::forks`]): a demo's first edit makes
    /// the person's own plate, the demo stays as it ships. An empty plate's working copy is
    /// empty (its old files go).
    pub fn autosave(&mut self) -> Result<(), String> {
        let Some(store) = self.store.clone() else { return Ok(()) };
        if self.plate.is_none() {
            if !self.forks() {
                return Ok(());
            }
            let origin = self.demo.as_ref().map_or(plates::Origin::Plate, |(key, _)| plates::Origin::Demo(key.clone()));
            let plate = self.adopt_working_id(&store, None, origin)?;
            store.set_current(Some(&plates::Current::Plate(plate.id.clone())))?;
            self.plate = Some(plate.id);
            self.demo = None;
            self.autosaved = autosave::Snapshot::default();
            self.thumb_autosaved = false;
        }
        let id = self.plate.clone().expect("a plate by now");
        let mut record = store.read(&id).unwrap_or(plates::Plate { id: id.clone(), name: String::new(), file: None, edited: 0, printer: String::new(), modified: false, saved: String::new(), origin: plates::Origin::Plate, removed: false });
        // a plate taken off the list is on its way off the bed: it keeps what it had
        if record.removed {
            return Ok(());
        }
        let dir = store.dir(&id);
        let changed = !self.autosaved.is_current(&self.model, self.path.as_deref());
        self.autosaved.save(&dir, &self.model, self.path.as_deref())?;
        let picture = store.picture(&id);
        let mut pictured = false;
        if self.model.project().scripts.is_empty() {
            match std::fs::remove_file(&picture) {
                Ok(()) => pictured = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", picture.display())),
            }
            self.thumb_autosaved = false;
        } else {
            match &self.thumb {
                Some(png) if !self.thumb_autosaved => {
                    // the view's own picture: no painted one's content goes with it
                    let _ = std::fs::remove_file(plates::painted_content(&picture));
                    write_atomic(&picture, png)?;
                    self.thumb_autosaved = true;
                    pictured = true;
                }
                None if picture.exists() => {
                    let _ = std::fs::remove_file(&picture);
                    pictured = true;
                }
                _ => {}
            }
        }
        let (modified, name, printer) = (self.modified(), self.model.name.clone(), self.eval.machine.name.clone());
        let saved = self.saved.as_ref().map(plates::content_text).unwrap_or_default();
        if changed || record.modified != modified || record.name != name || record.printer != printer || record.file != self.path || record.saved != saved {
            if changed {
                record.edited = makepad_civil_time::now_secs();
            }
            (record.modified, record.name, record.printer, record.file, record.saved) = (modified, name, printer, self.path.clone(), saved);
            store.write(&record)?;
            pictured = true;
        }
        if pictured {
            self.plates_rev += 1;
        }
        Ok(())
    }

    /// Make `doc` the document on the bed, built. The plate on the bed is autosaved first, so
    /// a switch never loses an edit; one whose working copy cannot be written stays on the bed.
    /// A document that does not build leaves the bed as it was, and so does a cancel of the job
    /// it is built for (`makepad_jobs::current`), even one that comes after its build: the plate
    /// on the bed keeps its build and its colours. `kept`: where `doc` was in [`Self::kept`]
    /// (put aside before): it goes back there when it does not come on the bed.
    fn take(&mut self, mut doc: Doc, kept: Option<usize>) -> Result<Arc<Evaluated>, String> {
        self.autosave().map_err(|e| format!("the plate on the bed could not be kept, so it stays open: {e}"))?;
        let stamp = self.build_stamp();
        // The host's inputs (the boards its case scripts read) are the incoming document's own,
        // from its electronics documents and assets ([`Self::env_for`]), else the bed's; a build
        // made under others is not reused. The boards build only when a script first reads them.
        let env = match &self.env_for {
            Some(env_for) => env_for(doc.model.docs(), doc.model.elec_assets()),
            None => self.model.env().clone(),
        };
        let env_moved = !doc.model.env().same(&env);
        doc.model.set_env(env);
        doc.model.set_steps(self.model.steps().clone(), self.model.logged().clone());
        let reuse = doc.built.filter(|b| b.2 == stamp && !env_moved);
        // the document's own build goes aside with it
        let built = match (&self.compiled, &self.last) {
            (Some(c), Some(ev)) if self.built_head == Some(self.model.head().content) => Some((c.clone(), ev.clone(), stamp)),
            _ => None,
        };
        // the bed's build as it is, for a switch that does not happen
        let was = (self.compiled.clone(), self.last.clone(), self.built_head, self.eval.mapped.clone(), self.eval.inspected());
        let previous = Doc {
            working_id: std::mem::replace(&mut self.working_id, doc.working_id),
            model: std::mem::replace(&mut self.model, doc.model),
            path: std::mem::replace(&mut self.path, doc.path),
            saved: std::mem::replace(&mut self.saved, doc.saved),
            plate: std::mem::replace(&mut self.plate, doc.plate),
            demo: std::mem::replace(&mut self.demo, doc.demo),
            autosaved: std::mem::replace(&mut self.autosaved, doc.autosaved),
            thumb: std::mem::replace(&mut self.thumb, doc.thumb),
            built,
        };
        let shown = match reuse {
            Some((c, mut ev, _)) => {
                if ev.colour_mode != self.eval.colour_mode || ev.printer_linked != self.eval.printer_linked {
                    Arc::make_mut(&mut ev).set_colour_view(self.eval.colour_mode, self.eval.printer_linked);
                }
                // in its own colours, as they were last mapped for it, not the plate's before it:
                // the app maps this plate's next and sends the mapping only when it has moved
                self.eval.mapped = ev.mapped.clone();
                // its display meshes as the view shows them now (a mesh that fails says so and
                // shows as the build made it)
                let ev = match self.eval.shown_at_view(&ev) {
                    Ok(Some(shown)) => Arc::new(shown),
                    Ok(None) => ev,
                    Err(e) => {
                        let mut ev = (*ev).clone();
                        ev.warnings.push(e);
                        Arc::new(ev)
                    }
                };
                self.compiled = Some(c);
                self.last = Some(ev.clone());
                self.built_head = Some(self.model.head().content);
                self.keep_bodies();
                Ok(ev)
            }
            None => self.rebuild(),
        };
        // a cancel that came as it was built still leaves the bed as it was
        let cancelled = makepad_jobs::current().is_some_and(|j| j.is_cancelled());
        let shown = if cancelled { Err("cancelled".to_string()) } else { shown };
        match shown {
            Ok(ev) => {
                self.thumb_autosaved = self.thumb.is_some() && self.plate.is_some();
                self.made_waiting = None;
                self.made_last = None;
                if let Some(store) = &self.store {
                    let current = match (&self.plate, &self.demo) {
                        (Some(id), _) => Some(plates::Current::Plate(id.clone())),
                        (None, Some((key, _))) => Some(plates::Current::Demo(key.clone())),
                        (None, None) => None,
                    };
                    if let Err(e) = store.set_current(current.as_ref()) {
                        eprintln!("fab: which plate is on the bed could not be written: {e}");
                    }
                }
                // the plate (or demo) put aside keeps its history and its build for a switch back
                // (just autosaved, it is its working copy)
                let bed = Doc { working_id: self.working_id.clone(), model: FabModel::new(""), path: None, saved: None, plate: self.plate.clone(), demo: self.demo.clone(), autosaved: autosave::Snapshot::default(), thumb: None, built: None };
                if (previous.plate.is_some() || previous.demo.is_some()) && !previous.same(&bed) {
                    self.kept.retain(|d| !d.same(&previous));
                    self.kept.insert(0, previous);
                    self.kept.truncate(KEPT_PLATES);
                    for d in self.kept.iter_mut().skip(KEPT_BUILT) {
                        d.built = None;
                    }
                }
                if let Some(opened) = self.on_model_open.clone() { opened(self); }
                self.switched += 1;
                self.plates_rev += 1;
                Ok(ev)
            }
            Err(e) => {
                // the document's build, when it was made, goes back aside with it
                let built = match (&self.compiled, &self.last) {
                    (Some(c), Some(ev)) if self.built_head == Some(self.model.head().content) => Some((c.clone(), ev.clone(), stamp)),
                    _ => None,
                };
                let doc = Doc {
                    working_id: std::mem::replace(&mut self.working_id, previous.working_id),
                    model: std::mem::replace(&mut self.model, previous.model),
                    path: std::mem::replace(&mut self.path, previous.path),
                    saved: std::mem::replace(&mut self.saved, previous.saved),
                    plate: std::mem::replace(&mut self.plate, previous.plate),
                    demo: std::mem::replace(&mut self.demo, previous.demo),
                    autosaved: std::mem::replace(&mut self.autosaved, previous.autosaved),
                    thumb: std::mem::replace(&mut self.thumb, previous.thumb),
                    built,
                };
                let (compiled, last, built_head, mapped, inspected) = was;
                (self.compiled, self.last, self.built_head, self.eval.mapped) = (compiled, last, built_head, mapped);
                self.keep_bodies();
                self.eval.put_back(inspected);
                if let Some(at) = kept {
                    self.kept.insert(at.min(self.kept.len()), doc);
                }
                Err(if cancelled { e } else { format!("does not build: {e}") })
            }
        }
    }

    /// Make `model` the document on the bed, built, as a plate: the plate of its file `path`
    /// (the model was just read from it), else a new plate on the list now. The plate it
    /// replaces is autosaved first and stays on the list ([`Self::take`]).
    pub fn replace_model(&mut self, mut model: FabModel, path: Option<PathBuf>) -> Result<Arc<Evaluated>, String> {
        let saved = path.as_ref().map(|_| model.content());
        let (plate, made) = match &self.store {
            Some(store) => match path.as_ref().and_then(|p| store.find_file(p)) {
                Some(plate) => {
                    model.name = plate.name;
                    (Some(plate.id), false)
                }
                None => {
                    if let Some(p) = &path {
                        model.name = store.name_of(p);
                    }
                    (Some(store.create(&model.name, path.clone(), saved.as_ref().map(plates::content_text).unwrap_or_default(), plates::Origin::Plate)?.id), true)
                }
            },
            None => (None, false),
        };
        let r = self.take(Doc { working_id: Default::default(), model, path, saved, plate: plate.clone(), demo: None, autosaved: autosave::Snapshot::default(), thumb: None, built: None }, None);
        // A plate made for a document that does not build is no plate.
        if let (Err(_), Some(store), Some(id), true) = (&r, &self.store, &plate, made) {
            store.forget_new(id);
        }
        r
    }

    /// Show a demo as it ships (`key`): it is no plate until its first edit, which makes the
    /// person's own copy.
    pub fn open_demo(&mut self, model: FabModel, key: &str) -> Result<Arc<Evaluated>, String> {
        // shown before and not edited since (an edit made it a plate): as it was left
        let at = self.kept.iter().position(|d| d.plate.is_none() && d.demo.as_ref().is_some_and(|(k, _)| k == key));
        let doc = at.map(|i| self.kept.remove(i)).unwrap_or_else(|| {
            let head = model.head();
            Doc { working_id: Default::default(), model, path: None, saved: None, plate: None, demo: Some((key.to_string(), head)), autosaved: autosave::Snapshot::default(), thumb: None, built: None }
        });
        self.take(doc, at)
    }

    /// Put plate `id` on the bed as it was left: its working copy, else its file, else empty (a
    /// working copy that does not read back is kept aside first: [`Self::read_back`]). The plate
    /// on the bed is autosaved first. The plate already on the bed stays as it is.
    pub fn switch(&mut self, id: &str) -> Result<Arc<Evaluated>, String> {
        let store = self.store.clone().ok_or("this session keeps no plates")?;
        if self.plate.as_deref() == Some(id) {
            return self.last.clone().map_or_else(|| self.rebuild(), Ok);
        }
        let plate = store.read(id).ok_or_else(|| format!("no plate {id}"))?;
        let saved = plates::content_bytes(&plate.saved);
        let at = self.kept.iter().position(|d| d.plate.as_deref() == Some(id));
        let doc = if let Some(mut doc) = at.map(|i| self.kept.remove(i)) {
            // as it was put aside, with its undo history; its name and file are the record's
            if !plate.name.is_empty() {
                doc.model.name = plate.name.clone();
            }
            doc.path = plate.file.clone();
            doc.saved = saved;
            doc
        } else { match self.read_back(&store, &plate)? {
            Some(autosave::Recovered { mut model, snapshot: autosaved, .. }) => {
                if !plate.name.is_empty() {
                    model.name = plate.name.clone();
                }
                let thumb = std::fs::read(store.picture(id)).ok().map(Arc::from);
                Doc { working_id: Default::default(), model, path: plate.file.clone(), saved, plate: Some(plate.id.clone()), demo: None, autosaved, thumb, built: None }
            }
            None => {
                let (model, saved) = match &plate.file {
                    Some(file) if file.is_file() => {
                        let model = project::load(file, self.model.env())?;
                        let content = model.content();
                        (model, Some(content))
                    }
                    Some(file) => return Err(format!("{} is gone", file.display())),
                    None => (FabModel::new(&plate.name), None),
                };
                Doc { working_id: Default::default(), model, path: plate.file.clone(), saved, plate: Some(plate.id.clone()), demo: None, autosaved: autosave::Snapshot::default(), thumb: None, built: None }
            }
        } };
        let ev = self.take(doc, at)?;
        // opened again, a plate taken off the list is back on it
        if plate.removed {
            let mut plate = plate;
            plate.removed = false;
            store.write(&plate)?;
        }
        Ok(ev)
    }

    /// Plate `plate`'s working copy read back to go on the bed. One that does not read back is
    /// kept aside whole first, so that no autosave prunes or writes over it, and is said in the
    /// log and on its row ([`plates::PlateStore::kept_aside`]): the plate opens as it would with
    /// none. A file changed outside Fab is taken as it reads and said the same way
    /// ([`Self::outside_edits`]).
    fn read_back(&mut self, store: &plates::PlateStore, plate: &plates::Plate) -> Result<Option<autosave::Recovered>, String> {
        let dir = store.dir(&plate.id);
        match autosave::Snapshot::recover(&dir) {
            Ok(recovered) => {
                let changed = recovered.as_ref().map_or(&[][..], |r| &r.changed[..]);
                for line in changed {
                    eprintln!("fab: plate '{}': {line}", plate.name);
                }
                if !changed.is_empty() {
                    self.outside_edits.insert(plate.id.clone(), changed.to_vec());
                    self.plates_rev += 1;
                }
                Ok(recovered)
            }
            Err(why) => {
                let kept = autosave::keep_working_copy_aside(&dir, &why).map_err(|e| format!("its working copy does not read back ({why}), and could not be kept aside: {e}"))?;
                eprintln!("fab: plate '{}': its working copy did not read back ({why}); it is kept as it was in {}", plate.name, kept.display());
                self.plates_rev += 1;
                Ok(None)
            }
        }
    }

    /// `base`, or `base 2`, `base 3`… : a name no listed plate has.
    pub fn fresh_name(&self, base: &str) -> String {
        let taken: Vec<String> = self.plates().into_iter().map(|p| p.name).collect();
        (1..).map(|n| if n == 1 { base.to_string() } else { format!("{base} {n}") }).find(|n| !taken.contains(n)).expect("a free name")
    }

    /// A new, empty plate on the bed, on the list now.
    pub fn new_plate(&mut self, name: &str) -> Result<Arc<Evaluated>, String> {
        self.replace_model(FabModel::new(name), None)
    }

    /// Save the plate: its file (`to`, else where it was opened or last saved, else a folder of
    /// its own in the plates home), its scripts, meshes and picture. The working copy and the
    /// file agree after it. A demo saved becomes a plate.
    pub fn save(&mut self, to: Option<PathBuf>) -> Result<PathBuf, String> {
        let home = self.store.as_ref().map_or_else(|| self.documents.clone(), |s| s.home.clone());
        let path = match to.or_else(|| self.path.clone()) {
            Some(p) => p,
            None => match &self.store {
                Some(store) => store.new_file(&self.model.name),
                None => home.join(format!("{}.{}", safe_stem(&self.model.name), project::EXTENSION)),
            },
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        project::save(&self.model, &path)?;
        if let Some(png) = &self.thumb {
            let _ = write_atomic(&project::thumbnail_path(&path), png);
        }
        self.saved = Some(self.model.content());
        self.path = Some(path.clone());
        if self.store.is_some() && self.plate.is_none() {
            // a demo or a fresh document saved is a plate from now on
            let store = self.store.clone().expect("a store");
            let origin = self.demo.take().map_or(plates::Origin::Plate, |(key, _)| plates::Origin::Demo(key));
            let plate = self.adopt_working_id(&store, Some(path.clone()), origin)?;
            store.set_current(Some(&plates::Current::Plate(plate.id.clone())))?;
            self.plate = Some(plate.id);
        }
        self.autosave()?;
        self.plates_rev += 1;
        Ok(path)
    }

    /// Read the plate's file again (its working copy has nothing the file lacks, or it would be
    /// lost: a plate with edits is saved first).
    pub fn reload(&mut self) -> Result<Arc<Evaluated>, String> {
        let path = self.path.clone().ok_or("the plate has no file yet")?;
        if self.modified() {
            return Err("the plate has edits its file does not: save it first".into());
        }
        let model = project::load(&path, self.model.env())?;
        let saved = Some(model.content());
        self.take(Doc { working_id: Default::default(), model, path: Some(path), saved, plate: self.plate.clone(), demo: None, autosaved: autosave::Snapshot::default(), thumb: None, built: None }, None)
    }

    /// Open what an earlier run had on the bed ([`Self::left_on_bed`], its writes flushed, then
    /// [`Self::reopen`]). None: nothing was on the bed.
    pub fn recover(&mut self) -> Option<Result<Arc<Evaluated>, String>> {
        let (left, unflushed) = self.left_on_bed();
        if let Some(Err(e)) = unflushed.map(plates::Unflushed::finish) {
            eprintln!("fab: the plates of earlier versions are not flushed to the disk, so the next start folds them in again: {e}");
        }
        self.reopen(left?)
    }

    /// What an earlier run had on the bed (None: nothing), the plates of earlier versions folded
    /// into the list first ([`plates::PlateStore::migrate_unflushed`]): files read, nothing
    /// built, so the list can be shown before that plate is; and what the fold wrote, to flush
    /// off the way ([`plates::Unflushed::finish`]).
    pub fn left_on_bed(&self) -> (Option<plates::Current>, Option<plates::Unflushed>) {
        let Some(store) = self.store.clone() else { return (None, None) };
        let fab = store.root.parent().unwrap_or(Path::new(".")).to_path_buf();
        let env = self.model.env().clone();
        // one that does not read back stays where it is, and is not listed
        let crash_copy = |dir: &Path| match autosave::Snapshot::recover(dir) {
            Ok(copy) => copy.map(|r| (r.model.content(), r.model.name.clone(), r.path)),
            Err(e) => {
                eprintln!("fab: the crash copy an earlier version left in {} does not read back, so it is no plate (it stays there): {e}", dir.display());
                None
            }
        };
        // a file read for its content only: one that does not read is listed as such, and says
        // why there, so this read reports nothing
        let file = |path: &Path| makepad_script::inspect_registry::unreported(|| project::load(path, &env)).ok().map(|m| m.content());
        let unflushed = match store.migrate_unflushed(&fab, &crash_copy, &file) {
            Ok(None) => None,
            Ok(Some((made, unflushed))) => {
                if made > 0 {
                    eprintln!("fab: {made} plates of earlier versions are on the plate list");
                }
                Some(unflushed)
            }
            Err(e) => {
                eprintln!("fab: the plates of earlier versions could not all be listed: {e}");
                None
            }
        };
        (store.current(), unflushed)
    }

    /// Put back on the bed what an earlier run had there ([`Self::left_on_bed`]): the plate it
    /// names, or the demo (`demo` builds a demo by key). None: that demo is no more.
    pub fn reopen(&mut self, left: plates::Current) -> Option<Result<Arc<Evaluated>, String>> {
        match left {
            plates::Current::Plate(id) => Some(self.switch(&id).map_err(|e| format!("the plate on the bed when Fab last ran: {e}"))),
            plates::Current::Demo(key) => (self.demos.build)(&key).map(|m| self.open_demo(m, &key)),
        }
    }

    /// Open a project (`.fab.splash`): its plate, as it was left when it is on the list, else a
    /// new plate of the file, used in place. An object script (`.cad.splash`) opens as a new
    /// plate that places it once.
    pub fn open(&mut self, path: &Path) -> Result<Arc<Evaluated>, String> {
        if project::is_script(path) {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let file = path.to_string_lossy().to_string();
            // The paint files it reads, beside it.
            let dir = path.parent().unwrap_or(Path::new("."));
            let files = fab_model::program::paint_files(&text).into_iter().filter_map(|f| std::fs::read_to_string(dir.join(&f)).ok().map(|t| (f, t))).collect();
            let mut strokes = BTreeMap::new();
            project::load_strokes(dir, &text, &mut strokes)?;
            let mut model = FabModel::with_script_files(&project::stem(&file), &file, &text, files, strokes);
            // and the pictures its picture parts print
            let mut pictures = fab_model::picture::Pictures::new();
            fab_model::picture::load(dir, &text, &mut pictures)?;
            model.pictures = Arc::new(pictures);
            return self.replace_model(model, None).map_err(|e| format!("{}: {e}", path.display()));
        }
        if let Some(plate) = self.store.as_ref().and_then(|s| s.find_file(path)) {
            return self.switch(&plate.id).map_err(|e| format!("{}: {e}", path.display()));
        }
        let model = project::load(path, self.model.env())?;
        self.replace_model(model, Some(path.to_path_buf())).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The plates on the list, the most recently edited first.
    pub fn plates(&self) -> Vec<plates::Plate> {
        self.store.as_ref().map(|s| s.list()).unwrap_or_default()
    }

    /// Copy plate `id` as a new plate on the list, with no file until it is saved: its working
    /// copy as it was left (the plate on the bed's autosaved first), else its file.
    pub fn duplicate_plate(&mut self, id: &str) -> Result<plates::Plate, String> {
        let store = self.store.clone().ok_or("this session keeps no plates")?;
        if self.plate.as_deref() == Some(id) {
            self.autosave()?;
        }
        let plate = store.read(id).ok_or_else(|| format!("no plate {id}"))?;
        let env = self.model.env().clone();
        // one whose working copy does not read back is not copied as its file has it
        let model = match autosave::Snapshot::recover(&store.dir(id)).map_err(|e| format!("{}'s working copy does not read back: {e}", plate.name))? {
            Some(r) => Some(r.model),
            None => plate.file.as_ref().map(|f| project::load(f, &env)).transpose()?,
        };
        let mut copy = store.create(&format!("{} copy", plate.name), None, String::new(), plate.origin.clone())?;
        if let Some(mut model) = model {
            model.name = copy.name.clone();
            autosave::Snapshot::default().save(&store.dir(&copy.id), &model, None)?;
            copy.modified = !model.is_empty();
        }
        if let Some(png) = store.picture_of(&plate).and_then(|p| std::fs::read(p).ok()) {
            write_atomic(&store.picture(&copy.id), &png)?;
        }
        copy.printer = plate.printer.clone();
        store.write(&copy)?;
        self.plates_rev += 1;
        Ok(copy)
    }

    /// Name plate `id` `name`: a saved plate's file (and its folder of its own in the plates
    /// home) is renamed with it.
    pub fn rename_plate(&mut self, id: &str, name: &str) -> Result<plates::Plate, String> {
        let store = self.store.clone().ok_or("this session keeps no plates")?;
        let on_bed = self.plate.as_deref() == Some(id);
        if on_bed {
            self.autosave()?;
        }
        let plate = store.rename(id, name)?;
        if on_bed {
            self.model.name = plate.name.clone();
            self.path = plate.file.clone();
            self.autosave()?;
        }
        self.plates_rev += 1;
        Ok(plate)
    }

    /// Take plate `id` off the list; its file and working copy stay. The plate on the bed taken
    /// off gives the bed to the next one on the list (or a new, empty plate): that answer is the
    /// new bed.
    pub fn remove_plate(&mut self, id: &str) -> Result<Option<Arc<Evaluated>>, String> {
        let store = self.store.clone().ok_or("this session keeps no plates")?;
        let on_bed = self.plate.as_deref() == Some(id);
        if on_bed {
            self.autosave()?;
        }
        store.remove(id)?;
        self.plates_rev += 1;
        if on_bed { self.next_on_bed().map(Some) } else { Ok(None) }
    }

    /// Move plate `id`'s files and working copy to the trash (`trash` moves one path): the
    /// person confirmed it. As [`Self::remove_plate`] for the bed.
    pub fn delete_plate(&mut self, id: &str, trash: &dyn Fn(&Path) -> Result<(), String>) -> Result<Option<Arc<Evaluated>>, String> {
        let store = self.store.clone().ok_or("this session keeps no plates")?;
        let on_bed = self.plate.as_deref() == Some(id);
        store.remove(id)?;
        let bed = if on_bed { Some(self.next_on_bed()?) } else { None };
        self.kept.retain(|d| d.plate.as_deref() != Some(id));
        store.delete(id, trash)?;
        self.plates_rev += 1;
        Ok(bed)
    }

    fn next_on_bed(&mut self) -> Result<Arc<Evaluated>, String> {
        match self.plates().first() {
            Some(next) => self.switch(&next.id.clone()),
            None => {
                let name = self.fresh_name("Untitled");
                self.new_plate(&name)
            }
        }
    }

    /// `fab.plates`: the list, and a plate opened, copied, renamed or taken off it.
    fn plates_tool(&mut self, args: &Value) -> Reply {
        if self.store.is_none() {
            return Reply::err("this session keeps no plates");
        }
        let id = str_arg(args, "id").map(str::to_string);
        let need_id = || id.clone().ok_or_else(|| Reply::err("give the plate's 'id' (fab.plates lists them)"));
        let bed = |session: &Self, ev: Option<Arc<Evaluated>>| {
            let update = ev.and_then(|ev| session.compiled.clone().map(|c| (c, ev)));
            Reply { images: Vec::new(), ok: true, text: out::text(&obj(vec![("on_bed", opt(session.plate.as_ref().map(s))), ("name", s(&session.model.name)), ("head", s(session.model.head().text()))])), update, render: None }
        };
        match str_arg(args, "action").unwrap_or("list") {
            "list" => {
                let today = makepad_civil_time::local_or_utc(makepad_civil_time::now_secs()).day_number();
                let rows = self.plates().into_iter().map(|p| obj(vec![
                    ("id", s(&p.id)),
                    ("name", s(&p.name)),
                    ("file", opt(p.file.as_ref().map(|f| s(f.display().to_string())))),
                    ("edited", s(plates::short_when(p.edited, today))),
                    ("printer", s(&p.printer)),
                    ("modified", flag(p.modified)),
                    ("on_bed", flag(self.plate.as_deref() == Some(p.id.as_str()))),
                    ("from", s(match &p.origin { plates::Origin::Plate => String::new(), plates::Origin::Made => "a picture".into(), plates::Origin::Demo(k) => format!("demo {k}") })),
                ]));
                let demos = self.demos.list.iter().map(|(key, title)| obj(vec![("key", s(key)), ("title", s(title))]));
                Reply::ok(obj(vec![("plates", arr(rows)), ("demos", arr(demos)), ("demo_on_bed", opt(self.demo_key().map(s)))]))
            }
            "open" => match (str_arg(args, "demo"), need_id()) {
                (Some(key), _) => match (self.demos.build)(key) {
                    Some(model) => match self.open_demo(model, key) {
                        Ok(ev) => bed(self, Some(ev)),
                        Err(e) => Reply::err(e),
                    },
                    None => Reply::err(format!("no demo '{key}' (fab.plates lists them)")),
                },
                (None, Ok(id)) => match self.switch(&id) {
                    Ok(ev) => bed(self, Some(ev)),
                    Err(e) => Reply::err(e),
                },
                (None, Err(r)) => r,
            },
            "duplicate" => match need_id() {
                Ok(id) => match self.duplicate_plate(&id) {
                    Ok(copy) => Reply::ok(obj(vec![("id", s(&copy.id)), ("name", s(&copy.name))])),
                    Err(e) => Reply::err(e),
                },
                Err(r) => r,
            },
            "rename" => match (need_id(), str_arg(args, "name")) {
                (Ok(id), Some(name)) => match self.rename_plate(&id, name) {
                    Ok(p) => Reply::ok(obj(vec![("id", s(&p.id)), ("name", s(&p.name)), ("file", opt(p.file.as_ref().map(|f| s(f.display().to_string()))))])),
                    Err(e) => Reply::err(e),
                },
                (Err(r), _) => r,
                (_, None) => Reply::err("give the new 'name'"),
            },
            "remove" => match need_id() {
                Ok(id) => match self.remove_plate(&id) {
                    Ok(ev) => bed(self, ev),
                    Err(e) => Reply::err(e),
                },
                Err(r) => r,
            },
            other => Reply::err(format!("'action' must be list, open, duplicate, rename or remove, not {other:?}")),
        }
    }

    /// The viewport's last settled frame (a small PNG): it goes into the plate's working copy;
    /// the newest image-to-mesh result, kept before it had a picture, gets this one.
    pub fn set_thumbnail(&mut self, png: Arc<[u8]>) {
        if let Some(file) = self.made_waiting.take() {
            let _ = write_atomic(&project::thumbnail_path(&file), &png);
        }
        self.thumb = Some(png);
        self.thumb_autosaved = false;
    }

    /// Save a render of the newest made part beside its kept project and return that file.
    pub fn set_made_thumbnail(&mut self, png: &[u8]) -> Option<PathBuf> {
        let file = self.made_last.as_ref()?;
        if let Err(e) = write_atomic(&project::thumbnail_path(file), png) {
            eprintln!("fab: the made part's picture could not be saved: {e}");
            return None;
        }
        if self.made_waiting.as_ref() == Some(file) {
            self.made_waiting = None;
        }
        Some(file.clone())
    }

    fn head_arg(&self, args: &Value) -> Result<Head, Reply> {
        let text = str_arg(args, "expected").ok_or_else(|| Reply::err("missing 'expected': pass the head from fab.inspect"))?;
        if self.model.head().matches(text) {
            Ok(self.model.head())
        } else {
            Err(self.apply_error(ApplyError::StaleHead { current: self.model.head() }))
        }
    }

    /// Apply one batch to the model. Only history restores a build by another path.
    /// `one_shot` selects the refusal wording for tools that cannot also edit the expect.
    fn commit_changes(
        &mut self, request_id: &str, expected: Head, changes: &[Change], cancelled: Option<&dyn Fn() -> bool>, one_shot: bool,
    ) -> Result<(fab_model::Applied, Option<(Compiled, Evaluated)>), ApplyError> {
        self.commit_changes_labeled(request_id, expected, changes, cancelled, one_shot, None)
    }

    fn commit_changes_labeled(
        &mut self, request_id: &str, expected: Head, changes: &[Change], cancelled: Option<&dyn Fn() -> bool>, one_shot: bool, label: Option<String>,
    ) -> Result<(fab_model::Applied, Option<(Compiled, Evaluated)>), ApplyError> {
        let author = self.commit_author();
        let before = self.last.as_deref();
        let expects_gate = |next: &Evaluated| match author {
            Author::Person => None,
            Author::Ai | Author::Agent => fab_model::expect::newly_failing(before, next, one_shot).or_else(|| newly_off_bed(before, next)),
        };
        self.model.apply_gated_labeled(&mut self.eval, request_id, expected, changes, cancelled, &expects_gate, label)
    }

    /// A placement report has no source span. On a refusal, point into the submitted script
    /// or edits when its statement can be identified. Edits have their own line numbering.
    fn placement_source(&self, failure: &mut fab_model::eval::Failure, args: &Value, changes: &[Change]) {
        use makepad_script::syntax::{self, Kind};
        let Some((instance, solid)) = fab_model::program::split_id(&failure.id) else { return };
        let input = str_arg(args, "edits");
        let (file, text) = match (input, changes) {
            (Some(edits), _) => ("edits", edits),
            (None, [Change::Script { file, edit: Edit::Program { source } } | Change::AddInstance { file, source: Some(source), .. }]) => (file.as_str(), source.as_str()),
            _ => return,
        };
        if input.is_none() {
            failure.file = file.to_string();
            if let Some(old) = self.model.source(file) {
                // A replacement of existing source: where its edit starts, including when the
                // changed length came from a parameter on another line than the part itself.
                let at = text.chars().zip(old.chars()).take_while(|(a, b)| a == b).map(|(c, _)| c.len_utf8()).sum();
                (failure.line, failure.col) = makepad_model_script::lang::names::Names::line_col_of(text, at);
                return;
            }
        }
        let Ok(tree) = syntax::parse(text) else { return };
        let object = self.compiled.as_ref().and_then(|c| c.object(&failure.id)).map_or(solid, |(_, o)| o.name.as_str());
        let mut matches = tree.statements().iter().filter(|stmt| {
            if input.is_none() {
                return stmt.let_name() == Some(solid);
            }
            if tree.statements().len() == 1 { return true; }
            let Some(args) = stmt.call("place") else { return false };
            let field = |key| args.iter().find_map(|arg| match &arg.kind {
                Kind::Entry { name, value, .. } if name == key => Some(value.as_ref()),
                _ => None,
            });
            let (base, link) = fab_model::program::split_link(instance);
            let target = args.first().and_then(|arg| arg.string());
            let same_instance = match field("link") {
                None => target == Some(instance),
                Some(value) => target == Some(base) && value.number() == Some(f64::from(link)),
            };
            same_instance && field("object").is_none_or(|arg| arg.string() == Some(object))
        });
        if let (Some(stmt), None) = (matches.next(), matches.next()) {
            failure.file = file.to_string();
            (failure.line, failure.col) = makepad_model_script::lang::names::Names::line_col_of(text, stmt.span.start);
        }
    }

    fn apply_error(&self, e: ApplyError) -> Reply {
        let mut fields = vec![("error", s(e.to_string())), ("head", s(self.model.head().text()))];
        if let ApplyError::Build(f) = &e {
            if !f.file.is_empty() {
                fields.push(("script", s(&f.file)));
            }
            fields.push(("at", s(format!("{}:{}", f.line, f.col))));
            if !f.id.is_empty() {
                fields.push(("solid", s(&f.id)));
            }
            // its statement's selector names nothing on its body any more: what it lost
            if f.lost.is_some() {
                fields.push(("lost", s(fab_model::eval::lost_words(&f.op).1)));
                fields.push(("draft", s(fab_model::eval::lost_what(&f.id, &f.op))));
            }
        }
        Reply { images: Vec::new(), ok: false, text: out::text(&obj(fields)), update: None, render: None }
    }

    fn commit_reply(&mut self, mut fields: Vec<(&str, S)>, built: Option<(Compiled, Evaluated)>) -> Reply {
        // the selector report: what written selections took since the build before
        let built = built.map(|(c, mut ev)| {
            if let Some(before) = &self.last {
                ev.drifted = fab_model::eval::selector_report(&before.taken, &ev.taken);
                ev.warnings.extend(ev.drifted.iter().map(|d| d.line.clone()));
            }
            (c, ev)
        });
        // what it built leads, so the AI reads it first; then what it changed of the person's numbers
        if let Some((c, ev)) = &built {
            fields.insert(fields.len().min(1), ("built", checks::built_value(self.last.as_deref(), ev, false)));
            let changed = self.compiled.as_deref().map(|before| checks::expects_changed(before, c)).unwrap_or_default();
            if !changed.is_empty() {
                fields.insert(fields.len().min(2), ("expects_changed", arr(changed.into_iter().map(s))));
            }
        }
        let update = built.map(|(c, ev)| self.keep(c, ev));
        if let Some(ev) = &self.last {
            fields.push(("plate", plate_value(ev)));
        }
        Reply { images: Vec::new(), ok: true, text: out::text(&obj(fields)), update, render: None }
    }

    /// Where solid `name` is (`instance/solid`, or a solid's id that one instance has): the frame
    /// of the printed part it is built into, as the plate lays it out now, and its model box.
    pub fn locate(&self, name: &str) -> Result<Located, String> {
        let (Some(ev), Some(c)) = (&self.last, &self.compiled) else { return Err("nothing is built yet".into()) };
        let bare = !name.contains('/');
        let found: Vec<&fab_model::eval::ObjectInfo> = ev.objects.iter().filter(|o| o.name == name || (bare && fab_model::program::split_id(&o.name).is_some_and(|(_, s)| s == name))).collect();
        let info = match found.as_slice() {
            [one] => *one,
            [] => return Err(format!("no solid '{name}' (fab.tree lists them)")),
            many => return Err(format!("'{name}' is a solid of several instances: name one ({})", many.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", "))),
        };
        let (inst, i) = c.part(&info.name).ok_or_else(|| format!("no solid '{}'", info.name))?;
        let p = &inst.script.program;
        let mut at = Some(i);
        while let Some(j) = at {
            let id = fab_model::program::part_id(&inst.name, &p.solids[j].id);
            if let Some(k) = ev.plate.parts.iter().position(|part| part.object == id) {
                return Ok(Located { name: info.name.clone(), part: k, to_plate: ev.model_to_plate[k], bounds: info.bounds });
            }
            at = p.solids[j].parent;
        }
        Err(format!("'{}' is in no printed object on the plate", info.name))
    }

    /// `set` sites, and what `remove` and `move` take, written as fab.params keys them
    /// (`instance/site`, `instance/fill#2`) made the script's own (`site`), and the script of
    /// their instance: what the batch edits.
    fn keyed_sites(&self, edits: &mut [Edit]) -> Result<Option<String>, String> {
        let mut file: Option<String> = None;
        for edit in edits.iter_mut() {
            let keys = match edit {
                Edit::Set { site, .. } => vec![site],
                Edit::Remove { solid } => vec![solid],
                Edit::Move { solid, to } => vec![solid, to],
                _ => continue,
            };
            for site in keys {
                let Some((instance, rest)) = fab_model::program::split_id(site) else { continue };
                let found = self.compiled.as_ref().and_then(|c| c.instance(instance));
                let script = found.map(|i| i.script.file.clone()).ok_or_else(|| format!("'{site}': no instance '{instance}' on the plate (fab.params keys are instance/site)"))?;
                if file.as_ref().is_some_and(|f| *f != script) {
                    return Err(format!("'{site}' is of '{script}': one batch edits one script"));
                }
                *site = rest.to_string();
                file = Some(script);
            }
        }
        Ok(file)
    }

    /// `fab.script`'s `edits`, Splash statements editing one script, as the changes they make:
    /// its `set` sites keyed as fab.params keys them name the script, else `script` in `args`
    /// does, else the project's only one. fab.variants reads each variant's the same way.
    pub(crate) fn script_changes(&self, text: &str, args: &Value) -> Result<Vec<Change>, String> {
        let mut parsed = edits::parse(text)?;
        let file = match (self.keyed_sites(&mut parsed)?, str_arg(args, "script")) {
            (Some(k), Some(f)) if k != f => return Err(format!("the sites' instance is of '{k}', not of the script '{f}'")),
            (Some(k), _) => k,
            (None, _) => self.script_arg(args)?,
        };
        if self.model.source(&file).is_none() {
            return Err(format!("no script '{file}' in the project: write it with fab.script"));
        }
        Ok(parsed.into_iter().map(|edit| Change::Script { file: file.clone(), edit }).collect())
    }

    /// The script a tool names (`script`), else the project's only one.
    fn script_arg(&self, args: &Value) -> Result<String, String> {
        match str_arg(args, "script") {
            Some(f) => Ok(f.to_string()),
            None => {
                // object scripts only (paint files are read by them)
                let files: Vec<&String> = self.model.state().scripts.keys().filter(|f| !fab_model::program::is_paint_file(f)).collect();
                match files.as_slice() {
                    [one] => Ok((*one).clone()),
                    [] => Err("the project has no script yet: write one with fab.script".into()),
                    many => Err(format!("name the script: {}", many.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", "))),
                }
            }
        }
    }

    /// Move parts on the plate as one undo step: each `PartMove`'s `delta` (printer mm, Z up;
    /// a turn, a uniform scale and a shift) is taken on the part as it is now and written into
    /// its object's placement in the project (an assembly's bodies move together). The object
    /// drops onto the plate. This is the one edit path for moving objects: the headset's grabs
    /// and the desktop's tools both commit through it.
    pub fn place_parts(&mut self, moves: &[PartMove]) -> Reply {
        let request_id = self.next_request_id("place");
        self.place_parts_request(moves, &request_id, self.model.head(), false, None)
    }

    /// The placement transaction shared by direct moves and tools, including previews.
    fn place_parts_request(&mut self, moves: &[PartMove], request_id: &str, expected: Head, dry_run: bool, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let changes = match self.place_changes(moves, true) {
            Ok(changes) => changes,
            Err(why) => return why,
        };
        let Some(ev) = self.last.clone() else { return Reply::err("the plate is not built yet") };
        if dry_run {
            return match self.model.preview(&mut self.eval, &changes, cancelled) {
                Ok((_, _, next)) => Reply::ok(obj(vec![
                    ("dry_run", flag(true)), ("committed", flag(false)),
                    ("diff", plate_diff(&ev, &next)), ("plate", plate_value(&next)),
                ])),
                Err(e) => self.apply_error(e),
            };
        }
        match self.commit_changes(request_id, expected, &changes, cancelled, false) {
            Ok((applied, built)) => self.commit_reply(vec![
                ("head", s(applied.head.text())), ("replayed", flag(applied.replayed)),
                ("changed", arr(applied.changed.iter().map(s))),
            ], built),
            Err(e) => self.apply_error(e),
        }
    }

    /// Resolve tool-only positions once, on the plate as this batch shapes it. A translation
    /// leaves the offset from the origin to either footprint anchor unchanged.
    fn resolve_place_anchors(&mut self, edits: &mut [Change], cancelled: Option<&dyn Fn() -> bool>) -> Result<Vec<S>, Reply> {
        let anchored = |c: &Change| matches!(c, Change::Place { arg: PlaceArg::Center(_) | PlaceArg::Corner(_), .. });
        if !edits.iter().any(anchored) { return Ok(Vec::new()); }

        let preview = self.built_head != Some(self.model.head().content) || self.last.is_none() || edits.iter().any(|c| match c {
            Change::AddInstance { .. } | Change::RemoveInstance { .. } | Change::Param { .. } => true,
            Change::Place { arg: PlaceArg::Rotate(_) | PlaceArg::Scale(_) | PlaceArg::Show(_), .. } => true,
            Change::Place { instance, .. } if anchored(c) => self.compiled.as_ref().and_then(|c| c.instance(instance)).is_none(),
            _ => false,
        });
        let draft;
        let (compiled, ev) = if preview {
            // As in place_dropped, build the bodies once in their provisional positions. The
            // final placement reuses them. Restore the inspected geometry even if the edit fails.
            let mut provisional = edits.to_vec();
            for change in &mut provisional {
                if anchored(change) {
                    if let Change::Place { arg, .. } = change { *arg = PlaceArg::At(None); }
                }
            }
            let inspected = self.eval.inspected();
            let result = self.model.preview(&mut self.eval, &provisional, cancelled);
            self.eval.put_back(inspected);
            draft = result.map_err(|e| self.apply_error(e))?;
            (&draft.1, &draft.2)
        } else {
            (self.compiled.as_deref().unwrap(), self.last.as_deref().unwrap())
        };

        let parts: HashMap<_, _> = ev.plate.placed_parts().enumerate().map(|(i, p)| (p.object.as_str(), i)).collect();
        let bounds = |i: usize| ev.reports.get(i).map_or_else(|| ev.placed_part(i).bounds(), |r| r.bounds);
        let mut written = Vec::new();
        for change in edits {
            let Change::Place { instance, object, arg } = change else { continue };
            let (key, target) = match *arg {
                PlaceArg::Center(xy) => ("center", xy),
                PlaceArg::Corner(xy) => ("corner", xy),
                _ => continue,
            };
            let inst = compiled.instance(instance).ok_or_else(|| Reply::err(format!("no instance '{instance}'; place it first")))?;
            let emitted = if object.is_empty() {
                match inst.script.objects.as_slice() {
                    [one] => Some(one),
                    _ => inst.script.object(&inst.script_instance),
                }.ok_or_else(|| Reply::err(format!("place: name the object '{instance}' should move")))?
            } else {
                inst.script.object(object).ok_or_else(|| Reply::err(format!("'{instance}' emits no object '{object}'")))?
            };
            if let Some(insert) = &emitted.insert {
                return Err(Reply::err(format!("'{}' rides in '{}'; place its host", emitted.name, insert.into)));
            }
            // One placement moves every body the object emits. Include excluded placements.
            let mut bodies = emitted.bodies.iter().filter_map(|&b| {
                let id = fab_model::program::part_id(instance, &inst.script.program.solids[b].id);
                parts.get(id.as_str()).copied()
            });
            let id = fab_model::program::part_id(instance, &emitted.name);
            let Some(first) = bodies.next() else { return Err(Reply::err(format!("'{id}' has no footprint on the bed; place it first"))) };
            let b = bodies.fold(bounds(first), |b, i| b.union(&bounds(i)));
            let f = ev.placed_frame(first).expect("every placed part has its frame");
            let anchor = if key == "center" { b.center() } else { b.min };
            let at = std::array::from_fn(|k| target[k] + (f[k][3] - anchor[k]));
            *arg = PlaceArg::At(Some(at));
            written.push(obj(vec![("object", s(id)), (key, mm2(target)), ("at", mm2(at))]));
        }
        Ok(written)
    }

    /// A move's changes. `settle`: a tilted single mesh lies down on its nearest face, as a person's
    /// turn does; a turn computed to the degree goes as it's asked.
    pub(crate) fn place_changes(&mut self, moves: &[PartMove], settle: bool) -> Result<Vec<Change>, Reply> {
        if self.last.is_none() || self.compiled.is_none() {
            if let Err(e) = self.rebuild() {
                return Err(Reply::err(e));
            }
        }
        let ev = self.last.clone().unwrap();
        let c = self.compiled.clone().unwrap();
        let mut changes = Vec::new();
        for m in moves {
            let Some(index) = ev.plate.placed_parts().position(|p| p.object == m.object) else {
                return Err(Reply::err(format!("no part '{}' on the plate", m.object)));
            };
            let Some((inst, object)) = c.object(&m.object).map(|(i, o)| (i.name.clone(), o.name.clone())) else {
                return Err(Reply::err(format!("no object for '{}'", m.object)));
            };
            // Every body of the object moves with it.
            let group = ev.placed_handles(index).and_then(|h| h.place.clone());
            let bodies: Vec<(&fab_plate::Part, Option<&fab_model::eval::PartField>)> = ev.plate.placed_parts().enumerate().filter(|(i, _)| ev.placed_handles(*i).is_some_and(|h| h.place.is_some() && h.place == group)).map(|(i, p)| (p, ev.placed_field(i))).collect();
            let d = &m.delta;
            let lin = [[d[0][0], d[0][1], d[0][2]], [d[1][0], d[1][1], d[1][2]], [d[2][0], d[2][1], d[2][2]]];
            let k = (lin[0][0] * lin[0][0] + lin[1][0] * lin[1][0] + lin[2][0] * lin[2][0]).sqrt();
            if !(k.is_finite() && k > 1e-6) {
                return Err(Reply::err("a move's delta must be a turn, a uniform scale and a shift"));
            }
            let mut turn: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| lin[i][j] / k));
            let moved = |q: &[f64; 3]| -> [f64; 3] { std::array::from_fn(|i| d[i][0] * q[0] + d[i][1] * q[1] + d[i][2] * q[2] + d[i][3]) };
            // Where the object's model origin (what its `at` places) goes.
            let Some(f) = ev.placed_frame(index) else { return Err(Reply::err(format!("'{}' has no frame on the plate", m.object))) };
            let mut origin = moved(&[f[0][3], f[1][3], f[2][3]]);
            // A tilted single mesh body lies down on its nearest face: the largest face within
            // 20 degrees of facing the plate is turned to face it exactly, about its centre.
            if settle && turn[2][2] < 1.0 - 1e-6 && bodies.len() == 1 && bodies[0].1.is_none() {
                let part = bodies[0].0;
                if let Some(align) = lay_flat(&part.mesh, &turn) {
                    let mut b = fab_plate::Bounds::EMPTY;
                    for q in &part.mesh.positions {
                        b.add_point(moved(q));
                    }
                    let c0 = b.center();
                    turn = mat3_mul(&align, &turn);
                    let v = [origin[0] - c0[0], origin[1] - c0[1], origin[2] - c0[2]];
                    origin = std::array::from_fn(|i| c0[i] + align[i][0] * v[0] + align[i][1] * v[1] + align[i][2] * v[2]);
                }
            }
            let h = ev.placed_handles(index).cloned().unwrap_or_default();
            let r0 = fab_model::eval::rotation_xyz(h.rotate);
            let e = fab_model::eval::euler_xyz(&mat3_mul(&turn, &r0));
            let round = |v: f64| (v * 1000.0).round() / 1000.0;
            let rotate = if e[0].abs() < 1e-3 && e[1].abs() < 1e-3 { [0.0, 0.0, round(e[2])] } else { e.map(round) };
            // A turn keeps a scale by axis (it scales the part before its turn); a uniform one
            // multiplies each axis.
            let scale = h.scale.map(|s| round(s.max(1e-9) * k)).map(|s| if (s - 1.0).abs() < 1e-6 { 1.0 } else { s });
            let place = |arg: PlaceArg| Change::Place { instance: inst.clone(), object: object.clone(), arg };
            changes.push(place(PlaceArg::At(Some([round(origin[0]), round(origin[1])]))));
            changes.push(place(PlaceArg::Rotate(rotate)));
            changes.push(place(PlaceArg::Scale(scale)));
            changes.push(place(PlaceArg::Drop(true)));
        }
        if changes.is_empty() {
            return Err(Reply::err("nothing to move"));
        }
        Ok(changes)
    }

    fn lay_flat_tool(&mut self, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        let Some(request_id) = str_arg(args, "request_id") else { return Reply::err("missing 'request_id'") };
        let expected = match self.head_arg(args) {
            Ok(h) => h,
            Err(r) => return r,
        };
        if args.get("normal").is_some() == args.get("point").is_some() {
            return Reply::err("give exactly one of 'normal' or 'point'; both or neither is invalid");
        }
        let Some(object) = str_arg(args, "object") else { return Reply::err("missing 'object': the part's solid id or printed name from fab.inspect") };
        let Some(ev) = self.last.clone() else { return Reply::err("the plate is not built yet") };
        // Lay flat also accepts the qualified id of a placement excluded from printing.
        let object = if ev.plate.excluded.iter().any(|p| p.part.object == object) {
            object
        } else {
            match printed_part_index(&ev.plate.parts, object) {
                Ok(i) => &ev.plate.parts[i].object,
                Err(e) => return Reply::err(e),
            }
        };
        let normal = match args.get("normal") {
            Some(value) => match point_arg::<3>(value) {
                Ok(n) => n,
                Err(e) => return Reply::err(format!("'normal': {e}")),
            },
            None => {
                let point = match point_arg::<3>(args.get("point").unwrap()) {
                    Ok(p) => p,
                    Err(e) => return Reply::err(format!("'point': {e}")),
                };
                let found = match self.surface_query(object).and_then(|q| q.nearest_plate(point)) {
                    Ok(f) => f,
                    Err(e) => return Reply::err(e),
                };
                // Surface tools round each plate coordinate to 0.01 mm. The resulting point
                // can be sqrt(3) * 0.005 mm off the surface; accept that rounding.
                if !found.distance.is_finite() || found.distance.abs() > 0.01 {
                    return Reply::err(format!("'point' is not near part '{object}' ({} mm from its surface); use the plate point from fab.surface or fab.pick", found.distance.abs()));
                }
                found.normal
            }
        };
        if normal.iter().all(|x| *x == 0.0) {
            return Reply::err("'normal' is zero; give the face's nonzero outward plate normal from fab.surface or fab.pick");
        }
        let Some(movement) = lay_flat_move(&ev, object, normal) else { return Reply::err("the part has no placed bounds; inspect the plate again") };
        self.place_parts_request(&[movement], request_id, expected, args.get("dry_run").and_then(Value::as_bool) == Some(true), cancelled)
    }

    fn instance_arg(&self, args: &Value) -> Result<String, String> {
        let id = str_arg(args, "object").ok_or("missing 'object': an instance, part or emitted object id")?;
        let c = self.compiled.as_ref().ok_or("the project is not compiled")?;
        c.instance(id).map(|i| i.name.clone()).or_else(|| c.object(id).map(|(i, _)| i.name.clone()))
            .ok_or_else(|| format!("no instance or object '{id}'"))
    }

    fn duplicate_change(&self, args: &Value) -> Result<Change, String> {
        let instance = self.instance_arg(args)?;
        let at = match args.get("at") {
            Some(value) => point_arg::<2>(value)?,
            None => {
                let c = self.compiled.as_ref().ok_or("the project is not compiled")?;
                let ev = self.last.as_ref().ok_or("the plate has not evaluated")?;
                let i = c.instance(&instance).ok_or("no source instance")?;
                let first = i.script.objects.first().ok_or("the source script emits no objects")?;
                let ids: Vec<String> = first.bodies.iter().map(|b| fab_model::program::part_id(&instance, &i.script.program.solids[*b].id)).collect();
                let bounds = ev.plate.placed_parts().filter(|p| ids.contains(&p.object)).map(|p| p.bounds()).reduce(|a, b| a.union(&b));
                let bounds = bounds.ok_or("the first source object is hidden; supply 'at' in plate mm")?;
                let size = bounds.size();
                let colours: Vec<&str> = ev.plate.placed_parts().enumerate().filter(|(_, p)| ids.contains(&p.object)).flat_map(|(k, _)| ev.part_colours(k)).collect();
                let nozzles = ev.nozzles_for(&colours);
                free_spot(ev, [size[0], size[1]], size[2], &nozzles).ok_or_else(|| no_room(&ev.machine, &instance, [size[0], size[1]], &nozzles))?
            }
        };
        match str_arg(args, "mode") {
            Some("linked") => Ok(Change::Clone { instance, at: Some(at) }),
            Some("copy") => Ok(Change::Copy { instance, at: Some(at) }),
            _ => Err("'mode' must be linked or copy".into()),
        }
    }

    /// Answer a tool call. Standalone callers enter as AI. The app's jobs carry their author.
    pub fn call(&mut self, tool: &str, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        match self.author {
            Some(_) => self.dispatch(tool, args, cancelled),
            None => self.as_author(Author::Ai).dispatch(tool, args, cancelled),
        }
    }

    fn dispatch(&mut self, tool: &str, args: &Value, cancelled: Option<&dyn Fn() -> bool>) -> Reply {
        if !matches!(tool, "guide" | "board" | "printer" | "print" | "slice" | "force" | "new" | "save" | "import" | "plates" | "workspace" | "workspace_edit" | "show") && (self.last.is_none() || self.compiled.is_none()) {
            if let Err(e) = self.rebuild() {
                if !matches!(tool, "inspect" | "apply" | "script" | "history") {
                    return Reply::err(e);
                }
            }
        }
        match tool {
            img2mesh::TOOL => Reply::err("fab.image_to_mesh runs in the Fab app, which reaches the AI hub: call it through the app's chat or its agent link"),
            "surface" => self.surface_tool(args),
            "pick" => self.pick_tool(args),
            "annotations" => self.annotations_tool(args),
            "annotate" => self.annotate_tool(args),
            picks::TOOL => self.select_tool(args),
            round::TOOL => self.round_tool(args),
            pull::TOOL => self.pull_tool(args),
            hole::TOOL => self.hole_tool(args),
            shell::TOOL => self.shell_tool(args),
            volumes::TOOL => self.settings_volume_tool(args, cancelled),
            volumes::FIT_TOOL => self.fit_tool(args, cancelled),
            "make_printable" => match str_arg(args, "object") {
                Some(object) => self.make_printable(object, args.clone()),
                None => Reply::err("missing 'object': the imported mesh part's instance/object name"),
            },
            "check" => {
                let ev = match self.slicer_plate() {
                    Ok(ev) => ev,
                    Err(e) => return Reply::err(e),
                };
                let report = fab_printability::analyse_plate(&ev.machine, &ev.plate, &self.eval.materials);
                let mut answer = check_value(&report);
                let sliced = args.get("slice").and_then(Value::as_bool) == Some(true);
                let mut skipped: Vec<(String, String)> = Vec::new();
                // The mesh's own findings: specks, thin features, weak necks (model frame). A test
                // print's are its model's: it has no mesh of its own to read them from.
                let lw = self.eval.materials.line_width;
                if let Some((findings, _)) = (!self.test_mode).then(|| inspect::mesh_findings(&Value::Obj(Vec::new()), &ev, &mut self.eval, lw, false).ok()).flatten() {
                    if checks::has_error(&findings) {
                        checks::lead(&mut answer, checks::Check::Fail.fields());
                    }
                    if let (S::Object(_, fields), false) = (&mut answer, findings.is_empty()) {
                        fields.push(("mesh_findings".into(), arr(findings)));
                    }
                }
                // the person's numbers: a failing one fails the check, one not read is left out
                if let Some(rows) = checks::expects_value(&ev, true) {
                    if ev.expects.iter().any(|j| j.verdict() == fab_model::expect::Verdict::Fail) {
                        checks::lead(&mut answer, checks::Check::Fail.fields());
                    }
                    for j in &ev.expects {
                        if let fab_model::expect::Verdict::NotChecked(why) = j.verdict() {
                            skipped.push((format!("m.expect \"{}\" ({})", j.expect.name, j.instance), why));
                        }
                    }
                    if let S::Object(_, fields) = &mut answer {
                        fields.push(("expects".into(), rows));
                    }
                }
                // what this answer did not check: the slice's checks, and the field parts the
                // mesh findings leave to them
                if !sliced {
                    let fields: Vec<&str> = ev.plate.parts.iter().enumerate().filter(|(i, _)| ev.fields.get(*i).is_some_and(Option::is_some)).map(|(_, p)| p.object.as_str()).collect();
                    if !fields.is_empty() && !self.test_mode {
                        skipped.push((format!("mesh findings of {}", fields.join(", ")), "a field part is checked on its slice: fab.check {slice: true}".into()));
                    }
                    skipped.push(("sliced checks (thin walls, bridges, cantilevers, mid-air, supports, empty layers, flow)".into(), "they read a slice: fab.check {slice: true}".into()));
                }
                if !skipped.is_empty() {
                    if let S::Object(_, fields) = &mut answer {
                        fields.insert(1, ("not_checked".into(), checks::not_checked(&skipped)));
                    }
                }
                Reply { images: Vec::new(), ok: true, text: out::text(&answer), update: None, render: if args.get("slice").and_then(Value::as_bool) == Some(true) { Some(RenderRequest::CheckSlice) } else { None } }
            }
            "weave" => {
                let ev = self.last.clone().unwrap();
                let as_printed = args.get("as_printed").and_then(Value::as_bool) == Some(true);
                // As printed: sliced from the plate's mesh.
                let ev = if as_printed {
                    match self.meshed(ev, cancelled) {
                        Ok(ev) => ev,
                        Err(e) => return Reply::err(e),
                    }
                } else {
                    ev
                };
                Reply::ok(weave::check_value(&ev, &self.eval.profile, self.print_setup.as_ref(), as_printed))
            }
            "joints" | "simulate" => {
                let ev = self.last.clone().unwrap();
                if tool == "simulate" && ev.mech.is_empty() && !ev.weaves.is_empty() {
                    let scene = match weave::scene_of(args.get("scenario").and_then(Value::as_str).unwrap_or("drape")) {
                        Ok(s) => s,
                        Err(e) => return Reply::err(e),
                    };
                    let seconds = match args.get("seconds") {
                        Some(Value::F64(f)) => *f,
                        Some(Value::Int(i)) => *i as f64,
                        _ => 4.0,
                    };
                    let never = || false;
                    return match weave::sim_value(&ev, &self.eval.profile, scene, seconds, cancelled.unwrap_or(&never)) {
                        Ok(v) => Reply::ok(v),
                        Err(e) => Reply::err(e),
                    };
                }
                // The jointed object asked for (its instance), else the first.
                let wanted = str_arg(args, "object").map(|o| o.split('/').next().unwrap_or(o).to_string());
                let mech_state = match &wanted {
                    Some(w) => ev.mech.iter().find(|m| m.instance == *w).cloned(),
                    None => ev.mech.first().cloned(),
                };
                let Some(mech_state) = mech_state else {
                    if let Some(w) = wanted {
                        return Reply::err(format!("'{w}' has no joints"));
                    }
                    return Reply::ok(mech::check_value(None, &[]));
                };
                if cancelled.is_some_and(|c| c()) {
                    return Reply::err("cancelled");
                }
                let checks = self.joint_checks_of(&ev, &mech_state);
                if tool == "joints" {
                    return Reply::ok(mech::check_value(Some(&mech_state), &checks));
                }
                let asm = match mech_state.assembly(&checks, 1.0) {
                    Ok(asm) => asm,
                    Err(f) => return Reply::err(format!("the bends can't be measured: {f}")),
                };
                let slides: Vec<bool> = asm.joints.iter().map(|j| matches!(j.kind, makepad_csg_mech::sim::JointKind::Prismatic { .. })).collect();
                let opts = match mech::options(args, &slides) {
                    Ok(o) => o,
                    Err(e) => return Reply::err(e),
                };
                let never = || false;
                match mech_state.simulate(&asm, &opts, cancelled.unwrap_or(&never)) {
                    Ok(r) => Reply::ok(mech::sim_value(&r)),
                    Err(e) => Reply::err(e),
                }
            }
            "board" | "align" => Reply::err("the plate's board is served by the running Fab app"),
            "elec_read" | "elec_run" | "elec_edit" | ELEC_TEXT_TOOL => Reply::err("the plate's electronics are served by the running Fab app"),
            "printer" => Reply::err("printer telemetry is served by the running Fab app"),
            "slice" => Reply::err("the slice's state is served by the running Fab app"),
            "force" => Reply::err("the force study is solved and served by the running Fab app's Force view"),
            "print" => Reply::err("print requests are served by the running Fab app"),
            "workspace" | "workspace_edit" | "show" => Reply::err("workspaces are the running Fab app's window: this session has none"),
            "guide" => match guide_section(str_arg(args, "section")) {
                Ok(text) => Reply { images: Vec::new(), ok: true, text, update: None, render: None },
                Err(e) => Reply::err(e),
            },
            "settings" => Reply { images: Vec::new(), ok: true, text: settings_text(self.last.as_ref().unwrap(), str_arg(args, "find")), update: None, render: None },
            "colors" => Reply { images: Vec::new(), ok: true, text: colors_text(self.last.as_ref().unwrap()), update: None, render: None },
            "params" => match self.params() {
                Ok(model) => {
                    let (ev, c) = (self.last.clone().unwrap(), self.compiled.clone().unwrap());
                    let only: Vec<PartParams> = match str_arg(args, "part") {
                        Some(o) => {
                            let one: Vec<PartParams> = model.iter().filter(|p| p.object == o).cloned().collect();
                            if one.is_empty() {
                                let parts: Vec<&str> = model.iter().map(|p| p.object.as_str()).collect();
                                return Reply::err(format!("no plate part '{o}' (the plate's parts: {}); solid: \"{o}\" lists the keys of a solid inside one", parts.join(", ")));
                            }
                            one
                        }
                        None => model.to_vec(),
                    };
                    let (solid, prefix) = (str_arg(args, "solid"), str_arg(args, "prefix"));
                    let listing = if solid.is_some() || prefix.is_some() {
                        match params::filtered(&c, &only, solid, prefix) {
                            Ok(found) => params::model_text(&found, &ev, true),
                            Err(e) => return Reply::err(e),
                        }
                    } else {
                        params::summary_text(&only, &ev)
                    };
                    // The fill and stroke statements, which have no name: by number (`toy/fill#2`).
                    let instance = str_arg(args, "part").and_then(|o| fab_model::program::split_id(o).map(|(i, _)| i));
                    let mut paint = String::new();
                    for kind in ["fill", "stroke"] {
                        let listed = params::paint_text(&c, &ev, kind, instance, solid, prefix);
                        if !listed.is_empty() {
                            paint.push_str(&format!(" {kind}s: [\n    {}\n]", listed.join("\n    ")));
                        }
                    }
                    // The print colours, unless one solid's or prefix's keys were asked for.
                    let palette = if solid.is_none() && prefix.is_none() { format!(" palette: {}", out::text(&palette_value(&c, &ev))) } else { String::new() };
                    let text = format!("{{head: {} parts: {listing}{paint}{palette}}}", makepad_script::syntax::write::quote(&self.model.head().text()));
                    Reply { images: Vec::new(), ok: true, text, update: None, render: None }
                }
                Err(e) => Reply::err(e),
            },
            "exact" => {
                let Some(object) = str_arg(args, "object") else { return Reply::err("missing 'object'") };
                match self.eval.object_exact(object) {
                    Ok(Some(b)) => Reply::ok(inspect::exact_value(object, &b)),
                    Ok(None) => Reply::err(format!("'{object}' is not an exact body: only m.brep parts have exact answers; fab.measure measures its mesh")),
                    Err(e) => Reply::err(e),
                }
            }
            "section" if args.get("supports").and_then(Value::as_bool) == Some(true) => match self.support_section(args) {
                Ok((v, images)) => Reply { images, ok: true, text: out::text(&v), update: None, render: None },
                Err(e) => Reply::err(e),
            },
            "section" | "measure" | "check_mesh" | "tree" | "edges" if tool != "measure" || args.get("action").is_none() => {
                let ev = self.last.clone().unwrap();
                let c = self.compiled.clone().unwrap();
                let lw = self.eval.materials.line_width;
                match tool {
                    "section" => match inspect::section(args, &ev, &mut self.eval) {
                        Ok((v, images)) => Reply { images, ok: true, text: out::text(&v), update: None, render: None },
                        Err(e) => Reply::err(e),
                    },
                    "measure" if args.get("what").and_then(Value::as_str) == Some("mass") => self.mass_tool(args, &ev),
                    "measure" => inspect::measure_tool(args, &c, &ev, &mut self.eval).map(Reply::ok).unwrap_or_else(Reply::err),
                    "check_mesh" => inspect::check_mesh(args, &ev, &mut self.eval, lw).map(Reply::ok).unwrap_or_else(Reply::err),
                    "edges" => {
                        let Some(object) = str_arg(args, "object") else { return Reply::err("missing 'object'") };
                        // An exact body's own edges: those are what m.brep selectors pick from.
                        if let Ok(Some(b)) = self.eval.object_exact(object) {
                            return Reply::ok(obj(vec![("solid", s(object)), ("exact", flag(true)), ("edges", inspect::exact_edges(&b))]));
                        }
                        match self.eval.object_mesh(object) {
                            Ok(m) => Reply::ok(obj(vec![("solid", s(object)), ("edges", inspect::edges_value(&m))])),
                            Err(e) => Reply::err(e),
                        }
                    }
                    _ => {
                        let model = self.params().unwrap_or_default();
                        let objects = self.objects(&ev).map(|o| format!("objects: {} ", out::text(&o))).unwrap_or_default();
                        // a test print is a feature of the model it lives on, with no code of its own
                        let tests = out::text(&arr(self.model.project().test_prints.iter().map(|t| obj(vec![("name", s(&t.name)), ("kind", s("test print")), ("object", s(t.part()))]))));
                        Reply { images: Vec::new(), ok: true, text: format!("{{solids: {} features: {} {objects}test_prints: {tests} steps: {} params: {}}}", out::text(&solids_value(&c)), out::text(&volumes::features_value(&c, &ev)), out::text(&steps_value(&c, &ev)), params::model_text(&model, &ev, false)), update: None, render: None }
                    }
                }
            }
            "lay_flat" => self.lay_flat_tool(args, cancelled),
            "inspect" => {
                let mut fields = vec![
                    ("name", s(&self.model.name)),
                    ("head", s(self.model.head().text())),
                    ("can_undo", flag(self.model.can_undo())),
                    ("can_redo", flag(self.model.can_redo())),
                ];
                if let Some(ev) = &self.last {
                    fields.push(("built", checks::built_value(None, ev, true)));
                }
                if let Some(p) = &self.path {
                    fields.push(("path", s(p.display().to_string())));
                }
                fields.push(("project", s(self.model.project_text())));
                let sources = args.get("program").and_then(Value::as_bool) != Some(false);
                fields.push(("scripts", scripts_value(&self.model, sources)));
                if let (Some(c), Some(ev)) = (self.compiled.clone(), self.last.clone()) {
                    fields.push(("volumes", volumes::volumes_value(&c, &ev)));
                    if args.get("read_volumes").and_then(Value::as_bool) == Some(true) {
                        fields.push(("volumes_read", self.volumes_read(&c, &ev, cancelled)));
                    }
                }
                fields.push(("measures", arr(self.model.project().measures.iter().enumerate().map(|(index, m)| obj(vec![
                    ("index", int(index)), ("name", opt(m.name.as_ref().map(s))), ("kind", name(&m.kind)), ("points", arr(m.points.iter().copied().map(mm3))),
                ])))));
                fields.push(("slicing", s(if self.test_mode { "test prints" } else { "whole plate" })));
                if let Some(ev) = &self.last {
                    let project = self.model.project();
                    fields.push(("test_prints", arr(project.test_prints.iter().map(|t| {
                        let placed = ev.test_print_box(t);
                        obj(vec![
                            ("name", s(&t.name)),
                            ("object", s(t.part())),
                            ("on", flag(t.on)),
                            ("at", opt(placed.map(|b| mm3(b.center)))),
                            ("size", opt(placed.map(|b| mm3(b.size)))),
                            ("turn", opt(placed.map(|b| arr(b.turn.iter().copied().map(n))))),
                            ("pose", opt(t.pose.map(|p| arr(p.iter().copied().map(n))))),
                        ])
                    }))));
                    fields.push(("plate", plate_value(ev)));
                    if let Some(c) = &self.compiled {
                        fields.push(("palette", palette_value(c, ev)));
                    }
                    if let Some(objects) = self.objects(ev) {
                        fields.push(("objects", objects));
                    }
                }
                // What it cost: model evaluations and field meshes so far (a field part is
                // meshed only for an export or Mesh shading).
                let (evaluations, meshes) = self.counts();
                fields.push(("evaluations", int(evaluations)));
                fields.push(("field_meshes", int(meshes)));
                Reply::ok(obj(fields))
            }
            "apply" | "script" | "place" | "fill" | "duplicate" | "remove" | "measure" | "test_print" => {
                let Some(request_id) = str_arg(args, "request_id") else { return Reply::err("missing 'request_id'") };
                let expected = match self.head_arg(args) {
                    Ok(h) => h,
                    Err(r) => return r,
                };
                let mut placements = Vec::new();
                let edits = match tool {
                    "duplicate" => match self.duplicate_change(args) {
                        Ok(change) => vec![change],
                        Err(e) => return Reply::err(e),
                    },
                    "remove" => {
                        let instance = match self.instance_arg(args) {
                            Ok(instance) => instance,
                            Err(e) => return Reply::err(e),
                        };
                        vec![match str_arg(args, "scope").unwrap_or("instance") {
                            "instance" => Change::RemoveInstance { instance },
                            "script" => Change::RemoveScript { instance },
                            _ => return Reply::err("'scope' must be instance or script"),
                        }]
                    }
                    "measure" => match measure_change(args) {
                        Ok(change) => vec![change],
                        Err(e) => return Reply::err(e),
                    },
                    "test_print" => {
                        if matches!(str_arg(args, "action"), Some("mode" | "read")) {
                            return Reply::err("the Slicer's choice and its slice are served by the running Fab app");
                        }
                        let (Some(c), Some(ev)) = (self.compiled.clone(), self.last.clone()) else { return Reply::err("the plate is not built yet") };
                        let mut fit = |word: &str, object: Option<&str>| self.feature_box(&c, &ev, word, object);
                        let edits = match str_arg(args, "action") {
                            Some("lay") => self.lay_edit(&ev, args).map(|edit| vec![edit]),
                            _ => test_print_edits(args, &mut fit),
                        };
                        match edits.and_then(|edits| ev.test_print_changes(self.model.project(), &edits)) {
                            Ok(changes) => changes,
                            Err(e) => return Reply::err(e),
                        }
                    }
                    "script" => {
                        let Some(source) = str_arg(args, "source") else { return Reply::err("missing 'source': the object script") };
                        let file = match str_arg(args, "script") {
                            Some(f) => f.to_string(),
                            None if self.model.state().scripts.is_empty() => script_file(&safe_stem(&self.model.name)),
                            None => match self.script_arg(args) {
                                Ok(f) => f,
                                Err(e) => return Reply::err(e),
                            },
                        };
                        if !file.ends_with(&format!(".{}", fab_model::program::SCRIPT_EXTENSION)) {
                            return Reply::err(format!("a script's file ends in .{}", fab_model::program::SCRIPT_EXTENSION));
                        }
                        if self.model.source(&file).is_some() {
                            vec![Change::Script { file, edit: Edit::Program { source: source.to_string() } }]
                        } else {
                            vec![Change::AddInstance { file, source: Some(source.to_string()), name: None, at: None }]
                        }
                    }
                    "fill" => {
                        // One m.fill statement appended to the script: a feature by name, the object,
                        // or a region; paint changes no shape.
                        let seed: Vec<f64> = match args.get("seed") {
                            Some(Value::Arr(a)) => a.iter().filter_map(|v| match v { Value::F64(f) => Some(*f), Value::Int(i) => Some(*i as f64), _ => None }).collect(),
                            _ => Vec::new(),
                        };
                        let flood;
                        let region = if args.get("seed").is_some() {
                            if seed.len() != 3 || seed.iter().any(|v| !v.is_finite()) {
                                return Reply::err("'seed' is a surface point [x y z] in mm");
                            }
                            let angle = match args.get("angle") {
                                None => 30.0,
                                Some(Value::F64(f)) => *f,
                                Some(Value::Int(i)) => *i as f64,
                                Some(_) => return Reply::err("'angle' is in degrees"),
                            };
                            if !(0.0..=180.0).contains(&angle) {
                                return Reply::err("'angle' is 0..180 degrees");
                            }
                            flood = format!("Flood{{seed: {} angle: {}}}", out::text(&out::mm3([seed[0], seed[1], seed[2]])), format!("{}deg", (angle * 100.0).round() / 100.0));
                            flood.as_str()
                        } else {
                            let Some(region) = str_arg(args, "region").map(str::trim).filter(|r| !r.is_empty()) else { return Reply::err("missing 'region': a feature's name (holes), the object's name, or a region (m.box({size: [..] at: [..]})); or a 'seed' point to flood from") };
                            region
                        };
                        let rgb: Vec<f64> = match args.get("albedo") {
                            Some(Value::Arr(a)) => a.iter().filter_map(|v| match v { Value::F64(f) => Some(*f), Value::Int(i) => Some(*i as f64), _ => None }).collect(),
                            _ => Vec::new(),
                        };
                        if rgb.len() != 3 || rgb.iter().any(|v| !(0.0..=1.0).contains(v)) {
                            return Reply::err("'albedo' is [r g b], 0..1 linear");
                        }
                        let name = match args.get("color") {
                            None => String::new(),
                            Some(Value::Str(c)) if !c.trim().is_empty() => format!(" name: {}", out::quote(c.trim())),
                            Some(_) => return Reply::err("'color' is the name of a print colour"),
                        };
                        let file = match self.script_arg(args) {
                            Ok(f) => f,
                            Err(e) => return Reply::err(e),
                        };
                        let Some(source) = self.model.source(&file) else { return Reply::err(format!("no script '{file}' in the project")) };
                        let line = format!("{}m.fill({region}, Material{{albedo: [{} {} {}]{name}}})\n", if source.ends_with('\n') { "" } else { "\n" }, rgb[0], rgb[1], rgb[2]);
                        vec![Change::Script { file, edit: Edit::Splice { range: source.len()..source.len(), text: line } }]
                    }
                    "place" => {
                        let Some(text) = str_arg(args, "edits") else { return Reply::err("missing 'edits': project statements") };
                        match project::changes(text) {
                            Ok(c) if c.is_empty() => return Reply::err("no edits: place(\"vase\", at: [100mm 100mm]), params(\"vase\", {height: 80mm}), script(\"lid.cad.splash\", name: \"lid_2\"), remove(\"lid_2\")"),
                            Ok(mut c) => {
                                placements = match self.resolve_place_anchors(&mut c, cancelled) {
                                    Ok(written) => written,
                                    Err(reply) => return reply,
                                };
                                c
                            }
                            Err(d) => return Reply::err(d.iter().map(|d| format!("edits {d}")).collect::<Vec<_>>().join("; ")),
                        }
                    }
                    _ => {
                        let Some(text) = str_arg(args, "edits") else { return Reply::err("missing 'edits': Splash statements") };
                        match self.script_changes(text, args) {
                            Ok(changes) => changes,
                            Err(e) => return Reply::err(e),
                        }
                    }
                };
                let place_before = (tool == "place" && edits.iter().any(|change| matches!(change, Change::AddInstance { name: None, source: None, .. })))
                    .then(|| self.model.project().clone());
                let add_notes = |fields: &mut Vec<(&str, S)>, project: &project::Project| {
                    if let Some(before) = &place_before {
                        let notes = place_again(before, &edits, project);
                        if !notes.is_empty() { fields.push(("again", arr(notes))); }
                    }
                };
                let before = self.last.clone();
                if args.get("dry_run").and_then(Value::as_bool) == Some(true) {
                    return match self.model.preview(&mut self.eval, &edits, cancelled) {
                        Ok((state, c, ev)) => {
                            let mut fields = vec![("dry_run", flag(true)), ("committed", flag(false)), ("built", checks::built_value(before.as_deref(), &ev, false))];
                            add_notes(&mut fields, &state.project);
                            if !placements.is_empty() { fields.push(("placements", arr(placements))); }
                            let changed = self.compiled.as_deref().map(|was| checks::expects_changed(was, &c)).unwrap_or_default();
                            if !changed.is_empty() {
                                fields.push(("expects_changed", arr(changed.into_iter().map(s))));
                            }
                            if let Some(b) = &before {
                                fields.push(("diff", plate_diff(b, &ev)));
                            }
                            fields.push(("plate", plate_value(&ev)));
                            Reply::ok(obj(fields))
                        }
                        Err(e) => self.apply_error(e),
                    };
                }
                match self.commit_changes(request_id, expected, &edits, cancelled, false) {
                    Ok((applied, built)) => {
                        let mut fields = vec![
                            ("head", s(applied.head.text())),
                            ("replayed", flag(applied.replayed)),
                            ("added", arr(applied.added.iter().map(s))),
                            ("changed", arr(applied.changed.iter().map(s))),
                        ];
                        add_notes(&mut fields, self.model.project());
                        if !placements.is_empty() { fields.push(("placements", arr(placements))); }
                        if let (Some(b), Some((_, e))) = (&before, &built) {
                            fields.push(("diff", plate_diff(b, e)));
                        }
                        self.commit_reply(fields, built)
                    }
                    // the off-bed gate's refusal has no source position yet: find its statement
                    Err(ApplyError::Build(mut f)) if f.op == "place" && f.file.is_empty() => {
                        self.placement_source(&mut f, args, &edits);
                        self.apply_error(ApplyError::Build(f))
                    }
                    Err(e) => self.apply_error(e),
                }
            }
            "history" => {
                let expected = match self.head_arg(args) {
                    Ok(h) => h,
                    Err(r) => return r,
                };
                let action = match str_arg(args, "action") {
                    Some("undo") => HistoryAction::Undo,
                    Some("redo") => HistoryAction::Redo,
                    _ => return Reply::err("'action' must be undo or redo"),
                };
                let undoing_strength = matches!(action, HistoryAction::Undo) && self.strength_head == Some(self.model.head());
                match self.model.history(&mut self.eval, expected, action, cancelled) {
                    Ok((head, c, ev)) => {
                        // what the next list would call before and after is gone with it
                        if undoing_strength {
                            self.strength_head = None;
                            self.strength_before = None;
                            self.orient_was = None;
                            makepad_error_log::flight::mark_ab("fab.strength.undo", "one undo", 0, 0);
                        }
                        self.commit_reply(vec![("head", s(head.text()))], Some((c, ev)))
                    }
                    Err(e) => self.apply_error(e),
                }
            }
            "render" => {
                let Some(ev) = self.last.clone().filter(|e| !e.plate.parts.is_empty()) else {
                    return Reply::err("nothing on the plate to render yet");
                };
                let list = |k: &str| -> Vec<String> {
                    match args.get(k) {
                        Some(Value::Str(s)) => vec![s.clone()],
                        Some(Value::Arr(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
                        _ => Vec::new(),
                    }
                };
                let views = list("views");
                if views.len() > 8 {
                    return Reply::err("at most 8 views per render");
                }
                for v in &views {
                    let custom = v.split_once(',').is_some_and(|(a, b)| a.trim().parse::<f32>().is_ok() && b.trim().parse::<f32>().is_ok());
                    if !custom && !VIEW_NAMES.contains(&v.as_str()) {
                        return Reply::err(format!("unknown view '{v}': {} or \"yaw,pitch\" in degrees", VIEW_NAMES.join(", ")));
                    }
                }
                let objects = match compare::objects_arg(&ev, args) {
                    Ok(objects) => objects,
                    Err(e) => return Reply::err(e),
                };
                let camera = match camera_in_colours(args, self.eval.colour_mode, &|name| self.locate(name), &|n| self.mark_box(n)) {
                    Ok(camera) => camera,
                    Err(e) => return Reply::err(e),
                };
                if let Some(step) = args.get("step") {
                    let Some(id) = step.as_str() else { return Reply::err("'step' takes a step id from fab.tree") };
                    return match self.render_step(&ev, &objects, id, views, camera) {
                        Ok(render) => Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(render) },
                        Err(e) => Reply::err(e),
                    };
                }
                let names = list("test_print");
                let tubes = args.get("tubes").and_then(Value::as_bool) == Some(true);
                if !names.is_empty() || (self.test_mode && objects.is_empty() && !tubes && list("volume").is_empty() && list("spot").is_empty()) {
                    return self.render_pieces(&names, args, views, camera).unwrap_or_else(Reply::err);
                }
                if tubes {
                    return match self.tubes_look(&ev, &objects, views) {
                        Ok(look) => Reply { images: Vec::new(), ok: true, text: look.answer.clone(), update: None, render: Some(RenderRequest::Look(Arc::new(look))) },
                        Err(e) => Reply::err(e),
                    };
                }
                let wants = args.get("supports").and_then(Value::as_bool) == Some(true);
                match supports::render_extra(self, &ev, &objects, wants, &list("volume"), &list("spot")) {
                    Ok(extra) => Reply { images: Vec::new(), ok: true, text: String::new(), update: None, render: Some(RenderRequest::Sheets { views, objects, extra, camera, marks: if args.get("marks").and_then(Value::as_bool) == Some(false) { Vec::new() } else { self.render_marks() } }) },
                    Err(e) => Reply::err(e),
                }
            }
            "overhangs" => self.overhangs_tool(args, cancelled),
            strengthen::FIND_TOOL => self.weak_spots_tool(args, cancelled),
            tubes::TUBES_TOOL => self.tubes_tool(args),
            tubes::ORIENT_TOOL => self.orient_tool(args, cancelled),
            strength_plan::TOOL => self.strength_plan_tool(args, cancelled),
            strengthen::TOOL => self.strengthen_tool(args, cancelled),
            interference::TOOL => self.interference_tool(args, cancelled),
            supports::VIEW_TOOL => self.overhang_view_tool(args, cancelled),
            "supports" => self.supports_tool(args, cancelled),
            compare::TOOL => compare::tool(self, args),
            variants::TOOL => variants::tool(self, args, cancelled),
            "colour_read" => {
                let Some(path) = str_arg(args, "image") else { return Reply::err("give the swatch photo's path: image") };
                let Some(name) = str_arg(args, "name") else { return Reply::err("give the set a name: name (\"Matte CMYW\")") };
                self.read_colour_swatch(Path::new(path), name, str_arg(args, "object"))
            }
            "picture" => {
                let Some(path) = str_arg(args, "image") else { return Reply::err("give a picture file path: image") };
                let width = match args.get("width_mm") {
                    None | Some(Value::Null) => PICTURE_WIDTH_MM,
                    Some(Value::F64(w)) => *w,
                    Some(Value::Int(w)) => *w as f64,
                    Some(_) => return Reply::err("width_mm is a number of mm"),
                };
                self.add_picture(Path::new(path), width, None, str_arg(args, "name"))
            }
            "import" => {
                let Some(path) = args.get("path").and_then(Value::as_str) else { return Reply::err("give a mesh file path") };
                self.import_file(Path::new(path), args.get("new_plate").and_then(Value::as_bool).unwrap_or(false))
            }
            "new" => {
                let name = str_arg(args, "name").map_or_else(|| self.fresh_name("Untitled"), str::to_string);
                match self.replace_model(FabModel::new(&name), None) {
                    Ok(_) => {
                        let update = self.compiled.clone().zip(self.last.clone());
                        Reply { images: Vec::new(), ok: true, text: out::text(&obj(vec![("name", s(&name)), ("head", s(self.model.head().text()))])), update, render: None }
                    }
                    Err(e) => Reply::err(e),
                }
            }
            "save" => {
                let path = match str_arg(args, "path") {
                    Some(p) if project::is_project(Path::new(p)) => Some(PathBuf::from(p)),
                    Some(p) => {
                        let p = PathBuf::from(p);
                        let stem = p.file_name().and_then(|s| s.to_str()).unwrap_or("model").split('.').next().unwrap_or("model").to_string();
                        Some(p.with_file_name(format!("{stem}.{}", project::EXTENSION)))
                    }
                    None => None,
                };
                match self.save(path) {
                    Ok(path) => Reply::ok(obj(vec![("saved", s(path.display().to_string()))])),
                    Err(e) => Reply::err(e),
                }
            }
            "plates" => self.plates_tool(args),
            "export" => {
                let Some(ev) = self.last.clone() else { return Reply::err("nothing to export yet") };
                if ev.plate.parts.is_empty() {
                    return Reply::err("the plate is empty");
                }
                // A part drawn from its distance field has no mesh: the export meshes it (the
                // one place a field part is meshed, besides Mesh shading).
                let ev = match self.meshed(ev, cancelled) {
                    Ok(ev) => ev,
                    Err(e) => return Reply::err(e),
                };
                let format = match args.get("format").and_then(Value::as_str) {
                    Some("stl") => "stl",
                    Some("3mf") => "3mf",
                    _ => return Reply::err("'format' must be stl or 3mf"),
                };
                let path = match args.get("path").and_then(Value::as_str) {
                    Some(p) => PathBuf::from(p).with_extension(format),
                    None => {
                        let dir = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| self.documents.clone());
                        dir.join(format!("{}.{format}", safe_stem(&self.model.name)))
                    }
                };
                let bytes = match format {
                    "stl" => stl::plate_to_stl(&ev.plate),
                    _ => {
                        let mut plate = std::borrow::Cow::Borrowed(&ev.plate);
                        for (i, name) in ev.part_colors.iter().take(ev.plate.parts.len()).enumerate() {
                            let rgb = fab_model::eval::shown_color(&ev.colors, &ev.mapped.inks, name);
                            if plate.parts[i].color[..3] != rgb {
                                plate.to_mut().parts[i].color[..3].copy_from_slice(&rgb);
                            }
                        }
                        match threemf::plate_to_3mf(&plate, &threemf::Info { title: self.model.name.clone(), designer: String::new() }) {
                            Ok(b) => b,
                            Err(e) => return Reply::err(e.to_string()),
                        }
                    }
                };
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match write_atomic(&path, &bytes) {
                    Ok(()) => {
                        let open: usize = ev.plate.parts.iter().map(|p| p.mesh.open_edges()).sum();
                        let mut fields = vec![
                            ("exported", s(path.display().to_string())),
                            ("bytes", int(bytes.len())),
                            ("parts", int(ev.plate.parts.len())),
                            ("triangles", int(ev.plate.triangle_count())),
                        ];
                        if open > 0 {
                            fields.push(("warning", s(format!("{open} open edges: some parts are not closed solids"))));
                        }
                        if ev.blocking_issues() > 0 {
                            fields.push(("placement_issues", int(ev.blocking_issues())));
                        }
                        Reply::ok(obj(fields))
                    }
                    Err(e) => Reply::err(e),
                }
            }
            other => Reply::err(format!("unknown tool '{other}'")),
        }
    }
}

/// The chat and the agents refuse a service whose manifest does not hold, with all its tools.
#[cfg(test)]
#[test]
fn the_manifest_holds() {
    let manifest = manifest();
    manifest.validate().expect("the fab service's manifest");
    for (tool, argument) in [("colour_mode", "mode"), ("render", "colours")] {
        let definition = manifest.tools.iter().find(|t| t.name == tool).unwrap();
        let schema = makepad_model::json::parse(definition.parameters.as_bytes()).unwrap();
        let choices = schema.get("properties").unwrap().get(argument).unwrap().get("enum").unwrap().as_arr().unwrap();
        assert_eq!(choices.iter().filter_map(Value::as_str).collect::<Vec<_>>(), ["model", "material"]);
        assert!(schema.get("required").unwrap().as_arr().unwrap().iter().all(|v| v.as_str() != Some(argument)));
    }
}

#[cfg(test)]
mod annotate_tests;
#[cfg(test)]
mod supports_tests;
#[cfg(test)]
mod plates_tests;
#[cfg(test)]
mod tests;

