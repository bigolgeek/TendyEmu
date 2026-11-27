//! The Game Boy Advance's four timers: prescalers, cascade, reload and overflow requests.
//!
//! GBATEK, *GBA Timers*, is the whole of the hardware. Each timer has a 16-bit reload/counter at
//! `$0400010n` and a control word at `$0400010n+2`:
//!
//! - bits 0-1 pick the prescaler off the 16 777 216 Hz clock: `/1`, `/64`, `/256`, `/1024`;
//! - bit 2 makes a timer count the *overflows of the timer below it* instead of the clock, which is
//!   the only way to build a 32- or 64-bit counter out of 16-bit ones ("the prescaler value is
//!   ignored ... this function cannot be used for Timer 0");
//! - bit 6 asks for an interrupt when the counter overflows;
//! - bit 7 starts and stops it.
//!
//! **Reload.** A write to the reload register does not touch the running counter; the reload value
//! is copied into the counter "automatically upon timer overflows, or when the timer start bit
//! becomes changed from 0 to 1" — and a read of the same address returns the *counter*, which is
//! why the two live in different fields here. One write does both jobs: a 32-bit store of
//! reload-and-control in one go has the reload land first and the start edge copy it, which is the
//! documented case "the newly written reload value is recognized as new counter value".
//!
//! **Where the state lives.** Unlike the DMA registers, a timer's counter is not a register
//! anywhere: it is live state that only the clock changes, so it is carried in [`Timers`] (and in
//! the save state) rather than in the I/O page.

/// How many timers the GBA has.
pub const TIMER_COUNT: usize = 4;

/// `TM0CNT_L`: the first timer's reload/counter. `$04000100`.
pub const TIMER_BASE: usize = 0x100;
/// The stride from one timer's registers to the next: `TMnCNT_L`, `TMnCNT_H`.
pub const TIMER_STRIDE: usize = 4;

/// Control bits (GBATEK, *4000102h - TMxCNT_H*).
pub const PRESCALER_MASK: u16 = 0b11;
/// Bit 2: count the timer below instead of the clock.
pub const CASCADE: u16 = 1 << 2;
/// Bit 6: request an interrupt on overflow.
pub const IRQ: u16 = 1 << 6;
/// Bit 7: start/stop.
pub const START: u16 = 1 << 7;
/// The bits a write can change: 0-2, 6 and 7 ("3-5 Not used", "8-15 Not used").
pub const CONTROL_MASK: u16 = PRESCALER_MASK | CASCADE | IRQ | START;

/// The prescaler divisors, in control bits 0-1 order: "0=F/1, 1=F/64, 2=F/256, 3=F/1024".
pub const PRESCALERS: [u32; TIMER_COUNT] = [1, 64, 256, 1024];

/// One timer: the two registers a store can set, the counter a read returns, and the cycles it has
/// accumulated towards its next prescaled tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timer {
    /// `TMxCNT_L` as written: copied into `counter` on an overflow or a start edge.
    pub reload: u16,
    /// `TMxCNT_L` as read: what the timer has counted to.
    pub counter: u16,
    /// `TMxCNT_H`, masked to the bits that exist.
    pub control: u16,
    /// Master cycles accumulated towards the next prescaled tick, always below the prescaler.
    pub sub: u32,
}

impl Timer {
    /// The divisor the control word selects.
    pub fn prescaler(&self) -> u32 {
        PRESCALERS[usize::from(self.control & PRESCALER_MASK)]
    }

    /// True while the start bit is set.
    pub fn running(&self) -> bool {
        self.control & START != 0
    }

    /// True when this timer counts the overflow of the timer below it, so the prescaler is ignored.
    pub fn cascaded(&self) -> bool {
        self.control & CASCADE != 0
    }

    /// True when an overflow asks for an interrupt.
    pub fn wants_irq(&self) -> bool {
        self.control & IRQ != 0
    }

    /// One store to `TMxCNT_L` or `TMxCNT_H`, from whichever byte of it was written.
    pub fn write8(&mut self, offset: usize, value: u8) {
        match offset & 3 {
            0 => self.reload = (self.reload & 0xFF00) | u16::from(value),
            1 => self.reload = (self.reload & 0x00FF) | (u16::from(value) << 8),
            // The halves of the control word arrive as separate stores, so only the byte being
            // written changes.
            2 => self.set_control((self.control & 0xFF00) | u16::from(value)),
            _ => self.set_control((self.control & 0x00FF) | (u16::from(value) << 8)),
        }
    }

    /// A store to the control word, with the start edge's side effect.
    fn set_control(&mut self, value: u16) {
        let was_running = self.running();
        self.control = value & CONTROL_MASK;
        // "The reload value is copied into the counter ... when the timer start bit becomes changed
        // from 0 to 1."
        if !was_running && self.running() {
            self.counter = self.reload;
            self.sub = 0;
        }
    }

