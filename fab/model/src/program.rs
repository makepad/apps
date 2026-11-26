//! Object scripts and the compiled plate.
//!
//! An **object script** (`<name>.cad.splash`) is a Splash document in the model language
//! (`makepad_model_script::lang`) that emits objects with Fab's calls in `mod.fab`:
//!
//! - `object(solid, name: "lid")`: the solid is an object the script puts on the plate (named
//!   by its `let` unless `name` says otherwise; with neither, by its builder and line, `box@6`,
//!   until a write names it so: [`named_objects`]). Jointed solids are one object: emitting any
//!   body of a jointed assembly emits the whole assembly, and an assembly no `object(...)`
//!   names is emitted as it stands. Where and how an object prints is the project's business.
//! - `insert(solid, name: "Nut", into: "Base", clearance: 0.2mm)`: an object that never
//!   prints and rides on the object `into` names in this script. It uses that object's
//!   model-to-plate transform, never drops or arranges on its own, and keeps beads clear
//!   sideways by `clearance` (default 0.2 mm). With `embed: true` it is printed into its host:
//!   the slicer cuts its cavity (its shape swept up and grown by `clearance`), and the print
//!   pauses before the first layer over it so it can be dropped in.
//! - `panel(Panel{...})`: the script's panel, real Splash panel code.
//! - `handle(param, Arrow{from to} | Ring{center axis radius} | Stepper{at axis})`: where a
//!   declared parameter's drag handle sits, written with the parameter so it follows it.
//! - `m.fill`, `m.facets`, `m.stroke`, `m.strokes`, `m.erase`, `m.support_block`, `m.support_enforce`, `m.settings`: paint, kept out
//!   of every solid (`crate::paint`).
//! - `m.fixed`, `m.force`, `m.pressure`, `m.torque`: the force study, where the part is held
//!   and loaded, kept out of every solid (`crate::study`).
//! - `m.expect(name, solid, {x y z volume_mm3 count wall})`: a number the person gave, judged on
//!   every build (`crate::expect`).
//!
//! A script is a dynamic mesh file: the project places it (an **instance**, with its own
//! parameter values) and each object it emits gets its own placement. Each instance is
//! compiled on its own (its own VM and heap) and cached by source and parameter values.
//! Linked clones share its compiled script, so an edit rebuilds only the scripts it touches.
//!
//! Lengths are written with units (`150mm`); the kernel works in millimetres.

use makepad_model_script::lang::{self, Arg, Diag, Options, Program, RootFn, WeaveDefaults};
pub use makepad_model_script::lang::Assets;
use std::collections::{BTreeMap, HashMap};
pub use crate::generated::{GenInput, Generator, PanelBase, PanelHit};
pub use lang::{GenPart, GenSolid};
use std::ops::Range;
use std::sync::Arc;

/// Painted leaves of imported meshes, keyed by mesh asset name.
pub type FacetAssets = HashMap<String, Arc<fab_paint_field::Facets>>;

/// Kernel units (millimetres) per metre.
pub const UNIT: f64 = 1000.0;

/// An object script's file extension (`bracket.cad.splash`).
pub const SCRIPT_EXTENSION: &str = "cad.splash";

/// A paint file's extension (`bracket.paint.splash`): strokes and regions an object script
/// reads with `m.paint_file(..)`, kept beside it.
pub const PAINT_EXTENSION: &str = "paint.splash";

/// Is `file` a paint file?
pub fn is_paint_file(file: &str) -> bool {
    file.ends_with(&format!(".{PAINT_EXTENSION}"))
}

/// A strokes file's extension (`vase.strokes`): the hand-drawn strokes an object script reads
/// with `m.strokes(..)`, a binary log beside it (`fab_paint_field::strokes`).
pub const STROKES_EXTENSION: &str = "strokes";

/// Is `file` a strokes file?
pub fn is_strokes_file(file: &str) -> bool {
    file.ends_with(&format!(".{STROKES_EXTENSION}"))
}

