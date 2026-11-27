//! The Game Boy Advance's shelf: cartridge, memory map and frame clock.
//!
//! What these prove is what the slice is allowed to claim. The cartridge's header is parsed and a
//! file that is not one is refused by name; every region of GBATEK's map answers, with the mirroring
//! and the open bus the hardware has; the frame clock counts 228 scanlines of 1232 cycles and puts
//! `DISPSTAT`'s VBlank flag on lines 160-226. The picture itself is `crate::ppu`'s, proved in
//! `tests/gba_video.rs`; here only the shape of the frame is pinned — 240x160, RGB555, the same
//! colour type the Game Boy Color hands the front end.
//!
//! The public test ROMs in `roms/test/gba/` are gitignored, so the one test that needs them is
//! `#[ignore]`d, the way `tests/acid2.rs` handles its references.

use tendy_gb::joypad::{Buttons, Shoulders};
use tendy_gb::system::System;
use tendy_gba::bus::{
    CYCLES_PER_FRAME, CYCLES_PER_LINE, EWRAM_BASE, IWRAM_BASE, LINES_PER_FRAME, OAM_BASE,
    PALETTE_BASE, ROM_BASE, ROM_WS1_BASE, ROM_WS2_BASE, SRAM_BASE, VRAM_BASE,
};
use tendy_gba::cart::Cartridge;
use tendy_gba::cpu;
use tendy_gba::{Effect, EffectState, Gba, HEIGHT, PIXELS, WIDTH, irq};

/// A cartridge whose code starts at `$080000C0`: the entry point is the usual `b`.
fn synth_cart(code: &[u32]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-TEST\0");
    rom[0xAC..0xB0].copy_from_slice(b"TST1");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    for (index, word) in code.iter().enumerate() {
        let at = 0xC0 + index * 4;
        rom[at..at + 4].copy_from_slice(&word.to_le_bytes());
    }
    // The complement check over `$0A0-$0BC` goes in the last header byte.
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom
}

/// A machine running `code`.
fn machine(code: &[u32]) -> Gba {
    Gba::new(Cartridge::new(synth_cart(code)).expect("a synthesized cartridge"))
}

/// `DISPSTAT` as the CPU reads it.
fn dispstat(gba: &mut Gba) -> u16 {
    gba.bus.read16(0x0400_0004)
}

/// `VCOUNT`.
fn vcount(gba: &mut Gba) -> u16 {
    gba.bus.read16(0x0400_0006)
}

#[test]
fn a_cartridge_says_where_to_start_and_who_it_is() {
    let cart = Cartridge::new(synth_cart(&[])).expect("a synthesized cartridge");
    assert_eq!(cart.entry_point(), 0x0800_00C0);
    assert_eq!(cart.header.title, "GBA-TEST");
    assert_eq!(cart.header.game_code, "TST1");
    assert_eq!(cart.header.maker, "00");
    assert_eq!(cart.header.rom_bytes, 0x200);
    assert!(cart.header.complement_ok);
}

#[test]
fn a_file_that_is_not_a_cartridge_is_refused_by_name() {
    // A Game Boy cartridge handed over as a Game Boy Advance one.
    let mut gb_rom = vec![0u8; 0x8000];
    gb_rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    let why = Cartridge::new(gb_rom).unwrap_err();
    assert!(why.contains("not a Game Boy Advance cartridge"), "{why}");
    assert!(
        why.contains("$08000000"),
        "the refusal names the byte: {why}"
    );

    // A text file, refused by size.
    let why = Cartridge::new(b"hello".to_vec()).unwrap_err();
    assert!(why.contains("5 bytes"), "{why}");
}

#[test]
fn the_machine_boots_where_the_boot_sequence_leaves_it() {
    let mut gba = machine(&[]);
    // ARM state, System mode, PC at the cartridge's entry point, `SP_sys` live at the top of its
    // area and the supervisor pair parked in its own bank. GBATEK, *GBA Cartridge Header*: "With
    // all entry points, the CPU is initially set into system mode"; *Default memory usage at
    // 03007FXX*: "SP_svc=03007FE0h, SP_irq=03007FA0h, SP_usr=03007F00h".
    assert!(!gba.cpu.thumb());
    assert_eq!(gba.cpu.cpsr & 0x1F, cpu::MODE_SYS);
    assert_eq!(
        gba.cpu.cpsr & cpu::FLAG_I,
        0,
        "IRQs are enabled: the public BIOS suite enables IME and expects its handler to run"
    );
    assert_eq!(gba.cpu.pc(), 0x0800_00C0);
    assert_eq!(gba.cpu.r[13], cpu::SP_USR);
    assert_eq!(gba.cpu.bank_sp_lr[cpu::BANK_SVC], [cpu::SP_SVC, 0]);
    assert_eq!(cpu::SP_SVC, 0x0300_7FE0);
    assert_eq!(cpu::SP_IRQ, 0x0300_7FA0);
    assert_eq!(cpu::SP_USR, 0x0300_7F00);
    // The I/O page's post-boot values.
    assert_eq!(gba.bus.read16(0x0400_0000), 0x0000, "DISPCNT");
    assert_eq!(gba.bus.read16(0x0400_0088), 0x0200, "SOUNDBIAS");
    assert_eq!(gba.bus.read8(0x0400_0300), 0x01, "POSTFLG");
    // And the frame clock is at the top of the frame.
    assert_eq!(vcount(&mut machine(&[])), 0);
}

