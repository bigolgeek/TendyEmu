//! The two memory-copy calls and the source a cartridge did not align.
//!
//! GBATEK, *BIOS Memory Copy*, says what each call is built from and what the caller owes it:
//!
//! > Memcopy is implemented as repeated LDMIA/STMIA [Rb]!,r3 or LDRH/STRH r3,[r0,r5] instructions.
//! > Memfill as single LDMIA or LDRH followed by repeated STMIA [Rb]!,r3 or STRH r3,[r0,r5].
//!
//! > r0    Source address        (must be aligned by 4 for 32bit, by 2 for 16bit)
//!
//! So the function itself never says what a misaligned `r0` does — the *instruction* it is built from
//! does, and GBATEK's *ARM CPU Memory Alignments* answers for each of the three that a copy loop uses:
//!
//! > Mis-aligned STR,STRH,STM,LDM,LDRD,STRD,PUSH,POP (forced align): The mis-aligned low bit(s) are
//! > ignored, the memory access goes to a forcibly aligned (rounded-down) memory address.
//!
//! > Mis-aligned LDR,SWP (rotated read): Reads from forcibly aligned address "addr AND (NOT 3)", and
//! > does then rotate the data as "ROR (addr AND 3)*8".
//!
//! > On ARM7 aka ARMv4 aka NDS7/GBA: LDRH Rd,[odd] --> LDRH Rd,[odd-1] ROR 8 ;read to bit0-7 and
//! > bit24-31
//!
//! Three instructions, three different answers, and that is what these tests pin:
//!
//! * a word **copy** is `LDMIA`, which rounds its address down — the words below `r0` are copied;
//! * `CpuSet`'s word **fill** is a single `LDMIA` too, so it rounds down the same way, while
//!   `CpuFastSet`'s fill is one `LDR`, which rotates — the four bytes *at* `r0` become the fill word;
//! * a **halfword** read is `LDRH`, which on the GBA answers from `[r0-1]` rotated by eight, i.e. the
//!   odd byte in bits 0-7 and the byte before it in bits 24-31 — and `STRH` keeps bits 0-7, so the
//!   low byte is copied and the high byte is zero.
//!
//! Every call goes through a real [`Gba`]: the cartridge's first instruction is the `SWI` itself, and
//! one `step_instruction` runs it through the same dispatcher a game reaches.

use tendy_gba::Gba;
use tendy_gba::bus::{EWRAM_BASE, IWRAM_BASE};
use tendy_gba::cart::Cartridge;

/// The ARM `SWI` for `CpuSet` (`$0B`): "In ARM mode, only the upper 8bit of the 24bit comment field
/// are interpreted" — the number is in bits 16-23.
const SWI_CPU_SET: u32 = 0xEF0B_0000;
/// The ARM `SWI` for `CpuFastSet` (`$0C`).
const SWI_CPU_FAST_SET: u32 = 0xEF0C_0000;

/// A cartridge whose entry point is a single `SWI`, so the machine's first instruction is the call
/// under test.
fn machine(swi: u32) -> Gba {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-CPUS\0");
    rom[0xAC..0xB0].copy_from_slice(b"TCPS");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    rom[0xC0..0xC4].copy_from_slice(&swi.to_le_bytes());
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    Gba::new(Cartridge::new(rom).expect("a synthesized cartridge"))
}

/// Put `bytes` at `addr` one byte at a time, so an odd address is no obstacle to building the fixture.
fn put(gba: &mut Gba, addr: u32, bytes: &[u8]) {
    for (index, &byte) in bytes.iter().enumerate() {
        gba.bus.write8(addr + index as u32, byte);
    }
}

/// Set `R0`-`R2` and execute the cartridge's `SWI`, which is one instruction at `$080000C0`.
fn swi(gba: &mut Gba, src: u32, dest: u32, control: u32) {
    gba.cpu.r[0] = src;
    gba.cpu.r[1] = dest;
    gba.cpu.r[2] = control;
    gba.step_instruction();
}

