//! The three built-in palettes — shade index -> RGBA — and the CGB's own 15-bit -> RGBA expansion.

/// The palette chosen in Video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaletteName {
    #[default]
    DmgGreen,
    Grey,
    Pocket,
}

impl PaletteName {
    /// The name `config.toml` writes.
    pub fn name(self) -> &'static str {
        match self {
            PaletteName::DmgGreen => "dmg-green",
            PaletteName::Grey => "grey",
            PaletteName::Pocket => "pocket",
        }
    }

    pub fn from_name(name: &str) -> Option<PaletteName> {
        match name {
            "dmg-green" => Some(PaletteName::DmgGreen),
            "grey" => Some(PaletteName::Grey),
            "pocket" => Some(PaletteName::Pocket),
            _ => None,
        }
    }
}

/// Shade 0 lightest .. shade 3 darkest, as RGBA.
pub fn table(name: PaletteName) -> [[u8; 4]; 4] {
    match name {
        PaletteName::DmgGreen => [
            [0x9B, 0xBC, 0x0F, 0xFF],
            [0x8B, 0xAC, 0x0F, 0xFF],
            [0x30, 0x62, 0x30, 0xFF],
            [0x0F, 0x38, 0x0F, 0xFF],
        ],
        PaletteName::Grey => [
            [0xFF, 0xFF, 0xFF, 0xFF],
            [0xAA, 0xAA, 0xAA, 0xFF],
            [0x55, 0x55, 0x55, 0xFF],
            [0x00, 0x00, 0x00, 0xFF],
        ],
        PaletteName::Pocket => [
            [0xC4, 0xCF, 0xA1, 0xFF],
            [0x8B, 0x95, 0x6D, 0xFF],
            [0x4D, 0x53, 0x3C, 0xFF],
            [0x1F, 0x1F, 0x1F, 0xFF],
        ],
    }
}

/// One CGB colour as RGBA: RGB555 expanded to 8 bits a channel by `(v << 3) | (v >> 2)`, which is
/// the rule `cgb-acid2`'s own documentation gives for comparing an emulator's output with its
/// reference image, and the nearest 8-bit value to the 5-bit one the hardware stores (bit 15 is
/// ignored). A CGB frame has no `PaletteName`: its colours come from the cartridge.
pub fn rgb555(word: u16) -> [u8; 4] {
    let expand = |value: u16| {
        let v = value & 0x1F;
        ((v << 3) | (v >> 2)) as u8
    };
    [expand(word), expand(word >> 5), expand(word >> 10), 0xFF]
}

/// One resolved compatibility palette: the three palettes the colour boot ROM's ID picks for a
/// monochrome cartridge — the background, objects 0 and 2 (`OBP0`) and objects 1 and 3 (`OBP1`) —
/// each four RGB555 colours (Pan Docs, *Power-Up Sequence*, "Compatibility palettes"). In DMG
/// compatibility mode the background's attribute map reads all zeros (so it uses BG palette 0),
/// `BGP`/`OBP0`/`OBP1` index into these colours instead of the DMG's shades of grey, and objects
/// take `OBJ` palette 0 or 1 from bit 4 of their attribute (Pan Docs, *Palettes*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompatTriplet {
    /// BG palette 0's four colours, shade 0 lightest .. shade 3 darkest.
    pub bg: [u16; 4],
    /// OBJ palette 0 (`OBP0`), the same order.
    pub obj0: [u16; 4],
    /// OBJ palette 1 (`OBP1`), the same order.
    pub obj1: [u16; 4],
}

