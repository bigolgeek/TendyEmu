//! The `$FF00` packet receiver, `MLT_REQ` and the dispatch.
//!
//! Every expected byte below is the spec's layout and nothing else: Pan Docs, *Command Packet
//! Transfers* ("setting bit 4 to 0 will indicate a `0` bit, and setting bit 5 to 0 will indicate a
//! `1` bit", "the LSB of each byte first", "finally, a `0` bit must be transferred as a stop bit")
//! and *Command Summary*'s header byte, `Command Code × 8 + Length` (so `command() = header >> 3`
//! and `length() = header & 7`). The header literals in the table are written by hand from that
//! rule — `PAL23` is code `$01`, so a one-packet PAL23 is `0x01 << 3 | 1 == 0x09` — so a receiver
//! that shifted the other way, or read the length from the wrong nibble, cannot pass them.
//!
//! The polarity is Pan Docs' and is pinned twice: `p10_is_one_and_p20_is_zero` sends 128 pulses
//! on one line only and expects all ones or all zeros, and the table carries the all-`0xFF` and
//! all-`0x00` packets so the byte-level rows agree with it.

mod common;

use tendy_gb::Machine;
use tendy_gb::sgb::{Command, DEFAULT_RAMP, Packet, Responder, Sgb};
use tendy_gb::state::{StateReader, StateWriter};

/// The spec's PAL01 vector, byte for byte (`SPEC-FROM-S0.md`, test 1).
const PAL01: [u8; 16] = [
    0x01, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0x00,
];

