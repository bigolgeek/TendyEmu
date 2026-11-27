//! The save chips: SRAM, Flash and EEPROM, proven where the public suites cannot
//! see — a real save/load round trip through the `.sav` file for three chips, and the four public
//! save suites themselves (`sram.gba`, `flash64.gba`, `flash128.gba`, `none.gba`).
//!
//! Every protocol fact here is GBATEK's (*Backup Media*, *Backup IDs*); the byte-lane behaviour of
//! the 8-bit chips and the 0xFF fill of an erased chip are pinned by the suites' own sources in
//! `roms/test/gba/save/`. The public ROMs are gitignored, so the gate tests are `#[ignore]`d.

use std::path::PathBuf;
use tendy_gb::System;
use tendy_gba::Gba;
use tendy_gba::bus::{IO_BASE, IWRAM_BASE};
use tendy_gba::cart::Cartridge;
use tendy_gba::dma;
use tendy_gba::save::{EepromOutcome, EepromSize, SaveType};

/// A cartridge whose save chip is named by `save_id` (an ID string placed word-aligned after the
/// header) and whose code — for the round-trip tests — is never run.
fn synth_cart_with_save(save_id: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xAC].copy_from_slice(b"SAVE-TEST\0\0\0");
    rom[0xAC..0xB0].copy_from_slice(b"TST1");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    rom[0x100..0x100 + save_id.len()].copy_from_slice(save_id);
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine with `save_id`'s chip in the slot.
fn machine_with_save(save_id: &[u8]) -> Gba {
    Gba::new(Cartridge::new(synth_cart_with_save(save_id)).expect("a synthesized cartridge"))
}

/// The `.sav` path the Game Boy path would build for this cartridge, under a scratch directory named
/// for the test so two round-trip tests that share one process never share a file (the old code
/// shared one `gb-save-<pid>` directory and each test's `remove_dir_all` could delete a sibling's
/// file mid-flight).
fn scratch_sav(gba: &Gba, name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("gb-save-{name}-{}", std::process::id()));
    let id = gba.cart_identity();
    let path = tendy_gb::save::save_path(&dir, &id.title, id.crc32);
    (dir, path)
}

/// The write-request bits GBATEK documents: `10`, the address (MSB first), 64 data bits, `0`.
fn write_request(addr: u16, data: [u8; 8], address_bits: usize) -> Vec<bool> {
    let mut bits = vec![true, false];
    for bit in (0..address_bits).rev() {
        bits.push(addr & (1 << bit) != 0);
    }
    for &byte in &data {
        for bit in (0..8).rev() {
            bits.push(byte & (1 << bit) != 0);
        }
    }
    bits.push(false);
    bits
}

// ---------------------------------------------------------------- round trips through disk

