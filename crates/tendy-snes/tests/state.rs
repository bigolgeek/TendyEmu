//! The save-state format's own tests: the wire primitives, the FNV trailer and the refusals
//!
//! `resume_equals_uninterrupted`, `second_resume_still_agrees` and `every_device_round_trips` are
//! `#[ignore]`d: they need `Machine::encode_state`/`decode_state` to carry every device's §9.2
//! section, which happens when the CPU, PPU, APU and front-end lanes have all merged (§11's
//! gate-order rule). They are written now so the behaviour they demand is pinned, and they run at
//! the merge.

mod common;

use tendy_snes::Machine;
use tendy_snes::cart::Cartridge;
use tendy_snes::joypad::Buttons;
use tendy_snes::state::{
    STATE_MAGIC, STATE_VERSION, StateError, StateReader, StateWriter, fnv1a64, fnv1a64_continue,
    fnv1a64_slice,
};

/// A fresh machine on the synthesized LoROM cartridge.
fn machine() -> Machine {
    Machine::new(Cartridge::new(common::synth_lorom(0x8000, &[0xEA])).expect("the cartridge loads"))
}

#[test]
fn fnv1a64_known_vectors() {
    // FNV-1a, 64-bit, with the published offset basis $CBF29CE484222325 and prime
    // $100000001B3 (the reference vectors the design's `state.rs` shape comes from).
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    // Continuing a hash is the same as hashing the whole byte string.
    assert_eq!(
        fnv1a64_continue(fnv1a64(b"foo"), b"bar"),
        fnv1a64(b"foobar")
    );
    assert_eq!(fnv1a64_continue(fnv1a64(b""), b""), fnv1a64(b""));
    // A frame is `u16` words, so the slice fold is the same hash over little-endian bytes (§2.9).
    assert_eq!(
        fnv1a64_slice(&[0x0102, 0x0304]),
        fnv1a64(&[0x02, 0x01, 0x04, 0x03])
    );
}

#[test]
fn corrupt_state_leaves_machine_untouched() {
    // §9: "decode_state reads into a Machine clone and swaps only on success, so a corrupt state
    // leaves the running machine untouched."
    let mut machine = machine();
    machine.run_frame();
    machine.run_frame();
    let before = (
        machine.frames,
        machine.checksum,
        machine.last_frame_checksum,
    );
    let state = machine.encode_state();
    assert_eq!(&state[..4], &STATE_MAGIC, "§9: magic, then version");
    assert_eq!(
        u16::from_le_bytes([state[4], state[5]]),
        STATE_VERSION,
        "§9: the version is the first thing after the magic"
    );
    assert!(
        state.len() > 4 + 2 + 8,
        "the state is magic, version, a payload and an 8-byte trailer"
    );

    // A short state is truncated rather than half-loaded: at 10 bytes the fields run out, and so
    // does a state missing its last byte, which eats into the trailer.
    assert_eq!(
        machine.decode_state(&state[..10]),
        Err(StateError::Truncated)
    );
    assert_eq!(
        machine.decode_state(&state[..state.len() - 1]),
        Err(StateError::Truncated)
    );
    // The magic is checked before anything else is believed.
    let mut wrong_magic = state.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        machine.decode_state(&wrong_magic),
        Err(StateError::BadMagic)
    );
    // A flipped payload byte breaks the trailer.
    let mut corrupt = state.clone();
    corrupt[12] ^= 0x01; // inside `frames`, past the header and before the trailer
    assert_eq!(machine.decode_state(&corrupt), Err(StateError::BadChecksum));
    assert_eq!(
        (
            machine.frames,
            machine.checksum,
            machine.last_frame_checksum
        ),
        before
    );
}

#[test]
fn version_mismatch_is_refused() {
    // §9: "A version this build does not know is refused as BadVersion(v) rather than misread;
    // that is why the version is the first thing after the magic."
    let mut machine = machine();
    let mut state = machine.encode_state();
    let future = STATE_VERSION + 1;
    state[4..6].copy_from_slice(&future.to_le_bytes());
    assert_eq!(
        machine.decode_state(&state),
        Err(StateError::BadVersion(future))
    );
    // The version itself is inside the hashed bytes, so a bumped version cannot pass the trailer.
    let mut past = machine.encode_state();
    past[4..6].copy_from_slice(&future.to_le_bytes());
    let body = &past[4..past.len() - 8];
    assert_ne!(
        fnv1a64(body),
        u64::from_le_bytes(past[past.len() - 8..].try_into().unwrap())
    );
}

#[test]
fn rom_mismatch_is_refused() {
    // §9.1: "the ROM | identified by rom_crc32, compared on load; a different cartridge is
    // BadValue".
    let a = Cartridge::new(common::synth_lorom(0x8000, &[0xEA])).unwrap();
    let mut section = StateWriter::new();
    a.save_state(&mut section);
    let section = section.finish();

    // The positive control: the cartridge's own state loads into a copy of itself.
    let mut same = a.clone();
    assert_eq!(same.load_state(&mut StateReader::new(&section)), Ok(()));

    // The recorded identity is compared. `checksum::crc32` is the front-end lane's file and is
    // still a stub on this branch (every image reports 0), so two different images cannot be told
    // apart by it yet; the comparison is what this test pins, so the identity is set explicitly.
    let mut other = Cartridge::new(common::synth_lorom(0x8000, &[0x42])).unwrap();
    if other.rom_crc32 == a.rom_crc32 {
        other.rom_crc32 = !a.rom_crc32;
    }
    assert_eq!(
        other.load_state(&mut StateReader::new(&section)),
        Err(StateError::BadValue),
        "another ROM's state is refused"
    );

    // A different board is a different cartridge even before the identity is compared: map_mode
    // and the header fields are part of the recorded identity too.
    let mut hirom = Cartridge::new(common::synth_hirom(0x1_0000, &[0xEA])).unwrap();
    assert_eq!(
        hirom.load_state(&mut StateReader::new(&section)),
        Err(StateError::BadValue)
    );
}

