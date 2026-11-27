//! The echo unit's state: the FIR's coefficients and history, and the ring buffer's
//! position. Owned by the APU lane.
//!
//! The buffer is a ring of 4-byte entries at `ESA * $100`, each entry a left and a right 15-bit
//! sample (fullsnes "SNES APU DSP Echo Registers": "Byte 0: Lower 7bit of Left sample (stored in
//! bit1-7)", byte 1 the left high byte, bytes 2-3 the right). Wrapping happens at `EDL << 9`
//! entries — `EDL << 11` bytes — with `0` meaning a single entry (DESIGN amendment 17).

use crate::APU_RAM_SIZE;

/// The 8-tap FIR and the echo buffer's cursor.
#[derive(Clone, Default)]
pub struct Echo {
    pub fir: [i8; 8],
    /// The left channel's FIR history in a ring; `index` is the next slot to write. fullsnes
    /// "The FIR formula": `buf[(i-0) AND 7] = EchoRAM[addr] SAR 1`, and "the left and right stereo
    /// channels are filtered separately (no crosstalk), but with identical coefficients".
    pub buf_l: [i16; 8],
    /// The right channel's FIR history, the same ring with the entry's right word as its input.
    pub buf_r: [i16; 8],
    /// The FIR ring's cursor, `0..7`, shared by both histories.
    pub index: u8,
    /// Position inside the echo buffer, in 4-byte entries.
    pub ram_index: u32,
    /// The wrap length latched at entry 0: `EDL << 9` entries (DESIGN amendment 17).
    pub remain: u32,
}

impl Echo {
    /// The wrap length in 4-byte entries: `EDL << 9`, with `0` meaning one entry.
    ///
    /// fullsnes "Echo Buffer Notes": "If idx==0, set idx_max = EDL<<9"; and of `EDL`: "0-3 Echo
    /// Buffer Size (0=4 bytes, or 1..15=Size in 2K-byte steps)".
    pub fn wrap_entries(edl: u8) -> u32 {
        match u32::from(edl & 0x0F) {
            0 => 1,
            n => n << 9,
        }
    }

    /// The echo buffer's byte address for the entry at the cursor.
    fn address(&self, esa: u8) -> usize {
        ((u32::from(esa) * 0x100 + self.ram_index * 4) & 0xFFFF) as usize
    }

    /// One 32 kHz echo pass: read the entry at the cursor, run each channel's FIR over its own
    /// sample, and (when `write` is set) overwrite the entry with the voices' echo sums plus
    /// feedback.
    ///
    /// Returns the two FIR sums, which the caller mixes through `EVOLL`/`EVOLR`. `echo_l` and
    /// `echo_r` are the `EON` voices' outputs, `efb` the feedback volume. The formula is fullsnes's
    /// "The FIR formula" verbatim, including "if overflow occurred in LAST addition: saturate to
    /// min/max=-8000h/+7FFFh", "echo_input = echo_input AND FFFEh" and "Note that the left and
    /// right stereo channels are filtered separately (no crosstalk), but with identical
    /// coefficients."
    ///
    /// Eight arguments because the pass reads five registers and writes two channels; grouping them
    /// would only hide which hardware register each one is.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        ram: &mut [u8; APU_RAM_SIZE],
        esa: u8,
        edl: u8,
        efb: i8,
        echo_l: i16,
        echo_r: i16,
        write: bool,
    ) -> (i32, i32) {
        // "If idx==0, set idx_max = EDL<<9" (cycle 30-ish): latched at the start of a pass.
        if self.ram_index == 0 {
            self.remain = Echo::wrap_entries(edl);
        }
        let addr = self.address(esa);
        // The entry: fullsnes "6Dh - ESA", "Byte 0: Lower 7bit of Left sample (stored in bit1-7)
        // (bit0=unused/zero); Byte 1: Upper 8bit of Left sample", and bytes 2-3 the right sample.
        // The stored word is the 16-bit sample with bit 0 clear, so it is read as a *signed* word
        // and then "EchoRAM[addr] SAR 1" gives the 15-bit value: $FFFE is -2, not +$7FFF.
        let left = i16::from_le_bytes([ram[addr], ram[addr + 1]]);
        let right = i16::from_le_bytes([ram[addr + 2], ram[addr + 3]]);
        let slot = usize::from(self.index & 7);
        self.buf_l[slot] = left >> 1;
        self.buf_r[slot] = right >> 1;
        self.index = self.index.wrapping_add(1) & 7;

        // The eight taps, oldest first: `buf[(i-7) AND 7]*FIR0 .. buf[(i-0) AND 7]*FIR7`, each
        // `SAR 6`, with no overflow handling for the first seven and saturation on the last.
        let mut sum_l: i32 = 0;
        let mut sum_r: i32 = 0;
        for tap in 0..8u8 {
            // The newest sample was just written at `index - 1`; tap 0 is the oldest (eight back)
            // and tap 7 the newest, exactly as the source's `(i-7)`..`(i-0)` reads.
            let slot = usize::from(self.index.wrapping_sub(8 - tap) & 7);
            let coefficient = i32::from(self.fir[usize::from(tap)]);
            let term_l = (i32::from(self.buf_l[slot]) * coefficient) >> 6;
            let term_r = (i32::from(self.buf_r[slot]) * coefficient) >> 6;
            if tap == 7 {
                sum_l = (sum_l + term_l).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                sum_r = (sum_r + term_r).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
            } else {
                sum_l += term_l;
                sum_r += term_r;
            }
        }

        if write {
            // echo_input = EchoVoices + (sum*EFB SAR 7), masked with bit 0 cleared, per channel.
            let fb_l = (sum_l * i32::from(efb)) >> 7;
            let fb_r = (sum_r * i32::from(efb)) >> 7;
            let l = ((i32::from(echo_l) + fb_l) as u16) & 0xFFFE;
            let r = ((i32::from(echo_r) + fb_r) as u16) & 0xFFFE;
            ram[addr] = l as u8;
            ram[addr + 1] = (l >> 8) as u8;
            ram[addr + 2] = r as u8;
            ram[addr + 3] = (r >> 8) as u8;
        }

        // "ram_index=ram_index+1; if idx>=idx_max, idx=0" — the next pass latches a fresh length.
        self.ram_index += 1;
        if self.ram_index >= self.remain {
            self.ram_index = 0;
        }
        (sum_l, sum_r)
    }
}
