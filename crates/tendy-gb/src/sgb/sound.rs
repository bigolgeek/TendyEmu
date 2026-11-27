//! Super Game Boy sound: the SNES sound chip the SGB program talks to.
//!
//! Pan Docs names the target on both sides of the conversation: *SGB Command $09 — SOU_TRN*
//! transfers code and score data "to SNES Audio Processing Unit memory (APU-RAM)", and *SGB Command
//! $08 — SOUND* asks for one of 73 effects. Both are the Super NES's own sound chip — the SPC700,
//! its 16-bit DSP and its four ports — which lives in `tendy-spc` and is re-exported here as
//! [`crate::sgb::spc`].
//!
//! This file is the seam between the two machines: the four ports, a direct write into the chip's
//! RAM (`SOU_TRN`'s payload), an entry point, and the clock that keeps it in step with the Game Boy.
//! The protocol `docs/SGB-SOUND.md` describes drives it: [`SgbSound::boot`] loads the cartridge's own
//! driver, [`SgbSound::play`] puts a `SOUND` command's two words on the ports, and
//! [`SgbSound::transfer`] takes a `SOU_TRN` record list. With no [`SgbSound`] there is no chip at all,
//! and the Game Boy's own mixer output is byte-identical to before (the test pins it).

use std::path::Path;

use crate::sgb::packets::SoundRequest;
use crate::sgb::spc::{APU_RAM_SIZE, Apu, Host, Ipl, SPC_CLOCK_HZ};
use crate::state::{StateError, StateReader, StateWriter};

/// The clock the Game Boy side of an SGB runs at here, and so the clock the sound chip is paced
/// against: `crate::CLOCK_HZ`, 4 194 304 Hz. The SNES's own master clock is not modelled — nothing
/// on the Game Boy side can see it.
pub const SGB_CLOCK_HZ: u32 = crate::CLOCK_HZ;

/// The SGB cartridge image's size: 2 Mbit, the multiplier the SGB BIOS is made of.
pub const SGB_ROM_SIZE: usize = 262_144;

/// Where the SNES header's title sits in an SGB cartridge image.
const SGB_TITLE_AT: usize = 0x7FC0;

/// The title the SGB's own cartridge carries, at [`SGB_TITLE_AT`].
const SGB_TITLE: &[u8] = b"Super GAMEBOY";

/// Where the SGB program's own boot table sits in the cartridge image (`docs/SGB-SOUND.md` §1).
const DRIVER_TABLE_AT: usize = 0x3_0000;

/// Where that table's terminator has to land: `$3CAD9`, the end of the fifth payload (§1). The
/// table is walked, not assumed, so this is the *check* — a file whose records do not add up to it
/// is not the cartridge the document was measured on.
const DRIVER_TABLE_END: usize = 0x3_CAD9;

/// The SPC700 entry the terminator names, `$0400`: block 3 is the driver's code (§1).
const DRIVER_ENTRY: u16 = 0x0400;

/// The port byte of the idle word `$A9A9`/`$A9A9` the SGB program leaves its driver at frame 120
/// (§4): a port sends it too, right after starting the driver.
const IDLE_WORD: u8 = 0xA9;

/// The SGB's sound chip, its four ports and the clock that drives it.
///
/// `Clone` because a machine is; nothing here is shared.
#[derive(Clone)]
pub struct SgbSound {
    chip: Apu,
    /// The remainder of the dot-to-SPC-cycle division, in units of `1/SGB_CLOCK_HZ` of a dot:
    /// `dots × SPC_CLOCK_HZ` are earned and `SGB_CLOCK_HZ` of them are one SPC cycle, so the ratio
    /// stays exact over any number of calls — 1 024 000 SPC cycles per 4 194 304 dots, to the cycle.
    acc: u64,
}

impl SgbSound {
    /// A chip at power-on: its RAM zeroed, the IPL handshake armed with its `AA BB` greeting, the
    /// DSP's `FLG` at `$E0`.
    pub fn new() -> SgbSound {
        SgbSound {
            chip: Apu::new(Host::Sgb),
            acc: 0,
        }
    }

