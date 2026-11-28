//! AVI recording (DESIGN §6.1, §6.3): one uncompressed 24-bit `vids` stream and one 16-bit PCM
//! `auds` stream in a RIFF AVI 1.0 file, written under `<system>/recordings/<stem>-<n>.avi`.
//!
//! The layout is Microsoft's AVI RIFF File Reference: `RIFF`/`AVI ` -> `LIST hdrl` (`avih` plus one
//! `LIST strl` per stream, each `strh` + `strf`) -> `LIST movi` (`00db` video chunk then `01wb`
//! audio chunk per frame) -> `idx1`, the index that closes the file.
//!
//! The writer is pure. It goes through any [`Write`] + [`Seek`] — a `File` at runtime, a
//! `Cursor<Vec<u8>>` in the tests — and knows nothing about the emulator except the frame size and
//! rate in [`AviConfig`]. Frames arrive as the RGBA buffer every core's `Machine::frame` produces,
//! row 0 at the top; AVI wants bottom-up BGR with 4-byte-aligned rows, so [`AviWriter::write_frame`]
//! flips and converts on the way in.
//!
//! **Streaming.** The header is written once, up front, with the fields only known at the end
//! (`dwTotalFrames`, `dwMaxBytesPerSec`, each `strh.dwLength`, the `movi` and `RIFF` sizes) left
//! zero; [`AviWriter::finish`] appends `idx1` and seeks back to patch them. Nothing is buffered but
//! the 16-byte `idx1` entries, so a long recording costs no memory — and if the writer is dropped
//! without `finish`, `Drop` still closes the file, so a crash mid-recording leaves something a
//! player opens.
//!
//! **Size.** A RIFF size field is 32 bits, so the container cannot reach 4 GiB, and a 1 GiB
//! recording is already more than most players want to seek in. [`RIFF_LIMIT`] is the cap: one
//! 256x240 frame is 180 KiB of pixels, so about 11 MB/s and roughly 90 s of gameplay. A frame that
//! would push the file past the cap is refused with [`AviError::SizeLimit`] — nothing is written,
//! the refusal repeats for every later frame, and `finish` still ends the file with a valid index,
//! so the recording up to that point plays. The front end treats that refusal as "stop".

use std::cell::RefCell;
use std::fmt;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::app::App;
use crate::menu::{Row, RowAct, RowKind};
use crate::record;

/// The largest file this writer produces: 1 GiB, well inside what a 32-bit RIFF size can hold and
/// about 90 s of 256x240 gameplay. See the module comment.
pub const RIFF_LIMIT: u64 = 1 << 30;

/// Denominator of every frame-rate rational, so the header's `dwRate / dwScale` is exact.
pub const FPS_SCALE: u32 = 10_000;

/// `avih.dwFlags` bit 4: the file ends with an `idx1` chunk indexing every `movi` chunk.
const AVIF_HASINDEX: u32 = 0x10;

/// `idx1` entry flag: the chunk is a keyframe. Every uncompressed frame is one.
const AVIIF_KEYFRAME: u32 = 0x10;

/// `dwQuality` value meaning "default": the field is a 32-bit -1.
const QUALITY_DEFAULT: u32 = u32::MAX;

/// Bytes one 24-bit pixel row occupies, rounded up to the 4-byte boundary `BI_RGB` requires. A
/// 256-wide frame is 768 bytes and needs no padding; the rounding is here so an arbitrary width
/// (the tests use odd ones) still produces a legal bitmap.
pub fn bgr_stride(width: u32) -> u32 {
    (width * 3).div_ceil(4) * 4
}

/// What the file holds: pixel size, frame rate as the rational `rate / scale`, and audio format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AviConfig {
    pub width: u32,
    pub height: u32,
    /// Frames per second numerator: the machine's frame rate times [`FPS_SCALE`].
    pub rate: u32,
    /// Frames per second denominator: [`FPS_SCALE`].
    pub scale: u32,
    pub sample_rate: u32,
    /// 1 (mono) or 2 (stereo). The front end mixes stereo, so captures are stereo.
    pub channels: u16,
}

