//! Reset-time CPU/PPU realignment (`Nes::reset_aligned`).
//!
//! The reset button restarts the CPU but — on the default console — never touches the PPU, so every
//! reset lands on the same CPU/PPU relative phase. `reset_aligned(k)` advances the PPU clock by `k`
//! dots *before* the reset body, which shifts that phase without resetting anything in the PPU.
//! `reset()` is `reset_aligned(0)`, and the pins at the bottom were captured before `reset_aligned`
//! existed, so the default path is proven byte-identical.

use tendy_nes::nes::{Nes, PhaseChoice, PowerOn};
use tendy_nes::state::fnv1a64;

/// Dots per scanline (NTSC). Only used to fold `(scanline, dot)` into one comparable index.
const DOTS_PER_LINE: u32 = 341;

/// NROM image whose program sets palette entry 3, turns the background on and then spins:
/// `LDX`-free straight-line code at `$8000`, reset vector `$8000`, CHR bank filled with solid tile 0.
/// The picture is static, which makes the framebuffer digest a stable pin.
fn rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16384 + 8192, 0);
    let code = [
        0xA9, 0x3F, 0x8D, 0x06, 0x20, // LDA #$3F ; STA $2006
        0xA9, 0x00, 0x8D, 0x06, 0x20, // LDA #$00 ; STA $2006
        0xA9, 0x30, 0x8D, 0x07, 0x20, // LDA #$30 ; STA $2007  (palette[3] = white)
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E ; STA $2001  (rendering on)
        0xA9, 0x00, 0x8D, 0x05, 0x20, // LDA #$00 ; STA $2005
        0x8D, 0x05, 0x20, // STA $2005
        0xA9, 0x00, 0x8D, 0x00, 0x20, // LDA #$00 ; STA $2000
        0x4C, 0x21, 0x80, // JMP $8021
    ];
    v[16..16 + code.len()].copy_from_slice(&code);
    for b in v[16 + 16384..16 + 16384 + 8192].iter_mut() {
        *b = 0x01;
    }
    v[16 + 0x3FFC] = 0x00;
    v[16 + 0x3FFD] = 0x80;
    v
}

/// The PPU's position as one dot index. Every comparison below starts on vblank line 241, far from
/// the pre-render line's odd-frame skipped dot, so `scanline * 341 + dot` is exact here.
fn pos(nes: &Nes) -> u32 {
    u32::from(nes.bus.ppu.scanline) * DOTS_PER_LINE + u32::from(nes.bus.ppu.dot)
}

/// A console that has run two frames and then had the reset button pressed, optionally realigned.
fn after_reset(extra_dots: Option<u8>) -> Nes {
    let mut nes = Nes::new(&rom()).unwrap();
    for _ in 0..2 {
        nes.run_frame();
    }
    match extra_dots {
        Some(dots) => nes.reset_aligned(dots),
        None => nes.reset(),
    }
    nes
}

