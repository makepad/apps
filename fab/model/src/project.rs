//! The saved project: `<name>.fab.splash`, a Splash document of data that places object
//! scripts (`<name>.cad.splash`, see [`crate::program`]) on the plate. It holds no geometry:
//!
//! ```splash
//! use mod.fab.*
//! script("vase.cad.splash", name: "vase", params: {vase_height: 80mm})
//! color("Glaze", rgb: #x2a9d8f, at: [["Lab H2D", @right, "A2"]])
//! place("vase", at: [222mm, 165mm], color: "Glaze")
//! ```
//!
//! A list is written with a comma between its values: with a space alone, a negative value
//! after the first reads as a subtraction (`[10mm -20mm]` is one value).
//!
//! - `script(file, name:, params:)`: an **instance**, the script at `file` (relative to the
//!   project) with its declared parameters set by `params`; `name` (default: the file's stem)
//!   is unique in the project.
//! - `color(name, rgb:, at:, material:)`: a **print colour**, what parts and paint print as. Its
//!   identity is its name (two whites in two materials are two colours). `rgb` is a design
//!   colour set apart from the material (none: it shows its material's colour); `at:
//!   [[printer, side, tray], ...]` is where it prints on each printer it was mapped for (side
//!   @left / @right, or @tool_1 and up on a printer with numbered heads; tray as the printer
//!   labels it: "A2", "HT1", "Ext"). The nozzle follows
//!   from the tray. `material: "TPU"` is the material type it prints in (none: the job's): it
//!   picks a tray of that type, and a filament profile of it where the tray says none.
//! - `place(instance, object:, link:, at: [x, y], rotate:, scale:, drop:, name:, color:, paint:,
//!   print:, show:, settings:, supports:)`: how one emitted object prints (`object` may be left out
//!   when the instance emits one). Its model origin sits at `at` (none: its footprint
//!   centre arranged with the other free objects), turned about the bed's Z by `rotate` (or
//!   [x, y, z]) and scaled about its origin, so an edit of its shape never moves it; it drops
//!   onto the plate unless `drop: false`; `color` is
//!   the print colour it prints as (none: the first), `paint: [[#rrggbb, "name"], ...]` the
//!   print colour each of its script's paint materials prints as, `print: false` shows it
//!   without printing it, `show: false` leaves it off the plate, `hidden: true` keeps it on the
//!   plate but out of the views (a board shown for reference, turned off); `settings` sets its print
//!   modes; `supports: [{on: "tree" tips: [[x, y, z], ...]}]` marks its supports (a tree the
//!   user deleted, by its tips in the part's slice frame). `link: N` (positive integer) places
//!   a linked clone: its parameters belong to the same `script(...)`; omitted `link` is the
//!   original (0).
//! - `settings({key: value})`: the plate's global print settings, on top of the printer's
//!   process profile and under each object's own `settings`.
//!
//! - `measure(points: [[x, y, z], ...], kind: @distance, name: "Gap A")`: a saved ruler
//!   in plate coordinates, with lengths in units; kind defaults to distance. Rulers keep
//!   their order and do not affect geometry or slicing.
//!
//! The meshes scripts import (`m.import("cat")`) live beside the project in
//! `<name>.meshes/` as `cat.mesh` (little-endian: `FMSH`, the vertex count, x y z f32 per
//! vertex, the triangle count, three u32 indices per triangle). Painted leaves live there as
//! `cat.facets` (little-endian: `FFCT`, a u32 leaf count, nine f32 coordinates per leaf, then
//! one u8 state per leaf). Both are in the mesh asset's frame, in mm. Undo history is not saved.

use crate::program::{color_linear, Assets, FacetAssets, ScriptEnv, SCRIPT_EXTENSION, UNIT};
use makepad_model_script::lang::{self, Arg, Diag, Options, RootFn, V};
use makepad_model_script::lang::csg_sdf::mesh_field::{MeshData, MeshField};
use fab_paint_field::Facets;
use makepad_script::syntax::write::quote;
use makepad_core_util::sha256::Sha256;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A project's file extension (`demo.fab.splash`).
pub const EXTENSION: &str = "fab.splash";
pub const MESH_EXTENSION: &str = "mesh";
pub const FACETS_EXTENSION: &str = "facets";
pub const MEMORY_EXTENSION: &str = "eeprom";
const MAGIC: &[u8; 4] = b"FMSH";
const FACETS_MAGIC: &[u8; 4] = b"FFCT";

const ROOTS: &[RootFn] = &[
    RootFn { name: "settings", args: &["values"], model: false },
    RootFn { name: "measure", args: &["points", "kind", "name"], model: false },
    RootFn { name: "test_print", args: &["name", "instance", "object", "link", "at", "size", "turn", "pose", "on", "brim"], model: false },
    RootFn { name: "script", args: &["file", "name", "params"], model: false },
    RootFn { name: "color", args: &["name", "rgb", "at", "material"], model: false },
    RootFn { name: "place", args: &["instance", "object", "link", "at", "rotate", "scale", "drop", "name", "color", "paint", "print", "show", "hidden", "settings", "supports"], model: false },
];

/// A script placed in the project, with its parameter values.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptRef {
    pub name: String,
    pub file: String,
    /// Declared parameters set for this instance: name and value as the script reads it
    /// (lengths in metres, angles in radians).
    pub params: Vec<(String, f64)>,
}

/// How one emitted object prints.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub instance: String,
    pub object: String,
    /// Original placement (0), or a linked clone of the same script instance.
    pub link: u32,
    pub name: Option<String>,
    /// Where its model origin sits on the bed (mm); none: arranged with the free objects.
    pub at: Option<[f64; 2]>,
    /// Degrees about the printer's X, Y, Z.
    pub rotate: [f64; 3],
    /// Factors along the printer's X, Y, Z of the unturned object (`scale: 2` is all three; a
    /// list scales by axis), before its turn.
    pub scale: [f64; 3],
    pub drop: bool,
    /// The print colour it prints as (none: the project's first).
    pub color: Option<String>,
    /// Its script's paint materials (sRGB 0xRRGGBBAA) and the print colours they print as.
    pub paint: Vec<(u32, String)>,
    pub print: bool,
    pub show: bool,
    /// Kept on the plate but out of the views (`hidden: true`).
    pub hidden: bool,
    pub settings: Vec<(String, String)>,
    /// What the user marked on its supports, in the order marked.
    pub supports: Vec<SupportMark>,
}

/// A mark on a placement's supports (`supports: [...]` in `place`).
#[derive(Clone, Debug, PartialEq)]
pub enum SupportMark {
    /// `{on: "tree" tips: [[x, y, z], ...]}`: a tree the user deleted, by its tips (mm, in the
    /// part's slice frame, z its layer's print z): the planner gathers no tip there, and the
    /// trees about it grow as if it never stood.
    NoTree { tips: Vec<[f64; 3]> },
}

impl Placement {
    pub fn new(instance: &str, object: &str) -> Placement {
        Placement { instance: instance.into(), object: object.into(), link: 0, name: None, at: None, rotate: [0.0; 3], scale: [1.0; 3], drop: true, color: None, paint: Vec::new(), print: true, show: true, hidden: false, settings: Vec::new(), supports: Vec::new() }
    }
    /// As the evaluator takes it, for body `solid` of the object.
    pub fn placed(&self, solid: usize) -> crate::program::Placed {
        crate::program::Placed {
            solid,
            object: self.object.clone(),
            name: self.name.clone(),
            at: self.at,
            rotate: self.rotate,
            scale: self.scale,
            drop: self.drop,
            color: self.color.clone(),
            paint: self.paint.iter().map(|(c, name)| {
                let l = color_linear(*c);
                ([l[0], l[1], l[2]], name.clone())
            }).collect(),
            print: self.print,
            show: self.show,
            hidden: self.hidden,
            settings: {
                // The deleted trees' tips reach the planner as the part's own setting.
                let tips: Vec<[f64; 3]> = self.supports.iter().flat_map(|m| match m {
                    SupportMark::NoTree { tips } => tips.iter().copied(),
                }).collect();
                let mut settings = self.settings.clone();
                if !tips.is_empty() {
                    settings.push((makepad_fab_toolpath::DROPPED_TIPS.into(), makepad_fab_toolpath::dropped_text(&tips)));
                }
                settings
            },
        }
    }
}

/// A saved ruler in plate coordinates (millimetres); its index in `measures` is its id.
#[derive(Clone, Debug, PartialEq)]
pub struct Measure {
    pub name: Option<String>,
    pub kind: String,
    pub points: Vec<[f64; 3]>,
}

/// A test print: a box cut out of the plate to print that piece alone. It is the box and its
/// name, no more: no geometry, no settings, for what it prints comes from the plate. It lives in
/// its part's frame, so it moves, turns and scales with the part, and goes with it.
#[derive(Clone, Debug, PartialEq)]
pub struct TestPrint {
    /// What the user calls it; a plate's are unique.
    pub name: String,
    /// The part it lives on: its placement's instance, object and link.
    pub instance: String,
    pub object: String,
    pub link: u32,
    /// Its centre in the part's model frame (mm).
    pub at: [f64; 3],
    /// Its size along its own axes in the part's model frame (mm).
    pub size: [f64; 3],
    /// How it is turned in the part's model frame: degrees about X, then Y, then Z. That changes
    /// what it cuts and nothing else.
    pub turn: [f64; 3],
    /// How the piece lies on the bed: none, as printed, in the print's orientation on the print's
    /// layers; or its own orientation (degrees about the model's X, then Y, then Z) on layers of
    /// its own, which tests that orientation, not the print's.
    pub pose: Option<[f64; 3]>,
    /// Ticked: it shows, slices and prints when the test prints are chosen.
    pub on: bool,
    /// A brim of its own round the piece's first layer, outside it, at the profile's brim width
    /// (Fab's where the profile's brim takes none), whatever brim the profile prints: for a tall
    /// piece on a small footing.
    pub brim: bool,
}

/// One edit of the plate's test prints, in the plate's frame: what the list, the fields, the
/// gizmo and `fab.test_print` all ask for ([`crate::eval::Evaluated::test_print_changes`]).
#[derive(Clone, Debug, PartialEq)]
pub enum TestEdit {
    /// A new box on a part (`instance/object`); with no name, the first free one. Its numbers are
    /// on the plate, or `in_model`, in the part's own frame.
    Add { object: String, center: [f64; 3], size: [f64; 3], turn: [f64; 3], name: Option<String>, on: bool, in_model: bool },
    /// What is given of it; the rest stays. `pose: Some(None)` puts it back as printed.
    Set { name: String, center: Option<[f64; 3]>, size: Option<[f64; 3]>, turn: Option<[f64; 3]>, pose: Option<Option<[f64; 3]>>, on: Option<bool>, brim: Option<bool>, to: Option<String> },
    Remove { name: String },
    /// Its piece laid with `normal` facing down: a plate direction on the piece as its part stands
    /// there, as printed, whatever its pose is now.
    LayFlat { name: String, normal: [f64; 3] },
}

