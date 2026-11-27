//! What each core reports, and the drawer's rule that keeps a row its game cannot use
//! (DESIGN §2.3, §8; issue #16).
//!
//! Every expectation is the §2.3 table written out: the core's whole set is compared, so a
//! capability that appears or vanishes is a failure. The images are built in code, so the test
//! needs no ROM folder.

use std::fs;
use std::path::PathBuf;

use tendy_core::{Capabilities, Capability, Machine};
use tendy_gb::{GbMachine, GbModel, GbOptions};
use tendy_gba::GbaMachine;
use tendy_nes::console::ConsoleConfig;
use tendy_nes::{NesImage, NesMachine};
use tendy_snes::SnesMachine;

use tendyemu::app::App;
use tendyemu::caps::{self, RowGate};
use tendyemu::menu::{Request, Row, Rows};
use tendyemu::paths::Paths;

/// Every capability in the contract, so a reported set can be read back whole.
const ALL: [Capability; 22] = [
    Capability::ChannelMask,
    Capability::Cheats,
    Capability::SgbBorder,
    Capability::SgbSound,
    Capability::BootRom,
    Capability::Palettes,
    Capability::LcdFilter,
    Capability::Link,
    Capability::Tilt,
    Capability::DiskSwap,
    Capability::Debugger,
    Capability::Trace,
    Capability::MemoryView,
    Capability::Movies,
    Capability::Tas,
    Capability::RunAhead,
    Capability::HdPacks,
    Capability::CrtNtsc,
    Capability::Shaders,
    Capability::RetroAchievements,
    Capability::NsfPlayer,
    Capability::VsSystem,
];

/// The capabilities a machine reports, in [`ALL`] order.
fn reported(caps: Capabilities) -> Vec<Capability> {
    ALL.into_iter().filter(|cap| caps.has(*cap)).collect()
}

/// The set is exactly `expected`: nothing missing, nothing extra.
fn assert_caps(caps: Capabilities, expected: &[Capability], what: &str) {
    let have = reported(caps);
    assert_eq!(have.len(), expected.len(), "{what}: {have:?}");
    for cap in expected {
        assert!(caps.has(*cap), "{what}: missing {cap:?} in {have:?}");
    }
}

/// The eight capabilities every Game Boy model reports (DESIGN §2.3).
fn gb_base() -> Vec<Capability> {
    vec![
        Capability::ChannelMask,
        Capability::Cheats,
        Capability::BootRom,
        Capability::Palettes,
        Capability::LcdFilter,
        Capability::Link,
        Capability::Debugger,
        Capability::Trace,
    ]
}

/// The thirteen capabilities every NES image reports, whatever its medium (DESIGN §2.3).
fn nes_base() -> Vec<Capability> {
    vec![
        Capability::ChannelMask,
        Capability::Cheats,
        Capability::Palettes,
        Capability::Debugger,
        Capability::Trace,
        Capability::MemoryView,
        Capability::Movies,
        Capability::Tas,
        Capability::RunAhead,
        Capability::HdPacks,
        Capability::CrtNtsc,
        Capability::Shaders,
        Capability::RetroAchievements,
    ]
}

/// `base` plus the "only when" rows of the table.
fn plus(mut base: Vec<Capability>, extra: &[Capability]) -> Vec<Capability> {
    base.extend_from_slice(extra);
    base
}

/// A 32 KiB Game Boy cartridge. `cart_type` is the header byte at `$0147`; the header checksum
/// is computed the way the boot ROM checks it.
fn gb_rom(cart_type: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"CAPSGB  ");
    rom[0x147] = cart_type;
    rom[0x148] = 0x00;
    rom[0x149] = if cart_type == 0x03 || cart_type == 0x22 {
        0x02
    } else {
        0x00
    };
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// The Super Game Boy's own cartridge: 256 KiB with its title where the loader looks for it.
fn sgb_sound_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x40000];
    rom[0x7FC0..0x7FC0 + 13].copy_from_slice(b"Super GAMEBOY");
    rom
}