    /// A write to one of the four ports: `$2140`-`$2143` as the SNES side sees them, which the chip
    /// reads as `$F4`-`$F7`.
    pub fn write_port(&mut self, index: usize, value: u8) {
        self.chip.write_port(index, value);
    }

    /// The last value the chip wrote back through port `index`.
    pub fn read_port(&mut self, index: usize) -> u8 {
        self.chip.read_port(index)
    }

    /// `SOU_TRN`'s payload written straight into APU RAM at `spc_addr`.
    ///
    /// No port handshake on this path: Pan Docs' `SOU_TRN` moves the block to the APU's memory, and
    /// `docs/SGB-SOUND.md` says where. An address that runs past `$FFFF` wraps the way the chip's own
    /// bus does, so no caller can write outside the 64 KiB.
    pub fn load_block(&mut self, spc_addr: u16, bytes: &[u8]) {
        for (offset, &byte) in bytes.iter().enumerate() {
            let addr = spc_addr.wrapping_add(offset as u16);
            self.chip.ram[usize::from(addr)] = byte;
        }
    }

    /// Start the program at `pc`: the boot ROM's entry path, with the boot-ROM window closed.
    ///
    /// The register file is what the IPL ROM leaves on entry (`A = X = Y = 0`, `N` clear, `Z` set,
    /// `SP = $EF` from reset) — the same state the handshake's `JMP [$0000+X]` hands to a driver.
    pub fn start(&mut self, pc: u16) {
        self.chip.cpu.pc = pc;
        // `$F1` bit 7: the boot ROM is only in the window while it is set.
        self.chip.cpu.control &= 0x7F;
        self.chip.cpu.a = 0;
        self.chip.cpu.x = 0;
        self.chip.cpu.y = 0;
        self.chip.cpu.psw.set_n(false);
        self.chip.cpu.psw.set_z(true);
        self.chip.ipl = Ipl::Running;
    }

    /// Boot the sound driver out of the user's own SGB cartridge image (`docs/SGB-SOUND.md` §1):
    /// copy the five records at the image's table into APU RAM, start the SPC700 at the entry the
    /// table's terminator names, and leave the idle word `$A9A9`/`$A9A9` on the ports.
    ///
    /// This is §1's "for a port, the upload can be skipped": the payloads are the file's own bytes in
    /// file order, so loading them straight into APU RAM is what the 97-frame handshake leaves
    /// behind, and the driver cannot tell the difference. The `$A9A9` message is §4's frame-120
    /// write, the first thing the program says to a driver that is running.
    ///
    /// `Err` is one line and leaves the chip exactly as it was (`Apu::new`): a file whose table is not
    /// the one the document measured is silent, never a wrong program at `$0400`.
    pub fn boot(&mut self, rom: &[u8]) -> Result<(), String> {
        let (records, entry) = driver_records(rom)?;
        for (dest, payload) in &records {
            self.load_block(*dest, payload);
        }
        self.start(entry);
        for index in 0..4 {
            self.write_port(index, IDLE_WORD);
        }
        Ok(())
    }

    /// Play one `SOUND` (`$08`) request: §2's two words on the ports, the `$2140` pair first.
    ///
    /// All four bytes go out raw — the driver decodes pitches, volumes and the mute bit itself — and
    /// the high byte of each pair goes out before its word is complete, the order the program's
    /// 16-bit store produces: `$2141` before `$2140`, then `$2143` before `$2142` (§2).
    pub fn play(&mut self, request: SoundRequest) {
        self.write_port(1, request.effect_a);
        self.write_port(0, request.music);
        self.write_port(3, request.attributes);
        self.write_port(2, request.effect_b);
    }

    /// Take one `SOU_TRN` (`$09`) block: §3's three steps — `$FF` on `$2140` to stop the driver, the
    /// record list's payloads into APU RAM (the direct copy §1 says is equivalent to the uploader's
    /// handshake), then the SPC700 started at the address the terminator names.
    ///
    /// A refused transfer writes nothing at all: half a record list is worse than none, and the block
    /// was not the one Pan Docs describes.
    pub fn transfer(&mut self, transfer: &SoundTransfer) {
        if transfer.refused.is_some() {
            return;
        }
        self.write_port(0, 0xFF);
        for (dest, bytes) in &transfer.blocks {
            self.load_block(*dest, bytes);
        }
        if let Some(entry) = transfer.jump {
            self.start(entry);
        }
    }

