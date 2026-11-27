//! `tests/ppu.rs` — the line clock, the visible area, forced blank and brightness, the read block
//! (`$2134`-`$213F`), the three write-twice memories and the PPU's own multiplier.

use tendy_snes::ppu::{Ppu, PpuRegs};
use tendy_snes::{DOTS_PER_FRAME, DOTS_PER_LINE, HEIGHT, LINES_PER_FRAME, VISIBLE_DOT, WIDTH};

/// Ticks one dot at a time until the PPU sits on `(line, dot)`. Four master clocks are one dot with
/// no phase left over, so this is exact.
fn run_to(ppu: &mut Ppu, line: u16, dot: u16) {
    let mut guard = 0u64;
    while !(ppu.line == line && ppu.dot == dot) {
        ppu.tick(4);
        guard += 1;
        assert!(
            guard < u64::from(DOTS_PER_FRAME) * 2,
            "run_to({}, {}) never arrived",
            line,
            dot
        );
    }
}

/// Runs into `line`'s first dot, which is where the line is rendered.
fn at_line(ppu: &mut Ppu, line: u16) {
    run_to(ppu, line, 1);
}

/// The finished frame row for screen line `line` (1..=224).
fn row(ppu: &Ppu, line: u16) -> &[u16] {
    let start = (usize::from(line) - 1) * WIDTH;
    &ppu.frame[start..start + WIDTH]
}

/// CGRAM 0 — the backdrop — set through the ports, so `$2121`/`$2122` are exercised too. Brightness
/// 15 is set as well: `INIDISP` is 0 at power-on, and brightness 0 is black.
fn set_backdrop(ppu: &mut Ppu, colour: u16) {
    ppu.write(0x00, 0x0F);
    ppu.write(0x21, 0x00);
    ppu.write(0x22, colour as u8);
    ppu.write(0x22, (colour >> 8) as u8);
}

mod ppu {
    use super::*;

    /// §3.5, §3.6: 262 lines of 341 four-clock dots is 357 368 master clocks per frame.
    #[test]
    fn frame_is_262_lines_of_1364_clocks() {
        assert_eq!(
            DOTS_PER_LINE, 341,
            "fullsnes \"SNES Timing H/V Events\": 341 dots a line"
        );
        assert_eq!(
            LINES_PER_FRAME, 262,
            "fullsnes: V=261 is the last line in NTSC mode"
        );
        let mut ppu = Ppu::new();
        let mut starts = 0;
        for _ in 0..DOTS_PER_FRAME {
            if ppu.tick(4).0 & tendy_snes::ppu::LineEvents::START != 0 {
                starts += 1;
            }
        }
        assert_eq!(starts, LINES_PER_FRAME, "one START per line in a frame");
        assert_eq!(ppu.frames, 1, "one frame per 89 342 dots");
        assert_eq!((ppu.line, ppu.dot), (0, 0), "a frame ends on V=0, H=0");

        // The same frame in one call: 357 368 master clocks, and the END event.
        let mut whole = Ppu::new();
        let events = whole.tick(262 * 1364);
        assert_ne!(
            events.0 & tendy_snes::ppu::LineEvents::END,
            0,
            "END is raised on V=261's last dot"
        );
        assert_eq!(whole.frames, 1, "262 * 1364 master clocks is one frame");
        assert_eq!((whole.line, whole.dot), (0, 0));
    }

