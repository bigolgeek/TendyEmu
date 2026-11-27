//! Real SNES save states (#71): every device serialised, the round trip proven by output, and
//! older, foreign, truncated or impossible states refused without a panic or a half-load.
//!
//! The round trip is the same shape everywhere: run a machine, save, keep running and hash each
//! completed frame, the audio it drains and a fresh save; load the save into a fresh machine, run
//! the same frames, and demand all three agree. One save is taken mid-frame at an instruction
//! boundary, the finest step the public API has, so a save does not only work at V-Blank.
//!
//! The picture buffer is a host-side output, rebuilt every frame, and is not part of machine state.
//! A save taken inside a frame therefore has one transitional picture, whose rows above the save
//! point were rendered before the save; that one picture is skipped in the mid-frame test, while its
//! audio — which is real chip output — is still compared.

mod common;

use std::path::PathBuf;

use tendy_snes::Machine;
use tendy_snes::cart::Cartridge;
use tendy_snes::cpu::Flags;
use tendy_snes::joypad::Buttons;
use tendy_snes::state::{STATE_MAGIC, StateError, fnv1a64_continue};
use tendy_snes::{DOTS_PER_LINE, LINES_PER_FRAME};

const TEST_ROM: &str = "snes-test-roms/inidisp_hammer_0f.sfc";
const SAMPLE_RATE: u32 = 48_000;

/// A program that sets the backdrop colour, then writes WRAM, the VRAM data port and INIDISP's
/// brightness every loop. Frames keep changing, and the machine exercises the CPU, WRAM, the PPU's
/// registers, CGRAM and VRAM, and the APU's idle output.
const PROGRAM: &[u8] = &[
    0xA9, 0x00, // lda #$00
    0x8D, 0x21, 0x21, // sta $2121
    0xA9, 0xFF, // lda #$ff
    0x8D, 0x22, 0x21, // sta $2122
    0xA9, 0x7F, // lda #$7f
    0x8D, 0x22, 0x21, // sta $2122
    0xA9, 0x0F, // lda #$0f
    0x8D, 0x00, 0x21, // sta $2100
    0xAD, 0x00, 0x00, // lda $0000
    0x1A, // inc a
    0x8D, 0x00, 0x00, // sta $0000
    0x29, 0x0F, // and #$0f
    0x8D, 0x00, 0x21, // sta $2100
    0x8D, 0x18, 0x21, // sta $2118
    0x80, 0xEF, // bra loop
];

fn machine(rom: Vec<u8>) -> Machine {
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    machine.set_sample_rate(SAMPLE_RATE);
    machine
}

fn synthetic() -> Machine {
    machine(common::synth_lorom(0x8000, PROGRAM))
}

/// The test ROM's bytes, or `None` when the suite is not on this machine.
fn test_rom() -> Option<Vec<u8>> {
    let root = std::env::var_os("TENDY_TEST_ROMS")?;
    let path = PathBuf::from(root).join(TEST_ROM);
    if !path.is_file() {
        println!("skipped: {} is not present", path.display());
        return None;
    }
    Some(std::fs::read(&path).unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display())))
}

/// FNV-1a over the little-endian bytes of the drained stereo samples.
struct AudioHash(u64);

impl AudioHash {
    fn new() -> AudioHash {
        AudioHash(tendy_snes::state::fnv1a64(&[]))
    }

    fn feed(&mut self, samples: &[f32]) {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_bits().to_le_bytes());
        }
        self.0 = fnv1a64_continue(self.0, &bytes);
    }
}

/// Drains everything the APU has produced so far into `hash`.
fn drain(machine: &mut Machine, hash: &mut AudioHash) {
    let mut buffer = [0.0f32; 8192];
    loop {
        let frames = machine.drain_audio(&mut buffer);
        if frames == 0 {
            break;
        }
        hash.feed(&buffer[..frames * 2]);
        if frames < buffer.len() / 2 {
            break;
        }
    }
}

