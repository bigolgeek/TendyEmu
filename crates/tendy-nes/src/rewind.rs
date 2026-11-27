//! Rewind history: a bounded ring of whole-machine snapshots, plus the command-line
//! arithmetic that sizes it from `--rewind-seconds`.
//!
//! WHY there is no second state format here: [`crate::nes::Nes::save_state`] already serialises
//! every piece of emulated state to bytes (CPU, bus, cartridge, PPU including its framebuffer, APU,
//! joypads) and [`crate::nes::Nes::load_state`] restores exactly those bytes. A rewind entry is one
//! of those vectors unchanged and a step back is `load_state` on it, which is why this module adds
//! no state field and `STATE_VERSION` stays where it was.
//!
//! WHY the ring is bounded twice: a frame count bounds the window but not the memory, because the
//! size of one state belongs to the machine (a ~130 KiB state at 600 frames is ~78 MiB, and a mapper
//! with large CHR-RAM or a bigger bus makes it bigger). The frame capacity and a byte budget are
//! both enforced, whichever binds first, and the startup line reports frames kept x the measured
//! state size, so the bound is a number an operator can check rather than a promise.
//!
//! When a bound is reached the oldest snapshot is dropped: rewind is a fixed window into the past,
//! never a growing log, and the newest snapshot — the frame on screen — always survives.

use std::collections::VecDeque;

/// Frames the ring keeps by default: 600, about ten seconds at the NES's ~60 Hz frame rate.
/// `--rewind-seconds` replaces it (converted at the region's own rate) and `0` turns rewind off.
pub const DEFAULT_REWIND_FRAMES: usize = 600;

/// Bytes the ring may hold by default: 128 MiB, roughly sixteen seconds of ~130 KiB states. A byte
/// budget is what keeps a long `--rewind-seconds` on a large state from doubling the process's
/// memory: the ring trims the window instead of refusing to start.
pub const DEFAULT_REWIND_BUDGET: usize = 128 * 1024 * 1024;

/// Most frames a `--rewind-seconds` value can ask for: an hour at 60 Hz. Without a cap a mistyped
/// value (or an infinite one) would be reported as an absurd frame count even though the byte
/// budget binds long before it; an hour is far past any window the budget can hold.
pub const MAX_REWIND_FRAMES: usize = 216_000;

/// Frames a `--rewind-seconds` value asks for on a machine running at `fps`: `seconds * fps`
/// rounded to the nearest frame, so "10 s" is the same window on NTSC (~601 frames at 60.099 Hz)
/// and on PAL (500 at 50 Hz). A zero, negative, NaN or infinite value is no history at all, which
/// is what `--rewind-seconds 0` means.
pub fn frames_for(seconds: f64, fps: f64) -> usize {
    if !seconds.is_finite() || seconds <= 0.0 || !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    ((seconds * fps).round() as usize).min(MAX_REWIND_FRAMES)
}

/// Frames a ring of `frames` slots can really keep when one state is `state_len` bytes and the
/// budget is `budget` bytes: the tighter of the two bounds. At least one frame survives even when a
/// single state is larger than the whole budget, because a ring that keeps nothing could not
/// rewind at all, and that one frame is what the startup line then reports.
pub fn kept_frames(frames: usize, state_len: usize, budget: usize) -> usize {
    if frames == 0 || state_len == 0 {
        return frames;
    }
    frames.min((budget / state_len).max(1))
}

/// The rewind history: snapshots newest-last, at most `capacity` of them and at most `budget`
/// bytes in total. Capacity 0 is rewind off, and then `push` stores nothing at all.
pub struct Rewind {
    states: VecDeque<Vec<u8>>,
    capacity: usize,
    budget: usize,
    bytes: usize,
}

impl Rewind {
    /// A ring of `capacity` frames under the default byte budget.
    pub fn new(capacity: usize) -> Rewind {
        Rewind::with_budget(capacity, DEFAULT_REWIND_BUDGET)
    }