    /// §3.5: the visible dots are 22-277, and screen line `n` lands in frame row `n-1` while line
    /// 225 never touches the frame.
    #[test]
    fn visible_area_is_dots_22_to_277() {
        assert_eq!(VISIBLE_DOT, 22, "fullsnes: \"H=22-277(?) draw picture\"");
        assert_eq!(
            VISIBLE_DOT as usize + WIDTH - 1,
            277,
            "256 visible dots starting at dot 22 end at dot 277"
        );

        let mut ppu = Ppu::new();
        set_backdrop(&mut ppu, 0x7FFF);
        at_line(&mut ppu, 1);
        assert_eq!(row(&ppu, 1)[0], 0x7FFF, "line 1 is frame row 0");
        assert_eq!(row(&ppu, 1).len(), WIDTH);

        // A new backdrop colour between line 224 and 225: line 225 is V-blank and writes no row.
        at_line(&mut ppu, 224);
        assert_eq!(row(&ppu, 224)[0], 0x7FFF, "line 224 is frame row 223");
        set_backdrop(&mut ppu, 0x001F);
        at_line(&mut ppu, 225);
        assert_eq!(
            row(&ppu, 224)[0],
            0x7FFF,
            "line 225 is V-blank and writes no row"
        );
        at_line(&mut ppu, 226);
        assert_eq!(row(&ppu, 224)[0], 0x7FFF, "line 226 is not visible either");
        // The next frame does reach line 224 again, and now with the new colour.
        at_line(&mut ppu, 224);
        assert_eq!(
            row(&ppu, 224)[0],
            0x001F,
            "the next frame's line 224 is the new backdrop"
        );
        assert_eq!(ppu.frame.len(), WIDTH * HEIGHT);
    }

    /// §4.3, `$2100` bit 7: forced blank is black regardless of CGRAM and brightness.
    #[test]
    fn forced_blank_shows_black() {
        let mut ppu = Ppu::new();
        set_backdrop(&mut ppu, 0x7FFF);
        ppu.write(0x00, 0x8F); // forced blank, brightness 15
        at_line(&mut ppu, 10);
        assert!(
            row(&ppu, 10).iter().all(|&pixel| pixel == 0),
            "forced blank is black: fullsnes \"Forced Blank Color\""
        );
        ppu.write(0x00, 0x0F);
        at_line(&mut ppu, 11);
        assert_eq!(
            row(&ppu, 11)[0],
            0x7FFF,
            "clearing forced blank shows the backdrop again"
        );
    }

    /// §4.3, `$2100` bits 3-0: `N = 0` is black, otherwise the channel is `N+1` sixteenths.
    #[test]
    fn brightness_scales_the_picture() {
        let mut ppu = Ppu::new();
        set_backdrop(&mut ppu, 0x7FFF);
        ppu.write(0x00, 0x08); // brightness 8 -> each channel 31 * 9 / 16 = 17
        at_line(&mut ppu, 5);
        let expected = (17u16 << 10) | (17 << 5) | 17;
        assert_eq!(
            row(&ppu, 5)[0],
            expected,
            "brightness 8 multiplies each channel by 9/16: fullsnes \"2100h - INIDISP\""
        );
        ppu.write(0x00, 0x00);
        at_line(&mut ppu, 6);
        assert_eq!(row(&ppu, 6)[0], 0, "brightness 0 is black, not 1/16");
        ppu.write(0x00, 0x0F);
        at_line(&mut ppu, 7);
        assert_eq!(
            row(&ppu, 7)[0],
            0x7FFF,
            "brightness 15 is the picture unchanged"
        );
    }

    /// `$2137`/`$213C`: the latch follows the dot counter up the line.
    #[test]
    fn hv_latch_is_monotonic_within_a_line() {
        let mut ppu = Ppu::new();
        run_to(&mut ppu, 40, 10);
        ppu.latch_hv();
        let first = ppu.latch_h;
        run_to(&mut ppu, 40, 300);
        ppu.latch_hv();
        let second = ppu.latch_h;
        assert!(
            second > first,
            "H rises across a line: {first} then {second}"
        );
        assert_eq!(second, 300);
        assert_eq!(ppu.latch_v, 40, "$213D latches the V counter");
        // $213C: the low byte, then bit 0 of the value (bits 7-1 are PPU2 open bus).
        ppu.reset_latch();
        assert_eq!(ppu.read(0x3C), Some(second as u8));
        assert_eq!(ppu.read(0x3C), Some((second >> 8) as u8 & 0x01));
        assert_eq!(ppu.read(0x3D), Some(40));
        assert_eq!(ppu.read(0x3D), Some(0));
    }

