/// DMC timer periods in CPU cycles (NTSC), indexed by the low 4 bits of $4010.
pub const DMC_RATES: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

/// PAL (2A07) DMC timer periods in CPU cycles.
pub const DMC_RATES_PAL: [u16; 16] = [
    398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50,
];

/// The largest `$4011` step `--reduce-dmc-pop` leaves alone, in DAC counts. A direct load
/// that moves the level by more than this keeps only half of the move; anything smaller is the
/// console's own step.
pub const DMC_POP_THRESHOLD: i16 = 50;

/// The rate table a region's DMC uses.
pub fn rates(pal: bool) -> &'static [u16; 16] {
    if pal { &DMC_RATES_PAL } else { &DMC_RATES }
}

/// Why a DMC DMA request was raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaKind {
    /// $4015 enable with an empty buffer (after the load delay).
    Load,
    /// The output unit consumed the sample buffer.
    Reload,
    /// Explicit-stop bug: a reload DMA that starts but is aborted after its halt cycle (1 cycle stolen).
    Abort,
}

/// The delta modulation channel: plays 1-bit delta samples read from CPU memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dmc {
    pub irq_enabled: bool,
    pub looping: bool,
    pub timer_period: u16,
    pub timer: u16,
    /// 7-bit output level.
    pub output: u8,
    pub sample_addr: u16,
    pub sample_len: u16,
    pub current_addr: u16,
    pub bytes_remaining: u16,
    /// Sample buffer; None when empty (the system then fetches the next byte).
    pub buffer: Option<u8>,
    pub shift: u8,
    pub bits_remaining: u8,
    pub silence: bool,
    pub irq: bool,
    /// CPU cycles until a $4015-triggered load DMA request becomes visible (0 = none pending).
    pub load_delay: u8,
    /// True when the timer reloaded on the most recent CPU cycle (a same-cycle $4010 write then wins).
    pub just_reloaded: bool,
    /// Latched DMA request. Raised by APU events, cleared when serviced (`fill`) or cancelled ($4015 disable).
    pub dma_req: Option<DmaKind>,
    /// $4015 disable one or two cycles before a reload would schedule: CPU cycles until the aborted DMA is requested.
    pub abort_countdown: Option<u8>,
    /// PAL: index `DMC_RATES_PAL` instead of the NTSC table.
    pub pal: bool,
}

impl Default for Dmc {
    fn default() -> Self {
        Dmc {
            irq_enabled: false,
            looping: false,
            timer_period: DMC_RATES[0],
            timer: 0,
            output: 0,
            sample_addr: 0xC000,
            sample_len: 1,
            current_addr: 0xC000,
            bytes_remaining: 0,
            buffer: None,
            shift: 0,
            bits_remaining: 8,
            silence: true,
            irq: false,
            load_delay: 0,
            just_reloaded: false,
            dma_req: None,
            abort_countdown: None,
            pal: false,
        }
    }
}

impl Dmc {
    /// Register write; `reg` is 0..=3 ($4010-$4013).
    ///
    /// `reduce_pop` is the host-side `--reduce-dmc-pop` option. It softens a large `$4011`
    /// direct load — the jump a game makes when it patches the DAC mid-sample — and leaves every
    /// other register alone; off, the default, every write is the console's own.
    pub fn write(&mut self, reg: u16, val: u8, reduce_pop: bool) {
        match reg & 3 {
            0 => {
                self.irq_enabled = val & 0x80 != 0;
                if !self.irq_enabled {
                    self.irq = false;
                }
                self.looping = val & 0x40 != 0;
                self.timer_period = rates(self.pal)[usize::from(val & 0x0F)];
                if self.just_reloaded {
                    // The write landed on the same cycle as the reload: the timer takes the new period.
                    self.timer = self.timer_period - 1;
                }
            }
            1 => {
                let previous = self.output;
                self.output = val & 0x7F;
                if reduce_pop {
                    // Mesen's rule (`ReduceDmcPopping`): a direct load that moves the level by
                    // more than `DMC_POP_THRESHOLD` keeps half of the move, so the click a
                    // mid-sample patch makes is halved instead of removed. The write still
                    // applies at once, like the console's, so the softened level *is* the
                    // channel's level — a save state taken while the option is on stores it.
                    // Integer division truncates toward zero, as Mesen's does: 0 -> 127 lands on
                    // 64 and 127 -> 0 on 63.
                    let delta = i16::from(self.output) - i16::from(previous);
                    if delta.abs() > DMC_POP_THRESHOLD {
                        self.output = (i16::from(self.output) - delta / 2) as u8;
                    }
                }
            }
            2 => self.sample_addr = 0xC000 + u16::from(val) * 64,
            _ => self.sample_len = u16::from(val) * 16 + 1,
        }
    }

