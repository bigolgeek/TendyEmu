//! The BIOS sound driver: `$1A`-`$1F`, `$28`/`$29` and the undocumented `$20`-`$24`.
//!
//! This is the software mixer the GBA BIOS carries for cartridges that do not write the FIFOs
//! themselves. GBATEK, *BIOS Sound Functions*, describes its **state** in full — the `SoundArea`
//! work area the caller owns, the `SndCh` channel records inside it, the `WaveData` a channel points
//! at, and the 32-bit mode word — and says nothing about the mixing loop. So the loop below is
//! derived from those structures and from the one caller this machine has: X-Men: Reign of
//! Apocalypse, which initialises with its work area at `$0200F420`, sets mode `$00030000`, starts
//! four channels and calls `Main` then `Vsync` once a frame. Its writes are the layout's authority,
//! and they disagree with GBATEK's byte count in two places, which is why they were measured before
//! anything was implemented:
//!
//! * `vchn[0]` sits at **`+$50`**, not at `+$14` — the header is bigger than GBATEK's field list
//!   adds up to;
//! * a channel record is **`$40` bytes**, not the 48 its listed fields add up to, and the caller
//!   writes `fr` at `+$20` and `wp` at `+$24`.
//!
//! The two facts are self-consistent (`$50 + 8 * $40 = $250`, where the `pcmbuf` follows), and the
//! caller's own numbers confirm the `fr` scale: channel `fr` `$2B11` (11 025) from a wave whose
//! `freq` is `$02B0AA32` (45 133 362) is key 36, and `45 133 362 * 2^((36-180)/12) = 11 025`.
//!
//! **Where the driver's state lives.** Almost all of it is in the caller's work area, exactly as
//! GBATEK lays it out, so a save state carries it for free: the mode word, the playback rate, the
//! sample accumulator and each channel's mixer position and envelope level all sit in the area's
//! "user access prohibited" bytes, which the caller never writes. The one thing that cannot live
//! there is the address of the area itself; [`crate::bios::Bios::sound_work`] holds it, which
//! is why the Advance's state version moved to 9.
//!
//! **What is mixed.** Per `Main`: one envelope step per channel (1/60 s as GBATEK says), then
//! `rate / 59.7275` samples — the GBA's own frame rate, not a rounded 60 Hz, because the FIFOs drain
//! at the timer's rate and a 60 Hz mix would starve them by 0.45 %. Each sample advances every
//! voice's phase by `fr / rate`, reads that byte from the `WaveData`, and sums
//! `data * level/255 * lv/255` for the left buffer and the same with `rv` for the right. FIFO A
//! carries left and FIFO B right, which is the only way two mono FIFOs can produce the stereo the
//! `rv`/`lv` pair implies.
//!
//! **What is not modelled.** Reverb (GBATEK gives a 0-127 field and no filter), and the undocumented
//! `$20`-`$24` entries (GBATEK's own heading is "SoundWhatever0..4"), which are answered and do
//! nothing rather than refused: a title that calls them keeps running.

use crate::CLOCK_HZ;
use crate::bus::{Bus, CYCLES_PER_FRAME, IO_BASE};
use crate::cpu::Cpu;
use crate::sound::{self, FIFO_A, FIFO_B, SOUNDBIAS, SOUNDCNT_H, SOUNDCNT_X};

/// The word this driver writes at `SoundArea::ident` once [`init`] has run. GBATEK says the field
/// "flags ... whether the work area has been initialized" without giving its value; any non-zero
/// constant does that job, and this one is the four ASCII bytes "Smsh", which is what the field is
/// called in the tools' own headers.
pub const IDENT: u32 = 0x6873_6D53;

/// `vchn[0]`: measured from X-Men's own writes (see the module documentation).
pub const VCHN: u32 = 0x50;
/// One `SndCh`, as the caller writes it: measured, not the sum of GBATEK's field list.
pub const CHANNEL_STRIDE: u32 = 0x40;
/// `MAX`: GBATEK — "currently 8 channels are available".
pub const CHANNELS: usize = 8;
/// The `pcmbuf` that follows the channel array: `$50 + 8 * $40`.
pub const PCMBUF: u32 = VCHN + CHANNELS as u32 * CHANNEL_STRIDE;

