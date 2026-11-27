//! `Nes::save_state` runs once per frame on the rewind path, and it used to write into a `Vec`
//! that started empty and grew by doubling. Measured on main before the fix, a snapshot of each
//! board below took 1 allocation and 9 reallocations (10 on MMC1 with CHR-RAM), and the buffer the
//! rewind ring kept was 10-15% larger than the state (152,310 bytes for the 138,358-byte NROM
//! state). The writer is now sized from the previous snapshot: one allocation, capacity within
//! `SLACK` bytes of the length, and the same bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use tendy_nes::nes::Nes;

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static REALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most `Nes::save_state` reserves beyond the previous snapshot's length.
const SLACK: usize = 64;

/// iNES image for `mapper`: two 16 KB PRG banks of NOPs whose vectors point at $8000, and `chr`
/// 8 KB CHR-ROM banks (0 = CHR-RAM).
fn rom(mapper: u8, chr: u8) -> Vec<u8> {
    let mut v = vec![
        0x4E,
        0x45,
        0x53,
        0x1A,
        2,
        chr,
        mapper << 4,
        mapper & 0xF0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    let mut prg = vec![0xEAu8; 2 * 16384];
    let n = prg.len();
    prg[n - 6..].copy_from_slice(&[0x00, 0x80, 0x00, 0x80, 0x00, 0x80]);
    v.extend(prg);
    v.resize(v.len() + chr as usize * 8192, 0);
    v
}

/// `nes.save_state()` with the heap allocations and reallocations it made.
fn counted_save(nes: &Nes) -> (Vec<u8>, usize, usize) {
    let (a, r) = (
        ALLOCS.load(Ordering::Relaxed),
        REALLOCS.load(Ordering::Relaxed),
    );
    let state = nes.save_state();
    (
        state,
        ALLOCS.load(Ordering::Relaxed) - a,
        REALLOCS.load(Ordering::Relaxed) - r,
    )
}

#[test]
fn a_snapshot_is_one_allocation_of_its_own_size() {
    for (name, image) in [
        ("NROM", rom(0, 1)),
        ("MMC1 CHR-RAM", rom(1, 0)),
        ("MMC3", rom(4, 2)),
    ] {
        let mut nes = Nes::new(&image).unwrap();
        nes.run_frame();
        // The first snapshot has no previous size to go on, so it may grow; it is the reference
        // for the bytes, since the buffer's starting size cannot change what is written into it.
        let first = nes.save_state();
        let (again, allocs, reallocs) = counted_save(&nes);
        assert!(
            again == first,
            "{name}: the same machine must save the same bytes"
        );
        assert_eq!(
            (allocs, reallocs),
            (1, 0),
            "{name}: allocations, reallocations for one snapshot"
        );
        assert!(
            again.capacity() - again.len() <= SLACK,
            "{name}: a {}-byte snapshot holds {} bytes of heap",
            again.len(),
            again.capacity()
        );
        // The rewind path: one snapshot per frame, kept, for two seconds of play.
        let mut ring = Vec::new();
        for frame in 0..120 {
            nes.run_frame();
            let (state, allocs, reallocs) = counted_save(&nes);
            assert_eq!(
                (allocs, reallocs),
                (1, 0),
                "{name} frame {frame}: allocations, reallocations"
            );
            assert!(
                state.capacity() - state.len() <= SLACK,
                "{name} frame {frame}: capacity {}",
                state.capacity()
            );
            ring.push(state);
        }
    }
}
