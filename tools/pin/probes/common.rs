//! Helpers shared by the four probes. Plain std: the probe must build against a source checkout
//! and nothing in this repo.
//!
//! Output is pin-file text: `[run]` headers, `key = value` lines, and `# skipped:` comments. A
//! skipped run prints no header, so the pin file only ever holds measurements.

#![allow(dead_code)]

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// FNV-1a 64, the hash every source state file and every pin uses.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    continue_fnv1a64(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continues an FNV-1a 64 hash over more bytes.
pub fn continue_fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// FNV-1a 64 over `words`, each as a little-endian `u16` (the colour-word frame checksum).
pub fn fnv1a64_words(words: &[u16]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for word in words {
        hash = continue_fnv1a64(hash, &word.to_le_bytes());
    }
    hash
}

/// A 64-bit hash as the lower-case, zero-padded hex a pin file stores.
pub fn hex(hash: u64) -> String {
    format!("{hash:016x}")
}

/// What the probe prints: run headers, pins, and skip comments. `measured` counts real pins.
pub struct Report {
    measured: usize,
}

impl Report {
    /// An empty report.
    pub fn new() -> Report {
        Report { measured: 0 }
    }

    /// Starts a run. Nothing is printed for a run with no pins.
    pub fn run(&mut self, name: &str) {
        println!("\n[{name}]");
    }

    /// Prints one `key = value` pin.
    pub fn pin(&mut self, key: impl AsRef<str>, value: impl fmt::Display) {
        println!("{} = {value}", key.as_ref());
        self.measured += 1;
    }

    /// Prints the DESIGN §5.6 skip sentence as a comment, so the pin file still parses.
    pub fn skip(&mut self, why: impl fmt::Display) {
        println!("# skipped: {why}");
    }

    /// Says so when a probe found no ROMs and therefore measured nothing.
    pub fn finish(&self) {
        if self.measured == 0 {
            println!("# no pins measured");
        }
    }
}

/// `TENDY_TEST_ROMS`, if it is set and names a folder. Prints the one clear line and returns
/// `None` otherwise; the caller exits 0 without failing a build.
pub fn test_roms() -> Option<PathBuf> {
    match env::var_os("TENDY_TEST_ROMS") {
        None => {
            println!("# skipped: TENDY_TEST_ROMS is not set (no pins measured)");
            None
        }
        Some(value) => {
            let root = PathBuf::from(value);
            if root.is_dir() {
                Some(root)
            } else {
                println!("# skipped: TENDY_TEST_ROMS is not a folder (no pins measured)");
                None
            }
        }
    }
}

/// A test ROM inside `TENDY_TEST_ROMS`, or the DESIGN §5.6 skip line.
pub fn rom(root: &Path, name: &str) -> Option<PathBuf> {
    let path = root.join(name);
    if path.is_file() {
        Some(path)
    } else {
        println!("# skipped: {name} not in TENDY_TEST_ROMS");
        None
    }
}

/// Reads a file or panics with its path: a ROM that is present but unreadable is a broken probe
/// run, not a skip.
pub fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|why| panic!("cannot read {}: {why}", path.display()))
}

/// Where the synthetic SPC700 image starts in APU RAM (issue #6's sound pin).
pub const SPC_PROGRAM: u16 = 0x0200;
/// Where the synthetic image's sample directory page sits; `DIR` is a page number, so this is
/// `$0300` and the program's 158 bytes at `$0200` never touch it.
pub const SPC_DIR: u16 = 0x0300;
/// Where the synthetic image's one BRR block sits; instrument 0 starts and loops here.
pub const SPC_BRR: u16 = 0x0310;