#[test]
fn every_region_of_the_memory_map_answers() {
    let mut gba = machine(&[]);
    let bus = &mut gba.bus;
    // EWRAM: 256 KiB, mirrored every 256 KiB across `$02000000-$02FFFFFF`.
    bus.write8(EWRAM_BASE + 0x10, 0x5A);
    assert_eq!(bus.read8(EWRAM_BASE + 0x10), 0x5A);
    assert_eq!(bus.read8(EWRAM_BASE + 0x4_0010), 0x5A, "mirrored");
    assert_eq!(bus.read8(EWRAM_BASE + 0xF_C0010), 0x5A, "$02FC0010");
    // IWRAM: 32 KiB, mirrored every 32 KiB.
    bus.write8(IWRAM_BASE + 0x20, 0xA5);
    assert_eq!(bus.read8(IWRAM_BASE + 0x20), 0xA5);
    assert_eq!(bus.read8(IWRAM_BASE + 0x8020), 0xA5, "mirrored");
    assert_eq!(bus.read8(0x030F_8020), 0xA5, "$030F8020");
    // Palette, VRAM and OAM have their documented sizes. Halfwords for VRAM and OAM: neither takes
    // an 8-bit write (G2, from GBATEK's *Address Bus Width* table — `memory.gba` tests 50 and 52).
    bus.write8(PALETTE_BASE + 0x3FE, 0x11);
    assert_eq!(bus.read8(PALETTE_BASE + 0x3FE), 0x11);
    assert_eq!(
        bus.read8(PALETTE_BASE + 0x7FE),
        0x11,
        "mirrored every 1 KiB"
    );
    bus.write16(VRAM_BASE + 0x1_7FFE, 0x2222);
    assert_eq!(
        bus.read16(VRAM_BASE + 0x1_7FFE),
        0x2222,
        "the last of 96 KiB"
    );
    bus.write16(OAM_BASE + 0x3FE, 0x3333);
    assert_eq!(bus.read16(OAM_BASE + 0x3FE), 0x3333);
    // The three ROM windows show the cartridge from its own start.
    assert_eq!(bus.read8(ROM_BASE), 0x2E, "the entry point's low byte");
    assert_eq!(bus.read8(ROM_WS1_BASE), 0x2E);
    assert_eq!(bus.read8(ROM_WS2_BASE), 0x2E);
    // A write to ROM is ignored, not a panic.
    bus.write8(ROM_BASE, 0xFF);
    assert_eq!(bus.read8(ROM_BASE), 0x2E);
    // The save window is at `$0E000000`; this cartridge has no save chip (no ID string), so the
    // window reads 0xFF until something stores into it. A store is the proof that a chip is there
    // (`none.gba`, a pure read, stays silent), and from then on the byte comes back.
    bus.write8(SRAM_BASE + 0x42, 0x99);
    assert_eq!(bus.read8(SRAM_BASE + 0x42), 0x99, "the store landed");
    // Reading undecoded space returns the last value on the bus.
    bus.write32(IWRAM_BASE, 0xDEAD_BEEF);
    assert_eq!(bus.read32(0x1000_0000), 0xDEAD_BEEF, "open bus");
    // The BIOS region is not open bus: it answers with the opcode the BIOS last fetched, which is
    // what GBATEK's *Reading from BIOS Memory* says a cartridge sees (G2).
    assert_eq!(
        bus.read32(0x0000_0000),
        tendy_gba::bios::OPCODE_AFTER_BOOT,
        "the BIOS read-protection latch"
    );
}

#[test]
fn the_frame_clock_is_228_lines_of_1232_cycles_with_the_vblank_flag_where_gbatek_puts_it() {
    let mut gba = machine(&[]);
    assert_eq!(vcount(&mut gba), 0);
    for line in 0..LINES_PER_FRAME {
        let line_now = vcount(&mut gba);
        assert_eq!(line_now, line, "VCOUNT counts 0-227");
        let flag = dispstat(&mut gba) & 1;
        let want = u16::from((160..=226).contains(&line));
        assert_eq!(flag, want, "VBlank flag on line {line}");
        gba.bus.lcd.tick(CYCLES_PER_LINE);
    }
    assert_eq!(vcount(&mut gba), 0, "and wraps to 0");
    // Within a line the flag does not move; the HBlank flag does, at cycle 1006 — on a line that
    // has one. GBATEK, *DISPSTAT*: the H-Blank flag is "set in lines 2-159,162-225", so line 0 has
    // neither the flag nor the event.
    assert_eq!(dispstat(&mut gba) & 1, 0);
    gba.bus.lcd.tick(CYCLES_PER_LINE * 2 + 1006);
    assert_ne!(
        dispstat(&mut gba) & 2,
        0,
        "HBlank flag up after the 960 drawing cycles of line 2"
    );
    gba.bus.lcd.tick(CYCLES_PER_LINE - 1006);
    assert_eq!(dispstat(&mut gba) & 2, 0, "and down at the next line");
}

#[test]
fn sixty_frames_take_sixty_frames_of_two_hundred_and_eighty_thousand_cycles() {
    let mut gba = machine(&[]);
    for _ in 0..60 {
        gba.run_frame();
    }
    assert_eq!(gba.frames(), 60);
    assert_eq!(gba.bus.lcd.frames, 60, "the LCD finished 60 frames");
    // 1232 x 228 = 280 896, so sixty frames are 16 853 760 cycles. The frame boundary is the
    // LCD's, so no drift accumulates; the only slack is the last instruction of each frame, which
    // may cross the boundary by a few cycles, exactly as the hardware's pipeline does.
    let expected = 60 * u64::from(CYCLES_PER_FRAME);
    assert_eq!(expected, 16_853_760);
    let spent = gba.bus.cycles;
    assert!(spent >= expected, "{spent} < {expected}");
    assert!(
        spent < expected + 60 * 24,
        "sixty frames cost {spent} cycles, {} over the {expected} they should",
        spent - expected
    );
    assert_eq!(
        vcount(&mut gba),
        0,
        "and the clock is at the top of a frame"
    );
}

#[test]
fn an_arm_program_runs_and_writes_through_the_bus() {
    // mov r0,#0x03000000 / mov r1,#0x42 / strb r1,[r0] / b -2
    let mut gba = machine(&[0xE3A0_0403, 0xE3A0_1042, 0xE5C0_1000, 0xEAFF_FFFE]);
    for _ in 0..3 {
        gba.step_instruction();
    }
    assert_eq!(gba.cpu.r[0], IWRAM_BASE);
    assert_eq!(gba.cpu.r[1], 0x42);
    assert_eq!(gba.bus.read8(IWRAM_BASE), 0x42, "the store reached the bus");
    // Running a frame leaves the program where it is (a `b -2` loop) and consumes the frame. The
    // three instructions above are already inside the frame, so it costs a little less than a whole
    // one; the LCD's frame counter, not the instruction count, is what completes.
    let before = gba.bus.cycles;
    gba.run_frame();
    assert_eq!(gba.frames(), 1);
    assert_eq!(gba.bus.lcd.frames, 1);
    assert!(gba.bus.cycles - before <= u64::from(CYCLES_PER_FRAME));
    assert!(gba.bus.cycles >= u64::from(CYCLES_PER_FRAME));
}

