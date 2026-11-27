//! The `.sav` body, its clock footer, the paths and the atomic replace.

mod common;

use std::path::{Path, PathBuf};
use tendy_gb::save::{decode_save, encode_save, load_save, save_path, state_path, write_atomic};
use tendy_gb::{Cartridge, Machine};

fn rom_bytes(banks: usize, cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = common::synth_rom_banked(banks, cart_type, &common::entry(&[0x00]));
    rom[0x0149] = ram_code;
    fix_checksum(&mut rom);
    rom
}

fn fix_checksum(rom: &mut [u8]) {
    let mut x: u8 = 0;
    for &byte in &rom[0x0134..=0x014C] {
        x = x.wrapping_sub(byte).wrapping_sub(1);
    }
    rom[0x014D] = x;
}

fn machine(banks: usize, cart_type: u8, ram_code: u8) -> Machine {
    Machine::new(Cartridge::new(rom_bytes(banks, cart_type, ram_code)).expect("cartridge"))
}

/// An empty directory under `target/`, emptied first, so no test writes beside the source tree.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cart-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn ram_round_trip() {
    let rom = rom_bytes(2, 0x03, 0x02);
    let mut m = Machine::new(Cartridge::new(rom.clone()).expect("cartridge"));
    for (i, byte) in m.bus.cart.ram.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(7);
    }

    let encoded = encode_save(&m).expect("cartridge RAM");
    assert_eq!(encoded.len(), 0x2000);
    assert_eq!(encoded, m.battery().expect("battery"));

    let mut fresh = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(decode_save(&mut fresh, &encoded).expect("decodes"));
    assert_eq!(
        fresh.battery().expect("battery"),
        m.battery().expect("battery")
    );
    assert_eq!(fresh.bus.cart.ram, m.bus.cart.ram);

    // Nothing to load: no change, no error.
    let mut empty = Machine::new(Cartridge::new(rom_bytes(2, 0x03, 0x02)).expect("cartridge"));
    assert!(!decode_save(&mut empty, &[]).expect("decodes"));
    assert_eq!(empty.battery().expect("battery"), &vec![0u8; 0x2000][..]);
}

#[test]
fn rtc_footer_round_trip() {
    let now = 1_700_000_000_000u64;
    let rom = rom_bytes(2, 0x10, 0x02);
    let mut m = Machine::new(Cartridge::new(rom.clone()).expect("cartridge"));
    m.set_rtc([1, 2, 3, 4, 5]);
    // The first tick only sets the RTC's epoch, so the registers survive it.
    m.tick_rtc(now);
    assert_eq!(m.rtc(), Some([1, 2, 3, 4, 5]));

    // UNVERIFIED: "wall clock when written" is the RTC's own `base_ms`, the last value the front end
    // passed to `tick_rtc`; `encode_save` takes no clock argument and has no other source for it.
    let encoded = encode_save(&m).expect("RAM and clock");
    assert_eq!(encoded.len(), 0x2000 + 18);
    assert_eq!(&encoded[0x2000..0x2004], b"RTCL");
    assert_eq!(encoded[0x2004], 1, "footer version");
    assert_eq!(&encoded[0x2005..0x200A], &[1, 2, 3, 4, 5]);
    assert_eq!(
        u64::from_le_bytes(encoded[0x200A..0x2012].try_into().expect("eight bytes")),
        now
    );

    let mut fresh = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(decode_save(&mut fresh, &encoded).expect("decodes"));
    assert_eq!(fresh.rtc(), Some([1, 2, 3, 4, 5]));
    assert_eq!(
        fresh.bus.cart.rtc.as_ref().expect("clock").base_ms,
        now,
        "the footer's clock is the new epoch"
    );

    // Time passes while the machine is off, and the counters keep going from the saved second.
    fresh.tick_rtc(now + 5_000);
    assert_eq!(
        fresh.bus.cart.rtc.as_ref().expect("clock").secs,
        1 + 5,
        "five seconds of wall clock"
    );
}

