//! SGB sound: what a cartridge's sound commands are
//! answered with here.
//!
//! Pan Docs, *SGB Command $08 — SOUND* is the whole register: byte 1 and 2 the two effect codes,
//! byte 3 their pitches and volumes, byte 4 the music (BGM) score code. There is no Game Boy side
//! register for SGB sound — Pan Docs' *Memory Map* gives `FF80-FFFE` as "High RAM (HRAM)" — and the
//! only audio path Pan Docs describes is the SNES one: *SGB Command $09 — SOU_TRN* transfers sound
//! code and score data to "SNES Audio Processing Unit memory (APU-RAM)", whose memory map (*SGB
//! Command $09 — SOU_TRN*) is a Super NES APU, not a Game Boy.
//!
//! The emulator has a Super NES APU (`tendy_gb::sgb::sound::SgbSound`, `tendy_gb::sgb::spc`), so the
//! command is now answered by real hardware - but only when the user supplies the SGB BIOS file:
//! with no `sgb_rom` there is no chip, and `SOUND`'s effects stay silent, as they were. The jump
//! table between the two - which effect code keys which voice - is the cartridge's own program's
//! business (`docs/SGB-SOUND.md`), and the machine sends an upload to the chip.
//!
//! The `SOUND` test below is what a cartridge can observe either way: its packet is understood -
//! decoded as `SOUND`, not swallowed as an unknown code - and its four bytes are readable back.
//!
//! The rest of this file is the chip's own wiring: that no file means no chip and no
//! sample moved, that a chip only exists inside a Super Game Boy, that a tone on the chip reaches
//! the mixer, that frames clock it, that a state round-trips with it, that the user's cartridge
//! image is checked, and that `SOU_TRN`'s 4 KiB parses into the packets Pan Docs describes.

mod common;

use tendy_gb::sgb::packets::SoundRequest;
use tendy_gb::sgb::{Command, Packet, Sgb};

/// One write per pulse, the way a game drives `$FF00`: `0x10` a one bit, `0x20` a zero, stop last.
fn bus_send(m: &mut tendy_gb::Machine, bytes: &[u8]) {
    m.bus.write(0xFF00, 0x00);
    for &byte in bytes {
        for bit in 0..8 {
            m.bus
                .write(0xFF00, if (byte >> bit) & 1 == 1 { 0x10 } else { 0x20 });
        }
    }
    m.bus.write(0xFF00, 0x20);
}

/// `SOUND` (`$08`), length 1: header `$41`, then Pan Docs' four bytes.
fn sound_packet() -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x41; // $08 << 3 | 1
    bytes[1] = 0x1D; // effect A: Level UP (Pan Docs' *Sound Effect A IDs*)
    bytes[2] = 0x04; // effect B: Wind (Pan Docs' *Sound Effect B IDs*)
    bytes[3] = 0b10_01_11_10; // A pitch 2, A volume 1, B pitch 3, B volume 2
    bytes[4] = 0x00; // no music score code
    Packet { bytes }
}

/// `$08` decodes as `SOUND`, and its four bytes are recorded: the packet is understood.
#[test]
fn a_sound_command_is_recorded() {
    assert_eq!(
        Command::from_code(0x08),
        Command::Sound,
        "Pan Docs calls $08 SGB Command $08 — SOUND"
    );

    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    assert_eq!(m.bus.ppu.sgb.sound(), None, "no SOUND has arrived yet");

    bus_send(&mut m, &sound_packet().bytes);
    let request = m
        .bus
        .ppu
        .sgb
        .sound()
        .expect("the packet reached Sgb::apply as SOUND");
    assert_eq!(request.effect_a, 0x1D);
    assert_eq!(request.effect_b, 0x04);
    assert_eq!(request.attributes, 0b10_01_11_10);
    assert_eq!(request.music, 0x00);
}