/// A 32 KiB Game Boy Advance cartridge: an ARM branch at $08000000 and a sound header.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // `b $080000C0`: the offset is (0xC0 - 8) / 4 = 0x2E.
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"CAPSGBA ");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An iNES cartridge: 16 KiB of PRG (NOPs, with the reset vector at $8000) and 8 KiB of CHR.
fn nes_rom() -> Vec<u8> {
    let mut rom = vec![b'N', b'E', b'S', 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    rom.extend(std::iter::repeat_n(0xEAu8, 16 * 1024));
    rom.extend(std::iter::repeat_n(0x00u8, 8 * 1024));
    let vectors = 16 + 16 * 1024 - 6;
    for at in [vectors, vectors + 2, vectors + 4] {
        rom[at] = 0x00;
        rom[at + 1] = 0x80;
    }
    rom
}

/// A one-side Famicom Disk System disk: disk-info block, one file, four bytes of file data.
fn fds_image() -> Vec<u8> {
    use tendy_nes::mapper::fds;
    let mut side = vec![0u8; fds::SIDE_LEN];
    side[0] = 1; // disk info block
    side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
    side[0x38] = 2; // file-count block
    side[0x39] = 1;
    side[0x3A] = 3; // file header block
    side[0x3A + 0x0B] = 0x00;
    side[0x3A + 0x0C] = 0x60;
    side[0x3A + 0x0D] = 0x04;
    side[0x3A + 0x10] = 4; // file data block
    side[0x3A + 0x11..0x3A + 0x15].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    let mut image = fds::FDS_MAGIC.to_vec();
    image.push(1);
    image.extend_from_slice(&[0u8; 11]);
    image.extend_from_slice(&side);
    image
}

/// An 8 KiB FDS BIOS stub: `NOP` fill with the reset vector at $E000.
fn fds_bios() -> Vec<u8> {
    let mut bios = vec![0xEAu8; tendy_nes::mapper::fds::BIOS_LEN];
    bios[0x1FFC] = 0x00;
    bios[0x1FFD] = 0xE0;
    bios
}

/// A one-song NSF whose PLAY routine starts pulse 1; INIT is a bare RTS.
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
    nsf[0x0E..0x17].copy_from_slice(b"caps test");
    nsf.extend_from_slice(&program);
    nsf
}

/// A 32 KiB LoROM cartridge: a header at $7FC0 whose map nibble names LoROM.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"CAPS SNES TEST GAME  ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

#[test]
fn capabilities_gb() {
    // Every model runs the cartridges it always did: the same eight, no border or sound chip
    // until the console is an SGB, no tilt until the cartridge is an MBC7.
    for model in [GbModel::Auto, GbModel::Dmg, GbModel::Cgb] {
        let machine = GbMachine::new(
            gb_rom(0x00),
            GbOptions {
                model,
                ..GbOptions::default()
            },
        )
        .unwrap_or_else(|why| panic!("{model:?} boots: {why}"));
        assert_caps(machine.capabilities(), &gb_base(), &format!("{model:?}"));
    }

    // An MBC7 cartridge adds tilt, and only tilt.
    let tilt = GbMachine::new(
        gb_rom(0x22),
        GbOptions {
            model: GbModel::Dmg,
            ..GbOptions::default()
        },
    )
    .expect("an MBC7 cartridge boots");
    assert_caps(
        tilt.capabilities(),
        &plus(gb_base(), &[Capability::Tilt]),
        "MBC7",
    );
}

#[test]
fn capabilities_sgb_with_sound_rom() {
    let sgb = GbMachine::new(
        gb_rom(0x00),
        GbOptions {
            model: GbModel::Sgb,
            sgb_sound_rom: Some(sgb_sound_rom()),
            ..GbOptions::default()
        },
    )
    .expect("the Super Game Boy boots the cartridge");
    assert_caps(
        sgb.capabilities(),
        &plus(gb_base(), &[Capability::SgbBorder, Capability::SgbSound]),
        "SGB with sound",
    );

    // Without the cartridge the border stays and the sound is not offered.
    let silent = GbMachine::new(
        gb_rom(0x00),
        GbOptions {
            model: GbModel::Sgb,
            ..GbOptions::default()
        },
    )
    .expect("the Super Game Boy boots the cartridge");
    assert_caps(
        silent.capabilities(),
        &plus(gb_base(), &[Capability::SgbBorder]),
        "SGB without sound",
    );
}

