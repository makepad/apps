//! Benches record stimulus and checks against a design unit or their own circuit function.
//! Each block bench calls its circuit function and lowers once per evaluation, then compares
//! its circuit once with the previous one for reuse. Nothing here runs per frame.

mod sites;

use crate::circuit::{
    self, Build, Shared,
    read::{Handle, Reader, V, arg, field, lut_id, reported_error},
};
use crate::eval::{BenchOut, BenchPanel, BenchProbe, BenchScope, Evaluated, PanelEntry, PanelGroup, UnitEval, UnitState};
use crate::map::{BenchKey, BenchSite, Origin, SourceMap, SourceRange};
use crate::source::{Doc, DocKind, Libs, Sources, located};
use fab_elec_bench::{
    BenchSpec,
    bench::{BenchSource, Firmware, ScopeProbe, SourceKind, Stub, Supply},
    expect::{Bound, Expect, Measure, Probe, When},
};
use fab_elec_circuit::{Circuit, NetKind, QtyUnit};
use fab_elec_core::{
    BENCH_SCOPE_BASE, BoardId, CompRef, Diag, NetId, NetRef, PinIx, PinRef, Sel,
    drive::{Drive, Pull},
};
use fab_elec_design::{ProbeOf, ProbeQuantity, Tip, Unit};
use fab_elec_parts::PartKind;
use makepad_script::{
    syntax::{Kind, Node},
    *,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Clone)]
enum Value {
    Drive(Drive),
    Probe(Probe),
    Measure {
        probe: Probe,
        measure: Measure,
        when: When,
        at: SourceRange,
    },
}

/// Evaluated context for editing stimuli, keyed by the bench body or an existing statement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stimulus {
    /// The body supplies the circuit whose net names a new stimulus can use.
    Circuit(Arc<Circuit>),
    Supply(Supply),
    Source(SourceKind),
    Ambiguous,
}

impl Stimulus {
    /// The call, unit and evaluated value of an editable numeric option.
    pub(crate) fn option(&self, key: &str) -> Result<(&'static str, QtyUnit, Option<f64>), String> {
        use SourceKind::*;
        let (call, value) = match self {
            Self::Supply(s) => ("supply", match key {
                "v" => Some(s.v), "limit" => s.limit, "ramp" => s.ramp, _ => None,
            }),
            Self::Source(Vdc(dc)) => ("vsrc", Some(*dc)),
            Self::Source(Idc(dc)) => ("isrc", Some(*dc)),
            Self::Source(Pulse { low, high, at, width, every, edge }) => ("pulse", match key {
                "v" => Some(*high), "low" => Some(*low), "at" => Some(*at),
                "width" => Some(*width), "every" => *every, "edge" => Some(*edge), _ => None,
            }),
            Self::Source(LoadR(_) | LoadI(_)) if matches!(key, "rise" | "at") => {
                return Err(format!("{key} is only allowed with step"));
            }
            Self::Source(LoadR(_)) if key == "i" => return Err("load already has r".into()),
            Self::Source(LoadI(_)) if key == "r" => return Err("load already has i".into()),
            Self::Source(LoadStep { .. }) if matches!(key, "i" | "r") => {
                return Err("load already has step".into());
            }
            Self::Source(LoadR(r) | LoadI(r)) => ("load", Some(*r)),
            Self::Source(LoadStep { at, rise, .. }) => ("load", match key {
                "at" => Some(*at), "rise" => Some(*rise), _ => None,
            }),
            Self::Ambiguous => return Err("ambiguous source pair".into()),
            Self::Circuit(_) => return Err("the bench body has no stimulus options".into()),
        };
        let unit = stimulus_unit(call, key)
            .ok_or_else(|| format!("{call} has no numeric option {key}"))?;
        Ok((call, unit, value))
    }
}

struct Record {
    out: BenchOut,
    circuit: Option<Arc<Circuit>>,
    circuit_fn: Option<ScriptValue>,
    /// The `panel` option and its site, read once the bench's circuit is known.
    panel: Option<(ScriptValue, SourceRange)>,
    own_map: Vec<(Sel, SourceRange)>,
    docs: Shared,
    /// Project firmware names from Sources; the VM's docs only hold evaluated documents.
    firmware_docs: Rc<HashSet<String>>,
    diags: Vec<Diag>,
    sites: Vec<(BenchKey, BenchSite)>,
    stimulus_sites: Vec<(BenchKey, BenchSite, Stimulus)>,
    stub_sites: Vec<(String, SourceRange)>,
    /// Each firmware call, by component path.
    fw_sites: Vec<(String, SourceRange)>,
    values: HashMap<ScriptObject, (Value, Origin)>,
    roots: Vec<ScriptObjectRef>,
    stubs: Vec<Stub>,
    pressed: Vec<String>,
    turned: Vec<(String, f64)>,
    supplies: Vec<Supply>,
    sources: Vec<BenchSource>,
    probes: Vec<ScopeProbe>,
    firmware: Vec<Firmware>,
    nets: Vec<fab_elec_bench::net::NetEndpoint>,
    failed_unit: bool,
}

type Bench = Rc<RefCell<Record>>;

/// Previous names keep their identities even when failed or not yet reached this evaluation.
struct Ledger {
    previous: HashMap<String, (Arc<Circuit>, u64)>,
    held: HashSet<BoardId>,
    next: u32,
    next_generation: Option<u64>,
}

impl Ledger {
    fn new(prev: Option<&Evaluated>) -> Self {
        let mut previous = HashMap::new();
        let mut held = HashSet::new();
        let mut generation = 0u64;
        for bench in prev.into_iter().flat_map(|p| &p.benches) {
            if bench.scope_id().is_bench_scope() {
                held.insert(bench.scope_id());
            }
            if let BenchScope::Own {
                circuit,
                generation: g,
            } = &bench.scope
            {
                previous.insert(bench.name.clone(), (circuit.clone(), *g));
                generation = generation.max(*g);
            }
        }
        let next = held
            .iter()
            .map(|id| u32::from(id.0) + 1)
            .max()
            .unwrap_or(u32::from(BENCH_SCOPE_BASE));
        Self {
            previous,
            held,
            next,
            next_generation: generation.checked_add(1),
        }
    }

    fn scope(&mut self, name: &str) -> Option<BoardId> {
        if let Some((circuit, _)) = self.previous.get(name) {
            return Some(circuit.unit);
        }
        let id = if self.next <= u32::from(u16::MAX) {
            let id = BoardId(self.next as u16);
            self.next += 1;
            id
        } else {
            (BENCH_SCOPE_BASE..=u16::MAX)
                .map(BoardId)
                .find(|id| !self.held.contains(id))?
        };
        self.held.insert(id);
        Some(id)
    }

    fn circuit(&mut self, name: &str, new: Circuit) -> Result<(Arc<Circuit>, u64), &'static str> {
        if let Some((circuit, generation)) = self.previous.get(name) {
            if **circuit == new {
                return Ok((circuit.clone(), *generation));
            }
        }
        let generation = self
            .next_generation
            .ok_or("no bench circuit generation is left")?;
        self.next_generation = generation.checked_add(1);
        Ok((Arc::new(new), generation))
    }
}

impl Record {
    fn fail(&mut self, at: &SourceRange, message: impl AsRef<str>) -> ScriptValue {
        self.diags.push(located(
            at.doc,
            at.span.clone(),
            format!("{}: {}", self.out.name, message.as_ref()),
        ));
        NIL
    }

    /// The only place stimulus records are fitted to the current simulator spec.
    fn finish(&mut self) {
        self.out.spec = BenchSpec {
            stubs: std::mem::take(&mut self.stubs),
            pressed: std::mem::take(&mut self.pressed),
            turned: std::mem::take(&mut self.turned),
            supplies: std::mem::take(&mut self.supplies),
            sources: std::mem::take(&mut self.sources),
            probes: std::mem::take(&mut self.probes),
            firmware: std::mem::take(&mut self.firmware),
            nets: std::mem::take(&mut self.nets),
        };
    }

    fn value(&mut self, vm: &mut ScriptVm, value: Value, at: &SourceRange) -> ScriptValue {
        let object = vm.bx.heap_mut().new_object();
        self.roots.push(vm.new_object_ref(object));
        self.values
            .insert(object, (value, sites::origin(vm, &self.docs, at)));
        object.into()
    }

    fn get(&self, raw: ScriptValue) -> Option<&(Value, Origin)> {
        self.values.get(&raw.as_object()?)
    }

    fn circuit(&self) -> &Circuit {
        self.circuit
            .as_deref()
            .expect("bench natives are only installed for an available circuit")
    }

    fn net_name(&self, net: NetRef) -> Result<String, String> {
        self.circuit().nets.iter().find(|n| n.id == net.net)
            .map(|n| n.name.clone()).ok_or_else(|| "net is not in this circuit".into())
    }
}

pub(crate) fn evaluate(
    prev: Option<&Evaluated>,
    sources: &Sources,
    libs: &Libs,
    units: &[Unit],
    evals: &[UnitEval],
    map: &mut SourceMap,
    cancel: &AtomicBool,
) -> Option<Vec<BenchOut>> {
    let mut out = Vec::new();
    let mut names = HashSet::new();
    let ledger = Rc::new(RefCell::new(Ledger::new(prev)));
    let firmware_docs = Rc::new(sources.docs().iter()
        .filter(|doc| doc.kind == DocKind::Firmware).map(|doc| doc.name.clone()).collect());
    let now = Now { units, firmware_docs };
    for doc in sources.docs().iter().filter(|d| d.kind == DocKind::Bench) {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let (records, mut diags) = run(doc, sources, libs, &now, evals, &mut names, &ledger);
        for r in &records {
            diags.extend(r.borrow().diags.clone());
        }
        let old: Vec<&BenchOut> = prev
            .into_iter()
            .flat_map(|p| &p.benches)
            .filter(|b| b.doc == doc.id)
            .collect();
        let mut seen = HashSet::new();
        for r in records {
            let mut r = r.borrow_mut();
            r.finish();
            seen.insert(r.out.name.clone());
            if diags.is_empty() {
                for (sel, range) in &r.own_map {
                    map.insert(sel.clone(), range.clone());
                }
                for (key, at) in &r.sites {
                    map.bench_sites
                        .insert((r.out.name.clone(), key.clone()), at.clone());
                }
                if let Some(circuit) = &r.circuit {
                    map.bench_stimuli.insert((r.out.name.clone(), BenchKey::Body), Stimulus::Circuit(circuit.clone()));
                }
                for (key, site, value) in &r.stimulus_sites {
                    let key = (r.out.name.clone(), key.clone());
                    map.bench_sites.insert(key.clone(), site.clone());
                    map.bench_stimuli.entry(key)
                        .and_modify(|v| *v = Stimulus::Ambiguous).or_insert_with(|| value.clone());
                }
                for (path, at) in &r.stub_sites {
                    map.bench_stubs
                        .insert((r.out.name.clone(), path.clone()), at.clone());
                }
                for (path, at) in &r.fw_sites {
                    map.bench_firmware
                        .insert((r.out.name.clone(), path.clone()), at.clone());
                }
                out.push(r.out.clone());
            } else {
                let before = old
                    .iter()
                    .copied()
                    .find(|b| b.name == r.out.name && !matches!(b.state, UnitState::Failed(_)));
                if let Some(before) = before.filter(|_| !r.failed_unit) {
                    out.push(keep(before, prev.expect("previous bench"), &diags, map, &now));
                } else {
                    r.out.state = UnitState::Failed(named_diags(&r.out.name, &diags));
                    out.push(r.out.clone());
                }
            }
        }
        // A syntax/runtime error can prevent later calls from being reached. Keep those too.
        if !diags.is_empty() {
            for before in old.into_iter().filter(|b| !seen.contains(&b.name)) {
                names.insert(before.name.clone());
                if matches!(before.state, UnitState::Failed(_)) {
                    let mut failed = before.clone();
                    failed.state = UnitState::Failed(named_diags(&before.name, &diags));
                    out.push(failed);
                } else {
                    out.push(keep(before, prev.expect("previous bench"), &diags, map, &now));
                }
            }
        }
    }
    // Kept benches retain their evaluated values alongside their retained source sites.
    for bench in &out {
        if matches!(bench.state, UnitState::Failed(_)) {
            continue;
        }
        for expect in &bench.expects {
            map.bench_expects
                .insert((bench.name.clone(), expect.name.clone()), expect.clone());
        }
        for stub in &bench.spec.stubs {
            for (pin, drive) in &stub.pins {
                map.bench_drives.insert(
                    (bench.name.clone(), stub.path.clone(), pin.clone()),
                    drive.clone(),
                );
            }
        }
    }
    Some(out)
}

