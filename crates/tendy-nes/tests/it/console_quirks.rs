//! Console quirk plumbing (`src/console.rs`).
//!
//! The quirk set adds the toggles and the constructors that carry them, and no behaviour of its own: every
//! quirk is off (`ppu_2004_readable` on) until the code that implements it reads the field. The tests
//! here are the ones that keep that honest.
//!
//! * [`default_path_reproduces_the_pre_s2_bytes`] is the default-off proof. The
//!   pins were captured (framebuffer digest per frame, `save_state()` length and digest, drained
//!   audio sample bits) by a throwaway test before `src/console.rs` existed, and are asserted again
//!   here through **both** constructors. If the console constructor moved the default
//!   path by one byte, this fails before any baseline does.
//! * [`quirks_are_configuration_not_state`] is the "never serialised" rule: changing the quirks of a
//!   live console leaves its `save_state()` bytes untouched.
//! * the rest check that the set reaches the machine and the PPU, including the one derived field
//!   (`oam_pal_refresh`, which needs PAL timing) and the FDS constructor.

use tendy_nes::cart::{Region, split};
use tendy_nes::console::{ConsoleConfig, PpuQuirks, Quirks};
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::fds;
use tendy_nes::mapper::load;
use tendy_nes::nes::{Nes, PowerOn};
use tendy_nes::ppu::Ppu;
use tendy_nes::state::fnv1a64;

use crate::common::loop_rom;
use crate::common::program;

// ---------------------------------------------------------------------------------------------
// The pre-quirk capture (two frames, default console)
// ---------------------------------------------------------------------------------------------

/// FNV-1a 64 of the synthetic image: if this moves, the other pins are not comparable any more.
const PINNED_ROM: u64 = 0xf1bf_9cad_901d_bd40;
/// Framebuffer digest after frame 1 and frame 2, re-derived when the backdrop override landed:
/// this ROM never writes `$2001`, so it runs in forced blank, and it leaves `v` in palette RAM
/// (`$3F00` upward) - under the backdrop-override rule (`src/ppu/render.rs`) the screen therefore
/// shows the palette entry `v` addresses rather than entry 0, which is what hardware does.
const PINNED_FRAMES: [u64; 2] = [11_123_182_993_705_806_486, 2_569_953_806_806_307_085];
/// The pinned `save_state()` bytes carry `STATE_VERSION` 2 and end with the `warmup` byte; the
/// OAM-decay stamps then append 256 zero bytes and the pending `$2006` scroll-glitch landing one
/// zero tag byte, bumping the version to 4. Zapper support then appends a device block (one
/// presence byte plus a default gun's aim and trigger per port, 12 bytes) and the Vaus paddle an
/// 8-byte one (a presence byte plus its position, button and shift), both bumping the version to 5.
/// The band-limited resynthesizer adds a 106-byte block (level, ring position, sub-sample phase, 25
/// pending corrections) to the APU's state, so the length and byte pins below are re-derived from
/// the merged tree: the block is part of the baseline now, the quirk fields are still appended
/// after the PPU and still zero, and the device blocks are still last (the paddle before the gun).
/// `Ppu.vram` then grew from 2 KB to 4 KB, which adds 2048 bytes to the middle of the state and
/// bumps the version to 8: the length here and the warm-up offset below are re-derived for that
/// layout, and the byte pin still runs with the version and the appended quirk/device fields
/// removed. The one-byte `Apu.frame_irq_new` was then removed and the version bumped to 9, so the
/// length and byte pin are re-derived once more: the reconstruction below is the pre-warm-up
/// layout minus that byte, not the pre-warm-up state itself, and it still proves the appended quirk
/// and device fields are zero.
const PINNED_STATE_LEN: usize = 138_068;
/// FNV-1a 64 of the whole state. Re-derived with the backdrop override: `Ppu::save_state` carries
/// the framebuffer and the two-dot pipeline's `backdrop` byte, so the override moves this pin even
/// though every register, VRAM and palette byte it folds is unchanged. The resynthesizer and the
/// override were re-derived together when the two branches merged, so this is the merged tree's
/// value (both the synth block in the APU and the backdrop byte in the PPU are folded in).
const PINNED_STATE: u64 = 0x5a7d_6f98_f1b3_21e6;
/// Offset of the byte 128e appends, for this ROM and run: the end of the PPU's state, which starts
/// at `6 + 8 + cpu(21) + work RAM(2048) + cart(8196)` in `Nes::save_state`. If a PPU field is added
/// before `warmup`, this moves — that is a layout change and the pin must be re-derived, not patched.
const WARMUP_STATE_OFFSET: usize = 137_774;
/// The Vaus paddle's device block at the very end, written before the Zapper's: one presence byte
/// plus a default paddle's position, fire and shift per port, 4 bytes each.
const PADDLE_STATE_BYTES: usize = 8;
/// The 32 `u64` OAM-decay stamps after the warm-up byte (spec §4): 256 bytes, all
/// zero on the default path because the quirk is what writes a stamp and it is off.
const OAM_STAMP_STATE_OFFSET: usize = WARMUP_STATE_OFFSET + 1;
const OAM_STAMP_STATE_BYTES: usize = 32 * 8;
/// The `glitch_2006` tag byte after the stamps (spec §4): `0` (no pending update) on
/// the default path, because only the quirk arms it.
const GLITCH_2006_STATE_OFFSET: usize = OAM_STAMP_STATE_OFFSET + OAM_STAMP_STATE_BYTES;
const GLITCH_2006_STATE_BYTES: usize = 1;
/// The Zapper's device block after the last bus field (`last_2007`): one presence byte plus a
/// default Zapper's `x` (u16), `y` (u16) and trigger (bool) per port, 6 bytes each.
const ZAPPER_STATE_BYTES: usize = 12;
/// The Four Score block, after the Zapper block: a presence byte, pads 3 and 4 (3 bytes each) and
/// the two read counters, 9 bytes on a detached default.
const FOUR_SCORE_STATE_BYTES: usize = 9;
/// The Power Pad block (`STATE_VERSION` 11), between the Four Score and expansion blocks:
/// per port a presence byte plus a default mat's buttons (u16), latch (u16), read count and strobe,
/// 7 bytes each.
const POWER_PAD_STATE_BYTES: usize = 14;
/// The expansion-port block, last: a kind byte and a u16 length, no payload with nothing attached.
const EXPANSION_STATE_BYTES: usize = 3;
/// Drained APU samples over the two frames, as `f32::to_bits`.
const PINNED_AUDIO_SAMPLES: usize = 1408;
const PINNED_AUDIO: u64 = 0xeade_e4a5_a00f_c38b;