#[test]
fn clock_only_cartridge_still_saves() {
    // $0F/$10 with $0149 = $00: no RAM, so `Machine::battery()` is `None` and the old autosave
    // left no `.sav` at all; the clock alone is what the file holds.
    let now = 1_700_000_000_000u64;
    let rom = rom_bytes(2, 0x0F, 0x00);
    let mut m = Machine::new(Cartridge::new(rom.clone()).expect("cartridge"));
    assert_eq!(m.battery(), None, "no RAM");
    m.set_rtc([11, 22, 3, 4, 5]);
    m.tick_rtc(now);

    let body = encode_save(&m).expect("a clock-only cartridge still saves");
    assert_eq!(body.len(), 18, "the footer alone");
    assert_eq!(&body[..4], b"RTCL");
    assert_eq!(body[4], 1, "footer version");
    assert_eq!(&body[5..10], &[11, 22, 3, 4, 5]);

    let mut fresh = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(decode_save(&mut fresh, &body).expect("decodes"));
    assert_eq!(fresh.rtc(), Some([11, 22, 3, 4, 5]));
    assert_eq!(fresh.bus.cart.rtc.as_ref().expect("clock").base_ms, now);
}

#[test]
fn closed_time_is_caught_up_on_load() {
    let now = 1_700_000_000_000u64;
    let rom = rom_bytes(2, 0x10, 0x02);
    let mut m = Machine::new(Cartridge::new(rom.clone()).expect("cartridge"));
    m.set_rtc([1, 2, 3, 4, 5]);
    m.tick_rtc(now);
    let body = encode_save(&m).expect("body");

    // The program was closed for 90 s; `Session::open`'s load walks the clock forward by that gap.
    let mut fresh = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(load_save(&mut fresh, &body, now + 90_000).expect("decodes"));
    assert_eq!(fresh.rtc(), Some([1, 2, 3, 4, 5]), "latched registers");
    let clock = fresh.bus.cart.rtc.as_ref().expect("clock");
    assert_eq!((clock.mins, clock.secs), (3, 31), "2:01 plus 90 s");
}

#[test]
fn short_file_loads_as_ram_only() {
    // A file of exactly the RAM body, which is what older emulators and this emulator's first
    // version write for a cartridge without a clock.
    let rom = rom_bytes(2, 0x03, 0x02);
    let body: Vec<u8> = (0..0x2000u32).map(|i| i as u8).collect();
    let mut m = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(decode_save(&mut m, &body).expect("decodes"));
    assert_eq!(m.battery().expect("battery"), &body[..]);

    // The same for a cartridge with a clock: RAM only, and the RTC is left as it was.
    let rom = rom_bytes(2, 0x10, 0x02);
    let mut clock = Machine::new(Cartridge::new(rom).expect("cartridge"));
    clock.set_rtc([9, 8, 7, 6, 5]);
    assert!(decode_save(&mut clock, &body).expect("decodes"));
    assert_eq!(clock.battery().expect("battery"), &body[..]);
    assert_eq!(clock.rtc(), Some([9, 8, 7, 6, 5]));
}

#[test]
fn corrupt_file_is_refused_and_kept() {
    let dir = scratch("corrupt");
    let path = dir.join("game.sav");
    let mut m = machine(2, 0x03, 0x02);

    // A file longer than the RAM body that is not a footer.
    let mut bad = vec![0xAAu8; 0x2000 + 4];
    bad[0x2000..0x2004].copy_from_slice(b"NOPE");
    std::fs::write(&path, &bad).expect("write");
    assert_eq!(
        decode_save(&mut m, &bad),
        Err("save file is corrupt".to_string())
    );
    assert_eq!(std::fs::read(&path).expect("read"), bad, "left untouched");

    // Shorter than the RAM body.
    let short = vec![0u8; 0x1000];
    assert_eq!(
        decode_save(&mut m, &short),
        Err("save file is corrupt".to_string())
    );

    // A footer cut in half.
    let mut cut = vec![0u8; 0x2000];
    cut.extend_from_slice(b"RTCL\x01");
    assert_eq!(
        decode_save(&mut m, &cut),
        Err("save file is corrupt".to_string())
    );

    // A footer from a format this build does not know.
    let mut future = vec![0u8; 0x2000];
    future.extend_from_slice(b"RTCL");
    future.push(2);
    future.extend_from_slice(&[0u8; 13]);
    assert_eq!(
        decode_save(&mut m, &future),
        Err("save file has format version 2, expected 1".to_string())
    );

    // Nothing above touched the machine's RAM.
    assert_eq!(m.battery().expect("battery"), &vec![0u8; 0x2000][..]);
}

