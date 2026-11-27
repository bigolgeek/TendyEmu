//! PPU glitch quirks: OAM (sprite) DRAM decay, the `$2006` second-write scroll glitch and the
//! `$2000`/`$2005`/`$2006` first-write (dot-257 copy) glitch.
//!
//! Spec: `docs/spec-console-quirks.md` §3.1 (the hardware rule and the constants), §3.2 (the scroll
//! glitch's landing dots and result), §4 (the save-state layout; `STATE_VERSION` 5, after the
//! Zapper device block) and the quirks spec (the fields are always serialised, whatever the quirks,
//! and every
//! layout change bumps the version).
//!
//! Every case here is opt-in. The quirk-off default path is pinned elsewhere and not duplicated:
//! `tests/console_quirks.rs::default_path_reproduces_the_pre_s2_bytes` and
//! `tests/reset_align.rs::default_reset_path_is_byte_identical` carry the framebuffer and
//! `save_state()` pins, and the AccuracyCoin / TASVideos suites cover the `$2004`, OAM-DMA and
//! hybrid-address rows (`oam_read`, `oam_stress`, `sprite_ram`) that the quirks must leave alone.

use tendy_nes::cart::split;
use tendy_nes::console::{ConsoleConfig, PpuQuirks, Quirks};
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::load;
use tendy_nes::nes::{Nes, PowerOn};
use tendy_nes::ppu::{OAM_DECAY_CYCLES, Ppu};
use tendy_nes::state::{StateReader, StateWriter, fnv1a64};

use crate::common::loop_rom;

fn boot_console(rom: &[u8], config: &ConsoleConfig) -> Nes {
    Nes::from_rom_data_console(split(rom).expect("parse rom"), fnv1a64(rom), config)
        .expect("build console")
}

/// The decay quirk on, everything else default.
fn decay() -> Quirks {
    Quirks {
        oam_decay: true,
        ..Quirks::default()
    }
}

fn decay_config() -> ConsoleConfig {
    ConsoleConfig::with_quirks(PowerOn::default(), decay())
}

fn decay_console(timing: u8) -> Nes {
    boot_console(&loop_rom(0x08, timing), &decay_config())
}

/// A standalone PPU with the quirk on, at CPU cycle 0, and a loop cartridge for the register writes.
fn decay_ppu() -> (Ppu, Box<dyn tendy_nes::cart::Mapper>) {
    let mut ppu = Ppu::new();
    ppu.quirks.oam_decay = true;
    ppu.cpu_cycle = 0;
    (ppu, load(&loop_rom(0x08, 0)).expect("mapper"))
}

/// The decay pattern of the row `addr` is in: every byte becomes its own OAM address, except the
/// attribute byte of each sprite, whose bits 2-4 do not physically exist (`b & $E3`).
fn row_after_decay(addr: u8) -> [u8; 8] {
    let mut row = [0; 8];
    for (i, slot) in row.iter_mut().enumerate() {
        let b = (addr & 0xF8) | i as u8;
        *slot = if b & 3 == 2 { b & 0xE3 } else { b };
    }
    row
}

/// Fills all 256 bytes of OAM with `$4014` from `$0200`, leaving OAMADDR back at 0. The page values
/// are chosen so that no byte equals the decay pattern byte for its address, so "intact" and
/// "decayed" cannot be confused.
fn fill_oam(nes: &mut Nes) {
    for i in 0..256 {
        nes.bus.ram[0x200 + i] = (i as u8) ^ 0x5A;
    }
    nes.bus.write(0x4014, 0x02);
    nes.bus.read(0); // queued by the write, the DMA runs on this read cycle
}

// ---------------------------------------------------------------------------------------------
// §3.1 tests 1-3: the row stamp rule, on a standalone PPU
// ---------------------------------------------------------------------------------------------