/// The default path is byte-identical to the tree before `reset_aligned` existed: after two frames
/// and again after a reset plus two frames, the framebuffer digest and `save_state()` bytes are the
/// pinned values captured before the quirk work. The quirks appended the `Ppu.warmup` byte, the 32
/// `Ppu.oam_stamp` values and the `Ppu.glitch_2006` tag byte and bumped `STATE_VERSION` to 4, and
/// then the Zapper's 12-byte block and the Vaus paddle's 8-byte block at the very end, bumping it
/// to 5, so the pins below are the pre-warm-up bytes and the test removes those additions before
/// comparing (offset = end of the PPU's state: `6 + 8 + cpu(21) + ram(2048) + cart(8196)` + PPU
/// state, before the warm-up byte).
/// The band-limited resynthesizer adds a 106-byte block to the APU's state, so the byte and
/// length pins here are re-derived from that tree (the framebuffer and PPU-position pins did not
/// move). `Ppu.vram` then grew from 2 KB to 4 KB, adding 2048 bytes before the warm-up byte and
/// bumping `STATE_VERSION` to 8, so the length, offset and byte pins below are re-derived again.
/// `Apu.frame_irq_new` was then removed, one byte at the end of the state, and the version bumped
/// to 9: the length and byte pins are re-derived once more, and the reconstruction is the
/// pre-warm-up layout minus that byte.
#[test]
fn default_reset_path_is_byte_identical() {
    const PIN_FRAME: u64 = 0x36C3_C14D_7673_0325;
    const PIN_STATE_LEN: usize = 138_068;
    const PIN_STATE: u64 = 0x3846_8948_5456_db6b;
    const PIN_FRAME_AFTER_RESET: u64 = 0x36C3_C14D_7673_0325;
    const PIN_STATE_AFTER_RESET: u64 = 0x9520_97bc_4148_9eb9;
    const WARMUP_STATE_OFFSET: usize = 137_774;
    /// The warm-up byte, the 32 OAM-decay stamps and the `glitch_2006` tag.
    const QUIRK_STATE_BYTES: usize = 1 + 32 * 8 + 1;
    /// The Vaus paddle's device block at the very end: a presence byte plus a default paddle per
    /// port.
    const PADDLE_STATE_BYTES: usize = 8;
    /// The Zapper's device block at the end of the state: a presence byte plus a default Zapper's
    /// aim and trigger per port.
    const ZAPPER_STATE_BYTES: usize = 12;
    /// The Four Score block: presence byte, pads 3/4 and read counters.
    const FOUR_SCORE_STATE_BYTES: usize = 9;
    /// The Power Pad block (`STATE_VERSION` 11): 7 bytes per port, between the Four Score and
    /// expansion blocks.
    const POWER_PAD_STATE_BYTES: usize = 14;
    /// The expansion-port block: kind byte and u16 length.
    const EXPANSION_STATE_BYTES: usize = 3;
    /// All device blocks, stripped together, in write order: paddle, Zapper, Four Score, Power Pad,
    /// expansion.
    const DEVICE_STATE_BYTES: usize = PADDLE_STATE_BYTES
        + ZAPPER_STATE_BYTES
        + FOUR_SCORE_STATE_BYTES
        + POWER_PAD_STATE_BYTES
        + EXPANSION_STATE_BYTES;

    /// The state with the later additions undone: the appended `warmup` byte and stamps dropped,
    /// the Zapper's trailing device block dropped and the version field put back to 1. Must equal
    /// the pre-warm-up bytes exactly.
    fn without_quirk_fields(state: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(PIN_STATE_LEN);
        v.extend_from_slice(&state[..WARMUP_STATE_OFFSET]);
        v.extend_from_slice(
            &state[WARMUP_STATE_OFFSET + QUIRK_STATE_BYTES..state.len() - DEVICE_STATE_BYTES],
        );
        v[4] = 1;
        v
    }

    let mut nes = Nes::new(&rom()).unwrap();
    for _ in 0..2 {
        nes.run_frame();
    }
    assert_eq!(
        fnv1a64(&nes.bus.ppu.framebuffer),
        PIN_FRAME,
        "framebuffer after two frames"
    );
    let state = nes.save_state();
    assert_eq!(
        state.len(),
        PIN_STATE_LEN + QUIRK_STATE_BYTES + DEVICE_STATE_BYTES,
        "save_state length"
    );
    assert_eq!(
        state[WARMUP_STATE_OFFSET], 0,
        "a default console's warm-up is off"
    );
    assert_eq!(
        fnv1a64(&without_quirk_fields(&state)),
        PIN_STATE,
        "save_state bytes"
    );
    assert_eq!(
        pos(&nes),
        241 * DOTS_PER_LINE + 8,
        "PPU position after two frames"
    );

    nes.reset();
    for _ in 0..2 {
        nes.run_frame();
    }
    assert_eq!(
        fnv1a64(&nes.bus.ppu.framebuffer),
        PIN_FRAME_AFTER_RESET,
        "framebuffer after reset plus two frames"
    );
    let state = nes.save_state();
    assert_eq!(
        state.len(),
        PIN_STATE_LEN + QUIRK_STATE_BYTES + DEVICE_STATE_BYTES,
        "save_state length after reset"
    );
    assert_eq!(
        state[WARMUP_STATE_OFFSET], 0,
        "still no warm-up after a default reset"
    );
    assert_eq!(
        fnv1a64(&without_quirk_fields(&state)),
        PIN_STATE_AFTER_RESET,
        "save_state bytes after reset"
    );
}

/// `reset()` is literally `reset_aligned(0)`: same CPU, APU, PPU and framebuffer bytes.
#[test]
fn reset_is_reset_aligned_zero() {
    let mut plain = Nes::new(&rom()).unwrap();
    for _ in 0..2 {
        plain.run_frame();
    }
    let mut aligned = Nes::new(&rom()).unwrap();
    for _ in 0..2 {
        aligned.run_frame();
    }
    assert_eq!(
        plain.save_state(),
        aligned.save_state(),
        "states before the reset must match"
    );

    plain.reset();
    aligned.reset_aligned(0);
    assert_eq!(
        plain.save_state(),
        aligned.save_state(),
        "reset() must equal reset_aligned(0)"
    );
    assert_eq!(pos(&plain), pos(&aligned));
    assert_eq!(plain.bus.ppu.framebuffer, aligned.bus.ppu.framebuffer);

    // And the equality survives actual emulation afterwards, not just the immediate state.
    for _ in 0..2 {
        plain.run_frame();
        aligned.run_frame();
    }
    assert_eq!(plain.save_state(), aligned.save_state());
}

