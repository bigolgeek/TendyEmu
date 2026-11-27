use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::load;
use tendy_nes::mapper::mmc2_mmc4::Mmc2Mmc4;
use tendy_nes::state::{StateReader, StateWriter};

/// PRG has `prg_banks` 16 KB banks where every byte holds its bank number; CHR is `chr_kb` KB where
/// every byte holds its 4 KB page number (CHR-RAM when chr_kb is 0: 8 KB of zeros).
fn rom_data(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
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
            battery: false,
            trainer: false,
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

fn board() -> Mmc2Mmc4 {
    Mmc2Mmc4::new(rom_data(9, 16, 32), false)
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
fn load_builds_mmc2() {
    // Mapper 9, vertical mirroring: 16 x 16 KB PRG banks where every byte holds its bank number.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 16, 4, 0x90, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    for bank in 0..16u8 {
        v.extend(vec![bank; 0x4000]);
    }
    v.resize(v.len() + 4 * 8192, 0);
    let mut m = load(&v).unwrap();
    assert_eq!(m.cpu_read(0x8000), 0);
    // 8 KB banks 29, 30 and 31 are the top halves of 16 KB banks 14, 15 and 15.
    assert_eq!(m.cpu_read(0xA000), 14);
    assert_eq!(m.cpu_read(0xC000), 15);
    assert_eq!(m.cpu_read(0xE000), 15);
    // 8 KB bank 3 is the top half of 16 KB bank 1.
    m.cpu_write(0xA000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 1);
}

#[test]
fn prg_bank_switches_8000_only() {
    let mut m = board();
    m.cpu_write(0xA000, 0x03);
    // 8 KB bank 3 is the top half of 16 KB bank 1.
    assert_eq!(m.cpu_read(0x8000), 1);
    // 16 x 16 KB = 32 x 8 KB banks: the last three are 29, 30 and 31 (16 KB banks 14, 15, 15).
    assert_eq!(m.cpu_read(0xA000), 14);
    assert_eq!(m.cpu_read(0xC000), 15);
    assert_eq!(m.cpu_read(0xE000), 15);
}

#[test]
fn prg_bank_uses_low_four_bits() {
    let mut m = board();
    m.cpu_write(0xA000, 0xF5);
    // 8 KB bank 5 is the top half of 16 KB bank 2.
    assert_eq!(m.cpu_read(0x8000), 2);
}

#[test]
fn chr_latch_switches_after_the_read() {
    let mut m = board();
    m.cpu_write(0xB000, 1);
    m.cpu_write(0xC000, 2);
    // Power-on latch is $FE, so the $C000 register is the one in use.
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x0FD8), 2);
    // Only now does the latch follow tile $FD.
    assert_eq!(m.ppu_read(0x0000), 1);
}

#[test]
fn chr_latch_zero_triggers_on_exact_addresses_only() {
    // MMC2 (unlike MMC4): $0FDB is not a latch 0 trigger.
    let mut m = board();
    m.cpu_write(0xB000, 1);
    m.cpu_write(0xC000, 2);
    assert_eq!(m.ppu_read(0x0FDB), 2);
    assert_eq!(m.ppu_read(0x0000), 2);
    assert_eq!(m.ppu_read(0x0FD8), 2);
    assert_eq!(m.ppu_read(0x0000), 1);
}

#[test]
fn chr_latch_returns_to_fe() {
    let mut m = board();
    m.cpu_write(0xB000, 1);
    m.cpu_write(0xC000, 2);
    assert_eq!(m.ppu_read(0x0FD8), 2);
    assert_eq!(m.ppu_read(0x0000), 1);
    assert_eq!(m.ppu_read(0x0FE8), 1);
    assert_eq!(m.ppu_read(0x0000), 2);
}

#[test]
fn chr_latch_one_uses_upper_pattern_table() {
    let mut m = board();
    m.cpu_write(0xD000, 4);
    m.cpu_write(0xE000, 5);
    assert_eq!(m.ppu_read(0x1000), 5);
    assert_eq!(m.ppu_read(0x1FD8), 5);
    assert_eq!(m.ppu_read(0x1000), 4);
    assert_eq!(m.ppu_read(0x1FE8), 4);
    assert_eq!(m.ppu_read(0x1000), 5);
    // A $0000-table fetch does not touch latch 1 (latch 0 is still $FE, so it reads bank 0).
    assert_eq!(m.ppu_read(0x0FD8), 0);
    assert_eq!(m.ppu_read(0x1000), 5);
}

