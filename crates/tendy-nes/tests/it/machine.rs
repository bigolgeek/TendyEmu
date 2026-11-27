//! The `Machine` wrapper's own mappings (DESIGN §8): what the front end will call, tested
//! outside the imported core's suite.

use tendy_core::{Buttons, Machine};

use tendy_nes::apu::gain::Channel;
use tendy_nes::console::ConsoleConfig;
use tendy_nes::machine_impl::{NesImage, NesMachine};

use crate::common;

/// A one-song NSF whose PLAY routine starts pulse 1; INIT is a bare RTS.
fn nsf_image() -> Vec<u8> {
    let program: [u8; 22] = [
        0x60, 0xA9, 0xBF, 0x8D, 0x00, 0x40, 0xA9, 0x08, 0x8D, 0x01, 0x40, 0xA9, 0x40, 0x8D, 0x02,
        0x40, 0xA9, 0x00, 0x8D, 0x03, 0x40, 0x60,
    ];
    let mut nsf = vec![0u8; 0x80];
    nsf[0..5].copy_from_slice(b"NESM\x1A");
    nsf[5] = 1;
    nsf[6] = 1;
    nsf[7] = 1;
    nsf[8..10].copy_from_slice(&0x8000u16.to_le_bytes());
    nsf[10..12].copy_from_slice(&0x8000u16.to_le_bytes());
    nsf[12..14].copy_from_slice(&0x8001u16.to_le_bytes());
    nsf.extend_from_slice(&program);
    nsf
}

fn cart() -> NesMachine {
    NesMachine::new(
        common::loop_rom(0, 0),
        NesImage::Cartridge,
        None,
        ConsoleConfig::default(),
    )
    .expect("the harness's own NROM image loads")
}

fn nsf() -> NesMachine {
    NesMachine::new(
        nsf_image(),
        NesImage::Nsf { song: 1 },
        None,
        ConsoleConfig::default(),
    )
    .expect("the harness's own NSF loads")
}

#[test]
fn nes_buttons_map() {
    let mut machine = cart();
    for (button, bit) in [
        (Buttons::A, 0x01),
        (Buttons::B, 0x02),
        (Buttons::SELECT, 0x04),
        (Buttons::START, 0x08),
        (Buttons::UP, 0x10),
        (Buttons::DOWN, 0x20),
        (Buttons::LEFT, 0x40),
        (Buttons::RIGHT, 0x80),
    ] {
        machine.set_buttons(button);
        assert_eq!(machine.nes.bus.pads[0].buttons, bit);
        assert_eq!(machine.nes.bus.pads[1].buttons, 0, "player 2 is untouched");
    }
    machine.set_buttons(Buttons::A.with(Buttons::START));
    assert_eq!(machine.nes.bus.pads[0].buttons, 0x09);
    machine.set_buttons(Buttons::UP.with(Buttons::DOWN));
    assert_eq!(
        machine.nes.bus.pads[0].buttons, 0,
        "opposing D-pad bits drop"
    );
    machine.set_buttons(Buttons::NONE);
    assert_eq!(machine.nes.bus.pads[0].buttons, 0);
}

#[test]
fn nes_drain_interleaves() {
    let mut machine = nsf();
    machine.set_sample_rate(48_000);
    assert_eq!(machine.sample_rate(), 48_000);
    machine.run_frame();
    machine.run_frame();
    let mut out = vec![0.0f32; 4096 * 2];
    let frames = machine.drain_audio(&mut out);
    assert!(frames > 0, "two frames of APU output");
    assert!(frames <= 4096);
    // An NSF's APU is mono, so the wrapper repeats it on both sides.
    for pair in out[..frames * 2].as_chunks::<2>().0 {
        assert_eq!(pair[0], pair[1]);
    }
    assert_eq!(machine.drain_audio(&mut out), 0, "the buffer drained");
}

#[test]
fn nes_channel_mask_mutes() {
    let mut machine = nsf();
    machine.set_channel_mask(0);
    for channel in Channel::ALL {
        assert_eq!(machine.nes.bus.apu.gain.get(channel), 0.0);
    }
    machine.set_channel_mask(0x3F);
    for channel in Channel::ALL {
        assert_eq!(machine.nes.bus.apu.gain.get(channel), 1.0);
    }
    // Bit 0 is pulse 1; the rest stay silent.
    machine.set_channel_mask(0x01);
    assert_eq!(machine.nes.bus.apu.gain.get(Channel::Pulse1), 1.0);
    for channel in Channel::ALL.into_iter().filter(|c| *c != Channel::Pulse1) {
        assert_eq!(machine.nes.bus.apu.gain.get(channel), 0.0);
    }
}

#[test]
fn nes_disk_side_swaps() {
    // A two-side disk built in code: two 65,500-byte sides, which is all the drive reads to mount
    // and swap. The BIOS buffer is eight kilobytes of zeros — `Fds::new` needs a dump of that
    // length, and nothing here steps the CPU, so no real disk or BIOS file is required.
    let mut image = vec![0u8; 2 * 65_500];
    image[0] = 0x01; // a real side opens with its disk-info block
    let mut machine = NesMachine::new(
        image,
        NesImage::Disk { qd: false },
        Some(vec![0u8; 8192]),
        ConsoleConfig::default(),
    )
    .expect("a synthetic two-side image mounts");
    assert_eq!(machine.disk_side(), Some((0, 2)), "side A of two");
    assert_eq!(machine.next_disk_side(), Some((1, 2)), "advances to side B");
    assert_eq!(machine.disk_side(), Some((1, 2)));
    assert_eq!(machine.next_disk_side(), Some((0, 2)), "wraps to side A");
}
