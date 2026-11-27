//! The DMA and HDMA controller: the eight transfer units, the A-bus step, `$420B` ordering, the
//! byte counter and the per-line HDMA table (fullsnes "SNES DMA
//! Transfers", "SNES DMA and HDMA Channel 0..7 Registers", "SNES DMA and HDMA Notes", "HDMA Table
//! Formats").
//!
//! Most of these drive the controller through [`BusPort`] directly, so the test sees each B-bus
//! write and each source address as a list rather than guessing from device state; the two that
//! need the machine's own decode (the 8-clocks-a-byte rule and the `$7E` table bank) use `Bus`.

mod common;

use tendy_snes::Machine;
use tendy_snes::addr::Addr;
use tendy_snes::cart::Cartridge;
use tendy_snes::dma::{BusPort, Dma};
use tendy_snes::ppu::LineEvents;

/// A recording bus: `data[offset]` is what a read returns, and every write is kept in order.
struct Mock {
    data: Vec<u8>,
    reads: Vec<Addr>,
    writes: Vec<(u8, u8)>,
}

impl Mock {
    /// The pattern sits at `$1000`, which is where the tests point `A1T`.
    fn new(data: &[u8]) -> Mock {
        let mut image = vec![0u8; 0x1_0000];
        image[0x1000..0x1000 + data.len()].copy_from_slice(data);
        Mock {
            data: image,
            reads: Vec::new(),
            writes: Vec::new(),
        }
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) {
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
}

impl BusPort for Mock {
    fn read(&mut self, addr: Addr) -> u8 {
        self.reads.push(addr);
        self.data[usize::from(addr.offset())]
    }

