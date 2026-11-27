//! Famicom Disk System (mapper 20) — the 2C33 RAM adapter.
//!
//! The FDS has no PRG-ROM of its own: an 8 KB BIOS sits at `$E000-$FFFF`, 32 KB of PRG RAM at
//! `$6000-$DFFF` and 8 KB of CHR RAM at PPU `$0000-$1FFF`. Everything console-specific hangs off the
//! 2C33's register file at `$4020-$4097`:
//!
//! * `$4020/$4021` timer-IRQ reload value, `$4022` its control (enable/repeat), `$4023` the master
//!   I/O enable for the disk and sound register banks,
//! * `$4024` (write) / `$4031` (read) for disk bytes, `$4030` the status register, `$4025` the
//!   control register (motor, transfer direction, nametable arrangement, CRC, transfer IRQ),
//!   `$4032` drive status,
//! * `$4040-$40A0` for the wavetable sound channel, which lives in [`crate::apu::expansion::fds`].
//!
//! The disk side model follows FCEUX's `src/fds.cpp`: the drive hands over one byte per transfer,
//! walking the block structure of the side (`$38`-byte disk info, 2-byte file count, 16-byte file
//! header, `1 + size` file data). Each transfer takes [`BYTE_TRANSFER_CYCLES`] CPU cycles — 150
//! cycles per byte is 95.4 kbit/s at 1.789 MHz, the Quick Disk's documented data rate and the
//! constant FCEUX uses — and raises the byte-transfer flag (`$4030` bit 7) plus the disk IRQ when
//! `$4025` bit 7 enables it.

use crate::apu::expansion::fds::FdsSound;
use crate::cart::{
    CartError, CartHooks, Mapper, Mirroring, RomData, chr_read, chr_write, copy_prefix, split,
};
use crate::state::{StateError, StateReader, StateWriter};

/// The fwNES `.fds` header magic.
pub const FDS_MAGIC: [u8; 4] = *b"FDS\x1a";
/// Length of the fwNES `.fds` header.
pub const FDS_HEADER_LEN: usize = 16;
/// Bytes per side in a `.fds` image.
pub const SIDE_LEN: usize = 65_500;
/// Bytes per side in a `.qd` image (CRCs included, no gaps).
pub const QD_SIDE_LEN: usize = 65_536;
/// PRG RAM the RAM adapter carries.
pub const PRG_RAM_LEN: usize = 32_768;
/// CHR RAM the RAM adapter carries.
pub const CHR_RAM_LEN: usize = 8_192;
/// BIOS length; a `disksys.rom` dump is exactly this.
pub const BIOS_LEN: usize = 8_192;
/// CPU cycles one disk byte takes at the Quick Disk's ~96 kbit/s data rate.
pub const BYTE_TRANSFER_CYCLES: u32 = 150;
/// Side count cap; no retail FDS release used more than a handful of sides.
pub const MAX_SIDES: usize = 8;
/// Leading bytes of a battery `.sav` for a disk: the sides are the data, so the file carries them
/// back to back behind this magic and the side count. A `.sav` without it was written
/// before the magic, when an FDS save held the 32 KB of PRG RAM instead of the disk.
pub const SAVE_MAGIC: [u8; 4] = *b"FDS\x00";

/// Block ids the drive walks, in the order they appear on a side.
const BLOCK_INIT: u8 = 0;
const BLOCK_VOLUME: u8 = 1;
const BLOCK_FILE_COUNT: u8 = 2;
const BLOCK_FILE_HEADER: u8 = 3;
const BLOCK_FILE_DATA: u8 = 4;

/// Length of a block's payload, from the block code and (for file data) the file header.
fn block_len(block: u8, file_size: u16) -> u32 {
    match block {
        BLOCK_VOLUME => 0x38,
        BLOCK_FILE_COUNT => 0x02,
        BLOCK_FILE_HEADER => 0x10,
        BLOCK_FILE_DATA => 1 + u32::from(file_size),
        _ => 0,
    }
}

/// The parsed sides of a disk image plus the image's declared side count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskImage {
    pub sides: Vec<Vec<u8>>,
    /// Side count from the fwNES header, when the image had one.
    pub declared_sides: Option<usize>,
}

impl DiskImage {
    pub fn side_count(&self) -> usize {
        self.sides.len()
    }
}

/// Parse a `.fds`/`.qd` image into 65500-byte sides.
///
/// * A 16-byte `FDS\x1a` header is optional; its byte 4 is the declared side count.
/// * `.fds` sides are 65500 bytes, `.qd` sides 65536 bytes with a CRC after every block, which is
///   stripped so the drive sees the same block layout a `.fds` image has.
/// * A trailing partial side is zero-filled to a whole side, so an image whose last side was
///   trimmed short still mounts ("seam" padding). A header that promises more sides than the file
///   holds is an error rather than a silent truncation.
pub fn parse_image(bytes: &[u8], qd: bool) -> Result<DiskImage, String> {
    let (data, declared_sides) = if !qd && bytes.starts_with(&FDS_MAGIC) {
        if bytes.len() < FDS_HEADER_LEN {
            return Err("fds: image ends inside the 16-byte header".to_string());
        }
        (&bytes[FDS_HEADER_LEN..], Some(usize::from(bytes[4])))
    } else {
        (bytes, None)
    };
    if data.is_empty() {
        return Err("disk image holds no data after the header".to_string());
    }

    let side_len = if qd { QD_SIDE_LEN } else { SIDE_LEN };
    let mut sides = Vec::new();
    for chunk in data.chunks(side_len) {
        let mut side = if qd {
            strip_qd_crcs(chunk)
        } else {
            chunk.to_vec()
        };
        if side.len() < SIDE_LEN {
            side.resize(SIDE_LEN, 0);
        }
        side.truncate(SIDE_LEN);
        sides.push(side);
    }
    sides.truncate(MAX_SIDES);
    if let Some(declared) = declared_sides
        && declared > sides.len()
    {
        return Err(format!(
            "fds: header declares {declared} sides but the file holds {}",
            sides.len()
        ));
    }
    Ok(DiskImage {
        sides,
        declared_sides,
    })
}

