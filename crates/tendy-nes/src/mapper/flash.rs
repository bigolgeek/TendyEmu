//! SST39SF010A/SST39SF020A/SST39SF040 flash command state machine, in chip address space.
//!
//! These are the 1, 2 and 4 Mbit (128, 256 and 512 KB) x8 "Multi-Purpose Flash" parts that
//! flash-based NES boards use, and that several homebrew carts use to keep save data in PRG flash.
//! The module models the part itself: [`Flash::read`] and [`Flash::write`] take chip addresses —
//! A0 up to the part's AMS — and know nothing about a mapper's window or bank register. A board
//! turns its window into a chip address and calls in here.
//!
//! Command set: Microchip DS20005022F ("SST39SF010A/SST39SF020A/SST39SF040", rev F, 2025),
//! Table 4-2 "SOFTWARE COMMAND SEQUENCE", with the product IDs of Table 3-1. The decoder below is
//! exactly that table's rows:
//!
//! | command           | 1         | 2         | 3         | 4         | 5         | 6         |
//! |-------------------|-----------|-----------|-----------|-----------|-----------|-----------|
//! | Byte program      | 5555 ← AA | 2AAA ← 55 | 5555 ← A0 | BA ← data |           |           |
//! | Sector erase      | 5555 ← AA | 2AAA ← 55 | 5555 ← 80 | 5555 ← AA | 2AAA ← 55 | SA ← 30   |
//! | Chip erase        | 5555 ← AA | 2AAA ← 55 | 5555 ← 80 | 5555 ← AA | 2AAA ← 55 | 5555 ← 10 |
//! | Software ID entry | 5555 ← AA | 2AAA ← 55 | 5555 ← 90 |           |           |           |
//! | Software ID exit  | XX ← F0   |           |           |           |           |           |
//! | Software ID exit  | 5555 ← AA | 2AAA ← 55 | 5555 ← F0 |           |           |           |
//!
//! Table 4-2 note 1 is why the decoder masks command addresses down to A14-A0: the sequence's
//! addresses are "A14-A0 (Hex)", and AMS-A15 "can be VIL or VIH, but no other value", so a bank bit
//! left set on the bus does not stop a command cycle from counting. Table 3-1 and its note 5 are
//! why software-ID mode answers only chip addresses `$0000` (manufacturer `$BF`) and `$0001` (the
//! device ID), while every other address keeps reading the array.
//!
//! Program and erase are instantaneous here. Table 7-2's 20 µs byte program, 25 ms sector erase and
//! 100 ms chip erase are busy time, and the two ways to watch for the end of it — Data# Polling
//! (DQ7) and Toggle Bit (DQ6), §3.5-§3.7 — only describe a write that is still in flight. Nothing
//! in this module is ever busy, so neither the timings nor the status bits are modelled, and
//! neither is erase suspend/resume (`B0`/`30`), which has no meaning without an erase in progress.
//! Those belong with a timing model, not with the command decoder.
//!
//! A board that keeps its save data in the flash needs to know which 4 KB sectors changed since the
//! image on disk was loaded: [`Flash::dirty_sectors`] is that list, one bit per sector, so the
//! frontend can write back only what the game touched. [`Flash::clear_dirty`] is what it calls once
//! the image is safely on disk again.

use crate::state::StateError;

/// Erase granularity. Every part in the family has uniform 4 KB sectors (DS20005022F §3.3).
pub const SECTOR_SIZE: usize = 4 * 1024;

/// Manufacturer ID, read at chip address `$0000` in software-ID mode (DS20005022F Table 3-1).
pub const MANUFACTURER_ID: u8 = 0xBF;

/// An erased cell reads `$FF`; sector and chip erase both set the whole array to this (§3.3, §3.4).
pub const ERASED: u8 = 0xFF;

/// Table 4-2's `5555H`, decoded on A14-A0.
const ADDR_1: u32 = 0x5555;
/// Table 4-2's `2AAAH`, decoded on A14-A0.
const ADDR_2: u32 = 0x2AAA;
/// A14-A0; AMS-A15 are "VIL or VIH, but no other value" for a command cycle (Table 4-2 note 1).
const CMD_ADDR_MASK: u32 = 0x7FFF;

