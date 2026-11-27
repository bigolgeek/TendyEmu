//! Namco 163 audio: up to eight 4-bit wavetable channels sharing 128 bytes of chip RAM.
//!
//! The chip has no register file of its own: the game writes the chip's RAM through an address
//! port and a data port, and the channel control registers are simply RAM bytes `$40-$7F`
//! (channel 1 at `$40-$47` … channel 8 at `$78-$7F`):
//!
//! ```text
//! $F800-$FFFF  IAAA AAAA   internal address, I = auto-increment on data-port accesses
//! $4800-$4FFF  DDDD DDDD   data port
//!
//! base+0  frequency low 8          base+1  phase low 8
//! base+2  frequency middle 8       base+3  phase middle 8
//! base+4  LLLL LLFF  wave length + frequency high 2
//! base+5  phase high 8             base+6  wave address (4-bit samples)
//! base+7  .CCC VVVV  volume + number of enabled channels (this register only)
//! ```
//!
//! One channel is updated every 15 CPU cycles, highest-numbered channel first, and only over the
//! enabled channels: `C = 0` means channel 8 alone, `C = 7` all eight. Each update adds the
//! channel's 18-bit frequency to its 24-bit phase and takes one 4-bit sample; the sample is
//! biased by -8 and scaled by the channel volume.
//!
//! The wiki notes the chip outputs one channel at a time rather than mixing them, which for eight
//! channels is an audible 14.9 kHz whine. This module sums the channels and divides by the number
//! enabled — the approximation the wiki itself recommends — so the switching frequency does not
//! alias into the 44.1 kHz output.
//!
//! The address latch is registered: a `$F800` write is installed at the start of the *next* CPU
//! cycle (see [`Namco163Audio::clock`]), so a data-port access that lands in the same cycle as the
//! address write still uses the previous address.

use crate::state::{StateError, StateReader, StateWriter};

/// Peak of a fully-driven chip against the 2A03 mixer. The N163 mixes at 11-19.5 dB above the
/// APU depending on the board's mixing resistor (NES 2.0 mappers 19 submappers 3-5), so the board
/// picks the scale from the submapper; this is the submapper-0 (unspecified) default, the ~13 dB
/// "ordinary" mix.
pub const N163_AUDIO_SCALE: f32 = 0.25;

/// The chip's RAM, in bytes.
pub const N163_RAM_LEN: usize = 128;

/// Cycles per channel update.
const CHANNEL_PERIOD: u8 = 15;

/// The first channel's register block in RAM: channel 1 at `$40-$47`, channel 8 at `$78-$7F`.
const CHANNEL_BASE: usize = 0x40;

#[derive(Clone, Debug)]
pub struct Namco163Audio {
    ram: [u8; N163_RAM_LEN],
    /// 7-bit address installed by `$F800`.
    addr: u8,
    auto_increment: bool,
    /// A `$F800` write waiting for the next CPU cycle to install it.
    pending: Option<(u8, bool)>,
    /// Cycles since the last channel update, 0..CHANNEL_PERIOD.
    schedule: u8,
    /// Channel this schedule step updates, 0..=7 (index, channel number = index + 1).
    next_channel: usize,
    /// Each channel's last computed output, held until its next update.
    levels: [i16; 8],
    /// `$E000` bit 6: the generator is muted while set (it keeps running).
    disabled: bool,
    /// Mixing level from the board's NES 2.0 submapper.
    scale: f32,
    /// The cached level: [`Namco163Audio::analog_sum`] of the held channel levels, rebuilt only
    /// when something it is built from moved instead of on every CPU cycle.
    level: f32,
    /// `level` is stale: a channel updated, the RAM changed, or the chip was muted/unmuted.
    level_dirty: bool,
}

impl Default for Namco163Audio {
    fn default() -> Self {
        Namco163Audio {
            ram: [0; N163_RAM_LEN],
            addr: 0,
            auto_increment: false,
            pending: None,
            schedule: 0,
            next_channel: 7,
            levels: [0; 8],
            disabled: false,
            scale: N163_AUDIO_SCALE,
            level: 0.0,
            level_dirty: true,
        }
    }
}

impl Namco163Audio {
    pub fn new() -> Self {
        Self::default()
    }

