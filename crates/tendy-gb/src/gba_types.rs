//! The two Advance types the Game Boy's [`System`](crate::system::System) trait names.
//!
//! The trait carries `effect_state` and `swi_trace` so one front end can ask both consoles the same
//! question; both answer with a type that lives in the Advance core. Copying the two type
//! definitions here — they are plain data, no Advance machinery behind them — lets `tendy-gb`
//! compile on its own. `tendy-gba` re-exports these, so there is still exactly one `EffectState` and
//! one `SwiCall` in the workspace.

/// `BLDCNT` bits 6-7: the colour effect the renderer applies to a pixel, named as GBATEK names the
/// four settings (*BLDCNT - Alpha/Brightness Effects*: "00=None, 01=Alpha, 10=Brightness increase,
/// 11=Brightness decrease").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    None,
    Alpha,
    Brighten,
    Darken,
}

impl Effect {
    /// The mode `BLDCNT` bits 6-7 select.
    pub fn from_bldcnt(bldcnt: u16) -> Effect {
        match (bldcnt >> 6) & 3 {
            1 => Effect::Alpha,
            2 => Effect::Brighten,
            3 => Effect::Darken,
            _ => Effect::None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Effect::None => "none",
            Effect::Alpha => "alpha",
            Effect::Brighten => "brighten",
            Effect::Darken => "darken",
        }
    }
}

/// `BLDCNT` and `BLDY` as the core held them when a run stopped, and the mode they select.
///
/// The sweep needs this beside every result: a one-colour frame is a *faded* picture when the
/// brightness register multiplied it out of existence, and only a `blank` when no effect was in
/// force. Reading the registers the renderer read is the difference between reporting documented
/// behaviour and reporting a mystery.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EffectState {
    /// `BLDCNT` (`04000050h`): target bits 0-5 and 8-13, the effect in bits 6-7.
    pub bldcnt: u16,
    /// `BLDY` (`04000054h`): the brightness coefficient, five bits.
    pub bldy: u8,
    /// The mode bits 6-7 select.
    pub effect: Effect,
}

impl EffectState {
    pub fn new(bldcnt: u16, bldy: u8) -> EffectState {
        EffectState {
            bldcnt,
            bldy,
            effect: Effect::from_bldcnt(bldcnt),
        }
    }

    /// `BLDY`'s `EVY` as the renderer uses it: five bits, clamped at 16/16 (GBATEK: "17..31=16/16").
    pub fn evy(self) -> u32 {
        u32::from(self.bldy & 0x1F).min(16)
    }

    /// Whether a brightness effect is in force — `BLDCNT` selecting brighten or darken, at least one
    /// first target selected (bits 0-5), and `EVY` non-zero.
    ///
    /// That is exactly the condition under which the renderer multiplies a pixel by something other
    /// than 1, read off the registers rather than guessed from colour arithmetic.
    pub fn brightness_in_force(self) -> bool {
        matches!(self.effect, Effect::Brighten | Effect::Darken)
            && self.bldcnt & 0x3F != 0
            && self.evy() > 0
    }

    /// The registers and the mode, as the headless run's summary line prints them. `in force` is the
    /// core's own verdict (`brightness_in_force`), so the sweep that reads this line adds no
    /// arithmetic of its own: it records what the machine did.
    pub fn line(self) -> String {
        format!(
            "BLDCNT ${:04X} BLDY {} ({}{})",
            self.bldcnt,
            self.bldy,
            self.effect.name(),
            if self.brightness_in_force() {
                ", in force"
            } else {
                ""
            }
        )
    }
}

/// One recorded software interrupt, as `--trace-swi` reports it: the number the dispatcher decoded,
/// the caller's `R0`-`R3`, the caller's `LR` and the return address the core had advanced `R15` to.
///
/// `ret` is the address after the `SWI` (`R15` is already past the instruction when the call runs),
/// and `lr` is what the caller left in the link register — the pair is what a reader needs to find
/// the call site in a disassembly. `refused` says this call was the one where the dispatcher had no
/// implementation for the number — a refusal is recorded once per run, and it halts the CPU for that
/// call, which a V-Blank interrupt then wakes, so a title that calls a refused entry every frame
/// keeps calling it: the record flags the first only. Runtime-only: a trace is how a session is
/// measured, not what the session is, so a save state does not carry it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwiCall {
    pub number: u32,
    pub args: [u32; 4],
    pub lr: u32,
    pub ret: u32,
    pub refused: bool,
}