/// The three densities of the family, as Table 3-1 identifies them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// SST39SF010A: 128 KB, A0-A16.
    Sf010,
    /// SST39SF020A: 256 KB, A0-A17.
    Sf020,
    /// SST39SF040: 512 KB, A0-A18.
    Sf040,
}

impl Part {
    /// Image size in bytes: 128K x 8, 256K x 8, 512K x 8 (DS20005022F front page).
    pub const fn bytes(self) -> usize {
        match self {
            Part::Sf010 => 128 * 1024,
            Part::Sf020 => 256 * 1024,
            Part::Sf040 => 512 * 1024,
        }
    }

    /// Device ID read at `$0001` in software-ID mode (Table 3-1).
    pub const fn device_id(self) -> u8 {
        match self {
            Part::Sf010 => 0xB5,
            Part::Sf020 => 0xB6,
            Part::Sf040 => 0xB7,
        }
    }

    /// Number of uniform 4 KB sectors: 32, 64 or 128.
    pub const fn sectors(self) -> usize {
        self.bytes() / SECTOR_SIZE
    }
}

/// Where the command decoder is in Table 4-2's sequences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    /// No sequence in progress; a lone `AA` at `$5555` starts one, everything else is ignored.
    #[default]
    Idle,
    /// Saw `AA` at `$5555`; waiting for `55` at `$2AAA`.
    Unlock,
    /// Saw `AA`/`55`; waiting for the command byte at `$5555`.
    Command,
    /// Saw `80` at `$5555`; waiting for the erase sequence's own `AA`.
    EraseUnlock,
    /// Saw the erase sequence's `AA`; waiting for its `55` at `$2AAA`.
    EraseUnlock2,
    /// Saw the whole six-byte prefix; waiting for `30` (sector) or `10` (chip).
    EraseCommand,
    /// Saw `A0`; the next write is the byte address and data, not a command.
    Program,
}

impl Phase {
    /// The byte a save state keeps. The discriminants are the declaration order, so the mapping is
    /// written out rather than relied on.
    const fn code(self) -> u8 {
        match self {
            Phase::Idle => 0,
            Phase::Unlock => 1,
            Phase::Command => 2,
            Phase::EraseUnlock => 3,
            Phase::EraseUnlock2 => 4,
            Phase::EraseCommand => 5,
            Phase::Program => 6,
        }
    }

    /// A position read back from a state. Anything else is not a position this decoder wrote, so
    /// the state is corrupt: it fails rather than resuming from a made-up phase.
    const fn from_code(code: u8) -> Result<Phase, StateError> {
        match code {
            0 => Ok(Phase::Idle),
            1 => Ok(Phase::Unlock),
            2 => Ok(Phase::Command),
            3 => Ok(Phase::EraseUnlock),
            4 => Ok(Phase::EraseUnlock2),
            5 => Ok(Phase::EraseCommand),
            6 => Ok(Phase::Program),
            _ => Err(StateError::BadValue),
        }
    }
}

/// One SST39SF0x0 flash chip: its array, the command decoder, and the dirty-sector bitmap.
pub struct Flash {
    part: Part,
    mem: Vec<u8>,
    phase: Phase,
    /// Software-ID mode: reads at `$0000`/`$0001` answer the ID instead of the array.
    id_mode: bool,
    /// One bit per sector, set when the sector's contents changed since the image was loaded.
    dirty: Vec<u64>,
}

impl Flash {
    /// A part as it comes out of the box: every cell erased.
    pub fn new(part: Part) -> Flash {
        Flash {
            part,
            mem: vec![ERASED; part.bytes()],
            phase: Phase::Idle,
            id_mode: false,
            dirty: vec![0; part.sectors().div_ceil(64)],
        }
    }

    /// Wrap an image read back from disk (or from a save state). It must be exactly the part's
    /// size. The bitmap starts clear, because the image is what is already persisted.
    pub fn from_image(part: Part, image: Vec<u8>) -> Flash {
        assert_eq!(
            image.len(),
            part.bytes(),
            "flash image is not the size of the part"
        );
        Flash {
            part,
            mem: image,
            phase: Phase::Idle,
            id_mode: false,
            dirty: vec![0; part.sectors().div_ceil(64)],
        }
    }

    pub fn part(&self) -> Part {
        self.part
    }

    /// The array, in chip-address order. This is what a frontend writes back to disk.
    pub fn image(&self) -> &[u8] {
        &self.mem
    }

