//! `$04000120`-`$0400015A`: the link port — its registers, the transfer engine, and the
//! multiplayer protocol.
//!
//! **The registers** are the I/O page's own bytes: `SIODATA32`/`SIOMULTI0-3` at `$120-$126`,
//! `SIOCNT` at `$128`, `SIOMLT_SEND`/`SIODATA8` at `$12A`, `RCNT` at `$134`, and the JOY bus's
//! `JOYCNT`, `JOY_RECV`, `JOY_TRANS` and `JOYSTAT` at `$140`, `$150`, `$154` and `$158` (GBATEK,
//! *GBA I/O Map*). Storage lives in [`crate::bus::Bus::io`] and therefore in the save state,
//! which is why this slice adds no field to the Advance snapshot; this module owns what the bits
//! *mean*, and the bits that no store ever writes.
//!
//! **The transfer engine** is here, and it is one clock: a normal-mode transfer shifts 8 or 32
//! bits at the selected rate and a multiplayer transfer clocks up to four 16-bit slots, and in
//! both cases the transfer ends at a cycle the unit can work out when it starts. Nothing waits on
//! anything.
//!
//! **The cable** is a [`LinkPort`]: one method that trades a value with the other side and one
//! that says what the cable knows about itself. [`SocketPort`] wears
//! [`tendy_gb::link`]'s existing byte transport (the same `Link`, the same threads, the same two
//! channels) with the framing a transfer needs — a Game Boy transfer is exactly one byte, so that
//! module never had to say how wide a transfer is; the Advance's is one, two or four. [`bench`] is
//! the in-process cable the tests below use, and is what makes a three-unit multiplayer transfer
//! provable without three windows.
//!
//! **What the cable cannot carry** is the clock: a byte stream has no SC line, so a slave times
//! its own busy bit with its own rate selection even though GBATEK says that selection is ignored
//! on hardware ("the transfer rate is supplied by the remote GBA"), and every unit has to arm
//! itself rather than being clocked by the parent. Both are departures from the hardware.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::CLOCK_HZ;
use crate::bus::{CYCLES_PER_FRAME, IO_SIZE};
use tendy_gb::link::{Link, LinkStatus};

/// `$04000120`: `SIODATA32`'s low half in normal 32-bit mode, `SIOMULTI0` in multiplayer mode.
pub const SIODATA32: usize = 0x120;
/// `$04000120`: the parent's slot.
pub const SIOMULTI0: usize = 0x120;
/// `$04000122`: the first child's slot (also `SIODATA32`'s high half).
pub const SIOMULTI1: usize = 0x122;
/// `$04000124`: the second child's slot.
pub const SIOMULTI2: usize = 0x124;
/// `$04000126`: the third child's slot.
pub const SIOMULTI3: usize = 0x126;
/// `$04000128`: the control register.
pub const SIOCNT: usize = 0x128;
/// `$0400012A`: the multiplayer send register.
pub const SIOMLT_SEND: usize = 0x12A;
/// `$0400012A`: the normal 8-bit data register, the same bytes as [`SIOMLT_SEND`].
pub const SIODATA8: usize = 0x12A;
/// `$04000134`: the mode selection register.
pub const RCNT: usize = 0x134;
/// `$04000140`: the JOY bus control register.
pub const JOYCNT: usize = 0x140;
/// `$04000150`: the JOY bus receive register.
pub const JOY_RECV: usize = 0x150;
/// `$04000154`: the JOY bus send register.
pub const JOY_TRANS: usize = 0x154;
/// `$04000158`: the JOY bus receive status register.
pub const JOYSTAT: usize = 0x158;

/// `SIOCNT`'s high byte.
const SIOCNT_HI: usize = SIOCNT + 1;
/// `SIOMLT_SEND`'s high byte.
const SIOMLT_SEND_HI: usize = SIOMLT_SEND + 1;
/// `RCNT`'s high byte.
const RCNT_HI: usize = RCNT + 1;
/// The last byte of `JOY_RECV`.
const JOY_RECV_HI: usize = JOY_RECV + 3;
/// The last byte of `JOY_TRANS`.
const JOY_TRANS_HI: usize = JOY_TRANS + 3;

/// `SIOCNT` bit 0, normal mode: the shift clock is this unit's (master) rather than the peer's.
pub const SC_INTERNAL: u16 = 1 << 0;
/// `SIOCNT` bit 1, normal mode: the 2 MHz shift clock instead of the 256 KHz one.
pub const SC_FAST: u16 = 1 << 1;
/// `SIOCNT` bit 2, normal mode: the SI line is high — the peer is not holding it low. (In
/// multiplayer mode bit 2 is the SI terminal: 0 for the parent, 1 for a child.)
pub const SI_HIGH: u16 = 1 << 2;
/// `SIOCNT` bit 3, normal mode: SO is high while no transfer runs (0 = low).
pub const SO_HIGH: u16 = 1 << 3;
/// `SIOCNT` bit 3, multiplayer mode: SD is high — every unit on the cable is in multiplayer mode
/// and ready (0 = bad connection). The same bit as [`SO_HIGH`], with the other mode's meaning.
pub const SD_READY: u16 = 1 << 3;
/// `SIOCNT` bit 7: a transfer is running. Read-only; a store of 1 starts one, and the unit clears
/// it when the transfer completes.
pub const START: u16 = 1 << 7;
/// `SIOCNT` bit 12, normal mode: 32 bits a transfer instead of 8. (Normal mode has no 16-bit
/// length: GBATEK, *4000128h - SIOCNT ... NORMAL Mode*, gives bits 0 and 1 of the length field as
/// "0=8bit, 1=32bit", and sets bits 0 and 1 of the mode field to "0=8bit, 1=32bit".)
pub const LENGTH32: u16 = 1 << 12;
/// `SIOCNT` bit 13: multiplayer mode rather than normal mode.
pub const MULTI_MODE: u16 = 1 << 13;
/// `SIOCNT` bit 14: raise the serial interrupt when the transfer completes.
pub const IRQ_ENABLE: u16 = 1 << 14;
/// `SIOCNT` bit 6, multiplayer mode: a unit did not answer (0 = all answered).
pub const MULTI_ERROR: u16 = 1 << 6;