/// Runs `frames` frames, hashing each completed frame and the audio drained after it.
fn run_hashed(machine: &mut Machine, frames: u64) -> (Vec<u64>, u64) {
    let mut pictures = Vec::with_capacity(frames as usize);
    let mut audio = AudioHash::new();
    for _ in 0..frames {
        machine.run_frame();
        pictures.push(machine.last_frame_checksum());
        drain(machine, &mut audio);
    }
    (pictures, audio.0)
}

/// Steps whole instructions until the PPU is on `line` and past its first dot, i.e. mid-frame.
fn step_to_mid_frame(machine: &mut Machine, line: u16) {
    assert!(line < LINES_PER_FRAME as u16);
    while machine.bus.ppu.line != line || machine.bus.ppu.dot == 0 {
        machine.cpu.step(&mut machine.bus);
        assert!(u32::from(machine.bus.ppu.dot) < DOTS_PER_LINE);
    }
}

/// The shared round trip: `original` sits at the save point; a fresh machine that loads its state
/// must produce the same next `frames` frames, the same audio and the same end state. `mid_frame`
/// drops the one transitional picture a save taken inside a frame leaves behind.
fn resume_agrees(mut original: Machine, frames: u64, fresh: impl Fn() -> Machine, mid_frame: bool) {
    let state = original.encode_state();
    let (pictures, audio) = run_hashed(&mut original, frames);
    let end = original.encode_state();

    let mut restored = fresh();
    restored
        .decode_state(&state)
        .expect("the state is accepted");
    let (restored_pictures, restored_audio) = run_hashed(&mut restored, frames);
    let restored_end = restored.encode_state();

    let (pictures, restored_pictures) = if mid_frame {
        (pictures[1..].to_vec(), restored_pictures[1..].to_vec())
    } else {
        (pictures, restored_pictures)
    };
    assert_eq!(restored_pictures, pictures, "frames diverge after a load");
    assert_eq!(restored_audio, audio, "audio diverges after a load");
    if mid_frame {
        // `checksum` folds every picture, and the rows above the save point were rendered before
        // the save and are not in the state, so the running fold cannot agree after a mid-frame
        // load — nor can the state's trailing hash of it. Every other byte must, and the per-frame
        // pictures above already prove the device state.
        assert_state_eq(
            &without_picture_fold(&restored_end),
            &without_picture_fold(&end),
        );
    } else {
        assert_state_eq(&restored_end, &end);
        assert_eq!(restored.frame_checksum(), original.frame_checksum());
    }
    assert_eq!(restored.frames, original.frames);
    assert_eq!(
        restored.last_frame_checksum(),
        original.last_frame_checksum()
    );
}

/// Blanks the running picture fold and the trailing hash derived from it. The fold sits after the
/// magic, version and `frames`; the trailing hash is the last eight bytes.
fn without_picture_fold(state: &[u8]) -> Vec<u8> {
    let start = STATE_MAGIC.len() + size_of::<u16>() + size_of::<u64>();
    let mut copy = state.to_vec();
    copy[start..start + size_of::<u64>()].fill(0);
    copy[state.len() - size_of::<u64>()..].fill(0);
    copy
}

/// One way to put a single machine field out of range.
type Poison = fn(&mut Machine);

/// Byte equality with the first differing offset named, so a field that is not carried shows up as
/// a position in the state rather than a wall of bytes.
fn assert_state_eq(actual: &[u8], expected: &[u8]) {
    if actual == expected {
        return;
    }
    let at = actual
        .iter()
        .zip(expected)
        .position(|(left, right)| left != right)
        .unwrap_or(actual.len().min(expected.len()));
    let from = at.saturating_sub(8);
    let to = (at + 8).min(actual.len()).min(expected.len());
    panic!(
        "states differ at byte {at} (lengths {} and {}): {:?} != {:?}",
        actual.len(),
        expected.len(),
        &actual[from..to],
        &expected[from..to]
    );
}