    /// NES 2.0 mapper-19 submapper: 3/4/5 select the progressively louder mixing resistors, 0-2
    /// leave the ordinary level (submapper 2 is "no expansion sound" and is muted by the board).
    pub fn set_submapper(&mut self, submapper: u8) {
        self.scale = match submapper {
            4 => N163_AUDIO_SCALE * 1.58, // +4 dB over the ordinary mix
            5 => N163_AUDIO_SCALE * 2.00, // +6 dB
            _ => N163_AUDIO_SCALE,
        };
        self.level_dirty = true;
    }

    /// `$F800-$FFFF`: latch the internal address. The latch is installed by the next
    /// [`Namco163Audio::clock`].
    pub fn write_address(&mut self, val: u8) {
        self.pending = Some((val & 0x7F, val & 0x80 != 0));
    }

    /// `$4800-$4FFF` read, with the auto-increment side effect.
    pub fn read_data(&mut self) -> u8 {
        let i = usize::from(self.addr);
        let v = self.ram[i];
        self.bump_address();
        v
    }

    /// `$4800-$4FFF` write, with the auto-increment side effect.
    pub fn write_data(&mut self, val: u8) {
        let i = usize::from(self.addr);
        self.ram[i] = val;
        self.bump_address();
        // The channel registers ($40-$7F, including the enabled-channel count) live in this RAM.
        self.level_dirty = true;
    }

    /// Auto-increment stops at `$7F`; it does not wrap (NESdev correction to the older docs).
    fn bump_address(&mut self) {
        if self.auto_increment && self.addr < 0x7F {
            self.addr += 1;
        }
    }

    /// The internal RAM, for the board's save state and tests.
    pub fn ram(&self) -> &[u8; N163_RAM_LEN] {
        &self.ram
    }

    /// The chip RAM with the phase counters [`Namco163Audio::update_channel`] rewrites on its own
    /// (bytes 1, 3 and 5 of each channel block) blanked. The battery save still carries the
    /// real bytes; the autosave compares this copy so a running chip cannot make every save look
    /// changed.
    pub fn ram_for_change_check(&self) -> [u8; N163_RAM_LEN] {
        let mut ram = self.ram;
        for ch in 0..8 {
            let base = CHANNEL_BASE + ch * 8;
            ram[base + 1] = 0;
            ram[base + 3] = 0;
            ram[base + 5] = 0;
        }
        ram
    }

    pub fn load_ram(&mut self, ram: &[u8; N163_RAM_LEN]) {
        self.ram = *ram;
        self.level_dirty = true;
    }

    /// `$E000` bit 6: mute (the generators keep running).
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
        self.level_dirty = true;
    }

    /// Number of enabled channels, 1..=8 (register `$7F`, bits 4-6).
    fn enabled_channels(&self) -> usize {
        usize::from((self.ram[0x7F] >> 4) & 0x07) + 1
    }

    /// One channel update: add the frequency to the phase, fetch the sample, hold the level.
    fn update_channel(&mut self) {
        self.level_dirty = true;
        let count = self.enabled_channels();
        let channel = self.next_channel;
        self.next_channel = if channel <= 8 - count { 7 } else { channel - 1 };

        let base = CHANNEL_BASE + channel * 8;
        let mut phase = u32::from(self.ram[base + 1])
            | (u32::from(self.ram[base + 3]) << 8)
            | (u32::from(self.ram[base + 5]) << 16);
        let freq = u32::from(self.ram[base])
            | (u32::from(self.ram[base + 2]) << 8)
            | (u32::from(self.ram[base + 4] & 0x03) << 16);
        // Wave length = 256 - (L * 4) 4-bit samples, i.e. the top six bits of $7C inverted.
        let length = 256 - u32::from(self.ram[base + 4] & 0xFC);
        let volume = i16::from(self.ram[base + 7] & 0x0F);
        let offset = u32::from(self.ram[base + 6]);

        let span = length << 16;
        phase = (phase + freq) % span;
        self.ram[base + 1] = phase as u8;
        self.ram[base + 3] = (phase >> 8) as u8;
        self.ram[base + 5] = (phase >> 16) as u8;

        let index = ((phase >> 16) + offset) & 0xFF;
        let sample = (self.ram[(index >> 1) as usize] >> ((index & 1) * 4)) & 0x0F;
        self.levels[channel] = (i16::from(sample) - 8) * volume;
    }

    /// Sum the held channel levels, over the enabled channels only.
    fn analog_sum(&self) -> f32 {
        if self.disabled {
            return 0.0;
        }
        let count = self.enabled_channels();
        let first = 8 - count;
        let mut sum: f32 = 0.0;
        for level in self.levels.iter().skip(first) {
            sum += f32::from(*level);
        }
        // Full scale is one channel at sample 15, volume 15 (+105) or sample 0 (-120).
        (sum / (count as f32 * 120.0)) * self.scale
    }
}

