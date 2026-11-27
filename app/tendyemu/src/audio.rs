//! Sound: one output stream, whichever core runs (DESIGN §6.1, §6.3; issue #19).
//!
//! Two halves. [`Mixer`] is headless: it drains whatever core is running, applies the volume,
//! converts the core's rate to the device's and queues the result in the [`SampleRing`]. The
//! device half is one cpal stream, opened on the first frame of the live window and held for the
//! whole run, so switching systems only retunes the mixer and never restarts the stream
//! (§6.4 steps 2 and 5). `negotiate_rate` asks the card for the rate the `audio_rate` setting
//! names and never panics when the card says no.
//!
//! Nothing here opens a sound card unless the window loop is live ([`crate::file_pick`]), so a
//! library caller and a `cargo test` run stay silent. With no card the core's queue is still
//! drained every frame, so it cannot grow without bound.
//!
//! The ring, the rate converter and the device open are ported from gb's front end (gb-rust
//! 449154a) minus its linear-interpolation baseline, which TendyEmu never selects.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use tendy_core::{Machine, SystemKind};

use crate::app::App;
use crate::menu::{Row, RowAct};

/// The rate a machine is told to produce at until a device answers with its own.
pub const SAMPLE_RATE: u32 = 48_000;

/// Stereo frames the ring holds: about 0.37 s at 44.1 kHz, enough for one display frame of
/// scheduling jitter at any rate.
pub const RING_CAPACITY: usize = 16_384;

/// Taps of the sinc kernel when the machine's rate is at or below the card's.
pub const SINC_TAPS: usize = 32;
/// Fractional positions per source frame interval: one coefficient row each.
pub const SINC_PHASES: usize = 512;
/// The kernel's cutoff as a fraction of the lower of the two Nyquist frequencies.
pub const SINC_CUTOFF: f64 = 0.92;
/// The longest kernel a deep decimation may build (a quarter of the source rate).
pub const SINC_TAPS_MAX: usize = 128;

/// The most the fill controller may stretch the source rate: ±0.5 % is 8.6 cents, inaudible.
pub const TRIM_MAX: f32 = 0.005;
/// How hard the fill controller leans: 4 % of the fill error saturates the trim.
pub const FILL_GAIN: f64 = 0.04;

/// No rows yet: the volume and rate rows need `App::setting`, which is #21's.
pub fn rows(_app: &App) -> Vec<Row> {
    Vec::new()
}

/// No audio row does anything yet.
pub fn on_row(_app: &mut App, _index: usize, _act: RowAct) -> bool {
    false
}

// ------------------------------------------------------------------------------------- the ring

/// A lock-protected interleaved stereo queue shared by the producer and the device callback.
#[derive(Clone, Debug, Default)]
pub struct SampleRing {
    data: Arc<Mutex<VecDeque<f32>>>,
}

impl SampleRing {
    pub fn new() -> SampleRing {
        SampleRing::default()
    }

    /// Appends interleaved L/R samples; a trailing odd sample is ignored.
    pub fn push_stereo(&self, lr: &[f32]) {
        if let Ok(mut q) = self.data.lock() {
            let pairs = lr.len() / 2;
            for i in 0..pairs {
                q.push_back(lr[i * 2]);
                q.push_back(lr[i * 2 + 1]);
            }
            while q.len() > RING_CAPACITY * 2 {
                q.pop_front();
            }
        }
    }

    /// Pops up to `out.len()` samples; returns how many were written.
    pub fn pop_into(&self, out: &mut [f32]) -> usize {
        let Ok(mut q) = self.data.lock() else {
            return 0;
        };
        let n = out.len().min(q.len());
        for slot in out.iter_mut().take(n) {
            *slot = q.pop_front().unwrap_or(0.0);
        }
        n
    }

    /// Pops what the ring holds and **zero-fills the rest**: an underrun is silence, never a
    /// repeat of the previous chunk.
    pub fn pop_or_zero(&self, out: &mut [f32]) -> usize {
        let n = self.pop_into(out);
        out[n..].fill(0.0);
        n
    }

