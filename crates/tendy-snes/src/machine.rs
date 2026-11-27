//! The machine: the CPU and the bus composed, the one frame loop in the crate, and the entry
//! points the front end and the tests use (DESIGN §2.2).

use crate::apu::Host;
use crate::bus::Bus;
use crate::cart::Cartridge;
use crate::cpu::Cpu;
use crate::joypad::Buttons;
use crate::state::{
    STATE_MAGIC, STATE_VERSION, StateError, StateReader, StateWriter, fnv1a64, fnv1a64_continue,
    fnv1a64_slice,
};
use crate::{AUDIO_STAGING, DOTS_PER_LINE, LINES_PER_FRAME, REFRESH_TICKS};

/// One console: the bus, the CPU on it, and the frame-checksum bookkeeping.
///
/// `Machine` owns both halves of the CPU/`Bus` pair, the only shape that works: `Cpu::step` takes
/// `&mut Bus`, so a `Cpu` inside `Bus` could not be passed to it.
#[derive(Clone)]
pub struct Machine {
    pub bus: Bus,
    pub cpu: Cpu,
    pub frames: u64,
    /// fnv1a64 folded over every finished frame, in order.
    pub checksum: u64,
    /// fnv1a64 of the last finished frame alone.
    pub last_frame_checksum: u64,
}

impl Machine {
    /// Reset state (§4.5): the CPU at `E = 1`, the reset vector read from the cartridge's `$FFFC`,
    /// PPU, APU, DMA and I/O at their power-on values, the APU's IPL handshake armed.
    pub fn new(cart: Cartridge) -> Machine {
        let mut bus = Bus::new(cart);
        let cpu = Cpu::new(&mut bus);
        Machine {
            bus,
            cpu,
            frames: 0,
            checksum: 0,
            last_frame_checksum: 0,
        }
    }

    /// Game > Reset. The CPU, PPU, APU (including APU RAM), DMA and I/O return to their power-on
    /// values; cartridge SRAM keeps its contents.
    pub fn reset(&mut self) {
        self.bus.ppu = crate::ppu::Ppu::new();
        self.bus.apu = crate::apu::Apu::new(crate::apu::Host::Snes);
        self.bus.dma = crate::dma::Dma::new();
        self.bus.joypad = crate::joypad::Joypad::new();
        self.bus.io = crate::io::CpuIo::default();
        self.bus.master = 0;
        self.bus.mdr = 0;
        self.bus.nmi_latch = false;
        self.bus.stall = 0;
        self.cpu.reset(&mut self.bus);
        self.frames = 0;
        self.checksum = 0;
        self.last_frame_checksum = 0;
    }

    /// Runs whole frames until 262 lines of 1364 master clocks have elapsed. Returns master ticks
    /// consumed. The one place the frame loop lives; it allocates nothing.
    pub fn run_frame(&mut self) -> u32 {
        let mut ticks = 0;
        while !self.bus.ppu.frame_ready {
            ticks += self.cpu.step(&mut self.bus);
        }
        self.bus.ppu.take_frame();
        self.frames += 1;
        let frame = self.bus.ppu.frame();
        self.last_frame_checksum = fnv1a64_slice(frame);
        // §2.2 folds the frame with `fnv1a64_continue`, which §2.9 declares over `&[u8]`; a frame
        // is `&[u16]`, so the fold walks each word's little-endian bytes without allocating.
        for word in frame {
            self.checksum = fnv1a64_continue(self.checksum, &word.to_le_bytes());
        }
        ticks
    }

    /// 57 344 BGR555 words, row-major, top-left first.
    pub fn frame(&self) -> &[u16] {
        self.bus.ppu.frame()
    }

    pub fn frame_checksum(&self) -> u64 {
        self.checksum
    }

    pub fn last_frame_checksum(&self) -> u64 {
        self.last_frame_checksum
    }

    pub fn buttons(&self) -> Buttons {
        self.bus.joypad.pressed
    }

    pub fn set_buttons(&mut self, buttons: Buttons) {
        self.bus.joypad.set(buttons);
    }

    /// Interleaved stereo f32 at 32 kHz; returns stereo frames written.
    pub fn drain_audio(&mut self, out: &mut [f32]) -> usize {
        self.bus.apu.read_samples(out)
    }

    pub fn set_sample_rate(&mut self, hz: u32) {
        self.bus.apu.set_sample_rate(hz);
    }

    /// §10.3's eight CHANNELS rows. Forwards to `Apu::set_channel_mask`, whose mask is in the state.
    pub fn set_channel_mask(&mut self, mask: u8) {
        self.bus.apu.set_channel_mask(mask);
    }

    /// Cartridge SRAM as the chip holds it, `None` when the cartridge has none.
    pub fn battery(&self) -> Option<&[u8]> {
        if self.bus.cart.ram.is_empty() {
            None
        } else {
            Some(self.bus.cart.ram())
        }
    }

    /// Loads a `.srm` body into the cartridge's SRAM.
    ///
    /// A body longer than the chip is truncated and a shorter one fills the front, which is §8.5's
    /// rule; a cartridge with no SRAM refuses one.
    pub fn load_battery(&mut self, ram: &[u8]) -> Result<(), String> {
        if self.bus.cart.ram.is_empty() {
            return Err("the cartridge has no battery SRAM".to_string());
        }
        let len = self.bus.cart.ram.len().min(ram.len());
        self.bus.cart.ram[..len].copy_from_slice(&ram[..len]);
        self.bus.cart.ram_dirty = false;
        Ok(())
    }

