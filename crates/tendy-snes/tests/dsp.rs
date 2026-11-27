//! The DSP: BRR, pitch, interpolation, envelopes, noise and echo.
//!
//! Every expected number is fullsnes's, from the sections named beside it; the failure messages say
//! which formula the number came from.

use tendy_snes::APU_RAM_SIZE;
use tendy_snes::apu::Dsp;
use tendy_snes::apu::echo::Echo;
use tendy_snes::apu::voice::EnvState;

/// A DSP, its APU RAM, and the register writes every test needs.
struct Rig {
    dsp: Dsp,
    ram: Box<[u8; APU_RAM_SIZE]>,
}

impl Rig {
    /// Power-on DSP with `FLG` unmuted. Voice 0 is set up but not keyed on.
    fn new() -> Rig {
        let mut rig = Rig {
            dsp: Dsp::new(),
            ram: Box::new([0; APU_RAM_SIZE]),
        };
        // DIR = $0200; instrument 0 points at $0300 for both start and loop.
        rig.ram[0x0200] = 0x00;
        rig.ram[0x0201] = 0x03;
        rig.ram[0x0202] = 0x00;
        rig.ram[0x0203] = 0x03;
        rig.dsp.write(0x6C, 0x00); // FLG: unmuted, echo writes on, noise stopped
        rig.dsp.write(0x5D, 0x02); // DIR
        rig.dsp.write(0x0C, 0x7F); // MVOLL
        rig.dsp.write(0x1C, 0x7F); // MVOLR
        rig.dsp.write(0x00, 0x7F); // V0VOLL
        rig.dsp.write(0x01, 0x7F); // V0VOLR
        rig.dsp.write(0x04, 0x00); // V0SRCN
        // Direct GAIN at $7F: envelope level = $7F * 16, constant (no ADSR movement).
        rig.dsp.write(0x05, 0x00);
        rig.dsp.write(0x07, 0x7F);
        rig.set_pitch(0x1000); // $1000 = 32000 Hz, one nibble a sample
        rig
    }

    fn set_pitch(&mut self, pitch: u16) {
        self.dsp.write(0x02, pitch as u8);
        self.dsp.write(0x03, (pitch >> 8) as u8);
    }

    /// Writes one 9-byte BRR block: the header, then sixteen nibbles.
    fn block(&mut self, at: usize, header: u8, nibbles: [u8; 16]) {
        self.ram[at] = header;
        for i in 0..8 {
            self.ram[at + 1 + i] = (nibbles[2 * i] << 4) | (nibbles[2 * i + 1] & 0x0F);
        }
    }

    /// A BRR block at $0300 with `shift`/`filter`/`code` in its header.
    fn tone(&mut self, filter: u8, shift: u8, code: u8, nibble: u8) {
        let header = (shift << 4) | (filter << 2) | code;
        self.block(0x0300, header, [nibble; 16]);
    }

    /// A BRR ramp at $0300: nibbles 0..15 in order, so the decoded samples are
    /// `0, $800, $1000, ... $3800, -$4000, ... -$800` at shift 12.
    fn ramp(&mut self, filter: u8, shift: u8, code: u8) {
        let header = (shift << 4) | (filter << 2) | code;
        let mut nibbles = [0u8; 16];
        for (k, nibble) in nibbles.iter_mut().enumerate() {
            *nibble = k as u8;
        }
        self.block(0x0300, header, nibbles);
    }

    fn key_on(&mut self, voice: u8) {
        self.dsp.write(0x4C, 1 << voice);
    }

    fn sample(&mut self) -> (i16, i16) {
        self.dsp.sample(&mut self.ram)
    }

    /// Voice 0's OUTX, the sample after the envelope and before `VxVOLL/R`.
    fn sample_out(&self) -> i16 {
        self.dsp.voices[0].out
    }

    fn run(&mut self, samples: usize) {
        for _ in 0..samples {
            self.sample();
        }
    }
}

/// fullsnes's four exact BRR filter formulas with the 16-bit clamp and the 15-bit clip, written out
/// so the test's expected value comes from the source and not from the implementation.
fn filtered(filter: u8, sample: i32, old: i32, older: i32) -> i16 {
    let new = match filter {
        0 => sample,
        1 => sample + old + ((-old) >> 4),
        2 => sample + old * 2 + ((-old * 3) >> 5) - older + (older >> 4),
        _ => sample + old * 2 + ((-old * 13) >> 6) - older + ((older * 3) >> 4),
    };
    let mut new = new.clamp(-0x8000, 0x7FFF);
    if new > 0x3FFF {
        new -= 0x8000;
    } else if new < -0x4000 {
        new += 0x8000;
    }
    new as i16
}