/// Cycles a bit takes at 256 KHz: 16 777 216 / 262 144 = 64, exactly.
pub const CYCLES_PER_BIT_SLOW: u32 = 64;
/// Cycles a bit takes at 2 MHz: 16 777 216 / 2 097 152 = 8, exactly.
pub const CYCLES_PER_BIT_FAST: u32 = 8;
/// The four multiplayer baud rates `SIOCNT` bits 0-1 select (GBATEK, *SIOCNT ... MULTI-PLAYER
/// Mode*: "0-3: 9600,38400,57600,115200 bps").
pub const MULTI_BAUD: [u32; 4] = [9_600, 38_400, 57_600, 115_200];
/// Bits one unit's multiplayer word takes: 16 data plus a start and a stop bit (GBATEK,
/// *Transmission Time*: "16 data bits plus start/stop bits for each GBA").
pub const MULTI_BITS_PER_UNIT: u32 = 18;
/// Bits between two units' words. GBATEK's table counts one delay per gap and does not say how
/// long it is, so this machine spends one bit on it.
pub const MULTI_GAP_BITS: u32 = 1;
/// Bits a unit has to start answering in before the parent gives up on it. GBATEK: "The average
/// Delay and Timeout periods are unknown?", so this machine spends one unit's word — the only
/// interval the protocol itself defines — and the report says so.
pub const MULTI_TIMEOUT_BITS: u32 = 18;

/// A normal-mode transfer waits two frames for the peer's value before completing with ones, the
/// same two-frame budget `tendy_gb::serial` gives the Game Boy's cable: one frame is long enough that
/// a peer a frame behind still answers and short enough that a dead one stutters rather than
/// stalls the game. 561 792 cycles, 33.5 ms.
pub const LINK_TIMEOUT_CYCLES: u32 = 2 * CYCLES_PER_FRAME;
/// The cable is asked for the answer once every this many cycles while a transfer waits: 4 391
/// polls a frame at the outside, and none at all while the shift is running.
const POLL_EVERY: u32 = 128;

/// How many bits a `SIOCNT` length field selects in normal mode.
pub const fn normal_bits(siocnt: u16) -> u8 {
    if siocnt & LENGTH32 != 0 { 32 } else { 8 }
}

/// Cycles a normal-mode transfer takes at the rate `siocnt` selects.
pub fn normal_cycles(siocnt: u16, bits: u8) -> u32 {
    let per_bit = if siocnt & SC_FAST != 0 {
        CYCLES_PER_BIT_FAST
    } else {
        CYCLES_PER_BIT_SLOW
    };
    u32::from(bits) * per_bit
}

/// Cycles a multiplayer transfer of `units` units takes at `baud`: 18 bits a unit, a bit between
/// units, and one unit's word as the timeout when fewer than four are on the cable (GBATEK's
/// table, with this machine's answer for the delay and timeout it does not publish).
pub fn multi_cycles(baud: u32, units: usize) -> u32 {
    let units = units.clamp(1, 4) as u32;
    let bits = MULTI_BITS_PER_UNIT * units
        + MULTI_GAP_BITS * units.saturating_sub(1)
        + if units < 4 { MULTI_TIMEOUT_BITS } else { 0 };
    let cycles = u64::from(bits) * u64::from(CLOCK_HZ) / u64::from(baud);
    cycles as u32
}

/// What the engine asks the cable on one poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// This unit's value goes onto the cable: `bits` of it — 8 or 32 in normal mode, 16 in
    /// multiplayer mode, which is the only mode that has a 16-bit unit.
    Put { out: u32, bits: u8 },
    /// Look for the answer to the transfer that `Put` started, without waiting for it.
    Take,
}

/// What one transfer brought back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wire {
    /// The 16-bit values in slot order: slot 0 is the parent's, slot 1 the first child's. A normal
    /// transfer uses slot 0 for the low half of the answer and slot 1 for the high half.
    pub slots: [u16; 4],
    /// How many units answered, so an empty slot is not mistaken for a unit that sent `FFFFh`.
    pub units: usize,
}

/// What the cable says about itself, which is all `SIOCNT`'s line bits report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    /// How many units are on this cable, this one included. 1 with nothing plugged in.
    pub units: usize,
    /// Whether every other unit has put its value on the cable — the state a master waits for
    /// before it starts, and what SI reads.
    pub ready: bool,
    /// Whether every unit is in multiplayer mode, which is what the SD line reports there.
    pub sd: bool,
}

