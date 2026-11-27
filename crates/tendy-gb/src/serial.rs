//! `$FF01`/`$FF02`: the shift register, and the byte it swaps with a second player.
//!
//! Without a cable the port behaves exactly as a real console's does with nothing plugged in: an
//! internal-clock transfer shifts eight ones in and ends at `$FF` after 1 024 M-cycles, and an
//! external-clock transfer never completes because no clock is driving it. That path is untouched
//! by the cable and every checksum that depended on it is bit-identical.
//!
//! With a cable (`Link > Role: Host` or `Player 2`) the eight bits are still shifted at the
//! hardware's 128 M-cycles per bit, and the byte the other side's shift register holds is swapped
//! in when they are done: the master hands its byte to the peer as the transfer starts, and the
//! transfer ends when the peer's byte arrives. The slave is driven by the peer's clock, so it hands
//! its byte over the moment `SC` bit 7 is set with bit 0 clear and ends the same way, on the byte
//! that comes back. That is one packet per transfer instead of one per bit, and it is what the
//! registers can observe - `SB` only ever shows the byte that came in, never the bits. A byte is
//! an answer only to a finished shift: one that arrives before this end's eighth bit stays in the
//! transport's queue and is read once `bits_left` is 0, so a peer that answers early - the ordinary
//! timeline, since the master sends first - ends the transfer exactly once.
//!
//! Nothing here blocks. A transfer that gets no answer completes with `$FF` and says so once after
//! two frames ([`LINK_TIMEOUT_CYCLES`]), so a peer that dies mid-game costs a stutter and a notice,
//! never a hang; a peer that disconnects outright is noticed on the next poll and both ends fall
//! back to the cable-less line. Link state lives on the front end's side of the save state, not in
//! it: a state written while connected holds only `sb`, `sc`, `bits_left` and `counter`, so it
//! loads on a build whose `Link > Role` is `Off`.

use crate::bus::IF_SERIAL;
use crate::link::{Link, LinkStatus};
use crate::state::{StateError, StateReader, StateWriter};

/// Capacity of the serial log: 16 KiB, allocated once.
const OUT_CAP: usize = 16 * 1024;

/// The serial port, and the byte log test ROMs report through.
#[derive(Clone)]
pub struct Serial {
    pub sb: u8,
    pub sc: u8,
    /// 8 at the start of an internal-clock transfer, else 0.
    pub bits_left: u8,
    /// Machine cycles until the next bit.
    pub counter: u16,
    /// Every byte a transfer completed, capped at 16 KiB.
    pub out: Vec<u8>,
    /// Set by a write of `SC` with bit 7 set and bit 0 clear (external clock) with no live cable,
    /// held until `Machine::take_link_notice` reads it.
    pub link_notice: bool,
    /// The other end of the cable; `None` while `Link > Role` is `Off`. Session state, never saved.
    ///
    /// A clone of a `Serial` shares the cable rather than unplugging it: `Machine::clone` is how a
    /// state is decoded, and loading a state must not drop the other player.
    pub link: Option<Link>,
    /// What the cable last had to say. The front end takes it and shows it once.
    pub link_message: Option<LinkMessage>,
    /// M-cycles spent waiting for the peer's byte, `None` when no transfer is waiting.
    wait: Option<u32>,
    /// Cycles since the cable was last polled, so a poll is one lock every `POLL_EVERY` cycles.
    poll: u8,
    /// Whether the cable was connected at the last poll. False means the port behaves exactly as
    /// it does with nothing plugged in.
    cable_live: bool,
    /// Whether the two-frame timeout has already raised its notice for this silence.
    said: bool,
}

/// What the cable has to say when it says anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMessage {
    /// The peer went away mid-game: both ends fall back to the `$FF`-in line and say so once.
    PeerGone,
    /// A transfer waited two frames for the peer's byte and completed with `$FF` instead.
    PeerSilent,
}

impl LinkMessage {
    /// The status bar's text.
    pub fn text(self) -> &'static str {
        match self {
            LinkMessage::PeerGone => "link: the other player disconnected",
            LinkMessage::PeerSilent => "link: the other player stopped answering",
        }
    }
}

