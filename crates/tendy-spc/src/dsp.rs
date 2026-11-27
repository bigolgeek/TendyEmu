//! The DSP: the 128-register file, the 8 voices, the shared noise generator and echo.
//! Owned by the APU lane.
//!
//! Sources: fullsnes "SNES APU DSP BRR Samples", "SNES APU DSP BRR Pitch", "SNES APU DSP ADSR/Gain
//! Envelope", "SNES APU DSP Volume Registers", "SNES APU DSP Control Registers", "SNES APU DSP Echo
//! Registers" and "SNES APU Low Level Timings". Every number below is quoted from those sections in
//! the comment beside it.

use crate::echo::Echo;
use crate::state::{StateRead, StateWrite};
use crate::voice::{EnvState, Voice};
use crate::{APU_RAM_SIZE, Host};

/// The 4-point Gaussian interpolation table, 512 entries, as fullsnes prints it under "4-Point
/// Gaussian Interpolation". It is not the ideal curve: the four weights of one index sum to
/// `$7FF`..`$801`, and that documented quirk is kept.
#[rustfmt::skip]
const GAUSS: [i16; 512] = [
    0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,
    0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x002,0x002,0x002,0x002,0x002,
    0x002,0x002,0x003,0x003,0x003,0x003,0x003,0x004,0x004,0x004,0x004,0x004,0x005,0x005,0x005,0x005,
    0x006,0x006,0x006,0x006,0x007,0x007,0x007,0x008,0x008,0x008,0x009,0x009,0x009,0x00A,0x00A,0x00A,
    0x00B,0x00B,0x00B,0x00C,0x00C,0x00D,0x00D,0x00E,0x00E,0x00F,0x00F,0x00F,0x010,0x010,0x011,0x011,
    0x012,0x013,0x013,0x014,0x014,0x015,0x015,0x016,0x017,0x017,0x018,0x018,0x019,0x01A,0x01B,0x01B,
    0x01C,0x01D,0x01D,0x01E,0x01F,0x020,0x020,0x021,0x022,0x023,0x024,0x024,0x025,0x026,0x027,0x028,
    0x029,0x02A,0x02B,0x02C,0x02D,0x02E,0x02F,0x030,0x031,0x032,0x033,0x034,0x035,0x036,0x037,0x038,
    0x03A,0x03B,0x03C,0x03D,0x03E,0x040,0x041,0x042,0x043,0x045,0x046,0x047,0x049,0x04A,0x04C,0x04D,
    0x04E,0x050,0x051,0x053,0x054,0x056,0x057,0x059,0x05A,0x05C,0x05E,0x05F,0x061,0x063,0x064,0x066,
    0x068,0x06A,0x06B,0x06D,0x06F,0x071,0x073,0x075,0x076,0x078,0x07A,0x07C,0x07E,0x080,0x082,0x084,
    0x086,0x089,0x08B,0x08D,0x08F,0x091,0x093,0x096,0x098,0x09A,0x09C,0x09F,0x0A1,0x0A3,0x0A6,0x0A8,
    0x0AB,0x0AD,0x0AF,0x0B2,0x0B4,0x0B7,0x0BA,0x0BC,0x0BF,0x0C1,0x0C4,0x0C7,0x0C9,0x0CC,0x0CF,0x0D2,
    0x0D4,0x0D7,0x0DA,0x0DD,0x0E0,0x0E3,0x0E6,0x0E9,0x0EC,0x0EF,0x0F2,0x0F5,0x0F8,0x0FB,0x0FE,0x101,
    0x104,0x107,0x10B,0x10E,0x111,0x114,0x118,0x11B,0x11E,0x122,0x125,0x129,0x12C,0x130,0x133,0x137,
    0x13A,0x13E,0x141,0x145,0x148,0x14C,0x150,0x153,0x157,0x15B,0x15F,0x162,0x166,0x16A,0x16E,0x172,
    0x176,0x17A,0x17D,0x181,0x185,0x189,0x18D,0x191,0x195,0x19A,0x19E,0x1A2,0x1A6,0x1AA,0x1AE,0x1B2,
    0x1B7,0x1BB,0x1BF,0x1C3,0x1C8,0x1CC,0x1D0,0x1D5,0x1D9,0x1DD,0x1E2,0x1E6,0x1EB,0x1EF,0x1F3,0x1F8,
    0x1FC,0x201,0x205,0x20A,0x20F,0x213,0x218,0x21C,0x221,0x226,0x22A,0x22F,0x233,0x238,0x23D,0x241,
    0x246,0x24B,0x250,0x254,0x259,0x25E,0x263,0x267,0x26C,0x271,0x276,0x27B,0x280,0x284,0x289,0x28E,
    0x293,0x298,0x29D,0x2A2,0x2A6,0x2AB,0x2B0,0x2B5,0x2BA,0x2BF,0x2C4,0x2C9,0x2CE,0x2D3,0x2D8,0x2DC,
    0x2E1,0x2E6,0x2EB,0x2F0,0x2F5,0x2FA,0x2FF,0x304,0x309,0x30E,0x313,0x318,0x31D,0x322,0x326,0x32B,
    0x330,0x335,0x33A,0x33F,0x344,0x349,0x34E,0x353,0x357,0x35C,0x361,0x366,0x36B,0x370,0x374,0x379,
    0x37E,0x383,0x388,0x38C,0x391,0x396,0x39B,0x39F,0x3A4,0x3A9,0x3AD,0x3B2,0x3B7,0x3BB,0x3C0,0x3C5,
    0x3C9,0x3CE,0x3D2,0x3D7,0x3DC,0x3E0,0x3E5,0x3E9,0x3ED,0x3F2,0x3F6,0x3FB,0x3FF,0x403,0x408,0x40C,
    0x410,0x415,0x419,0x41D,0x421,0x425,0x42A,0x42E,0x432,0x436,0x43A,0x43E,0x442,0x446,0x44A,0x44E,
    0x452,0x455,0x459,0x45D,0x461,0x465,0x468,0x46C,0x470,0x473,0x477,0x47A,0x47E,0x481,0x485,0x488,
    0x48C,0x48F,0x492,0x496,0x499,0x49C,0x49F,0x4A2,0x4A6,0x4A9,0x4AC,0x4AF,0x4B2,0x4B5,0x4B7,0x4BA,
    0x4BD,0x4C0,0x4C3,0x4C5,0x4C8,0x4CB,0x4CD,0x4D0,0x4D2,0x4D5,0x4D7,0x4D9,0x4DC,0x4DE,0x4E0,0x4E3,
    0x4E5,0x4E7,0x4E9,0x4EB,0x4ED,0x4EF,0x4F1,0x4F3,0x4F5,0x4F6,0x4F8,0x4FA,0x4FB,0x4FD,0x4FF,0x500,
    0x502,0x503,0x504,0x506,0x507,0x508,0x50A,0x50B,0x50C,0x50D,0x50E,0x50F,0x510,0x511,0x511,0x512,
    0x513,0x514,0x514,0x515,0x516,0x516,0x517,0x517,0x517,0x518,0x518,0x518,0x518,0x518,0x519,0x519,
];