/// The first of `Test 1`, `Test 2`.. not among `taken`.
pub fn next_test_name(taken: &[String]) -> String {
    (1..).map(|n| format!("Test {n}")).find(|n| !taken.contains(n)).unwrap_or_default()
}

/// A test print's box on the plate (mm; degrees about the plate's X, then Y, then Z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedBox {
    pub center: [f64; 3],
    pub size: [f64; 3],
    pub turn: [f64; 3],
}

impl TestPrint {
    /// Its id on the plate: the part it lives on.
    pub fn part(&self) -> String {
        crate::program::part_id(&crate::program::link_name(&self.instance, self.link), &self.object)
    }

    /// Where it stands on the plate, with its part's frame (`model_to_plate`): it moves and turns
    /// with the part as one, and a part's scale along an axis scales the sides along it. It stays
    /// a box: a side at a slant to an uneven scale takes that side's own stretch.
    pub fn placed(&self, frame: &[[f64; 4]; 3]) -> PlacedBox {
        let center = std::array::from_fn(|r| frame[r][0] * self.at[0] + frame[r][1] * self.at[1] + frame[r][2] * self.at[2] + frame[r][3]);
        let own = crate::eval::rotation_xyz(self.turn);
        let size = std::array::from_fn(|c| self.size[c] * stretch(frame, &own, c));
        let turn = mat3_mul(&frame_turn(frame), &own);
        PlacedBox { center, size, turn: crate::eval::euler_xyz(&turn) }
    }

    /// How its piece lies, as the turn on the plate from as printed: none as printed. The pose is
    /// the model's own turn on the bed, so what the part's frame has turned already comes off.
    pub fn laid(&self, frame: &[[f64; 4]; 3]) -> Option<[[f64; 3]; 3]> {
        let part = frame_turn(frame);
        let back = std::array::from_fn(|r| std::array::from_fn(|c| part[c][r]));
        self.pose.map(|pose| mat3_mul(&crate::eval::rotation_xyz(pose), &back))
    }

    /// Lay its piece with unit plate direction `normal` facing down: `normal` as its part stands
    /// on the plate, as printed. A lay that leaves it as printed is none.
    pub fn lay_on(&mut self, normal: [f64; 3], frame: &[[f64; 4]; 3]) {
        let lay = crate::eval::turn_onto(normal, [0.0, 0.0, -1.0]);
        let flat = (0..3).all(|r| (0..3).all(|c| (lay[r][c] - if r == c { 1.0 } else { 0.0 }).abs() < 1e-9));
        self.pose = (!flat).then(|| crate::eval::euler_xyz(&mat3_mul(&lay, &frame_turn(frame))).map(kept));
    }

    /// Put it at `placed` on the plate: the inverse of [`TestPrint::placed`], to what the project
    /// script keeps of a number, so a reload is the same box. A frame that has collapsed (no such
    /// part can be placed) leaves it where it is.
    pub fn set_placed(&mut self, placed: PlacedBox, frame: &[[f64; 4]; 3]) {
        if let Some(at) = model_point(frame, placed.center) {
            let part = frame_turn(frame);
            let back = std::array::from_fn(|r| std::array::from_fn(|c| part[c][r]));
            let own = mat3_mul(&back, &crate::eval::rotation_xyz(placed.turn));
            self.at = at.map(kept);
            self.size = std::array::from_fn(|c| kept(placed.size[c] / stretch(frame, &own, c)));
            self.turn = crate::eval::euler_xyz(&own).map(kept);
        }
    }
}

/// A number as the project script writes it.
fn kept(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// How long a frame makes a unit along column `c` of `own`.
fn stretch(frame: &[[f64; 4]; 3], own: &[[f64; 3]; 3], c: usize) -> f64 {
    let v: [f64; 3] = std::array::from_fn(|r| (0..3).map(|k| frame[r][k] * own[k][c]).sum());
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12)
}

/// A frame's turn: its columns made square and unit, which takes a part's scale out.
fn frame_turn(frame: &[[f64; 4]; 3]) -> [[f64; 3]; 3] {
    let column = |c: usize| -> [f64; 3] { std::array::from_fn(|r| frame[r][c]) };
    let unit = |v: [f64; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
        v.map(|x| x / l)
    };
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let x = unit(column(0));
    let z = unit(cross(x, column(1)));
    let y = cross(z, x);
    std::array::from_fn(|r| [x[r], y[r], z[r]])
}

fn mat3_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum()))
}

/// The model point a frame puts at plate point `p`.
fn model_point(frame: &[[f64; 4]; 3], p: [f64; 3]) -> Option<[f64; 3]> {
    let m = |r: usize, c: usize| frame[r][c];
    let d = [p[0] - m(0, 3), p[1] - m(1, 3), p[2] - m(2, 3)];
    let cross = |a: usize, b: usize| {
        [m(1, a) * m(2, b) - m(2, a) * m(1, b), m(2, a) * m(0, b) - m(0, a) * m(2, b), m(0, a) * m(1, b) - m(1, a) * m(0, b)]
    };
    let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let (yz, zx, xy) = (cross(1, 2), cross(2, 0), cross(0, 1));
    let det = dot([m(0, 0), m(1, 0), m(2, 0)], yz);
    (det.abs() > 1e-12).then(|| [dot(yz, d) / det, dot(zx, d) / det, dot(xy, d) / det])
}

/// A print colour as the project writes it (`color(...)`).
#[derive(Clone, Debug, PartialEq)]
pub struct ColorDef {
    pub name: String,
    /// sRGB 0xRRGGBBAA, set apart from the material's colour; none: the material's.
    pub rgb: Option<u32>,
    pub at: Vec<fab_plate::PrintColorAt>,
    /// The material type it prints in ("TPU"); none: the job's own material.
    pub material: Option<String>,
}

impl ColorDef {
    pub fn new(name: &str) -> ColorDef {
        ColorDef { name: name.into(), rgb: None, at: Vec::new(), material: None }
    }
}

/// The project: its scripts, linked placements and saved rulers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Project {
    pub scripts: Vec<ScriptRef>,
    /// One per emitted object the project places (objects without one are free).
    pub places: Vec<Placement>,
    /// Saved rulers in creation order; they have no evaluation or slicing effect.
    pub measures: Vec<Measure>,
    /// The test prints in creation order; they have no effect on the plate's own print.
    pub test_prints: Vec<TestPrint>,
    /// The plate's global print settings (key, value), sorted by key.
    pub settings: Vec<(String, String)>,
    /// The print colours, in order.
    pub colors: Vec<ColorDef>,
}

impl Project {
    pub fn test_print(&self, name: &str) -> Option<&TestPrint> {
        self.test_prints.iter().find(|t| t.name == name)
    }
    pub fn color(&self, name: &str) -> Option<&ColorDef> {
        self.colors.iter().find(|c| c.name == name)
    }
    pub fn script(&self, instance: &str) -> Option<&ScriptRef> {
        self.scripts.iter().find(|s| s.name == instance)
    }
    pub fn place(&self, instance: &str, object: &str) -> Option<&Placement> {
        self.place_link(instance, object, 0)
    }
    /// The placement of an object, made when it has none (original link 0).
    pub fn place_mut(&mut self, instance: &str, object: &str) -> &mut Placement {
        self.place_link_mut(instance, object, 0)
    }
    /// One object's placement in the original (0) or a linked clone.
    pub fn place_link(&self, instance: &str, object: &str, link: u32) -> Option<&Placement> {
        self.places.iter().find(|p| p.instance == instance && p.object == object && p.link == link)
    }
    /// One link's placement of an object, made with defaults when it has none.
    pub fn place_link_mut(&mut self, instance: &str, object: &str, link: u32) -> &mut Placement {
        match self.places.iter().position(|p| p.instance == instance && p.object == object && p.link == link) {
            Some(i) => &mut self.places[i],
            None => {
                let mut p = Placement::new(instance, object);
                p.link = link;
                self.places.push(p);
                self.places.last_mut().unwrap()
            }
        }
    }
    /// An instance name the project does not use yet, from `base`.
    pub fn fresh_name(&self, base: &str) -> String {
        let base = if base.is_empty() { "object" } else { base };
        if self.script(base).is_none() {
            return base.to_string();
        }
        (2..).map(|i| format!("{base}{i}")).find(|n| self.script(n).is_none()).unwrap()
    }
}

/// A script file's instance name: its file stem (`vase` of `parts/vase.cad.splash`).
pub fn stem(file: &str) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let name = name.strip_suffix(&format!(".{SCRIPT_EXTENSION}")).unwrap_or(name);
    name.split('.').next().unwrap_or(name).to_string()
}

fn diag(loc: Option<(u32, u32)>, message: impl Into<String>) -> Diag {
    let (line, col) = loc.unwrap_or((1, 1));
    Diag { line, col, message: message.into() }
}

/// Read a project. Errors name the line.
pub fn parse(text: &str) -> Result<Project, Vec<Diag>> {
    let assets = Assets::new();
    let program = lang::evaluate(text, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &lang::NO_INPUTS, unit: UNIT, roots: ROOTS, overrides: Default::default(), weave: None, solved: Default::default() })?;
    if !program.solids.is_empty() {
        let s = &program.solids[0];
        return Err(vec![diag(s.loc, "a project (.fab.splash) holds no geometry: model it in an object script (.cad.splash) and place that with script(...)")]);
    }
    let mut project = Project::default();
    let mut errors = Vec::new();
    for root in &program.roots {
        let r = match root.kind {
            "script" => script_ref(root).map(|s| {
                if project.script(&s.name).is_some() {
                    errors.push(diag(root.loc, format!("two scripts are named '{}': give one another name", s.name)));
                } else {
                    project.scripts.push(s);
                }
            }),
            "place" => placement(root).map(|p| {
                if project.place_link(&p.instance, &p.object, p.link).is_some() {
                    errors.push(diag(root.loc, format!("'{}' '{}' link {} is placed twice", p.instance, p.object, p.link)));
                } else {
                    project.places.push(p);
                }
            }),
            "measure" => measure(root).map(|m| project.measures.push(m)),
            "test_print" => test_print(root).map(|t| {
                if project.test_print(&t.name).is_some() {
                    errors.push(diag(root.loc, format!("two test prints are named '{}'", t.name)));
                } else {
                    project.test_prints.push(t);
                }
            }),
            "color" => color_def(root).map(|c| {
                if project.color(&c.name).is_some() {
                    errors.push(diag(root.loc, format!("two colours are named '{}'", c.name)));
                } else {
                    project.colors.push(c);
                }
            }),
            "settings" => setting_values(root, None).map(|v| {
                project.settings.extend(v);
                project.settings.sort_by(|a, b| a.0.cmp(&b.0));
                project.settings.dedup_by(|b, a| {
                    if a.0 == b.0 {
                        a.1 = b.1.clone();
                        true
                    } else {
                        false
                    }
                });
            }),
            _ => Ok(()),
        };
        if let Err(e) = r {
            errors.push(e);
        }
    }
    for p in &project.places {
        for name in p.color.iter().chain(p.paint.iter().map(|(_, n)| n)) {
            if project.color(name).is_none() {
                errors.push(diag(None, format!("place(\"{}\"): no color(\"{name}\") in the project", p.instance)));
            }
        }
        if project.script(&p.instance).is_none() {
            errors.push(diag(None, format!("place(\"{}\"): no script(..., name: \"{}\") in the project", p.instance, p.instance)));
        }
    }
    for t in &project.test_prints {
        if project.script(&t.instance).is_none() {
            errors.push(diag(None, format!("test_print(\"{}\"): no script(..., name: \"{}\") in the project", t.name, t.instance)));
        }
    }
    if errors.is_empty() {
        Ok(project)
    } else {
        Err(errors)
    }
}