    /// A ring of `capacity` frames whose snapshots may total `budget` bytes. The tighter bound
    /// wins: the oldest states are dropped until the new one fits.
    pub fn with_budget(capacity: usize, budget: usize) -> Rewind {
        Rewind {
            states: VecDeque::new(),
            capacity,
            budget,
            bytes: 0,
        }
    }

    /// False for `--rewind-seconds 0`: no snapshot is stored, so rewind costs nothing.
    pub fn is_enabled(&self) -> bool {
        self.capacity > 0
    }

    /// Frames asked for, before the byte budget trims them (`kept_frames` gives the result).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Snapshots held right now: also how many single-frame steps back a hold can still take.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Bytes the held snapshots occupy, i.e. what this feature is costing right now.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Store one snapshot as the newest frame. Dropping the oldest first is what makes the ring
    /// bounded and keeps the memory at capacity x the size of one state; a disabled ring stores
    /// nothing, so `--rewind-seconds 0` allocates no history at all.
    pub fn push(&mut self, state: Vec<u8>) {
        if !self.is_enabled() {
            return;
        }
        while !self.states.is_empty()
            && (self.states.len() >= self.capacity || self.bytes + state.len() > self.budget)
        {
            if let Some(old) = self.states.pop_front() {
                self.bytes -= old.len();
            }
        }
        self.bytes += state.len();
        self.states.push_back(state);
    }

    /// The snapshot one frame back, removed from the ring so the next hold tick goes one frame
    /// further back. Handing out the newest first is what makes a hold walk backwards through
    /// history instead of restoring the same frame again.
    pub fn step_back(&mut self) -> Option<Vec<u8>> {
        let state = self.states.pop_back()?;
        self.bytes -= state.len();
        Some(state)
    }