#[test]
fn brr_filters_zero_to_three() {
    // Shift 12, nibble 7: sample = (7 SHL 12) SAR 1 = $3800. The four decodes at key-on fill the
    // Gaussian history (Anomie §BRR DECODING: "BRR data is always decoded in a group of 4 samples"),
    // so the seventh output sample is the fifth sequential decode.
    for filter in 0..4u8 {
        let mut rig = Rig::new();
        rig.tone(filter, 12, 0, 7);
        rig.key_on(0);
        let mut got = Vec::new();
        for _ in 0..13 {
            rig.sample();
            got.push(rig.dsp.voices[0].old);
        }
        let mut old = 0i32;
        let mut older = 0i32;
        let mut expected = Vec::new();
        for _ in 0..11 {
            let new = filtered(filter, 0x3800, old, older);
            older = old;
            old = i32::from(new);
            expected.push(new);
        }
        assert_eq!(
            &got[6..13],
            &expected[4..],
            "filter {filter}: fullsnes \"Bit Rate Reduction (BRR) Format\" gives \
             new = sample + old*1+((-old*1) SAR 4) and its two successors; the first four decodes \
             happen at key-on, so the seven output samples are the fifth to eleventh"
        );
    }

    // The literal key-on decodes, so the numbers are pinned rather than only self-consistent.
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.run(2); // the KON poll
    assert_eq!(
        rig.dsp.voices[0].nibble, 4,
        "key-on decodes the first group of four BRR samples"
    );
    assert_eq!(rig.dsp.voices[0].old, 0x3800, "filter 0 passes the sample");
    rig.run(5);
    assert_eq!(rig.dsp.voices[0].old, 0x3800, "filter 0 does not recurse");

    let mut rig = Rig::new();
    rig.tone(1, 12, 0, 7);
    rig.key_on(0);
    rig.run(2);
    // Four sequential decodes from old=older=0: $3800, then $3800+$3800+($3800 SAR 4) = $6C80
    // clipped to $6C80 - $8000 = -$1380 (the 15-bit lost-sign rule), then $9656, then -$24A4.
    assert_eq!(
        rig.dsp.voices[0].old, -0x24A4,
        "filter 1's fourth decode: new = sample + old*1 + ((-old*1) SAR 4)"
    );
}

#[test]
fn brr_shift_and_end_codes() {
    // "When shift=13..15, decoding works as if shift=12 and nibble=(nibble SAR 3)."
    for shift in 13..=15u8 {
        let mut rig = Rig::new();
        rig.tone(0, shift, 0, 7);
        rig.key_on(0);
        rig.run(7);
        assert_eq!(
            rig.dsp.voices[0].old, 0,
            "shift {shift} decodes as shift 12 with nibble 7 SAR 3 = 0"
        );
    }
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.run(7);
    assert_eq!(
        rig.dsp.voices[0].old, 0x3800,
        "shift 12 keeps the nibble: (7 SHL 12) SAR 1"
    );

    // "Code 1 = End+Mute (jump to Loop-address, set ENDx flag, Release, Env=000h)".
    let mut rig = Rig::new();
    rig.tone(0, 12, 1, 7);
    rig.set_pitch(0x3FFF); // the counter carries after five real samples
    rig.key_on(0);
    rig.run(20);
    assert_eq!(
        rig.dsp.voices[0].state,
        EnvState::Release,
        "an end-code 1 block releases the voice"
    );
    assert_eq!(rig.dsp.voices[0].env, 0, "and zeroes the envelope");
    assert_eq!(
        rig.dsp.voices[0].brr, 0x0300,
        "and jumps to the loop address in the DIR table"
    );
    assert_eq!(rig.dsp.endx, 0x01, "ENDX records the end code");

    // "Code 3 = End+Loop (jump to Loop-address, set ENDx flag)" — no release.
    let mut rig = Rig::new();
    rig.tone(0, 12, 3, 7);
    rig.set_pitch(0x3FFF);
    rig.key_on(0);
    rig.run(20);
    assert_ne!(
        rig.dsp.voices[0].state,
        EnvState::Release,
        "an end-code 3 block loops without releasing"
    );
    assert_eq!(rig.dsp.voices[0].brr, 0x0300, "and loops");
    assert_eq!(rig.dsp.endx, 0x01, "ENDX records the end code");

    // Codes 0 and 2 continue at the next block: $0300 + 9, again and again.
    for code in [0u8, 2] {
        let mut rig = Rig::new();
        rig.tone(0, 12, code, 7);
        rig.set_pitch(0x3FFF);
        rig.key_on(0);
        rig.run(20);
        let advanced = rig.dsp.voices[0].brr;
        assert!(
            advanced > 0x0300,
            "code {code} continues past the first block, at ${advanced:04X}"
        );
        assert_eq!(
            (advanced - 0x0300) % 9,
            0,
            "each advance is one 9-byte BRR block, not {advanced} bytes"
        );
    }

    // A write to ENDX clears every bit ("Any write to this register will clear ALL bits").
    rig.dsp.endx = 0xFF;
    rig.dsp.write(0x7C, 0x00);
    assert_eq!(rig.dsp.endx, 0, "writing ENDX acknowledges every voice");
}