    /// `$213E` bits 7-6 and 3-0.
    #[test]
    fn stat77_reports_the_version_and_overflow_flags() {
        let mut ppu = Ppu::new();
        assert_eq!(
            ppu.read(0x3E),
            Some(0x01),
            "$213E reports PPU1 version 1 and no overflow"
        );
        ppu.obj_time_overflow = true;
        assert_eq!(ppu.read(0x3E), Some(0x81));
        ppu.obj_range_overflow = true;
        assert_eq!(ppu.read(0x3E), Some(0xC1));
    }

    /// `$213F` bit 6 clears on read, and reading resets both read flip-flops (`$213F`).
    #[test]
    fn stat78_clears_the_latch_flag() {
        let mut ppu = Ppu::new();
        ppu.latch_hv();
        let first = ppu.read(0x3F).expect("$213F is readable");
        assert_ne!(first & 0x40, 0, "bit 6 is the latch flag after $2137");
        assert_eq!(first & 0x0F, 0x03, "$213F reports PPU2 version 3");
        assert_eq!((first >> 5) & 1, 0, "bit 5 is PPU2 open bus, answered 0");
        let second = ppu.read(0x3F).expect("$213F is readable");
        assert_eq!(second & 0x40, 0, "reading $213F clears the latch flag");
        assert!(!ppu.latch_flag);
        // The read flip-flops are back to their first-read state.
        ppu.latch_hv();
        assert_eq!(ppu.read(0x3C), Some(ppu.latch_h as u8));
    }

    /// `$2104`/`$2138`: two writes below `$200` make one word, at `$200` and above each byte
    /// (fullsnes "2104h - OAMDATA").
    #[test]
    fn oam_write_twice_and_read_back() {
        let mut ppu = Ppu::new();
        ppu.write(0x02, 0x00); // OAMADDL
        ppu.write(0x03, 0x00); // OAMADDH
        ppu.write(0x04, 0x34);
        ppu.write(0x04, 0x12);
        assert_eq!(
            (ppu.oam[0], ppu.oam[1]),
            (0x34, 0x12),
            "the word is low byte first"
        );
        // The address stepped by two bytes; read the pair back through $2138.
        ppu.write(0x02, 0x00);
        ppu.write(0x03, 0x00);
        assert_eq!(ppu.read(0x38), Some(0x34));
        assert_eq!(ppu.read(0x38), Some(0x12));
        // Above $200 every write lands on its own byte.
        ppu.write(0x02, 0xFF);
        ppu.write(0x03, 0x01); // word $1FF -> byte $3FE, mirrored to $21E
        ppu.write(0x04, 0x5A);
        assert_eq!(
            ppu.oam[0x21E], 0x5A,
            "the high table's bytes are written one at a time"
        );
        ppu.write(0x02, 0x00);
        ppu.write(0x03, 0x01);
        ppu.write(0x04, 0x6B);
        assert_eq!(ppu.oam[0x200], 0x6B);
    }

    /// `$2121`/`$2122`/`$213B`.
    #[test]
    fn cgram_write_twice_and_read_back() {
        let mut ppu = Ppu::new();
        ppu.write(0x21, 0x05);
        ppu.write(0x22, 0x1F);
        ppu.write(0x22, 0x7C);
        assert_eq!(
            ppu.cgram[5], 0x7C1F,
            "low byte then high byte make one CGRAM word"
        );
        assert_eq!(ppu.regs.cgadd, 6, "the address steps after the word");
        ppu.write(0x21, 0x05);
        assert_eq!(ppu.read(0x3B), Some(0x1F));
        assert_eq!(
            ppu.read(0x3B),
            Some(0x7C),
            "bit 15 reads as 0, the PPU2 open bus answer"
        );
        assert_eq!(ppu.regs.cgadd, 6);
    }