#[test]
fn the_system_view_is_a_two_hundred_and_forty_by_one_sixty_picture_in_colour() {
    let mut gba = machine(&[]);
    assert_eq!(gba.console_name(), "Game Boy Advance");
    assert!(!gba.is_cgb());
    assert_eq!(gba.cgb_speed(), 1);
    assert!(
        !gba.booting(),
        "no boot ROM: the cartridge owns the machine"
    );
    assert_eq!(WIDTH, 240);
    assert_eq!(HEIGHT, 160);
    assert_eq!(PIXELS, 38_400);
    gba.set_buttons(tendy_gb::joypad::Buttons::A);
    gba.run_frame();
    let frame = gba.frame();
    assert_eq!((frame.width, frame.height), (240, 160));
    assert_eq!(frame.len(), 240 * 160);
    assert_eq!(frame.colours.len(), 240 * 160, "RGB555, one word a pixel");
    assert!(
        frame.in_colour(),
        "a Game Boy Advance frame is drawn from its colours"
    );
    // A cartridge that never touches `DISPCNT` displays its backdrop, which is palette entry 0 —
    // black on a machine that has written no palette.
    assert!(
        frame.colours.iter().all(|&word| word == 0),
        "the backdrop, not a blank field: palette entry 0"
    );
    // Audio is silent rather than absent: a machine that never enabled its sound unit still
    // produces the front end's sample count, and every sample is exactly zero.
    let mut out = [1.0f32; 64];
    let frames = gba.drain_audio(&mut out);
    assert!(frames > 0, "the drain produces samples");
    assert!(
        out[..frames * 2].iter().all(|s| *s == 0.0),
        "and a sound unit that was never enabled is exactly silent"
    );
    // A Game Boy Advance has no GameShark codes here, and says so instead of ignoring them.
    let notice = gba.apply_cheats(&[tendy_gb::cheats::CheatEntry {
        text: "0101F0C1".to_owned(),
        on: true,
    }]);
    assert_eq!(notice.len(), 1);
    assert!(notice[0].contains("Game Boy only"), "{:?}", notice);
    // An empty list says nothing at all.
    assert!(gba.apply_cheats(&[]).is_empty());
}

/// `KEYINPUT`, `KEYCNT` and `IF`, as the CPU addresses them.
const KEYINPUT_ADDR: u32 = 0x0400_0130;
const KEYCNT_ADDR: u32 = 0x0400_0132;
const IF_ADDR: u32 = 0x0400_0202;

/// G7: the Advance's ten buttons. Bits 0-7 are the Game Boy's eight in a byte, and the two
/// shoulders travel beside them as [`tendy_gb::joypad::Shoulders`] — R and L on `KEYINPUT` bits 8 and 9.
#[test]
fn the_two_shoulders_reach_keyinput_bits_eight_and_nine() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    assert_eq!(
        gba.bus.read16(KEYINPUT_ADDR),
        0x03FF,
        "nothing pressed reads all ten released"
    );
    gba.set_shoulders(Shoulders::LEFT);
    assert_eq!(
        gba.bus.read16(KEYINPUT_ADDR),
        0x03FF & !(1 << 9),
        "L is bit 9"
    );
    gba.set_shoulders(Shoulders::RIGHT);
    assert_eq!(
        gba.bus.read16(KEYINPUT_ADDR),
        0x03FF & !(1 << 8),
        "R is bit 8"
    );
    gba.set_shoulders(Shoulders::LEFT | Shoulders::RIGHT);
    assert_eq!(gba.bus.read16(KEYINPUT_ADDR), 0x03FF & !0x0300);
    // A shoulder never disturbs the Game Boy's eight bits, and the register is read-only.
    gba.set_buttons(Buttons::A);
    assert_eq!(gba.bus.read16(KEYINPUT_ADDR), 0x03FF & !0x0300 & !(1 << 0));
    gba.bus.write16(KEYINPUT_ADDR, 0);
    assert_eq!(gba.bus.read16(KEYINPUT_ADDR), 0x03FF & !0x0300 & !(1 << 0));
}

/// G7: `KEYCNT` at `$04000132` — bits 0-9 the keys watched, bit 14 the enable, bit 15 the condition
/// (GBATEK: 0 = a request if *any* watched key is pressed, 1 = if *all* of them are).
#[test]
fn the_keypad_irq_follows_keycnt() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    let ack = |gba: &mut Gba| gba.bus.write16(IF_ADDR, 1 << 12);
    let raised = |gba: &mut Gba| gba.bus.irq_flags() & (1 << 12) != 0;

    // Watch L alone, OR: pressing L raises the request, and arming the watch with nothing held
    // raises nothing.
    gba.bus.write16(KEYCNT_ADDR, 0x4000 | (1 << 9));
    assert!(!raised(&mut gba), "nothing held yet");
    gba.set_shoulders(Shoulders::LEFT);
    assert!(raised(&mut gba), "L matched the OR watch");

    // A key outside the mask is not watched.
    ack(&mut gba);
    gba.set_shoulders(Shoulders::RIGHT);
    assert!(!raised(&mut gba), "R is not in the mask");

    // AND over A and B: neither alone matches, both together do.
    gba.bus.write16(KEYCNT_ADDR, 0xC000 | 0x0003);
    gba.set_buttons(Buttons::A);
    assert!(!raised(&mut gba), "AND needs both keys");
    gba.set_buttons(Buttons::A | Buttons::B);
    assert!(raised(&mut gba), "A and B are both held");

    // A store to `KEYCNT` can complete the condition by itself, with the keys already held.
    ack(&mut gba);
    gba.bus.write16(KEYCNT_ADDR, 0x0000);
    ack(&mut gba);
    gba.bus.write16(KEYCNT_ADDR, 0x4000 | 0x0003);
    assert!(raised(&mut gba), "arming the watch raised the request");

    // With the enable clear, nothing is watched however the keys sit.
    ack(&mut gba);
    gba.bus.write16(KEYCNT_ADDR, 0x8000 | 0x0003);
    gba.set_buttons(Buttons::NONE);
    gba.set_buttons(Buttons::A | Buttons::B);
    assert!(!raised(&mut gba), "bit 14 clear means no request");
    // The register reads back its two control bits and the ten-bit mask, nothing else.
    gba.bus.write16(KEYCNT_ADDR, 0xC3FF);
    assert_eq!(gba.bus.read16(KEYCNT_ADDR), 0xC3FF);
}

