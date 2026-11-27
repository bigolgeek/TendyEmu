//! What the BIOS's own `$00-$1C` vector table means to a core that has no BIOS image (H16).
//!
//! `$04` is the undefined-instruction vector. A cartridge walks into data, the ARM7TDMI takes the
//! exception, and the program counter lands on `$00000004` — the address GBATEK's *Exception
//! Vectors* table numbers `BASE+04h`. Twelve cartridges in the compatibility census end there.
//!
//! Three things must hold at that address, and they are the three this file pins:
//!
//! 1. **The machine is not parked.** A vector entry is hardware the cartridge can reach, not a fault
//!    in the core, so the core keeps stepping and the census can still see it.
//! 2. **Interrupts still arrive.** The exception itself masks IRQs (GBATEK, *Actions performed by
//!    CPU when entering an exception*: "IRQs disabled (I=1), done by ALL exceptions"), so the test
//!    clears the `I` bit the way a handler would and lets the LCD's V-Blank raise the request: the
//!    BIOS's `$18` → `$128` sequence still runs and still reaches the user's handler.
//! 3. **The entry is recorded**, so a probe can name the address rather than infer it.
//!
//! The document does not print the opcode at `$00000004` and does not describe what the GBA BIOS's
//! handler does on a normal cartridge; `crate::bios`'s `step` carries that quotation and the
//! consequence. Nothing here invents a handler: the program counter stays on the vector, and the
//! step costs what an unimplemented BIOS entry costs.

use tendy_gba::Gba;
use tendy_gba::bios::{OPCODE_AFTER_IRQ, OPCODE_DURING_IRQ};
use tendy_gba::bus::{DISPSTAT, IE, IF, IME, IO_BASE, IWRAM_BASE};
use tendy_gba::cart::Cartridge;
use tendy_gba::cpu;

/// The word the flag the BIOS writes for `IntrWait` lives at: "3007FF8h 2 Interrupt Check Flag".
const IRQ_FLAGS: u32 = 0x0300_7FF8;
/// "3007FFCh 4 Pointer to user IRQ handler (32bit ARM code)".
const IRQ_HANDLER_POINTER: u32 = 0x0300_7FFC;

/// A cartridge whose code starts at `$080000C0`: the entry point is the usual `b`.
fn synth_cart() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"GBA-VEC\0");
    rom[0xAC..0xB0].copy_from_slice(b"VEC1");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    // `b $080000C0`: the cartridge spins on itself, so the only reason the PC is not in the
    // cartridge is the one the test puts it there for.
    rom[0xC0..0xC4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine with a user IRQ handler installed, and every IRQ enabled the way a cartridge's boot
/// sequence leaves the machine: V-Blank in `DISPSTAT`, bit 0 in `IE`, `IME` on.
///
/// The handler acknowledges the request — "User MUST manually acknowledge by writing to IF
/// register" (GBATEK, *Recommended User Interrupt handling*) — and returns, so one frame is one
/// interrupt rather than a request the handler leaves standing.
///
/// The exception that reaches `$04` leaves `I` set — that is the hardware's own doing, not this
/// core's — so the test clears it, exactly as a handler that wanted to be interrupted again would.
fn machine() -> Gba {
    let mut gba = Gba::new(Cartridge::new(synth_cart()).expect("a synthesized cartridge"));
    let handler = IWRAM_BASE + 0x400;
    let program = [
        0xE3A0_1001, // mov   r1,1h
        0xE280_0C02, // add   r0,r0,200h   ; r0 is 4000000h on entry, so this is 4000202h (IF)
        0xE1C0_10B2, // strh  r1,[r0,2h]   ; acknowledge the request
        0xE12F_FF1E, // bx    lr
    ];
    for (index, word) in program.into_iter().enumerate() {
        gba.bus.write32(handler + index as u32 * 4, word);
    }
    gba.bus.write32(IRQ_HANDLER_POINTER, handler);
    gba.bus.write16(IO_BASE + DISPSTAT as u32, 1 << 3); // V-Blank raises an IRQ
    gba.bus.write16(IO_BASE + IE as u32, 1); // IE: V-Blank
    gba.bus.write16(IO_BASE + IME as u32, 1);
    gba.cpu.cpsr &= !cpu::FLAG_I;
    gba
}

#[test]
fn a_machine_at_the_undefined_vector_is_not_parked() {
    let mut gba = machine();
    gba.cpu.r[15] = cpu::VECTOR_UND;

    let retired = gba.cpu.instructions;
    for _ in 0..10_000 {
        gba.step_instruction();
    }

    assert!(
        !gba.cpu.halted,
        "a vector entry is the machine's, not a fault of the core"
    );
    assert_eq!(
        gba.bus.bios.unknown_entry,
        Some(cpu::VECTOR_UND),
        "the entry is recorded, so a probe can name the address"
    );
    assert_eq!(
        gba.cpu.r[15],
        cpu::VECTOR_UND,
        "and the core stays on the vector: no handler is invented for it"
    );
    assert_eq!(
        gba.cpu.instructions, retired,
        "nothing is fetched or retired at an address the document leaves empty"
    );
}

#[test]
fn a_machine_at_the_undefined_vector_still_takes_the_vblank_interrupt() {
    let mut gba = machine();
    gba.cpu.r[15] = cpu::VECTOR_UND;
    assert_eq!(
        gba.bus.read16(IO_BASE + IF as u32) & 1,
        0,
        "nothing pending before the frame runs"
    );

    gba.run_frame();

    assert!(
        !gba.cpu.halted,
        "the frame finished with the core still running"
    );
    assert_eq!(
        gba.bus.read16(IRQ_FLAGS) & 1,
        1,
        "the BIOS handler sampled V-Blank out of `IE & IF`"
    );
    assert_eq!(
        gba.bus.bios.latch, OPCODE_AFTER_IRQ,
        "`$18` -> `$128` -> the user's `bx lr` -> `$138`, and back"
    );
    assert_ne!(
        gba.bus.bios.latch, OPCODE_DURING_IRQ,
        "the IRQ did not stall inside the handler"
    );
    assert_eq!(
        gba.cpu.r[15],
        cpu::VECTOR_UND,
        "back on the vector the interrupt found it on"
    );
    assert_eq!(
        gba.bus.bios.unknown_entry,
        Some(cpu::VECTOR_UND),
        "and the entry is still recorded"
    );
}