    fn write_bbus(&mut self, dest: u8, value: u8) {
        self.writes.push((dest, value));
    }
}

fn fresh() -> Machine {
    fresh_rom(&[])
}

/// A machine whose ROM has `patches` written into it, for the transfers that need a cartridge
/// source rather than WRAM.
fn fresh_rom(patches: &[(usize, &[u8])]) -> Machine {
    let mut rom = common::synth_lorom(0x8000, &[0xEA]);
    for (offset, bytes) in patches {
        rom[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    let cart = Cartridge::new(rom).expect("the synthesized cartridge loads");
    Machine::new(cart)
}

/// One channel set up for a general-purpose transfer.
fn channel(dma: &mut Dma, mode: u8, bbad: u8, source: Addr, count: u16) {
    let c = &mut dma.channels[0];
    c.dmap = mode;
    c.bbad = bbad;
    c.a1t = source;
    c.das = count;
}

#[test]
fn modes_zero_to_seven_write_the_b_bus_in_order() {
    // The B-bus table, verbatim: the B-bus offsets each unit writes, in order. One unit each,
    // so the write list is the table.
    let expected: [&[(u8, u8)]; 8] = [
        &[(0x18, 0x11)],
        &[(0x18, 0x11), (0x19, 0x22)],
        &[(0x18, 0x11), (0x18, 0x22)],
        &[(0x18, 0x11), (0x18, 0x22), (0x19, 0x33), (0x19, 0x44)],
        &[(0x18, 0x11), (0x19, 0x22), (0x1A, 0x33), (0x1B, 0x44)],
        &[(0x18, 0x11), (0x19, 0x22), (0x18, 0x33), (0x19, 0x44)],
        &[(0x18, 0x11), (0x18, 0x22)],
        &[(0x18, 0x11), (0x18, 0x22), (0x19, 0x33), (0x19, 0x44)],
    ];
    for mode in 0u8..8 {
        let mut dma = Dma::new();
        let size = Dma::unit_size(mode) as u16;
        channel(&mut dma, mode, 0x18, Addr::new(0, 0x1000), size);
        let mut mock = Mock::new(&[0x11, 0x22, 0x33, 0x44]);
        dma.mdmaen = 0x01;
        dma.run_gpdma(0x01, &mut mock);
        assert_eq!(
            mock.writes,
            expected[usize::from(mode)],
            "mode {mode} writes the B-bus as the table says"
        );
        assert_eq!(
            dma.mdmaen, 0,
            "mode {mode}: MDMAEN bit cleared on completion"
        );
    }
}

#[test]
fn a_bus_step_increment_decrement_fixed() {
    // fullsnes `DMAPn` bits 4-3: "(0=Increment, 2=Decrement, 1/3=Fixed)".
    let cases: [(u8, [u16; 4], &str); 3] = [
        (0x00, [0x1000, 0x1001, 0x1002, 0x1003], "0 = increment"),
        (0x10, [0x1000, 0x0FFF, 0x0FFE, 0x0FFD], "2 = decrement"),
        (0x08, [0x1000, 0x1000, 0x1000, 0x1000], "1 = fixed"),
    ];
    for (dmap, addresses, what) in cases {
        let mut dma = Dma::new();
        channel(&mut dma, 0, 0x18, Addr::new(0, 0x1000), 4);
        dma.channels[0].dmap = dmap;
        let mut mock = Mock::new(&[1, 2, 3, 4]);
        dma.run_gpdma(0x01, &mut mock);
        let read: Vec<u16> = mock.reads.iter().map(|a| a.offset()).collect();
        assert_eq!(read, addresses, "A-bus step {what}: {read:X?}");
    }
    // Step 3 is also fixed.
    let mut dma = Dma::new();
    channel(&mut dma, 0, 0x18, Addr::new(0, 0x1000), 2);
    dma.channels[0].dmap = 0x18;
    let mut mock = Mock::new(&[1, 2, 3, 4]);
    dma.run_gpdma(0x01, &mut mock);
    assert_eq!(
        mock.reads.iter().map(|a| a.offset()).collect::<Vec<_>>(),
        vec![0x1000, 0x1000],
        "3 = fixed"
    );
}

#[test]
fn mdmaen_runs_channels_in_order() {
    // fullsnes: "DMA and HDMA Transfer order is Channel 0 first... Channel 7 last."
    let mut dma = Dma::new();
    for (index, bbad) in [(0usize, 0x18u8), (2, 0x22), (7, 0x30)] {
        let c = &mut dma.channels[index];
        c.dmap = 0;
        c.bbad = bbad;
        c.a1t = Addr::new(0, 0x1000 + index as u16 * 0x10);
        c.das = 2;
    }
    let mut mock = Mock::new(&[0xAA; 0x100]);
    dma.mdmaen = 0x85;
    dma.run_gpdma(0x85, &mut mock);
    let order: Vec<u8> = mock.writes.iter().map(|&(dest, _)| dest).collect();
    assert_eq!(
        order,
        vec![0x18, 0x18, 0x22, 0x22, 0x30, 0x30],
        "channels ran 0, 2, 7 in order, each to completion"
    );
    assert_eq!(dma.mdmaen, 0, "every bit that ran was cleared");
}

#[test]
fn zero_byte_counter_is_64k() {
    // fullsnes "SNES DMA and HDMA Notes": a `DAS` of $0000 means 65536 bytes.
    let mut dma = Dma::new();
    channel(&mut dma, 0, 0x18, Addr::new(0, 0x0000), 0);
    let mut mock = Mock::new(&[0x5A; 0x100]);
    dma.run_gpdma(0x01, &mut mock);
    assert_eq!(
        mock.writes.len(),
        65_536,
        "a zero byte counter transfers $10000 bytes"
    );
}

#[test]
fn partial_unit_transfers_the_remainder() {
    // fullsnes's own example: "len=5 would transfer one whole Unit, plus the first byte of the
    // second Unit". Mode 4's unit is four bytes.
    let mut dma = Dma::new();
    channel(&mut dma, 4, 0x18, Addr::new(0, 0x1000), 5);
    let mut mock = Mock::new(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    dma.run_gpdma(0x01, &mut mock);
    let dests: Vec<u8> = mock.writes.iter().map(|&(dest, _)| dest).collect();
    let values: Vec<u8> = mock.writes.iter().map(|&(_, value)| value).collect();
    assert_eq!(
        dests,
        vec![0x18, 0x19, 0x1A, 0x1B, 0x18],
        "one unit plus one byte"
    );
    assert_eq!(values, vec![0x11, 0x22, 0x33, 0x44, 0x55]);
    assert_eq!(dma.channels[0].das, 0, "the byte counter reached zero");
    assert_eq!(
        dma.channels[0].a1t.offset(),
        0x1005,
        "the A-bus address advanced five bytes"
    );
}

#[test]
fn eight_clocks_per_byte() {
    // fullsnes "SNES DMA and HDMA Notes": "fixed 8 master cycles per byte for DMA". The `$420B`
    // write itself is an ordinary 6-clock I/O access. The source is the cartridge, because a
    // WRAM-to-WRAM transfer is the one the chip refuses.
    let mut machine = fresh_rom(&[(
        0x1000,
        &[
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D,
            0x4E, 0x4F,
        ],
    )]);
    let bus = &mut machine.bus;
    bus.io.wmadd = 0;
    let c = &mut bus.dma.channels[0];
    c.dmap = 0;
    c.bbad = 0x80; // the WRAM port, so the transfer is observable
    c.a1t = Addr::new(0x00, 0x9000);
    c.das = 16;
    let before = bus.master;
    bus.write(Addr::new(0, 0x420B), 0x01);
    assert_eq!(
        bus.master - before,
        6 + 16 * 8,
        "the write's own 6 clocks plus 8 master clocks for each of 16 bytes"
    );
    for i in 0..16u16 {
        assert_eq!(
            bus.wram[usize::from(i)],
            (0x40 + i) as u8,
            "byte {i} landed through the WRAM port"
        );
    }
    assert_eq!(
        bus.io.wmadd, 16,
        "the WRAM port's counter advanced with the transfer"
    );
}

#[test]
fn wram_to_wram_transfer_does_nothing() {
    // fullsnes "SNES DMA and HDMA Notes": the chip "is unable to process both at once", so a
    // WRAM-to-WRAM transfer does nothing at all.
    let mut machine = fresh();
    let bus = &mut machine.bus;
    bus.io.wmadd = 0x100;
    let c = &mut bus.dma.channels[0];
    c.dmap = 0;
    c.bbad = 0x80;
    c.a1t = Addr::new(0x7E, 0x2000);
    c.das = 8;
    let before = bus.master;
    bus.write(Addr::new(0, 0x420B), 0x01);
    assert_eq!(
        bus.master - before,
        6,
        "no byte was moved, so no byte was charged"
    );
    assert_eq!(bus.io.wmadd, 0x100, "the WRAM port's counter did not move");
    assert_eq!(bus.wram[0x100], 0, "nothing was written");
}

/// A direct HDMA table at `$00:1000` moving one byte a line to `$2118`.
fn hdma_channel(dma: &mut Dma) {
    let c = &mut dma.channels[0];
    c.dmap = 0x00; // mode 0, direct, incrementing
    c.bbad = 0x18;
    c.a1t = Addr::new(0, 0x1000);
    c.das = 0;
    c.dasb = 0;
    dma.hdmaen = 0x01;
    dma.begin_frame();
}

#[test]
fn hdma_moves_one_unit_per_line() {
    // fullsnes "HDMA Table Formats": `01h..80h` "transfer 1 unit on 1 line, then pause X-01h lines".
    let mut dma = Dma::new();
    hdma_channel(&mut dma);
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x01, 0xAA, 0x01, 0xBB, 0x00]);
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes, vec![(0x18, 0xAA)], "line 1 moved one unit");
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0xAA), (0x18, 0xBB)],
        "line 2 fetched the next entry and moved it"
    );
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 2, "the 00h entry ended the channel");
    assert!(
        dma.channels[0].line_end,
        "the channel is terminated for the frame"
    );

    // With a longer count the entry repeats: `03h` transfers on one line and pauses two.
    let mut dma = Dma::new();
    hdma_channel(&mut dma);
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x03, 0xAA, 0x00]);
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 1, "line 1 of the entry");
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 1, "line 2 is a pause");
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 1, "line 3 is the second pause");
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 1, "the 00h entry has now been fetched");
}

