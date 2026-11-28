//! Recording (DESIGN §6.1, §6.3): a WAV of the audio, a GIF of the picture, or both, written under
//! `<system>/recordings/<stem>-<n>`; MP4 by handing frames to an ffmpeg that is already on `PATH`.
//!
//! Both encoders are hand-written so the crate list gains nothing. The WAV is the canonical 44-byte
//! RIFF header over 16-bit signed PCM; the GIF is GIF89a with a global colour table and one
//! LZW-compressed image per emulated frame. Nothing is held in memory beyond the frame being
//! encoded, so a long capture costs disk, not RAM: the GIF is one image block written as it is
//! encoded and the WAV is streamed, with its header's two length fields patched in at the end.
//!
//! A frame crosses the boundary as RGBA8888, whatever the system. The GIF quantizes it into a fixed
//! 3-3-2 cube of 256 entries — all a GIF table can hold — and the MP4 hands the same picture to
//! ffmpeg as raw RGB. The size and rate come from the machine itself, so no constant here knows
//! which console is being captured.
//!
//! A capture stops itself at [`MAX_SECONDS`], so a forgotten stop cannot fill the disk, and it stops
//! when another game opens: a recording belongs to the cartridge it started on.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::OnceLock;

use crate::app::App;
use crate::menu::{Row, RowAct, RowKind};

/// The longest a capture runs before it stops itself: ten minutes of emulated time.
pub const MAX_SECONDS: u64 = 600;

/// A GIF frame delay is in hundredths of a second; 59.73 Hz is 1.675 of them and a file can only
/// say whole ones. 2 is 50 Hz, the closest a GIF can get without halving the frame rate.
pub const GIF_DELAY_CS: u16 = 2;

/// The frame rate the cap counts in. Every core runs near 60 Hz; the exact rate does not matter to
/// a safety cap that is ten minutes long.
const FPS: f64 = 59.7275;

/// The frames a picture capture keeps before it stops at `seconds`.
pub fn cap_frames(seconds: u64) -> u64 {
    (seconds as f64 * FPS).ceil() as u64
}

/// The audio frames a WAV capture keeps before it stops at `seconds`.
pub fn cap_audio_frames(seconds: u64, hz: u32) -> u64 {
    seconds.saturating_mul(u64::from(hz))
}

/// What the Record row is set to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordMode {
    /// Nothing is captured.
    #[default]
    Off,
    Gif,
    Wav,
    /// Both files of one capture, named with the same index.
    GifWav,
    /// Handed to ffmpeg's stdin as raw RGB; refused when ffmpeg is not on `PATH`.
    Mp4,
}

impl RecordMode {
    /// The word `config.toml` writes.
    pub fn name(self) -> &'static str {
        match self {
            RecordMode::Off => "off",
            RecordMode::Gif => "gif",
            RecordMode::Wav => "wav",
            RecordMode::GifWav => "gif_wav",
            RecordMode::Mp4 => "mp4",
        }
    }

    /// The settings-file spelling, or `None` for anything else.
    pub fn from_name(word: &str) -> Option<RecordMode> {
        match word {
            "off" => Some(RecordMode::Off),
            "gif" => Some(RecordMode::Gif),
            "wav" => Some(RecordMode::Wav),
            "gif_wav" => Some(RecordMode::GifWav),
            "mp4" => Some(RecordMode::Mp4),
            _ => None,
        }
    }

    /// The row's value column. `ffmpeg` is whether the encoder is already on `PATH`: the MP4 row
    /// names its own missing dependency rather than offering a capture that cannot start.
    pub fn label(self, ffmpeg: bool) -> &'static str {
        match self {
            RecordMode::Off => "Off",
            RecordMode::Gif => "GIF",
            RecordMode::Wav => "WAV",
            RecordMode::GifWav => "GIF + WAV",
            RecordMode::Mp4 if ffmpeg => "MP4",
            RecordMode::Mp4 => "MP4 (needs ffmpeg)",
        }
    }

    /// `off` -> `gif` -> `wav` -> `gif + wav` -> `mp4` -> `off`, each way round.
    pub fn step(self, forward: bool) -> RecordMode {
        match (self, forward) {
            (RecordMode::Off, true) => RecordMode::Gif,
            (RecordMode::Gif, true) => RecordMode::Wav,
            (RecordMode::Wav, true) => RecordMode::GifWav,
            (RecordMode::GifWav, true) => RecordMode::Mp4,
            (RecordMode::Mp4, true) => RecordMode::Off,
            (RecordMode::Off, false) => RecordMode::Mp4,
            (RecordMode::Gif, false) => RecordMode::Off,
            (RecordMode::Wav, false) => RecordMode::Gif,
            (RecordMode::GifWav, false) => RecordMode::Wav,
            (RecordMode::Mp4, false) => RecordMode::GifWav,
        }
    }

    pub fn wants_gif(self) -> bool {
        matches!(self, RecordMode::Gif | RecordMode::GifWav)
    }

    pub fn wants_wav(self) -> bool {
        matches!(self, RecordMode::Wav | RecordMode::GifWav)
    }

    pub fn wants_mp4(self) -> bool {
        matches!(self, RecordMode::Mp4)
    }

    /// The extensions this mode writes, in the order the notice names them.
    fn suffixes(self) -> &'static [&'static str] {
        match self {
            RecordMode::Off => &[],
            RecordMode::Gif => &["gif"],
            RecordMode::Wav => &["wav"],
            RecordMode::GifWav => &["gif", "wav"],
            RecordMode::Mp4 => &["mp4"],
        }
    }
}

