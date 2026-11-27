use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::mapper::action52::Action52;
use tendy_nes::mapper::cnrom::Cnrom;
use tendy_nes::mapper::cnrom_security::CnromSecurity;
use tendy_nes::mapper::cnrom_unrom::{CnromUnrom, Kind as CuKind};
use tendy_nes::mapper::cprom::Cprom;
use tendy_nes::mapper::gnrom_moved::{GnromMoved, Kind as GnromKind};
use tendy_nes::mapper::gtrom::Gtrom;
use tendy_nes::mapper::j87::J87;
use tendy_nes::mapper::mmc1::Mmc1;
use tendy_nes::mapper::nina::Nina;
use tendy_nes::mapper::nrom::Nrom;
use tendy_nes::mapper::sunsoft_latch::{Sunsoft1, Sunsoft2};
use tendy_nes::mapper::unrom_variants::{Un1rom, UnromInverted};
use tendy_nes::mapper::unrom512::Unrom512;
use tendy_nes::mapper::uxrom::Uxrom;
use tendy_nes::mapper::vs::Vs;
use tendy_nes::state::{StateError, StateReader, StateWriter};

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

/// `rom_data` with 0xFF in every PRG byte after its bank number, so a board whose register writes
/// conflict with the PRG bus latches the value the test wrote. Each bank's first byte still holds its
/// number, which is what the tests read to tell the banks apart.
fn rom_data_ffff(mapper: u8, prg_banks: usize, chr_kb: usize) -> RomData {
    let mut rom = rom_data(mapper, prg_banks, chr_kb);
    for bank in 0..prg_banks {
        rom.prg[bank * 0x4000] = bank as u8;
        rom.prg[bank * 0x4000 + 1..(bank + 1) * 0x4000].fill(0xFF);
    }
    rom
}

fn mmc1_write(m: &mut Mmc1, addr: u16, val: u8) {
    for i in 0..5 {
        m.cpu_write(addr, (val >> i) & 1);
    }
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
fn nrom_chr_ram() {
    let mut a = Nrom::new(rom_data(0, 2, 0));
    a.cpu_write(0x6010, 0x5A);
    a.ppu_write(0x0123, 0x77);
    let mut b = Nrom::new(rom_data(0, 2, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0123), 0x77);
}

#[test]
fn nrom_chr_rom() {
    let mut a = Nrom::new(rom_data(0, 2, 8));
    a.cpu_write(0x7000, 9);
    let mut b = Nrom::new(rom_data(0, 2, 8));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
}

#[test]
fn uxrom() {
    let mut a = Uxrom::new(rom_data(2, 8, 0));
    a.cpu_write(0x8000, 5);
    a.cpu_write(0x6001, 0x33);
    a.ppu_write(0x1000, 0x44);
    let mut b = Uxrom::new(rom_data(2, 8, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 5);
}

#[test]
fn cnrom() {
    let mut a = Cnrom::new(rom_data(3, 2, 32));
    a.cpu_write(0x8000, 2);
    a.cpu_write(0x6002, 0x12);
    let mut b = Cnrom::new(rom_data(3, 2, 32));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0000), 4);
}