#[test]
fn mirroring_register() {
    let mut m = board();
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    m.cpu_write(0xF000, 0x01);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
    m.cpu_write(0xF000, 0x00);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn prg_ram_is_eight_kb() {
    let mut m = board();
    assert_eq!(m.prg_ram().len(), 8192);
    m.cpu_write(0x6FFF, 0x42);
    assert_eq!(m.cpu_read(0x6FFF), 0x42);
}

#[test]
fn state_roundtrip() {
    let mut a = board();
    a.cpu_write(0xA000, 0x07);
    a.cpu_write(0xB000, 3);
    a.cpu_write(0xC000, 6);
    a.cpu_write(0xD000, 1);
    a.cpu_write(0xE000, 4);
    a.cpu_write(0xF000, 0x01);
    a.cpu_write(0x6010, 0x5A);
    a.ppu_read(0x0FD8); // latch 0 = $FD

    let mut b = Mmc2Mmc4::new(rom_data(9, 16, 32), false);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0000), 3);
    assert_eq!(b.cpu_read(0x6010), 0x5A);
}

#[test]
fn state_roundtrip_chr_ram() {
    let mut a = Mmc2Mmc4::new(rom_data(9, 16, 0), false);
    a.cpu_write(0xB000, 1);
    a.cpu_write(0xC000, 0);
    a.ppu_write(0x0123, 0x77);
    a.ppu_read(0x0FE8);

    let mut b = Mmc2Mmc4::new(rom_data(9, 16, 0), false);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0123), 0x77);
}

fn mmc4_board() -> Mmc2Mmc4 {
    Mmc2Mmc4::new(rom_data(10, 16, 32), true)
}

/// The two chips are one struct, and the one `mmc4` flag accounts for all three places they
/// differ — the PRG window, the `cpu_read` windows and the width of the left latch's trigger range.
/// If the code is ever forked back into two boards, this test stops compiling, which is the point.
#[test]
fn one_flag_selects_every_documented_difference() {
    let mut mmc2 = Mmc2Mmc4::new(rom_data(9, 16, 32), false);
    let mut mmc4 = Mmc2Mmc4::new(rom_data(10, 16, 32), true);
    for m in [&mut mmc2, &mut mmc4] {
        m.cpu_write(0xA000, 0x05);
    }
    // PRG window: $A000 is the fixed third-from-last 8 KB bank on MMC2 (8 KB bank 29 is the top
    // half of 16 KB bank 14) and still inside MMC4's switchable 16 KB window.
    assert_eq!(mmc2.cpu_read(0xA000), 14, "MMC2: $A000 is fixed");
    assert_eq!(
        mmc4.cpu_read(0xA000),
        5,
        "MMC4: $A000 follows the 16 KB bank"
    );
    // Latch width: $0FDB is inside MMC4's $0FD8-$0FDF range and not MMC2's single $0FD8.
    for m in [&mut mmc2, &mut mmc4] {
        m.cpu_write(0xB000, 1);
        m.cpu_write(0xC000, 2);
        m.ppu_read(0x0FDB);
    }
    assert_eq!(mmc2.ppu_read(0x0000), 2, "MMC2: $0FDB is not a trigger");
    assert_eq!(mmc4.ppu_read(0x0000), 1, "MMC4: $0FDB is a trigger");
}

#[test]
fn load_builds_mmc4() {
    // Mapper 10, vertical mirroring: 16 x 16 KB PRG banks where every byte holds its bank number.
    let mut v = vec![
        0x4E, 0x45, 0x53, 0x1A, 16, 4, 0xA0, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    for bank in 0..16u8 {
        v.extend(vec![bank; 0x4000]);
    }
    v.resize(v.len() + 4 * 8192, 0);
    let mut m = load(&v).unwrap();
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 15);
    // $A000 selects a whole 16 KB bank, so $8000-$BFFF all come from it.
    m.cpu_write(0xA000, 0x03);
    assert_eq!(m.cpu_read(0x8000), 3);
    assert_eq!(m.cpu_read(0xBFFF), 3);
    assert_eq!(m.cpu_read(0xC000), 15);
}