impl Namco163Audio {
    /// One CPU cycle: the address latch, the channel schedule and the cached level.
    pub fn clock(&mut self) {
        if let Some((addr, inc)) = self.pending.take() {
            self.addr = addr;
            self.auto_increment = inc;
        }
        self.schedule += 1;
        if self.schedule >= CHANNEL_PERIOD {
            self.schedule = 0;
            self.update_channel();
        }
        // the sum only moves when a channel update, a RAM write or a mute/unmute moved it, so
        // it is rebuilt at most once every `CHANNEL_PERIOD` cycles instead of on every cycle.
        if self.level_dirty {
            self.level = self.analog_sum();
            self.level_dirty = false;
        }
    }

    /// The chip's current contribution, in the 2A03 mixer's scale.
    pub fn output(&self) -> f32 {
        self.level
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.ram);
        w.u8(self.addr);
        w.bool(self.auto_increment);
        match self.pending {
            Some((addr, inc)) => {
                w.bool(true);
                w.u8(addr);
                w.bool(inc);
            }
            None => w.bool(false),
        }
        w.u8(self.schedule);
        w.u8(u8::try_from(self.next_channel).unwrap_or(0));
        for l in self.levels {
            w.u16(l as u16);
        }
        w.bool(self.disabled);
        w.f32(self.scale);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        r.bytes_into(&mut self.ram)?;
        self.addr = r.u8()?;
        // the address ports index the 128-byte RAM, so a damaged file must fail here.
        if self.addr > 0x7F {
            return Err(StateError::BadValue);
        }
        self.auto_increment = r.bool()?;
        self.pending = if r.bool()? {
            let addr = r.u8()?;
            // The latch installs into `self.addr` on the next clock, so it needs the same range.
            if addr > 0x7F {
                return Err(StateError::BadValue);
            }
            Some((addr, r.bool()?))
        } else {
            None
        };
        self.schedule = r.u8()?;
        // The next `clock` adds one to it, so a value at or past the period would overflow (and a
        // value past 15 is not reachable from `clock`).
        if self.schedule >= CHANNEL_PERIOD {
            return Err(StateError::BadValue);
        }
        self.next_channel = usize::from(r.u8()?);
        // `next_channel` indexes `levels` and the channel registers in RAM.
        if self.next_channel > 7 {
            return Err(StateError::BadValue);
        }
        for l in &mut self.levels {
            *l = r.u16()? as i16;
        }
        self.disabled = r.bool()?;
        self.scale = r.f32()?;
        // `level` is derived from the channel registers, so it is not in the file; rebuild
        // it here, exactly as the next clock would. `level_dirty` was never stored either, so a
        // state saved right after a RAM write used to come back with that write's stale level.
        self.level = self.analog_sum();
        self.level_dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one chip-RAM byte through the address/data ports, the way a game uploads a wavetable.
    fn poke(chip: &mut Namco163Audio, addr: u8, val: u8) {
        chip.write_address(addr);
        chip.clock();
        chip.write_data(val);
        chip.clock();
    }

    /// Enable `count` channels and give channel 8 a volume.
    fn enable(chip: &mut Namco163Audio, count: usize, volume: u8) {
        let c = u8::try_from(count - 1).unwrap();
        poke(chip, 0x7F, (c << 4) | volume);
    }

    #[test]
    fn address_latch_installs_on_the_next_cycle() {
        let mut chip = Namco163Audio::new();
        chip.write_address(0x40);
        // Same cycle as the address write: the latch has not installed yet.
        chip.write_data(0x5A);
        assert_eq!(
            chip.ram()[0],
            0x5A,
            "the data port still points at the old address"
        );

        chip.clock();
        chip.write_data(0x77);
        assert_eq!(chip.ram()[0x40], 0x77);
    }