/// FNV-1a 64 over a byte slice: the same fold `tests/game_snapshots.rs` and `tests/determinism.rs`
/// use.
fn digest(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// A 32 KB NROM image with [`program`] at `$C000` (NROM-256's fixed bank) and all three vectors
/// pointing at it.
fn synthetic() -> Vec<u8> {
    let base = 0xC000u16;
    let (prg_kb, chr_kb) = (32usize, 8usize);
    let prg_banks = (prg_kb / 16) as u8;
    let chr_banks = (chr_kb / 8) as u8;
    let mut rom = vec![
        0x4E, 0x45, 0x53, 0x1A, prg_banks, chr_banks, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let prg_len = prg_banks as usize * 0x4000;
    rom.extend((0..prg_len).map(|i| (i / 0x4000) as u8));
    let code = program(base);
    let start = 16 + prg_len - 0x4000;
    rom[start..start + code.len()].copy_from_slice(&code);
    for vector in [16 + prg_len - 6, 16 + prg_len - 4, 16 + prg_len - 2] {
        rom[vector] = (base & 0xFF) as u8;
        rom[vector + 1] = (base >> 8) as u8;
    }
    rom.resize(16 + prg_len + chr_banks as usize * 0x2000, 0);
    rom
}

/// The console constructor under test.
fn boot_console(rom: &[u8], config: &ConsoleConfig) -> Nes {
    Nes::from_rom_data_console(split(rom).expect("parse rom"), fnv1a64(rom), config)
        .expect("build console")
}

/// What one two-frame run records: a framebuffer digest per frame, the final `save_state()` and the
/// drained audio sample bits.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    frames: Vec<u64>,
    state: Vec<u8>,
    audio: Vec<u32>,
}

fn run(mut nes: Nes) -> Run {
    let mut frames = Vec::new();
    let mut audio = Vec::new();
    for _ in 0..2 {
        nes.run_frame();
        frames.push(digest(&nes.bus.ppu.framebuffer));
        audio.extend(
            std::mem::take(&mut nes.bus.apu.samples)
                .iter()
                .map(|s| s.to_bits()),
        );
    }
    let state = nes.save_state();
    Run {
        frames,
        state,
        audio,
    }
}

/// Every toggle away from its default.
fn all_quirks() -> Quirks {
    Quirks {
        ppu_reset_on_reset: true,
        ppu_2004_readable: false,
        ppu_warmup: true,
        oam_decay: true,
        scroll_glitch_2006: true,
        first_write_glitch: true,
        nes101_port: true,
        famicom_port: true,
    }
}

// ---------------------------------------------------------------------------------------------
// The default-off proof
// ---------------------------------------------------------------------------------------------

/// The "any new option leaves the default path byte-identical" rule: a default
/// `ConsoleConfig` reproduces the pre-quirk console exactly — pinned framebuffer digests, pinned
/// `save_state()` bytes, pinned audio bits — through the plain constructor and through the new
/// console constructor. The framebuffer and state pins were re-derived once, by the backdrop
/// override (the state carries the framebuffer); the audio pins are still the pre-quirk samples.
#[test]
fn default_path_reproduces_the_pre_s2_bytes() {
    let rom = synthetic();
    assert_eq!(
        fnv1a64(&rom),
        PINNED_ROM,
        "the synthetic image moved; the pins are stale"
    );

    let plain = run(Nes::new(&rom).expect("Nes::new"));
    let console = run(boot_console(&rom, &ConsoleConfig::default()));

    for (label, got) in [("Nes::new", &plain), ("ConsoleConfig::default()", &console)] {
        assert_eq!(
            got.frames,
            PINNED_FRAMES.to_vec(),
            "{label}: framebuffer digests"
        );
        // The additions to the default path since the pin: one `0` byte at the end of the PPU's
        // state (the warm-up), 256 zero bytes (the OAM-decay stamps), one more `0` tag byte
        // (the pending scroll glitch), the 8-byte paddle block and the 12-byte Zapper
        // block at the very end (`STATE_VERSION` 5). Removing them must reproduce the pre-warm-up
        // bytes exactly.
        assert_eq!(
            got.state.len(),
            PINNED_STATE_LEN
                + 1
                + OAM_STAMP_STATE_BYTES
                + GLITCH_2006_STATE_BYTES
                + PADDLE_STATE_BYTES
                + ZAPPER_STATE_BYTES
                + FOUR_SCORE_STATE_BYTES
                + POWER_PAD_STATE_BYTES
                + EXPANSION_STATE_BYTES,
            "{label}: save_state length"
        );
        assert_eq!(&got.state[4..6], &[12, 0], "{label}: STATE_VERSION 12");
        assert_eq!(
            got.state[WARMUP_STATE_OFFSET], 0,
            "{label}: a default console's warm-up is off"
        );
        assert!(
            got.state[OAM_STAMP_STATE_OFFSET..OAM_STAMP_STATE_OFFSET + OAM_STAMP_STATE_BYTES]
                .iter()
                .all(|&b| b == 0),
            "{label}: a default console's OAM-decay stamps are all zero"
        );
        assert_eq!(
            got.state[GLITCH_2006_STATE_OFFSET], 0,
            "{label}: a default console has no pending $2006 scroll glitch"
        );
        assert_eq!(
            fnv1a64(&state_without_the_quirk_fields(&got.state)),
            PINNED_STATE,
            "{label}: save_state bytes, with the quirk fields and the version bump undone"
        );
        assert_eq!(
            got.audio.len(),
            PINNED_AUDIO_SAMPLES,
            "{label}: audio sample count"
        );
        let audio_bytes: Vec<u8> = got.audio.iter().flat_map(|b| b.to_le_bytes()).collect();
        assert_eq!(digest(&audio_bytes), PINNED_AUDIO, "{label}: audio samples");
    }
    assert_eq!(
        plain, console,
        "the two constructors must build the same console"
    );
}

/// A `save_state()` with the later additions undone: the appended `Ppu.warmup` byte, the 32
/// `Ppu.oam_stamp` values, the `Ppu.glitch_2006` tag byte and the two trailing device blocks
/// (the paddle, the Zapper) dropped, and `STATE_VERSION` put back to 1. Equals the
/// pre-warm-up state minus the one-byte `Apu.frame_irq_new` that was removed (the v9 shrink), so it
/// is not byte for byte the pre-warm-up state.
fn state_without_the_quirk_fields(state: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(PINNED_STATE_LEN);
    v.extend_from_slice(&state[..WARMUP_STATE_OFFSET]);
    v.extend_from_slice(
        &state[GLITCH_2006_STATE_OFFSET + GLITCH_2006_STATE_BYTES
            ..state.len()
                - PADDLE_STATE_BYTES
                - ZAPPER_STATE_BYTES
                - FOUR_SCORE_STATE_BYTES
                - POWER_PAD_STATE_BYTES
                - EXPANSION_STATE_BYTES],
    );
    v[4] = 1;
    v
}

/// `from_rom_data_power_on` is the console constructor with default quirks, so it must be
/// byte-identical to it — for a non-default power-on too, where the RAM/alignment path runs.
#[test]
fn power_on_constructor_delegates_with_default_quirks() {
    let rom = synthetic();
    let power_on = PowerOn {
        seed: 7,
        ..PowerOn::default()
    };
    let via_power_on =
        run(Nes::from_rom_data_power_on(split(&rom).unwrap(), fnv1a64(&rom), &power_on).unwrap());
    let via_console = run(boot_console(&rom, &ConsoleConfig::new(power_on)));
    assert_eq!(via_power_on, via_console);
}

// ---------------------------------------------------------------------------------------------
// Configuration, not state
// ---------------------------------------------------------------------------------------------

/// Quirks are never serialised: flipping every toggle on a live console leaves `save_state()` bytes
/// untouched. This is the property that lets a state file be shared between quirks, and it stays
/// true as the individual behaviours land.
#[test]
fn quirks_are_configuration_not_state() {
    let rom = synthetic();
    let mut nes = boot_console(&rom, &ConsoleConfig::default());
    nes.run_frame();
    nes.run_frame();
    let before = nes.save_state();

    nes.quirks = all_quirks();
    nes.bus.ppu.quirks = all_quirks().ppu(nes.region());

    assert_eq!(
        before,
        nes.save_state(),
        "quirks leaked into the save state"
    );
}

// ---------------------------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------------------------

/// The set reaches the console and the PPU's own copy, field for field.
#[test]
fn quirks_reach_the_console_and_the_ppu() {
    let rom = synthetic();
    let quirks = all_quirks();
    let nes = boot_console(
        &rom,
        &ConsoleConfig::with_quirks(PowerOn::default(), quirks),
    );

    assert_eq!(nes.quirks, quirks, "Nes::quirks");
    assert_eq!(nes.bus.ppu.quirks, quirks.ppu(Region::Ntsc), "Ppu::quirks");
    assert_eq!(
        nes.bus.ppu.quirks,
        PpuQuirks {
            ppu_2004_readable: false,
            ppu_warmup: true,
            oam_decay: true,
            oam_pal_refresh: false, // NTSC has no 265-310 refresh window
            scroll_glitch_2006: true,
            first_write_glitch: true,
        }
    );

    // A default console carries the defaults, and a `Ppu` built on its own does too.
    let default = Nes::new(&rom).unwrap();
    assert_eq!(default.quirks, Quirks::default());
    assert_eq!(default.bus.ppu.quirks, PpuQuirks::default());
    assert_eq!(Ppu::new().quirks, PpuQuirks::default());
    assert_eq!(Ppu::new_aligned(3).quirks, PpuQuirks::default());
}

/// `oam_pal_refresh` is derived from `oam_decay` **and** PAL timing, at construction and again if
/// the region changes under a live PPU.
#[test]
fn pal_timing_derives_the_refresh_window() {
    let quirks = Quirks {
        oam_decay: true,
        ..Quirks::default()
    };
    let config = ConsoleConfig::with_quirks(PowerOn::default(), quirks);

    let pal = boot_console(&loop_rom(0x08, 1), &config);
    assert_eq!(pal.region(), Region::Pal);
    assert!(
        pal.bus.ppu.quirks.oam_pal_refresh,
        "PAL refreshes OAM rows 265-310"
    );

    let ntsc = boot_console(&loop_rom(0x08, 0), &config);
    assert_eq!(ntsc.region(), Region::Ntsc);
    assert!(!ntsc.bus.ppu.quirks.oam_pal_refresh);

    // Decay off: no refresh window anywhere.
    let off = boot_console(&loop_rom(0x08, 1), &ConsoleConfig::default());
    assert!(!off.bus.ppu.quirks.oam_pal_refresh);

    // A region change after construction re-derives it.
    let mut ppu = Ppu::new();
    ppu.quirks.oam_decay = true;
    ppu.set_region(Region::Pal);
    assert!(ppu.quirks.oam_pal_refresh);
    ppu.set_region(Region::Ntsc);
    assert!(!ppu.quirks.oam_pal_refresh);
}

// ---------------------------------------------------------------------------------------------
// The FDS constructor
// ---------------------------------------------------------------------------------------------

/// A one-side disk: disk-info block, one file, four bytes of file data (`tests/fds.rs`'s builder).
fn disk_image() -> Vec<u8> {
    let mut side = vec![0u8; fds::SIDE_LEN];
    side[0] = 1; // disk info block
    side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
    side[0x38] = 2; // file-count block
    side[0x39] = 1;
    side[0x3A] = 3; // file header block
    side[0x3A + 0x0B] = 0x00;
    side[0x3A + 0x0C] = 0x60;
    side[0x3A + 0x0D] = 0x04;
    side[0x3A + 0x10] = 4; // file data block
    side[0x3A + 0x11..0x3A + 0x15].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let mut image = Vec::from(&fds::FDS_MAGIC[..]);
    image.push(1);
    image.extend_from_slice(&[0u8; 11]);
    image.extend_from_slice(&side);
    image
}

/// An 8 KB BIOS stub: `NOP` fill, `code` at `$E000`, reset vector at `$E000`.
fn bios_with(code: &[u8]) -> Vec<u8> {
    let mut bios = vec![0xEAu8; fds::BIOS_LEN];
    bios[..code.len()].copy_from_slice(code);
    bios[0x1FFC] = 0x00;
    bios[0x1FFD] = 0xE0;
    bios
}

/// The FDS path honours the same quirks, and `from_fds_power_on` is still the default-quirks
/// version of it.
#[test]
fn fds_console_honours_quirks() {
    let (disk, bios) = (disk_image(), bios_with(&[0xA9, 0x42, 0x4C, 0x00, 0xE0]));
    let quirks = Quirks {
        ppu_warmup: true,
        oam_decay: true,
        ..Quirks::default()
    };
    let config = ConsoleConfig::with_quirks(PowerOn::default(), quirks);

    let mut fds = Nes::from_fds_console(&disk, &bios, false, &config).expect("mount");
    assert_eq!(fds.quirks, quirks);
    assert_eq!(fds.bus.ppu.quirks, quirks.ppu(Region::Ntsc));
    assert!(fds.bus.ppu.quirks.ppu_warmup);

    // Non-default quirks are not serialised here either.
    fds.run_frame();
    fds.run_frame();
    let before = fds.save_state();
    fds.quirks = Quirks::default();
    fds.bus.ppu.quirks = Quirks::default().ppu(Region::Ntsc);
    assert_eq!(
        before,
        fds.save_state(),
        "quirks leaked into the FDS save state"
    );

    // The delegating constructor is byte-identical to the console one with default quirks.
    let mut plain =
        Nes::from_fds_power_on(&disk, &bios, false, &PowerOn::default()).expect("mount");
    let mut by_console =
        Nes::from_fds_console(&disk, &bios, false, &ConsoleConfig::default()).expect("mount");
    for _ in 0..2 {
        plain.run_frame();
        by_console.run_frame();
    }
    assert_eq!(plain.save_state(), by_console.save_state());
    assert_eq!(plain.quirks, Quirks::default());
}

// ---------------------------------------------------------------------------------------------
// §2.2 `ppu_2004_readable = false`: OAMDATA is write-only
// ---------------------------------------------------------------------------------------------

/// With the quirk on, a `$2004` read returns PPU open bus instead of OAM — outside rendering. The
/// write still lands in OAM and still steps OAMADDR, and the read must leave both alone.
#[test]
fn no_2004_read_returns_ppu_open_bus() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.quirks.ppu_2004_readable = false;

    ppu.write_register(cart.as_mut(), 0x2003, 0x00); // OAMADDR = $00, I/O latch = $00
    ppu.write_register(cart.as_mut(), 0x2004, 0x5A); // oam[0] = $5A, OAMADDR = $01, latch = $5A
    ppu.write_register(cart.as_mut(), 0x2003, 0x00); // latch back to $00

    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0x00,
        "write-only $2004 must read PPU open bus, not OAM ($5A)"
    );
    // A write to the read-only $2002 still sets the I/O latch, so $2004 must return that value.
    ppu.write_register(cart.as_mut(), 0x2002, 0xC3);
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0xC3,
        "the read returns the latch, not OAM"
    );

    assert_eq!(ppu.oam[0], 0x5A, "the write still landed in OAM");
    assert_eq!(ppu.oam_read(0), 0x5A, "OAM contents are intact");
    assert_eq!(ppu.oam_addr, 0x00, "reads must not step OAMADDR");
}