/// The strokes files a script reads (`m.strokes("x.strokes")`, and a support statement's
/// `m.support_block("x.block.strokes")`), from its syntax.
pub fn strokes_files(source: &str) -> Vec<String> {
    let mut out = calls_of(source, "m.strokes");
    for f in calls_of(source, "m.support_block").into_iter().chain(calls_of(source, "m.support_enforce")) {
        if is_strokes_file(&f) && !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// The first string argument of every `call(..)` in a script, each once.
fn calls_of(source: &str, call: &str) -> Vec<String> {
    calls_of_with_dynamic(source, call).0
}

/// Literal first arguments, and whether any matching call needs a computed name.
pub(crate) fn calls_of_with_dynamic(source: &str, call: &str) -> (Vec<String>, bool) {
    let Ok(names) = lang::names::parse(source) else { return (Vec::new(), false) };
    let mut out = Vec::new();
    let mut dynamic = false;
    names.syntax.walk(&mut |_, node| {
        if let Some((name, args)) = node.as_call() {
            // `board(..)`, or through a module path (`mod.fab.board(..)`, `fab.board(..)`)
            if name == call || name.strip_suffix(call).is_some_and(|head| head.ends_with('.')) {
                if let Some(f) = args.first().and_then(|a| a.string()) {
                    if !out.iter().any(|o: &String| o == f) {
                        out.push(f.to_string());
                    }
                } else {
                    dynamic = true;
                }
            }
        }
    });
    (out, dynamic)
}

/// The paint files a script reads (`m.paint_file("x.paint.splash")`), from its syntax.
pub fn paint_files(source: &str) -> Vec<String> {
    calls_of(source, "m.paint_file")
}

/// What a host adds to its scripts' language: libraries and host inputs (`lang::Library`,
/// `lang::Inputs`). Default: none. Made with [`ScriptEnv::new`], which reads once what inputs the
/// libraries' own calls name, for the compile key.
#[derive(Clone, Default)]
pub struct ScriptEnv {
    /// Inherited by this compiled script on parameter previews and generated-part passes.
    pub(crate) imports: Arc<[String]>,
    pub libraries: Arc<Vec<lang::Library>>,
    pub inputs: Arc<lang::Inputs>,
    pub generators: BTreeMap<String, Generator>,
    /// Per input kind: the names the libraries' calls read, and whether one computes its name.
    /// Found at the first compile that keys on a given value's kind (a parse of every library),
    /// never when the env is made: a host registers its kinds for nothing.
    library_reads: Arc<std::sync::OnceLock<Vec<(String, Vec<String>, bool)>>>,
}

impl ScriptEnv {
    /// O(1): the libraries and inputs as given.
    pub fn new(libraries: Vec<lang::Library>, inputs: lang::Inputs) -> ScriptEnv {
        ScriptEnv { libraries: Arc::new(libraries), inputs: Arc::new(inputs), imports: Arc::default(), library_reads: Arc::default(), generators: BTreeMap::new() }
    }

    pub fn with_generator(mut self, kind: impl Into<String>, generator: Generator) -> Self {
        self.generators.insert(kind.into(), generator);
        self
    }

    pub(crate) fn library_reads(&self) -> &[(String, Vec<String>, bool)] {
        self.library_reads.get_or_init(|| {
            self.inputs.kinds.iter().map(|kind| {
                let (mut names, mut dynamic) = (Vec::new(), false);
                for lib in self.libraries.iter() {
                    let (more, d) = calls_of_with_dynamic(&lib.source, kind);
                    names.extend(more);
                    dynamic |= d;
                }
                (kind.clone(), names, dynamic)
            }).collect()
        })
    }

    /// Whether the host gave any value of `kind` (else only its lazy provider serves it).
    pub(crate) fn gives(&self, kind: &str) -> bool {
        self.inputs.values.range((kind.to_string(), String::new())..).next().is_some_and(|((k, _), _)| k == kind)
    }

    /// The same env, not a copy of equal content: one `set_env` made both (O(1)).
    pub fn same(&self, other: &ScriptEnv) -> bool {
        Arc::ptr_eq(&self.libraries, &other.libraries) && Arc::ptr_eq(&self.inputs, &other.inputs) && Arc::ptr_eq(&self.imports, &other.imports)
    }
}

impl std::fmt::Debug for ScriptEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptEnv").field("libraries", &self.libraries).field("inputs", &self.inputs).field("imports", &self.imports).field("generators", &self.generators.keys().collect::<Vec<_>>()).finish()
    }
}

/// What a compile needs from the printer: the weave builders' bar and clearances (measured ones when the
/// printer and filament are calibrated). Held by the model, set by the app.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileContext {
    pub weave: WeaveDefaults,
}

impl CompileContext {
    pub fn from_profile(p: &makepad_csg_mech::Profile) -> CompileContext {
        CompileContext { weave: WeaveDefaults { bar: (2.0 * p.nozzle).max(0.6), clearance: p.c_xy(), clearance_z: p.c_z() } }
    }
}

impl Default for CompileContext {
    /// A 0.4 mm nozzle, 0.2 mm layers, PLA.
    fn default() -> CompileContext {
        CompileContext::from_profile(&makepad_csg_mech::Profile::default())
    }
}

pub const ROOTS: &[RootFn] = &[
    RootFn { name: "object", args: &["solid", "name"], model: false },
    RootFn { name: "insert", args: &["solid", "name", "into", "clearance", "embed", "melt"], model: false },
    RootFn { name: "panel", args: &["panel"], model: false },
    RootFn { name: "handle", args: &["param", "shape"], model: false },
    // Paint: statements about the model's surface, never solids (`crate::paint`).
    RootFn { name: "fill", args: &["region", "material"], model: true },
    RootFn { name: "facets", args: &["of", "materials"], model: true },
    RootFn { name: "stroke", args: &["points", "radius", "material", "opts"], model: true },
    RootFn { name: "erase", args: &["region"], model: true },
    RootFn { name: "support_block", args: &["region"], model: true },
    RootFn { name: "support_enforce", args: &["region"], model: true },
    RootFn { name: "no_trees", args: &["region"], model: true },
    RootFn { name: "settings", args: &["region", "opts"], model: true },
    RootFn { name: "paint_file", args: &["file"], model: true },
    RootFn { name: "strokes", args: &["file"], model: true },
    // The force study: where the part is held and loaded, never solids (`crate::study`).
    RootFn { name: "fixed", args: &["region", "rigid"], model: true },
    RootFn { name: "force", args: &["region", "newtons"], model: true },
    RootFn { name: "pressure", args: &["region", "mpa"], model: true },
    RootFn { name: "torque", args: &["region", "nmm"], model: true },
    // The person's numbers: a solid's size, volume, count or wall, judged on every build (`crate::expect`).
    RootFn { name: "expect", args: &["name", "solid", "want"], model: true },
];

