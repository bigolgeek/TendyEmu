//! The Game Boy Advance's interrupt controller: fourteen request bits, one vector.
//!
//! GBATEK, *GBA Interrupt Control*, is the whole of the hardware: `IE` at `$04000200` says which
//! requests are allowed through, `IF` at `$04000202` holds the requests themselves (writing a `1`
//! to a bit *acknowledges* it, writing `0` leaves it alone), and `IME` at `$04000208` is the master
//! switch in front of both. A request that survives `IE & IF` with `IME` set becomes the ARM7TDMI's
//! single IRQ line; the CPU's own `I` flag can mask it, and the BIOS handler then reads `IF` to find
//! out what to service.
//!
//! **There is no FIQ.** The ARM7TDMI has a second interrupt input, but on the Advance it is tied
//! off — "the FIQ signal is shortcut to VDD35 … there is no way to generate a FIQ by hardware"
//! (GBATEK, *Fast Interrupt (FIQ)*) — so every bit in `IE`/`IF` is an IRQ at `$18`, DMA 0-3,
//! keypad and Game Pak included. The CPU module used to route bits 8-13 to `$1C`, where the BIOS
//! has no entry; that was removed, and `crate::cpu` keeps its FIQ mode and vector only
//! because the ARM7TDMI can enter FIQ from software.
//!
//! The requests this machine raises are the display's in [`crate::bus`] (VBlank, HBlank and the
//! V-Counter match), the four timers in [`crate::timer`], the end of a DMA transfer in
//! [`crate::bus`], and the keypad's `KEYCNT` condition (G7). Serial and the cartridge's own IRQ
//! line have no source here yet (G9).
//!
//! **On priority.** GBATEK's GBA chapter lists the bits in numeric order and publishes no
//! priority table; the ARM7TDMI has one IRQ vector, so the order cannot be observed by software
//! either — a handler that cares reads `IF` and decides for itself ("User program may freely assign
//! priority to each reason by own logic", GBATEK, *Recommended User Interrupt handling*). What this
//! module does own is a documented *reporting* order, used when a test asks which request a set of
//! flags names: VBlank, then the V-Counter match, then HBlank, then the timers, serial, DMA, keypad
//! and the cartridge. That is the order the display's own events happen in (a line begins, drawing
//! ends, the frame's blank begins), which is the only ordering hardware gives anyone.

/// The number of interrupt requests GBATEK's GBA table names (bits 0-13).
pub const COUNT: usize = 14;

/// Bit 0: the display's vertical blank.
pub const VBLANK: usize = 0;
/// Bit 1: the display's horizontal blank.
pub const HBLANK: usize = 1;
/// Bit 2: `VCOUNT` matched the `DISPSTAT` V-Count setting.
pub const VCOUNT: usize = 2;
/// Bits 3-6: timers 0-3 overflowed.
pub const TIMER0: usize = 3;
/// Bit 7: a serial transfer finished (no source yet).
pub const SERIAL: usize = 7;
/// Bits 8-11: DMA channels 0-3 finished a transfer.
pub const DMA0: usize = 8;
/// Bit 12: the keypad matched `KEYCNT` (G7: [`crate::bus::Bus::keycnt`] and the condition its
/// bits 14-15 select).
pub const KEYPAD: usize = 12;
/// Bit 13: the cartridge's own IRQ line (no source yet).
pub const GAME_PAK: usize = 13;

/// Every request bit this machine implements: bits 0-13 (GBATEK: "14-15 Not used").
pub const IMPLEMENTED: u16 = 0x3FFF;

/// The request's bit, for `IE`/`IF`.
pub fn bit(index: usize) -> u16 {
    1 << index
}

/// What GBATEK's `IE`/`IF` table calls the request, for a test's failure message.
pub fn name(index: usize) -> &'static str {
    match index {
        VBLANK => "V-Blank",
        HBLANK => "H-Blank",
        VCOUNT => "V-Counter match",
        TIMER0 => "Timer 0 overflow",
        4 => "Timer 1 overflow",
        5 => "Timer 2 overflow",
        6 => "Timer 3 overflow",
        SERIAL => "Serial",
        DMA0 => "DMA 0",
        9 => "DMA 1",
        10 => "DMA 2",
        11 => "DMA 3",
        KEYPAD => "Keypad",
        GAME_PAK => "Game Pak",
        _ => "no request",
    }
}

/// The order requests are reported in, highest first. See the module documentation for why this is
/// this machine's order rather than GBATEK's (which publishes none).
const ORDER: [usize; COUNT] = [
    VBLANK, VCOUNT, HBLANK, TIMER0, 4, 5, 6, SERIAL, DMA0, 9, 10, 11, KEYPAD, GAME_PAK,
];

/// Where `index` sorts in [`ORDER`], lowest first. A request outside the table sorts last.
pub fn priority(index: usize) -> usize {
    ORDER
        .iter()
        .position(|&known| known == index)
        .unwrap_or(COUNT)
}

/// The highest-priority request among the bits of `flags`, or `None` when no bit is set.
pub fn highest(flags: u16) -> Option<usize> {
    ORDER.iter().copied().find(|&index| flags & bit(index) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_has_a_bit_a_name_and_a_place() {
        for (index, name) in ORDER.iter().zip(ORDER.iter().map(|&index| name(index))) {
            assert_eq!(bit(*index), 1 << index, "{name}");
            assert_ne!(name, "no request");
        }
        assert_eq!(ORDER.len(), COUNT);
        assert_eq!(IMPLEMENTED, 0x3FFF, "bits 0-13");
    }

    #[test]
    fn the_highest_priority_set_bit_is_the_one_reported() {
        assert_eq!(highest(0), None);
        assert_eq!(highest(bit(HBLANK)), Some(HBLANK));
        // VBlank above HBlank, HBlank's own bit above the V-Counter match's, timers below both.
        assert_eq!(highest(bit(VBLANK) | bit(HBLANK)), Some(VBLANK));
        assert_eq!(highest(bit(HBLANK) | bit(VCOUNT)), Some(VCOUNT));
        assert_eq!(highest(bit(TIMER0) | bit(VCOUNT)), Some(VCOUNT));
        assert_eq!(
            highest(bit(TIMER0) | bit(DMA0) | bit(KEYPAD)),
            Some(TIMER0),
            "a timer overflow above a DMA end and the keypad"
        );
        assert_eq!(highest(bit(GAME_PAK) | bit(SERIAL)), Some(SERIAL));
        // Everything at once reports the display's own event, which is what fires first in a line.
        assert_eq!(highest(IMPLEMENTED), Some(VBLANK));
    }
}
