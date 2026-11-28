//! Circuit documents (C5): a `.ckt.splash` runs in its own VM with `mod.elec` and the libraries,
//! its builders record what they make where they were called, and the records lower into a
//! `Circuit` and the unit's source-map entries once the VM is done (handles, never VM values,
//! cross into the model; ELANG gate 3 Q1).
//!
//! Naming happens at the call: an item's path is its block instance's path, then the `let`s round
//! the call in each frame of the running thread with the iteration positions of the loops round it
//! (`mcu.dec[2]`, architect change 1), else its builder and a count (`r.1`).
//!
//! Cost: one VM per circuit document per evaluation; a builder call is a call-site query, a hash
//! lookup in the document's names and a few records. Nothing here runs per frame.

mod natives;
mod names;
pub(crate) mod read;
mod hints;

pub(crate) use natives::net_handle;

use crate::map::{BodySite, PinSite, SourceRange, ValueSite};
use crate::source::{located, LIB_DOC};
use fab_elec_circuit::{
    Block, BlockDef, BlockSource, Bus, Circuit, ClassRules, Component, Intent, Net, NetClass, NetKind, ParamValue,
    RefKind, SheetHints, Target, Value,
};
use fab_elec_core::{
    BlockId, BoardId, BusId, ClassId, CompId, CompRef, Diag, DocId, NetId, NetRef, PinIx, Sel,
};
use fab_elec_parts::{Fields, Footprint, Gfx, Pad, PartDef, PinKind, Symbol, VRange};
use fab_elec_core::LayerId;
use makepad_script::*;
use names::Names;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Instructions one circuit document may run: a guard against a loop that never ends (the
/// fixture runs well under 1% of it).
pub(crate) const EVAL_LIMIT: u64 = 500_000_000;
const MAX_FRAMES: usize = 64;

/// One document the VM runs, by the file name its code carries.
pub(crate) struct DocInfo {
    pub id: DocId,
    pub file: String,
    pub lib: bool,
    text: Arc<str>,
    /// Its syntax and creation sites, read when a frame or a site first needs them: a run reaches
    /// a few of the library documents, never all of them.
    names: std::cell::OnceCell<Result<Names, syntax::SyntaxError>>,
}

impl DocInfo {
    fn new(d: &crate::source::Doc, file: String) -> DocInfo {
        DocInfo { id: d.id, file, lib: d.lib.is_some(), text: d.text.clone(), names: std::cell::OnceCell::new() }
    }

    fn parsed(&self) -> &Result<Names, syntax::SyntaxError> {
        self.names.get_or_init(|| names::parse_with(&self.text, &crate::units::circuit_units()))
    }

    /// The document's syntax and creation sites; `None` when it does not parse.
    pub fn names(&self) -> Option<&Names> {
        self.parsed().as_ref().ok()
    }
}

/// Where a builder was called: the document range of the call and the item's own path.
#[derive(Clone, Debug)]
pub(crate) struct Here {
    pub doc: DocId,
    pub span: Range<u32>,
    pub callee: Option<String>,
    pub path: Option<String>,
    /// Loop positions inside the call's own function, outermost first.
    pub loops: Vec<u32>,
}

pub(crate) struct PartRec {
    pub def: Arc<PartDef>,
    pub prefix: String,
    /// The defining call, for the catalog's statement comments and library ownership.
    pub here: Here,
}

/// An `e.body(part, spec)` override: applied to the part now, or when its name is defined.
pub(crate) struct BodyOverride {
    pub name: String,
    pub body: fab_elec_parts::Body3d,
    /// Whether the override states the height and the ports; what it leaves stays the part's own.
    pub height: bool,
    pub ports: bool,
    /// Its mesh is not in the project: the part keeps its own body, marked with the mesh.
    pub missing: bool,
    pub here: Here,
    pub applied: bool,
    /// The part's own body, which the override replaced (set when it is applied).
    pub own: Option<fab_elec_parts::Body3d>,
}

pub(crate) struct SymRec {
    pub symbol: Symbol,
    pub pins: Vec<(String, PinKind)>,
}

pub(crate) struct CompRec {
    pub path: String,
    pub part: usize,
    pub value: Option<Value>,
    /// The reference written on this call. An omitted reference is numbered during lowering.
    pub refdes: Option<String>,
    pub dnp: bool,
    pub added: Option<u32>,
    pub conns: Vec<(PinIx, usize)>,
    pub nc: Vec<PinIx>,
    pub inst: usize,
    pub fields: Fields,
    pub here: Here,
    /// Where the value is written (counts are filled at lowering).
    pub site: Option<ValueSite>,
    /// Where each pin's connections are written (counts are filled at lowering).
    pub pin_sites: Vec<(PinIx, PinSite)>,
}

/// Find a maker's written options, positional or named. Only its argument list is examined.
pub(crate) fn comp_options_arg(args: &[syntax::Node], position: usize) -> Option<(usize, &syntax::Node)> {
    let key = if position == 4 { "rest" } else { "opts" };
    args.iter().enumerate().rev().find_map(|(i, arg)| match &arg.kind {
        syntax::Kind::Entry { name, value, .. } => (name == key).then_some((i, value.as_ref())),
        _ => (i == position).then_some((i, arg)),
    })
}

/// Whether text can be stored verbatim as a component reference.
pub(crate) fn valid_ref(text: &str) -> bool {
    !text.is_empty() && !text.contains(char::is_whitespace)
}

pub(crate) struct NetRec {
    pub name: String,
    /// Named by the document (`e.net({name: ..})`, a bus), not after a pin.
    pub explicit: bool,
    pub kind: NetKind,
    /// The instance whose path the derived name sits under; None for a document-given name.
    pub scope: Option<usize>,
    pub volts: Option<VRange>,
    pub class: Option<String>,
    pub here: Here,
}

pub(crate) struct BusRec {
    pub name: String,
    pub members: Vec<(String, usize)>,
}

pub(crate) struct DefRec {
    pub def: BlockDef,
    pub build: Option<ScriptValue>,
    pub params: Option<ScriptObject>,
    /// The `e.block` call.
    pub here: Here,
}

pub(crate) struct InstRec {
    pub path: String,
    /// The definition's circuit id (`Build::used`).
    pub def: Option<usize>,
    /// The definition's record (`Ctx::defs`).
    pub rec: Option<usize>,
    pub parent: Option<usize>,
    pub ports: Vec<(String, Vec<usize>)>,
    pub params: Vec<(String, ParamValue)>,
    pub here: Option<Here>,
}

/// An intent with record indices (nets resolve through merges at lowering).
pub(crate) enum IntentRec {
    Near { a: Tgt, b: Tgt, max: i32 },
    Group { name: String, members: Vec<Tgt> },
    Side { t: Tgt, side: fab_elec_core::Side },
    Fixed { t: Tgt, edge: Option<fab_elec_circuit::Edge>, at: Option<fab_elec_core::P>, rot: Option<fab_elec_core::Angle> },
    Width { net: usize, w: i32 },
    Pair { p: usize, n: usize, gap: Option<i32>, z0: Option<f64>, skew: Option<i32>, here: Here },
    LengthMatch { nets: Vec<usize>, tol: i32, here: Here },
}

#[derive(Copy, Clone)]
pub(crate) enum Tgt {
    Comp(usize),
    Pin(usize, PinIx),
    Inst(usize),
}

/// A document's session: libraries and definitions shared by its circuit builds.
#[derive(Default)]
pub(crate) struct Ctx {
    pub docs: Vec<DocInfo>,
    pub assets: Vec<String>,
    pub diags: Vec<Diag>,
    /// Located warnings the natives raise (a mesh the project does not hold yet); a unit's
    /// lowering takes them into its notes.
    pub warnings: Vec<Diag>,
    pub parts: Vec<PartRec>,
    pub part_names: HashMap<String, usize>,
    /// Body overrides, kept in the session: a bench's own circuit builds use them too.
    pub bodies: Vec<BodyOverride>,
    pub pads: Vec<Pad>,
    pub fps: Vec<Footprint>,
    pub gfx: Vec<(LayerId, Gfx)>,
    pub syms: Vec<SymRec>,
    pub defs: Vec<DefRec>,
    pub ports: Vec<fab_elec_circuit::PortDef>,
    pub keep: Vec<ScriptObjectRef>,
    pub holder: Option<ScriptObject>,
    /// The document exporting each module's catalog, in library load order; "" is the project.
    pub catalogs: Vec<(String, DocId)>,
    /// Value expressions resolved once per call site and loop position: the instances of one
    /// block share them (`None`: no expression node at that range).
    pub resolved: RefCell<HashMap<(DocId, Range<u32>, Vec<u32>), Option<(crate::resolve::Term, Vec<crate::resolve::Leaf>)>>>,
    /// The circuit being built; a bench's circuit function runs against a build of its own.
    pub build: Build,
    /// Zero belongs to the document; circuit functions take successive nonzero serials.
    pub next_build_serial: u64,
}

/// One circuit's records: what the natives make while a circuit document (or a bench's
/// circuit function) runs, and what `lower` turns into a `Circuit`. The session in `Ctx` outlives
/// it; nothing here may refer to another build.
#[derive(Default)]
pub(crate) struct Build {
    /// Identifies the records a build-scoped VM handle indexes.
    pub serial: u64,
    pub comps: Vec<CompRec>,
    pub nets: Vec<NetRec>,
    /// Union-find over `nets`: a merged net points at the net it joined.
    pub parent: Vec<usize>,
    /// Successful joins in call order, with the record indices given to `union`.
    pub joins: Vec<(usize, usize, Here)>,
    pub buses: Vec<BusRec>,
    pub classes: Vec<(String, ClassRules)>,
    /// Block instances; `insts[0]` is the root.
    pub insts: Vec<InstRec>,
    /// The instances whose build fns are running, innermost last.
    pub stack: Vec<usize>,
    pub intents: Vec<IntentRec>,
    /// Call sites of buses and net classes, whose records carry none (an intent names records
    /// that do).
    pub other_sites: Vec<Here>,
    pub hints: SheetHints,
    pub hint_keys: Vec<(String, Here)>,
    pub sheet_seen: bool,
    /// Unnamed items per (instance, builder): `r.1`, `r.2`.
    pub unnamed: HashMap<(usize, String), u32>,
    /// The block definitions this circuit instantiates, by index into `Ctx::defs`, in the order
    /// of their first instance: a definition's id in this circuit is its position.
    pub used: Vec<usize>,
}

pub(crate) type Shared = Rc<RefCell<Ctx>>;

impl Build {
    pub fn root() -> Build {
        Build {
            insts: vec![InstRec { path: String::new(), def: None, rec: None, parent: None, ports: vec![], params: vec![], here: None }],
            ..Build::default()
        }
    }

    pub fn inst(&self) -> usize {
        self.stack.last().copied().unwrap_or(0)
    }

    pub fn find(&self, mut n: usize) -> usize {
        while self.parent[n] != n {
            n = self.parent[n];
        }
        n
    }

    /// Join two nets; the earlier one stays, with a document name over a pin's.
    pub fn union(&mut self, a: usize, b: usize, here: &Here) -> usize {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return ra;
        }
        self.joins.push((a, b, here.clone()));
        let (keep, gone) = if ra < rb { (ra, rb) } else { (rb, ra) };
        if !self.nets[keep].explicit && self.nets[gone].explicit {
            self.nets[keep].name = self.nets[gone].name.clone();
            self.nets[keep].scope = self.nets[gone].scope;
            self.nets[keep].explicit = true;
        }
        if self.nets[keep].volts.is_none() {
            self.nets[keep].volts = self.nets[gone].volts;
        }
        if self.nets[keep].kind == NetKind::Signal {
            self.nets[keep].kind = self.nets[gone].kind;
        }
        if self.nets[keep].class.is_none() {
            self.nets[keep].class = self.nets[gone].class.clone();
        }
        self.parent[gone] = keep;
        keep
    }

    /// Replay only for a collapsed pair; the first join connecting its halves is responsible.
    fn joining_call(&self, p: usize, n: usize) -> &Here {
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut parent: Vec<usize> = (0..self.nets.len()).collect();
        let mut size = vec![1usize; self.nets.len()];
        for (a, b, here) in &self.joins {
            let (mut a, mut b) = (find(&mut parent, *a), find(&mut parent, *b));
            if size[a] < size[b] {
                std::mem::swap(&mut a, &mut b);
            }
            parent[b] = a;
            size[a] += size[b];
            if find(&mut parent, p) == find(&mut parent, n) {
                return here;
            }
        }
        unreachable!("merged pair nets have a recorded joining call")
    }

    pub fn new_net(&mut self, name: String, explicit: bool, scope: Option<usize>, here: Here) -> usize {
        let n = self.nets.len();
        self.nets.push(NetRec { name, explicit, kind: NetKind::Signal, scope, volts: None, class: None, here });
        self.parent.push(n);
        n
    }

    /// The net pin `pin` of component `comp` is on.
    pub fn pin_net(&self, comp: usize, pin: PinIx) -> Option<usize> {
        self.comps[comp].conns.iter().find(|(p, _)| *p == pin).map(|(_, n)| self.find(*n))
    }

    /// Put a pin on a net (joining nets when the pin is on one already).
    pub fn attach(&mut self, comp: usize, pin: PinIx, net: usize, here: &Here) {
        match self.pin_net(comp, pin) {
            Some(old) => {
                self.union(old, net, here);
            }
            None => self.comps[comp].conns.push((pin, net)),
        }
    }
}