#[test]
fn hdma_repeat_mode_moves_several_units() {
    // `81h..FFh`: "transfer X-80h units on X-80h lines (repeat mode)".
    let mut dma = Dma::new();
    hdma_channel(&mut dma);
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x82, 0xAA, 0xBB, 0x00]);
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0xAA)],
        "the first of the two units"
    );
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0xAA), (0x18, 0xBB)],
        "the second unit, from the same entry"
    );
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 2, "the repeat count is used up");
    assert!(
        dma.channels[0].line_end,
        "and the 00h entry ended the channel"
    );
}

#[test]
fn hdma_table_ends_and_reloads_next_frame() {
    // V=0 reloads the table pointer (fullsnes: "H=6, V=0 reload HDMA registers"), which is the one
    // thing a cartridge can observe about what the counters do in v-blank (§6.5's fourth row).
    let mut dma = Dma::new();
    hdma_channel(&mut dma);
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x01, 0xAA, 0x00]);
    dma.hdma_line(&mut mock);
    dma.hdma_line(&mut mock);
    assert!(dma.channels[0].line_end);
    dma.hdma_line(&mut mock);
    assert_eq!(mock.writes.len(), 1, "nothing moves on a later line");
    dma.begin_frame();
    assert!(
        !dma.channels[0].line_end,
        "the frame reload cleared the end"
    );
    assert_eq!(
        dma.channels[0].a2a, 0x1000,
        "the table pointer reloaded from A1T"
    );
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0xAA), (0x18, 0xAA)],
        "the next frame starts at the table's first entry again"
    );
}

