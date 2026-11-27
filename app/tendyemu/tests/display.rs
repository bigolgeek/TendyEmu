//! Display: one window, five picture shapes (DESIGN §2.4, §6.4, §7.2, §8; issue #18).
//!
//! The picture's size comes from each core's own `frame()`, so every test opens a machine: built
//! here in code, or read from the folders named by `TENDY_TEST_ROMS` and `TENDY_NES_TEST_ROMS`
//! where a core has a real cartridge to offer. A run without those folders prints the skip line
//! and still checks every size the images can build. The geometry is checked without a window:
//! `display_rect`, `fit_scale` and `Follower` are pure.
//!
//! U19 rides on `native_size_snes`: the Super NES picture is 256x224 and no ROM moves it. The
//! manifest in `scripts/test-roms.txt` holds no hi-res or overscan image, so the real-ROM half
//! runs the INIDISP hammer test; the core builds 256 columns and 224 lines only (§3.6 C7), which
//! is what makes 512-wide and 239-line output impossible rather than merely unused.

use std::fs;
use std::path::{Path, PathBuf};

use tendyemu::app::App;
use tendyemu::config;
use tendyemu::display::{self, Aspect, DisplayOptions, Follower, MAX_SCALE, MIN_SCALE, Picture};
use tendyemu::menu::RowAct;
use tendyemu::paths::Paths;
use tendyemu::settings::Direction;
use tendyemu::status_bar::BAR_H;

/// A scratch config folder, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-display-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch folder is writable");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    /// Writes `bytes` as `name` inside the scratch folder and hands back the path.
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

/// A 32 KiB Game Boy cartridge.
fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"DISPLAY ");
    rom[0x147] = 0x00;
    rom[0x148] = 0x00;
    rom[0x149] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// A 32 KiB Game Boy Advance cartridge: an ARM branch at $08000000 and a sound header.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"DISPLAY ");
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

/// A 32 KiB LoROM cartridge: a header at $7FC0 whose map nibble names LoROM.
fn snes_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let at = 0x7FC0;
    rom[at..at + 21].copy_from_slice(b"DISPLAY SNES TEST    ");
    rom[at + 0x15] = 0x20;
    rom[at + 0x16] = 0x00;
    rom[at + 0x17] = 0x05;
    rom
}

fn all_roms() -> Vec<(String, Vec<u8>)> {
    vec![
        ("display.gb".to_string(), gb_rom()),
        ("display.gba".to_string(), gba_rom()),
        ("display.nes".to_string(), nes_rom()),
        ("display.sfc".to_string(), snes_rom()),
    ]
}

/// The running picture's size, as the window loop reads it.
fn size(app: &mut App) -> Picture {
    let frame = app.frame().expect("a running machine has a picture");
    Picture::from_frame((frame.width, frame.height))
}

/// Opens a ROM built in code and hands back its picture.
fn open_built(app: &mut App, scratch: &Scratch, name: &str, bytes: &[u8]) -> Picture {
    app.open(&scratch.rom(name, bytes))
        .unwrap_or_else(|why| panic!("{name}: {why}"));
    size(app)
}

fn test_roms() -> Option<PathBuf> {
    std::env::var_os("TENDY_TEST_ROMS")
        .map(PathBuf::from)
        .filter(|folder| folder.is_dir())
}

fn nes_test_roms() -> Option<PathBuf> {
    std::env::var_os("TENDY_NES_TEST_ROMS")
        .map(PathBuf::from)
        .filter(|folder| folder.is_dir())
}

/// Opens a real cartridge from `folder`, or prints the skip line and answers `None`.
fn open_real(app: &mut App, folder: Option<&Path>, name: &str, hint: &str) -> Option<Picture> {
    let Some(folder) = folder else {
        println!("# skipped: {hint} is not set (no real cartridge measured)");
        return None;
    };
    let path = folder.join(name);
    if !path.is_file() {
        println!("# skipped: {name} not in {hint}");
        return None;
    }
    app.open(&path)
        .unwrap_or_else(|why| panic!("{}: {why}", path.display()));
    Some(size(app))
}

#[test]
fn native_size_gb() {
    let scratch = Scratch::new("gb");
    let mut app = App::new(scratch.paths());
    let picture = open_built(&mut app, &scratch, "display.gb", &gb_rom());
    assert_eq!(picture, Picture::GAME_BOY, "DMG is 160x144");
    assert_eq!(
        display::native_size(tendy_core::SystemKind::Gb, false),
        picture
    );
    let options = DisplayOptions::DEFAULT;
    assert_eq!(display::window_size(picture, &options), (480, 456));
    assert_eq!(
        display::display_rect(picture, &options, 480.0, 456.0, 1.0),
        (0.0, 0.0, 480.0, 432.0),
        "3x the picture, the bar under it"
    );
    if let Some(real) = open_real(
        &mut app,
        test_roms().as_deref(),
        "dmg-acid2.gb",
        "TENDY_TEST_ROMS",
    ) {
        assert_eq!(real, Picture::GAME_BOY, "a real DMG cartridge");
    }
}