/// The record is the answer, not sound: the command leaves the picture alone, and the APU's output
/// is byte for byte what a packet of the same length with no meaning costs — the SOUND packet is not
/// a path into the mixer.
///
/// The control is a `$1F` packet: the same 130 pulses, decoded and dropped, so both runs spend the
/// same cycles and any difference in the samples is the SOUND command's. The control also makes
/// sound, so "identical" is not two silent buffers agreeing.
#[test]
fn a_sound_command_plays_nothing() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    let pixels = m.bus.ppu.sgb.border().pixels().to_vec();
    let mask = m.bus.ppu.sgb.mask();
    bus_send(&mut m, &sound_packet().bytes);
    assert_eq!(
        m.bus.ppu.sgb.border().pixels(),
        &pixels[..],
        "no picture moved"
    );
    assert_eq!(m.bus.ppu.sgb.mask(), mask, "no mask moved");

    let with_sound = two_frames(&sound_packet().bytes);
    let control = two_frames(&unknown_packet().bytes);
    assert!(
        control.iter().any(|&sample| sample != 0.0),
        "the control run produces sound, so the comparison below means something"
    );
    assert_eq!(
        with_sound.len(),
        control.len(),
        "the same number of samples"
    );
    if let Some(at) = with_sound.iter().zip(&control).position(|(a, b)| a != b) {
        panic!(
            "the SGB SOUND command changed sample {at} of {}: {} became {}",
            with_sound.len(),
            control[at],
            with_sound[at]
        );
    }
}

/// A `$1F` packet: a command Pan Docs' summary does not list, so it is decoded and dropped, and it
/// costs the same 130 pulses as the SOUND packet the test compares it with.
fn unknown_packet() -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x1F << 3 | 1;
    Packet { bytes }
}

/// Two frames of APU output from a fresh SGB machine, after sending one packet.
fn two_frames(packet: &[u8]) -> Vec<f32> {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    bus_send(&mut m, packet);
    let mut samples = vec![0.0f32; 16_384];
    let mut frames = 0;
    for _ in 0..2 {
        m.run_frame();
        frames = m.drain_audio(&mut samples);
    }
    samples[..frames * 2].to_vec()
}

/// `SOU_TRN`'s payload into the chip's RAM, and the DSP set up the way a driver would: instrument 0
/// is a looping square wave on voice 0 at a fixed GAIN (the same BRR block the chip's own tests use,
/// so a wrong sample shows up as a wrong number rather than as noise).
fn arm_the_chip(sound: &mut tendy_gb::sgb::SgbSound) {
    sound.load_block(0x0200, &[0x00, 0x03, 0x00, 0x03]); // DIR: start and loop at $0300
    let mut block = [0u8; 9];
    block[0] = (12 << 4) | 3; // shift 12, filter 0, end+loop
    block[1..].copy_from_slice(&[0x77, 0x77, 0x99, 0x99, 0x77, 0x77, 0x99, 0x99]);
    sound.load_block(0x0300, &block);
    let chip = sound.chip_mut();
    chip.dsp.write(0x5D, 0x02); // DIR
    chip.dsp.write(0x6C, 0x00); // unmuted, echo writes on
    chip.dsp.write(0x0C, 0x7F); // MVOLL
    chip.dsp.write(0x1C, 0x7F); // MVOLR
    chip.dsp.write(0x00, 0x7F); // V0VOLL
    chip.dsp.write(0x01, 0x7F); // V0VOLR
    chip.dsp.write(0x04, 0x00); // V0SRCN
    chip.dsp.write(0x02, 0x00); // V0PITCHL = $1000: 32 000 Hz
    chip.dsp.write(0x03, 0x10);
    chip.dsp.write(0x05, 0x00); // GAIN mode
    chip.dsp.write(0x07, 0x7F); // direct GAIN $7F
    chip.dsp.write(0x4C, 0x01); // KON voice 0
}

/// An SGB machine with the blank cartridge, its own SGB cartridge image installed, and the chip's
/// driver armed.
fn machine_with_a_chip() -> tendy_gb::Machine {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    m.set_sgb_sound(vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE]);
    arm_the_chip(m.bus.apu.sgb.as_mut().expect("the chip is installed"));
    m
}

/// Two frames of the machine's mixed stereo output.
fn frames_of(m: &mut tendy_gb::Machine) -> Vec<f32> {
    let mut samples = vec![0.0f32; 16_384];
    let mut frames = 0;
    for _ in 0..2 {
        m.run_frame();
        frames = m.drain_audio(&mut samples);
    }
    samples[..frames * 2].to_vec()
}

/// No SGB cartridge image, no chip, and every sample is what it was before this leg existed: a
/// machine that calls nothing is byte-identical to one whose setting simply has no file.
#[test]
fn without_a_file_there_is_no_chip_and_no_sample_moves() {
    let mut plain = common::machine(common::synth_rom(&[]));
    plain.bus.ppu.sgb = Sgb::on();
    let mut untouched = common::machine(common::synth_rom(&[]));
    untouched.bus.ppu.sgb = Sgb::on();

    assert!(plain.bus.apu.sgb.is_none(), "no file, no chip");
    assert_eq!(
        frames_of(&mut plain),
        frames_of(&mut untouched),
        "nothing about a machine with no SGB file differs from one that was never offered one"
    );
}

