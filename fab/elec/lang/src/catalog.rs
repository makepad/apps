//! Library definitions, enumerated lazily once per library or project definition snapshot.

use crate::circuit::{self, Build, Ctx, DefRec, Here, PartRec, Shared};
use crate::circuit::read::{field, key_string, lut_id, Handle, Reader, V};
use crate::eval::Evaluated;
use crate::map::SourceRange;
use crate::source::{Doc, DocKind, Libs, Sources};
use fab_elec_circuit::{Circuit, NetKind, PortKind};
use fab_elec_core::{canon::Writer, BoardId, Canon, ContentHash, Diag, DocId};
use fab_elec_parts::{PartDef, PinKind, VRange};
use makepad_script::syntax::{self, Kind, Node, Syntax};
use makepad_script::{NoTrap, ScriptObject, ScriptValue, ScriptVm, ScriptVmBase, ScriptVmHost};
use std::fmt;
use std::sync::{Arc, OnceLock};

/// The Library panel's entries, from the project's and the libraries' definitions; sorted by (lib, name).
#[derive(Clone, PartialEq, Debug)]
pub struct Catalog {
    /// The definitions available in the Library panel.
    pub entries: Vec<CatalogEntry>,
}

/// One definition offered by the Library panel as an AddPart template.
#[derive(Clone, PartialEq, Debug)]
pub struct CatalogEntry {
    /// The library name; "" is the project.
    pub lib: String,
    /// The definition's name within its library.
    pub name: String,
    /// The definition's category in the Library panel.
    pub kind: CatalogKind,
    /// The title shown in the Library panel.
    pub title: String,
    /// The owned comment's first sentence, "" without one.
    pub summary: String,
    /// The pins in placeholder order.
    pub pins: Vec<CatalogPin>,
    /// The AddPart template, defaults filled in.
    pub stmt: String,
    /// The template's binding, for chip text.
    pub stem: String,
    /// The defaults as display text.
    pub defaults: Vec<(String, String)>,
    /// The library's own definition, for passives and parts.
    pub part: Option<Arc<PartDef>>,
    /// The definition's content hash.
    pub hash: u64,
}

/// The kind of definition offered by a catalog entry.
#[derive(Clone, PartialEq, Debug)]
pub enum CatalogKind {
    /// A passive whose part definition supplies its PartKind.
    Passive,
    /// A library part definition.
    Part,
    /// A circuit block definition.
    Block,
}

/// One catalog pin, in the template's placeholder order.
#[derive(Clone, PartialEq, Debug)]
pub struct CatalogPin {
    /// The pin's name in the definition.
    pub name: String,
    /// The pin's electrical kind.
    pub kind: PinKind,
}

pub(crate) struct LibDefs {
    entries: Vec<CatalogEntry>,
    diags: Vec<Diag>,
    inputs: u64,
}

impl fmt::Debug for LibDefs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LibDefs")
            .field("entries", &self.entries.len())
            .field("diags", &self.diags.len())
            .finish()
    }
}

/// What a catalog's definitions were built from: cheap to clone (shared texts).
#[derive(Clone)]
pub struct CatalogSource {
    libs: Libs,
    docs: Vec<Doc>,
    assets: Vec<String>,
}

impl fmt::Debug for CatalogSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatalogSource")
            .field("docs", &self.docs.len())
            .field("library_docs", &self.libs.docs().len())
            .field("assets", &self.assets.len())
            .finish()
    }
}

/// The inputs and lazy catalog of a project's part and block documents.
/// Circuit-only edits share this snapshot; no VM runs until the catalog is requested.
pub struct ProjectDefs {
    source: Arc<CatalogSource>,
    entries: OnceLock<(Vec<CatalogEntry>, Vec<Diag>)>,
    catalog: OnceLock<Arc<Catalog>>,
}

impl ProjectDefs {
    /// Capture definition documents and assets without evaluating them, including on a failed load.
    pub fn new(sources: &Sources, libs: &Libs) -> Self {
        Self {
            source: Arc::new(CatalogSource {
                libs: libs.clone(),
                docs: definition_docs(sources).cloned().collect(),
                assets: sources.mesh_assets().to_vec(),
            }),
            entries: OnceLock::new(),
            catalog: OnceLock::new(),
        }
    }

    /// The shared definition inputs, retained across circuit-only edits.
    pub fn source(&self) -> &Arc<CatalogSource> {
        &self.source
    }

