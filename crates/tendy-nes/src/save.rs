//! Battery saves, core half: the bytes a battery holds and how to restore them. The path handling,
//! atomic writes and autosave bookkeeping stay in the front end (imported from `nes-rust`'s
//! `src/save.rs`; see `docs/DESIGN.md` §5.4).

use crate::nes::Nes;

/// Everything the battery save holds right now: the board's battery data when the cartridge has a
/// battery (PRG RAM, or the disk sides on an FDS, see [`crate::cart::Mapper::battery_data`]), then
/// CHR memory when the header declares CHR-NVRAM. The frontend compares snapshots to skip
/// rewriting an unchanged save, so the board's [`crate::cart::Mapper::battery_change_key`] leaves
/// out bytes the cartridge rewrites on its own — the `.sav` still gets them, but a chip that
/// keeps moving them cannot make every autosave look changed.
pub fn battery_snapshot(nes: &Nes) -> Vec<u8> {
    let mut out = Vec::new();
    if nes.battery {
        out.extend_from_slice(&nes.bus.cart().battery_change_key());
    }
    if nes.chr_battery {
        out.extend_from_slice(nes.bus.cart().chr_ram());
    }
    out
}

/// Restore a body that [`battery_snapshot`] produced: the board's battery data first (PRG RAM, the
/// disk sides on an FDS, or N163 PRG RAM plus chip RAM), then CHR-NVRAM when the header declares
/// it. Returns whether anything was loaded; an empty body on a battery-less board loads nothing.
pub fn load_battery_body(nes: &mut Nes, bytes: &[u8]) -> bool {
    let mut loaded = false;
    let mut rest = bytes;
    if nes.battery && !bytes.is_empty() {
        let prg_len = nes.bus.cart().battery_data().len().min(bytes.len());
        nes.bus.cart_mut().load_battery_data(&bytes[..prg_len]);
        rest = &bytes[prg_len..];
        loaded = true;
    }
    if nes.chr_battery && !rest.is_empty() {
        nes.bus.cart_mut().load_chr_ram(rest);
        loaded = true;
    }
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NES 2.0 NROM image: 16 KB PRG, no CHR-ROM, byte 11 = `b11`, flags 6 = `flags6`.
    fn nes2_rom(b11: u8, flags6: u8) -> Vec<u8> {
        let mut v = vec![
            0x4E, 0x45, 0x53, 0x1A, 0x01, 0x00, flags6, 0x08, 0x00, 0x00, 0x00, b11, 0x00, 0, 0, 0,
        ];
        v.resize(16 + 16384, 0);
        v
    }

    #[test]
    fn snapshot_holds_prg_then_chr() {
        let rom = nes2_rom(0x70, 0x02);
        let mut nes = Nes::new(&rom).unwrap();
        nes.bus.cart_mut().ppu_write(0x0000, 0xAB);
        let snap = battery_snapshot(&nes);
        assert_eq!(snap.len(), 8192 + 8192);
        assert_eq!(snap[8192], 0xAB);
    }
}
