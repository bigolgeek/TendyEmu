//! Save-state serialisation for the PPU. Field order is fixed; see `crate::state`.

use super::render::{Background, LagPixel};
use super::sprites::SpriteSlot;
use super::{DOTS_PER_LINE, Ppu};
use crate::state::{StateError, StateReader, StateWriter};

impl Background {
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.nt);
        w.u8(self.at);
        w.u8(self.lo);
        w.u8(self.hi);
        w.u16(self.pat_lo);
        w.u16(self.pat_hi);
        w.u16(self.attr_lo);
        w.u16(self.attr_hi);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.nt = r.u8()?;
        self.at = r.u8()?;
        self.lo = r.u8()?;
        self.hi = r.u8()?;
        self.pat_lo = r.u16()?;
        self.pat_hi = r.u16()?;
        self.attr_lo = r.u16()?;
        self.attr_hi = r.u16()?;
        Ok(())
    }
}

impl SpriteSlot {
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.x);
        w.u8(self.lo);
        w.u8(self.hi);
        w.u8(self.attr);
        w.bool(self.is_zero);
        w.bool(self.halted);
        w.u8(self.f_y);
        w.u8(self.f_tile);
        w.u8(self.f_attr);
        w.bool(self.f_in_range);
        w.u8(self.f_lo);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.x = r.u8()?;
        self.lo = r.u8()?;
        self.hi = r.u8()?;
        self.attr = r.u8()?;
        self.is_zero = r.bool()?;
        self.halted = r.bool()?;
        self.f_y = r.u8()?;
        self.f_tile = r.u8()?;
        self.f_attr = r.u8()?;
        self.f_in_range = r.bool()?;
        self.f_lo = r.u8()?;
        Ok(())
    }
}

impl LagPixel {
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.x);
        w.u8(self.bg);
        w.u8(self.bg_raw);
        w.u8(self.backdrop);
        w.u8(self.spr);
        w.u8(self.spr_raw);
        w.bool(self.behind);
    }

    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.x = r.u8()?;
        self.bg = r.u8()?;
        self.bg_raw = r.u8()?;
        self.backdrop = r.u8()?;
        self.spr = r.u8()?;
        self.spr_raw = r.u8()?;
        self.behind = r.bool()?;
        Ok(())
    }
}

/// Writes a pending `(dot, value)` pair as the tag byte, dot and value; `None` is the zero tag
/// alone. Every pair field uses this encoding, so one field cannot drift from another.
fn save_opt_pair(w: &mut StateWriter, pair: Option<(u16, u16)>) {
    match pair {
        Some((dot, value)) => {
            w.u8(1);
            w.u16(dot);
            w.u16(value);
        }
        None => w.u8(0),
    }
}

/// Reads what [`save_opt_pair`] wrote.
fn load_opt_pair(r: &mut StateReader) -> Result<Option<(u16, u16)>, StateError> {
    if r.bool()? {
        let dot = r.u16()?;
        Ok(Some((dot, r.u16()?)))
    } else {
        Ok(None)
    }
}