    pub(crate) fn matches(&self, sources: &Sources, libs: &Libs) -> bool {
        same_docs(self.source.docs.iter(), definition_docs(sources))
            && self.source.libs.same(libs) && self.source.assets.as_slice() == sources.mesh_assets()
    }
}

impl fmt::Debug for ProjectDefs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProjectDefs")
            .field("docs", &self.source.docs.len())
            .field("library_docs", &self.source.libs.docs().len())
            .field("assets", &self.source.assets.len())
            .field("entries", &self.entries.get().map_or(0, |(entries, _)| entries.len()))
            .field("diags", &self.entries.get().map_or(0, |(_, diags)| diags.len()))
            .field("catalog_entries", &self.catalog.get().map_or(0, |catalog| catalog.entries.len()))
            .finish()
    }
}

/// Equal definition inputs by content: the same documents, library documents and assets. Reuse
/// decisions go through `matches`, which compares texts by identity.
impl PartialEq for ProjectDefs {
    fn eq(&self, other: &Self) -> bool {
        self.source.docs == other.source.docs && self.source.libs == other.source.libs && self.source.assets == other.source.assets
    }
}

fn definition_docs(sources: &Sources) -> impl Iterator<Item = &Doc> {
    sources.all_docs().iter().filter(|doc| doc.present && is_definition(doc))
}

fn is_definition(doc: &Doc) -> bool {
    matches!(doc.kind, DocKind::Part | DocKind::Blocks)
}

fn same_docs<'a, 'b>(a: impl Iterator<Item = &'a Doc>, mut b: impl Iterator<Item = &'b Doc>) -> bool {
    for a in a {
        if !b.next().is_some_and(|b| a.id == b.id && Arc::ptr_eq(&a.text, &b.text)) {
            return false;
        }
    }
    b.next().is_none()
}

/// The definitions offered by the Library panel, sorted by library and export name.
/// Each library snapshot and project definition change runs one enumeration VM on first use;
/// later calls share the catalog Arc. A mismatched library snapshot is evaluated uncached.
pub fn catalog(libs: &Libs, evaluated: &Evaluated) -> Arc<Catalog> {
    defs_catalog(libs, &evaluated.defs)
}

/// [`catalog`] from the definition inputs alone, which a failed load also keeps
/// ([`ProjectDefs::new`]); the same cache and cost.
pub fn defs_catalog(libs: &Libs, project: &ProjectDefs) -> Arc<Catalog> {
    let library = libs.defs.get_or_init(|| {
        let inputs = library_inputs(libs);
        let (entries, diags) = enumerate(libs, None, inputs);
        Arc::new(LibDefs { entries, diags, inputs })
    });
    if !project.source.libs.same(libs) {
        let (entries, _) = enumerate(libs, Some(project), project_inputs(project, library.inputs));
        return joined(&entries, &library.entries);
    }
    project.catalog.get_or_init(|| {
        let (entries, _) = project.entries.get_or_init(|| {
            enumerate(libs, Some(project), project_inputs(project, library.inputs))
        });
        joined(entries, &library.entries)
    }).clone()
}

/// The rail a miniature's power port sits at when the port declares no volts. Only miniatures use
/// it, never a design, and a miniature that does names those ports (`Miniature::previewed`).
const MINIATURE_RAIL: f64 = 3.3;

/// One block built alone for the Library panel.
#[derive(Clone, Debug)]
pub struct Miniature {
    pub circuit: Arc<Circuit>,
    /// The power ports that declare no volts, previewed at the nominal rail, in port order.
    pub previewed: Vec<String>,
}

impl Miniature {
    /// The tile's detail line about its rail: "previewed at 3.3 V" (naming the ports), or `None`
    /// when every power port declares its volts.
    pub fn rail_note(&self) -> Option<String> {
        (!self.previewed.is_empty()).then(|| format!("{} previewed at {MINIATURE_RAIL} V", self.previewed.join(", ")))
    }
}

