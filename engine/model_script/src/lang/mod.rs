//! The model language: a Splash program that builds parts, one builder call a part.
//!
//! ```splash
//! let m = mod.model
//! let height = param(70mm, min: 40mm, max: 120mm, label: "Height")
//! let body = m.extrude({sketch: Rect{size: [46mm 6mm]} plane: @xy height: 64mm both: true})
//! let bolts = for x in [-20mm 20mm] { m.cylinder({radius: 2mm height: 20mm at: [x 0 0]}) }
//! let bracket = m.subtract(m.fillet(body, 1.2mm), bolts)
//! ```
//!
//! A builder (`m.box`, `m.subtract`, ... [`protos`]) returns an opaque handle;
//! numbers and vectors stay values. [`evaluate`] runs the program in its own VM,
//! lowers every part, in the order the builders ran, to a node of the typed
//! solid graph (`makepad_csg_graph`): objects to typed ops, never text. A part
//! is named by the `let`s it is bound by (`bracket`, `bolts[1]`; a part made in
//! a fn by the call's binding first, `left.body`) and remembers the call it was
//! written as.
//! `m.link(part, "host:key")` returns that same part with a host-resolved key; a boolean
//! operand keeps its own key, and the host reads the key of the solid it emits. A part's
//! `frame: Frame{name at rotate}` is a named frame in its own frame that the host reads
//! (an enclosure's board frame); it moves with the part's placement.
//! A solid states how it carries light with `optics: {throughput: 0.2 source: @estimate}`
//! or `m.optics(part, {throughput: 0.2})`. This record stays on that solid, including when
//! it is a boolean operand; the boolean result does not inherit it. `label: "Light pipe D1"`
//! names a body for a person where its object lists several.
//! A solid reserves board space with `keepouts: [..]` or
//! `m.keepouts(part, [..])`. These records stay on that solid; their outlines use the
//! board's own coordinates. The height band is measured outward from the surface named
//! by `side: @top | @bottom`, defaulting to `@top`. An optional `feature` key ties the
//! record to its port's part. A record with `room: true` reserves space only against other rooms.
//!
//! Units: the VM holds lengths in metres and angles in radians (`150mm`,
//! `90°`); `Options::unit` scales lengths into the kernel's (1000 for a
//! millimetre kernel). Angles reach the kernel in degrees.
//!
//! A host can add [`Library`] sources, evaluated before the document in the same VM;
//! each library ends with an object whose fields become part of [`Options::extension`].
//! [`Inputs`] supply values by kind and name, with one host function per kind in that
//! module. Their sources are Splash expressions and their hashes let the host cache a
//! compile on the inputs it reads. [`Program::inputs_read`] records those reads once,
//! in first-read order. A host can register kinds whose values it builds only when a
//! script first asks for one ([`Inputs::lazy`]): registering costs the kind's name, and
//! the first read of a name builds it on the evaluating thread.
//!
//! `m.panel(base, {name: "lid" pose: frame cuts: [..]})` accepts host additions and cuts.
//! Generators query the base shell before these cuts. A host part can claim the keys of
//! cuts linked with `m.link(cut, key)`; only unclaimed cuts apply, and unkeyed cuts always apply.
//! `m.generated("kind", {panel: lid ..params})` requests a host-built part; it is empty
//! until a second evaluation supplies [`Options::generated`]. Builders and panel edits
//! use the model frame. Panel names default to their `let`; pose is passed through.
//! A panel without its own `frame` inherits its base's named frame.

pub mod edit;
mod generated;
pub mod heat_set;
pub mod screw;
pub use generated::{GenPart, GenRequest, GenSolid, PanelReq};
pub mod names;
pub mod protos;
mod isolate;
mod texture;
mod veins;
mod vm;
mod voronoi;

pub use isolate::ModuleIsolate;
pub use vm::{Obj, ParamCall, Reader, V};
pub use makepad_csg_sdf as csg_sdf;
pub use makepad_csg_graph as csg_graph;

use makepad_csg_graph::edges::{EdgeFilter, EdgeKind, EdgeName, EdgeSel, Face as FaceRole, FaceName, Role};
use makepad_csg_graph::{Align, BoolMode, BrepOp, SplitPiece, SplitPins, Font, SdfKernel, SdfNode, SdfShape, GearSpec, Graph, HoleKind, LoftSection, MirrorPlane, NodeId, Op, Open, Path3, Plane, Ring, Seg, Shape, Sketch, P3};
use makepad_csg_mech::weave::{self, Frame, SquareLink, Weave, WeaveProfile};
use names::Names;
use protos::{Kind, Proto};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// A problem at a 1-based line and column of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diag {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl Diag {
    fn is_array_minus_warning(&self) -> bool {
        self.message.starts_with('`') && self.message.contains("` inside [ ] reads as one value, ")
    }

    fn is_list_size_mismatch(&self) -> bool {
        let Some((_, sizes)) = self.message.split_once("' must be ") else { return false };
        let Some((expected, actual)) = sizes.split_once(" numbers [..], not a list of ") else { return false };
        expected.parse::<usize>().is_ok() && actual.parse::<usize>().is_ok()
    }
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Which source produced the diagnostics when a document inherits imports.
#[derive(Debug)]
pub enum EvalError {
    Imports(Vec<Diag>),
    Document(Vec<Diag>),
}

impl EvalError {
    pub fn diagnostics(self) -> Vec<Diag> {
        match self {
            Self::Imports(d) | Self::Document(d) => d,
        }
    }
}

impl From<Vec<Diag>> for EvalError {
    fn from(d: Vec<Diag>) -> Self {
        Self::Document(d)
    }
}

/// A root call the host offers (`place(solid, at: ..)`): its name and argument names.
#[derive(Clone, Copy, Debug)]
pub struct RootFn {
    pub name: &'static str,
    pub args: &'static [&'static str],
    /// On `mod.model` (`m.fill(..)`) rather than the host's module (`object(..)`): a statement
    /// about the model that is no solid (paint).
    pub model: bool,
}

/// Meshes the host has, by name (`m.import("cat")`).
pub type Assets = HashMap<String, Arc<makepad_csg_sdf::mesh_field::MeshField>>;

/// Pictures beside a script, by file name: width and height in pixels.
pub type Images = HashMap<String, [u32; 2]>;

/// Printer-derived weave lengths in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeaveDefaults {
    pub bar: f64,
    pub clearance: f64,
    pub clearance_z: f64,
}

/// A Splash library the host adds to the language (see module docs).
#[derive(Clone, Debug)]
pub struct Library {
    pub file: String,
    pub source: Arc<str>,
}

/// A value the host gives scripts: its Splash source (an expression) and its content hash.
#[derive(Clone, Debug)]
pub struct Input {
    pub source: Arc<str>,
    pub hash: [u8; 16],
}

/// An input call's record (`board("x", {..})`), as plain values: what a provider reads.
#[derive(Clone, Debug, PartialEq)]
pub enum RecordValue {
    /// A number as the script holds it (a length in metres: `10.9mm` is 0.0109).
    Num(f64),
    /// An id (`@micro`), without the `@`.
    Id(String),
    Text(String),
    Bool(bool),
    List(Vec<RecordValue>),
    Rec(std::collections::BTreeMap<String, RecordValue>),
}

impl RecordValue {
    /// The canonical text: keys sorted (BTreeMap order), numbers to 12 significant digits in
    /// exponent form (`format!("{:.11e}", n)`, with -0 written as 0), ids as `@id`, text quoted with
    /// `{:?}`, lists `[a, b]`, records `{k: v, k2: v2}`. Two records that hold the same values give
    /// the same text: `{height: 10.9mm}` and `{height: 1.09cm}` are one key.
    pub fn canonical(&self) -> String {
        match self {
            Self::Num(n) => format!("{:.11e}", if *n == 0.0 { 0.0 } else { *n }),
            Self::Id(id) => format!("@{id}"),
            Self::Text(text) => format!("{text:?}"),
            Self::Bool(flag) => flag.to_string(),
            Self::List(values) => format!("[{}]", values.iter().map(Self::canonical).collect::<Vec<_>>().join(", ")),
            Self::Rec(fields) => format!("{{{}}}", fields.iter().map(|(key, value)| format!("{key}: {}", value.canonical())).collect::<Vec<_>>().join(", ")),
        }
    }
}

/// Why a provider refuses a record: the key (dotted path into the record, `usb.height`) and what
/// is wrong with it. The script reports it at the call's line.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub key: String,
    pub message: String,
}

/// The name an input read is kept and stamped under: `name` alone without a record, else
/// `name` + " " + the record's canonical text. (The key type of Inputs::values and
/// Program::inputs_read stays (kind, String).)
pub fn input_key(name: &str, record: Option<&RecordValue>) -> String {
    match record {
        Some(record) => format!("{name} {}", record.canonical()),
        None => name.to_string(),
    }
}

/// Values the host gives scripts by kind and name.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    /// The host fns installed in the host's module, one per kind.
    pub kinds: Vec<String>,
    /// (kind, name) -> its value.
    pub values: std::collections::BTreeMap<(String, String), Input>,
    /// The values of `kinds` the host builds on first use: a read of a name `values` lacks asks
    /// it, once per evaluation (it keeps what it built).
    pub lazy: Option<Lazy>,
}

impl Inputs {
    /// What a cache keys the value of `key` by: its hash when `values` has it or the [`Lazy`]
    /// provider has built it (so a host that later gives the same value keys the same), else
    /// for a kind that provider serves, the provider (its values do not change while it lives),
    /// else nothing.
    pub fn stamp(&self, key: &(String, String)) -> Option<[u8; 16]> {
        if let Some(input) = self.values.get(key) {
            return Some(input.hash);
        }
        let lazy = self.lazy.as_ref().filter(|_| self.kinds.contains(&key.0))?;
        if let Some(hash) = lazy.provider.hash(&key.0, &key.1) {
            return Some(hash);
        }
        let mut stamp = [0xff; 16];
        stamp[8..].copy_from_slice(&lazy.id.to_le_bytes());
        Some(stamp)
    }
}

/// What builds a host's lazy input values ([`Inputs::lazy`]). It is called on the thread
/// that evaluates the script that reads one, and keeps what it built: its values are fixed
/// for its life, so a host whose values change makes another.
pub trait Provide: Send + Sync {
    /// `kind`'s value named `name`, built now if it was not before; None when it has none.
    fn provide(&self, kind: &str, name: &str) -> Option<Input>;
    /// `kind`'s value named `name` built for `record` (a `Rec`), built now if it was not before.
    /// Ok(None) when it has no such name; Err when it refuses the record. A provider keeps what it
    /// built under `input_key(name, Some(record))`, which `hash` is then asked for. Default: refuses
    /// every record (key "" and the message "{kind} takes no options").
    fn provide_with(&self, kind: &str, name: &str, record: &RecordValue) -> Result<Option<Input>, Refusal> {
        let _ = (name, record);
        Err(Refusal { key: String::new(), message: format!("{kind} takes no options") })
    }
    /// Why `kind` has no value named `name` when the provider knows one (its own source does
    /// not build), asked only after `provide` gave None. Default: None (the name is not one it
    /// has, and the message lists `names`).
    fn refused(&self, kind: &str, name: &str) -> Option<Refusal> {
        let _ = (kind, name);
        None
    }
    /// The names `kind` has, for the message a name it lacks gets.
    fn names(&self, kind: &str) -> Vec<String>;
    /// The hash of `kind`'s value named `name` if it has built it (it builds nothing for this).
    fn hash(&self, kind: &str, name: &str) -> Option<[u8; 16]>;
}

/// A [`Provide`] with its identity: two of them never share an id, so a cache that keys on
/// it never serves one provider's values for another's.
#[derive(Clone)]
pub struct Lazy {
    pub id: u64,
    pub provider: Arc<dyn Provide>,
}

impl Lazy {
    pub fn new(provider: Arc<dyn Provide>) -> Lazy {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Lazy { id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed), provider }
    }
}

impl std::fmt::Debug for Lazy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lazy({})", self.id)
    }
}

/// No inputs: what a host without any passes.
pub static NO_INPUTS: Inputs = Inputs { kinds: Vec::new(), values: std::collections::BTreeMap::new(), lazy: None };

pub struct Options<'a> {
    /// Host-built parts, keyed by the requested part id. Empty on pass one.
    pub generated: &'a HashMap<String, GenPart>,
    /// Meshes `Import` names.
    pub assets: &'a Assets,
    /// The pictures `Picture` names, by file: their size in pixels (the host reads them).
    pub images: &'a Images,
    /// The module the host's root calls live in (`fab` for `use mod.fab.*`).
    pub extension: &'a str,
    pub libraries: &'a [Library],
    pub inputs: &'a Inputs,
    /// Kernel units per metre.
    pub unit: f64,
    pub roots: &'a [RootFn],
    /// Param values to use instead of the written ones, by the call's (line, column).
    pub overrides: HashMap<(u32, u32), f64>,
    /// Printer-derived weave lengths; None requires explicit bar and clearances.
    pub weave: Option<WeaveDefaults>,
    /// The fields a host solved for the parts that ask, by the part's id. A grown part's
    /// (`m.grow`): its shape as a distance, in the body's frame ([`GrowRequest`]). A graded part's
    /// (`m.voronoi(.., {adapt: @stress})`): von Mises over the part's reference stress, sampled in the part's frame on the
    /// grid its [`StressRequest`] asked for. A graded part without one builds ungraded and asks.
    pub solved: HashMap<String, Arc<csg_sdf::SdfGrid>>,
}

/// A grown part's ask for its shape: the shape in `source`, over `lo..hi` (the body's frame: inside
/// the part's `SdfNode::Id` and its own placement), passed back in [`Options::solved`] under `part`.
#[derive(Clone, Debug)]
pub struct GrowRequest {
    /// The grown part's id (its `let` name).
    pub part: String,
    pub lo: P3,
    pub hi: P3,
    /// The share of `source` it keeps.
    pub mass: f64,
    /// Its thinnest member, kernel units.
    pub member: f64,
    /// The space it grows in, solid, in the body's frame.
    pub source: SdfNode,
}

/// A part's ask for a slime mould's veins raised on its source (`m.veins`): grow `pattern` on
/// `source` (the body's frame: inside the part's `SdfNode::Id` and its own placement; `lo..hi` its
/// box), raise a tube on each vein and pass back those that print without support in
/// [`Options::solved`] under `part`, as a grid's table of twelve words a tube in the source's
/// units: a, ra, b, rb (a round cone) and the normal and offset of the plane it is cut off under
/// the skin by (what is past `dot(p, n) = offset` away from `n` goes).
#[derive(Clone, Debug)]
pub struct VeinsRequest {
    /// The veined part's id (its `let` name).
    pub part: String,
    pub lo: P3,
    pub hi: P3,
    /// The part's source, solid, in the body's frame.
    pub source: SdfNode,
    /// The slime mould, as written (`Physarum{..}`).
    pub pattern: PatternArg,
}

/// A graded part's ask for the stress in its source (a solve of the object with `source` in its
/// body's place): sample it on a grid over `lo..hi` (the body's frame: inside the part's
/// `SdfNode::Id` and its own placement) at `spacing`, and pass it back in [`Options::solved`]
/// under `part`.
#[derive(Clone, Debug)]
pub struct StressRequest {
    /// The graded part's id (its `let` name).
    pub part: String,
    pub lo: P3,
    pub hi: P3,
    pub spacing: f64,
    /// The part's source, solid, in the body's frame.
    pub source: SdfNode,
}

/// A declared parameter.
#[derive(Clone, Debug)]
pub struct Param {
    /// The `let` it is bound to.
    pub name: Option<String>,
    pub call: ParamCall,
    /// The bytes of its first argument (the value as written).
    pub value_span: Option<Range<usize>>,
}

/// The top-level call of a script fn that a part was made inside (`let bow = bow_loop(..)`).
/// The fn's code is every call's, so an edit of the part edits this call, not the fn.
#[derive(Clone, Debug, PartialEq)]
pub struct FnCall {
    /// The call's bytes.
    pub span: Range<usize>,
    /// The calls on the way from it to the part, outermost first: each one written in the code of
    /// the fn the call before it runs.
    pub inner: Vec<Range<usize>>,
    /// The part is the call's value: the call's `let` names it (`bow`), not a part made on
    /// the way (`bow.ring`).
    pub value: bool,
}

/// This records how light passes through a solid from a source to its exit face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Optics {
    /// This is the share of the source's flux that leaves the exit face, from 0 to 1.
    pub throughput: f64,
    /// This says whether the throughput is an estimate or a measurement.
    pub source: OpticsSource,
}

/// This identifies how a solid's optical throughput was determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpticsSource {
    /// The throughput is an estimate.
    Estimate,
    /// The throughput is a measurement.
    Measured,
}

/// The board surface from which a keepout's height band is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepoutSide {
    Top,
    Bottom,
}

/// This records space on a board that this solid keeps clear of other parts.
#[derive(Clone, Debug, PartialEq)]
pub struct Keepout {
    /// This names the board by its unit name.
    pub board: String,
    /// This names the thing keeping the space clear, with one record per thing.
    pub name: String,
    /// The host key of the port whose part carries this record, if any.
    pub feature: Option<String>,
    /// A room reserves its outline only against other rooms.
    pub room: bool,
    /// This is the outline in the board's own frame, in kernel units, with the board's
    /// x and y coordinates.
    pub area: Vec<[f64; 2]>,
    /// The board surface the band is measured from (`@top` unless the record says `@bottom`).
    pub side: KeepoutSide,
    /// This is the occupied band `[low, high]`, in kernel units, measured outward from the
    /// board's surface on `side`.
    pub z: [f64; 2],
}

/// One solid of the program.
#[derive(Clone, Debug)]
pub struct Solid {
    pub id: String,
    pub proto: &'static str,
    /// The mesh asset named by an Import, none for other solids.
    pub asset: Option<String>,
    /// Its node (after its placement, when it has one).
    pub node: NodeId,
    pub loc: Option<(u32, u32)>,
    /// The bytes it was written in.
    pub span: Option<Range<usize>>,
    /// Made inside a fn of the script: the call at the top level it came from.
    pub call: Option<FnCall>,
    pub parent: Option<usize>,
    /// The feature tree puts each solid under its first user.
    pub children: Vec<usize>,
    /// The solids this step reads, in argument order, including repeated, shared and
    /// bypassed operands. A value that never lowered to a solid has no entry.
    pub inputs: Vec<usize>,
    /// The segment names of an exact generator's direct Path, indexed by its side faces.
    pub sides: Vec<Option<String>>,
    /// Named children whose name or segment object occurs twice, in ascending side order.
    ambiguous_sides: Vec<usize>,
    /// Named by a `let`.
    pub named: bool,
    /// Bypassed when used as a boolean operand.
    pub off: bool,
    /// An opaque key resolved by the host, attached with `m.link`.
    pub link: Option<String>,
    /// The name a person reads for this body where its object lists several (`label: "Light
    /// pipe D1"`); `None` leaves the host its default.
    pub label: Option<String>,
    /// This records how this solid carries light, without inheriting an operand's optics.
    pub optics: Option<Optics>,
    /// These are the spaces this solid keeps clear, without inheriting an operand's keepouts.
    pub keepouts: Vec<Keepout>,
    /// A named frame for the host (`frame: Frame{name at rotate}`): its name, origin (kernel
    /// units) and rotation (degrees) in the part's own frame, before its own placement.
    /// A panel inherits its base's frame unless it supplies its own.
    pub frame: Option<(String, P3, P3)>,
    /// A picture tile or a colour swatch: what its paint is made of.
    pub picture: Option<PictureArg>,
    /// Where each selector a B-rep step names its body's edges or faces by was written (line,
    /// column of `m.edge(..)` or `b.top`), in the order the step takes them: a lost one's text
    /// is replaced there. `None`: made where no line of the script shows it.
    pub selectors: Vec<Option<(u32, u32)>>,
}

impl Solid {
    /// A child name that selects this side unambiguously when written back to the sketch.
    pub fn side_name(&self, index: usize) -> Option<&str> {
        let name = self.sides.get(index)?.as_deref()?;
        self.ambiguous_sides.binary_search(&index).is_err().then_some(name)
    }
}

/// A flat part printed in full colour from the loaded filaments (`m.picture`), or the swatch
/// that measures them (`m.colour_swatch`): what the host makes its paint from.
#[derive(Clone, Debug)]
pub struct PictureArg {
    /// The picture's file beside the script; `None` for a swatch.
    pub image: Option<String>,
    /// Its plan size (kernel units): along x and along z.
    pub size: [f64; 2],
    pub layers: u32,
    /// Each layer's height (kernel units).
    pub layer: f64,
    /// The first layer is the face (printed face down, mirrored).
    pub face_down: bool,
    /// The shortest run of one filament along a line, in beads.
    pub min_run: u32,
    /// How much fewer changes of filament weigh against colour (0..1).
    pub smooth: f64,
    /// A calibrated filament set by name.
    pub set: Option<String>,
    /// Materials for a first guess before any calibration.
    pub filaments: Vec<V>,
}

