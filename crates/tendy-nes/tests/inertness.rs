//! The inertness harness (DESIGN §5.6): every `nes/*` run in
//! `baselines/pins.toml`, re-measured against the imported code.
//!
//! The measurements are the probe's, byte for byte (`tools/pin/probes/nes.rs`): frame checksums
//! over the framebuffer then the emphasis plane, the two-channel audio hash (left samples then
//! right, in order, as little-endian `f32`), the state's length and hash, and the roundtrip
//! sentence. A moved file that changed behaviour moves one of these.
//!
//! The ROMs are public test assets and are never committed (DESIGN §10): point `TENDY_TEST_ROMS`
//! at the folder that holds `apu_test/` and (for the FDS run) `fds-bios.rom` and
//! `fds-game.fds`.

use std::collections::BTreeMap;

use tendy_core::pin;
use tendy_nes::console::ConsoleConfig;
use tendy_nes::nes::Nes;

/// The pins were taken at this rate.
const SAMPLE_RATE: f64 = 48_000.0;
/// The first manifest entry that makes sound: Blargg's APU length-counter test.
const CART_ROM: &str = "apu_test/rom_singles/1-len_ctr.nes";

// ---------------------------------------------------------------------------------------------
// The probe's helpers, unchanged
// ---------------------------------------------------------------------------------------------

fn continue_fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// FNV-1a 64 over the framebuffer and then the emphasis plane.
fn frame_hash(nes: &Nes) -> u64 {
    let hash = continue_fnv1a64(0xcbf2_9ce4_8422_2325, &nes.bus.ppu.framebuffer);
    continue_fnv1a64(hash, &nes.bus.ppu.emphasis)
}

fn hex(hash: u64) -> String {
    format!("{hash:016x}")
}

/// The audio pin: FNV-1a 64 over the drained samples as little-endian `f32`, in order.
struct AudioHash {
    hash: u64,
    peak: f32,
}

impl AudioHash {
    fn new() -> AudioHash {
        AudioHash {
            hash: 0xcbf2_9ce4_8422_2325,
            peak: 0.0,
        }
    }

    fn feed(&mut self, samples: &[f32]) {
        for sample in samples {
            self.hash = continue_fnv1a64(self.hash, &sample.to_le_bytes());
            let magnitude = sample.abs();
            if magnitude > self.peak {
                self.peak = magnitude;
            }
        }
    }

    fn value(&self) -> u64 {
        self.hash
    }

    fn peak(&self) -> f32 {
        self.peak
    }
}

fn advance(nes: &mut Nes, frames: u64, mut audio: Option<&mut AudioHash>) {
    for _ in 0..frames {
        nes.run_frame();
        if let Some(audio) = audio.as_deref_mut() {
            audio.feed(&nes.bus.apu.samples);
            audio.feed(&nes.bus.apu.samples_right);
            nes.bus.apu.samples.clear();
            nes.bus.apu.samples_right.clear();
        }
    }
}

fn state_pins(out: &mut BTreeMap<String, String>, nes: &Nes, at: u64) {
    let blob = nes.save_state();
    out.insert(
        format!("state@{at}"),
        hex(continue_fnv1a64(0xcbf2_9ce4_8422_2325, &blob)),
    );
    out.insert(format!("state_len@{at}"), blob.len().to_string());
}

fn roundtrip(out: &mut BTreeMap<String, String>, nes: &mut Nes, at: u64) {
    let blob = nes.save_state();
    advance(nes, 60, None);
    let expected = frame_hash(nes);
    let value = match nes.load_state(&blob) {
        Ok(()) => {
            advance(nes, 60, None);
            if frame_hash(nes) == expected {
                "ok".to_string()
            } else {
                "mismatch".to_string()
            }
        }
        Err(why) => format!("load refused: {why:?}"),
    };
    out.insert(format!("roundtrip@{at}"), value);
}

/// A one-song NSF whose PLAY routine starts pulse 1 on a $700 period. INIT is a bare RTS.
fn nsf_image() -> Vec<u8> {
    let program: [u8; 22] = [
        0x60, // $8000 INIT: rts
        0xA9, 0xBF, 0x8D, 0x00,
        0x40, // $8001 PLAY: lda #$bf; sta $4000 (duty 2, halt, volume 15)
        0xA9, 0x08, 0x8D, 0x01, 0x40, // lda #$08; sta $4001 (sweep off)
        0xA9, 0x40, 0x8D, 0x02, 0x40, // lda #$40; sta $4002 (period low)
        0xA9, 0x00, 0x8D, 0x03, 0x40, // lda #$00; sta $4003 (period high, restart)
        0x60, // rts
    ];
    let mut nsf = vec![0u8; 0x80];
    nsf[0..5].copy_from_slice(b"NESM\x1A");
    nsf[5] = 1; // version
    nsf[6] = 1; // songs
    nsf[7] = 1; // starting song
    nsf[8..10].copy_from_slice(&0x8000u16.to_le_bytes()); // load address
    nsf[10..12].copy_from_slice(&0x8000u16.to_le_bytes()); // INIT
    nsf[12..14].copy_from_slice(&0x8001u16.to_le_bytes()); // PLAY
    nsf[0x0E..0x17].copy_from_slice(b"tendy pin");
    nsf.extend_from_slice(&program);
    nsf
}

// ---------------------------------------------------------------------------------------------
// The pin file and the ROM folder
// ---------------------------------------------------------------------------------------------