#[test]
fn mmc1_with_partial_shift() {
    let mut a = Mmc1::new(rom_data(1, 8, 0));
    mmc1_write(&mut a, 0x8000, 0x0E);
    mmc1_write(&mut a, 0xA000, 3);
    mmc1_write(&mut a, 0xC000, 1);
    mmc1_write(&mut a, 0xE000, 4);
    a.cpu_write(0x7000, 0x99);
    a.ppu_write(0x0200, 0x66);
    a.cpu_write(0x8000, 1); // shift register mid-load
    a.cpu_write(0x8000, 0);
    let mut b = Mmc1::new(rom_data(1, 8, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    // Finishing the partial load behaves identically.
    for m in [&mut a, &mut b] {
        for _ in 0..3 {
            m.cpu_write(0x8000, 1);
        }
    }
    assert_same(&mut a, &mut b);
}

#[test]
fn action52() {
    let mut a = Action52::new(rom_data(228, 8, 8));
    a.cpu_write(0xA5C3, 0x02);
    let mut b = Action52::new(rom_data(228, 8, 8));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
}

#[test]
fn cprom() {
    let mut a = Cprom::new(rom_data_ffff(13, 2, 0));
    a.cpu_write(0xFFF0, 0x01);
    a.ppu_write(0x1000, 0x5A);
    let mut b = Cprom::new(rom_data_ffff(13, 2, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.ppu_read(0x1000),
        0x5A,
        "the page and its contents survive"
    );
}

#[test]
fn j87() {
    let mut a = J87::new(rom_data(87, 2, 32));
    a.cpu_write(0x6000, 0x01);
    let mut b = J87::new(rom_data(87, 2, 32));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0000), 4, "the crossed-bit CHR bank survives");
}

/// A state saved between the cycles of a flash command resumes the command: the image, the
/// register, both RAM chips, the decoder's position and the software-ID latch all come back.
#[test]
fn gtrom_with_a_half_finished_flash_sequence() {
    let mut a = Gtrom::new(rom_data(111, 32, 0));
    a.cpu_write(0x5000, 0x35); // PRG page 5, CHR page 1, nametable page 1
    a.ppu_write(0x0123, 0x77);
    let mut ciram = [0u8; 0x1000];
    a.nt_write(0x2123, 0x66, &mut ciram);
    // Software ID (Table 4-2): AA, 55, 90. The ID reads at chip address 0, which the PRG window
    // reaches only on page 0 — hence the page-0 write before the read.
    a.cpu_write(0xD555, 0xAA);
    a.cpu_write(0xAAAA, 0x55);
    a.cpu_write(0xD555, 0x90);
    a.cpu_write(0x5000, 0x30); // PRG page 0, the RAM pages as they were
    assert_eq!(
        a.cpu_read(0x8000),
        0xBF,
        "the ID latch is on before the save"
    );
    a.cpu_write(0xD555, 0xAA); // the unlock, with the rest of the sequence still to come

    let mut b = Gtrom::new(rom_data(111, 32, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.nt_read(0x2123, &ciram),
        Some(0x66),
        "the chosen nametable page and its byte"
    );
    // $BF only while the latch is on: without the saved `id_mode` this reads the PRG byte $00.
    assert_eq!(
        b.cpu_read(0x8000),
        0xBF,
        "the software-ID latch is restored"
    );

    // Both boards finish the byte-program sequence from where the state left off.
    for m in [&mut a, &mut b] {
        m.cpu_write(0xAAAA, 0x55);
        m.cpu_write(0xD555, 0xA0);
        m.cpu_write(0xC000, 0x00); // chip $4000: PRG byte $01 on page 0, programmed to $00
    }
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.cpu_read(0xC000),
        0x00,
        "the program cycle both boards ran is on the chip"
    );
}

#[test]
fn sunsoft1() {
    let mut a = Sunsoft1::new(rom_data(184, 2, 32));
    a.cpu_write(0x6000, 0x53);
    let mut b = Sunsoft1::new(rom_data(184, 2, 32));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0000), 3);
    assert_eq!(b.ppu_read(0x1000), 5, "both page registers survive");
}

#[test]
fn sunsoft2() {
    let mut a = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    a.cpu_write(0xFFF0, 0x31);
    a.ppu_write(0x0010, 0x5A);
    let mut b = Sunsoft2::new(rom_data_ffff(93, 8, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 3, "the PRG bank survives");
    assert_eq!(
        b.ppu_read(0x0010),
        0x5A,
        "and the CHR-RAM with its enable bit"
    );
}

#[test]
fn un1rom() {
    let mut a = Un1rom::new(rom_data_ffff(94, 8, 0));
    a.cpu_write(0xFFF0, 0x0C);
    a.ppu_write(0x0100, 0x66);
    let mut b = Un1rom::new(rom_data_ffff(94, 8, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 3, "the shifted bank register survives");
    assert_eq!(b.ppu_read(0x0100), 0x66);
}

#[test]
fn unrom_inverted() {
    let mut a = UnromInverted::new(rom_data_ffff(180, 8, 0));
    a.cpu_write(0xFFF0, 0x05);
    a.ppu_write(0x0200, 0x77);
    let mut b = UnromInverted::new(rom_data_ffff(180, 8, 0));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0xC000), 5, "the upper bank survives");
    assert_eq!(b.ppu_read(0x0200), 0x77);
}

#[test]
fn gnrom_moved_38() {
    let mut a = GnromMoved::new(rom_data(38, 8, 32), GnromKind::BitCorp);
    a.cpu_write(0x7000, 0x31);
    let mut b = GnromMoved::new(rom_data(38, 8, 32), GnromKind::BitCorp);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 2, "the PRG and CHR banks survive");
    assert_eq!(b.ppu_read(0x0000), 6);
}

#[test]
fn gnrom_moved_140() {
    let mut a = GnromMoved::new(rom_data(140, 8, 32), GnromKind::JalecoJf);
    a.cpu_write(0x6000, 0x23);
    let mut b = GnromMoved::new(rom_data(140, 8, 32), GnromKind::JalecoJf);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 4);
    assert_eq!(b.ppu_read(0x0000), 6);
}