/// Copy the blocks of one `.qd` side into `.fds` layout, dropping the two CRC bytes that follow
/// every block. Stops at the first non-block byte (the zero fill after the last file).
///
/// A file-data block does not hold its own length: the 16-byte file-header block copied just before
/// it carries the file size at offsets $0D-$0E (the same bytes [`Fds::read_disk_byte`] reads), so
/// the size is remembered across blocks and handed to [`block_len`] when the data block arrives.
pub fn strip_qd_crcs(chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIDE_LEN);
    let mut pos = 0;
    let mut file_size = 0u16;
    while pos + 2 <= chunk.len() {
        let block = chunk[pos];
        let len = block_len(block, file_size) as usize;
        if len == 0 || pos + len > chunk.len() {
            break;
        }
        if block == BLOCK_FILE_HEADER {
            file_size = u16::from(chunk[pos + 0x0D]) | (u16::from(chunk[pos + 0x0E]) << 8);
        }
        out.extend_from_slice(&chunk[pos..pos + len]);
        pos += len + 2; // skip the block's CRC16
    }
    out
}

/// The FDS board.
#[derive(Clone, Debug)]
pub struct Fds {
    bios: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Vec<u8>,
    /// Disk sides as loaded (pristine), and the live copy the drive reads and writes.
    original: Vec<Vec<u8>>,
    sides: Vec<Vec<u8>>,
    side: usize,
    inserted: bool,

    /// `$4020/$4021`: timer-IRQ reload value.
    pub irq_reload: u16,
    /// Timer-IRQ down counter.
    pub irq_counter: u16,
    /// `$4022` bit 1.
    pub irq_enabled: bool,
    /// `$4022` bit 0.
    pub irq_repeat: bool,
    /// A timer IRQ is waiting to be acknowledged.
    pub irq_flag: bool,
    /// A disk (byte-transfer) IRQ is waiting to be acknowledged.
    pub disk_irq: bool,
    /// `$4023`: bit 0 disk registers, bit 1 sound registers.
    pub io_enable: u8,
    /// `$4025`: control register.
    pub control: u8,
    /// `$4024`: the byte staged for the drive when the next transfer completes.
    pub write_data: u8,
    /// `$4026`: expansion-port output latch.
    pub ext_out: u8,
    /// `$4030` bit 7: the byte transfer flag.
    pub byte_flag: bool,

    /// Which block of the side the head is on and how far into it.
    pub block: u8,
    pub block_start: u32,
    pub block_len: u32,
    pub disk_addr: u32,
    /// Size of the file being read/written, captured from its header block.
    pub file_size: u16,
    /// A transfer has been serviced at least once since the last block change.
    disk_access: bool,
    /// Cycles until the current byte transfer completes (0 = idle).
    pub transfer_timer: u32,

    /// The 2C33's wavetable channel.
    pub sound: FdsSound,
}

impl Fds {
    /// Build the board from a parsed disk image and a BIOS dump.
    ///
    /// The BIOS is validated here because every FDS boot begins in it: a missing or short dump is a
    /// load-time error, not something to discover a few thousand cycles into the reset sequence.
    pub fn new(image: DiskImage, bios: &[u8]) -> Result<Fds, CartError> {
        if bios.len() < BIOS_LEN || image.sides.is_empty() {
            return Err(CartError::Truncated);
        }
        let mut board = Fds {
            bios: bios[..BIOS_LEN].to_vec(),
            prg_ram: vec![0; PRG_RAM_LEN],
            chr: vec![0; CHR_RAM_LEN],
            original: image.sides.clone(),
            sides: image.sides,
            side: 0,
            inserted: true,
            irq_reload: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_repeat: false,
            irq_flag: false,
            disk_irq: false,
            io_enable: 0,
            control: 0,
            write_data: 0,
            ext_out: 0,
            byte_flag: false,
            block: BLOCK_INIT,
            block_start: 0,
            block_len: 0,
            disk_addr: 0,
            file_size: 0,
            disk_access: false,
            transfer_timer: 0,
            sound: FdsSound::new(),
        };
        board.reset_seek();
        Ok(board)
    }

    /// Build the board from an iNES-style mapper-20 wrapper: the 8 KB BIOS is still an external
    /// file, and the disk sides live where CHR-ROM would (`chr` is one or more 65500-byte sides).
    pub fn from_rom(rom: RomData, bios: &[u8]) -> Result<Fds, CartError> {
        if rom.chr.len() < SIDE_LEN {
            return Err(CartError::Truncated);
        }
        let mut sides = Vec::new();
        for chunk in rom.chr.chunks(SIDE_LEN) {
            let mut side = chunk.to_vec();
            side.resize(SIDE_LEN, 0);
            sides.push(side);
        }
        Fds::new(
            DiskImage {
                sides,
                declared_sides: None,
            },
            bios,
        )
    }

    /// Number of sides in the mounted image.
    pub fn side_count(&self) -> usize {
        self.sides.len()
    }

    /// The mounted side, counting from zero.
    pub fn side(&self) -> usize {
        self.side
    }

    /// Is a side in the drive?
    pub fn inserted(&self) -> bool {
        self.inserted
    }

    /// Mount side `n` (wrapping) and put the head back at the start of the side, where the BIOS
    /// expects a freshly inserted disk. Returns `(side, side count)`.
    pub fn mount_side(&mut self, n: usize) -> (usize, usize) {
        if !self.sides.is_empty() {
            self.side = n % self.sides.len();
        }
        self.inserted = true;
        self.reset_seek();
        (self.side, self.sides.len())
    }

    /// Mount the next side of the image, wrapping to side A after the last one. This is the side
    /// swap a frontend keybind drives.
    pub fn next_side(&mut self) -> (usize, usize) {
        self.mount_side(self.side + 1)
    }

    /// Take the disk out of the drive.
    pub fn eject(&mut self) {
        self.inserted = false;
        self.reset_seek();
    }

    /// Put the mounted disk back in.
    pub fn insert(&mut self) {
        self.inserted = true;
        self.reset_seek();
    }

    /// Head back to the start of the side: block 0, no pending transfer.
    fn reset_seek(&mut self) {
        self.block = BLOCK_INIT;
        self.block_start = 0;
        self.block_len = 0;
        self.disk_addr = 0;
        self.file_size = 0;
        self.disk_access = false;
        self.transfer_timer = 0;
        self.byte_flag = false;
        self.disk_irq = false;
    }

    /// Start the next byte transfer: the flag (and IRQ, when `$4025` bit 7 is set) arrives after
    /// [`BYTE_TRANSFER_CYCLES`].
    fn schedule_transfer(&mut self) {
        if self.inserted {
            self.transfer_timer = BYTE_TRANSFER_CYCLES;
        }
    }

