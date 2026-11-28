//! Running kernel code so that it can't take the app down.
//!
//! The kernel recurses deeply and its algorithms carry invariants, so a call runs inside
//! [`guarded`], on a thread with a big stack (see [`stack_size`]). There a panic and a
//! stack limit come back as a [`BrepError`], and a cancellation stops the work at a poll.

use crate::stack::{arm_stack, probe, StackExhausted};
use makepad_csg_brep::kernel::{CancelToken as KernelToken, Kernel};
use makepad_csg_brep::{makepad_hooks, SolidError};
use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Why a body could not be built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrepError {
    /// The kernel refused or failed; the message says what it was doing.
    Kernel(String),
    /// Bad input from the model.
    Input(String),
    /// A newer edit replaced this build.
    Cancelled,
    /// The kernel recursed past its stack limit, or panicked on an internal invariant.
    Internal(String),
    /// A selector names nothing on the body any more: `index` is its place among the names the
    /// step selects by (its named edges, or the faces it leaves open).
    Lost { index: usize, message: String },
}

impl std::fmt::Display for BrepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrepError::Kernel(m) | BrepError::Input(m) | BrepError::Lost { message: m, .. } => f.write_str(m),
            BrepError::Cancelled => f.write_str("cancelled by a newer edit"),
            BrepError::Internal(m) => write!(f, "the kernel gave up: {m}"),
        }
    }
}

impl std::error::Error for BrepError {}

/// A stop is a stop: a cancel is `Cancelled`, and a spent work budget is `Internal` (Fab sets no
/// fuel, so it means a bug). Every other kernel failure stays `Kernel`, whatever its class:
/// the engine treats `Internal` as unrecoverable, and most failures at the pin are unclassed.
impl From<SolidError> for BrepError {
    fn from(e: SolidError) -> BrepError {
        match e.root_cause() {
            SolidError::Cancelled { .. } => BrepError::Cancelled,
            SolidError::OutOfFuel { .. } => BrepError::Internal(e.to_string()),
            _ => BrepError::Kernel(e.to_string()),
        }
    }
}

impl BrepError {
    pub(crate) fn input_context(self, context: impl std::fmt::Display) -> Self {
        match self {
            Self::Cancelled | Self::Internal(_) => self,
            _ => Self::Input(format!("{context}: {self}")),
        }
    }
}

/// The stack of a kernel worker thread. Only the pages it touches are ever committed.
pub const fn stack_size() -> usize {
    1 << 30
}

/// How far from the stack's end the kernel stops recursing.
const STACK_MARGIN: usize = 16 << 20;

/// Cancels the work of one [`guarded`] call from another thread.
#[derive(Clone, Default)]
pub struct CancelToken(KernelToken);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }
    pub fn cancel(&self) {
        self.0.cancel()
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    pub(crate) fn either(&self, other: &Self) -> Self {
        Self(self.0.either(&other.0))
    }
    pub(crate) fn check(&self) -> Result<(), BrepError> {
        self.0.check("build")?;
        Ok(())
    }
}

thread_local! {
    static CURRENT: RefCell<Option<KernelToken>> = const { RefCell::new(None) };
    static TICK: Cell<Option<fn()>> = const { Cell::new(None) };
}

pub(crate) fn current_token() -> Option<KernelToken> {
    CURRENT.with(|current| current.borrow().clone())
}

#[inline]
pub(crate) fn check_entry(k: &Kernel, operation: &'static str) -> Result<(), BrepError> {
    probe();
    k.check_cancelled(operation)?;
    Ok(())
}

fn observe_poll() {
    probe();
    if let Some(tick) = TICK.with(Cell::get) {
        tick();
    }
}

/// Runs `f` with the stack limit and `cancel` armed on this thread, whose stack is
/// `stack` bytes, and `tick` called now and then from inside the kernel (a checkpoint). A
/// panic, a stack limit and a cancel become errors. Call it on a thread spawned with that
/// stack size.
pub fn guarded<T>(stack: usize, cancel: &CancelToken, tick: Option<fn()>, f: impl FnOnce() -> Result<T, BrepError>) -> Result<T, BrepError> {
    let _floor = arm_stack(stack.saturating_sub(STACK_MARGIN).max(1));
    let previous = CURRENT.with(|current| current.replace(Some(cancel.0.clone())));
    let previous_tick = TICK.with(|t| t.replace(tick));
    let _observer = makepad_hooks::set_poll_observer(observe_poll);
    let result = caught(|| {
        cancel.check()?;
        let result = f();
        cancel.check()?;
        result
    });
    CURRENT.with(|current| current.replace(previous));
    TICK.with(|t| t.set(previous_tick));
    result
}

/// `f` with its panics as errors (a cancel too), inside a [`guarded`] call: for work whose
/// failure need not end the call (the faces' names a listing can do without).
pub(crate) fn caught<T>(f: impl FnOnce() -> Result<T, BrepError>) -> Result<T, BrepError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => Err(from_panic(payload)),
    }
}

fn from_panic(payload: Box<dyn std::any::Any + Send>) -> BrepError {
    match payload.downcast::<StackExhausted>() {
        Ok(_) => BrepError::Internal("recursion too deep (stack limit)".into()),
        Err(p) => {
            let message = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
            BrepError::Internal(message.unwrap_or_else(|| "internal panic".into()))
        }
    }
}
