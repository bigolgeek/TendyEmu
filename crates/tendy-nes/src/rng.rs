//! The core's one seeded RNG, and the seeded streams built on it.
//!
//! Everything random-looking the emulator derives from `--seed` is splitmix64: one
//! golden-ratio increment, then a two-round avalanche mix (Steele, Lea and Flood,
//! "Fast Splittable Pseudorandom Number Generators"). It is the smallest generator
//! whose whole output sequence is specified by four constants, so a seed means the
//! same thing in every build, on every platform, and in any other tool that wants to
//! reproduce this emulator's power-on state.
//!
//! A *domain* keeps the streams built on one seed apart: [`derive`] XORs a fixed
//! 64-bit tag into the seed, so the work-RAM page, the power-on alignment and the
//! reset-button alignment draw from three unrelated-looking sequences instead of the
//! same one. The tags are part of the seed's meaning and are therefore **fixed
//! forever** — changing one silently rewrites what every existing seed does, so each
//! tag is pinned by a test below.
//!
//! Two different byte shapes are needed, and they must not be confused:
//! - [`SplitMix64::fill_bytes`] fills a buffer **eight bytes per step**, least-significant
//!   byte first; [`power_on_ram`](crate::bus::power_on_ram) is its caller.
//! - [`SplitMix64::next_u8`] draws a **fresh 64-bit step per byte** and keeps the low
//!   byte. It exists for callers with one value per register (a mapper's bank
//!   registers); using it where the page fill needs `next_u64` would produce a
//!   different page for the same seed.

/// A splitmix64 generator: `state` is the seed plus however many steps have been taken.
///
/// `Clone`/`Copy` so a caller can branch a stream (derive two independent values from
/// one seed without disturbing the original), and `PartialEq` so tests can prove two
/// derivations start from the same place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator starting at `seed`: the first `next_u64()` is splitmix64's output
    /// number one for that seed, not the seed itself.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64-bit value: add the golden-ratio increment, then avalanche-mix.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The low byte of one fresh 64-bit step. One step per byte, so this is *not* the
    /// byte stream of the 2 KB work-RAM page (see the module docs).
    pub fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    /// Fill `buf` eight bytes per step, least-significant byte first, continuing this
    /// stream. This is the work-RAM page's byte shape ([`power_on_ram`](crate::bus::power_on_ram)),
    /// for any length: a final partial chunk takes the low bytes of one more step.
    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64();
            for (i, byte) in chunk.iter_mut().enumerate() {
                *byte = (word >> (8 * i)) as u8;
            }
        }
    }

    /// A value in `0..n` from one fresh 64-bit step. `n` must be at least 1.
    ///
    /// `% n` is slightly biased towards the low values (the bias is at most
    /// `n / 2^64`, i.e. unreachable in practice for the small `n` used here), and the
    /// bias is deterministic, so a seed still replays exactly.
    pub fn next_below(&mut self, n: u32) -> u32 {
        assert!(n > 0, "next_below(0) has no values to return");
        (self.next_u64() % u64::from(n)) as u32
    }
}

/// The fixed tag that separates the seed-derived streams.
///
/// The tags are XORed into the seed by [`derive`]. `Ram` and `Align` are the values the
/// hand-rolled splitmix64 copies produced before this module existed (`bus.rs`
/// `power_on_ram` used the raw seed, `PhaseChoice::resolve` used `0x000A_116E`), so
/// moving those two call sites here had to keep them byte-for-byte. `ResetAlign` is new
/// (it carries the reset-button alignment) and its value is the four ASCII bytes
/// `RSET`: arbitrary, but self-describing and fixed from the day it shipped.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeedDomain {
    /// The 2 KB CPU work-RAM page at power-on (`--ram-init random`).
    Ram = 0x0000_0000,
    /// The CPU/PPU power-on alignment (`--align random`).
    Align = 0x000A_116E,
    /// The reset-button alignment, further mixed with the reset index by its caller.
    ResetAlign = 0x5253_4554,
    /// The PPU's nametable RAM, primary OAM and secondary OAM at power-on
    /// (`--ram-init random`). Tag is the ASCII bytes `PRAM`.
    PpuRam = 0x5052_414D,
    /// A cartridge's volatile PRG RAM, then CHR-RAM, at power-on (`--ram-init random`).
    /// Tag is the ASCII bytes `CRAM`.
    CartRam = 0x4352_414D,
}

impl SeedDomain {
    /// The tag this domain XORs into a seed.
    pub const fn tag(self) -> u64 {
        self as u64
    }
}

