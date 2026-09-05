//! Optional diagnostic build only. Counts Rust/System allocations, excluding native-library heaps.
#[cfg(feature = "heap-diagnostics")]
mod tracked {
    #[cfg(not(feature = "mimalloc"))]
    use std::alloc::System;
    use std::alloc::{GlobalAlloc, Layout};
    #[cfg(feature = "mimalloc")]
    const BACKING: mimalloc::MiMalloc = mimalloc::MiMalloc;
    #[cfg(not(feature = "mimalloc"))]
    const BACKING: System = System;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    pub struct Tracker;
    fn add(bytes: usize) {
        let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(live, Ordering::Relaxed);
    }
    unsafe impl GlobalAlloc for Tracker {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { BACKING.alloc(layout) };
            if !pointer.is_null() {
                add(layout.size());
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { BACKING.alloc_zeroed(layout) };
            if !pointer.is_null() {
                add(layout.size());
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { BACKING.dealloc(pointer, layout) };
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            COUNT.fetch_sub(1, Ordering::Relaxed);
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let result = unsafe { BACKING.realloc(pointer, layout, size) };
            if !result.is_null() {
                if size >= layout.size() {
                    add(size - layout.size());
                } else {
                    LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
                }
            }
            result
        }
    }
    pub fn record() {
        crate::performance::sample("rust_live_bytes", LIVE.load(Ordering::Relaxed) as f64);
        crate::performance::sample(
            "rust_live_allocations",
            COUNT.load(Ordering::Relaxed) as f64,
        );
        crate::performance::sample("rust_peak_bytes", PEAK.load(Ordering::Relaxed) as f64);
    }
}
#[cfg(feature = "heap-diagnostics")]
#[global_allocator]
static ALLOCATOR: tracked::Tracker = tracked::Tracker;

#[cfg(all(feature = "mimalloc", not(feature = "heap-diagnostics")))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub fn record() {
    #[cfg(feature = "heap-diagnostics")]
    tracked::record();
}