/// A plate value, including the height bands saved with calibration coupons.
pub fn checked_plate_value(key: &str, value: &str) -> Result<String, String> {
    if key == "fab_calibration_bands" {
        makepad_fab_gcode::cal_bands::parse(value)?;
        Ok(value.to_string())
    } else {
        fab_plate::checked_value(key, value)
    }
}

/// A `{key: value}` of print settings: under argument `field` of a `place`, or (none) the
/// plate's, the `settings` statement's. An object may set only what can differ per object.
fn setting_values(root: &lang::Root, field: Option<&str>) -> Result<Vec<(String, String)>, Diag> {
    let bad = |m: String| diag(root.loc, m);
    let arg = field.unwrap_or("values");
    let what = field.map_or("settings(...)".to_string(), |f| format!("'{f}'"));
    let Some(V::Obj(o)) = value(root, arg) else { return Err(bad(format!("{what} takes an object: {{key: value}}"))) };
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in &o.fields {
        if field.is_none() && matches!(v, V::Nil) {
            continue;
        }
        if k == "fab_calibration_bands" {
            if field.is_some() { return Err(bad("calibration bands belong to the plate".into())); }
            let V::Str(text) = v else { return Err(bad("calibration bands must be text".into())) };
            let text = checked_plate_value(k, text).map_err(&bad)?;
            out.retain(|(key, _)| key != k);
            out.push((k.clone(), text));
            continue;
        }
        let Some(setting) = fab_plate::print_setting(k) else {
            let keys: Vec<&str> = fab_plate::PRINT_SETTINGS.iter().filter(|s| field.is_none() || s.scope == fab_plate::SettingScope::Object).map(|s| s.key).collect();
            return Err(bad(format!("{what} has '{k}'; the settings are: {}", keys.join(", "))));
        };
        if let (Some(_), fab_plate::SettingScope::Plate(why)) = (field, setting.scope) {
            return Err(bad(format!("'{k}' is the plate's, not an object's: {why} Set it with settings({{{k}: ...}})")));
        }
        let text = setting_text(k, v).map_err(&bad)?;
        out.retain(|(key, _)| key != k);
        out.push((k.clone(), text));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// A setting's value as the slicer reads it: text, a number (in the setting's listed unit, so
/// `15` is 15%) or true or false, checked against the setting ([`fab_plate::checked_value`]).
pub(crate) fn setting_text(k: &str, v: &V) -> Result<String, String> {
    let text = match v {
        V::Str(s) => s.clone(),
        V::Num(n) => format!("{n}"),
        V::Bool(b) => (if *b { "1" } else { "0" }).into(),
        v => return Err(format!("'{k}' must be text, a number or true or false, not {}", v.describe())),
    };
    fab_plate::checked_value(k, &text)
}

fn value<'a>(root: &'a lang::Root, k: &str) -> Option<&'a V> {
    match root.get(k) {
        Some(Arg::Value(v)) => Some(v),
        _ => None,
    }
}

/// A script, paint or strokes file must stay inside the project's folder.
pub fn script_file_name(file: &str) -> Result<(), String> {
    if file.is_empty() {
        return Err("a script file name must not be empty".into());
    }
    if file.contains('\0') {
        return Err("a script file name must not contain NUL".into());
    }
    if file.contains('\\') {
        return Err("a script file name must not contain backslashes; use '/'".into());
    }
    if Path::new(file).has_root() || (file.as_bytes().get(1) == Some(&b':') && file.as_bytes()[0].is_ascii_alphabetic()) {
        return Err("a script file name must be relative, without a root or drive prefix".into());
    }
    for component in Path::new(file).components() {
        match component {
            std::path::Component::ParentDir => return Err("a script file name must not contain '..' components".into()),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return Err("a script file name must be relative, without a root or drive prefix".into()),
            _ => {}
        }
    }
    Ok(())
}

fn script_ref(root: &lang::Root) -> Result<ScriptRef, Diag> {
    let bad = |m: String| diag(root.loc, format!("script: {m}"));
    let file = match value(root, "file") {
        Some(V::Str(s)) => s.clone(),
        _ => return Err(bad(format!("the first argument is the script's file: script(\"vase.{SCRIPT_EXTENSION}\")"))),
    };
    script_file_name(&file).map_err(bad)?;
    let name = match value(root, "name") {
        None => stem(&file),
        Some(V::Str(s)) if !s.is_empty() && !s.contains('/') => s.clone(),
        Some(v) => return Err(bad(format!("'name' is text without '/', not {}", v.describe()))),
    };
    let mut params = Vec::new();
    match value(root, "params") {
        None => {}
        Some(V::Obj(o)) => {
            for (k, v) in &o.fields {
                match v {
                    V::Num(n) if n.is_finite() => params.push((k.clone(), *n)),
                    V::Bool(b) => params.push((k.clone(), if *b { 1.0 } else { 0.0 })),
                    v => return Err(bad(format!("'params.{k}' is a number, not {}", v.describe()))),
                }
            }
        }
        Some(v) => return Err(bad(format!("'params' is an object {{name: value ...}}, not {}", v.describe()))),
    }
    Ok(ScriptRef { name, file, params })
}

fn placement(root: &lang::Root) -> Result<Placement, Diag> {
    let bad = |m: String| diag(root.loc, format!("place: {m}"));
    let instance = match value(root, "instance") {
        Some(V::Str(s)) if !s.is_empty() => s.clone(),
        _ => return Err(bad("the first argument is the script's name: place(\"vase\", at: [100mm 100mm])".into())),
    };
    let num = |k: &str| -> Result<Option<f64>, Diag> {
        match value(root, k) {
            None => Ok(None),
            Some(V::Num(n)) if n.is_finite() => Ok(Some(*n)),
            Some(v) => Err(bad(format!("'{k}' must be a number, not {}", v.describe()))),
        }
    };
    let nums = |k: &str, v: &V, n: usize| -> Result<Vec<f64>, Diag> {
        match v {
            V::Arr(a) if a.len() == n => a.iter().map(|x| x.num().filter(|x| x.is_finite()).ok_or_else(|| bad(format!("'{k}' has a value that is not a number")))).collect(),
            other => Err(bad(format!("'{k}' must be {n} numbers, not {}", other.describe()))),
        }
    };
    let flag = |k: &str, default: bool| -> Result<bool, Diag> {
        match value(root, k) {
            None => Ok(default),
            Some(V::Bool(b)) => Ok(*b),
            Some(v) => Err(bad(format!("'{k}' must be true or false, not {}", v.describe()))),
        }
    };
    let mut p = Placement::new(&instance, "");
    p.object = match value(root, "object") {
        None => String::new(),
        Some(V::Str(s)) => s.clone(),
        Some(v) => return Err(bad(format!("'object' is the emitted object's name, not {}", v.describe()))),
    };
    p.link = match num("link")? {
        None => 0,
        Some(n) if n.fract() == 0.0 && (1.0..=u32::MAX as f64).contains(&n) => n as u32,
        Some(_) => return Err(bad("'link' must be a positive integer; omit it for the original".into())),
    };
    p.at = match value(root, "at") {
        None => None,
        Some(v) => {
            let a = nums("at", v, 2)?;
            Some([a[0] * UNIT, a[1] * UNIT])
        }
    };
    p.rotate = match value(root, "rotate") {
        None => [0.0; 3],
        Some(V::Num(z)) => [0.0, 0.0, z.to_degrees()],
        Some(v) => {
            let a = nums("rotate", v, 3)?;
            [a[0].to_degrees(), a[1].to_degrees(), a[2].to_degrees()]
        }
    };
    p.scale = match value(root, "scale") {
        None => [1.0; 3],
        Some(V::Num(k)) => [*k; 3],
        Some(v) => {
            let a = nums("scale", v, 3)?;
            [a[0], a[1], a[2]]
        }
    };
    if !p.scale.iter().all(|&k| k > 0.0 && k <= 1000.0) {
        return Err(bad("'scale' must be a positive factor, or three: along X, Y and Z".into()));
    }
    p.color = match value(root, "color") {
        None => None,
        Some(V::Str(c)) if !c.is_empty() => Some(c.clone()),
        Some(v) => return Err(bad(format!("'color' is the name of a print colour, color(\"name\"), not {}", v.describe()))),
    };
    p.paint = match value(root, "paint") {
        None => Vec::new(),
        Some(V::Arr(pairs)) => {
            let mut paint = Vec::with_capacity(pairs.len());
            for pair in pairs {
                let V::Arr(pair) = pair else { return Err(bad("'paint' is a list of [#rrggbb \"colour\"] pairs".into())) };
                let [V::Color(rgb), V::Str(name)] = pair.as_slice() else { return Err(bad("'paint' is a list of [#rrggbb \"colour\"] pairs".into())) };
                paint.push((*rgb, name.clone()));
            }
            paint
        }
        Some(v) => return Err(bad(format!("'paint' is a list of [#rrggbb \"colour\"] pairs, not {}", v.describe()))),
    };
    p.name = match value(root, "name") {
        None => None,
        Some(V::Str(s)) => Some(s.clone()),
        Some(v) => return Err(bad(format!("'name' is text, not {}", v.describe()))),
    };
    if value(root, "settings").is_some() {
        p.settings = setting_values(root, Some("settings"))?;
    }
    p.settings.sort_by(|a, b| a.0.cmp(&b.0));
    p.drop = flag("drop", true)?;
    p.print = flag("print", true)?;
    p.show = flag("show", true)?;
    p.hidden = flag("hidden", false)?;
    p.supports = match value(root, "supports") {
        None => Vec::new(),
        Some(V::Arr(marks)) => marks.iter().map(|m| support_mark(m).map_err(bad)).collect::<Result<_, _>>()?,
        Some(v) => return Err(bad(format!("'supports' is a list of marks like {{on: \"tree\" tips: [[x, y, z], ...]}}, not {}", v.describe()))),
    };
    Ok(p)
}

