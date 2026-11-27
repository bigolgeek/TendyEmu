use tendy_nes::cart::{Mapper, Mirroring};
use tendy_nes::state::{StateReader, StateWriter};

/// 8 KB PRG banks.
const PRG_BANKS: usize = 16;
/// 1 KB CHR banks.
const CHR_BANKS: usize = 64;

/// iNES file for mapper 4 with vertical mirroring: 8 KB PRG bank `k` is filled with `k` and 1 KB CHR
/// bank `k` is filled with `k`.
fn rom_bytes() -> Vec<u8> {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 8, 0x41, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    for bank in 0..PRG_BANKS {
        v.extend(vec![bank as u8; 0x2000]);
    }
    for bank in 0..CHR_BANKS {
        v.extend(vec![bank as u8; 0x400]);
    }
    v
}

fn board() -> Box<dyn Mapper> {
    tendy_nes::mapper::load(&rom_bytes()).unwrap()
}

fn roundtrip(a: &dyn Mapper, b: &mut dyn Mapper) {
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut r = StateReader::new(&bytes);
    b.load_state(&mut r).unwrap();
    assert_eq!(r.remaining(), 0, "load must consume the whole state");
}

fn assert_same(a: &mut dyn Mapper, b: &mut dyn Mapper) {
    for addr in (0x6000u32..=0xFFFF).step_by(0x101) {
        assert_eq!(
            a.cpu_read(addr as u16),
            b.cpu_read(addr as u16),
            "cpu {addr:#06x}"
        );
    }
    for addr in (0x0000u16..=0x1FFF).step_by(0x41) {
        assert_eq!(a.ppu_read(addr), b.ppu_read(addr), "ppu {addr:#06x}");
    }
    assert_eq!(a.mirroring(), b.mirroring());
    assert_eq!(a.prg_ram(), b.prg_ram());
}

#[test]
fn power_on_fixes_last_two_banks() {
    let mut m = board();
    assert_eq!(m.cpu_read(0xE000), 15);
    assert_eq!(m.cpu_read(0xC000), 14);
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xA000), 0);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn prg_banking_modes() {
    let mut m = board();
    m.cpu_write(0x8000, 6);
    m.cpu_write(0x8001, 3);
    assert_eq!(m.cpu_read(0x8000), 3);
    m.cpu_write(0x8000, 7);
    m.cpu_write(0x8001, 5);
    assert_eq!(m.cpu_read(0xA000), 5);

    // bank_select bit 6: $C000 becomes R6 and $8000 the second-to-last bank.
    m.cpu_write(0x8000, 0x46);
    assert_eq!(m.cpu_read(0xC000), 3);
    assert_eq!(m.cpu_read(0x8000), 14);
    assert_eq!(m.cpu_read(0xE000), 15);

    // Bank numbers wrap modulo the 8 KB bank count.
    m.cpu_write(0x8000, 6);
    m.cpu_write(0x8001, 16 + 3);
    assert_eq!(m.cpu_read(0x8000), 3);
}

#[test]
fn chr_banking() {
    let mut m = board();
    m.cpu_write(0x8000, 0);
    m.cpu_write(0x8001, 5);
    // R0 covers $0000-$07FF as the 1 KB banks R0 & 0xFE and +1.
    assert_eq!(m.ppu_read(0x0000), 4);
    assert_eq!(m.ppu_read(0x0400), 5);

    m.cpu_write(0x8000, 2);
    m.cpu_write(0x8001, 9);
    assert_eq!(m.ppu_read(0x1000), 9);
    m.cpu_write(0x8000, 5);
    m.cpu_write(0x8001, 12);
    assert_eq!(m.ppu_read(0x1C00), 12);

    // bank_select bit 7 swaps the 4 KB halves, so $1000 now reads through R0.
    m.cpu_write(0x8000, 0x80);
    assert_eq!(m.ppu_read(0x1000), 4);
}

#[test]
fn mirroring_register() {
    let mut m = board();
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    m.cpu_write(0xA000, 1);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
    m.cpu_write(0xA000, 0);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn prg_ram_round_trips() {
    let mut m = board();
    assert_eq!(m.prg_ram().len(), 8192);
    m.cpu_write(0x6000, 0x11);
    m.cpu_write(0x7FFF, 0x22);
    assert_eq!(m.cpu_read(0x6000), 0x11);
    assert_eq!(m.cpu_read(0x7FFF), 0x22);
    m.load_prg_ram(&[0xAB; 8192]);
    assert_eq!(m.cpu_read(0x6123), 0xAB);
}

#[test]
fn save_load_round_trip() {
    let mut a = board();
    a.cpu_write(0x8000, 6);
    a.cpu_write(0x8001, 6);
    a.cpu_write(0x8000, 2);
    a.cpu_write(0x8001, 9);
    a.cpu_write(0xA000, 1);
    a.cpu_write(0x6004, 0x5A);
    a.cpu_clock();
    a.cpu_clock();

    let mut b = board();
    roundtrip(&*a, &mut *b);
    assert_same(&mut *a, &mut *b);
    assert_eq!(b.cpu_read(0x6004), 0x5A);
    assert_eq!(b.mirroring(), Mirroring::Horizontal);
    assert_eq!(b.cpu_read(0x8000), 6);
    assert_eq!(b.ppu_read(0x1000), 9);
}

#[test]
fn prg_rom_offset_follows_banking_modes() {
    let mut m = board();
    let probes = [0x8000u16, 0xBFFF, 0xC000, 0xFFFF];
    let at = |m: &dyn Mapper| probes.map(|a| m.prg_rom_offset(a).unwrap());
    // Power-on: R6 = R7 = 0 at $8000/$A000, second-to-last at $C000, last at $E000.
    assert_eq!(at(&*m), [0, 0x1FFF, 14 * 0x2000, 15 * 0x2000 + 0x1FFF]);
    assert_eq!(m.prg_rom_offset(0x7FFF), None);
    m.cpu_write(0x8000, 6);
    m.cpu_write(0x8001, 3);
    m.cpu_write(0x8000, 7);
    m.cpu_write(0x8001, 5);
    // Mode 0: R6 at $8000, R7 at $A000.
    assert_eq!(
        at(&*m),
        [
            3 * 0x2000,
            5 * 0x2000 + 0x1FFF,
            14 * 0x2000,
            15 * 0x2000 + 0x1FFF
        ]
    );
    // Mode 1 (bank-select bit 6): second-to-last at $8000, R6 at $C000.
    m.cpu_write(0x8000, 0x46);
    assert_eq!(
        at(&*m),
        [
            14 * 0x2000,
            5 * 0x2000 + 0x1FFF,
            3 * 0x2000,
            15 * 0x2000 + 0x1FFF
        ]
    );
    for a in probes {
        let o = m.prg_rom_offset(a).unwrap();
        assert_eq!((o / 0x2000) as u8, m.cpu_read(a), "{a:#06x}");
    }
}