fn named_diags(name: &str, diags: &[Diag]) -> Vec<Diag> {
    diags
        .iter()
        .cloned()
        .map(|mut d| {
            if !d.message.starts_with(&format!("{name}:")) {
                d.message = format!("{name}: {}", d.message);
            }
            d
        })
        .collect()
}

/// This evaluation's units and the project's firmware document names, which benches resolve
/// against and kept benches are checked against.
struct Now<'a> {
    units: &'a [Unit],
    firmware_docs: Rc<HashSet<String>>,
}

fn keep(before: &BenchOut, prev: &Evaluated, diags: &[Diag], map: &mut SourceMap, now: &Now) -> BenchOut {
    if matches!(before.scope, BenchScope::Own { .. }) {
        for (sel, range) in prev.map.of_doc(before.doc) {
            if sel.unit() == Some(before.scope_id()) {
                map.insert(sel, range);
            }
        }
    }
    for ((name, key), at) in &prev.map.bench_sites {
        if name == &before.name {
            let target = (name.clone(), key.clone());
            if let Some(value) = prev.map.bench_stimuli.get(&target) {
                map.bench_stimuli.insert(target.clone(), value.clone());
            }
            map.bench_sites.insert(target, at.clone());
        }
    }
    for ((name, path), at) in &prev.map.bench_stubs {
        if name == &before.name {
            map.bench_stubs
                .insert((name.clone(), path.clone()), at.clone());
        }
    }
    for ((name, path), at) in &prev.map.bench_firmware {
        if name == &before.name {
            map.bench_firmware
                .insert((name.clone(), path.clone()), at.clone());
        }
    }
    let mut kept = before.clone();
    let mut kept_diags = named_diags(&before.name, diags);
    kept_diags.extend(stale(before, now, map, diags));
    kept.state = UnitState::Kept(kept_diags);
    kept
}

/// The names a kept bench still runs with that this evaluation no longer has: a stubbed or
/// firmware component gone from its design unit, a firmware part that is no longer an MCU, a
/// firmware document gone from the project. Each is an error at its call, unless one of `diags`
/// already sits inside that call. Cost: O(stubs + firmware) component lookups.
fn stale(before: &BenchOut, now: &Now, map: &SourceMap, diags: &[Diag]) -> Vec<Diag> {
    let BenchScope::Design(id) = before.scope else { return Vec::new() };
    let Some(unit) = now.units.iter().find(|unit| unit.id == id) else { return Vec::new() };
    let name = &before.name;
    let mut out = Vec::new();
    let mut error = |site: Option<&SourceRange>, message: String| {
        let Some(site) = site else { return };
        let covered = diags.iter().flat_map(|d| &d.sel).any(|sel| matches!(sel,
            Sel::Source { doc, start, end } if *doc == site.doc && site.span.start <= *start && *end <= site.span.end));
        if !covered {
            out.push(located(site.doc, site.span.clone(), format!("{name}: {message}")));
        }
    };
    for stub in &before.spec.stubs {
        if unit.circuit.comp(&stub.path).is_none() {
            error(map.bench_stubs.get(&(name.clone(), stub.path.clone())),
                format!("{}: no such component in the circuit now; its stub drives nothing", stub.path));
        }
    }
    for fw in &before.spec.firmware {
        let site = map.bench_firmware.get(&(name.clone(), fw.path.clone()));
        match unit.circuit.comp(&fw.path) {
            None => error(site, format!("{}: no such component in the circuit now; its firmware runs nowhere", fw.path)),
            Some(comp) if comp.part.kind != PartKind::Mcu => error(site, format!("{}: firmware runs on an MCU part", fw.path)),
            Some(_) => {}
        }
        if !now.firmware_docs.contains(&fw.doc) {
            error(site, format!("firmware: no firmware document {} in the project; {} runs no firmware", fw.doc, fw.path));
        }
    }
    out
}

fn run(
    doc: &Doc,
    sources: &Sources,
    libs: &Libs,
    now: &Now,
    evals: &[UnitEval],
    names: &mut HashSet<String>,
    ledger: &Rc<RefCell<Ledger>>,
) -> (Vec<Bench>, Vec<Diag>) {
    let mut host = ScriptVmHost::new((), ());
    let mut vm = ScriptVm {
        host: host.host_context(),
        bx: Box::new(ScriptVmBase::new()),
    };
    let docs = match circuit::prepare_vm(&mut vm, doc, libs) {
        Ok(docs) => docs,
        Err(diags) => return (vec![], diags),
    };
    let records = Rc::new(RefCell::new(Vec::new()));
    let known = Rc::new(RefCell::new(names.clone()));
    let module = vm.module(id_lut!(elec));
    {
        let (records, known, docs) = (records.clone(), known.clone(), docs.clone());
        let ledger = ledger.clone();
        let (units, evals) = (now.units.to_vec(), evals.to_vec());
        let firmware_docs = now.firmware_docs.clone();
        add(
            &mut vm,
            module,
            "bench",
            &["name", "opts", "build"],
            move |vm, args| {
                let at = sites::call(vm, &docs);
                let name = read(vm, args, "name");
                let Some(name) = name.text() else {
                    docs.borrow_mut().diags.push(located(
                        at.doc,
                        at.span,
                        "bench: expected a name string",
                    ));
                    return NIL;
                };
                let r = Rc::new(RefCell::new(Record {
                    out: BenchOut {
                        name: name.to_string(),
                        doc: at.doc,
                        scope: BenchScope::Design(BoardId::default()),
                        spec: BenchSpec::default(),
                        expects: vec![],
                        probes: vec![],
                        stop: None,
                        live: false,
                        panel: None,
                        state: UnitState::Built,
                    },
                    circuit: None,
                    circuit_fn: None,
                    panel: None,
                    own_map: vec![],
                    docs: docs.clone(),
                    firmware_docs: firmware_docs.clone(),
                    diags: vec![],
                    sites: vec![],
                    stimulus_sites: vec![],
                    stub_sites: vec![],
                    fw_sites: vec![],
                    values: HashMap::new(),
                    roots: vec![],
                    stubs: vec![],
                    pressed: vec![],
                    turned: vec![],
                    supplies: vec![],
                    sources: vec![],
                    probes: vec![],
                    firmware: vec![],
                    nets: vec![],
                    failed_unit: false,
                }));
                records.borrow_mut().push(r.clone());
                if !known.borrow_mut().insert(name.to_string()) {
                    return r.borrow_mut().fail(&at, "duplicate bench name");
                }
                let opts = arg(vm, args, id_lut!(opts));
                let configured = configure(vm, &mut r.borrow_mut(), opts, &at, &units, &evals);
                if let Err((site, message)) = configured {
                    return r.borrow_mut().fail(&site, message);
                }
                let circuit_fn = r.borrow().circuit_fn;
                if let Some(circuit_fn) = circuit_fn {
                    if !build_circuit(vm, &r, circuit_fn, opts, &at, &ledger) {
                        return NIL;
                    }
                }
                let panel = r.borrow_mut().panel.take();
                let circuit = r.borrow().circuit.clone();
                if let Some(((raw, site), circuit)) = panel.zip(circuit) {
                    match read_panel(vm, raw, &circuit, &site) {
                        Ok(panel) => r.borrow_mut().out.panel = Some(panel),
                        Err((site, message)) => return r.borrow_mut().fail(&site, message),
                    }
                }
                let build = arg(vm, args, id_lut!(build));
                if !build.as_object().is_some_and(|o| vm.bx.heap_ref().is_fn(o)) {
                    return r.borrow_mut().fail(&at, "expected a bench function");
                }
                if let Some(build) = sites::argument(&docs, &at, 2) {
                    r.borrow_mut()
                        .sites
                        .push((BenchKey::Body, sites::range(&at, &build).into()));
                }
                let b = bench_object(vm, &r);
                let c = circuit_object(vm, &r);
                vm.call(build, &[b.into(), c.into()]);
                // Attribute interpreter errors while this bench's name is still known.
                let errors = vm.take_errors();
                for error in errors {
                    let diag = circuit::vm_diag(&docs.borrow(), &error);
                    r.borrow_mut().diags.extend(named_diags(name, &[diag]));
                }
                NIL
            },
        );
    }
    let native = field(&vm, module, id_lut!(bench));
    vm.set_injected_global(id_lut!(bench), native);
    circuit::load_libraries(&mut vm, &docs);
    circuit::load_project(&mut vm, &docs, sources);
    vm.with_instruction_limit(circuit::EVAL_LIMIT, |vm| {
        vm.eval(ScriptMod::new("", "", &doc.name, 1, 0, &doc.text, &[]))
    });
    {
        let mut cx = docs.borrow_mut();
        let build = &cx.build;
        let first = build.nets.iter().map(|n| &n.here)
            .chain(build.comps.iter().map(|c| &c.here))
            .chain(build.insts.iter().skip(1).filter_map(|i| i.here.as_ref()))
            .chain(build.other_sites.iter())
            .min_by_key(|h| (h.doc, h.span.start))
            .cloned();
        if let Some(h) = first {
            cx.diags.push(located(h.doc, h.span, "circuit items in a bench document go in a bench's circuit: fn"));
        }
    }
    let mut diags = docs.borrow().diags.clone();
    for error in vm.take_errors() {
        diags.push(circuit::vm_diag(&docs.borrow(), &error));
    }
    drop(vm);
    *names = known.borrow().clone();
    let records = std::mem::take(&mut *records.borrow_mut());
    (records, diags)
}