/// The 5-bit ADSR/GAIN/noise rate table in 32 kHz sample units ("ADSR/Gain (and Noise) Rates":
/// "00h=Stop 04h=1024 08h=384 ... 1Fh=1"; the values are "1,3,5 SHL n").
const RATE_UNITS: [u32; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32,
    24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1,
];

/// The S-DSP.
#[derive(Clone)]
pub struct Dsp {
    /// The 128-byte RAM-like register buffer, as written (`$80`-`$FF` mirror `$00`-`$7F`).
    pub regs: [u8; 0x80],
    pub voices: [Voice; 8],
    /// The one noise generator, 15 bits, shared by every voice that sets `NON`.
    pub noise: u16,
    pub echo: Echo,
    /// 32 kHz samples since reset.
    pub samples: u64,
    pub kon: u8,
    pub koff: u8,
    pub endx: u8,
    pub mask: u8,
}

impl Default for Dsp {
    fn default() -> Dsp {
        Dsp {
            regs: [0; 0x80],
            voices: std::array::from_fn(|_| Voice::default()),
            noise: 0,
            echo: Echo::default(),
            samples: 0,
            kon: 0,
            koff: 0,
            endx: 0,
            mask: 0,
        }
    }
}

impl Dsp {
    /// Power-on: every register zero except `FLG` (`$6C`) = `$E0`, keyed off, muted, echo writes
    /// off, noise stopped (fullsnes "The initial value on Reset is E0h"). The noise
    /// generator starts at `-4000h` ("initially -4000h after reset").
    pub fn new() -> Dsp {
        let mut dsp = Dsp::default();
        dsp.regs[0x6C] = 0xE0;
        dsp.noise = 0x4000;
        dsp
    }

