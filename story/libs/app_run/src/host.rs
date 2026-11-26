//! The script API as natives on the `app` object a run's
//! `script: fn(app)` receives. Every native reads its arguments, hands them
//! to [`Call::parse`](crate::call::Call::parse) and runs the result on the
//! [`Runner`] to completion (the virtual clock: every call runs in whole
//! frames and never retries input); the first failure ends the run (later
//! calls do nothing). Calls also feed the focus track.
//!
//! A dry script (`session: None`) only checks arguments and collects the
//! targets, for validate mode's resolution against the launch dump.

use crate::call::Call;
use crate::runner::{Clock, Poll, RunLimits, Runner};
use crate::doc::RunOutput;
use crate::session::Session;
use crate::values::{read, Sv};
use makepad_script::*;
use std::path::PathBuf;

/// What the script natives run against: the app (none in a dry run) and the
/// runner that executes the calls on its virtual clock.
pub struct Script {
    pub session: Option<Session>,
    pub runner: Runner,
}

impl Script {
    pub fn new(session: Option<Session>, output: RunOutput, limits: RunLimits, drop_root: PathBuf) -> Script {
        Script { session, runner: Runner::new(output, Clock::Virtual, limits, drop_root) }
    }

    pub(crate) fn s(&self) -> &Session {
        self.session.as_ref().unwrap()
    }

    /// Check and run one call. A failure is recorded in the runner's track.
    pub fn call(&mut self, name: &str, args: &[Sv]) {
        let call = match Call::parse(name, args) {
            Ok(call) => call,
            Err(e) => return self.runner.fail(name, e),
        };
        let Some(session) = self.session.as_mut() else {
            if let Err(e) = self.runner.dry(call) {
                self.runner.fail(name, e);
            }
            return;
        };
        self.runner.begin(call);
        // On the virtual clock the runner steps frames itself, so a call
        // finishes (or fails) within one poll.
        if self.runner.poll(session, 0.0) == Poll::Pending {
            self.runner.fail(name, "a virtual-clock call did not finish".into());
        }
    }
}

unsafe impl makepad_script::ScriptHostCapability for Script {
    const ID: makepad_script::LiveId = makepad_script::LiveId::from_str("makepad.app_run.runner.v1");
}

fn rt<'a>(vm: &'a mut ScriptVm) -> &'a mut Script {
    vm.host.get_mut::<Script>().unwrap()
}

/// Build the `app` object with every call of the API.
pub fn app_object(vm: &mut ScriptVm) -> ScriptValue {
    let app = vm.bx.heap_mut().new_object();
    macro_rules! call {
        ($name:ident, [$($arg:ident),*]) => {
            vm.add_method(app, id_lut!($name), script_args!($($arg = NIL),*), |vm, args| {
                let keys: &[LiveId] = &[$(id!($arg)),*];
                let a: Vec<Sv> = keys.iter().map(|key| {
                    let v = vm.bx.heap_ref().value(args, (*key).into(), NoTrap);
                    read(vm, v)
                }).collect();
                if allow(vm) {
                    rt(vm).call(stringify!($name), &a);
                }
                NIL
            });
        };
    }

    call!(click, [target, opts]);
    call!(drag, [from, to, opts]);
    call!(scroll, [target, dx, dy, opts]);
    call!(type, [target, text, opts]);
    call!(key, [code, opts]);
    call!(drop, [target, path, opts]);
    call!(resize, [width, height]);
    call!(hold, [seconds]);
    call!(wait_settled, [opts]);
    call!(wait_text, [target, text, opts]);
    call!(wait_visible, [target, opts]);
    call!(wait_state, [target, opts]);
    call!(mark, [name]);
    call!(cut, []);
    call!(resume, []);
    call!(focus, [target, opts]);
    call!(focus_rect, [rect, opts]);
    call!(wide, [opts]);

    app.into()
}

/// A script error since the last call fails the run before the next one.
fn allow(vm: &mut ScriptVm) -> bool {
    let errors = vm.take_errors();
    vm.capture_errors();
    if !errors.is_empty() {
        rt(vm).runner.fail("script", errors.join("\n"));
    }
    !rt(vm).runner.failed
}
