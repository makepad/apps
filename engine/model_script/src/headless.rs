//! A model program evaluated on its own (an editable model document): a fresh VM with
//! `game.model_new`, the programs its `m.build()` posts captured instead of queued, and
//! what the builder said. No game loop and no app.

use crate::model_verbs::{capture_programs, v_model_new, ScriptModelRequest};
use crate::ModelHost;
use makepad_script::*;
use std::cell::RefCell;
use std::rc::Rc;

/// What one evaluation made.
pub struct Evaluated {
    /// Every program the source built, in order.
    pub programs: Vec<ScriptModelRequest>,
    /// The builder's messages (unknown options, warnings).
    pub messages: Vec<String>,
}

struct Collect { messages: RefCell<Vec<String>>, errors: RefCell<Vec<String>> }
impl ModelHost for Collect {
    fn log(&self, message: String) { self.messages.borrow_mut().push(message); }
    fn error(&self, message: String) { self.errors.borrow_mut().push(message); }
}

/// Instructions one evaluation may run.
const EVAL_LIMIT: usize = 50_000_000;

/// Evaluate `source` (`let game = mod.game` / `let m = game.model_new(..)` ... `m.build()`).
/// Errors are the VM's (`file:line:col: message`).
pub fn evaluate(file: &str, source: &str) -> Result<Evaluated, Vec<String>> {
    let mut host_box = Box::new(ScriptVmHost::new((), ()));
    let mut vm = ScriptVm { host: host_box.host_context(), bx: Box::new(ScriptVmBase::new()) };
    let module = vm.new_module(id!(game));
    let log = Rc::new(Collect { messages: RefCell::new(Vec::new()), errors: RefCell::new(Vec::new()) });
    let host: Rc<dyn ModelHost> = log.clone();
    vm.add_method(module, id!(model_new), &[], move |vm, args| v_model_new(vm, &host, args));
    vm.capture_errors();
    let ((), programs) = capture_programs(|| {
        vm.with_instruction_limit(EVAL_LIMIT as u64, |vm| {
            let math = vm.module(id!(math));
            let pod = vm.module(id!(pod));
            vm.eval_module(ScriptMod::new("", "", file, 1, 0, source, &[]), &[math, pod]);
        });
    });
    let mut errors: Vec<String> = vm.take_errors().iter().map(|e| e.to_string()).collect();
    // A builder call that did nothing is an error of the program here.
    errors.extend(log.errors.borrow().iter().cloned());
    drop(vm);
    drop(host_box);
    if !errors.is_empty() {
        return Err(errors);
    }
    let messages = log.messages.borrow().clone();
    Ok(Evaluated { programs, messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_captured_not_queued() {
        let src = "use mod.pod.*\nlet game = mod.game\nlet m = game.model_new(\"crate\")\nlet body = m.box({size: vec3(1, 1, 1)})\nlet lid = m.box({size: vec3(1, 0.1, 1) pos: vec3(0, 0.55, 0)})\nm.build()\n";
        let out = evaluate("crate.splash", src).unwrap();
        assert_eq!(out.programs.len(), 1);
        assert_eq!(out.programs[0].alias, "gen/mdl/crate");
        assert!(crate::model_verbs::take_script_model_requests().iter().all(|r| r.alias != "gen/mdl/crate"));
        let built = makepad_model::build_program(&out.programs[0].program, makepad_model::Limits::default(), &mut |_| Err("no imports".into()), None).unwrap();
        assert_eq!(built.document.objects().count(), 2);
        assert!(evaluate("bad.splash", "let game = mod.game\nlet m = game.model_new(\"x\")\nm.nope(\n").is_err());
        let errors = evaluate("array.splash", "let game = mod.game\nlet m = game.model_new(\"x\")\nlet b = m.box({size: [1 1 1]})\nm.build()\n").err().expect("an array is not a vec3");
        assert!(errors.iter().any(|e| e.contains("is not a position")), "an array names the vec3 mistake, not a NaN: {errors:?}");
    }
}