/// Each block alone: one net per port, named after the port, with the port's kind. Entries that
/// are not blocks, or whose export no longer exists in `src`, give a located error.
/// Cost: one VM (the libraries and the project's definition documents) per call, then one build
/// and lowering per entry; the caller asks only for the tiles that became visible.
pub fn blocks_alone(src: &CatalogSource, entries: &[&CatalogEntry]) -> Vec<Result<Miniature, Vec<Diag>>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let mut host = ScriptVmHost::new((), ());
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    let empty = Doc {
        id: DocId(u32::MAX), present: true, name: "catalog.ckt.splash".into(),
        kind: DocKind::Circuit, text: "".into(), lib: None,
    };
    let ctx = match circuit::prepare_vm(&mut vm, &empty, &src.libs) {
        Ok(ctx) => ctx,
        Err(diags) => return entries.iter().map(|_| Err(diags.clone())).collect(),
    };
    ctx.borrow_mut().docs.retain(|doc| doc.id == empty.id || src.libs.doc(doc.id).is_some_and(is_definition));
    circuit::load_libraries(&mut vm, &ctx);
    circuit::load_project_docs(&mut vm, &ctx, src.docs.iter(), &src.assets);
    // This VM is only for miniatures. Planned blocks show their ports, even if a builder exists.
    for rec in &mut ctx.borrow_mut().defs {
        if rec.def.planned {
            rec.build = None;
        }
    }
    // A definition document's own mistakes are the catalog's diagnostics; a miniature reports only
    // what its block's build says, so one broken document does not blank every tile.
    ctx.borrow_mut().diags.clear();
    let _ = vm.take_errors();
    entries.iter().map(|entry| {
        if entry.kind != CatalogKind::Block {
            return Err(vec![entry_error(src, &ctx.borrow(), entry, "not a block")]);
        }
        build_block(&mut vm, &ctx, src, entry)
    }).collect()
}

fn entry_error(src: &CatalogSource, ctx: &Ctx, entry: &CatalogEntry, message: &str) -> Diag {
    let mut at = SourceRange { doc: DocId(u32::MAX), span: 0..0 };
    for doc in src.docs.iter().chain(src.libs.docs()).filter(|doc| {
        is_definition(doc) && doc.lib.as_deref().unwrap_or("") == entry.lib
    }) {
        if at.doc == DocId(u32::MAX) {
            at = SourceRange { doc: doc.id, span: 0..doc.text.len() as u32 };
        }
        if let Some(part) = ctx.parts.iter().find(|part| {
            part.here.doc == doc.id && entry.part.as_ref().is_some_and(|def| def.name == part.def.name)
        }) {
            at = SourceRange { doc: part.here.doc, span: part.here.span.clone() };
            break;
        }
        if let Some(stmt) = ctx.docs.iter().find(|d| d.id == doc.id).and_then(|d| d.names())
            .and_then(|names| names.syntax.statements().iter().find(|stmt| stmt.let_name() == Some(entry.name.as_str())))
        {
            at = SourceRange { doc: doc.id, span: stmt.span.start as u32..stmt.span.end as u32 };
            break;
        }
    }
    crate::source::located(at.doc, at.span, format!("{}.{}: {message}", entry.lib, entry.name))
}