/// The statements that mark a solid as an area of the plate rather than something printed: the
/// trees kept out of it (`m.no_trees`), print settings in it (`m.settings`).
pub const AREA_ROOTS: [&str; 2] = ["no_trees", "settings"];

/// The solids `m.no_trees` marks in `p`, and the solids each is built from.
pub fn no_trees_solids(p: &Program) -> Vec<usize> {
    marked_solids(p, &AREA_ROOTS[..1])
}

/// The solids `m.settings` sets print settings in, and the solids each is built from.
pub fn settings_solids(p: &Program) -> Vec<usize> {
    marked_solids(p, &AREA_ROOTS[1..])
}

/// The solids an area statement ([`AREA_ROOTS`]) marks in `p`, and the solids each is built
/// from: they go with every part of the script (its handles, its gizmo, its box in the view).
pub fn area_solids(p: &Program) -> Vec<usize> {
    marked_solids(p, &AREA_ROOTS)
}

/// How many areas statements of `kind` ([`AREA_ROOTS`]) mark in `p`: their distinct solids.
pub fn marked_areas(p: &Program, kind: &str) -> usize {
    let mut marked: Vec<usize> = p.roots.iter().filter(|r| r.kind == kind).filter_map(|r| match r.get("region") {
        Some(Arg::Solid(i)) => Some(*i),
        _ => None,
    }).collect();
    marked.sort_unstable();
    marked.dedup();
    marked.len()
}

/// How many of `p`'s solids build what it prints: excluding areas and inserts
/// no printed object is built from.
pub fn printed_solids(p: &Program) -> usize {
    let objects: Vec<usize> = p.roots.iter().filter(|r| r.kind == "object").filter_map(|r| match r.get("solid") {
        Some(Arg::Solid(i)) => Some(*i),
        _ => None,
    }).collect();
    let builds = |i: usize| std::iter::successors(Some(i), |&j| p.solids[j].parent).any(|j| objects.contains(&j));
    let mut areas = area_solids(p);
    areas.extend(marked_solids(p, &["insert"]));
    (0..p.solids.len()).filter(|&i| !areas.contains(&i) || builds(i)).count()
}

fn marked_solids(p: &Program, kinds: &[&str]) -> Vec<usize> {
    let marked: Vec<usize> = p.roots.iter().filter(|r| kinds.contains(&r.kind)).flat_map(|r| r.args.iter()).flat_map(|(_, a)| match a {
        Arg::Solid(i) => vec![*i],
        Arg::Solids(ids) => ids.clone(),
        _ => Vec::new(),
    }).collect();
    let under = |i: usize| std::iter::successors(Some(i), |&j| p.solids[j].parent).any(|j| marked.contains(&j));
    (0..p.solids.len()).filter(|&i| under(i)).collect()
}

/// What a paint file may hold: paint, no solids it emits.
pub const PAINT_ROOTS: &[RootFn] = &[
    RootFn { name: "fill", args: &["region", "material"], model: true },
    RootFn { name: "stroke", args: &["points", "radius", "material", "opts"], model: true },
    RootFn { name: "erase", args: &["region"], model: true },
    RootFn { name: "support_block", args: &["region"], model: true },
    RootFn { name: "support_enforce", args: &["region"], model: true },
];

/// The script a new object starts with.
pub const EMPTY: &str = "use mod.model.*\nuse mod.fab.*\nlet m = mod.model\n\n";

/// One object a script emits: a body, a jointed assembly of several, or the bodies
/// `object([..])` lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Emitted {
    pub name: String,
    pub link: Option<String>,
    pub insert: Option<InsertOf>,
    /// The solid `object(...)` names (the first it lists; an assembly's first body when none
    /// does).
    pub solid: usize,
    /// Its bodies: the solid, or every solid of its jointed assembly; of a list, each listed
    /// body with its assembly's, in the list's order.
    pub bodies: Vec<usize>,
    /// The bodies a list names, in its order, when it names two or more: they print as one
    /// body, each layer the union of theirs, a later one owning where it overlaps an earlier
    /// ([`fab_plate::Part::body_of`]). A jointed assembly's other bodies print apart, as a
    /// joint must.
    pub glued: Vec<usize>,
    pub loc: Option<(u32, u32)>,
}

/// A non-printed solid attached to an object of the same script.
#[derive(Clone, Debug, PartialEq)]
pub struct InsertOf {
    pub into: String,
    /// Sideways bead clearance in millimetres.
    pub clearance: f64,
    /// Printed into its host (`embed: true`): a cavity is cut for it and the print pauses
    /// for it to be placed.
    pub embed: bool,
    /// Melted into its host after the print (`melt: heat_insert(..)`): its hole in the model
    /// frame, millimetres.
    pub melt: Option<fab_plate::melt::Melt>,
}

