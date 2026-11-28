//! The drawer's rules, ported from the old Game Boy app's `tests/menu.rs` (#25).
//!
//! The shared drawer came from that app (§6.1); these are the rules the old file pinned with a
//! real cartridge open. The rest of that file either tests a row the parity table records as a gap
//! or is already pinned inside `menu.rs`; `baselines/dropped-tests-gb.txt` says which.

use std::fs;
use std::path::PathBuf;

use tendyemu::app::App;
use tendyemu::menu::{self, Entry, Level, RowAct};
use tendyemu::paths::Paths;
use tendyemu::settings::Direction;

/// A scratch config root, removed when the test ends.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Scratch {
        let root =
            std::env::temp_dir().join(format!("tendyemu-gb-menu-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("the scratch root");
        Scratch { root }
    }

    fn paths(&self) -> Paths {
        Paths::new(self.root.clone())
    }

    fn rom(&self, bytes: &[u8]) -> PathBuf {
        let path = self.root.join("drawer.gb");
        fs::write(&path, bytes).expect("the ROM");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn gb_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    rom[0x134..0x13C].copy_from_slice(b"DRAWER  ");
    rom[0x148] = 0x00;
    let mut sum = 0u8;
    for byte in &rom[0x134..=0x14C] {
        sum = sum.wrapping_sub(*byte).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA8].copy_from_slice(b"DRAWER!!");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(*byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// An app with a cartridge of `bytes` open.
fn app_with(scratch: &Scratch, bytes: &[u8]) -> App {
    let path = scratch.rom(bytes);
    let mut app = App::new(scratch.paths());
    app.open(&path).expect("the cartridge opens");
    app
}

/// The drawer as the app fills it, on one page.
fn drawer(app: &App, level: Level) -> menu::Menu {
    let mut menu = menu::Menu::new();
    menu.set_rows(menu::Rows::collect(app));
    menu.open();
    menu.set_level(level);
    menu
}

#[test]
fn top_level_is_the_gb_apps_rows() {
    let scratch = Scratch::new("top");
    let app = app_with(&scratch, &gb_rom());
    let labels: Vec<String> = drawer(&app, Level::Top)
        .entries()
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Item(item) => Some(item.row.label),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        [
            "Open ROM...",
            "Recent ROMs",
            "Game",
            "Video",
            "Audio",
            "Input",
            "Emulation",
            "Quit",
        ],
        "the old app's nine top-level rows, minus Link, which the parity table records as a gap"
    );
}

#[test]
fn every_row_has_a_description() {
    let scratch = Scratch::new("descriptions");
    let gb = app_with(&scratch, &gb_rom());
    let gba = app_with(&scratch, &gba_rom());
    for level in Level::ALL {
        assert!(!level.title().trim().is_empty(), "{level:?} has no title");
        for app in [&gb, &gba] {
            for entry in drawer(app, level).entries() {
                let Entry::Item(item) = entry else {
                    continue;
                };
                assert!(
                    !item.row.desc.trim().is_empty(),
                    "{level:?} row {:?} has no description",
                    item.row.label
                );
                assert!(
                    item.row.desc.len() <= 120,
                    "{level:?} row {:?} describes itself in {} characters",
                    item.row.label,
                    item.row.desc.len()
                );
            }
        }
    }
}

#[test]
fn a_click_on_a_choice_row_steps_it_forward_and_the_chevron_steps_it_back() {
    let scratch = Scratch::new("click");
    let app = app_with(&scratch, &gb_rom());
    let mut menu = drawer(&app, Level::Video);
    let entries = menu.entries();
    // The page opens on its group heading; the first row under it is Scale.
    let row = entries
        .iter()
        .position(|entry| matches!(entry, Entry::Item(_)))
        .expect("the Video page has rows");
    let Entry::Item(first) = &entries[row] else {
        unreachable!("just found a row");
    };
    assert_eq!(first.row.label, "Scale");
    assert_eq!(first.row.kind, menu::RowKind::Choice);
    let at = match first.act {
        menu::ItemAct::Module(at) => at,
        menu::ItemAct::Own(_) => panic!("Scale is a module row"),
    };

    // Enter is the reference: a step to the right.
    assert!(menu::enter(&mut menu));
    let by_enter = menu.take_request().expect("Enter leaves a request");
    assert_eq!(
        by_enter,
        menu::Request::Row(at, RowAct::Step(Direction::Right))
    );

    // A click past the chevron zone does what Enter does, and leaves the cursor on the row.
    assert!(menu::click(&mut menu, row, 100.0));
    assert_eq!(menu.cursor(), row);
    assert_eq!(
        menu.take_request(),
        Some(by_enter),
        "a click does what Enter does"
    );

    // The first 24 px of the row are the chevron: the same click steps it back.
    assert!(menu::click(&mut menu, row, menu::ROW_FILL_X + 1.0));
    assert_eq!(
        menu.take_request(),
        Some(menu::Request::Row(at, RowAct::Step(Direction::Left)))
    );
}

#[test]
fn a_heading_or_a_gap_takes_no_click() {
    let scratch = Scratch::new("hit");
    let app = app_with(&scratch, &gb_rom());
    let menu = drawer(&app, Level::Video);
    let entries = menu.entries();
    let layout = menu::layout(320.0, 600.0, &entries, 0);
    let at = |index: usize| {
        *layout
            .slots
            .iter()
            .find(|slot| slot.index == index)
            .unwrap_or_else(|| panic!("entry {index} is drawn"))
    };

    let gap = entries
        .iter()
        .position(|entry| matches!(entry, Entry::Gap))
        .expect("the Video page groups its rows");
    let slot = at(gap);
    assert!(!slot.item, "a gap is drawn but is not a row");
    assert_eq!(menu::hit(&layout, 40.0, slot.y + slot.h / 2.0), None);

    let heading = entries
        .iter()
        .position(|entry| matches!(entry, Entry::Heading(_)))
        .expect("every group starts with its heading");
    let slot = at(heading);
    assert!(!slot.item, "a heading is drawn but is not a row");
    assert_eq!(menu::hit(&layout, 40.0, slot.y + slot.h / 2.0), None);

    // The row under them is a row again.
    let row = at(heading + 1);
    assert!(row.item, "the first row under the heading can be clicked");
    assert_eq!(menu::hit(&layout, 40.0, row.y + 1.0), Some(heading + 1));
}