fn build_block(vm: &mut ScriptVm, ctx: &Shared, src: &CatalogSource, entry: &CatalogEntry) -> Result<Miniature, Vec<Diag>> {
    let module = vm.module(lut_id(if entry.lib.is_empty() { "project" } else { &entry.lib }));
    let value = if module == ScriptObject::ZERO { None } else {
        Some(field(vm, module, lut_id(&entry.name))).filter(|value| !value.is_nil())
    };
    let Some(value) = value else {
        return Err(vec![entry_error(src, &ctx.borrow(), entry, "export no longer exists")]);
    };
    let (ports, here) = {
        let cx = ctx.borrow();
        let index = cx.holder.and_then(|holder| {
            (0..cx.defs.len()).find(|index| field(vm, holder, lut_id(&format!("__block{index}"))) == value)
        });
        let Some(index) = index else {
            return Err(vec![entry_error(src, &cx, entry, "not a block")]);
        };
        (cx.defs[index].def.ports.clone(), cx.defs[index].here.clone())
    };
    let args = vm.bx.heap_mut().new_object();
    let _root = vm.new_object_ref(args);
    let (saved, mark, previewed) = {
        let mut cx = ctx.borrow_mut();
        let serial = cx.next_build_serial;
        cx.next_build_serial = serial.checked_add(1).ok_or_else(|| vec![crate::source::located(
            here.doc, here.span.clone(), format!("{}.{}: no circuit build serial is left", entry.lib, entry.name),
        )])?;
        let saved = std::mem::replace(&mut cx.build, Build::root());
        cx.build.serial = serial;
        publish_build(vm, serial);
        let mark = cx.diags.len();
        let mut previewed = Vec::new();
        for port in &ports {
            let names = match port.kind {
                PortKind::Bus { width } => (0..width).map(|i| format!("{}[{i}]", port.name)).collect(),
                PortKind::Pair => ["p", "n"].map(|side| format!("{}.{side}", port.name)).to_vec(),
                _ => vec![port.name.clone()],
            };
            let kind = match port.kind {
                PortKind::Ground => NetKind::Ground,
                PortKind::PowerIn | PortKind::PowerOut => NetKind::Power,
                _ => NetKind::Signal,
            };
            // A miniature only draws the block: a power port without declared volts sits at the
            // nominal rail, so a block that sizes its parts from its rail (led_driver) builds, and
            // the miniature says so.
            let volts = port.volts.clone().or_else(|| (kind == NetKind::Power).then(|| {
                previewed.push(port.name.clone());
                VRange::fixed(MINIATURE_RAIL)
            }));
            let ends: Vec<_> = names.into_iter().map(|name| {
                let net = cx.build.new_net(name, true, None, here.clone());
                cx.build.nets[net].kind = kind;
                cx.build.nets[net].volts = volts.clone();
                circuit::net_handle(vm, &cx, net)
            }).collect();
            let value = if matches!(port.kind, PortKind::Bus { .. } | PortKind::Pair) {
                let heap = vm.bx.heap_mut();
                let array = heap.new_array();
                for end in ends {
                    heap.array_push(array, end, NoTrap);
                }
                array.into()
            } else {
                ends[0]
            };
            vm.bx.heap_mut().set_value_def(args, lut_id(&port.name).into(), value);
        }
        (saved, mark, previewed)
    };
    vm.with_instruction_limit(circuit::EVAL_LIMIT, |vm| vm.call(value, &[args.into()]));
    let errors = vm.take_errors();
    let mut cx = ctx.borrow_mut();
    let build = std::mem::replace(&mut cx.build, saved);
    publish_build(vm, cx.build.serial);
    let mut diags: Vec<_> = errors.iter().map(|error| circuit::vm_diag(&cx, error)).collect();
    diags.extend(cx.diags.split_off(mark));
    if !diags.is_empty() {
        return Err(diags);
    }
    let root = SourceRange { doc: here.doc, span: here.span };
    // The upper id range never names a design unit; miniatures stay outside the design.
    circuit::lower(BoardId(u16::MAX), &entry.name, root, &cx, build)
        .map(|out| Miniature { circuit: Arc::new(out.circuit), previewed })
}

fn joined(project: &[CatalogEntry], library: &[CatalogEntry]) -> Arc<Catalog> {
    let mut entries: Vec<_> = project.iter().chain(library).cloned().collect();
    entries.sort_by(|a, b| (&a.lib, &a.name).cmp(&(&b.lib, &b.name)));
    Arc::new(Catalog { entries })
}

fn hash64(hash: ContentHash) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&hash.0[..8]);
    u64::from_le_bytes(bytes)
}

fn library_inputs(libs: &Libs) -> u64 {
    let mut w = Writer::hasher();
    w.count(libs.docs().len());
    for doc in libs.docs() {
        doc.lib.canon(&mut w);
        w.str(&doc.name);
        w.str(&doc.text);
    }
    hash64(w.finish())
}

fn project_inputs(project: &ProjectDefs, library: u64) -> u64 {
    let mut w = Writer::hasher();
    w.u64(library);
    w.count(project.source.docs.len());
    for doc in &project.source.docs {
        w.str(&doc.name);
        w.str(&doc.text);
    }
    project.source.assets.canon(&mut w);
    hash64(w.finish())
}

fn block_hash(inputs: u64, lib: &str, name: &str) -> u64 {
    let mut w = Writer::hasher();
    w.u64(inputs);
    w.str(lib);
    w.str(name);
    hash64(w.finish())
}