    /// Advance the chip by `gb_dots` Game Boy dots (T-cycles).
    ///
    /// The SPC700 runs at 1 024 000 Hz against the Game Boy's 4 194 304 Hz: exactly 125 SPC cycles
    /// per 512 dots. The remainder is carried in [`SgbSound::acc`], so a run of any length lands on
    /// the same count as one call of the sum — no float, no drift.
    pub fn tick(&mut self, gb_dots: u32) {
        self.acc += u64::from(gb_dots) * u64::from(SPC_CLOCK_HZ);
        let cycles = (self.acc / u64::from(SGB_CLOCK_HZ)) as u32;
        self.acc %= u64::from(SGB_CLOCK_HZ);
        if cycles > 0 {
            self.chip.tick(cycles);
        }
    }

    /// Interleaved stereo f32 at the DSP's 32 kHz; returns how many stereo frames were written.
    pub fn read_samples(&mut self, out: &mut [f32]) -> usize {
        self.chip.read_samples(out)
    }

    /// The chip's RAM, for a leg that has to look at what was uploaded.
    pub fn ram(&self) -> &[u8; APU_RAM_SIZE] {
        &self.chip.ram
    }

    /// The chip itself, for a leg that has to look inside it, and for the tests.
    pub fn chip(&self) -> &Apu {
        &self.chip
    }

    /// The chip itself, mutably: what a driver uploaded by `SOU_TRN` runs on, and what the
    /// SGB side reaches when it has to set the DSP up directly.
    pub fn chip_mut(&mut self) -> &mut Apu {
        &mut self.chip
    }

    /// The chip's cycle count: the clock's ledger, which the drift test pins.
    pub fn cycles(&self) -> u64 {
        self.chip.cycles
    }

    /// APU RAM, the chip and the clock's remainder.
    pub fn save_state(&self, w: &mut StateWriter) {
        self.chip.save_state(w);
        w.u64(self.acc);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.chip.load_state(r)?;
        self.acc = r.u64()?;
        Ok(())
    }
}

impl Default for SgbSound {
    fn default() -> Self {
        SgbSound::new()
    }
}

/// What a `SOU_TRN` (`$09`) block said, parsed and not applied.
///
/// Pan Docs, *SGB Command $09 — SOU_TRN*: the command's own packet carries nothing ("Byte 1-F Not
/// used"), and "The sound code/data is sent by VRAM transfer as a contiguous list of packets. All
/// 16-bit values are little-endian." A data packet is a 16-bit size, a 16-bit APU address and then
/// that many bytes; a packet whose size is `$0000` is instead a jump packet — two zero bytes and the
/// address to start the uploaded program at, "use $0400 to safely restart the built-in SGB BIOS'
/// N-SPC sound engine". The jump packet ends the list.
///
/// Nothing here knows what the bytes mean: `docs/SGB-SOUND.md` is what the SGB program
/// uploads, and when and how it is applied is up to the machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SoundTransfer {
    /// Each data packet, in the order it arrived: where it goes in APU RAM and what it is.
    pub blocks: Vec<(u16, Vec<u8>)>,
    /// The jump packet's address, when the list ended with one.
    pub jump: Option<u16>,
    /// Why the block is not a whole transfer, when it is not — one line, and `blocks` still holds
    /// everything that parsed before the fault. `None` is a transfer that ran to its end.
    pub refused: Option<&'static str>,
}