/// A row read exactly `OAM_DECAY_CYCLES` after it was refreshed is still there, and the read
/// refreshes it (Mesen's comparison is `<=`).
#[test]
fn oam_row_survives_exactly_3000_cycles() {
    let (mut ppu, mut cart) = decay_ppu();
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    ppu.write_register(cart.as_mut(), 0x2004, 0xAA);
    assert_eq!(ppu.oam_stamp[2], 0, "the write stamped row 2 at cycle 0");

    ppu.cpu_cycle = OAM_DECAY_CYCLES;
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0xAA,
        "3000 cycles is within the decay interval"
    );
    assert_eq!(
        ppu.oam_stamp[2], OAM_DECAY_CYCLES,
        "and the read refreshed it"
    );
    assert_eq!(ppu.oam[0x10], 0xAA, "the row was not overwritten");
}

/// One cycle later the row has decayed: all eight bytes take the decay pattern and the stamp is not
/// refreshed (Mesen), so the row keeps reading as decayed. Other rows are untouched.
#[test]
fn oam_row_decays_after_3001_cycles() {
    let (mut ppu, mut cart) = decay_ppu();
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    ppu.write_register(cart.as_mut(), 0x2004, 0xAA);

    ppu.cpu_cycle = OAM_DECAY_CYCLES + 1;
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0x10,
        "the decayed byte is the row's own address"
    );
    assert_eq!(
        &ppu.oam[0x10..0x18],
        &[0x10, 0x11, 0x02, 0x13, 0x14, 0x15, 0x02, 0x17]
    );
    assert_eq!(ppu.oam_stamp[2], 0, "a decay must not refresh the stamp");
    assert!(
        ppu.oam_stamp[..2].iter().all(|&s| s == 0),
        "other rows are untouched"
    );
    assert!(
        ppu.oam_stamp[3..].iter().all(|&s| s == 0),
        "other rows are untouched"
    );
    assert_eq!(ppu.oam[0], 0, "row 0 was not decayed");
}

/// A write refreshes its row, so the interval restarts there even if the row was written earlier.
#[test]
fn oam_write_refreshes_row() {
    let (mut ppu, mut cart) = decay_ppu();
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    ppu.write_register(cart.as_mut(), 0x2004, 0xAA);

    ppu.cpu_cycle = 2900;
    ppu.write_register(cart.as_mut(), 0x2003, 0x11);
    ppu.write_register(cart.as_mut(), 0x2004, 0xBB);
    assert_eq!(ppu.oam_stamp[2], 2900);

    ppu.cpu_cycle = 5800;
    ppu.write_register(cart.as_mut(), 0x2003, 0x10);
    assert_eq!(
        ppu.read_register(cart.as_mut(), 0x2004),
        0xAA,
        "5800 - 2900 is still inside the interval"
    );
}

// ---------------------------------------------------------------------------------------------
// §3.1 tests 4-7: the console-level refresh paths
// ---------------------------------------------------------------------------------------------

/// `$4014` writes through `$2004`, so a DMA refresh every row it touches; all 32 stamps are inside
/// the DMA's own cycle span.
#[test]
fn oam_dma_refreshes_rows() {
    let mut nes = decay_console(0);
    fill_oam(&mut nes);
    for (row, &stamp) in nes.bus.ppu.oam_stamp.iter().enumerate() {
        assert!(stamp > 0, "row {row} was never stamped by the DMA");
        let elapsed = nes.bus.cycles.saturating_sub(stamp);
        assert!(
            elapsed <= 600,
            "row {row} was stamped {elapsed} cycles before the DMA ended"
        );
    }
}

