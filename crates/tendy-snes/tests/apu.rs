//! The APU as the machine sees it: the IPL handshake, the clock ratio and the tone.
//!
//! The handshake is driven the way fullsnes's "Uploader" drives it — the S-CPU's four port writes,
//! byte for byte — rather than through a cartridge, because the 65816 lane is not in this worktree.

use tendy_snes::apu::spc700::SpcBus;
use tendy_snes::apu::{Apu, Host, Ipl};
use tendy_snes::cart::Cartridge;
use tendy_snes::machine::Machine;
use tendy_snes::state::{StateReader, StateWriter, fnv1a64_continue};
use tendy_snes::{AUDIO_STAGING, MASTER_CLOCK_HZ, MASTER_TICKS_PER_FRAME, SPC_CLOCK_HZ};

mod common;

/// One command: the destination in `$2142`/`$2143`, `$2141` = 01 (transfer), then the kick.
fn kick(apu: &mut Apu, destination: u16, kick_byte: u8) {
    apu.write_port(2, destination as u8);
    apu.write_port(3, (destination >> 8) as u8);
    apu.write_port(1, 0x01);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
}

/// Polls `$2140` the way an uploader does, handing the SPC clocks while it waits. The boot ROM reads
/// the ports from software (§7.3), so a write is only seen once the SPC runs — never at the instant
/// the S-CPU makes it, which is exactly the bug that was fixed.
fn await_echo(apu: &mut Apu, want: u8) {
    for _ in 0..200_000 {
        if apu.read_port(0) == want {
            return;
        }
        apu.tick(64); // a couple of SPC cycles: one pass of the ROM's poll loop is 7-9
    }
    panic!(
        "the boot ROM never echoed ${want:02X}; $2140 holds ${:02X}",
        apu.read_port(0)
    );
}

/// Enough master clocks for the SPC to take several steps.
fn settle(apu: &mut Apu) {
    apu.tick(MASTER_TICKS_PER_FRAME / 60);
}

/// Transfers one block and returns the kick for the next command:
/// "kick=(index+2 AND FFh) OR 1".
fn transfer(apu: &mut Apu, kick_byte: u8, destination: u16, data: &[u8]) -> u8 {
    kick(apu, destination, kick_byte);
    for (index, byte) in data.iter().enumerate() {
        apu.write_port(1, *byte);
        apu.write_port(0, index as u8);
        await_echo(apu, index as u8);
    }
    ((data.len().wrapping_add(1) & 0xFF) | 1) as u8
}

/// The entry command: `$2141` = 0 and the kick.
fn entry(apu: &mut Apu, kick_byte: u8, entry_point: u16) {
    apu.write_port(2, entry_point as u8);
    apu.write_port(3, (entry_point >> 8) as u8);
    apu.write_port(1, 0x00);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
}

/// A DSP register write the way an SPC700 program makes it: `$F2` then `$F3`.
fn dsp_write(apu: &mut Apu, reg: u8, value: u8) {
    let mut bus = SpcBus {
        ram: &mut apu.ram,
        dsp: &mut apu.dsp,
        to_apu: &mut apu.to_apu,
        from_apu: &mut apu.from_apu,
    };
    apu.cpu.write(&mut bus, 0x00F2, reg);
    apu.cpu.write(&mut bus, 0x00F3, value);
}