fn publish_build(vm: &mut ScriptVm, serial: u64) {
    let module = vm.bx.heap_ref().module(id_lut!(elec));
    vm.bx
        .heap_mut()
        .set_value_def(module, id_lut!(__build).into(), (serial as f64).into());
}

fn build_circuit(
    vm: &mut ScriptVm,
    bench: &Bench,
    circuit_fn: ScriptValue,
    raw: ScriptValue,
    at: &SourceRange,
    ledger: &Rc<RefCell<Ledger>>,
) -> bool {
    let (docs, name) = {
        let r = bench.borrow();
        (r.docs.clone(), r.out.name.clone())
    };
    let root = field_site(vm, &docs, raw, "circuit", true, at);
    // Until the circuit builds, the bench holds its id with an empty circuit: a failure with a
    // previous evaluation is Kept as any bench error is, one without fails and keeps the id.
    let Some(scope_id) = ledger.borrow_mut().scope(&name) else {
        bench.borrow_mut().fail(&root, "no bench scope id is left");
        return false;
    };
    bench.borrow_mut().out.scope = BenchScope::Own {
        circuit: Arc::new(Circuit {
            unit: scope_id,
            name: name.clone(),
            ..Circuit::default()
        }),
        generation: 0,
    };
    let (mark, saved) = {
        let mut cx = docs.borrow_mut();
        let serial = cx.next_build_serial;
        let Some(next) = serial.checked_add(1) else {
            bench
                .borrow_mut()
                .fail(&root, "no circuit build serial is left");
            return false;
        };
        cx.next_build_serial = next;
        let saved = std::mem::replace(&mut cx.build, Build::root());
        cx.build.serial = serial;
        publish_build(vm, serial);
        (cx.diags.len(), saved)
    };
    vm.call(circuit_fn, &[]);
    let errors = vm.take_errors();
    let build = {
        let mut cx = docs.borrow_mut();
        let build = std::mem::replace(&mut cx.build, saved);
        publish_build(vm, cx.build.serial);
        let mut r = bench.borrow_mut();
        for error in errors {
            r.diags
                .extend(named_diags(&name, &[circuit::vm_diag(&cx, &error)]));
        }
        r.diags
            .extend(named_diags(&name, &cx.diags.split_off(mark)));
        build
    };
    if !bench.borrow().diags.is_empty() {
        return false;
    }
    let lowered = circuit::lower(scope_id, &name, root.clone(), &docs.borrow(), build);
    let mut r = bench.borrow_mut();
    let out = match lowered {
        Ok(out) => out,
        Err(diags) => {
            r.diags.extend(named_diags(&name, &diags));
            return false;
        }
    };
    let mut grounds = out
        .circuit
        .nets
        .iter()
        .filter(|n| n.kind == NetKind::Ground);
    if grounds.next().is_none() {
        r.fail(
            &root,
            "the bench circuit has no ground net (e.net({kind: @ground}))",
        );
        return false;
    }
    if let Some(second) = grounds.next() {
        let sel = Sel::Net(NetRef {
            board: scope_id,
            net: second.id,
        });
        let at = out
            .map
            .iter()
            .find(|(s, _)| *s == sel)
            .map(|(_, range)| range)
            .expect("lowering maps every net to its call site");
        r.fail(at, "a second ground net; a bench circuit has exactly one");
        return false;
    }
    let (circuit, generation) = match ledger.borrow_mut().circuit(&name, out.circuit) {
        Ok(value) => value,
        Err(message) => {
            r.fail(&root, message);
            return false;
        }
    };
    r.out.scope = BenchScope::Own {
        circuit: circuit.clone(),
        generation,
    };
    r.circuit = Some(circuit);
    r.own_map = out.map;
    // A bench does not use the lowering's value sites, pin sites or notes.
    true
}

type Fault = (SourceRange, String);

