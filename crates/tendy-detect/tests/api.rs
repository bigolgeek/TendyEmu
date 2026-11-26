//! The tendy-detect public API tests (DESIGN §8, #4).

use tendy_detect::Refusal;

#[test]
fn refusal_not_a_rom_text() {
    assert_eq!(
        Refusal::not_a_rom("game.smc").to_string(),
        "refused: not a recognised ROM (game.smc)"
    );
}

#[test]
fn refusal_no_rom_inside_text() {
    assert_eq!(
        Refusal::no_rom_inside("roms.zip").to_string(),
        "refused: no ROM inside roms.zip"
    );
}