/// Rendering keeps OAM alive: sprite evaluation reads every row on every visible line, which is the
/// refresh. With rendering then off, the same row decays.
#[test]
fn rendering_refreshes_rows() {
    let mut nes = decay_console(0);
    fill_oam(&mut nes);
    let original = nes.bus.ppu.oam;

    nes.bus.write(0x2001, 0x18); // background + sprites
    for _ in 0..3 {
        nes.run_frame();
    }

    // In vblank, outside the render window: the row is still there, and the read refreshes it.
    nes.bus.write(0x2003, 0x80);
    assert_eq!(
        nes.bus.read(0x2004),
        original[0x80],
        "three rendered frames refreshed every row"
    );

    // Rendering off: nothing refreshes DRAM any more, so the row is stale one vblank later.
    nes.bus.write(0x2001, 0x00);
    let start = nes.bus.cycles;
    while nes.bus.cycles < start + OAM_DECAY_CYCLES + 100 {
        nes.step();
    }
    assert!(
        nes.bus.ppu.scanline < 240,
        "still inside the frame, rendering off"
    );
    nes.bus.write(0x2003, 0x80);
    assert_eq!(
        nes.bus.read(0x2004),
        0x80,
        "with rendering off and no refresh, the row decayed to its own address"
    );
    assert_eq!(&nes.bus.ppu.oam[0x80..0x88], &row_after_decay(0x80));
}

/// The 2C07 refreshes OAM on scanlines 265-310 whatever rendering is doing: a row written long
/// before the window (line 250, more than one decay interval from line 0) is still intact at the top
/// of the next frame, and only decays once the window is past.
#[test]
fn pal_refresh_window() {
    let mut nes = decay_console(1); // PAL
    assert!(
        nes.bus.ppu.quirks.oam_pal_refresh,
        "PAL derives the refresh window"
    );

    nes.bus.write(0x2001, 0x00); // rendering stays off throughout
    // Line 250 is ~62 lines (about 7000 CPU cycles) before line 0: without the window the row would
    // be long decayed by then, so this only passes if lines 265-310 really do refresh it.
    while nes.bus.ppu.scanline < 250 {
        nes.step();
    }
    nes.bus.write(0x2003, 0x10);
    nes.bus.write(0x2004, 0xAA);
    assert!(
        nes.bus.ppu.scanline < 265,
        "the write is before the refresh window"
    );

    while nes.bus.ppu.scanline != 0 {
        nes.step();
    }
    assert_eq!(
        nes.bus.ppu.oam[0x10], 0xAA,
        "the window refreshed the row, it did not decay it"
    );
    nes.bus.write(0x2003, 0x10);
    assert_eq!(
        nes.bus.read(0x2004),
        0xAA,
        "lines 265-310 refreshed the row, so it is intact on line 0"
    );

    // Past the window (lines 0..=264), with rendering still off, nothing refreshes the row.
    let start = nes.bus.cycles;
    while nes.bus.cycles < start + OAM_DECAY_CYCLES + 100 {
        nes.step();
    }
    assert!(
        nes.bus.ppu.scanline < 264,
        "still outside the refresh window"
    );
    nes.bus.write(0x2003, 0x10);
    assert_eq!(nes.bus.read(0x2004), 0x10, "and now it has decayed");
}

/// The default path never touches the stamps: a console with default quirks runs DMA and `$2004`
/// traffic for two frames and leaves all 32 stamps zero, which is what its save state carries.
#[test]
fn oam_decay_off_never_touches_stamps() {
    let mut nes = boot_console(&loop_rom(0x08, 0), &ConsoleConfig::default());
    assert!(!nes.bus.ppu.quirks.oam_decay, "the quirk is off by default");
    fill_oam(&mut nes);
    nes.bus.write(0x2003, 0x40);
    for _ in 0..2 {
        nes.run_frame();
        let _ = nes.bus.read(0x2004);
    }
    assert_eq!(nes.bus.ppu.oam_stamp, [0; 32]);
    assert_eq!(&nes.save_state()[4..6], &[12, 0], "STATE_VERSION 12");
}

// ---------------------------------------------------------------------------------------------
// The save-state layout (spec §4)
// ---------------------------------------------------------------------------------------------

