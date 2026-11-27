//! The CGB's 32 KiB of work RAM and the bank `SVBK` maps at `$D000`.
//!
//! Pan Docs, "FF70 — SVBK/WBK": "In CGB Mode, 32 KiB of internal RAM are available. This memory is
//! divided into 8 banks of 4 KiB each. Bank 0 is always available in memory at C000–CFFF, banks 1–7
//! can be selected into the address space at D000–DFFF", and a written bank "0 … maps bank 1
//! instead". Pan Docs, *Memory Map*: `$E000-$FDFF` is "Echo RAM (mirror of `$C000-$DDFF`)" and
//! `$D000-$DFFF` is switchable "in CGB mode" only.
//!
//! Each test names the rule it pins. The bytes are put there through the bus, so every rule is
//! asserted on what a cartridge can reach, not on an internal index.

mod common;

use tendy_gb::{Cartridge, Machine, WRAM_BANK_SIZE};

/// `$0143` for a cartridge that only runs on a CGB.
const CGB_ONLY: u8 = 0xC0;
/// `$0143` for one that runs on both.
const CGB_ENHANCED: u8 = 0x80;
/// `$0143` for a plain DMG cartridge.
const DMG: u8 = 0x00;

/// `SVBK`.
const SVBK: u16 = 0xFF70;
/// The first HDMA register, `$FF51`.
const HDMA1: u16 = 0xFF51;
/// `$FF55`, which starts a transfer.
const HDMA5: u16 = 0xFF55;

/// A ROM with the CGB flag and a matching header checksum — the shape `tests/cgb.rs` uses.
fn rom(flag: u8, code: &[u8]) -> Vec<u8> {
    let mut rom = common::synth_rom(&common::entry(code));
    rom[0x0143] = flag;
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x014D] = x;
    rom
}

fn machine(flag: u8) -> Machine {
    Machine::new(Cartridge::new(rom(flag, &[])).expect("synthesized ROM"))
}

/// The byte the bus puts at `bank`'s own storage, without going through the address space: the
/// bank's first byte, which `$D000` shows when `SVBK` selects it. Used only to say *which* bank a
/// write landed in — the write itself went through the bus.
fn stored(m: &Machine, bank: usize) -> u8 {
    m.bus.wram[bank * WRAM_BANK_SIZE]
}

/// A write to `$D000` with `SVBK`=3 lands in bank 3, and bank 1 — what the same address showed a
/// moment ago — keeps its byte. This is the bug the slice exists for: one 8 KiB array would have
/// the second write overwrite the first.
#[test]
fn a_write_to_d000_with_svbk_3_lands_in_bank_3_and_leaves_bank_1_alone() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 1);
    m.bus.write(0xD000, 0x11);
    m.bus.write(SVBK, 3);
    m.bus.write(0xD000, 0x33);

    assert_eq!(stored(&m, 3), 0x33, "the write went to bank 3");
    assert_eq!(stored(&m, 1), 0x11, "bank 1 kept its byte");

    m.bus.write(SVBK, 1);
    assert_eq!(m.peek(0xD000), 0x11, "bank 1 reads back as it was");
    m.bus.write(SVBK, 3);
    assert_eq!(m.peek(0xD000), 0x33, "bank 3 reads back what it was given");
}

/// A write of 0 to `SVBK` selects bank 1, not bank 0 (Pan Docs: "except 0, which maps bank 1").
/// Bank 0 is not reachable at `$D000` at all, so the proof is that `SVBK`=0 and `SVBK`=1 read and
/// write the same storage while `SVBK`=2 is a different byte.
#[test]
fn svbk_zero_selects_bank_1() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 0x00);
    assert_eq!(m.bus.cgb.wram_bank(), 1, "a written 0 reports as bank 1");
    m.bus.write(0xD000, 0x5A);

    assert_eq!(stored(&m, 1), 0x5A, "the byte is bank 1's");
    assert_eq!(
        stored(&m, 0),
        0x00,
        "and not bank 0's, whose $D000 half is unreachable"
    );

    m.bus.write(SVBK, 1);
    assert_eq!(m.peek(0xD000), 0x5A, "$D000 with SVBK=1 is the same bank");
    m.bus.write(SVBK, 2);
    assert_eq!(
        m.peek(0xD000),
        0x00,
        "$D000 with SVBK=2 is a different bank"
    );
}