impl Ctx {
    /// A pin's net, made and named after the pin (`usb.j.CC1`) when it is on none yet.
    pub fn net_of_pin(&mut self, comp: usize, pin: PinIx) -> usize {
        if let Some(n) = self.build.pin_net(comp, pin) {
            return n;
        }
        let c = &self.build.comps[comp];
        let name = format!("{}.{}", c.path, self.parts[c.part].def.pins[pin.ix()].name);
        let here = c.here.clone();
        let n = self.build.new_net(name, false, Some(c.inst), here);
        self.build.comps[comp].conns.push((pin, n));
        n
    }

    pub fn doc_by_file(&self, file: &str) -> Option<usize> {
        self.docs.iter().position(|d| d.file == file)
    }
}

/// A unit's lowering: its circuit and where everything in it is written.
pub(crate) struct UnitOut {
    pub circuit: Circuit,
    pub notes: Vec<Diag>,
    pub map: Vec<(Sel, SourceRange)>,
    pub added: Vec<(CompId, u32)>,
    pub values: Vec<(CompRef, ValueSite)>,
    pub pins: Vec<((CompRef, PinIx), Vec<PinSite>)>,
    /// The `e.body` overrides applied to this unit's parts, by part name.
    pub bodies: Vec<(String, BodySite)>,
    pub phases: Phases,
}