#[test]
fn a_word_copy_from_an_unaligned_source_reads_the_boundary_below_it() {
    // "Memcopy is implemented as repeated LDMIA/STMIA": the load is an `LDM`, so "the mis-aligned low
    // bit(s) are ignored" and the copy starts at `$02000000 & !3` — here `r0 = $02000002`, which a
    // mask to the transfer's own width (two bytes) would leave where it is.
    let mut gba = machine(SWI_CPU_SET);
    for index in 0..8u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, 0x1122_0000 + index);
    }
    swi(&mut gba, EWRAM_BASE + 2, IWRAM_BASE, 4 | (1 << 26));
    for index in 0..4u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            0x1122_0000 + index,
            "word {index} came from the boundary below r0"
        );
    }
}

#[test]
fn a_fast_copy_from_an_unaligned_source_reads_the_boundary_below_it() {
    // `CpuFastSet` is words only and is the same `LDMIA` loop, so the same rounding applies.
    let mut gba = machine(SWI_CPU_FAST_SET);
    for index in 0..8u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, 0x1122_0000 + index);
    }
    swi(&mut gba, EWRAM_BASE + 1, IWRAM_BASE, 8);
    for index in 0..8u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            0x1122_0000 + index,
            "word {index} came from the boundary below r0"
        );
    }
}

#[test]
fn a_fast_fill_from_an_unaligned_source_reads_the_rotated_word() {
    // `CpuFastSet`'s fill is the one read in either call that is *not* an `LDM`: "Memfill as single
    // LDR followed by repeated STMIA" — one rotated read at `r0` and then the same word over and
    // over. So `r0` stays where the cartridge put it and the fill word is the four bytes *at* it,
    // not the rounded word below it.
    let mut gba = machine(SWI_CPU_FAST_SET);
    put(
        &mut gba,
        EWRAM_BASE,
        &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17],
    );
    swi(&mut gba, EWRAM_BASE + 1, IWRAM_BASE, 8 | (1 << 24));
    // Bytes `$11 $12 $13 $14` at `$02000001`, little-endian.
    for index in 0..8u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            0x1413_1211,
            "word {index} is the rotated read of r0"
        );
    }
    // "Fixed Source Address": the source did not walk, so the byte below it is still its own.
    assert_eq!(gba.bus.read8(EWRAM_BASE), 0x10);
}

#[test]
fn a_cpu_set_word_fill_from_an_unaligned_source_reads_the_boundary_below_it() {
    // `CpuSet`'s word fill is the other single-read shape, and it is an `LDM`: "Memfill as single
    // LDMIA or LDRH followed by repeated STMIA" — so it rounds down where `CpuFastSet`'s `LDR`
    // rotates. Same source bytes as the test above, two bytes past the boundary, and the fill word is
    // the aligned one.
    let mut gba = machine(SWI_CPU_SET);
    put(
        &mut gba,
        EWRAM_BASE,
        &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17],
    );
    swi(
        &mut gba,
        EWRAM_BASE + 2,
        IWRAM_BASE,
        4 | (1 << 26) | (1 << 24),
    );
    for index in 0..4u32 {
        assert_eq!(
            gba.bus.read32(IWRAM_BASE + index * 4),
            0x1312_1110,
            "word {index} is the boundary below r0, not the rotated read"
        );
    }
}

#[test]
fn a_halfword_copy_from_an_odd_source_reads_like_the_odd_ldrh() {
    // "LDRH Rd,[odd] --> LDRH Rd,[odd-1] ROR 8 ;read to bit0-7 and bit24-31" — the odd byte lands in
    // bits 0-7 and the byte before it in bits 24-31 — and `STRH` writes bits 0-15 only. Four
    // halfwords from `$02000001`, `$02000003`, `$02000005`, `$02000007`: the odd bytes, high byte 0.
    let mut gba = machine(SWI_CPU_SET);
    put(
        &mut gba,
        EWRAM_BASE,
        &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17],
    );
    swi(&mut gba, EWRAM_BASE + 1, IWRAM_BASE, 4);
    let expected = [0x0011u16, 0x0013, 0x0015, 0x0017];
    for (index, &value) in expected.iter().enumerate() {
        assert_eq!(
            gba.bus.read16(IWRAM_BASE + index as u32 * 2),
            value,
            "halfword {index} holds the odd byte and no high byte"
        );
    }
}