    /// Computes `ENVX`/`OUTX`/`ENDX` and returns the register's value.
    ///
    /// `$80`-`$FF` mirror `$00`-`$7F`; `x8` is the envelope's upper 7 bits and `x9`
    /// the sample's high byte, both live rather than stored.
    pub fn read(&mut self, reg: u8) -> u8 {
        let r = reg & 0x7F;
        let voice = usize::from(r >> 4);
        if voice < 8 {
            match r & 0x0F {
                0x8 => return (self.voices[voice].env >> 4) as u8,
                0x9 => return (self.voices[voice].out >> 8) as u8,
                _ => {}
            }
        }
        match r {
            0x7C => self.endx,
            _ => self.regs[usize::from(r)],
        }
    }

    /// Writes a DSP register, including the write-to-acknowledge `ENDX` and `FLG`'s soft reset.
    pub fn write(&mut self, reg: u8, value: u8) {
        let r = reg & 0x7F;
        match r {
            // "Any write to this register will clear ALL bits, no matter what value is written."
            0x7C => {
                self.endx = 0;
                self.regs[usize::from(r)] = 0;
            }
            // FLG: soft reset keys off every voice and zeroes its envelope.
            0x6C => {
                self.regs[usize::from(r)] = value;
                if value & 0x80 != 0 {
                    for voice in &mut self.voices {
                        voice.state = EnvState::Release;
                        voice.env = 0;
                    }
                }
            }
            // KOFF: "KOFF and FLG.7 exert their influence constantly until a new value is written."
            0x5C => {
                self.regs[usize::from(r)] = value;
                self.koff = value;
            }
            // KON: the register keeps what was written, and the bit is the latch the 16 kHz poll
            // consumes ("Set the 'internal' value of KON to 0").
            0x4C => {
                self.regs[usize::from(r)] = value;
                self.kon |= value;
            }
            _ => {
                self.regs[usize::from(r)] = value;
                let voice = usize::from(r >> 4);
                if voice < 8 {
                    match r & 0x0F {
                        // x2/x3: the pitch, 14 bits.
                        0x2 => {
                            self.voices[voice].pitch =
                                (self.voices[voice].pitch & 0x3F00) | u16::from(value);
                        }
                        0x3 => {
                            self.voices[voice].pitch = (self.voices[voice].pitch & 0x00FF)
                                | ((u16::from(value) & 0x3F) << 8);
                        }
                        0x4 => self.voices[voice].srcn = value,
                        0x5 => self.voices[voice].adsr1 = value,
                        0x6 => self.voices[voice].adsr2 = value,
                        0x7 => self.voices[voice].gain = value,
                        // xF: the echo FIR tap for this voice.
                        0xF => self.echo.fir[voice] = value as i8,
                        _ => {}
                    }
                }
            }
        }
    }

    pub fn set_mask(&mut self, mask: u8) {
        self.mask = mask;
    }

    /// How many voices have a non-zero envelope: what a driver has keyed and not yet released.
    ///
    /// `KOFF` moves even an unkeyed voice to `Release` (fullsnes "KON/KOFF Notes" step 1), so the
    /// state alone would report all eight for any driver that writes `KOFF`; the envelope is the
    /// register (`ENVX`, `x8`) a listener would call "this voice is playing".
    pub fn keyed_voices(&self) -> u8 {
        self.voices.iter().filter(|voice| voice.env > 0).count() as u8
    }

