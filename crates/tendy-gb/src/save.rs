//! Battery saves: the `.sav` path, the RAM+RTC body and the atomic replace.

use crate::machine::Machine;
use std::path::{Path, PathBuf};

/// `"<config dir>/saves/<rom stem> [<crc32>].sav"`.
pub fn save_path(config_dir: &Path, rom_stem: &str, rom_crc32: u32) -> PathBuf {
    config_dir
        .join("saves")
        .join(format!("{rom_stem} [{rom_crc32:08x}].sav"))
}

/// `"<rom stem>.state"` beside the ROM.
pub fn state_path(rom: &Path) -> PathBuf {
    rom.with_extension("state")
}

/// `"<rom stem>-<slot>.state"` beside the ROM: one of the nine save-state slots; its
/// thumbnails hang off the same name.
pub fn slot_state_path(rom: &Path, slot: u8) -> PathBuf {
    let base = state_path(rom);
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_owned());
    base.with_file_name(format!("{stem}-{slot}.state"))
}

/// The four-byte marker in front of the RTC footer.
const RTC_MAGIC: [u8; 4] = *b"RTCL";
/// Format version of that footer.
const RTC_VERSION: u8 = 1;
/// The four-byte marker in front of a board-owned footer (MBC6's flash, MBC7's EEPROM).
const BOARD_MAGIC: [u8; 4] = *b"NVOL";
/// Format version of that footer.
const BOARD_VERSION: u8 = 1;
/// Message §5.5 uses for a file it cannot read.
const CORRUPT: &str = "save file is corrupt";

/// The body: cartridge RAM exactly as the chip holds it, then, for a board that owns
/// non-volatile bytes of its own, `b"NVOL"`, the version, a `u32` length and the bytes, then, for a
/// cartridge with a clock, `b"RTCL"`, the version, the five latched registers and the wall clock the
/// file was written at.
///
/// `None` when the cartridge has none of the three. The wall clock is the RTC's own `base_ms`, which
/// is the last whole second the front end's `Machine::tick_rtc` took the counters forward -
/// `encode_save` has no other source for it and deliberately takes no clock argument.
pub fn encode_save(m: &Machine) -> Option<Vec<u8>> {
    let ram = m.battery().unwrap_or(&[]);
    let rtc = m.bus.cart.rtc.as_ref();
    let board = m.bus.cart.board_save();
    if ram.is_empty() && rtc.is_none() && board.is_none() {
        return None;
    }
    let mut out = Vec::with_capacity(ram.len() + 18);
    out.extend_from_slice(ram);
    if let Some(board) = board {
        out.extend_from_slice(&BOARD_MAGIC);
        out.push(BOARD_VERSION);
        out.extend_from_slice(&(board.len() as u32).to_le_bytes());
        out.extend_from_slice(board);
    }
    if let Some(rtc) = rtc {
        out.extend_from_slice(&RTC_MAGIC);
        out.push(RTC_VERSION);
        out.extend_from_slice(&rtc.latched);
        out.extend_from_slice(&rtc.base_ms.to_le_bytes());
    }
    Some(out)
}

/// Reads §5.5's body back. Returns whether anything was loaded; `Err` is the message for the error
/// screen.
///
/// A file of exactly `ram_len` bytes is RAM only; a longer one carries footers, each introduced by
/// its four-byte marker.
pub fn decode_save(m: &mut Machine, bytes: &[u8]) -> Result<bool, String> {
    if bytes.is_empty() {
        return Ok(false);
    }
    let ram_len = m.battery().map_or(0, <[u8]>::len);
    if bytes.len() == ram_len {
        if ram_len > 0 {
            m.load_battery(bytes)?;
        }
        return Ok(true);
    }
    if bytes.len() < ram_len {
        return Err(CORRUPT.to_string());
    }
    let mut board: Option<&[u8]> = None;
    let mut regs = [0u8; 5];
    let mut written = 0u64;
    let mut has_rtc = false;
    let mut pos = ram_len;
    while pos < bytes.len() {
        if bytes.len() - pos < 5 {
            return Err(CORRUPT.to_string());
        }
        let magic = &bytes[pos..pos + 4];
        let version = bytes[pos + 4];
        if magic == RTC_MAGIC {
            if bytes.len() - pos < 4 + 1 + 5 + 8 {
                return Err(CORRUPT.to_string());
            }
            if version != RTC_VERSION {
                return Err(format!(
                    "save file has format version {version}, expected {RTC_VERSION}"
                ));
            }
            regs.copy_from_slice(&bytes[pos + 5..pos + 10]);
            written = u64::from_le_bytes(
                bytes[pos + 10..pos + 18]
                    .try_into()
                    .map_err(|_| CORRUPT.to_string())?,
            );
            has_rtc = true;
            pos += 18;
        } else if magic == BOARD_MAGIC {
            if bytes.len() - pos < 9 {
                return Err(CORRUPT.to_string());
            }
            if version != BOARD_VERSION {
                return Err(format!(
                    "save file has format version {version}, expected {BOARD_VERSION}"
                ));
            }
            let len = u32::from_le_bytes(
                bytes[pos + 5..pos + 9]
                    .try_into()
                    .map_err(|_| CORRUPT.to_string())?,
            ) as usize;
            if bytes.len() - pos < 9 + len {
                return Err(CORRUPT.to_string());
            }
            board = Some(&bytes[pos + 9..pos + 9 + len]);
            pos += 9 + len;
        } else {
            return Err(CORRUPT.to_string());
        }
    }
    if ram_len > 0 {
        m.load_battery(&bytes[..ram_len])?;
    }
    if let Some(board) = board {
        m.bus.cart.load_board_save(board)?;
    }
    if has_rtc {
        m.set_rtc(regs);
        if let Some(rtc) = &mut m.bus.cart.rtc {
            rtc.base_ms = written;
        }
    }
    Ok(true)
}

/// `Session::open`'s half of §5.5: read the file, then take the clock forward over the time the
/// program was closed. `Err` is the message for the error screen.
pub fn load_save(m: &mut Machine, bytes: &[u8], now_ms: u64) -> Result<bool, String> {
    let loaded = decode_save(m, bytes)?;
    m.tick_rtc(now_ms);
    Ok(loaded)
}

/// Temp file + rename, so a crash never truncates a save.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Writes a save state through `write_atomic`.
pub fn store_state(path: &Path, state: &[u8]) -> std::io::Result<()> {
    write_atomic(path, state)
}