#[test]
fn sram_round_trips_through_the_sav_file() {
    let mut gba = machine_with_save(b"SRAM_V001");
    gba.bus.write8(0x0E00_0042, 0x99);
    gba.bus.write8(0x0E00_7FFF, 0x55);
    let body = gba.battery_body().expect("an SRAM cartridge has a body");
    assert_eq!(body.len(), 0x8000, "32 KiB");

    let (dir, path) = scratch_sav(&gba, "sram");
    tendy_gb::save::write_atomic(&path, &body).expect("write the .sav");

    // Drop the machine, load the file again, and get the same bytes back.
    let mut again = machine_with_save(b"SRAM_V001");
    let saved = std::fs::read(&path).expect("read the .sav back");
    again.load_battery_body(&saved, 0).expect("load it");
    assert_eq!(again.bus.read8(0x0E00_0042), 0x99);
    assert_eq!(again.bus.read8(0x0E00_7FFF), 0x55);
    assert_eq!(
        again.battery_body().expect("still a body"),
        body,
        "the whole chip is byte-for-byte identical"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cartridge whose ROM names no chip gets one from the first **store** into the window, and only
/// from a store: a read of `$0E000000` with nothing behind it is the no-chip case `none.gba` pins,
/// and a read of `$0D000000` is the ROM mirror (GBATEK, *GBA Memory Map*: `0C000000-0DFFFFFF` Game
/// Pak ROM) that three reference cartridges read for their own reasons.
#[test]
fn a_none_cartridge_provisions_sram_on_the_first_store_and_only_then() {
    // Reads alone: the window answers 0xFF and no chip appears.
    let mut read_only = machine_with_save(b"NO-SAVE-ID");
    assert_eq!(
        read_only.bus.save.kind(),
        SaveType::None,
        "no chip is named"
    );
    assert_eq!(read_only.bus.read8(0x0E00_0000), 0xFF);
    assert_eq!(read_only.bus.read16(0x0E00_0000), 0xFFFF);
    assert_eq!(read_only.bus.read32(0x0E00_0000), 0xFFFF_FFFF);
    assert_eq!(
        read_only.bus.save.kind(),
        SaveType::None,
        "a read never creates a chip"
    );

    // One store: the 32 KiB SRAM chip appears and the byte is there, mirrored by the window.
    let mut stores = machine_with_save(b"NO-SAVE-ID");
    stores.bus.write8(0x0E00_0000, 0x42);
    assert_eq!(stores.bus.save.kind(), SaveType::Sram, "a store creates it");
    assert_eq!(stores.bus.read8(0x0E00_0000), 0x42);
    assert_eq!(stores.bus.read8(0x0E01_0000), 0x42, "mirrored at $0E010000");
    assert_eq!(stores.battery_body().map(|body| body.len()), Some(0x8000));
}

#[test]
fn flash_round_trips_through_the_sav_file() {
    let mut gba = machine_with_save(b"FLASH512_V001");
    // Program two bytes the documented way: AA/55/A0, then the data byte.
    for (offset, byte) in [(0x1000usize, 0x3Cu8), (0x2000, 0xC3)] {
        gba.bus.write8(0x0E00_5555, 0xAA);
        gba.bus.write8(0x0E00_2AAA, 0x55);
        gba.bus.write8(0x0E00_5555, 0xA0);
        gba.bus.write8(0x0E00_0000 + offset as u32, byte);
    }
    let body = gba.battery_body().expect("a flash cartridge has a body");
    assert_eq!(body.len(), 0x1_0000, "64 KiB");

    let (dir, path) = scratch_sav(&gba, "flash");
    tendy_gb::save::write_atomic(&path, &body).expect("write the .sav");

    let mut again = machine_with_save(b"FLASH512_V001");
    let saved = std::fs::read(&path).expect("read the .sav back");
    again.load_battery_body(&saved, 0).expect("load it");
    assert_eq!(again.bus.read8(0x0E00_1000), 0x3C);
    assert_eq!(again.bus.read8(0x0E00_2000), 0xC3);
    assert_eq!(
        again.bus.read8(0x0E00_0000),
        0xFF,
        "never-written stays 0xFF"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eeprom_round_trips_through_the_sav_file() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    // Drive the documented 17-bit protocol: an 8 KiB chip is written with an 81-bit request.
    let request = write_request(0x2AA, *b"ABCDEFGH", 14);
    let outcome = gba
        .bus
        .save
        .eeprom()
        .expect("an EEPROM cartridge")
        .receive(&request, gba.bus.cycles);
    assert_eq!(outcome, Ok(EepromOutcome::Written));
    let body = gba.battery_body().expect("a determined EEPROM has a body");
    assert_eq!(body.len(), 0x2000, "8 KiB");

    let (dir, path) = scratch_sav(&gba, "eeprom");
    tendy_gb::save::write_atomic(&path, &body).expect("write the .sav");

    let mut again = machine_with_save(b"EEPROM_V122");
    let saved = std::fs::read(&path).expect("read the .sav back");
    // The reloaded machine's EEPROM size is undecided; the file's length decides it.
    again.load_battery_body(&saved, 0).expect("load it");
    assert_eq!(
        again.bus.save.eeprom().unwrap().known_size(),
        Some(EepromSize::K64kbit)
    );
    let response = again.bus.save.eeprom().unwrap().response(0x2AA);
    let mut got = [0u8; 8];
    for (byte_index, byte) in got.iter_mut().enumerate() {
        for bit_index in 0..8 {
            if response[4 + byte_index * 8 + bit_index] {
                *byte |= 1 << (7 - bit_index);
            }
        }
    }
    assert_eq!(got, *b"ABCDEFGH");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the EEPROM's window

/// The EEPROM's window: GBATEK, *GBA Cart Backup EEPROM*, "On carts with 16MB or smaller ROM,
/// eeprom can be alternately accessed anywhere at D000000h-DFFFFFFh".
const EEPROM_WINDOW: u32 = 0x0D00_0000;

/// The read request's bits: `11`, the address (MSB first), `0` — GBATEK's *Set Address (For
/// Reading)*.
fn read_request(addr: u16, address_bits: usize) -> Vec<bool> {
    let mut bits = vec![true, true];
    for bit in (0..address_bits).rev() {
        bits.push(addr & (1 << bit) != 0);
    }
    bits.push(false);
    bits
}

/// Clock a bit stream into the chip the documented way: one halfword per bit, bit 0 only (GBATEK:
/// "one halfword for each bit, bit1-15 of the halfwords are don't care, only bit0 is used").
fn clock_in(gba: &mut Gba, bits: &[bool]) {
    for &bit in bits {
        gba.bus.write16(EEPROM_WINDOW, u16::from(bit));
    }
}

/// Clock `count` bits out of the chip, each read's bit 0.
fn clock_out(gba: &mut Gba, count: usize) -> Vec<bool> {
    (0..count)
        .map(|_| gba.bus.read16(EEPROM_WINDOW) & 1 != 0)
        .collect()
}

/// The 64 data bits of a read-out as eight bytes, byte 0 first (GBATEK: "4 bits - ignore these,
/// 64 bits - data (conventionally MSB first)").
fn read_out_bytes(bits: &[bool]) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    for (index, byte) in bytes.iter_mut().enumerate() {
        for bit in 0..8 {
            if bits[index * 8 + bit] {
                *byte |= 1 << (7 - bit);
            }
        }
    }
    bytes
}

/// One DMA3 transfer in the shape GBATEK gives the EEPROM: 16-bit units, both addresses
/// incrementing, starting immediately — "DMA3CNT=80000000h+length".
fn dma3(gba: &mut Gba, sad: u32, dad: u32, units: u16) {
    let base = IO_BASE + (dma::BASE + 3 * dma::STRIDE) as u32;
    gba.bus.write32(base + dma::SAD as u32, sad);
    gba.bus.write32(base + dma::DAD as u32, dad);
    gba.bus.write16(base + dma::COUNT as u32, units);
    gba.bus.write16(base + dma::CONTROL as u32, dma::ENABLE);
}

/// A frame is 228 lines of 1232 cycles, and an EEPROM write programs in 108 368 cycles, so one
/// frame is enough for the chip to become Ready again.
fn wait_until_ready(gba: &mut Gba) {
    for _ in 0..4 {
        gba.run_frame();
    }
}

#[test]
fn an_idle_eeprom_answers_the_ready_bit_at_d000000() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    // The cartridge's own first instruction halfword is `$002E` — bit 0 clear — which is the value
    // the ROM window answered here. GBATEK, *Write Data to Address*: "keep reading from the chip, by
    // normal LDRH [DFFFF00h], until Bit 0 of the returned data becomes \"1\" (Ready)".
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW),
        0x0001,
        "an idle EEPROM has bit 0 set"
    );
    assert_eq!(gba.bus.read8(EEPROM_WINDOW), 1);
    // The poll is a loop, so the bit stays set for as long as nothing asks the chip anything.
    for _ in 0..64 {
        assert_eq!(gba.bus.read16(EEPROM_WINDOW) & 1, 1);
    }
}