    /// One 32 kHz sample for all eight voices plus echo. Returns the two channels.
    ///
    /// The order is: decode and envelope each voice, then echo. `KON`/`KOFF` are
    /// polled every other sample (16 kHz, fullsnes "KON/KOFF Notes"), the noise generator is shared
    /// and updated at the `FLG` rate, and the mixer applies fullsnes's "Output Mixer" chain.
    pub fn sample(&mut self, ram: &mut [u8; APU_RAM_SIZE]) -> (i16, i16) {
        self.samples += 1;
        // "These registers seem to be polled only at 16000 Hz, when every other sample is due to
        // be output."
        if self.samples & 1 == 0 {
            self.poll_keys(ram);
        }

        // The shared noise generator: "At the selected noise rate, the level is updated".
        let noise_rate = self.regs[0x6C] & 0x1F;
        if noise_rate != 0 {
            let units = u64::from(RATE_UNITS[usize::from(noise_rate)]);
            if self.samples.is_multiple_of(units) {
                let bit = (self.noise ^ (self.noise >> 1)) & 1;
                self.noise = ((self.noise >> 1) & 0x3FFF) | (bit << 14);
            }
        }

        let non = self.regs[0x3D];
        let pmon = self.regs[0x2D];
        let noise = self.noise;

        for v in 0..8 {
            self.run_voice(v, ram, non, pmon, noise);
        }

        let flg = self.regs[0x6C];
        let eon = self.regs[0x4D];
        let mut sum_l: i32 = 0;
        let mut sum_r: i32 = 0;
        let mut echo_l: i32 = 0;
        let mut echo_r: i32 = 0;
        for v in 0..8 {
            let sample = i32::from(self.voices[v].out);
            let voll = i32::from(self.regs[v * 0x10] as i8);
            let volr = i32::from(self.regs[v * 0x10 + 1] as i8);
            let term_l = (sample * voll) >> 6;
            let term_r = (sample * volr) >> 6;
            // "with 16bit overflow handling (after each addition)"
            if self.mask & (1 << v) == 0 {
                sum_l = saturate16(sum_l + term_l);
                sum_r = saturate16(sum_r + term_r);
            }
            if eon & (1 << v) != 0 {
                echo_l = saturate16(echo_l + term_l);
                echo_r = saturate16(echo_r + term_r);
            }
        }

        let mvol_l = i32::from(self.regs[0x0C] as i8);
        let mvol_r = i32::from(self.regs[0x1C] as i8);
        let mut left = (sum_l * mvol_l) >> 7;
        let mut right = (sum_r * mvol_r) >> 7;

        // The echo pass reads its entry, filters each channel and writes the new one back.
        let (fir_l, fir_r) = self.echo.tick(
            ram,
            self.regs[0x6D],
            self.regs[0x7D],
            self.regs[0x0D] as i8,
            echo_l as i16,
            echo_r as i16,
            flg & 0x20 == 0,
        );
        // "audio_output=NormalVoices+((sum*EVOLx) SAR 7)" (fullsnes "The FIR formula"): EVOLL
        // scales the left FIR sum into the left channel and EVOLR the right into the right.
        left += (fir_l * i32::from(self.regs[0x2C] as i8)) >> 7;
        right += (fir_r * i32::from(self.regs[0x3C] as i8)) >> 7;

        if flg & 0x40 != 0 {
            left = 0;
            right = 0;
        }
        // "sum = sum XOR FFFFh ;-final phase inversion (as done by built-in post-amp)".
        ((!left as u16) as i16, (!right as u16) as i16)
    }

    /// Applies `FLG`'s soft reset, `KOFF` and the pending `KON` set (fullsnes "KON/KOFF Notes",
    /// steps 1-3).
    fn poll_keys(&mut self, ram: &mut [u8; APU_RAM_SIZE]) {
        let soft = self.regs[0x6C] & 0x80 != 0;
        for v in 0..8 {
            if soft || self.koff & (1 << v) != 0 {
                self.voices[v].state = EnvState::Release;
                if soft {
                    self.voices[v].env = 0;
                }
            }
        }
        if self.kon != 0 {
            let kon = self.kon;
            for v in 0..8 {
                if kon & (1 << v) != 0 {
                    self.key_on(v, ram);
                }
            }
            self.kon = 0;
        }
    }