/// A root call's argument.
#[derive(Clone, Debug)]
pub enum Arg {
    Solid(usize),
    /// A list of solids (`let holes = for .. { m.cylinder(..) }`).
    Solids(Vec<usize>),
    /// A `Mix{materials field}`: its field compiled.
    Mix(MixArg),
    Value(V),
}

/// What gives a `Mix{..}` its place at a point.
#[derive(Clone, Debug)]
pub enum MixField {
    /// The fn of a point (metres, the model frame) giving its place in `materials`: 0 the
    /// first, 1 the second, a fraction between two a blend of them.
    Fn(SdfKernel),
    /// A pattern grown on the part (`Turing{..}`, `Lichtenberg{..}`, `Scales{..}`,
    /// `Physarum{..}`, `Flow{..}`), 0..1 over the materials.
    Pattern(PatternArg),
}

/// A grown pattern as written (`Turing{preset: @spots scale: 5mm}`): its kind, its preset and
/// the numbers it gives (lengths in millimetres), for the host to grow.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternArg {
    pub kind: &'static str,
    pub preset: &'static str,
    pub values: Vec<(&'static str, f64)>,
    pub loc: Option<(u32, u32)>,
}

/// A `Mix{..}` as a root call takes it (`m.fill(vase, Mix{..})`).
#[derive(Clone, Debug)]
pub struct MixArg {
    pub field: MixField,
    pub materials: Vec<V>,
    /// 0: a blend turns as the field gives it; 1: at once halfway.
    pub sharp: f64,
    /// Added to every place.
    pub balance: f64,
}

/// A root call as the host reads it.
#[derive(Clone, Debug)]
pub struct Root {
    pub kind: &'static str,
    pub args: Vec<(String, Arg)>,
    pub loc: Option<(u32, u32)>,
}

impl Root {
    pub fn get(&self, key: &str) -> Option<&Arg> {
        self.args.iter().find(|(k, _)| k == key).map(|(_, a)| a)
    }
}

/// A joint between two solids (`m.hinge`, `m.ball`, `m.slider`, `m.bearing`, one per link pair of
/// `m.chain`, one per part of `m.bond`, its `a` the part they are bonded to), an anchor (`m.anchor`) or
/// a string (`m.string`): it changes the solids it names when the host builds them, it is not a solid
/// itself.
#[derive(Clone, Debug, PartialEq)]
pub struct JointCall {
    /// Named by its `let` (`elbow`), a chain's joints by the chain's name and index (`chain[2]`).
    pub id: String,
    /// `Hinge`, `BallJoint`, `Slider`, `Bearing`, `Bond`, `Anchor` or `String`.
    pub proto: &'static str,
    pub a: usize,
    /// None for an anchor.
    pub b: Option<usize>,
    /// Kernel units; none: where the two parts meet (a chain's joints). A string's start.
    pub at: Option<P3>,
    /// A string's end (kernel units).
    pub to: Option<P3>,
    /// Unit-free direction.
    pub axis: Option<P3>,
    /// A hinge's brake shoe part (`shoe:`).
    pub shoe: Option<usize>,
    /// The other fields that were given: lengths in kernel units, angles in radians, flags 0/1.
    pub values: Vec<(&'static str, f64)>,
    pub mode: &'static str,
    /// A hinge's or slider's motor: `position` or `speed`.
    pub motor: Option<&'static str>,
    pub loc: Option<(u32, u32)>,
    pub span: Option<Range<usize>>,
}

impl JointCall {
    pub fn value(&self, key: &str) -> Option<f64> {
        self.values.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

/// An evaluated program.
pub struct Program {
    /// Host inputs read by (kind, name), once each in first-read order.
    pub inputs_read: Vec<(String, String)>,
    /// What a person should know of how it builds, not an error.
    pub notes: Vec<Diag>,
    pub graph: Graph,
    pub panels: Vec<PanelReq>,
    pub gen_requests: Vec<GenRequest>,
    /// The filament per material slot of each Field node; slot 0 is the default.
    pub material_filaments: HashMap<NodeId, Vec<Option<u32>>>,
    pub solids: Vec<Solid>,
    /// Joints and anchors, in the order they were written.
    pub joints: Vec<JointCall>,
    pub params: Vec<Param>,
    pub roots: Vec<Root>,
    pub names: Arc<Names>,
    /// The graded parts' asks for their stress, whether or not one was passed in.
    pub stress_requests: Vec<StressRequest>,
    /// The grown parts' asks for their shape, likewise.
    pub grow_requests: Vec<GrowRequest>,
    /// The veined parts' asks for their tubes, likewise.
    pub veins_requests: Vec<VeinsRequest>,
    /// The instructions the script ran, of its budget [`EVAL_LIMIT`].
    pub instructions: u64,
    /// The calls the script left for its host (`m.host_call`), in the order they were made.
    pub host_calls: Vec<HostCall>,
}

/// The keepout records in `list` (the list a solid's `keepouts` field takes, read from a
/// [`ModuleIsolate`] call's result for one), as a solid's own are read: lengths in `unit` per metre,
/// the same validation and the same errors.
pub fn read_keepouts(list: &V, unit: f64) -> Result<Vec<Keepout>, Diag> {
    let holder = Obj { ident: 0, part: None, proto: None, fields: vec![("keepouts".to_string(), list.clone())], items: Vec::new(), children: Vec::new(), loc: None };
    let proto = &protos::PROTOS[0];
    Fields { obj: &holder, proto, unit }.keepouts().map_err(|mut d| {
        d.message = d.message.strip_prefix(&format!("{}: ", proto.name)).map(str::to_string).unwrap_or(d.message);
        d
    })
}

/// A library fn the script left for its host to call again (`m.host_call(light_plan, {..},
/// board: "unit")`), with the arguments the script called it with: plain values, by the fn's
/// parameter names. The host adds the arguments it owns and calls it in a [`ModuleIsolate`].
#[derive(Clone, Debug)]
pub struct HostCall {
    /// The unit the call is about.
    pub board: String,
    /// The name the library exports the fn under.
    pub function: String,
    pub args: Vec<(String, V)>,
    pub loc: Option<(u32, u32)>,
}

pub use vm::EVAL_LIMIT;

impl Program {
    pub fn solid(&self, id: &str) -> Option<&Solid> {
        self.solids.iter().find(|s| s.id == id)
    }
    pub fn source(&self) -> &str {
        self.names.syntax.source()
    }
}

thread_local! {
    /// [`work_done`]'s counts on this thread.
    static WORK: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// How many program evaluations and syntax parses of a program's names this thread has run so
/// far. A test reads it before and after a path to check how much work the path does by count,
/// not by timing.
pub fn work_done() -> (u64, u64) {
    WORK.with(|w| w.get())
}

fn count_work(evaluations: u64, parses: u64) {
    WORK.with(|w| {
        let (e, p) = w.get();
        w.set((e + evaluations, p + parses));
    });
}

/// A program's creation sites, from its syntax alone.
pub fn names_of(source: &str) -> Result<Arc<Names>, Vec<Diag>> {
    names::parse(source).map(Arc::new).map_err(|e| {
        let n = names::Names::line_col_of(source, e.at);
        vec![Diag { line: n.0, col: n.1, message: e.message }]
    })
}

/// Evaluate a program and lower its solids.
pub fn evaluate(source: &str, opts: &Options) -> Result<Program, Vec<Diag>> {
    evaluate_with_names(source, opts, None)
}

/// Evaluate again with prior creation sites when their source text is exactly unchanged.
/// Changed source is parsed afresh, including edits that preserve its length or locations.
pub fn evaluate_with_names(source: &str, opts: &Options, prior: Option<Arc<Names>>) -> Result<Program, Vec<Diag>> {
    evaluate_skipping(source, opts, prior, &[])
}

/// [`evaluate_with_names`], the B-rep steps of the solids in `skip` lowered as their input (a
/// draft whose statement lost its selector builds as if that statement were not there; the
/// solid keeps its id, line and selectors).
pub fn evaluate_skipping(source: &str, opts: &Options, prior: Option<Arc<Names>>, skip: &[String]) -> Result<Program, Vec<Diag>> {
    opts.evaluate_with_imports(source, &[], prior, skip).map_err(EvalError::diagnostics)
}

impl Options<'_> {
    /// Evaluate with the enclosing document's ordered `use` statements in a parent scope.
    pub fn evaluate_with_imports(&self, source: &str, imports: &[String], prior: Option<Arc<Names>>, skip: &[String]) -> Result<Program, EvalError> {
        let names = match prior.filter(|names| names.syntax.source() == source) {
            Some(names) => names,
            None => names_of(source)?,
        };
        count_work(1, 0);
        let ev = vm::evaluate(source, self, imports)?;
        lower_program(ev, self, names, skip).map_err(EvalError::Document)
    }
}

fn lower_program(mut ev: vm::Evaluated, opts: &Options, names: Arc<Names>, skip: &[String]) -> Result<Program, Vec<Diag>> {
    let ctx = ev.ctx.clone();
    let instructions = ev.instructions;
    let mut part_names = part_names(&names, &ctx.borrow().parts);
    let out = ev.with_vm(|vm| {
        let ctx = ctx.borrow();
        let objects = ctx.parts.iter().map(|p| p.object).collect();
        let mut reader = Reader::new(vm, objects);
        // A root's `name` names the part it takes when no `let` does: `object(m.box(..), name:
        // "lid")` is `lid`, on whatever line it is written.
        for call in &ctx.roots {
            let (Some((_, part)), Some((_, name))) = (call.args.first(), call.args.iter().find(|(k, _)| k == "name")) else { continue };
            let (V::Obj(o), V::Str(name)) = (reader.read(*part), reader.read(*name)) else { continue };
            if let Some(part) = o.part.filter(|p| part_names[*p].is_empty()) {
                if !name.is_empty() && !name.contains('/') && !part_names.contains(&name) {
                    part_names[part] = name;
                }
            }
        }
        let named_lines = part_names.iter().filter(|n| n.contains('@')).cloned().collect();
        let fn_calls = fn_calls(&names, &ctx.parts, &part_names);
        let mut l = Lower { reader, graph: Graph::new(), solids: Vec::new(), notes: Vec::new(), memo: HashMap::new(), path_segments: HashMap::new(), profile_paths: HashMap::new(), scaled_holes: HashMap::new(), bodies: HashMap::new(), lowering: Vec::new(), roots: std::collections::HashSet::new(), names: &names, part_names: &part_names, pieces: ctx.parts.iter().map(|p| p.piece).collect(), fn_calls: &fn_calls, named_lines, unit: opts.unit, assets: opts.assets, images: opts.images, materials: Vec::new(), material_filaments: HashMap::new(), weave: opts.weave, texture_lib: ctx.texture.as_ref().map(|r| r.as_object()), solved: &opts.solved, stress_requests: Vec::new(), grow_requests: Vec::new(), veins_requests: Vec::new(), generated: opts.generated, panels: Vec::new(), gen_requests: Vec::new(), gen_ids: HashMap::new(), skip };
        for call in &ctx.roots {
            for (_, value) in &call.args {
                let v = l.reader.read(*value);
                for v in flat(&v) {
                    if let Some(part) = v.obj().and_then(|o| o.part) {
                        l.roots.insert(part);
                    }
                }
            }
        }
        l.prepare_generated(&ctx.parts).map_err(|e| vec![e])?;
        let mut errors = Vec::new();
        let mut joints = Vec::new();
        // Every part, in the order it was built (a part only takes earlier ones); joints name parts.
        for i in 0..ctx.parts.len() {
            let v = l.reader.part(i);
            let is_joint = matches!(&v, V::Obj(o) if o.proto.as_deref().and_then(protos::proto).is_some_and(|p| p.group == protos::Group::Joint));
            let r = if is_joint { l.joint(&v, i, &mut joints) } else { l.solid(&v).map(|_| ()) };
            if let Err(e) = r {
                errors.push(e);
                break;
            }
        }
        let mut roots = Vec::new();
        for call in &ctx.roots {
            let mut args = Vec::new();
            for (key, value) in &call.args {
                let v = l.reader.read(*value);
                let arg = match &v {
                    V::Obj(o) if o.part.is_some() => match l.solid(&v) {
                        Ok(i) => Arg::Solid(i),
                        Err(e) => {
                            errors.push(e);
                            continue;
                        }
                    },
                    V::Obj(o) if o.proto.as_deref() == Some("Mix") => match l.mix(o) {
                        Ok(m) => Arg::Mix(m),
                        Err(e) => {
                            errors.push(e);
                            continue;
                        }
                    },
                    V::Arr(items) if !items.is_empty() && items.iter().all(|x| matches!(x, V::Obj(o) if o.part.is_some())) => {
                        let mut ids = Vec::with_capacity(items.len());
                        for x in items {
                            match l.solid(x) {
                                Ok(i) => ids.push(i),
                                Err(e) => errors.push(e),
                            }
                        }
                        Arg::Solids(ids)
                    }
                    _ => Arg::Value(v),
                };
                args.push((key.clone(), arg));
            }
            roots.push(Root { kind: call.kind, args, loc: call.loc });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok((l.graph, l.material_filaments, l.solids, roots, joints, l.notes, l.stress_requests, l.grow_requests, l.veins_requests, l.panels, l.gen_requests))
    }).map_err(|mut errors| {
        ev.warnings.retain(Diag::is_array_minus_warning);
        if ev.warnings.is_empty() {
            return errors;
        }
        for error in errors.iter_mut().filter(|e| e.is_list_size_mismatch()) {
            // Fields::nums reports the builder's creation site. Its syntax span also
            // covers arguments on later lines, up to the end of this particular call.
            let Some(site) = names.site(Some((error.line, error.col))) else { continue };
            let span = names.line_col(site.span.start)..names.line_col(site.span.end);
            for warning in ev.warnings.iter().filter(|w| span.contains(&(w.line, w.col))) {
                error.message.push_str(&format!(". Array warning at {warning}"));
            }
        }
        errors.append(&mut ev.warnings);
        errors
    })?;
    let (graph, material_filaments, solids, roots, joints, mut notes, stress_requests, grow_requests, veins_requests, panels, gen_requests) = out;
    let recorded: Vec<_> = ctx.borrow().host_calls.iter().map(|c| (c.board.clone(), c.function.clone(), c.args, c.loc)).collect();
    let mut host_calls = Vec::with_capacity(recorded.len());
    let mut call_errors = Vec::new();
    ev.with_vm(|vm| {
        let mut reader = Reader::new(vm, Vec::new());
        for (board, function, args, loc) in recorded {
            let at = loc.unwrap_or((1, 1));
            let args = match reader.read(args) {
                V::Obj(o) if o.proto.is_none() && o.items.is_empty() => o.fields.clone(),
                _ => {
                    call_errors.push(Diag { line: at.0, col: at.1, message: "m.host_call's arguments are a record of plain values".into() });
                    continue;
                }
            };
            match args.iter().try_for_each(|(k, v)| isolate::plain(v).map_err(|why| format!("m.host_call argument '{k}' {why}"))) {
                Ok(()) => host_calls.push(HostCall { board, function, args, loc }),
                Err(message) => call_errors.push(Diag { line: at.0, col: at.1, message }),
            }
        }
    });
    if !call_errors.is_empty() {
        return Err(call_errors);
    }
    notes.splice(0..0, ev.warnings);
    let params = ctx.borrow().params.iter().map(|call| {
        let site = names.site(call.loc);
        let value_span = call.loc.and_then(|loc| names.offset(loc)).and_then(|at| names.param_value_span(at));
        Param { name: site.and_then(|s| s.name.clone()), call: call.clone(), value_span }
    }).collect();
    let inputs_read = ctx.borrow().inputs_read.clone();
    Ok(Program { graph, material_filaments, solids, joints, params, roots, names, notes, stress_requests, grow_requests, veins_requests, panels, gen_requests, instructions, inputs_read, host_calls })
}

/// Each part made inside a script fn: the call at the top level it came from, and whether the
/// part is that call's value (the call's `let` alone names it).
fn fn_calls(names: &Names, parts: &[vm::PartCall], part_names: &[String]) -> Vec<Option<FnCall>> {
    parts
        .iter()
        .zip(part_names)
        .map(|(p, name)| {
            let site = names.call_site(*p.stack.first()?)?;
            let value = site.owns && matches!(site.binds.as_slice(), [b] if !b.looped && b.name == *name);
            let inner = p.stack[1..].iter().filter_map(|l| Some(names.call_site(*l)?.span)).collect();
            Some(FnCall { span: site.span, inner, value })
        })
        .collect()
}

/// Each part's name: the `let`s round the calls it was made inside, then round its own
/// builder call (`left.body`), indexed where a `let` holds a list (`bolts[2]`). A
/// name two parts share without a list goes to the last (the value the `let` or
/// call ends with); the earlier ones get their builder after it (`bracket.fillet`),
/// then a count.
/// The `let` names a part is made under: those of the script call sites it was made through,
/// outermost first, then its own site's.
fn part_binds(names: &Names, p: &vm::PartCall) -> Vec<names::Bind> {
    let mut binds = Vec::new();
    for at in &p.stack {
        if let Some(site) = names.call_site(*at) {
            binds.extend(site.binds);
        }
    }
    if let Some(site) = names.site(p.loc) {
        binds.extend(site.binds.iter().cloned());
    }
    binds
}

fn part_names(names: &Names, parts: &[vm::PartCall]) -> Vec<String> {
    let segs: Vec<Vec<names::Bind>> = parts.iter().map(|p| part_binds(names, p)).collect();
    let base = |b: &[names::Bind]| b.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(".");
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    let mut count: HashMap<String, usize> = HashMap::new();
    for b in &segs {
        *count.entry(base(b)).or_default() += 1;
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (b, p) in segs.iter().zip(parts) {
        let key = base(b);
        let n = seen.entry(key.clone()).or_default();
        let k = *n;
        *n += 1;
        let total = count[&key];
        let name = match b.iter().position(|x| x.looped) {
            Some(at) if total > 1 => b.iter().enumerate().map(|(j, x)| if j == at { format!("{}[{k}]", x.name) } else { x.name.clone() }).collect::<Vec<_>>().join("."),
            // a piece of a call that gives a list (`let halves = m.split(..)`: `halves[1]`)
            _ if p.piece.is_some() && !key.is_empty() => format!("{key}[{}]", p.piece.unwrap_or(0)),
            _ if key.is_empty() => String::new(),
            _ if k + 1 == total => key,
            _ => {
                let first = format!("{key}.{}", p.call);
                (1..).map(|n| if n == 1 { first.clone() } else { format!("{first}#{n}") }).find(|c| !taken.contains(c)).unwrap()
            }
        };
        taken.insert(name.clone());
        out.push(name);
    }
    out
}

/// One part or a list of them, lists of lists flattened (`[holes bores]`).
fn flat(v: &V) -> Vec<&V> {
    match v {
        V::Arr(list) => list.iter().flat_map(flat).collect(),
        one => vec![one],
    }
}

struct Lower<'a, 'v, 'b> {
    reader: Reader<'v, 'b>,
    graph: Graph,
    solids: Vec<Solid>,
    memo: HashMap<usize, usize>,
    /// Local side indices by Path and segment heap identity. A list can reuse an object.
    path_segments: HashMap<usize, HashMap<usize, Vec<usize>>>,
    /// Each exact generator's Paths and their starting side indices in the flattened sketch.
    profile_paths: HashMap<usize, Vec<(usize, Option<usize>)>>,
    /// Hole steps drilled under a scaled placement, by the placement key they bypassed.
    scaled_holes: HashMap<csg_graph::Key, HashSet<NodeId>>,
    /// Each part's distance field before its placement, by part: lowered once, however many
    /// blends take it.
    bodies: HashMap<usize, (SdfNode, ([f64; 3], [f64; 3]))>,
    /// The solids being lowered, innermost last: a distance-field part's operands are its
    /// children as a boolean's are.
    lowering: Vec<usize>,
    /// Emitted solids keep their own operation even when their `off` field is set.
    roots: std::collections::HashSet<usize>,
    names: &'a Names,
    part_names: &'a [String],
    /// Each part's place in the list its call gave, for a call that makes several (`m.split`).
    pieces: Vec<Option<u32>>,
    fn_calls: &'a [Option<FnCall>],
    /// The names of the line scheme (`box@6`) a root's `name` gave a part.
    named_lines: std::collections::HashSet<String>,
    unit: f64,
    assets: &'a Assets,
    images: &'a Images,
    weave: Option<WeaveDefaults>,
    /// The materials of the field being lowered, by object (slot = index + 1).
    materials: Vec<(*const Obj, csg_sdf::SdfMaterial, Option<u32>)>,
    material_filaments: HashMap<NodeId, Vec<Option<u32>>>,
    notes: Vec<Diag>,
    /// The texture library's object (its patterns and mappings).
    texture_lib: Option<makepad_script::ScriptObject>,
    solved: &'a HashMap<String, Arc<csg_sdf::SdfGrid>>,
    stress_requests: Vec<StressRequest>,
    grow_requests: Vec<GrowRequest>,
    veins_requests: Vec<VeinsRequest>,
    generated: &'a HashMap<String, GenPart>,
    panels: Vec<PanelReq>,
    gen_requests: Vec<GenRequest>,
    /// Each generated part's id, by part: where it was made (`prepare_generated`).
    gen_ids: HashMap<usize, String>,
    /// The solids lowered as their input ([`evaluate_skipping`]).
    skip: &'a [String],
}

/// The parts that lower to a distance field.
const FIELD_PARTS: &[&str] = &["Box", "Sphere", "Cylinder", "Torus", "Ellipsoid", "Capsule", "Tube", "Import", "Cage", "Sdf", "Blend", "Carve", "Instance", "Bone", "Twist", "Bend", "Taper", "Fold", "Texture", "Voronoi", "Grow", "Veins", "Offset"];

/// The sketch solids: meshes on their own (and shelled or copied), exact distance fields inside
/// a blend or a carve.
const SKETCH_FIELDS: &[&str] = &["Extrude", "Revolve"];

type Res<T> = Result<T, Diag>;

/// One object's fields, checked against its prototype.
struct Fields<'o> {
    obj: &'o Obj,
    proto: &'static Proto,
    unit: f64,
}

impl<'o> Fields<'o> {
    fn err<T>(&self, message: impl Into<String>) -> Res<T> {
        let (line, col) = self.obj.loc.unwrap_or((1, 1));
        Err(Diag { line, col, message: format!("{}: {}", self.proto.name, message.into()) })
    }
    fn get(&self, key: &str) -> Option<&'o V> {
        match self.obj.get(key) {
            Some(V::Nil) | None => None,
            Some(v) => Some(v),
        }
    }
    fn need(&self, key: &str) -> Res<&'o V> {
        match self.get(key) {
            Some(v) => Ok(v),
            None => {
                let help = self.proto.field(key).map(|f| f.help).unwrap_or("");
                self.err(format!("'{key}' is missing{}", if help.is_empty() { String::new() } else { format!(" ({help})") }))
            }
        }
    }
    fn number(&self, key: &str, v: &V) -> Res<f64> {
        match v {
            V::Num(n) if n.is_finite() => Ok(*n),
            other => self.err(format!("'{key}' must be a number, not {}", other.describe())),
        }
    }
    fn opt_num(&self, key: &str) -> Res<Option<f64>> {
        self.get(key).map(|v| self.number(key, v)).transpose()
    }
    fn num(&self, key: &str) -> Res<f64> {
        self.number(key, self.need(key)?)
    }
    fn len(&self, key: &str) -> Res<f64> {
        Ok(self.num(key)? * self.unit)
    }
    fn opt_len(&self, key: &str) -> Res<Option<f64>> {
        Ok(self.opt_num(key)?.map(|v| v * self.unit))
    }
    fn opt_angle(&self, key: &str) -> Res<Option<f64>> {
        Ok(self.opt_num(key)?.map(f64::to_degrees))
    }
    fn count(&self, key: &str, default: u32) -> Res<u32> {
        match self.opt_num(key)? {
            None => Ok(default),
            Some(n) if n >= 0.0 && n.fract() == 0.0 && n <= u32::MAX as f64 => Ok(n as u32),
            Some(n) => self.err(format!("'{key}' must be a whole number, not {n}")),
        }
    }
    fn flag(&self, key: &str, default: bool) -> Res<bool> {
        match self.get(key) {
            None => Ok(default),
            Some(V::Bool(b)) => Ok(*b),
            Some(other) => self.err(format!("'{key}' must be true or false, not {}", other.describe())),
        }
    }
    fn choice(&self, key: &str, default: &'static str) -> Res<&'static str> {
        let Some(Kind::Choice(names)) = self.proto.field(key).map(|f| f.kind) else { return Ok(default) };
        match self.get(key) {
            None => Ok(default),
            Some(V::Id(s) | V::Str(s)) => match names.iter().find(|n| **n == s.as_str() || (**n == "center" && s == "centre")) {
                Some(n) => Ok(n),
                None => self.err(format!("'{key}' is one of {}, not @{s}", names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(", "))),
            },
            Some(other) => self.err(format!("'{key}' is a name like @{}, not {}", names[0], other.describe())),
        }
    }
    fn text(&self, key: &str) -> Res<String> {
        match self.need(key)? {
            V::Str(s) => Ok(s.clone()),
            other => self.err(format!("'{key}' must be text, not {}", other.describe())),
        }
    }
    /// Reading the two optics fields costs O(1) per solid.
    fn optics(&self) -> Res<Option<Optics>> {
        let Some(value) = self.obj.get("optics") else { return Ok(None) };
        let obj = match value {
            V::Obj(o) if o.proto.is_none() && o.items.is_empty() => o,
            _ => return self.err("'optics' must be {throughput: 0..1 source: @estimate | @measured}"),
        };
        for (key, _) in &obj.fields {
            if !matches!(key.as_str(), "throughput" | "source") {
                return self.err(format!("'optics' has no field '{key}' (fields: throughput, source)"));
            }
        }
        let throughput = match obj.get("throughput") {
            Some(v) => self.number("optics.throughput", v)?,
            None => return self.err("'optics.throughput' is missing"),
        };
        if !(0.0..=1.0).contains(&throughput) {
            return self.err("'optics.throughput' must be a number from 0 to 1");
        }
        let source = match obj.get("source") {
            None => OpticsSource::Estimate,
            Some(V::Id(s) | V::Str(s)) if s == "estimate" => OpticsSource::Estimate,
            Some(V::Id(s) | V::Str(s)) if s == "measured" => OpticsSource::Measured,
            Some(_) => return self.err("'optics.source' must be @estimate or @measured"),
        };
        Ok(Some(Optics { throughput, source }))
    }
    /// Reading and validating keepouts costs O(points) per solid and converts lengths to kernel units.
    fn keepouts(&self) -> Res<Vec<Keepout>> {
        let Some(value) = self.obj.get("keepouts") else { return Ok(Vec::new()) };
        let V::Arr(list) = value else { return self.err("'keepouts' must be a list of {board name area z} records") };
        list.iter().map(|value| {
            let obj = match value {
                V::Obj(o) if o.proto.is_none() && o.items.is_empty() => o,
                _ => return self.err("'keepouts' must contain {board name area z} records"),
            };
            for (key, _) in &obj.fields {
                if !matches!(key.as_str(), "board" | "name" | "area" | "side" | "z" | "feature" | "room") {
                    return self.err(format!("'keepouts.{key}' is not a field (fields: board, name, area, side, z, feature, room)"));
                }
            }
            let text = |key: &str| -> Res<String> {
                match obj.get(key) {
                    Some(V::Str(s)) if !s.is_empty() => Ok(s.clone()),
                    _ => self.err(format!("'keepouts.{key}' must be non-empty text")),
                }
            };
            let board = text("board")?;
            let name = text("name")?;
            let feature = obj.get("feature").map(|_| text("feature")).transpose()?;
            let room = match obj.get("room") {
                None => false,
                Some(V::Bool(b)) => *b,
                Some(_) => return self.err("'keepouts.room' must be true or false"),
            };
            let side = match obj.get("side") {
                None => KeepoutSide::Top,
                Some(V::Id(s)) if s == "top" => KeepoutSide::Top,
                Some(V::Id(s)) if s == "bottom" => KeepoutSide::Bottom,
                Some(_) => return self.err("'keepouts.side' must be @top or @bottom"),
            };
            let area = match obj.get("area") {
                Some(V::Arr(points)) if points.len() >= 3 => points.iter().map(|point| {
                    let p = self.p2_of("keepouts.area", point)?;
                    if !p.iter().all(|n| n.is_finite()) {
                        return self.err("'keepouts.area' must contain two finite numbers per point in kernel units");
                    }
                    Ok(p)
                }).collect::<Res<Vec<_>>>()?,
                _ => return self.err("'keepouts.area' must be a list of at least three [x, y] points"),
            };
            let z = match obj.get("z") {
                Some(value) => self.p2_of("keepouts.z", value)?,
                None => return self.err("'keepouts.z' is missing"),
            };
            if !z.iter().all(|n| n.is_finite()) || z[0] >= z[1] {
                return self.err("'keepouts.z' must be two finite numbers [low, high] with low < high in kernel units");
            }
            Ok(Keepout { board, name, feature, room, area, side, z })
        }).collect()
    }
    /// A `rotate` in degrees about X, then Y, then Z: three angles, or `{to: [x y z]}`, the
    /// part's own `from` axis (default its Y) turned onto that direction the shortest way, then
    /// `spin` about it.
    fn rotation(&self, key: &str) -> Res<Option<P3>> {
        let Some(v) = self.get(key) else { return Ok(None) };
        let V::Obj(o) = v else {
            let a = self.nums(key, v, 3)?;
            return Ok(Some([a[0].to_degrees(), a[1].to_degrees(), a[2].to_degrees()]));
        };
        let wrong = || self.err(format!("'{key}' is [x y z] angles or {{to: [x y z] from: [x y z] spin: 30°}} (from: the part's axis, default its Y), not {}", v.describe()));
        if o.proto.is_some() || o.fields.iter().any(|(k, _)| !matches!(k.as_str(), "to" | "from" | "spin")) {
            return wrong();
        }
        let direction = |k: &str, default: Option<P3>| -> Res<Option<P3>> {
            match o.get(k) {
                Some(d) => {
                    let a = self.nums(&format!("{key}.{k}"), d, 3)?;
                    Ok(Some([a[0], a[1], a[2]]))
                }
                None => Ok(default),
            }
        };
        let Some(to) = direction("to", None)? else { return wrong() };
        let from = direction("from", Some([0.0, 1.0, 0.0]))?.unwrap();
        let spin = match o.get("spin") {
            Some(a) => self.number(&format!("{key}.spin"), a)?,
            None => 0.0,
        };
        match turn_onto(from, to, spin) {
            Some(r) => Ok(Some(r)),
            None => self.err(format!("'{key}': `to` and `from` are directions, not zero")),
        }
    }
    fn nums(&self, key: &str, v: &V, n: usize) -> Res<Vec<f64>> {
        match v {
            V::Arr(a) if a.len() == n => a.iter().map(|x| self.number(key, x)).collect(),
            other => self.err(format!("'{key}' must be {n} numbers [..], not {}", other.describe())),
        }
    }
    fn p3_of(&self, key: &str, v: &V) -> Res<P3> {
        let a = self.nums(key, v, 3)?;
        Ok([a[0] * self.unit, a[1] * self.unit, a[2] * self.unit])
    }
    fn p2_of(&self, key: &str, v: &V) -> Res<[f64; 2]> {
        let a = self.nums(key, v, 2)?;
        Ok([a[0] * self.unit, a[1] * self.unit])
    }
    fn p3(&self, key: &str) -> Res<P3> {
        self.p3_of(key, self.need(key)?)
    }
    /// A direction `[x y z]` (plain numbers), as a unit vector; `default` when not given.
    fn dir(&self, key: &str, default: Option<P3>) -> Res<P3> {
        let v = match (self.get(key), default) {
            (Some(v), _) => {
                let a = self.nums(key, v, 3)?;
                [a[0], a[1], a[2]]
            }
            (None, Some(d)) => d,
            (None, None) => return self.err(format!("'{key}' is missing (a direction [x y z])")),
        };
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if !(l > 1e-12) {
            return self.err(format!("'{key}' must not be [0 0 0]"));
        }
        Ok(v.map(|c| c / l))
    }
    fn opt_p3(&self, key: &str) -> Res<Option<P3>> {
        self.get(key).map(|v| self.p3_of(key, v)).transpose()
    }
    fn p2(&self, key: &str) -> Res<[f64; 2]> {
        self.p2_of(key, self.need(key)?)
    }
    fn opt_p2(&self, key: &str) -> Res<[f64; 2]> {
        Ok(self.get(key).map(|v| self.p2_of(key, v)).transpose()?.unwrap_or([0.0, 0.0]))
    }
    fn points2(&self, key: &str) -> Res<Vec<[f64; 2]>> {
        match self.need(key)? {
            V::Arr(list) => list.iter().map(|p| self.p2_of(key, p)).collect(),
            other => self.err(format!("'{key}' must be a list of [u v], not {}", other.describe())),
        }
    }
    /// One point or a list of points.
    fn points3(&self, key: &str) -> Res<Vec<P3>> {
        match self.need(key)? {
            V::Arr(list) if list.first().is_some_and(|x| matches!(x, V::Arr(_))) => list.iter().map(|p| self.p3_of(key, p)).collect(),
            v => Ok(vec![self.p3_of(key, v)?]),
        }
    }
    fn check(&self) -> Res<()> {
        let mut child_names = HashSet::new();
        if self.proto.name == "Path" {
            for (name, child) in &self.obj.children {
                let Some(name) = name.as_deref() else { continue };
                if matches!(name, "start" | "from" | "segs" | "at") {
                    let (line, col) = child.obj().and_then(|o| o.loc).or(self.obj.loc).unwrap_or((1, 1));
                    return Err(Diag { line, col, message: format!("a segment can't be named `{name}`: the Path uses that name; call it base, lip, ...") });
                }
                child_names.insert(name);
            }
            if !self.obj.children.is_empty() && self.obj.get("segs").is_some() {
                return self.err("a Path takes its segments as children or as segs: [..], not both");
            }
        }
        for (k, _) in &self.obj.fields {
            if self.proto.field(k).is_none() && !child_names.contains(k.as_str()) {
                let mut known: Vec<&str> = self.proto.fields.iter().map(|f| f.key).collect();
                if self.proto.is_solid() {
                    known.extend(protos::PLACEMENT.iter().map(|f| f.key));
                }
                return self.err(format!("has no field '{k}' (fields: {})", known.join(", ")));
            }
        }
        for f in self.proto.fields {
            if f.required && self.get(f.key).is_none() {
                return self.err(format!("'{}' is missing ({})", f.key, f.help));
            }
        }
        Ok(())
    }
}

/// A Path has one side per segment and one more if the last piece leaves a closing line.
fn path_side_count(start: [f64; 2], segs: &[Seg]) -> usize {
    let mut at = start;
    let mut prev = start;
    for seg in segs {
        let to = match seg {
            Seg::Line(to) | Seg::ArcThrough { to, .. } | Seg::ArcRadius { to, .. } | Seg::Tangent(to) => *to,
            Seg::Spline(points) => points.last().copied().unwrap_or(at),
            Seg::Rel(d) => [at[0] + d[0], at[1] + d[1]],
            Seg::H(dx) => [at[0] + dx, at[1]],
            Seg::V(dy) => [at[0], at[1] + dy],
            Seg::Polar { len, deg } => [at[0] + len * deg.to_radians().cos(), at[1] + len * deg.to_radians().sin()],
            Seg::Fillet(_) | Seg::Chamfer(_) => continue,
        };
        prev = at;
        at = to;
    }
    let out = [start[0] - at[0], start[1] - at[1]];
    let closing = out[0].hypot(out[1]);
    // A final corner can trim the whole closing line. Its incoming and outgoing
    // directions are those of the untrimmed lines, even after an earlier corner.
    let trimmed = match segs.last() {
        Some(Seg::Chamfer(d)) => *d,
        Some(Seg::Fillet(r)) if closing > 0.0 => {
            let inc = [at[0] - prev[0], at[1] - prev[1]];
            let cos = (inc[0] * out[0] + inc[1] * out[1]) / (inc[0].hypot(inc[1]) * closing);
            r * ((1.0 - cos).max(0.0) / (1.0 + cos).max(1e-12)).sqrt()
        }
        _ => 0.0,
    };
    segs.len() + usize::from((closing - trimmed).abs() > 1e-9)
}

/// Number of exact profile pieces preceding a later Path, for a repeated segment's diagnostic.
fn shape_side_count(shape: &Shape) -> Option<usize> {
    Some(match shape {
        Shape::Path { start, segs } => path_side_count(*start, segs),
        Shape::Rect { corner, chamfer, .. } => if corner.max(*chamfer) > 0.0 { 8 } else { 4 },
        Shape::Circle { .. } => 1,
        Shape::Slot { a, b, .. } => if (a[0] - b[0]).hypot(a[1] - b[1]) < 1e-12 { 1 } else { 4 },
        Shape::Regular { sides, .. } => *sides,
        Shape::Polygon { points, fillet } => if *fillet > 0.0 { points.len().saturating_mul(2).saturating_sub(1) } else { points.len() },
        Shape::Offset { of, d } => return shape_side_count(&makepad_csg_cad::sketch::offset_shape(of, *d)?.ok()?),
        Shape::Ellipse { .. } | Shape::Gear { .. } => return None,
    })
}

/// Express a selector on a placed body in the names its input carries before placement.
fn face_before_place(name: &mut FaceName, placed: &csg_graph::Key, input: &csg_graph::Key) {
    if name.origin == *placed {
        name.origin = *input;
    }
    match &mut name.role {
        Role::Blend(a, b) => {
            face_before_place(a, placed, input);
            face_before_place(b, placed, input);
            if a > b {
                std::mem::swap(a, b);
            }
        }
        Role::Corner(faces) => {
            for face in faces.iter_mut() {
                face_before_place(face, placed, input);
            }
            faces.sort();
            faces.dedup();
        }
        Role::Inner(Some(face)) => face_before_place(face, placed, input),
        Role::Named(_) | Role::Side(_) | Role::Inner(None) => {}
    }
}

/// A mirror plane by its choice name: `xz` flips Y, `xy` flips Z, anything else (`yz`) X.
fn mirror_plane(name: &str) -> MirrorPlane {
    match name {
        "xz" => MirrorPlane::Xz,
        "xy" => MirrorPlane::Xy,
        _ => MirrorPlane::Yz,
    }
}

fn plane_of(s: &str) -> Plane {
    match s {
        "xy" => Plane::Xy,
        "yz" => Plane::Yz,
        _ => Plane::Xz,
    }
}

impl<'a, 'v, 'b> Lower<'a, 'v, 'b> {
    fn fields<'o>(&self, obj: &'o Obj) -> Res<Fields<'o>> {
        let name = obj.proto.as_deref().unwrap_or("");
        let Some(proto) = protos::proto(name) else {
            let (line, col) = obj.loc.unwrap_or((1, 1));
            return Err(Diag { line, col, message: format!("'{name}' is not a model type") });
        };
        let f = Fields { obj, proto, unit: self.unit };
        f.check()?;
        Ok(f)
    }

