//! The three memory maps and the SRAM windows: pure address arithmetic, so a save state has
//! nothing to serialise for the board.

use crate::addr::Addr;
use crate::cart::Board;

/// The address inside a ROM image that `addr` maps to, already masked to the image.
pub fn rom_offset(board: Board, addr: Addr, mask: usize) -> usize {
    match board {
        Board::LoRom => {
            ((((addr.bank() & 0x7F) as usize) << 15) | (addr.offset() as usize & 0x7FFF)) & mask
        }
        Board::HiRom => (addr.linear() & 0x3F_FFFF) & mask,
        Board::ExHiRom => {
            let base = if addr.bank() & 0x80 != 0 {
                0
            } else {
                0x40_0000
            };
            (base + (addr.linear() & 0x3F_FFFF)) & mask
        }
    }
}

/// The SRAM index `addr` addresses on `board`, or `None` when it is not in an SRAM window.
///
/// The mask is every board's mirroring for a small chip: a 2 KiB cartridge repeats through its
/// 8 KiB window. A window with **no chip** is still a window and answers `Some(0)`, so
/// `Cartridge::read` returns `$FF` for an unpopulated socket instead of falling through to the ROM
/// ("A cartridge with no SRAM but with a board that has an SRAM window reads `$FF` and
/// drops writes, which is what an unpopulated socket does").
pub fn ram_offset(board: Board, addr: Addr, ram_len: usize) -> Option<usize> {
    let index = window_offset(board, addr, ram_len)?;
    if ram_len == 0 {
        return Some(0);
    }
    Some(index & (ram_len - 1))
}

/// The board's SRAM window decode, before the chip's size masks it, or `None` outside a window.
///
/// Sources: fullsnes "SNES Memory", §Battery-backed SRAM — *"There are two basic SRAM mapping
/// schemes ... HiROM ---> SRAM at 30h-3Fh,B0h-BFh:6000h-7FFFh ;small 8K SRAM bank(s) / LoROM --->
/// SRAM at 70h-7Dh,F0h-FFh:0000h-7FFFh ;big 32K SRAM bank(s)"* — and the same page's note that
/// *"SRAM is usually also mirrored to both WS1 and WS2 areas. SRAM in bank 30h-3Fh is often also
/// mirrored to 20h-2Fh"*, which is why the HiROM line here spans `$20`-`$3F`.
///
/// A LoROM board gets **both** windows, the second one only when the cartridge actually has a chip:
/// the `$0060`-`$7FFF` low half of a LoROM bank is expansion, not ROM, so a LoROM image with a large
/// SRAM can be wired there exactly as a HiROM one is. Gamars Puzzle — a LoROM image that declares
/// 128 KiB, i.e. sixteen 8 KiB banks, the width of this window — copies a 256-byte block there and
/// reads it back to verify it; with the writes dropped the block never verifies and the copy loop
/// never ends.
fn window_offset(board: Board, addr: Addr, ram_len: usize) -> Option<usize> {
    let bank = addr.bank();
    let offset = addr.offset();
    match board {
        Board::LoRom => {
            // Banks $70-$7D and $F0-$FF, offsets $0000-$7FFF. `bank & 0x0F` is what maps a 64/128
            // KiB chip's 32 KiB chunks to bank $70, $71, ...
            if matches!(bank, 0x70..=0x7D | 0xF0..=0xFF) && offset <= 0x7FFF {
                return Some((((bank & 0x0F) as usize) << 15) | (offset as usize & 0x7FFF));
            }
            // The 8 KiB window fullsnes gives the "HiROM" scheme, on a board that has a chip in it.
            // Without one this region is expansion/open bus, which this machine does not model, and
            // decoding it would take ROM bytes away from an image that reads them.
            if ram_len > 0
                && matches!(bank, 0x20..=0x3F | 0xA0..=0xBF)
                && (0x6000..=0x7FFF).contains(&offset)
            {
                return Some((((bank & 0x1F) as usize) << 13) | (offset as usize & 0x1FFF));
            }
            None
        }
        // ExHiROM is a HiROM board with an inverted A23, so its SRAM window is HiROM's.
        Board::HiRom | Board::ExHiRom => {
            if !matches!(bank, 0x20..=0x3F | 0xA0..=0xBF) || !(0x6000..=0x7FFF).contains(&offset) {
                return None;
            }
            Some((((bank & 0x1F) as usize) << 13) | (offset as usize & 0x1FFF))
        }
    }
}