/// A heat-set insert's hole from what `heat_insert(..)` gives (metres, the model frame), in
/// millimetres: its size, mouth (`at`), axis into the part (-`dir`), hole, depth, taper, wall and
/// floor.
fn melt_of(o: &lang::Obj) -> Result<fab_plate::melt::Melt, String> {
    let num = |k: &str| match o.get(k) {
        Some(lang::V::Num(n)) if n.is_finite() && *n >= 0.0 => Ok(n * UNIT),
        _ => Err(format!("needs '{k}', a length, as heat_insert(..) gives it")),
    };
    let p3 = |k: &str| match o.get(k) {
        Some(lang::V::Arr(a)) if a.len() == 3 => match (a[0].num(), a[1].num(), a[2].num()) {
            (Some(x), Some(y), Some(z)) if x.is_finite() && y.is_finite() && z.is_finite() => Ok([x, y, z]),
            _ => Err(format!("'{k}' must be [x y z]")),
        },
        _ => Err(format!("needs '{k}' [x y z], as heat_insert(..) gives it")),
    };
    let size = match o.get("size") {
        Some(lang::V::Str(s)) => s.clone(),
        _ => return Err("needs 'size', as heat_insert(..) gives it".into()),
    };
    let dir = p3("dir")?;
    let n = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if !(n > 1e-9) {
        return Err("'dir' must point somewhere".into());
    }
    let (hole, depth) = (num("hole")?, num("depth")?);
    if !(hole > 0.0 && depth > 0.0) {
        return Err("needs a hole and a depth over zero".into());
    }
    Ok(fab_plate::melt::Melt {
        size,
        mouth: p3("at")?.map(|v| v * UNIT),
        axis: dir.map(|v| -v / n),
        hole,
        depth,
        taper: num("taper")?,
        wall: num("wall")?,
        floor: num("floor")?,
    })
}

/// One object script compiled with one set of parameter values.
pub struct Script {
    /// The file the project references it by.
    pub file: String,
    pub program: Program,
    /// The meshes it could import (a recompile with moved values uses the same).
    pub assets: Arc<Assets>,
    /// The imported meshes' painted leaves, shared through recompiles.
    pub facets: Arc<FacetAssets>,
    /// The pictures beside it its picture parts print (a recompile with moved values uses the same).
    pub pictures: Arc<crate::picture::Pictures>,
    /// The printer inputs it was compiled with (a recompile with moved values uses the same).
    pub context: CompileContext,
    /// The language environment it was compiled with, also used for moved values.
    pub env: ScriptEnv,
    pub objects: Vec<Emitted>,
    /// Where the `panel(...)` call is, when there is one.
    pub panel: Option<(u32, u32)>,
    /// Each emitted body's paint (`crate::paint`), by its solid; none for an unpainted one.
    pub paint: HashMap<usize, crate::paint::ObjectPaint>,
    /// The paint files it read (a recompile with moved values reads the same).
    pub paint_files: Arc<HashMap<String, String>>,
    /// Each emitted object's force study (`crate::study`), by object name; none without one.
    pub study: HashMap<String, Arc<crate::study::Study>>,
    /// The person's checks in the order the script makes them.
    pub expects: Vec<crate::expect::Expect>,
    /// The fields its graded and grown parts were compiled with (a recompile with moved values uses
    /// the same); a part without one asks in `program.stress_requests` or `program.grow_requests`.
    pub solved: Solved,
    /// The values used for both passes, including overrides in a live preview.
    pub(crate) overrides: HashMap<(u32, u32), f64>,
    /// The solids built as their input: a draft's statements that lost their selector
    /// ([`lang::evaluate_skipping`]).
    pub(crate) skip: Vec<String>,
    pub generated: HashMap<String, GenPart>,
    /// The combined request keys, including the panel bases and inputs read.
    pub generated_key: [u8; 32],
}

/// The fields solved for graded and grown parts, by part id ([`lang::Options::solved`]).
pub type Solved = HashMap<String, Arc<makepad_model_script::lang::csg_sdf::SdfGrid>>;

impl std::fmt::Debug for Script {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Script({}: {} solids, {} objects)", self.file, self.program.solids.len(), self.objects.len())
    }
}

impl Script {
    pub fn source(&self) -> &str {
        self.program.source()
    }
    pub fn object(&self, name: &str) -> Option<&Emitted> {
        self.objects.iter().find(|o| o.name == name)
    }
    /// The emitted object a solid prints in.
    pub fn object_of(&self, solid: usize) -> Option<&Emitted> {
        self.objects.iter().find(|o| o.bodies.contains(&solid))
    }
    /// A declared parameter's value as the script writes it (VM units) and its call's location.
    pub fn param(&self, name: &str) -> Option<(f64, (u32, u32))> {
        let q = self.program.params.iter().find(|q| q.name.as_deref() == Some(name))?;
        Some((q.call.value, q.call.loc?))
    }
}

fn diag(loc: Option<(u32, u32)>, message: impl Into<String>) -> Diag {
    let (line, col) = loc.unwrap_or((1, 1));
    Diag { line, col, message: message.into() }
}

/// sRGB 0xRRGGBBAA to linear RGBA.
pub fn color_linear(c: u32) -> [f32; 4] {
    let ch = |shift: u32| {
        let s = ((c >> shift) & 0xff) as f32 / 255.0;
        if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    [ch(24), ch(16), ch(8), (c & 0xff) as f32 / 255.0]
}

/// Linear RGB to sRGB 0xRRGGBBff.
pub fn color_srgb(rgb: [f32; 3]) -> u32 {
    let byte = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (e * 255.0).round() as u32
    };
    (byte(rgb[0]) << 24) | (byte(rgb[1]) << 16) | (byte(rgb[2]) << 8) | 0xff
}