/// The mixer with no chip is not merely deterministic ([`without_a_file_there_is_no_chip_and_no_sample_moves`]):
/// it is the same audio `main` produced before this leg existed. This pin is the FNV-1a over the f32
/// bits of 60 frames of an SGB machine answering `SOUND` packets, measured on `main` at `03a1713` and
/// unchanged here; a future mixer edit that moves it must say which sample it moved and why.
#[test]
fn a_machine_with_no_sgb_file_mixes_the_audio_main_produced() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    // The same pulse stream as `bus_send`, with the `$30` start a real game sends first: the pin was
    // measured with this exact stream on `main` at `03a1713` and here, and both are 7 388 282 382 909
    // 378 851.
    m.bus.write(0xFF00, 0x30);
    bus_send(&mut m, sound_packet().bytes.as_slice());
    let mut samples = vec![0.0f32; 16_384];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for _ in 0..60 {
        m.run_frame();
        let frames = m.drain_audio(&mut samples);
        for sample in &samples[..frames * 2] {
            hash ^= u64::from(sample.to_bits());
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    assert_eq!(
        hash, 7_388_282_382_909_378_851,
        "the pin `main` produced at 03a1713"
    );
}

/// The chip is installed only inside a Super Game Boy: on a DMG or a CGB the call does nothing.
#[test]
fn the_chip_exists_only_inside_a_super_game_boy() {
    let mut dmg = common::machine(common::synth_rom(&[]));
    dmg.set_sgb_sound(vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE]);
    assert!(
        dmg.bus.apu.sgb.is_none(),
        "a DMG has no SGB sound, whatever file the setting names"
    );

    let mut sgb = common::machine(common::synth_rom(&[]));
    sgb.bus.ppu.sgb = Sgb::on();
    sgb.set_sgb_sound(vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE]);
    assert!(sgb.bus.apu.sgb.is_some(), "an SGB gets the chip");
}

/// A tone the driver keyed on reaches the audio the front end drains: the chip is wired into the
/// mixer, resampled from 32 kHz to the output rate, and audible above the Game Boy's own silence.
#[test]
fn a_tone_on_the_sgb_chip_reaches_the_mixer() {
    let mut m = machine_with_a_chip();
    let samples = frames_of(&mut m);
    let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    assert!(
        peak > 0.005,
        "a keyed-on SGB voice at volumes $7F is audible through the mixer, saw peak {peak}"
    );
    assert!(
        samples.iter().any(|&sample| sample > 0.0) && samples.iter().any(|&sample| sample < 0.0),
        "a square wave swings both ways"
    );
}

/// The chip's clock is driven by the Game Boy's own dots: two frames advance it by two frames' worth
/// of SPC cycles, so the two machines cannot drift apart.
#[test]
fn running_frames_clocks_the_chip() {
    let mut m = machine_with_a_chip();
    let before = m.bus.apu.sgb.as_ref().unwrap().cycles();
    m.run_frame();
    m.run_frame();
    let after = m.bus.apu.sgb.as_ref().unwrap().cycles();
    // 140 448 dots × 1 024 000 / 4 194 304 = 34 289 cycles, ± one instruction.
    assert!(
        after.wrapping_sub(before).abs_diff(34_289) <= 12,
        "two frames are 34 289 SPC cycles +- one instruction, saw {}",
        after - before
    );
}

/// A chip's state — RAM, the SPC700, the DSP, the clock's remainder — survives a save and a load, and
/// the audio after the load is the audio the machine would have produced.
#[test]
fn a_machine_with_a_chip_saves_and_loads() {
    let mut m = machine_with_a_chip();
    m.run_frame();
    let saved = m.encode_state();

    let mut resumed = common::machine(common::synth_rom(&[]));
    resumed.bus.ppu.sgb = Sgb::on();
    resumed
        .decode_state(&saved)
        .expect("a state with a chip loads into a machine with one");

    assert!(
        resumed.bus.apu.sgb.is_some(),
        "the state brought the chip with it"
    );
    assert_eq!(
        resumed.bus.apu.sgb.as_ref().unwrap().cycles(),
        m.bus.apu.sgb.as_ref().unwrap().cycles(),
        "and the clock's remainder"
    );
    let after_load = frames_of(&mut resumed);
    let continued = frames_of(&mut m);
    assert_eq!(
        after_load.len(),
        continued.len(),
        "the same number of samples"
    );
    if let Some(at) = after_load.iter().zip(&continued).position(|(a, b)| a != b) {
        panic!(
            "the resumed machine's audio differs at sample {at} of {}: {} became {}",
            continued.len(),
            continued[at],
            after_load[at]
        );
    }
}