#[test]
fn brr_decodes_once_per_nibble_at_every_pitch() {
    // fullsnes "BRR Pitch": "Counter.Bit15-12 indicates the current sample (within a BRR block)",
    // and the SNESdev `S-DSP_registers` page gives the BRR sample rate as
    // "VxPITCH * 32000 Hz / $1000". A nibble is therefore decoded exactly when the counter's low
    // 12 bits carry into the nibble index: none at pitch 0, one per two output samples at $0800,
    // one per sample at $1000, two at $2000 and three at $3000.
    for filter in [0u8, 1] {
        for pitch in [0x0000u16, 0x0800, 0x1000, 0x2000, 0x3000] {
            let mut rig = Rig::new();
            rig.ramp(filter, 12, 3); // End+Loop, so the ramp repeats
            rig.set_pitch(pitch);
            rig.key_on(0);
            // The KON poll decodes the first group of four (Anomie §BRR DECODING).
            rig.sample();
            rig.sample();
            rig.run(4); // the five empty samples
            assert_eq!(
                rig.dsp.voices[0].nibble, 4,
                "pitch {pitch:#06X}, filter {filter}: key-on decodes the first four samples"
            );
            assert_eq!(
                rig.dsp.voices[0].counter, 0,
                "the empty samples do not step"
            );

            // The decoded sequence from fullsnes's exact filters; the history carries across the
            // loop ("this does carry over between groups and blocks", Anomie §BRR DECODING).
            let mut sequence = Vec::new();
            let (mut old, mut older) = (0i32, 0i32);
            for k in 0..64 {
                // Nibble k as a signed 4-bit value: 0..7 positive, 8..15 = -8..-1.
                let nibble = (((k % 16) as i8) << 4) >> 4;
                let sample = i32::from(nibble) * 0x800; // (nibble SHL 12) SAR 1
                let new = filtered(filter, sample, old, older);
                older = old;
                old = i32::from(new);
                sequence.push(new);
            }

            let mut decoded = 4usize;
            for output in 0..8 {
                let before = rig.dsp.voices[0].counter;
                rig.sample();
                let expected = ((u32::from(before) & 0x0FFF) + u32::from(pitch)) >> 12;
                decoded += expected as usize;
                assert_eq!(
                    rig.dsp.voices[0].nibble,
                    (decoded % 16) as u8,
                    "filter {filter}, pitch {pitch:#06X}, output {output}: the decoder advances \
                     one nibble per $1000 the counter carries"
                );
                assert_eq!(
                    rig.dsp.voices[0].old,
                    sequence[decoded - 1],
                    "filter {filter}, pitch {pitch:#06X}, output {output}: the filter ran once per \
                     decoded nibble, not once per output sample"
                );
                assert_eq!(
                    rig.dsp.voices[0].buf,
                    [
                        sequence[decoded - 1],
                        sequence[decoded - 2],
                        sequence[decoded - 3],
                        sequence[decoded - 4]
                    ],
                    "filter {filter}, pitch {pitch:#06X}, output {output}: the Gaussian taps are \
                     the four most recently decoded samples"
                );
            }
        }
    }
}

#[test]
fn pitch_counter_and_gaussian() {
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    // The KON poll, five empty samples, then one nibble a sample at pitch $1000.
    rig.run(11);
    assert_eq!(
        rig.dsp.voices[0].counter, 0x5000,
        "the counter advances by VxPITCH a sample: 5 decodes * $1000"
    );
    rig.run(5);
    assert_eq!(
        rig.dsp.voices[0].counter, 0xA000,
        "the counter keeps the fractional part as a 16-bit sum"
    );

    // Four equal samples of $3800 interpolate to $3800 within the table's $7FF..$801 quirk.
    assert_eq!(
        rig.dsp.voices[0].buf, [0x3800; 4],
        "the four-sample history is the decoded samples"
    );
    // OUTX carries the sample after the envelope, so it is interp * $7F0 SAR 11.
    let interp = 0x3800i32;
    let env = 0x7F0i32;
    let expected = ((interp * env) >> 11) as i16;
    let seen = rig.dsp.voices[0].out;
    assert!(
        (i32::from(seen) - i32::from(expected)).abs() <= 8,
        "the Gaussian sum of four equal samples is the sample itself (weights $7FF..$801), \
         so OUTX is {expected} +- 8, not {seen}"
    );
}