/// Compile an object script with parameter values overridden (by the param call's location).
/// `files` reads the project's other files (the paint files a script names).
pub fn compile_with(file: &str, source: &str, names: Option<Arc<lang::names::Names>>, assets: &Arc<Assets>, facets: &Arc<FacetAssets>, pictures: &Arc<crate::picture::Pictures>, context: &CompileContext, env: &ScriptEnv, overrides: HashMap<(u32, u32), f64>, solved: &Solved, skip: &[String], files: &dyn Fn(&str) -> Option<String>) -> Result<Script, Vec<Diag>> {
    compile_generated(file, source, names, assets, facets, pictures, context, env, overrides, solved, skip, &HashMap::new(), files)
        .map_err(lang::EvalError::diagnostics)
}

pub(crate) fn compile_generated(file: &str, source: &str, names: Option<Arc<lang::names::Names>>, assets: &Arc<Assets>, facets: &Arc<FacetAssets>, pictures: &Arc<crate::picture::Pictures>, context: &CompileContext, env: &ScriptEnv, overrides: HashMap<(u32, u32), f64>, solved: &Solved, skip: &[String], generated: &HashMap<String, GenPart>, files: &dyn Fn(&str) -> Option<String>) -> Result<Script, lang::EvalError> {
    compile_document(file, source, names, assets, facets, pictures, context, env.clone(), overrides, solved, skip, generated, files)
}

pub(crate) fn compile_document(file: &str, source: &str, names: Option<Arc<lang::names::Names>>, assets: &Arc<Assets>, facets: &Arc<FacetAssets>, pictures: &Arc<crate::picture::Pictures>, context: &CompileContext, env: ScriptEnv, overrides: HashMap<(u32, u32), f64>, solved: &Solved, skip: &[String], generated: &HashMap<String, GenPart>, files: &dyn Fn(&str) -> Option<String>) -> Result<Script, lang::EvalError> {
    let images = crate::picture::sizes(pictures);
    let opts = Options { assets, images: &images, extension: "fab", libraries: &env.libraries, inputs: &env.inputs, unit: UNIT, roots: ROOTS, overrides: overrides.clone(), weave: Some(context.weave), solved: solved.clone(), generated };
    let program = opts.evaluate_with_imports(source, &env.imports, names, skip)?;
    // Jointed solids: one assembly per connected set.
    let n = program.solids.len();
    let mut set: Vec<usize> = (0..n).collect();
    fn find(set: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while set[r] != r {
            r = set[r];
        }
        set[i] = r;
        r
    }
    let mut jointed = vec![false; n];
    for j in &program.joints {
        jointed[j.a] = true;
        // a brake's shoe prints in the hinge's eye: a body of its assembly; a string joins nothing in print
        for b in j.b.into_iter().chain(j.shoe) {
            jointed[b] = true;
            if j.proto != "String" {
                let (ra, rb) = (find(&mut set, j.a), find(&mut set, b));
                set[ra] = rb;
            }
        }
    }
    let bodies_of = |set: &mut [usize], s: usize| -> Vec<usize> {
        if !jointed[s] {
            return vec![s];
        }
        let r = find(set, s);
        (0..n).filter(|&i| jointed[i] && find(set, i) == r).collect()
    };
    let mut objects: Vec<Emitted> = Vec::new();
    let mut panel = None;
    let mut errors = Vec::new();
    for root in &program.roots {
        match root.kind {
            "object" | "insert" => {
                let insert = if root.kind == "insert" {
                    let into = match root.get("into") {
                        Some(Arg::Value(lang::V::Str(s))) if !s.is_empty() => s.clone(),
                        _ => {
                            errors.push(diag(root.loc, "insert: it needs into: \"<object>\""));
                            continue;
                        }
                    };
                    let clearance = match root.get("clearance") {
                        None => 0.2,
                        Some(Arg::Value(lang::V::Num(n))) if n.is_finite() && *n >= 0.0 => n * UNIT,
                        _ => {
                            errors.push(diag(root.loc, "insert: 'clearance' must be a non-negative length"));
                            continue;
                        }
                    };
                    let embed = match root.get("embed") {
                        None => false,
                        Some(Arg::Value(lang::V::Bool(b))) => *b,
                        _ => {
                            errors.push(diag(root.loc, "insert: 'embed' must be true or false"));
                            continue;
                        }
                    };
                    let melt = match root.get("melt") {
                        None | Some(Arg::Value(lang::V::Nil)) => None,
                        Some(Arg::Value(lang::V::Obj(o))) if !embed => match melt_of(o) {
                            Ok(m) => Some(m),
                            Err(e) => {
                                errors.push(diag(root.loc, &format!("insert: 'melt' {e}")));
                                continue;
                            }
                        },
                        Some(Arg::Value(lang::V::Obj(_))) => {
                            errors.push(diag(root.loc, "insert: an item is printed into its part (embed) or melted in after the print (melt), not both"));
                            continue;
                        }
                        _ => {
                            errors.push(diag(root.loc, "insert: 'melt' takes a heat-set insert: melt: heat_insert(\"M3\", {at dir})"));
                            continue;
                        }
                    };
                    Some(InsertOf { into, clearance, embed, melt })
                } else { None };
                let listed = match root.get("solid") {
                    Some(Arg::Solid(solid)) => vec![*solid],
                    Some(Arg::Solids(solids)) => {
                        let mut each: Vec<usize> = Vec::with_capacity(solids.len());
                        for &s in solids {
                            if !each.contains(&s) {
                                each.push(s);
                            }
                        }
                        each
                    }
                    _ => {
                        errors.push(diag(root.loc, "object: the first argument is the solid the script emits, or a list of the bodies that print as one: object(bracket), object([head, hair], name: \"bust\")"));
                        continue;
                    }
                };
                let Some(solid) = listed.first() else {
                    errors.push(diag(root.loc, format!("{}: the solid list must not be empty", root.kind)));
                    continue;
                };
                let name = match root.get("name") {
                    None => program.solids[*solid].id.clone(),
                    Some(Arg::Value(lang::V::Str(s))) if !s.is_empty() && !s.contains('/') => s.clone(),
                    Some(_) => {
                        errors.push(diag(root.loc, "object: 'name' is text without '/'"));
                        continue;
                    }
                };
                if name.is_empty() {
                    errors.push(diag(root.loc, "object: bind the solid with a let, or give it a name: object(m.box(..), name: \"block\")"));
                    continue;
                }
                let mut bodies: Vec<usize> = Vec::new();
                for &s in &listed {
                    for b in bodies_of(&mut set, s) {
                        if !bodies.contains(&b) {
                            bodies.push(b);
                        }
                    }
                }
                if let Some(prev) = objects.iter().find(|o| o.name == name || o.bodies.iter().any(|b| bodies.contains(b))) {
                    errors.push(diag(root.loc, format!("'{name}' is emitted twice (first as '{}' at line {})", prev.name, prev.loc.map_or(0, |l| l.0))));
                    continue;
                }
                let glued = if listed.len() > 1 { listed.clone() } else { Vec::new() };
                objects.push(Emitted { name, link: program.solids[*solid].link.clone(), insert, solid: *solid, bodies, glued, loc: root.loc });
            }
            "panel" => panel = root.loc,
            _ => {}
        }
    }
    // A jointed assembly no object(...) names is emitted as it stands (it prints together).
    for j in &program.joints {
        if objects.iter().any(|o| o.bodies.contains(&j.a)) {
            continue;
        }
        let bodies = bodies_of(&mut set, j.a);
        let name = program.solids[bodies[0]].id.clone();
        objects.push(Emitted { name, link: program.solids[bodies[0]].link.clone(), insert: None, solid: bodies[0], bodies, glued: Vec::new(), loc: j.loc });
    }
    let hosts: std::collections::HashSet<&str> = objects.iter().filter(|o| o.insert.is_none()).map(|o| o.name.as_str()).collect();
    let available = objects.iter().filter(|o| o.insert.is_none()).map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ");
    for o in &objects {
        if o.insert.as_ref().is_some_and(|i| !hosts.contains(i.into.as_str())) {
            errors.push(diag(o.loc, format!("insert: 'into' names no object of this script (there is: {available})")));
        }
    }
    if !errors.is_empty() {
        return Err(lang::EvalError::Document(errors));
    }
    let read = std::cell::RefCell::new(HashMap::new());
    let reading = |f: &str| {
        let t = files(f);
        if let Some(t) = &t {
            read.borrow_mut().insert(f.to_string(), t.clone());
        }
        t
    };
    let paint = crate::paint::compile(&program, &objects, assets, facets, pictures, &reading)?;
    let paint_files = Arc::new(read.into_inner());
    let study = crate::study::compile(&program, &objects)?;
    let expects = crate::expect::compile(&program)?;
    Ok(Script { file: file.to_string(), program, assets: assets.clone(), facets: facets.clone(), pictures: pictures.clone(), context: *context, env, objects, panel, paint, paint_files, study, expects, solved: solved.clone(), overrides, skip: skip.to_vec(), generated: generated.clone(), generated_key: [0; 32] })
}

