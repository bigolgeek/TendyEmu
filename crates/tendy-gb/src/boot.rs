//! The boot ROMs we ship: one hand-assembled 256-byte page at `$0000-$00FF` for the monochrome
//! machine, and one hand-assembled 2048-byte colour page for the Game Boy Color, both off unless
//! chosen.
//!
//! No byte here comes from an image. Each instruction is written from the behaviour described in
//! Pan Docs ("Power-Up Sequence", "The Cartridge Header", "Interrupts", "LCD Control",
//! "Rendering overview", "Audio Registers", "CGB Registers", "Palettes"); the splash is our
//! own 8x8 patterns, and there is no Nintendo logo. Both ROMs are plain SM83 programs, so each can
//! be read against a public opcode table.
//!
//! What it does, in order: sets `SP` to the top of HRAM, turns the LCD off and clears VRAM and
//! HRAM, uploads one solid tile and our "GB" glyphs, draws two rules with the glyphs between them,
//! turns the LCD on, plays a two-note chime on pulse channel 1 while 55 VBlanks go by — the whole
//! run is 59.7 frames, one second, which is how long the splash stays readable and how long the
//! chime takes to fade, as on the mask ROM — verifies the cartridge header checksum over `$0134-$014C`, and either parks on
//! the splash (bad checksum) or writes `$FF50` as its last instruction and falls through into the
//! cartridge at `$0100` with the documented register file.
//!
//! It lives in the same 256-byte window as the mask ROM it replaces, so it may only address memory
//! through `LD (nn),A`, `LDH`, and the cart/VRAM/HRAM windows; the only data in the page is the
//! 32 bytes of glyph patterns at `$00C1`. The 27 bytes between the glyphs and the last two
//! instructions are NOPs, because the unmap cannot be followed by a jump: the next fetch would come
//! from the cartridge, not from here, so the ROM has to end where the cartridge begins.

use crate::machine::{BOOT_ROM_SIZE, CGB_BOOT_ROM_SIZE};