/// `$11` MLT_REQ, one packet, control byte 1 — two joypads.
const MLT_REQ_TWO: [u8; 16] = [
    0x89, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// `$0E` ICON_EN, one packet, byte 1 = `$04`: register-file transfer disabled.
const ICON_EN_4: [u8; 16] = [
    0x71, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// One pulse per write, as the spec's `send` helper: start, 128 data bits LSB-first, stop bit 0.
fn send(resp: &mut Responder, bytes: [u8; 16]) -> Option<Packet> {
    assert!(
        resp.write_joyp(0x00).is_none(),
        "start pulse completed a packet"
    );
    for byte in bytes {
        for bit in 0..8 {
            let one = (byte >> bit) & 1 == 1;
            assert!(
                resp.write_joyp(if one { 0x10 } else { 0x20 }).is_none(),
                "a data pulse completed a packet"
            );
        }
    }
    resp.write_joyp(0x20) // stop bit: a 0
}

/// The same pulses, through the machine's `$FF00` hook. Returns nothing: the bus delivers the
/// packet itself on the stop bit.
fn bus_send(m: &mut Machine, bytes: [u8; 16]) {
    m.bus.write(0xFF00, 0x00);
    for byte in bytes {
        for bit in 0..8 {
            let one = (byte >> bit) & 1 == 1;
            m.bus.write(0xFF00, if one { 0x10 } else { 0x20 });
        }
    }
    m.bus.write(0xFF00, 0x20);
}

/// One table row: the exact 16 bytes written, and the command and length the spec's header byte
/// layout gives them.
struct Row {
    name: &'static str,
    bytes: [u8; 16],
    command: u8,
    length: u8,
}

/// The spec's table. Each header literal is `command << 3 | length` written out by hand:
/// PAL01 `$00`/1 = `0x01`, PAL23 `$01`/1 = `0x09`, PAL03 `$02`/1 = `0x11`, PAL12 `$03`/1 = `0x19`,
/// PAL_SET `$0A`/3 = `0x53`, PAL_TRN `$0B`/0 = `0x58`, MLT_REQ `$11`/1 = `0x89`, ICON_EN
/// `$0E`/1 = `0x71`, ATTR_TRN `$15`/0 = `0xA8`, ATTR_SET `$16`/0 = `0xB0`, MASK_EN `$17`/0 =
/// `0xB8`, PAL_PRI `$19`/0 = `0xC8`, DATA_TRN `$10`/0 = `0x80`.
const ROWS: &[Row] = &[
    Row {
        name: "PAL01, 15 data bytes, last one $00 (padding that must not be trimmed)",
        bytes: PAL01,
        command: 0x00,
        length: 1,
    },
    Row {
        name: "PAL23 ($01 << 3 | 1)",
        bytes: [
            0x09, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0xE0, 0x03, 0x00, 0x7C, 0xFF,
            0x7F, 0x00,
        ],
        command: 0x01,
        length: 1,
    },
    Row {
        name: "PAL03 ($02 << 3 | 1)",
        bytes: [
            0x11, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0xE0, 0x03, 0x00, 0x7C, 0xFF,
            0x7F, 0x00,
        ],
        command: 0x02,
        length: 1,
    },
    Row {
        name: "PAL12 ($03 << 3 | 1)",
        bytes: [
            0x19, 0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F, 0xE0, 0x03, 0x00, 0x7C, 0xFF,
            0x7F, 0x00,
        ],
        command: 0x03,
        length: 1,
    },
    Row {
        name: "PAL_SET ($0A << 3 | 3)",
        bytes: [
            0x53, 0x00, 0x01, 0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x0A,
        length: 3,
    },
    Row {
        name: "PAL_TRN ($0B << 3 | 0)",
        bytes: [
            0x58, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x0B,
        length: 0,
    },
    Row {
        name: "ATTR_TRN ($15 << 3 | 0)",
        bytes: [
            0xA8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x15,
        length: 0,
    },
    Row {
        name: "ATTR_SET ($16 << 3 | 0)",
        bytes: [
            0xB0, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x16,
        length: 0,
    },
    Row {
        name: "MASK_EN ($17 << 3 | 0)",
        bytes: [
            0xB8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x17,
        length: 0,
    },
    Row {
        name: "MLT_REQ ($11 << 3 | 1), two joypads",
        bytes: MLT_REQ_TWO,
        command: 0x11,
        length: 1,
    },
    Row {
        name: "ICON_EN ($0E << 3 | 1)",
        bytes: ICON_EN_4,
        command: 0x0E,
        length: 1,
    },
    Row {
        name: "PAL_PRI ($19 << 3 | 0)",
        bytes: [
            0xC8, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x19,
        length: 0,
    },
    Row {
        name: "DATA_TRN ($10 << 3 | 0), last byte $FF (padding that must not be trimmed)",
        bytes: [
            0x80, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x0F, 0x1E, 0x2D, 0x3C, 0x4B,
            0x5A, 0xFF,
        ],
        command: 0x10,
        length: 0,
    },
    Row {
        name: "all ones: 128 pulses on P14-low decode to $FF everywhere",
        bytes: [0xFF; 16],
        command: 0x1F,
        length: 7,
    },
    Row {
        name: "all zeros: 128 pulses on P15-low decode to $00 everywhere",
        bytes: [0x00; 16],
        command: 0x00,
        length: 0,
    },
    // The length rule for command ids 0-3: the low three bits are the count, the high five the
    // code, for every command in 0-3 and every length 1-7.
    Row {
        name: "command 0, length 2 ($00 << 3 | 2)",
        bytes: [
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x00,
        length: 2,
    },
    Row {
        name: "command 1, length 3 ($01 << 3 | 3)",
        bytes: [
            0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x01,
        length: 3,
    },
    Row {
        name: "command 2, length 5 ($02 << 3 | 5)",
        bytes: [
            0x15, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x02,
        length: 5,
    },
    Row {
        name: "command 3, length 7 ($03 << 3 | 7)",
        bytes: [
            0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ],
        command: 0x03,
        length: 7,
    },
];

#[test]
fn the_packet_table_decodes_to_the_spec_layout() {
    let mut good = 0;
    for row in ROWS {
        let mut resp = Responder::new();
        let got = send(&mut resp, row.bytes)
            .unwrap_or_else(|| panic!("{}: the stop bit produced no packet", row.name));
        assert_eq!(got.bytes, row.bytes, "{}: bytes", row.name);
        assert_eq!(got.command(), row.command, "{}: command", row.name);
        assert_eq!(got.length(), row.length, "{}: length", row.name);
        // Second send in a row: the receiver is idle again, and a second packet is identical.
        let again = send(&mut resp, row.bytes)
            .unwrap_or_else(|| panic!("{}: the second packet produced nothing", row.name));
        assert_eq!(again, got, "{}: the second packet differs", row.name);
        good += 1;
    }
    assert_eq!(good, ROWS.len());
    assert!(
        good >= 12,
        "the spec's table wants at least twelve good packets, got {good}"
    );
}

#[test]
fn p10_is_one_and_p20_is_zero() {
    // Pan Docs: "setting bit 4 to 0 will indicate a `0` bit, and setting bit 5 to 0 will indicate
    // a `1` bit". A pulse is one write, so 0x10 (bit 5 low) is a 1 and 0x20 (bit 4 low) is a 0.
    let mut ones = Responder::new();
    assert!(ones.write_joyp(0x00).is_none());
    for _ in 0..128 {
        assert!(ones.write_joyp(0x10).is_none());
    }
    let packet = ones
        .write_joyp(0x20)
        .expect("stop bit completes the packet");
    assert_eq!(packet.bytes, [0xFF; 16], "0x10 is a 1 bit");

    let mut zeros = Responder::new();
    assert!(zeros.write_joyp(0x00).is_none());
    for _ in 0..128 {
        assert!(zeros.write_joyp(0x20).is_none());
    }
    let packet = zeros
        .write_joyp(0x20)
        .expect("stop bit completes the packet");
    assert_eq!(packet.bytes, [0x00; 16], "0x20 is a 0 bit");
}

#[test]
fn the_bits_arrive_lsb_first() {
    // 0xAA is 1010_1010: sent bit 0 first it is 0,1,0,1,..., which is exactly 0x55 read the other
    // way. A receiver that shifted the other way would return 0x55 for 0xAA.
    let mut resp = Responder::new();
    let mut bytes = [0u8; 16];
    bytes[0] = 0xAA;
    bytes[1] = 0x55;
    bytes[15] = 0x80;
    let got = send(&mut resp, bytes).expect("packet");
    assert_eq!(got.bytes, bytes);
    assert_eq!(got.bytes[0], 0xAA);
    assert_eq!(got.bytes[1], 0x55);
    assert_eq!(
        got.bytes[15], 0x80,
        "the last bit of the last byte is bit 7"
    );
}

#[test]
fn an_unknown_command_decodes_and_is_not_consumed() {
    // Header 0xFB: code $1F, length 3. Pan Docs' summary has no $1F.
    let bytes = [
        0xFB, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    let mut resp = Responder::new();
    let got = send(&mut resp, bytes).expect("a packet, whatever its command");
    assert_eq!(got.command(), 0x1F);
    assert_eq!(got.length(), 3);
    assert_eq!(Command::from_code(got.command()), Command::Unknown(0x1F));
    assert!(
        !resp.deliver(&got),
        "an unknown command is not consumed: the caller drops it"
    );
}

#[test]
fn a_truncated_packet_never_completes() {
    let mut resp = Responder::new();
    assert!(resp.write_joyp(0x00).is_none());
    for bit in 0..64 {
        // Half of PAL01's bits, no stop bit.
        let byte = PAL01[bit / 8];
        let one = (byte >> (bit % 8)) & 1 == 1;
        assert!(resp.write_joyp(if one { 0x10 } else { 0x20 }).is_none());
    }
    // The stop bit is not a stop bit here: it is bit 64 of the data.
    assert!(resp.write_joyp(0x20).is_none());
    // The next start pulse throws the fragment away; the receiver cannot splice it into a packet.
    assert!(resp.write_joyp(0x00).is_none());
    let got = send(&mut resp, PAL01).expect("a fresh packet after a truncation");
    assert_eq!(got.bytes, PAL01);

    // A stop bit with no data at all is also nothing: only 128 bits make a packet.
    let mut empty = Responder::new();
    assert!(empty.write_joyp(0x00).is_none());
    assert!(empty.write_joyp(0x20).is_none());
}

#[test]
fn spaces_and_row_selects_change_nothing() {
    let mut resp = Responder::new();
    // Idle, a single line low is an ordinary joypad row select, not a start: 400 of them must not
    // begin a packet and must not accumulate one.
    for _ in 0..200 {
        assert!(resp.write_joyp(0x20).is_none());
    }
    for _ in 0..200 {
        assert!(resp.write_joyp(0x10).is_none());
    }
    for _ in 0..50 {
        assert!(resp.write_joyp(0x30).is_none());
    }
    // Now a real packet, with the space between pulses (0x30) after every single write.
    assert!(resp.write_joyp(0x00).is_none());
    for byte in PAL01 {
        for bit in 0..8 {
            let one = (byte >> bit) & 1 == 1;
            assert!(resp.write_joyp(if one { 0x10 } else { 0x20 }).is_none());
            assert!(resp.write_joyp(0x30).is_none());
        }
    }
    let got = resp.write_joyp(0x20).expect("stop bit after the spaces");
    assert_eq!(got.bytes, PAL01);
}

#[test]
fn a_start_pulse_resets_a_packet_in_flight() {
    let mut resp = Responder::new();
    assert!(resp.write_joyp(0x00).is_none());
    // 40 bits of nonsense: the first byte would be 0xF5 if this run were kept.
    for bit in 0..40 {
        assert!(
            resp.write_joyp(if bit % 2 == 0 { 0x10 } else { 0x20 })
                .is_none()
        );
    }
    // Pan Docs: bits 4 and 5 both 0 "will reset and start the ICD2 packet receiving circuit".
    let got = send(&mut resp, PAL01).expect("the restarted packet");
    assert_eq!(
        got.bytes, PAL01,
        "the 40 stale bits must not survive the start"
    );
}

#[test]
fn a_one_stop_bit_discards_the_packet() {
    let mut resp = Responder::new();
    assert!(resp.write_joyp(0x00).is_none());
    for byte in PAL01 {
        for bit in 0..8 {
            let one = (byte >> bit) & 1 == 1;
            assert!(resp.write_joyp(if one { 0x10 } else { 0x20 }).is_none());
        }
    }
    // A `1` where the stop bit belongs: undocumented by Pan Docs, read here as discard.
    assert!(resp.write_joyp(0x10).is_none());
    // The receiver is idle now, not waiting for a stop bit: this lone `0` starts nothing.
    assert!(resp.write_joyp(0x20).is_none());
    // And nothing was delivered: the next packet is complete on its own.
    let got = send(&mut resp, PAL01).expect("packet after a discarded one");
    assert_eq!(got.bytes, PAL01);
}

#[test]
fn two_packets_in_a_row_are_two_packets() {
    let mut resp = Responder::new();
    let first = send(&mut resp, PAL01).expect("first");
    let second = send(&mut resp, PAL01).expect("second");
    assert_eq!(first, second);
    assert_eq!(first.bytes, PAL01);
}

#[test]
fn a_two_packet_transmission_reassembles_after_the_stop_bit() {
    // Pan Docs: the first packet's header says how many packets the transmission holds, so the
    // first header is `$09 << 3 | 2` = 0x4A. The receiver is byte-transparent between packets: the
    // second packet arrives after a `0` stop bit and starts from the next start pulse.
    let mut payload = [0u8; 32];
    payload[0] = 0x4A;
    for (i, byte) in payload.iter_mut().enumerate().skip(1) {
        *byte = (i as u8).wrapping_mul(7) ^ 0x5A;
    }
    let mut first = [0u8; 16];
    first.copy_from_slice(&payload[..16]);
    let mut second = [0u8; 16];
    second.copy_from_slice(&payload[16..]);

    let mut resp = Responder::new();
    let a = send(&mut resp, first).expect("first packet of the transmission");
    // A space between the packets must not disturb the second one.
    assert!(resp.write_joyp(0x30).is_none());
    let b = send(&mut resp, second).expect("second packet of the transmission");
    assert_eq!(a.bytes, first);
    assert_eq!(b.bytes, second);
    assert_eq!(a.length(), 2, "the header claims two packets");
    assert_eq!(a.command(), 0x09);

    let mut reassembled = [0u8; 32];
    reassembled[..16].copy_from_slice(&a.bytes);
    reassembled[16..].copy_from_slice(&b.bytes);
    assert_eq!(reassembled, payload);
}

#[test]
fn mlt_req_sets_the_players_and_p15_selects_them() {
    let mut resp = Responder::new();
    // One player: `$FF00` reads pass through untouched.
    assert_eq!(resp.players(), 1);
    assert_eq!(resp.read_joyp(0x30), 0x30);

    let packet = send(&mut resp, MLT_REQ_TWO).expect("MLT_REQ");
    assert!(resp.deliver(&packet), "MLT_REQ is the receiver's own");
    assert_eq!(resp.players(), 2);
    assert_eq!(resp.player(), 0);
    // Pan Docs: with P14 and P15 both high, the low nibble is $F for player 1, $E for player 2.
    assert_eq!(resp.read_joyp(0x30), 0x3F);

    // Pan Docs: "The next joypad is automatically selected when P15 goes from LOW (0) to HIGH (1)".
    assert!(resp.write_joyp(0x10).is_none(), "P15 low");
    assert!(resp.write_joyp(0x30).is_none(), "P15 rising");
    assert_eq!(resp.player(), 1);
    assert_eq!(resp.read_joyp(0x30), 0x3E);

    assert!(resp.write_joyp(0x10).is_none());
    assert!(resp.write_joyp(0x30).is_none());
    assert_eq!(resp.read_joyp(0x30), 0x3F, "two players wrap after two");
    assert_eq!(resp.player(), 0);

    // With both lines low the read is the matrix, not the ID.
    assert_eq!(resp.read_joyp(0x00), 0x00);

    // $03 is four joypads: $F, $E, $D, $C then back to $F. PAN Docs defines no $02.
    let mut four = Responder::new();
    let mut req = MLT_REQ_TWO;
    req[1] = 0x03;
    let packet = send(&mut four, req).expect("MLT_REQ 4");
    assert!(four.deliver(&packet));
    assert_eq!(four.players(), 4);
    for expected in [0x3F, 0x3E, 0x3D, 0x3C, 0x3F] {
        assert_eq!(four.read_joyp(0x30), expected);
        four.write_joyp(0x10);
        four.write_joyp(0x30);
    }

    // A control byte Pan Docs does not define (2) means one player.
    let mut odd = Responder::new();
    let mut req = MLT_REQ_TWO;
    req[1] = 0x02;
    let packet = send(&mut odd, req).expect("MLT_REQ 2");
    assert!(odd.deliver(&packet));
    assert_eq!(odd.players(), 1);
    assert_eq!(odd.read_joyp(0x30), 0x30);
}

#[test]
fn icon_en_bit_2_stops_delivery_after_the_receiver_consumes_the_pulses() {
    // Through the machine, so the hook and the dispatch are both exercised.
    let mut m = common::machine(common::synth_rom(&[]));
    m.bus.ppu.sgb = Sgb::on();
    bus_send(&mut m, ICON_EN_4);
    assert_eq!(m.bus.sgb_rx.icons(), 0x04);

    // PAL01 is still received and decoded, but `deliver` consumes it: `Sgb::apply` is never
    // reached, so the palettes keep the clean-room ramp (and the body stays untouched).
    bus_send(&mut m, PAL01);
    assert_eq!(
        m.bus.sgb_rx.icons(),
        0x04,
        "a later packet cannot clear bit 2"
    );
    for palette in m.bus.ppu.sgb.palettes().visible() {
        assert_eq!(
            *palette, DEFAULT_RAMP,
            "ICON_EN bit 2 must stop the packet before apply"
        );
    }

    // And the receiver itself says so: the same packet through a bare Responder is consumed.
    let mut resp = Responder::new();
    let icon = send(&mut resp, ICON_EN_4).expect("ICON_EN");
    assert!(resp.deliver(&icon));
    let pal = send(&mut resp, PAL01).expect("PAL01 decoded");
    assert_eq!(pal.bytes, PAL01, "the pulses still decode");
    assert!(resp.deliver(&pal), "but delivery is disabled");
}

#[test]
fn the_ff00_hook_is_inert_with_sgb_off() {
    // The receiver must not fire on a game's ordinary `P1` reads and writes. With SGB off the
    // value at `$FF00` is exactly `Joypad`'s, for the same write, as it always was.
    for value in [0x00u8, 0x10, 0x20, 0x30, 0x05, 0x15, 0x25, 0x35] {
        let mut m = common::machine(common::synth_rom(&[]));
        assert!(!m.bus.ppu.sgb.enabled());
        m.bus.write(0xFF00, value);
        assert_eq!(
            m.bus.read(0xFF00),
            m.bus.joypad.read(),
            "SGB off: a read after writing {value:#04X} is Joypad's"
        );
    }

    // A whole packet's worth of pulses with SGB off must not move a frame either. The control
    // cartridge is the same program with `$FF80` (work RAM, no receiver) in place of `$FF00`, so
    // it spends exactly the same machine cycles: removing the instructions would move the frame
    // boundary, and the check would then be about timing rather than about the hook.
    let frames = |port: u8| {
        let code = vec![
            0x3E, 0x10, 0xE0, port, // ld a,$10 ; ldh (port),a
            0x3E, 0x20, 0xE0, port, // ld a,$20 ; ldh (port),a
            0x3E, 0x00, 0xE0, port, // ld a,$00 ; ldh (port),a  (a start pulse on $FF00)
            0x18, 0xF2, // jr -14
        ];
        common::run_frames(common::synth_rom(&common::entry(&code)), 30)
    };
    let plain = frames(0x80);
    let fed = frames(0x00);
    assert_eq!(
        fed, plain,
        "an SGB-off machine's frames must not move when $FF00 carries packets"
    );
}

#[test]
fn a_packet_in_flight_is_dropped_by_load_state() {
    let mut resp = Responder::new();
    let packet = send(&mut resp, MLT_REQ_TWO).expect("MLT_REQ");
    assert!(resp.deliver(&packet));
    resp.write_joyp(0x10);
    resp.write_joyp(0x30);
    let icon = send(&mut resp, ICON_EN_4).expect("ICON_EN");
    assert!(resp.deliver(&icon));

    let mut w = StateWriter::new();
    resp.save_state(&mut w);
    let bytes = w.finish();
    let mut back = Responder::new();
    back.load_state(&mut StateReader::new(&bytes))
        .expect("the receiver's own state loads");
    assert_eq!(back.player(), resp.player());
    assert_eq!(back.players(), resp.players());
    assert_eq!(back.icons(), resp.icons());
    assert_eq!(back.read_joyp(0x30), resp.read_joyp(0x30));

    // A packet half-transferred when the state was written is not in the state: the stop bit after
    // the load closes nothing.
    let mut flying = Responder::new();
    assert!(flying.write_joyp(0x00).is_none());
    for _ in 0..40 {
        assert!(flying.write_joyp(0x10).is_none());
    }
    let mut w = StateWriter::new();
    flying.save_state(&mut w);
    let bytes = w.finish();
    let mut loaded = Responder::new();
    loaded
        .load_state(&mut StateReader::new(&bytes))
        .expect("load");
    assert!(
        loaded.write_joyp(0x20).is_none(),
        "the packet in flight must not survive load_state"
    );
    // The next real packet is intact.
    let fresh = send(&mut loaded, PAL01).expect("packet after a load");
    assert_eq!(fresh.bytes, PAL01);
}