fn enumerate(libs: &Libs, project: Option<&ProjectDefs>, inputs: u64) -> (Vec<CatalogEntry>, Vec<Diag>) {
    let mut host = ScriptVmHost::new((), ());
    let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
    // This document belongs to neither snapshot and is never evaluated.
    let empty = Doc {
        id: DocId(u32::MAX), present: true, name: "catalog.ckt.splash".into(),
        kind: DocKind::Circuit, text: "".into(), lib: None,
    };
    let ctx = match circuit::prepare_vm(&mut vm, &empty, libs) {
        Ok(ctx) => ctx,
        Err(diags) => return (vec![], diags),
    };
    // Libs accepts other suffixes too; only definition documents belong in this VM.
    ctx.borrow_mut().docs.retain(|doc| doc.id == empty.id || libs.doc(doc.id).is_some_and(is_definition));
    circuit::load_libraries(&mut vm, &ctx);
    let loaded = {
        let ctx = ctx.borrow();
        (ctx.parts.len(), ctx.defs.len())
    };
    let (mut entries, mut returned) = offers(&mut vm, &ctx, false);
    let offered = {
        let ctx = ctx.borrow();
        (ctx.parts.len(), ctx.defs.len())
    };
    let mut part_range = 0..loaded.0;
    if let Some(project) = project {
        // Library diagnostics are already retained by LibDefs.
        ctx.borrow_mut().diags.clear();
        let _ = vm.take_errors();
        circuit::load_project_docs(&mut vm, &ctx, project.source.docs.iter(), &project.source.assets);
        part_range = offered.0..ctx.borrow().parts.len();
        (entries, returned) = offers(&mut vm, &ctx, true);
    }
    let mut ctx = ctx.borrow_mut();
    for index in part_range.filter(|index| !returned.contains(index)) {
        let part = &ctx.parts[index];
        let lib = if project.is_some() { "" } else {
            libs.doc(part.here.doc).and_then(|doc| doc.lib.as_deref()).unwrap_or("")
        };
        entries.push(part_entry(&ctx, part, lib));
    }
    let def_range = if project.is_some() { offered.1..ctx.defs.len() } else { 0..offered.1 };
    let mut methods = Vec::new();
    if let Some(holder) = ctx.holder {
        for index in def_range {
            methods.push((index, field(&vm, holder, lut_id(&format!("__block{index}")))));
        }
    }
    let mut modules = Vec::new();
    if project.is_some() {
        modules.push(String::new());
    } else {
        for doc in libs.docs() {
            if let Some(lib) = &doc.lib {
                if !modules.contains(lib) {
                    modules.push(lib.clone());
                }
            }
        }
    }
    let mut diags = std::mem::take(&mut ctx.diags);
    for lib in modules {
        let module = vm.module(lut_id(if lib.is_empty() { "project" } else { &lib }));
        if module == ScriptObject::ZERO {
            continue;
        }
        for export in vm.bx.heap_ref().map_entries(module) {
            let Some(&(index, _)) = methods.iter().find(|(_, value)| *value == export.value) else { continue };
            let name = key_string(&vm, export.key);
            let def = &ctx.defs[index];
            entries.push(block_entry(&ctx, def, &lib, &name, inputs, &mut diags));
        }
    }
    diags.extend(vm.take_errors().iter().map(|error| circuit::vm_diag(&ctx, error)));
    (entries, diags)
}