pub fn compile(file: &str, source: &str, assets: &Arc<Assets>, facets: &Arc<FacetAssets>, pictures: &Arc<crate::picture::Pictures>, context: &CompileContext, files: &dyn Fn(&str) -> Option<String>) -> Result<Script, Vec<Diag>> {
    compile_with(file, source, None, assets, facets, pictures, context, &ScriptEnv::default(), HashMap::new(), &Solved::new(), &[], files)
}

/// An `object(...)` call of a program: where the code of the solid it emits is written, the
/// name it emits, and its call when that name is its solid's builder and line (`box@6`: no
/// `name` and no `let`).
struct ObjectCall {
    solid: Range<usize>,
    emits: Option<String>,
    lined: Option<makepad_script::syntax::Loc>,
}

/// A program's object calls, in the order they are written.
fn object_calls(p: &Program) -> Vec<ObjectCall> {
    use makepad_script::syntax::{Kind, Loc};
    let objects: Vec<(&lang::Root, usize)> = p.roots.iter().filter(|r| matches!(r.kind, "object" | "insert")).filter_map(|r| Some((r, p.names.offset(r.loc?)?))).collect();
    // Each root's call: the first call node written there (at its name or its bracket).
    let wanted: std::collections::HashSet<usize> = objects.iter().map(|(_, at)| *at).collect();
    let mut calls: HashMap<usize, Loc> = HashMap::new();
    p.names.syntax.walk(&mut |loc, node| {
        if let Kind::Call { open, .. } = &node.kind {
            for at in [*open, node.span.start] {
                if wanted.contains(&at) {
                    calls.entry(at).or_insert_with(|| loc.clone());
                }
            }
        }
    });
    let mut out: Vec<ObjectCall> = objects.iter().filter_map(|(r, at)| {
        let call = calls.get(at)?.clone();
        let Kind::Call { args, .. } = &p.names.syntax.node(&call)?.kind else { return None };
        let solid = args.first()?.span.clone();
        let Some(Arg::Solid(i)) = r.get("solid") else { return None };
        let id = p.solids.get(*i)?.id.clone();
        Some(match r.get("name") {
            None if id.contains('@') => ObjectCall { solid, emits: Some(id), lined: Some(call) },
            None => ObjectCall { solid, emits: Some(id), lined: None },
            Some(Arg::Value(lang::V::Str(s))) => ObjectCall { solid, emits: Some(s.clone()), lined: None },
            Some(_) => ObjectCall { solid, emits: None, lined: None },
        })
    }).collect();
    out.sort_by_key(|o| o.solid.start);
    out.dedup_by_key(|o| o.solid.start);
    out
}