impl Default for Lines {
    fn default() -> Lines {
        Lines {
            units: 1,
            ready: false,
            sd: false,
        }
    }
}

/// One end of a link cable, as the serial hardware sees it.
///
/// [`LinkPort::trade`] is the whole data path: the card's "trade a value with the other side,
/// return theirs". It never blocks — a transport that has nothing yet says so, and the engine's
/// own clock decides when that has become too late — and it is called once per machine tick, so a
/// socket's latency costs the emulator a poll and never a wait.
pub trait LinkPort: Send {
    /// `Put` writes this unit's value onto the cable; `Take` brings the other side's back once all
    /// of it has arrived. `None` from `Take` means "not yet", never "never".
    fn trade(&mut self, ask: Ask) -> Option<Wire>;
    /// How many units are on the cable and whether they are ready. The hardware learns both from
    /// SI/SD; a transport has to be asked.
    fn lines(&mut self) -> Lines;
}

/// A handle to one end of the cable, shared by every clone of the machine — as [`tendy_gb::link::Link`]
/// is for the Game Boy's: `Gba` decodes a state by cloning, and a clone that unplugged the cable
/// would drop the other player.
#[derive(Clone)]
pub struct Cable {
    inner: Arc<Mutex<Box<dyn LinkPort>>>,
}

impl Cable {
    /// Wrap a port in a cable.
    pub fn new(port: impl LinkPort + 'static) -> Cable {
        Cable {
            inner: Arc::new(Mutex::new(Box::new(port))),
        }
    }

    /// The port, with a poisoned lock taken rather than panicked on: one failed send on a dead
    /// socket must not take the machine down.
    fn port(&self) -> MutexGuard<'_, Box<dyn LinkPort>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The localhost cable: [`tendy_gb::link`]'s byte transport, wearing the framing a transfer needs.
///
/// That module moves one byte a transfer because a Game Boy transfer *is* one byte, so it never
/// had to say how wide a transfer is. An Advance transfer is one, two or four bytes, and both ends
/// know the width from their own `SIOCNT` before they start — both must set the same mode for the
/// protocol to work, which is what "read SIOCNT bit 3 to verify that all GBAs are in Multi-Player
/// mode" is about — so the count is kept here rather than put on the wire.
pub struct SocketPort {
    link: Link,
    /// This unit's slot on the cable: 0 for the parent (the host), 1 for a child (player 2).
    slot: usize,
    /// Bytes this transfer moves, and the bytes of the answer that have arrived. The buffer is a
    /// short queue rather than one transfer's worth, because a slave's value often arrives before
    /// the master has finished its own shift and must survive until the master asks for it.
    want: usize,
    got: [u8; 8],
    have: usize,
}

impl SocketPort {
    /// Player `slot`'s end of a socket cable.
    pub fn new(link: Link, slot: usize) -> SocketPort {
        SocketPort {
            link,
            slot: slot.min(3),
            want: 0,
            got: [0; 8],
            have: 0,
        }
    }

    /// Take whatever the peer has sent into the queue, without waiting.
    fn drain(&mut self) {
        while self.have < self.got.len() {
            match self.link.try_recv() {
                Some(byte) => {
                    self.got[self.have] = byte;
                    self.have += 1;
                }
                None => break,
            }
        }
    }
}

impl LinkPort for SocketPort {
    fn trade(&mut self, ask: Ask) -> Option<Wire> {
        match ask {
            Ask::Put { out, bits } => {
                self.want = usize::from(bits) / 8;
                for byte in &out.to_le_bytes()[..self.want] {
                    self.link.send(*byte);
                }
                None
            }
            Ask::Take => {
                self.drain();
                if self.want == 0 || self.have < self.want {
                    return None;
                }
                let mut value = [0u8; 4];
                value[..self.want].copy_from_slice(&self.got[..self.want]);
                self.got.copy_within(self.want.., 0);
                self.have -= self.want;
                let mut slots = [0xFFFFu16; 4];
                if self.want == 2 {
                    // A pair's multiplayer transfer: the peer's halfword is the slot this unit is
                    // not, and both ends read the same SIOMULTI registers.
                    slots[1 - self.slot] = u16::from_le_bytes([value[0], value[1]]);
                } else {
                    let whole = u32::from_le_bytes(value);
                    slots[0] = whole as u16;
                    slots[1] = (whole >> 16) as u16;
                }
                Some(Wire {
                    slots,
                    units: self.lines().units,
                })
            }
        }
    }

    /// Two units while the socket is up, this one alone while it is not. The byte transport
    /// carries no SO or SD line, so "ready" is the peer having put something on the wire at all.
    fn lines(&mut self) -> Lines {
        self.drain();
        let connected = self.link.status() == LinkStatus::Connected;
        Lines {
            units: if connected { 2 } else { 1 },
            ready: connected && self.have > 0,
            sd: connected,
        }
    }
}

/// One end of the in-process cable [`bench`] makes.
struct BenchUnit {
    shared: Arc<Mutex<BenchState>>,
    me: usize,
}

impl LinkPort for BenchUnit {
    fn trade(&mut self, ask: Ask) -> Option<Wire> {
        let mut state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        match ask {
            Ask::Put { out, bits } => {
                state.bits = bits;
                state.values[self.me] = Some(out);
                state.round();
                None
            }
            Ask::Take => state.answers[self.me].take(),
        }
    }

