//! One DSP voice's state: BRR decode position, the pitch counter, the envelope and
//! the four interpolated samples. Owned by the APU lane.

/// Where a voice's envelope is.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum EnvState {
    #[default]
    Off,
    Attack,
    Decay,
    Sustain,
    Release,
}

impl EnvState {
    /// The state as §9.2's table encodes it: one `u8`.
    pub fn to_u8(self) -> u8 {
        match self {
            EnvState::Off => 0,
            EnvState::Attack => 1,
            EnvState::Decay => 2,
            EnvState::Sustain => 3,
            EnvState::Release => 4,
        }
    }

    /// The inverse of [`EnvState::to_u8`]; anything else is `Off`.
    pub fn from_u8(v: u8) -> EnvState {
        match v {
            1 => EnvState::Attack,
            2 => EnvState::Decay,
            3 => EnvState::Sustain,
            4 => EnvState::Release,
            _ => EnvState::Off,
        }
    }
}

/// One of the DSP's eight voices, 36 bytes as §9.2's table counts them.
#[derive(Clone, Default)]
pub struct Voice {
    pub pitch: u16,
    pub counter: u16,
    pub srcn: u8,
    /// The current BRR block address in APU RAM, as a `u32`: the SNES core carried it in the bus's
    /// 24-bit `Addr` and the SGB core in a `u16`, and the wider integer is the one that loses
    /// nothing. The decoder reads it back through `as u16`, so every address it forms stays inside
    /// the 64 KiB the DSP can reach.
    pub brr: u32,
    /// Nibbles decoded out of the current block, `0..=15`. fullsnes "BRR Pitch": "Counter.Bit15-12
    /// indicates the current sample (within a BRR block)", so the decoder advances one nibble per
    /// `$1000` the pitch counter carries. It leads the counter by four: the key-on decodes a group
    /// of four (Anomie §BRR DECODING: "BRR data is always decoded in a group of 4 samples"), which
    /// is exactly the lead this port's four-sample Gaussian history needs.
    pub nibble: u8,
    pub brr_header: u8,
    pub adsr1: u8,
    pub adsr2: u8,
    pub gain: u8,
    /// 11 bits, `0..=$7FF`.
    pub env: u16,
    pub state: EnvState,
    /// Sample units until the next envelope step.
    pub rate_acc: u32,
    pub old: i16,
    pub older: i16,
    /// The last four interpolated samples, newest first: `buf[0] = new`, `buf[3] = oldest`.
    pub buf: [i16; 4],
    /// OUTX, the value pitch modulation reads.
    pub out: i16,
    /// The five empty samples after KON.
    pub kon_delay: u8,
}

impl Voice {
    /// A voice at power-on: silent, envelope off, nothing buffered.
    pub fn silence(&mut self) {
        self.state = EnvState::Off;
        self.env = 0;
        self.rate_acc = 0;
        self.old = 0;
        self.older = 0;
        self.buf = [0; 4];
        self.out = 0;
    }

    /// Pushes one 15-bit BRR sample through the history: `buf[0]` is newest.
    pub fn push(&mut self, sample: i16) {
        self.buf = [sample, self.buf[0], self.buf[1], self.buf[2]];
    }
}