/// The test driver's SPC program, written through the DSP ports: a looping square-wave BRR sample on
/// voice 0 with a fixed GAIN envelope.
fn arm_the_tone(apu: &mut Apu) {
    // DIR at $0200: instrument 0 starts and loops at $0300.
    let dir = 0x0200usize;
    apu.ram[dir] = 0x00;
    apu.ram[dir + 1] = 0x03;
    apu.ram[dir + 2] = 0x00;
    apu.ram[dir + 3] = 0x03;
    // One BRR block: shift 12, filter 0, code 3 (end+loop), a square wave of +7 and -7 nibbles.
    apu.ram[0x0300] = (12 << 4) | 3;
    let wave = [0x77u8, 0x77, 0x99, 0x99, 0x77, 0x77, 0x99, 0x99];
    apu.ram[0x0301..0x0309].copy_from_slice(&wave);

    dsp_write(apu, 0x5D, 0x02); // DIR
    dsp_write(apu, 0x6C, 0x00); // FLG: unmuted, echo writes on, noise stopped
    dsp_write(apu, 0x0C, 0x7F); // MVOLL
    dsp_write(apu, 0x1C, 0x7F); // MVOLR
    dsp_write(apu, 0x00, 0x7F); // V0VOLL
    dsp_write(apu, 0x01, 0x7F); // V0VOLR
    dsp_write(apu, 0x04, 0x00); // V0SRCN
    dsp_write(apu, 0x02, 0x00); // V0PITCHL = $1000: 32000 Hz
    dsp_write(apu, 0x03, 0x10);
    dsp_write(apu, 0x05, 0x00); // GAIN mode
    dsp_write(apu, 0x07, 0x7F); // direct GAIN $7F
    dsp_write(apu, 0x4C, 0x01); // KON voice 0
}

/// Runs `frames` frames of master clocks and returns the drained interleaved stereo samples.
fn run_frames(apu: &mut Apu, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut buffer = vec![0.0f32; AUDIO_STAGING];
    for _ in 0..frames {
        apu.tick(MASTER_TICKS_PER_FRAME);
        let n = apu.read_samples(&mut buffer);
        out.extend_from_slice(&buffer[..n * 2]);
    }
    out
}

#[test]
fn ipl_greets_with_aa_bb() {
    let mut apu = Apu::new(Host::Snes);
    assert_eq!(
        apu.ipl,
        Ipl::AwaitKick,
        "the handshake is armed at power-on"
    );
    assert_eq!(
        apu.read_port(0),
        0xAA,
        "the boot ROM writes $AA to $F4 first (fullsnes \"Boot ROM Disassembly\")"
    );
    assert_eq!(apu.read_port(1), 0xBB, "and $BB to $F5 second");
    assert_eq!(
        apu.read_port(2),
        0x00,
        "the other two latches start at zero"
    );
    assert_eq!(apu.read_port(3), 0x00);
    // The ROM's writes are I/O writes, so they land in RAM too (§7.1).
    assert_eq!(
        apu.ram[0x00F4], 0xAA,
        "the greeting lands in APU RAM at $F4"
    );
    assert_eq!(apu.ram[0x00F5], 0xBB, "and at $F5");
}

#[test]
fn upload_transfers_a_block_and_jumps() {
    let mut apu = Apu::new(Host::Snes);
    assert_eq!(apu.read_port(0), 0xAA);
    assert_eq!(apu.read_port(1), 0xBB);

    // A 256-byte program whose bytes are their own address, so a shifted block cannot pass.
    let mut program = Vec::new();
    for i in 0..256u32 {
        program.push((i as u8) ^ 0x5A);
    }
    let next = transfer(&mut apu, 0xCC, 0x0200, &program);
    for (i, byte) in program.iter().enumerate() {
        assert_eq!(
            apu.ram[0x0200 + i],
            *byte,
            "byte {i} of the block at $0200 after the handshake"
        );
    }
    assert_eq!(
        apu.ram[0x0200 + 0x100],
        0x00,
        "the transfer stops at the block's length"
    );

    entry(&mut apu, next, 0x0200);
    assert_eq!(
        apu.ipl,
        Ipl::Running,
        "the entry command starts the program"
    );
    assert_eq!(apu.cpu.pc, 0x0200, "and jumps to the entry point");
    assert_eq!(apu.cpu.a, 0, "with A = 0, as `jmp [0000+x]` requires");
    assert_eq!(apu.cpu.x, 0, "X = 0");
    assert_eq!(apu.cpu.y, 0, "Y = 0");
    assert_eq!(apu.cpu.sp, 0xEF, "SP = $EF");
    assert_eq!(apu.cpu.psw.0 & 0x02, 0x02, "PSW bit 1 (Z) is set");
    assert_eq!(apu.cpu.psw.0 & !0x02, 0, "PSW = $02 exactly");
}