/// Samples in one buffer half. The fastest documented playback rate is 42 048 Hz, and
/// `42048 * 280896 / 16777216 = 704` exactly, so no mode can overrun a half.
pub const CHUNK: usize = 704;
/// The area this driver owns: the header, the channels, and two halves for each side.
pub const AREA_LEN: u32 = PCMBUF + 4 * CHUNK as u32;

/// `SndCh` fields, at their measured offsets.
const SF: u32 = 0x00;
const RV: u32 = 0x02;
const LV: u32 = 0x03;
const AT: u32 = 0x04;
const DE: u32 = 0x05;
const SU: u32 = 0x06;
const RE: u32 = 0x07;
const FR: u32 = 0x20;
const WP: u32 = 0x24;

/// The driver's own per-channel bytes, in the `r2`/`r3` fields GBATEK marks "User access
/// prohibited": the mixer's phase, the envelope level, and whether the channel has been started.
const PHASE: u32 = 0x08;
const LEVEL: u32 = 0x0C;
const STATE: u32 = 0x0D;

/// `SoundArea` header fields this driver uses. `ident`, `DmaCount`, `reverb`, `d1`, `func`, `intp`
/// and `NoUse` are GBATEK's own names for the first 20 bytes; the rest are bytes GBATEK's field list
/// does not reach, and the caller does not touch either.
const H_IDENT: u32 = 0x00;
const H_CHANNELS: u32 = 0x06;
const H_MODE: u32 = 0x08;
const H_RATE: u32 = 0x0C;
const H_ACC: u32 = 0x10;
const H_MASTER: u32 = 0x18;
const H_DAC: u32 = 0x19;
const H_FRONT: u32 = 0x1A;
const H_PAUSED: u32 = 0x1B;
const H_SAMPLES: u32 = 0x1C;

/// `WaveData`'s header length: `type`, `stat`, `freq`, `loop`, `size`, then `data[]`.
const WAVE_DATA: u32 = 16;
/// `WaveData::stat` bit 14: "forward loop".
const WAVE_LOOP: u16 = 0x4000;

/// The playback rates the mode word's bits 16-19 select (GBATEK, *SWI 1Bh*), 1-based in the word.
pub const RATES: [u32; 12] = [
    5734, 7884, 10512, 13379, 15768, 18157, 21024, 26758, 31536, 36314, 40137, 42048,
];

/// The sound-DMA control word: enable, special timing, 32-bit, repeat, fixed destination. GBATEK's
/// *Sound DMA*: "4 units of 32bits (16 bytes) are transferred ... The destination address will not
/// be incremented in FIFO mode", and the repeat bit is what keeps the channel armed between blocks.
const DMA_CONTROL: u16 = 0xB640;

/// Timer registers, as offsets in the I/O page. The driver uses timer 1 (see [`apply_mode`]).
const TM1CNT_L: usize = 0x104;
const TM1CNT_H: usize = 0x106;
/// DMA1 and DMA2, whose destinations the FIFOs are.
const DMA1: usize = 0xB0 + 0x0C;
const DMA2: usize = 0xB0 + 2 * 0x0C;

/// The cycles each entry charges. The real bodies are longer; these are estimates chosen so a
/// per-frame caller pays something proportional to the work (the mix scales with sample count).
const INIT_CYCLES: u32 = 64;
const MODE_CYCLES: u32 = 32;
const MAIN_BASE_CYCLES: u32 = 16;
const VSYNC_CYCLES: u32 = 32;
const CLEAR_CYCLES: u32 = 48;

// ---------------------------------------------------------------------------- memory helpers

fn rd8(bus: &mut Bus, at: u32) -> u8 {
    bus.read8(at)
}

fn wr8(bus: &mut Bus, at: u32, value: u8) {
    bus.write8(at, value);
}

fn rd16(bus: &mut Bus, at: u32) -> u16 {
    bus.read16(at)
}

fn wr16(bus: &mut Bus, at: u32, value: u16) {
    bus.write16(at, value);
}

fn rd32(bus: &mut Bus, at: u32) -> u32 {
    bus.read32(at)
}

fn wr32(bus: &mut Bus, at: u32, value: u32) {
    bus.write32(at, value);
}

fn io16(bus: &mut Bus, offset: usize, value: u16) {
    bus.write16(IO_BASE + offset as u32, value);
}