    /// Scan a chip address. Out-of-range addresses wrap: all three parts are a power of two long,
    /// and the pins above AMS are NC on the smaller ones, so a board wired for the bigger part
    /// aliases the image rather than running off the end.
    pub fn read(&self, addr: u32) -> u8 {
        if self.id_mode {
            // Table 3-1 note 5: the ID is read "with AMS-A1 = 0", so only chip addresses 0 and 1
            // decode to it and the rest of the array still reads through.
            match addr {
                0 => MANUFACTURER_ID,
                1 => self.part.device_id(),
                _ => self.mem[self.offset(addr)],
            }
        } else {
            self.mem[self.offset(addr)]
        }
    }

    /// A bus write to a chip address: either a command cycle or, inside the program sequence, the
    /// byte address and data. A write that is not part of a sequence changes nothing, which is what
    /// the part's write protection amounts to in software (§3.10).
    pub fn write(&mut self, addr: u32, val: u8) {
        let cmd = addr & CMD_ADDR_MASK;
        match self.phase {
            Phase::Idle => {
                if cmd == ADDR_1 && val == 0xAA {
                    self.phase = Phase::Unlock;
                } else if val == 0xF0 {
                    // Table 4-2 note 6: the single-write software ID exit is equivalent to the
                    // three-cycle one. Outside ID mode it is a no-op.
                    self.id_mode = false;
                }
            }
            Phase::Unlock => {
                self.phase = if cmd == ADDR_2 && val == 0x55 {
                    Phase::Command
                } else {
                    Phase::Idle
                };
            }
            Phase::Command => {
                self.phase = Phase::Idle;
                if cmd == ADDR_1 {
                    match val {
                        0xA0 => self.phase = Phase::Program,
                        0x80 => self.phase = Phase::EraseUnlock,
                        0x90 => self.id_mode = true,
                        0xF0 => self.id_mode = false,
                        _ => {}
                    }
                }
            }
            Phase::EraseUnlock => {
                self.phase = if cmd == ADDR_1 && val == 0xAA {
                    Phase::EraseUnlock2
                } else {
                    Phase::Idle
                };
            }
            Phase::EraseUnlock2 => {
                self.phase = if cmd == ADDR_2 && val == 0x55 {
                    Phase::EraseCommand
                } else {
                    Phase::Idle
                };
            }
            Phase::EraseCommand => {
                self.phase = Phase::Idle;
                if val == 0x30 {
                    // The last cycle's address selects the sector (Table 4-2 note 3); it need not
                    // be the sector's first byte.
                    self.erase_sector(self.offset(addr) / SECTOR_SIZE);
                } else if val == 0x10 && cmd == ADDR_1 {
                    self.chip_erase();
                }
            }
            Phase::Program => {
                self.phase = Phase::Idle;
                self.program(addr, val);
            }
        }
    }

    /// Number of 4 KB sectors.
    pub fn sector_count(&self) -> usize {
        self.part.sectors()
    }

    /// Whether `sector` changed since the image was loaded.
    pub fn sector_is_dirty(&self, sector: usize) -> bool {
        sector < self.sector_count() && self.dirty[sector / 64] & (1 << (sector % 64)) != 0
    }

