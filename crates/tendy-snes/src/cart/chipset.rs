//! The coprocessor table: what a cartridge's `$FFD6` byte (subclassed by `$FFBF` where fullsnes
//! says so) says it carries. v1 runs three board kinds and refuses the rest
//! **by name**, which is a feature: a named refusal is better than a cartridge that runs wrong.
//!
//! Sources: fullsnes "Chipset (ROM/RAM information on cart) (FFD6h) (and some subclassed via
//! FFBFh)" for the byte table, and fullsnes "DSPn/ST010/ST011 Cartridge Header" for the two
//! families whose variant the header cannot carry.

/// The board a cartridge carries, from the `$FFD6` byte and its `$FFBF` subclass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chipset {
    Rom,
    RomRam,
    RomRamBattery,
    Dsp1,
    Dsp1B,
    Dsp2,
    Dsp3,
    Dsp4,
    Gsu1,
    Gsu2,
    Sa1,
    SdD1,
    Spc7110,
    Spc7110Rtc,
    Cx4,
    Obc1,
    St010,
    St011,
    Sgb,
    Satellaview,
    Unknown(u8),
}

impl Chipset {
    /// The chipset byte `$FFD6` (with `$FFBF` where fullsnes subclasses it) → the board's kind.
    ///
    /// The byte encodes a family and its RAM/battery flags, not one chip: fullsnes's pattern is
    /// `x3h` ROM+coprocessor, `x4h` +RAM, `x5h` +RAM+battery, `x6h` +battery, `x9h` +RTC, and the
    /// high nibble names the family (`0xh` DSP, `1xh` GSU, `2xh` OBC1, `3xh` SA-1, `4xh` S-DD1,
    /// `5xh` S-RTC, `Exh` Super Game Boy / Satellaview, `Fxh` custom via `$FFBF`). A refusal only
    /// needs the family's name, so the values below are the ones fullsnes says shipped.
    ///
    /// Two variant questions are not decidable from the header at all — fullsnes
    /// "DSPn/ST010/ST011 Cartridge Header": "The header [...] includes no info whether a DSPn game
    /// uses a DSP1, DSP2, DSP3, or DSP4, and no info if a ST010/ST011 game uses ST010 or ST011."
    /// Those families are refused under their first member's name; the other names stay in the
    /// enum for a future board database, and a GSU is refined by image size in
    /// [`Chipset::for_rom_size`].
    ///
    /// Every value this table does not name is `Unknown(byte)` and is refused with the byte
    /// printed, which is what `$55` (S-RTC, which §2.7's enum has no variant for) and `$F5.02`
    /// (ST018) get.
    pub fn from_byte(byte: u8, subtype: u8) -> Chipset {
        match byte {
            0x00 => Chipset::Rom,
            0x01 => Chipset::RomRam,
            0x02 => Chipset::RomRamBattery,
            // 0xh: DSP family. 03h DSP, 04h +RAM, 05h +RAM+battery, 06h +battery, 09h RTC-4513.
            0x03..=0x09 => Chipset::Dsp1,
            // 1xh: GSU family. 13h MarioChip1/expansion RAM, 14h +RAM, 15h +RAM+battery,
            // 1Ah GSU1+RAM+battery+fast (Stunt Race).
            0x13..=0x1A => Chipset::Gsu1,
            // 2xh: OBC1 family; 25h is the one fullsnes lists as produced.
            0x23 | 0x25 => Chipset::Obc1,
            // 3xh: SA-1 family; 32h, 34h and 35h shipped.
            0x32..=0x35 => Chipset::Sa1,
            // 4xh: S-DD1 family; 43h and 45h shipped.
            0x43..=0x45 => Chipset::SdD1,
            0xE3 => Chipset::Sgb,
            0xE5 => Chipset::Satellaview,
            // Fxh: the custom chips, "subclassed via [FFBFh]=xxh".
            0xF3 => match subtype {
                0x10 => Chipset::Cx4,
                _ => Chipset::Unknown(byte),
            },
            0xF5 => match subtype {
                0x00 => Chipset::Spc7110,
                _ => Chipset::Unknown(byte),
            },
            0xF6 => match subtype {
                0x01 => Chipset::St010,
                _ => Chipset::Unknown(byte),
            },
            0xF9 => match subtype {
                0x00 => Chipset::Spc7110Rtc,
                _ => Chipset::Unknown(byte),
            },
            other => Chipset::Unknown(other),
        }
    }

    /// Names a GSU board from the image's size, the only place that fact exists.
    ///
    /// fullsnes's `$FFD6` table: "14h ROM+GSU+RAM ;\ROM size up to 1MByte → GSU1" and
    /// "15h ROM+GSU+RAM+Battery ;/ROM size above 1MByte → GSU2". The size is the padded image
    /// (`Cartridge::rom`), so a non-power-of-two image is judged as the hardware rounds it.
    pub fn for_rom_size(self, bytes: usize) -> Chipset {
        match self {
            Chipset::Gsu1 if bytes > 0x10_0000 => Chipset::Gsu2,
            other => other,
        }
    }

    /// The board's name, as the refusal message and the status bar print it (fullsnes's names).
    pub fn name(self) -> &'static str {
        match self {
            Chipset::Rom => "ROM",
            Chipset::RomRam => "ROM+RAM",
            Chipset::RomRamBattery => "ROM+RAM+battery",
            Chipset::Dsp1 => "DSP-1",
            Chipset::Dsp1B => "DSP-1B",
            Chipset::Dsp2 => "DSP-2",
            Chipset::Dsp3 => "DSP-3",
            Chipset::Dsp4 => "DSP-4",
            Chipset::Gsu1 => "SuperFX/GSU-1",
            Chipset::Gsu2 => "SuperFX/GSU-2",
            Chipset::Sa1 => "SA-1",
            Chipset::SdD1 => "S-DD1",
            Chipset::Spc7110 => "SPC7110",
            Chipset::Spc7110Rtc => "SPC7110+RTC",
            Chipset::Cx4 => "CX4",
            Chipset::Obc1 => "OBC1",
            Chipset::St010 => "ST010",
            Chipset::St011 => "ST011",
            Chipset::Sgb => "SGB",
            Chipset::Satellaview => "Satellaview",
            Chipset::Unknown(_) => "unknown",
        }
    }

    /// True for the three the v1 core runs: ROM only, ROM+RAM, ROM+RAM+battery.
    pub fn is_supported(self) -> bool {
        matches!(
            self,
            Chipset::Rom | Chipset::RomRam | Chipset::RomRamBattery
        )
    }
}