fn io32(bus: &mut Bus, offset: usize, value: u32) {
    bus.write32(IO_BASE + offset as u32, value);
}

fn io_read16(bus: &mut Bus, offset: usize) -> u16 {
    bus.read16(IO_BASE + offset as u32)
}

/// The 64-bit sample accumulator, as two words.
fn rd64(bus: &mut Bus, at: u32) -> u64 {
    u64::from(rd32(bus, at)) | u64::from(rd32(bus, at + 4)) << 32
}

fn wr64(bus: &mut Bus, at: u32, value: u64) {
    wr32(bus, at, value as u32);
    wr32(bus, at + 4, (value >> 32) as u32);
}

// ---------------------------------------------------------------------------- the work area

/// The work area, if this machine has one this driver can trust: a word-aligned address in work RAM
/// whose `ident` is [`IDENT`]. A `RegisterRamReset` that clears EWRAM therefore stops the driver
/// rather than leaving it mixing into a work area that is no longer its own.
fn area(bus: &Bus) -> Option<u32> {
    let at = bus.bios.sound_work?;
    let ram = (0x0200_0000..0x0204_0000).contains(&at) || (0x0300_0000..0x0300_8000).contains(&at);
    if !ram || at & 3 != 0 {
        return None;
    }
    Some(at)
}

fn initialized(bus: &mut Bus) -> Option<u32> {
    let at = area(bus)?;
    (rd32(bus, at + H_IDENT) == IDENT).then_some(at)
}

fn rate_of(bus: &mut Bus, work: u32) -> u32 {
    rd32(bus, work + H_RATE).max(1)
}

fn channel_count(bus: &mut Bus, work: u32) -> usize {
    usize::from(rd16(bus, work + H_CHANNELS)).clamp(1, CHANNELS)
}

fn front(bus: &mut Bus, work: u32) -> usize {
    usize::from(rd8(bus, work + H_FRONT) & 1)
}

/// Where one half-buffer of one side starts: `side` 0 is left (FIFO A), 1 is right (FIFO B).
fn buffer(work: u32, side: usize, half: usize) -> u32 {
    work + PCMBUF + ((side * 2 + (half & 1)) * CHUNK) as u32
}

// ---------------------------------------------------------------------------- the calls

/// `SoundDriverInit` (`$1A`): take the caller's work area, clear it, and bring the driver up in
/// GBATEK's defaults. The caller must have secured the area itself — "It is essential that the work
/// area already be secured at the time this function is called".
pub fn init(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let work = cpu.r[0] & !3;
    let ram =
        (0x0200_0000..0x0204_0000).contains(&work) || (0x0300_0000..0x0300_8000).contains(&work);
    if !ram {
        // Not a work area at all: leave the driver alone rather than scribble over I/O or ROM.
        return INIT_CYCLES;
    }
    for at in 0..AREA_LEN {
        wr8(bus, work + at, 0);
    }
    bus.bios.sound_work = Some(work);
    wr32(bus, work + H_IDENT, IDENT);
    wr8(bus, work + 4, 2); // DmaCount: FIFO A and FIFO B
    apply_mode(bus, work, DEFAULT_MODE);
    INIT_CYCLES
}

/// The mode word the driver starts with: GBATEK's defaults are 8 channels, master 15, rate 4
/// (13 379 Hz) and DAC field 9 (8 bits), with reverb off.
pub const DEFAULT_MODE: u32 = (4 << 16) | (15 << 12) | (8 << 8) | (9 << 20);

/// `SoundDriverMode` (`$1B`): decode the 32-bit mode word and program the hardware for it.
///
/// A field of zero is not a setting — GBATEK's ranges are 1-12 channels, 1-15 master volume, 1-12
/// playback rate and 8-11 DAC bits — so each out-of-range field falls back to the documented
/// default. X-Men's word is `$00030000`: rate 3 (10 512 Hz) and zero everywhere else, which is
/// exactly the case this rule exists for.
pub fn mode(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    if let Some(work) = initialized(bus) {
        apply_mode(bus, work, cpu.r[0]);
    }
    MODE_CYCLES
}