    /// Forgets every queued frame, keeping the buffer's capacity.
    pub fn clear(&self) {
        if let Ok(mut q) = self.data.lock() {
            q.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.data.lock().map(|q| q.len()).unwrap_or(0) / 2
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `volume` 0-100 -> gain `(v/100)^2`, squared so the top half of the range is audible.
pub fn volume_gain(volume: u8) -> f32 {
    let v = f32::from(volume.min(100)) / 100.0;
    v * v
}

// ------------------------------------------------------------------------------- the converter

/// A Blackman-windowed sinc lowpass, precomputed once per converter into a phase table.
///
/// Row `p` holds the taps for the fractional position `p / SINC_PHASES` between two source
/// frames; every row is normalised to unity DC gain, so a constant stream stays constant at
/// every phase.
#[derive(Clone, Debug)]
struct Kernel {
    taps: usize,
    table: Vec<f32>,
}

impl Kernel {
    fn new(from_hz: u32, to_hz: u32) -> Kernel {
        let ratio = (f64::from(to_hz) / f64::from(from_hz)).min(1.0);
        let mut taps = if ratio >= 1.0 {
            SINC_TAPS
        } else {
            // An even count: a symmetric window needs whole pairs.
            ((SINC_TAPS as f64 / ratio).ceil() as usize + 1) & !1
        };
        taps = taps.clamp(2, SINC_TAPS_MAX);
        let half = taps / 2;
        let cutoff = 0.5 * SINC_CUTOFF * ratio;
        let mut table = vec![0.0f32; (SINC_PHASES + 1) * taps];
        for p in 0..=SINC_PHASES {
            let frac = p as f64 / SINC_PHASES as f64;
            let row = &mut table[p * taps..(p + 1) * taps];
            let mut sum = 0.0;
            for (j, slot) in row.iter_mut().enumerate() {
                let t = j as f64 - (half as f64 - 1.0) - frac;
                let value = windowed_sinc(t, half as f64, cutoff);
                *slot = value as f32;
                sum += value;
            }
            if sum != 0.0 {
                let norm = (1.0 / sum) as f32;
                for slot in row.iter_mut() {
                    *slot *= norm;
                }
            }
        }
        Kernel { taps, table }
    }

    /// One polyphase evaluation: the interleaved window `window` (taps long, tap 0 first) at the
    /// fractional position `frac`, both channels.
    fn apply(&self, window: &[[f32; 2]], frac: f32) -> (f32, f32) {
        let pf = frac * SINC_PHASES as f32;
        let p0 = (pf as usize).min(SINC_PHASES - 1);
        let alpha = pf - p0 as f32;
        let a = &self.table[p0 * self.taps..(p0 + 1) * self.taps];
        let b = &self.table[(p0 + 1) * self.taps..(p0 + 2) * self.taps];
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for (j, frame) in window.iter().enumerate() {
            let h = a[j] + (b[j] - a[j]) * alpha;
            left += frame[0] * h;
            right += frame[1] * h;
        }
        (left, right)
    }
}

/// The ideal lowpass `2 f sin(π f t)/(π f t)` times a Blackman window over `|t| < half`.
fn windowed_sinc(t: f64, half: f64, cutoff: f64) -> f64 {
    if t.abs() >= half {
        return 0.0;
    }
    let arg = 2.0 * cutoff * t;
    let sinc = if arg == 0.0 {
        1.0
    } else {
        (PI * arg).sin() / (PI * arg)
    };
    let window = 0.42 + 0.5 * (PI * t / half).cos() + 0.08 * (2.0 * PI * t / half).cos();
    2.0 * cutoff * sinc * window
}

/// A stateful resampler from one rate to another, interleaved stereo in and out.
///
/// It is stateful because the core hands over one emulated frame's batch at a time: the
/// fractional position and the source frames around it carry across calls, so the seam between
/// two batches neither repeats nor drops a frame. A converter whose two rates are equal is a
/// straight copy.
#[derive(Clone, Debug)]
pub struct Converter {
    from: u32,
    to: u32,
    /// The phase table, `None` on a straight copy.
    kernel: Option<Kernel>,
    /// The source frames retained across the seam. Two kernel widths, so the next output's whole
    /// window is inside them after a batch ends.
    prev: Vec<[f32; 2]>,
    /// The retained frames plus this batch, rebuilt in place each call.
    scratch: Vec<[f32; 2]>,
    /// Position of the next output frame, in source frames after the start of the last batch.
    pos: f64,
    /// False until the first batch anchors the history on real audio.
    seeded: bool,
    /// The fill controller's stretch of the source rate: `1.0` is exact, `1.005` produces 0.5 %
    /// more frames than the two rates ask for. This is how the queue follows a card whose crystal
    /// is not the console's, without the difference ever becoming an underrun.
    trim: f32,
}

impl Converter {
    /// A converter from `from_hz` to `to_hz`; the kernel is built here, once.
    pub fn new(from_hz: u32, to_hz: u32) -> Converter {
        let from = from_hz.max(1);
        let to = to_hz.max(1);
        Converter {
            from,
            to,
            kernel: (from != to).then(|| Kernel::new(from, to)),
            prev: Vec::new(),
            scratch: Vec::new(),
            pos: 0.0,
            seeded: false,
            trim: 1.0,
        }
    }

    /// The source rate the converter is built for.
    pub fn from_hz(&self) -> u32 {
        self.from
    }

    /// The rate it produces.
    pub fn to_hz(&self) -> u32 {
        self.to
    }

    /// The fill controller's current stretch of the source rate.
    pub fn trim(&self) -> f32 {
        self.trim
    }

    /// Stretches the source rate by `trim` (`1.0` is exact): `1.005` produces 0.5 % more frames.
    /// The caller keeps it inside a few cents, so the pitch never moves enough to hear.
    pub fn set_trim(&mut self, trim: f32) {
        let trim = trim.clamp(1.0 - TRIM_MAX, 1.0 + TRIM_MAX);
        if (trim - self.trim).abs() < 1e-6 {
            return;
        }
        self.trim = trim;
        // A straight copy can only be stretched if there is a kernel to stretch it with.
        if self.kernel.is_none() && (trim - 1.0).abs() > 1e-6 {
            self.kernel = Some(Kernel::new(self.from, self.to));
        }
    }

    /// Whether the two rates are equal and the trim is exact, so [`convert`](Self::convert) is a
    /// straight copy.
    pub fn is_identity(&self) -> bool {
        self.kernel.is_none()
    }

    /// Forgets the seam: no held frames, no fractional position, unseeded. The next batch anchors
    /// the history on its own first frame, so a rate change restarts on real audio.
    pub fn reset(&mut self) {
        self.prev.clear();
        self.pos = 0.0;
        self.seeded = false;
    }

    /// Convert one batch of interleaved stereo samples, replacing `out`. A trailing odd sample is
    /// ignored. The state carries the seam between calls.
    pub fn convert(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let Some(kernel) = self.kernel.as_ref() else {
            out.extend_from_slice(&input[..frames * 2]);
            return;
        };
        let taps = kernel.taps;
        let half = taps / 2;
        if !self.seeded {
            // Anchor the history a whole kernel wide on the first frame, so a constant stream is
            // exactly constant from sample zero instead of ramping in.
            let first = [input[0], input[1]];
            self.prev.clear();
            self.prev.resize(taps, first);
            self.pos = 0.0;
            self.seeded = true;
        }
        let prev_len = self.prev.len();
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.prev);
        self.scratch
            .extend(input[..frames * 2].as_chunks::<2>().0.iter().copied());
        let total = self.scratch.len();
        let step = f64::from(self.from) / (f64::from(self.to) * f64::from(self.trim));
        // `pos` is the next output's position relative to the batch start; the batch starts
        // `prev_len` frames into the scratch.
        let mut x = prev_len as f64 + self.pos;
        // Emit while the whole window exists: the first tap reads a frame the scratch holds and
        // the last one too.
        while let Some(base) = window_start(x, half, total) {
            let frac = (x - x.floor()) as f32;
            let (left, right) = kernel.apply(&self.scratch[base..base + taps], frac);
            out.push(left);
            out.push(right);
            x += step;
        }
        // Retain two kernel widths: the next output may sit inside the tail of this batch, and its
        // own window reaches a further `half` frames back.
        let keep = taps.min(total);
        self.prev.clear();
        self.prev.extend_from_slice(&self.scratch[total - keep..]);
        self.pos = x - total as f64;
    }
}

/// The frame the sinc window's first tap reads for an output at `x`, or `None` when the window
/// would run off either end of a scratch of `total` frames.
fn window_start(x: f64, half: usize, total: usize) -> Option<usize> {
    let k = x.floor() as i64;
    let base = k - (half as i64 - 1);
    let last = k + half as i64;
    if base < 0 || last > total as i64 - 1 {
        return None;
    }
    Some(base as usize)
}

// ------------------------------------------------------------------------------------- the mix

/// The core's samples, at the device's rate, in the device's queue.
///
/// One mixer lives for the whole run: a system switch changes the machine and the machine's rate,
/// and [`push`](Self::push) rebuilds the converter when it sees a new rate, so the queue itself
/// survives the switch (§6.4).
pub struct Mixer {
    ring: SampleRing,
    device_hz: u32,
    converter: Converter,
    /// One drain's worth of the core's own samples, interleaved.
    buffer: Vec<f32>,
    /// The converted batch, interleaved.
    resampled: Vec<f32>,
    /// How many times the converter was rebuilt for a new source rate.
    retunes: u32,
}

impl Mixer {
    /// A mixer feeding a queue at `device_hz`.
    pub fn new(device_hz: u32) -> Mixer {
        Mixer::with_ring(SampleRing::new(), device_hz)
    }

    /// A mixer feeding `ring`: the device callback already holds a clone of it.
    pub fn with_ring(ring: SampleRing, device_hz: u32) -> Mixer {
        let device_hz = device_hz.max(1);
        Mixer {
            ring,
            device_hz,
            converter: Converter::new(device_hz, device_hz),
            buffer: vec![0.0; 8_192],
            resampled: Vec::new(),
            retunes: 0,
        }
    }

    /// The rate the queue is filled at.
    pub fn device_hz(&self) -> u32 {
        self.device_hz
    }

    /// The rate the converter is built for: the last rate the core reported.
    pub fn source_hz(&self) -> u32 {
        self.converter.from_hz()
    }

    /// How many times a new source rate rebuilt the converter.
    pub fn retunes(&self) -> u32 {
        self.retunes
    }

    /// The queue the device callback drains.
    pub fn ring(&self) -> &SampleRing {
        &self.ring
    }

    /// Drains everything the machine has queued, scales it by `gain`, converts it to the device's
    /// rate and pushes it. Returns the frames queued.
    ///
    /// The source rate is the rate the machine really queues at ([`source_hz`]), and a change in
    /// it rebuilds the converter rather than the stream: that is what a system switch costs.
    pub fn push(&mut self, machine: &mut dyn Machine, gain: f32) -> usize {
        let source = source_hz(machine);
        if source != self.converter.from_hz() {
            self.converter = Converter::new(source, self.device_hz);
            self.retunes += 1;
        }
        self.trim_for_fill();
        let mut pushed = 0;
        loop {
            let frames = machine.drain_audio(&mut self.buffer);
            if frames == 0 {
                break;
            }
            let source = &mut self.buffer[..frames * 2];
            if gain != 1.0 {
                for sample in source.iter_mut() {
                    *sample *= gain;
                }
            }
            self.converter.convert(source, &mut self.resampled);
            self.ring.push_stereo(&self.resampled);
            pushed += self.resampled.len() / 2;
        }
        pushed
    }

    /// Leans on the converter's rate so the queue stays at [`PREFILL_FRAMES`].
    ///
    /// A card whose crystal is a fraction of a percent off the console's drains the queue or
    /// overfills it; at 0.3 s per hour the queue would empty in minutes. Here the trim gives that
    /// fraction back, and the queue stops moving (§6.4; snes's fill controller, one proportional
    /// term). Silence is still a pause, not a machine running a fraction of a percent slow.
    fn trim_for_fill(&mut self) {
        let fill = self.ring.len() as f64;
        let error = (fill - PREFILL_FRAMES as f64) / PREFILL_FRAMES as f64;
        let limit = f64::from(TRIM_MAX);
        let trim = (1.0 - FILL_GAIN * error).clamp(1.0 - limit, 1.0 + limit);
        self.converter.set_trim(trim as f32);
    }

    /// The fill controller's current stretch of the source rate.
    pub fn trim(&self) -> f32 {
        self.converter.trim()
    }
}

/// The rate a machine really queues at.
///
/// Most cores honour [`set_sample_rate`](tendy_core::Machine::set_sample_rate) and report what
/// they were told. The SNES DSP cannot: it takes one sample every 32 SPC cycles, 32 kHz on its
/// fixed 1.024 MHz clock, whatever the request — and `sample_rate()` then reports the request, not
/// the console. So for that one the hardware's own rate is the truth (DESIGN §6.1: a converter
/// from any core rate).
pub fn source_hz(machine: &dyn Machine) -> u32 {
    match machine.kind() {
        SystemKind::Snes => SNES_DSP_HZ,
        _ => machine.sample_rate().max(1),
    }
}

/// The SNES DSP's sample clock: the SPC700's 1.024 MHz divided by 32.
pub const SNES_DSP_HZ: u32 = 32_000;

// ---------------------------------------------------------------------------------- the device

/// The one live output: the stream, held for the whole run, and the mixer feeding it.
struct Live {
    /// Kept only so the stream keeps playing: dropping it stops the sound.
    stream: Option<cpal::Stream>,
    mixer: Mixer,
    /// False until the queue holds [`PREFILL_FRAMES`]: a stream that starts on an empty queue
    /// underruns on every callback until the first display frame lands.
    playing: bool,
}

impl Drop for Live {
    /// Lets the stream die with the process instead of in a TLS destructor.
    ///
    /// cpal's WASAPI backend keeps a thread-local device monitor whose `Drop` runs after the
    /// thread's TLS is already torn down, which aborts the process when a stream is dropped during
    /// thread exit (seen here in a test binary). A stream is process-wide state that is going away
    /// anyway; forgetting it turns a crash on quit into the OS cleaning up.
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            std::mem::forget(stream);
        }
    }
}

