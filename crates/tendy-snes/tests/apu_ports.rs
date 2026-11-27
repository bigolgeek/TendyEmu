//! The `$2140` handshake, driven by a synthetic cartridge.
//!
//! The S-CPU writes a byte to `$2140` and polls for the echo, while the SPC700 runs an uploaded
//! program that waits for that byte with `CMP $F4,#imm` and echoes it. `CMP dp,#imm` is one of the
//! three ALU forms whose destination is memory rather than `A`; if the compare reads `A` instead of
//! `$F4`, the SPC never sees the S-CPU's byte and the cartridge polls forever — the mode-E stall,
//! in miniature.
//!
//! The program is uploaded through the IPL the way an uploader does it (§7.3), so the test also
//! exercises the handshake the cartridge depends on.

use tendy_snes::addr::Addr;
use tendy_snes::apu::{Apu, Host};
use tendy_snes::cart::Cartridge;
use tendy_snes::machine::Machine;

mod common;

/// One command: the destination in `$2142`/`$2143`, `$2141`, then the kick.
fn kick(apu: &mut Apu, destination: u16, mode: u8, kick_byte: u8) {
    apu.write_port(2, destination as u8);
    apu.write_port(3, (destination >> 8) as u8);
    apu.write_port(1, mode);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
}

/// Polls `$2140` the way an uploader does, handing the SPC clocks while it waits: the boot ROM
/// reads the ports from software, so a write is only seen once the SPC runs.
fn await_echo(apu: &mut Apu, want: u8) {
    for _ in 0..200_000 {
        if apu.read_port(0) == want {
            return;
        }
        apu.tick(64); // a couple of SPC cycles: one pass of the ROM's poll loop is 7-9
    }
    panic!(
        "the boot ROM never echoed ${want:02X}; $2140 holds ${:02X}",
        apu.read_port(0)
    );
}

/// Uploads `data` to `destination` and enters it, the full handshake: a transfer kick, one
/// index/data pair per byte, then the entry command (`$2141` = 0).
fn upload(apu: &mut Apu, destination: u16, data: &[u8]) {
    kick(apu, destination, 0x01, 0xCC);
    for (index, byte) in data.iter().enumerate() {
        apu.write_port(1, *byte);
        apu.write_port(0, index as u8);
        await_echo(apu, index as u8);
    }
    let next = (data.len().wrapping_add(1) & 0xFF) | 1;
    kick(apu, destination, 0x00, next as u8);
}

/// The SPC program: wait on port 0 for `$39`, echo it, spin. The port byte is copied to a direct-page
/// mailbox and compared there with `CMP dp,#imm` — the ALU form whose destination is memory — so
/// `A` is deliberately clobbered in between: the compare must read the mailbox, not `A`.
const SPC_ECHO: [u8; 16] = [
    0xE4, 0xF4, // $0200 mov  a,$F4
    0xC4, 0x07, // $0202 mov  $07,a
    0xE8, 0x00, // $0204 mov  a,#$00
    0x78, 0x39, 0x07, // $0206 cmp  $07,#$39
    0xD0, 0xF5, // $0209 bne  $0200
    0x8F, 0x39, 0xF4, // $020B mov  $F4,#$39   — the echo
    0x2F, 0xFE, // $020E bra  *
];

/// The cartridge: write `$39` to `$2140`, poll `$2140` until it reads back, then lift force-blank.
fn polling_cart() -> Vec<u8> {
    let code = [
        0xA9, 0x39, // lda #$39
        0x8D, 0x40, 0x21, // sta $2140
        0xAD, 0x40, 0x21, // lda $2140
        0xC9, 0x39, // cmp #$39
        0xD0, 0xF9, // bne -7
        0xA9, 0x0F, // lda #$0F
        0x8D, 0x00, 0x21, // sta $2100
        0x80, 0xFE, // bra *
    ];
    common::synth_lorom(0x8000, &code)
}

#[test]
fn the_echo_does_not_lag_a_store() {
    let cart = Cartridge::new(polling_cart()).expect("the synthesized cartridge loads");
    let mut machine = Machine::new(cart);

    // The uploader runs before the cartridge starts, exactly as the PSU/boot order works: the APU
    // is ready and waiting for a program by the time the S-CPU's first instruction retires.
    upload(&mut machine.bus.apu, 0x0200, &SPC_ECHO);
    assert_eq!(
        machine.bus.apu.cpu.pc, 0x0200,
        "the entry command did not start the program"
    );

    for _ in 0..60 {
        machine.run_frame();
    }

    assert_eq!(
        machine.bus.ppu.regs.inidisp, 0x0F,
        "the cartridge never saw $39 echoed back; $2140 = ${:02X}, wrote ${:02X}, SPC pc = {:04X}",
        machine.bus.apu.from_apu[0], machine.bus.apu.to_apu[0], machine.bus.apu.cpu.pc
    );
    assert_eq!(machine.bus.apu.from_apu[0], 0x39);
}

