//! NES pins (DESIGN §5.6): an iNES cartridge, an NSF built in code, and (when the user has
//! supplied the BIOS and a disk image) an FDS disk.

mod common;

use common::{AudioHash, Report, continue_fnv1a64, fnv1a64, hex, read, rom, test_roms};
use nes::nes::Nes;

const SAMPLE_RATE: f64 = 48_000.0;
/// The first manifest entry that makes sound: Blargg's APU length-counter test.
const CART_ROM: &str = "apu_test/rom_singles/1-len_ctr.nes";

fn main() {
    let Some(root) = test_roms() else { return };
    let mut report = Report::new();

    cart(&root, &mut report);
    nsf(&mut report);
    fds(&root, &mut report);
    report.finish();
}

/// fnv1a64 over the framebuffer and then the emphasis plane.
fn frame_hash(nes: &Nes) -> u64 {
    let hash = fnv1a64(&nes.bus.ppu.framebuffer);
    continue_fnv1a64(hash, &nes.bus.ppu.emphasis)
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

fn state_pins(report: &mut Report, nes: &Nes, at: u64) {
    let blob = nes.save_state();
    report.pin(format!("state@{at}"), hex(fnv1a64(&blob)));
    report.pin(format!("state_len@{at}"), blob.len());
}

fn roundtrip(report: &mut Report, nes: &mut Nes, at: u64) {
    let blob = nes.save_state();
    advance(nes, 60, None);
    let expected = frame_hash(nes);
    match nes.load_state(&blob) {
        Ok(()) => {
            advance(nes, 60, None);
            let value = if frame_hash(nes) == expected {
                "ok"
            } else {
                "mismatch"
            };
            report.pin(format!("roundtrip@{at}"), value);
        }
        Err(why) => report.pin(format!("roundtrip@{at}"), format!("load refused: {why:?}")),
    }
}

fn cart(root: &std::path::Path, report: &mut Report) {
    if rom(root, CART_ROM).is_none() {
        return;
    }
    report.run("nes/cart");
    let bytes = read(&root.join(CART_ROM));
    let mut nes = Nes::new(&bytes).expect("the APU test is an iNES cartridge");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut nes, 60, Some(&mut audio));
    report.pin("frame@60", hex(frame_hash(&nes)));
    advance(&mut nes, 540, Some(&mut audio));
    report.pin("frame@600", hex(frame_hash(&nes)));
    advance(&mut nes, 2400, Some(&mut audio));
    report.pin("frame@3000", hex(frame_hash(&nes)));
    report.pin("audio", hex(audio.value()));
    report.pin("audio_peak", format!("{:.6}", audio.peak()));
    state_pins(report, &nes, 3000);
    roundtrip(report, &mut nes, 3000);
}

fn nsf(report: &mut Report) {
    report.run("nes/nsf");
    let bytes = nsf_image();
    let mut nes = Nes::from_nsf(&bytes, 1, &nes::console::ConsoleConfig::default())
        .expect("the probe's own NSF is valid");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    let mut audio = AudioHash::new();
    advance(&mut nes, 600, Some(&mut audio));
    report.pin("audio@600", hex(audio.value()));
    report.pin("audio_peak@600", format!("{:.6}", audio.peak()));
    roundtrip(report, &mut nes, 600);
}

fn fds(root: &std::path::Path, report: &mut Report) {
    let Some(bios) = rom(root, "fds-bios.rom") else {
        return;
    };
    let Some(disk) = rom(root, "fds-game.fds") else {
        return;
    };
    report.run("nes/fds");
    let mut nes = Nes::from_fds(&read(&disk), &read(&bios), false)
        .expect("the user's FDS BIOS and disk load");
    nes.bus.apu.set_sample_rate(SAMPLE_RATE);
    advance(&mut nes, 600, None);
    report.pin("frame@600", hex(frame_hash(&nes)));
    state_pins(report, &nes, 600);
    roundtrip(report, &mut nes, 600);
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