/// H6: the keypad request, end to end, on a cartridge we build.
///
/// The three tests above prove the register and the flag; none of them makes a *cartridge* take an
/// interrupt, which is the thing a Game Boy Advance user would notice if it were broken. This
/// cartridge is hand-built bytes like every other synthetic one here: an ARM program that points
/// `[$03007FFC]` at a handler in its own ROM, arms `KEYCNT`, enables the request in `IE`, sets
/// `IME`, and spins. The handler records what it saw at entry — `IF`, `KEYINPUT`, `IE`, `IME`,
/// `VCOUNT` and a run count — in IWRAM at `$03000100`, acknowledges `IF` bit 12 and returns through
/// the BIOS epilogue with `bx lr`, which is how the real BIOS hands a user handler back.
///
/// ```text
/// $080000C0  mov r0,#$04000000            ; the BIOS prologue leaves this in r0 as well
/// $080000C4  ldr r1,[pc,#handler]         ; the handler's address, patched from the pool
/// $080000C8  ldr r2,[pc,#$03007FFC]
/// $080000CC  str r1,[r2]                  ; the user IRQ handler the BIOS will load
/// $080000D0  ldr r1,[pc,#keycnt]
/// $080000D4  add r12,r0,#$100             ; $1xx: KEYCNT is at +$32
/// $080000D8  strh r1,[r12,#$32]           ; KEYCNT = $132
/// $080000DC  add r12,r0,#$200             ; $2xx: IE at +0, IME at +8
/// $080000E0  mov r1,#$1000
/// $080000E4  strh r1,[r12,#0]             ; IE: the keypad, bit 12
/// $080000E8  mov r1,#1
/// $080000EC  strh r1,[r12,#8]             ; IME
/// $080000F0  b .                          ; and wait for a key
/// $080000F4  add r3,r0,#$200              ; the handler: IF, IE and IME
/// $080000F8  ldrh r1,[r3,#2]              ; IF at entry
/// ```
///
/// A halfword transfer's immediate is eight bits, so `$132` cannot be addressed as
/// `[r0,#$132]`; the two `add`s are how a cartridge reaches the I/O page's upper half with one.
struct KeypadCart {
    words: Vec<u32>,
    literals: Vec<u32>,
    fixups: Vec<(usize, usize)>,
}

impl KeypadCart {
    /// `strh <rd>, [<rn>, #<imm>]`. A halfword transfer's immediate is eight bits — `imm4H:imm4L`,
    /// up to `$FF` — which is why the program below builds the I/O page's `$1xx` and `$2xx` halves
    /// in registers rather than reaching for `#<imm>`.
    fn strh(rn: u32, rd: u32, imm: u32) -> u32 {
        assert!(imm < 0x100, "a halfword immediate is eight bits");
        0xE1C0_00B0 | (rn << 16) | (rd << 12) | ((imm & 0xF0) << 4) | (imm & 0x0F)
    }

    /// `ldrh <rd>, [<rn>, #<imm>]`.
    fn ldrh(rn: u32, rd: u32, imm: u32) -> u32 {
        assert!(imm < 0x100, "a halfword immediate is eight bits");
        0xE1D0_00B0 | (rn << 16) | (rd << 12) | ((imm & 0xF0) << 4) | (imm & 0x0F)
    }

    /// `str <rd>, [<rn>, #<imm>]`.
    fn str(rn: u32, rd: u32, imm: u32) -> u32 {
        0xE580_0000 | (rn << 16) | (rd << 12) | imm
    }

    /// `ldr <rd>, [<rn>, #<imm>]`.
    fn ldr(rn: u32, rd: u32, imm: u32) -> u32 {
        0xE590_0000 | (rn << 16) | (rd << 12) | imm
    }

    fn word(&mut self, word: u32) -> usize {
        self.words.push(word);
        self.words.len() - 1
    }

    /// `ldr <rd>, [pc, #pool]` of `value`; the offset is patched when the pool is placed.
    fn literal(&mut self, rd: u32, value: u32) {
        let literal = self.literals.len();
        self.literals.push(value);
        let at = self.word(0xE59F_0000 | (rd << 12));
        self.fixups.push((at, literal));
    }

    /// The program with its literal pool at the end, every `ldr`'s offset patched.
    fn finish(mut self) -> Vec<u32> {
        let pool = self.words.len();
        for (at, literal) in &self.fixups {
            // `ldr rd,[pc,#imm]` reads the word at the instruction's address + 8.
            let offset = ((pool + literal - at - 2) * 4) as u32;
            assert!(
                offset < 0x1000,
                "a literal pool entry beyond the LDR's reach"
            );
            self.words[*at] |= offset;
        }
        self.words.extend(self.literals);
        self.words
    }
}

/// `mov <rd>, #<imm>`, with the encoded `imm12` (`rotate << 8 | imm8`) given directly.
fn mov_imm(rd: u32, imm12: u32) -> u32 {
    0xE3A0_0000 | (rd << 12) | imm12
}

/// `add <rd>, <rn>, #<imm>`, with the encoded `imm12` given directly.
fn add_imm(rn: u32, rd: u32, imm12: u32) -> u32 {
    0xE280_0000 | (rn << 16) | (rd << 12) | imm12
}