    #[test]
    fn auto_increment_stops_at_the_end_of_ram() {
        let mut chip = Namco163Audio::new();
        chip.write_address(0x80 | 0x7E);
        chip.clock();
        chip.write_data(0x11);
        chip.write_data(0x22);
        chip.write_data(0x33);
        assert_eq!(chip.ram()[0x7E], 0x11);
        assert_eq!(
            chip.ram()[0x7F],
            0x33,
            "the address stays at $7F, so the last write wins"
        );
        assert_eq!(chip.addr, 0x7F);
    }

    #[test]
    fn auto_increment_reads_walk_the_table() {
        let mut chip = Namco163Audio::new();
        poke(&mut chip, 0x20, 0xAB);
        poke(&mut chip, 0x21, 0xCD);
        chip.write_address(0x80 | 0x20);
        chip.clock();
        assert_eq!(chip.read_data(), 0xAB);
        assert_eq!(chip.read_data(), 0xCD);
    }

    #[test]
    fn one_channel_is_updated_every_fifteen_cycles() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 8, 0);
        chip.schedule = 0;
        chip.next_channel = 7;
        let mut updates = 0;
        let mut last = chip.next_channel;
        for _ in 0..(15 * 8) {
            chip.clock();
            if chip.next_channel != last {
                updates += 1;
                last = chip.next_channel;
            }
        }
        assert!(
            (7..=8).contains(&updates),
            "eight updates in 120 cycles, got {updates}"
        );
        assert_eq!(chip.schedule, 0);
    }

    #[test]
    fn phase_advances_by_the_frequency_each_update() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 1, 0);
        poke(&mut chip, 0x78, 0x00); // frequency low
        poke(&mut chip, 0x7A, 0x01); // frequency middle = 1 -> 0x0100 per update
        for _ in 0..15 {
            chip.clock();
        }
        let phase = u32::from(chip.ram()[0x79])
            | (u32::from(chip.ram()[0x7B]) << 8)
            | (u32::from(chip.ram()[0x7D]) << 16);
        assert_eq!(phase, 0x0100, "phase = frequency after the first update");
    }

    #[test]
    fn sample_is_biased_by_eight_and_scaled_by_volume() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 1, 15);
        // Waveform at $00: two 4-bit samples per byte, low nibble first. Sample 0 = $0F.
        poke(&mut chip, 0x00, 0x0F);
        poke(&mut chip, 0x7E, 0x00); // wave address
        for _ in 0..16 {
            chip.clock();
        }
        // sample 0 = $0F -> (15 - 8) * 15 = 105 -> 105 / (1 * 120) * scale
        let expected = 105.0 / 120.0 * N163_AUDIO_SCALE;
        assert!(
            (chip.output() - expected).abs() < 1e-6,
            "got {}",
            chip.output()
        );
    }

    #[test]
    fn only_enabled_channels_are_summed_and_scaled_by_the_count() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 1, 15);
        poke(&mut chip, 0x00, 0xFF);
        poke(&mut chip, 0x7E, 0x00);
        for _ in 0..15 {
            chip.clock();
        }
        let one = chip.output();
        // Enabling a second channel (channel 7, volume 0) halves the per-channel gain; the silent
        // channel contributes nothing, so the level halves.
        enable(&mut chip, 2, 15);
        for _ in 0..30 {
            chip.clock();
        }
        assert!(
            (chip.output() - one / 2.0).abs() < 0.02,
            "one {} two {}",
            one,
            chip.output()
        );
    }

    #[test]
    fn sound_disable_mutes_without_stopping_the_generator() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 1, 15);
        poke(&mut chip, 0x00, 0xFF);
        poke(&mut chip, 0x7E, 0x00);
        for _ in 0..15 {
            chip.clock();
        }
        assert!(chip.output() > 0.0);
        chip.set_disabled(true);
        chip.clock();
        assert_eq!(chip.output(), 0.0);
        chip.set_disabled(false);
        chip.clock();
        assert!(chip.output() > 0.0);
    }

    #[test]
    fn submapper_picks_the_mixing_level() {
        let mut quiet = Namco163Audio::new();
        let mut loud = Namco163Audio::new();
        loud.set_submapper(5);
        assert!(loud.scale > quiet.scale);
        quiet.set_submapper(2);
        assert_eq!(quiet.scale, N163_AUDIO_SCALE);
    }

    #[test]
    fn state_round_trips() {
        let mut a = Namco163Audio::new();
        enable(&mut a, 4, 12);
        for i in 0..64u8 {
            poke(&mut a, i, i.wrapping_mul(7));
        }
        for _ in 0..500 {
            a.clock();
        }
        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Namco163Audio::new();
        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        for _ in 0..500 {
            a.clock();
            b.clock();
            assert_eq!(a.output(), b.output());
        }
    }

    /// `level` is a cache of `analog_sum`, so after every clock it must be bit-identical to a
    /// fresh sum — that is what pins "the skipped recomputes were pure".
    #[test]
    fn the_cached_level_is_a_fresh_sum_after_every_clock() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 4, 15);
        for i in 0..64u8 {
            poke(&mut chip, i, i.wrapping_mul(11));
        }
        for cycle in 0..2000 {
            chip.clock();
            assert_eq!(
                chip.output().to_bits(),
                chip.analog_sum().to_bits(),
                "cycle {cycle}"
            );
        }
    }

    /// A mute, a submapper change and a RAM write all reach the level on the next clock, exactly as
    /// they did when it was rebuilt unconditionally.
    #[test]
    fn a_dirty_input_reaches_the_level_on_the_next_clock() {
        let mut chip = Namco163Audio::new();
        enable(&mut chip, 1, 15);
        poke(&mut chip, 0x00, 0xFF);
        for _ in 0..15 {
            chip.clock();
        }
        assert!(chip.output() > 0.0);

        chip.set_disabled(true);
        assert!(
            chip.output() > 0.0,
            "the mute is not visible until the clock"
        );
        chip.clock();
        assert_eq!(chip.output(), 0.0);

        chip.set_disabled(false);
        chip.clock();
        let quiet = chip.output();
        assert!(quiet > 0.0, "the channel resumes");
        chip.set_submapper(5);
        chip.clock();
        assert!(chip.output() > quiet, "the submapper's scale is picked up");
        assert_eq!(chip.output().to_bits(), chip.analog_sum().to_bits());
    }

    /// `addr` and `next_channel` index the 128-byte RAM and the eight channel slots, and the
    /// pending latch installs itself into `addr` on the next clock. A state carrying any of them
    /// out of range used to load cleanly and panic in `read_data`/`update_channel`.
    #[test]
    fn out_of_range_state_fields_are_a_bad_value() {
        fn rejects(mutate: fn(&mut Namco163Audio), name: &str) {
            let mut chip = Namco163Audio::new();
            mutate(&mut chip);
            let mut w = StateWriter::new();
            chip.save_state(&mut w);
            let bytes = w.finish();
            assert_eq!(
                Namco163Audio::new().load_state(&mut StateReader::new(&bytes)),
                Err(StateError::BadValue),
                "{name} must be rejected on load"
            );
        }
        rejects(|c| c.addr = 0x80, "addr $80");
        rejects(|c| c.pending = Some((0x80, false)), "pending addr $80");
        rejects(|c| c.next_channel = 8, "next_channel 8");
        rejects(|c| c.schedule = CHANNEL_PERIOD, "schedule past the period");
    }

    /// `level` is derived and no longer saved. A state taken right after the mixing scale
    /// changed (the cache is one clock behind) must load with the level the loaded state implies.
    #[test]
    fn a_loaded_level_is_rebuilt_from_the_loaded_state() {
        let mut a = Namco163Audio::new();
        enable(&mut a, 4, 12);
        for i in 0..64u8 {
            poke(&mut a, i, i.wrapping_mul(7));
        }
        for _ in 0..500 {
            a.clock();
        }
        let stale = a.output();
        a.set_submapper(5); // a louder mixing resistor; `a.level` is stale until the next clock
        assert_eq!(a.output().to_bits(), stale.to_bits());

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let bytes = w.finish();
        let mut b = Namco163Audio::new();
        b.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(
            b.output().to_bits(),
            b.analog_sum().to_bits(),
            "the loaded level is the loaded sum"
        );
        assert_ne!(
            b.output().to_bits(),
            stale.to_bits(),
            "the stale level must not come back"
        );
        a.clock();
        b.clock();
        assert_eq!(a.output().to_bits(), b.output().to_bits());
    }
}
