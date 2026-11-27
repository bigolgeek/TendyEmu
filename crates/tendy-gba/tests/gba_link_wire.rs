//! Two Advances, two processes, one socket (G9b).
//!
//! The link engine and its socket framing were proved in one process by `tests/gba_link.rs` on
//! `link::pipe` — a channel pair, not a socket. This file is the other half of the claim: two
//! *separate* processes, each a real `Gba` running the same synthetic cartridge, instance A
//! listening on `127.0.0.1` and instance B dialling it, trading one word a frame for 120 frames.
//! Both sides print a transcript — round, frame the transfer completed on, what was sent, what came
//! back, how many units answered, how many *emulated cycles* the transfer cost and how much *wall
//! clock* this host spent — and the parent test reads both and checks that each side received the
//! other's bytes, in order, with nothing lost or repeated. The emulated cost is the claim, against
//! the hardware's two-frame window; the wall clock is printed and never asserted, because it
//! measures this machine's scheduler and not the cable.
//!
//! The child is this same test binary, re-executed with [`CHILD_ROLE`] set and the one test filtered
//! for. That keeps the proof in `cargo test` with no new binary, no example and no ROM file: the
//! cartridge is synthesized here, and nothing is written to disk.
//!
//! **What this cannot say.** No two-player cartridge is available in this worktree — the library is
//! single-player titles — so the "cartridge" is a `b .` spin with the serial registers poked the way
//! a cartridge would poke them, exactly as `tests/gba_link.rs` does it. A real two-player title is
//! the user's test.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tendy_gb::link::{self, Link, LinkStatus};
use tendy_gba::Gba;
use tendy_gba::bus::CYCLES_PER_FRAME;
use tendy_gba::cart::Cartridge;
use tendy_gba::serial;

const SIODATA32: u32 = 0x0400_0120;
const SIOMULTI0: u32 = 0x0400_0120;
const SIOMULTI1: u32 = 0x0400_0122;
const SIOCNT: u32 = 0x0400_0128;
const SIOMLT_SEND: u32 = 0x0400_012A;
const RCNT: u32 = 0x0400_0134;

/// Set on the child process: `host` or `player2`.
const CHILD_ROLE: &str = "GB_LINK_WIRE_ROLE";
/// Set on the child process: the port to listen on or dial.
const CHILD_PORT: &str = "GB_LINK_WIRE_PORT";
/// Frames the child runs, and transfers it makes, one a frame.
const FRAMES: u64 = 120;
/// Multiplayer rounds measured after the normal-mode ones, on the same socket.
const MULTI_ROUNDS: usize = 24;
/// The baud the multiplayer rounds use: the slowest one, so a round's emulated window (96 104
/// cycles, 5.7 ms) is long enough that a loopback answer arriving late is not a lost unit.
const MULTI_BAUD_INDEX: u16 = 0;

/// How long the harness will wait, in *wall* clock, for a peer's value before it calls the
/// transfer lost. Seconds, not milliseconds: on this machine three lanes run at once and the
/// transport's threads can go unscheduled for a hundred milliseconds at a time, which is a fact
/// about the host and not about the cable.
const WALL_DEADLINE: Duration = Duration::from_secs(10);
/// How long to hand the machine back while the wire is empty. The emulated clock does not advance
/// across it, so it costs the transfer nothing it is measuring. (Half a millisecond, because that
/// is what this host's timer rounds `sleep` to.)
const EMPTY_WIRE_GIVE_BACK: Duration = Duration::from_micros(200);
/// How much wall clock one emulated poll is worth while a value is half-arrived. The engine asks
/// the cable once every 128 cycles (`POLL_EVERY` in `crate::serial`), so at a millisecond a
/// poll the two-frame budget buys 4 389 ms of wall clock for the rest of a 32-bit answer — four
/// socket writes — to arrive in.
const POLL_GIVE_BACK: Duration = Duration::from_micros(1_000);
/// The engine's poll interval, in emulated cycles. A copy of `serial`'s private `POLL_EVERY`: the
/// pacing above is the only thing that reads it, and the transfer is correct whatever it is set
/// to, so a silent drift costs wall time and never an assertion.
const EMULATED_POLL: u64 = 128;

/// A cartridge whose code starts at `$080000C0` and is one instruction: `b .`. The same one
/// `tests/gba_link.rs` builds, so both files drive the same machine.
fn spin_cart() -> Vec<u8> {
    let mut rom = vec![0u8; 0x200];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xA9].copy_from_slice(b"GBA-LINK\0");
    rom[0xAC..0xB0].copy_from_slice(b"TLNK");
    rom[0xB0..0xB2].copy_from_slice(b"00");
    rom[0xB2] = 0x96;
    let mut x: u8 = 0;
    for &byte in &rom[0xA0..=0xBC] {
        x = x.wrapping_sub(byte);
    }
    rom[0xBD] = x.wrapping_sub(0x19);
    rom[0xC0..0xC4].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes());
    rom
}

fn machine() -> Gba {
    Gba::new(Cartridge::new(spin_cart()).expect("a synthesized cartridge"))
}

fn busy(gba: &mut Gba) -> bool {
    gba.bus.read16(SIOCNT) & serial::START != 0
}