    /// The sectors that changed since the image was loaded, in ascending order.
    pub fn dirty_sectors(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.sector_count()).filter(|&sector| self.sector_is_dirty(sector))
    }

    /// One bit per sector, set when that sector is dirty. A save-state or persistence layer can
    /// copy this instead of asking sector by sector.
    pub fn dirty_bitmap(&self) -> &[u64] {
        &self.dirty
    }

    /// No sector is dirty: the image matches what is on disk.
    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|&word| word != 0)
    }

    /// Forget every dirty bit. The image is untouched; the caller has just persisted it.
    pub fn clear_dirty(&mut self) {
        self.dirty.fill(0);
    }

    /// The chip's whole state, for a board's save state: the array, the command decoder's
    /// position, the software-ID latch and the dirty bitmap. A board that keeps the chip in its own
    /// `save_state` needs all four — a half-finished sequence is decoder state, and the dirty bits
    /// are what the next persistence pass writes back.
    pub fn save_state(&self, w: &mut crate::state::StateWriter) {
        w.bytes(&self.mem);
        w.u8(self.phase.code());
        w.bool(self.id_mode);
        for &word in &self.dirty {
            w.u64(word);
        }
    }

    /// Restore what [`Flash::save_state`] wrote, into a chip of the same part. Everything is read
    /// before anything is assigned, so a corrupt state leaves the chip untouched.
    pub fn load_state(
        &mut self,
        r: &mut crate::state::StateReader,
    ) -> Result<(), crate::state::StateError> {
        let image = r.bytes_vec()?;
        if image.len() != self.mem.len() {
            return Err(crate::state::StateError::LengthMismatch {
                expected: self.mem.len(),
                found: image.len(),
            });
        }
        let phase = Phase::from_code(r.u8()?)?;
        let id_mode = r.bool()?;
        let mut dirty = vec![0u64; self.dirty.len()];
        for word in &mut dirty {
            *word = r.u64()?;
        }
        self.mem = image;
        self.phase = phase;
        self.id_mode = id_mode;
        self.dirty = dirty;
        Ok(())
    }

    fn offset(&self, addr: u32) -> usize {
        (addr as usize) & (self.mem.len() - 1)
    }

    /// §3.2: the sector must be erased before it is programmed. On a byte that is not erased a
    /// program pulse can only clear bits, so the result is `old & data` and writing `$FF` over
    /// programmed data changes nothing. A byte that does not change leaves the sector clean.
    fn program(&mut self, addr: u32, data: u8) {
        let i = self.offset(addr);
        let new = self.mem[i] & data;
        if new != self.mem[i] {
            self.mem[i] = new;
            self.mark_dirty(i / SECTOR_SIZE);
        }
    }

    /// §3.3: a sector erase sets all 4 KB of the addressed sector to `$FF`.
    fn erase_sector(&mut self, sector: usize) {
        let start = sector * SECTOR_SIZE;
        let end = start + SECTOR_SIZE;
        if self.mem[start..end].iter().any(|&b| b != ERASED) {
            self.mem[start..end].fill(ERASED);
            self.mark_dirty(sector);
        }
    }

    /// §3.4: a chip erase sets the whole array to `$FF`.
    fn chip_erase(&mut self) {
        for sector in 0..self.sector_count() {
            self.erase_sector(sector);
        }
    }

    fn mark_dirty(&mut self, sector: usize) {
        self.dirty[sector / 64] |= 1 << (sector % 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{StateError, StateReader, StateWriter};

    fn flash() -> Flash {
        Flash::new(Part::Sf040)
    }

    /// The first two cycles every program and erase sequence starts with.
    fn unlock(f: &mut Flash) {
        f.write(0x5555, 0xAA);
        f.write(0x2AAA, 0x55);
    }

    /// A whole byte-program sequence, from idle.
    fn program(f: &mut Flash, addr: u32, data: u8) {
        unlock(f);
        f.write(0x5555, 0xA0);
        f.write(addr, data);
    }

    /// The six-byte sector-erase sequence, addressing the sector through `addr`.
    fn erase_sector(f: &mut Flash, addr: u32) {
        unlock(f);
        f.write(0x5555, 0x80);
        unlock(f);
        f.write(addr, 0x30);
    }

    fn chip_erase(f: &mut Flash) {
        unlock(f);
        f.write(0x5555, 0x80);
        unlock(f);
        f.write(0x5555, 0x10);
    }

    #[test]
    fn part_geometry_matches_the_datasheet() {
        assert_eq!(
            (
                Part::Sf010.bytes(),
                Part::Sf010.device_id(),
                Part::Sf010.sectors()
            ),
            (128 * 1024, 0xB5, 32)
        );
        assert_eq!(
            (
                Part::Sf020.bytes(),
                Part::Sf020.device_id(),
                Part::Sf020.sectors()
            ),
            (256 * 1024, 0xB6, 64)
        );
        assert_eq!(
            (
                Part::Sf040.bytes(),
                Part::Sf040.device_id(),
                Part::Sf040.sectors()
            ),
            (512 * 1024, 0xB7, 128)
        );
    }

    #[test]
    fn a_new_part_is_erased_and_clean() {
        let f = flash();
        assert_eq!(f.image().len(), 512 * 1024);
        assert!(f.image().iter().all(|&b| b == ERASED));
        assert!(!f.any_dirty());
        assert_eq!(f.read(0), ERASED);
        assert_eq!(f.read(0x7_FFFF), ERASED);
    }

    #[test]
    fn byte_program_writes_one_byte_and_dirties_its_sector() {
        let mut f = flash();
        program(&mut f, 0x1_2345, 0x5A);

        assert_eq!(f.read(0x1_2345), 0x5A);
        assert_eq!(f.read(0x1_2344), ERASED);
        assert_eq!(f.read(0x1_2346), ERASED);
        assert!(f.sector_is_dirty(0x1_2345 / SECTOR_SIZE));
        assert!(!f.sector_is_dirty(0));
        assert_eq!(f.dirty_sectors().collect::<Vec<_>>(), vec![0x12]);
    }

    #[test]
    fn program_can_only_clear_bits() {
        let mut f = flash();
        program(&mut f, 0x100, 0x0F);
        assert_eq!(f.read(0x100), 0x0F);

        // The datasheet wants the sector erased first (§3.2); a second program pulse over a
        // programmed byte can still only clear bits, so $F0 leaves $00 rather than $F0.
        program(&mut f, 0x100, 0xF0);
        assert_eq!(f.read(0x100), 0x00);

        // Writing ones over zeroes changes nothing at all.
        program(&mut f, 0x100, 0xFF);
        assert_eq!(f.read(0x100), 0x00);
    }

    #[test]
    fn sector_erase_clears_its_whole_sector_and_nothing_else() {
        let mut f = flash();
        program(&mut f, 0x8000, 0x00);
        program(&mut f, 0x8FFF, 0x00);
        program(&mut f, 0x9000, 0x00);

        // The sixth cycle's address picks the sector, so an address inside it is enough.
        erase_sector(&mut f, 0x8100);

        assert_eq!(f.read(0x8000), ERASED);
        assert_eq!(f.read(0x8FFF), ERASED);
        assert_eq!(f.read(0x9000), 0x00);
    }

    #[test]
    fn chip_erase_clears_every_sector() {
        let mut f = flash();
        program(&mut f, 0x0000, 0x00);
        program(&mut f, 0x1_0000, 0x00);
        program(&mut f, 0x7_0000, 0x00);
        assert_eq!(f.dirty_sectors().collect::<Vec<_>>(), vec![0, 16, 112]);

        chip_erase(&mut f);

        assert!(f.image().iter().all(|&b| b == ERASED));
        // The three sectors still differ from the disk image, so they stay dirty until persisted.
        assert_eq!(f.dirty_sectors().collect::<Vec<_>>(), vec![0, 16, 112]);
    }

    #[test]
    fn software_id_answers_only_addresses_zero_and_one() {
        let mut f = flash();
        program(&mut f, 0x1_0000, 0x00);
        unlock(&mut f);
        f.write(0x5555, 0x90);

        assert_eq!(f.read(0x0000), MANUFACTURER_ID);
        assert_eq!(f.read(0x0001), 0xB7);
        // Table 3-1 note 5 reads the ID with AMS-A1 = 0: everywhere else the array still answers.
        assert_eq!(f.read(0x0002), ERASED);
        assert_eq!(f.read(0x1_0000), 0x00);
    }

    #[test]
    fn software_id_exits_on_a_single_f0_or_the_three_cycle_sequence() {
        let mut f = flash();

        unlock(&mut f);
        f.write(0x5555, 0x90);
        assert_eq!(f.read(0x0000), MANUFACTURER_ID);
        f.write(0x4000, 0xF0); // any address, Table 4-2 note 6
        assert_eq!(f.read(0x0000), ERASED);

        unlock(&mut f);
        f.write(0x5555, 0x90);
        assert_eq!(f.read(0x0001), 0xB7);
        unlock(&mut f);
        f.write(0x5555, 0xF0);
        assert_eq!(f.read(0x0001), ERASED);
    }

    #[test]
    fn wrong_unlock_data_is_ignored() {
        let mut f = flash();
        f.write(0x5555, 0xAA);
        f.write(0x2AAA, 0x00); // not 55: the sequence aborts
        f.write(0x5555, 0xA0); // would be the program command
        f.write(0x0042, 0x00); // would be the byte

        assert_eq!(f.read(0x0042), ERASED);
        assert!(!f.any_dirty());

        // ...and the decoder is back at idle, so a fresh sequence works.
        program(&mut f, 0x0042, 0x00);
        assert_eq!(f.read(0x0042), 0x00);
    }

    #[test]
    fn wrong_unlock_address_is_ignored() {
        let mut f = flash();
        f.write(0x5554, 0xAA);
        f.write(0x2AAA, 0x55);
        f.write(0x5555, 0xA0);
        f.write(0x0042, 0x00);

        assert_eq!(f.read(0x0042), ERASED);
        assert!(!f.any_dirty());
    }

    #[test]
    fn an_erase_needs_its_own_unlock() {
        let mut f = flash();
        program(&mut f, 0x1800, 0x00);

        // 80 followed straight by 30 is not a sector erase: Table 4-2 wants AA/55 again.
        unlock(&mut f);
        f.write(0x5555, 0x80);
        f.write(0x1800, 0x30);
        assert_eq!(f.read(0x1800), 0x00);

        erase_sector(&mut f, 0x1800);
        assert_eq!(f.read(0x1800), ERASED);
    }

    #[test]
    fn chip_erase_requires_the_5555_address() {
        let mut f = flash();
        program(&mut f, 0x1800, 0x00);

        unlock(&mut f);
        f.write(0x5555, 0x80);
        unlock(&mut f);
        f.write(0x2AAA, 0x10); // 10 at the wrong address does not erase anything
        assert_eq!(f.read(0x1800), 0x00);
    }

    #[test]
    fn unlock_ignores_the_address_pins_above_a14() {
        let mut f = flash();
        // Table 4-2 note 1: AMS-A15 may be VIL or VIH, so a leftover bank bit must not break it.
        f.write(0x7_5555, 0xAA);
        f.write(0x6_AAAA, 0x55);
        f.write(0x4_5555, 0xA0);
        f.write(0x1234, 0x00);

        assert_eq!(f.read(0x1234), 0x00);
    }

    #[test]
    fn a_plain_write_changes_nothing() {
        let mut f = flash();
        f.write(0x1234, 0x00);
        assert_eq!(f.read(0x1234), ERASED);
        assert!(!f.any_dirty());
    }

    #[test]
    fn the_dirty_bitmap_is_per_sector_and_clears_without_touching_the_image() {
        let mut f = flash();
        program(&mut f, 0x0000, 0x00);
        program(&mut f, 0x2_0000, 0x00);

        assert_eq!(f.dirty_bitmap().len(), 2);
        assert_eq!(f.dirty_sectors().collect::<Vec<_>>(), vec![0, 32]);

        f.clear_dirty();
        assert!(!f.any_dirty());
        assert_eq!(f.read(0x0000), 0x00);
        assert_eq!(f.read(0x2_0000), 0x00);
    }

    #[test]
    fn from_image_starts_clean() {
        // An image read back from disk is clean, even when it is not erased.
        let mut f = Flash::from_image(Part::Sf010, vec![0x00; 128 * 1024]);
        assert_eq!(f.read(0x1_FFFF), 0x00);
        assert!(!f.any_dirty());

        // Erasing a sector that held data on disk changes it, so that sector becomes dirty.
        erase_sector(&mut f, 0x1000);
        assert_eq!(f.read(0x1000), ERASED);
        assert_eq!(f.dirty_sectors().collect::<Vec<_>>(), vec![1]);
    }

    /// A phase tag this decoder never wrote is a corrupt state: it fails, and nothing is applied.
    #[test]
    fn an_unknown_phase_tag_is_a_corrupt_state() {
        let mut f = flash();
        program(&mut f, 0x1234, 0x00);
        let mut w = StateWriter::new();
        f.save_state(&mut w);
        let mut state = w.finish();
        let tag = 4 + f.image().len(); // length-prefixed image, then the phase tag
        state[tag] = 9;

        let mut g = flash();
        assert_eq!(
            g.load_state(&mut StateReader::new(&state)),
            Err(StateError::BadValue)
        );
        assert_eq!(
            g.read(0x1234),
            ERASED,
            "the rejected state left the chip untouched"
        );
    }

    #[test]
    fn smaller_parts_alias_the_pins_above_ams() {
        // A17/A18 are NC on the 128 KB part, so a board that drives them wraps round the image.
        let mut f = Flash::new(Part::Sf010);
        program(&mut f, 0x2_0000, 0x00);
        assert_eq!(f.read(0x0000), 0x00);
        assert_eq!(f.read(0x2_0000), 0x00);
    }
}