/// The program, byte for byte. Every range below names its purpose and its Pan Docs section.
///
/// Pan Docs, "Power-Up Sequence" — "Monochrome models (DMG0, DMG, MGB)": the mask ROM runs from
/// `$0000` with interrupts disabled and the stack at the top of HRAM, before the cartridge is
/// allowed to execute.
pub const BUILT_IN: [u8; BOOT_ROM_SIZE] = [
    // $0000 — Interrupts off: the boot ROM owns the CPU until it hands over.
    // Pan Docs, "Power-Up Sequence": the CPU starts with IME cleared; DI makes that explicit.
    0xF3, // DI
    // $0001-$0003 — Stack pointer at the top of HRAM.
    // Pan Docs, "Power-Up Sequence" — "CPU registers": SP=$FFFE after hand-off.
    0x31, 0xFE, 0xFF, // LD SP,$FFFE
    // $0004-$0006 — LCD off, so the VRAM rewrite below is not competing with the PPU.
    // Pan Docs, "LCD Control" — "LCDC.7 — LCD enable": 0 = Off; "Rendering overview" —
    // "Accessing VRAM and OAM": while the LCD is off, VRAM accepts every write.
    0xAF, // XOR A
    0xE0, 0x40, // LDH ($40),A — LCDC=0
    // $0007-$0013 — Clear all 8192 bytes of VRAM ($8000-$9FFF).
    // Pan Docs, "Power-Up Sequence" — "Monochrome models": the mask ROM clears VRAM before it
    // draws anything. The loop ends with A=0, which the HRAM clear below relies on.
    0x21, 0x00, 0x80, // LD HL,$8000
    0x01, 0x00, 0x20, // LD BC,$2000 — 8192 bytes
    0xAF, // XOR A (clr:)
    0x22, // LD (HL+),A
    0x0B, // DEC BC
    0x78, // LD A,B
    0xB1, // OR C
    0x20, 0xF9, // JR NZ,clr ($000D)
    // $0014-$001C — Clear the 127 bytes of HRAM ($FF80-$FFFE) the mask ROM leaves at zero.
    // Pan Docs, "Power-Up Sequence" — "Console state after boot ROM hand-off": the cartridge may
    // not rely on HRAM contents; we hand it zeroes, matching the no-boot path in `Machine::new`.
    0x21, 0x80, 0xFF, // LD HL,$FF80
    0x0E, 0x7F, // LD C,$7F — 127 bytes
    0x22, // LD (HL+),A (hclr:)
    0x0D, // DEC C
    0x20, 0xFC, // JR NZ,hclr ($0019)
    // $001D-$0027 — Upload tile 1 (index 1, at $8010): a solid 8x8 block, our rule colour.
    // Pan Docs, "Rendering overview" — tile data lives at $8000-$97FF; LCDC.4=1 selects it.
    0x21, 0x10, 0x80, // LD HL,$8010 — tile 1
    0x3E, 0xFF, // LD A,$FF
    0x06, 0x10, // LD B,16 — 16 bytes = 8 rows of two bitplanes
    0x22, // LD (HL+),A (ts:)
    0x05, // DEC B
    0x20, 0xFC, // JR NZ,ts ($0024)
    // $0028-$0035 — Upload tiles 3 and 4 (at $8030-$804F): our own "G" and "B", 8x8, one bit
    // plane each, copied from the glyph table at $00C1. Rows are [pattern, $00]; BGP=$FC maps
    // colour 1 to the darkest shade. Pan Docs, "Rendering overview" — tile data layout.
    0x21, 0x30, 0x80, // LD HL,$8030 — tiles 3 and 4
    0x11, 0xC1, 0x00, // LD DE,$00C1 — the glyph data
    0x06, 0x20, // LD B,32 — two tiles
    0x1A, // LD A,(DE) (tp:)
    0x22, // LD (HL+),A
    0x13, // INC DE
    0x05, // DEC B
    0x20, 0xFA, // JR NZ,tp ($0030)
    // $0036-$0040 — Draw the top rule: tile 1 across row 6 of the $9800 map (20 visible tiles).
    // Pan Docs, "Rendering overview" — "Background map": 32x32 bytes at $9800, 20x18 visible.
    0x3E, 0x01, // LD A,$01 — tile 1
    0x21, 0xC0, 0x98, // LD HL,$98C0 — row 6, column 0
    0x06, 0x14, // LD B,20
    0x22, // LD (HL+),A (ln1:)
    0x05, // DEC B
    0x20, 0xFC, // JR NZ,ln1 ($003D)
    // $0041-$0049 — Draw the bottom rule: tile 1 across row 11.
    0x21, 0x60, 0x99, // LD HL,$9960 — row 11, column 0
    0x06, 0x14, // LD B,20
    0x22, // LD (HL+),A (ln2:)
    0x05, // DEC B
    0x20, 0xFC, // JR NZ,ln2 ($0046)
    // $004A-$0052 — Place "G" (tile 3) at column 8 and "B" (tile 4) at column 10 of row 8.
    0x21, 0x08, 0x99, // LD HL,$9908 — row 8, column 8
    0x3E, 0x03, // LD A,$03 — tile 3
    0x22, // LD (HL+),A — "G"
    0x23, // INC HL — leave column 9 blank
    0x3C, // INC A — tile 4
    0x77, // LD (HL),A — "B"
    // $0053-$0056 — LCD on: BG on, tile data at $8000, map at $9800, 8x8 OBJs.
    // Pan Docs, "LCD Control" — LCDC.7=1, LCDC.4=1, LCDC.3=0, LCDC.0=1 → $91. BGP ($FC) and
    // SCX/SCY are already at their post-boot values in `Ppu::new`.
    0x3E, 0x91, // LD A,$91
    0xE0, 0x40, // LDH ($40),A
    // $0057-$0062 — Power the APU on. Writing NR52 with bit 7 set resets every NR register.
    // Then master volume and routing: channel 1 to both outputs.
    // Pan Docs, "Audio Registers" — "FF26 — NR52", "FF24 — NR50", "FF25 — NR51".
    0x3E, 0x80, // LD A,$80 — NR52: APU on
    0xE0, 0x26, // LDH ($26),A
    0x3E, 0x77, // LD A,$77 — NR50: both sides at 7
    0xE0, 0x24, // LDH ($24),A
    0x3E, 0x11, // LD A,$11 — NR51: channel 1 left and right
    0xE0, 0x25, // LDH ($25),A
    // $0063-$006D — Pulse channel 1: no sweep, 50% duty, volume 15 with a fading envelope.
    // Pan Docs, "Audio Registers" — "FF10 — NR10", "FF11 — NR11", "FF12 — NR12".
    0xAF, // XOR A
    0xE0, 0x10, // LDH ($10),A — NR10: no sweep
    0x3E, 0x80, // LD A,$80 — duty 2 (50%)
    0xE0, 0x11, // LDH ($11),A — NR11
    // Period 2: the envelope steps at 64 Hz, so 15 steps is 0.47 s — shorter than either note's
    // share of the hold, which is what lets each note fade to zero on its own rather than being
    // cut off at the hand-over.
    0x3E, 0xF2, // LD A,$F2 — volume 15, decreasing, period 2
    0xE0, 0x12, // LDH ($12),A — NR12
    // $006E-$0075 — First note, C4 (period $060B): low byte, then trigger + high bits.
    // Pan Docs, "Audio Registers" — "FF13 — NR13", "FF14 — NR14": bit 7 triggers the channel.
    0x3E, 0x0B, // LD A,$0B
    0xE0, 0x13, // LDH ($13),A — NR13
    0x3E, 0x86, // LD A,$86 — trigger, period high $06
    0xE0, 0x14, // LDH ($14),A — NR14
    // $0076-$0084 — Hold the splash and let the first note ring: clear the request, poll IF bit 0
    // until the next VBlank, and count frames down in B until 20 have gone by. The LCD is on, so
    // one wait is one frame (70 224 dots, 17 556 machine cycles), and the note is heard for those
    // twenty. Writing IF is
    // how the DMG mask ROM acknowledges interrupts; IME stays off, so nothing is serviced.
    // Pan Docs, "Interrupts" — "FF0F — IF": bit 0 is the VBlank request, write-1-to-clear.
    // B is free here — the hand-over's B=$00 is loaded at $00B3 — so the frame counter lives in it.
    0x06, 0x14, // LD B,$14 — 20 frames for the first note
    0x3E, 0xFE, // LD A,$FE — every bit but VBlank set (f1:)
    0xE0, 0x0F, // LDH ($0F),A — IF: clear the VBlank request
    0xF0, 0x0F, // LDH A,($0F) (poll1:)
    0xE6, 0x01, // AND $01 — VBlank requested?
    0x28, 0xFA, // JR Z,poll1 ($007C)
    0x05, // DEC B — one frame gone
    0x20, 0xF3, // JR NZ,f1 ($0078)
    // $0085-$008C — Second note, E4 (period $0672): retrigger channel 1 a fourth above, now that
    // the first has had its twenty frames. Retriggering reloads the envelope to volume 15, so the
    // second note starts loud again and fades on its own.
    // Pan Docs, "Audio Registers" — "FF13 — NR13", "FF14 — NR14".
    0x3E, 0x72, // LD A,$72
    0xE0, 0x13, // LDH ($13),A — NR13
    0x3E, 0x86, // LD A,$86 — trigger, period high $06
    0xE0, 0x14, // LDH ($14),A — NR14
    // $008D-$009B — The rest of the hold: 35 more frames, so the splash is up for 55 in all and
    // the whole boot is 59.7 frames — one second, as the DMG mask ROM's own splash is — and the
    // second note has time to play out completely before the cartridge starts.
    // Pan Docs, "Interrupts" — "FF0F — IF".
    0x06, 0x23, // LD B,$23 — 35 frames for the second note
    0x3E, 0xFE, // LD A,$FE (f2:)
    0xE0, 0x0F, // LDH ($0F),A — IF: clear the VBlank request
    0xF0, 0x0F, // LDH A,($0F) (poll2:)
    0xE6, 0x01, // AND $01
    0x28, 0xFA, // JR Z,poll2 ($0093)
    0x05, // DEC B
    0x20, 0xF3, // JR NZ,f2 ($008F)
    // $009C-$00A7 — Header checksum over $0134-$014C: A = A - [HL] - 1 per byte, 25 bytes.
    // Pan Docs, "The Cartridge Header" — "014D — Header checksum": the ROM must compute this and
    // compare it with the byte the cartridge stores at $014D.
    0x21, 0x34, 0x01, // LD HL,$0134
    0x06, 0x19, // LD B,25 — $0134..$014C inclusive
    0xAF, // XOR A
    0x96, // SUB (HL) (ck:)
    0x3D, // DEC A
    0x23, // INC HL
    0x05, // DEC B
    0x20, 0xFA, // JR NZ,ck ($00A2)
    // $00A8-$00AD — Compare with $014D; a mismatch parks on the splash instead of starting the
    // cartridge, which is what the DMG mask ROM does.
    // Pan Docs, "The Cartridge Header" — "014D — Header checksum".
    0x21, 0x4D, 0x01, // LD HL,$014D
    0xBE, // CP (HL)
    0x20, 0x11, // JR NZ,park ($00BF)
    // $00AE-$00B2 — Load the documented AF through the stack: PUSH $01B0, POP AF. POP AF takes the
    // flag byte verbatim, so F=$B0 — including Z=1 with A=$01 — which no ALU instruction can do.
    // Pan Docs, "Power-Up Sequence" — "CPU registers": A=$01, F=$B0 (checksum byte non-zero),
    // B=$00, C=$13, D=$00, E=$D8, H=$01, L=$4D, SP=$FFFE. The PUSH/POP pair balances, so SP is
    // still $FFFE from $0001.
    0x21, 0xB0, 0x01, // LD HL,$01B0 — $01 high byte, $B0 flag byte
    0xE5, // PUSH HL
    0xF1, // POP AF — A=$01, F=$B0
    // $00B3-$00BB — The rest of the register file.
    // Pan Docs, "Power-Up Sequence" — "Console state after boot ROM hand-off" → "CPU registers".
    0x01, 0x13, 0x00, // LD BC,$0013 — B=$00, C=$13
    0x11, 0xD8, 0x00, // LD DE,$00D8 — D=$00, E=$D8
    0x21, 0x4D, 0x01, // LD HL,$014D — H=$01, L=$4D
    // $00BC-$00BE — Skip the park loop and the glyph data: the unmap must be the last instruction.
    0xC3, 0xFC, 0x00, // JP $00FC
    // $00BF-$00C0 — Park: a failed checksum keeps the splash on screen forever.
    0x18, 0xFE, // JR $ (park:)
    // $00C1-$00E0 — Glyph data, our own 8x8 designs: "G" then "B", one bit plane per row.
    // Reached only as data (LD DE,$00C1), never executed; Pan Docs, "Rendering overview" — tile
    // data is 16 bytes per tile, low byte then high byte for each of 8 rows.
    0x3C, 0x00, 0x42, 0x00, 0x80, 0x00, 0x80, 0x00, // G rows 0-3
    0x8E, 0x00, 0x82, 0x00, 0x42, 0x00, 0x3C, 0x00, // G rows 4-7
    0xFC, 0x00, 0x82, 0x00, 0x82, 0x00, 0xFC, 0x00, // B rows 0-3
    0x82, 0x00, 0x82, 0x00, 0x82, 0x00, 0xFC, 0x00, // B rows 4-7
    // $00E1-$00FB — Alignment padding: 27 NOPs, executed on the way to the last instruction. The
    // mask ROM ends with its unmap at $00FE-$00FF and falls through into $0100; so does this one,
    // because the unmap takes effect for the very next fetch. Pan Docs, "Power-Up Sequence" —
    // "FF50 — BANK: Boot ROM disable" (DMG: any non-zero value).
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, //
    // $00FC-$00FD — A=$01, already true from POP AF, written again so the last instructions read
    // like the mask ROM's ending. Pan Docs, "Power-Up Sequence" — "CPU registers": A=$01.
    0x3E, 0x01, // LD A,$01
    // $00FE-$00FF — Unmap and leave: PC becomes $0100 on the next fetch, which is the cartridge
    // entry point. Pan Docs, "The Cartridge Header" — "0100-0103 — Entry point".
    0xE0, 0x50, // LDH ($50),A
];