fn pinned(run: &str) -> Vec<pin::Pin> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../baselines/pins.toml");
    let text = std::fs::read_to_string(path).expect("baselines/pins.toml");
    pin::parse(&text)
        .expect("baselines/pins.toml parses")
        .into_iter()
        .filter(|p| p.run == run)
        .collect()
}

/// `TENDY_TEST_ROMS`: the shared public test-ROM folder the pins were taken from.
fn rom_root() -> std::path::PathBuf {
    let value = std::env::var_os("TENDY_TEST_ROMS").unwrap_or_else(|| {
        panic!("TENDY_TEST_ROMS must name the public test-ROM folder (DESIGN §5.6)")
    });
    std::path::PathBuf::from(value)
}

fn read_rom(name: &str) -> Vec<u8> {
    let path = rom_root().join(name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (listed in scripts/test-roms.txt)", path.display()))
}

fn rom_opt(name: &str) -> Option<Vec<u8>> {
    std::fs::read(rom_root().join(name)).ok()
}

/// Compares every pinned key of `run` and insists the re-measurement produced no key the pin file
/// lacks: a run that silently stopped measuring a value would otherwise pass.
fn assert_run(run: &str, measured: &BTreeMap<String, String>) {
    let pins = pinned(run);
    assert!(!pins.is_empty(), "baselines/pins.toml has no [{run}] run");
    for pin in &pins {
        let got = measured.get(&pin.key).unwrap_or_else(|| {
            panic!(
                "{run}: {} is pinned but the harness does not measure it",
                pin.key
            )
        });
        assert_eq!(got, &pin.value, "{run}: {} moved", pin.key);
    }
    let extra: Vec<&str> = measured
        .keys()
        .filter(|key| !pins.iter().any(|p| &p.key == *key))
        .map(String::as_str)
        .collect();
    assert!(
        extra.is_empty(),
        "{run}: measured keys the pin file lacks: {extra:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// The runs
// ---------------------------------------------------------------------------------------------

fn cart_measurements() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let bytes = read_rom(CART_ROM);
    let mut nes = Nes::new(&bytes).expect("the APU test is an iNES cartridge");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut nes, 60, Some(&mut audio));
    out.insert("frame@60".to_string(), hex(frame_hash(&nes)));
    advance(&mut nes, 540, Some(&mut audio));
    out.insert("frame@600".to_string(), hex(frame_hash(&nes)));
    advance(&mut nes, 2400, Some(&mut audio));
    out.insert("frame@3000".to_string(), hex(frame_hash(&nes)));
    out.insert("audio".to_string(), hex(audio.value()));
    out.insert("audio_peak".to_string(), format!("{:.6}", audio.peak()));
    state_pins(&mut out, &nes, 3000);
    roundtrip(&mut out, &mut nes, 3000);
    out
}

fn nsf_measurements() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let bytes = nsf_image();
    let mut nes = Nes::from_nsf(&bytes, 1, &ConsoleConfig::default())
        .expect("the harness's own NSF is valid");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut nes, 600, Some(&mut audio));
    out.insert("audio@600".to_string(), hex(audio.value()));
    out.insert("audio_peak@600".to_string(), format!("{:.6}", audio.peak()));
    roundtrip(&mut out, &mut nes, 600);
    out
}

#[test]
fn nes_pins_cart() {
    assert_run("nes/cart", &cart_measurements());
}

#[test]
fn nes_pins_nsf() {
    assert_run("nes/nsf", &nsf_measurements());
}

/// The FDS run was skipped when the pins were taken (`fds-bios.rom` was not in `TENDY_TEST_ROMS`),
/// so there is nothing to compare unless a `[nes/fds]` run appears in the pin file. With the
/// BIOS and disk present it still checks the roundtrip rule, which has no ROM-specific
/// value, and prints the measurements a future pin run would record.
#[test]
fn nes_pins_fds() {
    let (Some(bios), Some(disk)) = (rom_opt("fds-bios.rom"), rom_opt("fds-game.fds")) else {
        println!(
            "fds: SKIPPED (no fds-bios.rom or fds-game.fds in TENDY_TEST_ROMS; no pins were taken)"
        );
        return;
    };
    let mut out = BTreeMap::new();
    let mut nes = Nes::from_fds(&disk, &bios, false).expect("the FDS BIOS and disk load");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    advance(&mut nes, 600, None);
    out.insert("frame@600".to_string(), hex(frame_hash(&nes)));
    state_pins(&mut out, &nes, 600);
    roundtrip(&mut out, &mut nes, 600);
    assert_eq!(
        out.get("roundtrip@600").map(String::as_str),
        Some("ok"),
        "fds roundtrip"
    );
    if pinned("nes/fds").is_empty() {
        println!("fds: measured, but baselines/pins.toml has no [nes/fds] run: {out:?}");
    } else {
        assert_run("nes/fds", &out);
    }
}

/// The roundtrip rule on its own, from a ROM built in code: a state saved mid-run must, after a
/// reload, replay to the same picture sixty frames later.
#[test]
fn nes_pins_roundtrip() {
    let bytes = nsf_image();
    let mut nes = Nes::from_nsf(&bytes, 1, &ConsoleConfig::default())
        .expect("the harness's own NSF is valid");
    for _ in 0..120 {
        nes.run_frame();
    }
    let blob = nes.save_state();
    for _ in 0..60 {
        nes.run_frame();
    }
    let expected = frame_hash(&nes);
    nes.load_state(&blob).expect("the saved state loads");
    for _ in 0..60 {
        nes.run_frame();
    }
    assert_eq!(
        frame_hash(&nes),
        expected,
        "load_state did not reproduce the run"
    );
}