fn configure(
    vm: &mut ScriptVm,
    r: &mut Record,
    raw: ScriptValue,
    at: &SourceRange,
    units: &[Unit],
    evals: &[UnitEval],
) -> Result<(), Fault> {
    let opts = options(vm, &r.docs, raw, &["circuit", "run", "panel"], at, true)?;
    if let Some((_, panel)) = opts.iter().find(|(k, _)| k == "panel") {
        r.panel = Some((*panel, field_site(vm, &r.docs, raw, "panel", true, at)));
    }
    let circuit_at = field_site(vm, &r.docs, raw, "circuit", true, at);
    let name = match opts.iter().find(|(k, _)| k == "circuit") {
        Some((_, v)) => match Reader::new(vm).read(*v) {
            V::Str(name) => Some(name),
            V::Fn => {
                r.circuit_fn = Some(*v);
                None
            }
            _ => return Err((circuit_at, "circuit must be a unit name string or a circuit function".into())),
        },
        None if evals.len() == 1 => Some(evals[0].name.clone()),
        None => {
            return Err((
                at.clone(),
                "circuit is required unless the project has exactly one unit".into(),
            ));
        }
    };
    if let Some(name) = name {
        let Some(unit) = evals.iter().find(|u| u.name == name) else {
            return Err((circuit_at, format!("no unit named {name:?}")));
        };
        r.out.scope = BenchScope::Design(unit.id);
        if let UnitState::Failed(diags) = &unit.state {
            r.failed_unit = true;
            return Err((
                circuit_at,
                format!(
                    "unit {name:?} failed: {}",
                    diags
                        .iter()
                        .map(|d| d.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ));
        }
        r.circuit = units
            .iter()
            .find(|u| u.id == unit.id)
            .map(|u| u.circuit.clone());
        if r.circuit.is_none() {
            return Err((
                circuit_at,
                format!("unit {name:?} has no evaluated circuit"),
            ));
        }
    }
    if let Some((_, run)) = opts.iter().find(|(k, _)| k == "run") {
        let run_at = field_site(vm, &r.docs, raw, "run", true, at);
        for (key, value) in options(vm, &r.docs, *run, &["stop", "live"], &run_at, false)? {
            let key_at = field_site(vm, &r.docs, *run, &key, true, &run_at);
            if key == "live" {
                r.out.live = value.as_bool().ok_or_else(|| (key_at, "live: true or false".to_string()))?;
                continue;
            }
            let stop = number(value, "stop").map_err(|m| (key_at.clone(), m))?;
            if !stop.is_finite() || stop < 0.0 {
                return Err((key_at, "stop must be finite and nonnegative".into()));
            }
            r.out.stop = Some(stop);
        }
    }
    Ok(())
}

/// The `panel` option ([`BenchPanel`]) against the bench's circuit: O(parts + controls), once
/// per evaluation. A fault names the group and part; any fault fails the bench, as a bad probe
/// does, so the panel that shows is always one the circuit accepted.
fn read_panel(vm: &mut ScriptVm, raw: ScriptValue, circuit: &Circuit, at: &SourceRange) -> Result<BenchPanel, Fault> {
    let fault = |message: String| (at.clone(), format!("panel: {message}"));
    let known = |obj: &crate::circuit::read::Obj, allowed: &[&str], what: &str| match obj.fields.iter().find(|(key, _)| !allowed.contains(&key.as_str())) {
        Some((key, _)) => Err(fault(format!("unknown {what} option {key} (the options are {})", allowed.join(", ")))),
        None => Ok(()),
    };
    let V::Arr(groups) = Reader::new(vm).read(raw) else {
        return Err(fault("expected a list of groups, as [{group: \"Keys\" controls: [\"SW1\" \"SW2\"]}]".into()));
    };
    let parts: HashMap<&str, &fab_elec_circuit::Component> = circuit.comps.iter().map(|c| (c.refdes.as_str(), c)).collect();
    let mut named = HashSet::new();
    let mut panel = BenchPanel::default();
    for group in &groups {
        let V::Obj(group) = group else {
            return Err(fault("a group is {group: \"name\" controls: [...]}".into()));
        };
        known(group, &["group", "cols", "controls"], "group")?;
        let Some(name) = group.get("group").and_then(V::text) else {
            return Err(fault("a group needs its name, group: \"...\"".into()));
        };
        let cols = match group.get("cols") {
            None | Some(V::Nil) => None,
            Some(cols) => match cols.num().filter(|n| n.fract() == 0.0 && (1.0..=f64::from(u16::MAX)).contains(n)) {
                Some(n) => Some(n as u16),
                None => return Err(fault(format!("group {name:?}: cols is a whole number from 1"))),
            },
        };
        let Some(V::Arr(controls)) = group.get("controls") else {
            return Err(fault(format!("group {name:?}: controls is a list of parts, as [\"SW1\" {{part: \"RV1\" label: \"Decay\"}}]")));
        };
        let mut entries = Vec::with_capacity(controls.len());
        for control in controls {
            let (part, label) = match control {
                V::Str(part) => (part.as_str(), None),
                V::Obj(control) => {
                    known(control, &["part", "label"], "control")?;
                    let Some(part) = control.get("part").and_then(V::text) else {
                        return Err(fault(format!("group {name:?}: a control needs its part, part: \"SW1\"")));
                    };
                    let label = match control.get("label") {
                        None | Some(V::Nil) => None,
                        Some(V::Str(label)) => Some(label.clone()),
                        Some(_) => return Err(fault(format!("group {name:?}: {part}'s label is a string"))),
                    };
                    (part, label)
                }
                _ => return Err(fault(format!("group {name:?}: a control is a part, \"SW1\", or {{part: \"RV1\" label: \"Decay\"}}"))),
            };
            let Some(comp) = parts.get(part) else {
                return Err(fault(format!("group {name:?}: no part {part}")));
            };
            if !matches!(comp.part.kind, PartKind::Switch | PartKind::Potentiometer) {
                return Err(fault(format!("group {name:?}: {part} is not a switch or a potentiometer")));
            }
            if !named.insert(comp.id) {
                return Err(fault(format!("group {name:?}: {part} is already in the panel")));
            }
            entries.push(PanelEntry { comp: comp.id, label });
        }
        panel.groups.push(PanelGroup { name: name.to_owned(), cols, controls: entries });
    }
    Ok(panel)
}

fn add(
    vm: &mut ScriptVm,
    on: ScriptObject,
    name: &str,
    args: &[&str],
    f: impl Fn(&mut ScriptVm, ScriptObject) -> ScriptValue + 'static,
) {
    let defaults: Vec<_> = args.iter().map(|a| (lut_id(a), NIL)).collect();
    let keys: Vec<_> = defaults.iter().map(|(key, _)| *key).collect();
    vm.add_method(on, lut_id(name), &defaults, move |vm, args| {
        for key in &keys {
            let value = vm.bx.heap_ref().value(args, (*key).into(), NoTrap);
            if let Some(error) = reported_error(vm, value) {
                return error;
            }
        }
        f(vm, args)
    });
}

fn read(vm: &mut ScriptVm, args: ScriptObject, key: &str) -> V {
    let raw = arg(vm, args, lut_id(key));
    Reader::new(vm).read(raw)
}

fn number(raw: ScriptValue, name: &str) -> Result<f64, String> {
    raw.as_number()
        .ok_or_else(|| format!("{name}: expected a number"))
}

fn field_site(
    vm: &ScriptVm,
    docs: &Shared,
    object: ScriptValue,
    key: &str,
    value_only: bool,
    fallback: &SourceRange,
) -> SourceRange {
    sites::object(vm, docs, object)
        .and_then(|at| sites::field(docs, &at, key, value_only))
        .unwrap_or_else(|| fallback.clone())
}

/// Raw fields preserve both order and the identity of opaque values. Follow prototypes just
/// as the circuit reader does, with own fields taking precedence.
fn fields(vm: &mut ScriptVm, raw: ScriptValue) -> Result<Vec<(String, ScriptValue)>, String> {
    let Some(mut object) = raw.as_object().filter(|o| !vm.bx.heap_ref().is_fn(*o)) else {
        return Err("expected an options object".into());
    };
    let mut out = Vec::new();
    let mut visited = HashSet::new();
    // The visited set ends a prototype cycle; a chain has no length limit.
    while visited.insert(object) {
        let mut entries = vm.bx.heap_ref().map_entries_ordered(object);
        for i in 0..vm.bx.heap_ref().vec_len(object) {
            let entry = vm.bx.heap_ref().vec_key_value(object, i, NoTrap);
            entries.push(ScriptMapEntry {
                key: entry.key,
                value: entry.value,
            });
        }
        for entry in entries {
            let key = Reader::new(vm).read(entry.key);
            let Some(key) = key.text_or_id() else {
                return Err("an option has no field name".into());
            };
            if !out.iter().any(|(k, _)| k == key) {
                out.push((key.to_string(), entry.value));
            }
        }
        match vm.bx.heap_ref().proto(object).as_object() {
            Some(parent) => object = parent,
            None => break,
        }
    }
    Ok(out)
}

fn options(
    vm: &mut ScriptVm,
    docs: &Shared,
    raw: ScriptValue,
    allowed: &[&str],
    at: &SourceRange,
    optional: bool,
) -> Result<Vec<(String, ScriptValue)>, Fault> {
    if optional && raw.is_nil() {
        return Ok(vec![]);
    }
    let values = fields(vm, raw).map_err(|m| (at.clone(), m))?;
    for (key, _) in &values {
        if !allowed.contains(&key.as_str()) {
            return Err((
                field_site(vm, docs, raw, key, false, at),
                format!(
                    "unknown option {key} (the options are {})",
                    allowed.join(", ")
                ),
            ));
        }
    }
    Ok(values)
}

fn component(vm: &mut ScriptVm, r: &Record, raw: ScriptValue) -> Result<usize, String> {
    let value = Reader::new(vm).read(raw);
    if matches!(value, V::Stale) {
        return Err(value.describe());
    }
    match value.handle() {
        Some(Handle::Comp(i)) if i < r.circuit().comps.len() => Ok(i),
        _ => Err(format!(
            "expected a component, got {}",
            if matches!(value, V::Nil) {
                "nil".into()
            } else {
                value.describe()
            }
        )),
    }
}

fn pin(vm: &mut ScriptVm, r: &Record, raw: ScriptValue) -> Result<PinRef, String> {
    let value = Reader::new(vm).read(raw);
    if matches!(value, V::Stale) {
        return Err(value.describe());
    }
    let (i, p) = match value.handle() {
        Some(Handle::Pin(i, p)) => (i, p),
        Some(Handle::Comp(i)) => (i, 0),
        _ => return Err("expected a pin or component".into()),
    };
    let c = r
        .circuit()
        .comps
        .get(i)
        .ok_or("component is not in this circuit")?;
    if p as usize >= c.part.pins.len() {
        return Err("component has no such pin".into());
    }
    Ok(PinRef {
        board: r.out.scope_id(),
        comp: c.id,
        pin: PinIx(p),
    })
}

fn net(vm: &mut ScriptVm, r: &Record, raw: ScriptValue) -> Result<NetRef, String> {
    let value = Reader::new(vm).read(raw);
    if matches!(value, V::Stale) {
        return Err(value.describe());
    }
    let id = match value.handle() {
        Some(Handle::Net(i)) => r
            .circuit()
            .nets
            .get(i)
            .map(|n| n.id)
            .ok_or("net is not in this circuit")?,
        Some(Handle::Pin(i, p)) => {
            let c = r
                .circuit()
                .comps
                .get(i)
                .ok_or("component is not in this circuit")?;
            c.conns
                .iter()
                .find(|(pin, _)| pin.0 == p)
                .map(|(_, n)| *n)
                .ok_or_else(|| {
                    format!(
                        "{}.{} is unconnected",
                        c.path,
                        c.part.pins.get(p as usize).map_or("?", |p| p.name.as_str())
                    )
                })?
        }
        _ => return Err("expected a net or pin".into()),
    };
    Ok(NetRef {
        board: r.out.scope_id(),
        net: id,
    })
}

fn probe(r: &Record, raw: ScriptValue) -> Result<Probe, String> {
    match r.get(raw) {
        Some((Value::Probe(p), _)) => Ok(p.clone()),
        _ => Err("expected a probe".into()),
    }
}

fn bench_object(vm: &mut ScriptVm, bench: &Bench) -> ScriptObject {
    let b = vm.bx.heap_mut().new_object();
    bench.borrow_mut().roots.push(vm.new_object_ref(b));
    let methods: &[(&'static str, &[&str])] = &[
        ("probe", &["target", "opts"]),
        ("stub", &["comp", "pins", "opts"]),
        ("firmware", &["comp", "doc", "params"]),
        ("net", &["comp", "c", "opts"]),
        ("supply", &["net", "opts"]),
        ("vsrc", &["a", "b", "opts"]),
        ("isrc", &["a", "b", "opts"]),
        ("pulse", &["a", "b", "opts"]),
        ("load", &["a", "b", "opts"]),
        ("press", &["comp"]),
        ("turn", &["comp", "pos"]),
        ("pwm", &["freq", "duty"]),
        ("high", &[]),
        ("low", &[]),
        ("input", &["pull"]),
        ("v", &["a", "b"]),
        ("i", &["target"]),
        ("p", &["comp"]),
        ("now", &["probe", "when"]),
        ("mean", &["probe", "when"]),
        ("rms", &["probe", "when"]),
        ("min", &["probe", "when"]),
        ("max", &["probe", "when"]),
        ("pp", &["probe", "when"]),
        ("expect", &["name", "measure", "bounds"]),
    ];
    for &(name, args) in methods {
        let bench = bench.clone();
        add(vm, b, name, args, move |vm, args| {
            let mut r = bench.borrow_mut();
            let at = sites::call(vm, &r.docs);
            match invoke(vm, &mut r, name, args, &at) {
                Ok(value) => value,
                Err((at, message)) => r.fail(&at, message),
            }
        });
    }
    b
}

fn invoke(
    vm: &mut ScriptVm,
    r: &mut Record,
    name: &str,
    args: ScriptObject,
    at: &SourceRange,
) -> Result<ScriptValue, Fault> {
    let get = |vm: &ScriptVm, name| arg(vm, args, lut_id(name));
    let fault = |message| (at.clone(), message);
    let value = match name {
        "probe" => {
            let raw = get(vm, "target");
            let target_at = sites::argument(&r.docs, at, 0)
                .map(|node| sites::range(at, &node))
                .unwrap_or_else(|| at.clone());
            let probe = match r.get(raw) {
                Some((Value::Probe(p), _)) => p.clone(),
                Some((Value::Measure { .. }, _)) => return Err((target_at,
                    "b.probe takes a net or b.v(..), b.i(..), b.p(..), not a measure (a measure goes in b.expect)".into())),
                Some((Value::Drive(_), _)) => return Err((target_at,
                    "b.probe takes a net or b.v(..), b.i(..), b.p(..), not a drive".into())),
                None => Probe::V(net(vm, r, raw).map_err(fault)?),
            };
            let net_name = |net| r.net_name(net).map_err(fault);
            let of = match &probe {
                Probe::V(net) => ProbeOf::v(net_name(*net)?),
                Probe::VDiff(a, b) => {
                    if a.net == b.net {
                        return Err(fault(
                            "a differential probe needs two different nets".into(),
                        ));
                    }
                    ProbeOf::vdiff(net_name(*a)?, net_name(*b)?)
                }
                Probe::I(pin) => {
                    let comp = r
                        .circuit()
                        .comps
                        .iter()
                        .find(|c| c.id == pin.comp)
                        .ok_or_else(|| fault("component is not in this circuit".into()))?;
                    ProbeOf::i(format!(
                        "{}.{}",
                        comp.path, comp.part.pins[pin.pin.0 as usize].name
                    ))
                }
                Probe::P(comp) => {
                    let comp = r
                        .circuit()
                        .comps
                        .iter()
                        .find(|c| c.id == comp.comp)
                        .ok_or_else(|| fault("component is not in this circuit".into()))?;
                    ProbeOf::p(comp.path.clone())
                }
            };
            if r.out.probes.iter().any(|p| p.of == of) {
                return Err(fault(
                    if of.quantity == ProbeQuantity::V && of.reference.is_none() {
                        format!("{}: net is already probed", of.target)
                    } else {
                        format!("{of}: already probed")
                    },
                ));
            }
            let raw = get(vm, "opts");
            let opts = options(vm, &r.docs, raw, &["label", "load", "bw", "rise", "at"], at, true)?;
            let mut label = of.to_string();
            let mut pin = None;
            let mut tip = Tip::default();
            if opts.iter().any(|(key, _)| key == "bw") && opts.iter().any(|(key, _)| key == "rise")
            {
                return Err((
                    field_site(vm, &r.docs, raw, "rise", false, at),
                    "give bw or rise, not both".into(),
                ));
            }
            for (key, value) in opts {
                let value_at = field_site(vm, &r.docs, raw, &key, true, at);
                match key.as_str() {
                    "label" => {
                        label = match Reader::new(vm).read(value) {
                            V::Str(label) => label,
                            _ => return Err((value_at, "label must be a string".into())),
                        }
                    }
                    "load" => {
                        if of.quantity != ProbeQuantity::V {
                            return Err((field_site(vm, &r.docs, raw, "load", false, at),
                                "a current or power probe has no tip load (load is for voltage probes)".into()));
                        }
                        let load = options(vm, &r.docs, value, &["r", "c"], &value_at, false)?;
                        if load.is_empty() {
                            return Err((value_at, "a tip load needs r, c or both".into()));
                        }
                        for (key, raw_number) in load {
                            let load_at = field_site(vm, &r.docs, value, &key, true, &value_at);
                            let name = format!("load {key}");
                            let value =
                                number(raw_number, &name).map_err(|m| (load_at.clone(), m))?;
                            if !value.is_finite() || value <= 0.0 {
                                return Err((
                                    load_at,
                                    format!("{name} must be finite and positive"),
                                ));
                            }
                            if key == "r" {
                                tip.r = Some(value);
                            } else {
                                tip.c = Some(value);
                            }
                        }
                    }
                    "at" => {
                        pin = match Reader::new(vm).read(value) {
                            V::Str(pin) => Some(pin),
                            _ => return Err((value_at, "at must be a string naming a pin, \"<refdes>.<pin>\"".into())),
                        }
                    }
                    "bw" | "rise" => {
                        let value = number(value, &key).map_err(|m| (value_at.clone(), m))?;
                        if !value.is_finite() || value <= 0.0 {
                            return Err((value_at, format!("{key} must be finite and positive")));
                        }
                        tip.bw = Some(if key == "rise" { Tip::rise_bw(value) } else { value });
                    }
                    _ => unreachable!("options checks probe fields"),
                }
            }
            r.out.probes.push(BenchProbe {
                of: of.clone(),
                label: label.clone(),
                tip,
                at: pin,
            });
            r.probes.push(ScopeProbe { probe, label, tip });
            r.sites.push((BenchKey::Probe(of), at.clone().into()));
            return Ok(NIL);
        }
        "high" | "low" => Value::Drive(Drive::Level(name == "high")),
        "pwm" => {
            let freq = number(get(vm, "freq"), "frequency").map_err(fault)?;
            let duty = number(get(vm, "duty"), "duty").map_err(fault)?;
            if !freq.is_finite() || freq <= 0.0 {
                return Err(fault("frequency must be finite and positive".into()));
            }
            if !duty.is_finite() || !(0.0..=1.0).contains(&duty) {
                return Err(fault("duty must be between 0 and 1".into()));
            }
            Value::Drive(Drive::Pwm { freq, duty })
        }
        "input" => {
            let pull = match read(vm, args, "pull").id() {
                Some("up") => Pull::Up,
                Some("down") => Pull::Down,
                Some("none") => Pull::None,
                _ => return Err(fault("input expects @up, @down or @none".into())),
            };
            Value::Drive(Drive::Input(pull))
        }
        "stub" => {
            let raw_comp = get(vm, "comp");
            let comp = component(vm, r, raw_comp).map_err(fault)?;
            let path = r.circuit().comps[comp].path.clone();
            if r.firmware.iter().any(|fw| fw.path == path) {
                return Err(fault(format!("{path}: component runs firmware; it cannot be stubbed")));
            }
            if r.stubs.iter().any(|s| s.path == path) {
                return Err(fault(format!("{path}: component is already stubbed")));
            }
            let pins = get(vm, "pins");
            let mut drives = Vec::new();
            let mut ranges = Vec::new();
            for (pin, raw) in fields(vm, pins).map_err(fault)? {
                let pin_at = field_site(vm, &r.docs, pins, &pin, false, at);
                if !r.circuit().comps[comp]
                    .part
                    .pins
                    .iter()
                    .any(|p| p.name == pin)
                {
                    return Err((pin_at, format!("{path}: no pin named {pin}")));
                }
                let Some((Value::Drive(drive), made)) = r.get(raw) else {
                    return Err((pin_at, format!("{path}.{pin}: expected a drive")));
                };
                let pin_at = if pin_at == *at
                    || pin_at.doc != made.call.doc
                    || pin_at.span.start > made.call.span.start
                    || pin_at.span.end < made.call.span.end
                {
                    sites::drive_field(&r.docs, &made.call, &pin)
                } else {
                    pin_at
                };
                drives.push((pin.clone(), drive.clone()));
                ranges.push((
                    BenchKey::Stub(path.clone(), pin),
                    BenchSite {
                        at: pin_at,
                        origin: Some(made.clone()),
                    },
                ));
            }
            let raw = get(vm, "opts");
            let opts_at = sites::object(vm, &r.docs, raw).unwrap_or_else(|| at.clone());
            let opts = options(vm, &r.docs, raw, &["rail"], &opts_at, true)?;
            let rail_at = field_site(vm, &r.docs, raw, "rail", true, &opts_at);
            let rail = opts
                .iter()
                .find(|(key, _)| key == "rail")
                .map(|(_, value)| net(vm, r, *value).map_err(|m| (rail_at.clone(), m)))
                .transpose()?;
            let comp = &r.circuit().comps[comp];
            let supply_pin = ["VDD", "VCC", "VIO"].iter().find_map(|name| {
                comp.part.pins.iter().enumerate().find_map(|(i, pin)| {
                    (pin.name.eq_ignore_ascii_case(name)
                        && comp.conns.iter().any(|(p, n)| {
                            p.0 as usize == i && r.circuit().nets.iter().any(|net| net.id == *n)
                        }))
                    .then_some(pin.name.as_str())
                })
            });
            match (supply_pin, rail) {
                (None, None) => {
                    return Err(fault(format!(
                        "{path}: the part has no supply pin (VDD, VCC or VIO); give it a rail: {{rail: c.net(..)}}"
                    )));
                }
                (Some(pin), Some(_)) => {
                    return Err((
                        rail_at,
                        format!(
                            "{path}: rail is only for a part without a supply pin (it has {pin})"
                        ),
                    ));
                }
                _ => {}
            }
            r.stub_sites.push((path.clone(), at.clone()));
            r.stubs.push(Stub {
                path,
                pins: drives,
                rail,
            });
            r.sites.extend(ranges);
            return Ok(NIL);
        }
        "firmware" => {
            let raw = get(vm, "comp");
            let comp = component(vm, r, raw).map_err(fault)?;
            let comp = &r.circuit().comps[comp];
            let path = comp.path.clone();
            if comp.part.kind != PartKind::Mcu {
                return Err(fault(format!("{path}: firmware runs on an MCU part")));
            }
            if r.firmware.iter().any(|fw| fw.path == path) {
                return Err(fault(format!("{path}: component already runs firmware")));
            }
            if r.stubs.iter().any(|stub| stub.path == path) {
                return Err(fault(format!("{path}: component is stubbed; firmware drives its pins itself")));
            }
            let doc_at = sites::argument(&r.docs, at, 1)
                .map(|node| sites::range(at, &node)).unwrap_or_else(|| at.clone());
            let doc = read(vm, args, "doc");
            let Some(doc_name) = doc.text() else {
                return Err((doc_at, format!("firmware: expected a document name, got {}", doc.describe())));
            };
            if !r.firmware_docs.contains(doc_name) {
                return Err((doc_at, format!("firmware: no firmware document {doc_name} in the project")));
            }
            let raw = get(vm, "params");
            let mut params = Vec::new();
            if !raw.is_nil() {
                let params_at = sites::argument(&r.docs, at, 2)
                    .map(|node| sites::range(at, &node)).unwrap_or_else(|| at.clone());
                let entries = fields(vm, raw).map_err(|message| (params_at.clone(), format!("firmware: {message}")))?;
                for (key, value) in entries {
                    let value = Reader::new(vm).read(value);
                    let number = value.num();
                    if !number.is_some_and(f64::is_finite) {
                        let value_at = field_site(vm, &r.docs, raw, &key, true, &params_at);
                        let got = number.map_or_else(|| value.describe(), |n| n.to_string());
                        return Err((value_at, format!("firmware: {key}: expected a finite number, got {got}")));
                    }
                    let number = number.expect("a finite number was checked");
                    params.push((key, number));
                }
            }
            r.fw_sites.push((path.clone(), at.clone()));
            r.firmware.push(Firmware { path, doc: doc_name.to_owned(), params });
            return Ok(NIL);
        }
        "net" => {
            let endpoint = net_endpoint(vm, r, args, at)?;
            r.nets.push(endpoint);
            return Ok(NIL);
        }
        "supply" | "vsrc" | "isrc" | "pulse" | "load" => {
            stimulus(vm, r, name, args, at)?;
            return Ok(NIL);
        }
        "press" => {
            let raw = get(vm, "comp");
            let comp = component(vm, r, raw).map_err(fault)?;
            let comp = &r.circuit().comps[comp];
            if comp.part.kind != PartKind::Switch {
                return Err(fault(format!("{}: press expects a switch", comp.path)));
            }
            let path = comp.path.clone();
            r.pressed.push(path.clone());
            r.sites.push((BenchKey::Press(path), at.clone().into()));
            return Ok(NIL);
        }
        "turn" => {
            let raw = get(vm, "comp");
            let comp = component(vm, r, raw).map_err(fault)?;
            let comp = &r.circuit().comps[comp];
            if comp.part.kind != PartKind::Potentiometer {
                return Err(fault(format!("{}: turn expects a pot", comp.path)));
            }
            let pos = number(get(vm, "pos"), "pos").map_err(fault)?;
            if !(0.0..=1.0).contains(&pos) {
                return Err(fault(format!("{}: pos must be from 0 to 1", comp.path)));
            }
            let path = comp.path.clone();
            r.turned.retain(|(p, _)| *p != path);
            r.turned.push((path, pos));
            return Ok(NIL);
        }
        "v" => {
            let raw = get(vm, "a");
            let a = net(vm, r, raw).map_err(fault)?;
            let raw = get(vm, "b");
            let omitted = raw.is_nil() && sites::argument(&r.docs, at, 1).is_none();
            Value::Probe(if omitted {
                Probe::V(a)
            } else {
                Probe::VDiff(a, net(vm, r, raw).map_err(fault)?)
            })
        }
        "i" => {
            let raw = get(vm, "target");
            Value::Probe(Probe::I(pin(vm, r, raw).map_err(fault)?))
        }
        "p" => {
            let raw = get(vm, "comp");
            let i = component(vm, r, raw).map_err(fault)?;
            Value::Probe(Probe::P(CompRef {
                board: r.out.scope_id(),
                comp: r.circuit().comps[i].id,
            }))
        }
        "now" | "mean" | "rms" | "min" | "max" | "pp" => {
            let probe = probe(r, get(vm, "probe")).map_err(fault)?;
            let measure = match name {
                "now" => Measure::Now,
                "mean" => Measure::Mean,
                "rms" => Measure::Rms,
                "min" => Measure::Min,
                "max" => Measure::Max,
                _ => Measure::PeakToPeak,
            };
            let raw = get(vm, "when");
            let when_at = sites::object(vm, &r.docs, raw).unwrap_or_else(|| at.clone());
            let opts = options(
                vm,
                &r.docs,
                raw,
                &["window", "after", "from", "to"],
                &when_at,
                name == "now",
            )?;
            let has = |key| opts.iter().any(|(k, _)| k == key);
            let val = |key| -> Result<f64, Fault> {
                opts.iter()
                    .find(|(k, _)| k == key)
                    .map_or(Ok(0.0), |(_, v)| {
                        number(*v, key)
                            .map_err(|m| (field_site(vm, &r.docs, raw, key, true, &when_at), m))
                    })
            };
            let when = if has("from") || has("to") {
                if has("window") || has("after") {
                    return Err((when_at, "cannot mix span and trailing window fields".into()));
                }
                if !has("from") || !has("to") {
                    return Err((when_at, "a span requires both from and to".into()));
                }
                let (from, to) = (val("from")?, val("to")?);
                When::Span { from, to }
            } else {
                let (window, after) = (val("window")?, val("after")?);
                if !window.is_finite() || !after.is_finite() || window < 0.0 || after < 0.0 {
                    return Err((
                        when_at,
                        "window and after must be finite and nonnegative".into(),
                    ));
                }
                When::Trailing { window, after }
            };
            Value::Measure {
                probe,
                measure,
                when,
                at: when_at,
            }
        }
        "expect" => {
            expect(vm, r, args, at)?;
            return Ok(NIL);
        }
        _ => unreachable!("registered bench native"),
    };
    Ok(r.value(vm, value, at))
}

/// `b.net(comp, c, opts)`: connection `c` of the program on `comp` (which runs firmware): what the
/// twin plays for it (`fake`: "echo", "sink", `{files: "<folder>"}` or `{rules: [..]}`, under
/// `latency`, `jitter` and `rate` in bytes a second), and the real server it may reach (`url`), or a
/// service of this computer's AI hub (`hub`, with the chat `model` its turns ask for). A connection that sends a token names its
/// `endpoint`: the key the token is saved under in the person's own settings, never in the project;
/// its server is reached over TLS unless it is on this machine.
fn net_endpoint(vm: &mut ScriptVm, r: &mut Record, args: ScriptObject, at: &SourceRange) -> Result<fab_elec_bench::net::NetEndpoint, Fault> {
    use fab_elec_bench::net::{valid_model, valid_name, valid_path, Fake, NetEndpoint, Real, Server};
    let fault = |message: String| (at.clone(), format!("net: {message}"));
    let raw = arg(vm, args, lut_id("comp"));
    let comp = component(vm, r, raw).map_err(fault)?;
    let path = r.circuit().comps[comp].path.clone();
    if !r.firmware.iter().any(|fw| fw.path == path) {
        return Err(fault(format!("{path} runs no firmware: b.firmware(..) first")));
    }
    let c_at = sites::argument(&r.docs, at, 1).map(|node| sites::range(at, &node)).unwrap_or_else(|| at.clone());
    let c = read(vm, args, "c").num().filter(|c| c.fract() == 0.0 && (0.0..=255.0).contains(c))
        .ok_or_else(|| (c_at.clone(), "net: the connection is a whole number from 0 to 255 (the program's net declarations in order)".into()))? as u8;
    if r.nets.iter().any(|n| n.path == path && n.c == c) {
        return Err((c_at, format!("net: {path}'s connection {c} is already declared")));
    }
    let raw = arg(vm, args, lut_id("opts"));
    let opts_at = sites::object(vm, &r.docs, raw).unwrap_or_else(|| at.clone());
    let opts = options(vm, &r.docs, raw, &["endpoint", "url", "hub", "model", "fake", "latency", "jitter", "rate"], &opts_at, true)?;
    let written = sites::argument(&r.docs, at, 2);
    let site = |vm: &ScriptVm, key: &str| written.as_ref().and_then(|n| n.field(key)).map(|f| sites::range(at, &f.value))
        .unwrap_or_else(|| field_site(vm, &r.docs, raw, key, true, &opts_at));
    let mut endpoint = NetEndpoint { path, c, name: None, real: None, fake: Fake::Echo, latency: 0.0, jitter: 0.0, rate: 0.0 };
    let mut model = None;
    for (key, value) in &opts {
        let key_at = site(vm, key);
        let text = Reader::new(vm).read(*value).text().map(str::to_owned);
        match key.as_str() {
            "endpoint" => {
                let name = text.filter(|n| valid_name(n))
                    .ok_or_else(|| (key_at.clone(), "net: endpoint: a name of letters, digits, - and _".into()))?;
                endpoint.name = Some(name);
            }
            "url" | "hub" if endpoint.real.is_some() => {
                return Err((key_at, "net: url or hub: a connection reaches one real end".into()));
            }
            "url" => {
                let url = text.ok_or_else(|| (key_at.clone(), "net: url: expected a server address".into()))?;
                endpoint.real = Some(Real::Server(Server::parse(&url).map_err(|m| (key_at.clone(), format!("net: url: {m}")))?));
            }
            "hub" => {
                let service = text.filter(|n| valid_name(n))
                    .ok_or_else(|| (key_at.clone(), "net: hub: the name of a service of this computer's AI hub, like \"talk\"".into()))?;
                endpoint.real = Some(Real::Hub { service, model: None });
            }
            "model" => {
                let id = text.filter(|m| valid_model(m))
                    .ok_or_else(|| (key_at.clone(), "net: model: the id of a chat model, without spaces".into()))?;
                model = Some(id);
            }
            "fake" if value.as_object().is_some() => {
                let fake_at = sites::object(vm, &r.docs, *value).unwrap_or_else(|| key_at.clone());
                let fake = options(vm, &r.docs, *value, &["files", "rules"], &fake_at, false)?;
                let [(kind, inner)] = &fake[..] else {
                    return Err((fake_at, "net: fake: {files: \"<folder>\"} (the pictures in the project's firmware/<folder>/) or {rules: [..]}".into()));
                };
                let inner_at = field_site(vm, &r.docs, *value, kind, true, &fake_at);
                endpoint.fake = if kind == "files" {
                    let folder = Reader::new(vm).read(*inner).text().map(|f| f.trim_end_matches('/').to_owned()).filter(|f| valid_path(f))
                        .ok_or_else(|| (inner_at, "net: fake: files: a folder under the project's firmware/, like \"photos\"".into()))?;
                    Fake::Files(folder)
                } else {
                    Fake::Rules(net_rules(vm, r, *inner, &inner_at)?)
                };
            }
            "fake" => endpoint.fake = match text.as_deref() {
                Some("echo") => Fake::Echo,
                Some("sink") => Fake::Sink,
                _ => return Err((key_at, "net: fake: \"echo\", \"sink\", {files: \"<folder>\"} or {rules: [..]}".into())),
            },
            "latency" | "jitter" => {
                let node = written.as_ref().and_then(|n| n.field(key)).map(|f| &f.value);
                let seconds = option_number(r, *value, key, QtyUnit::Second, &key_at, node)?;
                if seconds < 0.0 { return Err((key_at, format!("net: {key}: must be nonnegative"))); }
                if key == "latency" { endpoint.latency = seconds } else { endpoint.jitter = seconds }
            }
            _ => {
                let rate = number(*value, "rate").ok().filter(|v| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| (key_at.clone(), "net: rate: bytes a second, nonnegative (0: no limit)".into()))?;
                endpoint.rate = rate;
            }
        }
    }
    if let Some(id) = model {
        let Some(Real::Hub { model, .. }) = &mut endpoint.real else {
            return Err((site(vm, "model"), "net: model: only a hub service's turns ask a chat model: add hub: \"talk\"".into()));
        };
        *model = Some(id);
    }
    if let (Some(_), Some(Real::Hub { .. })) = (&endpoint.name, &endpoint.real) {
        return Err((site(vm, "endpoint"), "net: endpoint: a hub service takes no token, so no endpoint name: Fab reaches this computer's hub as this computer".into()));
    }
    if let (Some(_), Some(Real::Server(server))) = (&endpoint.name, &endpoint.real) {
        if !server.carries_tokens() {
            return Err((site(vm, "url"), format!(
                "net: url: plaintext: a named endpoint's token goes only over wss:// or tls:// (or to this machine), not {}://",
                server.scheme.prefix())));
        }
    }
    Ok(endpoint)
}

/// A rules fake's table: a list of `{on: "<text>" | ready: true | binary: "<bytes>", whole, stop,
/// send: [..]}`. `on` answers a text message starting with its text (with `whole: true`, equal to
/// it), `binary` a binary message starting with its bytes; `stop: true` ends the sends under way
/// first and answers only if one was.
fn net_rules(vm: &mut ScriptVm, r: &Record, raw: ScriptValue, at: &SourceRange) -> Result<Vec<fab_elec_bench::net::RuleSpec>, Fault> {
    use fab_elec_bench::net::RuleSpec;
    use fab_elec_bench::net::When;
    let fault = |at: &SourceRange, message: &str| (at.clone(), format!("net: rules: {message}"));
    let items = list(vm, raw).ok_or_else(|| fault(at, "a list of rules, like [{on: \"hello \" send: [\"status ready\"]}]"))?;
    let mut rules = Vec::new();
    for item in items {
        let rule_at = sites::object(vm, &r.docs, item).unwrap_or_else(|| at.clone());
        joined(vm, item).map_err(|m| fault(&rule_at, m))?;
        let fields = options(vm, &r.docs, item, &["on", "ready", "binary", "whole", "stop", "send"], &rule_at, false)?;
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
        let site = |vm: &ScriptVm, key: &str| field_site(vm, &r.docs, item, key, true, &rule_at);
        let flag = |vm: &ScriptVm, key: &str| match get(key) {
            None => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| fault(&site(vm, key), &format!("{key}: true or false"))),
        };
        let text = |vm: &mut ScriptVm, key: &str| {
            let v = get(key).expect("a present key");
            Reader::new(vm).read(v).text().map(str::to_owned).ok_or_else(|| fault(&site(vm, key), &format!("{key}: a string")))
        };
        let when = match (get("on").is_some(), flag(vm, "ready")?, get("binary").is_some()) {
            (true, false, false) => When::Message { binary: false, bytes: text(vm, "on")?.into_bytes(), whole: flag(vm, "whole")? },
            (false, true, false) => When::Ready,
            (false, false, true) => When::Message { binary: true, bytes: text(vm, "binary")?.into_bytes(), whole: flag(vm, "whole")? },
            _ => return Err(fault(&rule_at, "a rule answers one thing: on: \"<text>\", ready: true or binary: \"<bytes>\"")),
        };
        let send = match get("send") {
            Some(v) => net_sends(vm, r, v, &site(vm, "send"))?,
            None => Vec::new(),
        };
        rules.push(RuleSpec { when, stop: flag(vm, "stop")?, send });
    }
    Ok(rules)
}

/// A rule's sends: each a text (a template, sent at once) or `{after, text | binary: [..], file |
/// wav (with rate), block, every}`. A binary head lists bytes (0 to 255) and templates; `file` and
/// `wav` name files under the project's firmware/; `block` cuts the body into messages of that many
/// units (samples of a wav, bytes of a file), `every` seconds apart.
fn net_sends(vm: &mut ScriptVm, r: &Record, raw: ScriptValue, at: &SourceRange) -> Result<Vec<fab_elec_bench::net::SendSpec>, Fault> {
    use fab_elec_bench::net::{template, valid_path, Body, SendSpec};
    use fab_elec_bench::net::Field;
    let fault = |at: &SourceRange, message: &str| (at.clone(), format!("net: send: {message}"));
    let items = list(vm, raw).ok_or_else(|| fault(at, "a list of messages, like [\"status ready\"]"))?;
    let mut sends = Vec::new();
    for item in items {
        let item_at = sites::object(vm, &r.docs, item).unwrap_or_else(|| at.clone());
        if let Some(line) = Reader::new(vm).read(item).text().map(str::to_owned) {
            let head = template(&line).map_err(|m| fault(&item_at, &m))?;
            sends.push(SendSpec { after: 0.0, binary: false, head, body: Body::None, block: 0, every: 0.0 });
            continue;
        }
        joined(vm, item).map_err(|m| fault(&item_at, m))?;
        let fields = options(vm, &r.docs, item, &["after", "text", "binary", "file", "wav", "rate", "block", "every"], &item_at, false)?;
        let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
        let site = |vm: &ScriptVm, key: &str| field_site(vm, &r.docs, item, key, true, &item_at);
        let seconds = |vm: &ScriptVm, key: &str| match get(key) {
            None => Ok(0.0),
            Some(v) => v.as_number().filter(|s| s.is_finite() && *s >= 0.0).ok_or_else(|| fault(&site(vm, key), &format!("{key}: seconds, nonnegative"))),
        };
        let whole = |vm: &ScriptVm, key: &str, min: f64| match get(key) {
            None => Ok(0),
            Some(v) => v.as_number().filter(|n| n.fract() == 0.0 && *n >= min && *n <= u32::MAX as f64).map(|n| n as usize)
                .ok_or_else(|| fault(&site(vm, key), &format!("{key}: a whole number, {min} at least"))),
        };
        let path = |vm: &mut ScriptVm, key: &str| {
            let v = get(key).expect("a present key");
            Reader::new(vm).read(v).text().map(str::to_owned).filter(|p| valid_path(p) && p.contains('/'))
                .ok_or_else(|| fault(&site(vm, key), &format!("{key}: a file under the project's firmware/, like \"voice/reply.wav\"")))
        };
        let (binary, head) = match (get("text"), get("binary")) {
            (Some(v), None) => {
                let line = Reader::new(vm).read(v).text().map(str::to_owned).ok_or_else(|| fault(&site(vm, "text"), "text: a string"))?;
                (false, template(&line).map_err(|m| fault(&site(vm, "text"), &m))?)
            }
            (None, Some(v)) => {
                let head_at = site(vm, "binary");
                let parts = list(vm, v).ok_or_else(|| fault(&head_at, "binary: a list of bytes and templates, like [\"A\" 0 \"{1:u16}\"]"))?;
                let mut head = Vec::new();
                for part in parts {
                    if let Some(n) = part.as_number() {
                        let byte = Some(n).filter(|n| n.fract() == 0.0 && (0.0..=255.0).contains(n))
                            .ok_or_else(|| fault(&head_at, &format!("binary: {n} is not a byte (0 to 255)")))?;
                        head.push(Field::Bytes(vec![byte as u8]));
                    } else {
                        let line = Reader::new(vm).read(part).text().map(str::to_owned)
                            .ok_or_else(|| fault(&head_at, "binary: bytes (0 to 255) and templates (strings)"))?;
                        head.extend(template(&line).map_err(|m| fault(&head_at, &m))?);
                    }
                }
                (true, head)
            }
            _ => return Err(fault(&item_at, "a message is text: \"..\" or binary: [..]")),
        };
        let body = match (get("file").is_some(), get("wav").is_some()) {
            (false, false) => Body::None,
            (true, false) => Body::File(path(vm, "file")?),
            (false, true) => {
                let rate = whole(vm, "rate", 1.0)?;
                if rate == 0 { return Err(fault(&item_at, "wav: its rate in samples a second, like rate: 16000")); }
                Body::Wav { file: path(vm, "wav")?, rate: rate as u32 }
            }
            (true, true) => return Err(fault(&item_at, "a message carries one file: file or wav")),
        };
        let block = whole(vm, "block", 0.0)?;
        if block > 0 && body == Body::None { return Err(fault(&site(vm, "block"), "block: cuts a file or wav into messages; this message has none")); }
        sends.push(SendSpec { after: seconds(vm, "after")?, binary, head, body, block, every: seconds(vm, "every")? });
    }
    Ok(sends)
}

/// Refuses a list item built on the item before it: in a list, a value followed by `{..}` without a
/// comma between them is one item, the object derived from that value, and the earlier item is lost.
fn joined(vm: &ScriptVm, item: ScriptValue) -> Result<(), &'static str> {
    match item.as_object() {
        // a literal's base is an id; a joined item's is the value before it
        Some(object) if { let base = vm.bx.heap_ref().proto(object); base.as_object().is_some() || base.is_string_like() } =>
            Err("this item joined the one before it (a value followed by {..} in a list is one item): separate the items with commas"),
        _ => Ok(()),
    }
}

/// A list's items (None: not a list).
fn list(vm: &ScriptVm, raw: ScriptValue) -> Option<Vec<ScriptValue>> {
    let array = raw.as_array()?;
    Some((0..vm.bx.heap_ref().array_len(array)).map(|i| vm.bx.heap_ref().array_index(array, i, NoTrap)).collect())
}

/// A bench pulse's edge when the bench gives none: fast beside any audio or logic timing a
/// bench measures, slow enough that the solver's steps over it stay few.
const PULSE_EDGE: f64 = 1e-6;

/// The unit of a supply's or source's numeric option (`call` supply, vsrc, isrc, pulse or load),
/// or None when the call has no such option.
pub fn stimulus_unit(call: &str, key: &str) -> Option<QtyUnit> {
    match (call, key) {
        ("supply" | "pulse", "v") | ("pulse", "low") | ("vsrc", "dc") => Some(QtyUnit::Volt),
        ("supply", "limit") | ("isrc", "dc") | ("load", "i") => Some(QtyUnit::Ampere),
        ("supply", "ramp") | ("pulse", "width" | "every" | "at" | "edge")
        | ("load", "at" | "rise") => Some(QtyUnit::Second),
        ("load", "r") => Some(QtyUnit::Ohm),
        _ => None,
    }
}

pub(crate) fn check_terminals(circuit: &Circuit, a: NetId, b: Option<NetId>) -> Result<(), String> {
    match b {
        None if circuit.nets.iter().any(|n| n.id == a && n.kind == NetKind::Ground) => {
            Err("cannot supply the ground net".into())
        }
        Some(b) if a == b => Err("source or load terminals must be different nets".into()),
        _ => Ok(()),
    }
}

fn stimulus(
    vm: &mut ScriptVm,
    r: &mut Record,
    name: &str,
    args: ScriptObject,
    at: &SourceRange,
) -> Result<(), Fault> {
    let fault = |message| (at.clone(), message);
    let a = arg(vm, args, lut_id(if name == "supply" { "net" } else { "a" }));
    let a = net(vm, r, a).map_err(fault)?;
    let b = if name == "supply" {
        a
    } else {
        let b = arg(vm, args, id_lut!(b));
        net(vm, r, b).map_err(fault)?
    };
    let raw = arg(vm, args, id_lut!(opts));
    let opts_at = sites::object(vm, &r.docs, raw).unwrap_or_else(|| at.clone());
    let allowed: &[&str] = match name {
        "supply" => &["v", "limit", "ramp"],
        "load" => &["r", "i", "step", "at", "rise"],
        "pulse" => &["v", "width", "every", "at", "low", "edge"],
        _ => &["dc"],
    };
    let opts = options(vm, &r.docs, raw, allowed, &opts_at, false)?;
    let written = sites::argument(&r.docs, at, if name == "supply" { 1 } else { 2 });
    let has = |key: &str| opts.iter().any(|(k, _)| k == key);
    let val = |key: &str| -> Result<Option<f64>, Fault> {
        opts.iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| {
                let node = written
                    .as_ref()
                    .and_then(|n| n.field(key))
                    .map(|f| &f.value);
                let at = node.map_or_else(
                    || field_site(vm, &r.docs, raw, key, true, &opts_at),
                    |node| sites::range(at, node),
                );
                let unit = stimulus_unit(name, key).expect("numeric stimulus option");
                option_number(r, *value, key, unit, &at, node)
            })
            .transpose()
    };
    let site = || BenchSite { at: at.clone(), origin: Some(sites::origin(vm, &r.docs, at)) };
    check_terminals(r.circuit(), a.net, (name != "supply").then_some(b.net)).map_err(fault)?;
    if name == "supply" {
        if r.supplies.iter().any(|s| s.net == a) {
            return Err(fault(
                "a second supply on the same net is not allowed".into(),
            ));
        }
        let supply = Supply {
            net: a,
            v: val("v")?.ok_or_else(|| (opts_at.clone(), "missing required option v".into()))?,
            limit: val("limit")?,
            ramp: val("ramp")?,
        };
        let key = BenchKey::Supply(r.net_name(a).map_err(fault)?);
        r.stimulus_sites.push((key, site(), Stimulus::Supply(supply.clone())));
        r.supplies.push(supply);
        return Ok(());
    }
    let step_currents = || {
        let step_at = field_site(vm, &r.docs, raw, "step", true, &opts_at);
        let step = opts
            .iter()
            .find(|(k, _)| k == "step")
            .expect("step option")
            .1;
        let array = step
            .as_array()
            .filter(|a| vm.bx.heap_ref().array_len(*a) == 2)
            .ok_or_else(|| {
                (
                    step_at.clone(),
                    "step: expected two currents [from to]".into(),
                )
            })?;
        let syntax = written.as_ref().and_then(|n| n.field("step"));
        let current = |i| {
            let node = syntax
                .and_then(|f| f.value.items())
                .and_then(|items| items.get(i));
            let at = node.map_or_else(|| step_at.clone(), |n| sites::range(&step_at, n));
            let value = vm.bx.heap_ref().array_index(array, i, NoTrap);
            option_number(r, value, "step", QtyUnit::Ampere, &at, node)
        };
        Ok((current(0)?, current(1)?))
    };
    let kind = source_kind(name, has, val, step_currents, |key, message| {
        let at = key.map_or_else(
            || opts_at.clone(),
            |key| field_site(vm, &r.docs, raw, key, true, &opts_at),
        );
        (at, message)
    })?;
    let key = BenchKey::Source(r.net_name(a).map_err(fault)?, r.net_name(b).map_err(fault)?);
    r.stimulus_sites.push((key, site(), Stimulus::Source(kind.clone())));
    r.sources.push(BenchSource { a, b, kind });
    Ok(())
}