/// Machine cycles per bit: the link port's 8 192 Hz bit clock is 512 T-cycles, 128 M-cycles.
const CYCLES_PER_BIT: u16 = 128;
/// Bits in a byte, and so the shift the transfer runs.
const BITS_PER_BYTE: u8 = 8;
/// A transfer waits two frames for the peer's byte - 35 112 M-cycles, 33.5 ms - before it
/// completes with `$FF`. Two frames is the budget: long enough that a peer a frame behind
/// still answers, short enough that a dead one stutters rather than stalls the game.
pub const LINK_TIMEOUT_CYCLES: u32 = 2 * crate::MCYCLES_PER_FRAME;
/// The cable is polled once every this many M-cycles: one bit's worth, so one poll per bit at most.
/// That is 137 polls a frame, against a lock and a channel read on every one of the 17 556.
const POLL_EVERY: u8 = 128;

impl Serial {
    /// Post-boot SB=00 SC=7E.
    pub fn new() -> Serial {
        Serial {
            sb: 0x00,
            sc: 0x7E,
            bits_left: 0,
            counter: 0,
            out: Vec::with_capacity(OUT_CAP),
            link_notice: false,
            link: None,
            link_message: None,
            wait: None,
            poll: 0,
            cable_live: false,
            said: false,
        }
    }

    /// Plug the cable in, or pull it out with `None`.
    ///
    /// The status is read once here so the first transfer after a `Role` change already knows
    /// whether a peer is on the other end; after that the poll keeps it level.
    pub fn attach(&mut self, link: Option<Link>) {
        self.link_message = None;
        self.said = false;
        self.poll = 0;
        self.wait = None;
        self.cable_live = link
            .as_ref()
            .is_some_and(|link| link.status() == LinkStatus::Connected);
        self.link = link;
    }

    /// A fresh port that keeps the cable: what a reset does, since the cable is session state and
    /// `Game > Reset` must not unplug it.
    pub fn restart(&mut self) {
        let link = self.link.take();
        *self = Serial::new();
        if link.is_some() {
            self.attach(link);
        }
    }

    /// Forget a transfer that was in flight: a state never holds one, so a load drops it.
    pub fn drop_transfer(&mut self) {
        self.wait = None;
        self.poll = 0;
    }

    /// True while this port owns a transfer: shifting eight bits, or waiting on the peer's byte.
    /// A CGB `STOP` speed switch is refused while it does (Pan Docs lists the serial port among the
    /// units that keep running through the pause).
    pub fn transferring(&self) -> bool {
        self.bits_left > 0 || self.wait.is_some()
    }

    /// One M-cycle of the bit clock. Returns `IF_SERIAL` on the cycle the transfer completes: the
    /// internal-clock transfer takes 1 024 M-cycles (`Serial_Data_Transfer_(Link_Cable).html`).
    pub fn tick(&mut self) -> u8 {
        let mut bits = 0;
        if self.link.is_some() {
            self.poll += 1;
            if self.poll >= POLL_EVERY {
                self.poll = 0;
                bits |= self.poll_cable();
            }
        }
        if self.bits_left == 0 {
            return bits;
        }
        self.counter += 1;
        if self.counter < CYCLES_PER_BIT {
            return bits;
        }
        self.counter = 0;
        if !self.cable_live {
            // The disconnected line reads 1, so the byte shifts in ones and ends at `$FF`.
            self.sb = (self.sb << 1) | 1;
        }
        self.bits_left -= 1;
        if self.bits_left > 0 {
            return bits;
        }
        if self.cable_live {
            // Eight bits are out. The transfer ends on the peer's byte, which the poll brings in.
            self.wait = Some(0);
        } else {
            self.sc &= !0x80;
            bits |= IF_SERIAL;
        }
        bits
    }

    /// One poll of the cable: the peer's byte if it has arrived, else the two-frame count.
    fn poll_cable(&mut self) -> u8 {
        let Some(link) = self.link.clone() else {
            return 0;
        };
        let status = self.refresh_cable(&link);
        let Some(waited) = self.wait else {
            return 0;
        };
        // A byte on the wire is this transfer's answer only once its own eight bits are out:
        // before that the peer's register is mid-shift, not answering. It stays in the
        // transport's queue - in order, one byte per transfer - and is read on the first poll
        // after the last bit. Taking it early used to end the transfer at `SC`, leave
        // `bits_left` counting down, and end it a second time on the phantom wait that followed.
        // A slave has `bits_left == 0` from the start of its transfer, so it still takes its
        // answer the moment it arrives.
        if self.bits_left == 0
            && let Some(byte) = link.try_recv()
        {
            self.sb = byte;
            self.wait = None;
            self.said = false;
            self.sc &= !0x80;
            return IF_SERIAL;
        }
        let waited = waited + u32::from(POLL_EVERY);
        if waited >= LINK_TIMEOUT_CYCLES {
            self.sb = 0xFF;
            self.wait = None;
            self.sc &= !0x80;
            if status == LinkStatus::Connected && !self.said {
                self.link_message = Some(LinkMessage::PeerSilent);
                self.said = true;
            }
            return IF_SERIAL;
        }
        self.wait = Some(waited);
        0
    }

