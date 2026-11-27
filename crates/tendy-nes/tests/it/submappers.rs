//! NES 2.0 submapper rules that change emulation: bus conflicts on mappers
//! 2/3/7/11 submapper 2, MMC1 submapper 5 (fixed 32 KB PRG) and 7 (hard-wired mirroring), MMC3
//! submapper 2 (hard-wired mirroring) and 3 (MC-ACC: A12 falls divided by 8, clocked on the first).
//! Submapper 0 keeps the plain iNES behaviour in every case.

use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::axrom::Axrom;
use tendy_nes::mapper::cnrom::Cnrom;
use tendy_nes::mapper::color_dreams::ColorDreams;
use tendy_nes::mapper::load;
use tendy_nes::mapper::mmc1::Mmc1;
use tendy_nes::mapper::mmc3::Mmc3;
use tendy_nes::mapper::uxrom::Uxrom;

/// A cartridge with an NES 2.0 header: `prg_banks` x 16 KB where every byte holds its 16 KB bank
/// number, and 8 KB CHR-RAM when `chr_kb` is 0, else CHR ROM whose bytes hold their 4 KB page number.
fn board(mapper: u8, prg_banks: usize, chr_kb: usize, submapper: u8) -> RomData {
    let chr_is_ram = chr_kb == 0;
    let chr = if chr_is_ram {
        vec![0; 8192]
    } else {
        (0..chr_kb * 1024).map(|i| (i / 0x1000) as u8).collect()
    };
    RomData {
        header: Header {
            prg_banks,
            chr_banks: chr_kb / 8,
            mapper: mapper.into(),
            mirroring: Mirroring::Vertical,
            nes2: true,
            submapper,
            ..Default::default()
        },
        prg: (0..prg_banks * 0x4000)
            .map(|i| (i / 0x4000) as u8)
            .collect(),
        chr,
        chr_is_ram,
        trainer: None,
    }
}

/// Load a 5-bit value into an MMC1 register through the serial port (LSB first).
fn mmc1_write(m: &mut Mmc1, addr: u16, val: u8) {
    for i in 0..5 {
        m.cpu_write(addr, (val >> i) & 1);
    }
}

// UxROM (mapper 2)

/// Submapper 2: the bank register latches the write ANDed with the PRG byte at $8000 (0x06).
#[test]
fn uxrom_submapper2_write_conflicts_with_prg() {
    let mut r = board(2, 8, 0, 2);
    r.prg[0] = 0x06;
    let mut m = Uxrom::new(r);
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(
        m.cpu_read(0x8000),
        6,
        "the latched bank is $FF AND the PRG byte"
    );
    // The fixed bank at $C000 is bank 7, so a write there conflicts with its own byte.
    m.cpu_write(0xC000, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 7);
}

/// Submappers 0 and 1 latch all eight bits, as plain iNES files always have.
#[test]
fn uxrom_submapper0_and_1_are_conflict_free() {
    for sub in [0, 1] {
        let mut r = board(2, 8, 0, sub);
        r.prg[0] = 0x06;
        let mut m = Uxrom::new(r);
        m.cpu_write(0x8000, 0xFF);
        assert_eq!(
            m.cpu_read(0x8000),
            7,
            "submapper {sub}: all eight bits latch"
        );
    }
}

// CNROM (mapper 3)

/// Submapper 2: the CHR bank register latches the write ANDed with the PRG byte at $8000 (0x02).
#[test]
fn cnrom_submapper2_write_conflicts_with_prg() {
    let mut r = board(3, 2, 16, 2);
    r.prg[0] = 0x02;
    let mut m = Cnrom::new(r);
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(
        m.ppu_read(0x0000),
        0,
        "CHR bank 2 wraps to the first 8 KB bank"
    );
}

/// Submappers 0 and 1 latch all eight bits: bank $FF wraps to the second CHR bank.
#[test]
fn cnrom_submapper0_and_1_are_conflict_free() {
    for sub in [0, 1] {
        let mut r = board(3, 2, 16, sub);
        r.prg[0] = 0x02;
        let mut m = Cnrom::new(r);
        m.cpu_write(0x8000, 0xFF);
        assert_eq!(
            m.ppu_read(0x0000),
            2,
            "submapper {sub}: all eight bits latch"
        );
    }
}

