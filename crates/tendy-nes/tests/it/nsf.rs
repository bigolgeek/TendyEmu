//! NSF playback: a hand-built tune runs on the real console through
//! [`Nes::from_nsf`], with the board's driver stub doing the work a player would otherwise do in
//! the host.

use tendy_nes::apu::SAMPLE_RATE;
use tendy_nes::bus::RamPowerOn;
use tendy_nes::cart::Region;
use tendy_nes::console::{ConsoleConfig, Quirks};
use tendy_nes::cpu::Bus;
use tendy_nes::nes::Nes;
use tendy_nes::nsf::{NMI_ENTRY, Nsf, NsfBoard, NsfError, NsfHeader, NsfInfo, RESET_ENTRY};

/// A single-song, 2A03, NTSC, standard-speed NSF header with `program` after it.
fn nsf(load: u16, init: u16, play: u16, program: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 0x80];
    v[0..5].copy_from_slice(b"NESM\x1A");
    v[5] = 1; // version
    v[6] = 1; // one song
    v[7] = 1; // starting song
    v[8..10].copy_from_slice(&load.to_le_bytes());
    v[10..12].copy_from_slice(&init.to_le_bytes());
    v[12..14].copy_from_slice(&play.to_le_bytes());
    v[0x6E..0x70].copy_from_slice(&16_639u16.to_le_bytes());
    v[0x78..0x7A].copy_from_slice(&19_997u16.to_le_bytes());
    v.extend_from_slice(program);
    v
}

/// A program whose `INIT` (at `$8000`) sets the triangle to `period`, enables it, and returns;
/// its `PLAY` at `$9000` is a bare `RTS`, exactly as the acceptance case asks.
fn triangle_program(period: u16) -> Vec<u8> {
    let mut p = vec![0u8; 0x2000];
    let init = [
        0xA9,
        0xFF,
        0x8D,
        0x08,
        0x40, // LDA #$FF / STA $4008: length counter halted, linear 127
        0xA9,
        period as u8,
        0x8D,
        0x0A,
        0x40, // LDA #lo / STA $400A
        0xA9,
        ((period >> 8) as u8 & 0x07) | 0x08,
        0x8D,
        0x0B,
        0x40, // LDA #hi | length load 1 / STA $400B
        0x60, // RTS
    ];
    p[..init.len()].copy_from_slice(&init);
    p[0x1000] = 0x60; // PLAY: RTS
    p
}

/// `INIT` at `$8000` records the A and X the driver passed (`$0300`/`$0301`); `PLAY` at `$9000`
/// counts its calls in `$0302`.
fn contract_program() -> Vec<u8> {
    let mut p = vec![0u8; 0x2000];
    p[..6].copy_from_slice(&[0x8D, 0x00, 0x03, 0x8E, 0x01, 0x03]); // STA $0300 / STX $0301
    p[6] = 0x60; // RTS
    p[0x1000..0x1004].copy_from_slice(&[0xEE, 0x02, 0x03, 0x60]); // INC $0302 / RTS
    p
}

fn boot(rom: &[u8], song: u8) -> Nes {
    Nes::from_nsf(rom, song, &ConsoleConfig::default()).expect("the tune should boot")
}

/// Frequency of a periodic stream, from the mean-removed zero crossings. The triangle is the only
/// audible channel in these files, so two crossings make one cycle.
fn measure_hz(samples: &[f32]) -> f64 {
    let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / samples.len() as f64;
    let mut crossings = 0u32;
    let mut previous = f64::from(samples[0]) - mean;
    for &sample in &samples[1..] {
        let value = f64::from(sample) - mean;
        if (previous <= 0.0) != (value <= 0.0) {
            crossings += 1;
        }
        previous = value;
    }
    let seconds = samples.len() as f64 / SAMPLE_RATE;
    f64::from(crossings) / 2.0 / seconds
}

