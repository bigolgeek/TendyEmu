//! Run-ahead: show the picture `k` frames into the future while the machine stays on
//! the committed frame.
//!
//! WHY the machine must not move: the next frame's input is applied to the state the last displayed
//! frame left behind. If the emulator really ran `k` frames ahead, the player's input for the next
//! frame would land `k` frames late in the game's own time — exactly the lag run-ahead exists to
//! remove — and every later frame would be off by the same amount. So the committed frame is the one
//! the machine keeps, and the frames past it are speculation: a `save_state()` before them, a
//! `load_state` after them.
//!
//! WHY the picture has to be copied out: the PPU's framebuffer and emphasis plane are part of the
//! save state (`crate::ppu::state`), so `load_state` puts the committed frame's picture back. The
//! speculative picture is only valid until that restore, which is why [`run_ahead_frame`] returns a
//! [`Picture`] that owns the two planes the frontend draws.
//!
//! WHY the samples are taken out and put back rather than truncated: `apu.samples` is a drain
//! buffer, not emulated state (`src/apu/state.rs`), and `Apu::load_state` clears it. Left alone, the
//! restore would throw away the committed frame's audio along with the speculation's, so the
//! committed samples are moved out before the speculative frames run and moved back after the
//! restore. The result is one frame of audio per displayed frame — the committed one, bit for bit.
//! The right-hand stereo stream (`samples_right`, and the right chain that fills it) is handled the
//! same way: `load_state` clears it and resets the chain, so both are put back too.
//!
//! WHY the lag flag is put back: `frame_pads_polled` is not in the save state, so the restore leaves
//! the last speculative frame's value in it. The committed frame's value is kept and written back.
//!
//! WHY `run_one` instead of calling [`Nes::run_frame`] here: how a frame is run belongs to the
//! frontend (a trace log, an event log, a movie player's own bookkeeping). This module decides only
//! the order of operations and what survives them. `run_one` is called `k + 1` times — once for the
//! committed frame and once per speculative one — and each call runs with whatever pads the machine
//! holds, so the committed input is held for as long as the caller does not move it: a frontend sets
//! the pads per frame around this call, which is the shape run-ahead needs.
//!
//! WHAT this costs: `k + 1` emulated frames, one `save_state` (~136 KiB on a small cartridge) and one
//! `load_state` per displayed frame, plus a 120 KiB picture copy. Nothing is added to the save state,
//! so `STATE_VERSION` does not move.
//!
//! WHAT is left to the frontend: the `--run-ahead` flag and its config key,
//! folding the extra frames into the pacer, and recording only the committed frame in a session
//! history ([`crate::history`]) — `History::step` runs *and* records one frame, so it cannot be the
//! `run_one` here without also recording the `k` speculative frames.

use std::fmt;

use crate::nes::Nes;

/// The two planes a frontend draws for one frame: 256 x 240 palette indices and the emphasis bits that
/// go with them (PPUMASK bits 5-7, one per pixel, parallel to the indices).
///
/// WHY this type exists at all: a run-ahead call ends by restoring the committed frame, which also
/// restores the PPU's own planes, so the speculative picture has to be a copy in the caller's hands by
/// then. It is built by [`Picture::of`] and by [`run_ahead_frame`], and it is deliberately separate
/// from the PPU: nothing here can disagree with the machine because nothing here writes to it.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    /// Palette indices (0..=63), row-major over 256 x 240: `ppu.framebuffer` as it was.
    pub framebuffer: Vec<u8>,
    /// PPUMASK >> 5 for each pixel, parallel to `framebuffer`: `ppu.emphasis` as it was.
    pub emphasis: Vec<u8>,
}

impl Picture {
    /// The picture the PPU is showing right now, copied out of the machine.
    pub fn of(nes: &Nes) -> Picture {
        Picture {
            framebuffer: nes.bus.ppu.framebuffer.clone(),
            emphasis: nes.bus.ppu.emphasis.clone(),
        }
    }

    /// Pixels per plane: 61 440 (256 x 240) on every region, so anything else means the plane came
    /// from somewhere other than [`Picture::of`].
    pub fn len(&self) -> usize {
        self.framebuffer.len()
    }

    /// True only for a picture that never saw a PPU; [`Picture::of`] always fills both planes.
    pub fn is_empty(&self) -> bool {
        self.framebuffer.is_empty()
    }
}

impl fmt::Debug for Picture {
    /// The sizes, never the pixels: a debug line must not turn into a 120 KiB dump.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Picture");
        d.field("framebuffer_bytes", &self.framebuffer.len());
        d.field("emphasis_bytes", &self.emphasis.len());
        d.finish()
    }
}

/// Run this frame's committed input, look `k` frames past it, and leave the machine on the committed
/// frame.
///
/// After the call the machine is one frame further on than it was (it ran the committed frame and then
/// took back the `k` speculative ones), `nes.bus.apu.samples` holds the committed frame's audio only,
/// and the returned [`Picture`] is what frame `t + k` looks like with the committed input held.
///
/// `run_one` runs exactly one frame with whatever pads the machine holds; see the module docs for why
/// it is a closure. The `k` speculative frames are not separate input frames: they are the committed
/// frame's input held, which works because the caller sets the pads before the call and does not move
/// them inside `run_one`.
///
/// `k = 0` is a plain frame: nothing is saved, restored or speculated, the picture is just copied out,
/// so the cost of run-ahead is exactly the `k + 1` frames and the state copy.
///
/// # Panics
///
/// If [`Nes::load_state`] rejects the state this call saved a moment earlier. That cannot happen while
/// the machine is the one that produced it: the only ways are a cartridge swapped underneath the call
/// (`rom_hash`) or another thread mutating the machine, and both are bugs in the caller. An error
/// return would make every caller handle a failure it cannot cause.
pub fn run_ahead_frame(nes: &mut Nes, k: usize, mut run_one: impl FnMut(&mut Nes)) -> Picture {
    run_one(nes);
    if k == 0 {
        return Picture::of(nes);
    }
    // The APU's drain buffer is not in the state and `load_state` clears it, so the committed frame's
    // samples leave the machine before the speculation runs and come back after the restore.
    let samples = std::mem::take(&mut nes.bus.apu.samples);
    let right = std::mem::take(&mut nes.bus.apu.samples_right);
    let right_chain = (nes.bus.apu.filter_right, nes.bus.apu.synth_right);
    let polled = nes.bus.frame_pads_polled;
    let state = nes.save_state();
    for _ in 0..k {
        run_one(nes);
    }
    let picture = Picture::of(nes);
    nes.load_state(&state)
        .expect("a state saved by this call loads back");
    nes.bus.apu.samples = samples;
    nes.bus.apu.samples_right = right;
    (nes.bus.apu.filter_right, nes.bus.apu.synth_right) = right_chain;
    nes.bus.frame_pads_polled = polled;
    picture
}