#[test]
fn board_owned_bytes_round_trip_after_the_ram() {
    // MBC7 has no battery RAM at all: its 256-byte EEPROM is the board's, and §5.5's body carries
    // it in a footer after the (empty) RAM body.
    let rom = rom_bytes(8, 0x22, 0x00);
    let mut m = Machine::new(Cartridge::new(rom.clone()).expect("cartridge"));
    assert_eq!(m.battery(), None, "no RAM to save");

    // Program word 0 with the 93LC56's pin protocol: `$0A` to `$0000`, `$40` to `$4000`, then the
    // EEPROM register at `$A080`.
    let cart = &mut m.bus.cart;
    cart.write(0x0000, 0x0A);
    cart.write(0x4000, 0x40);
    let bit = |cart: &mut Cartridge, b: u8| {
        let pins = 0x80 | ((b & 1) << 1);
        cart.write(0xA080, pins);
        cart.write(0xA080, pins | 0x40);
    };
    for b in (0..10).rev() {
        bit(cart, (0x260u16 >> b) as u8 & 1); // EWEN
    }
    cart.write(0xA080, 0x00);
    cart.write(0xA080, 0x80);
    for b in (0..10).rev() {
        bit(cart, (0x280u16 >> b) as u8 & 1); // WRITE word 0
    }
    for b in (0..16).rev() {
        bit(cart, (0x1234u16 >> b) as u8 & 1);
    }

    let encoded = encode_save(&m).expect("the board's EEPROM");
    assert_eq!(encoded.len(), 4 + 1 + 4 + 256, "no RAM, then the footer");
    assert_eq!(&encoded[..4], b"NVOL");
    assert_eq!(&encoded[9..11], &[0x34, 0x12], "word 0, low byte first");

    let mut fresh = Machine::new(Cartridge::new(rom).expect("cartridge"));
    assert!(decode_save(&mut fresh, &encoded).expect("decodes"));
    assert_eq!(
        fresh.bus.cart.board_save(),
        m.bus.cart.board_save(),
        "the EEPROM image comes back"
    );

    // A cut-off footer is refused, not half-loaded.
    assert_eq!(
        decode_save(&mut fresh, &encoded[..encoded.len() - 1]),
        Err("save file is corrupt".to_string())
    );

    // MBC6's flash and hidden region ride behind the RAM body the same way.
    let mut flashy = machine(64, 0x20, 0x00);
    let encoded = encode_save(&flashy).expect("the board's flash");
    assert_eq!(encoded.len(), 0x8000 + 4 + 1 + 4 + 0x10_0100);
    assert!(decode_save(&mut flashy, &encoded).expect("decodes"));
}

#[test]
fn path_is_stable_and_collision_free() {
    let config = Path::new("cfg");
    let a = save_path(config, "Tetris", 0x1234_5678);
    assert_eq!(a, save_path(config, "Tetris", 0x1234_5678));
    assert_eq!(a, PathBuf::from("cfg/saves/Tetris [12345678].sav"));
    assert!(a.ends_with("Tetris [12345678].sav"));

    // A different dump or a different title must not share a file.
    assert_ne!(a, save_path(config, "Tetris", 0x1234_5679));
    assert_ne!(a, save_path(config, "Tetris 2", 0x1234_5678));
    assert_ne!(a, save_path(Path::new("other"), "Tetris", 0x1234_5678));

    assert_eq!(
        state_path(Path::new("roms/a.gb")),
        PathBuf::from("roms/a.state")
    );
    assert_eq!(
        state_path(Path::new("roms/a.gbc")),
        PathBuf::from("roms/a.state")
    );
}

#[test]
fn atomic_write_leaves_no_temp_file() {
    let dir = scratch("atomic");
    let path = dir.join("game.sav");

    write_atomic(&path, b"first").expect("write");
    assert_eq!(std::fs::read(&path).expect("read"), b"first");

    write_atomic(&path, b"second").expect("replace");
    assert_eq!(std::fs::read(&path).expect("read"), b"second");

    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, vec!["game.sav".to_string()], "no .tmp left behind");

    // A nested path is created on demand, as `<config>/saves/` is on first save.
    let nested = dir.join("saves").join("nested.sav");
    write_atomic(&nested, b"x").expect("write");
    assert_eq!(std::fs::read(&nested).expect("read"), b"x");
}