/// Step until the running transfer is over, or three frames have gone by — the two-frame budget
/// plus one, so a transfer that never completes ends the loop rather than hanging the test.
///
/// **This loop spends the emulated clock, so it is only used where the emulated clock is itself
/// the claim:**
///
/// - [`the_peer_leaving_mid_transfer_completes_the_transfer_and_says_so_once`], where the engine's
///   two-frame timeout firing is the thing under test — pausing the clock would never let it fire;
/// - the multiplayer rounds in [`transcript`], where a round's 96 110-cycle window is the claim.
///
/// A normal-mode transfer waits for a peer in a *separate process* on a *separate clock*, so it
/// uses [`transfer`] instead: there the waiting is wall clock and the assertion is emulated cycles.
///
/// The give-back stays from H9, and it is kept because it is measured, not because it is old:
/// while the wire is empty — `RCNT` bit 2 (`SI`) high, the engine's own way of saying "the peer's
/// value is not here yet" — the loop hands the machine back for half a millisecond once every
/// 4 096 cycles. That is what took the multiplayer rounds from 20 or 21 of 24 two-unit to 24 of
/// 24; the two-frame timeout it was also papering over is now handled in [`transfer`] and no
/// longer depends on it.
fn run_until_idle(gba: &mut Gba) -> u64 {
    let started = gba.bus.cycles;
    let mut slept_at = started;
    while busy(gba) && gba.bus.cycles - started < 3 * u64::from(CYCLES_PER_FRAME) {
        gba.step_instruction();
        let spent = gba.bus.cycles - started;
        if spent > 4_096 && gba.bus.cycles - slept_at > 4_096 {
            slept_at = gba.bus.cycles;
            if gba.bus.read16(RCNT) & serial::SI_HIGH != 0 {
                std::thread::sleep(Duration::from_micros(500));
            }
        }
    }
    gba.bus.cycles - started
}

/// What one transfer cost, in both clocks: the emulated one that is the claim, and the wall one
/// that is only reported.
#[derive(Clone, Copy, Debug)]
struct Reading {
    /// Emulated cycles from arming to completion — the cost the hardware would charge.
    cycles: u64,
    /// Wall clock spent. Printed, never asserted against: it measures this host's scheduler.
    wall: Duration,
    /// Whether the engine reported the transfer over. `false` means [`WALL_DEADLINE`] was reached
    /// first, and the registers below are what the machine had to say at that instant.
    done: bool,
    /// `RCNT` when the transfer ended — bit 2 high means the wire was still empty.
    rcnt: u16,
    /// `SIOCNT` when the transfer ended — bit 7 is the engine's busy bit.
    siocnt: u16,
}

/// Run one armed normal-mode transfer to its end, **waiting on the wall clock and asserting on the
/// emulated one**.
///
/// The two clocks are the whole point. The hardware's claim is that an answer arrives inside two
/// frames — 561 792 emulated cycles — and that claim is asserted below, on a number this function
/// returns. The harness's own waiting is a different quantity: it is this machine's scheduler
/// handing the transport's reader and writer threads a core, and it is bounded in seconds. When
/// the wire is empty (`RCNT` bit 2 high) the emulated clock is simply **not advanced** — the loop
/// sleeps instead — so a peer that takes a third of a second to be scheduled is late in wall time,
/// which nothing here asserts, and not in emulated time, which is all that is asserted.
///
/// While a value is *partly* arrived (one of the four bytes of a 32-bit answer) the emulated clock
/// cannot be paused without pretending the value was there, so it is paced instead: one poll,
/// 128 cycles, per millisecond of wall clock. The two-frame budget is 4 389 polls, so the rest of
/// a word has four seconds to arrive in.
///
/// `armed` is the `SIOCNT` word that started the transfer; its length field selects the shift, and
/// the shift is this unit's own clock, so it always runs (and is always charged).
///
/// A run that reaches [`WALL_DEADLINE`] panics with the transcript's last crossing, the cycles
/// reached, the wall time spent, and what `SI` / `RCNT` / `SIOCNT` said at that instant: a red run
/// says why it was red.
fn transfer(gba: &mut Gba, cable: &Link, armed: u16, role: &str, what: &str) -> Reading {
    let started = Instant::now();
    let start_cycles = gba.bus.cycles;
    let shift = u64::from(serial::normal_cycles(armed, serial::normal_bits(armed)));
    let mut paced_at = start_cycles + shift;
    while busy(gba) {
        if gba.bus.cycles - start_cycles < shift {
            // The shift: the bits going out on this unit's own clock. Nothing can be taken back
            // from the cable until it is over, so it runs at full speed.
            gba.step_instruction();
            continue;
        }
        if gba.bus.read16(RCNT) & serial::SI_HIGH != 0 {
            if cable.status() != LinkStatus::Connected {
                // The peer left the cable. Let the engine's own clock reach the conclusion it has
                // for that — `PeerGone`, and the transfer ends — rather than waiting for a value
                // that is not coming.
                gba.step_instruction();
                continue;
            }
            if started.elapsed() >= WALL_DEADLINE {
                break;
            }
            std::thread::sleep(EMPTY_WIRE_GIVE_BACK);
            paced_at = gba.bus.cycles;
            continue;
        }
        if gba.bus.cycles.saturating_sub(paced_at) >= EMULATED_POLL {
            // A value is on the wire but the transfer is not over: a poll has just been spent
            // asking for the rest of it. Buy wall clock before spending another.
            paced_at = gba.bus.cycles;
            std::thread::sleep(POLL_GIVE_BACK);
            continue;
        }
        gba.step_instruction();
    }
    let reading = Reading {
        cycles: gba.bus.cycles - start_cycles,
        wall: started.elapsed(),
        done: !busy(gba),
        rcnt: gba.bus.read16(RCNT),
        siocnt: gba.bus.read16(SIOCNT),
    };
    if !reading.done {
        let last = gba.bus.serial.transcript.last().copied();
        let empty = reading.rcnt & serial::SI_HIGH != 0;
        panic!(
            "{role}: {what} did not complete: {:.3} s of wall clock and {} emulated cycles \
             ({:.2} frames, against a two-frame window of {}); RCNT {:04X} (SI {}, the wire was \
             {}), SIOCNT {:04X}, cable {}, message {:?}, last crossing {last:?}",
            reading.wall.as_secs_f64(),
            reading.cycles,
            reading.cycles as f64 / f64::from(CYCLES_PER_FRAME),
            serial::LINK_TIMEOUT_CYCLES,
            reading.rcnt,
            if empty { "high" } else { "low" },
            if empty { "empty" } else { "holding a value" },
            reading.siocnt,
            status_word(cable.status()),
            gba.bus.serial.message,
        );
    }
    reading
}