/// Construct a source from its options, with the caller providing values and error locations.
pub(crate) fn source_kind<E>(
    name: &str,
    has: impl Fn(&str) -> bool,
    val: impl Fn(&str) -> Result<Option<f64>, E>,
    step: impl FnOnce() -> Result<(f64, f64), E>,
    error: impl Fn(Option<&str>, String) -> E,
) -> Result<SourceKind, E> {
    let required = |key| val(key)?.ok_or_else(|| error(None, format!("missing required option {key}")));
    Ok(match name {
        "vsrc" => SourceKind::Vdc(required("dc")?),
        "isrc" => SourceKind::Idc(required("dc")?),
        // A pulse train (or one pulse, without `every`): `low` until `at`, then `v` for `width`,
        // each edge `edge` long (1 us unless given), again every `every`.
        "pulse" => SourceKind::Pulse {
            low: val("low")?.unwrap_or(0.0),
            high: required("v")?,
            at: val("at")?.unwrap_or(0.0),
            width: required("width")?,
            every: val("every")?,
            edge: val("edge")?.unwrap_or(PULSE_EDGE),
        },
        "load" => {
            if ["r", "i", "step"].iter().filter(|key| has(key)).count() != 1 {
                return Err(error(None, "load requires exactly one of r, i or step".into()));
            }
            if has("step") {
                let (from, to) = step()?;
                SourceKind::LoadStep {
                    from, to,
                    at: val("at")?.unwrap_or(0.0),
                    // The simulator ramps every step: a rise of 0 s has no defined edge.
                    rise: required("rise")?,
                }
            } else {
                for key in ["at", "rise"] {
                    if has(key) {
                        return Err(error(Some(key), format!("{key} is only allowed with step")));
                    }
                }
                if has("r") {
                    SourceKind::LoadR(required("r")?)
                } else {
                    SourceKind::LoadI(required("i")?)
                }
            }
        }
        _ => return Err(error(None, format!("unknown source kind {name:?} (expected vsrc, isrc, pulse or load)"))),
    })
}