#[test]
fn upload_with_interrupts_disabled_completes() {
    // §7.6's row: "the acknowledge for the last data byte lasts only for a few clock cycles… as a
    // workaround, disable IRQs and NMIs during upload". The workaround is that the uploader never
    // waits between the last data byte and the next command, which is what this drives.
    let mut apu = Apu::new(Host::Snes);
    let first: Vec<u8> = (0..64u32).map(|i| (i as u8) | 0x80).collect();
    let second: Vec<u8> = (0..32u32).map(|i| 0xF0u8.wrapping_add(i as u8)).collect();

    let next = transfer(&mut apu, 0xCC, 0x0400, &first);
    assert_eq!(next, 0x41, "(64 bytes: (64+1) AND FFh OR 1 is $41)");
    let next = transfer(&mut apu, next, 0x0500, &second);
    entry(&mut apu, next, 0x0400);

    for (i, byte) in first.iter().enumerate() {
        assert_eq!(apu.ram[0x0400 + i], *byte, "first block, byte {i}");
    }
    for (i, byte) in second.iter().enumerate() {
        assert_eq!(apu.ram[0x0500 + i], *byte, "second block, byte {i}");
    }
    assert_eq!(apu.ipl, Ipl::Running, "both blocks completed");
    assert_eq!(apu.cpu.pc, 0x0400, "and the entry point took");
}

#[test]
fn ports_are_two_independent_latches() {
    // "There are eight total registers accessed by these four addresses: four write-only output
    // ports to the S-CPU and four read-only input ports from the S-CPU."
    let mut apu = Apu::new(Host::Snes);
    apu.write_port(1, 0x22);
    assert_eq!(apu.to_apu[1], 0x22, "$2141 writes the S-CPU's output latch");
    assert_eq!(
        apu.read_port(1),
        0xBB,
        "$2141 still reads the SPC's latch, not the value the S-CPU just wrote"
    );

    // The SPC's own write at $F4 goes to the other latch and leaves the first alone.
    let mut bus = SpcBus {
        ram: &mut apu.ram,
        dsp: &mut apu.dsp,
        to_apu: &mut apu.to_apu,
        from_apu: &mut apu.from_apu,
    };
    apu.cpu.write(&mut bus, 0x00F4, 0x55);
    assert_eq!(
        apu.read_port(0),
        0x55,
        "$2140 reads what the SPC wrote at $F4"
    );
    assert_eq!(
        apu.to_apu[0], 0x00,
        "and the S-CPU's latch is untouched by it"
    );

    // The SPC reads the S-CPU's latch at the same address.
    apu.to_apu[0] = 0x11;
    let mut bus = SpcBus {
        ram: &mut apu.ram,
        dsp: &mut apu.dsp,
        to_apu: &mut apu.to_apu,
        from_apu: &mut apu.from_apu,
    };
    assert_eq!(
        apu.cpu.read(&mut bus, 0x00F4),
        0x11,
        "the SPC reads $2140's value at $F4"
    );

    // The other three indices are separate latches.
    apu.write_port(2, 0x33);
    apu.write_port(3, 0x44);
    assert_eq!(apu.to_apu[1..], [0x22, 0x33, 0x44]);
}

#[test]
fn sample_count_per_frame() {
    // §3.4: 357 368 master clocks give 532.5 samples, so a frame is 532 or 533 stereo frames.
    let mut apu = Apu::new(Host::Snes);
    for _ in 0..MASTER_TICKS_PER_FRAME {
        apu.tick(1);
    }
    let samples = apu.dsp.samples;
    assert!(
        (532..=533).contains(&samples),
        "one frame is 532 or 533 samples (357368 * 1024000/21477272/32 = 532.5), not {samples}"
    );

    let mut apu = Apu::new(Host::Snes);
    for _ in 0..10 {
        apu.tick(MASTER_TICKS_PER_FRAME);
    }
    let samples = apu.dsp.samples;
    assert!(
        (5324..=5326).contains(&samples),
        "ten frames are 5325 samples +-1 with no drift, not {samples}"
    );

    // The staged buffer is interleaved f32: two per sample.
    let mut apu = Apu::new(Host::Snes);
    apu.tick(MASTER_TICKS_PER_FRAME);
    let mut out = vec![0.0f32; AUDIO_STAGING];
    let frames = apu.read_samples(&mut out);
    assert!(
        (532..=533).contains(&frames),
        "drain_audio returns stereo frames, not f32: {frames}"
    );
}