/// The keypad-IRQ cartridge above, armed with `keycnt` as the value it stores to `KEYCNT`. Returns
/// the machine and the address of its handler.
fn keypad_cart(keycnt: u16) -> (Gba, u32) {
    let mut cart = KeypadCart {
        words: Vec::new(),
        literals: Vec::new(),
        fixups: Vec::new(),
    };
    // `$04000000` is `mov r0,#$04000000` (imm8 4, rotate 4); `$100` and `$200` are
    // `add r12,r0,#<imm>` with imm12 `$C01` and `$C02` (imm8 1 and 2, rotate 12).
    cart.word(mov_imm(0, 0x404)); // mov r0,#$04000000
    cart.literal(1, 0); // ldr r1,[pc,#handler] - patched below
    cart.literal(2, 0x0300_7FFC); // ldr r2,[pc,#$03007FFC]
    cart.word(KeypadCart::str(2, 1, 0)); // str r1,[r2]
    cart.literal(1, u32::from(keycnt)); // ldr r1,[pc,#keycnt]
    cart.word(add_imm(0, 12, 0xC01)); // add r12,r0,#$100
    cart.word(KeypadCart::strh(12, 1, 0x32)); // strh r1,[r12,#$32] - KEYCNT at $132
    cart.word(add_imm(0, 12, 0xC02)); // add r12,r0,#$200
    cart.word(mov_imm(1, 0xA01)); // mov r1,#$1000
    cart.word(KeypadCart::strh(12, 1, 0x00)); // strh r1,[r12,#0]  - IE at $200
    cart.word(mov_imm(1, 0x001)); // mov r1,#1
    cart.word(KeypadCart::strh(12, 1, 0x08)); // strh r1,[r12,#8]  - IME at $208
    cart.word(0xEAFF_FFFE); // b .
    let handler = 0x0800_00C0 + (cart.words.len() as u32) * 4;
    cart.word(add_imm(0, 3, 0xC02)); // add r3,r0,#$200 - IF, IE and IME
    cart.word(add_imm(0, 12, 0xC01)); // add r12,r0,#$100 - KEYINPUT
    cart.word(KeypadCart::ldrh(3, 1, 0x02)); // ldrh r1,[r3,#2]  - IF
    cart.literal(2, IWRAM_BASE + 0x100); // ldr r2,[pc,#witness]
    cart.word(KeypadCart::str(2, 1, 0)); // str r1,[r2]
    cart.word(KeypadCart::ldrh(12, 1, 0x30)); // ldrh r1,[r12,#$30] - KEYINPUT
    cart.word(KeypadCart::str(2, 1, 4)); // str r1,[r2,#4]
    cart.word(KeypadCart::ldrh(3, 1, 0x00)); // ldrh r1,[r3,#0]  - IE
    cart.word(KeypadCart::str(2, 1, 8)); // str r1,[r2,#8]
    cart.word(KeypadCart::ldrh(3, 1, 0x08)); // ldrh r1,[r3,#8]  - IME
    cart.word(KeypadCart::str(2, 1, 12)); // str r1,[r2,#12]
    cart.word(KeypadCart::ldrh(0, 1, 0x06)); // ldrh r1,[r0,#6]  - VCOUNT
    cart.word(KeypadCart::str(2, 1, 16)); // str r1,[r2,#16]
    cart.word(KeypadCart::ldr(2, 1, 20)); // ldr r1,[r2,#20] - the run count
    cart.word(0xE281_1001); // add r1,r1,#1
    cart.word(KeypadCart::str(2, 1, 20)); // str r1,[r2,#20]
    cart.word(mov_imm(1, 0xA01)); // mov r1,#$1000
    cart.word(KeypadCart::strh(3, 1, 0x02)); // strh r1,[r3,#2]  - acknowledge IF bit 12
    cart.word(0xE12F_FF1E); // bx lr - back to the BIOS epilogue at $138
    // The first literal is the `ldr r1,[pc,#handler]`: it holds the handler's address, which is
    // only known once the code is placed.
    cart.literals[0] = handler;
    (machine(&cart.finish()), handler)
}

/// `IF`, `KEYINPUT`, `IE`, `IME`, `VCOUNT` and the run count the handler recorded.
fn keypad_witness(gba: &mut Gba) -> [u32; 6] {
    let mut witness = [0u32; 6];
    for (index, word) in witness.iter_mut().enumerate() {
        *word = gba.bus.read32(IWRAM_BASE + 0x100 + index as u32 * 4);
    }
    witness
}

/// Step the handler — and the BIOS epilogue behind it — to completion: until it has recorded its run
/// count and the core is back in the mode the cartridge started in, System.
fn run_handler(gba: &mut Gba) -> [u32; 6] {
    for _ in 0..128 {
        let witness = keypad_witness(gba);
        if witness[5] != 0 && gba.cpu.mode() == cpu::MODE_SYS {
            return witness;
        }
        gba.step_instruction();
    }
    panic!(
        "the handler did not return: pc {:08X} mode {:02X} witness {:?}",
        gba.cpu.pc(),
        gba.cpu.mode(),
        keypad_witness(gba)
    );
}

/// Step until the program counter reaches `handler`, and say which frame and cycle its first
/// instruction ran on. `None` if `limit` instructions went by without it.
fn step_to_handler(gba: &mut Gba, handler: u32, limit: usize) -> Option<(u64, u64)> {
    for _ in 0..limit {
        if gba.cpu.pc() == handler {
            return Some((gba.bus.lcd.frames, gba.bus.cycles));
        }
        gba.step_instruction();
    }
    None
}

/// Step until the core is at the IRQ vector, and hand back `IE`, `IF`, `IME` and `CPSR` there.
fn step_to_vector(gba: &mut Gba, limit: usize) -> Option<(u16, u16, bool, u32)> {
    for _ in 0..limit {
        if gba.cpu.pc() == cpu::VECTOR_IRQ {
            return Some((
                gba.bus.ie(),
                gba.bus.irq_flags(),
                gba.bus.ime(),
                gba.cpu.cpsr,
            ));
        }
        gba.step_instruction();
    }
    None
}