/// The stamps are always serialised, so the layout does not depend on the config: a default console
/// and a decay-on console produce the same length, and each loads the other's state.
#[test]
fn state_layout_is_independent_of_the_decay_quirk() {
    let rom = loop_rom(0x08, 0);
    let mut plain = boot_console(&rom, &ConsoleConfig::default());
    let mut decayed = decay_console(0);
    for _ in 0..4 {
        plain.run_frame();
        decayed.run_frame();
    }
    let plain_state = plain.save_state();
    let decayed_state = decayed.save_state();
    assert_eq!(
        plain_state.len(),
        decayed_state.len(),
        "layout depends on config"
    );
    assert_eq!(&decayed_state[4..6], &[12, 0], "STATE_VERSION 12");

    let mut into_plain = boot_console(&rom, &ConsoleConfig::default());
    into_plain
        .load_state(&decayed_state)
        .expect("decay-on state into a default console");
    let mut into_decayed = decay_console(0);
    into_decayed
        .load_state(&plain_state)
        .expect("default state into a decay-on console");
}

/// With the quirk on, the stamps and the clock they are measured against both survive a save/load:
/// the loaded console resumes the same decay schedule, so both machines end in the same state.
#[test]
fn decay_on_state_round_trip_is_deterministic() {
    let config = decay_config();
    let mut a = boot_console(&loop_rom(0x08, 0), &config);
    fill_oam(&mut a);
    a.bus.write(0x2003, 0x40); // $2004 reads now hit row 8
    let stamp = a.bus.ppu.oam_stamp[8];
    assert!(stamp > 0, "the DMA stamped row 8");
    for _ in 0..200 {
        a.step();
    }

    let state = a.save_state();
    let mut b = boot_console(&loop_rom(0x08, 0), &config);
    b.load_state(&state).expect("load");
    assert_eq!(
        b.bus.ppu.oam_stamp, a.bus.ppu.oam_stamp,
        "the stamps are part of the state"
    );
    assert_eq!(
        b.save_state(),
        state,
        "a loaded console re-saves the same bytes"
    );

    // Run both past the threshold with rendering off, then read the row once.
    while a.bus.cycles < stamp + OAM_DECAY_CYCLES + 100 {
        a.step();
        b.step();
    }
    assert_eq!(a.bus.cycles, b.bus.cycles, "lockstep");
    assert_eq!(
        a.bus.read(0x2004),
        b.bus.read(0x2004),
        "the same decayed byte"
    );
    assert_eq!(
        a.bus.ppu.oam[0x40], 0x40,
        "row 8 decayed to its own address"
    );
    assert_eq!(a.bus.ppu.oam[0x46], 0x42, "attribute bits 2-4 do not exist");
    assert_eq!(
        a.save_state(),
        b.save_state(),
        "states agree across the decay"
    );
    assert_eq!(a.bus.ppu.framebuffer, b.bus.ppu.framebuffer);
}

// ---------------------------------------------------------------------------------------------
// §3.2 `scroll_glitch_2006`: the second `$2006` write lands on an increment or the copy
// ---------------------------------------------------------------------------------------------

/// Nesdev's coarse-X increment, copied so the expectations are independent of the emulator's code.
fn inc_x(v: u16) -> u16 {
    if v & 0x001F == 31 {
        (v & !0x001F) ^ 0x0400
    } else {
        v + 1
    }
}

/// Nesdev's coarse-Y increment (§3.2's `inc_y`, used by the dot-257 case).
fn inc_y(mut v: u16) -> u16 {
    if v & 0x7000 != 0x7000 {
        v + 0x1000
    } else {
        v &= !0x7000;
        let mut y = (v & 0x03E0) >> 5;
        if y == 29 {
            y = 0;
            v ^= 0x0800;
        } else if y == 31 {
            y = 0;
        } else {
            y += 1;
        }
        (v & !0x03E0) | (y << 5)
    }
}

/// The PPU's own `save_state()` bytes: field-by-field, the quirk flags are not in here.
fn ppu_bytes(ppu: &Ppu) -> Vec<u8> {
    let mut w = StateWriter::new();
    ppu.save_state(&mut w);
    w.finish()
}