/// The DMG page as the 256-byte window the hardware maps at `$0000`. The program fills the window:
/// its last instruction is the unmap at `$00FE-$00FF`, so the handover to `$0100` is a fall-through.
pub fn image() -> Vec<u8> {
    BUILT_IN.to_vec()
}

/// The colour page, byte for byte. The Game Boy Color's boot ROM is **2048 bytes**, and the bus
/// maps it as **two windows**: `$0000-$00FF` and `$0200-$08FF`, with the cartridge header visible
/// in the gap at `$0100-$01FF` (Pan Docs, *Power-Up Sequence*, "Color models (CGB)"). The array is
/// laid out to match: byte `n` is the byte the CPU fetches at `$n` for `n < $0100`, and at
/// `$n + $0100` for `$0100 <= n < $0800` — the header gap holds no byte of this ROM.
///
/// Every range below names its purpose and its Pan Docs section. The program, in order:
///
/// 1. Turns interrupts off, sets `SP` to the top of HRAM, turns the LCD off and clears VRAM.
/// 2. Uploads our own `GB` glyphs (the same patterns the DMG page uses) and a solid rule tile, and
///    draws the splash — a rule above and below, the mark between — in the colour palette.
/// 3. Plays a two-note chime on pulse channel 1 while the mark **scrolls** left across the splash,
///    one pixel per VBlank for 90 VBlanks (1.5 s), so a headless run can prove it moved.
/// 4. Verifies the cartridge header checksum over `$0134-$014C` (the header is visible in the gap).
/// 5. On success: fades the background palette to white over eight VBlanks, loads the CGB register
///    file Pan Docs' *Power-Up Sequence* gives (`A=$11`, `F=$80`, `B=$00`, `C=$00`, `D=$FF`,
///    `E=$56`, `H=$00`, `L=$0D`, `SP=$FFFE`), writes `$FF50` and falls through to `$0100`.
/// 6. On a bad checksum: parks on the splash, and never reaches `$0100`.
///
/// Because the header is visible at `$0100-$01FF`, the first window cannot fall through into the
/// second: it ends in `JP $0200`, and the program's body lives in the second window. The unmap
/// tail lives at `$00FC-$00FF` of the first window, so the hand-over is a fall-through into
/// `$0100` exactly as the DMG page's is.
pub const BUILT_IN_CGB: [u8; CGB_BOOT_ROM_SIZE] = {
    // The two windows' unused bytes are NOP padding (`$00`) and are never executed; every byte the
    // program uses is assigned below.
    let mut rom = [0x00u8; CGB_BOOT_ROM_SIZE];

    // ================= first window: $0000-$00FF =================
    // $0000 — interrupts off. Pan Docs, "Power-Up Sequence": the CPU starts with IME cleared; DI
    // makes that explicit.
    rom[0x000] = 0xF3; // DI
    // $0001-$0003 — SP at the top of HRAM.
    // Pan Docs, "Power-Up Sequence" — "CPU registers": SP=$FFFE after hand-off.
    rom[0x001] = 0x31; // LD SP,$FFFE
    rom[0x002] = 0xFE;
    rom[0x003] = 0xFF;
    // $0004 — A=0, reused by the VRAM clear below and the checksum later.
    rom[0x004] = 0xAF; // XOR A
    // $0005-$0006 — LCD off, so the VRAM rewrite is not competing with the PPU.
    // Pan Docs, "LCD Control" — "LCDC.7 — LCD enable": 0 = Off.
    rom[0x005] = 0xE0; // LDH ($40),A
    rom[0x006] = 0x40;
    // $0007-$0009 — into the second window. The header is visible at $0100-$01FF, so the page
    // cannot fall through past $00FF: it has to jump over the cartridge's own bytes.
    rom[0x007] = 0xC3; // JP $0200
    rom[0x008] = 0x00;
    rom[0x009] = 0x02;
    // $000A-$00FB — NOP padding (left $00), never executed.
    // $00FC-$00FD — A=$11, the value a cartridge reads to learn it is on a CGB.
    // Pan Docs, "CGB Registers" — "Detecting CGB (and GBA) functions": A=$11, and B's bit 0
    // distinguishes a CGB (0) from a GBA (1).
    rom[0x0FC] = 0x3E; // LD A,$11
    rom[0x0FD] = 0x11;
    // $00FE-$00FF — unmap and leave: PC becomes $0100 on the next fetch, the cartridge entry.
    // Pan Docs, "Power-Up Sequence" — "FF50 — BANK": any non-zero value disables the boot ROM.
    rom[0x0FE] = 0xE0; // LDH ($50),A
    rom[0x0FF] = 0x50;

    // ================= second window: $0200-$08FF =================
    // The gap at $0100-$01FF is the cartridge's; these bytes live at rom offset $0100 and up,
    // which is CPU $0200 and up.
    //
    // $0200-$020B — clear all 8192 bytes of VRAM bank 0 ($8000-$9FFF), one zero at a time. The
    // loop counts by its own address: `LD A,H` then `CP $A0` ends it the moment H reaches $A0,
    // which is exactly when HL has stepped past $9FFF. A is re-zeroed every iteration, because the
    // loop's `LD A,H` would otherwise leave the counter in it and write garbage after the first
    // byte. The two NOPs keep tile 1 at $020C, so the rest of the page's addresses are unchanged.
    // Pan Docs, "Power-Up Sequence": the colour ROM clears VRAM before it draws anything.
    rom[0x100] = 0x21; // LD HL,$8000
    rom[0x101] = 0x00;
    rom[0x102] = 0x80;
    rom[0x103] = 0xAF; // XOR A (clear:)
    rom[0x104] = 0x22; // LD (HL+),A
    rom[0x105] = 0x7C; // LD A,H
    rom[0x106] = 0xFE; // CP $A0 — past the end of VRAM?
    rom[0x107] = 0xA0;
    rom[0x108] = 0x20; // JR NZ,clear ($0203)
    rom[0x109] = 0xF9;
    rom[0x10A] = 0x00; // NOP — see the note above
    rom[0x10B] = 0x00; // NOP
    // $020C-$0216 — tile 1 (index 1, at $8010): a solid 8x8 block, our rule colour (shade 3).
    // Pan Docs, "Rendering overview" — tile data lives at $8000-$97FF; LCDC.4=1 selects it.
    rom[0x10C] = 0x21; // LD HL,$8010
    rom[0x10D] = 0x10;
    rom[0x10E] = 0x80;
    rom[0x10F] = 0x3E; // LD A,$FF
    rom[0x110] = 0xFF;
    rom[0x111] = 0x06; // LD B,16
    rom[0x112] = 0x10;
    rom[0x113] = 0x22; // LD (HL+),A (tile1:)
    rom[0x114] = 0x05; // DEC B
    rom[0x115] = 0x20; // JR NZ,tile1 ($0213)
    rom[0x116] = 0xFC;
    // $0217-$0224 — tiles 3 and 4 (at $8030-$804F): our own "G" and "B", 8x8, one bit plane each,
    // copied from the glyph table at $02E3. Shade 0 is palette colour 0 (white), shade 1 is colour 1.
    rom[0x117] = 0x21; // LD HL,$8030
    rom[0x118] = 0x30;
    rom[0x119] = 0x80;
    rom[0x11A] = 0x11; // LD DE,$02E3 — the glyph data
    rom[0x11B] = 0xE3;
    rom[0x11C] = 0x02;
    rom[0x11D] = 0x06; // LD B,32
    rom[0x11E] = 0x20;
    rom[0x11F] = 0x1A; // LD A,(DE) (tiles:)
    rom[0x120] = 0x22; // LD (HL+),A
    rom[0x121] = 0x13; // INC DE
    rom[0x122] = 0x05; // DEC B
    rom[0x123] = 0x20; // JR NZ,tiles ($021F)
    rom[0x124] = 0xFA;
    // $0225-$0233 — write BG palette 0 through BCPS/BCPD: index 0 with auto-increment, then the
    // eight bytes at $0303 (white, blue, white, green). Pan Docs, "Palettes" — "FF68/FF69".
    rom[0x125] = 0x3E; // LD A,$80 — BCPS: index 0, auto-increment
    rom[0x126] = 0x80;
    rom[0x127] = 0xE0; // LDH ($68),A
    rom[0x128] = 0x68;
    rom[0x129] = 0x21; // LD HL,$0303 — the palette data
    rom[0x12A] = 0x03;
    rom[0x12B] = 0x03;
    rom[0x12C] = 0x06; // LD B,8
    rom[0x12D] = 0x08;
    rom[0x12E] = 0x2A; // LD A,(HL+) (palette:)
    rom[0x12F] = 0xE0; // LDH ($69),A
    rom[0x130] = 0x69;
    rom[0x131] = 0x05; // DEC B
    rom[0x132] = 0x20; // JR NZ,palette ($022E)
    rom[0x133] = 0xFA;
    // $0234-$0247 — the two rules (tile 1) across all 32 columns of rows 6 and 11, so a
    // horizontal scroll never reveals a gap in the rules. Pan Docs, "Rendering overview" —
    // "Background map": 32x32 bytes at $9800.
    rom[0x134] = 0x3E; // LD A,$01 — tile 1
    rom[0x135] = 0x01;
    rom[0x136] = 0x21; // LD HL,$98C0 — row 6, column 0
    rom[0x137] = 0xC0;
    rom[0x138] = 0x98;
    rom[0x139] = 0x06; // LD B,32
    rom[0x13A] = 0x20;
    rom[0x13B] = 0x22; // LD (HL+),A (rule1:)
    rom[0x13C] = 0x05; // DEC B
    rom[0x13D] = 0x20; // JR NZ,rule1 ($023B)
    rom[0x13E] = 0xFC;
    rom[0x13F] = 0x21; // LD HL,$99C0 — row 11, column 0
    rom[0x140] = 0xC0;
    rom[0x141] = 0x99;
    rom[0x142] = 0x06; // LD B,32
    rom[0x143] = 0x20;
    rom[0x144] = 0x22; // LD (HL+),A (rule2:)
    rom[0x145] = 0x05; // DEC B
    rom[0x146] = 0x20; // JR NZ,rule2 ($0244)
    rom[0x147] = 0xFC;
    // $0248-$0250 — "G" (tile 3) at column 8 and "B" (tile 4) at column 10 of row 8. The mark
    // sits at map columns 8-10, so a 1-pixel-per-frame SCX scroll glides it left across the splash.
    rom[0x148] = 0x21; // LD HL,$9908 — row 8, column 8
    rom[0x149] = 0x08;
    rom[0x14A] = 0x99;
    rom[0x14B] = 0x3E; // LD A,$03 — tile 3
    rom[0x14C] = 0x03;
    rom[0x14D] = 0x22; // LD (HL+),A — "G"
    rom[0x14E] = 0x23; // INC HL — leave column 9 blank
    rom[0x14F] = 0x3C; // INC A — tile 4
    rom[0x150] = 0x77; // LD (HL),A — "B"
    // $0251-$0254 — LCD on: BG on, tile data at $8000, map at $9800, 8x8 -> $91.
    // Pan Docs, "LCD Control" — LCDC.7=1, LCDC.4=1, LCDC.0=1. In CGB mode the BG is always drawn
    // and LCDC.0 only sets its priority over objects, so $91 is the colour page's value too.
    rom[0x151] = 0x3E; // LD A,$91
    rom[0x152] = 0x91;
    rom[0x153] = 0xE0; // LDH ($40),A
    rom[0x154] = 0x40;
    // $0255-$026B — power the APU on and route channel 1 to both outputs, then pulse channel 1:
    // no sweep, 50% duty, volume 15 with a fading envelope. The same settings as the DMG page.
    // Pan Docs, "Audio Registers" — NR52/NR50/NR51/NR10/NR11/NR12.
    rom[0x155] = 0x3E; // LD A,$80 — NR52: APU on
    rom[0x156] = 0x80;
    rom[0x157] = 0xE0; // LDH ($26),A
    rom[0x158] = 0x26;
    rom[0x159] = 0x3E; // LD A,$77 — NR50: both sides at 7
    rom[0x15A] = 0x77;
    rom[0x15B] = 0xE0; // LDH ($24),A
    rom[0x15C] = 0x24;
    rom[0x15D] = 0x3E; // LD A,$11 — NR51: channel 1 left and right
    rom[0x15E] = 0x11;
    rom[0x15F] = 0xE0; // LDH ($25),A
    rom[0x160] = 0x25;
    rom[0x161] = 0xAF; // XOR A
    rom[0x162] = 0xE0; // LDH ($10),A — NR10: no sweep
    rom[0x163] = 0x10;
    rom[0x164] = 0x3E; // LD A,$80 — duty 2 (50%)
    rom[0x165] = 0x80;
    rom[0x166] = 0xE0; // LDH ($11),A — NR11
    rom[0x167] = 0x11;
    rom[0x168] = 0x3E; // LD A,$F2 — volume 15, decreasing, period 2
    rom[0x169] = 0xF2;
    rom[0x16A] = 0xE0; // LDH ($12),A — NR12
    rom[0x16B] = 0x12;
    // $026C-$0273 — first note, C4 (period $060B): low byte, then trigger + high bits.
    // Pan Docs, "Audio Registers" — NR13/NR14: bit 7 of NR14 triggers the channel.
    rom[0x16C] = 0x3E; // LD A,$0B
    rom[0x16D] = 0x0B;
    rom[0x16E] = 0xE0; // LDH ($13),A — NR13
    rom[0x16F] = 0x13;
    rom[0x170] = 0x3E; // LD A,$86 — trigger, period high $06
    rom[0x171] = 0x86;
    rom[0x172] = 0xE0; // LDH ($14),A — NR14
    rom[0x173] = 0x14;
    // $0274-$0287 — animation part 1: 45 VBlanks. Each VBlank: clear the request, poll IF bit 0,
    // then step SCX up by one, so the mark glides one pixel left per frame. The first note rings
    // for those 45 frames. B is the frame counter. Pan Docs, "Interrupts" — "FF0F — IF": bit 0 is
    // the VBlank request; the page acknowledges by writing a value with bit 0 clear (IME stays off,
    // so nothing is serviced).
    rom[0x174] = 0x06; // LD B,45 — 45 frames for the first note
    rom[0x175] = 0x2D;
    rom[0x176] = 0x3E; // LD A,$FE (anim1:)
    rom[0x177] = 0xFE;
    rom[0x178] = 0xE0; // LDH ($0F),A — clear the VBlank request
    rom[0x179] = 0x0F;
    rom[0x17A] = 0xF0; // LDH A,($0F) (poll1:)
    rom[0x17B] = 0x0F;
    rom[0x17C] = 0xE6; // AND $01 — VBlank requested?
    rom[0x17D] = 0x01;
    rom[0x17E] = 0x28; // JR Z,poll1 ($027A)
    rom[0x17F] = 0xFA;
    rom[0x180] = 0xF0; // LDH A,($43) — SCX
    rom[0x181] = 0x43;
    rom[0x182] = 0x3C; // INC A
    rom[0x183] = 0xE0; // LDH ($43),A
    rom[0x184] = 0x43;
    rom[0x185] = 0x05; // DEC B — one frame gone
    rom[0x186] = 0x20; // JR NZ,anim1 ($0276)
    rom[0x187] = 0xEE;
    // $0288-$028F — second note, E4 (period $0672): retrigger channel 1 a fourth above.
    rom[0x188] = 0x3E; // LD A,$72
    rom[0x189] = 0x72;
    rom[0x18A] = 0xE0; // LDH ($13),A — NR13
    rom[0x18B] = 0x13;
    rom[0x18C] = 0x3E; // LD A,$86 — trigger, period high $06
    rom[0x18D] = 0x86;
    rom[0x18E] = 0xE0; // LDH ($14),A — NR14
    rom[0x18F] = 0x14;
    // $0290-$02A3 — animation part 2: 45 more VBlanks, one more SCX pixel each, for the second
    // note. 90 VBlanks in all: 1.5 s of scroll.
    rom[0x190] = 0x06; // LD B,45 — 45 frames for the second note
    rom[0x191] = 0x2D;
    rom[0x192] = 0x3E; // LD A,$FE (anim2:)
    rom[0x193] = 0xFE;
    rom[0x194] = 0xE0; // LDH ($0F),A
    rom[0x195] = 0x0F;
    rom[0x196] = 0xF0; // LDH A,($0F) (poll2:)
    rom[0x197] = 0x0F;
    rom[0x198] = 0xE6; // AND $01
    rom[0x199] = 0x01;
    rom[0x19A] = 0x28; // JR Z,poll2 ($0296)
    rom[0x19B] = 0xFA;
    rom[0x19C] = 0xF0; // LDH A,($43)
    rom[0x19D] = 0x43;
    rom[0x19E] = 0x3C; // INC A
    rom[0x19F] = 0xE0; // LDH ($43),A
    rom[0x1A0] = 0x43;
    rom[0x1A1] = 0x05; // DEC B
    rom[0x1A2] = 0x20; // JR NZ,anim2 ($0292)
    rom[0x1A3] = 0xEE;
    // $02A4-$02AF — header checksum over $0134-$014C: A = A - [HL] - 1 per byte, 25 bytes. The
    // header is visible in the gap, so these reads come from the cartridge.
    // Pan Docs, "The Cartridge Header" — "014D — Header checksum".
    rom[0x1A4] = 0x21; // LD HL,$0134
    rom[0x1A5] = 0x34;
    rom[0x1A6] = 0x01;
    rom[0x1A7] = 0x06; // LD B,25 — $0134..$014C inclusive
    rom[0x1A8] = 0x19;
    rom[0x1A9] = 0xAF; // XOR A
    rom[0x1AA] = 0x96; // SUB (HL) (ck:)
    rom[0x1AB] = 0x3D; // DEC A
    rom[0x1AC] = 0x23; // INC HL
    rom[0x1AD] = 0x05; // DEC B
    rom[0x1AE] = 0x20; // JR NZ,ck ($02AA)
    rom[0x1AF] = 0xFA;
    // $02B0-$02B5 — compare with $014D; a mismatch parks on the splash instead of starting the
    // cartridge. Pan Docs, "The Cartridge Header" — "014D — Header checksum".
    rom[0x1B0] = 0x21; // LD HL,$014D
    rom[0x1B1] = 0x4D;
    rom[0x1B2] = 0x01;
    rom[0x1B3] = 0xBE; // CP (HL)
    rom[0x1B4] = 0x20; // JR NZ,park ($02E1)
    rom[0x1B5] = 0x2B;
    // $02B6-$02D3 — fade BG palette 0 to white: eight steps, one per VBlank, each writing the
    // full eight-byte palette from the table at $030B. Waiting for VBlank first keeps every
    // palette write in mode 1, where CRAM is free. Pan Docs, "Palettes" — "FF68/FF69".
    rom[0x1B6] = 0x21; // LD HL,$030B — the fade table
    rom[0x1B7] = 0x0B;
    rom[0x1B8] = 0x03;
    rom[0x1B9] = 0x06; // LD B,8 — eight steps
    rom[0x1BA] = 0x08;
    rom[0x1BB] = 0x3E; // LD A,$FE (fade:)
    rom[0x1BC] = 0xFE;
    rom[0x1BD] = 0xE0; // LDH ($0F),A
    rom[0x1BE] = 0x0F;
    rom[0x1BF] = 0xF0; // LDH A,($0F) (fade_wait:)
    rom[0x1C0] = 0x0F;
    rom[0x1C1] = 0xE6; // AND $01
    rom[0x1C2] = 0x01;
    rom[0x1C3] = 0x28; // JR Z,fade_wait ($02BF)
    rom[0x1C4] = 0xFA;
    rom[0x1C5] = 0x3E; // LD A,$80 — BCPS: index 0, auto-increment
    rom[0x1C6] = 0x80;
    rom[0x1C7] = 0xE0; // LDH ($68),A
    rom[0x1C8] = 0x68;
    rom[0x1C9] = 0x0E; // LD C,8 — eight bytes
    rom[0x1CA] = 0x08;
    rom[0x1CB] = 0x2A; // LD A,(HL+) (fade_byte:)
    rom[0x1CC] = 0xE0; // LDH ($69),A — BCPD
    rom[0x1CD] = 0x69;
    rom[0x1CE] = 0x0D; // DEC C
    rom[0x1CF] = 0x20; // JR NZ,fade_byte ($02CB)
    rom[0x1D0] = 0xFA;
    rom[0x1D1] = 0x05; // DEC B
    rom[0x1D2] = 0x20; // JR NZ,fade ($02BB)
    rom[0x1D3] = 0xE7;
    // $02D4-$02E0 — the CGB register file, then into the unmap tail. B=$00 C=$00 D=$FF E=$56
    // H=$00 L=$0D; XOR A gives A=0 with F=$80 (Z set, everything else clear), the flag byte a CGB
    // cartridge is handed. Pan Docs, "Power-Up Sequence" — "CPU registers" for a colour boot.
    rom[0x1D4] = 0x01; // LD BC,$0000
    rom[0x1D5] = 0x00;
    rom[0x1D6] = 0x00;
    rom[0x1D7] = 0x11; // LD DE,$FF56
    rom[0x1D8] = 0x56;
    rom[0x1D9] = 0xFF;
    rom[0x1DA] = 0x21; // LD HL,$000D
    rom[0x1DB] = 0x0D;
    rom[0x1DC] = 0x00;
    rom[0x1DD] = 0xAF; // XOR A — A=0, F=$80
    rom[0x1DE] = 0xC3; // JP $00FC — the unmap tail in the first window
    rom[0x1DF] = 0xFC;
    rom[0x1E0] = 0x00;
    // $02E1-$02E2 — park: a failed checksum loops here forever, the splash still on screen. Reached
    // only by the JR NZ at $02B4, never on the success path.
    rom[0x1E1] = 0x18; // JR $ (park:)
    rom[0x1E2] = 0xFE;
    // $02E3-$0302 — glyph data, our own 8x8 designs: "G" then "B", one bit plane per row, the same
    // patterns the DMG page uses. Reached only as data (LD DE,$02E3), never executed.
    rom[0x1E3] = 0x3C;
    rom[0x1E4] = 0x00;
    rom[0x1E5] = 0x42;
    rom[0x1E6] = 0x00; // G rows 0-3
    rom[0x1E7] = 0x80;
    rom[0x1E8] = 0x00;
    rom[0x1E9] = 0x80;
    rom[0x1EA] = 0x00;
    rom[0x1EB] = 0x8E;
    rom[0x1EC] = 0x00;
    rom[0x1ED] = 0x82;
    rom[0x1EE] = 0x00; // G rows 4-7
    rom[0x1EF] = 0x42;
    rom[0x1F0] = 0x00;
    rom[0x1F1] = 0x3C;
    rom[0x1F2] = 0x00;
    rom[0x1F3] = 0xFC;
    rom[0x1F4] = 0x00;
    rom[0x1F5] = 0x82;
    rom[0x1F6] = 0x00; // B rows 0-3
    rom[0x1F7] = 0x82;
    rom[0x1F8] = 0x00;
    rom[0x1F9] = 0xFC;
    rom[0x1FA] = 0x00;
    rom[0x1FB] = 0x82;
    rom[0x1FC] = 0x00;
    rom[0x1FD] = 0x82;
    rom[0x1FE] = 0x00; // B rows 4-7
    rom[0x1FF] = 0x82;
    rom[0x200] = 0x00;
    rom[0x201] = 0xFC;
    rom[0x202] = 0x00;
    // $0303-$030A — BG palette 0, little-endian RGB555: white ($7FFF), blue ($7C00), white,
    // green ($03E0). Shade 0 -> white background, shade 1 -> blue glyphs, shade 3 -> green rules.
    // Pan Docs, "Palettes" — a colour is two bytes, red in bits 0-4.
    rom[0x203] = 0xFF;
    rom[0x204] = 0x7F; // white
    rom[0x205] = 0x00;
    rom[0x206] = 0x7C; // blue
    rom[0x207] = 0xFF;
    rom[0x208] = 0x7F; // white
    rom[0x209] = 0xE0;
    rom[0x20A] = 0x03; // green
    // $030B-$034A — the fade table: eight steps, each the full eight-byte BG palette 0, from the
    // start colours above to all-white. Precomputed so the SM83 page does no arithmetic: it only
    // copies. (White and the two non-white colours are interpolated toward $7FFF.)
    rom[0x20B] = 0xFF;
    rom[0x20C] = 0x7F;
    rom[0x20D] = 0x84;
    rom[0x20E] = 0x7C; // step 1
    rom[0x20F] = 0xFF;
    rom[0x210] = 0x7F;
    rom[0x211] = 0xE4;
    rom[0x212] = 0x13;
    rom[0x213] = 0xFF;
    rom[0x214] = 0x7F;
    rom[0x215] = 0x08;
    rom[0x216] = 0x7D; // step 2
    rom[0x217] = 0xFF;
    rom[0x218] = 0x7F;
    rom[0x219] = 0xE8;
    rom[0x21A] = 0x23;
    rom[0x21B] = 0xFF;
    rom[0x21C] = 0x7F;
    rom[0x21D] = 0x8C;
    rom[0x21E] = 0x7D; // step 3
    rom[0x21F] = 0xFF;
    rom[0x220] = 0x7F;
    rom[0x221] = 0xEC;
    rom[0x222] = 0x33;
    rom[0x223] = 0xFF;
    rom[0x224] = 0x7F;
    rom[0x225] = 0x10;
    rom[0x226] = 0x7E; // step 4
    rom[0x227] = 0xFF;
    rom[0x228] = 0x7F;
    rom[0x229] = 0xF0;
    rom[0x22A] = 0x43;
    rom[0x22B] = 0xFF;
    rom[0x22C] = 0x7F;
    rom[0x22D] = 0x73;
    rom[0x22E] = 0x7E; // step 5
    rom[0x22F] = 0xFF;
    rom[0x230] = 0x7F;
    rom[0x231] = 0xF3;
    rom[0x232] = 0x4F;
    rom[0x233] = 0xFF;
    rom[0x234] = 0x7F;
    rom[0x235] = 0xF7;
    rom[0x236] = 0x7E; // step 6
    rom[0x237] = 0xFF;
    rom[0x238] = 0x7F;
    rom[0x239] = 0xF7;
    rom[0x23A] = 0x5F;
    rom[0x23B] = 0xFF;
    rom[0x23C] = 0x7F;
    rom[0x23D] = 0x7B;
    rom[0x23E] = 0x7F; // step 7
    rom[0x23F] = 0xFF;
    rom[0x240] = 0x7F;
    rom[0x241] = 0xFB;
    rom[0x242] = 0x6F;
    rom[0x243] = 0xFF;
    rom[0x244] = 0x7F;
    rom[0x245] = 0xFF;
    rom[0x246] = 0x7F; // step 8 (white)
    rom[0x247] = 0xFF;
    rom[0x248] = 0x7F;
    rom[0x249] = 0xFF;
    rom[0x24A] = 0x7F;
    // $034B-$08FF — NOP padding (left $00), never executed.

    rom
};