    /// The byte at the head, advancing it within the current block.
    fn read_disk_byte(&mut self) -> u8 {
        if !self.inserted || self.block == BLOCK_INIT || self.disk_addr >= self.block_len {
            return 0;
        }
        let side_len = self.sides[self.side].len();
        let pos = (self.block_start + self.disk_addr) as usize % side_len;
        let byte = self.sides[self.side][pos];
        if self.block == BLOCK_FILE_HEADER {
            match self.disk_addr {
                0x0D => self.file_size = u16::from(byte),
                0x0E => self.file_size |= u16::from(byte) << 8,
                _ => {}
            }
        }
        self.disk_addr += 1;
        byte
    }

    /// Store a byte at the head, advancing it within the current block.
    fn write_disk_byte(&mut self, val: u8) {
        if !self.inserted || self.block == BLOCK_INIT || self.disk_addr >= self.block_len {
            return;
        }
        let side_len = self.sides[self.side].len();
        let pos = (self.block_start + self.disk_addr) as usize % side_len;
        self.sides[self.side][pos] = val;
        if self.block == BLOCK_FILE_HEADER {
            match self.disk_addr {
                0x0D => self.file_size = u16::from(val),
                0x0E => self.file_size |= u16::from(val) << 8,
                _ => {}
            }
        }
        self.disk_addr += 1;
    }

    /// `$4030`: timer IRQ, disk IRQ, nametable arrangement and byte-transfer flag. Reading it
    /// acknowledges both IRQs and clears the byte-transfer flag.
    fn read_status(&mut self) -> u8 {
        let mut v = 0;
        if self.irq_flag {
            v |= 0x01;
        }
        if self.disk_irq {
            v |= 0x02;
        }
        v |= self.control & 0x08;
        if self.byte_flag {
            v |= 0x80;
        }
        self.irq_flag = false;
        self.disk_irq = false;
        self.byte_flag = false;
        v
    }

    /// `$4031`: hand the next byte of the mounted side to the CPU, and arm the next transfer.
    fn read_data(&mut self) -> u8 {
        self.byte_flag = false;
        self.disk_irq = false;
        if !self.inserted || self.control & 0x04 == 0 {
            return 0xFF;
        }
        self.disk_access = true;
        let byte = self.read_disk_byte();
        self.schedule_transfer();
        byte
    }

    /// `$4032`: disk present, ready, write-protected.
    fn read_drive_status(&self) -> u8 {
        let mut v = 0;
        if !self.inserted {
            v |= 0x05; // no disk, write protected
        }
        if !self.inserted || self.control & 0x01 == 0 || self.control & 0x02 != 0 {
            v |= 0x02; // not ready: motor stopped or scanning reset
        }
        v
    }

    /// `$4033`: expansion-port inputs. The battery/power bit is high on a working drive.
    fn read_ext_in(&self) -> u8 {
        0x80 | (self.ext_out & 0x7F)
    }

    /// `$4025` write: motor, transfer direction, nametable arrangement, CRC and transfer IRQ.
    fn write_control(&mut self, val: u8) {
        if self.inserted {
            let rising = val & 0x40 != 0 && self.control & 0x40 == 0;
            if rising {
                // The head moved on to the next block: finish the current one and re-arm.
                self.disk_access = false;
                self.block_start += self.disk_addr;
                self.disk_addr = 0;
                self.block += 1;
                if self.block > BLOCK_FILE_DATA {
                    self.block = BLOCK_FILE_HEADER;
                }
                self.block_len = block_len(self.block, self.file_size);
                self.schedule_transfer();
            }
            if val & 0x02 != 0 {
                // Motor stop resets the seek position on the model FCEUX uses.
                self.reset_seek();
            }
        }
        self.control = val;
    }

    /// `$4024` write: stage the byte the drive will take on the next transfer.
    fn write_data_register(&mut self, val: u8) {
        self.write_data = val;
        self.disk_irq = false;
        if !self.inserted || self.control & 0x04 != 0 {
            return; // read mode (or no disk): nothing to store
        }
        if !self.disk_access {
            // FCEUX's model consumes the first staged byte at the start of a block.
            self.disk_access = true;
            return;
        }
        self.write_disk_byte(val);
        self.schedule_transfer();
    }

    /// `$4022`: timer IRQ control. Ignored while the disk registers are disabled.
    fn write_irq_control(&mut self, val: u8) {
        if self.io_enable & 0x01 == 0 {
            return;
        }
        self.irq_enabled = val & 0x02 != 0;
        self.irq_repeat = val & 0x01 != 0;
        if self.irq_enabled {
            self.irq_counter = self.irq_reload;
        } else {
            self.irq_flag = false;
        }
    }

    /// `$4023`: master I/O enable. Clearing bit 0 stops the timer and acknowledges both IRQs;
    /// clearing bit 1 silences and resets the sound unit.
    fn write_io_enable(&mut self, val: u8) {
        self.io_enable = val;
        if val & 0x01 == 0 {
            self.irq_enabled = false;
            self.irq_flag = false;
            self.disk_irq = false;
        }
        if val & 0x02 == 0 {
            self.sound.reset();
        }
    }

    /// Whether the disk register bank (`$4022-$4033`) is reachable.
    fn disk_regs(&self) -> bool {
        self.io_enable & 0x01 != 0
    }

    /// Whether the sound register bank (`$4040-$4097`) is reachable.
    fn sound_regs(&self) -> bool {
        self.io_enable & 0x02 != 0
    }

    /// Mounted side and side count, for a frontend overlay.
    pub fn side_info(&self) -> (usize, usize) {
        (self.side, self.sides.len())
    }

    /// Bytes of the mounted image that differ from the pristine copy, as `(side, offset, value)`.
    /// Save states store this instead of all 65 KB per side.
    fn disk_diff(&self) -> Vec<(usize, u16, u8)> {
        let mut out = Vec::new();
        for (side, (live, original)) in self.sides.iter().zip(&self.original).enumerate() {
            for (pos, (now, was)) in live.iter().zip(original).enumerate() {
                if now != was {
                    out.push((side, pos as u16, *now));
                }
            }
        }
        out
    }
}