fn apply_mode(bus: &mut Bus, work: u32, word: u32) {
    let channels = ((word >> 8) & 0xF) as u16;
    let channels = if channels == 0 {
        8
    } else {
        channels.min(CHANNELS as u16)
    };
    let master = ((word >> 12) & 0xF) as u8;
    let master = if master == 0 { 15 } else { master };
    let rate_index = (word >> 16) & 0xF;
    let rate = if (1..=12).contains(&rate_index) {
        RATES[rate_index as usize - 1]
    } else {
        RATES[3]
    };
    let dac = ((word >> 20) & 0xF) as u8;
    let dac = if (8..=11).contains(&dac) { dac } else { 9 };

    wr32(bus, work + H_MODE, word);
    wr32(bus, work + H_RATE, rate);
    wr16(bus, work + H_CHANNELS, channels);
    wr8(bus, work + H_MASTER, master);
    wr8(bus, work + H_DAC, dac);
    // Bits 0-6 are the reverb value and bit 7 is GBATEK's "Direct Sound Reverb set (0=ignore,
    // 1=apply reverb value)"; the field is stored as given, so a caller can read back what it set.
    wr8(
        bus,
        work + 5,
        if word & 0x80 != 0 {
            (word & 0x7F) as u8
        } else {
            0
        },
    );

    // The sample clock. GBATEK's initialisation writes `0xFFFF - round(2^24 / playrate)`, and the
    // bus turns a timer period of `p` into `2^24 / p` samples a second.
    //
    // Timer 1, not timer 0: the driver has to leave a timer the caller can still use, and X-Men
    // writes both timers within its first frames (measured — `TM0CNT_L`/`TM1CNT_L` `$FDA8`,
    // controls `0000` at frame 2). That is why the clock is also re-armed on every `Vsync`: the
    // caller's own code may have stopped it since the last frame.
    clock(bus, rate);

    // FIFOs at full volume, A to the left and B to the right, both clocked by timer 1. The PSG ratio
    // bits are the caller's and are kept.
    let control = output_control(bus);
    io16(
        bus,
        SOUNDCNT_H,
        control | sound::FIFO_A_RESET | sound::FIFO_B_RESET,
    );
    io16(bus, SOUNDCNT_H, control);

    // The DAC's amplitude resolution is `SOUNDBIAS`'s sampling cycle: field 8-11 is 9-6 bits,
    // which is cycle 0-3. The bias level stays where `SoundBias` puts it.
    io16(bus, SOUNDBIAS, 0x0200 | (((dac - 8) as u16) << 14));
    // Master enable, leaving the PSG channel-enable bits as the caller left them.
    let x = io_read16(bus, SOUNDCNT_X);
    io16(bus, SOUNDCNT_X, x | sound::MASTER_ENABLE);
}

/// `SOUNDCNT_H` as this driver wants it: FIFOs at full volume, A left, B right, both on timer 1 —
/// with the PSG mix-ratio bits, which are the caller's, untouched.
fn output_control(bus: &mut Bus) -> u16 {
    (io_read16(bus, SOUNDCNT_H) & 0b11)
        | sound::FIFO_A_VOLUME
        | sound::FIFO_B_VOLUME
        | sound::FIFO_A_LEFT
        | sound::FIFO_B_RIGHT
        | sound::FIFO_A_TIMER
        | sound::FIFO_B_TIMER
}

/// Start the sample clock at `rate` samples a second on timer 1.
///
/// Writing the reload and the enable bit while the timer is already running is harmless: the
/// hardware (and this machine's timer) copies the reload into the counter only on a 0-to-1 start
/// edge, so a per-frame re-assert does not restart the count and lose a sample. Pops measure
/// 176 a frame at 10 512 Hz, which is the rate exactly.
fn clock(bus: &mut Bus, rate: u32) {
    let period = ((1u64 << 24) + u64::from(rate) / 2) / u64::from(rate);
    let reload = (0x1_0000u64 - period.clamp(1, 0x1_0000)) as u16;
    io16(bus, TM1CNT_L, reload);
    io16(bus, TM1CNT_H, 0x0080); // enable, prescaler 1, no cascade, no interrupt
}