    /// One byte of this timer's two registers, as a read of `TMxCNT_L`/`TMxCNT_H` returns it.
    pub fn read8(&self, offset: usize) -> u8 {
        match offset & 3 {
            0 => self.counter as u8,
            1 => (self.counter >> 8) as u8,
            2 => self.control as u8,
            _ => (self.control >> 8) as u8,
        }
    }

    /// Count `increments` clock ticks, returning how many times the counter overflowed.
    fn advance(&mut self, increments: u32) -> u32 {
        // The common case: no overflow, so one addition is enough.
        let room = u32::from(u16::MAX) - u32::from(self.counter);
        if increments <= room {
            self.counter += increments as u16;
            return 0;
        }
        // The first overflow happens when the counter passes `$FFFF`; after that the period is
        // `0x10000 - reload`, and the counter restarts at the reload value.
        let left = increments - room - 1;
        let period = 0x1_0000 - u32::from(self.reload);
        self.counter = self.reload.wrapping_add((left % period) as u16);
        1 + left / period
    }
}

/// The four timers, in the order the hardware chains them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timers {
    pub t: [Timer; TIMER_COUNT],
}

impl Timers {
    /// Four stopped timers with every register zero.
    pub fn new() -> Timers {
        Timers::default()
    }

    /// One byte of a timer register, `offset` being `$100`-`$10F`'s offset in the I/O page.
    pub fn read8(&self, offset: usize) -> u8 {
        let index = (offset - TIMER_BASE) / TIMER_STRIDE;
        self.t[index].read8(offset - TIMER_BASE - index * TIMER_STRIDE)
    }

    /// One store to a timer register; `offset` as in [`Timers::read8`].
    pub fn write8(&mut self, offset: usize, value: u8) {
        let index = (offset - TIMER_BASE) / TIMER_STRIDE;
        self.t[index].write8(offset - TIMER_BASE - index * TIMER_STRIDE, value);
    }

    /// True for an I/O offset that belongs to a timer.
    pub fn owns(offset: usize) -> bool {
        (TIMER_BASE..TIMER_BASE + TIMER_COUNT * TIMER_STRIDE).contains(&offset)
    }

    /// Advance every running timer `cycles` master cycles, returning the `IF` bits of the
    /// overflows that asked for an interrupt.
    pub fn tick(&mut self, cycles: u32) -> u16 {
        // How many times each timer is told to count this tick. A cascaded timer is told the
        // number of times the one below it overflowed, which is why this is computed in order.
        let mut increments = [0u32; TIMER_COUNT];
        for (index, timer) in self.t.iter_mut().enumerate() {
            if !timer.running() || timer.cascaded() {
                continue;
            }
            timer.sub += cycles;
            let prescaler = timer.prescaler();
            increments[index] = timer.sub / prescaler;
            timer.sub %= prescaler;
        }
        let mut flags = 0u16;
        for index in 0..TIMER_COUNT {
            let mut timer = self.t[index];
            if !timer.running() {
                continue;
            }
            let count = if timer.cascaded() {
                if index == 0 {
                    // GBATEK: "This function cannot be used for Timer 0"; a cascade bit there
                    // counts nothing.
                    continue;
                }
                increments[index - 1]
            } else {
                increments[index]
            };
            if count == 0 {
                continue;
            }
            let overflows = timer.advance(count);
            self.t[index] = timer;
            increments[index] = overflows;
            if overflows > 0 && timer.wants_irq() {
                flags |= 1 << (crate::irq::TIMER0 + index);
            }
        }
        flags
    }