// AxROM (mapper 7)

/// Submapper 2: the write is ANDed with the PRG byte at $8000 (0x02), so both the 32 KB bank and
/// the mirroring bit come from the conflicted value.
#[test]
fn axrom_submapper2_write_conflicts_with_prg() {
    let mut r = board(7, 16, 0, 2);
    r.prg[0] = 0x02;
    let mut m = Axrom::new(r);
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 4, "bank 2 maps 16 KB banks 4 and 5");
    assert_eq!(
        m.mirroring(),
        Mirroring::SingleScreenLower,
        "bit 4 of the conflicted value"
    );
}

/// Submappers 0 and 1 latch the byte unchanged; AxROM is conflict-free by default.
#[test]
fn axrom_submapper0_and_1_are_conflict_free() {
    for sub in [0, 1] {
        let mut r = board(7, 16, 0, sub);
        r.prg[0] = 0x02;
        let mut m = Axrom::new(r);
        m.cpu_write(0x8000, 0xFF);
        assert_eq!(
            m.cpu_read(0x8000),
            14,
            "submapper {sub}: bank 7 maps banks 14 and 15"
        );
        assert_eq!(m.mirroring(), Mirroring::SingleScreenUpper);
    }
}

// Color Dreams (mapper 11)

/// Submapper 2: the write is ANDed with the PRG byte at $8000 (0x24) — PRG bank 0, CHR bank 2.
#[test]
fn color_dreams_submapper2_write_conflicts_with_prg() {
    let mut r = board(11, 8, 16, 2);
    r.prg[0] = 0x24;
    let mut m = ColorDreams::new(r);
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(
        m.cpu_read(0x8000),
        0x24,
        "PRG bank 0 is mapped: $8000 reads the conflict byte"
    );
    assert_eq!(
        m.cpu_read(0xC000),
        1,
        "bank 0's second half, not bank 3's (6)"
    );
    assert_eq!(
        m.ppu_read(0x0000),
        0,
        "CHR bank 2 wraps to the first 8 KB bank"
    );
}

/// Submappers 0 and 1 latch all eight bits: PRG bank 3, CHR bank $F (wrapping to 1).
#[test]
fn color_dreams_submapper0_and_1_are_conflict_free() {
    for sub in [0, 1] {
        let mut r = board(11, 8, 16, sub);
        r.prg[0] = 0x24;
        let mut m = ColorDreams::new(r);
        m.cpu_write(0x8000, 0xFF);
        assert_eq!(
            m.cpu_read(0x8000),
            6,
            "submapper {sub}: PRG bank 3 maps banks 6 and 7"
        );
        assert_eq!(m.ppu_read(0x0000), 2);
    }
}

// MMC1 (mapper 1)

/// Submapper 5 (SEROM/SHROM/SH1ROM): the whole 32 KB of PRG is fixed; the PRG bank register at
/// $E000 has no function.
#[test]
fn mmc1_submapper5_prg_is_fixed() {
    let mut m = Mmc1::new(board(1, 2, 0, 5));
    mmc1_write(&mut m, 0xE000, 1);
    assert_eq!(
        m.cpu_read(0x8000),
        0,
        "PRG A14 is wired straight to the CPU"
    );
    assert_eq!(m.cpu_read(0xC000), 1);
    mmc1_write(&mut m, 0xE000, 0);
    assert_eq!(m.cpu_read(0x8000), 0);
}

/// With no submapper the same $E000 write switches the bank at $8000, as before.
#[test]
fn mmc1_submapper0_still_banks_prg() {
    let mut m = Mmc1::new(board(1, 2, 0, 0));
    mmc1_write(&mut m, 0xE000, 1);
    assert_eq!(m.cpu_read(0x8000), 1);
    assert_eq!(m.cpu_read(0xC000), 1);
}