/// `SoundDriverMain` (`$1C`): one frame of mixing into the buffer the next `Vsync` will play.
pub fn main(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let _ = cpu;
    let Some(work) = initialized(bus) else {
        return MAIN_BASE_CYCLES;
    };
    if rd8(bus, work + H_PAUSED) != 0 {
        return MAIN_BASE_CYCLES;
    }
    let rate = rate_of(bus, work);
    // Samples owed: the FIFO drains at `rate` and the frame is 280 896 cycles of a 16 777 216 Hz
    // clock, so a whole number of frames is an exact number of samples at every documented rate.
    let acc = rd64(bus, work + H_ACC) + u64::from(rate) * CYCLES_PER_FRAME as u64;
    let samples = (acc / u64::from(CLOCK_HZ)) as u32;
    wr64(bus, work + H_ACC, acc % u64::from(CLOCK_HZ));
    let samples = samples.min(CHUNK as u32);
    wr32(bus, work + H_SAMPLES, samples);

    let back = front(bus, work) ^ 1;
    let left_base = buffer(work, 0, back);
    let right_base = buffer(work, 1, back);
    // A short frame leaves the tail of the half-buffer holding the previous frame's samples; zeroing
    // it keeps a late pop from playing them.
    for at in samples..CHUNK as u32 {
        wr8(bus, left_base + at, 0);
        wr8(bus, right_base + at, 0);
    }

    let mut voices = voices(bus, work, rate);
    let master = i32::from(rd8(bus, work + H_MASTER).max(1));
    for index in 0..samples {
        let mut left = 0i32;
        let mut right = 0i32;
        for voice in &mut voices {
            if voice.sf & 0x80 == 0 {
                continue;
            }
            left += voice.sample(bus, true);
            right += voice.sample(bus, false);
            voice.advance(bus);
        }
        left = left * master / 15;
        right = right * master / 15;
        wr8(bus, left_base + index, clip(left));
        wr8(bus, right_base + index, clip(right));
    }
    store(bus, &voices);
    MAIN_BASE_CYCLES + samples
}

/// `SoundDriverVsync` (`$1D`): "An extremely short system call that resets the sound DMA."
///
/// The half `Main` has just filled becomes the half the DMA plays, and both FIFO channels are
/// re-armed at its start. Writing the source address and then the control word is what reloads the
/// channel's internal pointer — GBATEK: "Upon DMA Enable (Bit 15) changing from 0 to 1: Reloads SAD,
/// DAD, CNT_L".
pub fn vsync(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let _ = cpu;
    let Some(work) = initialized(bus) else {
        return VSYNC_CYCLES;
    };
    if rd8(bus, work + H_PAUSED) != 0 {
        return VSYNC_CYCLES;
    }
    let half = front(bus, work) ^ 1;
    wr8(bus, work + H_FRONT, half as u8);
    // "An extremely short system call that resets the sound DMA" — and, because a caller's own code
    // may have stopped them since the last frame, the sample clock and the output control too.
    let rate = rate_of(bus, work);
    clock(bus, rate);
    let control = output_control(bus);
    io16(bus, SOUNDCNT_H, control);
    arm(bus, DMA1, buffer(work, 0, half), FIFO_A);
    arm(bus, DMA2, buffer(work, 1, half), FIFO_B);
    VSYNC_CYCLES
}

fn arm(bus: &mut Bus, channel: usize, source: u32, fifo: usize) {
    io32(bus, channel, source); // SAD
    io32(bus, channel + 4, IO_BASE + fifo as u32); // DAD
    io16(bus, channel + 8, 4); // CNT_L: ignored in FIFO mode, written for the record
    io16(bus, channel + 10, DMA_CONTROL);
}

/// `SoundChannelClear` (`$1E`): "Clears all direct sound channels and stops the sound."
pub fn channel_clear(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let _ = cpu;
    if let Some(work) = initialized(bus) {
        for at in VCHN..VCHN + CHANNELS as u32 * CHANNEL_STRIDE {
            wr8(bus, work + at, 0);
        }
        for at in 0..4 * CHUNK as u32 {
            wr8(bus, work + PCMBUF + at, 0);
        }
        wr64(bus, work + H_ACC, 0);
        wr8(bus, work + H_FRONT, 0);
        silence(bus);
    }
    CLEAR_CYCLES
}