#[test]
fn mmc4_prg_bank_is_sixteen_kb() {
    let mut m = mmc4_board();
    m.cpu_write(0xA000, 0x05);
    assert_eq!(m.cpu_read(0x8000), 5);
    assert_eq!(m.cpu_read(0x9FFF), 5);
    assert_eq!(m.cpu_read(0xA000), 5);
    assert_eq!(m.cpu_read(0xBFFF), 5);
    // The last 16 KB bank stays fixed at $C000.
    assert_eq!(m.cpu_read(0xC000), 15);
    assert_eq!(m.cpu_read(0xFFFF), 15);
}

#[test]
fn mmc4_prg_bank_uses_low_four_bits() {
    let mut m = mmc4_board();
    m.cpu_write(0xA000, 0xF5);
    assert_eq!(m.cpu_read(0x8000), 5);
}

#[test]
fn mmc4_chr_latch_switches_after_the_read() {
    let mut m = mmc4_board();
    m.cpu_write(0xB000, 1);
    m.cpu_write(0xC000, 2);
    // Power-on latch is $FE, so the $C000 register is the one in use.
    assert_eq!(m.ppu_read(0x0000), 2);
    // Any fetch in the tile $FD range switches the latch, but only after the value is taken.
    assert_eq!(m.ppu_read(0x0FDB), 2);
    assert_eq!(m.ppu_read(0x0000), 1);
}

#[test]
fn mmc4_chr_latch_returns_to_fe() {
    let mut m = mmc4_board();
    m.cpu_write(0xB000, 1);
    m.cpu_write(0xC000, 2);
    assert_eq!(m.ppu_read(0x0FD8), 2);
    assert_eq!(m.ppu_read(0x0000), 1);
    assert_eq!(m.ppu_read(0x0FE8), 1);
    assert_eq!(m.ppu_read(0x0000), 2);
}

#[test]
fn mmc4_chr_latch_one_uses_upper_pattern_table() {
    let mut m = mmc4_board();
    m.cpu_write(0xD000, 4);
    m.cpu_write(0xE000, 5);
    assert_eq!(m.ppu_read(0x1000), 5);
    assert_eq!(m.ppu_read(0x1FD8), 5);
    assert_eq!(m.ppu_read(0x1000), 4);
    assert_eq!(m.ppu_read(0x1FE8), 4);
    assert_eq!(m.ppu_read(0x1000), 5);
    // A $0000-table fetch does not touch latch 1 (latch 0 is still $FE, so it reads bank 0).
    assert_eq!(m.ppu_read(0x0FD8), 0);
    assert_eq!(m.ppu_read(0x1000), 5);
}

#[test]
fn mmc4_mirroring_register() {
    let mut m = mmc4_board();
    assert_eq!(m.mirroring(), Mirroring::Vertical);
    m.cpu_write(0xF000, 0x01);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
    m.cpu_write(0xF000, 0x00);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn mmc4_prg_ram_is_eight_kb() {
    let mut m = mmc4_board();
    assert_eq!(m.prg_ram().len(), 8192);
    m.cpu_write(0x6FFF, 0x42);
    assert_eq!(m.cpu_read(0x6FFF), 0x42);
}

#[test]
fn mmc4_state_roundtrip() {
    let mut a = mmc4_board();
    a.cpu_write(0xA000, 0x09);
    a.cpu_write(0xB000, 3);
    a.cpu_write(0xC000, 6);
    a.cpu_write(0xD000, 1);
    a.cpu_write(0xE000, 4);
    a.cpu_write(0xF000, 0x01);
    a.cpu_write(0x6010, 0x5A);
    a.ppu_read(0x0FDB); // latch 0 = $FD

    let mut b = Mmc2Mmc4::new(rom_data(10, 16, 32), true);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 9);
    assert_eq!(b.ppu_read(0x0000), 3);
    assert_eq!(b.cpu_read(0x6010), 0x5A);
}

#[test]
fn mmc4_state_roundtrip_chr_ram() {
    let mut a = Mmc2Mmc4::new(rom_data(10, 16, 0), true);
    a.cpu_write(0xB000, 1);
    a.cpu_write(0xC000, 0);
    a.ppu_write(0x0123, 0x77);
    a.ppu_read(0x0FE8);

    let mut b = Mmc2Mmc4::new(rom_data(10, 16, 0), true);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0123), 0x77);
}