/// H6, case one: the key is pressed, and a cartridge's own handler runs.
///
/// **The frame arithmetic.** [`Gba::run_frame`] runs whole instructions until the LCD's frame
/// counter moves, so after two frames the clock is at 2 × 280 896 = 561 792 cycles *plus the last
/// instruction's overshoot*; measured here it reads 561 800. The front end's press raises `IF`
/// bit 12 inside [`tendy_gba::bus::Bus::set_buttons`] with no cycles of its own; the core then spends
/// 7 cycles taking the exception, 3 on the BIOS's `b $128` and 7 in the prologue at `$128` — 17 —
/// before the handler's first instruction, at 561 817. That is still inside frame 2 (which ends at
/// 842 688) and on its line 0, so the handler runs on frame **2** with `VCOUNT` 0.
#[test]
fn a_pressed_key_raises_the_keypad_request_and_the_cartridge_handler_runs() {
    let (mut gba, handler) = keypad_cart(0x4000 | 0x0001);
    gba.bus.trace_keycnt();
    for _ in 0..2 {
        gba.run_frame();
    }
    assert_eq!(
        gba.bus.irq_flags() & irq::bit(irq::KEYPAD),
        0,
        "the watch is armed with nothing held"
    );
    assert_eq!(keypad_witness(&mut gba)[5], 0, "the handler has not run");

    let pressed_at = gba.bus.cycles;
    gba.set_buttons(Buttons::A);
    let (ie, flags, ime, cpsr) = step_to_vector(&mut gba, 16).expect("the IRQ was taken");
    assert_eq!(ie, 0x1000, "IE holds the keypad request and nothing else");
    assert_eq!(
        flags & 0x1000,
        0x1000,
        "IF bit 12 is the request that fired"
    );
    assert!(ime, "IME let it through");
    assert_eq!(cpsr & 0x1F, cpu::MODE_IRQ, "the core is in IRQ mode");
    assert_ne!(cpsr & cpu::FLAG_I, 0, "and IRQs are masked on entry");

    let (frame, cycles) = step_to_handler(&mut gba, handler, 32).expect("the handler ran");
    assert_eq!(frame, 2, "the press landed at the start of frame 2");
    assert_eq!(
        cycles,
        pressed_at + 17,
        "the handler's first instruction: the 7-cycle exception, the BIOS's 3-cycle `b $128` and \
         its 7-cycle prologue, from the cycle the press landed"
    );

    let witness = run_handler(&mut gba);
    assert_eq!(witness[0] & 0x1000, 0x1000, "the handler saw IF bit 12");
    assert_eq!(witness[1] & 0x0001, 0, "KEYINPUT bit 0: A was down");
    assert_eq!(witness[2], 0x1000, "IE inside the handler");
    assert_eq!(witness[3], 1, "IME inside the handler");
    assert_eq!(witness[4], 0, "VCOUNT 0: the very top of the frame");
    assert_eq!(witness[5], 1, "the handler ran exactly once");
    assert_eq!(
        gba.cpu.mode(),
        cpu::MODE_SYS,
        "back where the IRQ came from"
    );
    assert_eq!(
        gba.bus.irq_flags() & irq::bit(irq::KEYPAD),
        0,
        "the handler's acknowledgement cleared IF bit 12"
    );

    // The probe's instrument saw the cartridge's own store: `$4001`, at frame 0.
    let writes = gba.bus.keycnt_writes.take().expect("the trace was armed");
    assert_eq!(writes.len(), 1, "one store: {writes:?}");
    assert_eq!(writes[0].value, 0x4001);
    assert_eq!(
        writes[0].value & 0x4000,
        0x4000,
        "bit 14: the request is enabled"
    );
    assert_eq!(
        writes[0].frame, 0,
        "the cartridge armed the watch on frame 0"
    );
    assert_eq!(writes[0].keys, 0x03FF, "with nothing held");
}

/// H6, case two: the same cartridge, and nobody presses the key.
#[test]
fn a_keypad_watch_on_a_key_nobody_presses_raises_no_request() {
    let (mut gba, handler) = keypad_cart(0x4000 | 0x0001);
    for _ in 0..8 {
        gba.run_frame();
    }
    assert_eq!(
        gba.bus.irq_flags() & irq::bit(irq::KEYPAD),
        0,
        "eight frames, no press, no request"
    );
    assert!(
        step_to_handler(&mut gba, handler, 32).is_none(),
        "the handler never ran"
    );
    assert_eq!(keypad_witness(&mut gba)[5], 0);
    assert_eq!(
        gba.cpu.mode(),
        cpu::MODE_SYS,
        "the core never left its boot mode"
    );
}

/// H6, case three: bit 15 inverted — the AND condition — holds the request until every watched key
/// is down, and one key alone raises nothing.
///
/// Frames: the watch is armed at frame 0 with `KEYCNT = $C003` (AND over A and B), A is pressed at
/// the start of frame 2 and B at the start of frame 4, so the request fires on frame **4**. The
/// arithmetic is the one case one states: 4 × 280 896 = 1 123 584 cycles plus the same overshoot,
/// then the same 17 cycles of exception and BIOS prologue before the handler's first instruction.
#[test]
fn the_and_condition_holds_the_request_until_every_watched_key_is_down() {
    let (mut gba, handler) = keypad_cart(0xC000 | 0x0003);
    for _ in 0..2 {
        gba.run_frame();
    }
    gba.set_buttons(Buttons::A);
    for _ in 0..2 {
        gba.run_frame();
    }
    assert_eq!(
        gba.bus.irq_flags() & irq::bit(irq::KEYPAD),
        0,
        "A alone does not satisfy `A and B`"
    );
    assert_eq!(keypad_witness(&mut gba)[5], 0, "and no handler ran");

    let pressed_at = gba.bus.cycles;
    gba.set_buttons(Buttons::A | Buttons::B);
    let (frame, cycles) = step_to_handler(&mut gba, handler, 64).expect("the handler ran");
    assert_eq!(frame, 4, "the second key landed at the start of frame 4");
    assert_eq!(
        cycles,
        pressed_at + 17,
        "the same 17 cycles from the press: the exception, `b $128` and the prologue"
    );
    let witness = run_handler(&mut gba);
    assert_eq!(witness[0] & 0x1000, 0x1000, "the handler saw IF bit 12");
    assert_eq!(witness[1] & 0x0003, 0, "both keys were down");
}