fn option_number(
    r: &Record,
    raw: ScriptValue,
    key: &str,
    expected: QtyUnit,
    at: &SourceRange,
    node: Option<&Node>,
) -> Result<f64, Fault> {
    let value = number(raw, key).map_err(|m| (at.clone(), m))?;
    // An expression or a variable has no declared dimension after evaluation.
    if let Some(unit) = node.and_then(|node| literal_unit(&r.docs, at, node)) {
        if unit != expected {
            let unit = match expected {
                QtyUnit::Volt => "V",
                QtyUnit::Ampere => "A",
                QtyUnit::Second => "s",
                QtyUnit::Ohm => "ohm",
                _ => unreachable!("stimulus unit"),
            };
            return Err((
                at.clone(),
                format!("{key}: expected {unit} for this option"),
            ));
        }
    }
    check_option_number(key, value).map_err(|m| (at.clone(), m))?;
    Ok(value)
}

pub(crate) fn check_option_number(key: &str, value: f64) -> Result<(), String> {
    let problem = match key {
        "limit" | "r" | "rise" if !value.is_finite() || value <= 0.0 => {
            Some("must be finite and greater than 0")
        }
        "ramp" | "at" if !value.is_finite() || value < 0.0 => {
            Some("must be finite and nonnegative")
        }
        _ if !value.is_finite() => Some("must be finite"),
        _ => None,
    };
    if let Some(problem) = problem {
        return Err(format!("{key} {problem}"));
    }
    Ok(())
}