/// Frames buffered before the card starts, about 64 ms at 48 kHz: one display frame of jitter
/// plus the callbacks a frame's worth of samples covers, so the steady state never starves.
pub const PREFILL_FRAMES: usize = 3 * 1024;

thread_local! {
    /// The running output. A stream is not `Send`, so it lives on the thread that opened it: the
    /// window loop's.
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
    /// Whether a device was already asked for; one failed open is not retried every frame.
    static ATTEMPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A frame's sound: drain the running core into the one live stream, or discard it when no window
/// is running.
///
/// The device is opened only once the window loop is live ([`crate::file_pick`]): a library
/// caller, a test, and `cargo test` itself never touch a sound card, and the core's queue is
/// still drained so it cannot grow.
pub fn on_frame(app: &mut App) {
    if app.machine().is_none() {
        return;
    }
    if !crate::file_pick::is_interactive() {
        discard(app);
        return;
    }
    let gain = gain(app);
    LIVE.with(|cell| {
        let mut live = cell.borrow_mut();
        if live.is_none() && !ATTEMPTED.get() {
            ATTEMPTED.set(true);
            match start(app) {
                Ok((running, rate, notice)) => {
                    app.set_sample_rate(rate);
                    if let Some(text) = notice {
                        app.show_notice(text, true);
                    }
                    *live = Some(running);
                }
                Err(why) => app.show_notice(format!("audio off: {why}"), true),
            }
        }
        match live.as_mut() {
            Some(running) => {
                if let Some(machine) = app.machine_mut() {
                    running.mixer.push(machine, gain);
                }
                if !running.playing && running.mixer.ring().len() >= PREFILL_FRAMES {
                    use cpal::traits::StreamTrait;
                    let played = running.stream.as_ref().map(cpal::Stream::play);
                    match played {
                        Some(Ok(())) => running.playing = true,
                        Some(Err(why)) => {
                            app.show_notice(format!("audio off: {why}"), true);
                            // Do not try to play a stream that refused once, and do not drop it
                            // here either: see `Live::drop`.
                            if let Some(stream) = running.stream.take() {
                                std::mem::forget(stream);
                            }
                            running.playing = true;
                        }
                        None => {}
                    }
                }
            }
            None => discard(app),
        }
    });
}

/// The volume the `[app]` section names, or full volume until #21 reads the file.
fn gain(app: &App) -> f32 {
    if app.muted() {
        return 0.0;
    }
    let system = app.system().unwrap_or(SystemKind::Gb);
    let volume = app
        .setting(system, "volume")
        .and_then(|value| value.trim().parse::<u8>().ok())
        .unwrap_or(100);
    volume_gain(volume)
}

/// The rate the `[app]` section's `audio_rate` asks the card for; `0` and none mean the card's own
/// default (gb's `negotiate_rate` rule).
fn audio_rate(app: &App) -> Option<u32> {
    let system = app.system().unwrap_or(SystemKind::Gb);
    app.setting(system, "audio_rate")
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|hz| *hz > 0)
}