/// Where one unit's build time went.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Phases {
    /// Reading the circuit document's syntax tree (a library document is read within `libs` or
    /// `run`, when its sites are first asked for).
    pub read: std::time::Duration,
    /// The VM running the library and project part/block documents.
    pub libs: std::time::Duration,
    /// The VM running the circuit document (its natives record parts, nets and instances).
    pub run: std::time::Duration,
    /// Records to the `Circuit`, the map and the sites.
    pub lower: std::time::Duration,
}

/// Run one circuit document with the libraries and lower what it made.
pub(crate) fn run_unit(unit: BoardId, name: &str, doc: &crate::source::Doc, sources: &crate::source::Sources, libs: &crate::source::Libs) -> Result<UnitOut, Vec<Diag>> {
    let mut host = Box::new(ScriptVmHost::new((), ()));
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    let mut phases = Phases::default();
    let mut clock = std::time::Instant::now();
    let mut lap = move || {
        let now = std::time::Instant::now();
        let d = now - clock;
        clock = now;
        d
    };
    let ctx = prepare_vm(&mut vm, doc, libs)?;
    phases.read = lap();
    load_libraries(&mut vm, &ctx);
    load_project(&mut vm, &ctx, sources);
    phases.libs = lap();
    let file = ctx
        .borrow()
        .docs
        .last()
        .map(|d| d.file.clone())
        .unwrap_or_default();
    vm.with_instruction_limit(EVAL_LIMIT, |vm| {
        vm.eval(ScriptMod::new("", "", &file, 1, 0, &doc.text, &[]))
    });
    phases.run = lap();
    let vm_errors = vm.take_errors();
    drop(vm);
    drop(host);
    let mut ctx = Rc::try_unwrap(ctx)
        .ok()
        .expect("the VM is gone, its natives with it")
        .into_inner();
    let mut diags = std::mem::take(&mut ctx.diags);
    for e in vm_errors {
        diags.push(vm_diag(&ctx, &e));
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    let build = std::mem::take(&mut ctx.build);
    let root = SourceRange { doc: doc.id, span: 0..doc.text.len() as u32 };
    let mut out = lower(unit, name, root, &ctx, build)?;
    phases.lower = lap();
    out.phases = phases;
    Ok(out)
}

/// Shared circuit/bench VM setup. Only libraries run here; a bench never evaluates a unit.
pub(crate) fn prepare_vm(
    vm: &mut ScriptVm,
    doc: &crate::source::Doc,
    libs: &crate::source::Libs,
) -> Result<Shared, Vec<Diag>> {
    vm.set_unit_table(&crate::units::circuit_units());
    let ctx: Shared = Rc::new(RefCell::new(Ctx { build: Build::root(), next_build_serial: 1, ..Ctx::default() }));
    // Library documents first, by library then name; then the circuit.
    let mut lib_docs: Vec<&crate::source::Doc> = libs.docs().iter().collect();
    // A library's `<lib>.part.splash` runs before its other documents.
    let key = |d: &crate::source::Doc| {
        let first = d.lib.as_deref().is_some_and(|l| d.name == format!("{l}.part.splash"));
        (d.lib.clone(), !first, d.name.clone())
    };
    lib_docs.sort_by_key(|d| key(d));
    for d in lib_docs.iter().copied().chain(std::iter::once(doc)) {
        let file = match &d.lib {
            Some(l) => format!("{l}/{}", d.name),
            None => d.name.clone(),
        };
        ctx.borrow_mut().docs.push(DocInfo::new(d, file));
    }
    // The circuit is read before it runs (a syntax error stops here, located); a library is read
    // when its sites are first asked for, or its error is reported.
    if let Some(e) = ctx.borrow().docs.last().and_then(|d| d.parsed().as_ref().err()) {
        return Err(vec![parse_diag(doc, e)]);
    }
    natives::install(vm, &ctx);
    vm.capture_errors();
    Ok(ctx)
}

pub(crate) fn load_libraries(vm: &mut ScriptVm, ctx: &Shared) {
    let mut by_lib: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, d) in ctx.borrow().docs.iter().enumerate().filter(|(_, d)| d.lib) {
        let lib = d.file.split('/').next().unwrap_or("").to_string();
        match by_lib.iter_mut().find(|(l, _)| *l == lib) {
            Some((_, v)) => v.push(i),
            None => by_lib.push((lib, vec![i])),
        }
    }
    for (lib, docs) in by_lib {
        let module = vm.new_module(read::lut_id(&lib));
        for i in docs {
            let (file, text) = {
                let c = ctx.borrow();
                (c.docs[i].file.clone(), c.docs[i].text.clone())
            };
            // What the document's top level binds, less its private (`__`) names, goes in the
            // library's module.
            let (_, body) = vm.with_instruction_limit(EVAL_LIMIT, |vm| vm.eval_module(ScriptMod::new("", "", &file, 1, 0, &text, &[]), &[]));
            if let Some(exports) = vm.scope_exports(body, &[]) {
                for entry in vm.bx.heap_ref().map_entries(exports) {
                    if entry.key == read::lut_id("catalog").into() {
                        let mut c = ctx.borrow_mut();
                        let doc = c.docs[i].id;
                        if c.catalogs.iter().any(|(name, _)| name == &lib) {
                            c.diags.push(located(doc, 0..text.len() as u32, format!("catalog: library {lib} already has a catalog")));
                            continue;
                        }
                        c.catalogs.push((lib.clone(), doc));
                    }
                    vm.bx.heap_mut().set_value_def(module, entry.key, entry.value);
                }
            }
        }
    }
}

