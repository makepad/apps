//! A downward-growing stack floor for the thread running a guarded operation.

use std::cell::Cell;

thread_local! {
    static FLOOR: Cell<usize> = const { Cell::new(0) };
}

pub(super) struct StackExhausted;

fn stack_address() -> usize {
    let marker = 0u8;
    std::ptr::from_ref(&marker) as usize
}

pub(super) struct Floor(usize);

impl Drop for Floor {
    fn drop(&mut self) {
        FLOOR.with(|floor| floor.set(self.0));
    }
}

pub(super) fn arm_stack(budget: usize) -> Floor {
    Floor(FLOOR.with(|floor| floor.replace(stack_address().saturating_sub(budget))))
}

#[inline]
pub(super) fn probe() {
    if stack_address() < FLOOR.with(Cell::get) {
        std::panic::resume_unwind(Box::new(StackExhausted));
    }
}