/// A slave arms when the line says the master has put its word: `RCNT` bit 2 (`SI`) reads low once
/// the peer's value is on the cable. That is the hardware's own start condition — a Game Boy Advance
/// that is not driving the clock cannot begin before the unit that is — and it is what keeps the two
/// processes in step: without it whichever side armed first loses its first transfer to the
/// two-frame timeout, and every transfer after it is one round behind (a fact this file's first run
/// printed, and the report records).
///
/// The read moves the socket's bytes into the port's queue without consuming them, so the transfer
/// that follows still gets the value the line promised.
fn wait_ready(gba: &mut Gba, role: &str, round: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while gba.bus.read16(RCNT) & serial::SI_HIGH != 0 {
        if Instant::now() > deadline {
            panic!("{role}: round {round} waited five seconds for the peer's word");
        }
        // A sleep, not a yield: this loop is waiting on the socket's reader thread, and a hot
        // spin here keeps that thread — and the peer's writer — off a core on a loaded machine.
        // Measured: with a yield the parent could sit here for longer than a whole multiplayer
        // round (3 ms of wall time) while the child's answer was still queued, and the child's
        // round then ended without it. Fifty microseconds per turn is plenty: the answer is a
        // loopback hop away.
        std::thread::sleep(Duration::from_micros(50));
    }
}

/// What one side's run left: how many transfers crossed, and how many multiplayer rounds carried
/// two units. Every `wire:` line is printed as it is produced rather than returned, so a side that
/// panics half way through has already said what it did — the transcript is in the pipe the parent
/// is draining, and a red run's transcript is the whole point.
struct Run {
    normal: usize,
    multi: usize,
    two_unit_rounds: usize,
}

/// One side of the cable: `host` listens, `player2` dials, and both then run [`FRAMES`] normal-mode
/// 32-bit transfers, one a frame, then [`MULTI_ROUNDS`] multiplayer rounds.
fn transcript(role: &str, port: u16) -> Run {
    let mut gba = machine();
    let (cable, slot) = match role {
        "host" => (
            link::tcp_host(port).expect("the listener must be makeable on a free port"),
            0usize,
        ),
        "player2" => (link::tcp_player2(port), 1usize),
        other => panic!("unknown role {other}"),
    };
    gba.attach_link(Some(cable.clone()), slot);
    println!(
        "wire: role {role} slot {slot} cable {}",
        status_word(cable.status())
    );
    // Both windows start on their own; neither can do anything until the other is on the socket.
    let deadline = Instant::now() + Duration::from_secs(20);
    while cable.status() != LinkStatus::Connected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    if cable.status() != LinkStatus::Connected {
        println!(
            "wire: role {role} gave up waiting: {}",
            status_word(cable.status())
        );
        return Run {
            normal: 0,
            multi: 0,
            two_unit_rounds: 0,
        };
    }
    println!(
        "wire: role {role} slot {slot} cable {} on 127.0.0.1:{port}",
        status_word(cable.status())
    );

    // This side's own word, per frame: two fixed halves plus the round, so a byte out of order or
    // a stale value is visible in the transcript rather than plausible.
    let base: u32 = if slot == 0 { 0x1100_0000 } else { 0x2200_0000 };
    let mut normal = 0usize;
    for round in 0..FRAMES as usize {
        let out = base + round as u32;
        // The child is a slave on this cable: it arms only once the master's word is on the line.
        if slot != 0 {
            wait_ready(&mut gba, role, round);
        }
        gba.bus.write32(SIODATA32, out);
        // The host drives the clock (SC = internal); the child's transfer runs on it, as on the
        // hardware — its own bit 0 is 0 (external clock).
        let internal = if slot == 0 { serial::SC_INTERNAL } else { 0 };
        let armed = serial::LENGTH32 | internal | serial::START;
        gba.bus.write16(SIOCNT, armed);
        // Wall clock to wait, emulated cycles to claim: see [`transfer`].
        let reading = transfer(&mut gba, &cable, armed, role, &format!("round {round}"));
        let got = gba.bus.read32(SIODATA32);
        let crossed = gba.bus.serial.transcript.last().copied();
        let (frame, units) = match crossed {
            Some(crossed) => (crossed.frame, crossed.units),
            None => (gba.bus.lcd.frames, 0),
        };
        println!(
            "wire: role {role} round {round} frame {frame} bits 32 out {out:08X} got {got:08X} \
             units {units} cycles {} micros {}",
            reading.cycles,
            reading.wall.as_micros()
        );
        normal += 1;
        gba.run_frame();
    }

    // The multiplayer rounds. `SIOMLT_SEND` is written **once**, before `SIOCNT` selects
    // multiplayer mode: a store made *while* that mode is on pushes the register onto the cable,
    // so a round that rewrote it would put two words on the wire for one transfer (the store's and
    // the start's). That defect lives in `crate::serial` and is H10's card, not this one's:
    // here the send register is fixed for the session and each round's word on the wire is the one
    // `start()` put there.
    // The two phases must not overlap. A normal-mode word is four bytes and a multiplayer word is
    // two, so a side that switched modes while its peer was still in the first phase would have its
    // words read as the other kind and never recover. A tenth of a second is longer than the worst
    // round measured above (9.6 ms) by an order of magnitude, and it is a pause, not a wait on the
    // protocol: nothing here is timed across it.
    std::thread::sleep(Duration::from_millis(100));
    let mine: u16 = if slot == 0 { 0xBEE0 } else { 0x5A50 };
    gba.bus.write16(SIOMLT_SEND, mine);
    let mut multi = 0usize;
    let mut two_unit_rounds = 0usize;
    for round in 0..MULTI_ROUNDS {
        // The opposite discipline from normal mode: here every unit puts its own word, and the
        // parent's clock collects them, so the *child* arms at once and the parent arms only when
        // the child's word is already on the line. That way the parent's first slot boundary finds
        // an answer rather than racing a poll schedule a socket answer can outrun.
        if slot == 0 {
            wait_ready(&mut gba, role, FRAMES as usize + round);
        }
        gba.bus.write16(
            SIOCNT,
            serial::MULTI_MODE | MULTI_BAUD_INDEX | serial::START,
        );
        let started = Instant::now();
        let cycles = run_until_idle(&mut gba);
        let micros = started.elapsed().as_micros();
        let parent = gba.bus.read16(SIOMULTI0);
        let child = gba.bus.read16(SIOMULTI1);
        let crossed = gba.bus.serial.transcript.last().copied();
        let (frame, units) = match crossed {
            Some(crossed) => (crossed.frame, crossed.units),
            None => (gba.bus.lcd.frames, 0),
        };
        if units == 2 {
            two_unit_rounds += 1;
        }
        println!(
            "wire: role {role} multi {round} frame {frame} units {units} boot {mine:04X} \
             parent {parent:04X} child {child:04X} cycles {cycles} micros {micros}"
        );
        multi += 1;
        gba.run_frame();
    }
    // The last answer is still crossing the socket when the rounds end. A player 2 that closed the
    // port the instant its own run finished would strand the host's last transfer; a fifth of a
    // second is far longer than a loopback round trip and costs the run nothing it measures.
    if slot != 0 {
        std::thread::sleep(Duration::from_millis(200));
    }
    gba.bus.serial.detach();
    Run {
        normal,
        multi,
        two_unit_rounds,
    }
}

