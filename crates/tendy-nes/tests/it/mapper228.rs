use tendy_nes::cart::{Header, Mapper, Mirroring, RomData};
use tendy_nes::controller::BUTTON_START;
use tendy_nes::mapper::action52::Action52;
use tendy_nes::mapper::load;
use tendy_nes::nes::Nes;
use tendy_nes::state::fnv1a64;

/// Action 52 layout: three 512 KB PRG chips (file chips 0, 1, 2 = board chips 0, 1, 3) where every byte holds
/// `file_chip * 32 + page`, and 512 KB of CHR where every byte holds its 8 KB bank number.
fn board() -> Action52 {
    Action52::new(RomData {
        header: Header {
            prg_banks: 96,
            chr_banks: 64,
            mapper: 228,
            mirroring: Mirroring::Vertical,
            battery: false,
            trainer: false,
            ..Default::default()
        },
        prg: (0..96 * 0x4000).map(|i| (i / 0x4000) as u8).collect(),
        chr: (0..64 * 0x2000).map(|i| (i / 0x2000) as u8).collect(),
        chr_is_ram: false,
        trainer: None,
    })
}

/// The register address for a chip, 16 KB page, PRG mode bit, CHR high bits and mirroring bit.
fn reg(chip: u16, page: u16, mode16: bool, chr_hi: u16, horizontal: bool) -> u16 {
    0x8000
        | (u16::from(horizontal) << 13)
        | (chip << 11)
        | (page << 6)
        | (u16::from(mode16) << 5)
        | chr_hi
}

#[test]
fn power_on_maps_first_32k() {
    let mut m = board();
    assert_eq!(m.cpu_read(0x8000), 0);
    assert_eq!(m.cpu_read(0xC000), 1);
    assert_eq!(m.ppu_read(0x0000), 0);
    assert_eq!(m.mirroring(), Mirroring::Vertical);
}

#[test]
fn mode_32k_ignores_page_bit_0() {
    let mut m = board();
    m.cpu_write(reg(0, 5, false, 0, false), 0);
    assert_eq!(m.cpu_read(0x8000), 4);
    assert_eq!(m.cpu_read(0xFFFF), 5);
}

#[test]
fn mode_16k_mirrors_page() {
    let mut m = board();
    m.cpu_write(reg(0, 5, true, 0, false), 0);
    assert_eq!(m.cpu_read(0x8000), 5);
    assert_eq!(m.cpu_read(0xC000), 5);
}

#[test]
fn chip_1_follows_chip_0() {
    let mut m = board();
    m.cpu_write(reg(1, 2, true, 0, false), 0);
    assert_eq!(m.cpu_read(0x8000), 34);
}

#[test]
fn chip_3_is_third_in_file() {
    let mut m = board();
    m.cpu_write(reg(3, 31, true, 0, false), 0);
    assert_eq!(m.cpu_read(0x8000), 95);
}

#[test]
fn chip_2_is_open_bus() {
    let mut m = board();
    m.cpu_write(reg(2, 0, true, 0, false), 0);
    assert_eq!(m.cpu_read(0x8000), 0x80);
    assert_eq!(m.cpu_read(0xC123), 0xC1);
}

#[test]
fn chr_bank_from_address_and_data() {
    let mut m = board();
    m.cpu_write(reg(0, 0, false, 0x0A, false), 0x03);
    assert_eq!(m.ppu_read(0x0000), 43);
    assert_eq!(m.ppu_read(0x1FFF), 43);
}

#[test]
fn chr_is_read_only() {
    let mut m = board();
    m.ppu_write(0x0010, 99);
    assert_eq!(m.ppu_read(0x0010), 0);
}

#[test]
fn mirroring_bit_13() {
    let mut m = board();
    m.cpu_write(reg(0, 0, false, 0, true), 0);
    assert_eq!(m.mirroring(), Mirroring::Horizontal);
}

#[test]
fn no_prg_ram() {
    assert!(board().prg_ram().is_empty());
}

#[test]
fn load_supports_228() {
    let mut bytes = vec![
        0x4E, 0x45, 0x53, 0x1A, 2, 1, 0x40, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    bytes.resize(16 + 2 * 16384 + 8192, 0);
    assert!(load(&bytes).is_ok());
}

#[test]
#[ignore = "needs your own copy of roms/Action 52 (U) [!].nes"]
fn action_52_reaches_menu() {
    let path = crate::common::rom_path("Action 52 (U) [!].nes");
    let mut nes = Nes::new(&std::fs::read(path).unwrap()).unwrap();
    // Press Start for 6 frames every 240 frames to skip the intro screens.
    for f in 1..=950 {
        let start = f % 240 >= 200 && f % 240 < 206;
        nes.bus.pads[0].buttons = if start { BUTTON_START } else { 0 };
        nes.run_frame();
    }
    assert_ne!(nes.bus.ppu.mask & 0x08, 0, "background never turned on");
    // Reviewed snapshot: the "ACTION 52" game menu (01. FIREBREATHER ... 18. ATMOS QUAKE).
    assert_eq!(fnv1a64(&nes.bus.ppu.framebuffer), 0xc99a_752d_36ce_63fb);
}