#[test]
fn capabilities_gba() {
    let machine = GbaMachine::new(gba_rom(), None).expect("a GBA cartridge boots");
    assert_caps(
        machine.capabilities(),
        &[
            Capability::ChannelMask,
            Capability::Cheats,
            Capability::Link,
            Capability::Debugger,
            Capability::Trace,
        ],
        "GBA",
    );
}

#[test]
fn capabilities_nes_cart() {
    let machine = NesMachine::new(
        nes_rom(),
        NesImage::Cartridge,
        None,
        ConsoleConfig::default(),
    )
    .expect("an iNES cartridge boots");
    // A cartridge has no disk to swap, no NSF player and no Vs. controls.
    assert_caps(machine.capabilities(), &nes_base(), "NES cartridge");
}

#[test]
fn capabilities_nes_disk() {
    let machine = NesMachine::new(
        fds_image(),
        NesImage::Disk { qd: false },
        Some(fds_bios()),
        ConsoleConfig::default(),
    )
    .expect("the disk mounts");
    assert_caps(
        machine.capabilities(),
        &plus(nes_base(), &[Capability::DiskSwap]),
        "FDS",
    );
}

#[test]
fn capabilities_nsf() {
    let machine = NesMachine::new(
        nsf_image(),
        NesImage::Nsf { song: 1 },
        None,
        ConsoleConfig::default(),
    )
    .expect("the NSF boots");
    assert_caps(
        machine.capabilities(),
        &plus(nes_base(), &[Capability::NsfPlayer]),
        "NSF",
    );
}

#[test]
fn capabilities_snes() {
    let machine = SnesMachine::new(snes_rom()).expect("a LoROM cartridge boots");
    assert_caps(machine.capabilities(), &[Capability::ChannelMask], "SNES");
}

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-caps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    fn rom(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, bytes).expect("the scratch ROM is writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn menu_hides_rows_without_capability() {
    // The drawer keeps a module's row only when the running core reports the row's capability
    // (menu::Rows::collect filters with caps::visible). So: every row a core's drawer holds has
    // its gate met.
    let scratch = Scratch::new("menu");
    let images: [(&str, Vec<u8>); 4] = [
        ("caps.gb", gb_rom(0x00)),
        ("caps.gba", gba_rom()),
        ("caps.nes", nes_rom()),
        ("caps.sfc", snes_rom()),
    ];
    let mut app = App::new(scratch.paths());
    for (name, bytes) in images {
        let path = scratch.rom(name, &bytes);
        app.open(&path)
            .unwrap_or_else(|why| panic!("{name} opens: {why}"));
        let caps = app.capabilities();
        assert_eq!(
            caps,
            app.machine().expect("a game runs").capabilities(),
            "{name}: the drawer reads the running core's report"
        );
        let rows = Rows::collect(&app);
        for group in rows.groups() {
            for row in &group.rows {
                assert!(
                    caps::visible(
                        &RowGate {
                            requires: row.requires
                        },
                        caps
                    ),
                    "{name}: `{}` survived in the {} group without its capability",
                    row.label,
                    group.module.heading(),
                );
            }
        }
    }

    // The rule on a row the system modules add later: a disk row is shown for a disk system and
    // hidden for every core that does not report disk swap.
    let disk = Row::action("Swap disk", "The other side", "", Request::Reset)
        .requiring(Capability::DiskSwap);
    let gate = RowGate {
        requires: disk.requires,
    };
    let snes = SnesMachine::new(snes_rom())
        .expect("a LoROM cartridge boots")
        .capabilities();
    let cartridge = NesMachine::new(
        nes_rom(),
        NesImage::Cartridge,
        None,
        ConsoleConfig::default(),
    )
    .expect("an iNES cartridge boots")
    .capabilities();
    let disk_system = NesMachine::new(
        fds_image(),
        NesImage::Disk { qd: false },
        Some(fds_bios()),
        ConsoleConfig::default(),
    )
    .expect("the disk mounts")
    .capabilities();
    assert!(!caps::visible(&gate, snes));
    assert!(!caps::visible(&gate, cartridge));
    assert!(caps::visible(&gate, disk_system));
}