/// Whether `ffmpeg` is already on `PATH`, probed once per process.
///
/// This never installs, downloads or looks anywhere else: MP4 works where the owner already has an
/// encoder and nowhere else.
pub fn ffmpeg_available() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| {
        command("ffmpeg")
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

/// A child process that does not flash a console window: the front end is a GUI process, and
/// ffmpeg writes its own diagnostics to the pipe rather than to a terminal.
fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW` from `processthreadsapi.h`.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The WAV header's length, and the canonical header size every 16-bit PCM WAV is measured from.
pub const WAV_HEADER_LEN: usize = 44;

/// The canonical 44-byte RIFF/WAVE header for 16-bit signed stereo PCM: `sample_frames` frames at
/// `hz`, which is `sample_frames * 4` bytes of samples.
pub fn wav_header(sample_frames: u32, hz: u32) -> [u8; WAV_HEADER_LEN] {
    let data = sample_frames.saturating_mul(4);
    let mut h = [0u8; WAV_HEADER_LEN];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&36u32.saturating_add(data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&2u16.to_le_bytes());
    h[24..28].copy_from_slice(&hz.to_le_bytes());
    h[28..32].copy_from_slice(&hz.saturating_mul(4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// One `f32` sample as 16-bit PCM: clamped to ±1 and scaled by 32767, so full scale is 32767 and
/// not 32768 — the asymmetric edge is the clipping side, where a louder sample is cut anyway.
pub fn pcm16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

/// One WAV file being written: the header up front, samples after it, the two length fields patched
/// when the capture stops.
struct WavFile {
    out: BufWriter<File>,
    /// 16-bit little-endian bytes for one batch, reused so a frame allocates nothing.
    bytes: Vec<u8>,
    frames: u64,
    hz: u32,
}

impl WavFile {
    fn create(path: &Path, hz: u32) -> io::Result<WavFile> {
        let mut out = BufWriter::new(File::create(path)?);
        // A zero-frame header first, so the file is a WAV from its first byte; the real lengths go
        // in when the capture stops.
        out.write_all(&wav_header(0, hz))?;
        Ok(WavFile {
            out,
            bytes: Vec::with_capacity(8_192),
            frames: 0,
            hz,
        })
    }

    /// `samples` is interleaved stereo: two `f32` per frame.
    fn push(&mut self, samples: &[f32]) -> io::Result<()> {
        self.bytes.clear();
        for sample in samples {
            self.bytes.extend_from_slice(&pcm16(*sample).to_le_bytes());
        }
        self.out.write_all(&self.bytes)?;
        self.frames += (samples.len() / 2) as u64;
        Ok(())
    }

    /// Flush, then rewrite the header now that the frame count is known.
    fn finish(mut self) -> io::Result<()> {
        self.out.flush()?;
        let mut file = self.out.into_inner().map_err(|why| why.into_error())?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&wav_header(
            self.frames.min(u64::from(u32::MAX)) as u32,
            self.hz,
        ))?;
        file.flush()
    }
}

/// The code that resets the dictionary mid-stream and the one that ends it: both are the two codes
/// above the pixel values, so they move with the table's size.
fn gif_clear(bits: u32) -> u16 {
    1 << bits.max(2)
}

/// A GIF dictionary holds 4096 codes: twelve bits is the widest a code can be.
const GIF_CODES: u16 = 4096;

/// How a capture turns the machine's RGBA frames into pixels: the machine's own size, and the
/// colour table a GIF carries for it.
///
/// Every core hands the front end RGBA8888, so the GIF quantizes into a fixed 3-3-2 cube of 256
/// entries and the MP4 drops the alpha byte. The size comes from the machine's frame at the moment
/// the capture starts, so a Game Boy, an Advance, an NES and a Super NES all write their own shape
/// without a constant here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoFormat {
    /// Visible width in pixels.
    pub width: u16,
    /// Visible height in pixels.
    pub height: u16,
}

impl VideoFormat {
    /// The format of a frame that is `width` x `height` pixels.
    pub fn new(width: u32, height: u32) -> VideoFormat {
        VideoFormat {
            width: width.min(u32::from(u16::MAX)) as u16,
            height: height.min(u32::from(u16::MAX)) as u16,
        }
    }

    /// Pixels in one frame: the number the encoder asserts a frame against.
    pub fn pixels(&self) -> usize {
        usize::from(self.width) * usize::from(self.height)
    }

    /// The GIF colour table: the 256 entries of a 3-3-2 cube, and the bits a pixel takes in the
    /// table (`8`).
    fn table(&self) -> (Vec<[u8; 3]>, u32) {
        let mut cube = Vec::with_capacity(256);
        for index in 0..256u16 {
            cube.push(cube_rgb(index as u8));
        }
        (cube, 8)
    }
}

/// One entry of the 3-3-2 cube: the colour an index names, at the middle of its bucket. Three bits
/// of red, three of green and two of blue is 256 entries — all a GIF's table can hold, and the
/// split the eye minds least, since green carries most of the luminance.
fn cube_rgb(index: u8) -> [u8; 3] {
    let r = index >> 5;
    let g = (index >> 2) & 0x07;
    let b = index & 0x03;
    [
        (u16::from(r) * 255 / 7) as u8,
        (u16::from(g) * 255 / 7) as u8,
        (u16::from(b) * 255 / 3) as u8,
    ]
}

/// The cube index an RGBA pixel falls in: the top three bits of each channel, two of blue's.
/// `(r3 << 5) | (g3 << 2) | b2`, which is the layout [`cube_rgb`] reads back.
pub fn cube_index(red: u8, green: u8, blue: u8) -> u8 {
    ((red >> 5) << 5) | ((green >> 5) << 2) | (blue >> 6)
}

/// The bit packer: GIF codes are packed least-significant bit first, into bytes.
struct BitSink {
    acc: u32,
    bits: u32,
}

impl BitSink {
    fn code(&mut self, code: u16, size: u32, out: &mut Vec<u8>) {
        self.acc |= u32::from(code) << self.bits;
        self.bits += size;
        while self.bits >= 8 {
            out.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    /// The last partial byte, if the stream did not end on a byte boundary.
    fn flush(&mut self, out: &mut Vec<u8>) {
        if self.bits > 0 {
            out.push((self.acc & 0xFF) as u8);
        }
        self.acc = 0;
        self.bits = 0;
    }
}

/// A GIF89a encoder: the file's header, one LZW-compressed image per frame, and the trailer.
///
/// The encoder keeps the dictionary and the output buffer between frames, so a capture allocates
/// once and then writes its frames as they are produced.
pub struct GifEncoder {
    /// The frame's own size.
    width: u16,
    height: u16,
    /// The global colour table: the 3-3-2 cube's 256 entries, as RGB.
    palette: Vec<[u8; 3]>,
    /// Bits a pixel takes in the table: 8 for the cube. It is also the LZW minimum code size.
    bits: u32,
    /// Compressed bytes of the frame being written, reused.
    codes: Vec<u8>,
    /// `(prefix * table_size + symbol) -> code`, 0 for "no entry" (assigned codes start above the
    /// two reserved ones).
    dict: Vec<u16>,
}

impl GifEncoder {
    /// An encoder for one format. The palette's alpha byte is dropped, because GIF has no alpha in
    /// a plain global table.
    pub fn new(format: VideoFormat) -> GifEncoder {
        let (palette, bits) = format.table();
        // A key is `prefix * table_size + symbol` and `prefix` is itself a dictionary code, so the
        // table has to hold every code (up to 4095) times every symbol: two megabytes, allocated
        // once, for the length of the capture.
        let dict_size = GIF_CODES as usize * (1usize << bits);
        GifEncoder {
            width: format.width,
            height: format.height,
            palette,
            bits,
            codes: Vec::new(),
            dict: vec![0u16; dict_size],
        }
    }

    /// The GIF89a signature, the logical screen descriptor, the global colour table and the
    /// NETSCAPE2.0 block that makes the file loop for ever. Everything before the first frame.
    pub fn header(&self, out: &mut impl Write) -> io::Result<()> {
        let mut head = Vec::with_capacity(13 + 12 + 19);
        head.extend_from_slice(b"GIF89a");
        head.extend_from_slice(&self.width.to_le_bytes());
        head.extend_from_slice(&self.height.to_le_bytes());
        // Global colour table present, 8-bit colour resolution, a table of 2^bits entries, so the
        // packed byte is 0b1_111_0_000 | (bits - 1) — _111 for the cube. Background 0; square
        // pixels (aspect ratio 0).
        head.extend_from_slice(&[0b1111_0000 | (self.bits as u8 - 1), 0, 0]);
        for rgb in &self.palette {
            head.extend_from_slice(rgb);
        }
        head.extend_from_slice(&[0x21, 0xFF, 0x0B]);
        head.extend_from_slice(b"NETSCAPE2.0");
        head.extend_from_slice(&[0x03, 0x01, 0x00, 0x00, 0x00]);
        out.write_all(&head)
    }

    /// One whole frame of table indices, with `delay_cs` hundredths of a second on screen.
    pub fn frame(&mut self, out: &mut impl Write, indices: &[u8], delay_cs: u16) -> io::Result<()> {
        assert_eq!(
            indices.len(),
            usize::from(self.width) * usize::from(self.height),
            "a GIF frame is the whole {}x{} picture",
            self.width,
            self.height
        );
        self.compress(indices);
        // Graphic control extension: disposal 0 (leave in place), no transparency, the delay.
        let mut gce = [0u8; 8];
        gce[..3].copy_from_slice(&[0x21, 0xF9, 0x04]);
        gce[3] = 0;
        gce[4..6].copy_from_slice(&delay_cs.to_le_bytes());
        out.write_all(&gce)?;
        // Image descriptor: at 0,0, the whole screen, no local table, not interlaced.
        let mut desc = [0u8; 10];
        desc[0] = 0x2C;
        desc[5..7].copy_from_slice(&self.width.to_le_bytes());
        desc[7..9].copy_from_slice(&self.height.to_le_bytes());
        out.write_all(&desc)?;
        out.write_all(&[self.bits.max(2) as u8])?;
        for block in self.codes.chunks(255) {
            out.write_all(&[block.len() as u8])?;
            out.write_all(block)?;
        }
        out.write_all(&[0])
    }

    /// The file's last byte.
    pub fn trailer(&self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(&[0x3B])
    }

    /// GIF's variable-length LZW over the frame's indices, into `self.codes`.
    ///
    /// The code size grows one code *after* the dictionary reaches the old size — `next > 1 << size`
    /// — because the decoder adds its last entry one step behind the encoder.
    fn compress(&mut self, indices: &[u8]) {
        let min_code_size = self.bits.max(2);
        let clear = gif_clear(self.bits);
        let end = clear + 1;
        let symbols = 1usize << min_code_size;
        self.codes.clear();
        self.dict.fill(0);
        let mut sink = BitSink { acc: 0, bits: 0 };
        let mut size = min_code_size + 1;
        let mut next = end + 1;
        sink.code(clear, size, &mut self.codes);
        let mut prefix = u16::from(indices[0]);
        for symbol in &indices[1..] {
            let symbol = u16::from(*symbol);
            let key = usize::from(prefix) * symbols + usize::from(symbol);
            if self.dict[key] != 0 {
                prefix = self.dict[key];
                continue;
            }
            sink.code(prefix, size, &mut self.codes);
            if next < GIF_CODES {
                self.dict[key] = next;
                next += 1;
                if next > (1 << size) && size < 12 {
                    size += 1;
                }
            } else {
                // The dictionary is full: the decoder resets at a clear code, so the encoder sends
                // one and starts again from the floor.
                sink.code(clear, size, &mut self.codes);
                self.dict.fill(0);
                size = min_code_size + 1;
                next = end + 1;
            }
            prefix = symbol;
        }
        sink.code(prefix, size, &mut self.codes);
        sink.code(end, size, &mut self.codes);
        sink.flush(&mut self.codes);
    }
}

/// One GIF file being written: a buffered file and the encoder's reused state.
struct GifFile {
    out: BufWriter<File>,
    enc: GifEncoder,
    frames: u64,
}

impl GifFile {
    fn create(path: &Path, format: VideoFormat) -> io::Result<GifFile> {
        let mut out = BufWriter::new(File::create(path)?);
        let enc = GifEncoder::new(format);
        enc.header(&mut out)?;
        Ok(GifFile {
            out,
            enc,
            frames: 0,
        })
    }

    fn push(&mut self, indices: &[u8]) -> io::Result<()> {
        self.enc.frame(&mut self.out, indices, GIF_DELAY_CS)?;
        self.frames += 1;
        Ok(())
    }

    fn finish(mut self) -> io::Result<()> {
        self.enc.trailer(&mut self.out)?;
        self.out.flush()
    }
}

/// One MP4 being made by ffmpeg: raw RGB frames on its stdin and no opinion of our own about the
/// container.
struct Mp4File {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Mp4File {
    /// Spawn ffmpeg to turn `pipe:0`'s raw RGB into an H.264 MP4 at `path`.
    ///
    /// The frame rate is the machine's own rate as a fraction with [`FPS_SCALE`](1_0000), so the
    /// file's timestamps are the emulator's and not a rounded 60.
    fn start(path: &Path, format: VideoFormat, rate: u32) -> Result<Mp4File, String> {
        let size = format!("{}x{}", format.width, format.height);
        let framerate = format!("{rate}/10000");
        let mut child = command("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                &size,
                "-framerate",
                &framerate,
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|why| format!("cannot start ffmpeg: {why}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "cannot write to ffmpeg: no stdin".to_string())?;
        Ok(Mp4File {
            child,
            stdin: Some(stdin),
        })
    }

    /// One frame of RGB24, already expanded from RGBA by the recorder.
    fn push(&mut self, rgb: &[u8]) -> io::Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            stdin.write_all(rgb)?;
        }
        Ok(())
    }

    /// Close the pipe and wait: ffmpeg finalises the file when its input ends.
    fn finish(mut self) -> Result<(), String> {
        drop(self.stdin.take());
        let status = self
            .child
            .wait()
            .map_err(|why| format!("cannot wait for ffmpeg: {why}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("ffmpeg failed ({status})"))
        }
    }
}

/// One capture in progress: whichever of the files the mode asked for, the counters the cap reads,
/// and the names the notice will print.
pub struct Recorder {
    /// The cartridge being captured: a capture belongs to the game it started on, and a different
    /// cartridge opening stops it rather than writing two games into one file.
    rom: PathBuf,
    wav: Option<WavFile>,
    gif: Option<GifFile>,
    mp4: Option<Mp4File>,
    /// The files this capture writes, in the order the notice names them.
    paths: Vec<PathBuf>,
    /// Picture frames captured, and audio frames written.
    frames: u64,
    audio_frames: u64,
    /// The cap for this capture, in emulated seconds.
    cap_seconds: u64,
    /// The device rate the WAV is written at.
    hz: u32,
    /// The picture's own size: the frame every file must be written at.
    format: VideoFormat,
    /// One frame of GIF table indices, reused.
    indices: Vec<u8>,
    /// One frame of RGB24 for the MP4 encoder, reused.
    rgb: Vec<u8>,
    /// The first write failure, if any: the capture stops and the notice says why.
    error: Option<String>,
}

impl Recorder {
    /// Start capturing `rom`'s next free index into the recordings folder.
    ///
    /// `ffmpeg` says whether the encoder is on `PATH`: an MP4 capture without it is refused here,
    /// before any file is made, with a notice rather than a silent failure. `rate` is the machine's
    /// frame rate times 10 000, so ffmpeg writes timestamps at the emulator's own speed.
    pub fn start(
        mode: RecordMode,
        rom: &Path,
        hz: u32,
        ffmpeg: bool,
        format: VideoFormat,
        rate: u32,
        dir: &Path,
    ) -> Result<Recorder, String> {
        Recorder::start_with_cap(mode, rom, hz, ffmpeg, format, rate, MAX_SECONDS, dir)
    }

    /// [`Recorder::start`] with the cap named: the tests' door, and the one a future probe uses so
    /// its files land under a scratch folder. The seven settings are each independent, so a
    /// settings struct would only add a name to every call site.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_cap(
        mode: RecordMode,
        rom: &Path,
        hz: u32,
        ffmpeg: bool,
        format: VideoFormat,
        rate: u32,
        cap_seconds: u64,
        dir: &Path,
    ) -> Result<Recorder, String> {
        if mode == RecordMode::Off {
            return Err("recording is off: set Record first".to_string());
        }
        if mode.wants_mp4() && !ffmpeg {
            return Err("MP4 needs ffmpeg on PATH".to_string());
        }
        std::fs::create_dir_all(dir)
            .map_err(|why| format!("cannot create {}: {why}", dir.display()))?;
        let index = next_index(rom, dir);
        let stem = stem(rom);
        let mut paths = Vec::with_capacity(mode.suffixes().len());
        let mut recorder = Recorder {
            rom: rom.to_path_buf(),
            wav: None,
            gif: None,
            mp4: None,
            paths: Vec::new(),
            frames: 0,
            audio_frames: 0,
            cap_seconds,
            hz,
            format,
            indices: Vec::with_capacity(format.pixels()),
            rgb: Vec::with_capacity(format.pixels() * 3),
            error: None,
        };
        for suffix in mode.suffixes() {
            let path = dir.join(format!("{stem}-{index}.{suffix}"));
            match *suffix {
                "wav" => {
                    recorder.wav = Some(
                        WavFile::create(&path, hz)
                            .map_err(|why| format!("cannot write {}: {why}", path.display()))?,
                    );
                }
                "gif" => {
                    recorder.gif = Some(
                        GifFile::create(&path, format)
                            .map_err(|why| format!("cannot write {}: {why}", path.display()))?,
                    );
                }
                _ => {
                    recorder.mp4 = match Mp4File::start(&path, format, rate) {
                        Ok(mp4) => Some(mp4),
                        Err(why) => {
                            // A refused ffmpeg makes the other file pointless to keep writing:
                            // close what did open, and report the failure.
                            if let Some(wav) = recorder.wav.take() {
                                let _ = wav.finish();
                            }
                            if let Some(gif) = recorder.gif.take() {
                                let _ = gif.finish();
                            }
                            for path in &recorder.paths {
                                let _ = std::fs::remove_file(path);
                            }
                            return Err(format!("{}: {why}", path.display()));
                        }
                    };
                }
            }
            paths.push(path);
        }
        recorder.paths = paths;
        Ok(recorder)
    }

    /// The files this capture is writing.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// The cartridge this capture belongs to.
    pub fn rom(&self) -> &Path {
        &self.rom
    }

    /// Whether a WAV is open, so the window knows the samples must be mixed and resampled even when
    /// there is no sound card to hand them to.
    pub fn wants_audio(&self) -> bool {
        self.wav.is_some()
    }

    /// The line the bar shows when the capture starts: what is being written.
    pub fn started_note(&self) -> String {
        let names: Vec<String> = self
            .paths
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();
        format!("recording {}", names.join(", "))
    }

    /// One emulated frame of RGBA8888, into every picture file the mode opened.
    pub fn push_frame(&mut self, rgba: &[u8]) {
        let expected = self.format.pixels() * 4;
        if rgba.len() != expected {
            self.error = Some(format!(
                "a frame of {} bytes is not this capture's {}x{} RGBA",
                rgba.len(),
                self.format.width,
                self.format.height
            ));
            self.gif = None;
            self.mp4 = None;
            return;
        }
        if self.gif.is_some() {
            self.indices.clear();
            for pixel in rgba.as_chunks::<4>().0 {
                self.indices.push(cube_index(pixel[0], pixel[1], pixel[2]));
            }
            if let Some(gif) = self.gif.as_mut()
                && let Err(why) = gif.push(&self.indices)
            {
                self.error = Some(format!("cannot write a GIF frame: {why}"));
                self.gif = None;
            }
        }
        if self.mp4.is_some() {
            self.rgb.clear();
            for pixel in rgba.as_chunks::<4>().0 {
                self.rgb.extend_from_slice(&pixel[..3]);
            }
            if let Some(mp4) = self.mp4.as_mut()
                && let Err(why) = mp4.push(&self.rgb)
            {
                self.error = Some(format!("cannot write to ffmpeg: {why}"));
                self.mp4 = None;
            }
        }
        self.frames += 1;
    }

    /// One batch of device-rate samples (interleaved stereo) into the WAV, if the mode asked for
    /// one.
    pub fn push_audio(&mut self, samples: &[f32]) {
        if let Some(wav) = self.wav.as_mut()
            && let Err(why) = wav.push(samples)
        {
            self.error = Some(format!("cannot write the WAV: {why}"));
            self.wav = None;
        }
        self.audio_frames += (samples.len() / 2) as u64;
    }

    /// Whether the capture has run its cap, or a write failed: the loop must stop it now.
    pub fn finished(&self) -> bool {
        self.error.is_some() || self.reached_cap()
    }

    /// The cap rule on its own, so it can be tested without writing ten minutes of file: a picture
    /// capture stops at [`cap_frames`], a WAV at [`cap_audio_frames`].
    pub fn reached_cap(&self) -> bool {
        (self.gif.is_some() || self.mp4.is_some()) && self.frames >= cap_frames(self.cap_seconds)
            || self.wav.is_some()
                && self.audio_frames >= cap_audio_frames(self.cap_seconds, self.hz)
    }

    /// The frames the picture files hold, for a test.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Close every file and say what happened. Consumes the recorder: a stopped capture is over.
    ///
    /// The notice names every file written and its size, and says when the cap stopped it rather
    /// than the row.
    pub fn stop(mut self) -> String {
        let capped = self.reached_cap();
        let mut failures = Vec::new();
        if let Some(mp4) = self.mp4.take()
            && let Err(why) = mp4.finish()
        {
            failures.push(why);
        }
        if let Some(gif) = self.gif.take()
            && let Err(why) = gif.finish()
        {
            failures.push(format!("cannot finish the GIF: {why}"));
        }
        if let Some(wav) = self.wav.take()
            && let Err(why) = wav.finish()
        {
            failures.push(format!("cannot finish the WAV: {why}"));
        }
        let sizes: Vec<String> = self
            .paths
            .iter()
            .filter_map(|path| {
                let size = std::fs::metadata(path).ok()?.len();
                Some(format!(
                    "{} ({})",
                    path.file_name()?.to_string_lossy(),
                    file_size(size)
                ))
            })
            .collect();
        if let Some(why) = self.error.take() {
            failures.push(why);
        }
        if !failures.is_empty() {
            return format!("recording failed: {}", failures.join("; "));
        }
        if capped {
            format!(
                "recording stopped at {} s: {}",
                self.cap_seconds,
                sizes.join(", ")
            )
        } else {
            format!("recording saved {}", sizes.join(", "))
        }
    }
}

/// A drop is a stop without the notice: the app quitting or a panic still closes the files, so a
/// WAV gets its lengths, a GIF its trailer and an MP4 its last frame instead of a truncated file.
impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(mp4) = self.mp4.take() {
            let _ = mp4.finish();
        }
        if let Some(gif) = self.gif.take() {
            let _ = gif.finish();
        }
        if let Some(wav) = self.wav.take() {
            let _ = wav.finish();
        }
    }
}

/// A byte count as the notice spells it: `1.9 MB`, `412 KB`.
fn file_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// The ROM's stem, for the file name: the same rule `screenshot.rs` uses.
fn stem(rom: &Path) -> String {
    rom.file_stem().map_or_else(
        || "recording".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

/// The next free index for `rom` in `dir`: one past the highest `<stem>-<n>.<anything>`, so a GIF
/// and its WAV of one capture share the number and a later capture never overwrites them.
pub fn next_index(rom: &Path, dir: &Path) -> u32 {
    let prefix = format!("{}-", stem(rom));
    let mut next = 1;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(tail) = name.strip_prefix(&prefix) else {
                continue;
            };
            let Some((number, _)) = tail.split_once('.') else {
                continue;
            };
            if let Ok(n) = number.parse::<u32>() {
                next = next.max(n + 1);
            }
        }
    }
    next
}

/// The session's chosen mode and the capture running, if any.
#[derive(Default)]
struct Session {
    mode: RecordMode,
    recorder: Option<Recorder>,
    rom: Option<PathBuf>,
    /// The machine's frame counter at the last pushed frame.
    last_frame: u64,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

/// A module row: the drawer sends its activation back to [`on_row`].
fn row(
    label: impl Into<String>,
    value: impl Into<String>,
    kind: RowKind,
    desc: &'static str,
) -> Row {
    Row {
        label: label.into(),
        value: value.into(),
        kind,
        desc,
        hotkey: "",
        systems: crate::menu::Systems::ALL,
        requires: None,
        act: None,
    }
}

/// The recording rows: the mode choice and the start/stop switch.
pub fn rows(app: &App) -> Vec<Row> {
    if app.machine().is_none() {
        return Vec::new();
    }
    let (mode, recording) = SESSION.with(|cell| {
        let session = cell.borrow();
        (session.mode, session.recorder.is_some())
    });
    vec![
        row(
            "Record",
            mode.label(ffmpeg_available()),
            RowKind::Choice,
            "What a capture writes",
        ),
        row(
            if recording {
                "Stop recording"
            } else {
                "Start recording"
            },
            "",
            RowKind::Action,
            "Write the game to the recordings folder",
        ),
    ]
}

/// Handles the recording rows: index 0 picks the mode, index 1 starts or stops the capture.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    match index {
        0 => {
            let forward = !matches!(act, RowAct::Step(crate::settings::Direction::Left));
            SESSION.with(|cell| {
                let mut session = cell.borrow_mut();
                session.mode = session.mode.step(forward);
            });
            true
        }
        1 => {
            if act != RowAct::Activate {
                return false;
            }
            if SESSION.with(|cell| cell.borrow().recorder.is_some()) {
                stop(app);
            } else if let Err(why) = start(app) {
                app.show_notice(format!("recording not started: {why}"), true);
            }
            true
        }
        _ => false,
    }
}

/// Starts capturing the running game in the chosen mode.
fn start(app: &mut App) -> Result<(), String> {
    let mode = SESSION.with(|cell| cell.borrow().mode);
    let system = app.system().ok_or("no game is running")?;
    let rom = app
        .rom()
        .map(Path::to_path_buf)
        .ok_or("the game was not opened from a file, so it has no recording name")?;
    let hz = app.sample_rate();
    let (format, rate) = {
        let machine = app.machine_mut().ok_or("no game is running")?;
        let rate = (machine.frame_rate() * 10_000.0).round() as u32;
        let frame = machine.frame();
        (VideoFormat::new(frame.width, frame.height), rate)
    };
    let dir = app.paths().recordings_dir(system);
    let recorder = Recorder::start(mode, &rom, hz, ffmpeg_available(), format, rate, &dir)?;
    let note = recorder.started_note();
    SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        session.recorder = Some(recorder);
        session.rom = Some(rom);
        session.last_frame = 0;
    });
    app.show_notice(note, false);
    Ok(())
}

/// Stops the capture, finishes its files and puts the result in the bar.
fn stop(app: &mut App) {
    let recorder = SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        session.rom = None;
        session.recorder.take()
    });
    if let Some(recorder) = recorder {
        let note = recorder.stop();
        let error = note.starts_with("recording failed");
        app.show_notice(note, error);
    }
}

/// One per-frame hook: push each new machine frame into the open capture.
pub fn on_frame(app: &mut App) {
    let rom = app.rom().map(PathBuf::from);
    let active = SESSION.with(|cell| cell.borrow().recorder.is_some());
    if !active {
        return;
    }
    let stale = SESSION.with(|cell| cell.borrow().rom != rom);
    if app.machine().is_none() || stale {
        // Another game opened, or the game closed: the capture belongs to the old cartridge.
        stop(app);
        return;
    }
    let frames = app.machine().map_or(0, |machine| machine.frames());
    let last = SESSION.with(|cell| cell.borrow().last_frame);
    // A tick that ran no frame leaves the counter where it was. A counter that went backwards is a
    // rewind: the frames after it are new pictures, so the capture keeps going from there.
    if frames == last {
        return;
    }
    SESSION.with(|cell| cell.borrow_mut().last_frame = frames);
    let mut recorder = SESSION.with(|cell| cell.borrow_mut().recorder.take());
    if let Some(recorder) = recorder.as_mut()
        && let Some(machine) = app.machine_mut()
    {
        let frame = machine.frame();
        recorder.push_frame(frame.rgba);
        if recorder.wants_audio() {
            // `frame`'s borrow ends with the statement above, so the machine is free again for the
            // sample queue; one batch per frame is enough at every core's rate.
            let mut samples = [0f32; 8_192];
            while let Some(machine) = app.machine_mut() {
                let got = machine.drain_audio(&mut samples);
                if got == 0 {
                    break;
                }
                recorder.push_audio(&samples[..got * 2]);
                if got < samples.len() / 2 {
                    break;
                }
            }
        }
    }
    let finished = recorder.as_ref().is_some_and(Recorder::finished);
    if finished {
        let note = recorder.take().map(Recorder::stop).unwrap_or_default();
        SESSION.with(|cell| cell.borrow_mut().rom = None);
        app.show_notice(note.clone(), note.starts_with("recording failed"));
    } else {
        SESSION.with(|cell| cell.borrow_mut().recorder = recorder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder under the system temp dir, removed at the end.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tendyemu-record-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_wav_header_is_the_canonical_one() {
        // Three frames at 48 kHz: 12 bytes of samples, so a 48-byte file.
        let h = wav_header(3, 48_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 36 + 12);
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(h[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(h[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(h[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 192_000);
        assert_eq!(u16::from_le_bytes(h[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(h[34..36].try_into().unwrap()), 16);
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 12);
    }

    #[test]
    fn a_sample_is_clipped_not_wrapped() {
        assert_eq!(pcm16(0.0), 0);
        assert_eq!(pcm16(1.0), 32_767);
        assert_eq!(pcm16(-1.0), -32_767);
        assert_eq!(pcm16(2.0), 32_767);
        assert_eq!(pcm16(-2.0), -32_767);
        assert_eq!(pcm16(0.5), 16_384); // 16383.5 rounds away from zero
    }

    #[test]
    fn the_row_cycles_the_five_modes() {
        let mut mode = RecordMode::Off;
        let mut seen = Vec::new();
        for _ in 0..5 {
            mode = mode.step(true);
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                RecordMode::Gif,
                RecordMode::Wav,
                RecordMode::GifWav,
                RecordMode::Mp4,
                RecordMode::Off
            ]
        );
        let mut back = Vec::new();
        for _ in 0..5 {
            mode = mode.step(false);
            back.push(mode);
        }
        assert_eq!(
            back,
            [
                RecordMode::Mp4,
                RecordMode::GifWav,
                RecordMode::Wav,
                RecordMode::Gif,
                RecordMode::Off
            ]
        );
    }

    #[test]
    fn the_mp4_row_says_when_it_has_no_encoder() {
        assert_eq!(RecordMode::Mp4.label(true), "MP4");
        assert_eq!(RecordMode::Mp4.label(false), "MP4 (needs ffmpeg)");
        assert_eq!(RecordMode::GifWav.label(false), "GIF + WAV");
        // Every word round-trips, so a file written by this build reads back the same.
        for mode in [
            RecordMode::Off,
            RecordMode::Gif,
            RecordMode::Wav,
            RecordMode::GifWav,
            RecordMode::Mp4,
        ] {
            assert_eq!(RecordMode::from_name(mode.name()), Some(mode));
        }
    }

    #[test]
    fn the_cap_is_ten_minutes_of_frames_and_of_samples() {
        // 600 s at 59.73 Hz is 35 837 frames; one frame short is not the cap yet.
        let frames = cap_frames(MAX_SECONDS);
        assert_eq!(frames, 35_837);
        assert_eq!(cap_frames(1), 60);
        assert_eq!(cap_audio_frames(600, 48_000), 28_800_000);
        assert_eq!(cap_audio_frames(1, 96_000), 96_000);
    }

    #[test]
    fn a_capture_without_ffmpeg_is_refused_before_any_file() {
        let dir = scratch("refused");
        let result = Recorder::start_with_cap(
            RecordMode::Mp4,
            Path::new("Tetris.gb"),
            48_000,
            false,
            VideoFormat::new(160, 144),
            597_275,
            MAX_SECONDS,
            &dir,
        );
        let why = match result {
            Err(why) => why,
            Ok(_) => panic!("an MP4 capture must be refused without ffmpeg"),
        };
        assert!(why.contains("ffmpeg"), "{why}");
        assert!(!dir.join("Tetris-1.mp4").exists(), "no half-made file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