#[test]
fn a_nine_bit_read_command_reads_the_block_back() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    let payload = *b"ABCDEFGH";
    // A 73-bit write for a 512-byte chip's block 3, the shape H15 measured in 0193.
    clock_in(&mut gba, &write_request(3, payload, 6));
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        0,
        "the ready bit is clear while the write programs"
    );
    wait_until_ready(&mut gba);
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        1,
        "and set again once it has"
    );

    // The documented read: a 9-bit request, then 68 bits read back.
    clock_in(&mut gba, &read_request(3, 6));
    let out = clock_out(&mut gba, 68);
    assert_eq!(out.len(), 68, "4 dummy bits and 64 data bits");
    assert!(
        !out[..4].iter().any(|&bit| bit),
        "the first four bits are ignored and read 0"
    );
    assert_eq!(read_out_bytes(&out[4..]), payload);
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        1,
        "ready again once the read-out has gone"
    );
}

#[test]
fn a_read_request_with_the_cartridges_own_trailing_bit_reads_the_block_and_returns_ready() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    let payload = *b"WXYZ0123";
    clock_in(&mut gba, &write_request(7, payload, 6));
    // The 73-bit write is committed by the next access, and the poll that does it answers 0 while
    // the chip programs — the cartridges' own first Ready poll (0352, 0490, 0785 all show it).
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        0,
        "the poll that commits the write answers 0"
    );
    wait_until_ready(&mut gba);
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        1,
        "Ready once programmed"
    );

    // The shape those cartridges send: nine bits whose last one is the cartridge's own buffer bit,
    // not GBATEK's "0" (the Classic NES Series put data bit 49 there — `gba_cnes_save.rs`). The chip
    // drives that wire itself on a read, so the value is clocked and not sampled.
    let mut request = read_request(7, 6);
    *request.last_mut().expect("a request has bits") = true;
    clock_in(&mut gba, &request);
    let out = clock_out(&mut gba, 68);
    assert_eq!(
        read_out_bytes(&out[4..]),
        payload,
        "the block comes back, not 68 Ready bits"
    );
    // A complete read command leaves the chip idle: bit 0 of the window is set again.
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        1,
        "the Ready bit is set after the read of the last bit"
    );
}