#[test]
fn spc_clock_ratio_is_exact() {
    // "1 024 000 SPC700 cycles per 21 477 272 master clocks, remainder carried" (§3.4): one frame of
    // 357 368 master clocks is 17 038 SPC cycles. The SPC runs whole instructions, so it may be up
    // to one instruction (12 clocks) ahead of or behind the exact figure at any instant; what must
    // not happen is drift.
    let exact = |frames: u64| {
        (frames * u64::from(MASTER_TICKS_PER_FRAME) * u64::from(SPC_CLOCK_HZ))
            / u64::from(MASTER_CLOCK_HZ)
    };

    let mut apu = Apu::new(Host::Snes);
    for _ in 0..MASTER_TICKS_PER_FRAME {
        apu.tick(1);
    }
    assert!(
        apu.cycles.abs_diff(exact(1)) <= 12,
        "one frame is 17038 SPC cycles +- one instruction, not {}",
        apu.cycles
    );

    // The same clocks in one chunk, and in the S-CPU's own access sizes, must agree exactly.
    let mut chunked = Apu::new(Host::Snes);
    chunked.tick(MASTER_TICKS_PER_FRAME);
    assert_eq!(
        chunked.cycles, apu.cycles,
        "the ratio does not depend on how the clocks are handed over"
    );

    let mut accesses = Apu::new(Host::Snes);
    let mut ticks = 0;
    while ticks < MASTER_TICKS_PER_FRAME {
        let step = 6.min(MASTER_TICKS_PER_FRAME - ticks);
        accesses.tick(step);
        ticks += step;
    }
    assert_eq!(
        accesses.cycles, apu.cycles,
        "6-clock accesses accumulate to the same cycle count"
    );

    // Sixty frames: still within one instruction of the exact figure, which is what "no drift" means.
    let mut long = Apu::new(Host::Snes);
    long.tick(MASTER_TICKS_PER_FRAME * 60);
    assert!(
        long.cycles.abs_diff(exact(60)) <= 12,
        "sixty frames give {} +- one instruction SPC cycles, not {}",
        exact(60),
        long.cycles
    );

    // And the sample clock divides those cycles by 32, exactly.
    assert_eq!(
        long.dsp.samples,
        long.cycles / 32,
        "the DSP takes one sample every 32 SPC cycles, so {0} cycles are {0} / 32 samples",
        long.cycles
    );
}

#[test]
fn a_tone_produces_a_non_silent_buffer() {
    let mut apu = Apu::new(Host::Snes);
    arm_the_tone(&mut apu);
    let samples = run_frames(&mut apu, 2);
    assert!(!samples.is_empty(), "the APU produced samples");
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    assert!(
        peak > 0.01,
        "a keyed-on voice with volumes at $7F peaks well above 0.01, saw {peak}"
    );
    // The mixer's final "XOR FFFFh" inversion means silence is -1 LSB, not 0.
    let silent = samples.iter().all(|sample| *sample == 1.0);
    assert!(!silent, "the tone is not the muted -1 LSB DC level");
}

#[test]
fn tone_sample_checksum_is_pinned() {
    let mut apu = Apu::new(Host::Snes);
    arm_the_tone(&mut apu);
    let samples = run_frames(&mut apu, 8);
    assert!(
        samples.len() >= 4096,
        "eight frames give more than 4096 samples, saw {}",
        samples.len()
    );
    let mut hash = 0xcbf2_9ce4_8422_2325u64; // FNV-1a's offset basis
    for sample in &samples[..4096] {
        hash = fnv1a64_continue(hash, &sample.to_bits().to_le_bytes());
    }
    assert_eq!(
        hash, 16_906_835_467_938_112_357,
        "the pinned FNV-1a over the first 4096 samples of the test driver's tone \
         (pinned and recorded in the commit that made it pass); it moved \
         when the BRR decoder began running once per nibble, which changed the sample values"
    );
}