/// Drops the core's queue: no window, so nothing is listening, but it must stay bounded.
fn discard(app: &mut App) {
    let Some(machine) = app.machine_mut() else {
        return;
    };
    let mut buffer = [0.0f32; 4_096];
    while machine.drain_audio(&mut buffer) > 0 {}
}

/// Opens the default output device and starts the stream at the negotiated rate.
///
/// Returns the running output, the rate the card actually opened at (what `App::sample_rate`
/// reports), and the refusal notice to show.
fn start(app: &App) -> Result<(Live, u32, Option<String>), String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    let default = device
        .default_output_config()
        .map_err(|why| why.to_string())?;
    let ranges: Vec<_> = device
        .supported_output_configs()
        .map(|it| it.collect())
        .unwrap_or_default();
    let want = audio_rate(app);
    let (mut chosen, mut notice) = negotiate_rate(want, &default, &ranges);
    let ring = SampleRing::new();
    let mut opened = build_stream(&device, &chosen, ring.clone());
    // Listed but refused: fall back to the card's own default rather than losing the sound.
    if opened.is_err() && chosen.sample_rate() != default.sample_rate() {
        let refused = chosen.sample_rate();
        chosen = default;
        notice = Some(rate_refused_notice(refused, chosen.sample_rate()));
        opened = build_stream(&device, &chosen, ring.clone());
    }
    let stream = opened?;
    let mixer = Mixer::with_ring(ring, chosen.sample_rate());
    Ok((
        Live {
            stream: Some(stream),
            mixer,
            playing: false,
        },
        chosen.sample_rate(),
        notice,
    ))
}

