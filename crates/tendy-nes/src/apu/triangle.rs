use super::units::LengthCounter;

const SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

/// The triangle channel: a 32-step sequence gated by a linear counter and a length counter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Triangle {
    pub timer_period: u16,
    pub timer: u16,
    pub seq_pos: u8,
    pub length: LengthCounter,
    /// Control flag: halts the length counter and keeps the linear counter reloading.
    pub control: bool,
    pub linear_reload_value: u8,
    pub linear_counter: u8,
    pub linear_reload: bool,
    /// The level the channel reports while its output is ultrasonic and
    /// [`AudioOptions::silence_ultrasonic_triangle`](super::AudioOptions::silence_ultrasonic_triangle)
    /// is on: the last level it played at a timer period of 2 or more. Tracked every clock while
    /// the option is on and never read while it is off. It is the DAC's held level, not emulated
    /// state, so a save state neither stores nor restores it.
    held: u8,
}

impl Triangle {
    /// Register write; `reg` is 0..=3 ($4008, $4009 unused, $400A, $400B).
    pub fn write(&mut self, reg: u16, val: u8) {
        match reg & 3 {
            0 => {
                self.control = val & 0x80 != 0;
                self.length.halt = self.control;
                self.linear_reload_value = val & 0x7F;
            }
            1 => {}
            2 => self.timer_period = (self.timer_period & 0x0700) | u16::from(val),
            _ => {
                self.timer_period = (self.timer_period & 0x00FF) | (u16::from(val & 0x07) << 8);
                self.length.load(val >> 3);
                self.linear_reload = true;
            }
        }
    }

    /// Clocked every CPU cycle; the sequence only advances while both counters are non-zero.
    ///
    /// `silence_ultrasonic` is [`Apu::options`](super::Apu::options)'s triangle switch. With it on
    /// the channel remembers the last level it played at a timer period of 2 or more, so
    /// [`Triangle::output`] can hold it while the timer runs faster than the DAC. The sequencer
    /// above runs identically either way: the option changes what is heard, never the state.
    pub fn clock_timer(&mut self, silence_ultrasonic: bool) {
        if self.timer == 0 {
            self.timer = self.timer_period;
            if self.length.active() && self.linear_counter > 0 {
                self.seq_pos = (self.seq_pos + 1) & 31;
            }
        } else {
            self.timer -= 1;
        }
        // An audible period: this step is what the DAC is playing, so it is the level an
        // ultrasonic period has to hold. A period below 2 never updates it — that is the freeze.
        if silence_ultrasonic && self.timer_period >= 2 {
            self.held = SEQUENCE[usize::from(self.seq_pos)];
        }
    }

    /// Quarter-frame clock: linear counter.
    pub fn clock_quarter(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control {
            self.linear_reload = false;
        }
    }

    /// Half-frame clock: length counter.
    pub fn clock_half(&mut self) {
        self.length.clock();
    }

