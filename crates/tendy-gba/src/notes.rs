//! The sentences a Game Boy Advance gives for front-end features that are Game Boy only (G7), and
//! the one word table the front end prints for the Advance's CPU mode (H3).
//!
//! The card for this leg named two — save states and cheats — and the first turned out not to
//! apply: the Advance's own state container (`GBAG`, version 9) carries the whole machine, and a
//! state taken mid-frame on a real cartridge resumes to the same checksum, so `F5`/`F7` work and
//! the drawer offers them. Two of the rest went the same way in H3: the recorder's encoders now
//! take the frame's own size and colour, so `F9` records an Advance, and the debugger panel is
//! console-aware, so only its three PPU views have nothing to draw. What genuinely does nothing on
//! an Advance is the cheat codes, whose encodings name a Game Boy address.
//!
//! Each sentence is written once, here, so the drawer's row and the key's notice cannot drift
//! apart, and each names the reason and what does work instead. They are one line each because
//! that is what the status bar shows.

/// `Game > Cheats`, and any code that reaches `System::apply_cheats` on this console.
pub const CHEATS: &str = "Cheat codes are Game Boy only: they encode a Game Boy address, and an Advance cartridge has none in this build.";

/// The debugger panel's Tiles, Map and OAM views, reached with `Tab` on an Advance: each draws a
/// Game Boy PPU's memory, and an Advance has none of it in this build. The Memory view and the
/// register header are the Advance's own (H3).
pub const DEBUGGER_VIEW: &str =
    "Tiles, Map and OAM read the Game Boy's PPU memory, so an Advance shows only the Memory view.";

/// The Advance's processor mode as `--trace-pc` and the panel's header print it.
///
/// GBATEK, *ARM7TDMI Processor*: `CPSR` bits 0-4 hold the mode, and the seven values are the seven
/// names below. Anything else is not a mode the ARM7TDMI defines, and saying so is better than
/// printing a number the reader has to look up.
pub fn mode_name(mode: u32) -> &'static str {
    match mode {
        crate::cpu::MODE_USR => "usr",
        crate::cpu::MODE_FIQ => "fiq",
        crate::cpu::MODE_IRQ => "irq",
        crate::cpu::MODE_SVC => "svc",
        crate::cpu::MODE_ABT => "abt",
        crate::cpu::MODE_UND => "und",
        crate::cpu::MODE_SYS => "sys",
        _ => "none",
    }
}