#[test]
fn test_rom_round_trip() {
    let Some(rom) = test_rom() else {
        return;
    };
    let mut runner = machine(rom.clone());
    run_hashed(&mut runner, 60);
    let fresh = || machine(rom.clone());
    resume_agrees(runner, 30, fresh, false);
}

#[test]
fn test_rom_mid_frame_round_trip() {
    let Some(rom) = test_rom() else {
        return;
    };
    let mut runner = machine(rom.clone());
    run_hashed(&mut runner, 30);
    step_to_mid_frame(&mut runner, 100);
    let fresh = || machine(rom.clone());
    resume_agrees(runner, 15, fresh, true);
}

#[test]
fn synthetic_round_trip() {
    let mut machine = synthetic();
    let (pictures, _) = run_hashed(&mut machine, 30);
    assert!(
        pictures.iter().any(|&hash| hash != pictures[0]),
        "the program must change the picture"
    );
    resume_agrees(machine, 20, synthetic, false);
}

#[test]
fn synthetic_mid_frame_round_trip() {
    let mut machine = synthetic();
    run_hashed(&mut machine, 6);
    machine.run_frame(); // leave the APU queue un-drained: the state must carry it
    step_to_mid_frame(&mut machine, 100);
    resume_agrees(machine, 10, synthetic, true);
}

#[test]
fn pending_audio_survives_the_round_trip() {
    let mut original = synthetic();
    original.run_frame();
    let state = original.encode_state();
    let mut restored = synthetic();
    restored
        .decode_state(&state)
        .expect("the state is accepted");

    let mut before = [0.0f32; 8192];
    let mut after = [0.0f32; 8192];
    let count_before = original.drain_audio(&mut before);
    let count_after = restored.drain_audio(&mut after);
    assert!(count_before > 0, "a frame must produce samples");
    assert_eq!(
        count_after, count_before,
        "the pending sample count differs"
    );
    assert_eq!(&before[..count_before * 2], &after[..count_after * 2]);
}

#[test]
fn save_load_save_is_byte_identical() {
    let mut machine = synthetic();
    run_hashed(&mut machine, 12);
    machine.run_frame(); // non-empty audio queue too
    step_to_mid_frame(&mut machine, 100);
    let state = machine.encode_state();

    let mut fresh = synthetic();
    fresh.decode_state(&state).expect("the state is accepted");
    assert_eq!(fresh.encode_state(), state);
    assert_eq!(fresh.frames, machine.frames);
    assert_eq!(fresh.bus.ppu.line, machine.bus.ppu.line);
    assert_eq!(fresh.bus.ppu.dot, machine.bus.ppu.dot);
}