/// Offers run only in enumeration VMs, once per library snapshot or project definition change.
fn offers(vm: &mut ScriptVm, ctx: &Shared, project: bool) -> (Vec<CatalogEntry>, Vec<usize>) {
    let errors = vm.take_errors();
    let diags: Vec<_> = errors.iter().map(|error| circuit::vm_diag(&ctx.borrow(), error)).collect();
    ctx.borrow_mut().diags.extend(diags);
    let catalogs: Vec<_> = ctx.borrow().catalogs.iter().filter(|(lib, _)| lib.is_empty() == project).cloned().collect();
    let mut entries = Vec::new();
    let mut returned = Vec::new();
    for (lib, doc) in catalogs {
        let tree = ctx.borrow().docs.iter().find(|d| d.id == doc)
            .and_then(|d| d.names()).map(|names| names.syntax.clone());
        let Some(tree) = tree else { continue };
        let stmt = tree.statements().iter().find(|stmt| stmt.let_name() == Some("catalog"));
        let span = stmt.map_or(0..tree.source().len(), |stmt| stmt.span.clone());
        let fail = |message| crate::source::located(doc, span.start as u32..span.end as u32, message);
        let list = stmt.and_then(|stmt| match &stmt.kind {
            Kind::Let { value: Some(value), .. } => match &value.kind {
                Kind::Fn { params, body, .. } if params.is_empty() => single_expression(body),
                _ => None,
            },
            _ => None,
        });
        let Some(list) = list.filter(|list| matches!(list.kind, Kind::Array { .. })) else {
            ctx.borrow_mut().diags.push(fail("catalog: expected let catalog = fn() { [..] }".to_string()));
            continue;
        };
        let module = vm.module(lut_id(if lib.is_empty() { "project" } else { &lib }));
        let function = field(vm, module, lut_id("catalog"));
        if !matches!(Reader::new(vm).read(function), V::Fn) {
            ctx.borrow_mut().diags.push(fail("catalog: export is not a function".to_string()));
            continue;
        }
        let value = vm.with_instruction_limit(circuit::EVAL_LIMIT, |vm| vm.call(function, &[]));
        for error in vm.take_errors() {
            ctx.borrow_mut().diags.push(fail(format!("catalog: {error}")));
        }
        let Some(array) = value.as_array() else {
            let description = Reader::new(vm).read(value).describe();
            ctx.borrow_mut().diags.push(fail(format!("catalog: expected a list, got {description}")));
            continue;
        };
        let _root = vm.bx.heap_mut().new_array_ref(array);
        let items = list.items().expect("a catalog body is a list literal");
        let len = vm.bx.heap_ref().array_len(array);
        if len != items.len() {
            ctx.borrow_mut().diags.push(fail(format!("catalog: returned {len} elements for {} offers", items.len())));
            continue;
        }
        for (index, node) in items.iter().enumerate() {
            let value = vm.bx.heap_ref().array_index(array, index, NoTrap);
            let result = match Reader::new(vm).read(value) {
                V::Handle(Handle::Part(part)) => {
                    let ctx = ctx.borrow();
                    let mut entry = part_entry(&ctx, &ctx.parts[part], &lib);
                    entry.stmt = part_stmt(&ctx.parts[part], tree.text(node));
                    Ok((entry, part))
                }
                V::Fn => function_offer(vm, ctx, doc, &lib, &tree, node, value),
                other => Err(format!("catalog: expected a part or function, got {}", other.describe())),
            };
            match result {
                Ok((mut entry, part)) => {
                    let comments = comment_lines(&tree, list, index);
                    if let Some(title) = comments.first() { entry.title = title.clone(); }
                    entry.summary = first_sentence(&comments.get(1..).unwrap_or_default().join(" "));
                    entries.push(entry);
                    returned.push(part);
                }
                Err(message) => ctx.borrow_mut().diags.push(crate::source::located(
                    doc, node.span.start as u32..node.span.end as u32, message,
                )),
            }
        }
    }
    (entries, returned)
}

fn single_expression(body: &Node) -> Option<&Node> {
    let block = body.block()?;
    (block.stmts.len() == 1).then(|| &block.stmts[0])
}

fn publish_build(vm: &mut ScriptVm, serial: u64) {
    let module = vm.module(lut_id("elec"));
    vm.bx.heap_mut().set_value_def(module, lut_id("__build").into(), (serial as f64).into());
}

