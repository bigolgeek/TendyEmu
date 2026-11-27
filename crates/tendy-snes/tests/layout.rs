//! The 16-bit `brr` invariant (DESIGN §4.2).
//!
//! The shared `Dsp` carries a voice's BRR address in a `u32`, because the SNES core carried it in
//! its 24-bit `Addr` and the SGB core in a `u16`. The DSP can only reach 64 KiB of APU RAM, so the
//! top half is always zero — and it has to stay that way, because the SGB host writes the field as a
//! `u16`: an address above `$FFFF` would mean the two hosts disagree about the same voice.
//!
//! Nothing in the decoder can produce one today: every assignment to `brr` goes through a `u16`
//! (see `tendy_spc::dsp`), which is exactly what this test is the tripwire for. It drives `brr` two
//! ways, so a future "just add 9 in the u32" would show up here rather than in a save file.

use std::path::PathBuf;

use tendy_snes::apu::{Apu, Host};
use tendy_snes::cart::Cartridge;
use tendy_snes::machine::Machine;

const TEST_ROM: &str = "snes-test-roms/inidisp_hammer_0f.sfc";

/// One slice of the master clock for the synthesised run: small enough that a voice step cannot
/// hide between two checks.
const SLICE: u32 = 128;

#[test]
fn brr_stays_16_bit_on_snes_pin_rom() {
    // 1. The pin ROM, stepped one CPU instruction at a time so the check lands between every pair of
    //    clocks. The suite is silent (see `inertness.rs`: no public SNES test ROM plays music), so
    //    this half shows the invariant holds over a whole machine's boot, not that a voice runs.
    match test_rom() {
        Some(rom) => {
            let cart = Cartridge::new(rom).expect("the pin ROM loads");
            let mut machine = Machine::new(cart);
            while machine.bus.master < 2 * u64::from(tendy_snes::MASTER_TICKS_PER_FRAME) {
                machine.cpu.step(&mut machine.bus);
                assert_sixteen_bit(&machine.bus.apu);
            }
        }
        None => println!("skipped: {TEST_ROM} not in TENDY_TEST_ROMS"),
    }

    // 2. A voice that really moves: keyed on, looping between two BRR blocks, driven a slice at a
    //    time for two frames. Without this half the check above would pass on a chip that never
    //    decodes anything.
    let mut apu = Apu::new(Host::Snes);
    // Directory page 4: source 0 starts at $0500 and loops to $0500.
    apu.ram[0x0400..0x0402].copy_from_slice(&0x0500u16.to_le_bytes());
    apu.ram[0x0402..0x0404].copy_from_slice(&0x0500u16.to_le_bytes());
    // Block at $0500: filter 0, code 0 (continue), sixteen one-nibble samples.
    apu.ram[0x0500] = 0x00;
    for i in 0..8 {
        apu.ram[0x0501 + i] = 0x12;
    }
    // Block at $0509: filter 0, code 3 (loop), so the voice returns to $0500 and plays forever.
    apu.ram[0x0509] = 0x03;
    apu.dsp.write(0x5D, 0x04); // DIR: $0400
    apu.dsp.write(0x00, 0x40); // V0 left volume
    apu.dsp.write(0x01, 0x40); // V0 right volume
    apu.dsp.write(0x02, 0x00); // V0 pitch: $1000, one sample per DSP sample
    apu.dsp.write(0x03, 0x10);
    apu.dsp.write(0x04, 0x00); // V0 source number 0
    apu.dsp.write(0x05, 0x00); // V0 ADSR1: direct gain
    apu.dsp.write(0x07, 0x7F); // V0 GAIN: full
    apu.dsp.write(0x6C, 0x20); // FLG: unmuted
    apu.dsp.write(0x4C, 0x01); // KON: voice 0

    for _ in 0..(tendy_snes::MASTER_TICKS_PER_FRAME * 2 / SLICE) {
        apu.tick(SLICE);
        assert_sixteen_bit(&apu);
    }
    assert_ne!(apu.dsp.keyed_voices(), 0, "the voice is still playing");
    assert!(
        matches!(apu.dsp.voices[0].brr, 0x0500 | 0x0509),
        "the decoder walks the two blocks and loops, got ${:X}",
        apu.dsp.voices[0].brr
    );
}

/// Every voice's `brr` inside the 64 KiB the DSP can address.
fn assert_sixteen_bit(apu: &Apu) {
    for (voice, state) in apu.dsp.voices.iter().enumerate() {
        assert!(
            state.brr <= 0xFFFF,
            "voice {voice}: brr ${:X} left the 64 KiB a u16 state field can carry",
            state.brr
        );
    }
}

/// The pin ROM's bytes, or the §5.6 skip line.
fn test_rom() -> Option<Vec<u8>> {
    let root = PathBuf::from(std::env::var_os("TENDY_TEST_ROMS")?);
    if !root.is_dir() {
        println!("skipped: TENDY_TEST_ROMS is not a folder");
        return None;
    }
    let path = root.join(TEST_ROM);
    if !path.is_file() {
        println!("skipped: {TEST_ROM} not in TENDY_TEST_ROMS");
        return None;
    }
    Some(std::fs::read(&path).unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display())))
}
