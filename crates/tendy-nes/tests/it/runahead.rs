//! Run-ahead: the machine keeps the committed frame, and the picture the call hands
//! back is `k` frames further on with the committed input held.
//!
//! Every case runs on a hand-built NROM cartridge whose program waits for vblank, latches pad port 1
//! into `$20`, stores it in `$0300`, paints palette entry 0 with `(pad ^ counter) & $3F`, sets
//! PPUMASK's emphasis bits from the pad's top three bits, and counts its own frames in `$10`. The
//! picture therefore changes from frame to frame even while the input is held — which is what makes
//! "the returned picture is frame `t + k`" a claim that can fail rather than a comparison of two
//! identical blank screens.

use tendy_nes::apu::gain::ChannelPan;
use tendy_nes::nes::Nes;
use tendy_nes::rng::SplitMix64;
use tendy_nes::runahead::{Picture, run_ahead_frame};

/// Where the program lives: the last 16 KB bank of a 32 KB NROM image, and what all three vectors
/// point at, so a reset restarts it.
const BASE: u16 = 0xC000;

/// The program: SEI, then a loop of "wait for vblank, latch pad port 1, paint palette entry 0 with
/// the pad XORed with the loop counter, set PPUMASK's emphasis from the pad, count the frame".
///
/// It never enables rendering, so every pixel is the backdrop — palette entry 0 — and `v` is moved out
/// of palette space after the palette write so the backdrop override cannot colour it. The emphasis
/// bits are the only thing in the second plane, and the palette index is the only thing in the first,
/// so the two planes together carry the pad and the frame counter.
fn program(sparse: bool) -> Vec<u8> {
    let mut p: Vec<u8> = Vec::new();
    p.extend_from_slice(&[0x78]); // SEI
    let latch = p.len() as u16;
    p.extend_from_slice(&[0xAD, 0x02, 0x20]); // LDA $2002
    p.extend_from_slice(&[0x10, 0xFB]); // BPL latch (wait for the vblank flag, which the read clears)
    // `sparse`: INC $11 / LDA $11 / AND #$03 / BNE past the pad read, so the pad is read on every
    // fourth frame only. The lag flag is then set on some frames and clear on the others.
    let skip = sparse.then(|| {
        p.extend_from_slice(&[0xE6, 0x11, 0xA5, 0x11, 0x29, 0x03, 0xD0, 0x00]);
        p.len() - 1
    });
    p.extend_from_slice(&[0xA9, 0x01, 0x8D, 0x16, 0x40]); // LDA #$01 / STA $4016 (strobe high)
    p.extend_from_slice(&[0xA9, 0x00, 0x8D, 0x16, 0x40]); // LDA #$00 / STA $4016 (shift mode)
    for _ in 0..8 {
        p.extend_from_slice(&[0xAD, 0x16, 0x40]); // LDA $4016
        p.extend_from_slice(&[0x4A]); // LSR A
        p.extend_from_slice(&[0x26, 0x20]); // ROL $20: the first read's bit lands in bit 0
    }
    if let Some(at) = skip {
        p[at] = (p.len() - (at + 1)) as u8;
    }
    p.extend_from_slice(&[0xA9, 0x3F, 0x8D, 0x06, 0x20]); // LDA #$3F / STA $2006
    p.extend_from_slice(&[0xA9, 0x00, 0x8D, 0x06, 0x20]); // LDA #$00 / STA $2006 ($3F00)
    p.extend_from_slice(&[0xA5, 0x20, 0x45, 0x10]); // LDA $20 / EOR $10
    p.extend_from_slice(&[0x29, 0x3F, 0x8D, 0x07, 0x20]); // AND #$3F / STA $2007
    p.extend_from_slice(&[0xA9, 0x20, 0x8D, 0x06, 0x20]); // LDA #$20 / STA $2006
    p.extend_from_slice(&[0xA9, 0x00, 0x8D, 0x06, 0x20]); // LDA #$00 / STA $2006 ($2000)
    p.extend_from_slice(&[0xA5, 0x20, 0x8D, 0x00, 0x03]); // LDA $20 / STA $0300 (pad latch)
    p.extend_from_slice(&[0xA5, 0x20, 0x29, 0xE0]); // LDA $20 / AND #$E0
    p.extend_from_slice(&[0x8D, 0x01, 0x20]); // STA $2001 (emphasis only: rendering stays off)
    p.extend_from_slice(&[0xE6, 0x10]); // INC $10 (frame counter)
    let back = BASE + latch;
    p.extend_from_slice(&[0x4C, back as u8, (back >> 8) as u8]); // JMP latch
    p
}