/// H6, case four: the shoulders travel through the same path, which is the request a Game Boy
/// Advance user hits first.
#[test]
fn the_shoulders_raise_the_keypad_request_through_the_same_path() {
    // OR over R and L, R held from the start of frame 2.
    let (mut gba, handler) = keypad_cart(0x4000 | 0x0300);
    for _ in 0..2 {
        gba.run_frame();
    }
    gba.set_shoulders(Shoulders::RIGHT);
    let pressed_at = gba.bus.cycles;
    let (ie, flags, _, _) = step_to_vector(&mut gba, 16).expect("R raised the request");
    assert_eq!(ie, 0x1000);
    assert_eq!(flags & 0x1000, 0x1000);
    let (frame, cycles) = step_to_handler(&mut gba, handler, 32).expect("the handler ran");
    assert_eq!(frame, 2);
    assert_eq!(
        cycles,
        pressed_at + 17,
        "R's press took the same 17-cycle path the face buttons take"
    );
    let witness = run_handler(&mut gba);
    assert_eq!(witness[0] & 0x1000, 0x1000, "IF bit 12 in the handler");
    assert_eq!(
        witness[1] & 0x0100,
        0,
        "KEYINPUT bit 8: R was down when the handler ran"
    );
    assert_eq!(witness[1] & 0x0200, 0x0200, "L was not");

    // The AND condition over both shoulders: R alone raises nothing, R and L together fire.
    let (mut gba, handler) = keypad_cart(0xC000 | 0x0300);
    for _ in 0..2 {
        gba.run_frame();
    }
    gba.set_shoulders(Shoulders::RIGHT);
    for _ in 0..2 {
        gba.run_frame();
    }
    assert_eq!(
        gba.bus.irq_flags() & irq::bit(irq::KEYPAD),
        0,
        "R alone does not satisfy `R and L`"
    );
    assert_eq!(
        keypad_witness(&mut gba)[5],
        0,
        "and no handler ran with R alone"
    );
    gba.set_shoulders(Shoulders::RIGHT | Shoulders::LEFT);
    let (frame, _) = step_to_handler(&mut gba, handler, 64).expect("the handler ran");
    assert_eq!(frame, 4);
    assert_eq!(
        run_handler(&mut gba)[1] & 0x0300,
        0,
        "both shoulders were down"
    );
}

#[test]
fn a_state_saves_and_loads_the_whole_machine() {
    let mut gba = machine(&[0xE3A0_0403, 0xEAFF_FFFE]);
    gba.step_instruction();
    gba.bus.write8(IWRAM_BASE + 0x100, 0x77);
    gba.bus.write16(VRAM_BASE + 0x10, 0x8888);
    gba.run_frame();
    let saved = gba.save_state();
    let before = (gba.cpu.r[0], gba.frames(), gba.bus.cycles);
    // Move the machine on, then put it back.
    for _ in 0..3 {
        gba.run_frame();
    }
    gba.bus.write8(IWRAM_BASE + 0x100, 0x00);
    gba.load_state(&saved).expect("our own state");
    assert_eq!((gba.cpu.r[0], gba.frames(), gba.bus.cycles), before);
    assert_eq!(gba.bus.read8(IWRAM_BASE + 0x100), 0x77);
    assert_eq!(gba.bus.read8(VRAM_BASE + 0x10), 0x88);
    // A Game Boy state is refused by its magic, not misread.
    let why = gba.load_state(b"GBST\0\0\0\0").unwrap_err();
    assert!(
        matches!(why, tendy_gb::state::StateError::BadMagic),
        "{why:?}"
    );
}

/// The state's version 8 additions: the DMA channels' internal cursors, without which a transfer
/// saved mid-flight resumes from its start, and the two shoulder buttons.
#[test]
fn a_state_carries_the_dma_cursors_and_the_shoulders() {
    let mut gba = machine(&[0xEAFF_FFFE]);
    gba.bus.dma.channels[1].cursor_sad = 0x0300_1234;
    gba.bus.dma.channels[1].cursor_dad = 0x0400_0020;
    gba.set_shoulders(Shoulders::LEFT);
    let saved = gba.save_state();
    // Move both facts, then put the state back.
    gba.bus.dma.channels[1].cursor_sad = 0;
    gba.bus.dma.channels[1].cursor_dad = 0;
    gba.set_shoulders(Shoulders::NONE);
    gba.load_state(&saved).expect("our own state");
    assert_eq!(
        gba.bus.dma.channels[1].cursor_sad, 0x0300_1234,
        "the source pointer resumed mid-transfer"
    );
    assert_eq!(gba.bus.dma.channels[1].cursor_dad, 0x0400_0020);
    assert_eq!(
        gba.bus.read16(0x0400_0130),
        0x03FF & !(1 << 9),
        "L is still held after the load"
    );
}

#[test]
#[ignore = "needs roms/test/gba/arm.gba, a public test ROM kept out of the repository"]
fn arm_gba_reports_no_failed_tests() {
    // The gate. The ROM writes the number of the first failing test into IWRAM as three digits and
    // writes nothing at all when every test passes; a cartridge's IWRAM starts zeroed, so twelve
    // zero bytes at $03000000 is the pass condition.
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/roms/test/gba/arm.gba");
    let rom = std::fs::read(path).expect("roms/test/gba/arm.gba (scripts/fetch-test-roms.sh)");
    let mut gba = Gba::new(Cartridge::new(rom).expect("a public test ROM"));
    for _ in 0..600 {
        gba.run_frame();
    }
    let witness = iwram_witness(&mut gba);
    let parked = gba.bus.read32(gba.cpu.pc() & !3);
    println!(
        "arm.gba: {} frames, {} cycles, {} instructions, PC {:08X} (B .), {} SWIs, witness {:?}",
        gba.frames(),
        gba.bus.cycles,
        gba.cpu.instructions,
        gba.cpu.pc(),
        gba.cpu.swi_calls,
        witness
    );
    // The failure path is the suites' only caller of `SWI`, and it starts with the `Div` that turns
    // the test number into digits before storing them, so a run with no SWI has no failures. The
    // twelve bytes at $03000000 are printed rather than asserted zero: the single-transfer tests use
    // that address as scratch (test 350 stores `$FFFFFFFF` there) and never restore it.
    assert_eq!(
        gba.cpu.swi_calls,
        0,
        "the failure path ran, for test {}",
        failed_test(witness)
    );
    assert_eq!(parked, 0xEAFF_FFFE, "the ROM reached its idle branch");
    // The PSR-transfer tests finish by writing SYS mode (test 252, `MSR CPSR_c, MODE_SYS`) and
    // never switch back, so the suite's own last mode is system, not the SVC it booted in.
    assert_eq!(gba.cpu.mode(), cpu::MODE_SYS, "the suite left SYS live");
    // The last group — block transfer, tests 500-532 — uses $03004500 upward as its scratch:
    // `stmfa r11!, {r0, r1}` with r0=32 and r1=64 leaves them at $03004504 and $03004508. A wild
    // branch that reached `eval` with a zero test register would leave these untouched, so they
    // are the proof that the suite's tail actually ran.
    assert_eq!(gba.bus.read32(IWRAM_BASE + 0x4504), 32, "test 500 ran");
    assert_eq!(gba.bus.read32(IWRAM_BASE + 0x4508), 64, "test 500 ran");
    let late_mark = gba.bus.read32(IWRAM_BASE + 0x48C4);
    assert!(
        (0x0800_0000..0x0800_0000 + 0x2278).contains(&late_mark),
        "a late block-transfer test stored a ROM address, got {late_mark:08X}"
    );
    assert_eq!(gba.cpu.swi, None, "no BIOS call was left unserviced");
}