    fn lines(&mut self) -> Lines {
        let state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        Lines {
            units: state.units,
            ready: state.ready(self.me),
            sd: state.units > 1,
        }
    }
}

/// What the units of a [`bench`] cable share.
struct BenchState {
    units: usize,
    bits: u8,
    values: [Option<u32>; 4],
    answers: [Option<Wire>; 4],
}

impl BenchState {
    /// Everyone else has put a value: what SI reports, and what a master waits for.
    fn ready(&self, me: usize) -> bool {
        self.units > 1 && (0..self.units).all(|unit| unit == me || self.values[unit].is_some())
    }

    /// One clock of the cable: when every unit has put its value, each unit's answer is filled in.
    fn round(&mut self) {
        if !(0..self.units).all(|unit| self.values[unit].is_some()) {
            return;
        }
        let shared = if self.bits == 16 {
            // Multiplayer: slot `n` is unit `n`'s halfword, and every unit reads the same four.
            let mut slots = [0xFFFFu16; 4];
            for (slot, value) in slots.iter_mut().zip(self.values).take(self.units) {
                *slot = value.unwrap_or(0xFFFF) as u16;
            }
            Wire {
                slots,
                units: self.units,
            }
        } else {
            Wire {
                slots: [0; 4],
                units: self.units,
            }
        };
        for unit in 0..self.units {
            let wire = if self.bits == 16 {
                shared
            } else {
                // Normal mode: GBATEK has it "used to communicate between two units", and each
                // end ends up holding the other's value.
                let peer = if self.units == 2 {
                    1 - unit
                } else {
                    (unit + 1) % self.units
                };
                let value = self.values[peer].unwrap_or(0xFFFF_FFFF);
                Wire {
                    slots: [value as u16, (value >> 16) as u16, 0, 0],
                    units: self.units,
                }
            };
            self.answers[unit] = Some(wire);
        }
        self.values = [None; 4];
    }
}

/// An in-process cable shared by up to four units: no socket, no threads, nothing that can block.
///
/// Each unit gets its own end. A unit's `Put` records its value and completes the round once every
/// unit has put — one clock of the real cable — after which each end's `Take` hands over the same
/// four slots. A unit that never puts is exactly the silent peer the engine's own clock gives up
/// on.
pub fn bench(units: usize) -> Vec<Cable> {
    let units = units.clamp(1, 4);
    let shared = Arc::new(Mutex::new(BenchState {
        units,
        bits: 8,
        values: [None; 4],
        answers: [None; 4],
    }));
    (0..units)
        .map(|me| {
            Cable::new(BenchUnit {
                shared: shared.clone(),
                me,
            })
        })
        .collect()
}

/// What the cable has to say when it says anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    /// The peer went away mid-game: the port behaves cable-less from here and the player is told
    /// once.
    PeerGone,
    /// A transfer waited two frames for the peer's value and completed with ones instead.
    PeerSilent,
}

impl Message {
    /// The status bar's text.
    pub fn text(self) -> &'static str {
        match self {
            Message::PeerGone => "link: the other player disconnected",
            Message::PeerSilent => "link: the other player stopped answering",
        }
    }
}

/// One transfer that crossed the cable, as the transcript prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Crossed {
    /// The frame the transfer completed on: the LCD's frame counter at that cycle.
    pub frame: u64,
    /// The master cycle the transfer completed on, which is exact: the bus stops its clock at the
    /// instant the unit names.
    pub cycle: u64,
    /// Bits moved: 8, 16 or 32.
    pub bits: u8,
    /// What this unit sent.
    pub out: u32,
    /// What came back, in slot order. A normal transfer uses the first two slots as the low and
    /// high halves of its answer.
    pub inp: [u16; 4],
    /// How many units answered.
    pub units: usize,
}

/// The transfer that is running, if one is.
#[derive(Clone)]
struct Run {
    mode: Mode,
    /// What this unit put on the cable.
    out: u32,
    /// Cycles the transfer has run.
    clocked: u32,
    /// Cycles until the unit next looks at the cable. Never zero while the transfer runs, so the
    /// clock always has a place to stop.
    left: u32,
    /// Units the cable declared when the transfer started, this one included.
    units: usize,
    /// Cycles the transfer lasts, for a multiplayer clock (18 bits a unit, GBATEK's table).
    total: u32,
    /// The answer, once the cable hands it over.
    wire: Option<Wire>,
    /// Cycles spent waiting for a normal-mode answer, against [`LINK_TIMEOUT_CYCLES`].
    waited: u32,
    /// Whether the shift itself is over (normal mode): from here the unit only looks for the
    /// answer.
    shifted: bool,
}

/// What the transfer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Normal mode: `bits` wide (8 or 32), with `master` saying whose clock drives it.
    Normal { bits: u8, master: bool },
    /// Multiplayer mode: 16 bits a unit at `baud`.
    Multi { baud: u32 },
}

/// The link port: the registers' meaning, the transfer in flight, and what crossed.
#[derive(Clone)]
pub struct Serial {
    /// The cable, `None` while `Link > Role` is `Off`. Session state, never saved.
    port: Option<Cable>,
    /// This unit's slot on the cable: 0 is the parent. Session state, never saved.
    slot: usize,
    /// The transfer in flight, if any. A transfer is never saved: a state never holds one.
    run: Option<Run>,
    /// Every transfer that completed, in order, with the frame it finished on. Capped, and never
    /// part of a save state.
    pub transcript: Vec<Crossed>,
    /// What the cable last had to say. The front end takes it and shows it once.
    pub message: Option<Message>,
    /// Whether the multiplayer ID bits mean anything yet: GBATEK, *SIOCNT ... MULTI-PLAYER Mode*,
    /// "The ID Bits are undefined until the first transfer has completed."
    id_known: bool,
}