    /// Lower a part (its handle read as its object).
    fn solid(&mut self, v: &V) -> Res<usize> {
        let obj = match v {
            V::Obj(o) if o.part.is_some() => o.clone(),
            other => {
                return Err(Diag { line: 1, col: 1, message: format!("expected a part (made by m.box(..), m.subtract(..), ...), got {}", other.describe()) });
            }
        };
        let part = obj.part.unwrap();
        if let Some(i) = self.memo.get(&part) {
            return Ok(*i);
        }
        let site = self.names.site(obj.loc).cloned();
        let proto = protos::proto(obj.proto.as_deref().unwrap_or("")).filter(|p| p.is_solid()).ok_or_else(|| Diag { line: 1, col: 1, message: "not a part".into() })?;
        let mut id = self.part_names[part].clone();
        if id.is_empty() {
            id = self.line_name(proto.call, obj.loc);
        }
        if let Some(generated) = self.gen_ids.get(&part) {
            id = generated.clone();
        }
        let named = !id.contains(['.', '[', '#', '@']) && site.as_ref().is_some_and(|s| s.name.as_deref() == Some(id.as_str()));
        let index = self.solids.len();
        let off = self.fields(&obj)?.flag("off", false)?;
        let link = match obj.get("link") {
            Some(V::Str(key)) => Some(key.clone()),
            None => None,
            Some(_) => return self.fields(&obj)?.err("'link' must be text"),
        };
        let label = match obj.get("label") {
            Some(V::Str(text)) => Some(text.clone()),
            None => None,
            Some(_) => return self.fields(&obj)?.err("'label' must be text"),
        };
        let optics = self.fields(&obj)?.optics()?;
        let keepouts = self.fields(&obj)?.keepouts()?;
        let frame = match obj.get("frame") {
            None => None,
            Some(V::Obj(o)) if o.proto.as_deref() == Some("Frame") => {
                let ff = self.fields(o)?;
                Some((ff.text("name")?, ff.opt_p3("at")?.unwrap_or([0.0; 3]), ff.rotation("rotate")?.unwrap_or([0.0; 3])))
            }
            Some(_) => return self.fields(&obj)?.err("'frame' must be Frame{name at rotate}"),
        };
        self.solids.push(Solid { id: id.clone(), proto: proto.name, asset: None, node: NodeId(0), loc: obj.loc, span: site.map(|s| s.span), call: self.fn_calls[part].clone(), parent: None, children: Vec::new(), inputs: Vec::new(), sides: Vec::new(), ambiguous_sides: Vec::new(), named, off, link, label, optics, keepouts, frame, picture: None, selectors: Vec::new() });
        self.memo.insert(part, index);
        self.lowering.push(index);
        let node = self.solid_op(&obj, &id, index);
        self.lowering.pop();
        self.solids[index].node = node?;
        // The resolved arguments still include everything the geometry bypassed. Read
        // each field once, with the positional fields first (including a panel's base).
        fn collect(value: &V, memo: &HashMap<usize, usize>, index: usize, inputs: &mut Vec<usize>) {
            match value {
                V::Arr(items) => {
                    for item in items {
                        collect(item, memo, index, inputs);
                    }
                }
                V::Obj(o) => {
                    if let Some(&i) = o.part.and_then(|p| memo.get(&p)).filter(|&&i| i != index) {
                        inputs.push(i);
                    }
                }
                _ => {}
            }
        }
        let options = proto.fields.iter().map(|f| f.key).filter(|k| !proto.args.contains(k));
        for value in proto.args.iter().copied().chain(options).filter_map(|key| obj.get(key)) {
            collect(value, &self.memo, index, &mut self.solids[index].inputs);
        }
        Ok(index)
    }