/// A PPU at `scanline == 10, dot == dot` whose `$2006` latch is armed with `t` high byte `$23`,
/// running with `mask` (the spec's setup). Rendering has been on (or off) for a whole frame.
fn at_dot(dot: u16, mask: u8) -> (Ppu, Box<dyn tendy_nes::cart::Mapper>) {
    let rom = loop_rom(0x08, 0);
    let mut cart = load(&rom).expect("mapper");
    let mut ppu = Ppu::new();
    ppu.write_register(cart.as_mut(), 0x2006, 0x23); // t = $23xx, w = 1
    ppu.mask = mask;
    while !(ppu.scanline == 10 && ppu.dot == dot) {
        ppu.tick(cart.as_mut());
    }
    (ppu, cart)
}

/// Runs the spec's setup at `dot`, writes the second `$2006` (`new = $23C5`) with the quirk `on` or
/// `off`, ticks the three dots up to the landing dot, and returns the PPU plus the pre-write `v`.
fn scroll_glitch_write(dot: u16, quirk_on: bool) -> (Ppu, u16) {
    let (mut ppu, mut cart) = at_dot(dot, 0x08);
    ppu.quirks.scroll_glitch_2006 = quirk_on;
    let old = ppu.v;
    ppu.write_register(cart.as_mut(), 0x2006, 0xC5);
    for _ in 0..3 {
        ppu.tick(cart.as_mut());
    }
    (ppu, old)
}

/// The written address `$2006=$23` then `$2006=$C5` builds.
const NEW: u16 = 0x23C5;

/// Landing dot 8 (write dot 6, the first coarse-X increment): the quirk ANDs `new` with `old + 1`
/// on the coarse-X/fine-X/nametable bits instead of leaving the increment alone.
#[test]
fn scroll_glitch_2006_on_coarse_x_increment() {
    let (on, old) = scroll_glitch_write(6, true);
    let (off, off_old) = scroll_glitch_write(6, false);
    assert_eq!(old, off_old, "the two runs start from the same v");

    assert_eq!(
        on.v,
        (NEW & !0x041F) | (inc_x(old) & NEW & 0x041F),
        "on: the glitched v"
    );
    assert_eq!(
        on.t, on.v,
        "the glitch corrupts t as well (Mesen: \"both V and T\")"
    );
    assert_eq!(
        off.v,
        inc_x(NEW),
        "off: today's behaviour — the plain coarse-X increment"
    );
    assert_eq!(off.t, NEW, "off: t is the written value");
    assert_eq!(on.glitch_2006, None, "the pending update is consumed");
    assert_eq!(off.glitch_2006, None, "a quirk-off PPU never arms it");
}

/// Landing dot 256 (write dot 254): the coarse-X increment, with the dot-256 coarse-Y increment not
/// touching any `0x041F` bit of the result.
#[test]
fn scroll_glitch_2006_on_dot_256() {
    let (on, old) = scroll_glitch_write(254, true);
    let (off, _) = scroll_glitch_write(254, false);

    assert_eq!(
        on.v,
        (NEW & !0x041F) | (inc_x(old) & NEW & 0x041F),
        "on: the glitched v"
    );
    assert_eq!(on.t, on.v);
    assert_eq!(off.v, inc_y(inc_x(NEW)), "off: coarse-X then coarse-Y on v");
    assert_eq!(off.t, NEW);
}

/// Landing dot 257 (write dot 255): the horizontal copy from `t` ANDed with the increments on the
/// pre-write value. The hybrid fetch the `hyb` path performs on that same dot is untouched.
#[test]
fn scroll_glitch_2006_on_dot_257_keeps_hybrid_fetch() {
    let (on, old) = scroll_glitch_write(255, true);
    let (off, _) = scroll_glitch_write(255, false);

    let expected = ((inc_y(inc_x(old)) & !0x041F) | (NEW & 0x041F)) & NEW;
    assert_eq!(on.v, expected, "on: the glitched v");
    assert_eq!(on.t, on.v);
    assert_eq!(off.v, NEW, "off: the hybrid landing sets v = t");

    assert_eq!(
        format!("{:?}", on.bg),
        format!("{:?}", off.bg),
        "the background fetch latches are untouched"
    );
    assert_eq!(
        on.fetch_bus, off.fetch_bus,
        "the hybrid nametable read is untouched"
    );
    assert_eq!(on.bg.nt, off.bg.nt, "the hybrid read result is untouched");
    assert!(
        on.hyb.is_none() && off.hyb.is_none(),
        "the hybrid path was still consumed"
    );
}