#[test]
fn pitch_modulation_scales_by_the_previous_voice() {
    // "Factor = VxOUTX(x-1); Factor = (Factor SAR 4)+400h; Step = (Step * Factor) SAR 10".
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x10, 0x7F); // voice 1's left volume
    rig.dsp.write(0x11, 0x7F);
    rig.dsp.write(0x14, 0x00); // voice 1's source
    rig.dsp.write(0x15, 0x00); // voice 1 in GAIN mode
    rig.dsp.write(0x17, 0x7F);
    rig.dsp.write(0x12, 0x00); // voice 1 pitch = $1000
    rig.dsp.write(0x13, 0x10);
    rig.dsp.write(0x2D, 0x02); // PMON bit 1: modulate voice 1 by voice 0
    rig.dsp.write(0x4C, 0x03); // key on both
    rig.run(12); // five empty samples each, then steady state
    let previous = i32::from(rig.dsp.voices[0].out);
    let factor = (previous >> 4) + 0x400;
    let before = u32::from(rig.dsp.voices[1].counter);
    rig.sample();
    let after = u32::from(rig.dsp.voices[1].counter);
    let step = after.wrapping_sub(before) & 0xFFFF;
    let expected = (((0x1000 * factor) >> 10) as u32) & 0xFFFF;
    assert_eq!(
        step, expected,
        "voice 0's OUTX ({previous}) gives factor {factor}, so the step is {expected}, not {step}"
    );
    assert_ne!(step, 0x1000, "the modulation actually changed the step");
}

#[test]
fn kon_has_five_empty_samples() {
    // "there are 5 'empty' samples before envelope updates and BRR decoding actually begin" — the
    // five samples output nothing, but the decoder preloads the first group of four BRR samples
    // during them (Anomie's key-on sequence: "At step S4, first BRR group is decoded").
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    let first = rig.sample();
    assert_eq!(first, (-1, -1), "nothing has been keyed on yet");
    // The key-on is polled at the second sample; the first group of four is decoded there.
    for n in 0..5 {
        rig.sample();
        assert_eq!(
            rig.dsp.voices[0].nibble,
            4,
            "empty sample {} of five after KON decodes nothing more: the four-sample group is \
             already in the buffer",
            n + 1
        );
        assert_eq!(
            rig.dsp.voices[0].env,
            0,
            "empty sample {} of five after KON does not step the envelope",
            n + 1
        );
        assert_eq!(
            rig.sample_out(),
            0,
            "empty sample {} of five after KON outputs nothing",
            n + 1
        );
    }
    rig.sample();
    assert_eq!(
        rig.dsp.voices[0].nibble, 5,
        "the sixth sample after KON decodes the fifth BRR nibble"
    );
    assert_ne!(rig.dsp.voices[0].env, 0, "and the envelope starts moving");
    assert_eq!(
        rig.dsp.voices[0].kon_delay, 0,
        "the five-sample delay is spent"
    );
    // The interpolation history was filled at key-on, so the voice is audible at once.
    rig.run(4);
    assert_ne!(rig.sample(), (-1, -1), "the voice is audible");
}

#[test]
fn kon_is_polled_at_16k() {
    // "These registers seem to be polled only at 16000 Hz, when every other sample is due to be
    // output."
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.sample();
    assert_eq!(
        rig.dsp.voices[0].kon_delay, 0,
        "an odd sample does not poll KON, so nothing was keyed on"
    );
    rig.sample();
    assert_eq!(
        rig.dsp.voices[0].kon_delay, 4,
        "the even sample polled KON: five empty samples, one already spent"
    );

    // KOFF is latched the same way and keeps acting until it is rewritten.
    rig.dsp.write(0x5C, 0x01);
    rig.run(2);
    assert_eq!(
        rig.dsp.voices[0].state,
        EnvState::Release,
        "KOFF at the next poll releases the voice"
    );
}

#[test]
fn adsr_attack_decay_sustain_release() {
    // Attack rate $F -> 31 -> "Step=+1024 when Rate=31"; decay rate 7 -> 30 (2 units a step);
    // sustain rate 31 and level 0 -> boundary $100.
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x05, 0xFF);
    rig.dsp.write(0x06, 0x1F);
    rig.key_on(0);
    rig.run(7);
    assert_eq!(
        rig.dsp.voices[0].env, 0x400,
        "the first attack step is +1024"
    );
    rig.run(1);
    assert_eq!(
        rig.dsp.voices[0].env, 0x7FF,
        "attack clips to $7FF at or above $800"
    );
    assert_eq!(
        rig.dsp.voices[0].state,
        EnvState::Decay,
        "attack hands over to decay at $7E0"
    );

    // Decay drops by -(((Level-1) SAR 8)+1) until the boundary.
    let decay_step = -(((0x7FF - 1) >> 8) + 1);
    let before = rig.dsp.voices[0].env;
    rig.run(2);
    assert_eq!(
        i32::from(rig.dsp.voices[0].env) - i32::from(before),
        decay_step,
        "decay steps -(((Level-1) SAR 8)+1) = {decay_step}"
    );
    rig.run(1000);
    assert_eq!(
        rig.dsp.voices[0].state,
        EnvState::Sustain,
        "decay hands over to sustain at the level boundary"
    );
    assert!(
        rig.dsp.voices[0].env <= 0x100,
        "the sustain boundary is (N+1)*$100 = $100, saw {}",
        rig.dsp.voices[0].env
    );

    // Release: "Rate=31, Step=-8".
    rig.dsp.write(0x5C, 0x01);
    rig.run(2);
    assert_eq!(rig.dsp.voices[0].state, EnvState::Release);
    let before = rig.dsp.voices[0].env;
    rig.run(1);
    assert_eq!(
        rig.dsp.voices[0].env,
        before.saturating_sub(8),
        "release steps -8 a sample"
    );
    rig.run(80);
    assert_eq!(rig.dsp.voices[0].env, 0, "release bottoms out at zero");
}