/// A state saved with no chip loads with no chip: the flag is written, so a DMG state does not grow
/// a chip on the way back in.
#[test]
fn a_machine_without_a_chip_saves_and_loads() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.run_frame();
    let saved = m.encode_state();
    let mut resumed = common::machine(common::synth_rom(&[]));
    resumed.decode_state(&saved).expect("the state loads");
    assert!(resumed.bus.apu.sgb.is_none(), "still no chip");
}

/// `load_rom`'s rules: the user's 256 KiB SGB cartridge image is accepted, and anything else is
/// refused in one line that says what was wrong — never a crash, never a guess.
#[test]
fn the_sgb_cartridge_image_is_checked() {
    use std::io::Write;

    let dir = std::env::temp_dir().join(format!("gb-sgb-rom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).expect("a scratch file");
        file.write_all(bytes).expect("the bytes");
        path
    };

    let mut good = vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE];
    good[0x7FC0..0x7FC0 + 13].copy_from_slice(b"Super GAMEBOY");
    let accepted = write("good.sfc", &good);
    assert_eq!(
        tendy_gb::sgb::sound::load_rom(&accepted).expect("a real SGB image loads"),
        good,
        "the file's bytes come back unchanged"
    );

    let short = write("short.sfc", &good[..0x7FC0]);
    let why = tendy_gb::sgb::sound::load_rom(&short).expect_err("a short file is refused");
    assert!(
        why.contains("262144") || why.contains("256"),
        "the refusal names the size: {why:?}"
    );

    let mut untitled = good.clone();
    untitled[0x7FC0] = b'x';
    let why = tendy_gb::sgb::sound::load_rom(&write("untitled.sfc", &untitled))
        .expect_err("a file without the title is refused");
    assert!(
        why.contains("Super GAMEBOY"),
        "the refusal names the title it looked for: {why:?}"
    );

    let why = tendy_gb::sgb::sound::load_rom(&dir.join("missing.sfc"))
        .expect_err("a file that is not there is refused");
    assert!(!why.is_empty(), "and says why");

    std::fs::remove_dir_all(&dir).ok();
}

/// `SOU_TRN`: header `$49` (`$09 << 3 | 1`), "Byte 1-F Not used (zero)" — the payload is in VRAM.
fn sou_trn() -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x49;
    Packet { bytes }
}