/// Realigning by `k` dots moves the PPU clock exactly `k` dots further on, and nothing else in the
/// PPU changes: this is a phase shift, not a reset.
#[test]
fn reset_aligned_moves_the_ppu_clock_by_that_many_dots() {
    let base = after_reset(Some(0));
    for k in 1..=3u8 {
        let shifted = after_reset(Some(k));
        assert_eq!(
            pos(&shifted),
            pos(&base) + u32::from(k),
            "reset_aligned({k}) must advance the PPU by {k} dots"
        );
        assert_eq!(
            shifted.bus.ppu.scanline, base.bus.ppu.scanline,
            "same scanline for k <= 3 here"
        );
        assert_eq!(
            shifted.bus.ppu.dot,
            base.bus.ppu.dot + u16::from(k),
            "dot counter"
        );

        // Realignment only: the PPU's registers, VRAM and OAM are untouched, unlike a PPU reset.
        assert_eq!(shifted.bus.ppu.ctrl, base.bus.ppu.ctrl, "ppuctrl");
        assert_eq!(shifted.bus.ppu.mask, base.bus.ppu.mask, "ppumask");
        assert_eq!(shifted.bus.ppu.v, base.bus.ppu.v, "v");
        assert_eq!(shifted.bus.ppu.t, base.bus.ppu.t, "t");
        assert_eq!(shifted.bus.ppu.w, base.bus.ppu.w, "w");
        assert_eq!(shifted.bus.ppu.vram, base.bus.ppu.vram, "vram");
        assert_eq!(shifted.bus.ppu.oam, base.bus.ppu.oam, "oam");

        // The reset still drops interrupts; the phase shift must not raise one of its own.
        assert!(
            !shifted.bus.ppu.nmi_out,
            "realignment must not leave an NMI raised"
        );
        assert!(!shifted.bus.nmi_latch);
        assert!(!shifted.cpu.nmi_pending);
        assert_eq!(
            shifted.cpu.pc, 0x8000,
            "CPU still starts from the reset vector"
        );
    }
}

/// Default-off proof: the frontend's reset loop with no `--reset-align` flag picks phase 0
/// on every press, so eight resets through `power_on.reset_phase_for(n)` are byte-identical to
/// eight plain `reset()`es — and the framebuffer still carries the pre-127a pin, so the default
/// path has not moved.
#[test]
fn frontend_reset_loop_with_default_options_is_byte_identical() {
    const PIN_FRAME: u64 = 0x36C3_C14D_7673_0325;

    let power_on = PowerOn::default();
    let mut driven = Nes::new(&rom()).unwrap();
    let mut plain = Nes::new(&rom()).unwrap();
    for _ in 0..2 {
        driven.run_frame();
        plain.run_frame();
    }
    assert_eq!(
        driven.save_state(),
        plain.save_state(),
        "same machine before any reset"
    );
    for n in 0..8u32 {
        let phase = power_on.reset_phase_for(n);
        assert_eq!(phase, 0, "the default must not realign reset {n}");
        driven.reset_aligned(phase);
        plain.reset();
        assert_eq!(
            driven.save_state(),
            plain.save_state(),
            "save_state after reset {n}"
        );
        assert_eq!(pos(&driven), pos(&plain), "PPU phase after reset {n}");
        for _ in 0..2 {
            driven.run_frame();
            plain.run_frame();
        }
        assert_eq!(
            fnv1a64(&driven.bus.ppu.framebuffer),
            PIN_FRAME,
            "framebuffer after reset {n} plus two frames"
        );
        assert_eq!(
            driven.save_state(),
            plain.save_state(),
            "save_state after reset {n}"
        );
    }
}

/// Opt-in path: `--reset-align random` draws a fresh phase per press from `(seed, index)`,
/// and the frontend's counter/realign loop applies exactly that many dots each time.
#[test]
fn random_reset_align_derives_a_phase_per_press() {
    const PRESSES: u32 = 8;
    let power_on = PowerOn {
        reset_phase: PhaseChoice::Random,
        seed: 42,
        ..PowerOn::default()
    };
    let phases: Vec<u8> = (0..16).map(|n| power_on.reset_phase_for(n)).collect();
    assert!(
        phases.iter().any(|&p| p > 0),
        "random reset alignment never moved: {phases:?}"
    );
    assert!(
        phases.iter().all(|&p| p < 4),
        "phases must stay in 0..=3: {phases:?}"
    );

    // Each press moves the PPU by exactly its own phase, relative to a plain reset at the
    // same point: the loop's only effect is the `reset_aligned(phase)` call.
    for n in 0..PRESSES {
        let phase = power_on.reset_phase_for(n);
        let mut a = Nes::new(&rom()).unwrap();
        let mut b = Nes::new(&rom()).unwrap();
        for _ in 0..2 {
            a.run_frame();
            b.run_frame();
        }
        a.reset_aligned(phase);
        b.reset();
        assert_eq!(
            pos(&a),
            pos(&b) + u32::from(phase),
            "reset {n} (phase {phase})"
        );
    }

    // The whole loop is reproducible: same seed and press count, same states throughout.
    let run = || {
        let mut nes = Nes::new(&rom()).unwrap();
        for _ in 0..2 {
            nes.run_frame();
        }
        let mut states = Vec::new();
        for n in 0..PRESSES {
            nes.reset_aligned(power_on.reset_phase_for(n));
            nes.run_frame();
            states.push(nes.save_state());
        }
        states
    };
    assert_eq!(
        run(),
        run(),
        "the same seed must replay every reset identically"
    );
}