fn rms(samples: &[f32]) -> f64 {
    let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / samples.len() as f64;
    (samples
        .iter()
        .map(|&s| (f64::from(s) - mean).powi(2))
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

#[test]
fn reset_enters_the_driver() {
    let rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    let mut nes = boot(&rom, 1);
    assert_eq!(
        nes.cpu.pc, RESET_ENTRY,
        "the board answers $FFFC/$FFFD with the driver"
    );
    assert_eq!(nes.bus.cart_mut().cpu_read(0xFFFA), NMI_ENTRY as u8);
    assert_eq!(nes.bus.cart_mut().cpu_read(0xFFFB), (NMI_ENTRY >> 8) as u8);
    assert_eq!(nes.region(), Region::Ntsc);
}

#[test]
fn init_gets_the_song_minus_one_and_play_runs_every_frame() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[6] = 3; // three songs
    let mut nes = boot(&rom, 2);
    for _ in 0..60 {
        nes.run_frame();
    }
    assert_eq!(nes.bus.ram[0x0300], 1, "INIT's A is song - 1");
    assert_eq!(nes.bus.ram[0x0301], 0, "INIT's X is 0 on NTSC");
    let plays = nes.bus.ram[0x0302];
    assert!(
        (50..=60).contains(&plays),
        "PLAY once per frame: {plays} calls in 60 frames"
    );
}

#[test]
fn zero_selects_the_files_starting_song() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[6] = 5;
    rom[7] = 4; // the file wants song 4 first
    let mut nes = boot(&rom, 0);
    for _ in 0..5 {
        nes.run_frame();
    }
    assert_eq!(nes.bus.ram[0x0300], 3, "song 4 arrives as 3");
}

#[test]
fn out_of_range_song_is_an_error() {
    let rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    assert_eq!(
        Nes::from_nsf(&rom, 2, &ConsoleConfig::default()).err(),
        Some(NsfError::BadSong(2))
    );
}

#[test]
fn pal_file_runs_as_pal_and_tells_init() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[0x7A] = 1; // PAL only
    let mut nes = boot(&rom, 1);
    assert_eq!(nes.region(), Region::Pal);
    for _ in 0..5 {
        nes.run_frame();
    }
    assert_eq!(nes.bus.ram[0x0301], 1, "INIT's X is 1 on a PAL file");
}

#[test]
fn bad_magic_is_a_load_error() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[3] = b'X';
    assert_eq!(
        Nes::from_nsf(&rom, 1, &ConsoleConfig::default()).err(),
        Some(NsfError::BadMagic)
    );
}

#[test]
fn expansion_chip_file_is_refused() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[0x7B] = 0x01; // VRC6
    assert_eq!(
        Nes::from_nsf(&rom, 1, &ConsoleConfig::default()).err(),
        Some(NsfError::UnsupportedChip(0x01))
    );
}

#[test]
fn triangle_tune_is_audible_at_the_init_period() {
    let period = 100u16;
    let rom = nsf(0x8000, 0x8000, 0x9000, &triangle_program(period));
    let mut nes = boot(&rom, 1);
    for _ in 0..60 {
        nes.run_frame();
    }
    let samples = std::mem::take(&mut nes.bus.apu.samples);
    assert!(
        samples.len() > 40_000,
        "about a second at {SAMPLE_RATE} Hz: {}",
        samples.len()
    );
    assert!(
        rms(&samples) > 0.01,
        "the tune plays: rms {}",
        rms(&samples)
    );

    // Full triangle cycle = 32 steps of (period + 1) CPU cycles.
    let expected = tendy_nes::CPU_HZ / (32.0 * f64::from(period + 1));
    let measured = measure_hz(&samples);
    assert!(
        (measured - expected).abs() < expected * 0.02,
        "measured {measured:.1} Hz, expected {expected:.1} Hz"
    );
}

#[test]
fn driver_clears_work_ram_before_init() {
    let rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    let mut config = ConsoleConfig::default();
    config.power_on.ram = RamPowerOn::Random;
    let mut nes = Nes::from_nsf(&rom, 1, &config).unwrap();
    nes.run_frame();
    assert!(
        nes.bus.ram[0x0400..0x0800].iter().all(|&b| b == 0),
        "the stub cleared $0400-$07FF the tune never touches"
    );
}