/// Inside the render window the quirk wins over the `oam_latch` path: a read returns open bus, not
/// the OAM read buffer.
#[test]
fn no_2004_read_while_rendering_returns_open_bus() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.quirks.ppu_2004_readable = false;

    ppu.write_register(cart.as_mut(), 0x2001, 0x18); // rendering on, I/O latch = $18
    // Scanline 10, dot 100: a visible line with rendering enabled, i.e. inside the render window.
    for _ in 0..(10 * 341 + 100) {
        ppu.tick(cart.as_mut());
    }
    assert_eq!((ppu.scanline, ppu.dot), (10, 100), "tick position");

    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0x18,
        "open bus, not the oam_latch the default path returns"
    );
}

/// The default is unchanged: with `PpuQuirks::default()` the same sequence reads OAM, which is the
/// behaviour AccuracyCoin's page-20 `$2004` rows pin.
#[test]
fn ppu_2004_readable_by_default() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    assert!(ppu.quirks.ppu_2004_readable, "the quirk is off by default");

    ppu.write_register(cart.as_mut(), 0x2003, 0x00);
    ppu.write_register(cart.as_mut(), 0x2004, 0x5A);
    ppu.write_register(cart.as_mut(), 0x2003, 0x00);

    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0x5A,
        "default path reads OAM"
    );
    assert_eq!(ppu.oam_addr, 0x00, "reads must not step OAMADDR");
}