/// How many transfer lines the transcript keeps: a session's worth of crossings for a report, and
/// nothing that grows without end in a long game.
const TRANSCRIPT_CAP: usize = 4096;

impl Serial {
    /// Power-on: no cable, no transfer, `SIOCNT` and the rest at the boot sequence's zeros.
    pub fn new() -> Serial {
        Serial {
            port: None,
            slot: 0,
            run: None,
            transcript: Vec::new(),
            message: None,
            id_known: false,
        }
    }

    /// Plug the cable in as unit `slot` of it: 0 is the parent (the host), 1-3 are children.
    pub fn attach(&mut self, port: Cable, slot: usize) {
        self.port = Some(port);
        self.slot = slot.min(3);
        self.run = None;
        self.message = None;
        self.id_known = false;
    }

    /// Pull the cable out. A transfer in flight is abandoned with it.
    pub fn detach(&mut self) {
        self.port = None;
        self.run = None;
        self.message = None;
        self.id_known = false;
    }

    /// The cable, for a reset that must not unplug the other player.
    pub fn cable(&self) -> Option<Cable> {
        self.port.clone()
    }

    /// This unit's slot, for the same reason.
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// What the cable says about itself, or a lone unit's answer when there is no cable.
    fn lines(&mut self) -> Lines {
        match &mut self.port {
            Some(port) => {
                let mut guard = port.port();
                guard.lines()
            }
            None => Lines::default(),
        }
    }

    /// Ask the cable for the answer to the running transfer. Never waits: a transport with nothing
    /// to give says so.
    fn poll(&mut self) -> Option<Wire> {
        match &mut self.port {
            Some(port) => {
                let mut guard = port.port();
                guard.trade(Ask::Take)
            }
            None => None,
        }
    }

    /// Whether a transfer is running, which is all `SIOCNT`'s busy bit is.
    pub fn busy(&self) -> bool {
        self.run.is_some()
    }

    /// Whether this unit's clock drives the running transfer: normal mode with `SIOCNT` bit 0 set.
    /// A slave's window is its own nominal clock here, because a byte stream carries none (see the
    /// module documentation).
    pub fn driving_clock(&self) -> bool {
        matches!(
            self.run,
            Some(Run {
                mode: Mode::Normal { master: true, .. },
                ..
            })
        )
    }

    /// Cycles until the unit next has to do something, or `None` while it is idle. The bus stops
    /// its clock here so a transfer completes on the cycle it is due rather than at the end of
    /// whatever instruction was executing, and a halted CPU wakes on it.
    pub fn cycles_to_deadline(&self) -> Option<u32> {
        self.run.as_ref().map(|run| run.left.max(1))
    }

    /// One slice of the master clock, which starts on master cycle `start`. Returns true when the
    /// transfer that just completed asked for an interrupt (`SIOCNT` bit 14), which the bus raises
    /// as request 7.
    pub fn tick(&mut self, io: &mut [u8; IO_SIZE], cycles: u32, start: u64, frame: u64) -> bool {
        let mut budget = cycles;
        let mut spent = 0u32;
        while budget > 0 {
            let Some(mut run) = self.run.take() else {
                return false;
            };
            if run.left > budget {
                run.left -= budget;
                run.clocked += budget;
                self.run = Some(run);
                return false;
            }
            budget -= run.left;
            run.clocked += run.left;
            spent += run.left;
            run.left = 0;
            if self.event(&mut run) {
                return self.finish(io, run, frame, start + u64::from(spent));
            }
            self.run = Some(run);
        }
        false
    }

    /// The clock reached the instant `run` was waiting for. Returns true when the transfer is over.
    fn event(&mut self, run: &mut Run) -> bool {
        // A peer that left the cable takes the transfer with it, and the player is told once. The
        // check is only made while a cable is plugged in: with none, nothing can leave.
        if self.port.is_some() && self.lines().units < run.units {
            self.message = Some(Message::PeerGone);
            return true;
        }
        match run.mode {
            Mode::Normal { .. } => {
                if !run.shifted {
                    // The last bit is out. The answer is whatever the cable has now; the shift
                    // itself never waits for it, so an in-process peer and a prompt socket both
                    // complete on exactly the cycle the clock says.
                    run.shifted = true;
                    run.wire = self.poll();
                    if run.wire.is_some() {
                        return true;
                    }
                    // Nothing plugged in at all: the line reads high and the shift is the whole
                    // transfer, with no two-frame wait for a peer that was never there.
                    if run.units < 2 {
                        return true;
                    }
                    run.left = POLL_EVERY;
                    return false;
                }
                run.wire = self.poll();
                if run.wire.is_some() {
                    return true;
                }
                run.waited += POLL_EVERY;
                if run.waited >= LINK_TIMEOUT_CYCLES {
                    return true;
                }
                run.left = POLL_EVERY;
                false
            }
            Mode::Multi { baud } => {
                // The parent clocks its own word, then a word a unit; a slot boundary is where a
                // unit's answer can leave or arrive. The answer is taken once: a later poll must
                // not overwrite the wire the cable already handed over.
                if run.wire.is_none() {
                    run.wire = self.poll();
                }
                if run.clocked >= run.total {
                    return true;
                }
                run.left = run.total - run.clocked;
                if let Some(next) = slot_boundary(baud, run.clocked, run.total) {
                    run.left = run.left.min(next);
                }
                false
            }
        }
    }