    /// `$2115`'s address translation and step (fullsnes "2115h - VMAIN").
    #[test]
    fn vram_address_remap_and_step() {
        let mut ppu = Ppu::new();
        // 8-bit rotate, step 1, increment after the high byte.
        ppu.write(0x15, 0x84);
        ppu.write(0x16, 0x00);
        ppu.write(0x17, 0x00);
        ppu.write(0x18, 0x11);
        ppu.write(0x19, 0x22);
        assert_eq!((ppu.vram[0], ppu.vram[1]), (0x11, 0x22));
        assert_eq!(ppu.regs.vmadd, 1, "step 1 after the high byte");
        // Word 1's low 8 bits rotate **left** by 3 -> word $08, byte $10. fullsnes "2115h - VMAIN":
        // "technically it does thrice left-rotate the lower 8, 9, or 10 bits of the Word-address",
        // with the 8-bit diagram `aaaaaaaaYYYxxxxx --> aaaaaaaaxxxxxYYY`; here YYYxxxxx is
        // `00000001` and xxxxxYYY is `00001000`.
        ppu.write(0x18, 0x33);
        ppu.write(0x19, 0x44);
        assert_eq!(
            (ppu.vram[0x10], ppu.vram[0x11]),
            (0x33, 0x44),
            "the 8-bit rotate turns word 1 into word $08: fullsnes \"2115h - VMAIN\""
        );
        // Step 32 with no translation.
        ppu.write(0x15, 0x01);
        ppu.write(0x16, 0x00);
        ppu.write(0x17, 0x01);
        ppu.write(0x18, 0x55);
        assert_eq!(ppu.regs.vmadd, 0x120, "step 32 from word $100");
        assert_eq!(ppu.vram[0x200], 0x55, "the low byte landed at word $100");
    }

    /// `$2115`'s three rotations all go **left** and stay inside their field (fullsnes "2115h -
    /// VMAIN": "technically it does thrice left-rotate the lower 8, 9, or 10 bits of the
    /// Word-address", diagrams `aaaaaaaaYYYxxxxx --> aaaaaaaaxxxxxYYY` and its 9/10-bit forms).
    ///
    /// Word `$0101` gives a different answer at every width, so a right-rotate or a rotate taken
    /// over the whole 16-bit address cannot pass by landing on the same word:
    ///
    /// | field | bits      | left by 3 | result word | byte   |
    /// |-------|-----------|-----------|-------------|--------|
    /// | 8     | `$01`     | `$08`     | `$0108`     | `$0210` |
    /// | 9     | `$101`    | `$00C`    | `$000C`     | `$0018` |
    /// | 10    | `$101`    | `$00A`    | `$000A`     | `$0014` |
    #[test]
    fn vmain_remap_rotates_left_by_three_bits() {
        let mut ppu = Ppu::new();
        for (translation, byte) in [(0x04u8, 0x210usize), (0x08, 0x018), (0x0C, 0x014)] {
            ppu.write(0x15, 0x80 | translation); // increment after the high byte
            ppu.write(0x16, 0x01);
            ppu.write(0x17, 0x01);
            ppu.write(0x18, 0x5A);
            assert_eq!(
                ppu.vram[byte], 0x5A,
                "VMAIN translation {translation:#04x} maps word $0101 to byte {byte:#06x}: \
                 fullsnes \"2115h - VMAIN\" left-rotates the low field by 3"
            );
        }
    }

