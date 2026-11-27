//! The four visible palettes, the 512 system palettes, `PAL01`/`PAL23`/`PAL03`/`PAL12`,
//! `PAL_SET`, `PAL_TRN`, `PAL_PRI`, their state and the two-line render hook.
//!
//! Every expectation is Pan Docs' *Palette Commands* text and nothing else: a colour is a 15-bit
//! little-endian RGB555 word; the four palette commands carry colour 0 of **both** palettes in
//! bytes 1-2 ("for SGB palette 0, color 0-3, and for SGB palette 1, color 1-3 (without separate
//! color 0)"); `PAL_SET` takes four system-palette IDs 0-511 and a flags byte; `PAL_TRN` uploads
//! 512 × 4 words from 4 KiB of VRAM.
//!
//! The table at the bottom is the leg's whole gate: each row performs a sequence of commands and
//! then compares **all four palettes of four colours**, the `PAL_PRI` byte and the live attribute
//! map. The expected palettes are hand-written words, not this file's own shifting arithmetic, so a
//! byte layout that is consistently wrong between sender and reader still fails.

mod common;

use tendy_gb::sgb::{DEFAULT_RAMP, Packet, Sgb, SgbPalettes};
use tendy_gb::state::{StateReader, StateWriter};

// ---------------------------------------------------------------- the packets, byte for byte

/// `PAL01`: header `$01` (`$00 << 3 | 1`).
const PAL01: Packet = Packet {
    bytes: [
        0x01, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33,
        0x00,
    ],
};

/// `PAL23`: header `$09` (`$01 << 3 | 1`).
const PAL23: Packet = Packet {
    bytes: [
        0x09, 0xBC, 0x0A, 0x44, 0x44, 0x55, 0x55, 0x66, 0x66, 0x77, 0x77, 0x88, 0x88, 0x99, 0x99,
        0x00,
    ],
};

/// `PAL03`: header `$11` (`$02 << 3 | 1`).
const PAL03: Packet = Packet {
    bytes: [
        0x11, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x66, 0x66, 0x77, 0x77,
        0x00,
    ],
};

/// `PAL12`: header `$19` (`$03 << 3 | 1`).
const PAL12: Packet = Packet {
    bytes: [
        0x19, 0x34, 0x12, 0x56, 0x56, 0x78, 0x78, 0x9A, 0x9A, 0xBC, 0xBC, 0xDE, 0xDE, 0xF0, 0xF0,
        0x00,
    ],
};

