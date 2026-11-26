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