/// `$C000-$CFFF` is bank 0 "always", whatever `SVBK` says: a cartridge's stack and its variables at
/// `$C000` do not move when it selects a bank, and bank 5's own `$C000`-offset bytes stay untouched.
#[test]
fn c000_is_bank_0_whatever_svbk_says() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 5);
    m.bus.write(0xC000, 0x77);
    m.bus.write(0xCFFF, 0x88);
    m.bus.write(SVBK, 2);

    assert_eq!(m.peek(0xC000), 0x77, "$C000 still reads bank 0");
    assert_eq!(m.peek(0xCFFF), 0x88, "$CFFF still reads bank 0");
    assert_eq!(stored(&m, 0), 0x77, "the byte is bank 0's");
    assert_eq!(stored(&m, 5), 0x00, "bank 5's own storage was not written");
}

/// Echo RAM mirrors the addressing: `$E000-$EFFF` mirrors bank 0 at `$C000-$CFFF`, and
/// `$F000-$FDFF` mirrors whatever bank `$D000` has, bank for bank (Pan Docs, *Memory Map*:
/// "E000-FDFF Echo RAM (mirror of C000-DDFF)").
#[test]
fn echo_ram_mirrors_the_bank_d000_has() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 3);
    m.bus.write(0xC123, 0x0C);
    m.bus.write(0xD356, 0x3D);
    m.bus.write(0xF000, 0xF3);

    assert_eq!(
        m.peek(0xE123),
        0x0C,
        "$E123 mirrors bank 0, not the selected bank"
    );
    assert_eq!(m.peek(0xD356), 0x3D, "$F000 wrote bank 3");
    assert_eq!(m.peek(0xF356), 0x3D, "$F356 mirrors $D356 in the same bank");
    assert_eq!(m.peek(0xF000), 0xF3, "and $F000 mirrors $D000");

    // A write through the mirror reaches the bank `$D000` has, and nothing else: bank 4's $D000 is
    // still empty when the cartridge selects it. The mirror stops at `$DDFF`, so $D189 is the byte
    // a $F189 write reaches.
    m.bus.write(0xF189, 0x99);
    assert_eq!(m.peek(0xD189), 0x99, "$D189 saw the echo's write");
    m.bus.write(SVBK, 4);
    assert_eq!(m.peek(0xD189), 0x00, "bank 4's $D189 is a different byte");
    assert_eq!(m.peek(0xF189), 0x00, "and so is its mirror");
}

/// The eight banks do not alias: each bank holds its own byte at `$D000`, and reading them back in
/// a different order returns what was written to each.
#[test]
fn the_eight_banks_do_not_alias_each_other() {
    let mut m = machine(CGB_ONLY);
    for bank in 0..8u8 {
        m.bus.write(SVBK, bank);
        m.bus.write(0xD000, 0xA0 | bank);
    }
    for bank in (0..8u8).rev() {
        m.bus.write(SVBK, bank);
        // A written 0 selects bank 1, whose byte was written twice: 0xA0 then 0xA1.
        let expected = if bank == 0 { 0xA1 } else { 0xA0 | bank };
        assert_eq!(m.peek(0xD000), expected, "bank {bank}'s byte");
    }
}

/// A DMG cartridge is unchanged: `SVBK` reads `$FF` and ignores writes, and `$C000-$DFFF` is the
/// one contiguous 8 KiB it has always been. Nothing behind the register moves, so a DMG title that
/// writes bank 5 into `$FF70` cannot reach a second bank.
#[test]
fn a_dmg_cartridge_cannot_reach_a_second_bank() {
    let mut m = machine(DMG);
    assert_eq!(m.peek(SVBK), 0xFF, "SVBK is a CGB register");
    m.bus.write(SVBK, 5);
    m.bus.write(0xD000, 0x42);
    m.bus.write(0xC000, 0x43);

    assert_eq!(m.peek(0xD000), 0x42, "the write landed");
    assert_eq!(m.peek(0xC000), 0x43, "the write landed");
    // The DMG's 8 KiB is banks 0 and 1 of the array, laid end to end: byte $D000 is offset $1000.
    assert_eq!(
        m.bus.wram[0x1000], 0x42,
        "$D000 is the ninth page of the one run"
    );
    assert_eq!(m.bus.wram[0x0000], 0x43, "$C000 is its first");
    assert_eq!(stored(&m, 5), 0x00, "bank 5 is not reachable at all");
    // Echo still mirrors it, as before.
    assert_eq!(m.peek(0xF000), 0x42, "echo follows the contiguous run");
}