impl Mapper for Fds {
    fn cpu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x6000..=0xDFFF => self.prg_ram[usize::from(addr - 0x6000) % PRG_RAM_LEN],
            0xE000..=0xFFFF => self.bios[usize::from(addr - 0xE000) % BIOS_LEN],
            _ => 0,
        }
    }

    fn cpu_write(&mut self, addr: u16, val: u8) {
        if let 0x6000..=0xDFFF = addr {
            let index = usize::from(addr - 0x6000) % PRG_RAM_LEN;
            self.prg_ram[index] = val;
        }
    }

    fn ppu_read(&mut self, addr: u16) -> u8 {
        chr_read(&self.chr, usize::from(addr))
    }

    fn ppu_write(&mut self, addr: u16, val: u8) {
        chr_write(&mut self.chr, usize::from(addr), val);
    }

    fn chr_peek(&self, addr: u16) -> u8 {
        chr_read(&self.chr, usize::from(addr))
    }

    fn mirroring(&self) -> Mirroring {
        // $4025 bit 3: 0 = vertical mirroring (horizontal arrangement), 1 = horizontal mirroring.
        if self.control & 0x08 != 0 {
            Mirroring::Horizontal
        } else {
            Mirroring::Vertical
        }
    }

    fn prg_ram(&self) -> &[u8] {
        &self.prg_ram
    }

    fn load_prg_ram(&mut self, data: &[u8]) {
        copy_prefix(&mut self.prg_ram, data);
    }

    /// A disk game saves to the disk, so the battery `.sav` carries the sides, not the 32 KB of
    /// PRG RAM the BIOS overwrites at the next boot. The layout is [`SAVE_MAGIC`], one
    /// side-count byte, then the sides back to back, [`SIDE_LEN`] bytes each.
    fn battery_data(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SAVE_MAGIC.len() + 1 + self.sides.len() * SIDE_LEN);
        out.extend_from_slice(&SAVE_MAGIC);
        out.push(self.sides.len() as u8);
        for side in &self.sides {
            out.extend_from_slice(side);
        }
        out
    }

    fn load_battery_data(&mut self, data: &[u8]) {
        let Some(rest) = data.strip_prefix(&SAVE_MAGIC) else {
            // A `.sav` with no magic held the 32 KB of PRG RAM; keep reading it as such.
            self.load_prg_ram(data);
            return;
        };
        let Some((&sides, body)) = rest.split_first() else {
            return;
        };
        for (i, side) in self.sides.iter_mut().enumerate().take(usize::from(sides)) {
            let at = i * SIDE_LEN;
            let Some(bytes) = body.get(at..at + SIDE_LEN) else {
                break;
            };
            side.copy_from_slice(bytes);
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq_flag || self.disk_irq
    }

    fn hooks(&self) -> CartHooks {
        CartHooks {
            irq: true,
            cpu_clock: true,
        }
    }

    fn cpu_clock(&mut self) {
        // Timer IRQ: the counter decrements every CPU cycle while enabled; at zero it fires,
        // reloads and, unless repeat is set, switches itself off.
        if self.irq_enabled {
            if self.irq_counter == 0 {
                self.irq_flag = true;
                self.irq_counter = self.irq_reload;
                if !self.irq_repeat {
                    self.irq_enabled = false;
                }
            } else {
                self.irq_counter -= 1;
            }
        }
        // Disk byte transfer: the flag rises after the transfer period.
        if self.transfer_timer > 0 {
            self.transfer_timer -= 1;
            if self.transfer_timer == 0 {
                self.byte_flag = true;
                if self.control & 0x80 != 0 {
                    self.disk_irq = true;
                }
            }
        }
        if self.sound_regs() {
            self.sound.clock();
        }
    }

    fn ext_read(&mut self, addr: u16) -> Option<u8> {
        let value = match addr {
            0x4030 => self.read_status(),
            0x4031 => self.read_data(),
            0x4032 => self.read_drive_status(),
            0x4033 => self.read_ext_in(),
            0x4040..=0x407F if self.sound_regs() => self.sound.read_wave(addr),
            0x4090..=0x4097 if self.sound_regs() => self.sound.read(addr),
            _ => return None,
        };
        Some(value)
    }

    fn ext_write(&mut self, addr: u16, val: u8) -> bool {
        match addr {
            0x4020 => self.irq_reload = (self.irq_reload & 0xFF00) | u16::from(val),
            0x4021 => self.irq_reload = (self.irq_reload & 0x00FF) | (u16::from(val) << 8),
            0x4022 => self.write_irq_control(val),
            0x4023 => self.write_io_enable(val),
            0x4024 if self.disk_regs() => self.write_data_register(val),
            0x4025 if self.disk_regs() => self.write_control(val),
            0x4026 => self.ext_out = val,
            0x4040..=0x407F if self.sound_regs() => self.sound.write_wave(addr, val),
            0x4080..=0x408A if self.sound_regs() => self.sound.write(addr, val),
            _ => return false,
        }
        true
    }

    fn expansion_audio(&self) -> f32 {
        // `Sound::mix_level` is a mixer-scale amplitude (the 2C33 is ~2.4x one square at full
        // scale); the generic hook is in pulse units, so convert the way the other chips do.
        if self.sound_regs() {
            crate::apu::Apu::amplitude_to_pulse_units(self.sound.mix_level())
        } else {
            0.0
        }
    }

    fn expansion_audio_name(&self) -> Option<&'static str> {
        Some("FDS 2C33")
    }

    fn next_disk_side(&mut self) -> Option<(usize, usize)> {
        Some(self.next_side())
    }

    fn disk_side(&self) -> Option<(usize, usize)> {
        Some(self.side_info())
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.control);
        w.u8(self.io_enable);
        w.u16(self.irq_reload);
        w.u16(self.irq_counter);
        w.bool(self.irq_enabled);
        w.bool(self.irq_repeat);
        w.bool(self.irq_flag);
        w.bool(self.disk_irq);
        w.u8(self.write_data);
        w.u8(self.ext_out);
        w.bool(self.byte_flag);
        w.u8(self.block);
        w.u32(self.block_start);
        w.u32(self.block_len);
        w.u32(self.disk_addr);
        w.u16(self.file_size);
        w.bool(self.disk_access);
        w.u32(self.transfer_timer);
        w.usize(self.side);
        w.bool(self.inserted);
        w.bytes(&self.prg_ram);
        w.bytes(&self.chr);
        // Disk writes are stored as a diff against the loaded image so a state stays small.
        let diff = self.disk_diff();
        w.usize(diff.len());
        for (side, pos, val) in diff {
            w.usize(side);
            w.u16(pos);
            w.u8(val);
        }
        save_sound(w, &self.sound);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.control = r.u8()?;
        self.io_enable = r.u8()?;
        self.irq_reload = r.u16()?;
        self.irq_counter = r.u16()?;
        self.irq_enabled = r.bool()?;
        self.irq_repeat = r.bool()?;
        self.irq_flag = r.bool()?;
        self.disk_irq = r.bool()?;
        self.write_data = r.u8()?;
        self.ext_out = r.u8()?;
        self.byte_flag = r.bool()?;
        self.block = r.u8()?;
        self.block_start = r.u32()?;
        self.block_len = r.u32()?;
        self.disk_addr = r.u32()?;
        self.file_size = r.u16()?;
        self.disk_access = r.bool()?;
        self.transfer_timer = r.u32()?;
        // every disk access indexes `sides[side]`, so a side the image does not have is
        // refused here rather than panicking at the next read.
        let side = r.usize()?;
        if side >= self.sides.len() {
            return Err(StateError::BadValue);
        }
        self.side = side;
        self.inserted = r.bool()?;
        r.bytes_into(&mut self.prg_ram)?;
        r.bytes_into(&mut self.chr)?;
        // Start from the image as loaded, then replay the state's writes.
        for (live, pristine) in self.sides.iter_mut().zip(&self.original) {
            live.copy_from_slice(pristine);
        }
        let count = r.usize()?;
        for _ in 0..count {
            let side = r.usize()?;
            let pos = usize::from(r.u16()?);
            let val = r.u8()?;
            if let Some(side) = self.sides.get_mut(side)
                && let Some(byte) = side.get_mut(pos)
            {
                *byte = val;
            }
        }
        load_sound(r, &mut self.sound)?;
        Ok(())
    }
}

