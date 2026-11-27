//! `DIV`/`TIMA`/`TMA`/`TAC` with the falling-edge behaviour of `Timer_Obscure_Behaviour.html`
//!
//! `DIV` is the upper byte of a 16-bit counter that counts **T-cycles**, +4 per M-cycle, which is
//! what makes `DIV` rise at 16 384 Hz and `DIV`'s bit 4 (counter bit 12) fall at 512 Hz — the
//! DIV-APU step the APU consumes.

use crate::bus::IF_TIMER;
use crate::state::{StateError, StateReader, StateWriter};

/// The 16-bit system counter's high byte is `DIV`; `TIMA` ticks on its selected bit's falling edge.
#[derive(Clone)]
pub struct Timer {
    /// The 16-bit system counter, in T-cycles: +4 per M-cycle.
    pub counter: u16,
    pub tima: u8,
    pub tma: u8,
    pub tac: u8,
    /// 0 idle; 1 = the overflow cycle (TIMA reads `$00`); 2 = the reload is pending and a TIMA
    /// write cancels it; 3 = the reload just happened, so a TIMA write in this cycle is ignored.
    pub overflow: u8,
    /// TMA as it was on the overflow cycle.
    pub reload_value: u8,
    /// One-M-cycle pulse: DIV bit 4 went 1 -> 0.
    pub div_apu_edge: bool,
}

impl Timer {
    /// Post-boot: counter = `$AB00`, TIMA=00 TMA=00 TAC=F8.
    pub fn new() -> Timer {
        Timer {
            counter: 0xAB00,
            tima: 0x00,
            tma: 0x00,
            tac: 0xF8,
            overflow: 0,
            reload_value: 0,
            div_apu_edge: false,
        }
    }

    /// One M-cycle: `counter += 4`, the selected bit's falling edge ticks TIMA, `overflow` advances.
    /// Returns `IF_TIMER` on the cycle the reload happens.
    pub fn tick(&mut self) -> u8 {
        let mut bits = 0;
        let old = self.counter;
        let new = old.wrapping_add(4);
        let edge = self.signal_at(old) && !self.signal_at(new);
        if old & 0x1000 != 0 && new & 0x1000 == 0 {
            // DIV's bit 4 is counter bit 12: the 512 Hz DIV-APU clock (Audio_details).
            self.div_apu_edge = true;
        }
        self.counter = new;

        // The reload happens one M-cycle after the overflow. A TIMA write before it cancels it
        // (§4.4); a write during the cycle it happens in is ignored, hence the one-cycle state 3.
        if self.overflow == 3 {
            self.overflow = 0;
        }
        if self.overflow == 2 {
            self.tima = self.reload_value;
            self.overflow = 3;
            bits |= IF_TIMER;
        }
        if edge {
            self.tick_tima();
        }
        if self.overflow == 1 {
            self.overflow = 2;
        }
        bits
    }

    /// One TIMA increment, including the overflow that starts the two-cycle reload sequence.
    fn tick_tima(&mut self) {
        if self.overflow == 1 || self.overflow == 2 {
            return; // the reload sequence owns TIMA until it finishes
        }
        self.tima = self.tima.wrapping_add(1);
        if self.tima == 0 {
            self.overflow = 1;
            self.reload_value = self.tma;
        }
    }

    /// True once per DIV bit-4 falling edge, and clears the flag.
    pub fn take_div_apu_edge(&mut self) -> bool {
        let edge = self.div_apu_edge;
        self.div_apu_edge = false;
        edge
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => self.tac | 0xF8,
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) -> u8 {
        match addr {
            0xFF04 => {
                // A DIV write resets the whole counter, which can be a falling edge on the selected
                // TAC bit and on DIV's bit 4 at the same time (Timer_Obscure_Behaviour).
                let edge = self.signal_at(self.counter) && !self.signal_at(0);
                if self.counter & 0x1000 != 0 {
                    self.div_apu_edge = true;
                }
                self.counter = 0;
                if edge {
                    self.tick_tima();
                    if self.overflow == 1 {
                        self.overflow = 2;
                    }
                }
            }
            0xFF05 => {
                if self.overflow == 3 {
                    // A write during the reload cycle is ignored.
                    return 0;
                }
                self.tima = value;
                if self.overflow == 2 {
                    // Writing TIMA in the M-cycle after the overflow cancels the reload, and with
                    // it the interrupt (`Timer_Obscure_Behaviour.html`).
                    self.overflow = 0;
                }
            }
            0xFF06 => {
                self.tma = value;
                if self.overflow >= 2 {
                    // A TMA write on the reload cycle reaches TIMA as well.
                    self.tima = value;
                    self.reload_value = value;
                }
            }
            0xFF07 => {
                let edge = self.signal();
                self.tac = value & 0x07;
                if edge && !self.signal() {
                    // Disabling the timer, or switching away from a set bit, ticks TIMA once.
                    self.tick_tima();
                    if self.overflow == 1 {
                        self.overflow = 2;
                    }
                }
            }
            _ => {}
        }
        0
    }

    /// The TAC multiplexer's output now: TAC bit 2 AND the counter bit TAC bits 1-0 select.
    fn signal(&self) -> bool {
        self.signal_at(self.counter)
    }

    fn signal_at(&self, counter: u16) -> bool {
        self.tac & 0x04 != 0 && (counter >> Timer::select_bit(self.tac)) & 1 == 1
    }

    /// TAC 00 -> bit 9 (4096 Hz), 01 -> bit 3 (262144), 10 -> bit 5 (65536), 11 -> bit 7 (16384).
    fn select_bit(tac: u8) -> u16 {
        match tac & 0x03 {
            0 => 9,
            1 => 3,
            2 => 5,
            _ => 7,
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u16(self.counter);
        w.u8(self.tima);
        w.u8(self.tma);
        w.u8(self.tac);
        w.u8(self.overflow);
        w.u8(self.reload_value);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.counter = r.u16()?;
        self.tima = r.u8()?;
        self.tma = r.u8()?;
        self.tac = r.u8()?;
        self.overflow = r.u8()?;
        self.reload_value = r.u8()?;
        Ok(())
    }
}

impl Default for Timer {
    fn default() -> Self {
        Timer::new()
    }
}
