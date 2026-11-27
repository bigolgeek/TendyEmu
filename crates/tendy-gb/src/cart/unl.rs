//! Unlicensed mappers: the Sachen 4-in-1 multicart, Wisdom Tree and the Pro Action Replay.
//!
//! Every protocol here was read out of the images themselves — the boot code and the writes it
//! makes to `$0000-$7FFF` — and confirmed by the cartridge drawing its own menu or title.

use crate::cart::{Mbc, RamAccess};
use crate::state::{StateError, StateReader, StateWriter};

/// The Rocket Games (Datel/InterAct) boards carry a decoy header — no Nintendo logo, a type byte
/// of `$97`/`$99`, and a RAM size byte that does not describe the silicon. The only two of the ten
/// that use cartridge RAM are the Space Invasion carts, and they prove it by writing MBC1's
/// enable nibble (`LD A,$0A` then a write to `$0000-$1FFF`) before reading their scratch buffer at
/// `$A700`. The other eight never write the enable nibble, so honouring their header RAM size would
/// allocate (and `.sav`) RAM the cartridge does not have. Return 8 KiB when the image carries that
/// enable sequence, none otherwise.
pub fn rocket_ram_bytes(rom: &[u8]) -> usize {
    for w in rom.windows(5) {
        if w[0] == 0x3E && w[1] == 0x0A && w[2] == 0xEA {
            let addr = u16::from(w[3]) | (u16::from(w[4]) << 8);
            if addr < 0x2000 {
                return 0x2000;
            }
        }
    }
    0
}

/// Which unlicensed board an image needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Board {
    /// Sachen 4-in-1 (`4B-0xx`): a write to `$0000-$1FFF` selects the bank at `$0000-$3FFF`, a write
    /// to `$2000-$3FFF` the one at `$4000-$7FFF`.
    Sachen,
    /// Wisdom Tree: a write to `$0000-$3FFF` selects the bank seen at *both* windows.
    WisdomTree,
    /// Datel Pro Action Replay: 32 KiB, no banking, 8 KiB of always-enabled work RAM.
    ProActionReplay,
}

/// The 48-byte Nintendo logo at `$0104-$0133`.
const LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// True for the trampoline at the start of every Wisdom Tree bank:
/// `LD BC,$0000; LD (BC),A; JP <entry>` — a bank switch followed by a jump that the switch cannot
/// break, because every bank carries the same three instructions there.
fn wisdom_tree_trampoline(rom: &[u8], at: usize) -> bool {
    rom.len() >= at + 7
        && rom[at] == 0x01
        && rom[at + 1] == 0x00
        && rom[at + 2] == 0x00
        && rom[at + 3] == 0x02
        && rom[at + 4] == 0xC3
}

/// Identifies the three boards, ahead of the header-type table.
///
/// Deliberately narrow: each rule pins the image size, the entry bytes and a run of boot code only
/// these boards carry, so a cartridge that already draws cannot be captured. A sweep over the
/// images on hand is the proof of that.
pub fn detect(rom: &[u8]) -> Option<Board> {
    if rom.len() < 0x8000 {
        return None;
    }
    let has_logo = rom[0x104..0x134] == LOGO;
    // Sachen 4-in-1: 256 KiB, no Nintendo logo at all (the header is a decoy), and the real reset
    // vector at `$0000` is `JP $0200`, optionally behind a `DI`.
    if rom.len() == 0x4_0000
        && !has_logo
        && (rom[0..3] == [0xC3, 0x00, 0x02] || rom[0..4] == [0xF3, 0xC3, 0x00, 0x02])
    {
        return Some(Board::Sachen);
    }
    // Wisdom Tree: `JP $0000` at the entry, a real logo, and the trampoline at `$0000`.
    if rom.len() >= 0x2_0000
        && has_logo
        && rom[0x100..0x103] == [0xC3, 0x00, 0x00]
        && wisdom_tree_trampoline(rom, 0)
    {
        return Some(Board::WisdomTree);
    }
    // Pro Action Replay: 32 KiB, no logo, and `NOP; JP $0150` where a licensed entry would be.
    if rom.len() == 0x8000
        && !has_logo
        && rom[0x100] == 0x00
        && rom[0x101..0x104] == [0xC3, 0x50, 0x01]
    {
        return Some(Board::ProActionReplay);
    }
    None
}