#[test]
fn every_device_field_round_trips() {
    let rom = common::synth_cart(tendy_snes::cart::Board::LoRom, 0x8000, 0x02, 0x03, PROGRAM);
    let mut original = machine(rom.clone());
    let other = machine(rom);

    original.bus.wram[0] = 0xA5;
    original.bus.wram[0x1_FFFF] = 0x5A;
    original.bus.master = 0x0102_0304_0506_0708;
    original.bus.mdr = 0xC3;
    original.bus.dma_open = 0x99;
    original.bus.dma_open_latch = true;
    original.bus.nmi_latch = true;
    original.bus.stall = 40;
    original.cpu.a = 0x1234;
    original.cpu.x = 0x2345;
    original.cpu.y = 0x3456;
    original.cpu.d = 0x4567;
    original.cpu.s = 0x5678;
    original.cpu.pc = 0x6789;
    original.cpu.p = Flags(0x7F);
    original.cpu.pb = 0x88;
    original.cpu.db = 0x99;
    original.cpu.waiting = true;
    original.cpu.irq_line = true;
    original.bus.ppu.frame_ready = true;
    original.bus.ppu.obj_range_overflow = true;
    original.bus.ppu.vram_prefetch = 0xAB;
    original.bus.ppu.m7_product = -123_456;
    original.bus.ppu.fixed_colour = 0x7FFF;
    original.bus.ppu.latch_h = 0x0123;
    original.bus.ppu.latch_v = 0x0145;
    original.bus.ppu.hv_read = 0x1F;
    original.bus.ppu.field = true;
    original.bus.ppu.frames = 0x0A0B_0C0D_0E0F_1011;
    original.bus.ppu.vram[0x1234] = 0x5A;
    original.bus.ppu.oam[0x1FF] = 0xA5;
    original.bus.ppu.cgram[0x1FE] = 0x7F1F;
    original.bus.apu.ram[0x1234] = 0x5A;
    original.bus.apu.to_apu[2] = 0x42;
    original.bus.apu.from_apu[3] = 0x24;
    original.bus.apu.ipl_index = 3;
    original.bus.apu.mask = 0xE0;
    original.bus.apu.dsp.mask = 0xE0;
    original.bus.apu.cycles = 0x1112_1314_1516_1718;
    original.bus.dma.channels[5].dmap = 0x07;
    original.bus.dma.channels[5].bbad = 0x18;
    original.bus.dma.channels[5].a1t = tendy_snes::addr::Addr(0x01_2345);
    original.bus.dma.channels[5].das = 0x4567;
    original.bus.dma.channels[5].a2a = 0x89AB;
    original.bus.dma.channels[5].indirect = tendy_snes::addr::Addr(0x02_3456);
    original.bus.dma.channels[5].line_end = true;
    original.bus.dma.mdmaen = 0x20;
    original.bus.dma.hdmaen = 0x80;
    original.bus.io.nmitimen = 0xB1;
    original.bus.io.memsel = 0x01;
    original.bus.io.joy_strobe = 0x01;
    original.bus.io.htime = 0x01D5;
    original.bus.io.vtime = 0x0080;
    original.bus.io.nmi_flag = true;
    original.bus.io.irq_flag = true;
    original.bus.io.auto_joy = true;
    original.bus.io.hblank = true;
    original.bus.io.mpy_result = 0x1234;
    original.bus.io.div_result = 0x4567;
    original.bus.io.div_remainder = 0x89AB;
    original.bus.io.wmadd = 0x0001_2345;
    original.bus.joypad.set(Buttons(0xA55A));
    original.bus.joypad.index = 3;
    original.bus.joypad.strobe = 1;
    original.bus.joypad.auto = true;
    original.bus.joypad.auto_timer = 0x0B00;
    original.bus.joypad.shift[2] = 0x1234;
    original.bus.cart.ram[0] = 0x5A;
    original.bus.cart.ram[0x7FF] = 0xA5;
    original.frames = 77;
    original.checksum = 0x0102_0304_0506_0708;
    original.last_frame_checksum = 0x1112_1314_1516_1718;

    let state = original.encode_state();
    let mut restored = other;
    restored
        .decode_state(&state)
        .expect("the state is accepted");
    assert_state_eq(&restored.encode_state(), &state);
}

#[test]
fn host_only_buffers_are_not_carried() {
    let mut original = synthetic();
    run_hashed(&mut original, 3);
    original.bus.ppu.trace_on = true;
    original.bus.ppu.trace_len = 7;
    original.bus.write_log = Some(vec![tendy_snes::bus::WriteRecord::default()].into_boxed_slice());
    // The picture buffer is a host-side output: a restored machine repaints every visible row
    // before its next completed frame, so carrying it would only inflate the state.
    original.bus.ppu.frame.fill(0x1234);
    let state = original.encode_state();

    let mut restored = synthetic();
    restored
        .decode_state(&state)
        .expect("the state is accepted");
    assert!(!restored.bus.ppu.trace_on);
    assert_eq!(restored.bus.ppu.trace_len, 0);
    assert!(restored.bus.write_log.is_none());
    assert_ne!(
        restored.bus.ppu.frame[100 * tendy_snes::WIDTH],
        original.bus.ppu.frame[100 * tendy_snes::WIDTH]
    );
}