    /// "Writing 1 to the KON bit will set the envelope to 0, the state to Attack, and will start the
    /// channel from the beginning (see DIR and VxSRCN)", with five empty samples first.
    fn key_on(&mut self, v: usize, ram: &mut [u8; APU_RAM_SIZE]) {
        let voice = &mut self.voices[v];
        voice.state = EnvState::Attack;
        voice.env = 0;
        voice.counter = 0;
        voice.nibble = 0;
        voice.brr_header = 0;
        voice.old = 0;
        voice.older = 0;
        voice.buf = [0; 4];
        // UNVERIFIED: Anomie's key-on sequence says "The final pre-KON BRR decode also occurs here
        // (which can matter if the first block of the new BRR data uses a non-Direct filter)", so a
        // filter-1..3 first block should start from the previous decode's history, not from zero.
        // Settle with a filter-1 first block keyed on over a voice already decoding.
        voice.out = 0;
        // The first step of the new phase applies at the first processed sample; the hardware's
        // phase is arbitrary ("the location of the 1st step may vary"), so v1 pins it.
        voice.rate_acc = 0;
        // "there are 5 'empty' samples before envelope updates and BRR decoding actually begin".
        voice.kon_delay = 5;
        let dir = u16::from(self.regs[0x5D]) << 8;
        let entry = dir.wrapping_add(u16::from(voice.srcn) * 4);
        let start =
            u16::from(ram[usize::from(entry)]) | (u16::from(ram[usize::from(entry) + 1]) << 8);
        voice.brr = u32::from(start);
        // The four samples behind the first interpolation window are decoded here, not at the first
        // output sample. Anomie §BRR DECODING: "Each voice has a 12-sample ring buffer for decoding
        // BRR data, divided into 3 groups of 4 samples. BRR data is always decoded in a group of 4
        // samples", and the key-on sequence decodes three groups before the first sample is output.
        // The 4-sample Gaussian history needs the first four; the other eight only move an end code
        // earlier than the counter does.
        // UNVERIFIED: whether the remaining eight are observably needed (Anomie: "the 'e' and 'l'
        // bits of the final block can be seen before the final few samples ... are output"). Settle
        // with an end-code block at pitch $0400, comparing ENDX's frame against hardware.
        for _ in 0..4 {
            self.decode_next(v, ram);
        }
    }

    /// One voice's sample: BRR decode, Gaussian (or noise), envelope, `OUTX`.
    fn run_voice(&mut self, v: usize, ram: &mut [u8; APU_RAM_SIZE], non: u8, pmon: u8, noise: u16) {
        if self.voices[v].state == EnvState::Off {
            self.voices[v].out = 0;
            return;
        }
        if self.voices[v].kon_delay > 0 {
            // The five empty samples after KON: no BRR decode, no envelope step.
            self.voices[v].kon_delay -= 1;
            self.voices[v].out = 0;
            return;
        }

        // Pitch: "Step = VxPitch", and with PMON "Factor = (OUTX_prev SAR 4)+400h,
        // Step = (Step * Factor) SAR 10" (fullsnes "Pitch Counter").
        let mut step = u32::from(self.voices[v].pitch);
        if v > 0 && pmon & (1 << v) != 0 {
            let factor = (i32::from(self.voices[v - 1].out) >> 4) + 0x400;
            step = ((step as i32 * factor) >> 10).max(0) as u32;
        }
        let old_counter = self.voices[v].counter;
        let total = u32::from(old_counter) + step;
        self.voices[v].counter = total as u16;

        // The BRR decoder runs once per *nibble*, not once per output sample: the counter's bit 15
        // to 12 is "the current sample (within a BRR block)" and the sample rate is
        // "VxPITCH * 32000 Hz / $1000" (SNESdev `S-DSP_registers`). So exactly as many nibbles are
        // decoded as the counter's low 12 bits carry into the nibble index this sample — zero at
        // pitch 0, one at $1000, several above it — and the filter history and Gaussian taps only
        // move when a new BRR sample does.
        let decodes = ((u32::from(old_counter) & 0x0FFF) + step) >> 12;
        let interpolated = if non & (1 << v) != 0 {
            // "Gaussian interpolation isn't applied on noise", and PITCH has no effect on it.
            // UNVERIFIED: Anomie §BRR DECODING says "BRR decoding never stops for a voice", so this
            // port's skip while NON is set leaves the nibble cursor behind if a driver clears NON.
            // Settle by keying a NON voice, clearing NON after 100 samples, and comparing the first
            // decoded sample with hardware.
            sign15(noise)
        } else {
            for _ in 0..decodes {
                self.decode_next(v, ram);
            }
            self.gauss(&self.voices[v])
        };

        self.step_envelope(v);
        // OUTX: "after envelope volume adjustment but before VxVOL[LR] is applied".
        let scaled = (i32::from(interpolated) * i32::from(self.voices[v].env)) >> 11;
        self.voices[v].out = scaled as i16;
    }

