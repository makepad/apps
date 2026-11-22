//! A counting allocator, for a host that measures what its live run's snapshots hold
//! (`LiveSpec::snapshots`, `Snapshots::bytes`). Installed as the global allocator
//! (`#[global_allocator] static A: fab_elec_sim::alloc::Counting = fab_elec_sim::alloc::Counting(System);`,
//! or over another allocator than the system's)
//! it counts, per thread, the bytes that thread allocated less those it freed while a
//! [`measure`] runs anywhere, and a snapshot's size is what the worker's thread allocated while it
//! took one. Without it the size of a snapshot is unknown, and the budget counts snapshots instead.
//!
//! The rest of the time an allocation or a free reads one gate (a relaxed load of a count written
//! only as a measure starts and ends, on a cache line of its own) and counts nothing: on a host
//! whose thread-locals are a call each (macOS), counting every allocation of the app cost a plate
//! load a few percent. The count is a thread-local `Cell`, const-initialised and without a
//! destructor, with no atomics and no lock. A block freed on another thread than the one that
//! allocated it moves the two counts the other way, so a count is only read as the difference of
//! two readings on one thread, within one measure (wrapping).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

/// The measures running: while it is 0 nothing counts. Alone on its cache line (128 bytes: a pair
/// of lines where the core fetches them together), so nothing written often shares it.
#[repr(align(128))]
struct Gate(AtomicU32);

static MEASURING: Gate = Gate(AtomicU32::new(0));

/// The allocator `A` (the system's unless named), counting per thread while a [`measure`] runs.
pub struct Counting<A = System>(pub A);

#[inline]
fn add(n: usize) {
    if MEASURING.0.load(Ordering::Relaxed) != 0 {
        let _ = COUNT.try_with(|c| c.set(c.get().wrapping_add(n as u64)));
    }
}

#[inline]
fn sub(n: usize) {
    if MEASURING.0.load(Ordering::Relaxed) != 0 {
        let _ = COUNT.try_with(|c| c.set(c.get().wrapping_sub(n as u64)));
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        add(layout.size());
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        sub(layout.size());
        unsafe { self.0.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size);
        sub(layout.size());
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }
}

/// The gate raised while it lives, lowered when it drops (a panic in the measure too).
struct Raised;

impl Raised {
    fn new() -> Raised {
        MEASURING.0.fetch_add(1, Ordering::Relaxed);
        Raised
    }
}

impl Drop for Raised {
    fn drop(&mut self) {
        MEASURING.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs `f` and returns what it returned and the bytes this thread allocated less those it freed
/// meanwhile (0 when it freed more), when [`Counting`] is the global allocator (None otherwise).
/// O(1) over `f` (the first call allocates once to find out whether it counts); every thread's
/// allocations count while it runs, and only this thread's are read.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Option<u64>) {
    static COUNTS: OnceLock<bool> = OnceLock::new();
    let _raised = Raised::new();
    let counts = *COUNTS.get_or_init(|| {
        // a box held across two readings: a counting allocator saw it go through
        let before = COUNT.with(Cell::get);
        let probe = std::hint::black_box(Box::new([0u8; 64]));
        let held = COUNT.with(Cell::get);
        drop(probe);
        held.wrapping_sub(before) == 64
    });
    let before = COUNT.with(Cell::get);
    let r = f();
    let grew = COUNT.with(Cell::get).wrapping_sub(before);
    (r, counts.then_some(if grew > u64::MAX / 2 { 0 } else { grew }))
}