#[test]
fn hdma_indirect_reads_the_pointer() {
    // `DMAPn` bit 6: the table holds a 2-byte pointer into `DASB`'s bank.
    let mut dma = Dma::new();
    let c = &mut dma.channels[0];
    c.dmap = 0x40; // mode 0, indirect
    c.bbad = 0x18;
    c.a1t = Addr::new(0, 0x1000);
    c.dasb = 0x02;
    dma.hdmaen = 0x01;
    dma.begin_frame();
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x01, 0x00, 0x30]); // one line, pointer $3000
    mock.poke(0x3000, &[0x5A]);
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0x5A)],
        "the data came from the pointer"
    );
    assert!(
        mock.reads.contains(&Addr::new(0x02, 0x3000)),
        "the pointer was read in DASB's bank $02: {:?}",
        mock.reads
    );
    assert_eq!(
        dma.channels[0].a2a, 0x1003,
        "the table advanced past the entry and pointer"
    );
}

#[test]
fn hdma_indirect_in_wram_uses_the_table_bank() {
    // The quirk: a table in `$7E`/`$7F` takes the data bank from the table and ignores
    // `DASB`, so a WRAM table needs no second bank byte.
    let mut machine = fresh();
    let bus = &mut machine.bus;
    // Table in bank $7E pointing at $2000; the data differs between $7E and $00, so the bank shows.
    bus.poke(Addr::new(0x7E, 0x1000), 0x01);
    bus.poke(Addr::new(0x7E, 0x1001), 0x00);
    bus.poke(Addr::new(0x7E, 0x1002), 0x20);
    bus.poke(Addr::new(0x7E, 0x2000), 0x5A);
    bus.poke(Addr::new(0x00, 0x2000), 0x11);
    bus.io.wmadd = 0;
    let c = &mut bus.dma.channels[0];
    c.dmap = 0x40; // mode 0, indirect
    c.bbad = 0x80; // the WRAM port
    c.a1t = Addr::new(0x7E, 0x1000);
    c.dasb = 0x00; // deliberately wrong: the table's bank must win
    bus.dma.hdmaen = 0x01;
    bus.dma.begin_frame();
    bus.ppu.line = 100;
    bus.on_line_events(LineEvents(LineEvents::HDMA));
    assert_eq!(
        bus.wram[0], 0x5A,
        "the indirect data came from bank $7E, not from DASB's bank $00"
    );
}

#[test]
fn hdma_started_mid_frame_moves_one_unit_per_line() {
    // §6.5's second row: v1 pins nocash's "Case 1" — an HDMA channel enabled mid-frame behaves like
    // one enabled at V=0 from the next line on.
    let mut dma = Dma::new();
    let c = &mut dma.channels[0];
    c.dmap = 0x00;
    c.bbad = 0x18;
    c.a1t = Addr::new(0, 0x1000);
    c.a2a = 0x1000; // the channel was never reloaded: it is switched on mid-frame
    c.dasb = 0;
    let mut mock = Mock::new(&[]);
    mock.poke(0x1000, &[0x01, 0xAA, 0x01, 0xBB]);
    dma.hdma_line(&mut mock);
    assert!(mock.writes.is_empty(), "the disabled channel did nothing");
    dma.hdmaen = 0x01; // enabled at H=278 of some line
    dma.hdma_line(&mut mock);
    dma.hdma_line(&mut mock);
    assert_eq!(
        mock.writes,
        vec![(0x18, 0xAA), (0x18, 0xBB)],
        "from the next line on it moves one unit a line"
    );
}