    /// Advances to the next 9-byte block when the pitch counter carries, honouring the end codes.
    fn next_block(&mut self, v: usize, ram: &[u8; APU_RAM_SIZE]) {
        let code = self.voices[v].brr_header & 0x03;
        // "Code 1 = End+Mute (jump to Loop-address, set ENDx flag, Release, Env=000h)",
        // "Code 3 = End+Loop (jump to Loop-address, set ENDx flag)".
        if code == 1 || code == 3 {
            let dir = u16::from(self.regs[0x5D]) << 8;
            let entry = usize::from(dir.wrapping_add(u16::from(self.voices[v].srcn) * 4));
            let looped = u16::from(ram[entry + 2]) | (u16::from(ram[entry + 3]) << 8);
            self.voices[v].brr = u32::from(looped);
            if code == 1 {
                self.voices[v].state = EnvState::Release;
                self.voices[v].env = 0;
            }
        } else {
            // Codes 0 and 2 continue at the next block ("Code 2 = Ignored (same as Code 0)").
            let next = (self.voices[v].brr as u16).wrapping_add(9);
            self.voices[v].brr = u32::from(next);
        }
        self.voices[v].nibble = 0;
        self.voices[v].brr_header = 0;
    }

    /// Decodes the next nibble of the current block and pushes it through the filter and the
    /// Gaussian history: fullsnes "Bit Rate Reduction (BRR) Format" and its four exact filter
    /// formulas. One call per BRR sample the pitch counter consumes — never once per output sample.
    fn decode_next(&mut self, v: usize, ram: &[u8; APU_RAM_SIZE]) -> i16 {
        let block = usize::from(self.voices[v].brr as u16);
        if self.voices[v].nibble == 0 {
            let header = ram[block];
            self.voices[v].brr_header = header;
            // "the bit is set at the START of decoding the BRR block, not at the end."
            if header & 0x03 == 1 || header & 0x03 == 3 {
                self.endx |= 1 << v;
            }
        }
        let nibble_index = self.voices[v].nibble;
        let header = self.voices[v].brr_header;
        let data = ram[(block + 1 + usize::from(nibble_index >> 1)) & 0xFFFF];
        let raw = if nibble_index & 1 == 0 {
            data >> 4
        } else {
            data & 0x0F
        };
        let mut nibble = i32::from((raw as i8) << 4 >> 4); // sign-extended -8..+7
        let mut shift = u32::from(header >> 4);
        if shift > 12 {
            // "When shift=13..15, decoding works as if shift=12 and nibble=(nibble SAR 3)".
            shift = 12;
            nibble >>= 3;
        }
        // "sample = (nibble SHL shift) SAR 1".
        let sample = (nibble << shift) >> 1;
        let filter = (header >> 2) & 0x03;
        let old = i32::from(self.voices[v].old);
        let older = i32::from(self.voices[v].older);
        let mut new = match filter {
            0 => sample,
            1 => sample + old + ((-old) >> 4),
            2 => sample + old * 2 + ((-old * 3) >> 5) - older + (older >> 4),
            _ => sample + old * 2 + ((-old * 13) >> 6) - older + ((older * 3) >> 4),
        };
        // The 16-bit clamp, then the 15-bit clip with its lost-sign rule (fullsnes, same section).
        new = new.clamp(-0x8000, 0x7FFF);
        if new > 0x3FFF {
            new -= 0x8000;
        } else if new < -0x4000 {
            new += 0x8000;
        }
        let new = new as i16;
        self.voices[v].older = self.voices[v].old;
        self.voices[v].old = new;
        self.voices[v].push(new);
        self.voices[v].nibble += 1;
        if self.voices[v].nibble == 16 {
            self.next_block(v, ram);
        }
        new
    }