/// The synthetic SPC700 program and its data, as one image loaded at [`SPC_PROGRAM`].
///
/// No ROM and no cartridge are involved: the program is 26 DSP register writes — `MOV $F2,#reg`
/// then `MOV $F3,#value` (`8F imm dp` on the SPC700) — that give one voice a looping BRR square
/// wave, arm echo with a non-zero FIR, and key the voice on, followed by `BRA *` (`2F FE`):
///
/// - voice 0: SRCN 0, pitch `$1000` (one BRR sample per output sample), direct GAIN `$7F`;
/// - echo: `ESA` `$3000`, `EDL` 1 (512 entries, 16 ms), one voice in (`EON`), `EFB` `$20`,
///   `EVOL` `$30`, FIR taps `$40 $20 $10 $08` then zeros;
/// - `FLG` `$00`: not reset, not muted, echo writes on.
///
/// The image is the same on every host, so both sound pins measure the same program. The two
/// test files (`crates/tendy-snes/tests/synthetic.rs`, `crates/tendy-gb/tests/synthetic.rs`)
/// carry a copy: the pins catch a drift between them immediately.
pub fn spc_image() -> Vec<u8> {
    const WRITES: [(u8, u8); 26] = [
        (0x5D, 0x03), // DIR: directory page $0300
        (0x0C, 0x50), // MVOLL
        (0x1C, 0x50), // MVOLR
        (0x00, 0x50), // V0VOLL
        (0x01, 0x50), // V0VOLR
        (0x02, 0x00), // V0PITCHL
        (0x03, 0x10), // V0PITCHH: $1000, 32 000 Hz
        (0x04, 0x00), // V0SRCN: instrument 0
        (0x05, 0x00), // V0ADSR1: 0 selects GAIN mode
        (0x07, 0x7F), // V0GAIN: direct, maximum
        (0x0F, 0x40), // FIR tap 0 = 64
        (0x1F, 0x20), // FIR tap 1 = 32
        (0x2F, 0x10), // FIR tap 2 = 16
        (0x3F, 0x08), // FIR tap 3 = 8
        (0x4F, 0x00), // FIR tap 4
        (0x5F, 0x00), // FIR tap 5
        (0x6F, 0x00), // FIR tap 6
        (0x7F, 0x00), // FIR tap 7
        (0x2C, 0x30), // EVOLL
        (0x3C, 0x30), // EVOLR
        (0x0D, 0x20), // EFB
        (0x6D, 0x30), // ESA: echo buffer at $3000
        (0x7D, 0x01), // EDL: 512 entries (16 ms)
        (0x4D, 0x01), // EON: voice 0 feeds the echo
        (0x6C, 0x00), // FLG: reset, mute and echo-write-disable all clear
        (0x4C, 0x01), // KON: voice 0 on
    ];
    let len = usize::from(SPC_BRR + 9 - SPC_PROGRAM);
    let mut image = vec![0u8; len];
    let mut at = 0;
    for (reg, value) in WRITES {
        image[at..at + 3].copy_from_slice(&[0x8F, reg, 0xF2]);
        image[at + 3..at + 6].copy_from_slice(&[0x8F, value, 0xF3]);
        at += 6;
    }
    image[at..at + 2].copy_from_slice(&[0x2F, 0xFE]);
    let dir = usize::from(SPC_DIR - SPC_PROGRAM);
    image[dir..dir + 4].copy_from_slice(&[
        SPC_BRR as u8,
        (SPC_BRR >> 8) as u8,
        SPC_BRR as u8,
        (SPC_BRR >> 8) as u8,
    ]);
    let brr = usize::from(SPC_BRR - SPC_PROGRAM);
    // Shift 12, filter 0, end+loop (code 3), then a square of +7 and -7 nibbles.
    image[brr] = (12 << 4) | 3;
    image[brr + 1..brr + 9].copy_from_slice(&[0x77, 0x77, 0x99, 0x99, 0x77, 0x77, 0x99, 0x99]);
    image
}

/// The audio pin: FNV-1a 64 over drained samples as little-endian `f32`, in order.
pub struct AudioHash {
    hash: u64,
    peak: f32,
    nonzero: u64,
    samples: u64,
}

impl AudioHash {
    /// A hash over no samples yet.
    pub fn new() -> AudioHash {
        AudioHash {
            hash: 0xcbf2_9ce4_8422_2325,
            peak: 0.0,
            nonzero: 0,
            samples: 0,
        }
    }

    /// Adds one drained batch.
    pub fn feed(&mut self, samples: &[f32]) {
        for sample in samples {
            self.hash = continue_fnv1a64(self.hash, &sample.to_le_bytes());
            let magnitude = sample.abs();
            if magnitude > self.peak {
                self.peak = magnitude;
            }
            if magnitude > 0.0 {
                self.nonzero += 1;
            }
            self.samples += 1;
        }
    }

    /// The pin value.
    pub fn value(&self) -> u64 {
        self.hash
    }

    /// The loudest sample's magnitude, for the report's "is this real sound?" check.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// How many samples were not exactly zero, of how many.
    pub fn nonzero(&self) -> (u64, u64) {
        (self.nonzero, self.samples)
    }
}
