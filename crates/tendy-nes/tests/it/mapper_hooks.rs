use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tendy_nes::cart::{CartHooks, Header, Mapper, Mirroring, RomData};
use tendy_nes::cpu::Bus;
use tendy_nes::mapper::mmc5::Mmc5;
use tendy_nes::nes::Nes;

use crate::common::rom_file;

/// Wraps the cartridge's real mapper, counts the two hooks the bus and PPU call on it, and reports
/// `hooks` so a test can make the bus skip them or call them every cycle.
struct HookCounter {
    inner: Box<dyn Mapper>,
    cpu_clocks: Rc<Cell<u64>>,
    ppu_bus: Rc<RefCell<Vec<u16>>>,
    hooks: CartHooks,
}

/// The console's board behind a counting wrapper. Every `Mapper` method is forwarded: one
/// left out would fall back to the trait default and quietly answer for the board instead of asking
/// it, a bug that has shipped before.
impl Mapper for HookCounter {
    fn cpu_read(&mut self, addr: u16) -> u8 {
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
        self.hooks
    }
    fn cpu_clock(&mut self) {
        self.cpu_clocks.set(self.cpu_clocks.get() + 1);
        self.inner.cpu_clock();
    }
    fn expansion_audio(&self) -> f32 {
        self.inner.expansion_audio()
    }
    fn expansion_audio_name(&self) -> Option<&'static str> {
        self.inner.expansion_audio_name()
    }
    fn ppu_bus(&mut self, addr: u16) {
        self.ppu_bus.borrow_mut().push(addr);
        self.inner.ppu_bus(addr);
    }
}

/// The hook recorders: a CPU-cycle counter and the PPU bus address trace.
type ClockRecorder = Rc<Cell<u64>>;
type BusRecorder = Rc<RefCell<Vec<u16>>>;

/// Nestest with its mapper wrapped: returns the console and the two hook recorders. `hooks` is what
/// the wrapper reports, so a test can have the bus call the hooks every cycle (both set) or skip
/// them (the default).
fn hooked_nestest(hooks: CartHooks) -> (Nes, ClockRecorder, BusRecorder) {
    let rom = rom_file("nestest.nes");
    let mut nes = Nes::new(&rom).unwrap();
    let cpu_clocks = Rc::new(Cell::new(0));
    let ppu_bus = Rc::new(RefCell::new(Vec::new()));
    let counter = HookCounter {
        inner: tendy_nes::mapper::load(&rom).unwrap(),
        cpu_clocks: Rc::clone(&cpu_clocks),
        ppu_bus: Rc::clone(&ppu_bus),
        hooks,
    };
    nes.bus.set_cart(Box::new(counter));
    (nes, cpu_clocks, ppu_bus)
}

/// The hooks the wrapper reports when a test wants every call made.
const ALL_HOOKS: CartHooks = CartHooks {
    irq: true,
    cpu_clock: true,
};

/// The no-op default must be called exactly once per CPU cycle, so the count is the bus's cycle delta.
#[test]
fn cpu_clock_is_called_once_per_cpu_cycle() {
    let (mut nes, cpu_clocks, _) = hooked_nestest(ALL_HOOKS);
    let before = nes.bus.cycles;
    nes.run_frame();
    let elapsed = nes.bus.cycles - before;
    assert!(elapsed > 0, "a frame advanced no CPU cycles");
    assert_eq!(
        cpu_clocks.get(),
        elapsed,
        "cpu_clock calls vs CPU cycles over one nestest frame"
    );
}

/// A board that reports no hooks is never called for them. Nestest is NROM, whose `irq_pending`
/// and `cpu_clock` are the empty defaults, so the wrapper is the only thing counting.
#[test]
fn a_board_with_no_hooks_is_never_called() {
    let (mut nes, cpu_clocks, _) = hooked_nestest(CartHooks::default());
    let before = nes.bus.cycles;
    nes.run_frame();
    assert!(nes.bus.cycles > before, "a frame advanced no CPU cycles");
    assert_eq!(
        cpu_clocks.get(),
        0,
        "cpu_clock calls on a board that reports no hooks"
    );
}