    /// The transfer is over: put the answer where the cartridge reads it, say what crossed, and
    /// report whether an interrupt is owed.
    fn finish(&mut self, io: &mut [u8; IO_SIZE], run: Run, frame: u64, cycle: u64) -> bool {
        let siocnt = u16::from_le_bytes([io[SIOCNT], io[SIOCNT + 1]]);
        let units;
        let inp;
        match run.mode {
            Mode::Normal { bits, .. } => {
                let value = match run.wire {
                    Some(wire) => {
                        units = wire.units;
                        if bits == 32 {
                            u32::from(wire.slots[0]) | (u32::from(wire.slots[1]) << 16)
                        } else {
                            u32::from(wire.slots[0] as u8)
                        }
                    }
                    // Nothing answered: the line reads high, so the register holds ones — the
                    // same `$FF` a Game Boy's shift register ends at off a dead line.
                    None => {
                        if run.units > 1 {
                            units = 0;
                            if self.message.is_none() {
                                self.message = Some(Message::PeerSilent);
                            }
                        } else {
                            units = 1;
                        }
                        if bits == 32 { u32::MAX } else { 0xFF }
                    }
                };
                if bits == 32 {
                    io[SIODATA32..SIODATA32 + 4].copy_from_slice(&value.to_le_bytes());
                    inp = [value as u16, (value >> 16) as u16, 0, 0];
                } else {
                    io[SIODATA8] = value as u8;
                    io[SIODATA8 + 1] = 0;
                    inp = [value as u16, 0, 0, 0];
                }
            }
            Mode::Multi { .. } => {
                let (slots, answered) = match run.wire {
                    Some(wire) => (wire.slots, wire.units),
                    None => ([0xFFFFu16; 4], 0),
                };
                units = answered;
                // The local send value is always in the local slot, answered or not: GBATEK,
                // *SIOMULTI0-3* — "as well as the local outgoing SIOMLT_SEND data".
                let mut slots = slots;
                slots[self.slot] = run.out as u16;
                for (slot, value) in slots.iter().enumerate() {
                    let at = SIOMULTI0 + 2 * slot;
                    io[at..at + 2].copy_from_slice(&value.to_le_bytes());
                }
                // A unit that did not answer is the error bit, and the slots it would have filled
                // stay FFFFh.
                if answered < run.units {
                    io[SIOCNT] |= MULTI_ERROR as u8;
                }
                self.id_known = true;
                inp = slots;
            }
        }
        if self.transcript.len() < TRANSCRIPT_CAP {
            let bits = match run.mode {
                Mode::Normal { bits, .. } => bits,
                Mode::Multi { .. } => 16,
            };
            self.transcript.push(Crossed {
                frame,
                cycle,
                bits,
                out: run.out,
                inp,
                units,
            });
        }
        siocnt & IRQ_ENABLE != 0
    }

    /// The register byte the cartridge reads, or the I/O page's storage for a byte this unit only
    /// keeps.
    pub fn read8(&mut self, io: &mut [u8; IO_SIZE], offset: usize) -> u8 {
        match offset {
            SIOCNT => self.siocnt(io) as u8,
            SIOCNT_HI => (self.siocnt(io) >> 8) as u8,
            RCNT => self.rcnt(io) as u8,
            RCNT_HI => (self.rcnt(io) >> 8) as u8,
            JOY_RECV..=JOY_RECV_HI => {
                // GBATEK, *JOYSTAT*: "Bit 3 is automatically reset when reading from local
                // JOY_RECV."
                io[JOYSTAT] &= !0b1000;
                io[offset]
            }
            _ => io[offset],
        }
    }

    /// One byte of the serial block. A store that starts a transfer arms the engine; nothing here
    /// completes one, because a store has no clock.
    pub fn write8(&mut self, io: &mut [u8; IO_SIZE], offset: usize, value: u8) {
        match offset {
            SIOCNT => {
                let old = u16::from_le_bytes([io[SIOCNT], io[SIOCNT + 1]]);
                self.write_siocnt(io, u16::from_le_bytes([value, old.to_le_bytes()[1]]));
            }
            SIOCNT_HI => {
                let old = u16::from_le_bytes([io[SIOCNT], io[SIOCNT + 1]]);
                self.write_siocnt(io, u16::from_le_bytes([old.to_le_bytes()[0], value]));
            }
            SIOMLT_SEND | SIOMLT_SEND_HI => {
                let mut half = u16::from_le_bytes([io[SIOMLT_SEND], io[SIOMLT_SEND + 1]]);
                if offset == SIOMLT_SEND {
                    half = (half & 0xFF00) | u16::from(value);
                } else {
                    half = (half & 0x00FF) | (u16::from(value) << 8);
                }
                io[SIOMLT_SEND..SIOMLT_SEND + 2].copy_from_slice(&half.to_le_bytes());
                // The register is a latch the start bit samples, not a transmit queue: GBATEK's
                // procedure writes here and *then* sets the start bit. `start` puts the value on
                // the cable, at the instant the hardware would sample it, so a store — including
                // the first of two — must not. A socket queues bytes in order, and an early put
                // would hand the peer the previous write (REVIEW.md, N1).
            }
            RCNT | RCNT_HI => io[offset] = value,
            JOYCNT => {
                // Bits 0-2 are acknowledged by writing a 1, as `IF`'s are; bit 6 is the reset
                // command's interrupt enable (GBATEK, *4000140h - JOYCNT*).
                let mut byte = io[JOYCNT];
                byte &= !(value & 0b111);
                byte |= value & 0b0100_0000;
                io[JOYCNT] = byte;
            }
            JOY_TRANS..=JOY_TRANS_HI => {
                io[offset] = value;
                // GBATEK, *JOYSTAT*: "Bit 1 is automatically set when writing to local JOY_TRANS."
                io[JOYSTAT] |= 0b10;
            }
            _ => io[offset] = value,
        }
    }