fn expect(
    vm: &mut ScriptVm,
    r: &mut Record,
    args: ScriptObject,
    at: &SourceRange,
) -> Result<(), Fault> {
    let name = read(vm, args, "name");
    let name = name
        .text()
        .ok_or_else(|| (at.clone(), "expected an expectation name string".into()))?
        .to_string();
    if r.out.expects.iter().any(|e| e.name == name) {
        return Err((at.clone(), format!("duplicate expectation name {name:?}")));
    }
    let raw = arg(vm, args, id_lut!(measure));
    let Some((
        Value::Measure {
            probe,
            measure,
            when,
            at: when_at,
        },
        _,
    )) = r.get(raw).cloned()
    else {
        return Err((at.clone(), "expected a measure".into()));
    };
    if let When::Span { from, to } = when {
        if !from.is_finite() || !to.is_finite() || to <= from {
            return Err((
                when_at,
                format!("span from {from} to {to} is empty or not finite"),
            ));
        }
    }
    let bounds = arg(vm, args, id_lut!(bounds));
    let opts = options(vm, &r.docs, bounds, &["min", "max"], at, false)?;
    let (mut min, mut max) = (f64::NEG_INFINITY, f64::INFINITY);
    for (key, raw) in opts {
        let bound_at = field_site(vm, &r.docs, bounds, &key, true, at);
        let value = number(raw, &key).map_err(|m| (bound_at.clone(), m))?;
        if value.is_nan() {
            return Err((bound_at, format!("{key} must not be NaN")));
        }
        // Only a literal written at this call has a declared dimension. Computed values
        // (including a variable holding a quantity) are plain SI numbers in the VM.
        if let Some(object) = sites::argument(&r.docs, at, 2) {
            if let Some(field) = object.field(&key) {
                if let Some(unit) =
                    literal_unit(&r.docs, &sites::range(at, &field.value), &field.value)
                {
                    let expected = match probe.unit() {
                        "V" => QtyUnit::Volt,
                        "A" => QtyUnit::Ampere,
                        _ => QtyUnit::Watt,
                    };
                    if unit != expected {
                        return Err((
                            sites::range(at, &field.value),
                            format!("{key}: expected {} bounds for this probe", probe.unit()),
                        ));
                    }
                }
            }
        }
        if key == "min" {
            min = value
        } else {
            max = value
        }
    }
    if min > max {
        return Err((at.clone(), "expectation min is greater than max".into()));
    }
    r.out.expects.push(Expect {
        name: name.clone(),
        probe,
        measure,
        when,
        bound: Bound::Range { min, max },
    });
    r.sites.push((
        BenchKey::Expect(name),
        BenchSite {
            at: at.clone(),
            origin: Some(sites::origin(vm, &r.docs, at)),
        },
    ));
    Ok(())
}

