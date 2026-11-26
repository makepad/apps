//! A module isolate: library modules loaded into a VM of their own, whose functions the host
//! calls by name with plain values and gets a plain value back.
//!
//! The host evaluates a document in one VM and lowers it, and that VM is dropped; a function the
//! document made cannot be called again afterwards. A host that wants to ask a library's own rule
//! a question later (a case generator's light-pipe plan, of a finished arrangement) loads the
//! library here instead: one fresh VM holding the language's value types and `math`, the
//! libraries evaluated in it, and no document, no builder and no host input. Nothing is written as
//! source: the arguments are [`V`]s copied into the isolate's heap, the result is read back into a
//! [`V`] with [`Reader`], and no handle to the isolate's heap ever leaves it.
//!
//! One isolate takes any number of calls. It is not `Send`: build it on the thread that calls it
//! (a worker, never the UI thread) and drop it there. A function that builds parts (`m.box`) has
//! no builder to call here: its error comes back located in the library's source, as any
//! evaluation error does.

use super::protos::PROTOS;
use super::vm::{diag_of, lut_id, Obj, EVAL_LIMIT};
use super::{Diag, Library, Reader, V};
use makepad_script::*;
use std::sync::Arc;

/// Library modules in a VM of their own: [`ModuleIsolate::call`] runs one of their functions.
pub struct ModuleIsolate {
    // Drop order: the root before the heap it points into, the heap before its host.
    module: ScriptObjectRef,
    bx: Option<Box<ScriptVmBase>>,
    #[allow(dead_code)]
    host: Box<ScriptVmHost<(), ()>>,
}

