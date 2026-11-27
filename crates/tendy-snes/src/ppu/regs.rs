//! `PpuRegs`: the `$2100`-`$213F` register file and its write decode.
//!
//! The field list is frozen (§2.5); the bodies are the PPU lane's. Two encodings inside the frozen
//! fields are worth reading before the code:
//!
//! - `oamadd` keeps the OAM **byte** address in bits 0-9 and the priority-rotation enable in bit 15.
//!   Hardware has a 9-bit reload word address, a 10-bit live byte address and the rotation bit, i.e.
//!   twenty bits for a sixteen-bit field. Bits 0-9 hold the live byte address, so
//!   `Ppu::reload_oamadd` can restore the word address only by forcing bit 0 clear; that is the one
//!   place v1 is not faithful and it is unobservable here — OAM evaluation walks all 128 entries by
//!   index, no port reads the address back, and every cartridge re-arms `$2102`/`$2103` before an
//!   OAM transfer (fullsnes "SNES Memory OAM Access", "2102h/2103h").
//! - Every "write twice" port here is the hardware's **previous-byte** latch, not a flip-flop:
//!   `M7_reg = new * 100h + M7_old; M7_old = new` (fullsnes "SNES PPU Rotation/Scaling"), and
//!   `BGnHOFS = (value << 8) | (bgofs_latch & ~7) | (bghofs_latch & 7)` with both latches set to
//!   `value` (SNESdev `PPU registers`, "BGnHOFS"). Both latches always hold the same byte, so the
//!   one `bg_old` field the design gives is enough.

/// The PPU register file, exactly as §2.5 declares it.
#[derive(Clone, Default)]
pub struct PpuRegs {
    pub inidisp: u8,
    pub obsel: u8,
    /// Bits 0-9: the live OAM byte address; bit 15: `$2103` bit 7, priority rotation.
    pub oamadd: u16,
    pub bgmode: u8,
    pub mosaic: u8,
    pub bgsc: [u8; 4],
    pub nba: [u8; 4],
    pub bghofs: [u16; 4],
    pub bgvofs: [u16; 4],
    pub vmain: u8,
    pub vmadd: u16,
    pub m7sel: u8,
    pub m7: [i16; 4],
    pub m7x: i16,
    pub m7y: i16,
    pub m7hofs: i16,
    pub m7vofs: i16,
    pub cgadd: u8,
    pub w12sel: u8,
    pub w34sel: u8,
    pub wobjsel: u8,
    pub wh: [u8; 4],
    pub wbglog: u8,
    pub wobjlog: u8,
    pub tm: u8,
    pub ts: u8,
    pub tmw: u8,
    pub tsw: u8,
    pub cgwsel: u8,
    pub cgadsub: u8,
    pub coldata: u8,
    pub setini: u8,
    /// The write-twice latches.
    pub bg_old: u8,
    pub m7_old: u8,
    pub vram_lsb: u8,
    pub oam_lsb: u8,
    pub cgram_lsb: u8,
    /// The last byte written to `$211C` (M7B), for the PPU's own signed multiplier.
    pub m7b_byte: u8,
}

/// `oamadd` bit 15: `$2103` bit 7, "OBJ Priority Rotation (0=OBJ #0, 1=OBJ #N)".
pub const OAMADD_ROTATE: u16 = 0x8000;
/// `oamadd` bits 0-9: the live OAM byte address.
pub const OAMADD_ADDRESS: u16 = 0x03FF;