/// The Sachen 4-in-1 multicart.
///
/// Each image carries a launcher that the menu copies to `$C01D`; on a selection it writes
/// `$2000 <- $FF`, a bank to `$0000-$1FFF` or `$4000-$5FFF`, `$2000 <- $00` and jumps to `$0200` in
/// the bank it just selected. The chosen game runs in a 32 KiB block: the bank written to
/// `$0000-$1FFF`/`$4000-$5FFF` at `$0000-$3FFF` and its odd partner at `$4000-$7FFF`, and writing
/// 1..3 to `$2000-$3FFF` picks the block's second, third or fourth bank at `$4000-$7FFF`.
#[derive(Clone)]
pub struct Sachen {
    /// Bank number at `$0000-$3FFF`, from writes to `$0000-$1FFF`.
    fixed: u8,
    /// Bank number at `$4000-$7FFF`, from writes to `$2000-$3FFF`.
    banked: u8,
    rom_size: usize,
}

impl Sachen {
    pub fn new(rom_banks: usize) -> Sachen {
        Sachen {
            fixed: 0,
            banked: 1,
            rom_size: rom_banks.max(1) * 0x4000,
        }
    }

    fn offset(&self, bank: u8, addr: u16) -> usize {
        ((bank as usize * 0x4000) | (addr as usize & 0x3FFF)) % self.rom_size
    }
}

impl Mbc for Sachen {
    fn rom_offset(&self, addr: u16) -> usize {
        if addr < 0x4000 {
            self.offset(self.fixed, addr)
        } else {
            self.offset(self.banked, addr)
        }
    }

    fn ram_access(&self, _addr: u16) -> RamAccess {
        RamAccess::Off
    }

    fn write_rom(&mut self, addr: u16, value: u8) {
        // A value the image cannot hold is not a bank: the board decodes four lines, and the images
        // rely on that (the `4B-006` menu writes `$FE` to `$0000` at boot and keeps running from
        // bank 0, while the `4B-009` launcher writes `$FC` there and still starts game 12).
        let banks = self.rom_size / 0x4000;
        let valid = (value as usize) < banks;
        match addr {
            // Both `$0000-$1FFF` and `$4000-$5FFF` carry a bank of the game's block. `4B-001` puts
            // the bank at `$0000` and writes `$FE`/`$F8` to `$4000` (too big for a 16-bank image);
            // `4B-009` puts it at `$4000` and writes `$FC`/`$FE` to `$0000`. The games' own switch
            // stub writes `$2000 <- $31` and then `$4000 <- <bank>` from HRAM, so `$4000-$5FFF` is a
            // register, not the ROM window it names.
            0x0000..=0x1FFF | 0x4000..=0x5FFF if valid => {
                self.fixed = value;
                self.banked = if (value as usize) + 1 < banks {
                    value + 1
                } else {
                    value
                };
            }
            // Writing 1..3 selects the second, third or fourth bank of the block. The games use it as
            // their bank API (`LD A,1; LD ($2000),A`, call, then restore from `$C00A`). `$00` and
            // values above 3 leave the window alone, which is what lets the launcher bracket its
            // handover with `$2000 <- $FF` ... `$2000 <- $00` without losing the pair.
            0x2000..=0x3FFF
                if (1..=3).contains(&value) && (self.fixed as usize) + (value as usize) < banks =>
            {
                self.banked = self.fixed + value;
            }
            _ => {}
        }
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    /// The menu bank's reset vector at `$0000` (`DI; JP $0200`) is the real entry; `$0100` holds
    /// the decoy header.
    fn entry_point(&self) -> u16 {
        0x0000
    }

    fn name(&self) -> &'static str {
        "sachen"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.fixed);
        w.u8(self.banked);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        let fixed = r.u8()?;
        let banked = r.u8()?;
        if (fixed as usize) * 0x4000 >= self.rom_size || (banked as usize) * 0x4000 >= self.rom_size
        {
            return Err(StateError::BadValue);
        }
        self.fixed = fixed;
        self.banked = banked;
        Ok(())
    }
}

