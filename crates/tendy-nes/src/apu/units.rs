/// Length counter load values, indexed by the 5-bit value written to bits 3-7 of a channel's 4th register.
pub const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

/// Envelope generator shared by the pulse and noise channels (clocked by quarter frames).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Envelope {
    pub start: bool,
    pub looping: bool,
    pub constant: bool,
    /// Constant volume, or the envelope period.
    pub volume: u8,
    pub divider: u8,
    pub decay: u8,
}

impl Envelope {
    /// Bits 0-5 of the channel's first register: bit 5 loop (also length halt), bit 4 constant, bits 0-3 volume/period.
    pub fn write(&mut self, val: u8) {
        self.looping = val & 0x20 != 0;
        self.constant = val & 0x10 != 0;
        self.volume = val & 0x0F;
    }

    pub fn clock(&mut self) {
        if self.start {
            self.start = false;
            self.decay = 15;
            self.divider = self.volume;
        } else if self.divider == 0 {
            self.divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if self.looping {
                self.decay = 15;
            }
        } else {
            self.divider -= 1;
        }
    }

    pub fn output(&self) -> u8 {
        if self.constant {
            self.volume
        } else {
            self.decay
        }
    }
}

/// Length counter: silences a channel when it reaches 0 (clocked by half frames unless halted).
///
/// A halt or a load written on the cycle of a half-frame clock must not be seen by that clock (blargg
/// 10/11): the frame counter runs at the start of the APU clock, before that cycle's register write.
/// `Apu::clock` therefore snapshots the written state around the frame counter (`begin_clock` /
/// `end_clock`), and these two fields carry it across the boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LengthCounter {
    pub enabled: bool,
    pub halt: bool,
    pub value: u8,
    /// `halt` as it was at the end of the previous APU clock: what the next half-frame clock must use.
    pub snap_halt: bool,
    /// A load made since the last APU clock: (the value it wrote, the value before the load).
    pub pending: Option<(u8, u8)>,
}

impl LengthCounter {
    /// Load from the 5-bit index (only while the channel is enabled in $4015). The new value is
    /// visible to `$4015` reads at once, but the next half-frame clock still sees the old one.
    pub fn load(&mut self, index: u8) {
        if self.enabled {
            let new = LENGTH_TABLE[usize::from(index & 0x1F)];
            self.pending = Some((new, self.value));
            self.value = new;
        }
    }

    /// Whether a write since the last APU clock has to be hidden from the frame counter: a load, or a
    /// halt that differs from `snap_halt`. When it is false, `begin_clock` and `end_clock` change
    /// nothing.
    #[inline]
    pub fn written(&self) -> bool {
        self.pending.is_some() || self.halt != self.snap_halt
    }

    /// Start of an APU clock: swap in the state from before this cycle's write, for the frame counter.
    /// Returns the live `halt`, to be handed back to `end_clock`.
    #[inline]
    pub fn begin_clock(&mut self) -> bool {
        let cur = self.halt;
        self.halt = self.snap_halt;
        if let Some((_, prev)) = self.pending {
            self.value = prev;
        }
        cur
    }