impl AviConfig {
    /// A config for a machine's `width` x `height` frame at `rate`/10 000 fps, with `sample_rate`
    /// and `channels` audio.
    pub fn new(width: u32, height: u32, rate: u32, sample_rate: u32, channels: u16) -> Self {
        Self {
            width,
            height,
            rate,
            scale: FPS_SCALE,
            sample_rate,
            channels,
        }
    }

    /// Bytes per audio sample frame: 16-bit samples, `channels` of them.
    pub fn block_align(&self) -> u32 {
        u32::from(self.channels) * 2
    }

    /// Payload bytes of one video frame, row padding included. Always even, because the stride is
    /// a multiple of four — which is why a `00db` chunk never needs a pad byte.
    pub fn frame_bytes(&self) -> u32 {
        bgr_stride(self.width) * self.height
    }

    /// Bytes one RGBA frame must have on the way in.
    pub fn rgba_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// `avih.dwMicroSecPerFrame`: the frame period in whole microseconds.
    pub fn micro_per_frame(&self) -> u32 {
        (u64::from(self.scale) * 1_000_000 / u64::from(self.rate)) as u32
    }

    fn validate(&self) -> Result<(), AviError> {
        if self.width == 0 || self.height == 0 {
            return Err(AviError::Config(
                "a frame cannot be zero pixels wide or tall",
            ));
        }
        if self.rate == 0 || self.scale == 0 {
            return Err(AviError::Config(
                "the frame rate needs a non-zero numerator and denominator",
            ));
        }
        if self.sample_rate == 0 {
            return Err(AviError::Config("the audio sample rate cannot be 0"));
        }
        if self.channels != 1 && self.channels != 2 {
            return Err(AviError::Config(
                "16-bit PCM audio is mono or stereo: channels must be 1 or 2",
            ));
        }
        if u64::from(self.frame_bytes()) > u64::from(u32::MAX) {
            return Err(AviError::Config("one frame must fit a 32-bit chunk size"));
        }
        Ok(())
    }
}

/// Why a write was refused. The only one a front end has to act on is [`AviError::SizeLimit`],
/// which means "stop recording and close the file".
#[derive(Debug)]
pub enum AviError {
    /// The underlying stream failed; the write position is unspecified.
    Io(io::Error),
    /// The frame's RGBA length is not `width * height * 4`.
    FrameSize { expected: usize, got: usize },
    /// A configuration the AVI headers cannot express.
    Config(&'static str),
    /// The audio sample count is not a whole number of sample frames for `channels`.
    AudioSamples { channels: u16, samples: usize },
    /// The next frame would push the file past the size cap. Nothing was written; `finish` still
    /// ends the file with a valid index.
    SizeLimit { limit: u64 },
    /// `write_frame` after the file was closed.
    Closed,
}

impl fmt::Display for AviError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AviError::Io(e) => write!(f, "AVI write failed: {e}"),
            AviError::FrameSize { expected, got } => {
                write!(f, "an RGBA frame must be {expected} bytes, got {got}")
            }
            AviError::Config(why) => write!(f, "cannot write this AVI: {why}"),
            AviError::AudioSamples { channels, samples } => write!(
                f,
                "{samples} audio samples is not a whole number of {channels}-channel sample frames"
            ),
            AviError::SizeLimit { limit } => {
                write!(
                    f,
                    "the recording reached the {limit}-byte cap; close it with finish()"
                )
            }
            AviError::Closed => write!(f, "the AVI file is already closed"),
        }
    }
}

impl std::error::Error for AviError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AviError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for AviError {
    fn from(e: io::Error) -> Self {
        AviError::Io(e)
    }
}

/// What a finished file holds, for the notice's "12.3 s" message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AviSummary {
    /// Frames written, the count `avih.dwTotalFrames` and the video `strh.dwLength` now carry.
    pub frames: u64,
    /// Length of the file in bytes, index included.
    pub bytes: u64,
    /// True when [`RIFF_LIMIT`] (or a lower cap) stopped the recording early.
    pub size_limit: bool,
}