impl ModuleIsolate {
    /// Evaluate `libraries` in order in a fresh VM. Each ends with an object of its functions,
    /// and the fields of all of them are what [`ModuleIsolate::call`] names (a later library
    /// replaces an earlier one's field of the same name). An error is located in the library's
    /// source ([`Diag::message`] starts with `file:line:col`).
    pub fn load(libraries: &[Library]) -> Result<Self, Vec<Diag>> {
        let mut host = Box::new(ScriptVmHost::new((), ()));
        let mut vm = ScriptVm { host: host.host_context(), bx: Box::new(ScriptVmBase::new()) };
        vm.set_script_domain("model");
        // The value types a library's functions write (`Rect{..}`), as a document sees them.
        let model = vm.new_module(id!(model));
        for p in PROTOS.iter().filter(|p| !p.is_solid()) {
            let proto = vm.bx.heap_mut().new_object();
            let tag = lut_id(p.name);
            vm.bx.heap_mut().set_value_def(proto, id_lut!(__proto).into(), ScriptValue::from_id(tag));
            vm.bx.heap_mut().set_value_def(model, tag.into(), proto.into());
        }
        vm.capture_errors();
        let module = vm.bx.heap_mut().new_object();
        let module = vm.new_object_ref(module);
        let mut errors = Vec::new();
        for lib in libraries {
            vm.capture_errors();
            let value = vm.with_instruction_limit(EVAL_LIMIT as u64, |vm| vm.eval(ScriptMod::new("", "", &lib.file, 1, 0, &lib.source, &[])));
            errors.extend(vm.take_errors().iter().map(|e| diag_of(e)));
            if !errors.is_empty() {
                break;
            }
            let Some(o) = value.as_object() else {
                errors.push(Diag { line: 1, col: 1, message: format!("library {} must end with an object of its fns", lib.file) });
                break;
            };
            let fields: Vec<_> = vm.bx.heap_ref().map_entries(o).iter().map(|e| (e.key, e.value)).collect();
            for (key, value) in fields {
                vm.bx.heap_mut().set_value_def(module.as_object(), key, value);
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let bx = Some(vm.bx);
        Ok(ModuleIsolate { module, bx, host })
    }

    /// Whether the libraries gave a function named `name`.
    pub fn has(&mut self, name: &str) -> bool {
        self.with_vm(|vm, module| vm.bx.heap_ref().value(module, lut_id(name).into(), NoTrap).as_object().is_some_and(|f| vm.bx.heap_ref().is_fn(f)))
    }

    /// Evaluate `source`, a Splash value written as data (the text a host gives its inputs), in the
    /// isolate and read it out as a plain value. `file` names it in an error.
    pub fn value(&mut self, file: &str, source: &str) -> Result<V, Vec<Diag>> {
        self.with_vm(|vm, _| {
            vm.capture_errors();
            let out = vm.with_instruction_limit(EVAL_LIMIT as u64, |vm| vm.eval(ScriptMod::new("", "", file, 1, 0, source, &[])));
            let errors: Vec<Diag> = vm.take_errors().iter().map(|e| diag_of(e)).collect();
            if !errors.is_empty() {
                return Err(errors);
            }
            let got = Reader::new(vm, Vec::new()).read(out);
            plain(&got).map(|()| got).map_err(|why| vec![Diag { line: 1, col: 1, message: format!("{file}: the value {why}") }])
        })
    }

    /// Call the function `name` with `args` copied into the isolate and read its result out.
    /// Arguments and result are plain values: nil, flags, numbers, text, ids, colours, lists and
    /// records. An argument that is a fn, an object with a prototype or something the reader
    /// cannot give plainly is refused, and so is such a result. Runs under the evaluation's
    /// instruction budget ([`EVAL_LIMIT`]), per call.
    pub fn call(&mut self, name: &str, args: &[V]) -> Result<V, Vec<Diag>> {
        self.run(name, |vm| {
            let mut values = Vec::with_capacity(args.len());
            for (i, arg) in args.iter().enumerate() {
                values.push(to_script(vm, arg).map_err(|why| format!("argument {} {why}", i + 1))?);
            }
            Ok(Args::Positional(values))
        })
    }

    /// [`ModuleIsolate::call`] with the arguments by the function's parameter names: the fields of
    /// `args`, a record (a parameter it has no field for is nil). What a value that names its own
    /// call (a function and the arguments it was made with) is called by.
    pub fn call_named(&mut self, name: &str, args: &V) -> Result<V, Vec<Diag>> {
        self.run(name, |vm| {
            let V::Obj(record) = args else { return Err("the arguments are a record of the parameters' names".to_string()) };
            if record.proto.is_some() || !record.items.is_empty() {
                return Err("the arguments are a plain record of the parameters' names".to_string());
            }
            let object = vm.bx.heap_mut().new_object();
            for (key, value) in &record.fields {
                let value = to_script(vm, value).map_err(|why| format!("argument '{key}' {why}"))?;
                vm.bx.heap_mut().set_value_def(object, lut_id(key).into(), value);
            }
            Ok(Args::Named(object))
        })
    }

    fn run(&mut self, name: &str, args: impl FnOnce(&mut ScriptVm) -> Result<Args, String>) -> Result<V, Vec<Diag>> {
        self.with_vm(|vm, module| {
            let func = vm.bx.heap_ref().value(module, lut_id(name).into(), NoTrap);
            if !func.as_object().is_some_and(|f| vm.bx.heap_ref().is_fn(f)) {
                let mut have: Vec<String> = vm.bx.heap_ref().map_entries(module).iter().filter(|e| e.value.as_object().is_some_and(|f| vm.bx.heap_ref().is_fn(f))).map(|e| e.key.as_id().and_then(|id| id.as_string(|s| s.map(str::to_string))).unwrap_or_default()).collect();
                have.sort();
                let have = if have.is_empty() { "there is none".to_string() } else { format!("there is: {}", have.join(", ")) };
                return Err(vec![Diag { line: 1, col: 1, message: format!("no fn named '{name}' in the loaded libraries ({have})") }]);
            }
            let args = args(vm).map_err(|why| vec![Diag { line: 1, col: 1, message: format!("{name}: {why}") }])?;
            // Capturing ends when its errors are taken: start it again for this call.
            vm.capture_errors();
            let out = vm.with_instruction_limit(EVAL_LIMIT as u64, |vm| match &args {
                Args::Positional(values) => vm.call(func, values),
                Args::Named(object) => vm.call_with_args_object(func, *object),
            });
            let mut errors: Vec<Diag> = vm.take_errors().iter().map(|e| diag_of(e)).collect();
            if errors.is_empty() && out.is_err() {
                errors.push(Diag { line: 1, col: 1, message: format!("{name} failed") });
            }
            if !errors.is_empty() {
                return Err(errors);
            }
            let got = Reader::new(vm, Vec::new()).read(out);
            match plain(&got) {
                Ok(()) => Ok(got),
                Err(why) => Err(vec![Diag { line: 1, col: 1, message: format!("{name}: the result {why}") }]),
            }
        })
    }

    fn with_vm<R>(&mut self, f: impl FnOnce(&mut ScriptVm, ScriptObject) -> R) -> R {
        let bx = self.bx.take().expect("the isolate's VM is in use");
        let mut vm = ScriptVm { host: self.host.host_context(), bx };
        let out = f(&mut vm, self.module.as_object());
        self.bx = Some(vm.bx);
        out
    }
}

enum Args {
    Positional(Vec<ScriptValue>),
    Named(ScriptObject),
}

/// `v` as a value of the VM: its heap holds a copy, nothing of `v`'s own.
fn to_script(vm: &mut ScriptVm, v: &V) -> Result<ScriptValue, String> {
    Ok(match v {
        V::Nil => NIL,
        V::Bool(b) => ScriptValue::from_bool(*b),
        V::Num(n) => (*n).into(),
        V::Str(s) => vm.bx.heap_mut().new_string_from_str(s),
        V::Id(s) => ScriptValue::from_escaped_id(lut_id(s)),
        V::Color(c) => ScriptValue::from_color(*c),
        V::Arr(items) => {
            let array = vm.bx.heap_mut().new_array();
            for item in items {
                let item = to_script(vm, item)?;
                vm.bx.heap_mut().array_push_unchecked(array, item);
            }
            array.into()
        }
        V::Obj(o) => {
            if o.proto.is_some() || o.part.is_some() {
                return Err("is an object with a prototype, not a plain record".into());
            }
            if !o.items.is_empty() {
                if !o.fields.is_empty() {
                    return Err("is an object with both fields and keyless entries".into());
                }
                return to_script(vm, &V::Arr(o.items.clone()));
            }
            let object = vm.bx.heap_mut().new_object();
            for (key, value) in &o.fields {
                let value = to_script(vm, value).map_err(|why| format!("field '{key}' {why}"))?;
                vm.bx.heap_mut().set_value_def(object, lut_id(key).into(), value);
            }
            object.into()
        }
        V::Fn(_) => return Err("is a fn, which cannot cross into the isolate".into()),
        V::Other(what) => return Err(format!("is {what}, not a plain value")),
    })
}

/// Whether `v` holds nothing of a VM's heap.
pub(super) fn plain(v: &V) -> Result<(), String> {
    match v {
        V::Fn(_) => Err("holds a fn, which cannot leave the isolate".into()),
        V::Other(what) => Err(format!("holds {what}, not a plain value")),
        V::Arr(items) => items.iter().try_for_each(plain),
        V::Obj(o) => o.fields.iter().map(|(_, v)| v).chain(o.items.iter()).try_for_each(plain),
        _ => Ok(()),
    }
}

impl V {
    /// A record of plain fields, in the order given: what a library function takes as `{k: v ..}`.
    pub fn record(fields: Vec<(&str, V)>) -> V {
        V::Obj(Arc::new(Obj { ident: 0, part: None, proto: None, fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(), items: Vec::new(), children: Vec::new(), loc: None }))
    }
}
