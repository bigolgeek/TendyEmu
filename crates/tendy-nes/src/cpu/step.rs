use super::opcodes::{Mn, Mode, OPS};
use super::{Bus, Cpu, FLAG_I};

impl Cpu {
    /// Run one instruction (or enter a pending interrupt) and return the CPU cycles it took,
    /// including the +1 page-cross penalty for opcodes that have one and branch penalties.
    /// A jammed CPU only reads $FFFF (keeping the system clock running) and reports 1 cycle.
    ///
    /// After the instruction the CPU polls the bus for interrupts. The bus reports what it saw before
    /// the instruction's last cycle, so an interrupt raised during the last cycle waits one more instruction.
    pub fn step(&mut self, bus: &mut impl Bus) -> u32 {
        if self.jammed {
            // The bus keeps running: the halted 6502 reads $FFFF every cycle.
            bus.read(0xFFFF);
            self.cycles += 1;
            return 1;
        }
        let i_masked = self.poll_i.take().unwrap_or(self.flag(FLAG_I));
        let i_before = self.flag(FLAG_I);
        // Same decision `next_interrupt_vector` predicts, with `poll_i` already consumed above.
        let vector = self.interrupt_vector_for(i_masked);
        if vector == Some(0xFFFA) {
            self.nmi_pending = false;
        }
        let (cycles, late_i) = match vector {
            Some(v) => (self.interrupt(bus, v), false),
            None => self.execute(bus),
        };
        self.cycles += u64::from(cycles);
        if self.skip_nmi_poll {
            self.skip_nmi_poll = false;
        } else if bus.nmi_poll() {
            self.nmi_pending = true;
        }
        if let Some(irq) = bus.irq_poll() {
            self.irq_line = irq;
            self.poll_i = Some(if late_i { i_before } else { self.flag(FLAG_I) });
        }
        cycles
    }

    /// Fetch and run one instruction. Returns its cycles, and whether it is CLI, SEI or PLP (which
    /// change the I flag only after the interrupt poll).
    fn execute(&mut self, bus: &mut impl Bus) -> (u32, bool) {
        let opcode = self.fetch(bus);
        let op = OPS[opcode as usize];
        if op.mn == Mn::Jsr {
            return (self.jsr(bus), false);
        }
        let operand = self.operand_for(bus, op.mode, op.writes);
        // NOPs with an operand still read it.
        if op.mn == Mn::Nop && op.mode != Mode::Imp {
            bus.read(operand.addr);
        }
        let mut cycles = u32::from(op.cycles);
        if op.page_penalty && operand.page_crossed {
            cycles += 1;
        }
        // One exhaustive dispatch: no `_` arm, so a mnemonic added to `Mn` fails to compile
        // here until it is given a group, instead of silently acting as a no-op after four failed
        // lookups. It also saves the failed lookups: branches used to fall through two groups and the
        // unofficial opcodes through three.
        let extra = match op.mn {
            // JSR fetched its own operands above; `operand` is not its address sequence.
            Mn::Jsr => unreachable!("JSR is dispatched before the operand fetch"),
            Mn::Lda
            | Mn::Ldx
            | Mn::Ldy
            | Mn::Sta
            | Mn::Stx
            | Mn::Sty
            | Mn::Tax
            | Mn::Tay
            | Mn::Txa
            | Mn::Tya
            | Mn::Tsx
            | Mn::Txs
            | Mn::Pha
            | Mn::Php
            | Mn::Pla
            | Mn::Plp
            | Mn::Clc
            | Mn::Sec
            | Mn::Cli
            | Mn::Sei
            | Mn::Cld
            | Mn::Sed
            | Mn::Clv
            | Mn::Inc
            | Mn::Dec
            | Mn::Inx
            | Mn::Iny
            | Mn::Dex
            | Mn::Dey
            | Mn::And
            | Mn::Ora
            | Mn::Eor
            | Mn::Bit
            | Mn::Nop => self.exec_basic(bus, op.mn, operand.addr),
            Mn::Adc
            | Mn::Sbc
            | Mn::Cmp
            | Mn::Cpx
            | Mn::Cpy
            | Mn::Asl
            | Mn::Lsr
            | Mn::Rol
            | Mn::Ror => self.exec_alu(bus, op.mn, op.mode, operand.addr),
            Mn::Bcc
            | Mn::Bcs
            | Mn::Bne
            | Mn::Beq
            | Mn::Bpl
            | Mn::Bmi
            | Mn::Bvc
            | Mn::Bvs
            | Mn::Jmp
            | Mn::Rts
            | Mn::Rti
            | Mn::Brk
            | Mn::Jam => self.exec_flow(bus, op.mn, operand),
            Mn::Lax
            | Mn::Sax
            | Mn::Dcp
            | Mn::Isb
            | Mn::Slo
            | Mn::Rla
            | Mn::Sre
            | Mn::Rra
            | Mn::Anc
            | Mn::Alr
            | Mn::Arr
            | Mn::Axs
            | Mn::Xaa
            | Mn::Las
            | Mn::Shx
            | Mn::Shy
            | Mn::Ahx
            | Mn::Tas => self.exec_unofficial(bus, op.mn, operand),
        };
        (cycles + extra, matches!(op.mn, Mn::Cli | Mn::Sei | Mn::Plp))
    }
}
