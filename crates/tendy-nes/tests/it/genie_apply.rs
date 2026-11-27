//! Game Genie codes applied to CPU reads, end to end: a code installed on the bus
//! substitutes the byte the CPU reads from `$8000-$FFFF`, an 8-letter code substitutes only
//! where its compare byte already sits, and the code list stays out of the save state.

use tendy_nes::cpu::Bus;
use tendy_nes::genie::{Code, encode};
use tendy_nes::nes::Nes;

/// A 16 KB NROM image with ROM bytes patched inside PRG (index = CPU address `$8000` at
/// offset 0, `$FFFF` at 0x3FFF, mirrored). Flat zeros do not run: reset goes to a `JMP`
/// loop at $8000 and every vector lands on the `RTI` at $8100, so `run_frame` is harmless.
fn nrom_with(prg: &[(usize, u8)]) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16_384 + 8_192, 0x00);
    v[16 + 0x100] = 0x40; // RTI at $8100 = the NMI/IRQ handler
    // Vectors, low byte first: NMI and IRQ -> $8100, RESET -> $8000.
    for (index, byte) in [
        (0x3FFAu64, 0x00u8),
        (0x3FFBu64, 0x81u8),
        (0x3FFCu64, 0x00u8),
        (0x3FFDu64, 0x80u8),
        (0x3FFEu64, 0x00u8),
        (0x3FFFu64, 0x81u8),
    ] {
        v[16 + index as usize] = byte;
    }
    for &(index, byte) in prg {
        v[16 + index] = byte;
    }
    v
}

/// A 6-letter code substitutes every read at its address; neighbours keep the ROM's
/// own bytes. The code's letters come out of `genie::encode`, not out of guesswork.
#[test]
fn a_six_letter_code_patches_the_cpu_read() {
    let rom = nrom_with(&[(0x122, 0x11), (0x123, 0xDE), (0x124, 0x5A)]);
    let word = encode(Code {
        addr: 0x8123,
        value: 0xBD,
        compare: None,
    });
    let code = tendy_nes::genie::decode(&word).unwrap();

    let mut nes = Nes::new(&rom).unwrap();
    assert_eq!(nes.bus.read(0x8123), 0xDE, "ROM byte before the code");
    nes.bus.set_genie_codes(vec![code]);
    assert_eq!(nes.bus.read(0x8123), 0xBD, "the code substitutes");
    // Neighbouring addresses read straight from the ROM, unchanged.
    assert_eq!(nes.bus.read(0x8122), 0x11);
    assert_eq!(nes.bus.read(0x8124), 0x5A);
}

/// An 8-letter code substitutes only when the ROM byte equals its compare byte.
/// The code is `SLXPLOVS`, which `genie.rs` pins to `$9123` value `$BD` compare `$DE`
/// (satoshinm's published vector), so the ROM just carries that byte.
#[test]
fn an_eight_letter_code_applies_only_over_the_compare_byte() {
    let code = tendy_nes::genie::decode("SLXPLOVS").unwrap();

    let mut nes = Nes::new(&nrom_with(&[(0x1123, 0xDE)])).unwrap();
    nes.bus.set_genie_codes(vec![code]);
    assert_eq!(nes.bus.read(0x9123), 0xBD, "compare matches: substituted");

    let mut other = Nes::new(&nrom_with(&[(0x1123, 0xDD)])).unwrap();
    other.bus.set_genie_codes(vec![code]);
    assert_eq!(other.bus.read(0x9123), 0xDD, "compare misses: ROM byte");
}

/// Codes are frontend configuration, not emulation state — a machine running with
/// codes installed saves exactly the bytes a machine without them saves, even though the
/// code list is on the bus.
#[test]
fn genie_codes_never_enter_the_save_state() {
    // NROM flat with an idle `JMP` loop at $8000 and `RTI` vectors, so two frames run.
    let rom = nrom_with(&[(0x000, 0x4C), (0x001, 0x00), (0x002, 0x80)]);

    let mut with_slot_empty = Nes::new(&rom).unwrap();
    with_slot_empty.bus.set_genie_codes(vec![]);

    let mut with_codes = Nes::new(&rom).unwrap();
    // A code the guest never reads: `$8F00` is outside the instruction loop at $8000,
    // so both machines fetch identical PRG bytes for the whole two frames.
    with_codes.bus.set_genie_codes(vec![Code {
        addr: 0x8F00,
        value: 0x42,
        compare: None,
    }]);
    assert_eq!(with_codes.bus.genie_codes().len(), 1);

    with_slot_empty.run_frame();
    with_slot_empty.run_frame();
    with_codes.run_frame();
    with_codes.run_frame();
    assert_eq!(
        with_codes.save_state(),
        with_slot_empty.save_state(),
        "the code list must not reach the machine state"
    );
    // ...and the state lands in a fresh machine without the frontend touching anything.
    let mut restored = Nes::new(&rom).unwrap();
    assert_eq!(restored.load_state(&with_codes.save_state()), Ok(()));
}