/// The config to open, and the notice to show if the rate the setting asked for was refused.
///
/// `want_hz` is the `audio_rate` setting. `ranges` is what the card lists. There are three
/// outcomes and only three: nothing asked for takes the card's own default; asked for and listed
/// at the default's channel count takes that config; asked for and not listed takes the card's
/// default and leaves a notice, and the converter bridges the core's rate to whatever the card
/// did open. Never a panic, never a device that fails to open because the setting asked for more
/// than the card has.
pub fn negotiate_rate(
    want_hz: Option<u32>,
    default: &cpal::SupportedStreamConfig,
    ranges: &[cpal::SupportedStreamConfigRange],
) -> (cpal::SupportedStreamConfig, Option<String>) {
    let Some(want) = want_hz else {
        return (*default, None);
    };
    match choose_config(want, default, ranges) {
        Some(config) => (config, None),
        None => (
            *default,
            Some(rate_refused_notice(want, default.sample_rate())),
        ),
    }
}

/// The card's supported config at `want_hz`, or `None` when it does not list that rate.
///
/// `default` is the card's own default config, whose channel count the callback is built around;
/// a range is only taken when it carries that many channels, and one whose sample format also
/// matches is preferred.
fn choose_config(
    want_hz: u32,
    default: &cpal::SupportedStreamConfig,
    ranges: &[cpal::SupportedStreamConfigRange],
) -> Option<cpal::SupportedStreamConfig> {
    let offers = |range: &&cpal::SupportedStreamConfigRange| {
        range.min_sample_rate() <= want_hz && want_hz <= range.max_sample_rate()
    };
    let chosen = ranges
        .iter()
        .filter(offers)
        .find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
        })
        .or_else(|| {
            ranges
                .iter()
                .filter(offers)
                .find(|range| range.channels() == default.channels())
        })?;
    chosen.try_with_sample_rate(want_hz)
}