#[test]
fn a_seventy_three_bit_write_stores_the_block() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    clock_in(&mut gba, &write_request(5, *b"12345678", 6));
    wait_until_ready(&mut gba);
    // The chip has it, read out through the documented read command...
    clock_in(&mut gba, &read_request(5, 6));
    let out = clock_out(&mut gba, 68);
    assert_eq!(read_out_bytes(&out[4..]), *b"12345678");
    // ...and the battery body carries it at block 5's offset, so it survives the `.sav`.
    let body = gba.battery_body().expect("a determined EEPROM has a body");
    assert_eq!(&body[40..48], b"12345678");
}

#[test]
fn a_non_eeprom_cartridge_still_sees_rom_in_the_window() {
    // The window belongs to the chip only when the cartridge has one: every other cartridge keeps
    // the ROM mirror it always had, which is what this test protects.
    for save_id in [&b"SRAM_V001"[..], &b"FLASH512_V001"[..], &b"HELLO"[..]] {
        let mut gba = machine_with_save(save_id);
        assert_eq!(
            gba.bus.read16(EEPROM_WINDOW),
            0x002E,
            "{}: the ROM mirror, bit 0 clear",
            String::from_utf8_lossy(save_id)
        );
        assert_eq!(
            gba.bus.read16(0x0C00_0000),
            0x002E,
            "as at the mirror below it"
        );
    }
}

#[test]
fn an_eight_kib_eeprom_uses_the_seventeen_and_eighty_one_bit_requests() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    // `EEPROM_V` does not say which chip it is: the 17-bit read request is the 8 KiB chip's, and it
    // is the first request that decides (GBATEK leaves "no autodection mechanism").
    clock_in(&mut gba, &read_request(0x2AA, 14));
    assert_eq!(
        gba.bus.save.eeprom().expect("an EEPROM").known_size(),
        Some(EepromSize::K64kbit),
        "17 bits is the 8 KiB chip's read request"
    );
    // The chip was never written, so the read-out is the 0xFF fill.
    let out = clock_out(&mut gba, 68);
    assert_eq!(read_out_bytes(&out[4..]), [0xFF; 8]);

    // An 81-bit write, and the 17-bit read that finds it again.
    clock_in(&mut gba, &write_request(0x2AA, *b"8KIBCHIP", 14));
    wait_until_ready(&mut gba);
    clock_in(&mut gba, &read_request(0x2AA, 14));
    let out = clock_out(&mut gba, 68);
    assert_eq!(read_out_bytes(&out[4..]), *b"8KIBCHIP");
}