/// Landing dots 328 and 336 (write dots 326 and 334): the preload coarse-X increments.
#[test]
fn scroll_glitch_2006_prefetch_dots() {
    for dot in [326u16, 334] {
        let (on, old) = scroll_glitch_write(dot, true);
        let (off, _) = scroll_glitch_write(dot, false);
        assert_eq!(
            on.v,
            (NEW & !0x041F) | (inc_x(old) & NEW & 0x041F),
            "on: dot {dot} + 2"
        );
        assert_eq!(on.t, on.v);
        assert_eq!(off.v, inc_x(NEW), "off: dot {dot} + 2");
    }
}

/// One dot either side of a landing dot is clean: dot 5 lands on 7 (no increment) and dot 7 lands on
/// 9, the hybrid dot. With the quirk off both runs are the same machine; with it on they must be too.
#[test]
fn scroll_glitch_2006_one_dot_away_is_clean() {
    for dot in [5u16, 7] {
        let (on, _) = scroll_glitch_write(dot, true);
        let (off, _) = scroll_glitch_write(dot, false);
        assert_eq!(on.v, off.v, "dot {dot}: v");
        assert_eq!(on.t, off.t, "dot {dot}: t");
        assert_eq!(on.glitch_2006, None, "dot {dot}: nothing armed");
        assert_eq!(
            ppu_bytes(&on),
            ppu_bytes(&off),
            "dot {dot}: the whole PPU state"
        );
    }
}

/// Rendering off: Mesen only applies the glitch while rendering, so the quirk cannot arm.
#[test]
fn scroll_glitch_2006_needs_rendering() {
    let (mut on, mut cart) = at_dot(6, 0x00);
    on.quirks.scroll_glitch_2006 = true;
    on.write_register(cart.as_mut(), 0x2006, 0xC5);
    for _ in 0..3 {
        on.tick(cart.as_mut());
    }
    let (mut off, mut cart) = at_dot(6, 0x00);
    off.quirks.scroll_glitch_2006 = false;
    off.write_register(cart.as_mut(), 0x2006, 0xC5);
    for _ in 0..3 {
        off.tick(cart.as_mut());
    }
    assert_eq!(
        on.v, NEW,
        "v is the written value (no increments with rendering off)"
    );
    assert_eq!(on.glitch_2006, None, "no rendering, no arm");
    assert_eq!(
        ppu_bytes(&on),
        ppu_bytes(&off),
        "the quirk is invisible with rendering off"
    );
}

/// The option defaults off, and a default PPU carries no pending update.
#[test]
fn scroll_glitch_2006_off_by_default() {
    assert!(!PpuQuirks::default().scroll_glitch_2006);
    assert!(!Quirks::default().scroll_glitch_2006);
    let ppu = Ppu::new();
    assert!(!ppu.quirks.scroll_glitch_2006);
    assert_eq!(ppu.glitch_2006, None);
}

/// The pending update is emulated state: it round-trips through `save_state`/`load_state`, and a PPU
/// reset drops it with the rest of the in-flight pipeline.
#[test]
fn scroll_glitch_2006_state_round_trip() {
    let mut ppu = Ppu::new();
    ppu.glitch_2006 = Some((8, 0x1234));
    let bytes = ppu_bytes(&ppu);

    let mut loaded = Ppu::new();
    loaded
        .load_state(&mut StateReader::new(&bytes))
        .expect("load");
    assert_eq!(loaded.glitch_2006, Some((8, 0x1234)));
    assert_eq!(
        ppu_bytes(&loaded),
        bytes,
        "a loaded PPU re-saves the same bytes"
    );

    ppu.reset();
    assert_eq!(
        ppu.glitch_2006, None,
        "the reset drops the in-flight update"
    );
}