#[test]
fn gain_modes() {
    // "Direct Gain: 0-6 Fixed Volume (Envelope Level = N*16, Rate=Infinite)".
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x05, 0x00);
    rig.dsp.write(0x07, 0x40);
    rig.key_on(0);
    rig.run(10);
    assert_eq!(rig.dsp.voices[0].env, 0x400, "direct GAIN $40 is $40 * 16");

    // Mode 0, linear decrease: "Rate=N, Step=-32".
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x05, 0x00);
    rig.dsp.write(0x07, 0x80 | 0x1F);
    rig.key_on(0);
    rig.run(6);
    rig.dsp.voices[0].env = 0x400;
    rig.sample();
    assert_eq!(
        rig.dsp.voices[0].env, 0x3E0,
        "linear decrease steps -32: $400 - $20"
    );
    rig.sample();
    assert_eq!(rig.dsp.voices[0].env, 0x3C0, "and -32 again");

    // Mode 3, bent increase: +32 below $600, then +8.
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x05, 0x00);
    rig.dsp.write(0x07, 0xE0 | 0x1F);
    rig.key_on(0);
    rig.run(8);
    let start = rig.dsp.voices[0].env;
    rig.sample();
    assert_eq!(
        rig.dsp.voices[0].env,
        start + 32,
        "bent increase takes +32 below $600"
    );

    // The envelope clips rather than wrapping ("clip E to 0 or 0x7ff").
    rig.dsp.voices[0].env = 0x7F0;
    rig.run(20);
    assert_eq!(rig.dsp.voices[0].env, 0x7FF, "the envelope clips at $7FF");
}

#[test]
fn noise_lfsr_shared_by_voices() {
    // "there is only one noise-generator (and only one noise frequency) shared for all voices",
    // and one update is
    //   Level = ((Level SHR 1) AND 3FFFh) OR ((Level.Bit0 XOR Level.Bit1) SHL 14)
    let mut rig = Rig::new();
    rig.dsp.write(0x6C, 0x1F); // noise rate 31 = every sample; unmuted
    rig.dsp.write(0x3D, 0x03); // NON: voices 0 and 1
    rig.tone(0, 12, 0, 7);
    rig.dsp.write(0x10, 0x7F);
    rig.dsp.write(0x11, 0x7F);
    rig.dsp.write(0x14, 0x00);
    rig.dsp.write(0x15, 0x00);
    rig.dsp.write(0x17, 0x7F);
    rig.dsp.write(0x12, 0x00);
    rig.dsp.write(0x13, 0x10);
    rig.dsp.write(0x4C, 0x03);
    assert_eq!(rig.dsp.noise, 0x4000, "the generator starts at -4000h");

    // The same update, fifteen times, written out from fullsnes's formula.
    let mut level = 0x4000u16;
    for _ in 0..15 {
        level = ((level >> 1) & 0x3FFF) | (((level ^ (level >> 1)) & 1) << 14);
    }
    rig.run(15);
    assert_eq!(
        rig.dsp.noise, level,
        "fifteen updates of the LFSR from -4000h give ${level:04X}"
    );
    assert_eq!(
        rig.dsp.voices[0].out, rig.dsp.voices[1].out,
        "every NON voice reads the same generator"
    );

    let mut rig = Rig::new();
    rig.dsp.write(0x6C, 0x1F);
    rig.dsp.write(0x3D, 0x01);
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.run(7);
    assert_eq!(rig.dsp.noise, 0x0080, "seven updates leave $0080");
    rig.run(1);
    assert_eq!(rig.dsp.noise, 0x0040, "the next update halves it again");
}