/// The naming moment of the objects known by their builder and line: `object(m.box(..))` emits
/// `box@6`, the name the project places it by, and a line added above it would make it `box@7`.
/// `after` is `before` after edits; the result is its source with each such object's name
/// written into its call (`object(m.box(..), name: "box@6")`), so that no line renames it
/// again, or None when the moment has not come. An object of `after` takes the line name the
/// object of `before` with the same solid code emitted (objects whose code changed pair in order
/// between those that kept theirs), else its own. The moment comes for all of a script's such
/// objects at once, when `keep` holds for one of their names; a name another object has or
/// takes first gets a number (`box@6 2`).
pub fn named_objects(before: &Program, after: &Program, keep: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
    let is_lined = |r: &lang::Root| matches!(r.kind, "object" | "insert") && r.get("name").is_none() && matches!(r.get("solid"), Some(Arg::Solid(i)) if after.solids.get(*i).is_some_and(|s| s.id.contains('@')));
    if !after.roots.iter().any(is_lined) {
        return Ok(None);
    }
    let new = object_calls(after);
    if new.iter().all(|o| o.lined.is_none()) {
        return Ok(None);
    }
    let old = object_calls(before);
    let code = |p: &Program, o: &ObjectCall| p.source()[o.solid.clone()].to_string();
    let a: Vec<String> = old.iter().map(|o| code(before, o)).collect();
    let b: Vec<String> = new.iter().map(|o| code(after, o)).collect();
    let mut was: Vec<Option<&str>> = vec![None; new.len()];
    for (i, j) in pair_in_order(&a, &b) {
        // A name of the line scheme, written or not; a chosen name the new code dropped is not.
        was[j] = old[i].emits.as_deref().filter(|n| n.contains('@'));
    }
    let mut taken: std::collections::BTreeSet<String> = new.iter().filter(|o| o.lined.is_none()).filter_map(|o| o.emits.clone()).collect();
    let mut lined: Vec<(bool, &str, &makepad_script::syntax::Loc)> = new.iter().zip(&was).filter_map(|(o, w)| Some((w.is_some(), (*w).or(o.emits.as_deref())?, o.lined.as_ref()?))).collect();
    if !lined.iter().any(|(_, want, _)| keep(want)) {
        return Ok(None);
    }
    // The ones that keep a name of `before` choose first.
    lined.sort_by_key(|(kept, _, _)| !kept);
    let syn = &after.names.syntax;
    let mut edits = Vec::new();
    for (_, want, loc) in lined {
        let name = match taken.contains(want) {
            true => (2..).map(|k| format!("{want} {k}")).find(|n| !taken.contains(n)).unwrap(),
            false => want.to_string(),
        };
        let Some(makepad_script::syntax::Kind::Call { args, .. }) = syn.node(loc).map(|n| &n.kind) else { return Err("an object call is not where the program says".into()) };
        // After the last argument, on its line: a name never moves a line.
        let sep = match args.as_slice() {
            [a, b, ..] if !after.source()[a.span.end..b.span.start].contains(',') => " ",
            _ => ", ",
        };
        let end = args.last().map_or(0, |a| a.span.end);
        edits.push(makepad_script::syntax::edit::Replace::new(end..end, format!("{sep}name: {}", makepad_script::syntax::write::quote(&name))));
        taken.insert(name);
    }
    syn.apply(&edits).map(|s| Some(s.source().to_string())).map_err(|e| e.message)
}

/// The print colours `source`'s materials name (`Material{name: "Eyes"}`, the text trimmed as
/// the paint reads it), in script order, each once.
pub fn material_names(source: &str) -> Vec<String> {
    let Ok(names) = lang::names::parse(source) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for (_, name) in material_name_sites(&names.syntax) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Each `Material{name: "..."}` of a script: the name's literal and the name it gives.
fn material_name_sites(syntax: &makepad_script::syntax::Syntax) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    syntax.walk(&mut |_, node| {
        let Some((Some(ty), _)) = node.object() else { return };
        if ty != "Material" && !ty.ends_with(".Material") {
            return;
        }
        if let Some(name) = node.field("name").and_then(|f| Some((f.value.span.clone(), f.value.string()?.trim().to_string()))) {
            out.push(name);
        }
    });
    out
}

/// `source` with its materials that name print colour `from` naming `to` (`None`: none names
/// it; a script that does not parse names none).
pub fn rename_material_names(source: &str, from: &str, to: &str) -> Result<Option<String>, String> {
    let Ok(names) = lang::names::parse(source) else { return Ok(None) };
    let edits: Vec<makepad_script::syntax::edit::Replace> = material_name_sites(&names.syntax)
        .into_iter()
        .filter(|(_, name)| name == from)
        .map(|(span, _)| makepad_script::syntax::edit::Replace::new(span, makepad_script::syntax::write::quote(to)))
        .collect();
    if edits.is_empty() {
        return Ok(None);
    }
    names.syntax.apply(&edits).map(|s| Some(s.source().to_string())).map_err(|e| e.message)
}