/// Stop the two FIFO channels and their sample clock, and reset both FIFOs.
fn silence(bus: &mut Bus) {
    io16(bus, DMA1 + 10, DMA_CONTROL & !0x8000);
    io16(bus, DMA2 + 10, DMA_CONTROL & !0x8000);
    io16(bus, TM1CNT_H, 0);
    let control = io_read16(bus, SOUNDCNT_H);
    io16(
        bus,
        SOUNDCNT_H,
        control | sound::FIFO_A_RESET | sound::FIFO_B_RESET,
    );
    io16(
        bus,
        SOUNDCNT_H,
        control & !(sound::FIFO_A_RESET | sound::FIFO_B_RESET),
    );
}

/// `SoundDriverVSyncOff` (`$28`): stop the DMA before V-Blanks stop arriving, "otherwise, even if you
/// exceed the limit of the buffer the DMA will not stop and noise will result".
pub fn vsync_off(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let _ = cpu;
    if let Some(work) = initialized(bus) {
        wr8(bus, work + H_PAUSED, 1);
        io16(bus, DMA1 + 10, DMA_CONTROL & !0x8000);
        io16(bus, DMA2 + 10, DMA_CONTROL & !0x8000);
    }
    16
}

/// `SoundDriverVSyncOn` (`$29`): allow the next `Vsync` to re-arm the channels.
pub fn vsync_on(cpu: &mut Cpu, bus: &mut Bus) -> u32 {
    let _ = cpu;
    if let Some(work) = initialized(bus) {
        wr8(bus, work + H_PAUSED, 0);
        wr8(bus, work + H_FRONT, 0);
    }
    16
}

/// `$20`-`$24`: GBATEK's "SoundWhatever0..4 (Undocumented)". Answered, and nothing else — a title
/// that calls them keeps running instead of halting on a refusal, and the driver invents no
/// behaviour the document does not describe.
pub fn undocumented(cpu: &mut Cpu) -> u32 {
    cpu.r[0] = 0;
    4
}

// ---------------------------------------------------------------------------- the mixer

/// One channel's state for the frame being mixed.
struct Voice {
    at: u32,
    sf: u8,
    level: i32,
    phase: u32,
    step: u32,
    data: u32,
    size: u32,
    loop_at: u32,
    looped: bool,
    lv: i32,
    rv: i32,
}

impl Voice {
    /// The byte under the phase pointer, weighted by the envelope and this side's volume.
    ///
    /// The arithmetic is integer throughout — `data * level/255 * volume/255`, truncated — because
    /// that is what makes a synthesized square wave and envelope hand-computable in a test.
    fn sample(&self, bus: &mut Bus, left: bool) -> i32 {
        let position = self.phase >> 16;
        if position >= self.size {
            return 0;
        }
        let value = i32::from(rd8(bus, self.data + position) as i8);
        let volume = if left { self.lv } else { self.rv };
        value * self.level * volume / (255 * 255)
    }

    /// Move the phase one output sample on, wrapping at the loop point or stopping at the end. The
    /// wrap keeps the sub-sample fraction, so a channel whose step does not divide its loop length
    /// does not drift.
    fn advance(&mut self, _bus: &mut Bus) {
        self.phase = self.phase.saturating_add(self.step);
        if self.phase >> 16 >= self.size {
            if self.looped {
                let over = self.phase.wrapping_sub(self.size << 16);
                self.phase = (self.loop_at << 16).wrapping_add(over);
            } else {
                self.sf = 0;
                self.phase = 0;
            }
        }
    }
}

/// The phases a channel's envelope moves through, kept in its private [`STATE`] byte.
const IDLE: u8 = 0;
const ATTACK: u8 = 1;
const DECAY: u8 = 2;
const RELEASE: u8 = 3;