/// The generator for `seed` in `domain`: the seed with the domain's tag XORed in, ready
/// for its first [`SplitMix64::next_u64`].
pub fn derive(seed: u64, domain: SeedDomain) -> SplitMix64 {
    SplitMix64::new(seed ^ domain.tag())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// splitmix64's published first outputs for seeds 0 and 1, and the values this
    /// emulator produced before the module existed (captured from the pre-S1
    /// `power_on_ram` and `PhaseChoice::resolve` implementations).
    #[test]
    fn splitmix64_first_outputs_are_pinned() {
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(SplitMix64::new(1).next_u64(), 0x910A_2DEC_8902_5CC1);
    }

    #[test]
    fn a_generator_walks_the_published_sequence() {
        let mut rng = SplitMix64::new(0);
        assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(rng.next_u64(), 0x06C4_5D18_8009_454F);
        assert_eq!(rng.next_u64(), 0xF88B_B8A8_724C_81EC);
        // A clone continues from the same place; the original is untouched by the copy.
        let mut copy = rng;
        assert_eq!(copy.next_u64(), rng.next_u64());
        assert_eq!(SplitMix64::new(0), SplitMix64::new(0));
    }

    /// The tags are the meaning of a seed. Each one is pinned, because changing one
    /// changes what every existing `--seed` produces.
    #[test]
    fn domain_tags_are_fixed() {
        assert_eq!(SeedDomain::Ram.tag(), 0x0000_0000);
        assert_eq!(SeedDomain::Align.tag(), 0x000A_116E);
        assert_eq!(SeedDomain::ResetAlign.tag(), 0x5253_4554); // "RSET"
        assert_eq!(
            SeedDomain::ResetAlign.tag(),
            u64::from(u32::from_be_bytes(*b"RSET"))
        );
        assert_eq!(SeedDomain::CartRam.tag(), 0x4352_414D); // "CRAM"
        assert_eq!(
            SeedDomain::CartRam.tag(),
            u64::from(u32::from_be_bytes(*b"CRAM"))
        );
        assert_eq!(SeedDomain::PpuRam.tag(), 0x5052_414D); // "PRAM"
        assert_eq!(
            SeedDomain::PpuRam.tag(),
            u64::from(u32::from_be_bytes(*b"PRAM"))
        );
    }

    /// The first value of every domain, for a spread of seeds. `Ram` values are the
    /// pre-S1 work-RAM stream (word for word), `Align` values reproduce the pre-S1
    /// `PhaseChoice::resolve` modulo 4, and `ResetAlign` is pinned at birth so #127b
    /// builds on a fixed sequence.
    #[test]
    fn every_domain_first_output_is_pinned() {
        const CASES: [(u64, u64, u64, u64); 5] = [
            // seed, Ram, Align, ResetAlign
            (
                0,
                0xE220_A839_7B1D_CDAF,
                0xB12C_1479_066A_43C1,
                0x423F_9D74_9535_B1D8,
            ),
            (
                1,
                0x910A_2DEC_8902_5CC1,
                0xCA34_8C26_FA1C_BA40,
                0xA030_8250_265E_23F8,
            ),
            (
                42,
                0xBDD7_3226_2FEB_6E95,
                0x4064_690B_96B9_E516,
                0xB344_9E10_AB81_E5AF,
            ),
            (
                0x1234_5678_9ABC_DEF0,
                0x1619_22C6_45CE_50E8,
                0x40C6_5220_9137_8860,
                0xF04C_B159_3A42_1B4D,
            ),
            (
                0xDECA_FACE_BEEF_0001,
                0x2A45_5217_E9FE_6514,
                0x0D0D_594C_9FC3_3D85,
                0x3D63_A2B9_06AB_1B8B,
            ),
        ];
        for (seed, ram, align, reset_align) in CASES {
            assert_eq!(
                derive(seed, SeedDomain::Ram).next_u64(),
                ram,
                "Ram seed {seed:#x}"
            );
            assert_eq!(
                derive(seed, SeedDomain::Align).next_u64(),
                align,
                "Align seed {seed:#x}"
            );
            assert_eq!(
                derive(seed, SeedDomain::ResetAlign).next_u64(),
                reset_align,
                "ResetAlign seed {seed:#x}"
            );
        }
    }

    /// The pre-S1 alignment values (`splitmix64(seed ^ 0x000A_116E) % 4`) are what the
    /// `--align random` flag produced before this module, and they stay.
    #[test]
    fn align_domain_reproduces_the_old_phase_values() {
        let phases: Vec<u8> = [
            0u64,
            1,
            42,
            0x1234_5678_9ABC_DEF0,
            0xDECA_FACE_BEEF_0001,
            u64::MAX,
        ]
        .iter()
        .map(|&seed| derive(seed, SeedDomain::Align).next_below(4) as u8)
        .collect();
        assert_eq!(phases, vec![1, 0, 2, 0, 1, 3]);
    }

    #[test]
    fn next_below_stays_in_range_and_is_deterministic() {
        for n in [1u32, 2, 3, 4, 7, 1000] {
            let mut a = derive(7, SeedDomain::Ram);
            let mut b = derive(7, SeedDomain::Ram);
            let draws: Vec<u32> = (0..64).map(|_| a.next_below(n)).collect();
            assert_eq!(
                draws,
                (0..64).map(|_| b.next_below(n)).collect::<Vec<u32>>()
            );
            assert!(
                draws.iter().all(|&v| v < n),
                "next_below({n}) left its range"
            );
        }
        // n = 1 is a constant function, and it still consumes one step.
        let mut stepped = derive(7, SeedDomain::Ram);
        let mut consumed = derive(7, SeedDomain::Ram);
        assert_eq!(consumed.next_below(1), 0);
        stepped.next_u64();
        assert_eq!(
            consumed, stepped,
            "next_below must consume exactly one step"
        );
        assert_eq!(consumed.next_u64(), stepped.next_u64());
    }

    #[test]
    fn fill_bytes_is_the_work_ram_shape() {
        let mut rng = derive(42, SeedDomain::Ram);
        let mut buf = [0u8; 21]; // two whole words and a 5-byte tail
        rng.fill_bytes(&mut buf);
        let mut words = derive(42, SeedDomain::Ram);
        let mut expect = Vec::new();
        for _ in 0..3 {
            expect.extend_from_slice(&words.next_u64().to_le_bytes());
        }
        assert_eq!(&buf[..], &expect[..21]);
        assert_eq!(rng, words, "three steps consumed, tail included");
    }

    #[test]
    fn next_u8_is_the_low_byte_of_a_fresh_step() {
        let mut byte_rng = derive(42, SeedDomain::Ram);
        let mut word_rng = derive(42, SeedDomain::Ram);
        for _ in 0..4 {
            let word = word_rng.next_u64();
            assert_eq!(byte_rng.next_u8(), word as u8);
        }
    }

    /// The point of domains: one seed does not put two streams on the same sequence.
    #[test]
    fn domains_are_pairwise_distinct_for_one_seed() {
        for seed in [0u64, 1, 2, 42, 0x1234_5678_9ABC_DEF0, u64::MAX] {
            let ram = derive(seed, SeedDomain::Ram).next_u64();
            let align = derive(seed, SeedDomain::Align).next_u64();
            let reset = derive(seed, SeedDomain::ResetAlign).next_u64();
            let ppu_ram = derive(seed, SeedDomain::PpuRam).next_u64();
            let cart_ram = derive(seed, SeedDomain::CartRam).next_u64();
            assert_ne!(ram, cart_ram, "Ram and CartRam collide at seed {seed:#x}");
            assert_ne!(
                align, cart_ram,
                "Align and CartRam collide at seed {seed:#x}"
            );
            assert_ne!(
                reset, cart_ram,
                "ResetAlign and CartRam collide at seed {seed:#x}"
            );
            assert_ne!(
                ppu_ram, cart_ram,
                "PpuRam and CartRam collide at seed {seed:#x}"
            );
            assert_ne!(ram, align, "Ram and Align collide at seed {seed:#x}");
            assert_ne!(ram, reset, "Ram and ResetAlign collide at seed {seed:#x}");
            assert_ne!(
                align, reset,
                "Align and ResetAlign collide at seed {seed:#x}"
            );
            assert_ne!(ram, ppu_ram, "Ram and PpuRam collide at seed {seed:#x}");
            assert_ne!(align, ppu_ram, "Align and PpuRam collide at seed {seed:#x}");
            assert_ne!(
                reset, ppu_ram,
                "ResetAlign and PpuRam collide at seed {seed:#x}"
            );
        }
    }

    /// `Ram` is the raw seed, byte-for-byte what `power_on_ram` always produced: no
    /// domain offset at all. This is the property that let S1 move the page fill
    /// without changing a single byte of it.
    #[test]
    fn ram_domain_is_the_raw_seed() {
        for seed in [0u64, 1, 42, 0x1234_5678_9ABC_DEF0] {
            let mut derived = derive(seed, SeedDomain::Ram);
            let mut raw = SplitMix64::new(seed);
            for step in 0..8 {
                assert_eq!(
                    derived.next_u64(),
                    raw.next_u64(),
                    "seed {seed:#x} step {step}"
                );
            }
        }
    }
}