/// A driver that sets its own stack, pushes a return address, and jumps back to `$FFC9` to ask the
/// uploader for another block — the "call me through the IPL" idiom Champions World Class Soccer's
/// driver uses. `$FFC9` is the ROM's `MOV $F4,#$AA`, i.e. the documented way to re-enter the
/// uploader without the `$FFC0` reset ("If you ever want to go back to this uploader, simply jump to
/// $FFC0 with IPL enabled and the P flag clear (or $FFC9 to skip resetting the stack and page 0)").
const SPC_REENTER: [u8; 12] = [
    0xCD, 0xF5, // $0200 mov x,#$F5
    0xBD, // $0202 mov sp,x
    0xE8, 0x03, // $0203 mov a,#$03
    0x2D, // $0205 push a          ; the high half of the return address
    0xE8, 0x00, // $0206 mov a,#$00
    0x2D, // $0208 push a          ; the low half
    0x5F, 0xC9, 0xFF, // $0209 jmp $FFC9
];

#[test]
fn the_ipl_entry_keeps_the_stack_a_driver_jumped_back_with() {
    let mut apu = Apu::new(Host::Snes);
    apu.ram[0x0050] = 0x6F; // RET: the routine the uploader is about to enter

    upload(&mut apu, 0x0200, &SPC_REENTER);
    assert_eq!(
        apu.cpu.pc, 0x0200,
        "the entry command did not start the program"
    );

    // Let the program set SP, push $0300 and jump into the boot-ROM window.
    for _ in 0..500 {
        if apu.cpu.pc == 0xFFC9 {
            break;
        }
        apu.tick(64);
    }
    assert_eq!(
        apu.cpu.pc, 0xFFC9,
        "the program must reach the IPL's re-entry point"
    );
    assert_eq!(apu.cpu.sp, 0xF3, "two pushes from $F5 leave SP at $F3");

    // The uploader's answer: zero in `$2141`, `$0050` in `$2142`/`$2143`, and the `$CC` kick. The
    // ROM's Start loop ends in `JMP [$0000+X]`, which writes no register but A, X and Y — and with
    // `$2141` zero those are all zero.
    kick(&mut apu, 0x0050, 0x00, 0xCC);
    eprintln!(
        "after kick: pc={:04X} sp={:02X} to={:02X?} from={:02X?} ram0={:02X} ram1={:02X} \
         f4={:02X} f5={:02X}",
        apu.cpu.pc,
        apu.cpu.sp,
        apu.to_apu,
        apu.from_apu,
        apu.ram[0],
        apu.ram[1],
        apu.ram[0x01F4],
        apu.ram[0x01F5]
    );
    // Exactly one instruction: the RET the entry landed on. Stopping there keeps the assertion about
    // the entry itself, not about whatever the pushed address runs next.
    apu.step_instruction();

    assert_eq!(
        apu.cpu.pc, 0x0300,
        "the RET at $0050 must return to the address the driver pushed; fullsnes' IPL listing has \
         no `SP` write between `$FFC9` and `JMP [$0000+X]`, so the stack a driver jumped back with \
         is the stack the entry runs on (SP was ${:02X})",
        apu.cpu.sp
    );
    assert_eq!(
        apu.cpu.sp, 0xF5,
        "the RET pops the two bytes the program pushed, back to $F5"
    );
}

/// `$2144-$217F` are mirrors of `$2140-$2143`, on both sides, and not open bus.
///
/// fullsnes "SNES Memory Map": "2144h..217Fh - APU Ports 2140-2143h mirrored to 2144h..217Fh", and
/// "SNES Unpredictable Things": "Ports 2144h..217Fh are APU mirrors, NOT open bus." A machine that
/// answers the data bus there loses the S-CPU's byte (on a store) or hands the cartridge the last
/// opcode byte instead of the APU's.
#[test]
fn the_apu_ports_mirror_through_2144_to_217f() {
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let cart = Cartridge::new(rom).expect("a ROM-only cartridge loads");
    let mut m = Machine::new(cart);

    // Write side: a store to a mirror lands in the same latch as the port it mirrors.
    m.bus.write(Addr::new(0x00, 0x2144), 0x5A);
    m.bus.write(Addr::new(0x00, 0x217F), 0xA5);
    assert_eq!(
        m.bus.apu.to_apu,
        [0x5A, 0x00, 0x00, 0xA5],
        "fullsnes: $2144 mirrors $2140 and $217F mirrors $2143, so the stores land in ports 0 and 3"
    );

    // Read side: the mirror answers the APU's latch, not the last byte on the data bus.
    m.bus.apu.from_apu = [0x11, 0x22, 0x33, 0x44];
    m.bus.mdr = 0x99;
    assert_eq!(
        m.bus.read(Addr::new(0x00, 0x2144)),
        0x11,
        "fullsnes: $2144 is a mirror of $2140 (port 0), not open bus — the CPU's $99 must not show"
    );
    assert_eq!(
        m.bus.read(Addr::new(0x00, 0x2155)),
        0x22,
        "fullsnes: the mirror is by port, so $2155 ($2155-$2140 = $15, and $15 & 3 = 1) is port 1"
    );
    assert_eq!(
        m.bus.read(Addr::new(0x00, 0x217F)),
        0x44,
        "fullsnes: $217F is the mirror of $2143 (port 3)"
    );
}