/// The colour page as the 2048 bytes the bus maps at `$0000-$00FF` + `$0200-$08FF`.
pub fn image_cgb() -> Vec<u8> {
    BUILT_IN_CGB.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_fills_the_boot_window() {
        assert_eq!(BUILT_IN.len(), BOOT_ROM_SIZE);
        // The last instruction is the unmap, and there is no jump after it: PC leaves at $0100.
        assert_eq!(&BUILT_IN[0xFC..], &[0x3E, 0x01, 0xE0, 0x50]);
        // The park loop sits after the jump that skips it, so a failed checksum cannot reach $0100.
        assert_eq!(&BUILT_IN[0xBC..0xC1], &[0xC3, 0xFC, 0x00, 0x18, 0xFE]);
    }

    #[test]
    fn the_hold_loop_runs_for_about_a_second() {
        // 20 frames for the first note, then 35 for the second: 55 VBlanks, 0.92 s of splash.
        assert_eq!(&BUILT_IN[0x76..0x78], &[0x06, 0x14], "first counter");
        assert_eq!(&BUILT_IN[0x8D..0x8F], &[0x06, 0x23], "second counter");
        // Each `JR NZ` must jump back to its own `LD A,$FE`. Landing on the `LDH ($0F),A` instead
        // would write A=$01 to IF — the value the poll left in it — which keeps the VBlank bit set
        // and ends the next wait immediately, so the loop would count to zero without waiting.
        for (at, want) in [(0x83, 0x78), (0x9A, 0x8F)] {
            assert_eq!(BUILT_IN[at], 0x20, "JR NZ at ${at:04X}");
            let target = at + 2 - (0x100 - usize::from(BUILT_IN[at + 1]));
            assert_eq!(target, want, "JR NZ at ${at:04X} loops to ${want:04X}");
            assert_eq!(BUILT_IN[want], 0x3E, "and lands on LD A,$FE");
        }
    }

    #[test]
    fn the_rom_points_its_data_load_at_the_glyph_table() {
        // `LD DE,pat` at $002B has to name $00C1, where the first glyph byte lives.
        assert_eq!(&BUILT_IN[0x2B..0x2E], &[0x11, 0xC1, 0x00]);
        assert_eq!(&BUILT_IN[0xC1..0xC7], &[0x3C, 0x00, 0x42, 0x00, 0x80, 0x00]);
    }

    #[test]
    fn the_colour_program_fills_the_two_windows() {
        assert_eq!(BUILT_IN_CGB.len(), CGB_BOOT_ROM_SIZE);
        // First window: DI, SP, LCD off, then a jump over the header into the second window.
        assert_eq!(
            &BUILT_IN_CGB[0x00..0x0A],
            &[0xF3, 0x31, 0xFE, 0xFF, 0xAF, 0xE0, 0x40, 0xC3, 0x00, 0x02]
        );
        // The unmap tail at $00FC-$00FF: A=$11 then write $FF50, falling through to $0100.
        assert_eq!(&BUILT_IN_CGB[0xFC..0x100], &[0x3E, 0x11, 0xE0, 0x50]);
        // Park is a JR -2 (0x18 0xFE) at CPU $02E1 = rom offset $01E1.
        assert_eq!(&BUILT_IN_CGB[0x1E1..0x1E3], &[0x18, 0xFE]);
        // The header gap holds no byte of this ROM: rom offset $0100 is CPU $0200, the second
        // window's first instruction (LD HL,$8000).
        assert_eq!(&BUILT_IN_CGB[0x100..0x103], &[0x21, 0x00, 0x80]);
    }

    #[test]
    fn the_colour_program_scrolls_for_ninety_vblanks() {
        // Two counters of 45 frames each: rom offsets $0174 (note 1) and $0190 (note 2).
        assert_eq!(&BUILT_IN_CGB[0x174..0x176], &[0x06, 0x2D], "first counter");
        assert_eq!(&BUILT_IN_CGB[0x190..0x192], &[0x06, 0x2D], "second counter");
        // Each `JR NZ` loops back to its own `LD A,$FE`, so each VBlank is a real wait, not a
        // count-to-zero on a stale request (the same hazard the DMG test guards).
        for (jrnz, want) in [(0x186, 0x176), (0x1A2, 0x192)] {
            assert_eq!(BUILT_IN_CGB[jrnz], 0x20, "JR NZ at ${jrnz:04X}");
            let target = jrnz + 2 - (0x100 - usize::from(BUILT_IN_CGB[jrnz + 1]));
            assert_eq!(target, want, "JR NZ at ${jrnz:04X} loops to ${want:04X}");
            assert_eq!(BUILT_IN_CGB[want], 0x3E, "and lands on LD A,$FE");
        }
        // SCX is stepped between the two waits: LDH A,($43); INC A; LDH ($43),A.
        assert_eq!(
            &BUILT_IN_CGB[0x180..0x185],
            &[0xF0, 0x43, 0x3C, 0xE0, 0x43],
            "the SCX step"
        );
    }

    #[test]
    fn the_colour_program_points_at_its_glyphs_and_palettes() {
        // `LD DE,$02E3` loads the glyph table; `LD HL,$0303` the palette; `LD HL,$030B` the fade.
        assert_eq!(&BUILT_IN_CGB[0x11A..0x11D], &[0x11, 0xE3, 0x02]);
        assert_eq!(&BUILT_IN_CGB[0x129..0x12C], &[0x21, 0x03, 0x03]);
        assert_eq!(&BUILT_IN_CGB[0x1B6..0x1B9], &[0x21, 0x0B, 0x03]);
        // The first glyph byte is the G pattern's first row.
        assert_eq!(BUILT_IN_CGB[0x1E3], 0x3C);
    }
}