/// One mark of a placement's `supports` list.
fn support_mark(v: &V) -> Result<SupportMark, String> {
    let V::Obj(o) = v else { return Err(format!("a support mark is {{on: \"tree\" tips: [[x, y, z], ...]}}, not {}", v.describe())) };
    let field = |k: &str| o.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v);
    match field("on") {
        Some(V::Str(on)) if on == "tree" => {}
        _ => return Err("a support mark says what it is on: on: \"tree\"".into()),
    }
    let Some(V::Arr(tips)) = field("tips") else { return Err("a tree mark lists its tips: tips: [[x, y, z], ...]".into()) };
    let tips = tips.iter().map(|t| match t {
        V::Arr(a) if a.len() == 3 => {
            let n: Vec<f64> = a.iter().filter_map(|x| x.num().filter(|x| x.is_finite())).collect();
            if n.len() == 3 { Ok([n[0] * UNIT, n[1] * UNIT, n[2] * UNIT]) } else { Err("a tip is three lengths: [x, y, z]".to_string()) }
        }
        other => Err(format!("a tip is three lengths [x, y, z], not {}", other.describe())),
    }).collect::<Result<Vec<_>, _>>()?;
    Ok(SupportMark::NoTree { tips })
}

/// The `supports: [...]` text of a placement's marks.
fn supports_text(marks: &[SupportMark]) -> String {
    let mark = |m: &SupportMark| match m {
        SupportMark::NoTree { tips } => format!("{{on: \"tree\" tips: [{}]}}", tips.iter().map(|t| format!("[{}mm, {}mm, {}mm]", num(t[0]), num(t[1]), num(t[2]))).collect::<Vec<_>>().join(", ")),
    };
    format!("supports: [{}]", marks.iter().map(mark).collect::<Vec<_>>().join(", "))
}