#[test]
fn a_truncated_state_is_refused_and_changes_nothing() {
    let mut machine = synthetic();
    run_hashed(&mut machine, 3);
    let state = machine.encode_state();
    let before = machine.encode_state();

    let mut lengths: Vec<usize> = (0..state.len()).step_by(997).collect();
    lengths.push(state.len() - 1);
    for length in lengths {
        let outcome = machine.decode_state(&state[..length]);
        assert!(outcome.is_err(), "a {length}-byte state was accepted");
        assert_eq!(
            machine.encode_state(),
            before,
            "a refused state changed the machine"
        );
    }
    machine
        .decode_state(&state)
        .expect("the whole state is accepted");
}

#[test]
fn a_foreign_or_old_state_is_refused_and_changes_nothing() {
    let mut first = synthetic();
    run_hashed(&mut first, 3);
    let good = first.encode_state();

    let mut wrong_magic = good.clone();
    wrong_magic[0] = b'X';
    let mut old = good.clone();
    old[STATE_MAGIC.len()..STATE_MAGIC.len() + 2].copy_from_slice(&2u16.to_le_bytes());
    let mut impossible = first.clone();
    impossible.bus.ppu.line = LINES_PER_FRAME as u16;
    let impossible = impossible.encode_state();

    let other_rom = machine(common::synth_lorom(0x8000, &[0xEA, 0xEA, 0xEA]));
    let cases: Vec<(Machine, Vec<u8>, StateError)> = vec![
        (synthetic(), wrong_magic, StateError::BadMagic),
        (synthetic(), old, StateError::BadVersion(2)),
        (synthetic(), impossible, StateError::BadValue),
        (other_rom, good, StateError::BadValue),
    ];
    for (mut machine, state, expected) in cases {
        let before = machine.encode_state();
        let outcome = machine.decode_state(&state);
        assert_eq!(outcome, Err(expected));
        assert_eq!(
            machine.encode_state(),
            before,
            "a refused state changed the machine"
        );
    }
}

#[test]
fn out_of_range_fields_are_refused_not_run() {
    let mut hot = synthetic();
    run_hashed(&mut hot, 2);
    let poisoned: Vec<(Poison, &str)> = vec![
        (|m| m.bus.ppu.line = LINES_PER_FRAME as u16, "PPU line"),
        (|m| m.bus.ppu.dot = DOTS_PER_LINE as u16, "PPU dot"),
        (|m| m.bus.stall = 41, "refresh stall"),
        (|m| m.bus.apu.sample_acc = 32, "sample accumulator"),
        (|m| m.bus.apu.dsp.echo.ram_index = 0x1_0000, "echo cursor"),
        (|m| m.bus.apu.dsp.samples = u64::MAX, "DSP sample count"),
        (|m| m.bus.apu.spc_acc = u32::MAX, "SPC clock ledger"),
        (|m| m.bus.apu.spc_acc = 0, "SPC clock ledger underflow"),
    ];
    for (poison, what) in poisoned {
        let mut machine = hot.clone();
        poison(&mut machine);
        let state = machine.encode_state();
        let mut target = synthetic();
        let before = target.encode_state();
        assert_eq!(
            target.decode_state(&state),
            Err(StateError::BadValue),
            "{what} was accepted"
        );
        assert_eq!(target.encode_state(), before);
    }
}

#[test]
fn the_version_2_state_is_refused() {
    let mut body = Vec::new();
    body.extend_from_slice(&2u16.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    body.extend_from_slice(&0u64.to_le_bytes());
    let mut state = Vec::new();
    state.extend_from_slice(&STATE_MAGIC);
    state.extend_from_slice(&body);
    state.extend_from_slice(&tendy_snes::state::fnv1a64(&body).to_le_bytes());
    assert_eq!(state.len(), 38);

    let mut machine = synthetic();
    let before = machine.encode_state();
    assert_eq!(machine.decode_state(&state), Err(StateError::BadVersion(2)));
    assert_eq!(machine.encode_state(), before);
}