/// The 45 assigned palette configurations, keyed by the packed selector the boot ROM reads from
/// its ID table (`entry | flags << 5`; see `crate::cgb::TRIPLET_IDS_AND_FLAGS`). The colours are
/// TCRF's published list of the boot ROM's palette entries (tcrf.net, *Game Boy Color Bootstrap
/// ROM*), each resolved through the documented shuffling flags — one documented list of assigned
/// colours, not a disassembly. The `$7C` entry (entry `$1C`, flags `$03`) is the dummy used for
/// every cartridge that fails the licensee check or is not in the title table, so a cartridge that
/// fails every check is still coloured rather than left grey.
pub const COMPAT_PALETTES: &[(u8, CompatTriplet)] = &[
    // Balloon Kid / Tetris Blast.
    (
        0x06,
        CompatTriplet {
            bg: [0x7fff, 0x027f, 0x001f, 0x0000],
            obj0: [0x7fff, 0x027f, 0x001f, 0x0000],
            obj1: [0x7fff, 0x027f, 0x001f, 0x0000],
        },
    ),
    // Tetris / Pocket Monsters Pikachu.
    (
        0x07,
        CompatTriplet {
            bg: [0x7fff, 0x03ff, 0x001f, 0x0000],
            obj0: [0x7fff, 0x03ff, 0x001f, 0x0000],
            obj1: [0x7fff, 0x03ff, 0x001f, 0x0000],
        },
    ),
    // Alleyway.
    (
        0x08,
        CompatTriplet {
            bg: [0x7e74, 0x03ff, 0x0180, 0x0000],
            obj0: [0x7e74, 0x03ff, 0x0180, 0x0000],
            obj1: [0x7e74, 0x03ff, 0x0180, 0x0000],
        },
    ),
    // F-1 Race / Yakuman / Mario's Picross.
    (
        0x12,
        CompatTriplet {
            bg: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj0: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj1: [0x7fff, 0x32bf, 0x00d0, 0x0000],
        },
    ),
    // Arcade Classic No. 3 - Galaga & Galaxian / Solar Striker.
    (
        0x13,
        CompatTriplet {
            bg: [0x0000, 0x4200, 0x037f, 0x7fff],
            obj0: [0x0000, 0x4200, 0x037f, 0x7fff],
            obj1: [0x0000, 0x4200, 0x037f, 0x7fff],
        },
    ),
    // X.
    (
        0x16,
        CompatTriplet {
            bg: [0x7fff, 0x5294, 0x294a, 0x0000],
            obj0: [0x7fff, 0x5294, 0x294a, 0x0000],
            obj1: [0x7fff, 0x5294, 0x294a, 0x0000],
        },
    ),
    // Pocket Camera.
    (
        0x1b,
        CompatTriplet {
            bg: [0x7fff, 0x033f, 0x0193, 0x0000],
            obj0: [0x7fff, 0x033f, 0x0193, 0x0000],
            obj1: [0x7fff, 0x033f, 0x0193, 0x0000],
        },
    ),
    // Radar Mission.
    (
        0x20,
        CompatTriplet {
            bg: [0x7fff, 0x42b5, 0x3dc8, 0x0000],
            obj0: [0x7fff, 0x01df, 0x0112, 0x0000],
            obj1: [0x7fff, 0x42b5, 0x3dc8, 0x0000],
        },
    ),
    // Pocket Monsters Ao (Pokemon Blue).
    (
        0x2b,
        CompatTriplet {
            bg: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Kaeru no Tame ni Kane wa Naru.
    (
        0x2d,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x6e31, 0x454a, 0x0000],
        },
    ),
    // Game Boy Camera Gold / Pokemon Red.
    (
        0x30,
        CompatTriplet {
            bg: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj0: [0x7fff, 0x1bef, 0x0200, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // James Bond 007 / Pokemon Green.
    (
        0x3c,
        CompatTriplet {
            bg: [0x7fff, 0x1bef, 0x6180, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x1bef, 0x6180, 0x0000],
        },
    ),
    // Dr. Mario.
    (
        0x4b,
        CompatTriplet {
            bg: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj0: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Pinocchio.
    (
        0x4c,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj1: [0x231f, 0x035f, 0x00f2, 0x0009],
        },
    ),
    // Moguranya / Mole Mania.
    (
        0x60,
        CompatTriplet {
            bg: [0x7fff, 0x42b5, 0x3dc8, 0x0000],
            obj0: [0x7fff, 0x01df, 0x0112, 0x0000],
            obj1: [0x7fff, 0x01df, 0x0112, 0x0000],
        },
    ),
    // Game & Watch Gallery / Game Boy Gallery.
    (
        0x64,
        CompatTriplet {
            bg: [0x7fff, 0x03ef, 0x01d6, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Mario & Yoshi / Yoshi.
    (
        0x65,
        CompatTriplet {
            bg: [0x7fff, 0x03ea, 0x011f, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Donkey Kong.
    (
        0x66,
        CompatTriplet {
            bg: [0x7fff, 0x027f, 0x001f, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Kirby's Pinball Land.
    (
        0x68,
        CompatTriplet {
            bg: [0x7e74, 0x03ff, 0x0180, 0x0000],
            obj0: [0x299f, 0x001a, 0x000c, 0x0000],
            obj1: [0x299f, 0x001a, 0x000c, 0x0000],
        },
    ),
    // Super Mario Land.
    (
        0x6a,
        CompatTriplet {
            bg: [0x7ed6, 0x4bff, 0x2175, 0x0000],
            obj0: [0x0000, 0x7fff, 0x421f, 0x1cf2],
            obj1: [0x0000, 0x7fff, 0x421f, 0x1cf2],
        },
    ),
    // Pocket Bomberman.
    (
        0x6c,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x231f, 0x035f, 0x00f2, 0x0009],
            obj1: [0x231f, 0x035f, 0x00f2, 0x0009],
        },
    ),
    // Kid Icarus - Of Myths and Monsters.
    (
        0x6d,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Arcade Classic No. 1 - Asteroids & Missile Command.
    (
        0x6e,
        CompatTriplet {
            bg: [0x7fff, 0x1bef, 0x0200, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Chessmaster / Dynablaster.
    (
        0x6f,
        CompatTriplet {
            bg: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj0: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Battletoads in Ragnarok's World.
    (
        0x72,
        CompatTriplet {
            bg: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj0: [0x7fff, 0x1bef, 0x0200, 0x0000],
            obj1: [0x7fff, 0x1bef, 0x0200, 0x0000],
        },
    ),
    // The dummy entry: every cartridge that fails a check or is not in the title table.
    (
        0x7c,
        CompatTriplet {
            bg: [0x7fff, 0x1bef, 0x6180, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Tetris Attack / Yoshi no Panepon.
    (
        0x85,
        CompatTriplet {
            bg: [0x7fff, 0x03ea, 0x011f, 0x0000],
            obj0: [0x7fff, 0x03ea, 0x011f, 0x0000],
            obj1: [0x7fff, 0x7eeb, 0x001f, 0x7c00],
        },
    ),
    // Yoshi no Cookie / Yoshi's Cookie.
    (
        0x86,
        CompatTriplet {
            bg: [0x7fff, 0x027f, 0x001f, 0x0000],
            obj0: [0x7fff, 0x027f, 0x001f, 0x0000],
            obj1: [0x7fff, 0x7eeb, 0x001f, 0x7c00],
        },
    ),
    // Qix / Tetris 2 / Tetris Flash.
    (
        0x87,
        CompatTriplet {
            bg: [0x7fff, 0x03ff, 0x001f, 0x0000],
            obj0: [0x7fff, 0x03ff, 0x001f, 0x0000],
            obj1: [0x7fff, 0x7eeb, 0x001f, 0x7c00],
        },
    ),
    // Game Boy Wars / Wario Land - Super Mario Land 3.
    (
        0xa0,
        CompatTriplet {
            bg: [0x7fff, 0x42b5, 0x3dc8, 0x0000],
            obj0: [0x7fff, 0x01df, 0x0112, 0x0000],
            obj1: [0x7fff, 0x7eeb, 0x001f, 0x7c00],
        },
    ),
    // Donkey Kong Land / Super Donkey Kong GB.
    (
        0xa1,
        CompatTriplet {
            bg: [0x4fff, 0x7ed2, 0x3a4c, 0x1ce0],
            obj0: [0x231f, 0x035f, 0x00f2, 0x0009],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Soccer / Tennis / Top Rank Tennis.
    (
        0xa2,
        CompatTriplet {
            bg: [0x03ed, 0x7fff, 0x255f, 0x0000],
            obj0: [0x7fff, 0x7fff, 0x7e8c, 0x7c00],
            obj1: [0x7fff, 0x32bf, 0x00d0, 0x0000],
        },
    ),
    // Baseball.
    (
        0xa3,
        CompatTriplet {
            bg: [0x036a, 0x021f, 0x03ff, 0x7fff],
            obj0: [0x7fff, 0x7fff, 0x7e8c, 0x7c00],
            obj1: [0x7fff, 0x421f, 0x1cf2, 0x0000],
        },
    ),
    // Hoshi no Kirby / Kirby's Dream Land.
    (
        0xa8,
        CompatTriplet {
            bg: [0x7e74, 0x03ff, 0x0180, 0x0000],
            obj0: [0x299f, 0x001a, 0x000c, 0x0000],
            obj1: [0x7c00, 0x7fff, 0x3fff, 0x7e00],
        },
    ),
    // Super Mario Land 2 - 6 Golden Coins.
    (
        0xa9,
        CompatTriplet {
            bg: [0x67ff, 0x77ac, 0x1a13, 0x2d6b],
            obj0: [0x7fff, 0x01df, 0x0112, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Wave Race.
    (
        0xab,
        CompatTriplet {
            bg: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x3fff, 0x7e00, 0x001f],
        },
    ),
    // Donkey Kong Land (Japan) / Donkey Kong Land III.
    (
        0xac,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x231f, 0x035f, 0x00f2, 0x0009],
            obj1: [0x7fff, 0x7eeb, 0x001f, 0x7c00],
        },
    ),
    // Killer Instinct.
    (
        0xad,
        CompatTriplet {
            bg: [0x7fff, 0x6e31, 0x454a, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x32bf, 0x00d0, 0x0000],
        },
    ),
    // Magnetic Soccer / Mystic Quest / Othello.
    (
        0xae,
        CompatTriplet {
            bg: [0x7fff, 0x1bef, 0x0200, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Adventures of Lolo / Mega Man.
    (
        0xaf,
        CompatTriplet {
            bg: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj0: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj1: [0x7fff, 0x1bef, 0x0200, 0x0000],
        },
    ),
    // Legend of Zelda, The - Link's Awakening.
    (
        0xb1,
        CompatTriplet {
            bg: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj0: [0x7fff, 0x03e0, 0x0206, 0x0120],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Boy and His Blob in the Rescue of Princess Blobette, A / Star Wars.
    (
        0xb2,
        CompatTriplet {
            bg: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj0: [0x7fff, 0x1bef, 0x0200, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Metroid II - Return of Samus.
    (
        0xb4,
        CompatTriplet {
            bg: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
            obj0: [0x03ff, 0x001f, 0x000c, 0x0000],
            obj1: [0x7fff, 0x1bef, 0x0200, 0x0000],
        },
    ),
    // Wario Land II.
    (
        0xb5,
        CompatTriplet {
            bg: [0x7fff, 0x42b5, 0x3dc8, 0x0000],
            obj0: [0x7fff, 0x32bf, 0x00d0, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
    // Pac-In-Time.
    (
        0xbc,
        CompatTriplet {
            bg: [0x7fff, 0x1bef, 0x6180, 0x0000],
            obj0: [0x7fff, 0x421f, 0x1cf2, 0x0000],
            obj1: [0x7fff, 0x7e8c, 0x7c00, 0x0000],
        },
    ),
];

/// The resolved triplet for a packed selector (`entry | flags << 5`). Falls back to the dummy
/// `$7C` entry, which is also what the boot ROM uses for every cartridge that fails a check.
pub fn compat_triplet(packed: u8) -> &'static CompatTriplet {
    COMPAT_PALETTES
        .iter()
        .find(|(selector, _)| *selector == packed)
        .map(|(_, triplet)| triplet)
        .unwrap_or_else(|| {
            COMPAT_PALETTES
                .iter()
                .find(|(selector, _)| *selector == 0x7C)
                .map(|(_, triplet)| triplet)
                .expect("the dummy compatibility palette is present")
        })
}