impl Ppu {
    /// Writes every field; fields added later are appended at the end and bump `STATE_VERSION`
    /// (`crate::state`), so an old layout is rejected before it can be misread.
    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.ctrl);
        w.u8(self.mask);
        w.u8(self.status);
        w.u8(self.oam_addr);
        w.bytes(&self.oam);
        w.u16(self.v);
        w.u16(self.t);
        w.u8(self.fine_x);
        w.bool(self.w);
        w.u8(self.read_buffer);
        w.u8(self.io_latch);
        w.u32(self.io_decay);
        w.bytes(&self.vram);
        w.bytes(&self.palette);
        w.u16(self.scanline);
        w.u16(self.dot);
        w.u64(self.frame);
        w.bool(self.nmi_out);
        w.bool(self.suppress_vbl);
        w.bool(self.nmi_cancel);
        w.bool(self.frame_ready);
        w.bytes(&self.framebuffer);
        w.bytes(&self.emphasis);
        w.u8(self.lag_len);
        for p in &self.lag_pipe {
            p.save_state(w);
        }
        self.bg.save_state(w);
        for s in &self.sprites {
            s.save_state(w);
        }
        w.bytes(&self.oam2);
        w.u8(self.eval_s);
        w.u8(self.oam_latch);
        w.bool(self.eval_in_range);
        w.bool(self.eval_done);
        w.bool(self.oam2_frozen);
        w.u8(self.oam2_addr);
        for &b in &self.rend_pipe {
            w.bool(b);
        }
        w.opt_u8(self.oam_corrupt_pending);
        w.bool(self.sprite_active_prev);
        w.u8(self.eval_bug);
        // `eval_count` is gone. Its byte stays so the layout is unchanged (no
        // `STATE_VERSION` bump) and a state written before the removal still loads.
        w.u8(0);
        w.bool(self.sprite0_in_oam2);
        // `fetch_frozen` is gone; placeholder byte, same reason.
        w.bool(false);
        w.u8(self.rend_run);
        w.u8(self.bg_idle);
        w.u8(self.read_pending);
        w.opt_u8(self.fetch_bus);
        w.u8(self.bus_last);
        match self.ale_latch {
            Some((d, b)) => {
                w.u8(1);
                w.u16(d);
                w.u8(b);
            }
            None => w.u8(0),
        }
        save_opt_pair(w, self.hyb);
        // Appended last, so a state from before the field is this state minus the final
        // byte. `STATE_VERSION` 2.
        w.bool(self.warmup);
        // The OAM-decay row stamps, appended after everything else and
        // saved whatever the quirks, so the layout never depends on config (spec §4).
        // `STATE_VERSION` 3.
        for &stamp in &self.oam_stamp {
            w.u64(stamp);
        }
        // The pending `$2006` scroll-glitch landing, encoded like `hyb`
        // (u8 tag, then dot and pre-write `v`). Appended last and saved whatever the quirks, so a
        // default PPU's state is this state with a `0` tag. `STATE_VERSION` 4.
        save_opt_pair(w, self.glitch_2006);
    }

    /// Reads what `save_state` wrote. On error `self` may be partly overwritten.
    pub fn load_state(&mut self, r: &mut StateReader) -> Result<(), StateError> {
        self.ctrl = r.u8()?;
        self.mask = r.u8()?;
        self.status = r.u8()?;
        self.oam_addr = r.u8()?;
        r.bytes_into(&mut self.oam)?;
        self.v = r.u16()?;
        self.t = r.u16()?;
        self.fine_x = r.u8()?;
        self.w = r.bool()?;
        self.read_buffer = r.u8()?;
        self.io_latch = r.u8()?;
        self.io_decay = r.u32()?;
        r.bytes_into(&mut self.vram)?;
        r.bytes_into(&mut self.palette)?;
        self.scanline = r.u16()?;
        if self.scanline >= self.lines_per_frame {
            // Saved on an overclocked machine (extra post-render lines, not serialised) and loaded on a
            // shorter one: the position is off the end of this frame, so start the next one.
            self.scanline = 0;
        }
        self.dot = r.u16()?;
        self.frame = r.u64()?;
        self.nmi_out = r.bool()?;
        self.suppress_vbl = r.bool()?;
        self.nmi_cancel = r.bool()?;
        self.frame_ready = r.bool()?;
        self.framebuffer.resize(256 * 240, 0);
        r.bytes_into(&mut self.framebuffer)?;
        self.emphasis.resize(256 * 240, 0);
        r.bytes_into(&mut self.emphasis)?;
        self.lag_len = r.u8()?;
        for p in &mut self.lag_pipe {
            p.load_state(r)?;
        }
        // The descriptors beside the pipe are not serialised (they are trace state, not machine
        // state), so a loaded pipe has nothing to pair with: start them clean rather than letting the
        // tracer read a pre-load pixel's provenance (docs/ppu-pixel.md §3).
        self.lag_src = Default::default();
        self.bg.load_state(r)?;
        for s in &mut self.sprites {
            s.load_state(r)?;
        }
        r.bytes_into(&mut self.oam2)?;
        self.eval_s = r.u8()?;
        self.oam_latch = r.u8()?;
        self.eval_in_range = r.bool()?;
        self.eval_done = r.bool()?;
        self.oam2_frozen = r.bool()?;
        self.oam2_addr = r.u8()?;
        for b in &mut self.rend_pipe {
            *b = r.bool()?;
        }
        self.oam_corrupt_pending = r.opt_u8()?;
        self.sprite_active_prev = r.bool()?;
        self.eval_bug = r.u8()?;
        // `eval_count`: placeholder byte, see `save_state`.
        let _ = r.u8()?;
        self.sprite0_in_oam2 = r.bool()?;
        // `fetch_frozen`: placeholder byte, see `save_state`.
        let _ = r.bool()?;
        self.rend_run = r.u8()?;
        self.bg_idle = r.u8()?;
        self.read_pending = r.u8()?;
        self.fetch_bus = r.opt_u8()?;
        self.bus_last = r.u8()?;
        self.ale_latch = if r.bool()? {
            let d = r.u16()?;
            Some((d, r.u8()?))
        } else {
            None
        };
        self.hyb = load_opt_pair(r)?;
        self.warmup = r.bool()?;
        for stamp in &mut self.oam_stamp {
            *stamp = r.u64()?;
        }
        self.glitch_2006 = load_opt_pair(r)?;
        // A state file is magic, version and ROM hash, never a checksum (`crate::state`), so every
        // field can be anything. These four are used as array indexes or counters, so a damaged or
        // hand-edited file would index `oam`/`oam2` out of bounds or leave a wrap test unreachable
        // and freeze the picture instead of reporting a bad state. `scanline` is already
        // covered by the clamp above. Self-written states always pass: `dot` is 0..=340,
        // `fine_x` is a 3-bit scroll value and `lag_len` counts the two-entry pipeline, and
        // `oam_corrupt_seed` masks the seed to 0..=31.
        if self.dot >= DOTS_PER_LINE
            || self.fine_x > 7
            || self.lag_len > 2
            || self.oam_corrupt_pending.is_some_and(|seed| seed >= 32)
        {
            return Err(StateError::BadValue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(ppu: &Ppu) -> Vec<u8> {
        let mut w = StateWriter::new();
        ppu.save_state(&mut w);
        w.finish()
    }

    /// FNV-1a of a saved state with every optional field set, and of an untouched PPU's state:
    /// measured before the helpers existed, so a different encoding fails here.
    const OPTIONAL_FIELDS_HASH: u64 = 0x4673_EB79_98E4_5513;
    const EMPTY_STATE_HASH: u64 = 0xF211_F165_4DCC_7EAC;

    fn load(bytes: &[u8]) -> Result<(), StateError> {
        Ppu::new().load_state(&mut StateReader::new(bytes))
    }

    /// Byte offset of the one byte that differs between two saves of otherwise identical PPUs, so a
    /// corruption test cannot silently land on a neighbouring field when the layout changes.
    fn offset_between(base: impl Fn(&mut Ppu), other: impl Fn(&mut Ppu)) -> usize {
        let mut a = Ppu::new();
        base(&mut a);
        let mut b = Ppu::new();
        other(&mut b);
        let (a, b) = (saved(&a), saved(&b));
        assert_eq!(a.len(), b.len());
        let diffs: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
        assert_eq!(diffs.len(), 1, "field must be one byte: {diffs:?}");
        diffs[0]
    }

    fn fnv(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    /// A PPU with every optional dot/value field and both lag-pipeline entries set, so the bytes
    /// `save_state` writes for them all differ from the empty case.
    fn with_optional_fields() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.ale_latch = Some((0x0123, 0x45));
        ppu.hyb = Some((0x0ABC, 0x1DEF));
        ppu.glitch_2006 = Some((0x0234, 0x5678));
        ppu.lag_len = 2;
        ppu.lag_pipe[0] = LagPixel {
            x: 1,
            bg: 2,
            bg_raw: 3,
            backdrop: 4,
            spr: 5,
            spr_raw: 6,
            behind: true,
        };
        ppu.lag_pipe[1] = LagPixel {
            x: 7,
            bg: 8,
            bg_raw: 9,
            backdrop: 10,
            spr: 11,
            spr_raw: 12,
            behind: false,
        };
        ppu
    }

    /// The optional pair fields are written by one shared helper and the lag pipeline by
    /// `LagPixel::save_state`, so this pins the bytes those fields contribute: a helper that
    /// encoded a field in another order would move the hash even though every round-trip still
    /// worked.
    #[test]
    fn optional_fields_and_lag_pipe_keep_their_bytes() {
        assert_eq!(fnv(&saved(&with_optional_fields())), OPTIONAL_FIELDS_HASH);
        assert_eq!(fnv(&saved(&Ppu::new())), EMPTY_STATE_HASH);
    }

    #[test]
    fn load_state_restores_the_optional_fields_and_the_lag_pipe() {
        let src = with_optional_fields();
        let bytes = saved(&src);
        let mut dst = Ppu::new();
        dst.load_state(&mut StateReader::new(&bytes)).unwrap();
        assert_eq!(dst.ale_latch, src.ale_latch);
        assert_eq!(dst.hyb, src.hyb);
        assert_eq!(dst.glitch_2006, src.glitch_2006);
        assert_eq!(dst.lag_len, src.lag_len);
        assert_eq!(dst.lag_pipe, src.lag_pipe);
    }

    /// Damaged states used to reach `tick`/`bg_pixel` and crash or freeze; they are refused instead. Each case is one field `load_state` now range-checks.
    #[test]
    fn load_state_refuses_values_used_as_indexes_or_counters() {
        let valid = saved(&Ppu::new());
        assert_eq!(load(&valid), Ok(()), "a self-written state still loads");

        // `dot` only wraps on `== DOTS_PER_LINE`: 341 and up never finish a frame.
        let at = offset_between(|_| (), |p| p.dot = 1);
        let mut bad = valid.clone();
        bad[at..at + 2].copy_from_slice(&(DOTS_PER_LINE + 1).to_le_bytes());
        assert_eq!(load(&bad), Err(StateError::BadValue), "dot");

        // `fine_x` of 16+ overflows the shift in `bg_pixel`.
        let at = offset_between(|_| (), |p| p.fine_x = 1);
        let mut bad = valid.clone();
        bad[at] = 16;
        assert_eq!(load(&bad), Err(StateError::BadValue), "fine_x");

        // `lag_len` above 2 never drains the two-entry pixel pipeline.
        let at = offset_between(|_| (), |p| p.lag_len = 1);
        let mut bad = valid.clone();
        bad[at] = 3;
        assert_eq!(load(&bad), Err(StateError::BadValue), "lag_len");

        // A seed of 32+ indexes `oam` (`seed * 8 + i`) and `oam2` (`seed`) out of bounds.
        let at = offset_between(
            |p| p.oam_corrupt_pending = Some(1),
            |p| p.oam_corrupt_pending = Some(2),
        );
        let mut bad = valid.clone();
        bad[at] = 32;
        assert_eq!(load(&bad), Err(StateError::BadValue), "oam_corrupt_pending");
    }
}
