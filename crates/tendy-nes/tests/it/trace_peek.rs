//! Tracer/peek rules: what the debug tools *see* must be what the
//! CPU sees, and looking must not change the machine.
//!
//! * a Game Genie code swaps a byte on the CPU's read path; `peek` and the tracer's
//!   disassembly read the cartridge directly and so printed the unpatched instruction.
//! * MMC5 acknowledges its scanline IRQ on a read of `$FFFA`/`$FFFB`; the tracer peeks
//!   PC..PC+2 for every line, so tracing code near the vectors changed the emulation.
//! * with `--trace-from`, a warmup line still paid the whole disassembly cost (and its
//!   cartridge reads) before being thrown away.

use std::cell::Cell;
use std::rc::Rc;

use tendy_nes::cart::{CartHooks, Header, Mapper, Mirroring, RomData};
use tendy_nes::cpu::Bus;
use tendy_nes::genie::Code;
use tendy_nes::mapper::mmc5::Mmc5;
use tendy_nes::mapper::n163::Namco163;
use tendy_nes::nes::Nes;
use tendy_nes::trace::{Tracer, disasm};

/// A 16 KB NROM image; `prg` holds (PRG offset, byte) patches, and the vectors land on the
/// `RTI` at $8100 so nothing runs away if a test steps.
fn nrom_with(prg: &[(usize, u8)]) -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    v.resize(16 + 16_384 + 8_192, 0x00);
    v[16 + 0x100] = 0x40; // RTI at $8100 = the NMI/IRQ handler
    for (index, byte) in [
        (0x3FFAu64, 0x00u8),
        (0x3FFBu64, 0x81u8),
        (0x3FFCu64, 0x00u8),
        (0x3FFDu64, 0x80u8),
        (0x3FFEu64, 0x00u8),
        (0x3FFFu64, 0x81u8),
    ] {
        v[16 + index as usize] = byte;
    }
    for &(index, byte) in prg {
        v[16 + index] = byte;
    }
    v
}

/// An unbounded stream at $C000: `NOP` then `JMP $C001`.
fn jump_loop_rom() -> Vec<u8> {
    nrom_with(&[(0x000, 0xEA), (0x001, 0x4C), (0x002, 0x01), (0x003, 0xC0)])
}

/// The code substitutes the operand byte the CPU reads, so the debugger's peek and the
/// trace's disassembly must show the substituted byte, not the cartridge's own.
#[test]
fn genie_bytes_show_in_peek_and_disassembly() {
    let rom = nrom_with(&[(0x000, 0xA9), (0x001, 0x42)]); // $8000: LDA #$42
    let mut nes = Nes::new(&rom).unwrap();
    assert_eq!(
        disasm(&mut nes, 0x8000),
        "A9 42  LDA #$42",
        "ROM byte before the code"
    );

    nes.bus.set_genie_codes(vec![Code {
        addr: 0x8001,
        value: 0x99,
        compare: None,
    }]);

    assert_eq!(
        nes.bus.read(0x8001),
        0x99,
        "the CPU's read applies the code"
    );
    assert_eq!(
        nes.bus.peek(0x8001),
        Some(0x99),
        "peek must show the byte the CPU reads"
    );
    assert_eq!(
        disasm(&mut nes, 0x8000),
        "A9 99  LDA #$99",
        "the trace must disassemble the instruction the CPU runs"
    );
}

/// A SysBus whose $8000+ window is a real MMC5, with no PPU/CPU state to run past it.
fn mmc5_bus() -> tendy_nes::bus::SysBus {
    let rom = RomData {
        header: Header {
            prg_banks: 8,
            chr_banks: 8,
            mapper: 5,
            mirroring: Mirroring::Vertical,
            ..Default::default()
        },
        prg: vec![0; 8 * 0x4000],
        chr: vec![0; 8 * 0x2000],
        chr_is_ram: false,
        trainer: None,
    };
    tendy_nes::bus::SysBus::new(Box::new(Mmc5::new(rom)))
}

/// `$5204`'s in-frame flag (bit 6), read without acknowledging the IRQ.
fn in_frame(bus: &mut tendy_nes::bus::SysBus) -> bool {
    bus.cart_mut().ext_read(0x5204).unwrap() & 0x40 != 0
}