fn save_sound(w: &mut StateWriter, s: &FdsSound) {
    w.bytes(&s.wave);
    w.bytes(&s.mod_table);
    for env in [&s.volume, &s.modulation] {
        w.u8(env.speed);
        w.u8(env.gain);
        w.bool(env.enable);
        w.bool(env.up);
        w.u32(env.timer);
    }
    w.u16(s.freq);
    w.u16(s.mod_freq);
    w.bool(s.wave_halt);
    w.bool(s.env_halt);
    w.bool(s.mod_disabled);
    w.u8(s.master_volume);
    w.bool(s.wave_write_enable);
    w.u8(s.env_speed);
    w.u32(s.mod_acc);
    w.u32(s.wave_acc);
    w.u8(s.mod_counter as u8);
}

fn load_sound(r: &mut StateReader, s: &mut FdsSound) -> Result<(), StateError> {
    r.bytes_into(&mut s.wave)?;
    r.bytes_into(&mut s.mod_table)?;
    for env in [&mut s.volume, &mut s.modulation] {
        env.speed = r.u8()?;
        env.gain = r.u8()?;
        env.enable = r.bool()?;
        env.up = r.bool()?;
        env.timer = r.u32()?;
    }
    s.freq = r.u16()?;
    s.mod_freq = r.u16()?;
    s.wave_halt = r.bool()?;
    s.env_halt = r.bool()?;
    s.mod_disabled = r.bool()?;
    s.master_volume = r.u8()?;
    s.wave_write_enable = r.bool()?;
    s.env_speed = r.u8()?;
    s.mod_acc = r.u32()?;
    s.wave_acc = r.u32()?;
    s.mod_counter = r.u8()? as i8;
    s.state_loaded();
    Ok(())
}

/// Build a mapper-20 board from an iNES image: the image's CHR carries the disk sides.
pub fn load_mapper20(rom: RomData, bios: &[u8]) -> Result<Box<dyn Mapper>, CartError> {
    Ok(Box::new(Fds::from_rom(rom, bios)?))
}