/// Where the fields only known at close sit in the header block [`header`] builds.
#[derive(Clone, Copy, Debug)]
struct HeaderOffsets {
    /// `RIFF` size.
    riff_size: u64,
    /// `avih.dwTotalFrames`.
    total_frames: u64,
    /// `avih.dwMaxBytesPerSec`.
    max_bytes_per_sec: u64,
    /// Video `strh.dwLength`.
    video_length: u64,
    /// Video `strh.dwSuggestedBufferSize`.
    video_buffer: u64,
    /// Audio `strh.dwLength`.
    audio_length: u64,
    /// `LIST movi` size.
    movi_size: u64,
    /// The `movi` FOURCC itself: `idx1` offsets are measured from here, so the first chunk is at 4.
    movi: u64,
}

/// Append a chunk: FOURCC, 32-bit size, payload, and a pad byte when the payload is odd.
/// Returns where the payload starts.
fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) -> usize {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    let at = out.len();
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    at
}

/// Start a `LIST`: the eight-byte header plus the list type, then the children. Returns the offset
/// of the size field, for [`patch_size`].
fn push_list(out: &mut Vec<u8>, kind: &[u8; 4]) -> usize {
    out.extend_from_slice(b"LIST");
    let at = out.len();
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(kind);
    at
}

/// Fill in a LIST size from everything appended since its size field: the size counts the bytes
/// after itself, the list type included.
fn patch_size(out: &mut [u8], at: usize) {
    let size = (out.len() - at - 4) as u32;
    out[at..at + 4].copy_from_slice(&size.to_le_bytes());
}