/// Run project parts, then blocks exported through `mod.project`, after the libraries.
pub(crate) fn load_project(vm: &mut ScriptVm, ctx: &Shared, sources: &crate::source::Sources) {
    load_project_docs(vm, ctx, sources.docs().into_iter(), sources.mesh_assets());
}

/// Load a definition snapshot without recreating or renumbering its project documents.
pub(crate) fn load_project_docs<'a>(
    vm: &mut ScriptVm, ctx: &Shared, docs: impl Iterator<Item = &'a crate::source::Doc>, assets: &[String],
) {
    let (mut parts, mut blocks) = (Vec::new(), Vec::new());
    for doc in docs {
        match doc.kind {
            crate::source::DocKind::Part => parts.push(doc),
            crate::source::DocKind::Blocks => blocks.push(doc),
            _ => {}
        }
    }
    parts.sort_by(|a, b| a.name.cmp(&b.name));
    blocks.sort_by(|a, b| a.name.cmp(&b.name));
    {
        let mut c = ctx.borrow_mut();
        // A part's mesh names one of the project's assets or the part library's models, in either VM.
        c.assets = assets.to_vec();
        let at = c.docs.len() - 1;
        c.docs.splice(at..at, parts.iter().chain(&blocks).map(|d| DocInfo::new(d, d.name.clone())));
    }
    for part in parts {
        vm.with_instruction_limit(EVAL_LIMIT, |vm| {
            vm.eval_module(ScriptMod::new("", "", &part.name, 1, 0, &part.text, &[]), &[])
        });
    }
    if blocks.is_empty() {
        return;
    }
    let module = vm.new_module(read::lut_id("project"));
    for block in blocks {
        let (_, body) = vm.with_instruction_limit(EVAL_LIMIT, |vm| {
            vm.eval_module(ScriptMod::new("", "", &block.name, 1, 0, &block.text, &[]), &[])
        });
        if let Some(exports) = vm.scope_exports(body, &[]) {
            for entry in vm.bx.heap_ref().map_entries(exports) {
                if entry.key == read::lut_id("catalog").into() {
                    let mut c = ctx.borrow_mut();
                    c.catalogs.retain(|(lib, _)| !lib.is_empty());
                    c.catalogs.push((String::new(), block.id));
                }
                vm.bx.heap_mut().set_value_def(module, entry.key, entry.value);
            }
        }
    }
}