// ---------------------------------------------------------------------------------------------
// The reset button's PPU half (`ppu_reset_on_reset`)
// ---------------------------------------------------------------------------------------------

/// A console whose reset button also resets the PPU, everything else unchanged.
fn ppu_reset_console() -> Nes {
    boot_console(
        &loop_rom(0x08, 0),
        &ConsoleConfig::with_quirks(
            PowerOn::default(),
            Quirks {
                ppu_reset_on_reset: true,
                ..Quirks::default()
            },
        ),
    )
}

/// Arm every register the PPU's reset line clears, then hand the state back. `$2000=$10` keeps NMI
/// off (`loop_rom` has no NMI handler), `$2001=$1E` turns rendering on, the two `$2005` writes give
/// `t=$216F`/`fine_x=5`, `$2006=$21` arms the second half of the address and leaves the toggle set,
/// and the read buffer is a value no register write could produce.
fn arm_reset_line_state(nes: &mut Nes) {
    nes.bus.write(0x2000, 0x10);
    nes.bus.write(0x2001, 0x1E);
    nes.bus.write(0x2005, 0x7D);
    nes.bus.write(0x2005, 0x5E);
    nes.bus.write(0x2006, 0x21);
    nes.bus.ppu.read_buffer = 0xAA;
}

/// The registers on the PPU's reset line go back to their power-on values and the PPU restarts at
/// the top of an even frame (nesdev "PPU power up state"; spec §2.1 table).
#[test]
fn ppu_reset_clears_the_reset_line_registers() {
    let mut nes = ppu_reset_console();
    nes.run_frame();
    arm_reset_line_state(&mut nes);
    let armed = &nes.bus.ppu;
    assert_eq!(
        (
            armed.ctrl,
            armed.mask,
            armed.t,
            armed.fine_x,
            armed.w,
            armed.read_buffer
        ),
        (0x10, 0x1E, 0x216F, 5, true, 0xAA),
        "the writes must arm the state this test resets away"
    );

    nes.reset();

    let ppu = &nes.bus.ppu;
    assert_eq!(ppu.ctrl, 0, "$2000 cleared");
    assert_eq!(ppu.mask, 0, "$2001 cleared");
    assert_eq!(
        ppu.t, 0,
        "the PPUSCROLL/PPUADDR latch (t) and fine X cleared"
    );
    assert_eq!(ppu.fine_x, 0, "fine X cleared");
    assert!(!ppu.w, "the $2005/$2006 write toggle cleared");
    assert_eq!(ppu.read_buffer, 0, "the $2007 read buffer cleared");
    assert_eq!(
        (ppu.scanline, ppu.dot),
        (0, 6),
        "top of the picture, plus the six dots the CPU's two reset-vector reads tick"
    );
    assert_eq!(
        ppu.frame % 2,
        0,
        "an even frame, so the pre-render line keeps its last dot"
    );
}