/// Submapper 7 (Kaiser KS-7058): the nametable arrangement is hard-wired; the control register's
/// mirroring bits do nothing.
#[test]
fn mmc1_submapper7_mirroring_is_hardwired() {
    let mut m = Mmc1::new(board(1, 2, 0, 7));
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "the header value stands"
    );
    mmc1_write(&mut m, 0x8000, 0x00); // one-screen lower
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    mmc1_write(&mut m, 0x8000, 0x03); // horizontal
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

/// With no submapper the control register still drives the mirroring, as before.
#[test]
fn mmc1_submapper0_mirroring_follows_control() {
    let mut m = Mmc1::new(board(1, 2, 0, 0));
    mmc1_write(&mut m, 0x8000, 0x03);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
}

// The header path: an NES 2.0 file's submapper nibble selects the behaviour.

/// flags6 = 0x20 (mapper low nibble 2), flags7 = 0x08 (NES 2.0), byte 8 = 0x20 (submapper 2).
#[test]
fn nes2_header_uxrom_submapper2_enforces_conflicts() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x20, 0x08, 0x20, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0);
    for (i, b) in v[16..].iter_mut().enumerate() {
        *b = (i / 16384) as u8;
    }
    v[16] = 0x06;
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(
        m.cpu_read(0x8000),
        6,
        "the latched bank is $FF AND the PRG byte"
    );
}

/// A plain iNES 1.0 file has no submapper: conflict-free, byte-for-byte the old behaviour.
#[test]
fn ines_header_uxrom_stays_conflict_free() {
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 8, 0, 0x20, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    v.resize(16 + 8 * 16384, 0);
    for (i, b) in v[16..].iter_mut().enumerate() {
        *b = (i / 16384) as u8;
    }
    let mut m = load(&v).unwrap();
    m.cpu_write(0x8000, 0xFF);
    assert_eq!(m.cpu_read(0x8000), 7, "all eight bits latch");
}

// MMC3 (mapper 4)

/// Submapper 2: MMC3C with hard-wired mirroring, so $A000 bit 0 is ignored and the header rules.
#[test]
fn mmc3_submapper2_mirroring_is_hardwired() {
    let mut m = Mmc3::new(board(4, 4, 8, 2));
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "the header's vertical mirroring"
    );
    m.cpu_write(0xA000, 1);
    assert_eq!(
        m.mirroring(),
        Mirroring::Vertical,
        "a hard-wired board ignores $A000 bit 0"
    );
}

/// Submapper 3: MC-ACC, whose IRQ counter clocks on A12's fall with the Sharp (rev B) fire rule.
#[test]
fn mmc3_submapper3_is_mc_acc() {
    let m = Mmc3::new(board(4, 4, 8, 3));
    assert!(m.irq.acc, "submapper 3 selects MC-ACC");
    assert!(
        !m.irq.rev_a,
        "MC-ACC fires on every counter rollover, like Sharp MMC3"
    );
}

/// Submapper 3: the board divides A12 down by 8 on the ppu bus, so eight falls clock the IRQ
/// counter exactly once (Mesen's McAcc.h `_counter` 1..8) and the ninth fall starts a new group.
#[test]
fn mmc3_submapper3_divides_a12_falls_by_eight() {
    let mut m = Mmc3::new(board(4, 4, 8, 3));
    m.cpu_write(0xC000, 5); // latch
    m.cpu_write(0xC001, 0); // reload on the first impulse
    m.cpu_write(0xE001, 0); // enable

    for _ in 0..8 {
        m.ppu_read(0x1000); // A12 high
        m.ppu_read(0x0000); // A12 fall
    }
    assert_eq!(m.irq.counter, 5, "eight falls clock once, on the first");
    assert!(!m.irq_pending());

    m.ppu_read(0x1000);
    m.ppu_read(0x0000);
    assert_eq!(
        m.irq.counter, 4,
        "the ninth fall opens the next group and clocks"
    );
}

/// Submapper 0 stays a plain Sharp MMC3 whose mirroring register works.
#[test]
fn mmc3_submapper0_is_sharp() {
    let mut m = Mmc3::new(board(4, 4, 8, 0));
    assert!(!m.irq.acc, "no submapper means a rise-clocked counter");
    m.cpu_write(0xA000, 1);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
}