/// The Wisdom Tree board: a 74LS377 octal latch that switches the whole 32 KiB ROM area.
///
/// Pan Docs, *Other MBCs* § Wisdom Tree: the latch clocks on the write pulse's rising edge, when the
/// data bus is already invalid, so the board uses the **address** lines A7-A0 and ignores the value
/// written, and it switches the whole 32 KiB area at once. The latch's eight outputs are the ROM's
/// A15-A22, so a write to `$YYXX` selects 32 KiB **block** `XX`: bank `XX*2` at `$0000-$3FFF` and
/// bank `XX*2+1` at `$4000-$7FFF`, with the CPU's A14 choosing the half. That is why the trampoline
/// - `LD BC,$0000; LD (BC),A; JP $094B` - sits at `$0000` of every **even** bank: writing `$0000`
///   selects block 0 and the jump lands on the copy in bank 0.
#[derive(Clone)]
pub struct WisdomTree {
    /// Latched address bus, A7-A0, from the last write to `$0000-$7FFF`.
    latch: u8,
    rom_size: usize,
}

impl WisdomTree {
    pub fn new(rom_banks: usize) -> WisdomTree {
        WisdomTree {
            latch: 0,
            rom_size: rom_banks.max(1) * 0x4000,
        }
    }

    fn bank(&self, addr: u16) -> usize {
        // The latch selects one 32 KiB block: its eight outputs are the ROM's A14-A21, so the
        // low byte written is the *block* number, and the CPU's A14 picks the half within it.
        // (Pan Docs: "it switches the whole 32 KiB ROM area ... to select bank $XX, write to
        // $YYXX".) Treating $XX as a 16 KiB bank number halves the block and lands every call
        // a bank early - Spiritual Warfare's switch stubs write $0004 and then call $1FC2,
        // $200A and $2092, which are only code in banks 8 and 9, not 4 and 5.
        let block = self.latch as usize * 2;
        let bank = block | usize::from(addr >= 0x4000);
        bank % (self.rom_size / 0x4000).max(1)
    }
}

impl Mbc for WisdomTree {
    fn rom_offset(&self, addr: u16) -> usize {
        (self.bank(addr) * 0x4000) | (addr as usize & 0x3FFF)
    }

    fn ram_access(&self, _addr: u16) -> RamAccess {
        RamAccess::Off
    }

    fn write_rom(&mut self, addr: u16, _value: u8) {
        self.latch = addr as u8;
    }

    fn take_latch_request(&mut self) -> bool {
        false
    }

    /// `JP $0000` at the entry: the trampoline at `$0000` is what puts the latch in a known state.
    fn entry_point(&self) -> u16 {
        0x0000
    }

    fn name(&self) -> &'static str {
        "wisdomtree"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(self.clone())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.latch);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.latch = r.u8()?;
        Ok(())
    }
}

/// The Datel Pro Action Replay.
///
/// A 32 KiB mask ROM with no banking, plus 8 KiB of work RAM at `$A000-$BFFF` that is always
/// enabled: the boot code fills `$B000-$B03E` with zero and then branches on the byte at `$B040`,
/// so the RAM has to answer without any enable write (the image never makes one).
#[derive(Clone, Copy, Default)]
pub struct ProActionReplay;

impl Mbc for ProActionReplay {
    fn rom_offset(&self, addr: u16) -> usize {
        (addr & 0x7FFF) as usize
    }

    fn ram_access(&self, addr: u16) -> RamAccess {
        RamAccess::Ram((addr as usize) & 0x1FFF)
    }

    fn write_rom(&mut self, _addr: u16, _value: u8) {}

    fn take_latch_request(&mut self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "actionreplay"
    }

    fn clone_box(&self) -> Box<dyn Mbc> {
        Box::new(*self)
    }

    fn save_state(&self, _w: &mut StateWriter) {}

    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), StateError> {
        Ok(())
    }
}