/// A document's syntax error, at its byte.
fn parse_diag(d: &crate::source::Doc, e: &syntax::SyntaxError) -> Diag {
    let at = (e.at.min(d.text.len())) as u32;
    located(d.id, at..at, format!("{}: {}", d.name, e.message))
}

/// A VM error (`file:line:col: message`) as a located diagnostic.
pub(crate) fn vm_diag(ctx: &Ctx, error: &str) -> Diag {
    let strip = |m: &str| match m.rfind(" (platform/script/") {
        Some(at) if m.ends_with(')') => m[..at].to_string(),
        _ => m.to_string(),
    };
    let mut parts = error.splitn(4, ':');
    let (file, line, col, rest) = (parts.next(), parts.next(), parts.next(), parts.next());
    let parsed = (file, line.and_then(|l| l.trim().parse::<u32>().ok()), col.and_then(|c| c.trim().parse::<u32>().ok()), rest);
    if let (Some(file), Some(line), Some(col), Some(rest)) = parsed {
        if let Some(d) = ctx.doc_by_file(file.trim()).map(|i| &ctx.docs[i]) {
            match d.parsed() {
                Ok(names) => {
                    let loc = (line.max(1), col + 1);
                    let span = names.node_span(loc).or_else(|| names.offset(loc).map(|o| o..o + 1)).unwrap_or(0..0);
                    return located(d.id, span.start as u32..span.end as u32, format!("{}:{line}:{}: {}", d.file, col + 1, strip(rest.trim())));
                }
                // A library that does not parse: its syntax error, at its byte.
                Err(e) => {
                    let at = (e.at.min(d.text.len())) as u32;
                    return located(d.id, at..at, format!("{}: {}", d.file, e.message));
                }
            }
        }
    }
    Diag::error(strip(error), vec![])
}