/// An NSF powers on through the same steps as every other console — the `--ram-init` fill
/// reaches the PPU's CHR-RAM and the `$4016` port quirk is armed — instead of a hand-copied subset
/// of `power_on_bus` that dropped both.
#[test]
fn power_on_fills_the_ppu_ram_and_arms_the_port_quirk() {
    let rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    let mut config = ConsoleConfig::default();
    config.power_on.ram = RamPowerOn::Ones;
    config.quirks = Quirks {
        nes101_port: true,
        ..Quirks::default()
    };
    let nes = Nes::from_nsf(&rom, 1, &config).unwrap();
    assert!(
        nes.bus.ppu.vram.iter().all(|&b| b == 0xFF),
        "the PPU's CHR-RAM missed the power-on fill"
    );
    assert_eq!(
        nes.bus.port1_open_mask, 0xE4,
        "the $4016 port quirk was not armed"
    );
}

#[test]
fn bank_register_write_maps_the_named_bank() {
    // Bank-switched: window 0 starts on bank 1, everything else on bank 0, and INIT lives at
    // $9000 in bank 0 (an RTS) so the machine boots.
    let mut rom = nsf(0x8000, 0x9000, 0x0000, &[]);
    rom[0x70] = 1; // bank 1 in the first window
    let mut program = vec![0x11u8; 0x1000];
    program[0] = 0x60; // bank 0: INIT at $9000 is an RTS
    program.extend(vec![0x22u8; 0x1000]);
    rom.extend_from_slice(&program);
    let mut nes = boot(&rom, 1);
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x8000),
        0x22,
        "window 0 is bank 1"
    );
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x9000),
        0x60,
        "window 1 is bank 0"
    );

    nes.bus.cart_mut().cpu_write(0x5FF8, 0);
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x8000),
        0x60,
        "$5FF8 names bank 0"
    );
    nes.bus.cart_mut().cpu_write(0x5FF9, 1);
    assert_eq!(
        nes.bus.cart_mut().cpu_read(0x9000),
        0x22,
        "$5FF9 names bank 1"
    );
    // And through the bus, where $5FF8 is in the cartridge's register window.
    nes.bus.write(0x5FF8, 1);
    assert_eq!(nes.bus.read(0x8000), 0x22);
}

#[test]
fn board_state_round_trips_through_the_header_and_program() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[6] = 2;
    rom[0x70] = 2;
    let nsf = tendy_nes::nsf::Nsf::parse(&rom).unwrap();
    let board = NsfBoard::new(&nsf.header, &nsf.program, 0);
    let header: &NsfHeader = &nsf.header;
    assert_eq!(header.song_count, 2);
    assert_eq!(board.bank_select()[0], 2);
    assert_eq!(board.driver()[0], 0x78, "the driver starts with SEI");
}

#[test]
fn save_load_round_trips_mid_song() {
    let rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    let mut nes = boot(&rom, 1);
    for _ in 0..20 {
        nes.run_frame();
    }
    let state = nes.save_state();
    let counter = nes.bus.ram[0x0302];
    assert!(counter > 10, "PLAY has been running: {counter}");

    nes.bus.apu.samples.clear();
    for _ in 0..10 {
        nes.run_frame();
    }
    let after: Vec<u32> = nes.bus.apu.samples.iter().map(|s| s.to_bits()).collect();
    let later = nes.bus.ram[0x0302];
    assert!(later > counter);
    assert!(
        !after.is_empty(),
        "the restored run has to make sound to compare"
    );

    nes.load_state(&state).unwrap();
    assert_eq!(nes.bus.ram[0x0302], counter, "the counter came back");
    nes.bus.apu.samples.clear();
    for _ in 0..10 {
        nes.run_frame();
    }
    let again: Vec<u32> = nes.bus.apu.samples.iter().map(|s| s.to_bits()).collect();
    assert_eq!(again, after, "ten frames replay sample for sample");
    assert_eq!(nes.bus.ram[0x0302], later);
}

#[test]
fn zero_song_count_is_refused_at_the_header() {
    let mut rom = nsf(0x8000, 0x8000, 0x9000, &contract_program());
    rom[6] = 0; // no songs at all
    assert_eq!(Nsf::parse(&rom).err(), Some(NsfError::BadSong(0)));
    assert_eq!(NsfInfo::read(&rom).err(), Some(NsfError::BadSong(0)));
}