/// The pairs of equal texts of `a` and `b` (their longest common subsequence), and between two
/// such pairs the texts of a stretch as long on both sides, in order.
fn pair_in_order(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let at = |i: usize, j: usize| i * (m + 1) + j;
    let mut len = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            len[at(i, j)] = if a[i] == b[j] { len[at(i + 1, j + 1)] + 1 } else { len[at(i + 1, j)].max(len[at(i, j + 1)]) };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j, mut i0, mut j0) = (0, 0, 0, 0);
    let stretch = |out: &mut Vec<(usize, usize)>, i0: usize, j0: usize, i: usize, j: usize| {
        if i - i0 == j - j0 {
            out.extend((0..i - i0).map(|k| (i0 + k, j0 + k)));
        }
    };
    while i < n && j < m {
        if a[i] == b[j] {
            stretch(&mut out, i0, j0, i, j);
            out.push((i, j));
            (i, j) = (i + 1, j + 1);
            (i0, j0) = (i, j);
        } else if len[at(i + 1, j)] >= len[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    stretch(&mut out, i0, j0, n, m);
    out
}

/// How one body of a placed object prints.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// Its index in the script's solids.
    pub solid: usize,
    /// The emitted object it is a body of.
    pub object: String,
    pub name: Option<String>,
    /// Where the object's model origin sits on the bed (mm).
    pub at: Option<[f64; 2]>,
    /// Degrees about the printer's X, Y, Z.
    pub rotate: [f64; 3],
    /// Factors along the printer's X, Y, Z of the unturned object, before its turn.
    pub scale: [f64; 3],
    pub drop: bool,
    /// The print colour it prints as (none: the project's first).
    pub color: Option<String>,
    /// Its script's paint materials (linear RGB) and the print colours they print as.
    pub paint: Vec<([f32; 3], String)>,
    pub print: bool,
    /// A hidden host retained only to place its visible inserts.
    pub show: bool,
    /// Kept on the plate but out of the views (`hidden: true`).
    pub hidden: bool,
    pub settings: Vec<(String, String)>,
}

/// A script placed on the plate: its parameter values, its compiled script and how each
/// body of each object it emits prints.
#[derive(Clone, Debug)]
pub struct Instance {
    /// Unique in the project; plate parts are `instance/solid`.
    pub name: String,
    /// The ScriptRef whose parameters and compiled script this placement shares.
    pub script_instance: String,
    /// Original (0), or a linked clone.
    pub link: u32,
    pub script: Arc<Script>,
    /// One per body of every emitted object on the plate, in emission order.
    pub placed: Vec<Placed>,
}

impl Instance {
    pub fn placed_of(&self, solid: usize) -> Option<&Placed> {
        self.placed.iter().find(|p| p.solid == solid)
    }
}

/// A linked placement id's script instance and link (`bracket~2` -> (`bracket`, 2)).
/// A name without a positive integer suffix denotes the original (link 0).
pub fn split_link(instance: &str) -> (&str, u32) {
    if let Some((base, suffix)) = instance.rsplit_once('~') {
        if !base.is_empty() && !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(link) = suffix.parse::<u32>() {
                if link > 0 {
                    return (base, link);
                }
            }
        }
    }
    (instance, 0)
}

/// A placement's instance id; link 0 keeps the script instance's name.
pub fn link_name(base: &str, link: u32) -> String {
    if link == 0 { base.to_string() } else { format!("{base}~{link}") }
}

/// A plate part's id: its instance and its solid (`vase/vase`, `puppet/arm_l`).
pub fn part_id(instance: &str, solid: &str) -> String {
    format!("{instance}/{solid}")
}

/// An id's instance and its script-local rest (a solid, an object or a parameter key).
pub fn split_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('/')
}

/// The project compiled: every instance's script and placements.
#[derive(Clone, Debug, Default)]
pub struct Compiled {
    pub instances: Vec<Instance>,
    /// The project's global print settings.
    pub settings: Vec<(String, String)>,
    /// The project's print colours.
    pub colors: Vec<fab_plate::PrintColor>,
    /// The strokes files the scripts read (`m.strokes`), bound into their paint when it is
    /// evaluated: a stroke recompiles no script.
    pub strokes: crate::StrokeFiles,
}

impl Compiled {
    pub fn instance(&self, name: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.name == name)
    }
    /// The instance and solid index of a plate part id (`instance/solid`).
    pub fn part(&self, id: &str) -> Option<(&Instance, usize)> {
        let (inst, solid) = split_id(id)?;
        let i = self.instance(inst)?;
        let s = i.script.program.solids.iter().position(|s| s.id == solid)?;
        Some((i, s))
    }
    /// The instance and emitted object of a part id or an object id (`instance/object`).
    pub fn object(&self, id: &str) -> Option<(&Instance, &Emitted)> {
        let (inst, rest) = split_id(id)?;
        let i = self.instance(inst)?;
        let o = i.script.object(rest).or_else(|| {
            let s = i.script.program.solids.iter().position(|s| s.id == rest)?;
            i.script.object_of(s)
        })?;
        Some((i, o))
    }
}