    fn restart(&mut self) {
        self.current_addr = self.sample_addr;
        self.bytes_remaining = self.sample_len;
    }

    /// $4015 bit 4: enabling restarts a finished sample; disabling stops it. Either way clears the DMC IRQ.
    pub fn set_enabled(&mut self, on: bool, odd_cycle: bool) {
        self.irq = false;
        self.abort_countdown = None;
        if !on {
            // A reload that the DMC already scheduled on this very cycle (or whose halt is waiting on a write) still runs.
            // One that would schedule within the next two cycles starts but aborts after its halt cycle.
            if self.dma_req != Some(DmaKind::Reload) {
                self.dma_req = None;
                if self.bytes_remaining > 0 {
                    self.arm_abort();
                }
            }
            self.bytes_remaining = 0;
            self.load_delay = 0;
        } else if self.bytes_remaining == 0 {
            self.restart();
            // Load DMA is not requested immediately: 3 cycles after an odd-cycle write, else 2.
            self.load_delay = if odd_cycle { 3 } else { 2 };
        }
    }

    /// The sample has just run out (counter about to reach 0) with a byte waiting in the buffer. If the output unit
    /// would empty the buffer within the next two cycles, its reload DMA still starts but is aborted after the halt cycle.
    fn arm_abort(&mut self) {
        if self.bits_remaining == 1
            && self.buffer.is_some()
            && self.load_delay == 0
            && self.timer <= 1
        {
            self.abort_countdown = Some(self.timer as u8);
        }
    }

    /// The address the DMC wants to read now: its buffer is empty, bytes remain and no load delay is pending.
    pub fn fetch_addr(&self) -> Option<u16> {
        if self.load_delay == 0 && self.buffer.is_none() && self.bytes_remaining > 0 {
            Some(self.current_addr)
        } else {
            None
        }
    }

    /// Deliver the byte read from `fetch_addr()`; at the end of the sample loop or raise the IRQ.
    pub fn fill(&mut self, byte: u8) {
        self.buffer = Some(byte);
        self.dma_req = None;
        self.current_addr = if self.current_addr == 0xFFFF {
            0x8000
        } else {
            self.current_addr + 1
        };
        self.bytes_remaining -= 1;
        if self.bytes_remaining == 0 {
            if self.looping {
                self.restart();
            } else {
                if self.irq_enabled {
                    self.irq = true;
                }
                // Implicit stop: the last byte was fetched just before the reload DMA would schedule.
                self.arm_abort();
            }
        }
    }