    /// A joint call: the parts it names (lowered as solids) and its fields.
    fn joint(&mut self, v: &V, part: usize, out: &mut Vec<JointCall>) -> Res<()> {
        let V::Obj(obj) = v else { return Err(Diag { line: 1, col: 1, message: "not a joint".into() }) };
        let f = self.fields(obj)?;
        f.check()?;
        let site = self.names.site(obj.loc).cloned();
        let id = match self.part_names[part].as_str() {
            "" => self.line_name(f.proto.call, obj.loc),
            n => n.to_string(),
        };
        let solid_of = |l: &mut Self, key: &str, v: &V| -> Res<usize> { l.solid(v).map_err(|e| Diag { message: format!("{}.{key}: {}", f.proto.name, e.message), ..e }) };
        let axis = match f.get("axis") {
            None => None,
            Some(v) => {
                let a = f.nums("axis", v, 3)?;
                let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                if n == 0.0 {
                    return f.err("'axis' must not be [0 0 0]");
                }
                Some([a[0] / n, a[1] / n, a[2] / n])
            }
        };
        let mut values = Vec::new();
        for field in f.proto.fields {
            if matches!(field.key, "a" | "b" | "at" | "axis" | "links" | "part" | "mode" | "joint" | "to" | "from" | "parts" | "shoe" | "motor") {
                continue;
            }
            match field.kind {
                Kind::Bool => {
                    if f.flag(field.key, false)? {
                        values.push((field.key, 1.0));
                    }
                }
                k if k.is_length() => {
                    if let Some(x) = f.opt_len(field.key)? {
                        values.push((field.key, if k == Kind::Diameter { x * 0.5 } else { x }));
                    }
                }
                _ => {
                    if let Some(x) = f.opt_num(field.key)? {
                        values.push((field.key, x));
                    }
                }
            }
        }
        let mode = if f.proto.field("mode").is_some() { f.choice("mode", "in_place")? } else { "in_place" };
        if mode == "snap" {
            return f.err("mode: @snap is not built yet; use @in_place (the default)");
        }
        let motor = match f.choice("motor", "none")? {
            "none" => None,
            kind => Some(kind),
        };
        if motor.is_none() && (f.get("torque").is_some() || f.get("force").is_some()) {
            return f.err("torque and force are a motor's: give motor: @position or @speed");
        }
        let span = site.map(|s| s.span);
        match f.proto.name {
            "Chain" => {
                let links = match f.need("links")? {
                    V::Arr(list) => list.clone(),
                    other => return f.err(format!("'links' must be a list of parts, not {}", other.describe())),
                };
                if links.len() < 2 {
                    return f.err("a chain needs at least two links");
                }
                let proto = if f.choice("joint", "hinge")? == "ball" { "BallJoint" } else { "Hinge" };
                let mut idx = Vec::new();
                for v in &links {
                    idx.push(solid_of(self, "links", v)?);
                }
                for k in 0..idx.len() - 1 {
                    out.push(JointCall { id: format!("{id}[{k}]"), proto, a: idx[k], b: Some(idx[k + 1]), at: None, to: None, axis, shoe: None, values: values.clone(), mode, motor: None, loc: obj.loc, span: span.clone() });
                }
            }
            "Bond" => {
                let to = solid_of(self, "to", f.need("to")?)?;
                let parts = match f.need("parts")? {
                    V::Arr(list) => list.clone(),
                    other => return f.err(format!("'parts' must be a list of parts, not {}", other.describe())),
                };
                if parts.is_empty() {
                    return f.err("a bond needs parts to bond");
                }
                // one call per part, in order: the host groups them by the bond's id
                for v in &parts {
                    let b = solid_of(self, "parts", v)?;
                    if b == to {
                        return f.err("a part cannot be bonded to itself");
                    }
                    out.push(JointCall { id: id.clone(), proto: "Bond", a: to, b: Some(b), at: None, to: None, axis, shoe: None, values: values.clone(), mode, motor: None, loc: obj.loc, span: span.clone() });
                }
            }
            "Anchor" => {
                let a = solid_of(self, "part", f.need("part")?)?;
                out.push(JointCall { id, proto: "Anchor", a, b: None, at: Some(f.p3("at")?), to: None, axis, shoe: None, values, mode, motor: None, loc: obj.loc, span });
            }
            "String" => {
                let a = solid_of(self, "a", f.need("a")?)?;
                let b = solid_of(self, "b", f.need("b")?)?;
                if a == b {
                    return f.err("a string runs between two different parts");
                }
                out.push(JointCall { id, proto: "String", a, b: Some(b), at: Some(f.p3("from")?), to: Some(f.p3("to")?), axis, shoe: None, values, mode, motor: None, loc: obj.loc, span });
            }
            name => {
                let a = solid_of(self, "a", f.need("a")?)?;
                let b = solid_of(self, "b", f.need("b")?)?;
                if a == b {
                    return f.err("a and b must be two different parts");
                }
                let proto = match name {
                    "Hinge" => "Hinge",
                    "Slider" => "Slider",
                    "Bearing" => "Bearing",
                    _ => "BallJoint",
                };
                let shoe = match f.get("shoe") {
                    Some(v) => Some(solid_of(self, "shoe", v)?),
                    None => None,
                };
                if shoe.is_some_and(|s| s == a || s == b) {
                    return f.err("a brake's shoe is a part of its own, not a or b");
                }
                out.push(JointCall { id, proto, a, b: Some(b), at: Some(f.p3("at")?), to: None, axis, shoe, values, mode, motor, loc: obj.loc, span });
            }
        }
        Ok(())
    }

    fn child(&mut self, v: &V, path: String, parent: usize) -> Res<NodeId> {
        let i = self.solid(v).map_err(|e| Diag { message: format!("{path}: {}", e.message), ..e })?;
        if self.solids[i].parent.is_none() && i != parent {
            self.solids[i].parent = Some(parent);
            self.solids[parent].children.push(i);
        }
        Ok(self.solids[i].node)
    }

    fn push(&mut self, f: &Fields, op: Op) -> Res<NodeId> {
        self.graph.push(op).or_else(|e| f.err(e))
    }