#[test]
fn echo_fir_and_feedback() {
    let mut rig = Rig::new();
    rig.dsp.write(0x6D, 0x05); // ESA = $0500
    rig.dsp.write(0x7D, 0x00); // EDL = 0: one 4-byte entry
    rig.dsp.write(0x0D, 0x40); // EFB = $40
    for tap in 0..7u8 {
        rig.dsp.write(tap * 0x10 + 0x0F, 0x00);
    }
    rig.dsp.write(0x7F, 0x7F); // FIR7 = $7F
    // The entry: a left sample of $1000 in bytes 0-1.
    rig.ram[0x0500] = 0x00;
    rig.ram[0x0501] = 0x20;
    let (fir_l, fir_r) = rig
        .dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x40, 0x0100, 0x0200, true);
    // The entry's word $2000 is the 16-bit sample $2000, and "EchoRAM[addr] SAR 1" gives $1000;
    // sum = $1000 * $7F SAR 6 = $1FC0. The right word is 0, so its own FIR sum is 0.
    assert_eq!(
        (fir_l, fir_r),
        (0x1FC0, 0),
        "the FIR7 tap is EchoRAM SAR 1 * FIR7 SAR 6, per channel"
    );
    // echo_input = $0100 + ($1FC0 * $40 SAR 7) = $0100 + $FE0 = $10E0, bit 0 clear.
    let left = u16::from(rig.ram[0x0500]) | (u16::from(rig.ram[0x0501]) << 8);
    let right = u16::from(rig.ram[0x0502]) | (u16::from(rig.ram[0x0503]) << 8);
    assert_eq!(
        left & 0xFFFE,
        0x10E0,
        "echo_input = EchoVoices + (sum*EFB SAR 7), masked with bit 0 clear"
    );
    assert_eq!(
        right & 0xFFFE,
        0x0200,
        "the right channel keeps its own sum: no left-derived feedback"
    );

    // With echo writes disabled the entry is read and left alone.
    rig.dsp.echo.ram_index = 0;
    rig.dsp.echo.remain = 0;
    rig.ram[0x0500] = 0x00;
    rig.ram[0x0501] = 0x20;
    rig.dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x40, 0x0100, 0x0200, false);
    assert_eq!(
        rig.ram[0x0501], 0x20,
        "FLG bit 5 disables the echo write, not the echo read"
    );
}

#[test]
fn echo_reads_the_buffer_as_a_signed_sample() {
    // fullsnes "The FIR formula": "buf[(i-0) AND 7] = EchoRAM[addr] SAR 1", over the entry of
    // "6Dh - ESA": "Byte 0: Lower 7bit of Left sample (stored in bit1-7) (bit0=unused/zero);
    // Byte 1: Upper 8bit of Left sample". The stored word is therefore a *signed* 16-bit value and
    // $FFFE (-2) must reach the FIR as -1, not as +$7FFF.
    let mut rig = Rig::new();
    rig.dsp.write(0x7D, 0x00); // EDL 0: one 4-byte entry
    for tap in 0..7u8 {
        rig.dsp.write(tap * 0x10 + 0x0F, 0x00);
    }
    rig.dsp.write(0x7F, 0x7F); // FIR7 = $7F
    rig.ram[0x0500] = 0xFE;
    rig.ram[0x0501] = 0xFF; // $FFFE: -2 with bit 0 clear
    let (left, right) = rig
        .dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x00, 0, 0, false);
    // sum = -1 * $7F SAR 6 = -2. The old reading gave +$3FFF -> (+$3FFF * $7F) SAR 6 = +$FE00.
    assert_eq!(
        (left, right),
        (-2, 0),
        "the echo buffer is read as signed: EchoRAM SAR 1 of $FFFE is -1, and FIR7 = $7F gives -2"
    );
    assert_eq!(
        rig.dsp.echo.buf_l[0], -1,
        "the FIR history holds the 15-bit signed sample"
    );
}

#[test]
fn echo_filters_left_and_right_separately() {
    // fullsnes "The FIR formula": "Note that the left and right stereo channels are filtered
    // separately (no crosstalk), but with identical coefficients", over the entry's two words.
    let mut rig = Rig::new();
    rig.dsp.write(0x7D, 0x00);
    for tap in 0..7u8 {
        rig.dsp.write(tap * 0x10 + 0x0F, 0x00);
    }
    rig.dsp.write(0x7F, 0x7F);
    // Right channel only: the word $2000 in bytes 2-3, left zero.
    rig.ram[0x0502] = 0x00;
    rig.ram[0x0503] = 0x20;
    let (left, right) = rig
        .dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x00, 0, 0, false);
    assert_eq!(
        (left, right),
        (0, 0x1FC0),
        "a right-only entry produces a right-only FIR sum: $2000 SAR 1 = $1000, * $7F SAR 6"
    );
    assert_eq!(
        rig.dsp.echo.buf_l[0], 0,
        "the left history saw the left word"
    );
    assert_eq!(
        rig.dsp.echo.buf_r[0], 0x1000,
        "the right history saw the right word"
    );

    // And the mirror: a left-only entry returns its sum on the left, the right stays zero.
    let mut rig = Rig::new();
    rig.dsp.write(0x7D, 0x00);
    for tap in 0..7u8 {
        rig.dsp.write(tap * 0x10 + 0x0F, 0x00);
    }
    rig.dsp.write(0x7F, 0x7F);
    rig.ram[0x0500] = 0x00;
    rig.ram[0x0501] = 0x20;
    let (left, right) = rig
        .dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x00, 0, 0, false);
    assert_eq!(
        (left, right),
        (0x1FC0, 0),
        "a left-only entry produces a left-only FIR sum"
    );
}

