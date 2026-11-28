//! Requested Rust allocation bytes. Excludes allocator slack, C heaps and stacks.
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

#[cfg(feature = "allocator-mimalloc")]
const BACKEND: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(not(feature = "allocator-mimalloc"))]
const BACKEND: std::alloc::System = std::alloc::System;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Installed only by fttrd; standalone tools retain their system allocator.
pub struct Allocator;

#[cfg(all(test, feature = "platform-fiberhome-an4475-g2"))]
pub(crate) mod allocation_test {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };
    thread_local! { static COUNT: Cell<Option<usize>> = const { Cell::new(None) }; }
    struct Counter;
    #[global_allocator]
    static TEST_ALLOCATOR: Counter = Counter;
    fn count() {
        let _ = COUNT.try_with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1));
            }
        });
    }
    unsafe impl GlobalAlloc for Counter {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            System.alloc(layout)
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            System.alloc_zeroed(layout)
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count();
            System.realloc(ptr, layout, size)
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            System.dealloc(ptr, layout);
        }
    }
    pub(crate) fn allocations(work: impl FnOnce()) -> usize {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                COUNT.set(None);
            }
        }
        assert!(COUNT.replace(Some(0)).is_none());
        let _reset = Reset;
        work();
        COUNT.get().unwrap()
    }
}

fn allocated(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Relaxed) + bytes;
    PEAK.fetch_max(live, Relaxed);
    ALLOCATED.fetch_add(bytes as u64, Relaxed);
}

// SAFETY: every allocation operation delegates the original layout/pointer to
// the same backend. Counters never allocate, log or take a lock.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = BACKEND.alloc(layout);
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = BACKEND.alloc_zeroed(layout);
        if !ptr.is_null() {
            allocated(layout.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        BACKEND.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let ptr = BACKEND.realloc(ptr, layout, size);
        if !ptr.is_null() {
            if size >= layout.size() {
                allocated(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Relaxed);
            }
        }
        ptr
    }
}

pub(crate) fn view() -> serde_json::Value {
    serde_json::json!({
        "allocator":if cfg!(feature="allocator-mimalloc") {"mimalloc"} else {"system"},
        "rust_requested_live_bytes":LIVE.load(Relaxed),
        "rust_requested_peak_bytes":PEAK.load(Relaxed),
        "rust_allocated_bytes_total":ALLOCATED.load(Relaxed),
        "startup_options":startup_options(),
        "scope":"Rust requested sizes; excludes allocator slack, C allocations and thread stacks",
    })
}

// These are startup requests, not a claim that every page can be reclaimed.
// procd supplies them before exec, when mimalloc first reads its environment.
// Do not set environment variables from main after allocator initialization.
fn startup_options() -> serde_json::Value {
    if !cfg!(feature = "allocator-mimalloc") {
        return serde_json::Value::Null;
    }
    const KEYS: [&str; 8] = [
        "MIMALLOC_ARENA_EAGER_COMMIT",
        "MIMALLOC_PAGE_COMMIT_ON_DEMAND",
        "MIMALLOC_PAGE_FULL_RETAIN",
        "MIMALLOC_PURGE_DELAY",
        "MIMALLOC_ARENA_PURGE_MULT",
        "MIMALLOC_PURGE_DECOMMITS",
        "MIMALLOC_ALLOW_THP",
        "MIMALLOC_GENERIC_COLLECT",
    ];
    KEYS.into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|v| (key.into(), v.into())))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tracks_actual_zeroed_reallocation_and_release_without_counting_freed_bytes_as_live() {
        // The library test runner uses System, so only these explicit calls are counted.
        let before = LIVE.load(Relaxed);
        unsafe {
            let layout = Layout::from_size_align(4096, 64).unwrap();
            let ptr = Allocator.alloc_zeroed(layout);
            assert!(!ptr.is_null());
            assert_eq!(LIVE.load(Relaxed), before + 4096);
            assert!(std::slice::from_raw_parts(ptr, 4096)
                .iter()
                .all(|v| *v == 0));
            let ptr = Allocator.realloc(ptr, layout, 8192);
            assert!(!ptr.is_null());
            assert_eq!(LIVE.load(Relaxed), before + 8192);
            let layout = Layout::from_size_align(8192, 64).unwrap();
            let ptr = Allocator.realloc(ptr, layout, 2048);
            assert!(!ptr.is_null());
            assert_eq!(LIVE.load(Relaxed), before + 2048);
            Allocator.dealloc(ptr, Layout::from_size_align(2048, 64).unwrap());
        }
        assert_eq!(LIVE.load(Relaxed), before);
    }
}