#[test]
fn hdma_runs_during_forced_blank() {
    // fullsnes "SNES DMA and HDMA Start/Enable Registers": "HDMA is running even during Forced
    // Blank." The event is raised the ordinary way; forced blank must not gate it.
    let mut machine = fresh();
    let bus = &mut machine.bus;
    bus.poke(Addr::new(0x7E, 0x1000), 0x01);
    bus.poke(Addr::new(0x7E, 0x1001), 0x5A);
    bus.io.wmadd = 0;
    bus.ppu.regs.inidisp = 0x80; // forced blank
    let c = &mut bus.dma.channels[0];
    c.dmap = 0x00;
    c.bbad = 0x80;
    c.a1t = Addr::new(0x7E, 0x1000);
    bus.dma.hdmaen = 0x01;
    bus.dma.begin_frame();
    bus.ppu.line = 100;
    bus.on_line_events(LineEvents(LineEvents::HDMA));
    assert_eq!(
        bus.wram[0], 0x5A,
        "the HDMA unit still moved during forced blank"
    );

    // The window is lines 0-224: line 225's event moves nothing (§3.3's gate).
    bus.io.wmadd = 0;
    bus.wram[0] = 0;
    let mut dma = std::mem::take(&mut bus.dma);
    dma.begin_frame();
    bus.dma = dma;
    bus.ppu.line = 240;
    bus.on_line_events(LineEvents(LineEvents::HDMA));
    assert_eq!(bus.wram[0], 0, "no HDMA outside lines 0-224");

    // And it is a real `Bus` write of 8 clocks: 4224 clocks is 528 HDMA bytes' worth, so a
    // single unit is 8 clocks (fullsnes: "fixed 8 master cycles per byte for DMA").
    let before = bus.master;
    bus.ppu.line = 50;
    bus.on_line_events(LineEvents(LineEvents::HDMA));
    assert_eq!(bus.master - before, 8, "an HDMA byte costs 8 master clocks");
}

/// `$43x0-$43xB` is not write-only: the eight channels' twelve registers each are read/write
/// memory, which is a documented 3.5 MHz scratch pad rather than a quirk of this machine.
///
/// fullsnes "SNES DMA and HDMA Notes", the timing list's 3.5 MHz tricks: "DMA registers at
/// 43x0h-43xBh provide 8x12 bytes of read/write-able \"memory\"", and every register in "SNES DMA
/// and HDMA Channel 0..7 Registers" is marked `(R/W)`. A cartridge leans on it: Madden NFL '94's
/// boot decompressor sets `D = $4300` and runs a six-byte program it assembles in channel 2's
/// registers (`$4320` = `MVN $7F,xx`, `$4323` = `JMP $F310`), so a machine that answered open bus
/// there would take the S-CPU somewhere the cartridge never wrote. This test holds the machine to
/// the document, not to any cartridge.
#[test]
fn dma_registers_are_read_write_memory() {
    let mut m = fresh();
    let values: [(u16, u8); 10] = [
        (0x0, 0x25), // DMAP: direction, indirect, bit 5 (R/W per fullsnes), A-bus step, unit
        (0x1, 0x18), // BBAD
        (0x2, 0x34), // A1T low
        (0x3, 0x12), // A1T high
        (0x4, 0x7E), // A1B
        (0x5, 0x78), // DAS low
        (0x6, 0x56), // DAS high
        (0x7, 0x00), // DASB
        (0xA, 0x5A), // $43xA, storage
        (0xB, 0xC3), // $43xB, and its $43xF mirror
    ];
    for channel in 0u8..8 {
        let base = 0x4300 + u16::from(channel) * 0x10;
        for (offset, value) in values {
            m.bus.write(Addr::new(0, base + offset), value);
        }
        // Park a known value on the data bus before reading back: a write-only register would
        // answer this byte instead of its own contents, which is what makes the assertion below a
        // test of storage rather than of the last write.
        let bus_byte = m.bus.read(Addr::new(0, 0x0000));
        assert_eq!(bus_byte, 0, "fresh WRAM answers $00");
        for (offset, value) in values {
            let at = Addr::new(0, base + offset);
            assert_eq!(
                m.bus.read(at),
                value,
                "fullsnes: the DMA registers at 43x0h-43xBh are read/write-able memory, so \
                 channel {channel}'s ${:02X} must read back ${value:02X} and not the data bus's \
                 ${bus_byte:02X}",
                base + offset
            );
        }
        // $43xB's mirror: the same byte answers at $43xF.
        let mirror = Addr::new(0, base + 0xF);
        m.bus.read(Addr::new(0, 0x0000));
        assert_eq!(
            m.bus.read(mirror),
            0xC3,
            "$43xF is the same storage as $43xB (fullsnes: the `(R/W)` row for 43xBh)"
        );
    }
}