    /// The one-line startup report: frames kept, that window in seconds at `fps`, the measured size
    /// of one state and the memory bound those multiply to. The frontend prints it so
    /// `--rewind-seconds` never spends memory the operator has not seen sized.
    pub fn startup_line(&self, state_len: usize, fps: f64) -> String {
        if !self.is_enabled() {
            return "rewind: off".to_string();
        }
        let kept = kept_frames(self.capacity, state_len, self.budget);
        let seconds = if fps > 0.0 { kept as f64 / fps } else { 0.0 };
        let state_kib = state_len as f64 / 1024.0;
        let held_mib = (kept * state_len) as f64 / (1024.0 * 1024.0);
        if kept < self.capacity {
            format!(
                "rewind: {kept} frames (~{seconds:.1} s) of {} asked, {state_kib:.1} KiB/state, {held_mib:.1} MiB ({} MiB budget)",
                self.capacity,
                self.budget / (1024 * 1024)
            )
        } else {
            format!(
                "rewind: {kept} frames (~{seconds:.1} s), {state_kib:.1} KiB/state, {held_mib:.1} MiB"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nes::Nes;
    use crate::state::fnv1a64;

    /// A 32 KiB NROM image with no code: the CPU sits in a `BRK` loop at `$0000`, so the picture
    /// only changes when the test paints one. That keeps the replay test about state, not about
    /// what a ROM happened to do.
    fn nrom() -> Vec<u8> {
        let mut rom = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        rom.resize(16 + 16 * 1024 + 8 * 1024, 0);
        rom
    }

    /// The visible picture as one number: a replay that lands on a different framebuffer or
    /// emphasis is one comparison away from the original run's per-frame digest.
    fn digest(nes: &Nes) -> u64 {
        let mut bytes =
            Vec::with_capacity(nes.bus.ppu.framebuffer.len() + nes.bus.ppu.emphasis.len());
        bytes.extend_from_slice(&nes.bus.ppu.framebuffer);
        bytes.extend_from_slice(&nes.bus.ppu.emphasis);
        fnv1a64(&bytes)
    }

    /// Give frame `frame` a picture of its own: the backdrop colour, one pattern-table byte and the
    /// pad byte move with the frame number, so a replay that restores anything less than the whole
    /// state cannot reproduce the digest.
    fn paint(nes: &mut Nes, frame: usize) {
        nes.bus.ppu.palette[0] = (frame % 64) as u8;
        nes.bus.ppu.vram[frame % 2048] = (frame * 7) as u8;
        nes.bus.pads[0].buttons = (frame % 16) as u8;
    }

    #[test]
    fn frames_for_follows_the_region_frame_rate() {
        assert_eq!(frames_for(10.0, 60.0988), 601);
        assert_eq!(frames_for(10.0, 50.0), 500);
        assert_eq!(frames_for(2.5, 60.0), 150);
        for off in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(frames_for(off, 60.0), 0, "{off}");
        }
        // A value past an hour is clamped, because the byte budget binds far earlier.
        assert_eq!(frames_for(1_000_000.0, 60.0), MAX_REWIND_FRAMES);
    }

    #[test]
    fn kept_frames_respects_the_budget() {
        assert_eq!(kept_frames(600, 1024, 600 * 1024), 600);
        assert_eq!(kept_frames(600, 1024, 600 * 1024 - 1), 599);
        // One state larger than the whole budget is still one rewind step.
        assert_eq!(kept_frames(600, 4096, 10), 1);
        assert_eq!(kept_frames(0, 4096, 10), 0);
    }

    #[test]
    fn ring_drops_the_oldest_at_capacity() {
        let mut ring = Rewind::new(3);
        for n in 0..5u8 {
            ring.push(vec![n]);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.bytes(), 3);
        // Newest first: 4, 3, 2. The snapshots for 0 and 1 were dropped when 3 and 4 arrived.
        assert_eq!(ring.step_back(), Some(vec![4]));
        assert_eq!(ring.step_back(), Some(vec![3]));
        assert_eq!(ring.step_back(), Some(vec![2]));
        assert_eq!(ring.step_back(), None);
        assert_eq!(ring.bytes(), 0);
    }

    #[test]
    fn capacity_zero_keeps_no_history() {
        let mut ring = Rewind::new(0);
        assert!(!ring.is_enabled());
        for n in 0..10u8 {
            ring.push(vec![n; 1024]);
        }
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.bytes(), 0);
        assert_eq!(ring.step_back(), None);
        assert_eq!(ring.startup_line(1024, 60.0), "rewind: off");
    }

    #[test]
    fn step_back_walks_backwards_one_frame_per_tick() {
        let mut ring = Rewind::new(600);
        for n in 0..30u8 {
            ring.push(vec![n]);
        }
        let mut seen = Vec::new();
        while let Some(state) = ring.step_back() {
            seen.push(state[0]);
        }
        assert_eq!(seen, (0..30u8).rev().collect::<Vec<_>>());
    }

    #[test]
    fn byte_budget_drops_the_oldest() {
        let mut ring = Rewind::with_budget(600, 250);
        ring.push(vec![0; 100]);
        ring.push(vec![1; 100]);
        assert_eq!(ring.len(), 2);
        // The third does not fit, so the oldest goes rather than the new frame being lost.
        ring.push(vec![2; 100]);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.bytes(), 200);
        assert_eq!(ring.step_back().map(|s| s[0]), Some(2));
        assert_eq!(ring.step_back().map(|s| s[0]), Some(1));
        assert_eq!(ring.bytes(), 0);
    }