/// Everything the reset line does not touch survives: `v` (the address itself, not the latch),
/// `$2002`, OAMADDR, OAM, VRAM, palette RAM and the PPU I/O latch (spec §2.1 "kept" row).
#[test]
fn ppu_reset_keeps_v_oamaddr_status_and_memories() {
    let mut nes = ppu_reset_console();
    nes.run_frame();
    {
        let ppu = &mut nes.bus.ppu;
        ppu.v = 0x2345;
        ppu.oam_addr = 0x40;
        ppu.status = 0xE0;
        ppu.oam[5] = 0x12;
        ppu.vram[7] = 0x34;
        ppu.palette[1] = 0x15;
        ppu.io_latch = 0xC3;
    }

    nes.reset();

    let ppu = &nes.bus.ppu;
    assert_eq!(ppu.v, 0x2345, "V is not on the reset line");
    assert_eq!(ppu.oam_addr, 0x40, "OAMADDR is not on the reset line");
    assert_eq!(ppu.status, 0xE0, "$2002 keeps its VBL / sprite flags");
    assert_eq!(ppu.oam[5], 0x12, "OAM survives");
    assert_eq!(ppu.vram[7], 0x34, "VRAM survives");
    assert_eq!(ppu.palette[1], 0x15, "palette RAM survives");
    assert_eq!(ppu.io_latch, 0xC3, "the PPU I/O latch survives");
    // Only the reset path moves the position, so this is also the proof the reset above ran.
    assert_eq!(
        (ppu.scanline, ppu.dot),
        (0, 6),
        "the PPU restarted at the top of the picture"
    );
}

/// The PPU comes out of a reset on an even frame, so the first pre-render line after it never skips
/// its last dot — and `frame` stays monotonic instead of restarting at 0.
#[test]
fn ppu_reset_rounds_frame_up_to_even() {
    let mut nes = ppu_reset_console();
    nes.bus.ppu.frame = 7;
    nes.reset();
    assert_eq!(nes.bus.ppu.frame, 8, "an odd frame rounds up");

    nes.bus.ppu.frame = 8;
    nes.reset();
    assert_eq!(nes.bus.ppu.frame, 8, "an even frame stays");
}