#[test]
fn a_halfword_copy_from_an_even_source_is_a_plain_ldrh() {
    // The halfword path's ordinary case, and the nearest thing to the odd one: an even source that is
    // not on a word boundary must not be rounded down the way the word copy is.
    let mut gba = machine(SWI_CPU_SET);
    put(
        &mut gba,
        EWRAM_BASE,
        &[0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19],
    );
    swi(&mut gba, EWRAM_BASE + 2, IWRAM_BASE, 4);
    let expected = [0x1312u16, 0x1514, 0x1716, 0x1918];
    for (index, &value) in expected.iter().enumerate() {
        assert_eq!(
            gba.bus.read16(IWRAM_BASE + index as u32 * 2),
            value,
            "halfword {index}"
        );
    }
}

#[test]
fn the_aligned_paths_are_unchanged() {
    // Sixteen-bit copy, sixteen-bit fill, word copy, word fill: the four shapes a cartridge uses
    // legitimately, so this file also shows the ordinary path still works.
    let mut gba = machine(SWI_CPU_SET);
    for index in 0..8u16 {
        gba.bus
            .write16(EWRAM_BASE + index as u32 * 2, 0x8000 + index);
    }
    swi(&mut gba, EWRAM_BASE, IWRAM_BASE, 8);
    for index in 0..8u16 {
        assert_eq!(
            gba.bus.read16(IWRAM_BASE + index as u32 * 2),
            0x8000 + index
        );
    }
    // The halfword fill takes its value from the first halfword and repeats it.
    let mut gba = machine(SWI_CPU_SET);
    gba.bus.write16(EWRAM_BASE, 0xBEEF);
    swi(&mut gba, EWRAM_BASE, IWRAM_BASE + 0x100, 8 | (1 << 24));
    for index in 0..8u32 {
        assert_eq!(gba.bus.read16(IWRAM_BASE + 0x100 + index * 2), 0xBEEF);
    }

    let mut gba = machine(SWI_CPU_SET);
    for index in 0..8u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, 0x1122_3344 + index);
    }
    swi(&mut gba, EWRAM_BASE, IWRAM_BASE, 8 | (1 << 26));
    for index in 0..8u32 {
        assert_eq!(gba.bus.read32(IWRAM_BASE + index * 4), 0x1122_3344 + index);
    }
    // The word fill's source stays put and its word is the aligned one.
    let mut gba = machine(SWI_CPU_SET);
    gba.bus.write32(EWRAM_BASE, 0xDEAD_BEEF);
    swi(
        &mut gba,
        EWRAM_BASE,
        IWRAM_BASE + 0x100,
        8 | (1 << 26) | (1 << 24),
    );
    for index in 0..8u32 {
        assert_eq!(gba.bus.read32(IWRAM_BASE + 0x100 + index * 4), 0xDEAD_BEEF);
    }

    // `CpuFastSet` copy and fill, both aligned: the block path in its ordinary form.
    let mut gba = machine(SWI_CPU_FAST_SET);
    for index in 0..8u32 {
        gba.bus.write32(EWRAM_BASE + index * 4, 0x5566_7788 + index);
    }
    swi(&mut gba, EWRAM_BASE, IWRAM_BASE, 8);
    for index in 0..8u32 {
        assert_eq!(gba.bus.read32(IWRAM_BASE + index * 4), 0x5566_7788 + index);
    }
    let mut gba = machine(SWI_CPU_FAST_SET);
    gba.bus.write32(EWRAM_BASE, 0xCAFE_F00D);
    swi(&mut gba, EWRAM_BASE, IWRAM_BASE + 0x100, 8 | (1 << 24));
    for index in 0..8u32 {
        assert_eq!(gba.bus.read32(IWRAM_BASE + 0x100 + index * 4), 0xCAFE_F00D);
    }
}
