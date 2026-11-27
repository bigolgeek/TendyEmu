//! Save-state serialisation for the CPU (field order is fixed; see `Cpu`).

use super::Cpu;
use crate::state::{StateError, StateReader, StateWriter};

impl Cpu {
    /// Writes every `Cpu` field in declaration order: a, x, y, sp, pc, p, cycles,
    /// nmi_pending, skip_nmi_poll, irq_line, poll_i, jammed.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.a);
        w.u8(self.x);
        w.u8(self.y);
        w.u8(self.sp);
        w.u16(self.pc);
        w.u8(self.p);
        w.u64(self.cycles);
        w.bool(self.nmi_pending);
        w.bool(self.skip_nmi_poll);
        w.bool(self.irq_line);
        w.opt_bool(self.poll_i);
        w.bool(self.jammed);
    }

    /// Mirror of `save_state`.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.a = r.u8()?;
        self.x = r.u8()?;
        self.y = r.u8()?;
        self.sp = r.u8()?;
        self.pc = r.u16()?;
        self.p = r.u8()?;
        self.cycles = r.u64()?;
        self.nmi_pending = r.bool()?;
        self.skip_nmi_poll = r.bool()?;
        self.irq_line = r.bool()?;
        self.poll_i = r.opt_bool()?;
        self.jammed = r.bool()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_field() {
        let cpu = Cpu {
            a: 0x12,
            x: 0x34,
            y: 0x56,
            sp: 0x78,
            pc: 0x9ABC,
            p: 0xA5,
            cycles: 0x0123_4567_89AB_CDEF,
            nmi_pending: true,
            skip_nmi_poll: false,
            irq_line: true,
            poll_i: Some(false),
            jammed: false,
        };

        let mut w = StateWriter::new();
        cpu.save_state(&mut w);
        // a, x, y, sp (4) + pc (2) + p (1) + cycles (8) + 3 bools (3) + opt_bool (2) + jammed (1).
        assert_eq!(w.len(), 21);

        let data = w.finish();
        let mut out = Cpu::new();
        out.load_state(&mut StateReader::new(&data)).unwrap();
        assert_eq!(out, cpu);
    }

    #[test]
    fn poll_i_none_round_trips() {
        let mut cpu = Cpu::new();
        assert_eq!(cpu.poll_i, None);
        cpu.poll_i = Some(true);

        let mut w = StateWriter::new();
        cpu.save_state(&mut w);
        let data = w.finish();

        let mut out = Cpu::new();
        out.load_state(&mut StateReader::new(&data)).unwrap();
        assert_eq!(out.poll_i, Some(true));

        let fresh = Cpu::new();
        let mut w = StateWriter::new();
        fresh.save_state(&mut w);
        let data = w.finish();
        let mut out = Cpu::new();
        out.poll_i = Some(false);
        out.load_state(&mut StateReader::new(&data)).unwrap();
        assert_eq!(out.poll_i, None);
        assert_eq!(out, fresh);
    }

    #[test]
    fn truncated_state_is_an_error() {
        let mut cpu = Cpu::new();
        assert_eq!(
            cpu.load_state(&mut StateReader::new(&[])),
            Err(StateError::Truncated)
        );
    }
}