/// The default path never reaches `Ppu::reset`: a `Quirks::default()` console resets exactly like the
/// pre-quirk constructors do, byte for byte, with every armed register still armed. Run against a
/// quirk-on console as well, so the test cannot pass by the branch being dead.
#[test]
fn default_reset_leaves_the_ppu_alone() {
    let rom = loop_rom(0x08, 0);

    let run = |mut nes: Nes| {
        for _ in 0..2 {
            nes.run_frame();
        }
        arm_reset_line_state(&mut nes);
        nes.reset();
        nes.run_frame();
        let ppu = &nes.bus.ppu;
        let regs = (ppu.ctrl, ppu.mask, ppu.t, ppu.w, ppu.read_buffer);
        (nes.save_state(), digest(&ppu.framebuffer), regs)
    };

    let plain = run(Nes::from_rom_data_power_on(
        split(&rom).expect("parse rom"),
        fnv1a64(&rom),
        &PowerOn::default(),
    )
    .expect("build console"));
    let console = run(boot_console(&rom, &ConsoleConfig::default()));

    assert_eq!(
        plain.2,
        (0x10, 0x1E, 0x216F, true, 0xAA),
        "the default reset must leave $2000/$2001/t/w/$2007-buffer alone"
    );
    assert_eq!(
        plain, console,
        "default `Quirks` must reset the PPU exactly as today"
    );
    assert_eq!(
        plain.0, console.0,
        "save_state() bytes after the reset and one more frame"
    );
    assert_eq!(
        plain.1, console.1,
        "framebuffer digest after the reset and one more frame"
    );

    // Non-vacuity: the same script on a quirk-on console must end somewhere else.
    let quirk = run(ppu_reset_console());
    assert_ne!(quirk.0, plain.0, "the quirk path must move the state");
    assert_ne!(
        quirk.2, plain.2,
        "the quirk path must clear the reset-line registers"
    );
}

// ---------------------------------------------------------------------------------------------
// The PPU write warm-up (`ppu_warmup`)
// ---------------------------------------------------------------------------------------------

/// The quirk on its own: the console has a warm-up, but its reset button still leaves the PPU alone.
fn warmup_only() -> Quirks {
    Quirks {
        ppu_warmup: true,
        ..Quirks::default()
    }
}

fn warmup_console(timing: u8) -> Nes {
    boot_console(
        &loop_rom(0x08, timing),
        &ConsoleConfig::with_quirks(PowerOn::default(), warmup_only()),
    )
}

/// While the reset signal is up, `$2000`, `$2001`, `$2005` and `$2006` are ignored — the scroll
/// latch does not even toggle — but the write still lands in the PPU's I/O latch (spec §2.3; Mesen2
/// runs `SetOpenBus` before its early return).
#[test]
fn warmup_ignores_2000_2001_2005_2006() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.warmup = true;

    ppu.write_register(cart.as_mut(), 0x2000, 0x80);
    ppu.write_register(cart.as_mut(), 0x2001, 0x1E);
    ppu.write_register(cart.as_mut(), 0x2005, 0x7D);
    ppu.write_register(cart.as_mut(), 0x2006, 0x21);

    assert_eq!(
        (ppu.ctrl, ppu.mask, ppu.t, ppu.fine_x, ppu.w),
        (0, 0, 0, 0, false),
        "all four registers and the latch are untouched by a gated write"
    );
    assert_eq!(ppu.io_latch, 0x21, "the I/O latch still takes the write");
}

/// The registers the reset signal does *not* gate keep working during the warm-up. `$2007` differs
/// from a plain read in one way only: the read buffer is held clear, so a buffered read returns 0
/// while `v` still steps and palette reads still return palette data (spec §2.3, nesdev over Mesen).
#[test]
fn warmup_keeps_2002_2003_2004_2007_working() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.warmup = true;

    // $2003 / $2004: OAM writes work normally.
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    ppu.write_register(cart.as_mut(), 0x2004, 0xAB);
    assert_eq!(ppu.oam[0x10], 0xAB, "the OAM write landed");
    assert_eq!(ppu.oam_addr, 0x11, "and stepped OAMADDR");

    // $2007: the nametable read goes through, but the buffer it fills is held clear.
    ppu.v = 0x2000;
    ppu.vram[0] = 0x55;
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2007),
        0,
        "the buffered read is held clear during the warm-up"
    );
    assert_eq!(ppu.read_buffer, 0, "and the buffer stays clear");
    assert_eq!(ppu.v, 0x2001, "`v` still steps");
    ppu.v = 0x2000;
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2007),
        0,
        "without the warm-up this read would return the buffered $55"
    );

    // $2002: the vblank flag is readable and the read clears it.
    ppu.status = 0x80;
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2002) & 0x80,
        0x80,
        "$2002 bit 7 is set"
    );
    assert_eq!(ppu.status & 0x80, 0, "$2002 read still clears vblank");
}

/// The warm-up ends on the pre-render line's dot 1 — the same signal that clears the vblank, sprite
/// 0 and overflow flags — not at a cycle constant (spec §2.3, §0.3).
#[test]
fn warmup_clears_on_prerender_dot_1() {
    let mut cart = load(&loop_rom(0x08, 0)).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.warmup = true;

    while !(ppu.scanline == 261 && ppu.dot == 1) {
        ppu.tick(cart.as_mut());
    }
    assert!(ppu.warmup, "still armed on the dot before the clear");
    ppu.write_register(cart.as_mut(), 0x2000, 0x80);
    assert_eq!(ppu.ctrl, 0, "a write on dot 1 itself is still ignored");

    ppu.tick(cart.as_mut());
    assert!(
        !ppu.warmup,
        "the pre-render line's dot 1 clears the reset signal"
    );
    ppu.write_register(cart.as_mut(), 0x2000, 0x80);
    assert_eq!(ppu.ctrl, 0x80, "and the next write lands");
}