/// Whether Annotate can write an automatic reference at a counted component call. O(1).
pub(crate) fn automatic_ref_kind(at: &SourceRange, comps_per_call: &HashMap<SourceRange, usize>) -> RefKind {
    if at.doc.0 & LIB_DOC == 0 && comps_per_call[at] == 1 { RefKind::Auto } else { RefKind::Shared }
}

/// Advance a numeric suffix. Stored references are text, so their digits need not fit in a machine word.
fn next_ref_number(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit < b'9' { *digit += 1; return; }
        *digit = b'0';
    }
    digits.insert(0, b'1');
}

/// Count each prefix in evaluation order, including stored refs. A stored R02 holds 2:
/// only a colliding automatic part moves, to the lowest free number outside the other counts.
/// Editor add paths write `added: N`. Those parts skip the count and are numbered after the
/// counted parts' final numbers in (N, evaluation order), so adding one renumbers no other part.
/// Linear in parts plus stored refs per prefix, with a sort of each prefix's automatic additions.
fn component_refs(records: &[CompRec], parts: &[PartRec]) -> Vec<String> {
    let mut by_prefix = HashMap::<&str, Vec<usize>>::new();
    let mut stored = Vec::new();
    let mut refs = Vec::with_capacity(records.len());
    for (i, rec) in records.iter().enumerate() {
        by_prefix.entry(&parts[rec.part].prefix).or_default().push(i);
        if let Some(reference) = &rec.refdes { stored.push((reference, rec.added.is_none())); }
        refs.push(rec.refdes.clone().unwrap_or_default());
    }
    for (prefix, indices) in by_prefix {
        let mut held = HashSet::new();
        let mut highest_stored = "";
        for &(reference, counted) in &stored {
            let Some(digits) = reference.strip_prefix(prefix) else { continue };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { continue; }
            let digits = digits.trim_start_matches('0');
            held.insert(digits);
            if counted && (digits.len(), digits) > (highest_stored.len(), highest_stored) { highest_stored = digits; }
        }
        let mut count = 0usize;
        let mut highest_auto = 0;
        let mut taken = HashSet::new();
        let mut displaced = Vec::new();
        let mut added = Vec::new();
        for i in indices {
            let rec = &records[i];
            if let Some(order) = rec.added {
                if rec.refdes.is_none() { added.push((order, i)); }
                continue;
            }
            count += 1;
            if rec.refdes.is_some() { continue; }
            if !held.is_empty() {
                if held.contains(count.to_string().as_str()) { displaced.push(i); continue; }
                taken.insert(count);
            }
            refs[i] = format!("{prefix}{count}");
            highest_auto = count;
        }
        let mut free = 0;
        for i in displaced {
            loop {
                free += 1;
                if !taken.contains(&free) && !held.contains(free.to_string().as_str()) { break; }
            }
            refs[i] = format!("{prefix}{free}");
            highest_auto = highest_auto.max(free);
        }
        if added.is_empty() { continue; }
        added.sort_unstable();
        let auto = highest_auto.to_string();
        let mut digits = (auto.len(), auto.as_str()).max((highest_stored.len(), highest_stored)).1.as_bytes().to_vec();
        for (_, i) in added {
            loop {
                next_ref_number(&mut digits);
                let number = std::str::from_utf8(&digits).expect("the suffix contains decimal digits");
                if !held.contains(number) { refs[i] = format!("{prefix}{number}"); break; }
            }
        }
    }
    refs
}