    /// The DAC level this cycle.
    ///
    /// With `silence_ultrasonic` on and a timer period below 2, the level is the one the channel
    /// last played ([`Triangle::held`]) rather than a sequence step at 1.79 MHz: the DAC cannot
    /// follow that, so on hardware the output stops moving. Off, this is the raw sequencer step,
    /// the pre-fix expression byte for byte.
    pub fn output(&self, silence_ultrasonic: bool) -> u8 {
        if silence_ultrasonic && self.timer_period < 2 {
            self.held
        } else {
            SEQUENCE[usize::from(self.seq_pos)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A triangle with its length counter enabled and loaded (index 1), linear reload value 5, timer period 2.
    fn playing(control: bool) -> Triangle {
        let mut t = Triangle::default();
        t.length.set_enabled(true);
        t.write(0, if control { 0x85 } else { 0x05 });
        t.write(2, 0x02);
        t.write(3, 0x08);
        t
    }

    #[test]
    fn holds_until_linear_counter_loaded() {
        let mut t = playing(false);

        for _ in 0..12 {
            t.clock_timer(false);
        }

        assert_eq!(t.output(false), 15);
    }

    #[test]
    fn steps_down_the_sequence() {
        let mut t = playing(false);
        t.clock_quarter();

        let mut outputs = Vec::new();

        for _ in 0..4 {
            for _ in 0..3 {
                t.clock_timer(false);
            }
            outputs.push(t.output(false));
        }

        assert_eq!(outputs, vec![14, 13, 12, 11]);
    }

    #[test]
    fn linear_counter_reloads_then_counts_down() {
        let mut t = playing(false);

        t.clock_quarter();
        assert_eq!(t.linear_counter, 5);
        assert!(!t.linear_reload);

        for _ in 0..5 {
            t.clock_quarter();
        }

        assert_eq!(t.linear_counter, 0);
    }

    #[test]
    fn control_keeps_reloading() {
        let mut t = playing(true);

        t.clock_quarter();
        t.clock_quarter();

        assert_eq!(t.linear_counter, 5);
        assert!(t.linear_reload);
    }

    #[test]
    fn control_halts_length() {
        let mut t = playing(true);

        t.clock_half();

        assert_eq!(t.length.value, 254);
    }

    #[test]
    fn length_counts_without_control() {
        let mut t = playing(false);

        t.clock_half();

        assert_eq!(t.length.value, 253);
    }

    /// The switch is the only thing the hold changes: the same triangle clocked with it on and
    /// off ends in the same sequencer position, timer, counters and length.
    #[test]
    fn the_hold_leaves_the_sequencer_identical() {
        let mut on = playing(true);
        let mut off = playing(true);
        on.write(2, 0x00);
        off.write(2, 0x00);
        // One quarter frame each, so both linear counters are loaded and the sequence runs.
        on.clock_quarter();
        off.clock_quarter();

        for cycle in 0..4096 {
            on.clock_timer(true);
            off.clock_timer(false);
            assert_eq!(on.seq_pos, off.seq_pos, "seq_pos at cycle {cycle}");
            assert_eq!(on.timer, off.timer, "timer at cycle {cycle}");
            assert_eq!(on.linear_counter, off.linear_counter, "cycle {cycle}");
            assert_eq!(on.length.value, off.length.value, "cycle {cycle}");
        }
        assert_eq!(on.timer_period, off.timer_period);
    }

    /// A period-0 triangle from power-on reports one constant level with the option on, and the
    /// raw sequencer steps — a 1.79 MHz square — with it off. `held` starts at 0, the DAC's level
    /// before any register write, so a channel that was never audible holds silence.
    #[test]
    fn the_ultrasonic_hold_is_one_level() {
        let mut t = playing(false);
        t.clock_quarter();
        t.write(2, 0x00);

        let mut held = Vec::new();
        let mut raw = Vec::new();
        for _ in 0..1024 {
            t.clock_timer(true);
            held.push(t.output(true));
            raw.push(t.output(false));
        }

        assert!(held.iter().all(|&level| level == 0), "held level {held:?}");
        assert!(
            raw.contains(&15),
            "the raw output reaches the top of the sequence"
        );
        assert!(
            raw.iter().any(|&level| level != raw[0]),
            "the raw output moves: {raw:?}"
        );
    }

    /// The hold freezes the level the note was playing when the period dropped below 2, so a game
    /// that writes period 0 mid-note keeps its DAC where it was instead of jumping to silence.
    #[test]
    fn the_hold_freezes_the_last_audible_level() {
        let mut t = playing(true);
        t.clock_quarter();
        for _ in 0..64 {
            t.clock_timer(true);
        }
        let playing_level = t.output(true);
        assert!(playing_level > 0, "the note is playing");

        t.write(2, 0x00);
        let mut levels = std::collections::BTreeSet::new();
        for _ in 0..1024 {
            t.clock_timer(true);
            levels.insert(t.output(true));
        }

        assert_eq!(levels.into_iter().collect::<Vec<_>>(), vec![playing_level]);
    }
}