/// A state taken with bank 5 selected comes back in bank 5 with the bytes of all eight banks: work
/// RAM is 32 KiB on the wire now, and `SVBK` says which bank `$D000` shows.
#[test]
fn a_save_state_taken_in_bank_5_resumes_in_bank_5_with_its_bytes() {
    let mut m = machine(CGB_ENHANCED);
    m.bus.write(SVBK, 5);
    m.bus.write(0xD000, 0x55);
    m.bus.write(0xC000, 0xC0);
    m.bus.write(SVBK, 7);
    m.bus.write(0xDFFF, 0x77);
    m.bus.write(SVBK, 5);
    let state = m.encode_state();

    let mut restored = machine(CGB_ENHANCED);
    restored.decode_state(&state).expect("state decodes");
    assert_eq!(restored.bus.cgb.wram_bank(), 5, "bank 5 came back selected");
    assert_eq!(restored.peek(0xD000), 0x55, "bank 5's byte came back");
    assert_eq!(restored.peek(0xC000), 0xC0, "bank 0's byte came back");
    restored.bus.write(SVBK, 7);
    assert_eq!(restored.peek(0xDFFF), 0x77, "bank 7's byte came back");

    // A state is not just the mapped 8 KiB: bank 4, never in the address space here, is in it too.
    let mut m = machine(CGB_ENHANCED);
    m.bus.write(SVBK, 4);
    m.bus.write(0xD000, 0x44);
    m.bus.write(SVBK, 1);
    let state = m.encode_state();
    let mut restored = machine(CGB_ENHANCED);
    restored.decode_state(&state).expect("state decodes");
    restored.bus.write(SVBK, 4);
    assert_eq!(restored.peek(0xD000), 0x44, "an unmapped bank round-trips");
}

/// The debugger's memory view is the byte the CPU would reach: `peek_stored` answers the bank
/// `SVBK` has selected, not a flattened 8 KiB.
#[test]
fn the_debugger_shows_the_selected_bank() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 2);
    m.bus.write(0xD000, 0x22);
    m.bus.write(SVBK, 6);
    m.bus.write(0xD000, 0x66);

    assert_eq!(m.peek(0xD000), 0x66, "the view follows SVBK=6");
    m.bus.write(SVBK, 2);
    assert_eq!(m.peek(0xD000), 0x22, "and SVBK=2");
    assert_eq!(m.peek(0xF000), 0x22, "with the echo showing the same bank");
}

/// A VRAM DMA's source is the address space, so a source at `$D000` reads the bank `SVBK` has
/// selected: two transfers of the same address, one per bank, copy two different pictures.
#[test]
fn a_vram_dma_source_reads_the_bank_d000_has() {
    let mut m = machine(CGB_ONLY);
    m.bus.write(SVBK, 1);
    for i in 0..16u8 {
        m.bus.write(0xD000 + i as u16, 0x10 + i);
    }
    m.bus.write(SVBK, 3);
    for i in 0..16u8 {
        m.bus.write(0xD000 + i as u16, 0x30 + i);
    }

    // Bank 3's block into $8000, then bank 1's block into $8010: the same source address twice.
    for (bank, dest) in [(3u8, 0x8000u16), (1, 0x8010)] {
        m.bus.write(SVBK, bank);
        m.bus.write(HDMA1, 0xD0);
        m.bus.write(HDMA1 + 1, 0x00);
        m.bus.write(HDMA1 + 2, (dest >> 8) as u8);
        m.bus.write(HDMA1 + 3, dest as u8);
        m.bus.write(HDMA5, 0x00);
        for _ in 0..8 {
            m.bus.tick();
        }
        assert!(!m.bus.vdma.active, "the general-purpose transfer finished");
    }

    let expected: Vec<u8> = (0..16u8).map(|i| 0x30 + i).collect();
    assert_eq!(&m.bus.ppu.vram[..16], &expected[..], "bank 3's block moved");
    let expected: Vec<u8> = (0..16u8).map(|i| 0x10 + i).collect();
    assert_eq!(
        &m.bus.ppu.vram[0x10..0x20],
        &expected[..],
        "bank 1's block moved"
    );
}