#[test]
fn nina_79() {
    let mut a = Nina::new(rom_data(79, 4, 64));
    a.cpu_write(0x4100, 0x0D);
    let mut b = Nina::new(rom_data(79, 4, 64));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.cpu_read(0x8000), 2);
    assert_eq!(b.ppu_read(0x0000), 10);
}

#[test]
fn cosmo_carrier_78() {
    let mut a = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, false);
    a.cpu_write(0x8001, 0x2A);
    let mut b = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, false);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.mirroring(),
        Mirroring::SingleScreenUpper,
        "the mirroring bit survives"
    );
    assert_eq!(b.cpu_read(0x8000), 2);
}

#[test]
fn holy_diver_78() {
    let mut a = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, true);
    a.cpu_write(0x8001, 0x1C);
    let mut b = CnromUnrom::new(rom_data_ffff(78, 8, 128), CuKind::M78, true);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.mirroring(),
        Mirroring::Vertical,
        "the mirroring bit survives"
    );
}

#[test]
fn arkanoid_2_152() {
    let mut a = CnromUnrom::new(rom_data_ffff(152, 8, 128), CuKind::M152, false);
    a.cpu_write(0x8001, 0xA5);
    let mut b = CnromUnrom::new(rom_data_ffff(152, 8, 128), CuKind::M152, false);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.mirroring(),
        Mirroring::SingleScreenUpper,
        "the mirroring bit survives"
    );
    assert_eq!(b.ppu_read(0x0000), 10);
}

#[test]
fn cnrom_security_185() {
    let mut rom = rom_data_ffff(185, 2, 8);
    rom.header.submapper = 4;
    let mut a = CnromSecurity::new(rom);
    a.cpu_write(0x8001, 0x02);
    let mut rom = rom_data_ffff(185, 2, 8);
    rom.header.submapper = 4;
    let mut b = CnromSecurity::new(rom);
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.ppu_read(0x0000),
        0,
        "the disabled CHR-ROM survives as disabled"
    );
}

#[test]
fn unrom512_with_a_half_finished_flash_command() {
    // AC5: the flash decoder is mid-sequence, which a save state has to keep. The battery
    // bit is what makes the board's PRG a flash chip (nesdev "UNROM 512").
    let flashable = || {
        let mut rom = rom_data(30, 16, 0);
        rom.header.battery = true;
        rom
    };
    let mut a = Unrom512::new(flashable());
    a.cpu_write(0xC000, 0x01); // register: PRG bank 1, so $9555 is chip address $5555
    a.cpu_write(0x9555, 0xAA); // waiting for 55 at $2AAA
    a.ppu_write(0x0100, 0x66);

    let mut b = Unrom512::new(flashable());
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(b.ppu_read(0x0100), 0x66);

    // Finishing the sequence reaches software-ID mode on both boards; a state that had lost the
    // half-finished position would leave the second one out of it.
    for m in [&mut a, &mut b] {
        m.cpu_write(0xC000, 0x00);
        m.cpu_write(0xAAAA, 0x55);
        m.cpu_write(0xC000, 0x01);
        m.cpu_write(0x9555, 0x90);
        m.cpu_write(0xC000, 0x00);
        assert_eq!(
            m.cpu_read(0x8000),
            tendy_nes::mapper::flash::MANUFACTURER_ID
        );
    }
}

#[test]
fn vs_mapper_99_board_state() {
    // 48 KiB of PRG, so the `$4016` bit 2 has a second `$8000` bank to switch to, and two 8 KiB
    // CHR pages for it to choose between.
    let mut a = Vs::new(rom_data(99, 3, 16));
    a.write_4016(0x04);
    a.cpu_write(0x6002, 0x12);
    let mut b = Vs::new(rom_data(99, 3, 16));
    roundtrip(&a, &mut b);
    assert_same(&mut a, &mut b);
    assert_eq!(
        b.ppu_read(0x0000),
        2,
        "the CHR page the bank bit chose survives"
    );
    assert_eq!(b.cpu_read(0x8000), 2, "and so does the PRG bank at $8000");
    assert_eq!(b.cpu_read(0x6002), 0x12, "PRG RAM rides along");
}

#[test]
fn truncated_state_is_an_error() {
    let a = Nrom::new(rom_data(0, 2, 0));
    let mut w = StateWriter::new();
    a.save_state(&mut w);
    let bytes = w.finish();
    let mut b = Nrom::new(rom_data(0, 2, 0));
    assert_eq!(
        b.load_state(&mut StateReader::new(&bytes[..bytes.len() - 1])),
        Err(StateError::Truncated)
    );
}