    /// How many master cycles until the earliest overflow of a running timer that counts the clock.
    /// A halted CPU can sleep this long and no longer, because an overflow with its `IE` bit set
    /// wakes it.
    pub fn cycles_to_overflow(&self) -> Option<u32> {
        let mut soonest: Option<u32> = None;
        for timer in &self.t {
            if !timer.running() || timer.cascaded() {
                continue;
            }
            let ticks = u32::from(u16::MAX - timer.counter) + 1;
            let cycles = ticks * timer.prescaler() - timer.sub;
            soonest = Some(soonest.map_or(cycles, |best: u32| best.min(cycles)));
        }
        soonest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A timer started with `control`, its reload set to `reload`.
    fn started(reload: u16, control: u16) -> Timer {
        Timer {
            reload,
            counter: reload,
            control: control & CONTROL_MASK,
            sub: 0,
        }
    }

    #[test]
    fn the_prescaler_comes_from_the_low_two_bits_and_the_counter_starts_at_the_reload() {
        let timer = Timer {
            reload: 0x1234,
            counter: 0x0000,
            control: CONTROL_MASK,
            sub: 0,
        };
        assert_eq!(timer.prescaler(), 1024);
        assert!(timer.running() && timer.cascaded() && timer.wants_irq());
        for (bits, divisor) in PRESCALERS.iter().enumerate() {
            let timer = Timer {
                control: bits as u16,
                ..Timer::default()
            };
            assert_eq!(timer.prescaler(), *divisor);
        }
    }

    #[test]
    fn a_start_edge_copies_the_reload_into_the_counter() {
        let mut timer = Timer {
            reload: 0x00FF,
            ..Timer::default()
        };
        timer.write8(2, (START | 0b01) as u8);
        assert!(timer.running());
        assert_eq!(
            timer.counter, 0x00FF,
            "the reload was copied on the start edge"
        );
        // Ticking at /64 does not move it until 64 cycles have gone by.
        let mut timers = Timers::new();
        timers.t[0] = timer;
        assert_eq!(timers.tick(63), 0);
        assert_eq!(timers.t[0].counter, 0x00FF);
        timers.tick(1);
        assert_eq!(timers.t[0].counter, 0x0100);
    }

    #[test]
    fn an_overflow_reloads_and_asks_for_an_interrupt_only_when_bit_six_is_set() {
        let mut timers = Timers::new();
        timers.t[1] = started(0xFFFE, START);
        assert_eq!(timers.tick(1), 0, "no IRQ bit: the overflow is silent");
        assert_eq!(timers.t[1].counter, 0xFFFE + 1);
        assert_eq!(timers.tick(1), 0);
        assert_eq!(timers.t[1].counter, 0xFFFE, "reloaded");

        let mut timers = Timers::new();
        timers.t[0] = started(0xFFFF, START | IRQ);
        assert_eq!(
            timers.tick(1),
            1 << crate::irq::TIMER0,
            "the timer 0 request bit"
        );
        assert_eq!(
            timers.t[0].counter, 0xFFFF,
            "0xFFFF + 1 wraps to its reload"
        );
    }

    #[test]
    fn a_stopped_timer_does_not_count_and_keeps_its_value() {
        let mut timers = Timers::new();
        timers.t[2] = started(0x0010, 0);
        assert_eq!(timers.tick(1000), 0);
        assert_eq!(timers.t[2].counter, 0x0010, "frozen where it started");
        assert_eq!(timers.cycles_to_overflow(), None, "nothing is running");
    }

    #[test]
    fn a_cascaded_timer_counts_the_overflows_of_the_one_below_it() {
        // Timer 0 runs at /1 and wraps every 4 cycles (reload $FFFC); timer 1 counts those
        // overflows rather than the clock.
        let mut timers = Timers::new();
        timers.t[0] = started(0xFFFC, START);
        timers.t[1] = started(0, START | CASCADE);
        // 16 increments at /1: 3 to reach $FFFF, then 12 more at four an overflow.
        timers.tick(16);
        assert_eq!(
            timers.t[0].counter, 0xFFFC,
            "timer 0 wrapped back to its reload"
        );
        assert_eq!(
            timers.t[1].counter, 4,
            "timer 1 counted timer 0's four overflows"
        );
        // And the cascade ignores timer 1's own prescaler bits.
        timers.t[1].control = START | CASCADE | 0b11;
        timers.tick(4);
        assert_eq!(timers.t[1].counter, 5);
    }

    #[test]
    fn timer_zero_ignores_its_cascade_bit() {
        let mut timers = Timers::new();
        timers.t[0] = started(0, START | CASCADE);
        assert_eq!(timers.tick(1000), 0);
        assert_eq!(timers.t[0].counter, 0, "nothing below timer 0 to count");
    }

    #[test]
    fn the_sleep_distance_is_the_next_overflow_of_a_running_timer() {
        let mut timers = Timers::new();
        timers.t[3] = started(0xFF00, START | 0b11); // /1024
        assert_eq!(timers.t[3].counter, 0xFF00);
        // 0x100 increments of 1024 cycles each.
        assert_eq!(timers.cycles_to_overflow(), Some(0x100 * 1024));
        timers.tick(1024);
        assert_eq!(timers.cycles_to_overflow(), Some(0xFF * 1024));
    }

    #[test]
    fn a_byte_read_of_the_counter_follows_the_count() {
        let timer = Timer {
            reload: 0x0100,
            counter: 0x0101,
            control: START | 0b01,
            sub: 0,
        };
        assert_eq!(timer.read8(0), 0x01);
        assert_eq!(timer.read8(1), 0x01);
        let mut timers = Timers::new();
        timers.t[0] = timer;
        timers.tick(64);
        assert_eq!(timers.read8(TIMER_BASE), 0x02, "TM0CNT_L low byte");
        assert_eq!(timers.read8(TIMER_BASE + 1), 0x01, "TM0CNT_L high byte");
        assert_eq!(timers.read8(TIMER_BASE + 2), START as u8 | 0b01);
        // A timer that has not counted reads the low byte of its reload.
        let unstarted = Timer {
            reload: 0x0100,
            ..Timer::default()
        };
        assert_eq!(unstarted.read8(0), 0x00);
    }
}