fn function_offer(
    vm: &mut ScriptVm, ctx: &Shared, doc: DocId, lib: &str, tree: &Syntax, node: &Node, value: ScriptValue,
) -> Result<(CatalogEntry, usize), String> {
    let Kind::Fn { params, body, .. } = &node.kind else {
        return Err("catalog: a function offer must be a function literal with one call".into());
    };
    let call = single_expression(body).filter(|call| matches!(call.kind, Kind::Call { .. }))
        .ok_or("catalog: a function offer's body must be one call expression")?;
    let (callee, args) = call.as_call().ok_or("catalog: an offer's call must have a named callee")?;
    let stem = callee.rsplit('.').next().unwrap_or(callee).to_string();
    let syntax = syntax::parse_with(tree.text(call), &crate::units::circuit_units()).map_err(|e| e.to_string())?;
    let mut replacements = Vec::new();
    syntax.walk(&mut |loc, node| {
        if let Some(index) = params.iter().position(|param| node.ident() == Some(param.name.as_str())) {
            replacements.push((loc.clone(), format!("_n{index}")));
        }
    });
    let stmt = format!("let {stem} = {}", syntax.replace_nodes(&replacements).map_err(|e| e.message)?.print());
    let mut defaults = Vec::new();
    if matches!(callee, "e.r" | "e.c" | "e.l") {
        if let Some(value) = args.first() { defaults.push(("value".into(), tree.text(value).to_string())); }
    }
    if let Some((_, fields)) = args.iter().find_map(Node::object) {
        defaults.extend(fields.iter().filter_map(|field| {
            field.name.as_ref().map(|name| (name.clone(), tree.text(&field.value).to_string()))
        }));
    }
    let name = std::iter::once(stem.clone()).chain(defaults.iter().map(|(_, value)| value.replace('"', "")))
        .collect::<Vec<_>>().join(" ");
    let here = Here { doc, span: node.span.start as u32..node.span.end as u32, callee: None, path: None, loops: vec![] };
    let (saved, mark, nets, ends) = {
        let mut cx = ctx.borrow_mut();
        let serial = cx.next_build_serial;
        cx.next_build_serial = serial.checked_add(1).ok_or("catalog: no circuit build serial is left")?;
        let saved = std::mem::replace(&mut cx.build, Build::root());
        cx.build.serial = serial;
        publish_build(vm, serial);
        let nets: Vec<_> = params.iter().map(|param| cx.build.new_net(param.name.clone(), false, Some(0), here.clone())).collect();
        let ends: Vec<_> = nets.iter().map(|net| circuit::net_handle(vm, &cx, *net)).collect();
        (saved, cx.diags.len(), nets, ends)
    };
    vm.with_instruction_limit(circuit::EVAL_LIMIT, |vm| vm.call(value, &ends));
    let mut errors = vm.take_errors();
    let mut cx = ctx.borrow_mut();
    let build = std::mem::replace(&mut cx.build, saved);
    publish_build(vm, cx.build.serial);
    errors.extend(cx.diags.split_off(mark).into_iter().map(|diag| diag.message));
    if !errors.is_empty() { return Err(format!("catalog: {}", errors.join("; "))); }
    if build.comps.len() != 1 {
        return Err(format!("catalog: a function offer must make one component, made {}", build.comps.len()));
    }
    let comp = &build.comps[0];
    let part = &cx.parts[comp.part].def;
    let pins = nets.iter().zip(params).map(|(net, param)| {
        let (pin, _) = comp.conns.iter().find(|(_, connected)| build.find(*connected) == build.find(*net))
            .ok_or_else(|| format!("catalog: parameter {} is on no component pin", param.name))?;
        let pin = &part.pins[pin.ix()];
        Ok(CatalogPin { name: pin.name.clone(), kind: pin.kind })
    }).collect::<Result<Vec<_>, String>>()?;
    let mut hash = Writer::hasher();
    part.content_hash().canon(&mut hash);
    hash.str(&stmt);
    Ok((CatalogEntry {
        lib: lib.into(), title: name.clone(), name, kind: CatalogKind::Passive, summary: String::new(),
        pins, stmt, stem, defaults, part: Some(part.clone()), hash: hash64(hash.finish()),
    }, comp.part))
}

fn key(name: &str) -> String {
    match syntax::write::key(name) {
        Ok(key) => key,
        // Native definitions may contain an empty pin name; keep it as a quoted key.
        Err(_) => syntax::write::quote(name),
    }
}

/// The binding stem for a part's refdes prefix: its letters, digits and underscores, lowercased
/// (`#FLG` makes `flg`), else `u`, so the statement always parses.
fn part_stem(prefix: &str) -> String {
    let stem: String = prefix.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect::<String>().to_lowercase();
    if stem.is_empty() || stem.starts_with(|c: char| c.is_ascii_digit()) { "u".to_string() } else { stem }
}

fn part_stmt(part: &PartRec, expression: &str) -> String {
    let stem = part_stem(&part.prefix);
    let fields = part.def.pins.iter().enumerate().map(|(i, pin)| format!("{}: _n{i}", key(&pin.name))).collect::<Vec<_>>().join(" ");
    format!("let {stem} = e.part({expression}, {{pins: {{{fields}}}}})")
}