#[test]
fn dma3_moves_the_same_bits_a_strh_would() {
    let mut gba = machine_with_save(b"EEPROM_V122");
    let buffer = IWRAM_BASE + 0x1000;
    let out = IWRAM_BASE + 0x2000;
    let payload = *b"DMABITS!";

    // H15 measured both directions in the wild: `dma3 sad 03007D80 dad 0D000000 count 73` and
    // `dma3 sad 0D000000 dad 03007E48 count 9`. The first is the write below.
    let request = write_request(2, payload, 6);
    for (index, &bit) in request.iter().enumerate() {
        gba.bus.write16(buffer + index as u32 * 2, u16::from(bit));
    }
    dma3(&mut gba, buffer, EEPROM_WINDOW, request.len() as u16);
    assert_eq!(
        gba.bus.read16(EEPROM_WINDOW) & 1,
        0,
        "the DMA's 73 bits are a write request, and the chip is programming"
    );
    wait_until_ready(&mut gba);

    // The 9-bit read command, the second measured shape...
    let request = read_request(2, 6);
    for (index, &bit) in request.iter().enumerate() {
        gba.bus.write16(buffer + index as u32 * 2, u16::from(bit));
    }
    dma3(&mut gba, buffer, EEPROM_WINDOW, request.len() as u16);
    // ...then the 68-bit read-out, through the same DMA path a STRH/LDRH pair would take.
    dma3(&mut gba, EEPROM_WINDOW, out, 68);
    let bits: Vec<bool> = (0..68u32)
        .map(|index| gba.bus.read16(out + index * 2) & 1 != 0)
        .collect();
    assert_eq!(read_out_bytes(&bits[4..]), payload);
}

// ---------------------------------------------------------------- the public gates

/// Run a save suite for 600 frames, the way G1's gates do.
fn run_suite(path: &str) -> Gba {
    let rom = std::fs::read(path)
        .unwrap_or_else(|why| panic!("{path}: {why} (roms/test/gba/save/ is gitignored)"));
    let mut gba = Gba::new(Cartridge::new(rom).expect("a public test ROM"));
    for _ in 0..600 {
        gba.run_frame();
    }
    gba
}

/// The idle branch the suites park on after `m_test_eval` returns.
const IDLE_BRANCH: u32 = 0xEAFF_FFFE;

/// Each suite's pass condition, worked out from its source: `m_exit n` sets `r12 = n` and jumps to
/// `eval`, where `m_test_eval` calls `SWI $60000` (Div) only on the failure path to print `n`. So a
/// pass is **no `SWI`**, the machine parked on `b .`, and the IWRAM witness (`$03000000`) untouched
/// — zero, because none of these suites use that address as scratch (unlike `arm.gba`'s block-
/// transfer tests, which is the trap the brief warns about).
fn assert_passed(gba: &mut Gba, name: &str) {
    let parked = gba.bus.read32(gba.cpu.pc() & !3);
    let witness: Vec<u8> = (0..12)
        .map(|index| gba.bus.read8(IWRAM_BASE + index))
        .collect();
    println!(
        "{name}: {} frames, {} cycles, {} instructions, {} SWIs, witness {witness:?}",
        gba.frames(),
        gba.bus.cycles,
        gba.cpu.instructions,
        gba.cpu.swi_calls
    );
    assert_eq!(gba.cpu.swi_calls, 0, "{name}: the failure path ran");
    assert_eq!(
        parked,
        IDLE_BRANCH,
        "{name}: the ROM never reached its idle branch, PC {:08X}",
        gba.cpu.pc()
    );
    assert_eq!(
        witness,
        vec![0u8; 12],
        "{name}: the IWRAM witness is not zero"
    );
}

#[test]
#[ignore = "needs roms/test/gba/save/sram.gba, a public test ROM kept out of the repository"]
fn sram_gba_reports_no_failed_tests() {
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/save/sram.gba"
    ));
    assert_passed(&mut gba, "sram.gba");
}

#[test]
#[ignore = "needs roms/test/gba/save/flash64.gba, a public test ROM kept out of the repository"]
fn flash64_gba_reports_no_failed_tests() {
    // flash64.gba declares both "FLASH_V" and "FLASH512_V" (its `flash:` label), so it is the 64 KiB
    // suite for both strings; there is no separate flash.gba on disk, only the shared flash.asm body.
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/save/flash64.gba"
    ));
    assert_passed(&mut gba, "flash64.gba");
}

#[test]
#[ignore = "needs roms/test/gba/save/flash128.gba, a public test ROM kept out of the repository"]
fn flash128_gba_reports_no_failed_tests() {
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/save/flash128.gba"
    ));
    assert_passed(&mut gba, "flash128.gba");
}

#[test]
#[ignore = "needs roms/test/gba/save/none.gba, a public test ROM kept out of the repository"]
fn none_gba_reports_no_failed_tests() {
    let mut gba = run_suite(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/save/none.gba"
    ));
    assert_passed(&mut gba, "none.gba");
}
