//! The cartridge: copier headers, the header score, the checksum, refusals and padding
//! (DESIGN §8, #10).

mod common;

use tendy_snes::Machine;
use tendy_snes::addr::Addr;
use tendy_snes::cart::{Board, Cartridge};

/// §8.2's sum over a whole image, with the four checksum bytes read as `FF FF 00 00`: "all bytes
/// in ROM added together; assume [FFDC-F]=FF,FF,0,0" (fullsnes `$FFDE`).
fn image_sum(rom: &[u8], at: usize) -> u16 {
    let mut sum: u32 = 0;
    for (i, b) in rom.iter().enumerate() {
        sum += match i.wrapping_sub(at) {
            0x1C | 0x1D => 0xFF,
            0x1E | 0x1F => 0x00,
            _ => u32::from(*b),
        };
    }
    sum as u16
}

/// The refusal line, asserting the image was refused at all (`Cartridge` has no `Debug`, so
/// `expect_err` is not available and the message is what these tests are after).
fn refusal(rom: Vec<u8>) -> String {
    match Cartridge::new(rom) {
        Ok(_) => panic!("the image must be refused"),
        Err(message) => message.to_string(),
    }
}

/// Writes `$FFDC`/`$FFDE` back so an edited image still carries its own checksum (§8.2).
fn reseal(rom: &mut [u8], at: usize) {
    let sum = image_sum(rom, at);
    rom[at + 0x1E..at + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[at + 0x1C..at + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
}

#[test]
fn copier_header_is_detected_by_size() {
    // fullsnes "SNES Cartridge ROM-Image Headers and File Extensions": "IF (filesize AND 3FFh)=200h
    // THEN HeaderPresent=True (Headerless cartridges are always sized N*1024 bytes, Carts with
    // header are N*1024+512 bytes)".
    let raw = common::synth_lorom(0x8000, &[0xEA]);
    assert_eq!(raw.len() % 1024, 0);
    let mut plain = Cartridge::new(raw.clone()).expect("a raw image loads");
    assert!(!plain.header.has_copier_header);
    assert_eq!(plain.rom.len(), raw.len());
    assert_eq!(plain.header.header_offset, 0x7FC0);

    let mut wrapped = vec![0xAAu8; 512];
    wrapped.extend_from_slice(&raw);
    assert_eq!(wrapped.len() % 1024, 512, "the size test's own premise");
    let mut with = Cartridge::new(wrapped).expect("a copier-wrapped image loads");

    assert!(with.header.has_copier_header);
    assert_eq!(
        with.rom, plain.rom,
        "exactly the 512-byte prefix is stripped, so every later read is unshifted"
    );
    assert_eq!(with.rom.len(), raw.len());
    assert_eq!(with.header.header_offset, 0x7FC0);
    assert_eq!(with.header.title, plain.header.title);
    assert_eq!(
        with.read(Addr::new(0x00, 0x8000)),
        plain.read(Addr::new(0x00, 0x8000))
    );
}

#[test]
fn header_found_for_each_board() {
    // §8.1: the header sits at CPU `$00:FFC0`, which is image offset `$7FC0` for LoROM, `$FFC0`
    // for HiROM and `$40FFC0` for ExHiROM (fullsnes "SNES Cartridge ROM Header").
    let lorom = Cartridge::new(common::synth_lorom(0x8000, &[0xEA])).unwrap();
    assert_eq!(lorom.board, Board::LoRom);
    assert_eq!(lorom.header.header_offset, 0x7FC0);
    assert_eq!(lorom.header.map_mode, 0);

    let hirom = Cartridge::new(common::synth_hirom(0x1_0000, &[0xEA])).unwrap();
    assert_eq!(hirom.board, Board::HiRom);
    assert_eq!(hirom.header.header_offset, 0xFFC0);
    assert_eq!(hirom.header.map_mode, 1);

    let exhirom =
        Cartridge::new(common::synth_cart(Board::ExHiRom, 0x80_0000, 0, 0, &[0xEA])).unwrap();
    assert_eq!(exhirom.board, Board::ExHiRom);
    assert_eq!(exhirom.header.header_offset, 0x40FFC0);
    assert_eq!(exhirom.header.map_mode, 5);
}

#[test]
fn image_with_no_findable_header_is_refused() {
    // §8.1's score must refuse an image whose best candidate is under 4 (§8.4's "the header could
    // not be found"), not accept the best of two bad ones. An erased image (all `$FF`) scores 0
    // at every candidate: no map nibble, no checksum, an impossible size code and an unnamed
    // chipset byte.
    let rom = vec![0xFFu8; 0x1_0000];
    let err = refusal(rom);
    assert_eq!(
        err,
        "refused: no cartridge header found (best score 0 of 13)"
    );
}

#[test]
fn refuses_a_zero_filled_image() {
    // §8.1: a zero-filled image satisfies `$FFD5`'s map nibble (4), the printable-title point
    // (`$FFD5`-adjacent NULs count as padding) and the named-chipset point (`$00` is ROM), so it
    // scores 6 — well above the threshold of 4 — and used to run as LoROM, a blank screen with a
    // board name invented for it. An image that is one repeated byte carries no cartridge,
    // whatever the score says, and is refused by the same `no cartridge header` line.
    let err = refusal(vec![0u8; 0x8000]);
    assert_eq!(
        err,
        "refused: no cartridge header found (best score 6 of 13)"
    );

    // The erased image takes the same line for the same reason, without the filler rule.
    let err = refusal(vec![0xFFu8; 0x8000]);
    assert_eq!(
        err,
        "refused: no cartridge header found (best score 0 of 13)"
    );
}

#[test]
fn a_well_formed_cartridge_is_not_refused() {
    // The other half of every refusal test: the loader's own image loads, keeps its board, and
    // asks for no notice and no refusal (§8.4).
    let cart = Cartridge::new(common::synth_lorom(0x8000, &[0xEA]))
        .expect("a well-formed LoROM image is not refused");
    assert_eq!(cart.board, Board::LoRom);
    assert_eq!(cart.header.title, "SNES-TEST");
    assert!(cart.header.checksum_ok);
    assert_eq!(cart.notice(), None, "a clean image says nothing");

    let hirom = Cartridge::new(common::synth_hirom(0x1_0000, &[0xEA]))
        .expect("a well-formed HiROM image is not refused");
    assert_eq!(hirom.board, Board::HiRom);
    assert_eq!(hirom.notice(), None);
}

#[test]
fn checksum_over_a_known_image() {
    let rom = common::synth_lorom(0x8000, &[0xEA]);
    let cart = Cartridge::new(rom.clone()).unwrap();
    let at = cart.header.header_offset;
    let expected = image_sum(&rom, at);
    assert_eq!(
        cart.header.checksum, expected,
        "§8.2: the sum of every byte, with $FFDC-$FFDF read as FF FF 00 00"
    );
    assert_eq!(cart.header.checksum_complement, expected ^ 0xFFFF);
    assert!(
        cart.header.checksum_ok,
        "the synthesized image carries its own correct checksum"
    );
}

#[test]
fn bad_checksum_still_loads() {
    // §8.2: "A bad checksum is not a refusal" — homebrew images usually have several header errors
    // and the cartridge still runs.
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    rom[0x100] ^= 0x5A; // a byte of program area, far from the header on purpose
    let cart = Cartridge::new(rom).expect("a bad checksum is not a refusal");
    assert!(!cart.header.checksum_ok);
    assert_eq!(cart.board, Board::LoRom);
    assert_eq!(cart.header.title, "SNES-TEST");
    assert_eq!(
        cart.notice(),
        Some("header checksum bad"),
        "§10.3: the bar reports it; §8.2: it is not a refusal"
    );
}

#[test]
fn refuses_a_coprocessor_with_its_name() {
    // fullsnes "Chipset (FFD6h)": the high nibble names the family and `x3h`..`x6h` are
    // co-processor boards; only `$00`, `$01` and `$02` are shapes this build runs (§8.4).
    let cases: &[(usize, u8, u8, &str)] = &[
        (0x8000, 0x03, 0x00, "DSP-1"),
        (0x8000, 0x13, 0x00, "SuperFX/GSU-1"),
        (0x20_0000, 0x15, 0x00, "SuperFX/GSU-2"),
        (0x8000, 0x25, 0x00, "OBC1"),
        (0x8000, 0x35, 0x00, "SA-1"),
        (0x8000, 0x45, 0x00, "S-DD1"),
        (0x8000, 0xE3, 0x00, "SGB"),
        (0x8000, 0xE5, 0x00, "Satellaview"),
        (0x8000, 0xF5, 0x00, "SPC7110"),
        (0x8000, 0xF9, 0x00, "SPC7110+RTC"),
        (0x8000, 0xF3, 0x10, "CX4"),
        (0x8000, 0xF6, 0x01, "ST010"),
    ];
    for &(size, byte, subtype, name) in cases {
        let mut rom = common::synth_cart(Board::LoRom, size, byte, 0, &[0xEA]);
        rom[0x7FC0 - 1] = subtype; // $FFBF, the chipset sub-type
        let err = refusal(rom);
        assert_eq!(
            err,
            format!("refused: {name}, not in v1 ($FFD6 = ${byte:02X})"),
            "§8.4's refusal is one line, with the chipset's name and its byte, worded in `cart::refusal`"
        );
    }
}

#[test]
fn every_refusal_class_starts_with_the_sweep_tag() {
    // §12.4's sweep classifies a failed row as `refused:` on this one tag and calls everything
    // else a crash — the tag is that agreement, not a second list of names. `refusal.rs` is the
    // only place a sentence is built, so a class added there cannot miss it.
    use tendy_snes::cart::refusal::REFUSED_TAG;
    let sentences = [
        refusal(vec![0u8; 0x8000]),
        refusal(vec![0u8; 0x1000]),
        refusal(common::synth_cart(Board::LoRom, 0x8000, 0x13, 0, &[0xEA])),
    ];
    for sentence in &sentences {
        assert!(
            sentence.starts_with(REFUSED_TAG),
            "{sentence:?} must start with {REFUSED_TAG:?}"
        );
    }
    assert_eq!(
        tendy_snes::cart::Refusal::TooSmall { bytes: 1 }.class(),
        "too small"
    );
    assert_eq!(
        tendy_snes::cart::Refusal::NoCartridgeInArchive {
            path: "x.zip".to_string()
        }
        .class(),
        "no cartridge in archive"
    );
    assert_eq!(
        tendy_snes::cart::Refusal::Unreadable {
            path: "x.sfc".to_string(),
            why: "gone".to_string(),
        }
        .class(),
        "unreadable file"
    );
}

#[test]
fn refuses_a_short_image() {
    // §8.4: "the smallest licensed board is 32 KiB, and a smaller image cannot hold vectors at
    // $FFE0-$FFFF in any map".
    let small = vec![0u8; 4096];
    let err = refusal(small.clone());
    assert_eq!(err, "refused: ROM is 4096 bytes, need at least 32768");

    // The same image behind a 512-byte copier header: the header is stripped first, so the
    // refusal still reports the image's own size (§8.1).
    let mut wrapped = vec![0xAAu8; 512];
    wrapped.extend_from_slice(&small);
    assert_eq!(wrapped.len() % 1024, 512);
    let err = refusal(wrapped);
    assert_eq!(err, "refused: ROM is 4096 bytes, need at least 32768");
}

#[test]
fn pal_cartridge_loads_with_a_notice() {
    // fullsnes "Country (FFD9h)": $02-$0C and $11 are PAL. §8.4 makes that a notice, not a
    // refusal: v1 runs it at NTSC timing and the bar says "PAL cartridge, NTSC timing" (§10.3).
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    rom[0x7FC0 + 0x19] = 0x02; // Europe
    let cart = Cartridge::new(rom).expect("a PAL image is playable, not refused");
    assert_eq!(cart.header.country, 0x02);
    assert!(cart.header.is_pal());
    assert_eq!(cart.notice(), Some("PAL cartridge, NTSC timing"));

    // A USA cartridge is silent.
    let ntsc = Cartridge::new(common::synth_lorom(0x8000, &[0xEA])).unwrap();
    assert_eq!(ntsc.header.country, 0x01);
    assert!(!ntsc.header.is_pal());
    assert_eq!(ntsc.notice(), None);
}

#[test]
fn five_mib_image_is_padded_and_runs() {
    // §8.2: a non-power-of-two image is completed by repeating its last power-of-two-sized chunk.
    // For 5 MiB (40 Mbit) that is 32 Mbit plus three more copies of the last 8 Mbit, which is how
    // fullsnes says the hardware rounds a 10 Mbit cart up to 16 Mbit ("8Mbit + 4x2Mbit").
    const FIVE_MIB: usize = 5 * 1024 * 1024;
    const CHUNK: usize = 1024 * 1024;
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    rom.resize(FIVE_MIB, 0xEA);
    rom[0x7FC0 + 0x17] = 13; // $FFD7: 8 MiB, the size the padding rounds this image up to
    let tail = rom[FIVE_MIB - CHUNK..].to_vec();

    let cart = Cartridge::new(rom).expect("a 40 Mbit image loads");
    assert_eq!(cart.rom.len(), 8 * 1024 * 1024);
    assert_eq!(cart.rom_mask, cart.rom.len() - 1);
    assert_eq!(cart.board, Board::LoRom);
    for (i, part) in cart.rom[FIVE_MIB..].chunks(CHUNK).enumerate() {
        assert_eq!(
            part,
            &tail[..part.len()],
            "padding block {i} repeats the last {CHUNK}-byte chunk (fullsnes \"ROM Size / Checksum Notes\")"
        );
    }
    // The pad is what makes `rom_mask` a mask: banks $C0-$FF address the second half of the padded
    // image and $80-$BF the first (§8.3).
    let mut cart = cart;
    assert_eq!(cart.read(Addr::new(0x80, 0x8000)), cart.rom[0]);
    assert_eq!(cart.read(Addr::new(0xC0, 0x8000)), cart.rom[0x20_0000]);

    // And it runs: one frame of the synthesized program without a panic.
    let mut machine = Machine::new(cart);
    machine.run_frame();
    assert_eq!(machine.frames, 1);
}

#[test]
fn title_is_trimmed() {
    // §8.2: `$FFC0-$FFD4`, 21 bytes, "trailing spaces and NULs trimmed".
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    let at = 0x7FC0;
    rom[at..at + 0x15].fill(b' ');
    rom[at..at + 9].copy_from_slice(b"HOME BREW");
    rom[at + 12..at + 0x15].fill(0);
    let cart = Cartridge::new(rom).expect("the header is still found");
    assert_eq!(cart.header.title, "HOME BREW");
    assert_eq!(cart.header.title.len(), 9, "no padding survives the trim");
}

#[test]
fn header_score_on_awkward_images() {
    // §8.1's score is v1's own invention and §8.6 leaves its thresholds to the sweep, so the
    // numbers it gives on the awkward images are pinned here: 13 is the maximum
    // (4 + 1 + 4 + 2 + 1 + 1).
    use tendy_snes::cart::header::score;

    // A complete header: every part agrees. A 64 KiB image, so both the LoROM and the HiROM
    // candidate offsets are inside it and can be compared.
    let good = common::synth_lorom(0x1_0000, &[0xEA]);
    assert_eq!(score(&good, 0x7FC0, Board::LoRom), 13);
    // ... and the HiROM candidate on the same image is empty space.
    assert_eq!(score(&good, 0xFFC0, Board::HiRom), 0);

    // Homebrew: no checksum at all, title padded with spaces. fullsnes "Cartridge Header
    // Variants" says such images are usual, so the score must stay well above the threshold of 4.
    let mut homebrew = good.clone();
    homebrew[0x7FC0 + 0x1C..0x7FC0 + 0x20].fill(0);
    assert_eq!(
        score(&homebrew, 0x7FC0, Board::LoRom),
        9,
        "4 map + 1 speed + 2 size + 1 title + 1 chipset, with the checksum's 4 lost"
    );
    assert!(Cartridge::new(homebrew).is_ok());

    // A header whose map byte claims HiROM on a LoROM image: the checksum's 4 outvotes the map
    // nibble's 4 at the true offset, so the real header still wins.
    let mut lying = good.clone();
    lying[0x7FC0 + 0x15] = 0x21; // $FFD5: fast, map mode 1 (HiROM)
    reseal(&mut lying, 0x7FC0);
    assert_eq!(
        score(&lying, 0x7FC0, Board::LoRom),
        9,
        "the map nibble's 4 is lost, the checksum's 4 is kept"
    );
    assert_eq!(score(&lying, 0xFFC0, Board::HiRom), 0);
    assert_eq!(
        Cartridge::new(lying).expect("the image loads").board,
        Board::LoRom,
        "the score, not the map byte, picks the board"
    );

    // An erased image: nothing scores, and nothing is accepted.
    let erased = vec![0xFFu8; 0x1_0000];
    assert_eq!(score(&erased, 0x7FC0, Board::LoRom), 0);
    assert_eq!(score(&erased, 0xFFC0, Board::HiRom), 0);
    assert_eq!(
        score(&erased, 0x40FFC0, Board::ExHiRom),
        i32::MIN,
        "past the image"
    );

    // The ExHiROM candidate only exists on an image that reaches $40FFC0.
    let ex = common::synth_cart(Board::ExHiRom, 0x80_0000, 0, 0, &[0xEA]);
    assert_eq!(score(&ex, 0x40FFC0, Board::ExHiRom), 13);
    assert_eq!(score(&ex, 0x7FC0, Board::LoRom), 0);
}