    /// A halfword store to a register whose two bytes are one write: `SIOCNT`'s start bit must not
    /// run before its mode bits have landed, and a `SIOMLT_SEND` halfword is one value on the
    /// cable rather than two.
    pub fn write16(&mut self, io: &mut [u8; IO_SIZE], offset: usize, value: u16) {
        match offset {
            SIOCNT => self.write_siocnt(io, value),
            SIOMLT_SEND => {
                // A store, not a put: see the byte store above and `start`, which is what the
                // hardware samples when the start bit is written.
                io[SIOMLT_SEND..SIOMLT_SEND + 2].copy_from_slice(&value.to_le_bytes());
            }
            _ => {
                let bytes = value.to_le_bytes();
                self.write8(io, offset, bytes[0]);
                self.write8(io, offset + 1, bytes[1]);
            }
        }
    }

    /// `SIOCNT`: bits 15, 13 and the mode's must-be-zero ones read back as the hardware documents,
    /// bits 2-7 are live, and a store of the start bit starts a transfer.
    fn write_siocnt(&mut self, io: &mut [u8; IO_SIZE], value: u16) {
        // Bit 15 is "Not used (Read only, always 0)" in both modes, and bit 6 is the multiplayer
        // error flag, which a store cannot set.
        let value = value & 0x7FBF;
        io[SIOCNT..SIOCNT + 2].copy_from_slice(&value.to_le_bytes());
        if value & START == 0 {
            // Clearing the start bit stops whatever is running, as it does on the Game Boy's port.
            self.run = None;
            return;
        }
        if self.run.is_some() {
            return;
        }
        self.start(io, value);
    }