    /// `$2139`/`$213A`: the prefetch register is refilled **before** the increment, so the first word
    /// arrives twice (fullsnes "SNES Memory VRAM Access").
    #[test]
    fn vram_read_prefetch_returns_the_first_word_twice() {
        let mut ppu = Ppu::new();
        ppu.vram[0x20] = 0x55; // word $10
        ppu.vram[0x21] = 0xAA;
        ppu.vram[0x22] = 0x88; // word $11
        ppu.vram[0x23] = 0x77;
        ppu.write(0x15, 0x80); // step 1, increment after the high byte
        ppu.write(0x16, 0x10);
        ppu.write(0x17, 0x00);
        assert_eq!(
            ppu.read(0x39),
            Some(0x55),
            "the first read is the prefetched word"
        );
        assert_eq!(
            ppu.read(0x3A),
            Some(0xAA),
            "the high byte of the same word: the prefetch refills from the old address"
        );
        assert_eq!(
            ppu.read(0x39),
            Some(0x55),
            "the third read is the same word again: prefetch happens **before** the increment"
        );
        assert_eq!(ppu.read(0x3A), Some(0xAA));
        assert_eq!(
            ppu.read(0x39),
            Some(0x88),
            "the fifth read is the next word's low byte"
        );
        assert_eq!(ppu.regs.vmadd, 0x12);
    }

    /// `$211B`/`$211C`/`$2134`-`$2136`: PPU1's own signed product, `M7A` times the last `$211C`
    /// **byte** as a signed 8-bit value (fullsnes "SNES Maths Multiply/Divide").
    #[test]
    fn m7_product_is_the_signed_m7a_times_m7b() {
        let mut ppu = Ppu::new();
        ppu.write(0x1B, 0x00); // M7A low
        ppu.write(0x1B, 0x01); // M7A high -> $0100
        ppu.write(0x1C, 0xFF); // M7B: the multiplier is this byte, $FF = -1
        assert_eq!(
            ppu.m7_product, -256,
            "(M7A as i16) * (last $211C byte as i8) = 256 * -1"
        );
        assert_eq!(ppu.read(0x34), Some(0x00), "MPYL");
        assert_eq!(ppu.read(0x35), Some(0xFF), "MPYM");
        assert_eq!(ppu.read(0x36), Some(0xFF), "MPYH");
        assert_eq!((-256i32) as u8, 0x00);
        // A fresh M7A multiplies by the byte kept from $211C — and the write-twice latch still holds
        // $FF, so M7A becomes $04FF, not $0400.
        ppu.write(0x1B, 0x04);
        assert_eq!(
            ppu.regs.m7[0], 0x04FF,
            "the shared m7_old latch supplies the low byte"
        );
        assert_eq!(ppu.m7_product, -1279, "1279 * -1");
        assert_eq!(ppu.read(0x34), Some(0x01), "MPYL of -1279");
        assert_eq!(ppu.read(0x35), Some(0xFB), "MPYM of -1279");
        assert_eq!(ppu.read(0x36), Some(0xFF), "MPYH of -1279");
    }

    /// §3.5: the NMI event is dot 0 of line 225, and the frame ends on line 261's last dot.
    #[test]
    fn nmi_event_lands_on_line_225() {
        let mut ppu = Ppu::new();
        let mut at = (0u16, 0u16);
        let mut end_at = (0u16, 0u16);
        let mut level_at_the_event = None;
        for _ in 0..DOTS_PER_FRAME {
            let (line, dot) = (ppu.line, ppu.dot);
            let events = ppu.tick(4);
            if events.0 & tendy_snes::ppu::LineEvents::VBLANK != 0 {
                at = (line, dot);
                level_at_the_event = Some(ppu.nmi);
            }
            if events.0 & tendy_snes::ppu::LineEvents::END != 0 {
                end_at = (line, dot);
            }
        }
        assert_eq!(
            at,
            (225, 0),
            "the NMI event lands on V=225 H=0: fullsnes \"H=0.5, V=225 set NMI flag\""
        );
        assert_eq!(
            level_at_the_event,
            Some(true),
            "the PPU's NMI line is up on the v-blank event"
        );
        assert!(
            !ppu.nmi,
            "and it is low again at V=0, where fullsnes \"4210h\" resets the flag: \"The flag gets \
             reset automatically at end of Vblank\""
        );
        assert_eq!(end_at, (261, 340), "END is the last dot of line 261");
        let _ = PpuRegs::default();
    }
}