    /// End of an APU clock: the write lands, but a reload is dropped when the clock just decremented
    /// the counter (the reload value only applies if the counter still holds what the load saw).
    #[inline]
    pub fn end_clock(&mut self, cur_halt: bool) {
        self.halt = cur_halt;
        if let Some((new, prev)) = self.pending.take()
            && self.value == prev
        {
            self.value = new;
        }
        self.snap_halt = self.halt;
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.value = 0;
            // A disable wins over any load written on the same cycle.
            self.pending = None;
        }
    }

    pub fn clock(&mut self) {
        if !self.halt && self.value > 0 {
            self.value -= 1;
        }
    }

    pub fn active(&self) -> bool {
        self.value > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_decays_every_period_plus_one() {
        let mut e = Envelope::default();
        e.write(0x02);
        e.start = true;

        let mut outputs = Vec::new();
        for _ in 0..8 {
            e.clock();
            outputs.push(e.output());
        }

        assert_eq!(outputs, vec![15, 15, 15, 14, 14, 14, 13, 13]);
    }

    #[test]
    fn envelope_loops_back_to_15() {
        let mut e = Envelope::default();
        e.write(0x20);
        e.start = true;

        let mut outputs = Vec::new();
        for _ in 0..18 {
            e.clock();
            outputs.push(e.output());
        }

        assert_eq!(
            outputs,
            vec![15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 15, 14]
        );
    }

    #[test]
    fn envelope_stops_at_zero_without_loop() {
        let mut e = Envelope::default();
        e.write(0x00);
        e.start = true;

        for _ in 0..20 {
            e.clock();
        }

        assert_eq!(e.output(), 0);
    }

    #[test]
    fn envelope_constant_volume() {
        let mut e = Envelope::default();
        e.write(0x17);
        e.start = true;
        e.clock();

        assert_eq!(e.output(), 7);
    }

    #[test]
    fn length_ignores_load_while_disabled() {
        let mut l = LengthCounter::default();
        l.load(1);

        assert_eq!(l.value, 0);
    }

    #[test]
    fn length_loads_from_table() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };

        l.load(1);
        assert_eq!(l.value, 254);

        l.load(0x1F);
        assert_eq!(l.value, 30);
    }

    #[test]
    fn length_counts_down_unless_halted() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };

        l.load(1);
        l.clock();
        assert_eq!(l.value, 253);

        l.halt = true;
        l.clock();
        assert_eq!(l.value, 253);
    }

    #[test]
    fn length_disable_clears() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };

        l.load(1);
        l.set_enabled(false);

        assert_eq!(l.value, 0);
        assert!(!l.active());
    }

    /// One full APU clock as `Apu::clock` runs it, with no half-frame event.
    fn idle_clock(l: &mut LengthCounter) {
        let cur = l.begin_clock();
        l.end_clock(cur);
    }

    /// A halt written on the cycle of a half-frame clock must not stop that clock (blargg 10).
    #[test]
    fn halt_written_before_the_clock_does_not_halt_that_clock() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };
        l.load(1);
        idle_clock(&mut l);
        assert_eq!(l.value, 254);

        // $4000 bit 5 written after this cycle's APU clock; the next one carries the half frame.
        l.halt = true;
        let cur = l.begin_clock();
        l.clock();
        l.end_clock(cur);

        assert_eq!(
            l.value, 253,
            "the clock that coincided with the halt write still counts"
        );
        // The halt is effective from the following clock on.
        idle_clock(&mut l);
        let cur = l.begin_clock();
        l.clock();
        l.end_clock(cur);
        assert_eq!(l.value, 253);
    }

    /// A reload written on the cycle of a half-frame clock still applies when the clock did not change
    /// the counter, i.e. when it was 0 (blargg 11).
    #[test]
    fn reload_at_zero_survives_the_coincident_clock() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };

        l.load(3); // 2
        let cur = l.begin_clock();
        l.clock(); // the clock still sees 0 and cannot decrement it
        l.end_clock(cur);

        assert_eq!(
            l.value, 2,
            "a reload during a clock that changed nothing applies"
        );
    }

    /// A reload written on the cycle of a half-frame clock is dropped when that clock decremented the
    /// counter (blargg 11: reload during the clock with a non-zero counter is ignored).
    #[test]
    fn reload_is_dropped_when_the_coincident_clock_decremented() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };
        l.load(1); // 254
        idle_clock(&mut l);
        assert_eq!(l.value, 254);

        l.load(0); // writes 10, but the clock below decrements the old 254
        let cur = l.begin_clock();
        l.clock();
        l.end_clock(cur);

        assert_eq!(
            l.value, 253,
            "the reload is ignored: the clock changed the counter"
        );
    }

    /// A `$4015` write that disables the channel drops a load written on the same cycle.
    #[test]
    fn disable_drops_a_pending_load() {
        let mut l = LengthCounter {
            enabled: true,
            ..Default::default()
        };

        l.load(1);
        l.set_enabled(false);
        idle_clock(&mut l);

        assert_eq!(l.value, 0);
    }
}