#[test]
fn channel_mask_silences_one_voice() {
    let mut plain = Apu::new(Host::Snes);
    arm_the_tone(&mut plain);
    let audible = run_frames(&mut plain, 2);

    let mut masked = Apu::new(Host::Snes);
    arm_the_tone(&mut masked);
    masked.set_channel_mask(0x01); // voice 0 silenced
    let silent = run_frames(&mut masked, 2);

    assert_eq!(audible.len(), silent.len(), "the same number of samples");
    assert!(
        audible.iter().any(|sample| sample.abs() > 0.01),
        "the unmasked voice is audible"
    );
    // With the only voice masked the mixer sums to zero, and the post-amp's XOR $FFFF makes that
    // -1 LSB, which is -1/32768.
    let dc = -1.0f32 / 32768.0;
    assert!(
        silent.iter().all(|sample| *sample == dc),
        "a masked voice leaves the mixer at -1 LSB"
    );
}

#[test]
fn state_round_trips() {
    // §9.2's APU, Spc700 and Dsp rows in table order: RAM, the two latch pairs, the IPL byte, the
    // two phase counters, the rate and the mask, the cycle count, then each device. The Dsp row is
    // 479 bytes: the echo unit carries one 8-sample FIR history per channel (16 more
    // bytes than §9.2's 33-byte echo entry), which is the §13 amendment the report asks for.
    let mut apu = Apu::new(Host::Snes);
    arm_the_tone(&mut apu);
    let next = transfer(&mut apu, 0xCC, 0x0200, &[0x5A; 32]);
    entry(&mut apu, next, 0x0200);
    apu.tick(MASTER_TICKS_PER_FRAME);

    let mut w = StateWriter::new();
    apu.save_state(&mut w);
    let saved = w.finish();
    assert_eq!(
        saved.len(),
        65_536 + 8 + 1 + 1 + 4 + 4 + 4 + 1 + 8 + 20 + 479,
        "the rows of §9.2: 65567 for Apu, 20 for Spc700, 479 for Dsp"
    );

    let mut loaded = Apu::new(Host::Snes);
    let mut r = StateReader::new(&saved);
    loaded.load_state(&mut r).expect("the state loads");
    assert_eq!(r.remaining(), 0, "the state is consumed exactly");

    let mut w2 = StateWriter::new();
    loaded.save_state(&mut w2);
    assert_eq!(
        w2.finish(),
        saved,
        "save, load and save again is byte-identical"
    );
}

