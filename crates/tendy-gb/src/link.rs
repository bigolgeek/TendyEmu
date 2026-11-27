//! The link cable's transport: one byte to the peer and one byte back.
//!
//! Two Game Boys on a cable shift a byte into each other at the same time: the master's clock
//! drives eight bits out of both shift registers, and each side ends up holding the other's byte.
//! This module carries that byte, not those bits - a [`Transport`] hands the peer one byte and
//! takes one back - because a bit-at-a-time socket protocol would need a packet every 128
//! M-cycles (122 us, 8 192 a frame) while the byte is all either side's shift register can
//! observe. `serial.rs` still runs the eight bits at the hardware's 128 M-cycles each and swaps
//! the bytes when they are done, so a transfer takes the same 1 024 M-cycles with a cable as
//! without one and nothing about a cable-less run moves.
//!
//! Two implementations: [`pipe`], an in-memory pair for tests, and [`tcp_host`] /
//! [`tcp_player2`], the same protocol over TCP on `127.0.0.1`. Nothing outside this module and
//! the window loop knows which one is in use.
//!
//! Threads: a socket is owned by a reader thread and a writer thread of its own, so the emulator
//! thread never blocks on it. It hands a byte to an unbounded channel and polls the other one; the
//! two-frame budget for the peer's answer lives in `serial.rs`, counted in machine cycles, so a
//! dead peer costs the emulator 137 non-blocking polls a frame and no waiting at all.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// The port `Link > Port` starts on: a high one, out of the way of anything else on a desk.
pub const DEFAULT_PORT: u16 = 8765;

/// The `Link > Port` row's range. The drawer has no text entry (v1's rule for every row), so the
/// row steps through ten consecutive ports rather than taking a number.
pub const FIRST_PORT: u16 = DEFAULT_PORT;
/// The last port `Link > Port` steps to.
pub const LAST_PORT: u16 = DEFAULT_PORT + 9;

/// How often the connect thread and the host's accept loop look again.
const RETRY_MS: u64 = 200;

/// The `Link > Role` row: which end of the cable this instance is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkRole {
    /// No cable at all: a disconnected line, exactly as a real console with nothing plugged in.
    #[default]
    Off,
    /// Listens on the port; the other player connects. Drives the clock in every transfer.
    Host,
    /// Connects to a host on the port. Its transfers run on the host's clock.
    Player2,
}

impl LinkRole {
    /// The word `config.toml` writes.
    pub fn name(self) -> &'static str {
        match self {
            LinkRole::Off => "off",
            LinkRole::Host => "host",
            LinkRole::Player2 => "player2",
        }
    }

    /// The settings-file spelling, or `None` for anything else.
    pub fn from_name(word: &str) -> Option<LinkRole> {
        match word {
            "off" => Some(LinkRole::Off),
            "host" => Some(LinkRole::Host),
            "player2" | "player_2" => Some(LinkRole::Player2),
            _ => None,
        }
    }

    /// The row's value column, before the cable's status is added.
    pub fn label(self) -> &'static str {
        match self {
            LinkRole::Off => "Off",
            LinkRole::Host => "Host",
            LinkRole::Player2 => "Player 2",
        }
    }

    /// `off -> host -> player 2` and back, each way round.
    pub fn step(self, forward: bool) -> LinkRole {
        match (self, forward) {
            (LinkRole::Off, true) => LinkRole::Host,
            (LinkRole::Host, true) => LinkRole::Player2,
            (LinkRole::Player2, true) => LinkRole::Off,
            (LinkRole::Off, false) => LinkRole::Player2,
            (LinkRole::Host, false) => LinkRole::Off,
            (LinkRole::Player2, false) => LinkRole::Host,
        }
    }
}

/// Where the cable stands, as the drawer prints it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkStatus {
    /// No cable: `Link > Role: Off`.
    #[default]
    Off,
    /// The socket is up and no peer has answered yet.
    Waiting,
    /// A peer is on the other end.
    Connected,
    /// The peer went away (or the socket could not be made), so the port behaves cable-less.
    Dropped,
}

// The status crosses to the socket threads as one atomic byte; `Off` never does, because it is the
// absence of a `Link` rather than a state one can be in.
const WAITING: u8 = 0;
const CONNECTED: u8 = 1;
const DROPPED: u8 = 2;

fn decode(status: &AtomicU8) -> LinkStatus {
    match status.load(Ordering::Relaxed) {
        CONNECTED => LinkStatus::Connected,
        DROPPED => LinkStatus::Dropped,
        _ => LinkStatus::Waiting,
    }
}