    /// The start bit was written: arm the transfer and put this unit's value on the cable.
    fn start(&mut self, io: &mut [u8; IO_SIZE], siocnt: u16) {
        if siocnt & MULTI_MODE != 0 {
            let baud = MULTI_BAUD[usize::from(siocnt & 0b11)];
            let units = self.lines().units.max(1);
            // GBATEK, *SIOMULTI0-3*: "These registers are automatically reset to FFFFh upon
            // transfer start."
            for slot in 0..4 {
                let at = SIOMULTI0 + 2 * slot;
                io[at..at + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
            }
            let out = u32::from(u16::from_le_bytes([io[SIOMLT_SEND], io[SIOMLT_SEND + 1]]));
            self.put(out, 16);
            // The error bit belongs to the transfer that is starting, not the last one.
            io[SIOCNT] &= !(MULTI_ERROR as u8);
            let total = multi_cycles(baud, units);
            self.id_known = false;
            self.run = Some(Run {
                mode: Mode::Multi { baud },
                out,
                clocked: 0,
                left: total,
                units,
                total,
                wire: None,
                waited: 0,
                shifted: false,
            });
            return;
        }
        let bits = normal_bits(siocnt);
        let master = siocnt & SC_INTERNAL != 0;
        let out = if bits == 32 {
            u32::from_le_bytes([
                io[SIODATA32],
                io[SIODATA32 + 1],
                io[SIODATA32 + 2],
                io[SIODATA32 + 3],
            ])
        } else {
            u32::from(io[SIODATA8])
        };
        self.put(out, bits);
        // A slave has no clock of its own on hardware — the master's drives the shift — but a
        // byte stream carries no clock at all, so the only one available is the local selection.
        let shift = normal_cycles(siocnt, bits);
        self.run = Some(Run {
            mode: Mode::Normal { bits, master },
            out,
            clocked: 0,
            left: shift,
            units: self.lines().units.max(1),
            total: shift,
            wire: None,
            waited: 0,
            shifted: false,
        });
    }

    /// Hand this unit's value to the cable.
    fn put(&mut self, out: u32, bits: u8) {
        if let Some(port) = &mut self.port {
            let mut guard = port.port();
            guard.trade(Ask::Put { out, bits });
        }
    }

    /// `SIOCNT` as the cartridge reads it.
    fn siocnt(&mut self, io: &[u8; IO_SIZE]) -> u16 {
        let stored = u16::from_le_bytes([io[SIOCNT], io[SIOCNT + 1]]);
        let busy = if self.run.is_some() { START } else { 0 };
        if stored & MULTI_MODE != 0 {
            // Bits 0-1 are the baud rate, 8-11 are unused storage, 12 must read 0, 13 is the mode
            // and 14 the interrupt enable; bit 6 is the error flag the last transfer left, and
            // bits 2-5 and 7 are live.
            let mut value = stored & 0x6F43;
            let lines = self.lines();
            if self.slot != 0 {
                value |= 1 << 2; // SI terminal: 1 = child
            }
            if lines.sd {
                value |= SD_READY;
            }
            if self.id_known {
                value |= (self.slot as u16) << 4;
            }
            if lines.units < 2 {
                // A cable with one unit on it is a bad connection, and a unit that has not
                // answered is the error the transfer reports.
                value &= !SD_READY;
            }
            return value | busy;
        }
        let mut value = stored & 0x5F0B;
        // SI reads high unless another unit is holding it low, which is what "ready" means: the
        // peer has put its value on the cable and is waiting for ours.
        if !self.lines().ready {
            value |= SI_HIGH;
        }
        value | busy
    }

    /// `RCNT` as the cartridge reads it: bits 0-3 are the live SC/SD/SI/SO state, bits 9-13 read
    /// zero and bit 15 must be zero outside General Purpose mode (GBATEK, *4000134h - RCNT*).
    fn rcnt(&mut self, io: &[u8; IO_SIZE]) -> u16 {
        let stored = u16::from_le_bytes([io[RCNT], io[RCNT + 1]]);
        let lines = self.lines();
        // Bits 0-3 are the live SC/SD/SI/SO state and bits 9-13 read zero, so only the bits the
        // hardware lets a store keep come through from the page: 4-8 and 14.
        let mut value = stored & 0x41F0;
        if !self.busy() {
            value |= 1 << 0; // SC is high while no transfer runs
        }
        if lines.sd {
            value |= 1 << 1; // SD
        }
        if !lines.ready {
            value |= 1 << 2; // SI
        }
        value |= 1 << 3; // SO: high while the port is idle
        value
    }

    /// A state was just decoded. The registers travelled in the I/O page; the transfer did not,
    /// because a state never holds one — so the busy bit goes with it.
    pub fn after_load(&mut self, io: &mut [u8; IO_SIZE]) {
        self.run = None;
        self.id_known = false;
        io[SIOCNT] &= !(START as u8);
    }
}

impl Default for Serial {
    fn default() -> Self {
        Serial::new()
    }
}

/// Where the multiplayer clock's next slot ends, if that is sooner than the transfer's own end.
fn slot_boundary(baud: u32, clocked: u32, total: u32) -> Option<u32> {
    let slot = MULTI_BITS_PER_UNIT * CLOCK_HZ / baud.max(1);
    if slot == 0 {
        return None;
    }
    let next = (clocked / slot + 1) * slot;
    (next > clocked && next < total).then(|| next - clocked)
}

/// True for an I/O offset this unit answers.
pub fn owns(offset: usize) -> bool {
    (SIODATA32..=SIOMLT_SEND_HI).contains(&offset)
        || (RCNT..=RCNT_HI).contains(&offset)
        || (JOYCNT..=JOYCNT + 1).contains(&offset)
        || (JOY_RECV..=JOYSTAT + 1).contains(&offset)
}

/// True for an offset whose two bytes are one register write.
pub fn owns16(offset: usize) -> bool {
    offset == SIOCNT || offset == SIOMLT_SEND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_transfer_is_a_bit_count_times_a_clock() {
        // 256 KHz: 16 777 216 / 262 144 = 64 cycles a bit, so 8 bits are 512 and 32 are 2 048.
        assert_eq!(CLOCK_HZ / 262_144, CYCLES_PER_BIT_SLOW);
        assert_eq!(CLOCK_HZ / 2_097_152, CYCLES_PER_BIT_FAST);
        assert_eq!(normal_cycles(0, 8), 512);
        assert_eq!(normal_cycles(0, 32), 2_048);
        assert_eq!(normal_cycles(SC_FAST, 8), 64);
        assert_eq!(normal_cycles(SC_FAST, 32), 256);
        assert_eq!(normal_bits(0), 8);
        assert_eq!(normal_bits(LENGTH32), 32);
    }

    #[test]
    fn a_multiplayer_transfer_is_eighteen_bits_a_unit_plus_its_delays_and_timeout() {
        // 115 200 bps: a bit is 16 777 216 / 115 200 = 145.6 cycles, so the totals are rounded
        // down by at most one cycle.
        assert_eq!(multi_cycles(115_200, 1), 36 * 16_777_216 / 115_200);
        assert_eq!(multi_cycles(115_200, 2), 55 * 16_777_216 / 115_200);
        assert_eq!(multi_cycles(115_200, 3), 74 * 16_777_216 / 115_200);
        assert_eq!(multi_cycles(115_200, 4), 75 * 16_777_216 / 115_200);
        assert!(multi_cycles(115_200, 2) > multi_cycles(115_200, 1));
        assert!(multi_cycles(9_600, 2) > multi_cycles(115_200, 2));
    }

    #[test]
    fn the_serial_block_is_the_documented_offsets() {
        assert!(owns(SIOMULTI0));
        assert!(owns(SIOMULTI3));
        assert!(owns(SIOCNT));
        assert!(owns(SIOMLT_SEND));
        assert!(owns(SIOMLT_SEND + 1));
        assert!(owns(RCNT + 1));
        assert!(owns(JOYSTAT + 1));
        assert!(!owns(0x11F));
        assert!(!owns(0x12C), "not used");
        assert!(!owns(0x142), "not used");
    }
}