#[test]
fn echo_is_mixed_into_its_own_channel() {
    // fullsnes "The FIR formula": "audio_output=NormalVoices+((sum*EVOLx) SAR 7)", with EVOLL
    // ("Left channel echo volume") on the left sum and EVOLR on the right.
    for (evoll, evolr, bytes) in [
        (0x40u8, 0x00u8, [0x00u8, 0x10, 0x00, 0x00]), // left word $1000
        (0x00u8, 0x40u8, [0x00u8, 0x00, 0x00, 0x10]), // right word $1000
    ] {
        let mut rig = Rig::new();
        rig.dsp.write(0x6C, 0x00); // unmuted, echo writes on
        rig.dsp.write(0x6D, 0x05); // ESA = $0500
        rig.dsp.write(0x7D, 0x00); // EDL = 0
        rig.dsp.write(0x0D, 0x00); // EFB = 0
        rig.dsp.write(0x0C, 0x00); // MVOLL
        rig.dsp.write(0x1C, 0x00); // MVOLR
        rig.dsp.write(0x2C, evoll);
        rig.dsp.write(0x3C, evolr);
        for tap in 0..7u8 {
            rig.dsp.write(tap * 0x10 + 0x0F, 0x00);
        }
        rig.dsp.write(0x7F, 0x7F); // FIR7 = $7F
        rig.ram[0x0500..0x0504].copy_from_slice(&bytes);
        let (left, right) = rig.sample();
        // sum = $1000 SAR 1 * $7F SAR 6 = $FE0; a $40 echo volume scales it to
        // ($FE0 * $40) SAR 7 = $7F0. The main mix is zero (no voices, MVOL 0), and the post-amp
        // inverts: !$7F0 = -$7F1 on that channel and -1 (silence) on the other.
        if evoll != 0 {
            assert_eq!(
                (left, right),
                (-0x7F1, -1),
                "EVOLL mixes the left FIR sum into the left channel only"
            );
        } else {
            assert_eq!(
                (left, right),
                (-1, -0x7F1),
                "EVOLR mixes the right FIR sum into the right channel only"
            );
        }
    }
}

#[test]
fn fir_saturates_on_the_last_tap() {
    // "if overflow occurred in LAST addition: saturate to min/max=-8000h/+7FFFh".
    let mut rig = Rig::new();
    rig.dsp.echo.buf_l = [0x3FFF; 8];
    rig.dsp.echo.buf_r = [0x3FFF; 8];
    rig.dsp.echo.fir = [0x7F; 8];
    // The entry the pass pushes is also $3FFF, so all eight taps are $3FFF * $7F SAR 6.
    rig.ram[0x0500] = 0xFE;
    rig.ram[0x0501] = 0x7F;
    let (sum_l, sum_r) = rig
        .dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x00, 0, 0, false);
    assert_eq!(
        (sum_l, sum_r),
        (0x7FFF, 0x7FFF),
        "eight taps of $3FFF * $7F SAR 6 overflow and saturate at +$7FFF, per channel"
    );
}

#[test]
fn echo_esa_change_lands_within_one_sample() {
    // "Changes to the ESA register are realized (almost) immediately: after changing ESA, the
    // hardware may keep access 1-2 samples at [OldBase+index]".
    let mut rig = Rig::new();
    rig.dsp.write(0x0D, 0x00);
    rig.ram[0x0500] = 0x00;
    rig.ram[0x0501] = 0x20;
    rig.dsp
        .echo
        .tick(&mut rig.ram, 0x05, 0x00, 0x00, 0x0800, 0x0800, true);
    let first = rig.ram[0x0501];
    assert_eq!(
        first, 0x08,
        "the first pass wrote $0800's high byte at $0500"
    );

    // The next pass, with ESA changed, writes at the new base on its very next sample.
    rig.dsp
        .echo
        .tick(&mut rig.ram, 0x06, 0x00, 0x00, 0x0800, 0x0800, true);
    let second = rig.ram[0x0601];
    assert_eq!(
        second, 0x08,
        "the new ESA takes effect on the next pass: $0800's high byte lands at $0600"
    );
    assert_eq!(
        rig.ram[0x0504], 0x00,
        "and the old base is left alone apart from the entry already written"
    );
}