pub(crate) fn color_def(root: &lang::Root) -> Result<ColorDef, Diag> {
    let bad = |m: String| diag(root.loc, format!("color: {m}"));
    let name = match value(root, "name") {
        Some(V::Str(s)) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err(bad("the first argument is the colour's name: color(\"Body\", rgb: #xe2672b)".into())),
    };
    let rgb = match value(root, "rgb") {
        None | Some(V::Nil) => None,
        Some(V::Color(c)) => Some(*c),
        Some(v) => return Err(bad(format!("'rgb' is a colour like #x2a9d8f, not {}", v.describe()))),
    };
    let mut at = Vec::new();
    match value(root, "at") {
        None => {}
        Some(V::Arr(list)) => {
            for e in list {
                let V::Arr(e) = e else { return Err(bad("'at' is a list of [\"printer\" @left \"A1\"]".into())) };
                let [V::Str(printer), V::Id(side) | V::Str(side), V::Str(tray)] = e.as_slice() else { return Err(bad("'at' is a list of [\"printer\" @left \"A1\"]".into())) };
                let numbered = side.strip_prefix("tool_").is_some_and(|n| !n.starts_with('0') && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
                if side != "left" && side != "right" && !numbered {
                    return Err(bad(format!("the side is @left, @right or a numbered head from @tool_1, not @{side}")));
                }
                at.push(fab_plate::PrintColorAt { printer: printer.clone(), side: side.clone(), tray: tray.clone() });
            }
        }
        Some(v) => return Err(bad(format!("'at' is a list of [\"printer\" @left \"A1\"], not {}", v.describe()))),
    }
    let material = match value(root, "material") {
        None | Some(V::Nil) => None,
        Some(V::Str(m)) if !m.trim().is_empty() => Some(m.trim().to_string()),
        Some(v) => return Err(bad(format!("'material' is a material type like \"TPU\", not {}", v.describe()))),
    };
    Ok(ColorDef { name, rgb, at, material })
}

/// The `color(...)` statement of a print colour.
pub fn color_text(c: &ColorDef) -> String {
    let mut args = vec![quote(&c.name)];
    if let Some(rgb) = c.rgb {
        args.push(format!("rgb: {}", hex(rgb)));
    }
    if let Some(m) = &c.material {
        args.push(format!("material: {}", quote(m)));
    }
    if !c.at.is_empty() {
        args.push(format!("at: [{}]", c.at.iter().map(|a| format!("[{}, @{}, {}]", quote(&a.printer), a.side, quote(&a.tray))).collect::<Vec<_>>().join(", ")));
    }
    format!("color({})", args.join(", "))
}

/// Validate saved ruler data before it becomes part of a transaction.
pub(crate) fn validate_measure(m: &Measure) -> Result<(), String> {
    let mut chars = m.kind.chars();
    if !chars.next().is_some_and(|c| c == '_' || c.is_alphabetic()) || !chars.all(|c| c == '_' || c.is_alphanumeric()) {
        return Err("measure: 'kind' must be an identifier".into());
    }
    if m.points.iter().flatten().any(|n| !n.is_finite()) {
        return Err("measure: points must be finite numbers".into());
    }
    Ok(())
}

fn measure(root: &lang::Root) -> Result<Measure, Diag> {
    let bad = |m: &str| diag(root.loc, format!("measure: {m}"));
    let Some(V::Arr(points)) = value(root, "points") else { return Err(bad("'points' must be [[x, y, z], ...]")) };
    let points = points.iter().map(|point| {
        let V::Arr(a) = point else { return Err(bad("each point must be [x, y, z]")) };
        let [V::Num(x), V::Num(y), V::Num(z)] = a.as_slice() else { return Err(bad("each point must be three lengths")) };
        Ok([x * UNIT, y * UNIT, z * UNIT])
    }).collect::<Result<Vec<_>, Diag>>()?;
    let kind = match value(root, "kind") {
        None => "distance".into(),
        Some(V::Id(k) | V::Str(k)) => k.clone(),
        Some(_) => return Err(bad("'kind' must be an identifier such as @distance")),
    };
    let name = match value(root, "name") {
        None => None,
        Some(V::Str(n)) => Some(n.clone()),
        Some(_) => return Err(bad("'name' must be text")),
    };
    let m = Measure { name, kind, points };
    validate_measure(&m).map_err(|m| diag(root.loc, m))?;
    Ok(m)
}

pub(crate) fn validate_test_print(t: &TestPrint) -> Result<(), String> {
    if t.name.trim().is_empty() {
        return Err("test_print: it needs a name".into());
    }
    if !t.at.iter().chain(&t.size).chain(&t.turn).chain(t.pose.iter().flatten()).all(|n| n.is_finite()) {
        return Err(format!("test_print(\"{}\"): its place, size and turn must be finite numbers", t.name));
    }
    if t.size.iter().any(|&n| n <= 0.0) {
        return Err(format!("test_print(\"{}\"): each side must be over 0", t.name));
    }
    Ok(())
}

fn test_print(root: &lang::Root) -> Result<TestPrint, Diag> {
    let bad = |m: String| diag(root.loc, format!("test_print: {m}"));
    let text = |k: &str, says: &str| match value(root, k) {
        Some(V::Str(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(bad(says.to_string())),
    };
    let lengths = |k: &str| -> Result<[f64; 3], Diag> {
        let Some(V::Arr(a)) = value(root, k) else { return Err(bad(format!("'{k}' must be [x, y, z]"))) };
        let [V::Num(x), V::Num(y), V::Num(z)] = a.as_slice() else { return Err(bad(format!("'{k}' must be three lengths"))) };
        Ok([x * UNIT, y * UNIT, z * UNIT])
    };
    let angles = |v: &V, k: &str| -> Result<[f64; 3], Diag> {
        let V::Arr(a) = v else { return Err(bad(format!("'{k}' must be three angles, about X, then Y, then Z"))) };
        let [V::Num(x), V::Num(y), V::Num(z)] = a.as_slice() else { return Err(bad(format!("'{k}' must be three angles, about X, then Y, then Z"))) };
        Ok([kept(x.to_degrees()), kept(y.to_degrees()), kept(z.to_degrees())])
    };
    let t = TestPrint {
        name: text("name", "the first argument is its name: test_print(\"Neck\", instance: \"figure\", object: \"body\", at: [0mm 0mm 40mm], size: [40mm 40mm 40mm])")?,
        instance: text("instance", "'instance' is the script's name its part is placed from")?,
        object: text("object", "'object' is the emitted object's name")?,
        link: match value(root, "link") {
            None => 0,
            Some(V::Num(n)) if n.fract() == 0.0 && (1.0..=u32::MAX as f64).contains(n) => *n as u32,
            Some(_) => return Err(bad("'link' must be a positive integer; omit it for the original".into())),
        },
        at: lengths("at")?,
        size: lengths("size")?,
        turn: match value(root, "turn") {
            None => [0.0; 3],
            Some(V::Num(a)) => [0.0, 0.0, kept(a.to_degrees())],
            Some(v) => angles(v, "turn")?,
        },
        pose: value(root, "pose").map(|v| angles(v, "pose")).transpose()?,
        on: match value(root, "on") {
            None => true,
            Some(V::Bool(b)) => *b,
            Some(_) => return Err(bad("'on' must be true or false".into())),
        },
        brim: match value(root, "brim") {
            None => false,
            Some(V::Bool(b)) => *b,
            Some(_) => return Err(bad("'brim' must be true or false".into())),
        },
    };
    validate_test_print(&t).map_err(|m| diag(root.loc, m))?;
    Ok(t)
}

const EDIT_ROOTS: &[RootFn] = &[
    RootFn { name: "settings", args: &["values"], model: false },
    RootFn { name: "measure", args: &["points", "kind", "name"], model: false },
    RootFn { name: "test_print", args: &["name", "instance", "object", "link", "at", "size", "turn", "pose", "on", "brim"], model: false },
    RootFn { name: "script", args: &["file", "name", "params"], model: false },
    RootFn { name: "color", args: &["name", "rgb", "at", "material"], model: false },
    RootFn { name: "place", args: &["instance", "object", "link", "at", "rotate", "scale", "drop", "name", "color", "paint", "print", "show", "hidden", "settings", "supports"], model: false },
    RootFn { name: "params", args: &["instance", "values"], model: false },
    RootFn { name: "remove", args: &["instance"], model: false },
    RootFn { name: "rename", args: &["color", "to"], model: false },
];

/// Project edits written as Splash statements: `script(file, name:, params:)` places a
/// script (again), `place(instance, object:, at: ...)` sets just the arguments it gives,
/// `params(instance, {name: value})` sets parameter values, `remove(instance)` takes an
/// instance away (including `remove("bracket~2")` for a linked clone). `link: N` on a
/// place edit targets that link; `measure(...)` adds a saved ruler; `rename(color: "Colour 2",
/// to: "Eyes")` renames a print colour and everything that names it.
pub fn changes(text: &str) -> Result<Vec<crate::Change>, Vec<Diag>> {
    use crate::{Change, PlaceArg};
    let assets = Assets::new();
    // The statements read with the project's calls in scope (one import line before them).
    let source = format!("use mod.fab.*\n{text}");
    let program = lang::evaluate(&source, &Options { generated: &Default::default(), assets: &assets, images: &Default::default(), extension: "fab", libraries: &[], inputs: &lang::NO_INPUTS, unit: UNIT, roots: EDIT_ROOTS, overrides: Default::default(), weave: None, solved: Default::default() }).map_err(|d| d.into_iter().map(|d| Diag { line: d.line.saturating_sub(1).max(1), ..d }).collect::<Vec<_>>())?;
    if let Some(s) = program.solids.first() {
        return Err(vec![diag(s.loc, "project edits hold no geometry: edit the object script with fab.apply")]);
    }
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let line = |d: Diag| Diag { line: d.line.saturating_sub(1).max(1), ..d };
    for root in &program.roots {
        let r: Result<(), Diag> = (|| {
            match root.kind {
                "script" => {
                    let s = script_ref(root)?;
                    let name = value(root, "name").map(|_| s.name.clone());
                    out.push(Change::AddInstance { file: s.file.clone(), source: None, name, at: None });
                    let instance = name_after(&s);
                    for (k, v) in s.params {
                        out.push(Change::Param { instance: instance.clone(), name: k, value: Some(v) });
                    }
                }
                "place" => {
                    let p = placement(root)?;
                    for (key, _) in &root.args {
                        let arg = match key.as_str() {
                            "at" => PlaceArg::At(p.at),
                            "rotate" => PlaceArg::Rotate(p.rotate),
                            "scale" => PlaceArg::Scale(p.scale),
                            "drop" => PlaceArg::Drop(p.drop),
                            "name" => PlaceArg::Name(p.name.clone()),
                            "color" => PlaceArg::Color(p.color.clone()),
                            "paint" => PlaceArg::Paint(p.paint.clone()),
                            "print" => PlaceArg::Print(p.print),
                            "show" => PlaceArg::Show(p.show),
                            "hidden" => PlaceArg::Hidden(p.hidden),
                            "settings" => PlaceArg::Settings(p.settings.clone()),
                            "supports" => PlaceArg::Supports(p.supports.clone()),
                            _ => continue,
                        };
                        out.push(Change::Place { instance: crate::program::link_name(&p.instance, p.link), object: p.object.clone(), arg });
                    }
                }
                "measure" => out.push(Change::MeasureAdd { measure: measure(root)? }),
                "test_print" => {
                    let t = test_print(root)?;
                    out.push(Change::TestPrint { name: t.name.clone(), set: Some(t) });
                }
                "color" => {
                    let c = color_def(root)?;
                    out.push(Change::Color { name: c.name.clone(), set: Some(c) });
                }
                "settings" => {
                    // `nil` puts a key back on the profile.
                    let reset = match value(root, "values") {
                        Some(V::Obj(o)) => o.fields.iter().filter(|(_, v)| matches!(v, V::Nil)).map(|(k, _)| k.clone()).collect(),
                        _ => Vec::new(),
                    };
                    out.push(Change::PlateSettings { set: setting_values(root, None)?, reset });
                }
                "params" => {
                    let Some(V::Str(instance)) = value(root, "instance") else { return Err(diag(root.loc, "params(\"vase\", {height: 80mm})")) };
                    let Some(V::Obj(o)) = value(root, "values") else { return Err(diag(root.loc, "params takes {name: value ...}")) };
                    for (k, v) in &o.fields {
                        let v = match v {
                            V::Num(n) if n.is_finite() => Some(*n),
                            V::Nil => None,
                            V::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                            v => return Err(diag(root.loc, format!("'{k}' is a number, not {}", v.describe()))),
                        };
                        out.push(Change::Param { instance: instance.clone(), name: k.clone(), value: v });
                    }
                }
                "remove" => {
                    let Some(V::Str(instance)) = value(root, "instance") else { return Err(diag(root.loc, "remove(\"vase\")")) };
                    out.push(Change::RemoveInstance { instance: instance.clone() });
                }
                "rename" => {
                    let (Some(V::Str(from)), Some(V::Str(to))) = (value(root, "color"), value(root, "to")) else {
                        return Err(diag(root.loc, "rename(color: \"Colour 2\", to: \"Eyes\"): a print colour's name and its new one"));
                    };
                    out.push(Change::RenameColor { from: from.clone(), to: to.clone() });
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = r {
            errors.push(line(e));
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// The instance name a `script(...)` edit gets (when the project has it free).
fn name_after(s: &ScriptRef) -> String {
    s.name.clone()
}

fn num(x: f64) -> String {
    let r = (x * 10_000.0).round() / 10_000.0;
    let s = format!("{r}");
    if s == "-0" { "0".into() } else { s }
}

fn hex(c: u32) -> String {
    if c & 0xff == 0xff {
        format!("#x{:06x}", c >> 8)
    } else {
        format!("#x{c:08x}")
    }
}

/// How a parameter's value is written: the unit its script writes it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Mm,
    Degrees,
    Plain,
}

fn settings_text(settings: &[(String, String)]) -> String {
    format!("{{{}}}", settings.iter().map(|(k, v)| format!("{k}: {}", quote(v))).collect::<Vec<_>>().join(" "))
}

/// Write a project. `unit` says how an instance's parameter is written (its script's literal).
pub fn write(project: &Project, unit: &dyn Fn(&str, &str) -> Unit) -> String {
    let mut out = String::from("// A Fab project: object scripts placed on the plate.\nuse mod.fab.*\n\n");
    if !project.settings.is_empty() {
        out.push_str(&format!("settings({})\n\n", settings_text(&project.settings)));
    }
    for c in &project.colors {
        out.push_str(&color_text(c));
        out.push('\n');
    }
    if !project.colors.is_empty() {
        out.push('\n');
    }
    for s in &project.scripts {
        out.push_str(&format!("script({}", quote(&s.file)));
        if s.name != stem(&s.file) {
            out.push_str(&format!(", name: {}", quote(&s.name)));
        }
        if !s.params.is_empty() {
            let fields: Vec<String> = s
                .params
                .iter()
                .map(|(k, v)| match unit(&s.name, k) {
                    Unit::Mm => format!("{k}: {}mm", num(v * UNIT)),
                    Unit::Degrees => format!("{k}: {}°", num(v.to_degrees())),
                    Unit::Plain => format!("{k}: {}", num(*v)),
                })
                .collect();
            out.push_str(&format!(", params: {{{}}}", fields.join(" ")));
        }
        out.push_str(")\n");
    }
    if !project.places.is_empty() {
        out.push('\n');
    }
    for p in &project.places {
        let mut args = vec![quote(&p.instance)];
        // Keep explicit object names so parse(write(project)) preserves the placement keys.
        if !p.object.is_empty() {
            args.push(format!("object: {}", quote(&p.object)));
        }
        if p.link > 0 {
            args.push(format!("link: {}", p.link));
        }
        if let Some(at) = p.at {
            args.push(format!("at: [{}mm, {}mm]", num(at[0]), num(at[1])));
        }
        if p.rotate != [0.0; 3] {
            if p.rotate[0] == 0.0 && p.rotate[1] == 0.0 {
                args.push(format!("rotate: {}°", num(p.rotate[2])));
            } else {
                args.push(format!("rotate: [{}°, {}°, {}°]", num(p.rotate[0]), num(p.rotate[1]), num(p.rotate[2])));
            }
        }
        if p.scale[1] == p.scale[0] && p.scale[2] == p.scale[0] {
            if p.scale[0] != 1.0 {
                args.push(format!("scale: {}", num(p.scale[0])));
            }
        } else {
            args.push(format!("scale: [{}, {}, {}]", num(p.scale[0]), num(p.scale[1]), num(p.scale[2])));
        }
        if !p.drop {
            args.push("drop: false".into());
        }
        if let Some(n) = &p.name {
            args.push(format!("name: {}", quote(n)));
        }
        if let Some(c) = &p.color {
            args.push(format!("color: {}", quote(c)));
        }
        if !p.print {
            args.push("print: false".into());
        }
        if !p.show {
            args.push("show: false".into());
        }
        if p.hidden {
            args.push("hidden: true".into());
        }
        if !p.paint.is_empty() {
            args.push(format!("paint: [{}]", p.paint.iter().map(|(c, n)| format!("[{}, {}]", hex(*c), quote(n))).collect::<Vec<_>>().join(", ")));
        }
        if !p.settings.is_empty() {
            args.push(format!("settings: {}", settings_text(&p.settings)));
        }
        if !p.supports.is_empty() {
            args.push(supports_text(&p.supports));
        }
        out.push_str(&format!("place({})\n", args.join(", ")));
    }
    for m in &project.measures {
        let points = m.points.iter().map(|p| format!("[{}mm, {}mm, {}mm]", num(p[0]), num(p[1]), num(p[2]))).collect::<Vec<_>>().join(", ");
        out.push_str(&format!("measure(points: [{points}], kind: @{}", m.kind));
        if let Some(name) = &m.name {
            out.push_str(&format!(", name: {}", quote(name)));
        }
        out.push_str(")\n");
    }
    for t in &project.test_prints {
        let three = |v: [f64; 3]| format!("[{}mm, {}mm, {}mm]", num(v[0]), num(v[1]), num(v[2]));
        out.push_str(&format!("test_print({}, instance: {}, object: {}", quote(&t.name), quote(&t.instance), quote(&t.object)));
        if t.link > 0 {
            out.push_str(&format!(", link: {}", t.link));
        }
        out.push_str(&format!(", at: {}, size: {}", three(t.at), three(t.size)));
        if num(t.turn[0]) != "0" || num(t.turn[1]) != "0" {
            out.push_str(&format!(", turn: [{}°, {}°, {}°]", num(t.turn[0]), num(t.turn[1]), num(t.turn[2])));
        } else if num(t.turn[2]) != "0" {
            out.push_str(&format!(", turn: {}°", num(t.turn[2])));
        }
        if let Some(p) = t.pose {
            out.push_str(&format!(", pose: [{}°, {}°, {}°]", num(p[0]), num(p[1]), num(p[2])));
        }
        if !t.on {
            out.push_str(", on: false");
        }
        if t.brim {
            out.push_str(", brim: true");
        }
        out.push_str(")\n");
    }
    out
}

/// The strokes files a script reads, from `dir` (one missing: its layer paints nothing until
/// the brush makes it).
pub fn load_strokes(dir: &Path, source: &str, out: &mut BTreeMap<String, Arc<fab_paint_field::Strokes>>) -> Result<(), String> {
    for f in crate::program::strokes_files(source) {
        script_file_name(&f)?;
        if out.contains_key(&f) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(dir.join(&f)) {
            let log = fab_paint_field::Strokes::from_bytes(&bytes).map_err(|e| format!("{}: {e}", dir.join(&f).display()))?;
            out.insert(f, Arc::new(log));
        }
    }
    Ok(())
}

/// The scripts a project references, read from beside it: file name -> source, and the strokes
/// files they read.
#[allow(clippy::type_complexity)]
pub fn load_scripts(project_path: &Path, project: &Project) -> Result<(BTreeMap<String, String>, BTreeMap<String, Arc<fab_paint_field::Strokes>>), String> {
    let dir = project_path.parent().unwrap_or(Path::new("."));
    let mut out = BTreeMap::new();
    let mut strokes = BTreeMap::new();
    for s in &project.scripts {
        script_file_name(&s.file)?;
        if out.contains_key(&s.file) {
            continue;
        }
        let path = dir.join(&s.file);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // The paint files it reads, beside it (one missing: its m.paint_file says so).
        for f in crate::program::paint_files(&text) {
            script_file_name(&f)?;
            if !out.contains_key(&f) {
                if let Ok(t) = std::fs::read_to_string(dir.join(&f)) {
                    out.insert(f, t);
                }
            }
        }
        load_strokes(dir, &text, &mut strokes)?;
        out.insert(s.file.clone(), text);
    }
    Ok((out, strokes))
}

/// Open a project file: its text, the scripts it references and the paint files they read
/// (beside it), its meshes and their painted leaves.
pub fn load(path: &Path, env: &ScriptEnv) -> Result<crate::FabModel, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let project = parse(&text).map_err(|d| d.iter().map(|d| format!("{}:{d}", path.display())).collect::<Vec<_>>().join("\n"))?;
    let (scripts, strokes) = load_scripts(path, &project)?;
    let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("model");
    let name = file.strip_suffix(&format!(".{EXTENSION}")).unwrap_or(file);
    let (assets, facets) = load_meshes(path)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let shapes = load_grown(dir, &scripts);
    let mut pictures = crate::picture::Pictures::new();
    for text in scripts.values() {
        crate::picture::load(dir, text, &mut pictures)?;
    }
    let (docs, elec_assets, memory) = load_elec(path)?;
    let mut model = crate::FabModel::load_with_assets(name, &text, scripts, strokes, Arc::new(assets), Arc::new(facets), Arc::new(pictures), env.clone()).map_err(|f| f.to_string())?;
    if !docs.is_empty() {
        model.set_docs(Arc::new(docs));
    }
    if !elec_assets.is_empty() {
        model.set_elec_assets(Arc::new(elec_assets));
    }
    if !memory.is_empty() {
        model.set_elec_memory(Arc::new(memory));
    }
    for (file, part, stored, grid) in shapes {
        model.set_grown(&file, &part, Some(Arc::new(grid)));
        model.set_stored(&file, &part, Some(Arc::new(stored)));
    }
    Ok(model)
}

/// The grown shapes kept beside the scripts. One that does not read is as none: its part grows.
fn load_grown(dir: &Path, scripts: &BTreeMap<String, String>) -> Vec<(String, String, crate::grown::Stored, makepad_csg_sdf::SdfGrid)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let names: Vec<String> = entries.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".grow")).collect();
    for file in scripts.keys() {
        for name in &names {
            let Some(part) = crate::grown::part_of(file, name) else { continue };
            if let Some((stored, grid)) = std::fs::read(dir.join(name)).ok().and_then(|bytes| crate::grown::Stored::read(&bytes).ok()) {
                out.push((file.clone(), part, stored, grid));
            }
        }
    }
    out
}

/// Write a file so a crash mid-write never leaves it half written: a temp
/// file beside it, flushed, renamed over it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_renamed(path, bytes, true)
}

/// [`write_atomic`] with the flush left for later ([`flush`]): the file reads back at once, from
/// the disk's cache, and no write waits on a slow disk; a crash before its flush may leave it
/// empty, so what says it is written waits for the flush.
pub fn write_unflushed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_renamed(path, bytes, false)
}

/// A file [`write_unflushed`] wrote, flushed to the disk.
pub fn flush(path: &Path) -> Result<(), String> {
    std::fs::File::open(path).and_then(|f| f.sync_all()).map_err(|e| format!("{}: {e}", path.display()))
}

/// A temp file beside `path`, flushed when `flush` says so, renamed over it.
fn write_renamed(path: &Path, bytes: &[u8], flush: bool) -> Result<(), String> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(bytes).and_then(|()| if flush { file.sync_all() } else { Ok(()) }).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Save a project: its text, scripts, meshes, painted leaves, and electronics documents and assets.
pub fn save(model: &crate::FabModel, path: &Path) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    for file in model.state().scripts.keys().chain(model.state().strokes.keys()) {
        script_file_name(file)?;
    }
    write_atomic(path, model.project_text().as_bytes())?;
    for (file, source) in &model.state().scripts {
        let p = dir.join(file);
        write_atomic(&p, source.as_bytes())?;
    }
    for (file, log) in model.state().strokes.iter() {
        let p = dir.join(file);
        write_atomic(&p, &log.to_bytes())?;
    }
    for (file, part, stored) in model.all_stored() {
        write_atomic(&dir.join(crate::grown::file_name(file, part)), &stored.bytes)?;
    }
    // the pictures its scripts print, beside them (one already there as it is: kept)
    for (file, picture) in model.pictures.iter() {
        script_file_name(file)?;
        let p = dir.join(file);
        if std::fs::read(&p).ok().as_deref() != Some(&picture.bytes[..]) {
            write_atomic(&p, &picture.bytes)?;
        }
    }
    save_meshes(path, &model.assets, &model.facets)?;
    save_elec(path, model.docs(), model.elec_assets(), model.elec_memory(), model.docs_removed(), model.elec_assets_removed())
}

/// The folder a project's electronics documents, assets and device memories live in: `<stem>.elec/` beside it.
pub fn elec_dir(project: &Path) -> PathBuf {
    let file = project.file_name().and_then(|s| s.to_str()).unwrap_or("model");
    let stem = file.strip_suffix(&format!(".{EXTENSION}")).unwrap_or(file);
    project.with_file_name(format!("{stem}.elec"))
}

/// The electronics folders whose files sit in folders of their own: part data in
/// `parts/<folder>/`, and each firmware's files in `firmware/<name>/` (the firmware document
/// `<name>.fw.splash` ships them with its image).
const ELEC_FOLDERS: [&str; 2] = ["parts/", "firmware/"];

/// Check each literal segment before joining a name to the electronics folder: no traversal
/// or aliases, and folders only under `parts/<folder>/` or `firmware/<folder>/`.
fn elec_file_name(file: &str) -> Result<(), String> {
    let mut segments = 0;
    for segment in file.split('/') {
        script_file_name(segment).map_err(|e| format!("invalid electronics file {file:?}: {e}"))?;
        if segment == "." {
            return Err(format!("invalid electronics file {file:?}: no '.' segments"));
        }
        segments += 1;
    }
    if segments != 1 && (!ELEC_FOLDERS.iter().any(|folder| file.starts_with(folder)) || segments < 3) {
        return Err(format!("an electronics file with folders must be under parts/<folder>/ or firmware/<folder>/, not {file:?}"));
    }
    Ok(())
}

/// A document is a top-level `.splash` file or one under `parts/<folder>/`, with safe segments.
pub fn doc_file_name(file: &str) -> Result<(), String> {
    elec_file_name(file)?;
    if !file.ends_with(".splash") {
        return Err(format!("an electronics document must end in `.splash`, not {file:?}"));
    }
    if file.starts_with("firmware/") {
        return Err(format!("a firmware's folder holds its files, not documents: {file:?}"));
    }
    Ok(())
}

/// An asset is any non-`.splash` file under `parts/<folder>/` or `firmware/<folder>/`, with safe
/// path segments.
pub fn elec_asset_file_name(file: &str) -> Result<(), String> {
    elec_file_name(file)?;
    if !ELEC_FOLDERS.iter().any(|folder| file.starts_with(folder)) || file.ends_with(".splash") {
        return Err(format!("an electronics asset must be a non-`.splash` file under parts/<folder>/ or firmware/<folder>/, not {file:?}"));
    }
    Ok(())
}

/// A device memory is a top-level `.eeprom` file with a safe name.
pub fn memory_file_name(file: &str) -> Result<(), String> {
    elec_file_name(file)?;
    if file.contains('/') || !file.ends_with(&format!(".{MEMORY_EXTENSION}")) {
        return Err(format!("a device memory must be a top-level `.eeprom` file, not {file:?}"));
    }
    Ok(())
}

/// Write electronics documents, assets and device memories into [`elec_dir`], creating their
/// folders as needed. Identical bytes are not rewritten. Top-level `.splash` files no longer
/// in `docs` and `.eeprom` files no longer in `memory` go. Under
/// `parts/`, only absent documents in `removed` whose bytes still match their last text go, and
/// absent assets in `assets_removed` whose bytes still hash as they did (an undone import's mesh);
/// files Fab did not remove and files the person rewrote stay.
/// Removal checks cost O(removed records) per save, reading and hashing only removed files;
/// nothing per frame.
pub fn save_elec(
    project: &Path, docs: &crate::Docs, assets: &crate::ElecAssets, memory: &crate::ElecMemory,
    removed: &BTreeMap<String, Arc<str>>, assets_removed: &BTreeMap<String, [u8; 32]>,
) -> Result<(), String> {
    for file in docs.keys() {
        doc_file_name(file)?;
    }
    for file in assets.keys() {
        elec_asset_file_name(file)?;
    }
    for file in memory.keys() {
        memory_file_name(file)?;
    }
    let dir = elec_dir(project);
    let files = docs.iter().map(|(file, text)| (file, text.as_bytes()))
        .chain(assets.iter().map(|(file, bytes)| (file, bytes.as_ref())))
        .chain(memory.iter().map(|(file, bytes)| (file, bytes.as_ref())));
    for (file, bytes) in files {
        let p = dir.join(file);
        if std::fs::read(&p).ok().as_deref() != Some(bytes) {
            write_atomic(&p, bytes)?;
        }
    }
    for (file, text) in removed {
        if !file.starts_with("parts/") || !file.ends_with(".splash") || docs.contains_key(file) {
            continue;
        }
        doc_file_name(file)?;
        let p = dir.join(file);
        let bytes = match std::fs::read(&p) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
        if bytes == text.as_bytes() {
            std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    for (file, hash) in assets_removed {
        if assets.contains_key(file) {
            continue;
        }
        elec_asset_file_name(file)?;
        let p = dir.join(file);
        let bytes = match std::fs::read(&p) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
        let mut h = Sha256::new();
        h.update(&bytes);
        if h.finish() == *hash {
            std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if kind.is_file() && ((name.ends_with(".splash") && !docs.contains_key(name))
            || (name.ends_with(&format!(".{MEMORY_EXTENSION}")) && !memory.contains_key(name))) {
            std::fs::remove_file(entry.path()).map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    if docs.is_empty() && assets.is_empty() && memory.is_empty() {
        let mut entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if entries.next().transpose().map_err(|e| format!("{}: {e}", dir.display()))?.is_none() {
            std::fs::remove_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    Ok(())
}

/// Read top-level documents and device memories, and every regular file under `parts/`:
/// `.splash` as text, top-level `.eeprom` as memory bytes, others as asset bytes. Missing folders
/// mean none; names use `/` on every platform. Directory links are not
/// followed, so a linked tree cannot make loading loop or escape the electronics folder. A save's
/// own `.tmp` files and names Fab could not save back are left on disk and not loaded, so a stray
/// file there never stops the project from opening.
pub fn load_elec(project: &Path) -> Result<(crate::Docs, crate::ElecAssets, crate::ElecMemory), String> {
    let dir = elec_dir(project);
    let mut docs = crate::Docs::new();
    let mut assets = crate::ElecAssets::new();
    let mut memory = crate::ElecMemory::new();
    let mut folders = vec![String::new()];
    while let Some(folder) = folders.pop() {
        let at = dir.join(&folder);
        let entries = match std::fs::read_dir(&at) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", at.display())),
        };
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", at.display()))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let file = if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") };
            if kind.is_dir() && (!folder.is_empty() || name == "parts" || name == "firmware") {
                folders.push(file);
            } else if kind.is_file() && (file.ends_with(".splash") || !folder.is_empty() || memory_file_name(&file).is_ok()) && !file.ends_with(".tmp") {
                if file.ends_with(".splash") {
                    if doc_file_name(&file).is_err() { continue }
                    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    docs.insert(file, text.into());
                } else if memory_file_name(&file).is_ok() {
                    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    memory.insert(file, bytes.into());
                } else {
                    if elec_asset_file_name(&file).is_err() { continue }
                    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                    assets.insert(file, bytes.into());
                }
            }
        }
    }
    Ok((docs, assets, memory))
}

/// Is `path` a project file (`.fab.splash`)?
pub fn is_project(path: &Path) -> bool {
    path.to_str().is_some_and(|p| p.ends_with(&format!(".{EXTENSION}")))
}

/// Is `path` an object script (`.cad.splash`)?
pub fn is_script(path: &Path) -> bool {
    path.to_str().is_some_and(|p| p.ends_with(&format!(".{SCRIPT_EXTENSION}")))
}

/// The folder a project's meshes live in.
pub fn meshes_dir(project: &Path) -> PathBuf {
    let file = project.file_name().and_then(|s| s.to_str()).unwrap_or("model");
    let stem = file.strip_suffix(&format!(".{EXTENSION}")).unwrap_or(file);
    project.with_file_name(format!("{stem}.meshes"))
}

/// The picture saved with a project: `<stem>.thumb.png` beside it (the viewport's last settled
/// frame, small). The plate list shows it and never renders a model to list one.
pub fn thumbnail_path(project: &Path) -> PathBuf {
    let file = project.file_name().and_then(|s| s.to_str()).unwrap_or("model");
    let stem = file.strip_suffix(&format!(".{EXTENSION}")).unwrap_or(file);
    project.with_file_name(format!("{stem}.thumb.png"))
}

pub fn mesh_bytes(m: &MeshData) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(m.positions.len() as u32).to_le_bytes());
    for p in &m.positions {
        p.iter().for_each(|x| out.extend_from_slice(&x.to_le_bytes()));
    }
    out.extend_from_slice(&(m.triangles.len() as u32).to_le_bytes());
    for t in &m.triangles {
        t.iter().for_each(|i| out.extend_from_slice(&i.to_le_bytes()));
    }
    out
}

pub fn mesh_of_bytes(b: &[u8]) -> Result<MeshData, String> {
    if !b.starts_with(MAGIC) {
        return Err("not a Fab mesh".into());
    }
    let mut at = 4;
    let mut word = || -> Result<u32, String> {
        let w = b.get(at..at + 4).ok_or("a truncated mesh")?;
        at += 4;
        Ok(u32::from_le_bytes(w.try_into().unwrap()))
    };
    let n = word()? as usize;
    if n > b.len() / 12 {
        return Err("a mesh's vertex count runs past its file".into());
    }
    let mut positions = Vec::with_capacity(n);
    for _ in 0..n {
        positions.push([f32::from_bits(word()?), f32::from_bits(word()?), f32::from_bits(word()?)]);
    }
    let t = word()? as usize;
    if t > b.len() / 12 {
        return Err("a mesh's triangle count runs past its file".into());
    }
    let mut triangles = Vec::with_capacity(t);
    for _ in 0..t {
        let tri = [word()?, word()?, word()?];
        if tri.iter().any(|i| *i as usize >= n) {
            return Err("a mesh triangle names a vertex it does not have".into());
        }
        triangles.push(tri);
    }
    Ok(MeshData { positions, triangles })
}

pub fn facets_bytes(facets: &Facets) -> Vec<u8> {
    let mut out = FACETS_MAGIC.to_vec();
    out.extend_from_slice(&(facets.tris().len() as u32).to_le_bytes());
    for t in facets.tris() {
        t.iter().flatten().for_each(|x| out.extend_from_slice(&x.to_le_bytes()));
    }
    out.extend_from_slice(facets.states());
    out
}

pub fn facets_of_bytes(b: &[u8]) -> Result<Facets, String> {
    if !b.starts_with(FACETS_MAGIC) {
        return Err("not Fab painted facets".into());
    }
    let count = b.get(4..8).ok_or("truncated painted facets")?;
    let n = u32::from_le_bytes(count.try_into().unwrap()) as usize;
    if n > (b.len() - 8) / 37 {
        return Err("a facet count runs past its file".into());
    }
    if b.len() - 8 != n * 37 {
        return Err("a facet count does not match its file".into());
    }
    let mut tris = Vec::with_capacity(n);
    let end = 8 + n * 36;
    for leaf in b[8..end].chunks_exact(36) {
        let t: [[f32; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|k| {
            let at = (i * 3 + k) * 4;
            f32::from_le_bytes(leaf[at..at + 4].try_into().unwrap())
        }));
        if t.iter().flatten().any(|v| !v.is_finite()) {
            return Err("a facet has a non-finite corner".into());
        }
        tris.push(t);
    }
    Ok(Facets::new(tris, b[end..].to_vec()))
}

/// Write the project's meshes and painted leaves beside it (none: no folder).
pub fn save_meshes(project: &Path, assets: &Assets, facets: &FacetAssets) -> Result<(), String> {
    let dir = meshes_dir(project);
    if !assets.is_empty() || !facets.is_empty() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for (name, m) in assets {
        let path = dir.join(format!("{name}.{MESH_EXTENSION}"));
        write_atomic(&path, &mesh_bytes(&m.data))?;
    }
    for (name, f) in facets {
        let path = dir.join(format!("{name}.{FACETS_EXTENSION}"));
        write_atomic(&path, &facets_bytes(f))?;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let name = path.file_stem().and_then(|s| s.to_str());
        let stale = match path.extension().and_then(|s| s.to_str()) {
            Some(MESH_EXTENSION) => name.is_none_or(|name| !assets.contains_key(name)),
            Some(FACETS_EXTENSION) => name.is_none_or(|name| !facets.contains_key(name)),
            _ => false,
        };
        if stale {
            let kind = entry.file_type().map_err(|e| format!("{}: {e}", path.display()))?;
            if kind.is_file() || kind.is_symlink() {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    if assets.is_empty() && facets.is_empty() {
        let mut entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if entries.next().transpose().map_err(|e| format!("{}: {e}", dir.display()))?.is_none() {
            std::fs::remove_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
    }
    Ok(())
}

/// The meshes and painted leaves beside a project, read together (no folder: none).
pub fn load_meshes(project: &Path) -> Result<(Assets, FacetAssets), String> {
    let dir = meshes_dir(project);
    let mut out = Assets::new();
    let mut facets = FacetAssets::new();
    let Ok(list) = std::fs::read_dir(&dir) else { return Ok((out, facets)) };
    for entry in list.flatten() {
        let path = entry.path();
        let extension = path.extension().and_then(|e| e.to_str());
        if !matches!(extension, Some(MESH_EXTENSION | FACETS_EXTENSION)) {
            continue;
        }
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if extension == Some(MESH_EXTENSION) {
            let data = mesh_of_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            out.insert(name, Arc::new(MeshField::new(data)));
        } else {
            let data = facets_of_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            facets.insert(name, Arc::new(data));
        }
    }
    Ok((out, facets))
}

/// Two checked values of one setting read the same: equal text, or the same number in the same
/// form ("100%" and "100.0%").
pub fn same_setting(a: &str, b: &str) -> bool {
    a == b || match (fab_plate::setting_number(a), fab_plate::setting_number(b)) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9 && a.trim().ends_with('%') == b.trim().ends_with('%'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Docs, ElecAssets, ElecMemory, FabModel};

    /// An asset Fab removed (an undone import's mesh) leaves the saved plate by its recorded hash;
    /// a file the person rewrote stays, and a returning asset clears its record.
    #[test]
    fn a_removed_asset_leaves_the_saved_plate_unless_its_file_was_rewritten() {
        let dir = std::env::temp_dir().join(format!("fab-elec-asset-removed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        let (mesh, rewritten) = ("parts/lcsc/C1.glb", "parts/lcsc/C2.glb");
        let mut model = FabModel::new("plate");
        model.set_docs(Arc::new(Docs::from([("circuit.ckt.splash".into(), Arc::<str>::from("circuit\n"))])));
        let both = Arc::new(ElecAssets::from([
            (mesh.into(), Arc::<[u8]>::from([1u8, 2, 3])), (rewritten.into(), Arc::<[u8]>::from([4u8, 5])),
        ]));
        model.set_elec_assets(both.clone());
        save(&model, &path).expect("save both meshes");
        let elec = elec_dir(&path);
        std::fs::write(elec.join(rewritten), [9u8, 9]).expect("the person rewrites the second mesh");
        model.set_elec_assets(Arc::new(ElecAssets::new()));
        assert_eq!(model.elec_assets_removed().keys().collect::<Vec<_>>(), [mesh, rewritten], "both removals are recorded");
        save(&model, &path).expect("save the removals");
        assert!(!elec.join(mesh).exists(), "the removed mesh leaves the saved plate");
        assert_eq!(std::fs::read(elec.join(rewritten)).expect("the rewritten mesh stays"), [9u8, 9], "a file the person rewrote stays");
        model.set_elec_assets(Arc::new(ElecAssets::from([(mesh.into(), both[mesh].clone())])));
        assert!(!model.elec_assets_removed().contains_key(mesh), "a returning asset clears its record");
        save(&model, &path).expect("save the returned mesh");
        assert_eq!(std::fs::read(elec.join(mesh)).expect("the returned mesh is saved"), [1u8, 2, 3], "the returned mesh is written again");
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }

    /// Electronics survive disk round trips without rewriting identical files or pruning a
    /// person's parts. Documents and assets affect content; device memories leave it unchanged.
    #[test]
    fn electronics_round_trip_and_preserve_parts() {
        let dir = std::env::temp_dir().join(format!("fab-elec-round-trip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        let mut model = FabModel::new("plate");
        assert_eq!(model.content(), model.head().content, "no electronics keeps the saved object hash");
        assert!(model.is_empty(), "no object scripts or electronics is empty");
        assert_eq!(load_elec(&path).expect("missing electronics is empty"), (Docs::new(), ElecAssets::new(), ElecMemory::new()),
            "missing electronics has no documents, assets or device memories");

        let docs = Arc::new(Docs::from([
            ("circuit.ckt.splash".into(), Arc::<str>::from("circuit\n")),
            ("parts/lcsc/C1.part.splash".into(), Arc::<str>::from("part\n")),
            ("parts/lcsc/nested/C2.part.splash".into(), Arc::<str>::from("nested part\n")),
        ]));
        let asset = "parts/lcsc/C1.glb";
        let assets = Arc::new(ElecAssets::from([
            (asset.into(), Arc::<[u8]>::from([0x00, 0xff, 0x80, 0x01])),
            ("firmware/handheld/logo.png".into(), Arc::<[u8]>::from([0x89, 0x50, 0x4e, 0x47])),
        ]));
        let memory_name = "board.mcu.eeprom";
        let memory_bytes: Arc<[u8]> = (0..1024).map(|i| i as u8).collect::<Vec<_>>().into();
        let memory = Arc::new(ElecMemory::from([(memory_name.into(), memory_bytes.clone())]));
        let head = model.head();
        model.set_docs(docs.clone());
        let docs_content = model.content();
        model.set_elec_assets(assets.clone());
        let content = model.content();
        assert_ne!(content, docs_content, "assets contribute to content");
        model.set_elec_memory(memory.clone());
        assert_eq!(model.content(), content, "device memory is not content");
        assert_eq!(model.head(), head, "electronics do not move the object head");
        save(&model, &path).expect("save documents, binary assets and device memories");
        let loaded = load(&path, model.env()).expect("load the saved plate");
        assert_eq!(loaded.docs(), &docs, "all documents return byte for byte");
        assert_eq!(loaded.elec_assets(), &assets, "binary assets return byte for byte");
        assert_eq!(loaded.elec_memory(), &memory, "device memories return byte for byte");
        assert_eq!(loaded.content(), content, "the saved plate has the same content");

        let elec = elec_dir(&path);
        assert_eq!(std::fs::read(elec.join(memory_name)).expect("read the saved device memory"), memory_bytes.as_ref(),
            "the top-level memory file holds exactly the device's bytes");
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let files: Vec<_> = docs.keys().chain(assets.keys()).chain(memory.keys()).map(|name| {
            let file = elec.join(name);
            std::fs::File::options().write(true).open(&file).expect("open saved electronics")
                .set_modified(old).expect("set a distinct modification time without sleeping");
            let modified = std::fs::metadata(&file).expect("saved file exists").modified().expect("read modification time");
            (file, modified)
        }).collect();
        save(&model, &path).expect("save unchanged electronics");
        for (file, modified) in files {
            assert_eq!(std::fs::metadata(&file).expect("file stays").modified().expect("read modification time"),
                modified, "unchanged electronics are not rewritten: {}", file.display());
        }

        model.set_elec_memory(Arc::default());
        save(&model, &path).expect("save the cleared device memory");
        assert!(!elec.join(memory_name).exists(), "clearing a device memory removes its file");

        let personal = "parts/lcsc/notes.bin";
        let bytes = [0x00, 0xfe, 0x81];
        std::fs::write(elec.join(personal), bytes).expect("put a person's file beside the part");
        std::fs::write(elec.join("parts/lcsc/C1.glb.tmp"), [0x01]).expect("leave a crashed save's temp file");
        std::fs::write(elec.join("old.splash"), "stale\n").expect("put a stale top-level document there");
        save(&model, &path).expect("save leaves the person's file");
        let loaded = load(&path, model.env()).expect("load includes the person's asset");
        assert_eq!(loaded.elec_assets()[personal].as_ref(), bytes, "a person's file is loaded byte for byte");
        assert!(!elec.join("old.splash").exists(), "stale top-level documents are still removed");
        assert!(!loaded.elec_assets().contains_key("parts/lcsc/C1.glb.tmp"), "a save's temp file is not an asset");

        model.set_elec_assets(Arc::new(ElecAssets::from([(asset.into(), Arc::<[u8]>::from([0x00, 0xff]))])));
        assert_ne!(model.content(), content, "an asset byte edit changes content");
        model.set_elec_assets(Arc::new(ElecAssets::from([("parts/lcsc/renamed.glb".into(), assets[asset].clone())])));
        assert_ne!(model.content(), content, "an asset rename changes content");
        model.set_elec_assets(Arc::new(assets.iter().map(|(name, bytes)| (name.clone(), Arc::<[u8]>::from(bytes.as_ref()))).collect()));
        assert_eq!(model.content(), content, "equal bytes in a fresh Arc keep content");
        model.set_elec_assets(Arc::default());
        assert_eq!(model.content(), docs_content, "removing assets restores the documents-only hash");
        model.set_docs(Arc::default());
        model.set_elec_assets(assets.clone());
        assert!(!model.is_empty(), "an asset-only plate is not empty");
        model.set_elec_assets(Arc::default());
        assert_eq!(model.content(), head.content, "removing all electronics restores the old hash");
        save(&model, &path).expect("save an emptied plate");
        assert!(!elec.join("circuit.ckt.splash").exists(), "the removed top-level document goes");
        // 4b3b: an asset Fab removed goes like a part document Fab removed (its bytes still match).
        assert!(!elec.join(asset).exists(), "the asset Fab removed goes");
        assert!(!elec.join("parts/lcsc/C1.part.splash").exists(), "the part document Fab removed goes");
        assert_eq!(std::fs::read(elec.join(personal)).expect("the person's file stays"), bytes);
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }

    /// A removed part stays removed after a disk round trip, while files Fab did not remove
    /// and a person's replacement text under a removed name survive.
    #[test]
    fn removed_part_documents_stay_removed_after_reload() {
        let dir = std::env::temp_dir().join(format!("fab-elec-removed-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        let mut model = FabModel::new("plate");
        let part = "parts/local/C1.part.splash";
        let rewritten = "parts/local/C2.part.splash";
        let kept = "parts/local/C3.part.splash";
        let personal_part = "parts/local/personal.part.splash";
        let personal = "parts/local/notes.bin";
        let asset = "parts/local/C1.glb";
        let docs = Arc::new(Docs::from([
            ("circuit.ckt.splash".into(), Arc::<str>::from("circuit\n")),
            (part.into(), Arc::<str>::from("part one\n")),
            (rewritten.into(), Arc::<str>::from("part two\n")),
            (kept.into(), Arc::<str>::from("part three\n")),
        ]));
        let assets = Arc::new(ElecAssets::from([(asset.into(), Arc::<[u8]>::from([0x00, 0xff, 0x80]))]));
        model.set_docs(docs.clone());
        model.set_elec_assets(assets.clone());
        let head = model.head();
        save(&model, &path).expect("save the plate before removing its parts");
        let elec = elec_dir(&path);
        assert_eq!(std::fs::read_to_string(elec.join(part)).expect("the part was saved"), docs[part].as_ref());
        let personal_bytes = [0x00, 0xfe, 0x81];
        std::fs::write(elec.join(personal), personal_bytes).expect("put a person's file beside the parts");
        let personal_text = "a part Fab never held\n";
        std::fs::write(elec.join(personal_part), personal_text).expect("put a person's part beside Fab's parts");

        let mut remaining = (*docs).clone();
        remaining.remove(part);
        remaining.remove(rewritten);
        model.set_docs(Arc::new(remaining.clone()));
        let replacement = "the person's replacement part\n";
        std::fs::write(elec.join(rewritten), replacement).expect("rewrite a removed part outside Fab");
        remaining.insert("circuit.ckt.splash".into(), Arc::from("circuit edited\n"));
        model.set_docs(Arc::new(remaining));
        assert_eq!(model.head(), head, "removing documents leaves the object head alone");
        save(&model, &path).expect("save the removal after another document update");
        assert!(!elec.join(part).exists(), "the part Fab removed is deleted from disk");
        save(&model, &path).expect("saving again tolerates an already removed part");
        let loaded = load(&path, model.env()).expect("reload the plate after the removal");
        assert!(!loaded.docs().contains_key(part), "a removed part does not return on reload");
        assert_eq!(loaded.docs()[kept], docs[kept], "a part Fab did not remove survives");
        assert_eq!(loaded.docs()[personal_part].as_ref(), personal_text, "a part never held by Fab survives");
        assert_eq!(loaded.docs()[rewritten].as_ref(), replacement, "a person's new text under a removed name survives");
        assert_eq!(loaded.elec_assets()[personal].as_ref(), personal_bytes, "the person's binary file survives");
        assert_eq!(loaded.elec_assets()[asset], assets[asset], "the removed part's asset survives");
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }

    /// Saves refuse traversal and mismatched kinds, naming the offending file before writing it.
    #[test]
    fn electronics_save_refuses_bad_names() {
        let dir = std::env::temp_dir().join(format!("fab-elec-bad-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("plate.fab.splash");
        for name in ["parts/../C1", "/parts/lcsc/C1", r"parts\lcsc\C1", "parts//lcsc/C1",
            "parts/./lcsc/C1", "parts/C1", "elsewhere/lcsc/C1", "parts/C:/C1"] {
            for document in [true, false] {
                let name = format!("{name}{}", if document { ".part.splash" } else { ".glb" });
                let mut model = FabModel::new("plate");
                if document {
                    model.set_docs(Arc::new(Docs::from([(name.clone(), "part\n".into())])));
                } else {
                    model.set_elec_assets(Arc::new(ElecAssets::from([(name.clone(), Arc::<[u8]>::from([0x00, 0xff]))])));
                }
                let error = save(&model, &path).expect_err("unsafe electronics name must be refused");
                assert!(error.contains(&format!("{name:?}")), "the error names {name:?}: {error}");
            }
        }
        for name in ["", "parts/lcsc/", "parts/lcsc/C1.glb"] {
            let docs = Docs::from([(name.into(), "part\n".into())]);
            let error = save_elec(&path, &docs, &ElecAssets::new(), &ElecMemory::new(), &Docs::new(), &BTreeMap::new()).expect_err("a document needs a valid .splash name");
            assert!(error.contains(&format!("{name:?}")), "the error names {name:?}: {error}");
        }
        for name in ["C1.glb", "parts/lcsc/C1.part.splash"] {
            let assets = ElecAssets::from([(name.into(), Arc::<[u8]>::from([0x00]))]);
            let error = save_elec(&path, &Docs::new(), &assets, &ElecMemory::new(), &Docs::new(), &BTreeMap::new()).expect_err("assets need non-.splash names under parts");
            assert!(error.contains(&format!("{name:?}")), "the error names {name:?}: {error}");
        }
        for name in ["parts/x/a.eeprom", "board.mcu"] {
            let memory = ElecMemory::from([(name.into(), Arc::<[u8]>::from([0x00]))]);
            let error = save_elec(&path, &Docs::new(), &ElecAssets::new(), &memory, &Docs::new(), &BTreeMap::new()).expect_err("device memories need top-level .eeprom names");
            assert!(error.contains(&format!("{name:?}")), "the error names {name:?}: {error}");
        }
        assert!(!elec_dir(&path).exists(), "invalid names write no electronics files");
        std::fs::remove_dir_all(&dir).expect("remove the test project");
    }
}