/// Turn the records into the unit's circuit, in the order things were made.
pub(crate) fn lower(unit: BoardId, name: &str, root: SourceRange, session: &Ctx, build: Build) -> Result<UnitOut, Vec<Diag>> {
    let mut errors = Vec::new();
    let circuit_doc = root.doc;
    let mut map: Vec<(Sel, SourceRange)> = vec![(Sel::Unit(unit), root)];
    let range = |h: &Here| SourceRange { doc: h.doc, span: h.span.clone() };

    let mut classes = vec![NetClass { id: ClassId(0), name: "default".into(), rules: ClassRules::default() }];
    for (name, rules) in &build.classes {
        classes.push(NetClass { id: ClassId(classes.len() as u16), name: name.clone(), rules: rules.clone() });
    }
    // Nets: the ones no merge removed, in the order they were made.
    let mut net_id: Vec<Option<NetId>> = vec![None; build.nets.len()];
    let mut nets = Vec::new();
    for (i, n) in build.nets.iter().enumerate() {
        if build.find(i) != i {
            continue;
        }
        let id = NetId(nets.len() as u32);
        net_id[i] = Some(id);
        let class = match &n.class {
            None => ClassId(0),
            Some(c) => match classes.iter().find(|k| &k.name == c) {
                Some(k) => k.id,
                None => {
                    errors.push(located(n.here.doc, n.here.span.clone(), format!("{}: no net class has the name {c:?} (netclass(\"{c}\", {{..}}) defines one)", n.name)));
                    ClassId(0)
                }
            },
        };
        if nets.iter().any(|m: &Net| m.name == n.name) {
            errors.push(located(n.here.doc, n.here.span.clone(), format!("{}: two nets have this name", n.name)));
        }
        map.push((Sel::Net(NetRef { board: unit, net: id }), range(&n.here)));
        nets.push(Net { id, name: n.name.clone(), class, volts: n.volts, kind: n.kind, scope: n.scope.map(|i| BlockId(i as u32)) });
    }
    let net = |i: usize| net_id[build.find(i)].expect("a representative has an id");
    let buses = build.buses.iter().enumerate().map(|(i, b)| Bus {
        id: BusId(i as u32),
        name: b.name.clone(),
        members: b.members.iter().map(|(m, n)| (m.clone(), net(*n))).collect(),
    }).collect();
    let mut tree = Vec::new();
    for (i, b) in build.insts.iter().enumerate() {
        if let Some(h) = &b.here {
            if build.insts[..i].iter().any(|o| o.path == b.path) {
                errors.push(located(h.doc, h.span.clone(), format!("{}: two block instances have this path (name them apart)", b.path)));
            }
            map.push((Sel::Block(unit, BlockId(i as u32)), range(h)));
        }
        tree.push(Block {
            id: BlockId(i as u32),
            path: b.path.clone(),
            def: b.def.map(|d| fab_elec_circuit::BlockDefId(d as u32)),
            parent: b.parent.map(|p| BlockId(p as u32)),
            ports: b.ports.iter().map(|(p, ns)| (p.clone(), ns.iter().map(|n| net(*n)).collect())).collect(),
            params: b.params.clone(),
            source: BlockSource::Code,
        });
    }
    let block_defs = build.used.iter().enumerate().map(|(u, &d)| {
        let mut d = session.defs[d].def.clone();
        d.id = fab_elec_circuit::BlockDefId(u as u32);
        d
    }).collect();
    // Value sites: how many components read each literal, how many instances share each call.
    let key = |r: &SourceRange| (r.doc, r.span.start, r.span.end);
    let mut literal_count: HashMap<(DocId, u32, u32), usize> = HashMap::new();
    for c in &build.comps {
        if let Some(site) = &c.site {
            site.visit(&mut |site| {
                if let ValueSite::Literal { at, .. } = site {
                    *literal_count.entry(key(at)).or_insert(0) += 1;
                }
            });
        }
    }
    let mut call_count: HashMap<(DocId, u32, u32), usize> = HashMap::new();
    for i in &build.insts {
        if let Some(h) = &i.here {
            *call_count.entry((h.doc, h.span.start, h.span.end)).or_insert(0) += 1;
        }
    }
    let mut pin_count: HashMap<(DocId, u32, u32, usize, &str), usize> = HashMap::new();
    for c in &build.comps {
        for (_, s) in &c.pin_sites {
            if let Some(k) = s.key() {
                *pin_count.entry(k).or_insert(0) += 1;
            }
        }
    }
    let mut pins: Vec<((CompRef, PinIx), Vec<PinSite>)> = Vec::new();
    for (i, c) in build.comps.iter().enumerate() {
        let comp = CompRef { board: unit, comp: CompId(i as u32) };
        for (pin, s) in &c.pin_sites {
            let mut s = s.clone();
            if let Some(k) = s.key() {
                let n = pin_count[&k];
                s.set_count(n);
            }
            match pins.iter_mut().find(|(k, _)| *k == (comp, *pin)) {
                Some((_, list)) => list.push(s),
                None => pins.push(((comp, *pin), vec![s])),
            }
        }
    }
    let mut values = Vec::new();
    for (i, c) in build.comps.iter().enumerate() {
        let Some(site) = &c.site else { continue };
        let mut site = site.clone();
        site.visit_mut(&mut |site| match site {
            ValueSite::Literal { at, count } => *count = literal_count[&key(at)],
            ValueSite::Param { call, count, .. } => *count = call_count.get(&key(call)).copied().unwrap_or(1),
            _ => {}
        });
        values.push((CompRef { board: unit, comp: CompId(i as u32) }, site));
    }
    // Bench circuits and catalog previews stand alone. Design units are classified together.
    let standalone_counts = unit.is_bench_scope().then(|| {
        let mut counts = HashMap::new();
        for c in &build.comps {
            *counts.entry(range(&c.here)).or_insert(0usize) += 1;
        }
        counts
    });
    let references = component_refs(&build.comps, &session.parts);
    let mut added = Vec::new();
    let mut comps = Vec::new();
    for (i, (c, refdes)) in build.comps.iter().zip(references).enumerate() {
        if build.comps[..i].iter().any(|o| o.path == c.path) {
            errors.push(located(c.here.doc, c.here.span.clone(), format!("{}: two components have this path (name them apart)", c.path)));
        }
        let part = &session.parts[c.part];
        let mut conns: Vec<(PinIx, NetId)> = c.conns.iter().map(|(p, n)| (*p, net(*n))).collect();
        conns.sort_by_key(|(p, _)| *p);
        let id = CompId(i as u32);
        if let Some(order) = c.added { added.push((id, order)); }
        let at = range(&c.here);
        let ref_kind = match (&c.refdes, &standalone_counts) {
            (Some(_), _) => RefKind::Stored,
            (None, Some(counts)) => automatic_ref_kind(&at, counts),
            (None, None) => RefKind::Auto,
        };
        map.push((Sel::Comp(CompRef { board: unit, comp: id }), at));
        comps.push(Component {
            id,
            path: c.path.clone(),
            refdes,
            ref_kind,
            part: part.def.clone(),
            value: c.value.clone(),
            conns,
            nc: c.nc.clone(),
            block: BlockId(c.inst as u32),
            dnp: c.dnp,
            fields: c.fields.clone(),
        });
    }
    let tgt = |t: &Tgt| match *t {
        Tgt::Comp(c) => Target::Comp(CompId(c as u32)),
        Tgt::Pin(c, p) => Target::Pin(CompId(c as u32), p),
        Tgt::Inst(b) => Target::Block(BlockId(b as u32)),
    };
    let mut intents = Vec::with_capacity(build.intents.len());
    for record in &build.intents {
        let intent = match record {
            IntentRec::Near { a, b, max } => Intent::Near { a: tgt(a), b: tgt(b), max: *max },
            IntentRec::Group { name, members } => Intent::Group { name: name.clone(), members: members.iter().map(tgt).collect() },
            IntentRec::Side { t, side } => Intent::Side { t: tgt(t), side: *side },
            IntentRec::Fixed { t, edge, at, rot } => Intent::Fixed { t: tgt(t), edge: *edge, at: *at, rot: *rot },
            IntentRec::Width { net: n, w } => Intent::Width { net: net(*n), w: *w },
            IntentRec::Pair { p, n, gap, z0, skew, here } => {
                let (p_net, n_net) = (net(*p), net(*n));
                if p_net == n_net {
                    let join = build.joining_call(*p, *n);
                    let callee = join.callee.as_deref().unwrap_or("call");
                    let mut diag = located(here.doc, here.span.clone(), format!("e.pair: p and n are one net: {callee} joins them"));
                    diag.sel.push(Sel::Source { doc: join.doc, start: join.span.start, end: join.span.end });
                    errors.push(diag);
                    continue;
                }
                let pair = Intent::Pair { p: p_net, n: n_net, gap: *gap, z0: *z0 };
                if let Some(tol) = skew {
                    intents.push(pair);
                    Intent::LengthMatch { nets: vec![p_net, n_net], tol: *tol }
                } else {
                    pair
                }
            }
            IntentRec::LengthMatch { nets, tol, here } => {
                // Net records may have been joined by a later call. Keep each distinct final net once.
                let mut seen = HashSet::new();
                let nets: Vec<_> = nets.iter().map(|n| net(*n)).filter(|n| seen.insert(*n)).collect();
                if nets.len() < 2 {
                    errors.push(located(here.doc, here.span.clone(), "e.match_length: nets must contain at least two distinct nets"));
                    continue;
                }
                Intent::LengthMatch { nets, tol: *tol }
            }
        };
        intents.push(intent);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut notes: Vec<Diag> = build.hint_keys.iter().filter(|(path, _)| {
        !build.comps.iter().any(|c| &c.path == path)
            && !build.insts.iter().any(|b| &b.path == path)
    }).map(|(path, h)| Diag::warning(
        format!("sheet hint path {path:?} names no component or block instance"),
        vec![Sel::Source { doc: h.doc, start: h.span.start, end: h.span.end }],
    )).collect();
    // Only an override written in the circuit document itself is this circuit's alone: one in a
    // project part document may be for another unit or a bench.
    if !unit.is_bench_scope() {
        notes.extend(session.warnings.iter().cloned());
        for body in session.bodies.iter().filter(|b| !b.applied && b.here.doc == circuit_doc) {
            let mut message = format!("e.body: no part named {} was made in this circuit", body.name);
            let prefix = body.name.split('_').next();
            let mut made: Vec<&str> = session.part_names.keys()
                .filter(|name| name.split('_').next() == prefix)
                .map(String::as_str).collect();
            made.sort_unstable();
            made.truncate(8);
            if !made.is_empty() {
                message.push_str(&format!("; parts made: {}", made.join(", ")));
            }
            notes.push(Diag::warning(message, vec![Sel::Source {
                doc: body.here.doc, start: body.here.span.start, end: body.here.span.end,
            }]));
        }
    }
    let circuit = Circuit {
        unit,
        name: name.to_string(),
        comps,
        nets,
        classes,
        buses,
        tree,
        block_defs,
        intents,
        hints: build.hints,
    };
    let bodies = session.bodies.iter().filter(|b| b.applied && session.part_names.contains_key(&b.name)).map(|b| (b.name.clone(), BodySite {
        at: SourceRange { doc: b.here.doc, span: b.here.span.clone() }, own: b.own.clone(),
    })).collect();
    Ok(UnitOut { circuit, notes, map, added, values, pins, bodies, phases: Phases::default() })
}

