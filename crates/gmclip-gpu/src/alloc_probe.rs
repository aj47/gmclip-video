//! A counting global allocator (feature `alloc-probe`, tests only): a test installs it
//! with `#[global_allocator]` and counts the allocations, reallocations and frees one
//! thread makes between [`start`] and [`stop`], to prove a real-time path (the playback
//! callback, H.02) never touches the heap. Everything else forwards to the system
//! allocator. Lives here because implementing `GlobalAlloc` is `unsafe`, which only this
//! crate may contain.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// The allocator: `#[global_allocator] static A: Counting = Counting;`.
#[derive(Debug, Default)]
pub struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: the thread's locals may already be gone while it exits
    let _ = ON.try_with(|on| {
        if on.get() {
            let _ = COUNT.try_with(|n| n.set(n.get() + 1));
        }
    });
}

/// Starts counting this thread's heap operations (from zero).
pub fn start() {
    COUNT.with(|n| n.set(0));
    ON.with(|on| on.set(true));
}

/// Stops counting; the number of heap operations since [`start`].
pub fn stop() -> u64 {
    ON.with(|on| on.set(false));
    COUNT.with(Cell::get)
}

// SAFETY: every method forwards to the system allocator with the caller's arguments
// unchanged; the bookkeeping is a const-initialised thread-local, which never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, passed on as is.
        unsafe { System.alloc(l) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note();
        // SAFETY: as `alloc`.
        unsafe { System.alloc_zeroed(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        // SAFETY: `p` came from this allocator, i.e. from `System`, with layout `l`.
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        // SAFETY: as `dealloc`, and the caller upholds `realloc`'s size contract.
        unsafe { System.realloc(p, l, n) }
    }
}