/// One end of the cable: a byte out to the peer, the peer's byte back.
///
/// `recv` takes a timeout so a caller that wants to wait can; `serial.rs` always passes
/// [`Duration::ZERO`], because the emulator thread must never block on the socket and its own
/// two-frame budget is counted in machine cycles.
pub trait Transport: Send {
    /// Hand one byte to the peer. Dropped, not queued, when no peer is connected: a byte the
    /// previous transfer could not deliver must not arrive in the middle of the next one.
    fn send(&mut self, byte: u8);
    /// The peer's next byte, or `None` when none arrived within `timeout`.
    fn recv(&mut self, timeout: Duration) -> Option<u8>;
    /// Where this end of the cable stands.
    fn status(&self) -> LinkStatus;
}

/// A handle to one end of the cable, shared by every clone of a [`Serial`](crate::serial::Serial).
///
/// The handle is an `Arc` on purpose: `Machine::clone` is how a state is decoded, and a
/// clone that unplugged the cable would make loading a state while connected drop the other player.
#[derive(Clone)]
pub struct Link {
    cable: Arc<Mutex<Box<dyn Transport>>>,
}

impl Link {
    fn new<T: Transport + 'static>(transport: T) -> Link {
        Link {
            cable: Arc::new(Mutex::new(Box::new(transport))),
        }
    }

    /// The inner transport, with a poisoned lock taken rather than panicked on: one failed `send`
    /// on a dead socket must not take the emulator down.
    fn cable(&self) -> MutexGuard<'_, Box<dyn Transport>> {
        self.cable.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Where the cable stands.
    pub fn status(&self) -> LinkStatus {
        self.cable().status()
    }

    /// Hand one byte to the peer.
    pub fn send(&self, byte: u8) {
        self.cable().send(byte);
    }

    /// The peer's next byte if one has arrived, without waiting.
    pub fn try_recv(&self) -> Option<u8> {
        self.cable().recv(Duration::ZERO)
    }

    /// The peer's next byte, waiting up to `timeout` for one. The emulator never calls this with a
    /// timeout; it is here because that is the shape of the protocol, and a probe or a test uses it.
    pub fn recv(&self, timeout: Duration) -> Option<u8> {
        self.cable().recv(timeout)
    }
}

/// The in-memory pipe: two ends that pass bytes to each other with no socket at all.
///
/// Both ends start connected. Dropping one sets the other's status to [`LinkStatus::Dropped`], so a
/// test can watch both sides fall back to `$FF` - and holding one end without ever answering is the
/// silent peer the two-frame timeout is measured against.
pub fn pipe() -> (Link, Link) {
    // Each end's sends land in the other end's inbox: `a` writes to the channel `b` reads.
    let (a_tx, a_rx) = mpsc::channel::<u8>();
    let (b_tx, b_rx) = mpsc::channel::<u8>();
    let a_status = Arc::new(AtomicU8::new(CONNECTED));
    let b_status = Arc::new(AtomicU8::new(CONNECTED));
    let a = Link::new(Pipe {
        out: a_tx,
        inc: b_rx,
        mine: a_status.clone(),
        theirs: b_status.clone(),
    });
    let b = Link::new(Pipe {
        out: b_tx,
        inc: a_rx,
        mine: b_status,
        theirs: a_status,
    });
    (a, b)
}

struct Pipe {
    /// Bytes this end sends land in the peer's `inc`.
    out: Sender<u8>,
    inc: Receiver<u8>,
    /// This end's own status, and the peer's: the peer reads `mine` as `theirs`.
    mine: Arc<AtomicU8>,
    theirs: Arc<AtomicU8>,
}

impl Transport for Pipe {
    fn send(&mut self, byte: u8) {
        if self.theirs.load(Ordering::Relaxed) == CONNECTED {
            let _ = self.out.send(byte);
        }
    }

    fn recv(&mut self, timeout: Duration) -> Option<u8> {
        if timeout.is_zero() {
            self.inc.try_recv().ok()
        } else {
            self.inc.recv_timeout(timeout).ok()
        }
    }

    fn status(&self) -> LinkStatus {
        decode(&self.theirs)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.mine.store(DROPPED, Ordering::Relaxed);
    }
}