    /// Keep every operand in the source tree; only enabled ones reach the operation.
    fn operands<'o>(&mut self, f: &Fields<'o>) -> Res<Vec<&'o V>> {
        let bypass = f.flag("off", false)? && !f.obj.part.is_some_and(|p| self.roots.contains(&p));
        let values = if f.proto.name == "Blend" {
            flat(f.need("of")?)
        } else {
            let b = flat(f.need("b")?);
            if b.is_empty() && f.proto.name != "Carve" && !bypass {
                return f.err("'b' is an empty list");
            }
            std::iter::once(f.need("a")?).chain(b).collect()
        };
        if values.is_empty() {
            return f.err("the operand list is empty");
        }
        let mut out = Vec::new();
        for (n, v) in values.into_iter().enumerate() {
            let i = self.solid(v)?;
            if let Some(&parent) = self.lowering.last() {
                self.child(v, self.solids[i].id.clone(), parent)?;
            }
            let enabled = if bypass { n == 0 } else { !self.solids[i].off };
            if enabled {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn solid_op(&mut self, obj: &Arc<Obj>, id: &str, index: usize) -> Res<NodeId> {
        let f = self.fields(obj)?;
        let mut filaments = None;
        let field_child = |l: &mut Self, key: &str| -> Res<NodeId> {
            let v = f.need(key)?;
            l.child(v, format!("{id}.{key}"), index)
        };
        let brep_child = |l: &mut Self, key: &str, v: &V| -> Res<NodeId> {
            let node = l.child(v, format!("{id}.{key}"), index)?;
            if !matches!(l.graph.node(node).op, Op::Brep { .. }) {
                return f.err(format!("m.brep.{}: '{key}' must be an m.brep part. A mix of engines goes through the mesh: use m.union, m.subtract or m.intersect", f.proto.call));
            }
            Ok(node)
        };
        let op = match f.proto.name {
            "Panel" => return self.panel(obj, id, index),
            "Generated" => return self.generated_part(obj, id),
            "BrepBox" => Op::Brep { op: BrepOp::Box { size: f.p3("size")? }, inputs: Vec::new() },
            "BrepSphere" => Op::Brep { op: BrepOp::Sphere { radius: f.len("radius")? }, inputs: Vec::new() },
            "BrepCylinder" => {
                let radius = f.len("radius")?;
                Op::Brep { op: BrepOp::Cylinder { radius, top_radius: f.opt_len("top_radius")?.unwrap_or(radius), height: f.len("height")? }, inputs: Vec::new() }
            }
            "BrepTorus" => Op::Brep { op: BrepOp::Torus { radius: f.len("radius")?, tube: f.len("tube")? }, inputs: Vec::new() },
            "BrepExtrude" => Op::Brep { op: BrepOp::Extrude { sketch: self.sketch(&f, "sketch")?, plane: plane_of(f.choice("plane", "xz")?), height: f.len("height")?, both: f.flag("both", false)? }, inputs: Vec::new() },
            "BrepRevolve" => Op::Brep { op: BrepOp::Revolve { sketch: self.sketch(&f, "sketch")?, angle: f.opt_angle("angle")?.unwrap_or(360.0) }, inputs: Vec::new() },
            "BrepSweep" => {
                let path = f.points3("path")?;
                // One size per point, or the end's size reached evenly along the way.
                let scale = match f.get("scale") {
                    None => Vec::new(),
                    Some(V::Arr(list)) => list.iter().map(|s| f.number("scale", s)).collect::<Res<_>>()?,
                    Some(v) => {
                        let end = f.number("scale", v)?;
                        let mut along = vec![0.0];
                        for w in path.windows(2) {
                            let d = (0..3).map(|i| (w[1][i] - w[0][i]).powi(2)).sum::<f64>().sqrt();
                            along.push(along[along.len() - 1] + d);
                        }
                        let total = along[along.len() - 1].max(f64::MIN_POSITIVE);
                        along.iter().map(|a| 1.0 + (end - 1.0) * a / total).collect()
                    }
                };
                Op::Brep { op: BrepOp::Sweep { sketch: self.sketch(&f, "sketch")?, path, scale }, inputs: Vec::new() }
            }
            "BrepLoft" => {
                let V::Arr(list) = f.need("sections")? else { return f.err("'sections' is a list of Section{sketch at}") };
                let mut sections = Vec::new();
                for s in list {
                    let Some(so) = s.obj().filter(|o| o.proto.as_deref() == Some("Section")) else { return f.err(format!("a section is a Section{{sketch at}}, not {}", s.describe())) };
                    let sf = self.fields(so)?;
                    sections.push(LoftSection { sketch: self.sketch(&sf, "sketch")?, at: sf.len("at")? });
                }
                Op::Brep { op: BrepOp::Loft { sections, plane: plane_of(f.choice("plane", "xz")?) }, inputs: Vec::new() }
            }
            "BrepUnion" | "BrepDifference" | "BrepIntersect" => {
                let list = self.operands(&f)?;
                let mut inputs = Vec::new();
                for (i, v) in list.into_iter().enumerate() {
                    inputs.push(brep_child(self, &format!("operand.{i}"), v)?);
                }
                if inputs.len() <= 1 {
                    let node = match inputs.first() {
                        Some(node) => *node,
                        None => self.push(&f, Op::Mesh { mesh: Arc::new(makepad_csg_graph::TriMesh::new()) })?,
                    };
                    return self.placed(&f, node);
                }
                let mode = match f.proto.name {
                    "BrepUnion" => BoolMode::Union,
                    "BrepDifference" => BoolMode::Difference,
                    _ => BoolMode::Intersection,
                };
                Op::Brep { op: BrepOp::Boolean { mode }, inputs }
            }
            "BrepFillet" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                let op = BrepOp::Fillet { radius: f.len("radius")?, edges: self.edges(&f, source)? };
                self.skipped(id, op, source)
            }
            "BrepChamfer" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                let op = BrepOp::Chamfer { size: f.len("size")?, edges: self.edges(&f, source)? };
                self.skipped(id, op, source)
            }
            "BrepShell" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                let is_face = |v: &V| matches!(v, V::Obj(o) if o.proto.as_deref() == Some("Face"));
                let open = match f.get("open") {
                    Some(v) if is_face(v) => Open::Faces(vec![self.selector_face(&f, "open", v, source)?]),
                    Some(V::Arr(list)) if !list.is_empty() && list.iter().all(is_face) => Open::Faces(list.iter().map(|v| self.selector_face(&f, "open", v, source)).collect::<Res<_>>()?),
                    _ => match f.choice("open", "top")? {
                        "bottom" => Open::Bottom,
                        "closed" => Open::Closed,
                        _ => Open::Top,
                    },
                };
                self.skipped(id, BrepOp::Shell { thickness: f.len("thickness")?, open }, source)
            }
            "BrepPull" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                let face = self.selector_face(&f, "face", f.need("face")?, source)?;
                let by = f.len("by")?;
                // (a pull by nothing leaves the body as it is: a drag passing 0)
                if by == 0.0 {
                    return self.placed(&f, source);
                }
                self.skipped(id, BrepOp::Pull { face, by }, source)
            }
            "BrepOffset" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                Op::Brep { op: BrepOp::Offset { distance: f.len("distance")? }, inputs: vec![source] }
            }
            "BrepMirror" => {
                let source = brep_child(self, "source", f.need("source")?)?;
                let op = BrepOp::Mirror { plane: mirror_plane(f.choice("plane", "yz")?), center: f.opt_p3("center")?.unwrap_or([0.0; 3]), keep: f.flag("keep", true)? };
                Op::Brep { op, inputs: vec![source] }
            }
            "Box" => {
                let size = f.p3("size")?;
                let radius = f.opt_len("radius")?;
                let taper = f.opt_p2("taper")?;
                let shift = f.opt_len("shift")?.unwrap_or(0.0);
                if taper == [0.0; 2] && shift == 0.0 {
                    Op::Box { size, radius: radius.unwrap_or(0.0) }
                } else {
                    if radius.is_some() {
                        return f.err("'radius' cannot be combined with 'taper' or 'shift'");
                    }
                    let [x, y, z] = size.map(|s| s * 0.5);
                    let (tx, tz) = ((size[0] + taper[0]) * 0.5, (size[2] + taper[1]) * 0.5);
                    Op::Hull { points: vec![
                        [-x, -y, -z], [x, -y, -z], [x, -y, z], [-x, -y, z],
                        [shift - tx, y, -tz], [shift + tx, y, -tz], [shift + tx, y, tz], [shift - tx, y, tz],
                    ] }
                }
            }
            "Hull" => {
                let points = match f.get("points") {
                    Some(V::Arr(list)) => list.iter().map(|p| f.p3_of("points", p)).collect::<Res<Vec<_>>>()?,
                    Some(_) => return f.err("'points' is a list of [x y z]"),
                    None => Vec::new(),
                };
                let parts = f.get("parts").map(flat).unwrap_or_default();
                if parts.is_empty() {
                    if points.is_empty() {
                        return f.err("a hull needs parts (m.hull([a b])) or points ({points: [[x y z] ...]})");
                    }
                    Op::Hull { points }
                } else {
                    let mut sources = Vec::new();
                    for v in parts {
                        sources.push(self.child(v, id.to_string(), index)?);
                    }
                    Op::HullOf { sources, points }
                }
            }
            "Picture" | "ColourSwatch" => {
                let arg = self.picture(&f)?;
                let size = [arg.size[0], arg.layers as f64 * arg.layer, arg.size[1]];
                self.solids[index].picture = Some(arg);
                Op::Box { size, radius: 0.0 }
            }
            "Sphere" => Op::Sphere { radius: f.len("radius")?, segments: f.count("segments", 48)?, rings: f.count("rings", 24)? },
            "Cylinder" => {
                let radius = f.len("radius")?;
                Op::Cylinder { radius, top_radius: f.opt_len("top_radius")?.unwrap_or(radius), height: f.len("height")?, segments: f.count("segments", 48)? }
            }
            "Torus" => Op::Torus { radius: f.len("radius")?, tube: f.len("tube")?, segments: f.count("segments", 48)?, sides: f.count("sides", 24)? },
            "Extrude" => Op::Extrude { sketch: self.sketch(&f, "sketch")?, plane: plane_of(f.choice("plane", "xz")?), height: f.len("height")?, draft: f.opt_angle("draft")?.unwrap_or(0.0), both: f.flag("both", false)? },
            "Revolve" => Op::Revolve { sketch: self.sketch(&f, "sketch")?, angle: f.opt_angle("angle")?.unwrap_or(360.0) },
            "Sweep" => {
                let path = match (f.get("path"), f.get("helix")) {
                    (Some(_), None) => match f.need("path")? {
                        V::Arr(list) => Path3::Points { points: list.iter().map(|p| f.p3_of("path", p)).collect::<Res<_>>()?, closed: f.flag("closed", false)? },
                        other => return f.err(format!("'path' must be a list of [x y z], not {}", other.describe())),
                    },
                    (None, Some(V::Obj(h))) if h.proto.as_deref() == Some("Helix") => {
                        let hf = self.fields(h)?;
                        Path3::Helix { radius: hf.len("radius")?, pitch: hf.len("pitch")?, turns: hf.num("turns")? }
                    }
                    _ => return f.err("give 'path' [[x y z] ...] or 'helix' Helix{radius pitch turns}"),
                };
                Op::Sweep { sketch: self.sketch(&f, "sketch")?, path, twist: f.opt_angle("twist")?.unwrap_or(0.0), scale: f.opt_num("scale")?.unwrap_or(1.0) }
            }
            "Loft" => {
                let V::Arr(list) = f.need("sections")? else { return f.err("'sections' is a list of Section{sketch at}") };
                let mut sections = Vec::new();
                for s in list {
                    let Some(so) = s.obj().filter(|o| o.proto.as_deref() == Some("Section")) else { return f.err(format!("a section is a Section{{sketch at}}, not {}", s.describe())) };
                    let sf = self.fields(so)?;
                    sections.push(LoftSection { sketch: self.sketch(&sf, "sketch")?, at: sf.len("at")? });
                }
                Op::Loft { sections, plane: plane_of(f.choice("plane", "xz")?) }
            }
            "RingLoft" => {
                let V::Arr(list) = f.need("rings")? else { return f.err("'rings' is a list of LoftRing{at u v points crease}") };
                let proto = protos::proto("LoftRing").expect("the RingLoft ring prototype");
                let mut rings = Vec::new();
                for r in list {
                    let Some(ro) = r.obj().filter(|o| o.proto.as_deref() == Some("LoftRing")) else { return f.err(format!("a ring is a LoftRing{{at u v points crease}}, not {}", r.describe())) };
                    let rf = Fields { obj: ro, proto, unit: self.unit };
                    rf.check()?;
                    // Axes are dimensionless and stay as written for the kernel's unit checks.
                    let u = rf.nums("u", rf.need("u")?, 3)?;
                    let v = rf.nums("v", rf.need("v")?, 3)?;
                    rings.push(Ring { origin: rf.p3("at")?, u: [u[0], u[1], u[2]], v: [v[0], v[1], v[2]], points: rf.points2("points")?, crease: rf.flag("crease", false)? });
                }
                Op::RingLoft { rings, prism: f.flag("prism", false)? }
            }
            "Text" => Op::Text {
                text: f.text("text")?,
                size: f.len("size")?,
                depth: f.len("depth")?,
                spacing: f.opt_len("spacing")?.unwrap_or(0.0),
                align: match f.choice("align", "left")? {
                    "center" => Align::Center,
                    "right" => Align::Right,
                    _ => Align::Left,
                },
                font: if f.choice("font", "sans")? == "mono" { Font::Mono } else { Font::Sans },
                plane: plane_of(f.choice("plane", "xz")?),
            },
            "Gear" => {
                let teeth = f.count("teeth", 0)?;
                let gear = GearSpec {
                    module: f.len("module")?,
                    teeth,
                    thickness: f.len("thickness")?,
                    pressure: f.opt_angle("pressure")?.unwrap_or(20.0),
                    backlash: f.opt_len("backlash")?.unwrap_or(0.1 * self.unit / 1000.0),
                    shift: f.opt_num("shift")?.unwrap_or(0.0),
                    bore: f.opt_len("bore")?.unwrap_or(0.0),
                };
                let mesh_with = match f.get("mesh_with") {
                    Some(_) => Some((field_child(self, "mesh_with")?, f.opt_angle("mesh_angle")?.unwrap_or(0.0))),
                    None => None,
                };
                Op::Gear { gear, plane: plane_of(f.choice("plane", "xz")?), mesh_with }
            }
            "Thread" => Op::Thread { diameter: f.len("diameter")?, pitch: f.opt_len("pitch")?, length: f.len("length")?, clearance: f.opt_len("clearance")?.unwrap_or(0.0), left: f.flag("left", false)?, runout: f.opt_len("runout")?, internal: f.flag("internal", false)? },
            "Fillet" => {
                let source = field_child(self, "source")?;
                Op::Fillet { source, radius: f.len("radius")?, edges: self.edges(&f, source)? }
            }
            "Chamfer" => {
                let source = field_child(self, "source")?;
                Op::Chamfer { source, size: f.len("size")?, edges: self.edges(&f, source)? }
            }
            "Hole" | "BrepHole" => {
                let exact = f.proto.name == "BrepHole";
                let source = if exact { brep_child(self, "source", f.need("source")?)? } else { field_child(self, "source")? };
                let kind = match f.choice("kind", "simple")? {
                    "counterbore" => HoleKind::Counterbore { bore_diameter: f.len("bore_diameter")?, bore_depth: f.len("bore_depth")? },
                    "countersink" => HoleKind::Countersink { sink_diameter: f.len("sink_diameter")?, angle: f.opt_angle("angle")?.unwrap_or(90.0) },
                    "tapped" => HoleKind::Tapped { pitch: f.opt_len("pitch")?, clearance: f.opt_len("clearance")?.unwrap_or(0.15 * self.unit / 1000.0) },
                    _ => HoleKind::Simple,
                };
                let dir = f.get("dir").map(|v| f.nums("dir", v, 3).map(|a| [a[0], a[1], a[2]])).transpose()?;
                let diameter = f.len("diameter")?;
                let depth = f.opt_len("depth")?;
                let at = f.points3("at")?;
                if exact {
                    let mut on = f.get("on").map(|v| self.selector_face(&f, "on", v, source)).transpose()?;
                    let hole = |at, dir, on| BrepOp::Hole { kind, diameter, depth, at, dir, on };
                    // A scaled cut is always re-placed, so its scale is explicit on the outer
                    // node. Use the written factors to distinguish even a near-unit scale.
                    match &self.graph.node(source).op {
                        Op::Brep { op: BrepOp::Place { at: moved, rotate, scale }, inputs } if *scale != [1.0; 3] && inputs.len() == 1 => {
                            let (placed, inner) = (BrepOp::Place { at: *moved, rotate: *rotate, scale: *scale }, inputs[0]);
                            let origin = self.graph.node(source).key;
                            if let Some(face) = &mut on {
                                face_before_place(face, &origin, &self.graph.node(inner).key);
                            }
                            let drilled = self.skipped(id, hole(at, dir, on), inner);
                            let drilled = self.push(&f, drilled)?;
                            // Known residual: scaling the drill with the body bypasses the
                            // source's placement names. Diagnose their later use at the selector.
                            if matches!(self.graph.node(drilled).op, Op::Brep { op: BrepOp::Hole { .. }, .. }) {
                                self.scaled_holes.entry(origin).or_default().insert(drilled);
                            }
                            Op::Brep { op: placed, inputs: vec![drilled] }
                        }
                        _ => {
                            let frame = csg_graph::eval::drill_frame(&self.graph, source);
                            let rotate = |v: P3| frame.map(|r| r[0] * v[0] + r[1] * v[1] + r[2] * v[2]);
                            let mut points = at;
                            for p in &mut points {
                                let v = rotate(*p);
                                *p = std::array::from_fn(|i| v[i] + frame[i][3]);
                            }
                            let dir = dir.or_else(|| on.is_none().then_some([0.0, -1.0, 0.0])).map(rotate);
                            self.skipped(id, hole(points, dir, on), source)
                        }
                    }
                } else {
                    Op::Hole { source, kind, diameter, depth, at, dir: dir.unwrap_or([0.0, -1.0, 0.0]) }
                }
            }
            "HoleRow" => {
                let from = f.p3("from")?;
                let to = f.opt_p3("to")?.unwrap_or(from);
                let count = f.count("count", 1)? as usize;
                if count == 0 {
                    return f.err("a hole row needs a count of 1 or more");
                }
                let radius = 0.5 * f.len("diameter")?;
                let length = f.len("length")?;
                let axis = f.dir("axis", Some([0.0, 1.0, 0.0]))?;
                let up = f.dir("up", Some([0.0, 1.0, 0.0]))?;
                let teardrop = f.flag("teardrop", dot3(axis, up).abs() < std::f64::consts::FRAC_1_SQRT_2)?;
                let segments = f.count("segments", 48)?;
                // each hole's frame: its cylinder's Y along the axis, its Z the print's up across it
                let lift = across(axis, up);
                let frame = [cross3(axis, lift), axis, lift];
                let cylinder = self.push(&f, Op::Cylinder { radius, top_radius: radius, height: length, segments })?;
                // the roof: a square of side r turned 45 degrees, from the centre to the tip r sqrt 2 above it
                let roof = if teardrop { Some(self.push(&f, Op::Box { size: [radius, length, radius], radius: 0.0 })?) } else { None };
                let mut holes = Vec::with_capacity(count);
                for i in 0..count {
                    let t = if count == 1 { 0.5 } else { i as f64 / (count - 1) as f64 };
                    let c: P3 = std::array::from_fn(|k| from[k] + (to[k] - from[k]) * t);
                    let mut hole = self.push(&f, Op::Transform { source: cylinder, at: c, rotate: euler_xyz(frame), scale: [1.0; 3] })?;
                    if let Some(roof) = roof {
                        let k = radius * std::f64::consts::FRAC_1_SQRT_2;
                        let at = std::array::from_fn(|j| c[j] + lift[j] * k);
                        let tip = self.push(&f, Op::Transform { source: roof, at, rotate: euler_xyz(turned(frame, 1)), scale: [1.0; 3] })?;
                        hole = self.push(&f, Op::Boolean { mode: BoolMode::Union, a: hole, b: vec![tip] })?;
                    }
                    holes.push(hole);
                }
                let a = holes.remove(0);
                if holes.is_empty() {
                    return self.placed(&f, a);
                }
                Op::Boolean { mode: BoolMode::Union, a, b: holes }
            }
            "Pocket" => {
                let center = f.p3("center")?;
                let size = f.p2("size")?;
                let depth = f.len("depth")?;
                let axis = f.dir("axis", None)?;
                let up = f.dir("up", Some([0.0, 1.0, 0.0]))?;
                let level = dot3(axis, up).abs() < std::f64::consts::FRAC_1_SQRT_2;
                let along = match f.get("along") {
                    Some(_) => {
                        let a = f.dir("along", None)?;
                        let d = dot3(a, axis);
                        let a: P3 = std::array::from_fn(|k| a[k] - d * axis[k]);
                        let l = dot3(a, a).sqrt();
                        if l < 1e-9 {
                            return f.err("'along' must lie across the axis");
                        }
                        a.map(|v| v / l)
                    }
                    // level across the axis: a roof's edge runs level
                    None => cross3(across(axis, up), axis),
                };
                let side = cross3(axis, along);
                let mm = self.unit / 1000.0;
                let over = f.opt_len("corner")?.unwrap_or(mm).max(0.0) + 0.5 * mm;
                let corner = (over - 0.5 * mm).min(0.499 * size[0]).min(0.499 * size[1]).min(0.499 * (depth + over));
                let roof = f.flag("roof", level)?;
                // the box's frame: X along, Y across, Z into the material; out past the opening by
                // the corner, so its rounding stays outside the part
                let frame = [along, side, axis];
                let block = self.push(&f, Op::Box { size: [size[0], size[1], depth + over], radius: corner })?;
                let at = std::array::from_fn(|k| center[k] + axis[k] * 0.5 * (depth - over));
                let body = self.push(&f, Op::Transform { source: block, at, rotate: euler_xyz(frame), scale: [1.0; 3] })?;
                let straight = size[0] - 2.0 * corner;
                if !roof || straight <= 0.0 {
                    return self.placed(&f, body);
                }
                // the roof: a square on the top edge at the face, turned 45 degrees about `along`,
                // `depth` from its centre to each corner
                let top = if dot3(side, up) >= 0.0 { 1.0 } else { -1.0 };
                let gable = self.push(&f, Op::Box { size: [straight, depth * std::f64::consts::SQRT_2, depth * std::f64::consts::SQRT_2], radius: 0.0 })?;
                let at = std::array::from_fn(|k| center[k] + side[k] * top * 0.5 * size[1]);
                let tip = self.push(&f, Op::Transform { source: gable, at, rotate: euler_xyz(turned(frame, 0)), scale: [1.0; 3] })?;
                Op::Boolean { mode: BoolMode::Union, a: body, b: vec![tip] }
            }
            "Split" => {
                let source = field_child(self, "source")?;
                let center = f.p3("center")?;
                let normal = f.dir("normal", Some([0.0, 1.0, 0.0]))?;
                let piece = match obj.part.and_then(|p| self.pieces[p]) {
                    Some(0) => SplitPiece::Below,
                    Some(1) => SplitPiece::Above,
                    Some(_) => SplitPiece::Pins,
                    None => return f.err("a split's pieces come from m.split"),
                };
                // Sized from the printer, as the joints are: the hole's gap in the layer, past
                // the pin's end across the layers, and a wall of two lines round each hole.
                let mm = self.unit / 1000.0;
                let pins = SplitPins {
                    count: f.count("pins", 2)?,
                    radius: f.opt_len("pin")?,
                    depth: f.opt_len("depth")?,
                    dowels: f.flag("dowels", false)?,
                    clearance: f.opt_len("clearance")?.or(self.weave.map(|w| w.clearance)).unwrap_or(0.35 * mm),
                    clearance_end: self.weave.map_or(0.4 * mm, |w| w.clearance_z),
                    wall: self.weave.map_or(0.8 * mm, |w| w.bar),
                };
                if pins.radius.is_some_and(|r| !(r > 0.0)) || pins.depth.is_some_and(|d| !(d > 0.0)) {
                    return f.err("'pin' and 'depth' must be positive lengths");
                }
                if pins.dowels && pins.count == 0 {
                    return f.err("dowels: true needs pins (pins: 0 makes none)");
                }
                // An exact part splits exactly, its names carried to the halves.
                if matches!(self.graph.node(source).op, Op::Brep { .. }) {
                    Op::Brep { op: BrepOp::Split { center, normal, piece, pins }, inputs: vec![source] }
                } else {
                    Op::Split { source, center, normal, piece, pins }
                }
            }
            "Mirror" => {
                let source = field_child(self, "source")?;
                let plane = mirror_plane(f.choice("plane", "yz")?);
                Op::Mirror { source, plane, center: f.opt_p3("center")?.unwrap_or([0.0; 3]), keep: f.flag("keep", true)? }
            }
            // A closed shell of a distance-field solid stays one field (it renders and edits live).
            "Shell" if f.choice("open", "top")? == "closed" && f.need("source")?.obj().and_then(|o| o.proto.as_deref()).is_some_and(|p| FIELD_PARTS.contains(&p)) => {
                self.materials.clear();
                let (node, (min, max)) = self.sdf_body(&obj)?;
                let (materials, slots) = self.material_table();
                filaments = Some(slots);
                Op::Field { shape: SdfShape { root: SdfNode::Id { id: csg_sdf::part_id(id), of: Box::new(node) }, min, max, materials }, cell: f.opt_len("cell")? }
            }
            "Shell" => {
                if f.choice("side", "inside")? == "both" {
                    return f.err("side: @both shells a distance-field solid (Blend, Carve, Ellipsoid ...) with open: @closed");
                }
                let source = field_child(self, "source")?;
                let open = match f.choice("open", "top")? {
                    "bottom" => Open::Bottom,
                    "closed" => Open::Closed,
                    _ => Open::Top,
                };
                Op::Shell { source, thickness: f.len("thickness")?, open, cell: f.opt_len("cell")? }
            }
            // An offset of a distance-field solid stays one field; any other is read through its mesh.
            "Offset" if f.need("source")?.obj().and_then(|o| o.proto.as_deref()).is_some_and(|p| FIELD_PARTS.contains(&p)) => {
                let (op, slots) = self.field_op(obj, id, &f)?;
                filaments = Some(slots);
                op
            }
            "Offset" => {
                let source = field_child(self, "source")?;
                Op::Offset { source, distance: f.len("distance")?, cell: f.opt_len("cell")? }
            }
            "Union" | "Difference" | "Intersect" => {
                let list = self.operands(&f)?;
                let mut nodes = Vec::new();
                for v in list {
                    nodes.push(self.child(v, id.to_string(), index)?);
                }
                if nodes.len() <= 1 {
                    let node = match nodes.first() {
                        Some(node) => *node,
                        None => self.push(&f, Op::Mesh { mesh: Arc::new(makepad_csg_graph::TriMesh::new()) })?,
                    };
                    return self.placed(&f, node);
                }
                let a = nodes.remove(0);
                let b = nodes;
                let mode = match f.proto.name {
                    "Union" => BoolMode::Union,
                    "Difference" => BoolMode::Difference,
                    _ => BoolMode::Intersection,
                };
                let exact = std::iter::once(&a).chain(&b).any(|n| matches!(self.graph.node(*n).op, Op::Brep { .. }));
                if exact {
                    let (line, col) = obj.loc.unwrap_or((1, 1));
                    self.notes.push(Diag { line, col, message: format!("{id}: {} goes through the mesh: the exact part in it is meshed here and stops being exact; m.brep.{} keeps it exact when both are m.brep parts", f.proto.call, f.proto.call) });
                }
                Op::Boolean { mode, a, b }
            }
            "Sdf" => {
                let V::Fn(func) = f.need("field")? else { return f.err("'field' is a fn(p) { return ... } giving the distance") };
                let bounds = match f.need("bounds")? {
                    V::Arr(b) if b.len() == 2 => [f.p3_of("bounds", &b[0])?, f.p3_of("bounds", &b[1])?],
                    other => return f.err(format!("'bounds' is [[x y z] [x y z]], not {}", other.describe())),
                };
                let kernel = self.kernel(&f, *func, (bounds[0], bounds[1]))?;
                // The fn reads points in metres: the kernel's are scaled into it.
                let root = SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: self.unit, of: Box::new(SdfNode::Kernel(kernel)) };
                filaments = Some(vec![None]);
                Op::Field { shape: SdfShape { root: SdfNode::Id { id: csg_sdf::part_id(id), of: Box::new(root) }, min: bounds[0], max: bounds[1], materials: Vec::new() }, cell: f.opt_len("cell")? }
            }
            "Import" => {
                let name = f.text("name")?;
                let mesh = self.assets.get(&name).ok_or_else(|| ()).or_else(|_| f.err(format!("no mesh '{name}' in the project (meshes: {})", { let mut k: Vec<&String> = self.assets.keys().collect(); k.sort(); k.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ") })))?;
                let mut tri = makepad_csg_graph::TriMesh::new();
                for p in &mesh.data.positions {
                    tri.add_vertex(makepad_csg_math::Vec3d::new(p[0] as f64, p[1] as f64, p[2] as f64));
                }
                for t in &mesh.data.triangles {
                    tri.add_triangle(t[0], t[1], t[2]);
                }
                self.solids[index].asset = Some(name);
                Op::Mesh { mesh: Arc::new(tri) }
            }
            "Cage" => {
                let (points, faces, levels) = cage_of(&f)?;
                Op::Cage { points, faces, levels }
            }
            "Links" => {
                let wp = self.weave_profile(&f)?;
                let sizes = weave_sizes(&f, "size")?;
                let link = SquareLink::new(&wp, None);
                let frame = Frame::new([0.0; 3], [0.0, 1.0, 0.0]);
                let (path, closed) = match f.need("path")? {
                    V::Arr(list) => {
                        let path = list.iter().map(|v| {
                            let p = frame.to_local(f.p3_of("path", v)?);
                            Ok([p[0], p[1]])
                        }).collect::<Res<Vec<_>>>()?;
                        (path, f.flag("closed", false)?)
                    }
                    _ => {
                        let loops = self.weave_loops(&f, "path", frame)?;
                        (loops.into_iter().next().unwrap_or_default(), f.flag("closed", true)?)
                    }
                };
                if path.len() < 2 {
                    return f.err(format!("the path is shorter than two links (pitch {} mm)", link.chain_pitch()));
                }
                let length: f64 = path.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum::<f64>()
                    + if closed && path.len() > 2 { let (a, b) = (path[0], path[path.len() - 1]); ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt() } else { 0.0 };
                let weave = weave::chain_graded(&wp, link, frame, &path, closed, &|t| profile_at(&sizes, link.a, t / length.max(1e-9)));
                if weave.links.len() < 2 {
                    return f.err(format!("the path is shorter than two links (pitch {} mm)", link.chain_pitch()));
                }
                Op::Links { links: Arc::new(weave.links()), sides: f.count("sides", 12)? }
            }
            "Mail" => {
                if f.need("region")?.obj().is_some_and(|o| o.part.is_some()) {
                    field_child(self, "region")?;
                }
                let weave = self.weave_mail(&f)?;
                Op::Links { links: Arc::new(weave.links()), sides: f.count("sides", 12)? }
            }
            "Drape" => {
                field_child(self, "mail")?;
                field_child(self, "surface")?;
                let (weave, _, _) = self.draped(&f)?;
                Op::Links { links: Arc::new(weave.links()), sides: f.count("sides", 12)? }
            }
            "Form" => {
                // the printed support under a drape: its surface, cut to the links' footprint, from the bed up
                let Some(skin) = f.need("drape")?.obj().filter(|o| o.proto.as_deref() == Some("Drape")) else { return f.err("'drape' is a m.drape(...)") };
                let df = self.fields(skin)?;
                let (weave, surface, (lo, hi)) = self.draped(&df)?;
                let links = weave.links();
                let margin = f.opt_len("margin")?.unwrap_or(0.0).max(0.0);
                let (mut min, mut max) = (links.min, links.max);
                min[1] = 0.0;
                for k in [0, 2] {
                    min[k] = (min[k] - margin).max(lo[k]);
                    max[k] = (max[k] + margin).min(hi[k]);
                }
                max[1] = max[1].min(hi[1]);
                if (0..3).any(|k| max[k] <= min[k]) {
                    return f.err("the drape's links are over none of its surface");
                }
                let size: P3 = std::array::from_fn(|k| max[k] - min[k]);
                let centre: P3 = std::array::from_fn(|k| 0.5 * (min[k] + max[k]));
                let cut = SdfNode::Transform { at: centre, rotate: [0.0; 3], scale: 1.0, of: Box::new(SdfNode::Box { size, radius: 0.0 }) };
                let root = SdfNode::Combine { op: csg_sdf::SdfOp::Intersection, blend: 0.0, of: vec![surface, cut] };
                self.materials.clear();
                let (materials, slots) = self.material_table();
                filaments = Some(slots);
                Op::Field { shape: SdfShape { root: SdfNode::Id { id: csg_sdf::part_id(id), of: Box::new(root) }, min, max, materials }, cell: f.opt_len("cell")? }
            }
            "Instance" if !f.need("source")?.obj().and_then(|o| o.proto.as_deref()).is_some_and(|p| FIELD_PARTS.contains(&p)) => {
                // Copies of a meshed solid: one transform each, joined.
                let source = field_child(self, "source")?;
                let mut copies = Vec::new();
                for (at, rotate, scale) in self.places(&f, f.need("places")?)? {
                    copies.push(self.push(&f, Op::Transform { source, at, rotate, scale: [scale; 3] })?);
                }
                if copies.is_empty() {
                    return f.err("'places' is empty");
                }
                let a = copies.remove(0);
                Op::Boolean { mode: BoolMode::Union, a, b: copies }
            }
            "Blend" | "Carve" | "Instance" | "Bone" | "Twist" | "Bend" | "Taper" | "Fold" | "Ellipsoid" | "Capsule" | "Tube" | "Texture" | "Voronoi" | "Grow" | "Veins" => {
                let (op, slots) = self.field_op(obj, id, &f)?;
                filaments = Some(slots);
                op
            }
            other => return f.err(format!("'{other}' is not a solid")),
        };
        let node = self.push(&f, op)?;
        if let Some(filaments) = filaments {
            self.material_filaments.insert(node, filaments);
        }
        self.placed(&f, node)
    }

    /// Placement also applies when a boolean passes a single operand through.
    fn placed(&mut self, f: &Fields, node: NodeId) -> Res<NodeId> {
        // Placement: scale, then rotate, then move.
        // An exact hole's `at` places the cutters on its source.
        let at = if f.proto.name == "BrepHole" { None } else { f.opt_p3("at")? };
        let rotate = f.rotation("rotate")?;
        // A part with a scale of its own (a sweep's sizes along the way) takes no placement scale.
        let scale = match f.get("scale") {
            _ if f.proto.fields.iter().any(|x| x.key == "scale") => None,
            Some(V::Num(s)) => Some([*s; 3]),
            Some(v) => {
                let a = f.nums("scale", v, 3)?;
                Some([a[0], a[1], a[2]])
            }
            None => None,
        };
        let exact = matches!(self.graph.node(node).op, Op::Brep { .. });
        // Each exact solid names its faces at its own placement. A hole drilled under
        // scale or a skipped step already ends in one.
        if at.is_none() && rotate.is_none() && scale.is_none()
            && (!exact || matches!(self.graph.node(node).op, Op::Brep { op: BrepOp::Place { .. }, .. })) {
            return Ok(node);
        }
        let (at, rotate, scale) = (at.unwrap_or([0.0; 3]), rotate.unwrap_or([0.0; 3]), scale.unwrap_or([1.0; 3]));
        // A placed body stays a body: the exact ops that follow can still read it.
        if exact {
            return self.push(&f, Op::Brep { op: BrepOp::Place { at, rotate, scale }, inputs: vec![node] });
        }
        self.push(&f, Op::Transform { source: node, at, rotate, scale })
    }

    /// A drape's links rested on its surface (each `clearance_z` over it at its closest), the surface's
    /// field and bounds. `f` is the m.drape(...) call's fields.
    fn draped(&mut self, f: &Fields) -> Res<(Weave, SdfNode, (P3, P3))> {
        let Some(mail) = f.need("mail")?.obj().filter(|o| o.proto.as_deref() == Some("Mail")) else { return f.err("'mail' is a m.mail(...)") };
        let mf = self.fields(mail)?;
        let wp = self.weave_profile(&mf)?;
        let mut weave = self.weave_mail(&mf)?;
        let frame = weave.frame;
        self.materials.clear();
        let (surface, (lo, hi)) = self.sdf_value(f.need("surface")?, &f)?;
        self.materials.clear();
        weave::drape(&mut weave, &|p| {
            let mut q = frame.to_model([p[0], p[1], 0.0]);
            if q[0] < lo[0] || q[0] > hi[0] || q[2] < lo[2] || q[2] > hi[2] {
                return None;
            }
            let distance = |p: P3| csg_sdf::distance(&surface, makepad_csg_math::Vec3d::new(p[0], p[1], p[2]));
            q[1] = hi[1] + 0.25;
            while q[1] > lo[1] {
                let mut above = q[1];
                q[1] = (q[1] - 0.25).max(lo[1]);
                if distance(q) > 0.0 {
                    continue;
                }
                let mut below = q[1];
                while above - below > 0.01 {
                    q[1] = 0.5 * (above + below);
                    if distance(q) <= 0.0 {
                        below = q[1];
                    } else {
                        above = q[1];
                    }
                }
                q[1] = 0.5 * (above + below);
                let gradient: P3 = std::array::from_fn(|k| {
                    let (mut a, mut b) = (q, q);
                    a[k] += 0.05;
                    b[k] -= 0.05;
                    (distance(a) - distance(b)) / 0.1
                });
                let length = gradient.iter().map(|x| x * x).sum::<f64>().sqrt();
                let normal = if length > 0.0 { gradient.map(|x| x / length) } else { frame.up };
                return Some((q[1] + wp.c_z, frame.to_local(normal)));
            }
            None
        });
        let distance = |p: P3| csg_sdf::distance(&surface, makepad_csg_math::Vec3d::new(p[0], p[1], p[2]));
        weave::rest_on(&mut weave, &distance, wp.c_z, 0.5);
        Ok((weave, surface, (lo, hi)))
    }

    /// A field part's op and its material slots' filaments.
    fn field_op(&mut self, obj: &Arc<Obj>, id: &str, f: &Fields) -> Res<(Op, Vec<Option<u32>>)> {
        self.materials.clear();
        let (node, (min, max)) = self.sdf_body(obj)?;
        let (materials, slots) = self.material_table();
        Ok((Op::Field { shape: SdfShape { root: SdfNode::Id { id: csg_sdf::part_id(id), of: Box::new(node) }, min, max, materials }, cell: f.opt_len("cell")? }, slots))
    }

    fn weave_profile(&self, f: &Fields) -> Res<WeaveProfile> {
        let length = |key: &str, default: Option<f64>| -> Res<f64> {
            match f.opt_len(key)?.or(default) {
                Some(n) => Ok(n),
                None => f.err(format!("'{key}' is needed here (no printer defaults)")),
            }
        };
        let r = length("bar", self.weave.map(|w| w.bar))?;
        let c_xy = length("clearance", self.weave.map(|w| w.clearance))?;
        let c_z = length("clearance_z", self.weave.map(|w| w.clearance_z))?;
        let leg = f.opt_num("leg")?.unwrap_or(std::f64::consts::FRAC_PI_4);
        if !r.is_finite() || r <= 0.0 || !c_xy.is_finite() || c_xy < 0.0 || !c_z.is_finite() || c_z < 0.0 {
            return f.err("'bar' must be positive and clearances non-negative finite lengths");
        }
        if leg <= 0.0 || leg >= std::f64::consts::FRAC_PI_2 {
            return f.err("'leg' must be between 0 and 90 degrees");
        }
        Ok(WeaveProfile { r, c_xy, c_z, layer: c_z * 0.5, leg, bridge_max: 10.0, foot: 0.25 })
    }

    /// Sketch outlines use the same plane and flattening tolerance as a default Extrude.
    fn weave_loops(&mut self, f: &Fields, key: &str, frame: Frame) -> Res<Vec<Vec<[f64; 2]>>> {
        let sketch = self.sketch(f, key)?;
        let loops = sketch.loops(&makepad_csg_graph::Tolerance::default()).or_else(|e| f.err(e))?;
        let (u, v, _) = makepad_csg_graph::eval::plane_frame(Plane::Xz);
        Ok(loops.into_iter().map(|l| l.into_iter().map(|p| {
            let q = u * p[0] + v * p[1];
            let q = frame.to_local([q.x, q.y, q.z]);
            [q[0], q[1]]
        }).collect()).collect())
    }

    /// Generate the flat mail again when a Drape needs its links before meshing.
    fn weave_mail(&mut self, f: &Fields) -> Res<Weave> {
        let wp = self.weave_profile(f)?;
        let link = SquareLink::new(&wp, None);
        let sizes = weave_sizes(f, "size")?;
        let sizes_y = match f.get("size_y") {
            Some(_) => weave_sizes(f, "size_y")?,
            None => sizes.clone(),
        };
        let frame = Frame::new([0.0; 3], [0.0, 1.0, 0.0]);
        let angle = f.opt_num("angle")?.unwrap_or(0.0);
        if f.choice("fill", "footprint")? == "volume" {
            return self.weave_volume(f, &wp, frame, angle);
        }
        let weave = if f.need("region")?.obj().is_some_and(|o| o.part.is_some()) {
            self.materials.clear();
            let (field, (min, max)) = self.sdf_value(f.need("region")?, f).or_else(|_| f.err("a mail region is a Shape, a Sketch or an SDF solid"))?;
            self.materials.clear();
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for c in 0..8 {
                let q = frame.to_local(std::array::from_fn(|k| if c & (1 << k) == 0 { min[k] } else { max[k] }));
                for k in 0..2 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
            }
            // a graded list spans the region along each axis (clamped past it when the weave is turned)
            let half = [0.5 * (hi[0] - lo[0]).max(1e-9), 0.5 * (hi[1] - lo[1]).max(1e-9)];
            weave::mail_graded(&wp, link, frame, lo, hi, angle, &|p| {
                let q = frame.to_model([p[0], p[1], link.thickness() * 0.5]);
                if csg_sdf::distance(&field, makepad_csg_math::Vec3d::new(q[0], q[1], q[2])) <= 0.0 { -1.0 } else { 1.0 }
            }, &|u| profile_at(&sizes, link.a, 0.5 + 0.5 * u / half[0]), &|v| profile_at(&sizes_y, link.a, 0.5 + 0.5 * v / half[1]))
        } else {
            let loops = self.weave_loops(f, "region", frame)?;
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for p in loops.iter().flatten() {
                for k in 0..2 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            // a graded list spans the region along each axis (clamped past it when the weave is turned)
            let half = [0.5 * (hi[0] - lo[0]).max(1e-9), 0.5 * (hi[1] - lo[1]).max(1e-9)];
            weave::mail_graded(&wp, link, frame, lo, hi, angle, &|p| {
                let inside = loops.iter().fold(false, |inside, l| inside ^ makepad_csg_cad::region::contains(l, p));
                if inside { -1.0 } else { 1.0 }
            }, &|u| profile_at(&sizes, link.a, 0.5 + 0.5 * u / half[0]), &|v| profile_at(&sizes_y, link.a, 0.5 + 0.5 * v / half[1]))
        };
        if weave.links.is_empty() {
            return f.err(format!("no link fits in the region: links are {} mm across", 2.0 * (link.a + link.r)));
        }
        Ok(weave)
    }

    /// A solid's volume filled with leaning links ([`weave::trellis`]): every link stands on the bed.
    fn weave_volume(&mut self, f: &Fields, wp: &WeaveProfile, frame: Frame, angle: f64) -> Res<Weave> {
        if !f.need("region")?.obj().is_some_and(|o| o.part.is_some()) {
            return f.err("fill: @volume fills a solid: the region is an SDF solid (a Shape or Sketch has no volume)");
        }
        let lean = f.opt_num("lean")?.unwrap_or(60f64.to_radians());
        if !(lean >= wp.leg - 1e-9 && lean < std::f64::consts::FRAC_PI_2) {
            return f.err(format!("'lean' is between {:.0}deg (the leg angle) and 90deg", wp.leg.to_degrees()));
        }
        self.materials.clear();
        let (field, (min, max)) = self.sdf_value(f.need("region")?, f).or_else(|_| f.err("a mail region is a Shape, a Sketch or an SDF solid"))?;
        self.materials.clear();
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for c in 0..8 {
            let q = frame.to_local(std::array::from_fn(|k| if c & (1 << k) == 0 { min[k] } else { max[k] }));
            for k in 0..3 {
                lo[k] = lo[k].min(q[k]);
                hi[k] = hi[k].max(q[k]);
            }
        }
        // the links stand on the bed: the fill starts there whatever the solid's own bottom
        lo[2] = lo[2].max(0.0);
        let link = weave::LeanLink::new(wp, lean);
        let weave = weave::trellis(wp, link, frame, lo, hi, angle, 0.0, &|p| csg_sdf::distance(&field, { let q = frame.to_model(p); makepad_csg_math::Vec3d::new(q[0], q[1], q[2]) }));
        if weave.links.is_empty() {
            return f.err(format!("no leaning link fits in the solid on the bed: the smallest stands {:.1} mm tall and reaches {:.1} mm along the lean", link.rise(0) + 2.0 * wp.r, link.run(0) + 2.0 * wp.r));
        }
        Ok(weave)
    }


    /// A solid as a distance field node (kernel units) and a box it fits in: what Blend and
    /// Carve combine.
    fn sdf_of(&mut self, obj: &Arc<Obj>) -> Res<(SdfNode, ([f64; 3], [f64; 3]))> {
        let (node, bounds) = self.sdf_body(obj)?;
        let f = self.fields(obj)?;
        let id = csg_sdf::part_id(&self.part_id(obj));
        let (node, bounds) = self.sdf_placed(&f, node, bounds)?;
        Ok((SdfNode::Id { id, of: Box::new(node) }, bounds))
    }

    /// A part's source id (its name, or its builder and line).
    fn part_id(&self, obj: &Obj) -> String {
        if let Some(generated) = obj.part.and_then(|p| self.gen_ids.get(&p)) {
            return generated.clone();
        }
        let name = obj.part.map(|p| self.part_names[p].clone()).unwrap_or_default();
        if !name.is_empty() {
            return name;
        }
        self.line_name(protos::proto(obj.proto.as_deref().unwrap_or("")).map_or("", |p| p.call), obj.loc)
    }

    /// An unnamed part's id: its builder and line (`box@6`), with a number when a root's
    /// `name` gave another part that name (`box@6 2`).
    fn line_name(&self, call: &str, loc: Option<(u32, u32)>) -> String {
        let id = match loc {
            Some((line, _)) => format!("{call}@{line}"),
            None => call.to_string(),
        };
        if !self.named_lines.contains(&id) {
            return id;
        }
        (2..).map(|k| format!("{id} {k}")).find(|n| !self.named_lines.contains(n)).unwrap()
    }

    /// The field's material and filament tables: the default, then each slot.
    fn material_table(&mut self) -> (Vec<csg_sdf::SdfMaterial>, Vec<Option<u32>>) {
        if self.materials.is_empty() {
            return (Vec::new(), vec![None]);
        }
        std::iter::once((csg_sdf::SdfMaterial::default(), None)).chain(self.materials.drain(..).map(|(_, m, filament)| (m, filament))).unzip()
    }

    /// One placement (kernel units, degrees): a Place{at rotate scale} or a point.
    fn place(&mut self, f: &Fields, v: &V) -> Res<(P3, P3, f64)> {
        match v {
            V::Arr(_) => Ok((f.p3_of("places", v)?, [0.0; 3], 1.0)),
            V::Obj(o) if o.proto.as_deref() == Some("Place") => {
                let pf = self.fields(o)?;
                let rotate = pf.rotation("rotate")?.unwrap_or([0.0; 3]);
                Ok((pf.opt_p3("at")?.unwrap_or([0.0; 3]), rotate, pf.opt_num("scale")?.unwrap_or(1.0)))
            }
            other => f.err(format!("a place is Place{{at rotate scale}} or [x y z], not {}", other.describe())),
        }
    }

    fn places(&mut self, f: &Fields, v: &V) -> Res<Vec<(P3, P3, f64)>> {
        match v {
            V::Arr(list) if list.iter().all(|x| matches!(x, V::Num(_))) => Ok(vec![self.place(f, v)?]),
            V::Arr(list) => list.iter().map(|x| self.place(f, x)).collect(),
            one => Ok(vec![self.place(f, one)?]),
        }
    }

    /// A part's distance field before its own placement (kernel units) and a box it fits in.
    fn sdf_body(&mut self, obj: &Arc<Obj>) -> Res<(SdfNode, ([f64; 3], [f64; 3]))> {
        if let Some(body) = obj.part.and_then(|p| self.bodies.get(&p)) {
            return Ok(body.clone());
        }
        let body = self.sdf_lowered(obj)?;
        if let Some(p) = obj.part {
            self.bodies.insert(p, body.clone());
        }
        Ok(body)
    }

    /// [`Lower::sdf_body`] the first time a compile asks for the part.
    fn sdf_lowered(&mut self, obj: &Arc<Obj>) -> Res<(SdfNode, ([f64; 3], [f64; 3]))> {
        use csg_sdf::{SdfDeform, SdfPlace};
        use makepad_csg_graph::SdfOp;
        let f = self.fields(obj)?;
        let around = |h: [f64; 3]| ([-h[0], -h[1], -h[2]], h);
        let union = |a: ([f64; 3], [f64; 3]), b: ([f64; 3], [f64; 3])| (std::array::from_fn(|i| a.0[i].min(b.0[i])), std::array::from_fn(|i| a.1[i].max(b.1[i])));
        let grow = |b: ([f64; 3], [f64; 3]), r: f64| (b.0.map(|x| x - r), b.1.map(|x| x + r));
        let (node, bounds) = match f.proto.name {
            "Box" => {
                if f.opt_p2("taper")? != [0.0; 2] || f.opt_len("shift")?.unwrap_or(0.0) != 0.0 {
                    return f.err("a box with 'taper' or 'shift' has no distance field");
                }
                let size = f.p3("size")?;
                (SdfNode::Box { size, radius: f.opt_len("radius")?.unwrap_or(0.0) }, around(size.map(|x| x * 0.5)))
            }
            "Sphere" => {
                let r = f.len("radius")?;
                (SdfNode::Sphere { radius: r }, around([r; 3]))
            }
            "Cylinder" => {
                let (r, h) = (f.len("radius")?, f.len("height")?);
                let top = f.opt_len("top_radius")?.unwrap_or(r);
                (SdfNode::Cylinder { radius: r, top_radius: top, height: h }, around([r.max(top), h * 0.5, r.max(top)]))
            }
            "Torus" => {
                let (r, t) = (f.len("radius")?, f.len("tube")?);
                (SdfNode::Torus { radius: r, tube: t }, around([r + t, t, r + t]))
            }
            "Ellipsoid" => {
                let radii = f.p3("radii")?;
                (SdfNode::Ellipsoid { radii }, around(radii))
            }
            "Capsule" => {
                let (a, b, radius) = (f.p3("a")?, f.p3("b")?, f.len("radius")?);
                if radius <= 0.0 {
                    return f.err(format!("'radius' must be more than 0, not {}", radius / self.unit));
                }
                let ends = (std::array::from_fn(|k| a[k].min(b[k])), std::array::from_fn(|k| a[k].max(b[k])));
                (SdfNode::Capsule { a, b, radius }, grow(ends, radius))
            }
            "Tube" => {
                let curve = |key: &str, v: &V| -> Res<Vec<[f64; 4]>> {
                    let V::Arr(list) = v else { return f.err(format!("'{key}' is a list of [x y z r], not {}", v.describe())) };
                    if list.is_empty() {
                        return f.err(format!("'{key}' has a tube with no points"));
                    }
                    list.iter()
                        .map(|p| {
                            let a = f.nums(key, p, 4)?;
                            if a[3].is_nan() || a[3] < 0.0 {
                                return f.err(format!("'{key}': a radius is a length of 0 or more, not {}", a[3]));
                            }
                            Ok([a[0] * f.unit, a[1] * f.unit, a[2] * f.unit, a[3] * f.unit])
                        })
                        .collect()
                };
                let curves = match (f.get("points"), f.get("curves")) {
                    (Some(v), None) => vec![curve("points", v)?],
                    (None, Some(V::Arr(list))) if !list.is_empty() => list.iter().map(|c| curve("curves", c)).collect::<Res<Vec<_>>>()?,
                    (None, Some(other)) => return f.err(format!("'curves' is a list of tubes, each [[x y z r] ...], not {}", other.describe())),
                    _ => return f.err("give 'points' [[x y z r] ...] for one tube, or 'curves' [[[x y z r] ...] ...] for many"),
                };
                let blend = f.opt_len("blend")?.unwrap_or(0.0);
                if blend < 0.0 {
                    return f.err(format!("'blend' is a width of 0 or more, not {blend}"));
                }
                let tubes = csg_sdf::Tubes::new(curves.into_iter().map(|pts| csg_sdf::TubeCurve { pts, slot: 0 }).collect(), blend);
                let b = (tubes.min, tubes.max);
                (SdfNode::Tubes(Arc::new(tubes)), b)
            }
            "Shell" => {
                if f.choice("open", "top")? != "closed" {
                    return f.err("a shell inside a distance field is open: @closed (cut its opening with m.carve)");
                }
                let t = f.len("thickness")?;
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                // Shell is |d| - t/2: a wall t thick on the surface, or moved inside it first.
                let (of, b) = match f.choice("side", "inside")? {
                    "both" => (of, (b.0.map(|x| x - t * 0.5), b.1.map(|x| x + t * 0.5))),
                    _ => (SdfNode::Round { radius: -t * 0.5, of: Box::new(of) }, b),
                };
                (SdfNode::Shell { thickness: t * 0.5, of: Box::new(of) }, b)
            }
            "Offset" => {
                let d = f.len("distance")?;
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                (SdfNode::Round { radius: d, of: Box::new(of) }, grow(b, d.max(0.0)))
            }
            "Import" => {
                let name = f.text("name")?;
                let mesh = self.assets.get(&name).cloned().ok_or(()).or_else(|_| f.err(format!("no mesh '{name}' in the project")))?;
                let b = (mesh.min.map(|x| x as f64), mesh.max.map(|x| x as f64));
                (SdfNode::Mesh(mesh), b)
            }
            "Cage" => {
                use makepad_csg_graph::subdiv::{catmull_clark, Levels};
                let (points, faces, levels) = cage_of(&f)?;
                let levels = levels.map_or(Levels::Chord(makepad_csg_graph::PRINT_CHORD), Levels::Steps);
                let points: Vec<makepad_csg_math::Vec3d> = points.iter().map(|p| makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).collect();
                let mesh = catmull_clark(&points, &faces, levels, false).or_else(|e| f.err(e))?;
                let data = csg_sdf::mesh_field::MeshData { positions: mesh.vertices.iter().map(|v| [v.x as f32, v.y as f32, v.z as f32]).collect(), triangles: mesh.triangles };
                let field = csg_sdf::mesh_field::MeshField::new(data);
                let b = (field.min.map(|x| x as f64), field.max.map(|x| x as f64));
                (SdfNode::Mesh(Arc::new(field)), b)
            }
            // A sketch's regions at the mesh extrusion's flattening, pulled along its plane's normal
            // or turned about Y as the mesh ops do.
            "Extrude" | "Revolve" => {
                let sketch = self.sketch(&f, "sketch")?;
                let regions = sketch.regions(&makepad_csg_graph::Tolerance::default()).or_else(|e| f.err(e))?;
                let loops: Vec<Vec<Vec<[f64; 2]>>> = regions.iter().map(|r| r.loops().cloned().collect()).collect();
                let form = if f.proto.name == "Extrude" {
                    let (u, v, w) = makepad_csg_graph::eval::plane_frame(plane_of(f.choice("plane", "xz")?));
                    let h = f.len("height")?;
                    let (z0, z1) = if f.flag("both", false)? { (-h * 0.5, h * 0.5) } else { (0.0, h) };
                    csg_sdf::SketchForm::Extrude { u: [u.x, u.y, u.z], v: [v.x, v.y, v.z], w: [w.x, w.y, w.z], z0, z1, draft: f.opt_angle("draft")?.unwrap_or(0.0) }
                } else {
                    csg_sdf::SketchForm::Revolve { angle: f.opt_angle("angle")?.unwrap_or(360.0) }
                };
                let solid = csg_sdf::SketchSolid::new(&loops, form).or_else(|e| f.err(e))?;
                let b = (solid.min, solid.max);
                (SdfNode::Sketch(Arc::new(solid)), b)
            }
            "Sdf" => {
                let V::Fn(func) = f.need("field")? else { return f.err("'field' is a fn(p)") };
                let bounds = match f.need("bounds")? {
                    V::Arr(b) if b.len() == 2 => (f.p3_of("bounds", &b[0])?, f.p3_of("bounds", &b[1])?),
                    other => return f.err(format!("'bounds' is [[x y z] [x y z]], not {}", other.describe())),
                };
                let kernel = self.kernel(&f, *func, bounds)?;
                (SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: self.unit, of: Box::new(SdfNode::Kernel(kernel)) }, bounds)
            }
            "Blend" | "Carve" => {
                let k = f.len("radius")?;
                let values = self.operands(&f)?;
                let mut parts = Vec::new();
                for v in values {
                    parts.push(self.sdf_value(v, &f)?);
                }
                // The distance to an empty mesh is infinite: all operands were bypassed.
                if parts.is_empty() {
                    return Ok((SdfNode::Mesh(Arc::new(csg_sdf::mesh_field::MeshField::new(Default::default()))), ([0.0; 3], [0.0; 3])));
                }
                if parts.len() == 1 {
                    return Ok(parts.remove(0));
                }
                let (op, b) = if f.proto.name == "Blend" {
                    (SdfOp::Union, parts.iter().map(|p| p.1).reduce(union).unwrap())
                } else {
                    (SdfOp::Difference, parts[0].1)
                };
                (SdfNode::Combine { op, blend: k, of: parts.into_iter().map(|p| p.0).collect() }, grow(b, k * 0.25))
            }
            "Instance" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let places: Vec<SdfPlace> = self.places(&f, f.need("places")?)?.into_iter().map(|(at, rotate, scale)| SdfPlace { at, rotate: csg_sdf::quat_from_degrees(rotate), scale }).collect();
                if places.is_empty() {
                    return f.err("'places' is empty");
                }
                let bounds = places.iter().map(|pl| csg_sdf::bounds::map_bounds(&b, |q| pl.out_of(q))).reduce(union).unwrap();
                (SdfNode::Instance { places, of: Box::new(of) }, bounds)
            }
            "Bone" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let rest = match f.get("rest") {
                    Some(v) => {
                        let (at, rotate, scale) = self.place(&f, v)?;
                        SdfPlace { at, rotate: csg_sdf::quat_from_degrees(rotate), scale }
                    }
                    None => SdfPlace::IDENTITY,
                };
                (SdfNode::Bone { id: csg_sdf::part_id(&self.part_id(obj)), rest, of: Box::new(of) }, b)
            }
            "Twist" | "Bend" | "Taper" | "Fold" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                // Rates are per metre; the field's points are kernel units.
                let deform = match f.proto.name {
                    "Twist" => SdfDeform::Twist { rate: f.num("rate")? / self.unit },
                    "Bend" => SdfDeform::Bend { rate: f.num("rate")? / self.unit },
                    "Taper" => SdfDeform::Taper { rate: f.num("rate")? / self.unit },
                    _ => SdfDeform::Mirror { axis: match f.choice("axis", "x")? {
                        "y" => 1,
                        "z" => 2,
                        _ => 0,
                    } },
                };
                let lipschitz = deform.lipschitz(Some(&b));
                (SdfNode::Deform { deform, lipschitz, of: Box::new(of) }, csg_sdf::deform_bounds(&deform, &b))
            }
            "Texture" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let tex = self.texture(&f, b)?;
                let b = csg_sdf::texture::texture_bounds(&tex, b);
                (SdfNode::Texture { tex: Arc::new(tex), of: Box::new(of) }, b)
            }
            "Voronoi" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let part = self.part_id(obj);
                (self.voronoi(&f, &part, of, b)?, b)
            }
            "Grow" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let part = self.part_id(obj);
                (self.grow(&f, &part, of, b)?, b)
            }
            "Veins" => {
                let (of, b) = self.sdf_value(f.need("source")?, &f)?;
                let part = self.part_id(obj);
                self.veins(&f, &part, of, b)?
            }
            other => return f.err(format!("{other} has no distance field: use {}", FIELD_PARTS.iter().chain(SKETCH_FIELDS).copied().collect::<Vec<_>>().join(", "))),
        };
        Ok((node, bounds))
    }

    /// A field node under a part's placement: a rigid move and a uniform scale keep it a distance.
    fn sdf_placed(&mut self, f: &Fields, node: SdfNode, bounds: ([f64; 3], [f64; 3])) -> Res<(SdfNode, ([f64; 3], [f64; 3]))> {
        let at = f.opt_p3("at")?;
        let rotate = f.rotation("rotate")?;
        let scale = match f.get("scale") {
            Some(V::Num(s)) => Some(*s),
            Some(_) => return f.err("inside a distance field a scale is one number (a uniform scale keeps the distance)"),
            None => None,
        };
        if at.is_none() && rotate.is_none() && scale.is_none() {
            return Ok((node, bounds));
        }
        let (at, rotate, scale) = (at.unwrap_or([0.0; 3]), rotate.unwrap_or([0.0; 3]), scale.unwrap_or(1.0));
        let m = makepad_csg_graph::eval::transform_matrix(at, rotate, [scale; 3]);
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for c in 0..8 {
            let q = [if c & 1 == 0 { bounds.0[0] } else { bounds.1[0] }, if c & 2 == 0 { bounds.0[1] } else { bounds.1[1] }, if c & 4 == 0 { bounds.0[2] } else { bounds.1[2] }];
            for i in 0..3 {
                let x = m[i][0] * q[0] + m[i][1] * q[1] + m[i][2] * q[2] + m[i][3];
                lo[i] = lo[i].min(x);
                hi[i] = hi[i].max(x);
            }
        }
        Ok((SdfNode::Transform { at, rotate, scale, of: Box::new(node) }, (lo, hi)))
    }

    fn sdf_value(&mut self, v: &V, f: &Fields) -> Res<(SdfNode, ([f64; 3], [f64; 3]))> {
        match v {
            V::Obj(o) if o.part.is_some() => {
                // An operand is a solid of its own (every part is lowered in build order).
                if let (Some(&i), Some(&parent)) = (o.part.and_then(|p| self.memo.get(&p)), self.lowering.last()) {
                    if self.solids[i].parent.is_none() && i != parent {
                        self.solids[i].parent = Some(parent);
                        self.solids[parent].children.push(i);
                    }
                }
                self.sdf_of(o)
            }
            other => f.err(format!("a solid to blend, not {}", other.describe())),
        }
    }


    /// A `Mix{materials field sharp balance}`: its field compiled as a kernel of a point in metres.
    fn mix(&mut self, obj: &Obj) -> Res<MixArg> {
        let f = self.fields(obj)?;
        let materials = match f.need("materials")? {
            V::Arr(a) if a.len() >= 2 => a.clone(),
            other => return f.err(format!("'materials' is a list of two or more: [Material{{..}} Material{{..}}], not {}", other.describe())),
        };
        let sharp = f.opt_num("sharp")?.unwrap_or(0.0);
        if !(0.0..=1.0).contains(&sharp) {
            return f.err(format!("'sharp' is 0..1, not {sharp}"));
        }
        let balance = f.opt_num("balance")?.unwrap_or(0.0);
        let field = match f.need("field")? {
            V::Obj(o) if o.proto.as_deref().is_some_and(|p| protos::GROWN.contains(&p)) => MixField::Pattern(self.grown(o)?),
            V::Fn(func) => {
                let Some(func) = func.as_object() else { return f.err("'field' is a fn") };
                let compiled = makepad_csg_sdf::SdfSplashExpr::compile(self.reader.vm, func, makepad_csg_sdf::SplashPoint::Vec3, &[]).map_err(|errors| {
                    let (line, col) = obj.loc.unwrap_or((1, 1));
                    Diag { line, col, message: format!("Mix field: {}", errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")) }
                })?;
                MixField::Fn(SdfKernel::splash(compiled, None))
            }
            _ => return f.err("'field' is a fn(p) { return ... } giving the place in materials (0 the first, 1 the second), or a grown pattern: Turing{preset: @spots scale: 5mm}, Lichtenberg{preset: @tree scale: 6mm}, Scales{preset: @spiral scale: 6mm}, Physarum{preset: @network scale: 8mm}, Flow{preset: @lines scale: 4mm}"),
        };
        Ok(MixArg { field, materials, sharp, balance })
    }

    /// A picture's or a swatch's fields; a picture's depth follows its image's.
    fn picture(&self, f: &Fields) -> Res<PictureArg> {
        let layers = f.count("layers", 4)?;
        if layers == 0 {
            return f.err(format!("'layers' must be positive, not {layers}"));
        }
        let layer = f.opt_len("layer")?.unwrap_or(0.16 * 0.001 * self.unit);
        let filaments = match f.get("filaments") {
            None => Vec::new(),
            Some(V::Arr(a)) if !a.is_empty() => a.clone(),
            Some(other) => return f.err(format!("'filaments' is a list of Material{{..}}, not {}", other.describe())),
        };
        let set = match f.get("set") {
            None => None,
            Some(V::Str(s)) => Some(s.clone()),
            Some(other) => return f.err(format!("'set' is a filament set's name, not {}", other.describe())),
        };
        if set.is_some() && !filaments.is_empty() {
            return f.err("give 'set' (a calibrated set) or 'filaments' (a first guess), not both");
        }
        let min_run = f.count("min_run", 2)?.max(1);
        let smooth = f.opt_num("smooth")?.unwrap_or(0.3);
        if !(0.0..=1.0).contains(&smooth) {
            return f.err(format!("'smooth' is 0..1, not {smooth}"));
        }
        let face_down = f.choice("face", "down")? == "down";
        let (image, size) = if f.proto.name == "Picture" {
            let image = f.text("image")?;
            let Some(&[w, h]) = self.images.get(&image) else {
                return f.err(format!("no picture '{image}' beside the script: give a PNG or a JPEG in its folder"));
            };
            let width = f.len("width")?;
            if !(width > 0.0) || w == 0 || h == 0 {
                return f.err("'width' must be over 0");
            }
            (Some(image), [width, width * h as f64 / w as f64])
        } else {
            (None, f.p2("size")?)
        };
        Ok(PictureArg { image, size, layers, layer, face_down, min_run, smooth, set, filaments })
    }

    /// A grown pattern (`Turing{..}`, `Lichtenberg{..}`, `Scales{..}`, `Physarum{..}`,
    /// `Flow{..}`): its preset (its family's first when none is given) and every number it gives,
    /// lengths in mm and angles in degrees.
    fn grown(&mut self, obj: &Obj) -> Res<PatternArg> {
        let f = self.fields(obj)?;
        let first = match f.proto.field("preset").map(|p| p.kind) {
            Some(protos::Kind::Choice(names)) => names[0],
            _ => return f.err("a grown pattern has presets"),
        };
        let preset = f.choice("preset", first)?;
        let mut values = Vec::new();
        for field in f.proto.fields {
            let v = match field.kind {
                protos::Kind::Length => f.opt_len(field.key)?,
                protos::Kind::Angle => f.opt_angle(field.key)?,
                protos::Kind::Count | protos::Kind::Number | protos::Kind::Ratio => f.opt_num(field.key)?,
                _ => None,
            };
            if let Some(v) = v {
                values.push((field.key, v));
            }
        }
        Ok(PatternArg { kind: f.proto.name, preset, values, loc: obj.loc })
    }

    /// A distance fn compiled as a kernel; `bounds` is the program's box in
    /// kernel units (the kernel keeps it in its own metres).
    fn kernel(&mut self, f: &Fields, func: makepad_script::ScriptValue, bounds: (P3, P3)) -> Res<SdfKernel> {
        let Some(obj) = func.as_object() else { return f.err("'field' is a fn") };
        // The kernel compiler reads the fn and its scope from the VM that made it.
        let compiled = makepad_csg_sdf::SdfSplashExpr::compile_traced(self.reader.vm, obj, makepad_csg_sdf::SplashPoint::Vec3, &[]).map_err(|errors| {
            let (line, col) = f.obj.loc.unwrap_or((1, 1));
            Diag { line, col, message: format!("Sdf field: {}", errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")) }
        })?;
        let local = (bounds.0.map(|x| x / self.unit), bounds.1.map(|x| x / self.unit));
        Ok(SdfKernel::splash(compiled, Some(local)))
    }

    fn sketch(&mut self, f: &Fields, key: &str) -> Res<Sketch> {
        let v = f.need(key)?;
        let list: Vec<&V> = match v {
            V::Obj(o) if o.proto.as_deref() == Some("Sketch") => {
                let sf = self.fields(o)?;
                match sf.need("shapes")? {
                    V::Arr(a) => a.iter().collect(),
                    one => vec![one],
                }
            }
            V::Arr(a) => a.iter().collect(),
            one => vec![one],
        };
        if list.is_empty() {
            return f.err(format!("'{key}' has no shapes"));
        }
        let mut shapes = Vec::new();
        let owner = self.lowering.last().copied().filter(|_| matches!(f.proto.name, "BrepExtrude" | "BrepRevolve"));
        let mut counted = 0;
        let mut offset = Some(0);
        for s in list {
            if let (Some(i), Some(path)) = (owner, s.obj().filter(|o| o.proto.as_deref() == Some("Path"))) {
                for shape in &shapes[counted..] {
                    offset = offset.zip(shape_side_count(shape)).map(|(n, count)| n + count);
                }
                counted = shapes.len();
                self.profile_paths.entry(i).or_default().push((path.ident, offset));
            }
            // SVG path data is as many outlines as it has closed subpaths.
            match s.obj().filter(|o| o.proto.as_deref() == Some("SvgPath")) {
                Some(o) => shapes.extend(self.svg_path(&self.fields(o)?)?),
                None => shapes.push(self.shape(s)?),
            }
        }
        if let (Some(i), Some(path), [Shape::Path { start, segs }]) = (owner, v.obj(), shapes.as_slice()) {
            if path.proto.as_deref() == Some("Path") {
                let solid = &mut self.solids[i];
                let indices = &self.path_segments[&path.ident];
                let mut seen = HashSet::new();
                let repeated: HashSet<_> = path.children.iter().filter_map(|(name, _)| name.as_deref()).filter(|name| !seen.insert(*name)).collect();
                for (k, (name, child)) in path.children.iter().enumerate() {
                    if name.as_deref().is_some_and(|n| repeated.contains(n) || child.obj().is_some_and(|o| indices[&o.ident].len() > 1)) {
                        solid.ambiguous_sides.push(k);
                    }
                    solid.sides.push(name.clone());
                }
                solid.sides.resize(path_side_count(*start, segs), None);
            }
        }
        Ok(Sketch::new(shapes))
    }

    /// An SvgPath's closed subpaths as polygons, curves flattened within 0.01 mm, y up.
    fn svg_path(&self, s: &Fields) -> Res<Vec<Shape>> {
        use makepad_svg::path::{PathCmd, VectorPath};
        let d = s.text("d")?;
        let mut path = VectorPath::new();
        makepad_svg::path_data::parse_path_data(&d, &mut path);
        // The flattening step in SVG units: 0.01 mm at the scale the drawing ends up at.
        let mm = self.unit / 1000.0;
        let controls: Vec<f64> = path.cmds.iter().flat_map(|c| match *c {
            PathCmd::MoveTo(x, _) | PathCmd::LineTo(x, _) => vec![x as f64],
            PathCmd::BezierTo(x1, _, x2, _, x, _) => vec![x1 as f64, x2 as f64, x as f64],
            _ => vec![],
        }).collect();
        let span = controls.iter().cloned().fold(f64::MIN, f64::max) - controls.iter().cloned().fold(f64::MAX, f64::min);
        let width = s.opt_len("width")?;
        let rough = match width {
            Some(w) if span > 0.0 => w / span,
            _ => mm,
        };
        let tol = 0.01 * mm / rough.max(1e-12);
        let mut loops: Vec<Vec<[f64; 2]>> = Vec::new();
        let mut current: Vec<[f64; 2]> = Vec::new();
        let finish = |current: &mut Vec<[f64; 2]>, loops: &mut Vec<Vec<[f64; 2]>>| {
            if current.len() > 1 && current.first() == current.last() {
                current.pop();
            }
            if current.len() >= 3 {
                loops.push(std::mem::take(current));
            }
            current.clear();
        };
        for cmd in &path.cmds {
            match *cmd {
                PathCmd::MoveTo(x, y) => {
                    finish(&mut current, &mut loops);
                    current.push([x as f64, y as f64]);
                }
                PathCmd::LineTo(x, y) => current.push([x as f64, y as f64]),
                PathCmd::BezierTo(x1, y1, x2, y2, x, y) => {
                    let p0 = *current.last().unwrap_or(&[x as f64, y as f64]);
                    let (p1, p2, p3) = ([x1 as f64, y1 as f64], [x2 as f64, y2 as f64], [x as f64, y as f64]);
                    // Steps from the curve's second differences: the chord error stays under tol.
                    let dd = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| ((a[0] - 2.0 * b[0] + c[0]).powi(2) + (a[1] - 2.0 * b[1] + c[1]).powi(2)).sqrt();
                    let bend = dd(p0, p1, p2).max(dd(p1, p2, p3));
                    let n = ((0.75 * bend / tol).sqrt().ceil() as usize).clamp(1, 512);
                    for i in 1..=n {
                        let t = i as f64 / n as f64;
                        let u = 1.0 - t;
                        let (a, b, c, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                        current.push([a * p0[0] + b * p1[0] + c * p2[0] + e * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + e * p3[1]]);
                    }
                }
                PathCmd::Close => {
                    let start = current.first().copied();
                    finish(&mut current, &mut loops);
                    // A path drawn on after Z starts where the closed one did.
                    current.extend(start);
                }
                PathCmd::Winding(_) => {}
            }
        }
        finish(&mut current, &mut loops);
        if loops.is_empty() {
            return s.err("the path data draws no outline of three or more points");
        }
        let (lo, hi) = loops.iter().flatten().fold((f64::MAX, f64::MIN), |(lo, hi), p| (lo.min(p[0]), hi.max(p[0])));
        let k = match width {
            Some(w) if hi > lo => w / (hi - lo),
            Some(_) => return s.err("the path data has no width to scale"),
            None => mm,
        };
        let at = s.opt_p2("at")?;
        Ok(loops.into_iter().map(|l| Shape::Polygon { points: l.into_iter().map(|p| [at[0] + p[0] * k, at[1] - p[1] * k]).collect(), fillet: 0.0 }).collect())
    }

    fn shape(&mut self, v: &V) -> Res<Shape> {
        let Some(o) = v.obj() else { return Err(Diag { line: 1, col: 1, message: format!("a sketch shape is Rect, Circle, Ellipse, Slot, Regular, Polygon, Path, Offset2D, Gear2D or SvgPath, not {}", v.describe()) }) };
        let s = self.fields(o)?;
        let at = s.opt_p2("at")?;
        let off = |p: [f64; 2]| [p[0] + at[0], p[1] + at[1]];
        Ok(match s.proto.name {
            "Rect" => Shape::Rect { center: at, size: s.p2("size")?, corner: s.opt_len("corner")?.unwrap_or(0.0), chamfer: s.opt_len("chamfer")?.unwrap_or(0.0) },
            "Circle" => Shape::Circle { center: at, r: s.len("radius")? },
            "Ellipse" => {
                let r = s.p2("radii")?;
                Shape::Ellipse { center: at, rx: r[0], ry: r[1] }
            }
            "Slot" => Shape::Slot { a: off(s.p2("from")?), b: off(s.p2("to")?), r: s.len("radius")? },
            "Regular" => {
                let sides = s.count("sides", 0)?;
                if sides < 3 {
                    return s.err("'sides' must be at least 3");
                }
                Shape::Regular { center: at, r: s.len("radius")?, sides: sides as usize, deg: s.opt_angle("angle")?.unwrap_or(0.0) }
            }
            "Polygon" => Shape::Polygon { points: s.points2("points")?.into_iter().map(off).collect(), fillet: s.opt_len("fillet")?.unwrap_or(0.0) },
            "Gear2D" => Shape::Gear {
                center: at,
                module: s.len("module")?,
                teeth: s.count("teeth", 0)? as usize,
                pressure_deg: s.opt_angle("pressure")?.unwrap_or(20.0),
                backlash: s.opt_len("backlash")?.unwrap_or(0.1 * self.unit / 1000.0),
                shift: s.opt_num("shift")?.unwrap_or(0.0),
                deg: s.opt_angle("angle")?.unwrap_or(0.0),
            },
            "SvgPath" => {
                let mut outlines = self.svg_path(&s)?;
                if outlines.len() != 1 {
                    return s.err(format!("this path data is {} outlines: put it in a sketch on its own, not inside another shape", outlines.len()));
                }
                outlines.pop().unwrap()
            }
            "Offset2D" => {
                let of = s.need("of")?;
                if of.obj().is_some_and(|o| o.proto.as_deref() == Some("Sketch")) || matches!(of, V::Arr(_)) {
                    return s.err("'of' is one shape: offset each shape of a sketch on its own");
                }
                Shape::Offset { of: Box::new(self.shape(of)?), d: s.len("d")? }
            }
            "Path" => {
                let start = off(s.opt_p2("from")?);
                let list: Vec<_> = if o.children.is_empty() {
                    let V::Arr(list) = s.need("segs")? else { return s.err("'segs' is a list of segments") };
                    list.iter().collect()
                } else {
                    o.children.iter().map(|(_, v)| v).collect()
                };
                let mut ids: HashMap<usize, Vec<usize>> = HashMap::new();
                let mut segs = Vec::with_capacity(list.len());
                for (i, g) in list.into_iter().enumerate() {
                    let Some(go) = g.obj() else {
                        if o.children.is_empty() {
                            return s.err(format!("a segment is LineTo, ArcTo, ... not {}", g.describe()));
                        }
                        let syn = &self.names.syntax;
                        let loc = self.names.site(o.loc).and_then(|site| edit::loc_at(syn, &site.span))
                            .and_then(|loc| syn.node(&loc)?.object())
                            .and_then(|(_, fields)| fields.iter().filter(|f| f.op == ":=" || f.name.is_none()).nth(i))
                            .map(|field| self.names.line_col(field.value.span.start));
                        let (line, col) = loc.or(o.loc).unwrap_or((1, 1));
                        return Err(Diag { line, col, message: format!("'{}' is not a path segment", g.describe()) });
                    };
                    let tag = go.proto.as_deref().unwrap_or("object");
                    if !protos::proto(tag).is_some_and(|p| p.group == protos::Group::Segment) {
                        let (line, col) = go.loc.or(o.loc).unwrap_or((1, 1));
                        return Err(Diag { line, col, message: format!("'{tag}' is not a path segment") });
                    }
                    ids.entry(go.ident).or_default().push(segs.len());
                    let gf = self.fields(go)?;
                    segs.push(match gf.proto.name {
                        "LineTo" => Seg::Line(off(gf.p2("to")?)),
                        "LineBy" => Seg::Rel(gf.p2("by")?),
                        "Horizontal" => Seg::H(gf.len("d")?),
                        "Vertical" => Seg::V(gf.len("d")?),
                        "Polar" => Seg::Polar { len: gf.len("length")?, deg: gf.num("angle")?.to_degrees() },
                        "ArcTo" => match gf.get("through") {
                            Some(_) => Seg::ArcThrough { through: off(gf.p2("through")?), to: off(gf.p2("to")?) },
                            None => Seg::ArcRadius { to: off(gf.p2("to")?), r: gf.len("r")?, large: gf.flag("large", false)? },
                        },
                        "TangentTo" => Seg::Tangent(off(gf.p2("to")?)),
                        "SplineTo" => Seg::Spline(gf.points2("points")?.into_iter().map(off).collect()),
                        "RoundCorner" => Seg::Fillet(gf.len("r")?),
                        "CutCorner" => Seg::Chamfer(gf.len("d")?),
                        other => return gf.err(format!("'{other}' is not a path segment")),
                    });
                }
                self.path_segments.insert(o.ident, ids);
                Shape::Path { start, segs }
            }
            other => return s.err(format!("'{other}' is not a sketch shape")),
        })
    }

    /// A B-rep step's op, or (its solid skipped, [`evaluate_skipping`]) its input as it is.
    fn skipped(&self, id: &str, op: BrepOp, source: NodeId) -> Op {
        let op = if self.skip.iter().any(|s| s == id) { BrepOp::Place { at: [0.0; 3], rotate: [0.0; 3], scale: [1.0; 3] } } else { op };
        Op::Brep { op, inputs: vec![source] }
    }

    /// Where the solid being lowered wrote its next selector ([`Solid::selectors`]).
    fn selector_at(&mut self, loc: Option<(u32, u32)>) {
        if let Some(&i) = self.lowering.last() {
            self.solids[i].selectors.push(loc);
        }
    }

    /// A face a B-rep step selects by, its place recorded ([`Solid::selectors`]).
    fn selector_face(&mut self, f: &Fields, key: &str, v: &V, input: NodeId) -> Res<FaceName> {
        let name = self.face_name(f, key, v, input)?;
        // A face its part names (`b.top`) was made with the part, at the part's call: no site
        // of its own ([`edit::selector_site`] finds it in the field).
        let made = v.obj().and_then(|o| o.loc);
        let of = v.obj().and_then(|o| o.get("of")).and_then(|part| part.obj()).and_then(|o| o.loc);
        self.selector_at(made.filter(|_| made != of));
        Ok(name)
    }

    /// Whether a drill under scale bypassed this origin on the way to the selector's input.
    /// A union can also bring the original placement back through another operand.
    fn lost_to_scaled_hole(&self, input: NodeId, origin: &csg_graph::Key) -> bool {
        let Some(holes) = self.scaled_holes.get(origin) else { return false };
        let mut pending = vec![input];
        let mut seen = HashSet::new();
        let mut drilled = false;
        while let Some(node) = pending.pop() {
            if !seen.insert(node) {
                continue;
            }
            let step = self.graph.node(node);
            if &step.key == origin {
                return false;
            }
            drilled |= holes.contains(&node);
            pending.extend(step.op.inputs());
        }
        drilled
    }

    /// Resolve a segment on the generator whose sketch contains it. The source spelling is
    /// needed only for a diagnostic; identity decides which side the expression denotes.
    fn segment_side(&self, f: &Fields, face: &Obj, owner: usize, segment: &Obj) -> Res<Role> {
        let indices = |path: &usize| self.path_segments.get(path).and_then(|ids| ids.get(&segment.ident));
        let mut matches = Vec::new();
        for (path, offset) in self.profile_paths.get(&owner).into_iter().flatten() {
            if let Some(sides) = indices(path) {
                matches.extend(sides.iter().map(|&i| (*offset, i)));
            }
        }
        if let [(Some(0), i)] = matches.as_slice() {
            return Ok(Role::Side(*i as u32));
        }
        let syn = &self.names.syntax;
        let arg = face.loc.and_then(|loc| self.names.call_site(loc)).and_then(|site| edit::loc_at(syn, &site.span))
            .and_then(|call| syn.node(&call.child(1))).map(|n| &syn.source()[n.span.clone()]).unwrap_or("segment");
        let path = arg.split_once('.').map_or("its Path", |(p, _)| p);
        match matches.as_slice() {
            [] => {
                let generator = self.solids.iter().enumerate().find(|(i, _)| {
                    self.profile_paths.get(i).is_some_and(|paths| paths.iter().any(|(p, offset)| *offset == Some(0) && indices(p).is_some()))
                });
                let (op, id) = generator.map_or(("extrude", "body"), |(_, s)| (if s.proto == "BrepRevolve" { "revolve" } else { "extrude" }, s.id.as_str()));
                f.err(format!("side({arg}) names a side of the {op} that drew {path}: write it on that part, e.g. {id}.side({arg})"))
            }
            [_] => f.err(format!("side({arg}): put {path} first in its sketch, or name the side by number")),
            _ => {
                let Some(sides) = matches.iter().map(|(off, i)| off.map(|o| (o + i).to_string())).collect::<Option<Vec<_>>>() else {
                    return f.err(format!("{arg} occurs more than once in this profile: name the side by number"));
                };
                let (last, others) = sides.split_last().unwrap();
                f.err(format!("{arg} makes sides {} and {last}: name the side by number, e.g. {}.side({})", others.join(", "), self.solids[owner].id, sides[0]))
            }
        }
    }

    /// A script face takes its role and the key of its solid's own exact step, through tags.
    fn face_name(&mut self, f: &Fields, key: &str, v: &V, input: NodeId) -> Res<FaceName> {
        let o = match v {
            V::Obj(o) if o.proto.as_deref() == Some("Face") => o.clone(),
            other => return f.err(format!("'{key}' takes faces of exact parts (b.top, r.face(m.edge(b.top, b.front))), not {}", other.describe())),
        };
        let index = self.solid(o.get("of").unwrap_or(&V::Nil))?;
        if self.lowering.contains(&index) {
            return f.err(format!("'{key}': {} cannot name its own faces", self.solids[index].id));
        }
        let segment_side = o.get("seg").and_then(V::obj).map(|seg| self.segment_side(f, &o, index, seg)).transpose()?;
        let mut node = self.solids[index].node;
        loop {
            match &self.graph.node(node).op {
                Op::Tag { source, .. } => node = *source,
                Op::Brep { .. } => break,
                _ => return f.err(format!("'{key}': {} is not an exact part; faces by name are on m.brep parts", self.solids[index].id)),
            }
        }
        let origin = self.graph.node(node).key;
        if self.lost_to_scaled_hole(input, &origin) {
            return f.err("a hole on a scaled placement takes the body's own face names with it; select the face before the hole, or place without scale");
        }
        let role = match o.get("role") {
            Some(V::Id(r)) => r.clone(),
            _ => return f.err(format!("'{key}': a face without its role")),
        };
        let role = match role.as_str() {
            "side" => match (segment_side, o.get("index")) {
                (Some(role), _) => role,
                (_, Some(V::Num(i))) => Role::Side(*i as u32),
                _ => return f.err(format!("'{key}': a side is e.side(2) or e.side(section.base)")),
            },
            "blend" => {
                let a = self.face_name(f, key, o.get("a").unwrap_or(&V::Nil), input)?;
                let b = self.face_name(f, key, o.get("b").unwrap_or(&V::Nil), input)?;
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                Role::Blend(Box::new(a), Box::new(b))
            }
            "corner" => {
                let Some(V::Arr(list)) = o.get("round") else { return f.err(format!("'{key}': a corner is r.corner(a, b, c)")) };
                let mut round = list.iter().map(|v| self.face_name(f, key, v, input)).collect::<Res<Vec<_>>>()?;
                round.sort();
                round.dedup();
                Role::Corner(round)
            }
            "inner" => Role::Inner(match o.get("outer") {
                Some(v) => Some(Box::new(self.face_name(f, key, v, input)?)),
                None => None,
            }),
            r => match FaceRole::from_name(r) {
                Some(face) => Role::Named(face),
                None => return f.err(format!("'{key}': no face '{r}'")),
            },
        };
        Ok(FaceName { origin, role })
    }

    fn edges(&mut self, f: &Fields, input: NodeId) -> Res<EdgeSel> {
        let unit = self.unit;
        let one = |v: &V| -> Res<EdgeFilter> {
            let V::Obj(o) = v else { return f.err(format!("an edge selector is @all or {{top: true ...}}, not {}", v.describe())) };
            let mut e = EdgeFilter::default();
            for (k, x) in &o.fields {
                let b = |x: &V| -> Res<bool> {
                    match x {
                        V::Bool(b) => Ok(*b),
                        _ => f.err(format!("edges '{k}' is true or false")),
                    }
                };
                match k.as_str() {
                    "top" => e.top = b(x)?,
                    "bottom" => e.bottom = b(x)?,
                    "convex" => e.convex = b(x)?,
                    "concave" => e.concave = b(x)?,
                    "y" => {
                        let a = f.nums("edges.y", x, 2)?;
                        e.y = Some((a[0] * unit, a[1] * unit));
                    }
                    "within" => match x {
                        V::Arr(b) if b.len() == 2 => e.within = Some((f.p3_of("edges.within", &b[0])?, f.p3_of("edges.within", &b[1])?)),
                        _ => return f.err("edges 'within' is [[x y z] [x y z]]"),
                    },
                    "dir" => {
                        e.dir = Some(match x {
                            V::Id(n) | V::Str(n) => {
                                let d = makepad_csg_cad::edges::named_dir(n).ok_or(()).or_else(|_| f.err(format!("edges 'dir' @{n}: use @x, @y, @z (or [x y z])")))?;
                                [d.x, d.y, d.z]
                            }
                            other => {
                                let a = f.nums("edges.dir", other, 3)?;
                                [a[0], a[1], a[2]]
                            }
                        })
                    }
                    "kind" => {
                        e.kind = Some(match x {
                            V::Id(n) if n == "line" => EdgeKind::Line,
                            V::Id(n) if n == "circle" => EdgeKind::Circle,
                            V::Id(n) if n == "curve" => EdgeKind::Curve,
                            _ => return f.err("edges 'kind' is @line, @circle or @curve"),
                        })
                    }
                    "min_length" => e.min_length = Some(f.number("edges.min_length", x)? * unit),
                    "near" => e.near = Some(f.p3_of("edges.near", x)?),
                    other => return f.err(format!("edges has no condition '{other}' (top, bottom, y, within, dir, kind, convex, concave, min_length, near)")),
                }
            }
            Ok(e)
        };
        let items: Vec<V> = match f.get("edges") {
            None => return Ok(EdgeSel::all()),
            Some(V::Id(a)) if a == "all" => return Ok(EdgeSel::all()),
            Some(V::Arr(list)) => list.clone(),
            Some(v) => vec![v.clone()],
        };
        let mut sel = EdgeSel::all();
        for v in &items {
            match v {
                // `m.edge(a, b, {near chain})`: an exact edge by its two faces.
                V::Obj(o) if o.proto.as_deref() == Some("Edge") => {
                    let a = self.face_name(f, "edges", o.get("a").unwrap_or(&V::Nil), input)?;
                    let b = self.face_name(f, "edges", o.get("b").unwrap_or(&V::Nil), input)?;
                    let near = match o.get("near") {
                        Some(p) => Some(f.p3_of("m.edge near", p)?),
                        None => None,
                    };
                    let chain = match o.get("chain") {
                        None => false,
                        Some(V::Bool(c)) => *c,
                        Some(_) => return f.err("m.edge 'chain' is true or false"),
                    };
                    sel.named.push(EdgeName { a, b, near, chain });
                    self.selector_at(o.loc);
                }
                v => sel.any.push(one(v)?),
            }
        }
        Ok(sel)
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod voronoi_tests;

/// A cage's points (in model units), faces and levels, checked: a cage that does not close
/// round a solid is an error at the call.
fn cage_of(f: &Fields) -> Res<(Vec<P3>, Vec<Vec<u32>>, Option<u32>)> {
    let V::Arr(list) = f.need("points")? else { return f.err("'points' is a list of [x y z]") };
    let points = list.iter().map(|p| f.p3_of("points", p)).collect::<Res<Vec<_>>>()?;
    let V::Arr(list) = f.need("faces")? else { return f.err("'faces' is a list of faces, each [i j k l]: its points by index") };
    let index = |v: &V| match v {
        V::Num(n) if *n >= 0.0 && n.fract() == 0.0 && *n <= u32::MAX as f64 => Ok(*n as u32),
        other => f.err(format!("'faces': a face names its points by index (0, 1, 2 ...), not {}", other.describe())),
    };
    let faces = list
        .iter()
        .map(|face| match face {
            V::Arr(ix) => ix.iter().map(index).collect::<Res<Vec<u32>>>(),
            other => f.err(format!("'faces' is a list of faces, each [i j k l], not {}", other.describe())),
        })
        .collect::<Res<Vec<_>>>()?;
    // csg_mesh::subdiv::catmull_clark enforces this kernel bound independently
    // of the editor slider; the other numeric slider ranges are hints only.
    let levels = match f.get("levels") {
        None => None,
        Some(_) => match f.count("levels", 0)? {
            n @ 1..=makepad_csg_graph::subdiv::MAX_LEVELS => Some(n),
            n => return f.err(format!("'levels' is 1 to {}, not {n}", makepad_csg_graph::subdiv::MAX_LEVELS)),
        },
    };
    let at: Vec<makepad_csg_math::Vec3d> = points.iter().map(|p| makepad_csg_math::Vec3d::new(p[0], p[1], p[2])).collect();
    makepad_csg_graph::subdiv::check(&at, &faces).or_else(|e| f.err(e))?;
    Ok((points, faces, levels))
}

/// A weave's link half sizes: one length, or a list spread evenly from one end to the other (along a chain's
/// path; across a mail's region). Empty: the smallest link that keeps the clearances.
fn weave_sizes(f: &Fields, key: &str) -> Res<Vec<f64>> {
    match f.get(key) {
        None => Ok(Vec::new()),
        Some(V::Arr(list)) if !list.is_empty() => list.iter().map(|v| Ok(f.number(key, v)? * f.unit)).collect(),
        Some(v) => Ok(vec![f.number(key, v)? * f.unit]),
    }
}

/// A size profile at `t` (0 at the start, 1 at the end), piecewise linear; `base` when there is none.
fn profile_at(sizes: &[f64], base: f64, t: f64) -> f64 {
    match sizes.len() {
        0 => base,
        1 => sizes[0],
        n => {
            let x = t.clamp(0.0, 1.0) * (n - 1) as f64;
            let k = (x.floor() as usize).min(n - 2);
            sizes[k] + (x - k as f64) * (sizes[k + 1] - sizes[k])
        }
    }
}

fn dot3(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross3(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// `up` less its part along unit `axis`, as a unit vector (some direction across the axis
/// when they are parallel).
fn across(axis: P3, up: P3) -> P3 {
    let d = dot3(axis, up);
    let a: P3 = std::array::from_fn(|k| up[k] - d * axis[k]);
    let l = dot3(a, a).sqrt();
    if l > 1e-9 {
        return a.map(|v| v / l);
    }
    let other = if axis[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
    let c = cross3(axis, other);
    let l = dot3(c, c).sqrt();
    c.map(|v| v / l)
}

/// A frame (its local X, Y, Z axes in the model) turned 45 degrees about its own axis `k`.
fn turned(frame: [P3; 3], k: usize) -> [P3; 3] {
    let (a, b) = ((k + 1) % 3, (k + 2) % 3);
    let (s, c) = (std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2);
    let mut out = frame;
    out[a] = std::array::from_fn(|j| c * frame[a][j] + s * frame[b][j]);
    out[b] = std::array::from_fn(|j| -s * frame[a][j] + c * frame[b][j]);
    out
}

/// The rotation (degrees about X, then Y, then Z, as a transform node takes it) that turns
/// direction `from` onto `to` the shortest way, then `spin` radians about `to`. None when either
/// is no direction.
fn turn_onto(from: P3, to: P3, spin: f64) -> Option<P3> {
    let unit = |v: P3| {
        let l = dot3(v, v).sqrt();
        (l > 1e-12).then(|| v.map(|x| x / l))
    };
    let (f, t) = (unit(from)?, unit(to)?);
    // turning `sin`, `cos` about unit `k` (Rodrigues)
    let about = |k: P3, sin: f64, cos: f64| -> [[f64; 3]; 3] {
        let skew = [[0.0, -k[2], k[1]], [k[2], 0.0, -k[0]], [-k[1], k[0], 0.0]];
        std::array::from_fn(|i| std::array::from_fn(|j| (if i == j { cos } else { 0.0 }) + k[i] * k[j] * (1.0 - cos) + skew[i][j] * sin))
    };
    let axis = cross3(f, t);
    let sin = dot3(axis, axis).sqrt();
    let onto = match unit(axis) {
        Some(k) if sin > 1e-9 => about(k, sin, dot3(f, t)),
        // parallel: none, or half a turn about any axis across it
        _ if dot3(f, t) > 0.0 => about(t, 0.0, 1.0),
        _ => about(across(f, [0.0, 1.0, 0.0]), 0.0, -1.0),
    };
    let spun = about(t, spin.sin(), spin.cos());
    let r: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| spun[i][k] * onto[k][j]).sum()));
    Some(euler_xyz(std::array::from_fn(|j| std::array::from_fn(|i| r[i][j]))))
}

/// The rotation (degrees about X, then Y, then Z, as a transform node takes it) that turns the
/// model's axes onto `frame`'s (its local X, Y, Z in the model).
fn euler_xyz(frame: [P3; 3]) -> P3 {
    // rows of R = Rz Ry Rx: R[i][j] = frame[j][i]
    let r = |i: usize, j: usize| frame[j][i];
    let sy = (-r(2, 0)).clamp(-1.0, 1.0);
    let y = sy.asin();
    let (x, z) = if sy.abs() < 1.0 - 1e-9 { (r(2, 1).atan2(r(2, 2)), r(1, 0).atan2(r(0, 0))) } else { (0.0, (-r(0, 1)).atan2(r(1, 1))) };
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}