/// Parse the 4 KiB block a `SOU_TRN` command reads.
///
/// A list that ends exactly on the block's last byte is complete: the print side may fill all 4 KiB
/// and stop, with no jump packet. A jump packet ends it early. Anything else — a size that does not
/// fit in what is left, a header with fewer than four bytes, a packet whose last byte is past
/// `$FFFF` — stops the parse and says why in [`SoundTransfer::refused`]. A packet past `$FFFF` is
/// refused rather than wrapped because Pan Docs' sample area ends at `$EEFF`: a block that crosses
/// the top of APU RAM is a malformed transfer, not one meant to spill into the program area.
pub fn parse_transfer(block: &[u8]) -> SoundTransfer {
    let mut parsed = SoundTransfer::default();
    let mut at = 0;
    while at < block.len() {
        if block.len() - at < 4 {
            parsed.refused = Some("a packet header is four bytes");
            break;
        }
        let size = usize::from(block[at]) | (usize::from(block[at + 1]) << 8);
        let address = u16::from(block[at + 2]) | (u16::from(block[at + 3]) << 8);
        if size == 0 {
            parsed.jump = Some(address);
            break;
        }
        let Some(data) = block.get(at + 4..at + 4 + size) else {
            parsed.refused = Some("a packet runs past the 4 KiB block");
            break;
        };
        if usize::from(address) + size > 0x1_0000 {
            parsed.refused = Some("a packet runs past the top of APU RAM");
            break;
        }
        parsed.blocks.push((address, data.to_vec()));
        at += 4 + size;
    }
    parsed
}

/// The payloads [`driver_records`] finds: where each one goes in APU RAM, and its bytes.
type DriverRecords<'a> = Vec<(u16, &'a [u8])>;

/// Walk the SGB program's own boot table at [`DRIVER_TABLE_AT`] (`docs/SGB-SOUND.md` §1): a `len`
/// u16, a `dest` u16 and `len` bytes, repeated, ended by `len = 0` whose `dest` is the entry.
///
/// The walk *is* the check. It reads the file's own numbers rather than a hard-coded length list, and
/// refuses any table whose terminator is not the documented `$3CAD9`/`$0400`: another SGB cartridge
/// would put its driver elsewhere, and copying its bytes to our addresses would run nothing.
///
/// shortcut: no SHA-1 of the image is taken (§5 step 1's strong check), because that needs a new
/// dependency; the table's exact ending is what is checked instead. Add SHA-1 if a second SGB
/// cartridge ever has to be told apart.
fn driver_records(rom: &[u8]) -> Result<(DriverRecords<'_>, u16), String> {
    let mut records = Vec::new();
    let mut at = DRIVER_TABLE_AT;
    loop {
        let Some(header) = rom.get(at..at + 4) else {
            return Err(format!(
                "SGB ROM file: the boot table from ${DRIVER_TABLE_AT:05X} runs off the end of the image"
            ));
        };
        let len = usize::from(header[0]) | (usize::from(header[1]) << 8);
        let dest = u16::from(header[2]) | (u16::from(header[3]) << 8);
        if len == 0 {
            if at != DRIVER_TABLE_END || dest != DRIVER_ENTRY {
                return Err(format!(
                    "SGB ROM file: the boot table ends at ${at:05X}/${dest:04X}, not \
                     ${DRIVER_TABLE_END:05X}/${DRIVER_ENTRY:04X}"
                ));
            }
            return Ok((records, dest));
        }
        let Some(payload) = rom.get(at + 4..at + 4 + len) else {
            return Err(format!(
                "SGB ROM file: a {len}-byte record at ${at:05X} runs off the end of the image"
            ));
        };
        records.push((dest, payload));
        at += 4 + len;
    }
}

/// Read the user's SGB cartridge file and check it is one: 262 144 bytes with `Super GAMEBOY` at
/// `$7FC0`. `Err` is one line for the status bar, and costs the SGB its sound — never the game.
pub fn load_rom(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        std::fs::read(path).map_err(|why| format!("SGB ROM file {}: {why}", path.display()))?;
    if bytes.len() != SGB_ROM_SIZE {
        return Err(format!(
            "SGB ROM file {}: {} bytes, expected {SGB_ROM_SIZE}",
            path.display(),
            bytes.len()
        ));
    }
    if !bytes[SGB_TITLE_AT..].starts_with(SGB_TITLE) {
        return Err(format!(
            "SGB ROM file {}: no `Super GAMEBOY` title at ${SGB_TITLE_AT:04X}",
            path.display()
        ));
    }
    Ok(bytes)
}