// ---------------------------------------------------------------------------------------------
// §3.3 tests 1-7: the first-write glitch ($2000 / $2005 / $2006 on the dot-257 horizontal copy)
// ---------------------------------------------------------------------------------------------

/// The §3.3 setup: a PPU at `scanline == 10, dot == dot` with `mask`, a chosen CPU data bus value,
/// `w` as the caller needs it, and the quirk `on` or `off`. Returns the PPU and the pre-write `v`.
fn first_write(
    dot: u16,
    mask: u8,
    quirk_on: bool,
    bus: u8,
    reg: u16,
    val: u8,
    w: bool,
) -> (Ppu, u16) {
    let (mut ppu, mut cart) = at_dot(dot, mask);
    ppu.quirks.first_write_glitch = quirk_on;
    ppu.cpu_bus = bus;
    ppu.w = w;
    let pre = ppu.v;
    ppu.write_register(cart.as_mut(), reg, val);
    (ppu, pre)
}

/// `$2000`: the dot-257 copy takes nametable X (bus bit 0, `<< 10`) from the CPU data bus, while `t`
/// keeps the written value. `STA $2100` drives `$21`; `STA $2000` drives `$20`.
#[test]
fn first_write_glitch_2000() {
    for bus in [0x21u8, 0x20] {
        let (on, pre) = first_write(258, 0x08, true, bus, 0x2000, 0x00, false);
        let (off, off_pre) = first_write(258, 0x08, false, bus, 0x2000, 0x00, false);
        assert_eq!(
            pre, off_pre,
            "bus ${bus:02X}: both runs start from the same v"
        );

        let want = if bus & 1 != 0 {
            pre | 0x0400
        } else {
            pre & !0x0400
        };
        assert_eq!(
            on.v, want,
            "bus ${bus:02X}: on — nametable X comes from the bus"
        );
        assert_eq!(
            on.t & 0x0C00,
            0,
            "bus ${bus:02X}: t takes the written value ($2000 = $00)"
        );
        assert_eq!(off.v, off_pre, "bus ${bus:02X}: off — v is untouched");
        assert_eq!(
            on.ctrl, off.ctrl,
            "bus ${bus:02X}: the quirk only garbles v"
        );
    }
}

/// `$2005` first write: coarse X comes from bus bits 7-3, while `t` and fine X take the written value.
#[test]
fn first_write_glitch_2005() {
    let (on, _) = first_write(258, 0x08, true, 0x21, 0x2005, 0x7D, false);
    assert_eq!(on.v & 0x001F, 4, "on: coarse X is $21 >> 3");
    assert_eq!(on.t & 0x001F, 0x0F, "t keeps the written value ($7D >> 3)");
    assert_eq!(on.fine_x, 5, "fine X keeps the written value");

    let (off, _) = first_write(258, 0x08, false, 0x21, 0x2005, 0x7D, false);
    assert_eq!(off.t & 0x001F, 0x0F, "off: t unchanged by the quirk");
    assert_eq!(off.fine_x, 5);
    assert_ne!(
        on.v, off.v,
        "on: the coarse-X copy really differs from the clean path"
    );
}

/// `$2006` first write: both nametable bits come from bus bits 3-2 while `t` keeps the written high
/// byte. `STA $2C06` drives `$2C`; `STA $2006` drives `$20`.
#[test]
fn first_write_glitch_2006() {
    let (on, _) = first_write(258, 0x08, true, 0x2C, 0x2006, 0x00, false);
    assert_eq!(
        on.v & 0x0C00,
        0x0C00,
        "on: both nametable bits set from $2C"
    );

    let (on_20, _) = first_write(258, 0x08, true, 0x20, 0x2006, 0x00, false);
    assert_eq!(on_20.v & 0x0C00, 0, "on: $20 clears both nametable bits");

    let (off, _) = first_write(258, 0x08, false, 0x2C, 0x2006, 0x00, false);
    assert_eq!(
        off.t >> 8,
        0x00,
        "t takes the written high byte, quirk or not"
    );
    assert_ne!(
        on.v & 0x0C00,
        off.v & 0x0C00,
        "on: the nametable bits differ from the clean path"
    );
}