    /// Clocked every CPU cycle (the rate table is in CPU cycles).
    pub fn clock_timer(&mut self) {
        self.just_reloaded = false;
        match self.abort_countdown {
            Some(0) => {
                self.abort_countdown = None;
                self.dma_req = Some(DmaKind::Abort);
            }
            Some(n) => self.abort_countdown = Some(n - 1),
            None => {}
        }
        if self.load_delay > 0 {
            self.load_delay -= 1;
            if self.load_delay == 0 && self.buffer.is_none() && self.bytes_remaining > 0 {
                self.dma_req = Some(DmaKind::Load);
            }
        }
        if self.timer > 0 {
            self.timer -= 1;
            return;
        }
        self.timer = self.timer_period - 1;
        self.just_reloaded = true;
        if !self.silence {
            if self.shift & 1 != 0 {
                if self.output <= 125 {
                    self.output += 2;
                }
            } else if self.output >= 2 {
                self.output -= 2;
            }
        }
        self.shift >>= 1;
        self.bits_remaining -= 1;
        if self.bits_remaining == 0 {
            self.bits_remaining = 8;
            match self.buffer.take() {
                Some(b) => {
                    if self.dma_req.is_none() && self.bytes_remaining > 0 && self.load_delay == 0 {
                        self.dma_req = Some(DmaKind::Reload);
                    }
                    self.silence = false;
                    self.shift = b;
                }
                None => self.silence = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_and_length_registers() {
        let mut d = Dmc::default();
        d.write(2, 0x01, false);
        d.write(3, 0x01, false);
        assert_eq!(d.sample_addr, 0xC040);
        assert_eq!(d.sample_len, 17);
    }

    #[test]
    fn enable_starts_sample() {
        let mut d = Dmc::default();
        d.write(2, 0x01, false);
        d.write(3, 0x01, false);
        d.set_enabled(true, false);
        assert_eq!(d.fetch_addr(), None);
        d.clock_timer();
        d.clock_timer();
        assert_eq!(d.fetch_addr(), Some(0xC040));
        assert_eq!(d.dma_req, Some(DmaKind::Load));
        assert_eq!(d.bytes_remaining, 17);
    }

    #[test]
    fn no_fetch_while_buffer_full() {
        let mut d = Dmc::default();
        d.write(3, 0x01, false);
        d.set_enabled(true, false);
        d.load_delay = 0;
        d.fill(0xAA);
        assert_eq!(d.fetch_addr(), None);
        assert_eq!(d.bytes_remaining, 16);
    }

    #[test]
    fn irq_at_end_of_sample() {
        let mut d = Dmc::default();
        d.write(0, 0x80, false);
        d.write(3, 0x00, false);
        d.set_enabled(true, false);
        d.load_delay = 0;
        d.fill(0);
        assert!(d.irq);
        assert_eq!(d.bytes_remaining, 0);
        d.set_enabled(true, false);
        assert!(!d.irq);
    }

    #[test]
    fn no_irq_when_disabled() {
        let mut d = Dmc::default();
        d.write(3, 0x00, false);
        d.set_enabled(true, false);
        d.load_delay = 0;
        d.fill(0);
        assert!(!d.irq);
    }

    #[test]
    fn loop_restarts_sample() {
        let mut d = Dmc::default();
        d.write(0, 0x40, false);
        d.write(3, 0x00, false);
        d.set_enabled(true, false);
        d.load_delay = 0;
        d.fill(0);
        assert_eq!(d.bytes_remaining, 1);
        assert_eq!(d.current_addr, 0xC000);
    }

    #[test]
    fn address_wraps_to_8000() {
        let mut d = Dmc {
            current_addr: 0xFFFF,
            bytes_remaining: 2,
            ..Default::default()
        };
        d.fill(0);
        assert_eq!(d.current_addr, 0x8000);
    }

    #[test]
    fn direct_load_sets_output() {
        let mut d = Dmc::default();
        d.write(1, 0xFF, false);
        assert_eq!(d.output, 127);
    }

    /// Off — the default — every `$4011` value is the console's direct load, top bit and all.
    #[test]
    fn the_pop_option_off_is_the_console_direct_load() {
        for value in 0..=0x7F_u8 {
            let mut d = Dmc::default();
            d.write(1, value, false);
            assert_eq!(d.output, value, "$4011 = {value}");
        }
        let mut d = Dmc::default();
        d.write(1, 0xFF, false);
        assert_eq!(d.output, 0x7F, "the top bit is not part of the level");
    }

    /// Mesen's `ReduceDmcPopping`: a move of more than 50 counts keeps half of it; a
    /// smaller one, or exactly the threshold, is left alone.
    #[test]
    fn the_pop_option_softens_a_large_direct_load() {
        // 0 -> 127: 127 - 127/2 = 64.
        let mut d = Dmc::default();
        d.write(1, 0x7F, true);
        assert_eq!(d.output, 64);

        // 0 -> 40 is under the threshold, so the console's write stands.
        let mut d = Dmc::default();
        d.write(1, 40, true);
        assert_eq!(d.output, 40);

        // The threshold itself is untouched; one count past it is softened.
        let mut d = Dmc::default();
        d.write(1, 50, true);
        assert_eq!(d.output, 50);
        let mut d = Dmc::default();
        d.write(1, 51, true);
        assert_eq!(d.output, 26, "51 - 51/2");

        // Falling keeps half of the move too, truncating toward zero as Mesen's C++ does: the
        // 127 -> 0 jump lands on 63, not 64.
        let mut d = Dmc::default();
        d.write(1, 0x7F, false);
        d.write(1, 0x00, true);
        assert_eq!(d.output, 63);
    }

    /// The option reaches only `$4011`: the period, address and length registers write the same
    /// fields with it on, so a run that never touches the direct load is unchanged by it.
    #[test]
    fn the_pop_option_touches_only_the_output_level() {
        let mut on = Dmc::default();
        let mut off = Dmc::default();
        for (reg, val) in [(0u16, 0x4F), (2, 0x07), (3, 0x1F)] {
            on.write(reg, val, true);
            off.write(reg, val, false);
        }
        assert_eq!(on, off);
    }

    #[test]
    fn plays_deltas_lsb_first() {
        let mut d = Dmc::default();
        d.write(0, 0x0F, false);
        d.write(1, 0x40, false);
        d.buffer = Some(0b0000_0101);

        let mut outputs = Vec::new();
        for _ in 0..16 {
            for _ in 0..54 {
                d.clock_timer();
            }
            outputs.push(d.output);
        }

        assert_eq!(
            outputs,
            vec![
                64, 64, 64, 64, 64, 64, 64, 64, 66, 64, 66, 64, 62, 60, 58, 56
            ]
        );
    }
}
