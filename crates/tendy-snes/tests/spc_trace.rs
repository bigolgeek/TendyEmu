//! The SPC700 diagnosis ring: what a row claims, and that the clock
//! the machine runs on keeps the rows coming — the timers advance *between* steps, which is the
//! property `tests/spc_flow_probe.rs` did not have when a timer-paced driver read as a dead one.
//!
//! These are the running tests behind `tests/spc_trace_probe.rs` (the `#[ignore]`d harness that
//! points at a real cartridge). No cartridge is needed here: a program is dropped into RAM and the
//! `PC` pointed at it, so every number is the machine's own, not a title's.

use tendy_snes::apu::spc700::{PortSide, TRACE_LEN};
use tendy_snes::apu::{Apu, EventKind, Host};

/// Drops `program` at `$0200` and enters it the way the IPL's `JMP [$0000+X]` does: `PC` only.
fn run(program: &[u8]) -> Apu {
    let mut apu = Apu::new(Host::Snes);
    apu.ram[0x0200..0x0200 + program.len()].copy_from_slice(program);
    apu.cpu.pc = 0x0200;
    apu
}

/// `MOV A,#$12` / `MOV $F4,A` / `MOV $F2,#$4C` / `MOV $F3,#$01`: one store to a CPU->APU port and
/// one `KON` write, so a row has something to carry besides the instruction.
const PROGRAM: [u8; 12] = [
    0xE8, 0x12, // $0200 mov a,#$12
    0xC4, 0xF4, // $0202 mov $F4,a
    0x8F, 0x4C, 0xF2, // $0204 mov $F2,#$4C  (DSPADDR = KON)
    0x8F, 0x01, 0xF3, // $0207 mov $F3,#$01  (DSPDATA = 1: key on voice 0)
    0x2F, 0xFE, // $020A bra *
];

#[test]
fn the_ring_is_off_until_a_harness_asks() {
    let mut apu = run(&PROGRAM);
    apu.tick(10_000);
    assert_eq!(
        apu.trace_len(),
        0,
        "a machine nobody is tracing must record nothing — the ring is `Option<Box<[SpcEvent]>>` \
         and is only allocated by `enable_trace`"
    );
    apu.enable_trace();
    apu.tick(10_000);
    assert!(
        apu.trace_len() > 0,
        "after `enable_trace` the ring must hold rows"
    );
}

#[test]
fn a_row_carries_the_instruction_and_the_registers_after_it() {
    let mut apu = run(&PROGRAM);
    apu.enable_trace();
    apu.set_trace_frame(7);
    for _ in 0..4 {
        apu.step_instruction();
    }

    let row = |n: u64| {
        apu.trace_event(n)
            .expect("the ring must still hold the row")
    };

    let first = row(0);
    assert_eq!(first.kind, EventKind::Step);
    assert_eq!(
        (first.pc, first.bytes),
        (0x0200, [0xE8, 0x12, 0xC4]),
        "a row names the PC and the three bytes there *before* the instruction: `MOV A,#$12` is \
         `E8 12` and the next opcode is `C4`"
    );
    assert_eq!(first.a, 0x12, "the registers are read after the step");
    assert_eq!(first.frame, 7, "the harness's frame stamp is on the row");
    assert!(first.port_writes().is_empty() && first.dsp_reg == 0xFF);

    let store = row(1);
    assert_eq!(
        (store.pc, store.bytes[0]),
        (0x0202, 0xC4),
        "`MOV $F4,A` starts at $0202"
    );
    let writes = store.port_writes();
    assert_eq!(writes.len(), 1, "one store, one port row");
    assert_eq!(
        (writes[0].index, writes[0].value, writes[0].side),
        (0, 0x12, PortSide::Spc),
        "the SPC wrote $F4 = $12; the S-CPU reads it at $2140 (fullsnes \"SNES APU Memory and I/O \
         Map\": `00F4h/00F5h APU Input Ports 0/1 (R/W)` on the SPC side)"
    );

    let dsp = row(3);
    assert_eq!(
        (dsp.pc, dsp.dsp_reg, dsp.dsp_value),
        (0x0207, 0x4C, 0x01),
        "the row carries a KON write as the register and the value, so a key-off/key-on can be read \
         off the trace without decoding the DSPADDR/DSPDATA pair by hand"
    );
}

/// `T0DIV = 1`, `CONTROL = $81`, then `BRA *` — a program that does nothing but let the clock run.
const TIMER: [u8; 10] = [
    0xE8, 0x01, // $0200 mov a,#$01
    0xC4, 0xFA, // $0202 mov $FA,a   (T0DIV)
    0xE8, 0x81, // $0204 mov a,#$81
    0xC4, 0xF1, // $0206 mov $F1,a   ($F1: boot-ROM bit + timer 0 enable)
    0x2F, 0xFE, // $0208 bra *
];

#[test]
fn the_timers_advance_between_the_rows() {
    let mut apu = run(&TIMER);
    apu.enable_trace();
    for _ in 0..2_000 {
        apu.step_instruction();
    }

    // The divide-by-one setup: `$FA` is the prescaler and `$F1` bit 0 the enable (fullsnes
    // "SPC700 I/O Ports", `00FAh - T0DIV`, `00F1h - CONTROL`).
    let last = apu.trace_len() - 1;
    assert_eq!(
        apu.trace_event(last).unwrap().t0div,
        0x01,
        "the row carries the timer's own divider, not the machine's"
    );

    let mut rises = 0u32;
    let mut previous = None;
    let mut saw_high = false;
    for n in 0..apu.trace_len() {
        let Some(e) = apu.trace_event(n) else {
            continue;
        };
        if let Some(p) = previous
            && e.t0out != p
        {
            rises += 1;
            saw_high |= e.t0out != 0;
        }
        previous = Some(e.t0out);
    }
    assert!(
        rises > 10,
        "2 000 steps at a divide-by-one T0DIV must move T0OUT many times, not {rises}: the clock has \
         to be ticked between steps (the defect was a harness that stepped the \
         SPC without ticking it, so a timer-paced driver read as a dead one)"
    );
    assert!(
        saw_high,
        "T0OUT only ever read as zero — either the timer never fired or the row is not reading \
         `$FD`'s live value"
    );
}

#[test]
fn the_ring_keeps_the_newest_rows() {
    let mut apu = run(&PROGRAM);
    apu.enable_trace();
    let steps = TRACE_LEN as u32 + 16;
    for _ in 0..steps {
        apu.step_instruction();
    }
    assert_eq!(
        apu.trace_len(),
        u64::from(steps),
        "the ring counts every row it has ever recorded"
    );
    assert!(
        apu.trace_event(0).is_none(),
        "the oldest rows are overwritten, not kept: the ring is a fixed 131 072-entry box"
    );
    assert!(
        apu.trace_event(apu.trace_len() - 1).is_some(),
        "the newest row is always there"
    );
}