    /// `SNST | u16 version | payload | u64 fnv1a64(version .. payload end)`.
    ///
    /// The payload is every device in one fixed order: the machine's own frame counters, the bus's
    /// scalars and work RAM, the CPU, the PPU, the APU (its shared block, then the staging queue the
    /// shared chip does not carry so the SGB state stays put), the DMA controller, the S-CPU's I/O,
    /// the joypad and the cartridge.
    pub fn encode_state(&self) -> Vec<u8> {
        let mut body = StateWriter::new();
        body.u16(STATE_VERSION);
        body.u64(self.frames);
        body.u64(self.checksum);
        body.u64(self.last_frame_checksum);
        body.u64(self.bus.master);
        body.u8(self.bus.mdr);
        body.u8(self.bus.dma_open);
        body.bool(self.bus.dma_open_latch);
        body.bool(self.bus.nmi_latch);
        body.u32(self.bus.stall);
        body.bytes(&self.bus.wram[..]);
        self.cpu.save_state(&mut body);
        self.bus.ppu.save_state(&mut body);
        self.bus.apu.save_state(&mut body);
        let pending = &self.bus.apu.out[self.bus.apu.read_pos..];
        body.u32(pending.len() as u32);
        for &sample in pending {
            body.u32(sample.to_bits());
        }
        self.bus.dma.save_state(&mut body);
        self.bus.io.save_state(&mut body);
        self.bus.joypad.save_state(&mut body);
        self.bus.cart.save_state(&mut body);
        let body = body.finish();
        let mut out = StateWriter::new();
        out.bytes(&STATE_MAGIC);
        out.bytes(&body);
        out.u64(fnv1a64(&body));
        out.finish()
    }

    /// Decodes a state produced by [`Machine::encode_state`], refusing a bad magic, an unknown
    /// version, a checksum that does not match, a state from another cartridge and any body whose
    /// fields this build cannot accept.
    ///
    /// The decode runs on a clone and is swapped in only after everything, the trailing length
    /// check included, has passed: a refused state leaves the machine exactly as it was. No byte of
    /// the input is trusted enough to panic on it — the ranges that could (the PPU's counters, the
    /// refresh stall, the APU's clock ledgers, the echo cursor) are checked before the swap.
    pub fn decode_state(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        let mut scratch = self.clone();
        scratch.decode_state_in_place(bytes)?;
        *self = scratch;
        Ok(())
    }

    /// The body of [`Machine::decode_state`], on a machine nobody else can see.
    fn decode_state_in_place(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        let mut r = StateReader::new(bytes);
        let mut magic = [0u8; 4];
        r.bytes_into(&mut magic)?;
        if magic != STATE_MAGIC {
            return Err(StateError::BadMagic);
        }
        let version = r.u16()?;
        if version != STATE_VERSION {
            return Err(StateError::BadVersion(version));
        }
        let Some(split) = bytes.len().checked_sub(8) else {
            return Err(StateError::Truncated);
        };
        let body = bytes
            .get(STATE_MAGIC.len()..split)
            .ok_or(StateError::Truncated)?;
        let stored = u64::from_le_bytes(
            bytes[split..]
                .try_into()
                .map_err(|_| StateError::Truncated)?,
        );
        if fnv1a64(body) != stored {
            return Err(StateError::BadChecksum);
        }

        self.frames = r.u64()?;
        self.checksum = r.u64()?;
        self.last_frame_checksum = r.u64()?;
        self.bus.master = r.u64()?;
        self.bus.mdr = r.u8()?;
        self.bus.dma_open = r.u8()?;
        self.bus.dma_open_latch = r.bool()?;
        self.bus.nmi_latch = r.bool()?;
        self.bus.stall = r.u32()?;
        r.bytes_into(&mut self.bus.wram[..])?;
        self.cpu.load_state(&mut r)?;
        self.bus.ppu.load_state(&mut r)?;
        self.bus.apu.load_state(&mut r)?;
        let pending = r.u32()? as usize;
        if pending > AUDIO_STAGING || !pending.is_multiple_of(2) {
            return Err(StateError::BadValue);
        }
        self.bus.apu.out.clear();
        self.bus.apu.read_pos = 0;
        for _ in 0..pending {
            self.bus.apu.out.push(f32::from_bits(r.u32()?));
        }
        self.bus.apu.len = self.bus.apu.out.len();
        self.bus.dma.load_state(&mut r)?;
        self.bus.io.load_state(&mut r)?;
        self.bus.joypad.load_state(&mut r)?;
        self.bus.cart.load_state(&mut r)?;
        // The trailer is the only thing left; anything else is a body this layout did not write.
        if r.remaining() != 8 {
            return Err(StateError::BadValue);
        }
        self.validate_restored()
    }

    /// Refuses a decoded value the rest of the machine cannot run with. Every bound here is one a
    /// running machine can never leave, so a state this build wrote always passes.
    fn validate_restored(&self) -> Result<(), StateError> {
        if self.bus.ppu.line >= LINES_PER_FRAME as u16
            || self.bus.ppu.dot >= DOTS_PER_LINE as u16
            || self.bus.stall > REFRESH_TICKS
            || self.bus.apu.sample_acc >= 32
            || self.bus.apu.dsp.echo.ram_index > 0xFFFF
            // `Dsp` doubles this every timer slot; a state cannot claim more than half the range.
            || self.bus.apu.dsp.samples > u64::MAX / 4
        {
            return Err(StateError::BadValue);
        }
        // The SPC ledger is stored biased and always ends a pass between `-64` cycles and zero;
        // a larger magnitude would spin `Apu::tick` for billions of iterations.
        let host = Host::Snes;
        let acc = i64::from(self.bus.apu.spc_acc) - host.acc_bias();
        if acc > 0 || acc < -64 * host.spend() {
            return Err(StateError::BadValue);
        }
        Ok(())
    }
}