/// Read every channel, take its envelope step for this frame, and turn its `fr` into a phase step.
fn voices(bus: &mut Bus, work: u32, rate: u32) -> Vec<Voice> {
    let mut voices = Vec::with_capacity(CHANNELS);
    for index in 0..channel_count(bus, work) {
        let at = work + VCHN + index as u32 * CHANNEL_STRIDE;
        let mut sf = rd8(bus, at + SF);
        let mut level = i32::from(rd8(bus, at + LEVEL));
        let mut phase = rd32(bus, at + PHASE);
        let mut state = rd8(bus, at + STATE);

        if sf & 0x80 == 0 {
            // Not started: the channel's mixer state is meaningless until the caller writes `80h`.
            state = IDLE;
            level = 0;
            phase = 0;
        } else {
            if state == IDLE {
                // "To start sound, set other parameters and then write 80h to here": the phase
                // starts at the waveform's first sample and the envelope at zero, so the attack
                // below carries it one step up — the frame a channel starts is already audible.
                state = ATTACK;
                level = 0;
                phase = 0;
            }
            if sf & 0x40 != 0 {
                // "To stop sound, logical OR 40h for a release-attached off (key-off)".
                state = RELEASE;
            }
            level = envelope(bus, at, &mut sf, &mut state, level);
        }
        if sf & 0x80 == 0 {
            state = IDLE;
            level = 0;
            phase = 0;
        }
        wr8(bus, at + STATE, state);
        wr8(bus, at + LEVEL, level.clamp(0, 255) as u8);
        wr32(bus, at + PHASE, phase);

        let frequency = rd32(bus, at + FR);
        let wave = rd32(bus, at + WP);
        let (size, loop_at, looped) = if wave == 0 {
            (0, 0, false)
        } else {
            let stat = rd16(bus, wave + 2);
            (
                rd32(bus, wave + 12),
                rd32(bus, wave + 8),
                stat & WAVE_LOOP != 0,
            )
        };
        voices.push(Voice {
            at,
            sf,
            level,
            phase,
            step: step(frequency, rate),
            data: wave + WAVE_DATA,
            size,
            loop_at: loop_at.min(size.saturating_sub(1)),
            looped: looped && size > 0,
            lv: i32::from(rd8(bus, at + LV)),
            rv: i32::from(rd8(bus, at + RV)),
        });
    }
    voices
}

/// One envelope step, GBATEK's attack/decay/sustain/release at 1/60 s per call.
///
/// * attack: "the volume begins at zero and increases every 1/60 second. When it reaches 255, the
///   process moves on to the next decay value";
/// * decay: "multiplied by `this value/256` every 1/60 sec. and when sustain value is reached, the
///   process moves to the sustain condition";
/// * release (`sf` bit 6): "the value is multiplied by `this value/256` every 1/60 sec. and when it
///   reaches zero, this channel is completely stopped".
///
/// A decay value of zero multiplies the level to zero, which is below any sustain value, so the
/// sustain clause pins it — that is why X-Men's `de = 0, su = $FF` holds a channel wide open instead
/// of silencing it on the second frame.
fn envelope(bus: &mut Bus, at: u32, sf: &mut u8, state: &mut u8, level: i32) -> i32 {
    match *state {
        RELEASE => {
            let release = i32::from(rd8(bus, at + RE));
            let level = level * release / 256;
            if level <= 0 {
                *sf = 0;
                *state = IDLE;
                return 0;
            }
            level
        }
        ATTACK => {
            let attack = i32::from(rd8(bus, at + AT));
            let level = (level + attack).min(255);
            if level >= 255 {
                *state = DECAY;
            }
            level
        }
        _ => {
            let decay = i32::from(rd8(bus, at + DE));
            let sustain = i32::from(rd8(bus, at + SU));
            // "when sustain value is reached, the process moves to the sustain condition" — the
            // level stops falling at `su`, so a decay of zero over a sustain of 255 holds 255.
            (level * decay / 256).max(sustain)
        }
    }
}

/// `fr` is a rate in samples a second (the scale [`crate::bios`]'s `MidiKey2Freq` returns), so
/// the phase advances by `fr / rate` source samples per output sample, in 16.16 fixed point.
fn step(frequency: u32, rate: u32) -> u32 {
    let value = (u64::from(frequency) << 16) / u64::from(rate.max(1));
    value.min(u64::from(u32::MAX)) as u32
}

/// Write the frame's phase and envelope back into the caller's channel records.
fn store(bus: &mut Bus, voices: &[Voice]) {
    for voice in voices {
        wr8(bus, voice.at + SF, voice.sf);
        wr8(bus, voice.at + LEVEL, voice.level.clamp(0, 255) as u8);
        wr32(bus, voice.at + PHASE, voice.phase);
    }
}

fn clip(value: i32) -> u8 {
    value.clamp(-128, 127) as i8 as u8
}