#[test]
fn outx_is_post_envelope() {
    // "This returns the high byte of the current sample for this voice, after envelope volume
    // adjustment but before VxVOL[LR] is applied."
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.run(8);
    let out = rig.dsp.voices[0].out;
    assert_ne!(out, 0, "the voice is producing a sample");
    assert_eq!(
        rig.dsp.read(0x09),
        (out >> 8) as u8,
        "OUTX is the high byte of the post-envelope sample"
    );
    assert_eq!(
        rig.dsp.read(0x08),
        (rig.dsp.voices[0].env >> 4) as u8,
        "ENVX is the envelope's upper seven bits"
    );

    // The voice volume is applied after OUTX, so writing it does not move OUTX.
    rig.dsp.write(0x00, 0x01);
    assert_eq!(
        rig.dsp.read(0x09),
        (out >> 8) as u8,
        "VxVOLL is applied after OUTX, so OUTX is unchanged"
    );
}

#[test]
fn flg_mute_and_soft_reset() {
    let mut rig = Rig::new();
    rig.tone(0, 12, 0, 7);
    rig.key_on(0);
    rig.run(8);
    assert_ne!(rig.sample(), (-1, -1), "the voice is audible unmuted");

    // "Mute Amplifier (0=Normal, 1=Mute) (doesn't stop internal processing)".
    rig.dsp.write(0x6C, 0x40);
    assert_eq!(
        rig.sample(),
        (-1, -1),
        "mute zeroes the mixer, and the post-amp's XOR $FFFF inverts it"
    );
    let env = rig.dsp.voices[0].env;
    rig.run(4);
    assert_eq!(
        rig.dsp.voices[0].env, env,
        "the envelope keeps running while muted (GAIN direct)"
    );

    // "Soft Reset: 1=KeyOff all voices, and set Envelopes=0".
    rig.dsp.write(0x6C, 0x80);
    assert_eq!(
        rig.dsp.voices[0].state,
        EnvState::Release,
        "soft reset keys every voice off"
    );
    assert_eq!(rig.dsp.voices[0].env, 0, "and zeroes every envelope");
}

#[test]
fn echo_wrap_length_is_edl_entries() {
    // amendment 17: `EDL << 9` 4-byte entries, and `0` means one.
    assert_eq!(Echo::wrap_entries(0), 1, "EDL 0 is a single 4-byte entry");
    assert_eq!(Echo::wrap_entries(1), 512, "EDL 1 is 2 KiB = 512 entries");
    assert_eq!(Echo::wrap_entries(15), 15 * 512, "EDL 15 is 30 KiB");
}

/// A synthetic driver in code: no cartridge and no ROM bytes, one BRR block written into APU RAM by
/// this test, keyed on with `KON`, and the mixed stereo output read back sample by sample.
///
/// The instrument is chosen so every number below can be worked out from fullsnes by hand:
/// shift 12, filter 0, End+Loop, every nibble `+7` gives `sample = (7 SHL 12) SAR 1 = $3800`
/// ("Bit Rate Reduction (BRR) Format"), direct GAIN `$7F` holds the envelope at `$7F0`
/// ("ADSR/Gain Envelope": `Envelope Level = N*16`), `V0VOLL/R = $7F` and `MVOL = $7F`.
///
/// The first output is the seventh sample: one for the `KON` poll and five "empty" samples
/// ("KON/KOFF Notes"). With pitch `$1000` the counter steps one nibble a sample, so the Gaussian
/// index is `(counter SAR 4) AND FFh` and each new sample enters `buf[0]`.
#[test]
fn a_keyed_voice_produces_a_pinned_stereo_sequence() {
    let mut rig = Rig::new();
    rig.tone(0, 12, 3, 7);
    rig.key_on(0);
    let mut got = Vec::new();
    for _ in 0..16 {
        got.push(rig.sample());
    }
    assert!(
        got.iter().any(|(left, _)| *left != 0),
        "the sequence must contain real output, or this test proves nothing: {got:?}"
    );
    // Six `-1`s are the post-amp's XOR of a zero mix (the KON poll and five empty samples), then
    // the first real output. The four samples decoded at key-on are all `$3800`, so the first
    // output is already the steady state: at counter `$1000` the Gaussian index is
    // `(counter SAR 4) AND FFh = 0` and the weights fullsnes prints for that index are
    // `gauss[$FF]=$172` on oldest, `gauss[$1FF]=$519` on older, `gauss[$100]=$176` on old and
    // `gauss[$0]=$000` on new — they sum to `$801`, the documented table quirk. With every tap
    // `$3800` the interpolated value is `($172+$519+$176)*$3800 SAR 11 = $3800` within that quirk,
    // so `OUTX = ($3800 * $7F0) SAR 11`, the mixer gives `((OUTX*$7F SAR 6)*$7F SAR 7) XOR FFFFh`
    // and the pinned `-28017` is the same steady value the old history reached four samples later.
    let expected: [(i16, i16); 16] = [
        (-1, -1),
        (-1, -1),
        (-1, -1),
        (-1, -1),
        (-1, -1),
        (-1, -1),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
        (-28017, -28017),
    ];
    assert_eq!(&got[..], &expected[..], "the pinned sequence");
}