#[test]
#[ignore = "needs roms/test/gba/thumb.gba, a public test ROM kept out of the repository"]
fn thumb_gba_reports_no_failed_tests() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/roms/test/gba/thumb.gba");
    let rom = std::fs::read(path).expect("roms/test/gba/thumb.gba (scripts/fetch-test-roms.sh)");
    let mut gba = Gba::new(Cartridge::new(rom).expect("a public test ROM"));
    for _ in 0..600 {
        gba.run_frame();
    }
    let witness = iwram_witness(&mut gba);
    // The Thumb suite returns to ARM state to print, so the idle loop is an ARM `b .` too.
    let parked = gba.bus.read32(gba.cpu.pc() & !3);
    println!(
        "thumb.gba: {} frames, {} cycles, {} instructions, PC {:08X}, {} SWIs, witness {:?}",
        gba.frames(),
        gba.bus.cycles,
        gba.cpu.instructions,
        gba.cpu.pc(),
        gba.cpu.swi_calls,
        witness
    );
    assert_eq!(
        gba.cpu.swi_calls,
        0,
        "the failure path ran, for test {}",
        failed_test(witness)
    );
    assert_eq!(parked, 0xEAFF_FFFE, "the ROM reached its idle branch");
    assert_eq!(gba.cpu.swi, None, "no BIOS call was left unserviced");
}

/// The twelve bytes the suites leave in IWRAM: hundreds, tens and ones, as words.
fn iwram_witness(gba: &mut Gba) -> [u8; 12] {
    let mut bytes = [0u8; 12];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = gba.bus.read8(IWRAM_BASE + index as u32);
    }
    bytes
}

/// The test number the witness digits encode: `100*h + 10*t + o`.
fn failed_test(witness: [u8; 12]) -> i32 {
    let word = |at: usize| {
        i32::from_le_bytes([
            witness[at],
            witness[at + 1],
            witness[at + 2],
            witness[at + 3],
        ])
    };
    100 * word(0) + 10 * word(4) + word(8)
}

#[test]
#[ignore = "needs roms/test/gba/arm.gba, and a release build to mean anything"]
fn the_core_retires_enough_instructions_to_be_plausible() {
    // The plain numbers the brief asks for. First the whole gate run — 600 frames from boot, which
    // is what the suite test above does — then the suite itself, stepping until it parks on the
    // idle branch, because after that the machine is only spinning `b .` and the rate would be a
    // measurement of the idle loop rather than of the interpreter.
    const IDLE: u32 = 0x0800_1EC4;
    let rom = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/roms/test/gba/arm.gba"
    ))
    .expect("roms/test/gba/arm.gba (scripts/fetch-test-roms.sh)");
    let mut gba = Gba::new(Cartridge::new(rom.clone()).expect("a public test ROM"));

    let started = std::time::Instant::now();
    for _ in 0..600 {
        gba.run_frame();
    }
    let gate = started.elapsed();
    let instructions = gba.cpu.instructions;
    let emulated = gba.bus.cycles as f64 / f64::from(tendy_gba::CLOCK_HZ);
    println!(
        "gate run: 600 frames, {instructions} instructions in {gate:?} — {:.1}M instructions/s, \
         {:.2} host seconds per emulated second ({:.1}x real time)",
        instructions as f64 / gate.as_secs_f64() / 1e6,
        gate.as_secs_f64() / emulated,
        emulated / gate.as_secs_f64(),
    );

    let mut gba = Gba::new(Cartridge::new(rom).expect("a public test ROM"));
    let started = std::time::Instant::now();
    let mut steps = 0u64;
    while gba.cpu.pc() != IDLE {
        gba.step_instruction();
        steps += 1;
        assert!(
            steps < 50_000_000,
            "the suite never reached its idle branch"
        );
    }
    let suite_time = started.elapsed();
    let suite_cycles = gba.bus.cycles;
    let per_second = steps as f64 / suite_time.as_secs_f64();
    println!(
        "the suite itself: {steps} instructions in {suite_time:?} — {:.1}M instructions/s, {:.2} \
         cycles an instruction, {:.1}x real time",
        per_second / 1e6,
        suite_cycles as f64 / steps as f64,
        per_second / f64::from(tendy_gba::CLOCK_HZ),
    );
    assert!(
        per_second > 16_777_216.0,
        "slower than the machine it emulates"
    );
}

/// The summary line prints `BLDCNT`/`BLDY` from the I/O page the renderer itself reads,
/// so a `$00F4` darken at `EVY` 16 — the fade bucket `dn` measured as a black frame — is what the
/// sweep sees, and a register no cartridge wrote is the `$0000` "no effect" case.
#[test]
fn the_effect_state_is_the_bldcnt_and_bldy_the_cartridge_wrote() {
    let mut gba = machine(&[]);
    assert_eq!(
        gba.effect_state(),
        Some(EffectState::new(0, 0)),
        "power-on selects no effect"
    );
    gba.bus.write16(0x0400_0050, 0x00F4);
    gba.bus.write8(0x0400_0054, 16);
    let state = gba.effect_state().expect("an Advance has a colour effect");
    assert_eq!(state.bldcnt, 0x00F4);
    assert_eq!(state.bldy, 16);
    assert_eq!(state.effect, Effect::Darken);
    assert!(
        state.brightness_in_force(),
        "darken at EVY 16 multiplies the picture to nothing"
    );
}