fn status_word(status: LinkStatus) -> &'static str {
    match status {
        LinkStatus::Off => "off",
        LinkStatus::Waiting => "waiting",
        LinkStatus::Connected => "connected",
        LinkStatus::Dropped => "dropped",
    }
}

/// The body of a child process: run one side, print the transcript, and leave. Only the one test is
/// filtered in, so nothing else runs here.
fn child(role: &str) -> ! {
    let port: u16 = std::env::var(CHILD_PORT)
        .expect("the port")
        .parse()
        .expect("a port number");
    let run = transcript(role, port);
    // The rows were printed as they happened; only the tally is left.
    println!(
        "wire: role {role} done rounds {} multi {} two-unit {}",
        run.normal, run.multi, run.two_unit_rounds
    );
    // The emulator's sockets close when this process ends: `exit` is the peer going away.
    std::process::exit(0);
}

/// True when this process is one side of the cable rather than the test that spawns both.
fn child_role() -> Option<String> {
    std::env::var(CHILD_ROLE).ok()
}

/// Only one test in this file may hold a loopback port at a time. They share `127.0.0.1` and the
/// ephemeral range, and two of them racing for one number would show up as a wrong word rather than
/// as a bind error, so the file serialises itself rather than trusting luck.
static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    PORT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A port nothing is listening on: bound and released, so the listener a moment later is ours.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
    listener.local_addr().expect("an address").port()
}