    #[test]
    fn one_state_bigger_than_the_budget_is_still_kept() {
        let mut ring = Rewind::with_budget(600, 10);
        ring.push(vec![7; 64]);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.bytes(), 64);
        assert_eq!(ring.step_back().map(|s| s[0]), Some(7));
    }

    #[test]
    fn startup_line_states_the_bound() {
        let ring = Rewind::new(600);
        assert_eq!(
            ring.startup_line(133 * 1024, 60.0),
            "rewind: 600 frames (~10.0 s), 133.0 KiB/state, 77.9 MiB"
        );
        // A window the byte budget cannot hold is reported trimmed, not silently cut.
        let long = Rewind::new(3600);
        assert_eq!(
            long.startup_line(133 * 1024, 60.0),
            "rewind: 985 frames (~16.4 s) of 3600 asked, 133.0 KiB/state, 127.9 MiB (128 MiB budget)"
        );
    }

    #[test]
    fn thirty_frames_of_rewind_replay_byte_identically() {
        let rom = nrom();
        let mut nes = Nes::new(&rom).unwrap();
        let mut ring = Rewind::new(600);
        let mut original = Vec::new();
        for frame in 0..90 {
            paint(&mut nes, frame);
            nes.run_frame();
            original.push(digest(&nes));
            ring.push(nes.save_state());
        }
        // A hold of 30 ticks: the newest snapshot is frame 89, so 30 pops land on frame 60 — and
        // the picture that comes back must be the one frame 60 showed.
        for _ in 0..30 {
            let state = ring.step_back().expect("30 frames of history");
            nes.load_state(&state).unwrap();
        }
        assert_eq!(
            digest(&nes),
            original[60],
            "the restored frame is the saved one"
        );
        for (frame, &expected) in original.iter().enumerate().skip(61) {
            paint(&mut nes, frame);
            nes.run_frame();
            assert_eq!(digest(&nes), expected, "replayed frame {frame} differs");
        }
    }

    #[test]
    fn a_rewound_snapshot_restores_registers_vram_and_the_apu() {
        let rom = nrom();
        let mut nes = Nes::new(&rom).unwrap();
        for frame in 0..40 {
            paint(&mut nes, frame);
            nes.run_frame();
        }
        // Sound, so the snapshot carries APU state the load has to bring back.
        for (addr, value) in [
            (0x4015, 0x0F),
            (0x4000, 0xBF),
            (0x4002, 0x40),
            (0x4003, 0x08),
        ] {
            nes.bus.apu.write_register(addr, value);
        }
        for _ in 0..4 {
            nes.run_frame();
        }
        let registers = (
            nes.cpu.a, nes.cpu.x, nes.cpu.y, nes.cpu.sp, nes.cpu.pc, nes.cpu.p,
        );
        let cycles = nes.cpu.cycles;
        let vram = nes.bus.ppu.vram;
        let apu = (
            nes.bus.apu.cycle,
            nes.bus.apu.frame_cycle,
            nes.bus.apu.pulse1.timer_period,
            nes.bus.apu.pulse1.length.value,
        );
        let snapshot = nes.save_state();
        let mut ring = Rewind::new(4);
        ring.push(snapshot.clone());
        // Play on with everything the state covers moved: registers, VRAM, APU and the picture.
        for frame in 40..52 {
            paint(&mut nes, frame);
            nes.run_frame();
        }
        nes.cpu.a = 0x5A;
        nes.cpu.x = 0xA5;
        nes.bus.ppu.vram[0] = 0xFF;
        nes.bus.apu.write_register(0x4000, 0x00);
        let rewound = ring.step_back().expect("one snapshot");
        nes.load_state(&rewound).unwrap();
        assert_eq!(
            (
                nes.cpu.a, nes.cpu.x, nes.cpu.y, nes.cpu.sp, nes.cpu.pc, nes.cpu.p
            ),
            registers,
            "CPU registers"
        );
        assert_eq!(nes.cpu.cycles, cycles, "CPU cycle counter");
        assert_eq!(nes.bus.ppu.vram, vram, "VRAM");
        assert_eq!(
            (
                nes.bus.apu.cycle,
                nes.bus.apu.frame_cycle,
                nes.bus.apu.pulse1.timer_period,
                nes.bus.apu.pulse1.length.value,
            ),
            apu,
            "APU"
        );
        assert_eq!(
            nes.save_state(),
            snapshot,
            "the whole machine must come back byte for byte"
        );
    }
}