/// A 32 KB PRG + 8 KB CHR NROM image with the program at `$C000` and every vector pointing at it.
fn synthetic_rom() -> Vec<u8> {
    image(false)
}

/// The same machine with the pad read on every fourth frame only (see `program`).
fn sparse_rom() -> Vec<u8> {
    image(true)
}

fn image(sparse: bool) -> Vec<u8> {
    let mut rom = vec![
        0x4E, 0x45, 0x53, 0x1A, // "NES\x1a"
        2, 1, // 32 KB PRG, 8 KB CHR
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // mapper 0, horizontal mirroring
    ];
    let mut prg = vec![0u8; 0x8000];
    let code = program(sparse);
    prg[0x4000..0x4000 + code.len()].copy_from_slice(&code);
    for vector in [0x7FFA, 0x7FFC, 0x7FFE] {
        prg[vector] = BASE as u8;
        prg[vector + 1] = (BASE >> 8) as u8;
    }
    rom.extend_from_slice(&prg);
    rom.resize(rom.len() + 0x2000, 0); // CHR
    rom
}

/// The seeded pad script: one splitmix64 draw per frame, derived from the frame number so that two
/// runs of the same length see the same byte at the same frame however they got there.
fn pad(seed: u64, frame: usize) -> u8 {
    let mut rng = SplitMix64::new(seed ^ (frame as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    rng.next_u8()
}

const SEED: u64 = 0x84A0_0000_5EED_0001;

fn script(seed: u64, frames: usize) -> Vec<u8> {
    (0..frames).map(|frame| pad(seed, frame)).collect()
}

/// Run `script` from power-on, one entry per frame, then `extra` more frames with `held` on pad 1, and
/// return the picture the PPU ends on. `extra = 1` is the committed frame alone; `extra = k + 1` is
/// the committed frame plus `k` held ones, i.e. the frame `k` past the committed one.
fn picture_after(rom: &[u8], script: &[u8], held: u8, extra: usize) -> Picture {
    let mut nes = Nes::new(rom).unwrap();
    for &p in script {
        nes.bus.pads[0].buttons = p;
        nes.bus.pads[1].buttons = 0;
        nes.run_frame();
    }
    nes.bus.pads[0].buttons = held;
    nes.bus.pads[1].buttons = 0;
    for _ in 0..extra {
        nes.run_frame();
    }
    Picture::of(&nes)
}

/// The straight run of `script` with no run-ahead, returning the state after the last frame.
fn straight_state(rom: &[u8], script: &[u8]) -> Vec<u8> {
    let mut nes = Nes::new(rom).unwrap();
    for &p in script {
        nes.bus.pads[0].buttons = p;
        nes.bus.pads[1].buttons = 0;
        nes.run_frame();
    }
    nes.save_state()
}

/// Bit patterns, so "the samples are equal" is bit for bit and not just approximate: two runs that
/// should be identical cannot pass on `-0.0 == 0.0`.
fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

/// The picture clause, k = 1: each call displays frame `t + 1` with frame `t`'s pad held, and
/// leaves the machine (and the PPU's own planes) on the committed frame.
#[test]
fn the_returned_picture_is_the_frame_k_ahead() {
    let rom = synthetic_rom();
    let script = script(SEED, 48);
    let mut nes = Nes::new(&rom).unwrap();
    let mut pictures = Vec::new();
    let mut ahead = 0;
    for t in 0..script.len() {
        nes.bus.pads[0].buttons = script[t];
        nes.bus.pads[1].buttons = 0;
        let picture = run_ahead_frame(&mut nes, 1, |n| n.run_frame());
        assert_eq!(picture.len(), 61_440, "one plane is 256 x 240");
        assert_eq!(picture.emphasis.len(), 61_440);
        assert_eq!(
            picture,
            picture_after(&rom, &script[..t], script[t], 2),
            "call {t}: the picture is frame t+1 with the input held"
        );
        let committed = picture_after(&rom, &script[..t], script[t], 1);
        assert_eq!(
            Picture::of(&nes),
            committed,
            "call {t}: the machine is left on the committed frame, picture included"
        );
        if picture != committed {
            ahead += 1;
        }
        pictures.push(picture);
    }
    // The pictures are not all the same frame: without this the equality above could hold on a
    // machine that never draws anything.
    let distinct: std::collections::HashSet<&Vec<u8>> =
        pictures.iter().map(|p| &p.framebuffer).collect();
    assert!(
        distinct.len() >= script.len() / 2,
        "{} distinct pictures over {} calls",
        distinct.len(),
        script.len()
    );
    assert!(
        ahead >= script.len() / 2,
        "{ahead} of {} calls displayed a picture the committed frame does not show",
        script.len()
    );
}

/// k = 2: the picture is two frames past the committed one, and the machine still stops after one.
#[test]
fn k_two_looks_two_frames_ahead() {
    let rom = synthetic_rom();
    let script = script(SEED + 1, 24);
    let mut nes = Nes::new(&rom).unwrap();
    let mut straight = Nes::new(&rom).unwrap();
    for t in 0..script.len() {
        nes.bus.pads[0].buttons = script[t];
        nes.bus.pads[1].buttons = 0;
        straight.bus.pads[0].buttons = script[t];
        straight.bus.pads[1].buttons = 0;
        let picture = run_ahead_frame(&mut nes, 2, |n| n.run_frame());
        straight.run_frame();
        assert_eq!(
            picture,
            picture_after(&rom, &script[..t], script[t], 3),
            "call {t}: the picture is frame t+2"
        );
        assert_ne!(
            picture,
            picture_after(&rom, &script[..t], script[t], 2),
            "call {t}: frame t+2 is not frame t+1"
        );
        assert_eq!(
            nes.save_state(),
            straight.save_state(),
            "call {t}: two speculative frames still leave the machine one frame on"
        );
    }
}

/// The state clause, k = 1: N calls are exactly a straight run of N frames with the script.
#[test]
fn n_calls_with_k_one_equal_a_straight_run_of_n_frames() {
    let rom = synthetic_rom();
    let script = script(SEED + 2, 120);
    let mut nes = Nes::new(&rom).unwrap();
    for &p in &script {
        nes.bus.pads[0].buttons = p;
        nes.bus.pads[1].buttons = 0;
        run_ahead_frame(&mut nes, 1, |n| n.run_frame());
    }
    assert_eq!(nes.save_state(), straight_state(&rom, &script));
}

/// k = 0 turns run-ahead off without changing the call shape: one frame, one picture, nothing saved
/// or restored that a caller could notice.
#[test]
fn k_zero_is_a_plain_frame() {
    let rom = synthetic_rom();
    let script = script(SEED + 3, 40);
    let mut nes = Nes::new(&rom).unwrap();
    let mut straight = Nes::new(&rom).unwrap();
    for (t, &p) in script.iter().enumerate() {
        nes.bus.pads[0].buttons = p;
        nes.bus.pads[1].buttons = 0;
        straight.bus.pads[0].buttons = p;
        straight.bus.pads[1].buttons = 0;
        let picture = run_ahead_frame(&mut nes, 0, |n| n.run_frame());
        straight.run_frame();
        assert_eq!(
            picture,
            Picture::of(&nes),
            "call {t}: the picture is the frame just run"
        );
        assert_eq!(picture, Picture::of(&straight));
        assert_eq!(nes.save_state(), straight.save_state(), "call {t}");
    }
}

/// The audio clause: one displayed frame drains exactly the committed frame's samples, bit for
/// bit, however many frames were speculated.
#[test]
fn only_the_committed_frames_samples_survive() {
    let rom = synthetic_rom();
    let script = script(SEED + 4, 40);
    let mut straight = Nes::new(&rom).unwrap();
    let mut expected = Vec::new();
    for &p in &script {
        straight.bus.pads[0].buttons = p;
        straight.bus.pads[1].buttons = 0;
        straight.run_frame();
        expected.push(bits(&std::mem::take(&mut straight.bus.apu.samples)));
    }

    let mut nes = Nes::new(&rom).unwrap();
    for (t, &p) in script.iter().enumerate() {
        nes.bus.pads[0].buttons = p;
        nes.bus.pads[1].buttons = 0;
        run_ahead_frame(&mut nes, 2, |n| n.run_frame());
        let drained = std::mem::take(&mut nes.bus.apu.samples);
        assert!(!drained.is_empty(), "call {t}: the APU produced samples");
        assert_eq!(
            bits(&drained),
            expected[t],
            "call {t}: {} samples against the straight run's {}",
            drained.len(),
            expected[t].len()
        );
    }
}

/// A `run_one` that moves the pads on every call still leaves the machine on the committed frame: the
/// speculation runs on whatever the closure feeds it, and the restore brings the committed pads (and
/// with them the committed picture) back along with everything else.
#[test]
fn a_run_one_that_moves_the_pads_still_keeps_the_committed_frame() {
    let rom = synthetic_rom();
    let mut nes = Nes::new(&rom).unwrap();
    let mut next = 0;
    let picture = run_ahead_frame(&mut nes, 2, |n| {
        n.bus.pads[0].buttons = pad(SEED + 5, next);
        n.bus.pads[1].buttons = 0;
        next += 1;
        n.run_frame();
    });
    assert_eq!(
        next, 3,
        "run_one ran the committed frame and two speculative ones"
    );
    assert_eq!(
        picture,
        picture_after(
            &rom,
            &[pad(SEED + 5, 0), pad(SEED + 5, 1)],
            pad(SEED + 5, 2),
            1
        ),
        "the picture is the third frame the closure ran, not the committed one"
    );
    assert_eq!(
        nes.save_state(),
        {
            let mut one = Nes::new(&rom).unwrap();
            one.bus.pads[0].buttons = pad(SEED + 5, 0);
            one.run_frame();
            one.save_state()
        },
        "the machine is one frame on, with the committed pad"
    );
}

/// With a pan set (stereo on), two frames of run-ahead give the same left and right
/// audio as a plain run, frame for frame. `load_state` clears `samples_right` and resets the
/// right-hand chain, so the restore has to put both back as well as the left samples.
#[test]
fn stereo_audio_is_the_same_with_run_ahead() {
    let rom = synthetic_rom();
    let script = script(SEED, 60);
    let mut pan = ChannelPan::default();
    pan.apply_list("pulse1=-1,triangle=0.5").unwrap();
    let mut plain = Nes::new(&rom).unwrap();
    plain.bus.apu.pan = pan;
    let mut ahead = Nes::new(&rom).unwrap();
    ahead.bus.apu.pan = pan;
    for (t, &p) in script.iter().enumerate() {
        for nes in [&mut plain, &mut ahead] {
            nes.bus.pads[0].buttons = p;
            nes.bus.pads[1].buttons = 0;
        }
        plain.run_frame();
        let left0 = std::mem::take(&mut plain.bus.apu.samples);
        let right0 = std::mem::take(&mut plain.bus.apu.samples_right);
        run_ahead_frame(&mut ahead, 2, |n| n.run_frame());
        let left2 = std::mem::take(&mut ahead.bus.apu.samples);
        let right2 = std::mem::take(&mut ahead.bus.apu.samples_right);
        assert!(
            !right0.is_empty(),
            "call {t}: the plain run has a right channel"
        );
        assert_eq!(left2.len(), left0.len(), "call {t}: left channel length");
        assert_eq!(right2.len(), right0.len(), "call {t}: right channel length");
        assert!(
            bits(&left2) == bits(&left0),
            "call {t}: left channel differs"
        );
        assert!(
            bits(&right2) == bits(&right0),
            "call {t}: right channel differs"
        );
    }
}

/// The lag flag main reads after a run-ahead frame is the committed frame's, frame
/// for frame. The sparse ROM reads the pad on every fourth frame, so the flag changes from frame to
/// frame, and the speculative frames (which read it on other frames) must not leak into it.
#[test]
fn the_lag_flag_after_run_ahead_is_the_committed_frames() {
    let rom = sparse_rom();
    let script = script(SEED, 60);
    let mut plain = Nes::new(&rom).unwrap();
    let mut ahead = Nes::new(&rom).unwrap();
    let mut flags = Vec::new();
    for &p in &script {
        for nes in [&mut plain, &mut ahead] {
            nes.bus.pads[0].buttons = p;
            nes.bus.pads[1].buttons = 0;
            nes.bus.frame_pads_polled = false; // what the frontend does before each frame
        }
        plain.run_frame();
        run_ahead_frame(&mut ahead, 2, |n| n.run_frame());
        flags.push(plain.bus.frame_pads_polled);
        assert_eq!(
            ahead.bus.frame_pads_polled,
            plain.bus.frame_pads_polled,
            "frame {}: the lag flag",
            flags.len() - 1
        );
    }
    assert!(
        flags.contains(&true) && flags.contains(&false),
        "the ROM polls on some frames only"
    );
}