/// Re-run this test binary as one side of the cable, with the one test filtered in.
fn spawn_side(role: &str, port: u16) -> std::process::Child {
    let exe = std::env::current_exe().expect("the test binary's own path");
    Command::new(exe)
        .args([
            "--exact",
            "two_processes_trade_one_word_a_frame_over_a_socket",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ROLE, role)
        .env(CHILD_PORT, port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("a child process")
}

/// Read a child's stdout on a thread of its own: a transcript larger than a pipe buffer would
/// otherwise block the child that is writing it, and the other child would wait for that one's
/// bytes — a deadlock between two processes that are only supposed to be trading words. Both
/// children get a reader before either is joined.
fn reader(child: &mut std::process::Child) -> std::thread::JoinHandle<Vec<String>> {
    let mut stdout = child.stdout.take().expect("a piped stdout");
    let mut stderr = child.stderr.take().expect("a piped stderr");
    std::thread::spawn(move || {
        // Both pipes are drained on threads of their own, for the same reason: a child's panic
        // message must not be able to fill a pipe and stall it.
        let err_reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let errs = err_reader.join().unwrap_or_default();
        text.lines()
            .chain(errs.lines())
            .map(str::to_owned)
            .collect()
    })
}

/// One normal-mode round, as a side printed it.
#[derive(Clone, Copy)]
struct WireRow {
    round: usize,
    frame: u64,
    out: u32,
    got: u32,
    units: usize,
    /// Emulated cycles the transfer cost — the claim.
    cycles: u64,
    /// Wall clock it took — reported, never asserted.
    micros: u128,
}

/// `wire: role host round 7 frame 7 bits 32 out 11000007 got 22000007 units 2 cycles 640 micros 210`
fn parse_round(line: &str) -> Option<WireRow> {
    let mut words = line.split_whitespace();
    if words.next()? != "wire:" {
        return None;
    }
    if words.next()? != "role" {
        return None;
    }
    let _role = words.next()?;
    if words.next()? != "round" {
        return None;
    }
    let round: usize = words.next()?.parse().ok()?;
    if words.next()? != "frame" {
        return None;
    }
    let frame: u64 = words.next()?.parse().ok()?;
    if words.next()? != "bits" {
        return None;
    }
    let _bits = words.next()?;
    if words.next()? != "out" {
        return None;
    }
    let out = u32::from_str_radix(words.next()?, 16).ok()?;
    if words.next()? != "got" {
        return None;
    }
    let got = u32::from_str_radix(words.next()?, 16).ok()?;
    if words.next()? != "units" {
        return None;
    }
    let units: usize = words.next()?.parse().ok()?;
    if words.next()? != "cycles" {
        return None;
    }
    let cycles: u64 = words.next()?.parse().ok()?;
    if words.next()? != "micros" {
        return None;
    }
    let micros: u128 = words.next()?.parse().ok()?;
    Some(WireRow {
        round,
        frame,
        out,
        got,
        units,
        cycles,
        micros,
    })
}

/// One multiplayer round, as a side printed it.
#[derive(Clone, Copy)]
struct MultiRow {
    round: usize,
    units: usize,
    boot: u16,
    parent: u16,
    child: u16,
    /// Emulated cycles the round cost: the protocol's own window.
    cycles: u64,
    /// Wall clock it took — reported, never asserted.
    micros: u128,
}

/// `wire: role host multi 3 frame 130 units 2 boot BEE0 parent BEE0 child 5A50 cycles 96110 micros 180`
fn parse_multi(line: &str) -> Option<MultiRow> {
    let mut words = line.split_whitespace().skip(1);
    if words.next()? != "role" {
        return None;
    }
    let _role = words.next()?;
    if words.next()? != "multi" {
        return None;
    }
    let round: usize = words.next()?.parse().ok()?;
    if words.next()? != "frame" {
        return None;
    }
    let _frame: u64 = words.next()?.parse().ok()?;
    if words.next()? != "units" {
        return None;
    }
    let units: usize = words.next()?.parse().ok()?;
    if words.next()? != "boot" {
        return None;
    }
    let boot = u16::from_str_radix(words.next()?, 16).ok()?;
    if words.next()? != "parent" {
        return None;
    }
    let parent = u16::from_str_radix(words.next()?, 16).ok()?;
    if words.next()? != "child" {
        return None;
    }
    let child = u16::from_str_radix(words.next()?, 16).ok()?;
    if words.next()? != "cycles" {
        return None;
    }
    let cycles: u64 = words.next()?.parse().ok()?;
    if words.next()? != "micros" {
        return None;
    }
    let micros: u128 = words.next()?.parse().ok()?;
    Some(MultiRow {
        round,
        units,
        boot,
        parent,
        child,
        cycles,
        micros,
    })
}

/// Both sides' lines, for a failure message: a red run prints what each side saw, which is the
/// whole of what a reader has to go on when this test fails on a loaded machine.
fn both_sides(host: &[String], peer: &[String]) -> String {
    format!("host:\n{}\nplayer 2:\n{}", host.join("\n"), peer.join("\n"))
}

#[test]
fn two_processes_trade_one_word_a_frame_over_a_socket() {
    if let Some(role) = child_role() {
        child(&role);
    }
    let _only = exclusive();
    let port = free_port();
    // The listener first, so the child that dials finds it: the other order works too — player 2
    // retries — but the proof does not need to lean on that here (a test below does).
    let mut host = spawn_side("host", port);
    let mut peer = spawn_side("player2", port);
    // Both readers are started before either child is waited on, so neither can block on a full
    // pipe while the other waits for its bytes.
    let host_reader = reader(&mut host);
    let peer_reader = reader(&mut peer);
    let host_all = host_reader.join().expect("the host's reader thread");
    let peer_all = peer_reader.join().expect("player 2's reader thread");
    let host_ok = host.wait().expect("the host process").success();
    let peer_ok = peer.wait().expect("the player 2 process").success();
    let wire_lines = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter(|line| line.starts_with("wire: "))
            .cloned()
            .collect()
    };
    let host_wire = wire_lines(&host_all);
    let peer_wire = wire_lines(&peer_all);
    assert!(
        host_ok && peer_ok,
        "a side failed:\nhost:\n{}\nplayer 2:\n{}",
        host_all.join("\n"),
        peer_all.join("\n")
    );
    for line in host_wire.iter().chain(peer_wire.iter()) {
        println!("{line}");
    }

    let host_rows: Vec<_> = host_wire.iter().filter_map(|l| parse_round(l)).collect();
    let peer_rows: Vec<_> = peer_wire.iter().filter_map(|l| parse_round(l)).collect();
    assert_eq!(
        host_rows.len(),
        FRAMES as usize,
        "the host's transcript is one row a frame:\n{}",
        host_all.join("\n")
    );
    assert_eq!(
        peer_rows.len(),
        FRAMES as usize,
        "player 2's transcript is one row a frame:\n{}",
        peer_all.join("\n")
    );

    // Every round: each side got exactly what the other sent, in order, from two units, and spent
    // less than two frames of emulated time doing it. The last of those is the claim; the `micros`
    // beside it is this host's scheduler and nothing is asserted about it.
    let mut frames_apart = 0u64;
    let mut worst_cycles = 0u64;
    let mut worst_micros = 0u128;
    let window = u64::from(serial::LINK_TIMEOUT_CYCLES);
    for (host_row, peer_row) in host_rows.iter().zip(&peer_rows) {
        let round = host_row.round;
        assert_eq!(
            round,
            peer_row.round,
            "the two transcripts disagree on the round\n{}",
            both_sides(&host_all, &peer_all)
        );
        assert_eq!(
            host_row.got,
            peer_row.out,
            "round {round}: the host did not receive player 2's word\n{}",
            both_sides(&host_all, &peer_all)
        );
        assert_eq!(
            peer_row.got,
            host_row.out,
            "round {round}: player 2 did not receive the host's word\n{}",
            both_sides(&host_all, &peer_all)
        );
        assert_eq!(
            host_row.units,
            2,
            "round {round}: the host saw {} units\n{}",
            host_row.units,
            both_sides(&host_all, &peer_all)
        );
        assert_eq!(
            peer_row.units,
            2,
            "round {round}: player 2 saw {} units\n{}",
            peer_row.units,
            both_sides(&host_all, &peer_all)
        );
        assert_eq!(
            host_row.out,
            0x1100_0000 + round as u32,
            "the host's own sequence"
        );
        assert_eq!(
            peer_row.out,
            0x2200_0000 + round as u32,
            "player 2's own sequence"
        );
        // The hardware fact: the answer was there inside the two-frame window, on both ends. A
        // transfer that spent the window would have completed with ones — its `units` check above
        // fails first — and one that spent *more* than the window never existed.
        for (side, spent) in [("host", host_row.cycles), ("player 2", peer_row.cycles)] {
            assert!(
                spent < window,
                "round {round}: {side} spent {spent} emulated cycles, at or past the two-frame \
                 window of {window} — the harness waited in emulated time as well as wall time\n{}",
                both_sides(&host_all, &peer_all)
            );
        }
        worst_cycles = worst_cycles.max(host_row.cycles.max(peer_row.cycles));
        worst_micros = worst_micros.max(host_row.micros);
        frames_apart = frames_apart.max(host_row.frame.abs_diff(peer_row.frame));
    }
    println!(
        "wire: {FRAMES} transfers crossed; the two sides were never more than {frames_apart} \
         frame(s) apart; the slowest transfer cost {worst_cycles} emulated cycles of the {window} \
         the hardware allows ({:.2} %) and {worst_micros} us of wall clock, which is reported and \
         not asserted",
        worst_cycles as f64 * 100.0 / window as f64
    );

    // The multiplayer rounds: two units, both slots filled, both sides reading the same two.
    let host_multi: Vec<_> = host_wire.iter().filter_map(|l| parse_multi(l)).collect();
    let peer_multi: Vec<_> = peer_wire.iter().filter_map(|l| parse_multi(l)).collect();
    assert_eq!(host_multi.len(), MULTI_ROUNDS);
    assert_eq!(peer_multi.len(), MULTI_ROUNDS);
    let mut two_unit_rounds = 0usize;
    let mut worst_multi_cycles = 0u64;
    let mut worst_multi_micros = 0u128;
    let round_window = u64::from(serial::multi_cycles(
        serial::MULTI_BAUD[usize::from(MULTI_BAUD_INDEX)],
        2,
    ));
    for (host_row, peer_row) in host_multi.iter().zip(&peer_multi) {
        let round = host_row.round;
        assert_eq!(round, peer_row.round);
        assert_eq!(host_row.boot, 0xBEE0, "the host's send register");
        assert_eq!(peer_row.boot, 0x5A50, "player 2's send register");
        // The round lasts the protocol's own window on both ends, whatever answered: 18 bits a
        // unit, a gap bit, and one unit's word for the timeout that a pair does not use up.
        for (side, spent) in [("host", host_row.cycles), ("player 2", peer_row.cycles)] {
            assert!(
                spent.abs_diff(round_window) <= 2,
                "round {round}: {side} spent {spent} emulated cycles, not the protocol's \
                 {round_window}\n{}",
                both_sides(&host_all, &peer_all)
            );
        }
        worst_multi_cycles = worst_multi_cycles.max(host_row.cycles);
        worst_multi_micros = worst_multi_micros.max(host_row.micros);
        if host_row.units == 2 && peer_row.units == 2 {
            two_unit_rounds += 1;
            assert_eq!(
                host_row.parent, 0xBEE0,
                "round {round}: slot 0 is the parent's word"
            );
            assert_eq!(
                host_row.child, 0x5A50,
                "round {round}: slot 1 is the child's word"
            );
            assert_eq!(
                peer_row.parent, 0xBEE0,
                "round {round}: both ends read the same slots"
            );
            assert_eq!(peer_row.child, 0x5A50);
        }
    }
    println!(
        "wire: {two_unit_rounds} of {MULTI_ROUNDS} multiplayer rounds carried two units; each \
         round cost {worst_multi_cycles} emulated cycles of the protocol's {round_window} and at \
         worst {worst_multi_micros} us of wall clock (reported, not asserted)"
    );
    // Two rounds of slack, and no more: the harness runs a 96 110-cycle round in a couple of
    // milliseconds of wall time, so a scheduling pause can still cost a slot. Five consecutive
    // runs under load printed 24 of 24; before `run_until_idle` learned to give the transport its
    // turn, the same runs printed 20 or 21.
    assert!(
        two_unit_rounds + 2 >= MULTI_ROUNDS,
        "only {two_unit_rounds} of {MULTI_ROUNDS} multiplayer rounds carried two units"
    );
}

/// The socket's own round trip, with no machine in the way: one byte out, the peer's byte back, two
/// hundred times. This is the number the transfers above pay on top of their emulated clock, and
/// separating it is what lets the report say which part of a 143 µs transfer is the cable.
#[test]
fn the_socket_round_trip_is_measured_without_a_machine() {
    let _only = exclusive();
    let port = free_port();
    let host = link::tcp_host(port).expect("the listener");
    let guest = link::tcp_player2(port);
    let deadline = Instant::now() + Duration::from_secs(10);
    while (host.status() != LinkStatus::Connected || guest.status() != LinkStatus::Connected)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host.status(), LinkStatus::Connected);
    assert_eq!(guest.status(), LinkStatus::Connected);
    let mut samples = Vec::with_capacity(200);
    for round in 0..200u8 {
        let started = Instant::now();
        host.send(round);
        assert_eq!(
            guest.recv(Duration::from_secs(2)),
            Some(round),
            "the host's byte"
        );
        guest.send(round.wrapping_add(1));
        assert_eq!(
            host.recv(Duration::from_secs(2)),
            Some(round.wrapping_add(1)),
            "player 2's answer"
        );
        samples.push(started.elapsed().as_micros());
    }
    samples.sort_unstable();
    println!(
        "wire: socket round trip, 200 samples: min {} us, median {} us, max {} us",
        samples[0],
        samples[samples.len() / 2],
        samples[samples.len() - 1]
    );
    assert!(samples[0] > 0, "a measured round trip takes time");
}