/// A host: listens on `127.0.0.1:port` for one peer.
///
/// The bind is synchronous, so a port already taken is a notice the player sees at once rather than
/// a cable that silently never works. The accept happens on a thread of its own; the emulator only
/// ever reads the status word and the incoming channel.
pub fn tcp_host(port: u16) -> Result<Link, String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|why| format!("link: cannot listen on 127.0.0.1:{port}: {why}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|why| format!("link: cannot poll 127.0.0.1:{port}: {why}"))?;
    let (out_tx, out_rx) = mpsc::channel::<u8>();
    let (inc_tx, inc_rx) = mpsc::channel::<u8>();
    let status = Arc::new(AtomicU8::new(WAITING));
    let link = Link::new(Tcp {
        out: out_tx,
        inc: inc_rx,
        status: status.clone(),
    });
    let flag = status.clone();
    std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) != DROPPED {
            match listener.accept() {
                Ok((stream, _)) => {
                    flag.store(CONNECTED, Ordering::Relaxed);
                    serve(stream, flag.clone(), out_rx, inc_tx);
                    return;
                }
                Err(why) if why.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(RETRY_MS / 10));
                }
                Err(_) => {
                    flag.store(DROPPED, Ordering::Relaxed);
                    return;
                }
            }
        }
    });
    Ok(link)
}

/// Player 2: connects to a host on `127.0.0.1:port`, retrying until one answers.
///
/// The retry is what lets either window be started first: a player 2 opened before the host waits
/// on [`LinkStatus::Waiting`] and connects the moment the host's listener is up. Nothing about a
/// refused connection is fatal, so no notice is raised for it.
pub fn tcp_player2(port: u16) -> Link {
    let (out_tx, out_rx) = mpsc::channel::<u8>();
    let (inc_tx, inc_rx) = mpsc::channel::<u8>();
    let status = Arc::new(AtomicU8::new(WAITING));
    let link = Link::new(Tcp {
        out: out_tx,
        inc: inc_rx,
        status: status.clone(),
    });
    let flag = status.clone();
    std::thread::spawn(move || {
        while flag.load(Ordering::Relaxed) != DROPPED {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => {
                    flag.store(CONNECTED, Ordering::Relaxed);
                    serve(stream, flag.clone(), out_rx, inc_tx);
                    return;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(RETRY_MS)),
            }
        }
    });
    link
}

/// Hand a connected socket to its two threads: one reads bytes into `inc`, the other writes the
/// bytes `out` yields. Either one ending marks the cable dropped, and shutting the socket down is
/// what wakes the other: a reader blocked on `read` otherwise waits for a peer that has gone.
fn serve(stream: TcpStream, status: Arc<AtomicU8>, out: Receiver<u8>, inc: Sender<u8>) {
    // Windows hands an accepted socket back in the listener's non-blocking mode: the reader below
    // wants to block on `read`, so the mode is cleared here rather than inherited.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let read_half = match stream.try_clone() {
        Ok(half) => half,
        Err(_) => {
            status.store(DROPPED, Ordering::Relaxed);
            return;
        }
    };
    let mut write_half = stream;
    let writer_flag = status.clone();
    std::thread::spawn(move || {
        for byte in out {
            if write_half.write_all(&[byte]).is_err() {
                break;
            }
        }
        let _ = write_half.shutdown(Shutdown::Both);
        writer_flag.store(DROPPED, Ordering::Relaxed);
    });
    std::thread::spawn(move || {
        let mut half = read_half;
        let mut byte = [0u8; 1];
        loop {
            match half.read(&mut byte) {
                Ok(1) => {
                    if inc.send(byte[0]).is_err() {
                        break;
                    }
                }
                Ok(_) => break,
                Err(why) if why.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        status.store(DROPPED, Ordering::Relaxed);
        let _ = half.shutdown(Shutdown::Both);
    });
}

struct Tcp {
    out: Sender<u8>,
    inc: Receiver<u8>,
    status: Arc<AtomicU8>,
}

impl Transport for Tcp {
    fn send(&mut self, byte: u8) {
        if self.status.load(Ordering::Relaxed) == CONNECTED {
            let _ = self.out.send(byte);
        }
    }

    fn recv(&mut self, timeout: Duration) -> Option<u8> {
        if timeout.is_zero() {
            self.inc.try_recv().ok()
        } else {
            self.inc.recv_timeout(timeout).ok()
        }
    }

    fn status(&self) -> LinkStatus {
        decode(&self.status)
    }
}

impl Drop for Tcp {
    /// Role set back to `Off`: the listening or connecting thread sees this and stops, so the port
    /// is free for the next `Host`.
    fn drop(&mut self) {
        self.status.store(DROPPED, Ordering::Relaxed);
    }
}