/// Parse a `.nes`-wrapped FDS image (mapper 20) and mount it.
pub fn from_ines(bytes: &[u8], bios: &[u8]) -> Result<Box<dyn Mapper>, CartError> {
    let rom = split(bytes)?;
    if rom.header.mapper != 20 {
        return Err(CartError::UnsupportedMapper(rom.header.mapper));
    }
    load_mapper20(rom, bios)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A control value that reads the mounted side: IRQ enabled, CRC on, read mode, scan on,
    /// motor running.
    const SCAN_READ: u8 = 0xE5;
    /// The same without the block-advance bit, used to re-arm the next rising edge.
    const SCAN_READ_HELD: u8 = SCAN_READ & !0x40;

    fn bios() -> Vec<u8> {
        let mut b = vec![0u8; BIOS_LEN];
        b[0] = 0xA9; // LDA #imm
        b[0x1FFC] = 0x00;
        b[0x1FFD] = 0xE0; // reset vector -> $E000
        b
    }

    // Offsets inside the synthetic side.
    const FILE_COUNT_AT: usize = 0x38;
    const FILE_HEADER_AT: usize = FILE_COUNT_AT + 2;
    const FILE_DATA_AT: usize = FILE_HEADER_AT + 0x10;

    /// A one-side image with a plausible block structure: disk info (56 bytes), file count,
    /// one file header of 16 bytes and 4 bytes of file data.
    fn image() -> DiskImage {
        let mut side = vec![0u8; SIDE_LEN];
        side[0] = BLOCK_VOLUME;
        side[1..15].copy_from_slice(b"*NINTENDO-HVC*");
        side[FILE_COUNT_AT] = BLOCK_FILE_COUNT;
        side[FILE_COUNT_AT + 1] = 1;
        side[FILE_HEADER_AT] = BLOCK_FILE_HEADER;
        side[FILE_HEADER_AT + 0x0B] = 0x00; // load address $6000
        side[FILE_HEADER_AT + 0x0C] = 0x60;
        side[FILE_HEADER_AT + 0x0D] = 0x04; // 4-byte file
        side[FILE_HEADER_AT + 0x0E] = 0x00;
        side[FILE_DATA_AT] = BLOCK_FILE_DATA;
        side[FILE_DATA_AT + 1..FILE_DATA_AT + 5].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        DiskImage {
            sides: vec![side],
            declared_sides: Some(1),
        }
    }

    fn board() -> Fds {
        Fds::new(image(), &bios()).unwrap()
    }

    /// Turn the disk registers on and start scanning the mounted side in read mode. Bit 6 is
    /// cleared first so the scan always begins on a rising edge, even after a side swap left it
    /// set.
    fn start_scan(b: &mut Fds) {
        b.ext_write(0x4023, 0x01);
        b.ext_write(0x4025, SCAN_READ_HELD);
        b.ext_write(0x4025, SCAN_READ);
    }

    /// Step the head to the next block by re-arming `$4025` bit 6.
    fn advance_block(b: &mut Fds) {
        b.ext_write(0x4025, SCAN_READ_HELD);
        b.ext_write(0x4025, SCAN_READ);
    }

    #[test]
    fn bios_is_mapped_at_e000_and_prg_ram_is_writable() {
        let mut b = board();
        assert_eq!(b.cpu_read(0xE000), 0xA9);
        assert_eq!(b.cpu_read(0xFFFC), 0x00);
        b.cpu_write(0x6000, 0x55);
        assert_eq!(b.cpu_read(0x6000), 0x55);
        b.cpu_write(0xDFFF, 0x7E);
        assert_eq!(b.cpu_read(0xDFFF), 0x7E);
        assert_eq!(b.cpu_read(0x6000), 0x55, "32K is distinct, not mirrored");
    }

    #[test]
    fn timer_irq_fires_after_the_reload_value() {
        let mut b = board();
        b.ext_write(0x4023, 0x01); // disk registers on
        b.ext_write(0x4020, 0x03); // reload = 3
        b.ext_write(0x4021, 0x00);
        b.ext_write(0x4022, 0x02); // enabled, no repeat
        assert!(!b.irq_pending(), "arming does not fire");
        for _ in 0..3 {
            b.cpu_clock();
        }
        assert!(!b.irq_pending(), "three decrements reach zero");
        b.cpu_clock();
        assert!(b.irq_pending(), "the fourth cycle fires");
        assert!(!b.irq_enabled, "one-shot clears the enable flag");
        b.cpu_clock();
        assert!(b.irq_pending(), "the flag stays until acknowledged");
    }

    #[test]
    fn repeat_mode_refires_and_reading_4030_acknowledges() {
        let mut b = board();
        b.ext_write(0x4023, 0x01);
        b.ext_write(0x4020, 0x01);
        b.ext_write(0x4021, 0x00);
        b.ext_write(0x4022, 0x03); // enabled + repeat
        for _ in 0..2 {
            b.cpu_clock();
        }
        assert!(b.irq_pending());
        let status = b.ext_read(0x4030).unwrap();
        assert_eq!(status & 1, 1, "timer bit");
        assert!(!b.irq_pending(), "reading $4030 acknowledges");
        for _ in 0..2 {
            b.cpu_clock();
        }
        assert!(b.irq_pending(), "repeat fires again");
    }

    #[test]
    fn irq_control_is_ignored_while_the_disk_registers_are_off() {
        let mut b = board();
        b.ext_write(0x4020, 0x00);
        b.ext_write(0x4021, 0x00);
        b.ext_write(0x4022, 0x03); // no effect: $4023 bit 0 is clear
        for _ in 0..4 {
            b.cpu_clock();
        }
        assert!(!b.irq_pending());
    }

    #[test]
    fn disabling_the_disk_registers_stops_the_timer() {
        let mut b = board();
        b.ext_write(0x4023, 0x01);
        b.ext_write(0x4020, 0x00);
        b.ext_write(0x4021, 0x00);
        b.ext_write(0x4022, 0x02);
        b.cpu_clock();
        assert!(b.irq_pending());
        b.ext_write(0x4023, 0x00);
        assert!(!b.irq_pending(), "$4023 bit 0 clear acknowledges and stops");
        assert!(!b.irq_enabled);
    }

    #[test]
    fn a_byte_transfer_raises_the_flag_and_the_irq_after_150_cycles() {
        let mut b = board();
        start_scan(&mut b);
        assert_eq!(b.transfer_timer, BYTE_TRANSFER_CYCLES);
        for _ in 0..BYTE_TRANSFER_CYCLES {
            b.cpu_clock();
        }
        assert!(b.byte_flag);
        assert!(b.disk_irq, "transfer IRQ enabled by $4025 bit 7");
        assert!(b.irq_pending());
        let status = b.ext_read(0x4030).unwrap();
        assert_eq!(status & 0x80, 0x80, "byte transfer flag");
        assert_eq!(status & 0x02, 0x02, "disk IRQ");
        assert!(!b.byte_flag, "reading $4030 clears the flag");
    }

    #[test]
    fn reads_walk_the_block_structure_of_the_side() {
        let mut b = board();
        start_scan(&mut b);
        // The first block is the 56-byte disk-info block, starting with its block code.
        assert_eq!(b.read_data(), BLOCK_VOLUME);
        assert_eq!(b.read_data(), b'*');
        assert_eq!(b.read_data(), b'N');
        assert_eq!(b.block, BLOCK_VOLUME);
        assert_eq!(b.disk_addr, 3);
        assert_eq!(b.transfer_timer, BYTE_TRANSFER_CYCLES, "each read re-arms");
        assert_eq!(b.control & 0x08, 0, "horizontal arrangement by default");
    }

    #[test]
    fn control_bit_three_switches_mirroring() {
        let mut b = board();
        b.ext_write(0x4023, 0x01);
        assert_eq!(b.mirroring(), Mirroring::Vertical);
        b.ext_write(0x4025, 0x08);
        assert_eq!(b.mirroring(), Mirroring::Horizontal);
        b.ext_write(0x4025, 0x00);
        assert_eq!(b.mirroring(), Mirroring::Vertical);
    }

    #[test]
    fn disk_control_writes_are_ignored_while_the_disk_registers_are_off() {
        let mut b = board();
        b.ext_write(0x4025, 0x08); // no effect: $4023 bit 0 clear
        assert_eq!(b.mirroring(), Mirroring::Vertical);
        assert_eq!(b.control, 0);
    }

    #[test]
    fn file_size_is_captured_from_the_header_block() {
        let mut b = board();
        start_scan(&mut b);
        for _ in 0..0x38 {
            b.read_data();
        }
        assert_eq!(b.block, BLOCK_VOLUME);
        advance_block(&mut b);
        assert_eq!(b.block, BLOCK_FILE_COUNT);
        for _ in 0..2 {
            b.read_data();
        }
        advance_block(&mut b);
        assert_eq!(b.block, BLOCK_FILE_HEADER);
        for _ in 0..16 {
            b.read_data();
        }
        assert_eq!(b.file_size, 4);
        advance_block(&mut b);
        assert_eq!(b.block, BLOCK_FILE_DATA);
        assert_eq!(b.block_len, 5, "one block code byte plus four data bytes");
        assert_eq!(b.read_data(), BLOCK_FILE_DATA);
        assert_eq!(b.read_data(), 0xDE);
        assert_eq!(b.read_data(), 0xAD);
        assert_eq!(b.read_data(), 0xBE);
        assert_eq!(b.read_data(), 0xEF);
    }

    #[test]
    fn writing_a_file_byte_stores_it_in_the_mounted_side() {
        let mut b = board();
        start_scan(&mut b);
        for _ in 0..0x38 {
            b.read_data();
        }
        advance_block(&mut b);
        for _ in 0..2 {
            b.read_data();
        }
        advance_block(&mut b);
        for _ in 0..16 {
            b.read_data();
        }
        advance_block(&mut b);
        assert_eq!(b.block, BLOCK_FILE_DATA);
        // Write mode: $4025 bit 2 clear (IRQ, CRC and scan stay set).
        b.ext_write(0x4025, SCAN_READ & !0x04);
        // The first staged byte after a block change is dropped, as in FCEUX's model.
        b.ext_write(0x4024, 0x11);
        b.ext_write(0x4024, 0x22);
        assert_eq!(b.sides[0][FILE_DATA_AT], 0x22);
        assert_eq!(
            b.sides[0][FILE_DATA_AT + 1],
            0xDE,
            "the dropped byte never lands"
        );
    }

    #[test]
    fn drive_status_reports_an_inserted_disk() {
        let mut b = board();
        assert_eq!(b.ext_read(0x4032).unwrap() & 0x01, 0, "disk present");
        b.eject();
        assert_eq!(
            b.ext_read(0x4032).unwrap() & 0x05,
            0x05,
            "no disk, protected"
        );
        assert_eq!(b.read_data(), 0xFF, "reads return $FF with no disk");
    }

    #[test]
    fn side_swap_remounts_at_the_start_of_the_next_side() {
        let mut img = image();
        img.sides.push(vec![0x11; SIDE_LEN]);
        let mut b = Fds::new(img, &bios()).unwrap();
        start_scan(&mut b);
        b.read_data();
        assert_eq!(b.disk_addr, 1);
        let (side, count) = b.next_side();
        assert_eq!((side, count), (1, 2));
        assert_eq!(b.disk_addr, 0, "the head returns to the start of the side");
        assert_eq!(b.block, BLOCK_INIT);
        assert!(b.inserted());
        // The BIOS restarts the scan on the freshly inserted side, which walks its blocks from
        // the top again.
        start_scan(&mut b);
        assert_eq!(b.read_data(), 0x11);
        assert_eq!(b.next_side().0, 0, "wraps back to side A");
    }

    #[test]
    fn chr_ram_is_writable_and_eight_kilobytes() {
        let mut b = board();
        b.ppu_write(0x0000, 0x5A);
        assert_eq!(b.ppu_read(0x0000), 0x5A);
        assert_eq!(b.ppu_read(0x2000), 0x5A, "8K wraps");
        assert_eq!(b.chr_peek(0x0000), 0x5A);
    }

    #[test]
    fn save_state_restores_the_timer_and_the_disk_position() {
        let mut b = board();
        start_scan(&mut b);
        b.ext_write(0x4020, 0x00);
        b.ext_write(0x4021, 0x01);
        b.ext_write(0x4022, 0x02);
        b.read_data();
        b.read_data();
        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let data = w.finish();

        let mut other = board();
        let mut r = StateReader::new(&data);
        other.load_state(&mut r).unwrap();
        assert_eq!(other.irq_reload, 0x0100);
        assert!(other.irq_enabled);
        assert_eq!(other.disk_addr, 2);
        assert_eq!(other.block, BLOCK_VOLUME);
        assert_eq!(other.side, 0);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn save_state_replays_disk_writes() {
        let mut b = board();
        b.sides[0][0] = 0x99;
        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let data = w.finish();

        let mut other = board();
        assert_eq!(other.sides[0][0], BLOCK_VOLUME);
        let mut r = StateReader::new(&data);
        other.load_state(&mut r).unwrap();
        assert_eq!(other.sides[0][0], 0x99);
    }

    #[test]
    fn a_state_naming_a_side_past_the_last_is_rejected() {
        // a damaged state used to load a side the image does not have, and the next disk
        // read indexed `sides` out of range. It is a bad value now, like any other.
        let mut b = board();
        b.side = 1;
        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let data = w.finish();
        let mut other = board();
        assert_eq!(
            other.load_state(&mut StateReader::new(&data)),
            Err(StateError::BadValue)
        );
    }

    #[test]
    fn valid_states_load_as_before() {
        // Equivalence pin: on a two-side image, each side is mounted, scanned and written,
        // saved, loaded into a fresh board and saved again. FNV-1a 64 over both saves and the
        // fresh board's disk bytes, recorded at 1b4ac87.
        let two_sides = || {
            let mut img = image();
            img.sides.push(vec![0x11; SIDE_LEN]);
            Fds::new(img, &bios()).unwrap()
        };
        let mut digest: u64 = 0xCBF2_9CE4_8422_2325;
        let mut feed = |bytes: &[u8]| {
            for &b in bytes {
                digest = (digest ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
            }
        };
        for n in 0..2 {
            let mut b = two_sides();
            b.mount_side(n);
            start_scan(&mut b);
            b.read_data();
            b.read_data();
            b.sides[n][0x100] = 0x5A;
            let mut w = StateWriter::new();
            b.save_state(&mut w);
            let first = w.finish();
            let mut other = two_sides();
            other.load_state(&mut StateReader::new(&first)).unwrap();
            start_scan(&mut other);
            let byte = other.read_data();
            let mut w = StateWriter::new();
            other.save_state(&mut w);
            let second = w.finish();
            feed(&first);
            feed(&second);
            feed(&[byte, other.side() as u8]);
            feed(&other.sides[n]);
        }
        assert_eq!(digest, 0xCE5B_491C_3D85_98E6);
    }

    #[test]
    fn fds_image_parses_with_and_without_the_header() {
        let mut with_header = Vec::from(FDS_MAGIC);
        with_header.push(1);
        with_header.extend_from_slice(&[0u8; 11]);
        with_header.extend_from_slice(&vec![7u8; SIDE_LEN]);
        let image = parse_image(&with_header, false).unwrap();
        assert_eq!(image.side_count(), 1);
        assert_eq!(image.declared_sides, Some(1));
        assert_eq!(image.sides[0][0], 7);

        let headerless = vec![9u8; SIDE_LEN];
        let image = parse_image(&headerless, false).unwrap();
        assert_eq!(image.side_count(), 1);
        assert_eq!(image.declared_sides, None);
        assert_eq!(image.sides[0][0], 9);
    }

    #[test]
    fn a_short_trailing_side_is_padded_to_a_whole_side() {
        let mut data = vec![1u8; SIDE_LEN];
        data.extend_from_slice(&[2u8; 100]);
        let image = parse_image(&data, false).unwrap();
        assert_eq!(image.side_count(), 2);
        assert_eq!(image.sides[1].len(), SIDE_LEN);
        assert_eq!(image.sides[1][99], 2);
        assert_eq!(image.sides[1][100], 0, "the seam is zero-filled");
    }

    #[test]
    fn a_header_promising_more_sides_than_the_file_holds_is_an_error() {
        let mut data = Vec::from(FDS_MAGIC);
        data.push(2);
        data.extend_from_slice(&[0u8; 11]);
        data.extend_from_slice(&vec![0u8; SIDE_LEN]);
        let err = parse_image(&data, false).unwrap_err();
        assert!(err.contains("declares 2 sides"), "{err}");
    }

    #[test]
    fn qd_sides_have_their_block_crcs_stripped() {
        // Disk info block: 56 bytes of block plus a 2-byte CRC, then the file-count block.
        let mut qd = vec![0u8; QD_SIDE_LEN];
        qd[0] = BLOCK_VOLUME;
        qd[1..15].copy_from_slice(b"*NINTENDO-HVC*");
        let count_at = 56 + 2;
        qd[count_at] = BLOCK_FILE_COUNT;
        qd[count_at + 1] = 0;
        let image = parse_image(&qd, true).unwrap();
        assert_eq!(image.side_count(), 1);
        assert_eq!(image.sides[0][1], b'*');
        assert_eq!(
            image.sides[0][56], BLOCK_FILE_COUNT,
            "CRC bytes are dropped"
        );
        assert_eq!(image.sides[0][57], 0);
    }

    #[test]
    fn qd_file_data_length_comes_from_the_file_header() {
        // One file on the side: disk info, file count, a 16-byte header whose $0D-$0E hold the
        // size, then the data block. The data bytes are $FF, so reading a length out of the data
        // block would ask for $FFFF bytes and drop the block.
        let size = 0x10usize;
        let mut qd = vec![0u8; QD_SIDE_LEN];
        let mut pos = 0;
        qd[pos] = BLOCK_VOLUME;
        qd[pos + 1..pos + 15].copy_from_slice(b"*NINTENDO-HVC*");
        pos += 0x38 + 2;
        qd[pos] = BLOCK_FILE_COUNT;
        pos += 0x02 + 2;
        qd[pos] = BLOCK_FILE_HEADER;
        qd[pos + 0x0D] = size as u8;
        pos += 0x10 + 2;
        qd[pos] = BLOCK_FILE_DATA;
        qd[pos + 1..pos + 1 + size].fill(0xFF);

        let image = parse_image(&qd, true).unwrap();
        let side = &image.sides[0];
        assert_eq!(
            side[0x38], BLOCK_FILE_COUNT,
            "the count block starts right after the volume"
        );
        assert_eq!(side[0x3A], BLOCK_FILE_HEADER);
        assert_eq!(side[0x3A + 0x0D], size as u8);
        assert_eq!(
            side[0x4A], BLOCK_FILE_DATA,
            "the data block starts right after the header"
        );
        assert_eq!(
            &side[0x4B..0x4B + size],
            &[0xFF; 0x10],
            "the whole file came across"
        );
        assert_eq!(
            side[0x4B + size],
            0,
            "the side is zero-filled after the file"
        );
    }

    #[test]
    fn a_board_without_a_bios_is_refused() {
        assert!(Fds::new(image(), &[0u8; 100]).is_err());
    }

    #[test]
    fn expansion_audio_only_plays_while_the_sound_registers_are_enabled() {
        let mut b = board();
        assert_eq!(b.expansion_audio(), 0.0);
        b.ext_write(0x4023, 0x02);
        b.ext_write(0x4089, 0x80);
        b.ext_write(0x4040, 0x3F);
        b.ext_write(0x4089, 0x00);
        b.ext_write(0x4080, 0xC0 | 0x20);
        for _ in 0..5_000 {
            b.cpu_clock();
        }
        assert!(b.expansion_audio() > 0.0);
        b.ext_write(0x4023, 0x00);
        assert_eq!(b.expansion_audio(), 0.0);
    }

    #[test]
    fn the_battery_save_carries_the_disk_sides() {
        // what a game writes to the disk is what the `.sav` must hold.
        let mut b = board();
        b.sides[0][0x100] = 0x77;
        let data = b.battery_data();
        assert_eq!(&data[..SAVE_MAGIC.len()], &SAVE_MAGIC[..]);
        assert_eq!(data.len(), SAVE_MAGIC.len() + 1 + SIDE_LEN);
        let mut other = board();
        other.load_battery_data(&data);
        assert_eq!(other.sides[0][0x100], 0x77, "the write came back");
        assert_eq!(
            other.sides[0][0], BLOCK_VOLUME,
            "the rest of the side is the image"
        );
    }

    #[test]
    fn a_save_written_before_421_still_loads_as_prg_ram() {
        // Before the magic, an FDS `.sav` held the 32 KB of PRG RAM, with no magic in front of it.
        let mut legacy = vec![0u8; PRG_RAM_LEN];
        legacy[0] = 0x5A;
        let mut b = board();
        b.load_battery_data(&legacy);
        assert_eq!(b.prg_ram[0], 0x5A);
        assert_eq!(b.sides[0][0], BLOCK_VOLUME, "the disk image is untouched");
    }
}