/// The NTSC boundary from power-on, counted in `SysBus::cycles` at the access: the last ignored
/// access is cycle 29667, the first accepted one 29668 (spec §2.3 table; 261 * 341 + 1 dots).
#[test]
fn warmup_boundary_ntsc_power_on() {
    let mut nes = warmup_console(0);
    assert!(nes.bus.ppu.warmup, "power-on arms the warm-up");

    while nes.bus.cycles < 29666 {
        nes.bus.read(0x0000);
    }
    nes.bus.write(0x2001, 0x01);
    assert_eq!(nes.bus.cycles, 29667, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0,
        "the last access before the clear is ignored"
    );

    nes.bus.write(0x2001, 0x02);
    assert_eq!(nes.bus.cycles, 29668, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0x02,
        "the first access after the clear lands"
    );
}

/// PAL runs 3.2 dots per CPU cycle and clears the warm-up on line 311, so its boundary is 33141 /
/// 33142. PAL's post-reset boundary depends on `cycles % 5`, so this is the power-on case only.
#[test]
fn warmup_boundary_pal_power_on() {
    let mut nes = warmup_console(1);
    assert_eq!(nes.region(), Region::Pal);
    assert_eq!(nes.bus.ppu.pre_render_line, 311);

    while nes.bus.cycles < 33140 {
        nes.bus.read(0x0000);
    }
    nes.bus.write(0x2001, 0x01);
    assert_eq!(nes.bus.cycles, 33141, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0,
        "the last access before the clear is ignored"
    );

    nes.bus.write(0x2001, 0x02);
    assert_eq!(nes.bus.cycles, 33142, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0x02,
        "the first access after the clear lands"
    );
}

/// Dendy has PAL's 312-line frame but 3 dots per CPU cycle, so its boundary is 35350 / 35351.
#[test]
fn warmup_boundary_dendy_power_on() {
    let mut nes = warmup_console(3);
    assert_eq!(nes.region(), Region::Dendy);
    assert_eq!(nes.bus.ppu.pre_render_line, 311);

    while nes.bus.cycles < 35349 {
        nes.bus.read(0x0000);
    }
    nes.bus.write(0x2001, 0x01);
    assert_eq!(nes.bus.cycles, 35350, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0,
        "the last access before the clear is ignored"
    );

    nes.bus.write(0x2001, 0x02);
    assert_eq!(nes.bus.cycles, 35351, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0x02,
        "the first access after the clear lands"
    );
}

/// A warm-up is re-armed only by `Ppu::reset`, i.e. only when the reset button also resets the PPU.
/// With `ppu_warmup` alone the reset leaves the PPU alone and writes land at once (spec §2.3 table).
///
/// The cycle figures are the NTSC ones offset by what the reset itself costs: `Nes::reset` reads the
/// reset vector twice (two CPU cycles, six PPU dots) and the PPU restarts at dot 0, so the clear
/// comes 29666 cycles after those reads — `r + 29668` in `SysBus::cycles`, `r + 29667` still armed.
#[test]
fn warmup_rearms_on_reset_only_with_ppu_reset() {
    let quirks = Quirks {
        ppu_warmup: true,
        ppu_reset_on_reset: true,
        ..Quirks::default()
    };
    let mut nes = boot_console(
        &loop_rom(0x08, 0),
        &ConsoleConfig::with_quirks(PowerOn::default(), quirks),
    );
    for _ in 0..2 {
        nes.run_frame();
    }
    assert!(
        !nes.bus.ppu.warmup,
        "the first frame's pre-render line cleared the warm-up"
    );

    let r = nes.bus.cycles;
    nes.reset();
    assert!(nes.bus.ppu.warmup, "the PPU reset re-arms the warm-up");
    assert_eq!(
        (nes.bus.ppu.scanline, nes.bus.ppu.dot),
        (0, 6),
        "the PPU restarted"
    );

    while nes.bus.cycles < r + 29666 {
        nes.bus.read(0x0000);
    }
    nes.bus.write(0x2001, 0x01);
    assert_eq!(nes.bus.cycles, r + 29667, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0,
        "the last access before the re-armed clear is ignored"
    );

    nes.bus.write(0x2001, 0x02);
    assert_eq!(nes.bus.cycles, r + 29668, "the access cycle");
    assert_eq!(
        nes.bus.ppu.mask, 0x02,
        "the first access after the clear lands"
    );

    // Warm-up only: the reset button does not touch the PPU, so nothing is re-armed.
    let mut nes = warmup_console(0);
    for _ in 0..2 {
        nes.run_frame();
    }
    assert!(
        !nes.bus.ppu.warmup,
        "cleared before the reset (run_frame stops at vblank)"
    );
    nes.reset();
    assert!(!nes.bus.ppu.warmup, "no PPU reset, no warm-up");
    nes.bus.write(0x2001, 0x01);
    assert_eq!(nes.bus.ppu.mask, 0x01, "the write is accepted at once");
}