/// Enter a frame the way the PPU does: three identical nametable fetches, the attribute read
/// that detects the line start, then the next nametable fetch that opens the frame.
fn enter_frame(bus: &mut tendy_nes::bus::SysBus) {
    for _ in 0..3 {
        bus.cart_mut().nt_read(0x2000, &[]);
    }
    bus.cart_mut().nt_read(0x23C0, &[]);
    bus.cart_mut().nt_read(0x2000, &[]);
}

/// An MMC5 read of the NMI vector leaves the frame (it acknowledges the scanline IRQ).
/// That is the CPU's behaviour and stays; `peek` promises not to touch the machine, so it must
/// return the byte without the side effect.
#[test]
fn peek_does_not_leave_the_mmc5_frame() {
    let mut bus = mmc5_bus();
    enter_frame(&mut bus);
    assert!(
        in_frame(&mut bus),
        "precondition: the detector opened a frame"
    );

    assert_eq!(
        bus.peek(0xFFFA),
        Some(0),
        "the vector byte comes back unmodified"
    );
    assert!(
        in_frame(&mut bus),
        "peek left the frame: it took the MMC5's read side effect"
    );

    // The real read still leaves the frame: the fix is in the peek path only.
    bus.cart_mut().cpu_read(0xFFFA);
    assert!(
        !in_frame(&mut bus),
        "a CPU read of $FFFA must keep leaving the frame"
    );
}

/// Counts every cartridge read, to see what a trace line costs the cartridge.
struct ReadCounter {
    inner: Box<dyn Mapper>,
    reads: Rc<Cell<u64>>,
}

/// Every other `Mapper` method is forwarded: the wrapper must answer exactly as the board
/// does, or the test measures the trait default instead of the cartridge.
impl Mapper for ReadCounter {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        self.reads.set(self.reads.get() + 1);
        self.inner.cpu_read(addr)
    }
    fn cpu_write(&mut self, addr: u16, val: u8) {
        self.inner.cpu_write(addr, val)
    }
    fn cpu_peek(&mut self, addr: u16) -> u8 {
        self.inner.cpu_peek(addr)
    }
    fn ppu_read(&mut self, addr: u16) -> u8 {
        self.inner.ppu_read(addr)
    }
    fn ppu_read_sprite(&mut self, addr: u16) -> u8 {
        self.inner.ppu_read_sprite(addr)
    }
    fn ppu_register_write(&mut self, addr: u16, val: u8) {
        self.inner.ppu_register_write(addr, val);
    }
    fn chr_peek(&self, addr: u16) -> u8 {
        self.inner.chr_peek(addr)
    }
    fn ppu_write(&mut self, addr: u16, val: u8) {
        self.inner.ppu_write(addr, val)
    }
    fn mirroring(&self) -> Mirroring {
        self.inner.mirroring()
    }
    fn nt_read(&mut self, addr: u16, ciram: &[u8]) -> Option<u8> {
        self.inner.nt_read(addr, ciram)
    }
    fn nt_write(&mut self, addr: u16, val: u8, ciram: &mut [u8]) -> bool {
        self.inner.nt_write(addr, val, ciram)
    }
    fn nt_peek(&self, addr: u16, ciram: &[u8]) -> Option<u8> {
        self.inner.nt_peek(addr, ciram)
    }
    fn prg_ram(&self) -> &[u8] {
        self.inner.prg_ram()
    }
    fn load_prg_ram(&mut self, data: &[u8]) {
        self.inner.load_prg_ram(data)
    }
    fn prg_rom_offset(&self, addr: u16) -> Option<usize> {
        self.inner.prg_rom_offset(addr)
    }
    fn chr_rom_offset(&self, addr: u16) -> Option<usize> {
        self.inner.chr_rom_offset(addr)
    }
    fn chr_ram(&self) -> &[u8] {
        self.inner.chr_ram()
    }
    fn load_chr_ram(&mut self, data: &[u8]) {
        self.inner.load_chr_ram(data)
    }
    fn battery_data(&self) -> Vec<u8> {
        self.inner.battery_data()
    }
    fn load_battery_data(&mut self, data: &[u8]) {
        self.inner.load_battery_data(data)
    }
    fn battery_change_key(&self) -> Vec<u8> {
        self.inner.battery_change_key()
    }
    fn power_on_ram(&mut self, mode: tendy_nes::bus::RamPowerOn, seed: u64) {
        self.inner.power_on_ram(mode, seed)
    }
    fn power_on_random(&mut self, seed: u64) {
        self.inner.power_on_random(seed)
    }
    fn write_4016(&mut self, val: u8) {
        self.inner.write_4016(val)
    }
    fn ext_read(&mut self, addr: u16) -> Option<u8> {
        self.inner.ext_read(addr)
    }
    fn ext_write(&mut self, addr: u16, val: u8) -> bool {
        self.inner.ext_write(addr, val)
    }
    fn next_disk_side(&mut self) -> Option<(usize, usize)> {
        self.inner.next_disk_side()
    }
    fn disk_side(&self) -> Option<(usize, usize)> {
        self.inner.disk_side()
    }
    fn save_state(&self, w: &mut tendy_nes::state::StateWriter) {
        self.inner.save_state(w)
    }
    fn load_state(
        &mut self,
        r: &mut tendy_nes::state::StateReader,
    ) -> Result<(), tendy_nes::state::StateError> {
        self.inner.load_state(r)
    }
    fn irq_pending(&self) -> bool {
        self.inner.irq_pending()
    }
    fn hooks(&self) -> CartHooks {
        self.inner.hooks()
    }
    fn cpu_clock(&mut self) {
        self.inner.cpu_clock()
    }
    fn expansion_audio(&self) -> f32 {
        self.inner.expansion_audio()
    }
    fn expansion_audio_name(&self) -> Option<&'static str> {
        self.inner.expansion_audio_name()
    }
    fn ppu_bus(&mut self, addr: u16) {
        self.inner.ppu_bus(addr)
    }
}