fn literal_unit(docs: &Shared, at: &SourceRange, node: &Node) -> Option<QtyUnit> {
    match &node.kind {
        Kind::Unary { op, operand } if op == "-" || op == "+" => {
            literal_unit(docs, &sites::range(at, operand), operand)
        }
        Kind::Number(_) => {
            let docs = docs.borrow();
            let syntax = &docs.docs.iter().find(|d| d.id == at.doc)?.names()?.syntax;
            let text = syntax.text(node);
            crate::units::circuit_units()
                .entries()
                .iter()
                .find_map(|entry| {
                    let digits = text.strip_suffix(entry.suffix.as_str())?;
                    digits.replace('_', "").parse::<f64>().ok()?;
                    crate::units::qty_of(&entry.suffix).map(|(unit, _, _)| unit)
                })
        }
        _ => None,
    }
}

/// The same tagged component, pin and net representation read by the circuit natives.
fn handle(vm: &mut ScriptVm, tag: LiveId, index: usize) -> ScriptObject {
    let build = field(vm, vm.bx.heap_ref().module(id_lut!(elec)), id_lut!(__build));
    let heap = vm.bx.heap_mut();
    let object = heap.new_object();
    heap.set_value_def(object, id_lut!(__proto).into(), ScriptValue::from_id(tag));
    heap.set_value_def(object, id_lut!(ix).into(), (index as f64).into());
    if [id_lut!(Comp), id_lut!(Pin), id_lut!(Net), id_lut!(Bus), id_lut!(Inst)].contains(&tag) {
        heap.set_value_def(object, id_lut!(build).into(), build);
    }
    object
}

fn circuit_object(vm: &mut ScriptVm, bench: &Bench) -> ScriptObject {
    let root = vm.bx.heap_mut().new_object();
    bench.borrow_mut().roots.push(vm.new_object_ref(root));
    {
        let r = bench.borrow();
        for (i, comp) in r.circuit().comps.iter().enumerate() {
            let leaf = handle(vm, id_lut!(Comp), i);
            for (p, pin) in comp.part.pins.iter().enumerate() {
                let h = handle(vm, id_lut!(Pin), i);
                vm.bx
                    .heap_mut()
                    .set_value_def(h, id_lut!(pin).into(), (p as f64).into());
                vm.bx
                    .heap_mut()
                    .set_value_def(leaf, lut_id(&pin.name).into(), h.into());
            }
            insert_path(vm, root.into(), &path_parts(&comp.path), leaf.into());
        }
    }
    for name in ["net", "bus"] {
        let bench = bench.clone();
        add(vm, root, name, &["name"], move |vm, args| {
            let mut r = bench.borrow_mut();
            let at = sites::call(vm, &r.docs);
            let value = read(vm, args, "name");
            let Some(key) = value.text() else {
                return r.fail(&at, format!("{name} expects a name string"));
            };
            if name == "net" {
                match r.circuit().nets.iter().position(|n| n.name == key) {
                    Some(i) => handle(vm, id_lut!(Net), i).into(),
                    None => r.fail(&at, format!("no net named {key:?}")),
                }
            } else {
                let Some(bus) = r.circuit().buses.iter().find(|b| b.name == key) else {
                    return r.fail(&at, format!("no bus named {key:?}"));
                };
                let members: Vec<_> = bus
                    .members
                    .iter()
                    .map(|(_, n)| {
                        r.circuit()
                            .nets
                            .iter()
                            .position(|net| net.id == *n)
                            .expect("bus member net")
                    })
                    .collect();
                let array = vm.bx.heap_mut().new_array();
                for i in members {
                    let net = handle(vm, id_lut!(Net), i);
                    vm.bx.heap_mut().array_push(array, net.into(), NoTrap);
                }
                array.into()
            }
        });
    }
    root
}

enum PathPart {
    Field(String),
    Index(usize),
}

fn path_parts(path: &str) -> Vec<PathPart> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let mut rest = part;
        if let Some(at) = rest.find('[') {
            out.push(PathPart::Field(rest[..at].into()));
            rest = &rest[at..];
            while let Some(after) = rest.strip_prefix('[') {
                let Some((index, tail)) = after.split_once(']') else {
                    break;
                };
                let Ok(index) = index.parse() else { break };
                out.push(PathPart::Index(index));
                rest = tail;
            }
        } else {
            out.push(PathPart::Field(rest.into()));
        }
    }
    out
}

fn insert_path(vm: &mut ScriptVm, parent: ScriptValue, path: &[PathPart], leaf: ScriptValue) {
    let Some((key, rest)) = path.split_first() else {
        return;
    };
    let get = |vm: &ScriptVm| match key {
        PathPart::Field(name) => field(vm, parent.as_object().expect("path object"), lut_id(name)),
        PathPart::Index(i) => {
            // A slot past the end is not made yet; reading it would be an out-of-bounds error value.
            let array = parent.as_array().expect("path array");
            let heap = vm.bx.heap_ref();
            if *i < heap.array_len(array) { heap.array_index(array, *i, NoTrap) } else { NIL }
        }
    };
    let child = if rest.is_empty() {
        leaf
    } else {
        let old = get(vm);
        if !old.is_nil() {
            old
        } else {
            match rest[0] {
                PathPart::Field(_) => vm.bx.heap_mut().new_object().into(),
                PathPart::Index(_) => vm.bx.heap_mut().new_array().into(),
            }
        }
    };
    match key {
        PathPart::Field(name) => {
            vm.bx.heap_mut().set_value_def(
                parent.as_object().expect("path object"),
                lut_id(name).into(),
                child,
            );
        }
        PathPart::Index(i) => {
            vm.bx.heap_mut().set_array_index(
                parent.as_array().expect("path array"),
                *i,
                child,
                NoTrap,
            );
        }
    }
    if !rest.is_empty() {
        insert_path(vm, child, rest, leaf)
    }
}