/// The DMA's A-bus open bus is **not** the S-CPU's MDR.
///
/// fullsnes "Open Bus for DMA": "For DMA reads, one may expect the same garbage as for CPU reads
/// (ie. when starting a DMA via 'MOV [420Bh],A', one would expect 42h as open bus value). However,
/// it takes a few cycles before the DMA transfer does actually start, during that time the
/// hardware 'forgets' the 42h value, and instread, DMA does always read FFh (High-Z) as open bus
/// value." The one documented exception is the wrap: "If DMA wraps from an used to unused address
/// (eg. from 1FFFh/WRAM to 2000h/unused) then the first 'unused' byte will be same as the last
/// 'used' byte, thereafter, it forgets that value (and returns FFh on further unused addresses)."
///
/// A cartridge that reads open bus with the CPU still gets the MDR — that rule is unchanged and
/// `tests/readlog_probe.rs` pins it — but a transfer that wraps out of work RAM must carry high-Z,
/// not whatever byte the `STA $420B` left on the bus.
#[test]
fn a_dma_reads_high_z_from_open_bus_not_the_cpu_mdr() {
    let mut m = fresh();
    // $00:1FFE/$1FFF are the work-RAM mirror; $00:2000-$00:2005 are the unused block above it.
    m.bus.poke(Addr::new(0x00, 0x1FFE), 0x11);
    m.bus.poke(Addr::new(0x00, 0x1FFF), 0x22);
    // The byte the `STA $420B` that starts this transfer leaves on the bus. An emulator that lends
    // the DMA the CPU's latch writes $5A to every open-bus byte.
    m.bus.mdr = 0x5A;
    let mut dma = Dma::new();
    // Mode 0: one byte a unit, A-bus incrementing, B-bus $2180 (the work-RAM data port).
    channel(&mut dma, 0, 0x80, Addr::new(0x00, 0x1FFE), 8);
    dma.mdmaen = 0x01;
    dma.run_gpdma(0x01, &mut m.bus);
    let got: Vec<u8> = (0..8).map(|i| m.bus.wram[i]).collect();
    assert_eq!(
        got,
        [0x11, 0x22, 0x22, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        "fullsnes \"Open Bus for DMA\": the transfer reads $11,$22 from the work-RAM mirror, the \
         first unused byte ($00:2000) is the last used byte ($22), and every further unused byte is \
         high-Z ($FF) — never the CPU's $5A"
    );
}

/// The register blocks a DMA cannot read from the A-bus (fullsnes "Open Bus for DMA").
///
/// "2100h-21FFh Open Bus (when used as A-Bus) (of course they work as B-Bus)", "4000h-41FFh Open
/// Bus (name 4016h/4017h cannot be read)", and "4210h-421Fh These do work (the only I/O ports that
/// are not open bus)".
#[test]
fn a_dma_reads_the_register_blocks_as_open_bus_except_4210_to_421f() {
    // $2100-$21FF: the B-bus registers are open bus on the A-bus.
    let mut m = fresh();
    m.bus.mdr = 0x5A;
    let mut dma = Dma::new();
    channel(&mut dma, 0, 0x80, Addr::new(0x00, 0x2100), 2);
    dma.mdmaen = 0x01;
    dma.run_gpdma(0x01, &mut m.bus);
    assert_eq!(
        &m.bus.wram[0..2],
        &[0xFF, 0xFF],
        "fullsnes: $2100-$21FF is open bus when used as the A-bus, and with no driven byte before \
         it the open bus is high-Z, not the CPU's $5A"
    );

    // $4000-$41FF: the joypad registers are open bus on the A-bus too.
    let mut m = fresh();
    let mut dma = Dma::new();
    channel(&mut dma, 0, 0x80, Addr::new(0x00, 0x4016), 1);
    dma.mdmaen = 0x01;
    dma.run_gpdma(0x01, &mut m.bus);
    assert_eq!(
        m.bus.wram[0], 0xFF,
        "fullsnes: $4000-$41FF is open bus on the DMA's A-bus (the manual joypad registers cannot \
         be read by the controller); the CPU read of $4016 is unchanged"
    );

    // $4210-$421F: the one register block that does work on the A-bus.
    let mut m = fresh();
    let mut dma = Dma::new();
    channel(&mut dma, 0, 0x80, Addr::new(0x00, 0x4212), 1);
    dma.mdmaen = 0x01;
    dma.run_gpdma(0x01, &mut m.bus);
    assert_eq!(
        m.bus.wram[0], 0x00,
        "fullsnes: $4210-$421F are the only I/O ports a DMA can read; $4212 at reset is HVBJOY = \
         $00, not the open bus's $FF"
    );
}