/// An NROM image whose program is `LDA #$80 / STA $2000 / loop: JMP loop` at `$8000`, with all three
/// vectors pointing at `$8000` so an NMI would restart the same code.
fn warmup_program_rom() -> Vec<u8> {
    let mut rom = vec![
        0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0x08, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    rom.resize(16 + 16384 + 8192, 0);
    rom[16..24].copy_from_slice(&[0xA9, 0x80, 0x8D, 0x00, 0x20, 0x4C, 0x05, 0x80]);
    for vector in [16 + 0x3FFA, 16 + 0x3FFC, 16 + 0x3FFE] {
        rom[vector] = 0x00;
        rom[vector + 1] = 0x80;
    }
    rom
}

/// End to end on a real program: with the `nes` preset's quirks on, the boot-time `STA $2000` is
/// dropped, so `$2000` stays clear, no NMI is taken (SP is still where the reset left it) and the
/// CPU sits in its loop. Without the quirk the same image ends up with NMI enabled.
#[test]
fn warmup_cpu_program() {
    let rom = warmup_program_rom();
    let quirks = Quirks {
        ppu_reset_on_reset: true,
        ppu_warmup: true,
        ..Quirks::default()
    };
    let mut nes = boot_console(
        &rom,
        &ConsoleConfig::with_quirks(PowerOn::default(), quirks),
    );
    nes.run_frame();
    assert_eq!(nes.bus.ppu.ctrl, 0, "the program's STA $2000 was ignored");
    assert_eq!(nes.cpu.pc, 0x8005, "the CPU is still in its loop");
    assert_eq!(nes.cpu.sp, 0xFD, "no NMI was taken (only the reset pushed)");

    let mut plain = boot_console(&rom, &ConsoleConfig::default());
    plain.run_frame();
    assert_eq!(
        plain.bus.ppu.ctrl, 0x80,
        "without the quirk the write lands"
    );
}

/// The warm-up is emulated state, so a state saved mid-warm-up must carry it: both machines cross
/// the boundary on the same dot and agree afterwards (spec §4, `STATE_VERSION` 2).
#[test]
fn warmup_state_roundtrip() {
    let quirks = Quirks {
        ppu_warmup: true,
        ppu_reset_on_reset: true,
        ..Quirks::default()
    };
    let config = ConsoleConfig::with_quirks(PowerOn::default(), quirks);

    let mut a = boot_console(&loop_rom(0x08, 0), &config);
    for _ in 0..20 {
        a.step();
    }
    assert!(a.bus.ppu.warmup, "still inside the warm-up");
    let state = a.save_state();

    let mut b = boot_console(&loop_rom(0x08, 0), &config);
    b.load_state(&state).expect("load");
    assert!(b.bus.ppu.warmup, "the warm-up survives the state");
    assert_eq!(
        a.save_state(),
        state,
        "and saving the loaded console is a fixpoint"
    );

    for _ in 0..2 {
        a.run_frame();
        b.run_frame();
    }
    assert!(!a.bus.ppu.warmup, "both crossed the boundary");
    assert_eq!(
        a.save_state(),
        b.save_state(),
        "states agree past the boundary"
    );
    assert_eq!(
        a.bus.ppu.framebuffer, b.bus.ppu.framebuffer,
        "and so do the pictures"
    );
}

/// The default-off proof for this quirk on its own: a default console has no warm-up, so the first
/// writes of a program land, and the byte the state carries is `0`.
#[test]
fn warmup_off_by_default() {
    let mut nes = boot_console(&loop_rom(0x08, 0), &ConsoleConfig::default());
    assert!(!nes.bus.ppu.warmup);
    nes.bus.write(0x2001, 0x1E);
    assert_eq!(
        nes.bus.ppu.mask, 0x1E,
        "a default console accepts the write at cycle 2"
    );
    nes.run_frame();
    let state = nes.save_state();
    assert_eq!(
        state[WARMUP_STATE_OFFSET], 0,
        "the serialised warm-up byte is clear"
    );
}

// ---------------------------------------------------------------------------------------------
// The controller-port open-bus mask as two quirks
// ---------------------------------------------------------------------------------------------

/// A console with `quirks` and nothing else changed.
fn port_console(quirks: Quirks) -> Nes {
    boot_console(
        &synthetic(),
        &ConsoleConfig::with_quirks(PowerOn::default(), quirks),
    )
}

fn port_quirk(name: &str) -> Quirks {
    let mut quirks = Quirks::default();
    assert!(quirks.set_name(name), "{name}");
    quirks
}

fn port_settings() -> [(Quirks, u8); 4] {
    [
        (Quirks::default(), 0xE0),
        (port_quirk("nes101-port"), 0xE4),
        (port_quirk("famicom-port"), 0xF8),
        (
            Quirks {
                nes101_port: true,
                famicom_port: true,
                ..Quirks::default()
            },
            0xFC,
        ),
    ]
}

/// nesdev "Standard controller": the undriven bits of a port read keep the last bus byte. Bits
/// 5-7 always float; bit 2 ($4016 microphone line, absent on a NES-101) and bits 3-4 (D3/D4,
/// wired to port 2 only) float on the `nes101-port` / `famicom-port` consoles. $4017 is unchanged.
#[test]
fn port_quirks_widen_the_4016_open_bus_mask() {
    for (quirks, mask) in port_settings() {
        assert_eq!(quirks.port1_open_bus_mask(), mask);
        let mut nes = port_console(quirks);
        nes.bus.open_bus = 0xFF;
        assert_eq!(nes.bus.read(0x4016) & !1, mask, "{quirks:?}");
        nes.bus.open_bus = 0xFF;
        assert_eq!(nes.bus.read(0x4017) & !1, 0xE0, "$4017 under {quirks:?}");
    }
}

/// A plain `LDA $4016` leaves $40 on the bus, so the extra bits read 0 and Paperboy's `$40/$41`
/// reads are untouched under every setting.
#[test]
fn port_quirks_leave_a_plain_4016_read_at_0x40() {
    for (quirks, _) in port_settings() {
        let mut nes = port_console(quirks);
        nes.bus.open_bus = 0x40;
        assert_eq!(nes.bus.read(0x4016) & !1, 0x40, "{quirks:?}");
        nes.bus.open_bus = 0x40;
        assert_eq!(nes.bus.read(0x4017) & !1, 0x40, "{quirks:?}");
    }
}

/// The mask is configuration: it never reaches the save state.
#[test]
fn port_quirks_stay_out_of_the_state() {
    let plain = port_console(Quirks::default()).save_state();
    for (quirks, _) in port_settings() {
        assert_eq!(port_console(quirks).save_state(), plain, "{quirks:?}");
    }
}