    /// Bring `cable_live` level with the transport, and say so once if the peer has gone.
    ///
    /// Called from the poll (every [`POLL_EVERY`] cycles) and from a write of `SC`, which is the
    /// moment that matters: a `Role` set a moment ago, or a peer that left a moment ago, has to be
    /// seen now rather than up to 128 cycles later, or one transfer runs the wrong way.
    fn refresh_cable(&mut self, link: &Link) -> LinkStatus {
        let status = link.status();
        if status == LinkStatus::Connected {
            self.cable_live = true;
        } else if self.cable_live {
            // The peer went away: from here the line reads ones, as an unplugged cable does, and
            // the player is told once.
            self.cable_live = false;
            self.link_message = Some(LinkMessage::PeerGone);
            self.said = true;
        }
        status
    }

    /// Whether a transfer started right now would reach a peer.
    fn cable_ready(&mut self) -> bool {
        match self.link.clone() {
            Some(link) => self.refresh_cable(&link) == LinkStatus::Connected,
            None => false,
        }
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0xFF01 => self.sb,
            0xFF02 => 0x7E | (self.sc & 0x81),
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, value: u8) -> u8 {
        match addr {
            0xFF01 => self.sb = value,
            0xFF02 => {
                self.sc = value & 0x81;
                if self.sc & 0x80 == 0 {
                    // Clearing bit 7 stops whatever was running.
                    self.bits_left = 0;
                    self.counter = 0;
                    self.wait = None;
                } else if self.sc & 0x01 != 0 {
                    self.bits_left = BITS_PER_BYTE;
                    self.counter = 0;
                    self.wait = None;
                    // Log the byte as the transfer starts: `SB` ends as the peer's byte (or `$FF`
                    // off a dead line), so the byte the game sent is only here, before the eight
                    // shifts. §3's field list has no room to keep it, and a transfer aborted
                    // mid-way has already been logged.
                    if self.out.len() < OUT_CAP {
                        self.out.push(self.sb);
                    }
                    if self.cable_ready() {
                        // The master hands its byte over now; the answer closes the transfer.
                        self.start_swap();
                    }
                } else {
                    // External clock: the peer's clock drives the eight bits.
                    self.bits_left = 0;
                    self.counter = 0;
                    if self.cable_ready() {
                        // The slave's byte goes out the moment `SC` bit 7 is set, and the transfer
                        // ends when the master's byte comes back.
                        if self.out.len() < OUT_CAP {
                            self.out.push(self.sb);
                        }
                        self.start_swap();
                    } else {
                        // Nothing drives the clock, so it never completes - exactly what a real
                        // console does with no cable attached. `link_notice` still records it for
                        // the machine-level tests; the front end has no notice to show for it.
                        self.link_notice = true;
                    }
                }
            }
            _ => {}
        }
        0
    }

    /// Hand this side's byte to the peer and start waiting for its answer.
    ///
    /// No queue surgery: the peer's bytes are consumed in the order they arrive, which is what a
    /// socket gives any protocol and what the slave needs - its byte often arrives before the
    /// slave has armed its own transfer. A master's answer is just as often already queued, so the
    /// wait is armed here but the byte itself is not read until the eight bits are out
    /// ([`Serial::poll_cable`]). A byte that lands after a two-frame timeout is therefore
    /// read as the next transfer's answer. It cannot be told apart from a peer that answered
    /// early, and a peer a whole transfer behind is already broken; the report says so.
    fn start_swap(&mut self) {
        if let Some(link) = self.link.clone() {
            link.send(self.sb);
            self.wait = Some(0);
        }
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.sb);
        w.u8(self.sc);
        w.u8(self.bits_left);
        w.u16(self.counter);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.sb = r.u8()?;
        self.sc = r.u8()?;
        self.bits_left = r.u8()?;
        self.counter = r.u16()?;
        self.drop_transfer();
        Ok(())
    }
}

impl Default for Serial {
    fn default() -> Self {
        Serial::new()
    }
}