    /// The 4-point interpolation of fullsnes's "4-Point Gaussian Interpolation", with `i` taken from
    /// the counter's bits 4-11.
    fn gauss(&self, voice: &Voice) -> i16 {
        let i = usize::from(((voice.counter >> 4) & 0xFF) as u8);
        let mut out = (i32::from(GAUSS[0xFF - i]) * i32::from(voice.buf[3])) >> 10;
        out += (i32::from(GAUSS[0x1FF - i]) * i32::from(voice.buf[2])) >> 10;
        out += (i32::from(GAUSS[0x100 + i]) * i32::from(voice.buf[1])) >> 10;
        // "... with 16bit overflow handling" on the last addition only.
        out = saturate16(out + ((i32::from(GAUSS[i]) * i32::from(voice.buf[0])) >> 10));
        (out >> 1) as i16
    }

    /// One envelope step, ADSR or GAIN (fullsnes "SNES APU DSP ADSR/Gain Envelope").
    fn step_envelope(&mut self, v: usize) {
        let voice = &mut self.voices[v];
        if voice.state == EnvState::Off {
            return;
        }
        // GAIN mode overrides attack/decay/sustain but not release.
        if voice.adsr1 & 0x80 == 0 && voice.state != EnvState::Release {
            if voice.gain & 0x80 == 0 {
                // "Direct Gain: 0-6 Fixed Volume (Envelope Level = N*16, Rate=Infinite)".
                voice.env = u16::from(voice.gain & 0x7F) * 16;
                voice.rate_acc = 0;
                return;
            }
            let rate = voice.gain & 0x1F;
            if !rate_due(voice, rate) {
                return;
            }
            let level = i32::from(voice.env);
            let step = match (voice.gain >> 5) & 0x03 {
                0 => -32,                       // Linear Decrease
                1 => -(((level - 1) >> 8) + 1), // Exp Decrease
                2 => 32,                        // Linear Increase
                _ => {
                    if level < 0x600 {
                        32
                    } else {
                        8
                    }
                } // Bent Increase
            };
            voice.env = clip11(level + step);
            return;
        }

        match voice.state {
            EnvState::Attack => {
                let rate = attack_rate(voice.adsr1);
                if !rate_due(voice, rate) {
                    return;
                }
                // "Step=+32 (or Step=+1024 when Rate=31)".
                let step = if rate == 31 { 1024 } else { 32 };
                let level = i32::from(voice.env) + step;
                if level >= 0x7E0 {
                    // "At Level>=7E0h: Switch from Attack to Decay mode (and clip level to
                    // max=7FFh if level>=800h)."
                    voice.env = clip11(level);
                    voice.state = EnvState::Decay;
                } else {
                    voice.env = level as u16;
                }
            }
            EnvState::Decay => {
                let rate = ((voice.adsr1 >> 4) & 0x07) * 2 + 16;
                if !rate_due(voice, rate) {
                    return;
                }
                let level = i32::from(voice.env);
                let step = -(((level - 1) >> 8) + 1);
                let boundary = i32::from(((voice.adsr2 >> 5) & 0x07) + 1) * 0x100;
                voice.env = clip11(level + step);
                // "At Level<=Boundary: Switch from Decay to Sustain mode."
                if i32::from(voice.env) <= boundary {
                    voice.state = EnvState::Sustain;
                }
            }
            EnvState::Sustain => {
                let rate = voice.adsr2 & 0x1F;
                if !rate_due(voice, rate) {
                    return;
                }
                let level = i32::from(voice.env);
                let step = -(((level - 1) >> 8) + 1);
                voice.env = clip11(level + step);
            }
            EnvState::Release => {
                // "Rate=31, Step=-8 (or Step=-800h when BRR-end)".
                if !rate_due(voice, 31) {
                    return;
                }
                voice.env = voice.env.saturating_sub(8);
            }
            EnvState::Off => {}
        }
    }

