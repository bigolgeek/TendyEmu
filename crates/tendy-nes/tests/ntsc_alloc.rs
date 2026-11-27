//! The NTSC filter is a per-frame hot path and must not allocate per scanline.
//!
//! `decode_line` used to build six `Vec<f32>` of `LINE_SAMPLES` for every one of the 240 lines:
//! 1,440 heap allocations and 4.5 MB of zero fill per frame. The scratch space is now stack
//! arrays, so a filtered frame after the first (which grows the output buffer) allocates nothing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use tendy_nes::ntsc::NtscFilter;
use tendy_nes::{SCREEN_HEIGHT, SCREEN_WIDTH};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn filtering_a_frame_does_not_allocate() {
    let n = SCREEN_WIDTH * SCREEN_HEIGHT;
    let indices: Vec<u8> = (0..n).map(|i| ((i * 7 + i / 256) % 64) as u8).collect();
    let emphasis = vec![0u8; n];
    let filter = NtscFilter::new();
    let mut out = Vec::new();
    // The first frame grows `out` to the frame size; every frame after that reuses it.
    filter.apply(&indices, &emphasis, &mut out);
    let before = ALLOCS.load(Ordering::Relaxed);
    filter.apply(&indices, &emphasis, &mut out);
    let allocated = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocated, 0,
        "{allocated} heap allocations in one filtered frame"
    );
    assert_eq!(out.len(), n * 3);
    // The window's path, `apply_rgba`, adds the RGB->RGBA copy into a caller-owned buffer: the
    // whole frame must allocate nothing on that path either.
    let mut rgba = vec![0u8; n * 4];
    let before = ALLOCS.load(Ordering::Relaxed);
    filter.apply_rgba(&indices, &emphasis, &mut out, &mut rgba);
    let allocated = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(
        allocated, 0,
        "{allocated} heap allocations in one RGBA frame"
    );
    assert_eq!(
        &rgba[..4],
        &[out[0], out[1], out[2], 255],
        "opaque copy of the RGB frame"
    );
}