/// A refused connection says which one failed, in one sentence: a port another program holds.
#[test]
fn a_port_already_held_is_one_sentence_not_a_cable() {
    let _only = exclusive();
    let port = free_port();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("a listener");
    let refused = link::tcp_host(port);
    assert!(
        refused.is_err(),
        "a second listener on the same port must fail"
    );
    let why = refused.err().unwrap();
    println!("wire: refusal, verbatim: {why}");
    assert!(
        why.starts_with("link: cannot listen on 127.0.0.1:"),
        "{why}"
    );
    assert!(
        why.contains(&port.to_string()),
        "the sentence names the port: {why}"
    );
    assert!(
        why.len() > "link: cannot listen on 127.0.0.1:".len(),
        "the sentence names the failure the operating system reported: {why}"
    );
    drop(listener);
}

/// The peer leaves in the middle of a transfer. The engine's own answer for a silent peer is the
/// thing under test: the transfer must complete, the cartridge must read `FFFFh`, the player must be
/// told once, and frames must keep advancing.
#[test]
fn the_peer_leaving_mid_transfer_completes_the_transfer_and_says_so_once() {
    let _only = exclusive();
    let port = free_port();
    let mut host = machine();
    let cable: Link = link::tcp_host(port).expect("the listener");
    host.attach_link(Some(cable.clone()), 0);
    let peer = link::tcp_player2(port);
    let deadline = Instant::now() + Duration::from_secs(10);
    while cable.status() != LinkStatus::Connected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        cable.status(),
        LinkStatus::Connected,
        "the pair must meet first"
    );
    // One clean transfer, so the cable is proved live before it is cut.
    host.bus.write32(SIODATA32, 0x1234_5678);
    host.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );
    run_until_idle(&mut host);
    assert_eq!(host.bus.serial.transcript.len(), 1);
    // Now a transfer, and the peer gone before its bytes arrive.
    host.bus.write32(SIODATA32, 0xDEAD_BEEF);
    host.bus.write16(
        SIOCNT,
        serial::LENGTH32 | serial::SC_INTERNAL | serial::START,
    );
    drop(peer);
    let started = Instant::now();
    let cycles = run_until_idle(&mut host);
    assert!(
        cycles < 3 * u64::from(CYCLES_PER_FRAME),
        "the transfer ended on its own clock ({cycles} cycles), it did not hang"
    );
    assert_eq!(
        host.bus.read32(SIODATA32),
        0xFFFF_FFFF,
        "a dead line reads ones, as an unplugged cable does"
    );
    assert_eq!(
        host.bus.serial.message,
        Some(serial::Message::PeerGone),
        "the player is told the peer left"
    );
    let frames_before = host.frames;
    for _ in 0..3 {
        host.run_frame();
    }
    assert_eq!(host.frames, frames_before + 3, "the machine keeps running");
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// The same two ends, in one process: A listens, B dials, and both machines run a transfer. This is
/// the socket path without the process boundary, so a failure here points at the framing rather
/// than at the spawn.
#[test]
fn a_host_and_a_player_2_in_one_process_swap_a_word() {
    let _only = exclusive();
    let port = free_port();
    let mut host = machine();
    let mut guest = machine();
    let host_cable = link::tcp_host(port).expect("the listener");
    let guest_cable = link::tcp_player2(port);
    host.attach_link(Some(host_cable.clone()), 0);
    guest.attach_link(Some(guest_cable.clone()), 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while (host_cable.status() != LinkStatus::Connected
        || guest_cable.status() != LinkStatus::Connected)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let host_armed = serial::LENGTH32 | serial::SC_INTERNAL | serial::START;
    let guest_armed = serial::LENGTH32 | serial::START;
    host.bus.write32(SIODATA32, 0xCAFE_0001);
    guest.bus.write32(SIODATA32, 0xFEED_0002);
    host.bus.write16(SIOCNT, host_armed);
    guest.bus.write16(SIOCNT, guest_armed);
    // Both words were put on the socket by the two stores above, so each machine's answer is
    // already queued and each is run with the same two clocks as the two-process harness: the
    // waiting is wall clock, the claim is emulated cycles. The machines are stepped one after the
    // other, not in lockstep — the cable is a byte queue with no clock line (see the module
    // documentation of `crate::serial`), so neither machine's emulated clock can see the
    // other's, and stepping them together only ever starved the four socket threads this test's
    // process owns.
    let host_reading = transfer(&mut host, &host_cable, host_armed, "host", "the swap");
    let guest_reading = transfer(
        &mut guest,
        &guest_cable,
        guest_armed,
        "player 2",
        "the swap",
    );
    let window = u64::from(serial::LINK_TIMEOUT_CYCLES);
    println!(
        "wire: one process, one word each way: host {} emulated cycles ({} us of wall clock), \
         player 2 {} emulated cycles ({} us of wall clock), against a two-frame window of {window}",
        host_reading.cycles,
        host_reading.wall.as_micros(),
        guest_reading.cycles,
        guest_reading.wall.as_micros()
    );
    assert_eq!(host.bus.read32(SIODATA32), 0xFEED_0002);
    assert_eq!(guest.bus.read32(SIODATA32), 0xCAFE_0001);
    assert_eq!(host.bus.serial.transcript[0].units, 2);
    assert_eq!(guest.bus.serial.transcript[0].units, 2);
    for (side, reading) in [("host", host_reading), ("player 2", guest_reading)] {
        assert!(
            reading.cycles < window,
            "{side} spent {} emulated cycles, at or past the two-frame window of {window}",
            reading.cycles
        );
    }
}

/// The defect this file was repaired for, made deterministic instead of hoped away: a peer whose
/// answer is three hundred milliseconds late in *wall* clock, and then arrives a byte at a time.
///
/// Nothing about the cable is slow here — a loopback hop is tens of microseconds. The peer's own
/// thread simply sleeps between reading the host's word and answering it, which is what being off
/// a core for a scheduler slice looks like from the other end of the cable; and it writes the four
/// bytes of its answer 25 ms apart, which is the same fact at the other end of a word. The hardware
/// claim does not move: the answer arrives inside the two-frame window of *emulated* time, 561 792
/// cycles, because the emulated clock is not a measure of how long this machine took to run the
/// test.
///
/// This is the regression test for the coupling the card was written about. On the harness it
/// replaces — a tight loop against the same two-frame budget — it fails every time, and with the
/// same signature the flake had: the transfer completes with ones, `units 0`, and a `PeerSilent`
/// message that this socket never earned.
#[test]
fn an_answer_three_hundred_milliseconds_late_is_late_in_wall_time_not_emulated_time() {
    let _only = exclusive();
    let port = free_port();
    let mut host = machine();
    let cable: Link = link::tcp_host(port).expect("the listener");
    host.attach_link(Some(cable.clone()), 0);
    let peer = link::tcp_player2(port);
    let deadline = Instant::now() + Duration::from_secs(10);
    while cable.status() != LinkStatus::Connected && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        cable.status(),
        LinkStatus::Connected,
        "the pair must meet first"
    );

    const LATE: Duration = Duration::from_millis(300);
    const ANSWER: u32 = 0x5A5A_1234;
    // The delay is inside the transfer, not before it: the peer reads the host's word first, then
    // does nothing for 300 ms, then answers. From the host's side that is a peer that stopped
    // being scheduled, and nothing about it is visible on the cable.
    //
    // The handle is cloned into the thread and the original is held until the host's transfer is
    // over. Dropping the last handle shuts the socket down — that is what "the peer left" is — and
    // a peer that leaves in the same instant its word arrives is the *other* test in this file, not
    // this one.
    let sender = peer.clone();
    let peer_thread = std::thread::spawn(move || {
        let mut bytes = [0u8; 4];
        for byte in bytes.iter_mut() {
            *byte = sender
                .recv(Duration::from_secs(5))
                .expect("the host's word, before the delay");
        }
        std::thread::sleep(LATE);
        // Four socket writes, a scheduler slice apart: a 32-bit answer that arrives a byte at a
        // time. This is the second half of the coupling — a value that is *partly* on the wire
        // stops the emulated clock from being paused (the engine is entitled to poll), so the
        // harness paces those polls in wall time instead.
        for byte in ANSWER.to_le_bytes() {
            sender.send(byte);
            std::thread::sleep(Duration::from_millis(25));
        }
        u32::from_le_bytes(bytes)
    });

    host.bus.write32(SIODATA32, 0xDEAD_0001);
    let armed = serial::LENGTH32 | serial::SC_INTERNAL | serial::START;
    host.bus.write16(SIOCNT, armed);
    let reading = transfer(&mut host, &cable, armed, "host", "an answer 300 ms late");
    let seen = peer_thread.join().expect("the peer's thread");
    let window = u64::from(serial::LINK_TIMEOUT_CYCLES);
    println!(
        "wire: a peer that answered {LATE:?} late: the host spent {} emulated cycles ({:.2} % of \
         the {window}-cycle window) and {} us of wall clock, and read {:08X}",
        reading.cycles,
        reading.cycles as f64 * 100.0 / window as f64,
        reading.wall.as_micros(),
        host.bus.read32(SIODATA32)
    );

    assert_eq!(seen, 0xDEAD_0001, "the peer received the host's word");
    assert_eq!(
        host.bus.read32(SIODATA32),
        ANSWER,
        "the late answer is the answer"
    );
    assert_eq!(
        host.bus.serial.transcript[0].units, 2,
        "a slow scheduler is not a missing unit"
    );
    assert!(
        reading.cycles < window,
        "the transfer cost {} emulated cycles, at or past the two-frame window of {window}",
        reading.cycles
    );
    assert!(
        reading.wall >= LATE,
        "the harness really did wait the peer's {} ms of wall clock, and took {:?}",
        LATE.as_millis(),
        reading.wall
    );
}