/// Only the first writes glitch: a `$2005` second write (`w = true`) at the same dot is clean.
#[test]
fn first_write_glitch_second_writes_unaffected() {
    let (on, _) = first_write(258, 0x08, true, 0x21, 0x2005, 0x7D, true);
    let (off, _) = first_write(258, 0x08, false, 0x21, 0x2005, 0x7D, true);
    assert_eq!(
        ppu_bytes(&on),
        ppu_bytes(&off),
        "a second write is byte-identical with the quirk on"
    );
}

/// One dot either side of the copy: the quirk stays out of the way entirely.
#[test]
fn first_write_glitch_one_dot_away() {
    for dot in [257u16, 259] {
        let (on, _) = first_write(dot, 0x08, true, 0x21, 0x2000, 0x00, false);
        let (off, _) = first_write(dot, 0x08, false, 0x21, 0x2000, 0x00, false);
        assert_eq!(
            ppu_bytes(&on),
            ppu_bytes(&off),
            "dot {dot}: the whole PPU state"
        );
    }
}

/// Rendering off: there is no dot-257 copy to garble, so the quirk cannot fire.
#[test]
fn first_write_glitch_needs_rendering() {
    let (on, _) = first_write(258, 0x00, true, 0x21, 0x2000, 0x00, false);
    let (off, _) = first_write(258, 0x00, false, 0x21, 0x2000, 0x00, false);
    assert_eq!(ppu_bytes(&on), ppu_bytes(&off), "no rendering, no glitch");
}

/// The option defaults off, and a default PPU never carries a bus value.
#[test]
fn first_write_glitch_off_by_default() {
    assert!(!PpuQuirks::default().first_write_glitch);
    assert!(!Quirks::default().first_write_glitch);
    let ppu = Ppu::new();
    assert!(!ppu.quirks.first_write_glitch);
    assert_eq!(ppu.cpu_bus, 0, "a standalone PPU carries no bus value");
}

/// The bus plumbing end to end: `$2105` (a `$2005` mirror) written from code reaching the PPU at dot
/// 258 takes the open bus the CPU drove *before* the write; the open bus then holds the new value.
#[test]
fn first_write_glitch_uses_bus_before_write() {
    let rom = loop_rom(0x08, 0);
    let quirks = Quirks {
        first_write_glitch: true,
        ..Quirks::default()
    };
    let mut nes = boot_console(
        &rom,
        &ConsoleConfig::with_quirks(PowerOn::default(), quirks),
    );
    nes.bus.write(0x2001, 0x08); // background rendering on
    while !(nes.bus.ppu.scanline == 10 && nes.bus.ppu.dot <= 255) {
        nes.bus.read(0);
    }
    let bus = &mut nes.bus;
    while bus.ppu.dot != 255 {
        let (ppu, cart) = bus.ppu_and_cart();
        ppu.tick(cart);
    }
    bus.open_bus = 0x21; // what `STA $21xx` leaves on the bus before the write
    bus.write(0x2105, 0x7D); // ticks 255-257, then writes at dot 258
    assert_eq!(bus.ppu.dot, 258, "the write lands on the copy dot");
    assert_eq!(
        bus.ppu.v & 0x001F,
        4,
        "coarse X came from the bus ($21 >> 3)"
    );
    assert_eq!(bus.ppu.cpu_bus, 0x21, "the PPU saw the pre-write bus value");
    assert_eq!(
        bus.open_bus, 0x7D,
        "the bus itself takes the written value afterwards"
    );
}