/// Skipping the calls a board does not implement does not change the machine. The same ROM runs
/// with the wrapper reporting no hooks (the bus skips `irq_pending` and `cpu_clock`) and with both set
/// (both called every cycle, as before the change). Every frame's framebuffer, the drained audio
/// samples and the save-state bytes must all be identical.
///
/// The expansion-audio fetch is not one of the two: `cart_audio` comes from `expansion_audio_name()`,
/// and the wrapped board here is NROM, which names no chip, so *both* runs skip that fetch.
#[test]
fn skipped_hooks_produce_identical_output() {
    fn run(hooks: CartHooks) -> (u64, Vec<u32>, Vec<u8>) {
        let (mut nes, _, _) = hooked_nestest(hooks);
        let mut digest: u64 = 0x243f_6a88_85a3_08d3;
        let mut audio: Vec<u32> = Vec::new();
        for _ in 0..60 {
            nes.run_frame();
            for chunk in nes.bus.ppu.framebuffer.as_chunks::<8>().0 {
                digest ^= u64::from_le_bytes(*chunk);
                digest = digest.rotate_left(11).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            audio.extend(nes.bus.apu.samples.drain(..).map(f32::to_bits));
        }
        (digest, audio, nes.save_state())
    }

    let skipped = run(CartHooks::default());
    let called = run(ALL_HOOKS);
    assert_eq!(skipped.0, called.0, "framebuffer digest of every frame");
    assert_eq!(skipped.1, called.1, "drained audio samples");
    assert_eq!(skipped.2, called.2, "save-state bytes");
}

/// `ppu_bus` reports the PPU address bus after a $2006 completion and after the $2007 increment.
#[test]
fn ppu_bus_reports_2006_and_2007_addresses() {
    let (mut nes, _, ppu_bus) = hooked_nestest(ALL_HOOKS);
    assert!(
        !nes.bus.ppu.rendering_enabled(),
        "rendering must be off for this check"
    );

    // Two writes complete a $2006 address: v = $2000.
    nes.bus.write(0x2006, 0x20);
    nes.bus.write(0x2006, 0x00);
    assert_eq!(ppu_bus.borrow().last().copied(), Some(0x2000));

    // A $2007 write then increments v by one (VRAM increment is 1 with PPUCTRL bit 2 clear).
    ppu_bus.borrow_mut().clear();
    nes.bus.write(0x2007, 0x5A);
    assert_eq!(ppu_bus.borrow().last().copied(), Some(0x2001));
}

/// A wrapper that forwards only some hooks loses the trait default's replacement — here an
/// MMC5 whose ExRAM is the nametable source. Through the wrapper the board must still answer from
/// ExRAM, not from the (all-zero) CIRAM the default would index.
#[test]
fn the_wrapper_forwards_an_mmc5_exram_nametable() {
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
    let mut m = HookCounter {
        inner: Box::new(Mmc5::new(rom)),
        cpu_clocks: Rc::new(Cell::new(0)),
        ppu_bus: Rc::new(RefCell::new(Vec::new())),
        hooks: CartHooks::default(),
    };
    m.cpu_write(0x5104, 2); // ExRAM write mode
    m.cpu_write(0x5C00, 0x42);
    m.cpu_write(0x5105, 0xAA); // all four tables -> ExRAM
    m.cpu_write(0x5104, 0); // mode 0: ExRAM is the nametable source
    let mut ciram = [0u8; 0x1000];
    assert_eq!(m.nt_read(0x2000, &ciram), Some(0x42));
    assert_eq!(m.nt_read(0x2C00, &ciram), Some(0x42));
    assert_eq!(m.nt_peek(0x2000, &ciram), Some(0x42));
    assert!(m.nt_write(0x2001, 0x55, &mut ciram));
    assert_eq!(ciram, [0u8; 0x1000], "the write went to ExRAM, not CIRAM");
}

/// Both doubles must forward **every** `Mapper` method. A method a double leaves out keeps
/// the trait default, which is not the inner board's answer — a bug that has shipped before,
/// each time with every test green. The method list is read out of the trait itself, so a
/// hook added later fails here until both wrappers forward it.
#[test]
fn both_doubles_forward_every_mapper_method() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cart = std::fs::read_to_string(root.join("src/cart.rs")).expect("src/cart.rs is readable");
    let methods = trait_methods(&cart);
    assert!(
        methods.len() > 20,
        "the trait scan found only {} methods; it is not looking at `pub trait Mapper`",
        methods.len()
    );

    let mut missing = Vec::new();
    for file in ["tests/it/mapper_hooks.rs", "tests/it/trace_peek.rs"] {
        let src =
            std::fs::read_to_string(root.join(file)).expect("the double's source is readable");
        let src = crate::common::strip_comments(&src);
        for name in &methods {
            if !src.contains(&format!("fn {name}(")) {
                missing.push(format!("{file} drops {name}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "a double that drops a hook answers with the trait default instead of the board's:\n  {}",
        missing.join("\n  ")
    );
}

/// Every method name declared by `pub trait Mapper` in `cart.rs`, comments stripped so a
/// commented-out method cannot satisfy the scan.
fn trait_methods(cart: &str) -> Vec<String> {
    let src = crate::common::strip_comments(cart);
    let header = "pub trait Mapper {";
    let start = src
        .find(header)
        .expect("cart.rs declares `pub trait Mapper`")
        + header.len();
    let mut depth = 1usize;
    let mut end = src.len();
    for (i, c) in src[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let block = &src[start..end];
    let mut names: Vec<String> = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("fn ") {
        let after = &rest[at + 3..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        rest = &after[name.len()..];
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}