impl PpuRegs {
    /// `$2100`-`$213F`, as an offset from `$2100`. The memory ports (`$2104`, `$2118`/`$2119`,
    /// `$2122`, `$2132`) need state outside this struct and are decoded in `Ppu::write`; this
    /// function handles the pure register stores and the two write-twice latches.
    pub fn write(&mut self, reg: u8, value: u8) {
        match reg {
            0x00 => self.inidisp = value,
            0x01 => self.obsel = value,
            0x02 => self.write_oamadd_low(value),
            0x03 => self.write_oamadd_high(value),
            // 0x04 OAMDATA: `Ppu::write`.
            0x05 => self.bgmode = value,
            0x06 => self.mosaic = value,
            0x07..=0x0A => self.bgsc[usize::from(reg - 0x07)] = value,
            0x0B => {
                self.nba[0] = value & 0x0F;
                self.nba[1] = value >> 4;
            }
            0x0C => {
                self.nba[2] = value & 0x0F;
                self.nba[3] = value >> 4;
            }
            0x0D..=0x14 => self.write_bg_scroll(reg - 0x0D, value),
            0x15 => self.vmain = value,
            0x16 => self.vmadd = (self.vmadd & 0xFF00) | u16::from(value),
            0x17 => self.vmadd = (self.vmadd & 0x00FF) | (u16::from(value) << 8),
            // 0x18/0x19 VMDATA: `Ppu::write`.
            0x1A => self.m7sel = value,
            0x1B..=0x1E => {
                let index = usize::from(reg - 0x1B);
                let wide = (i16::from(value) << 8) | i16::from(self.m7_old);
                self.m7[index] = wide;
                self.m7_old = value;
                if reg == 0x1C {
                    // "211Ch - M7B ... Any Write: Signed 8bit Multiplier" (fullsnes "SNES Maths
                    // Multiply/Divide"): the PPU's multiplier keeps the byte, not the 16-bit word.
                    self.m7b_byte = value;
                }
            }
            0x1F => self.m7x = self.write_m7_13(value),
            0x20 => self.m7y = self.write_m7_13(value),
            0x21 => self.cgadd = value,
            // 0x22 CGDATA: `Ppu::write`.
            0x23 => self.w12sel = value,
            0x24 => self.w34sel = value,
            0x25 => self.wobjsel = value,
            0x26..=0x29 => self.wh[usize::from(reg - 0x26)] = value,
            0x2A => self.wbglog = value,
            0x2B => self.wobjlog = value,
            0x2C => self.tm = value,
            0x2D => self.ts = value,
            0x2E => self.tmw = value,
            0x2F => self.tsw = value,
            0x30 => self.cgwsel = value,
            0x31 => self.cgadsub = value,
            0x32 => self.coldata = value,
            0x33 => self.setini = value,
            // `$2134`-`$213F` are read-only; a write there lands on nothing.
            _ => {}
        }
    }

    /// `$2102`: the low eight bits of the OAM address. Hardware copies the whole 9-bit reload value
    /// into the address register with bit 0 clear (fullsnes "2102h/2103h").
    fn write_oamadd_low(&mut self, value: u8) {
        let word = (self.oam_word() & 0x100) | u16::from(value);
        self.oamadd = (OAMADD_ROTATE & self.oamadd) | (word << 1);
    }

    /// `$2103`: bit 0 is the OAM address's bit 8, bit 7 selects priority rotation, and bits 1-7 are
    /// the OBJ number that becomes highest priority (fullsnes "2102h/2103h").
    fn write_oamadd_high(&mut self, value: u8) {
        let word = (self.oam_word() & 0x0FF) | (u16::from(value & 0x01) << 8);
        let rotation = (u16::from(value) & 0x80) << 8;
        self.oamadd = rotation | (word << 1);
    }

    /// The 9-bit OAM word address the live byte address currently points at.
    pub fn oam_word(&self) -> u16 {
        (self.oamadd & OAMADD_ADDRESS) >> 1
    }

    /// The 7-bit OBJ number `$2103` bits 1-7 select as highest priority, from the address's bits 1-7.
    pub fn oam_rotation_index(&self) -> u8 {
        (self.oam_word() as u8) >> 1
    }

    /// True when `$2103` bit 7 asked for priority rotation.
    pub fn oam_rotating(&self) -> bool {
        self.oamadd & OAMADD_ROTATE != 0
    }

    /// `$210D`-`$2114`: the eight BG scroll ports, 10 bits each, previous-byte latch. `$210D` and
    /// `$210E` double as `M7HOFS`/`M7VOFS` through a **separate** latch (fullsnes "SNES PPU
    /// Rotation/Scaling", "M7HOVS/M7VOFS Port Notes").
    fn write_bg_scroll(&mut self, which: u8, value: u8) {
        let layer = usize::from(which) / 2;
        let wide = (u16::from(value) << 8) | u16::from(self.bg_old);
        self.bg_old = value;
        if which & 1 == 0 {
            self.bghofs[layer] = wide & 0x03FF;
        } else {
            self.bgvofs[layer] = wide & 0x03FF;
        }
        if which < 2 {
            let m7 = (i16::from(value) << 8) | i16::from(self.m7_old);
            // 13-bit signed: 1 sign bit, 12 integer bits, no fraction (fullsnes "$210Dh - M7HOFS");
            // the left shift moves bit 12 into the sign, the right shift restores it.
            let signed = (m7 << 3) >> 3;
            if which == 0 {
                self.m7hofs = signed;
            } else {
                self.m7vofs = signed;
            }
            self.m7_old = value;
        }
    }

    /// `$211F`/`$2120`: a 13-bit signed value from the two-write latch.
    fn write_m7_13(&mut self, value: u8) -> i16 {
        let wide = i16::from(value) << 8 | i16::from(self.m7_old);
        self.m7_old = value;
        // Sign-extend bit 12.
        (wide << 3) >> 3
    }
}