/// One data packet: size, APU address, bytes — all little-endian (Pan Docs, *SGB Command $09*).
fn data_packet(address: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(&address.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// The jump packet: "Must be $0000" in the size, then the S-APU address to start at.
fn jump_packet(address: u16) -> Vec<u8> {
    let mut out = vec![0u8, 0u8];
    out.extend_from_slice(&address.to_le_bytes());
    out
}

/// A block padded out to the 4 KiB the transfer reads.
fn block_of(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut block = vec![0u8; tendy_gb::sgb::transfer::TRN_BYTES];
    let mut at = 0;
    for packet in packets {
        block[at..at + packet.len()].copy_from_slice(packet);
        at += packet.len();
    }
    block
}

/// The packets a `SOU_TRN` block holds are parsed in order, with the jump packet's address kept and
/// nothing applied: "The sound code/data is sent by VRAM transfer as a contiguous list of packets."
#[test]
fn sou_trn_parses_its_packets_and_jump() {
    let block = block_of(&[
        data_packet(0x0400, &[0x12, 0x34, 0x56]),
        data_packet(0x2B00, &[0xAA; 5]),
        jump_packet(0x0400),
    ]);
    let parsed = tendy_gb::sgb::sound::parse_transfer(&block);
    assert_eq!(parsed.refused, None, "a whole transfer is not refused");
    assert_eq!(
        parsed.blocks,
        vec![(0x0400, vec![0x12, 0x34, 0x56]), (0x2B00, vec![0xAA; 5]),],
        "each packet keeps its APU address and its bytes, in order"
    );
    assert_eq!(
        parsed.jump,
        Some(0x0400),
        "the jump packet's address is the SGB BIOS' own N-SPC entry point"
    );
}

/// A zero size is the jump packet, and it ends the list: bytes after it are not another packet.
#[test]
fn a_zero_size_packet_is_the_jump_and_ends_the_list() {
    let mut block = block_of(&[data_packet(0x0400, &[1, 2]), jump_packet(0x0B00)]);
    block[0x20..0x24].copy_from_slice(&[0x7F, 0x00, 0x00, 0x10]); // garbage past the jump
    let parsed = tendy_gb::sgb::sound::parse_transfer(&block);
    assert_eq!(parsed.refused, None);
    assert_eq!(parsed.blocks, vec![(0x0400, vec![1, 2])]);
    assert_eq!(parsed.jump, Some(0x0B00), "the jump address is kept");
}

/// A packet whose data does not fit in the block is refused, and says so.
#[test]
fn a_packet_past_the_4_kib_is_refused() {
    let mut block = vec![0u8; tendy_gb::sgb::transfer::TRN_BYTES];
    // 4 096 bytes claimed at $0400, and the block has 4 092 left for header and data together.
    block[0..4].copy_from_slice(&[0x00, 0x10, 0x00, 0x04]);
    let parsed = tendy_gb::sgb::sound::parse_transfer(&block);
    assert!(
        parsed.refused.unwrap().contains("4 KiB"),
        "the refusal names the block: {:?}",
        parsed.refused
    );
    assert!(parsed.blocks.is_empty(), "and nothing of it is kept");
}

/// A packet that would run past `$FFFF` is refused rather than wrapped into the program area.
#[test]
fn a_packet_past_the_top_of_apu_ram_is_refused() {
    let block = block_of(&[data_packet(0xFFF8, &[0u8; 16])]);
    let parsed = tendy_gb::sgb::sound::parse_transfer(&block);
    assert!(
        parsed.refused.unwrap().contains("$FFFF") || parsed.refused.unwrap().contains("top"),
        "the refusal names the top of APU RAM: {:?}",
        parsed.refused
    );
    assert!(parsed.blocks.is_empty());

    // The last byte at $FFFF exactly is inside it.
    let block = block_of(&[data_packet(0xFFF0, &[0u8; 16])]);
    assert_eq!(tendy_gb::sgb::sound::parse_transfer(&block).refused, None);
}

/// A block that fills all 4 KiB and stops is a whole transfer: the jump packet is how a list ends
/// early, not how it must end.
#[test]
fn a_full_block_needs_no_jump_packet() {
    let payload = vec![0x5Au8; tendy_gb::sgb::transfer::TRN_BYTES - 4];
    let parsed = tendy_gb::sgb::sound::parse_transfer(&block_of(&[data_packet(0x0400, &payload)]));
    assert_eq!(parsed.refused, None);
    assert_eq!(parsed.jump, None);
    assert_eq!(parsed.blocks.len(), 1);
    assert_eq!(parsed.blocks[0].1.len(), payload.len());
}

/// A header that is not four bytes is refused, and an empty block is an empty transfer rather than
/// a fault.
#[test]
fn a_truncated_header_is_refused() {
    assert_eq!(
        tendy_gb::sgb::sound::parse_transfer(&[0x10, 0x00]).refused,
        Some("a packet header is four bytes")
    );
    let empty = tendy_gb::sgb::sound::parse_transfer(&[]);
    assert_eq!(empty.refused, None, "no packet, no fault");
    assert!(empty.blocks.is_empty() && empty.jump.is_none());
}

/// The command's 4 KiB goes through the five-frame transfer window like the other `_TRN` commands,
/// and what comes out is stored on the SGB for the next leg, not uploaded.
#[test]
fn sou_trn_arrives_through_the_window_and_is_kept() {
    assert!(
        tendy_gb::sgb::transfer::is_trn(&sou_trn()),
        "SOU_TRN's 4 KiB is read by the transfer, so it waits its five frames"
    );

    let mut sgb = Sgb::on();
    assert!(sgb.sound_transfer().is_none(), "nothing yet");
    let block = block_of(&[data_packet(0x0400, &[9, 9, 9]), jump_packet(0x0400)]);
    sgb.apply(&sou_trn(), &block);
    let parsed = sgb.sound_transfer().expect("the block is kept");
    assert_eq!(parsed.blocks, vec![(0x0400, vec![9, 9, 9])]);
    assert_eq!(parsed.jump, Some(0x0400));
    assert_eq!(parsed.refused, None);
}

// --- The driver the cartridge carries, and the two commands that reach it --------------

/// The user's own SGB cartridge image: the `#[ignore]`d proofs read `sgb-cartridge.bin` from
/// `TENDY_TEST_ROMS` and no byte of it is committed.
fn user_rom() -> Vec<u8> {
    let folder = std::env::var("TENDY_TEST_ROMS")
        .expect("TENDY_TEST_ROMS names the folder holding sgb-cartridge.bin");
    let path = std::path::Path::new(&folder).join("sgb-cartridge.bin");
    tendy_gb::sgb::sound::load_rom(&path).expect("sgb-cartridge.bin in TENDY_TEST_ROMS loads")
}

/// A cartridge that jumps to itself and never writes a register: the Game Boy's own output settles
/// to one level, so anything above it came from the sound chip.
fn idle_rom() -> Vec<u8> {
    common::synth_rom(&common::entry(&[]))
}

/// An SGB machine, with the cartridge image as its sound chip's driver when one is given.
fn sgb_machine(rom: Option<Vec<u8>>) -> tendy_gb::Machine {
    let mut m = common::machine(idle_rom());
    m.bus.ppu.sgb = Sgb::on();
    if let Some(rom) = rom {
        m.set_sgb_sound(rom);
    }
    m
}

/// The chip alone, no mixer: its own 32 kHz samples over `frames` frames of Game Boy dots.
fn chip_peak(chip: &mut tendy_gb::sgb::SgbSound, frames: usize) -> (f32, bool, bool) {
    let mut samples = vec![0.0f32; 4096];
    let mut peak = 0.0f32;
    let (mut up, mut down) = (false, false);
    for _ in 0..frames {
        chip.tick(70_224);
        loop {
            let n = chip.read_samples(&mut samples);
            if n == 0 {
                break;
            }
            for &sample in &samples[..n * 2] {
                peak = peak.max(sample.abs());
                up |= sample > 0.0;
                down |= sample < 0.0;
            }
        }
    }
    (peak, up, down)
}

/// The largest |sample| the mixer sends in the next `frames` frames, and every voice the chip had
/// keyed while it ran.
fn peak_over(m: &mut tendy_gb::Machine, frames: usize) -> (f32, u8) {
    let mut samples = vec![0.0f32; 16384];
    let mut peak = 0.0f32;
    let mut keyed = 0u8;
    for _ in 0..frames {
        m.run_frame();
        let n = m.drain_audio(&mut samples);
        for &sample in &samples[..n * 2] {
            peak = peak.max(sample.abs());
        }
        if let Some(chip) = m.bus.apu.sgb.as_ref() {
            keyed |= chip.chip().dsp.keyed_voices();
        }
    }
    (peak, keyed)
}

/// The driver the cartridge carries, copied straight into APU RAM without the 97-frame handshake,
/// runs: it settles the DSP, answers a port write, and plays an effect on its own.
///
/// What was measured on the user's file, for the record: `$2140` written with `$5A` came back three
/// frames later (the driver walks the four ports one per pass, so one port is not a one-frame trip);
/// effect A `$0B` keys voices in the `$4C`/`$5C` write stream, and the chip's own peak reaches 0.073.
#[test]
#[ignore = "reads sgb-cartridge.bin from TENDY_TEST_ROMS"]
fn the_loaded_driver_runs_answers_the_ports_and_plays_an_effect() {
    let rom = user_rom();
    let mut chip = tendy_gb::sgb::SgbSound::new();
    chip.boot(&rom).expect("the cartridge's own boot table");

    let (quiet, ..) = chip_peak(&mut chip, 10);
    assert!(
        quiet < 1e-3,
        "the driver is silent until a command arrives: peak {quiet}"
    );
    chip.play(SoundRequest {
        effect_a: 0x0B,
        effect_b: 0x00,
        attributes: 0x00,
        music: 0x00,
    });
    let (peak, up, down) = chip_peak(&mut chip, 30);
    assert!(peak > 0.01, "effect A $0B plays on the chip: peak {peak}");
    assert!(up && down, "a waveform, not a DC offset");

    let mut m = sgb_machine(Some(rom));
    let _ = peak_over(&mut m, 15);
    m.bus.apu.sgb.as_mut().expect("a chip").write_port(0, 0x5A);
    let mut echoed_at = None;
    for frame in 1..=8 {
        m.run_frame();
        let mut samples = [0.0f32; 4096];
        m.drain_audio(&mut samples);
        if m.bus.apu.sgb.as_mut().expect("a chip").read_port(0) == 0x5A {
            echoed_at = Some(frame);
            break;
        }
    }
    let echoed_at = echoed_at.expect("the driver echoes $2140 back");
    assert!(
        echoed_at <= 6,
        "$2140 came back after {echoed_at} frames; measured 3"
    );

    let mut control = sgb_machine(None);
    let _ = peak_over(&mut control, 15);
    let mut packet = [0u8; 16];
    packet[0] = 0x41; // $08 << 3 | 1
    packet[1] = 0x0B; // effect A, attributes volume 0
    bus_send(&mut m, &packet);
    bus_send(&mut control, &packet);
    let (with_chip, keyed) = peak_over(&mut m, 40);
    let (no_chip, _) = peak_over(&mut control, 40);
    assert!(
        with_chip > no_chip + 0.005,
        "SOUND effect A $0B is audible in the mix: peak {with_chip}, no-chip control {no_chip}"
    );
    assert_ne!(keyed, 0, "the driver keyed voices: {keyed:02X}");
}

/// `SOU_TRN`'s record list lands in APU RAM at the addresses it names, and the jump packet's address
/// is where the SPC700 then runs: a program that keys voice 0 on a square sample and answers
/// `$2140`.
#[test]
#[ignore = "reads sgb-cartridge.bin from TENDY_TEST_ROMS"]
fn a_sou_trn_record_list_lands_in_apu_ram_and_plays() {
    let mut m = sgb_machine(Some(user_rom()));
    let (before, _) = peak_over(&mut m, 5);

    // DSP on, directory at $0200, voice 0 on sample 0 at full volume, gain mode, keyed, then answer
    // the S-CPU and loop on the jump itself.
    let mut program: Vec<u8> = vec![
        0x8F, 0x6C, 0xF2, 0x8F, 0x00, 0xF3, // FLG: unmuted, no soft reset
        0x8F, 0x0C, 0xF2, 0x8F, 0x7F, 0xF3, // main volume, left
        0x8F, 0x1C, 0xF2, 0x8F, 0x7F, 0xF3, // main volume, right
        0x8F, 0x5D, 0xF2, 0x8F, 0x02, 0xF3, // directory at $0200
        0x8F, 0x04, 0xF2, 0x8F, 0x00, 0xF3, // voice 0 plays sample 0
        0x8F, 0x00, 0xF2, 0x8F, 0x7F, 0xF3, // voice 0 volume, left
        0x8F, 0x01, 0xF2, 0x8F, 0x7F, 0xF3, // voice 0 volume, right
        0x8F, 0x02, 0xF2, 0x8F, 0x00, 0xF3, // pitch $1000
        0x8F, 0x03, 0xF2, 0x8F, 0x10, 0xF3, //
        0x8F, 0x05, 0xF2, 0x8F, 0x80, 0xF3, // ADSR1: gain mode
        0x8F, 0x07, 0xF2, 0x8F, 0x7F, 0xF3, // gain: full
        0x8F, 0x4C, 0xF2, 0x8F, 0x01, 0xF3, // KON: key voice 0
        0x8F, 0xA9, 0xF4, // the answer on $2140
        0x5F, 0x00, 0x2B, // JMP to itself, patched below
    ];
    let end = program.len();
    let loop_at = (end - 3) as u16 + 0x2B00;
    program[end - 2] = (loop_at & 0xFF) as u8;
    program[end - 1] = (loop_at >> 8) as u8;

    let directory = [0x00u8, 0x03, 0x00, 0x03]; // sample 0 starts and loops at $0300
    let mut sample = [0u8; 9];
    sample[0] = 0xC3; // range 12, filter 0, end and loop
    sample[1..].copy_from_slice(&[0x78, 0x78, 0x87, 0x87, 0x78, 0x78, 0x87, 0x87]);

    let block = block_of(&[
        data_packet(0x2B00, &program),
        data_packet(0x0200, &directory),
        data_packet(0x0300, &sample),
        jump_packet(0x2B00),
    ]);
    m.bus.ppu.sgb.apply(&sou_trn(), &block);
    let (after, _) = peak_over(&mut m, 20);

    let sound = m.bus.apu.sgb.as_ref().expect("a chip");
    assert_eq!(
        &sound.ram()[0x2B00..0x2B00 + end],
        &program[..],
        "the program is at the address its packet named"
    );
    assert_eq!(
        &sound.ram()[0x0300..0x0309],
        &sample[..],
        "and so is the sample"
    );
    assert_eq!(
        sound.chip().from_apu[0],
        0xA9,
        "the uploaded program runs and answers $2140"
    );
    assert!(
        (0x2B00..0x2B00 + end as u16).contains(&sound.chip().cpu.pc),
        "the SPC700 is inside the uploaded program, not the driver: pc ${:04X}",
        sound.chip().cpu.pc
    );
    assert!(
        after > before + 0.005,
        "the uploaded tone is audible in the mix: peak {after}, before the transfer {before}"
    );
}

/// A file that is not the cartridge `docs/SGB-SOUND.md` was measured on is refused with the offset
/// its table ended at, and the machine keeps a silent chip rather than running a wrong program.
#[test]
fn a_file_without_the_drivers_table_leaves_the_chip_silent() {
    let mut sound = tendy_gb::sgb::SgbSound::new();
    let refused = sound
        .boot(&vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE])
        .expect_err("a zeroed table is not the measured one");
    assert!(
        refused.contains("$3CAD9"),
        "the refusal names the terminator the document gives: {refused}"
    );
    assert_eq!(
        sound.chip().cpu.pc,
        0xFFC0,
        "nothing was started: still the boot ROM's first byte"
    );

    let mut m = common::machine(idle_rom());
    m.bus.ppu.sgb = Sgb::on();
    m.set_sgb_sound(vec![0u8; tendy_gb::sgb::sound::SGB_ROM_SIZE]);
    assert!(m.bus.apu.sgb.is_some(), "a chip, at power-on");
    let (peak, keyed) = peak_over(&mut m, 5);
    assert_eq!(keyed, 0, "and no voice ever keyed");
    assert!(peak < 0.26, "and it adds nothing audible: peak {peak}");
}

/// The port arithmetic of a `SOUND`, on a bare chip: no cartridge file, no driver, no ears.
///
/// `docs/SGB-SOUND.md` §2 gives the mapping — `$2141` effect A, `$2140` music score, `$2142` effect B,
/// `$2143` attributes, the `$2140` pair seen first. The two writes of a pair are atomic with respect to
/// the chip (it only reads its ports in `tick`), so what a test can pin is where each byte lands; the
/// order inside a pair is pinned by the trace in the document and by the ignored proof.
#[test]
fn a_sound_command_puts_each_byte_on_its_own_port() {
    let mut sound = tendy_gb::sgb::SgbSound::new();
    sound.play(SoundRequest {
        effect_a: 0x03,
        effect_b: 0x04,
        attributes: 0x10,
        music: 0x05,
    });
    // `read_port` is the chip's answer; what the SGB wrote sits in `to_apu`.
    assert_eq!(sound.chip().to_apu[0], 0x05, "music score on $2140");
    assert_eq!(sound.chip().to_apu[1], 0x03, "effect A on $2141");
    assert_eq!(sound.chip().to_apu[2], 0x04, "effect B on $2142");
    assert_eq!(sound.chip().to_apu[3], 0x10, "attributes on $2143");
}

/// The port arithmetic of a `SOU_TRN`: `$FF` first, the blocks into APU RAM, then the jump. A refused
/// list writes nothing — not even the `$FF`.
#[test]
fn a_sou_trn_writes_the_ff_the_blocks_and_the_jump_but_a_refused_one_writes_nothing() {
    let mut sound = tendy_gb::sgb::SgbSound::new();
    let transfer = tendy_gb::sgb::SoundTransfer {
        blocks: vec![(0x2B00, vec![0x11, 0x22, 0x33])],
        jump: Some(0x0400),
        refused: None,
    };
    sound.transfer(&transfer);
    assert_eq!(sound.chip().to_apu[0], 0xFF, "the driver's greeting");
    assert_eq!(&sound.ram()[0x2B00..0x2B03], &[0x11, 0x22, 0x33]);
    assert_eq!(
        sound.chip().cpu.pc,
        0x0400,
        "running from the terminator's entry"
    );

    let mut sound = tendy_gb::sgb::SgbSound::new();
    let refused = tendy_gb::sgb::SoundTransfer {
        blocks: vec![(0x2B00, vec![0x11, 0x22, 0x33])],
        jump: None,
        refused: Some("a block that is not the one Pan Docs describes"),
    };
    sound.transfer(&refused);
    assert_eq!(sound.chip().to_apu[0], 0x00, "no greeting");
    assert_eq!(sound.ram()[0x2B00], 0x00, "no half a record list");
}