fn part_entry(ctx: &Ctx, part: &PartRec, lib: &str) -> CatalogEntry {
    let def = &part.def;
    let stem = part_stem(&part.prefix);
    let title = ["title", "mpn"].iter().find_map(|key| {
        def.fields.0.iter().find(|(name, _)| name.as_str() == *key).map(|(_, text)| text.clone())
    }).unwrap_or_else(|| def.name.clone());
    let pins = def.pins.iter().map(|pin| CatalogPin { name: pin.name.clone(), kind: pin.kind }).collect();
    CatalogEntry {
        lib: lib.into(), name: def.name.clone(), kind: CatalogKind::Part, title,
        summary: summary(ctx, &part.here), pins,
        stmt: part_stmt(part, &syntax::write::quote(&def.name)),
        stem, defaults: vec![], part: Some(def.clone()), hash: hash64(def.content_hash()),
    }
}

fn block_entry(ctx: &Ctx, rec: &DefRec, lib: &str, name: &str, inputs: u64, diags: &mut Vec<Diag>) -> CatalogEntry {
    let def = &rec.def;
    let mut pins = Vec::new();
    let mut fields = Vec::new();
    for port in &def.ports {
        let start = pins.len();
        match port.kind {
            PortKind::Bus { width } => {
                pins.extend((0..width).map(|i| CatalogPin { name: format!("{}[{i}]", port.name), kind: PinKind::Bidi }));
            }
            PortKind::Pair => {
                pins.extend(["p", "n"].map(|side| CatalogPin { name: format!("{}.{side}", port.name), kind: PinKind::Bidi }));
            }
            _ => pins.push(CatalogPin { name: port.name.clone(), kind: pin_kind(&port.kind) }),
        }
        let placeholders = (start..pins.len()).map(|i| format!("_n{i}")).collect::<Vec<_>>().join(" ");
        let value = if matches!(port.kind, PortKind::Bus { .. } | PortKind::Pair) {
            format!("[{placeholders}]")
        } else {
            placeholders
        };
        fields.push(format!("{}: {value}", key(&port.name)));
    }
    let defaults = def.params.iter().map(|(name, value)| {
        let text = match crate::splice::param_text(value) {
            Ok(text) => text,
            Err(message) => {
                diags.push(crate::source::located(rec.here.doc, rec.here.span.clone(), message));
                format!("{value:?}")
            }
        };
        (name.clone(), text)
    }).collect();
    let module = if lib.is_empty() { "project" } else { lib };
    CatalogEntry {
        lib: lib.into(), name: name.into(), kind: CatalogKind::Block, title: def.name.clone(),
        summary: summary(ctx, &rec.here), pins,
        stmt: format!("let {name} = mod.{module}.{name}({{{}}})", fields.join(" ")),
        stem: name.into(), defaults, part: None, hash: block_hash(inputs, lib, name),
    }
}

fn pin_kind(kind: &PortKind) -> PinKind {
    match kind {
        PortKind::PowerIn | PortKind::Ground => PinKind::PowerIn,
        PortKind::PowerOut => PinKind::PowerOut,
        PortKind::In => PinKind::In,
        PortKind::Out => PinKind::Out,
        PortKind::Passive => PinKind::Passive,
        PortKind::Bidi | PortKind::Bus { .. } | PortKind::Pair | PortKind::Protocol(_) => PinKind::Bidi,
    }
}

fn summary(ctx: &Ctx, here: &Here) -> String {
    let Some(names) = ctx.docs.iter().find(|doc| doc.id == here.doc).and_then(|doc| doc.names()) else { return String::new() };
    let syntax = &names.syntax;
    let Some(index) = syntax.statements().iter().position(|stmt| {
        stmt.span.start <= here.span.start as usize && here.span.end as usize <= stmt.span.end
    }) else { return String::new() };
    first_sentence(&comment_lines(syntax, syntax.root(), index).join(" "))
}

fn comment_lines(tree: &Syntax, node: &Node, index: usize) -> Vec<String> {
    let Ok(range) = syntax::edit::owned_comments(tree.source(), node, index) else { return Vec::new() };
    tree.source()[range].lines().filter_map(|line| {
        line.trim_start().strip_prefix("//").map(|line| line.strip_prefix(' ').unwrap_or(line).to_string())
    }).collect()
}

fn first_sentence(text: &str) -> String {
    match text.char_indices().find(|&(i, c)| c == '.' && (i + 1 == text.len() || text.as_bytes()[i + 1] == b' ')) {
        Some((i, _)) => text[..=i].to_string(),
        None => text.to_string(),
    }
}