#[test]
fn native_size_sgb_border() {
    // The border picture is 256x224: the 160x144 screen inside the frame the cartridge sends.
    assert_eq!(
        display::native_size(tendy_core::SystemKind::Gb, true),
        Picture::SGB
    );
    assert_eq!(
        (Picture::SGB.width, Picture::SGB.height),
        (256, 224),
        "DESIGN §2.4"
    );
    let options = DisplayOptions::DEFAULT;
    assert_eq!(display::window_size(Picture::SGB, &options), (768, 696));
    assert_eq!(
        display::display_rect(Picture::SGB, &options, 768.0, 696.0, 1.0),
        (0.0, 0.0, 768.0, 672.0),
        "the border picture fills the window it sized"
    );
    // The same window holding only the borderless screen is 160x144 at 4x, centred in the border's
    // shape: the drawer's scale rule does not know which of the two it is looking at.
    let (x, y, w, h) = display::display_rect(Picture::GAME_BOY, &options, 768.0, 696.0, 1.0);
    assert_eq!((w, h), (640.0, 576.0));
    assert_eq!((x, y), (64.0, 48.0));
}

#[test]
fn native_size_gba() {
    let scratch = Scratch::new("gba");
    let mut app = App::new(scratch.paths());
    let picture = open_built(&mut app, &scratch, "display.gba", &gba_rom());
    assert_eq!(picture, Picture::ADVANCE, "240x160");
    assert_eq!(
        display::native_size(tendy_core::SystemKind::Gba, false),
        picture
    );
    let options = DisplayOptions::DEFAULT;
    assert_eq!(display::window_size(picture, &options), (720, 504));
    assert_eq!(
        display::display_rect(picture, &options, 720.0, 504.0, 1.0),
        (0.0, 0.0, 720.0, 480.0)
    );
}

#[test]
fn native_size_nes() {
    let scratch = Scratch::new("nes");
    let mut app = App::new(scratch.paths());
    let picture = open_built(&mut app, &scratch, "display.nes", &nes_rom());
    assert_eq!(picture, Picture::NES, "256x240");
    assert_eq!(
        display::native_size(tendy_core::SystemKind::Nes, false),
        picture
    );
    let options = DisplayOptions::DEFAULT;
    assert_eq!(display::window_size(picture, &options), (768, 744));
    assert_eq!(
        display::display_rect(picture, &options, 768.0, 744.0, 1.0),
        (0.0, 0.0, 768.0, 720.0)
    );
    if let Some(real) = open_real(
        &mut app,
        nes_test_roms().as_deref(),
        "apu_test.nes",
        "TENDY_NES_TEST_ROMS",
    ) {
        assert_eq!(real, Picture::NES, "a real iNES cartridge");
    }
}

#[test]
fn native_size_snes() {
    let scratch = Scratch::new("snes");
    let mut app = App::new(scratch.paths());
    let picture = open_built(&mut app, &scratch, "display.sfc", &snes_rom());
    assert_eq!(picture, Picture::SUPER_NES, "256x224");
    assert_eq!(
        display::native_size(tendy_core::SystemKind::Snes, false),
        picture
    );
    let options = DisplayOptions::DEFAULT;
    assert_eq!(display::window_size(picture, &options), (768, 696));
    assert_eq!(
        display::display_rect(picture, &options, 768.0, 696.0, 1.0),
        (0.0, 0.0, 768.0, 672.0)
    );

    // U19: a real SNES program runs and the picture stays 256x224. There is no hi-res or overscan
    // image in the manifest; the hammer toggles INIDISP every scanline and still draws 224 of them.
    let Some(folder) = test_roms() else {
        println!("# skipped: TENDY_TEST_ROMS is not set (no real Super NES program run)");
        return;
    };
    let path = folder.join("snes-test-roms/inidisp_hammer_0f.sfc");
    if !path.is_file() {
        println!("# skipped: snes-test-roms/inidisp_hammer_0f.sfc not in TENDY_TEST_ROMS");
        return;
    }
    app.open(&path)
        .unwrap_or_else(|why| panic!("{}: {why}", path.display()));
    for frame in 0..10 {
        let picture = size(&mut app);
        assert_eq!(
            picture,
            Picture::SUPER_NES,
            "frame {frame} is 256x224, never 512 wide or 239 lines (U19)"
        );
        app.tick(1.0 / 60.0);
    }
}

#[test]
fn switch_resizes_window() {
    let scratch = Scratch::new("switch");
    let mut app = App::new(scratch.paths());
    let mut follower = Follower::new();
    let options = DisplayOptions::DEFAULT;
    let mut seen = Vec::new();
    for (name, bytes) in all_roms() {
        let picture = open_built(&mut app, &scratch, &name, &bytes);
        let want = display::window_size(picture, &options);
        assert_eq!(
            follower.follow(Some(picture), options),
            Some(want),
            "{name}: switching to this picture resizes"
        );
        assert_eq!(
            follower.follow(Some(picture), options),
            None,
            "{name}: a second frame of the same picture asks for nothing"
        );
        seen.push((name, picture, want));
    }
    assert_eq!(
        seen.iter().map(|(_, _, size)| *size).collect::<Vec<_>>(),
        vec![(480, 456), (720, 504), (768, 744), (768, 696)],
        "GB, GBA, NES and SNES each size the one window at 3x"
    );
    // A scale step resizes the same picture without any system change.
    let (_, picture, _) = *seen.last().expect("four systems ran");
    let bigger = DisplayOptions {
        scale: MAX_SCALE,
        ..options
    };
    assert_eq!(
        follower.follow(Some(picture), bigger),
        Some(display::window_size(picture, &bigger))
    );
    // The smallest scale is a window, never a zero.
    let smallest = DisplayOptions {
        scale: MIN_SCALE,
        ..options
    };
    let (w, h) = display::window_size(picture, &smallest);
    assert_eq!((w, h), (256, 248));
}

