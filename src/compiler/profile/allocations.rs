//! Thread-local allocation traffic, not retained heap size or process RSS.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(super) struct Allocations {
    pub calls: u64,
    pub bytes: u64,
}

thread_local! {
    static TOTAL: Cell<Allocations> = const { Cell::new(Allocations { calls: 0, bytes: 0 }) };
}

pub(super) fn snapshot() -> Allocations {
    TOTAL.with(Cell::get)
}

impl Allocations {
    pub fn since(self, earlier: Self) -> Self {
        Self {
            calls: self.calls.saturating_sub(earlier.calls),
            bytes: self.bytes.saturating_sub(earlier.bytes),
        }
    }
}

fn record(size: usize, pointer: *mut u8) {
    if !pointer.is_null() {
        // No allocation, borrowing, or panic in allocator callbacks. TLS may
        // already be unavailable while the thread is being destroyed.
        let _ = TOTAL.try_with(|total| {
            let previous = total.get();
            total.set(Allocations {
                calls: previous.calls.saturating_add(1),
                bytes: previous.bytes.saturating_add(size as u64),
            });
        });
    }
}

struct ProfilingAllocator;

// SAFETY: Every allocation and deallocation is forwarded to System with the
// original layout/pointer; instrumentation only records successful requests.
unsafe impl GlobalAlloc for ProfilingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        record(layout.size(), pointer);
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        record(layout.size(), pointer);
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        record(size, pointer);
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: ProfilingAllocator = ProfilingAllocator;