    /// §9.2's `Dsp` row, in the table's order, in the host's `brr` width (§4.2's D2).
    ///
    /// Signed words are written `u16(v as u16)` on both hosts: that is the two's-complement pair of
    /// bytes the SNES's `i16` method wrote, so its state does not move, and it is the method the SGB
    /// host already used.
    pub fn save_state<W: StateWrite>(&self, w: &mut W, host: Host) {
        w.bytes(&self.regs);
        for voice in &self.voices {
            w.u16(voice.pitch);
            w.u16(voice.counter);
            w.u8(voice.srcn);
            match host {
                Host::Snes => w.u32(voice.brr),
                Host::Sgb => w.u16(voice.brr as u16),
            }
            w.u8(voice.nibble);
            w.u8(voice.brr_header);
            w.u8(voice.adsr1);
            w.u8(voice.adsr2);
            w.u8(voice.gain);
            w.u16(voice.env);
            w.u8(voice.state.to_u8());
            w.u32(voice.rate_acc);
            w.u16(voice.old as u16);
            w.u16(voice.older as u16);
            for &sample in &voice.buf {
                w.u16(sample as u16);
            }
            w.u16(voice.out as u16);
            w.u8(voice.kon_delay);
        }
        w.u16(self.noise);
        w.u8(self.kon);
        w.u8(self.koff);
        w.u8(self.endx);
        w.u8(self.mask);
        w.u64(self.samples);
        for &tap in &self.echo.fir {
            w.u8(tap as u8);
        }
        for &sample in &self.echo.buf_l {
            w.u16(sample as u16);
        }
        for &sample in &self.echo.buf_r {
            w.u16(sample as u16);
        }
        w.u8(self.echo.index);
        w.u32(self.echo.ram_index);
        w.u32(self.echo.remain);
    }

    /// The inverse of [`Dsp::save_state`], reading the host's `brr` width.
    pub fn load_state<R: StateRead>(&mut self, r: &mut R, host: Host) -> Result<(), R::Error> {
        r.bytes_into(&mut self.regs)?;
        for voice in &mut self.voices {
            voice.pitch = r.u16()?;
            voice.counter = r.u16()?;
            voice.srcn = r.u8()?;
            voice.brr = match host {
                Host::Snes => r.u32()?,
                Host::Sgb => u32::from(r.u16()?),
            };
            voice.nibble = r.u8()?;
            voice.brr_header = r.u8()?;
            voice.adsr1 = r.u8()?;
            voice.adsr2 = r.u8()?;
            voice.gain = r.u8()?;
            voice.env = r.u16()?;
            voice.state = EnvState::from_u8(r.u8()?);
            voice.rate_acc = r.u32()?;
            voice.old = r.u16()? as i16;
            voice.older = r.u16()? as i16;
            for sample in &mut voice.buf {
                *sample = r.u16()? as i16;
            }
            voice.out = r.u16()? as i16;
            voice.kon_delay = r.u8()?;
        }
        self.noise = r.u16()?;
        self.kon = r.u8()?;
        self.koff = r.u8()?;
        self.endx = r.u8()?;
        self.mask = r.u8()?;
        self.samples = r.u64()?;
        for tap in &mut self.echo.fir {
            *tap = r.u8()? as i8;
        }
        for sample in &mut self.echo.buf_l {
            *sample = r.u16()? as i16;
        }
        for sample in &mut self.echo.buf_r {
            *sample = r.u16()? as i16;
        }
        self.echo.index = r.u8()?;
        self.echo.ram_index = r.u32()?;
        self.echo.remain = r.u32()?;
        Ok(())
    }
}

/// The 4-bit attack rate as a 5-bit rate: "Rate=N*2+1".
fn attack_rate(adsr1: u8) -> u8 {
    (adsr1 & 0x0F) * 2 + 1
}

/// True when an envelope step is due at this rate, and reloads the accumulator if so.
fn rate_due(voice: &mut Voice, rate: u8) -> bool {
    let units = RATE_UNITS[usize::from(rate & 0x1F)];
    if units == 0 {
        return false; // "00h=Stop"
    }
    if voice.rate_acc == 0 {
        voice.rate_acc = units - 1;
        true
    } else {
        voice.rate_acc -= 1;
        false
    }
}

/// The envelope's 11-bit clip: "clip E to 0 or 0x7ff rather than wrapping".
fn clip11(level: i32) -> u16 {
    level.clamp(0, 0x7FF) as u16
}

/// A 16-bit addition with the overflow handling the mixer's "after each addition" needs.
fn saturate16(value: i32) -> i32 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
}

/// A 15-bit level as a signed sample: `-4000h..+3FFFh`.
fn sign15(level: u16) -> i16 {
    ((level << 1) as i16) >> 1
}
