//! The Splash model builder, shared by Sandbox and Stage.
//!
//! `model_new` records `libs/model` operations into a program, `character_new`
//! and `fps_arms_new` validate a character spec into one; either is hashed
//! (FNV-1a over the program, its class and `MODEL_GENERATOR_VERSION`) and
//! posted to a process-wide mailbox (`model_verbs::take_script_model_requests`)
//! that the build service (`makepad-model-build`) drains. The registry here
//! also answers what the host asks while builds run: a model's state, its
//! estimated bounds, build progress, LOD chains.
//!
//! No game loop: the host supplies only [`ModelHost`] (where messages go, and
//! an optional timer round builder methods). Sandbox's `game.model_new` /
//! `game.character_new` verbs are thin calls into this crate.

pub mod character_spec;
pub mod character_verb;
pub mod headless;
pub mod lang;
pub mod model_verbs;
pub mod rig_rest;
pub mod value;
pub mod vector;

use makepad_script::{LiveId, ScriptValue};

/// What a host decides for a recorded model program (`ModelHost::admit_program`).
#[derive(Clone, Debug, PartialEq)]
pub enum Admit {
    /// Post it to the build service.
    Build,
    /// Not built on this device: the alias stands for a model the app draws as
    /// a stand-in, and this reason is reported once for it.
    Withhold(String),
}

/// What the builder needs from the VM's host.
pub trait ModelHost {
    /// A message for the script's author (a validation warning, an unknown
    /// option, a failed build).
    fn log(&self, message: String);
    /// A builder call that did nothing because it was wrong (unknown method, bad
    /// argument): by default a message like any other.
    fn error(&self, message: String) {
        self.log(message)
    }
    /// Admit the recorded program of `alias` (`bytes`: its canonical encoding)
    /// before it enters the build service. A host with untrusted input may
    /// withhold it under its memory policy; the script carries on either way.
    fn admit_program(&self, _alias: &str, _bytes: usize) -> Admit { Admit::Build }
    /// Run one `m.<method>` call of a builder; a host may time it.
    fn call_method(&self, method: LiveId, call: &mut dyn FnMut() -> ScriptValue) -> ScriptValue {
        let _ = method;
        call()
    }
}