/// Installs the counter on `nes`'s cartridge, returning the shared read count. The wrapper is built
/// around a fresh board for the same ROM and installed through `set_cart` — the one way in, now that
/// the `cart` field is private, because a swap must re-read the cached hook flags.
fn count_reads(nes: &mut Nes, rom: &[u8]) -> Rc<Cell<u64>> {
    let reads = Rc::new(Cell::new(0u64));
    let inner = tendy_nes::mapper::load(rom).unwrap();
    nes.bus.set_cart(Box::new(ReadCounter {
        inner,
        reads: Rc::clone(&reads),
    }));
    reads
}

/// A line the `--trace-from` window throws away must not be built at all, so it must
/// not read the cartridge. One traced warmup instruction therefore costs exactly the CPU's own
/// fetches — the same count an unlogged step makes.
#[test]
fn a_discarded_warmup_line_reads_no_cartridge_bytes() {
    let rom = jump_loop_rom();
    let mut logged = Nes::new(&rom).unwrap();
    let mut plain = Nes::new(&rom).unwrap();
    let logged_reads = count_reads(&mut logged, &rom);
    let plain_reads = count_reads(&mut plain, &rom);

    let mut tracer = Tracer::new(Vec::new()).from_frame(1); // frame 0 is warmup
    tracer.log_file_line(&mut logged).unwrap();
    assert_eq!(tracer.lines, 0, "the warmup line must not be logged");
    plain.step();

    assert_eq!(
        logged.cpu.pc, plain.cpu.pc,
        "the warmup instruction ran exactly once"
    );
    assert_eq!(
        logged_reads.get(),
        plain_reads.get(),
        "a discarded line read the cartridge {} extra time(s)",
        logged_reads.get() as i64 - plain_reads.get() as i64
    );
}

/// `ReadCounter` must forward the nametable hooks. An N163 with a CHR-ROM page in the
/// nametable window cannot be mistaken for CIRAM, so the default path is a real wrong answer.
#[test]
fn the_wrapper_forwards_an_n163_chr_rom_nametable() {
    let rom = RomData {
        header: Header {
            prg_banks: 16,
            chr_banks: 16,
            mapper: 19,
            mirroring: Mirroring::Vertical,
            ..Default::default()
        },
        prg: vec![0; 0x40000],
        chr: (0..0x20000).map(|i| (i / 0x400) as u8).collect(), // each 1 KB bank holds its own number
        chr_is_ram: false,
        trainer: None,
    };
    let mut m = ReadCounter {
        inner: Box::new(Namco163::new(rom)),
        reads: Rc::new(Cell::new(0)),
    };
    m.cpu_write(0xC000, 0x11); // $2000 -> CHR bank $11
    let ciram = [0u8; 0x1000];
    assert_eq!(
        m.nt_read(0x2000, &ciram),
        Some(0x11),
        "CHR page, not ciram[0]"
    );
    assert_eq!(m.nt_peek(0x2000, &ciram), Some(0x11));
}