/// A 16 x 16 KB PRG image whose every byte holds its 8 KB bank number, and either CHR-ROM whose
/// every 4 KB page holds `page * 3 + 1`, or (`chr_kb == 0`) no CHR at all, which the board backs
/// with 8 KB of RAM.
fn image(mapper: u8, chr_kb: usize) -> Vec<u8> {
    let mut v = vec![
        0x4E,
        0x45,
        0x53,
        0x1A,
        16,
        (chr_kb / 8) as u8,
        mapper << 4,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    v.extend((0..16 * 0x4000).map(|i| (i / 0x2000) as u8));
    v.extend((0..chr_kb * 1024).map(|i| ((i / 0x1000) * 3 + 1) as u8));
    v
}

/// FNV-1a 64, so the digest of a whole read stream fits in one number.
struct Fold(u64);

impl Fold {
    fn new() -> Fold {
        Fold(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Everything a fixed script lets the machine observe from a board: the bytes every read returns
/// (CPU, PPU and the side-effect-free `chr_peek`) and the exact save-state bytes.
fn observe(mapper: u8, chr_kb: usize) -> (u64, u64) {
    let mut m = load(&image(mapper, chr_kb)).unwrap();
    // Every register, with values that exercise the masks, plus two PRG RAM bytes.
    m.cpu_write(0x6000, 0x5A);
    m.cpu_write(0x6010, 0xA5);
    m.cpu_write(0xA000, 0x07);
    m.cpu_write(0xB000, 0x03);
    m.cpu_write(0xC000, 0x1E);
    m.cpu_write(0xD000, 0x05);
    m.cpu_write(0xE000, 0x1F);
    m.cpu_write(0xF000, 0x01);

    let mut reads = Fold::new();
    for addr in (0x6000u32..=0xFFFF).step_by(0x37) {
        reads.byte(m.cpu_read(addr as u16));
    }
    // Each latch arm probed from the state it is not in: $0FDB comes first, so MMC2 has to ignore
    // it and MMC4 has to follow it. Then the whole trigger neighbourhood through both tables.
    for addr in [
        0x0FDBu16, 0x0FE9, 0x1FD9, 0x1FE9, 0x0FDD, 0x1FDF, 0x0FEF, 0x1FEF,
    ] {
        reads.byte(m.ppu_read(addr));
        reads.byte(m.chr_peek(0x0000));
        reads.byte(m.chr_peek(0x1000));
    }
    // The tile $FD / $FE triggers and their neighbours through both pattern tables: this walks
    // every latch arm, including the addresses where MMC2 and MMC4 are meant to differ.
    for addr in (0x0FD0u16..=0x0FF0).chain(0x1FD0..=0x1FF0) {
        reads.byte(m.ppu_read(addr));
        reads.byte(m.chr_peek(0x0000));
        reads.byte(m.chr_peek(0x1000));
    }
    // A CHR-ROM board ignores `ppu_write`, so one script covers both CHR kinds.
    for addr in (0x0000u16..=0x1FFF).step_by(0x123) {
        m.ppu_write(addr, 0x9C);
        reads.byte(m.ppu_read(addr));
    }

    let mut w = StateWriter::new();
    m.save_state(&mut w);
    let mut state = Fold::new();
    for b in w.finish() {
        state.byte(b);
    }
    (reads.0, state.0)
}

/// The read and state digests recorded from the two-file MMC2/MMC4 code (before the
/// merge) with this exact script. Both boards keep their own value: MMC2's 8 KB PRG banking and
/// narrow left latch differ from MMC4's, so only a merge that changed nothing can reproduce these
/// four pairs. Recorded twice, on the two-file code and on the merged board, and equal.
const GOLDEN: [(u8, usize, u64, u64); 4] = [
    (9, 32, 0xa6b8_be10_14a9_a8ec, 0x859e_f1aa_7048_67e5),
    (9, 0, 0x2b58_3dcc_26b6_7c6a, 0xb7d3_abaf_a7d8_f4f9),
    (10, 32, 0xdfa2_742b_2bed_310c, 0x859e_f1aa_7048_67e5),
    (10, 0, 0x1388_cff1_54fe_3d2f, 0xb7d3_abaf_a7d8_f4f9),
];

#[test]
fn golden_observable_behaviour() {
    for (mapper, chr_kb, reads, state) in GOLDEN {
        assert_eq!(
            observe(mapper, chr_kb),
            (reads, state),
            "mapper {mapper}, CHR {chr_kb} KB: the merge changed what the board shows the machine"
        );
    }
}

/// The goldens above never read a CHR-RAM page that a bank select chose, so a board that ignored
/// the $B000-$EFFF selects would still match them. This writes both 4 KB pages of an 8 KB CHR-RAM
/// through the $FE latch, then selects each page back and reads it: each select must show the page
/// written under it, on MMC2 and MMC4 alike.
#[test]
fn chr_ram_bank_select_moves_the_reads() {
    for mapper in [9u8, 10] {
        let mut m = Mmc2Mmc4::new(rom_data(mapper, 16, 0), mapper == 10);
        // Latch 0 is on $FE from power-on, so $C000 picks the page the left table reads through.
        m.cpu_write(0xC000, 0x00);
        for addr in 0x0000..0x1000u16 {
            m.ppu_write(addr, 0x11);
        }
        m.cpu_write(0xC000, 0x01);
        for addr in 0x0000..0x1000u16 {
            m.ppu_write(addr, 0x22);
        }
        for (select, fill) in [(0x00u8, 0x11u8), (0x01, 0x22)] {
            m.cpu_write(0xC000, select);
            for addr in 0x0000..0x1000u16 {
                assert_eq!(
                    m.chr_peek(addr),
                    fill,
                    "mapper {mapper}, select {select}: {addr:#06x}"
                );
            }
        }
    }
}

/// The save-state layout, byte by byte: `prg_bank`, the four CHR banks, both latches, the
/// mirroring bit, then `bytes`-framed PRG RAM and (CHR-RAM boards only) CHR. Written out here so
/// a field that moves or reorders fails with its name rather than as a mystery digest mismatch.
fn staged(mapper: u8, chr_kb: usize) -> Vec<u8> {
    let mut m = load(&image(mapper, chr_kb)).unwrap();
    m.cpu_write(0x6000, 0x5A);
    m.cpu_write(0x6010, 0xA5);
    m.cpu_write(0xA000, 0x07);
    m.cpu_write(0xB000, 0x03);
    m.cpu_write(0xC000, 0x1E);
    m.cpu_write(0xD000, 0x05);
    m.cpu_write(0xE000, 0x1F);
    m.cpu_write(0xF000, 0x01);
    m.ppu_read(0x0FD8); // latch 0 = $FD, after the fetch has used the old bank
    let mut w = StateWriter::new();
    m.save_state(&mut w);
    w.finish()
}

#[test]
fn save_state_layout_is_pinned() {
    for mapper in [9u8, 10] {
        let s = staged(mapper, 0);
        assert_eq!(
            &s[0..8],
            &[0x07, 0x03, 0x1E, 0x05, 0x1F, 0xFD, 0xFE, 0x01],
            "mapper {mapper}: prg_bank, chr_0_fd, chr_0_fe, chr_1_fd, chr_1_fe, latch_0, latch_1, horizontal"
        );
        assert_eq!(
            &s[8..12],
            &8192u32.to_le_bytes(),
            "mapper {mapper}: PRG RAM length"
        );
        assert_eq!(s[12], 0x5A, "mapper {mapper}: PRG RAM starts at 12");
        assert_eq!(
            s[12 + 0x10],
            0xA5,
            "mapper {mapper}: PRG RAM is not the register block"
        );
        assert_eq!(
            &s[8204..8208],
            &8192u32.to_le_bytes(),
            "mapper {mapper}: CHR RAM length"
        );
        assert_eq!(
            s.len(),
            8208 + 8192,
            "mapper {mapper}: nothing trails the CHR RAM"
        );
    }
    // A CHR-ROM board leaves CHR out of the state entirely.
    assert_eq!(
        staged(9, 32).len(),
        8204,
        "MMC2 CHR-ROM state ends after the PRG RAM"
    );
    assert_eq!(
        staged(10, 32).len(),
        8204,
        "MMC4 CHR-ROM state ends after the PRG RAM"
    );
}