#[test]
#[ignore = "needs the merged machine: every device saves and loads (§9.2); runs after all five lanes land"]
fn resume_equals_uninterrupted() {
    // §9.3's proof: 300 frames, encode, 300 more; a fresh machine resumed from the state must
    // reach the same frame. `Apu::out` is drained on both sides because it is not in the state.
    let mut audio = vec![0.0f32; tendy_snes::AUDIO_STAGING];

    let mut a = machine();
    for _ in 0..300 {
        a.run_frame();
    }
    let state = a.encode_state();
    for _ in 0..300 {
        a.run_frame();
    }
    a.drain_audio(&mut audio);
    let frames = a.frames;
    let last = a.last_frame_checksum();
    let total = a.frame_checksum();

    let mut b = machine();
    b.decode_state(&state)
        .expect("a state this build wrote decodes");
    assert_eq!(b.frames, 300, "the resume point is 300 frames in");
    for _ in 0..300 {
        b.run_frame();
    }
    b.drain_audio(&mut audio);
    assert_eq!(b.frames, frames);
    assert_eq!(
        b.last_frame_checksum(),
        last,
        "§9.3: resume must equal uninterrupted at frame 600"
    );
    assert_eq!(b.frame_checksum(), total, "and the running fold agrees");
}

#[test]
#[ignore = "needs the merged machine: every device saves and loads (§9.2); runs after all five lanes land"]
fn second_resume_still_agrees() {
    // §9.3's second half: after a resume the machines must still agree, and a state taken from
    // the resumed machine must resume to the same place again.
    let mut uninterrupted = machine();
    for _ in 0..600 {
        uninterrupted.run_frame();
    }
    let expected = uninterrupted.encode_state();

    let mut a = machine();
    for _ in 0..300 {
        a.run_frame();
    }
    let first = a.encode_state();
    let mut b = machine();
    b.decode_state(&first).unwrap();
    for _ in 0..300 {
        b.run_frame();
    }
    assert_eq!(
        b.encode_state(),
        expected,
        "the machine resumed at frame 300 encodes the same state as an uninterrupted run at 600"
    );

    // Now resume from the resumed machine: the second resume must agree with it after 300 more.
    let second = b.encode_state();
    let mut c = machine();
    c.decode_state(&second).unwrap();
    for _ in 0..300 {
        c.run_frame();
    }
    for _ in 0..300 {
        b.run_frame();
    }
    assert_eq!(
        c.encode_state(),
        b.encode_state(),
        "a second resume reaches the same frame-900 state"
    );
}

#[test]
#[ignore = "needs the merged machine: every device saves and loads (§9.2); runs after all five lanes land"]
fn every_device_round_trips() {
    // Every row of §9.2 must be reachable from `encode_state`: give each device a value that is
    // not its power-on value, round-trip the state, and require the re-encoding to be identical.
    // A field the table forgets shows up as a differing body.
    let mut a = machine();
    a.run_frame();
    a.bus.master = 0x1234_5678;
    a.bus.mdr = 0x5A;
    a.bus.nmi_latch = true;
    a.bus.stall = 40;
    a.bus.wram[0x1234] = 0x5A;
    a.bus.io.nmitimen = 0x81;
    a.bus.io.memsel = 0x01;
    a.bus.io.htime = 0x01FF;
    a.bus.io.mpya = 0x12;
    a.bus.io.mpyb = 0x34;
    a.bus.io.wmadd = 0x1_2345;
    a.bus.ppu.vram[0x100] = 0x77;
    a.bus.ppu.oam[0x20] = 0x99;
    a.bus.ppu.cgram[0x10] = 0x7FFF;
    a.bus.ppu.regs.inidisp = 0x0F;
    a.bus.ppu.dot = 123;
    a.bus.ppu.line = 45;
    a.bus.dma.channels[0].dmap = 0x01;
    a.bus.dma.channels[0].das = 0x0123;
    a.bus.dma.mdmaen = 0x01;
    a.bus.joypad.pressed = Buttons::A;
    a.bus.apu.ram[0x400] = 0x3C;
    a.bus.apu.to_apu = [1, 2, 3, 4];
    a.bus.apu.spc_acc = 0x00FF_FFFF;
    a.bus.apu.cycles = 0x1_0000;
    a.cpu.a = 0x1234;
    a.cpu.x = 0x5678;
    a.cpu.d = 0x0100;
    a.cpu.pb = 0x80;

    let first = a.encode_state();
    let mut b = machine();
    b.decode_state(&first).expect("the state decodes");
    assert_eq!(
        b.encode_state(),
        first,
        "every §9.2 field must survive the round trip"
    );
    assert_eq!(b.bus.wram[0x1234], 0x5A);
    assert_eq!(b.bus.io.htime, 0x01FF);
    assert_eq!(b.bus.ppu.cgram[0x10], 0x7FFF);
    assert_eq!(b.bus.apu.ram[0x400], 0x3C);
    assert_eq!(b.cpu.d, 0x0100);
}