#[test]
fn a_cartridge_uploads_two_bytes_and_the_spc_writes_back() {
    // The handshake end to end, the way a cartridge drives it: a synthesized S-CPU program kicks the
    // IPL with 16-bit stores, uploads a two-byte block, enters it, and reads back the byte the SPC's
    // own program writes to `$F4`. This is the shape that was broken: `STA $2140` writes `$2140`
    // before `$2141`, so a handshake that reacts to the `$2140` write stores the *previous* byte —
    // and the mode read at the kick is the previous command's, which entered the program before a
    // single byte of it had arrived.
    let program: Vec<u8> = vec![
        0x18, // clc
        0xFB, // xce             native mode, as a cartridge's boot does
        0xC2, 0x30, // rep #$30       16-bit A and X
        0xA9, 0xAA, 0xBB, // lda #$BBAA      the greeting, as a word
        0xCD, 0x40, 0x21, // w0: cmp $2140
        0xD0, 0xFB, //     bne w0
        0xA9, 0x00, 0x02, // lda #$0200      destination
        0x8D, 0x42, 0x21, // sta $2142       $2142 = $00, $2143 = $02
        0xA9, 0xCC, 0x01, // lda #$01CC      kick $CC, mode 01
        0x8D, 0x40, 0x21, // sta $2140       $2140 = $CC, $2141 = $01
        0xE2, 0x20, // sep #$20
        0xA9, 0xCC, // lda #$CC
        0xCD, 0x40, 0x21, // w1: cmp $2140
        0xD0, 0xFB, //     bne w1
        0xC2, 0x20, // rep #$20
        0xA9, 0x00, 0xC4, // lda #$C400      byte $C4 at index 0
        0x8D, 0x40, 0x21, // sta $2140
        0xE2, 0x20, // sep #$20
        0xA9, 0x00, // lda #$00
        0xCD, 0x40, 0x21, // w2: cmp $2140
        0xD0, 0xFB, //     bne w2
        0xC2, 0x20, // rep #$20
        0xA9, 0x01, 0xF4, // lda #$F401      byte $F4 at index 1
        0x8D, 0x40, 0x21, // sta $2140
        0xE2, 0x20, // sep #$20
        0xA9, 0x01, // lda #$01
        0xCD, 0x40, 0x21, // w3: cmp $2140
        0xD0, 0xFB, //     bne w3
        0xC2, 0x20, // rep #$20
        0xA9, 0x00, 0x02, // lda #$0200      entry point
        0x8D, 0x42, 0x21, // sta $2142
        0xA9, 0x03, 0x00, // lda #$0003      kick $03 = (2 + 1) | 1, mode 00
        0x8D, 0x40, 0x21, // sta $2140
        0xE2, 0x20, // sep #$20
        0xA9, 0x03, // lda #$03
        0xCD, 0x40, 0x21, // w4: cmp $2140
        0xD0, 0xFB, //     bne w4
        0xA9, 0x00, // lda #$00
        0xCD, 0x40, 0x21, // w5: cmp $2140      the SPC's own `MOV $F4,A`, A = 0
        0xD0, 0xFB, //     bne w5
        0xA9, 0x5A, // lda #$5A
        0x8D, 0x10, 0x00, // sta $0010         reached
    ];
    let rom = common::synth_lorom(0x8000, &program);
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);
    for _ in 0..30 {
        machine.run_frame();
    }

    let apu = &machine.bus.apu;
    assert_eq!(
        apu.ram[0x0200], 0xC4,
        "the block's first byte lands at the destination, not the command byte"
    );
    assert_eq!(apu.ram[0x0201], 0xF4, "and its second byte follows it");
    assert_eq!(
        apu.to_apu,
        [0x03, 0x00, 0x00, 0x02],
        "the last command was the entry: kick $03, mode $00, entry point $0200"
    );
    assert_eq!(
        machine.bus.wram[0x10], 0x5A,
        "the S-CPU read `$00` back at `$2140` — the uploaded `MOV $F4,A` — and recorded that it had"
    );
    assert_eq!(
        machine.cpu.a, 0x005A,
        "and the S-CPU finished the upload, which it cannot do without every echo"
    );
}

#[test]
fn jump_to_rom_ffc0_restarts_the_handshake() {
    // "Many games are jumping to ROM:FFC0h in order to 'reset' the SPC700 back to the transmission
    // phase."
    let mut apu = Apu::new(Host::Snes);
    let next = transfer(&mut apu, 0xCC, 0x0200, &[0x00; 16]);
    entry(&mut apu, next, 0x0200);
    assert_eq!(apu.ipl, Ipl::Running);

    apu.cpu.pc = 0xFFC0;
    settle(&mut apu); // enough clocks for the SPC to run one pass of the boot ROM
    assert_eq!(
        apu.ipl,
        Ipl::AwaitKick,
        "a jump to ROM:FFC0 re-arms the handshake"
    );
    assert_eq!(apu.read_port(0), 0xAA, "and the greeting is sent again");
    assert_eq!(apu.read_port(1), 0xBB);
    assert_eq!(apu.cpu.pc, 0xFFC0, "the SPC waits inside the boot ROM");

    // And the handshake runs again from there.
    let next = transfer(&mut apu, 0xCC, 0x0300, &[0x77; 8]);
    entry(&mut apu, next, 0x0300);
    assert_eq!(apu.ipl, Ipl::Running, "the second handshake completes");
    assert_eq!(apu.cpu.pc, 0x0300, "and jumps to the new entry point");
    for (i, byte) in [0x77u8; 8].iter().enumerate() {
        assert_eq!(apu.ram[0x0300 + i], *byte, "second upload, byte {i}");
    }
}