/// `AVIMAINHEADER`, 56 bytes: the fields a player reads to find the frame size, rate and count.
fn main_header(config: &AviConfig) -> Vec<u8> {
    let mut h = Vec::with_capacity(56);
    h.extend_from_slice(&config.micro_per_frame().to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // dwMaxBytesPerSec, patched at close
    h.extend_from_slice(&0u32.to_le_bytes()); // dwPaddingGranularity: none needed
    h.extend_from_slice(&AVIF_HASINDEX.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // dwTotalFrames, patched at close
    h.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    h.extend_from_slice(&2u32.to_le_bytes()); // dwStreams: video, audio
    h.extend_from_slice(&config.frame_bytes().to_le_bytes());
    h.extend_from_slice(&config.width.to_le_bytes());
    h.extend_from_slice(&config.height.to_le_bytes());
    h.extend_from_slice(&[0u8; 16]); // dwReserved[4]
    h
}

/// `AVISTREAMHEADER`, 56 bytes. `dwLength` (offset 32) is patched at close.
fn stream_header(
    kind: &[u8; 4],
    handler: &[u8; 4],
    scale: u32,
    rate: u32,
    sample_size: u32,
    suggested_buffer: u32,
    frame: (u16, u16),
) -> Vec<u8> {
    let mut h = Vec::with_capacity(56);
    h.extend_from_slice(kind);
    h.extend_from_slice(handler);
    h.extend_from_slice(&0u32.to_le_bytes()); // dwFlags
    h.extend_from_slice(&0u16.to_le_bytes()); // wPriority
    h.extend_from_slice(&0u16.to_le_bytes()); // wLanguage
    h.extend_from_slice(&0u32.to_le_bytes()); // dwInitialFrames
    h.extend_from_slice(&scale.to_le_bytes());
    h.extend_from_slice(&rate.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // dwStart
    h.extend_from_slice(&0u32.to_le_bytes()); // dwLength, patched at close
    h.extend_from_slice(&suggested_buffer.to_le_bytes());
    h.extend_from_slice(&QUALITY_DEFAULT.to_le_bytes());
    h.extend_from_slice(&sample_size.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes()); // rcFrame.left
    h.extend_from_slice(&0u16.to_le_bytes()); // rcFrame.top
    h.extend_from_slice(&frame.0.to_le_bytes()); // rcFrame.right
    h.extend_from_slice(&frame.1.to_le_bytes()); // rcFrame.bottom
    h
}

/// `BITMAPINFOHEADER`, 40 bytes. A positive `biHeight` is what makes the rows bottom-up, matching
/// the BGR payloads [`AviWriter::write_frame`] writes.
fn bitmap_info(config: &AviConfig) -> Vec<u8> {
    let mut b = Vec::with_capacity(40);
    b.extend_from_slice(&40u32.to_le_bytes()); // biSize
    b.extend_from_slice(&(config.width as i32).to_le_bytes());
    b.extend_from_slice(&(config.height as i32).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    b.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    b.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB, no compression
    b.extend_from_slice(&config.frame_bytes().to_le_bytes()); // biSizeImage
    b.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    b.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    b.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    b.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    b
}

/// `WAVEFORMATEX`, 18 bytes, for 16-bit PCM. `cbSize` 0 is legal for PCM, which needs no extra
/// bytes beyond the 16 the fixed fields take.
fn wave_format(config: &AviConfig) -> Vec<u8> {
    let block_align = config.block_align();
    let mut w = Vec::with_capacity(18);
    w.extend_from_slice(&1u16.to_le_bytes()); // wFormatTag: WAVE_FORMAT_PCM
    w.extend_from_slice(&config.channels.to_le_bytes());
    w.extend_from_slice(&config.sample_rate.to_le_bytes());
    w.extend_from_slice(&(config.sample_rate * block_align).to_le_bytes()); // nAvgBytesPerSec
    w.extend_from_slice(&(block_align as u16).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes()); // wBitsPerSample
    w.extend_from_slice(&0u16.to_le_bytes()); // cbSize
    w
}

/// The whole file up to the first `movi` chunk: everything that is known before a frame is.
fn header(config: &AviConfig) -> (Vec<u8>, HeaderOffsets) {
    let mut b = Vec::with_capacity(512);
    b.extend_from_slice(b"RIFF");
    let riff_size = b.len();
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(b"AVI ");

    let hdrl = push_list(&mut b, b"hdrl");
    let avih = push_chunk(&mut b, b"avih", &main_header(config));
    let total_frames = avih + 16;
    let max_bytes_per_sec = avih + 4;

    let video = push_list(&mut b, b"strl");
    let strh = push_chunk(
        &mut b,
        b"strh",
        &stream_header(
            b"vids",
            b"DIB ",
            config.scale,
            config.rate,
            0, // uncompressed: sample size 0, one sample per chunk
            config.frame_bytes(),
            (config.width as u16, config.height as u16),
        ),
    );
    let video_length = strh + 32;
    let video_buffer = strh + 36;
    push_chunk(&mut b, b"strf", &bitmap_info(config));
    patch_size(&mut b, video);

    let audio = push_list(&mut b, b"strl");
    let block_align = config.block_align();
    let strh = push_chunk(
        &mut b,
        b"strh",
        &stream_header(
            b"auds",
            b"\0\0\0\0",
            block_align,
            config.sample_rate * block_align,
            block_align,
            block_align * 1_024, // one of the 1024-sample-frame buffers the front end hands over
            (0, 0),
        ),
    );
    let audio_length = strh + 32;
    push_chunk(&mut b, b"strf", &wave_format(config));
    patch_size(&mut b, audio);

    patch_size(&mut b, hdrl);

    b.extend_from_slice(b"LIST");
    let movi_size = b.len();
    b.extend_from_slice(&0u32.to_le_bytes());
    let movi = b.len();
    b.extend_from_slice(b"movi");

    (
        b,
        HeaderOffsets {
            riff_size: riff_size as u64,
            total_frames: total_frames as u64,
            max_bytes_per_sec: max_bytes_per_sec as u64,
            video_length: video_length as u64,
            video_buffer: video_buffer as u64,
            audio_length: audio_length as u64,
            movi_size: movi_size as u64,
            movi: movi as u64,
        },
    )
}

/// Fill `out` with `rgba` as bottom-up BGR: row `height - 1` first, each row `width * 3` bytes
/// followed by zero padding to [`bgr_stride`]. Alpha is dropped, which is the whole of the
/// RGBA-to-AVI conversion.
fn to_bgr(config: &AviConfig, rgba: &[u8], out: &mut [u8]) {
    let width = config.width as usize;
    let height = config.height as usize;
    let stride = bgr_stride(config.width) as usize;
    out.fill(0);
    for y in 0..height {
        let source = &rgba[(height - 1 - y) * width * 4..][..width * 4];
        let row = &mut out[y * stride..y * stride + width * 3];
        for x in 0..width {
            row[x * 3] = source[x * 4 + 2];
            row[x * 3 + 1] = source[x * 4 + 1];
            row[x * 3 + 2] = source[x * 4];
        }
    }
}

/// A streaming AVI 1.0 writer: [`AviWriter::write_frame`] per emulated frame,
/// [`AviWriter::finish`] once at the end.
pub struct AviWriter<W: Write + Seek> {
    out: W,
    config: AviConfig,
    /// File length cap in bytes: [`RIFF_LIMIT`] unless lowered by [`AviWriter::with_limit`].
    limit: u64,
    /// Absolute offset the next appended byte lands at. Tracked here rather than read back from
    /// the stream, because `finish` seeks around to patch and the cursor is not the length then.
    pos: u64,
    frames: u64,
    /// Audio payload bytes written into `01wb` chunks, for the audio stream's `dwLength`.
    audio_bytes: u64,
    /// Largest per-frame payload (video + audio), for `avih.dwMaxBytesPerSec`.
    largest_frame: u32,
    /// `idx1` entries, 16 bytes each, appended as chunks are written.
    index: Vec<u8>,
    offsets: HeaderOffsets,
    /// Bottom-up BGR scratch for one frame, so a frame write allocates nothing.
    frame: Vec<u8>,
    /// Little-endian scratch for one frame's audio, likewise.
    audio: Vec<u8>,
    closed: bool,
    size_limit: bool,
}

impl<W: Write + Seek> AviWriter<W> {
    /// Start a recording: writes the header with the frame count and sizes left open.
    pub fn new(out: W, config: AviConfig) -> Result<Self, AviError> {
        Self::with_limit(out, config, RIFF_LIMIT)
    }

    /// The same with a lower size cap. The cap exists so the limit path is testable without
    /// recording a gigabyte; a front end wants [`AviWriter::new`].
    pub fn with_limit(out: W, config: AviConfig, limit: u64) -> Result<Self, AviError> {
        config.validate()?;
        let (prefix, offsets) = header(&config);
        let mut writer = Self {
            out,
            config,
            limit,
            pos: 0,
            frames: 0,
            audio_bytes: 0,
            largest_frame: 0,
            index: Vec::new(),
            offsets,
            frame: vec![0; config.frame_bytes() as usize],
            audio: Vec::new(),
            closed: false,
            size_limit: false,
        };
        writer.put(&prefix)?;
        Ok(writer)
    }

    /// The format this file carries.
    pub fn config(&self) -> &AviConfig {
        &self.config
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes put on the stream so far, the header included: the offset of the last chunk.
    pub fn bytes(&self) -> u64 {
        self.pos
    }

    /// True once a frame has been refused by the size cap. Every later frame is refused too.
    pub fn size_limit_reached(&self) -> bool {
        self.size_limit
    }

    /// Append one frame: the RGBA buffer `width * height * 4` bytes with row 0 at the top, then
    /// `samples` interleaved 16-bit PCM samples for the same frame (empty when the front end has
    /// none to hand over, in which case the frame gets no `01wb` chunk).
    ///
    /// A frame that would take the file past the cap writes nothing and returns
    /// [`AviError::SizeLimit`]; `finish` is still fine and still produces a valid index.
    pub fn write_frame(&mut self, rgba: &[u8], samples: &[i16]) -> Result<(), AviError> {
        if self.closed {
            return Err(AviError::Closed);
        }
        let expected = self.config.rgba_bytes();
        if rgba.len() != expected {
            return Err(AviError::FrameSize {
                expected,
                got: rgba.len(),
            });
        }
        let channels = usize::from(self.config.channels);
        if !samples.len().is_multiple_of(channels) {
            return Err(AviError::AudioSamples {
                channels: self.config.channels,
                samples: samples.len(),
            });
        }

        // Both payloads are even (the BGR stride is a multiple of four, 16-bit samples are two
        // bytes), so no chunk needs a pad byte; the costs below are the whole cost.
        let frame_bytes = u64::from(self.config.frame_bytes());
        let audio_bytes = (samples.len() * 2) as u64;
        let video_chunk = 8 + frame_bytes;
        let audio_chunk = if samples.is_empty() {
            0
        } else {
            8 + audio_bytes
        };
        // The `idx1` chunk `finish` appends: an 8-byte header plus a 16-byte entry for every
        // `movi` chunk written so far, this frame's included.
        let index = 8 + self.index.len() as u64 + 16 * if samples.is_empty() { 1 } else { 2 };
        if self.pos + video_chunk + audio_chunk + index > self.limit {
            self.size_limit = true;
            return Err(AviError::SizeLimit { limit: self.limit });
        }

        let mut frame = std::mem::take(&mut self.frame);
        to_bgr(&self.config, rgba, &mut frame);
        let written = self.put_chunk(b"00db", &frame);
        self.frame = frame;
        written?;

        if !samples.is_empty() {
            let mut audio = std::mem::take(&mut self.audio);
            audio.clear();
            audio.reserve(samples.len() * 2);
            for sample in samples {
                audio.extend_from_slice(&sample.to_le_bytes());
            }
            let written = self.put_chunk(b"01wb", &audio);
            self.audio = audio;
            written?;
            self.audio_bytes += audio_bytes;
        }

        self.frames += 1;
        self.largest_frame = self.largest_frame.max((frame_bytes + audio_bytes) as u32);
        Ok(())
    }

    /// Append `idx1`, patch the header fields that were only known now, and flush. Idempotent:
    /// `Drop` calls it for a writer that is never finished, and a second call does nothing.
    pub fn finish(mut self) -> Result<AviSummary, AviError> {
        self.close()?;
        Ok(AviSummary {
            frames: self.frames,
            bytes: self.pos,
            size_limit: self.size_limit,
        })
    }

    /// Write one `movi` chunk and remember its `idx1` entry.
    fn put_chunk(&mut self, id: &[u8; 4], data: &[u8]) -> Result<(), AviError> {
        let offset = (self.pos - self.offsets.movi) as u32;
        self.put(id)?;
        self.put(&(data.len() as u32).to_le_bytes())?;
        self.put(data)?;
        if data.len() % 2 == 1 {
            self.put(&[0])?;
        }
        self.index.extend_from_slice(id);
        self.index.extend_from_slice(&AVIIF_KEYFRAME.to_le_bytes());
        self.index.extend_from_slice(&offset.to_le_bytes());
        self.index
            .extend_from_slice(&(data.len() as u32).to_le_bytes());
        Ok(())
    }

    /// Append bytes, keeping [`AviWriter::pos`] equal to the file length.
    fn put(&mut self, bytes: &[u8]) -> Result<(), AviError> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    /// Write one 32-bit field back in the header, then return the cursor to the end.
    fn poke(&mut self, at: u64, value: u32) -> Result<(), AviError> {
        self.out.seek(SeekFrom::Start(at))?;
        self.out.write_all(&value.to_le_bytes())?;
        self.out.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }

    fn close(&mut self) -> Result<(), AviError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;

        let index = std::mem::take(&mut self.index);
        self.put(b"idx1")?;
        self.put(&(index.len() as u32).to_le_bytes())?;
        self.put(&index)?;

        let end = self.pos;
        let index_start = end - 8 - index.len() as u64;
        let block_align = u64::from(self.config.block_align());
        let frames = self.frames as u32;
        let audio_length = (self.audio_bytes / block_align) as u32;
        let max_bytes_per_sec = ((u64::from(self.largest_frame) * u64::from(self.config.rate))
            / u64::from(self.config.scale)) as u32;
        // `idx1` offsets and the two sizes are measured from the `movi` FOURCC.
        let movi_size = (index_start - (self.offsets.movi_size + 4)) as u32;
        let riff_size = (end - 8) as u32;

        self.poke(self.offsets.total_frames, frames)?;
        self.poke(self.offsets.video_length, frames)?;
        self.poke(self.offsets.video_buffer, self.config.frame_bytes())?;
        self.poke(self.offsets.audio_length, audio_length)?;
        self.poke(self.offsets.max_bytes_per_sec, max_bytes_per_sec)?;
        self.poke(self.offsets.movi_size, movi_size)?;
        self.poke(self.offsets.riff_size, riff_size)?;
        self.out.flush()?;
        Ok(())
    }
}

impl<W: Write + Seek> Drop for AviWriter<W> {
    /// A recording that is never finished still gets a valid index, so a front end that drops the
    /// writer mid-run (or panics) leaves a playable file instead of an unpatchable header.
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// The capture running, if any: the writer and the file it is making.
struct Recording {
    writer: AviWriter<File>,
    path: PathBuf,
}

/// The session's capture and the cartridge it belongs to.
#[derive(Default)]
struct Session {
    recording: Option<Recording>,
    rom: Option<PathBuf>,
    /// The machine's frame counter at the last frame pushed.
    last_frame: u64,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

/// The AVI row: start or stop.
pub fn rows(app: &App) -> Vec<Row> {
    if app.machine().is_none() {
        return Vec::new();
    }
    let recording = SESSION.with(|cell| cell.borrow().recording.is_some());
    vec![Row {
        label: if recording {
            "Stop AVI recording"
        } else {
            "Start AVI recording"
        }
        .to_string(),
        value: String::new(),
        kind: RowKind::Action,
        desc: "Uncompressed AVI with sound, up to one gigabyte",
        hotkey: "",
        systems: crate::menu::Systems::ALL,
        requires: None,
        act: None,
    }]
}

/// Enter on the row starts the capture, or stops the one running.
pub fn on_row(app: &mut App, index: usize, act: RowAct) -> bool {
    if index != 0 || act != RowAct::Activate {
        return false;
    }
    if SESSION.with(|cell| cell.borrow().recording.is_some()) {
        stop(app);
    } else if let Err(why) = start(app) {
        app.show_notice(format!("AVI recording not started: {why}"), true);
    }
    true
}

/// Starts an AVI capture of the running game.
fn start(app: &mut App) -> Result<(), String> {
    let system = app.system().ok_or("no game is running")?;
    let rom = app
        .rom()
        .map(Path::to_path_buf)
        .ok_or("the game was not opened from a file, so it has no recording name")?;
    let hz = app.sample_rate();
    let (width, height, rate) = {
        let machine = app.machine_mut().ok_or("no game is running")?;
        let rate = (machine.frame_rate() * f64::from(FPS_SCALE)).round() as u32;
        let frame = machine.frame();
        (frame.width, frame.height, rate)
    };
    let dir = app.paths().recordings_dir(system);
    std::fs::create_dir_all(&dir)
        .map_err(|why| format!("cannot create {}: {why}", dir.display()))?;
    let index = record::next_index(&rom, &dir);
    let stem = rom.file_stem().map_or_else(
        || "recording".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let path = dir.join(format!("{stem}-{index}.avi"));
    let file =
        File::create(&path).map_err(|why| format!("cannot write {}: {why}", path.display()))?;
    let config = AviConfig::new(width, height, rate, hz, 2);
    let writer = AviWriter::new(file, config)
        .map_err(|why| format!("cannot write {}: {why}", path.display()))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        session.recording = Some(Recording {
            writer,
            path: path.clone(),
        });
        session.rom = Some(rom);
        session.last_frame = 0;
    });
    app.show_notice(format!("recording {name}"), false);
    Ok(())
}

/// Stops the capture, closes the file and puts the result in the bar.
fn stop(app: &mut App) {
    let recording = SESSION.with(|cell| {
        let mut session = cell.borrow_mut();
        session.rom = None;
        session.recording.take()
    });
    if let Some(recording) = recording {
        finish(app, recording);
    }
}

/// Close `recording`'s writer and show its summary, or the failure.
fn finish(app: &mut App, recording: Recording) {
    let name = recording.path.file_name().map_or_else(
        || recording.path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    match recording.writer.finish() {
        Ok(summary) => {
            let note = if summary.size_limit {
                format!(
                    "AVI stopped at {} bytes: {name} ({} frames)",
                    summary.bytes, summary.frames
                )
            } else {
                format!("AVI saved {name} ({} frames)", summary.frames)
            };
            app.show_notice(note, false);
        }
        Err(why) => app.show_notice(format!("AVI not saved: {why}"), true),
    }
}

/// One per-frame hook: write the frame and its samples into the open capture.
pub fn on_frame(app: &mut App) {
    let rom = app.rom().map(PathBuf::from);
    let active = SESSION.with(|cell| cell.borrow().recording.is_some());
    if !active {
        return;
    }
    let stale = SESSION.with(|cell| cell.borrow().rom != rom);
    if app.machine().is_none() || stale {
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
    // The samples for this frame, converted once and shared by the video chunk's audio chunk.
    let mut samples = [0f32; 8_192];
    let mut pcm = Vec::new();
    if let Some(machine) = app.machine_mut() {
        loop {
            let got = machine.drain_audio(&mut samples);
            if got == 0 {
                break;
            }
            pcm.extend(samples[..got * 2].iter().map(|s| record::pcm16(*s)));
            if got < samples.len() / 2 {
                break;
            }
        }
    }
    let mut recording = SESSION.with(|cell| cell.borrow_mut().recording.take());
    let mut stop_now = false;
    if let Some(rec) = recording.as_mut()
        && let Some(machine) = app.machine_mut()
    {
        let frame = machine.frame();
        match rec.writer.write_frame(frame.rgba, &pcm) {
            Ok(()) => {}
            // The cap is the one refusal that leaves a good file: stop and close it.
            Err(AviError::SizeLimit { .. }) => stop_now = true,
            Err(why) => {
                app.show_notice(format!("AVI recording failed: {why}"), true);
                stop_now = true;
            }
        }
    }
    if stop_now {
        if let Some(rec) = recording.take() {
            finish(app, rec);
        }
    } else {
        SESSION.with(|cell| cell.borrow_mut().recording = recording);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config for a 16x8 capture at 60 fps, 48 kHz mono: small and easy to reason about.
    fn small() -> AviConfig {
        AviConfig::new(16, 8, 600_000, 48_000, 1)
    }

    #[test]
    fn stride_rounds_up_to_four() {
        assert_eq!(bgr_stride(256), 768);
        assert_eq!(bgr_stride(16), 48);
        assert_eq!(bgr_stride(15), 48);
        assert_eq!(bgr_stride(1), 4);
    }

    #[test]
    fn frame_rate_rationals() {
        let config = AviConfig::new(256, 240, 600_988, 44_100, 1);
        assert_eq!((config.rate, config.scale), (600_988, 10_000));
        assert_eq!(config.micro_per_frame(), 16_639);
        assert_eq!(config.frame_bytes(), 184_320);
        assert_eq!(config.rgba_bytes(), 245_760);

        let pal = AviConfig::new(256, 240, 500_070, 44_100, 1);
        assert_eq!(pal.micro_per_frame(), 19_997);
    }

    #[test]
    fn config_rejects_what_the_headers_cannot_express() {
        let base = small();
        for bad in [
            AviConfig { width: 0, ..base },
            AviConfig { height: 0, ..base },
            AviConfig { rate: 0, ..base },
            AviConfig { scale: 0, ..base },
            AviConfig {
                channels: 3,
                ..base
            },
            AviConfig {
                channels: 0,
                ..base
            },
            AviConfig {
                sample_rate: 0,
                ..base
            },
        ] {
            let cursor = std::io::Cursor::new(Vec::new());
            assert!(
                matches!(AviWriter::new(cursor, bad), Err(AviError::Config(_))),
                "{bad:?} must be refused"
            );
        }
    }

    #[test]
    fn the_header_names_two_streams_at_the_frame_size() {
        let config = AviConfig::new(16, 8, 600_000, 48_000, 2);
        let (bytes, offsets) = header(&config);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"AVI ");
        for fourcc in [
            b"hdrl".as_slice(),
            b"avih",
            b"strh",
            b"strf",
            b"vids",
            b"auds",
        ] {
            assert!(
                bytes.windows(4).any(|window| window == fourcc),
                "{} missing",
                String::from_utf8_lossy(fourcc)
            );
        }
        assert_eq!(offsets.movi, bytes.len() as u64 - 4);
        assert_eq!(&bytes[bytes.len() - 4..], b"movi");
    }

    #[test]
    fn a_finished_file_records_its_frame_count() {
        let config = small();
        let cursor = std::io::Cursor::new(Vec::new());
        let mut writer = AviWriter::new(cursor, config).expect("the config is expressible");
        let rgba = vec![0u8; config.rgba_bytes()];
        writer
            .write_frame(&rgba, &[0, 1, -1, 2])
            .expect("the frame fits");
        let summary = writer.finish().expect("the index is written");
        assert_eq!(summary.frames, 1);
        assert!(summary.bytes > 0);
        assert!(!summary.size_limit);
    }
}