/// The notice a refused rate leaves: the setting asked for 96 kHz, the card opened at 44.1 kHz,
/// and the core's own rate is converted to the rate the card did open.
pub fn rate_refused_notice(wanted_hz: u32, opened_hz: u32) -> String {
    format!(
        "{} refused by the card: opened at {}, resampling {} -> {}",
        khz(wanted_hz),
        khz(opened_hz),
        wanted_hz / 1000,
        opened_hz / 1000
    )
}

/// A rate as a notice spells it: `96 kHz`, `44.1 kHz`.
fn khz(hz: u32) -> String {
    if hz.is_multiple_of(1000) {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", f64::from(hz) / 1000.0)
    }
}

/// The output stream for one supported config: the ring's frames on the first two channels and
/// silence past what the ring holds, so a starved callback never replays the last chunk.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: SampleRing,
) -> Result<cpal::Stream, String> {
    match config.sample_format() {
        cpal::SampleFormat::F32 => build_typed::<f32>(device, config, ring),
        cpal::SampleFormat::I16 => build_typed::<i16>(device, config, ring),
        cpal::SampleFormat::U16 => build_typed::<u16>(device, config, ring),
        other => Err(format!("unsupported sample format {other}")),
    }
}

fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    ring: SampleRing,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;
    let channels = usize::from(config.channels()).max(1);
    let mut lr: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            config.config(),
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                lr.resize(frames * 2, 0.0);
                ring.pop_or_zero(&mut lr);
                for (index, slot) in data.chunks_mut(channels).enumerate() {
                    let left = T::from_sample(lr[index * 2]);
                    let right = T::from_sample(lr[index * 2 + 1]);
                    for (channel, out) in slot.iter_mut().enumerate() {
                        *out = if channel == 0 { left } else { right };
                    }
                }
            },
            |why| eprintln!("tendyemu: audio stream error: {why}"),
            None,
        )
        .map_err(|why| why.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{SampleFormat, SupportedBufferSize};

    fn config(hz: u32) -> cpal::SupportedStreamConfig {
        cpal::SupportedStreamConfig::new(2, hz, SupportedBufferSize::Unknown, SampleFormat::F32)
    }

    fn range(min: u32, max: u32) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(
            2,
            min,
            max,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        )
    }

    #[test]
    fn a_rate_the_card_lists_is_taken_without_a_notice() {
        let (chosen, notice) =
            negotiate_rate(Some(96_000), &config(48_000), &[range(44_100, 96_000)]);
        assert_eq!(chosen.sample_rate(), 96_000);
        assert_eq!(notice, None);
    }

    #[test]
    fn a_rate_the_card_refuses_leaves_the_default_and_a_notice() {
        let (chosen, notice) =
            negotiate_rate(Some(96_000), &config(44_100), &[range(44_100, 48_000)]);
        assert_eq!(chosen.sample_rate(), 44_100);
        let notice = notice.expect("a refusal is a notice");
        assert!(
            notice.contains("96 kHz") && notice.contains("44.1 kHz"),
            "{notice}"
        );
    }

    #[test]
    fn nothing_asked_takes_the_card_s_own_default() {
        let (chosen, notice) = negotiate_rate(None, &config(44_100), &[range(44_100, 96_000)]);
        assert_eq!(chosen.sample_rate(), 44_100);
        assert_eq!(notice, None);
    }

    #[test]
    fn an_underrun_reads_as_silence_not_as_the_last_chunk() {
        let ring = SampleRing::new();
        ring.push_stereo(&[0.5, -0.5, 0.25, -0.25]);
        let mut out = [9.0f32; 6];
        assert_eq!(ring.pop_or_zero(&mut out), 4);
        assert_eq!(out, [0.5, -0.5, 0.25, -0.25, 0.0, 0.0]);
    }

    #[test]
    fn a_matching_rate_is_a_straight_copy() {
        let mut converter = Converter::new(48_000, 48_000);
        let mut out = Vec::new();
        converter.convert(&[0.1, -0.1, 0.2, -0.2], &mut out);
        assert_eq!(out, [0.1, -0.1, 0.2, -0.2]);
        assert!(converter.is_identity());
    }

    /// A sine at `hz`, interleaved, `frames` long, at `rate`.
    fn tone(hz: f64, rate: u32, frames: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let value = (2.0 * PI * hz * n as f64 / f64::from(rate)).sin() as f32;
            samples.push(value);
            samples.push(value);
        }
        samples
    }

    /// The tone's frequency from its positive-going zero crossings, in Hz.
    fn measured_hz(samples: &[f32], rate: u32) -> f64 {
        let left: Vec<f32> = samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| pair[0])
            .collect();
        let mut crossings = 0;
        for pair in left.windows(2) {
            if pair[0] <= 0.0 && pair[1] > 0.0 {
                crossings += 1;
            }
        }
        f64::from(crossings) * f64::from(rate) / left.len() as f64
    }

    #[test]
    fn a_sinc_converter_keeps_the_pitch_at_every_core_rate() {
        for from in [32_000, 32_768, 44_100, 48_000] {
            let mut converter = Converter::new(from, 48_000);
            let mut out = Vec::new();
            let input = tone(1_000.0, from, 2 * from as usize);
            converter.convert(&input, &mut out);
            // Two seconds in, two seconds out: the only frames missing are the half kernel the
            // last output's window would need past the input's end.
            let expected = 2 * 48_000;
            assert!(
                (out.len() / 2).abs_diff(expected) < 64,
                "{from} Hz: {} frames out, about {expected}",
                out.len() / 2
            );
            let hz = measured_hz(&out, 48_000);
            assert!((hz - 1_000.0).abs() < 10.0, "{from} Hz: {hz} Hz out");
            let peak = out
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| pair[0].abs())
                .fold(0.0f32, f32::max);
            assert!((peak - 1.0).abs() < 0.05, "{from} Hz: peak {peak}");
        }
    }

    #[test]
    fn the_volume_curve_squares_the_slider() {
        assert_eq!(volume_gain(100), 1.0);
        assert!((volume_gain(50) - 0.25).abs() < 1e-6);
        assert_eq!(volume_gain(0), 0.0);
    }
}