/// `PAL_TRN`: header `$59` (`$0B << 3 | 1`). The data is in VRAM, not the packet.
const PAL_TRN: Packet = Packet {
    bytes: [0x59, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
};

/// `ATTR_TRN`: header `$AD` (`$15 << 3 | 1`), used here only to give `PAL_SET` a file to pick.
const ATTR_TRN: Packet = Packet {
    bytes: [0xAD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
};

// ---------------------------------------------------------------- expected palettes

/// The clean-room default, four times: our ramp, not the SGB BIOS's colours.
const D: [u16; 4] = DEFAULT_RAMP;

/// `PAL01`'s palette 0 and palette 1 — colour 0 (`$001F`) is the packet's bytes 1-2 for both.
const P0: [u16; 4] = [0x001F, 0x03E0, 0x7C00, 0x7FFF];
const P1: [u16; 4] = [0x001F, 0x1111, 0x2222, 0x3333];
/// `PAL23`'s palettes, sharing `$0ABC`.
const P2: [u16; 4] = [0x0ABC, 0x4444, 0x5555, 0x6666];
const P3: [u16; 4] = [0x0ABC, 0x7777, 0x8888, 0x9999];
/// `PAL03`'s palettes 0 and 3, sharing `$1111`.
const P03A: [u16; 4] = [0x1111, 0x2222, 0x3333, 0x4444];
const P03B: [u16; 4] = [0x1111, 0x5555, 0x6666, 0x7777];
/// `PAL12`'s palettes 1 and 2, sharing `$1234`.
const P12A: [u16; 4] = [0x1234, 0x5656, 0x7878, 0x9A9A];
const P12B: [u16; 4] = [0x1234, 0xBCBC, 0xDEDE, 0xF0F0];
/// System palettes out of the ramp block: palette `n` is bytes `8n..8n+8`, little-endian.
const S0: [u16; 4] = [0x0100, 0x0302, 0x0504, 0x0706];
const S1: [u16; 4] = [0x0908, 0x0B0A, 0x0D0C, 0x0F0E];
const S2: [u16; 4] = [0x1110, 0x1312, 0x1514, 0x1716];
const S3: [u16; 4] = [0x1918, 0x1B1A, 0x1D1C, 0x1F1E];
const S5: [u16; 4] = [0x2928, 0x2B2A, 0x2D2C, 0x2F2E];
const S511: [u16; 4] = [0xF9F8, 0xFBFA, 0xFDFC, 0xFFFE];

// ---------------------------------------------------------------- helpers

/// The 4 KiB `$8000-$8FFF` block: byte `i` is `i`. Palette `n`, colour `c` is therefore the word
/// `(8n + 2c) | ((8n + 2c + 1) << 8)` — the values `S0`-`S511` spell out.
fn ramp_block() -> Vec<u8> {
    (0..4096u32).map(|i| i as u8).collect()
}

/// File `n` of the ramp block: its 90 bytes, `90n .. 90n + 90`.
fn ramp_file(n: usize) -> [u8; 90] {
    let mut file = [0u8; 90];
    for (k, byte) in file.iter_mut().enumerate() {
        *byte = (n * 90 + k) as u8;
    }
    file
}

// ---------------------------------------------------------------- the store itself

#[test]
fn a_new_store_is_our_ramp_four_times() {
    let palettes = SgbPalettes::new();
    assert_eq!(palettes.visible(), &[DEFAULT_RAMP; 4]);
    assert_eq!(palettes.system()[0], DEFAULT_RAMP);
    assert_eq!(
        palettes.system()[511],
        DEFAULT_RAMP,
        "all 512 start as our ramp: no BIOS palette is ours to ship"
    );
    assert_eq!(palettes.priority(), 0);
}

#[test]
fn a_pair_shares_colour_zero_and_keeps_the_other_three_per_palette() {
    let mut palettes = SgbPalettes::new();
    palettes.set_pair(0, 1, &PAL01);
    assert_eq!(palettes.visible()[0], P0);
    assert_eq!(palettes.visible()[1], P1);
    assert_eq!(
        palettes.visible()[0][0],
        palettes.visible()[1][0],
        "bytes 1-2 are colour 0 of both palettes"
    );

    palettes.set_pair(2, 3, &PAL23);
    assert_eq!(palettes.visible()[2], P2);
    assert_eq!(palettes.visible()[3], P3);
    assert_eq!(palettes.visible()[2][0], palettes.visible()[3][0]);
}

#[test]
fn a_palette_lookup_masks_both_indices() {
    let mut palettes = SgbPalettes::new();
    palettes.set_pair(2, 3, &PAL23);
    assert_eq!(palettes.colour(3, 2), palettes.visible()[3][2]);
    assert_eq!(
        palettes.colour(7, 6),
        palettes.visible()[3][2],
        "7 and 6 wrap to 3 and 2: a corrupt attribute cannot panic"
    );
    assert_eq!(palettes.colour(0, 0), palettes.visible()[0][0]);
}

#[test]
fn sgb_off_answers_no_colour_and_sgb_on_answers_palette_zero() {
    assert_eq!(
        Sgb::off().colour(0, 0, 0),
        None,
        "an SGB-off machine keeps its own shade path"
    );

    let mut sgb = Sgb::on();
    sgb.apply(&PAL01, &[]);
    assert_eq!(
        sgb.colour(5, 7, 2),
        Some(0x7C00),
        "a blank attribute map puts every cell on palette 0"
    );
}

#[test]
fn pal_trn_reads_the_whole_block_little_endian() {
    let mut palettes = SgbPalettes::new();
    palettes.pal_trn(&ramp_block());
    assert_eq!(palettes.system()[0], S0);
    assert_eq!(palettes.system()[1][0], 0x0908);
    assert_eq!(palettes.system()[511][3], 0xFFFE);
}

#[test]
#[should_panic(expected = "PAL_TRN reads 4 096 bytes")]
fn pal_trn_refuses_a_short_block() {
    SgbPalettes::new().pal_trn(&[0u8; 4095]);
}

#[test]
fn the_palette_state_is_4_129_bytes_and_round_trips() {
    let mut palettes = SgbPalettes::new();
    palettes.set_pair(0, 1, &PAL01);
    palettes.set_pair(2, 3, &PAL23);
    palettes.pal_trn(&ramp_block());
    palettes.set_priority(2);

    let mut w = StateWriter::new();
    palettes.save_state(&mut w);
    assert_eq!(
        w.len(),
        32 + 4096 + 1,
        "4 x 4 + 512 x 4 words, then the byte"
    );
    let bytes = w.finish();

    // Decode into a *default* store, so a field the encoder skipped shows up as our ramp and a
    // priority of zero.
    let mut decoded = SgbPalettes::new();
    decoded
        .load_state(&mut StateReader::new(&bytes))
        .expect("state");
    assert_eq!(decoded.visible(), palettes.visible());
    assert!(
        decoded.system() == palettes.system(),
        "the 512 system palettes did not come back"
    );
    assert_eq!(decoded.priority(), 2);

    // A cut-down stream is an error, not a panic and not a half-loaded store.
    assert!(
        SgbPalettes::new()
            .load_state(&mut StateReader::new(&bytes[..100]))
            .is_err(),
        "100 bytes cannot hold the four visible palettes and the box"
    );
}

// ---------------------------------------------------------------- the render hook

#[test]
fn the_render_hook_turns_a_shade_into_an_sgb_colour() {
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    // Tile 0 and the map are already zero, so every pixel is colour index 0.
    m.bus.ppu.bgp = 0xE4; // index 0 -> shade 0
    m.run_frame();

    m.bus.ppu.sgb.apply(&PAL01, &[]);
    m.run_frame();
    assert_eq!(m.colours()[0], 0x001F, "palette 0 colour 0, from PAL01");
    assert_eq!(m.frame()[0], 0, "the shade buffer keeps its meaning");

    m.bus.ppu.bgp = 0xE7; // index 0 -> shade 3
    m.run_frame();
    assert_eq!(m.frame()[0], 3, "the DMG shade path still runs");
    assert_eq!(
        m.colours()[0],
        0x001F,
        "the colour is the palette's, not the shade's"
    );
}

#[test]
fn an_sgb_off_machine_draws_main_frame() {
    // `tests/sgb.rs`'s picture, byte for byte: adding the hook must not move it.
    const MAIN_RUNNING: u64 = 0xf146_549e_efd3_9f45;

    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::off();
    for (i, byte) in m.bus.ppu.vram.iter_mut().enumerate().take(16) {
        *byte = (i as u8).wrapping_mul(17);
    }
    for i in 0..32 * 32 {
        m.bus.ppu.vram[0x1800 + i] = [0u8, 1, 2, 3][(i / 5) % 4];
    }
    m.bus.ppu.bgp = 0xE4;
    for _ in 0..3 {
        m.run_frame();
    }
    assert_eq!(
        m.frame_checksum(),
        MAIN_RUNNING,
        "an SGB-off machine must draw main's frames: {MAIN_RUNNING:016x}"
    );
}

// ---------------------------------------------------------------- the whole store, twelve rows

/// One step a row asks an [`Sgb`] to perform.
enum Step {
    /// `Sgb::apply` with the packet and the VRAM block a transfer would read.
    Apply(Packet, Vec<u8>),
    /// `SgbPalettes` `save_state` -> `load_state` into a fresh store, which must come back equal.
    RoundTrip,
}

/// One row of the table: the steps, then all four palettes of four colours, the `PAL_PRI` byte and
/// the live 90-byte attribute map they must leave behind.
struct Row {
    name: &'static str,
    steps: Vec<Step>,
    visible: [[u16; 4]; 4],
    priority: u8,
    live: [u8; 90],
}

fn rows() -> Vec<Row> {
    let no_map = [0u8; 90];
    vec![
        // 1. The store as built: nothing sent.
        Row {
            name: "nothing: our ramp four times",
            steps: vec![],
            visible: [D; 4],
            priority: 0,
            live: no_map,
        },
        // 2. PAL01: palettes 0 and 1.
        Row {
            name: "PAL01",
            steps: vec![Step::Apply(PAL01, vec![])],
            visible: [P0, P1, D, D],
            priority: 0,
            live: no_map,
        },
        // 3. PAL23: palettes 2 and 3, and 0/1 keep the default.
        Row {
            name: "PAL23",
            steps: vec![Step::Apply(PAL23, vec![])],
            visible: [D, D, P2, P3],
            priority: 0,
            live: no_map,
        },
        // 4. Both pairs: all four set by two commands.
        Row {
            name: "PAL01 then PAL23",
            steps: vec![Step::Apply(PAL01, vec![]), Step::Apply(PAL23, vec![])],
            visible: [P0, P1, P2, P3],
            priority: 0,
            live: no_map,
        },
        // 5. PAL03: the odd pairing, 0 and 3, sharing colour 0.
        Row {
            name: "PAL03: palettes 0 and 3 share colour 0",
            steps: vec![Step::Apply(PAL03, vec![])],
            visible: [P03A, D, D, P03B],
            priority: 0,
            live: no_map,
        },
        // 6. PAL12 after PAL01: 1 and 2 take the new pair, 0 and 3 keep theirs.
        Row {
            name: "PAL01 then PAL12: palettes 1 and 2 move, 0 and 3 stay",
            steps: vec![Step::Apply(PAL01, vec![]), Step::Apply(PAL12, vec![])],
            visible: [P0, P12A, P12B, D],
            priority: 0,
            live: no_map,
        },
        // 7. PAL_TRN then PAL_SET: four system IDs, in the order the packet gives them.
        Row {
            name: "PAL_TRN then PAL_SET 3,2,1,0",
            steps: vec![
                Step::Apply(PAL_TRN, ramp_block()),
                Step::Apply(pal_set_with(&[3, 2, 1, 0], 0x00), vec![]),
            ],
            visible: [S3, S2, S1, S0],
            priority: 0,
            live: no_map,
        },
        // 8. PAL_SET's ATF bit: the four palettes arrive **and** file 2 becomes the live map.
        Row {
            name: "PAL_SET's ATF bit selects file 2 as well",
            steps: vec![
                Step::Apply(ATTR_TRN, ramp_block()),
                Step::Apply(PAL_TRN, ramp_block()),
                Step::Apply(pal_set_with(&[3, 2, 1, 0], 0x82), vec![]),
            ],
            visible: [S3, S2, S1, S0],
            priority: 0,
            live: ramp_file(2),
        },
        // 9. IDs above 511 wrap to their low nine bits instead of reading out of the box.
        Row {
            name: "PAL_SET with IDs 512, 65535, 1 and 5",
            steps: vec![
                Step::Apply(PAL_TRN, ramp_block()),
                Step::Apply(pal_set_with(&[0x0200, 0xFFFF, 1, 5], 0x00), vec![]),
            ],
            visible: [S0, S511, S1, S5],
            priority: 0,
            live: no_map,
        },
        // 10. PAL_PRI: the byte, and nothing else.
        Row {
            name: "PAL_PRI 3",
            steps: vec![Step::Apply(
                Packet {
                    bytes: [0xC9, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                },
                vec![],
            )],
            visible: [D; 4],
            priority: 3,
            live: no_map,
        },
        // 11. All of it, through the state encoder and back.
        Row {
            name: "save and load a full store",
            steps: vec![
                Step::Apply(PAL01, vec![]),
                Step::Apply(PAL23, vec![]),
                Step::Apply(PAL_TRN, ramp_block()),
                Step::Apply(
                    Packet {
                        bytes: [0xC9, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                    },
                    vec![],
                ),
                Step::RoundTrip,
            ],
            visible: [P0, P1, P2, P3],
            priority: 1,
            live: no_map,
        },
        // 12. PAL23 then PAL03: 2 keeps its pair, 0 and 3 take the new one.
        Row {
            name: "PAL23 then PAL03",
            steps: vec![Step::Apply(PAL23, vec![]), Step::Apply(PAL03, vec![])],
            visible: [P03A, D, P2, P03B],
            priority: 0,
            live: no_map,
        },
    ]
}

/// A `PAL_SET` packet: the header, four little-endian IDs, the flags byte.
fn pal_set_with(ids: &[u16; 4], flags: u8) -> Packet {
    let mut bytes = [0u8; 16];
    bytes[0] = 0x51;
    for (slot, id) in ids.iter().enumerate() {
        let [low, high] = id.to_le_bytes();
        bytes[1 + slot * 2] = low;
        bytes[2 + slot * 2] = high;
    }
    bytes[9] = flags;
    Packet { bytes }
}

#[test]
fn the_palette_table() {
    let rows = rows();
    assert!(rows.len() >= 10, "the spec asks for at least ten rows");

    let mut mismatches = Vec::new();
    for row in &rows {
        let mut sgb = Sgb::on();
        for step in &row.steps {
            match step {
                Step::Apply(packet, vram) => sgb.apply(packet, vram),
                Step::RoundTrip => {
                    let mut w = StateWriter::new();
                    sgb.palettes().save_state(&mut w);
                    let bytes = w.finish();
                    let mut decoded = SgbPalettes::new();
                    decoded
                        .load_state(&mut StateReader::new(&bytes))
                        .expect("palette state");
                    if &decoded != sgb.palettes() {
                        mismatches.push(format!(
                            "{}: the round trip decoded another store",
                            row.name
                        ));
                    }
                }
            }
        }

        if sgb.palettes().visible() != &row.visible {
            mismatches.push(format!(
                "{}: visible {:04X?}, expected {:04X?}",
                row.name,
                sgb.palettes().visible(),
                row.visible
            ));
        }
        if sgb.palettes().priority() != row.priority {
            mismatches.push(format!(
                "{}: priority {:#04X}, expected {:#04X}",
                row.name,
                sgb.palettes().priority(),
                row.priority
            ));
        }
        if sgb.attr().live() != &row.live {
            mismatches.push(format!("{}: the live attribute map moved", row.name));
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} rows disagree:\n{}",
        mismatches.len(),
        rows.len(),
        mismatches.join("\n")
    );
}