#[test]
fn integer_scale_fits() {
    let pictures = [
        Picture::GAME_BOY,
        Picture::SGB,
        Picture::ADVANCE,
        Picture::NES,
        Picture::SUPER_NES,
    ];
    for picture in pictures {
        for scale in MIN_SCALE..=MAX_SCALE {
            let options = DisplayOptions {
                scale,
                ..DisplayOptions::DEFAULT
            };
            let (win_w, win_h) = display::window_size(picture, &options);
            // The window the scale asks for is the window the scale fits: no letterbox and no
            // rounding drift at any size, shape or scale.
            assert_eq!(
                display::fit_scale(picture, &options, win_w as f32, win_h as f32, 1.0),
                scale,
                "{picture:?} at {scale}x"
            );
            assert_eq!(
                display::display_rect(picture, &options, win_w as f32, win_h as f32, 1.0),
                (
                    0.0,
                    0.0,
                    picture.width as f32 * scale as f32,
                    picture.height as f32 * scale as f32
                ),
                "{picture:?} at {scale}x"
            );
        }
        // A window half the size asks for one multiple, not a fraction.
        let options = DisplayOptions::DEFAULT;
        let (win_w, win_h) = display::window_size(picture, &options);
        let half = DisplayOptions {
            scale: 1,
            ..options
        };
        let (small_w, small_h) = display::window_size(picture, &half);
        assert!(small_w <= win_w && small_h <= win_h);
        assert_eq!(
            display::fit_scale(picture, &options, small_w as f32, small_h as f32, 1.0),
            1
        );
    }
    // The NES's 4:3 option is a 4:3 box at a whole multiple of the picture's height, and it stays
    // inside the window whatever the scale.
    let options = DisplayOptions {
        aspect: Aspect::Four3,
        ..DisplayOptions::DEFAULT
    };
    for scale in MIN_SCALE..=MAX_SCALE {
        let at = DisplayOptions { scale, ..options };
        let (win_w, win_h) = display::window_size(Picture::NES, &at);
        let (_, _, w, h) =
            display::display_rect(Picture::NES, &at, win_w as f32, win_h as f32, 1.0);
        assert!(
            w <= win_w as f32 && h <= win_h as f32 - 24.0,
            "{scale}x: {w}x{h} inside {win_w}x{win_h}"
        );
        let multiple = display::fit_scale(Picture::NES, &at, win_w as f32, win_h as f32, 1.0);
        assert_eq!(h, 240.0 * multiple as f32);
        assert!((w / h - 4.0 / 3.0).abs() < 1e-4, "{scale}x: {}", w / h);
    }
}

#[test]
fn the_display_options_are_settings_and_survive_a_restart() {
    let scratch = Scratch::new("options-file");
    let paths = scratch.paths();
    config::write_value(&paths, config::APP, "scale", "4").unwrap();
    config::write_value(&paths, config::APP, "fullscreen", "true").unwrap();
    config::write_value(&paths, config::APP, "status_bar", "false").unwrap();
    config::write_value(&paths, "gb", "integer_scale", "false").unwrap();

    let mut app = App::new(paths.clone());
    let options = display::options(&app);
    assert_eq!(options.scale, 4);
    assert!(options.fullscreen, "the [app] fullscreen flag");
    assert!(!options.integer_scale, "the GB section's own value");
    assert!(!options.bar, "the [app] status-bar flag");

    // The bar's height leaves the window when the bar is off.
    let with_bar = DisplayOptions { bar: true, ..options };
    let (_, without) = display::window_size(Picture::GAME_BOY, &options);
    let (_, with) = display::window_size(Picture::GAME_BOY, &with_bar);
    assert_eq!(with - without, BAR_H as i32);

    // A row writes the file; a new app reads the written value back.
    assert!(display::on_row(&mut app, 0, RowAct::Step(Direction::Right)));
    let restarted = App::new(paths.clone());
    assert_eq!(display::options(&restarted).scale, 5);

    // The NES's aspect key is that system's, not a process-wide value.
    let rom = scratch.rom("game.nes", &nes_rom());
    app.open(&rom).expect("the built NES cartridge opens");
    config::write_value(&paths, "nes", "aspect", "4:3").unwrap();
    assert_eq!(display::options(&app).aspect, Aspect::Four3);
    let next = App::new(paths);
    assert_eq!(
        display::options(&next).aspect,
        Aspect::Square,
        "no game runs, so no 4:3"
    );
}
