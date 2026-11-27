use super::Ppu;
use crate::cart::Mapper;

/// One of the 8 hardware sprite output units: pattern shifters, X counter and attributes.
/// `halted` means "the counter reached zero: the shifters are being shifted out".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpriteSlot {
    pub x: u8,
    pub lo: u8,
    pub hi: u8,
    pub attr: u8,
    pub is_zero: bool,
    pub halted: bool,
    // Fetch latches, written during the slot's 8-dot window in dots 257-320.
    pub f_y: u8,
    pub f_tile: u8,
    pub f_attr: u8,
    pub f_in_range: bool,
    pub f_lo: u8,
}

/// One sprite beyond the eight hardware slots, kept only while the sprite limit is removed.
/// Display only: it is built from primary OAM and `chr_peek` and never feeds sprite 0 hit, overflow
/// or any bus-visible state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtraSprite {
    pub x: u8,
    pub lo: u8,
    pub hi: u8,
    pub attr: u8,
    /// Pattern-table address of the row `lo`/`hi` came from, and the OAM index they were read from:
    /// the tile trace names a sprite by OAM index, and this one has no slot to name it.
    pub chr_addr: u16,
    pub oam: u8,
    /// CHR-ROM offset of that fetch; `None` for CHR RAM.
    pub chr_rom: Option<u32>,
}

impl Ppu {
    /// Sprite height in pixels: 16 when PPUCTRL bit 5 is set, else 8.
    pub fn sprite_height(&self) -> u16 {
        if self.ctrl & 0x20 != 0 { 16 } else { 8 }
    }

    /// One dot of the sprite evaluation state machine (see docs/PHASE1_OAM2_SPEC.md). Called before
    /// `dot` advances, only while rendering is enabled on lines 0..=239 and the pre-render line.
    pub fn sprite_eval_dot(&mut self) {
        // `tick` resets `dot` at `DOTS_PER_LINE` (341), so it is at most 340 here: evaluation is
        // always on the scanline `tick` is on and never wraps.
        let d = self.dot;
        debug_assert!(d < super::DOTS_PER_LINE);
        let line = self.scanline;
        let visible = line < 240;
        if matches!(d, 63 | 255 | 339) {
            self.oam2_frozen = false;
        }
        match d {
            1..=64 => {
                self.oam_latch = 0xFF;
                if visible {
                    self.oam2[((d - 1) >> 1) as usize] = 0xFF;
                }
            }
            65..=256 if visible => {
                if d == 65 {
                    self.eval_s = 0;
                    self.eval_in_range = false;
                    self.eval_done = false;
                    self.eval_bug = 0;
                    self.sprite0_in_oam2 = false;
                }
                let mut n = self.oam_addr >> 2;
                let mut m = self.oam_addr & 3;
                if d & 1 == 1 {
                    // The evaluation read is a real OAM read, so with the OAM-decay quirk on it is
                    // what keeps every row refreshed while rendering.
                    self.oam_latch = self.oam_read_live(self.oam_addr);
                } else if self.eval_done {
                    n = (n + 1) & 63;
                    self.oam_latch = self.oam2[(self.eval_s & 31) as usize];
                } else {
                    let latch = self.oam_latch;
                    if !self.eval_in_range
                        && line >= u16::from(latch)
                        && line - u16::from(latch) < self.sprite_height()
                    {
                        self.eval_in_range = true;
                        // The first sprite processed (dot 66) is "sprite zero", whatever its OAM index.
                        if d == 66 {
                            self.sprite0_in_oam2 = true;
                        }
                    }
                    if self.eval_s < 32 {
                        self.oam2[self.eval_s as usize] = latch;
                        if self.eval_in_range {
                            if self.eval_s & 3 == 0 {
                                let slot = usize::from(self.eval_s >> 2);
                                if let Some(t) = self.trace.as_deref_mut() {
                                    t.oam2_src[slot] = n;
                                }
                            }
                            // The 4th byte copied (X position) is range-checked like a Y; if it is out of
                            // range the address is (addr + 1) & $FC, which re-aligns a misaligned address.
                            let x_out_of_range = self.eval_s & 3 == 3
                                && !(line >= u16::from(latch)
                                    && line - u16::from(latch) < self.sprite_height());
                            m += 1;
                            self.eval_s += 1;
                            if self.eval_s == 32 {
                                self.oam2_frozen = true;
                            }
                            if m == 4 {
                                // The low address bits carry into the sprite index; a misaligned start
                                // (OAMADDR & 3 != 0) stays misaligned for the rest of the line.
                                m = 0;
                                n = (n + 1) & 63;
                                if n == 0 {
                                    self.eval_done = true;
                                }
                            } else if x_out_of_range {
                                m = 0;
                            }
                            if self.eval_s & 3 == 0 {
                                self.eval_in_range = false;
                            }
                        } else {
                            n = (n + 1) & 63;
                            if n == 0 {
                                self.eval_done = true;
                            }
                        }
                    } else {
                        self.oam_latch = self.oam2[(self.eval_s & 31) as usize];
                        if self.eval_in_range {
                            self.status |= 0x20;
                            m += 1;
                            if m == 4 {
                                n = (n + 1) & 63;
                                m = 0;
                            }
                            if self.eval_bug == 0 {
                                self.eval_bug = 3;
                            } else {
                                self.eval_bug -= 1;
                                if self.eval_bug == 0 {
                                    self.eval_done = true;
                                    m = 0;
                                }
                            }
                        } else {
                            n = (n + 1) & 63;
                            m = (m + 1) & 3;
                            if n == 0 {
                                self.eval_done = true;
                            }
                        }
                    }
                }
                self.oam_addr = (n << 2) | (m & 3);
            }
            257..=340 => {
                if d == 257 {
                    self.oam2_addr = 0;
                }
                if d <= 320 {
                    self.oam_addr = 0;
                }
                self.oam_latch = self.oam2[(self.oam2_addr & 31) as usize];
                // The OAM2 address steps on the Y, tile and attribute reads and after the 8th dot of a
                // slot (the X byte is read 5 times); it freezes at 0 once it overflows.
                if d <= 320 && !self.oam2_frozen && matches!((d - 257) % 8, 0 | 1 | 2 | 7) {
                    self.oam2_addr += 1;
                    if self.oam2_addr == 32 {
                        self.oam2_addr = 0;
                        self.oam2_frozen = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn oam2_byte(&self, k: usize, j: usize) -> u8 {
        if self.oam2_frozen {
            self.oam2[0]
        } else {
            self.oam2[k * 4 + j]
        }
    }

    /// Pattern address for slot `k` from its latches (always computed, even when out of range).
    fn sprite_pattern_addr(&self, k: usize) -> (u16, bool) {
        let s = &self.sprites[k];
        let h = self.sprite_height();
        let line = self.scanline & 255;
        let y = u16::from(s.f_y);
        let diff = line.wrapping_sub(y);
        let in_range = y <= line && diff < h;
        let mut row = diff & (h - 1);
        if s.f_attr & 0x80 != 0 {
            row ^= h - 1;
        }
        let tile = u16::from(s.f_tile);
        let addr = if h == 8 {
            (if self.ctrl & 0x08 != 0 { 0x1000 } else { 0 }) + tile * 16 + row
        } else {
            let idx = (tile & 0xFE) + u16::from(row >= 8);
            (tile & 1) * 0x1000 + idx * 16 + (row & 7)
        };
        (addr, in_range)
    }

    /// Sprite-limit removal: the in-range sprites after the first eight, read from primary OAM
    /// with `chr_peek` (no A12 edge, no MMC2/4 latch), for the line that is about to be drawn.
    fn build_extra_sprites(&mut self, cart: &dyn Mapper) {
        self.extra_sprites.clear();
        if self.scanline >= 240 {
            return;
        }
        let line = self.scanline;
        let h = self.sprite_height();
        let mut found = 0;
        for n in 0..64usize {
            let y = u16::from(self.oam[n * 4]);
            if !(y <= line && line - y < h) {
                continue;
            }
            found += 1;
            if found <= 8 {
                continue;
            }
            let tile = u16::from(self.oam[n * 4 + 1]);
            let attr = self.oam[n * 4 + 2];
            let mut row = (line - y) & (h - 1);
            if attr & 0x80 != 0 {
                row ^= h - 1;
            }
            let addr = if h == 8 {
                (if self.ctrl & 0x08 != 0 { 0x1000 } else { 0 }) + tile * 16 + row
            } else {
                (tile & 1) * 0x1000 + ((tile & 0xFE) + u16::from(row >= 8)) * 16 + (row & 7)
            };
            let (mut lo, mut hi) = (cart.chr_peek(addr), cart.chr_peek(addr + 8));
            if attr & 0x40 != 0 {
                lo = lo.reverse_bits();
                hi = hi.reverse_bits();
            }
            // §4 gap 1: the CHR-ROM offset is only needed while the tile trace records.
            let chr_rom = if self.trace.is_some() {
                cart.chr_rom_offset(addr & 0x1FFF)
                    .and_then(|off| u32::try_from(off).ok())
            } else {
                None
            };
            self.extra_sprites.push(ExtraSprite {
                x: self.oam[n * 4 + 3],
                lo,
                hi,
                attr,
                chr_addr: addr & 0x1FFF,
                oam: n as u8,
                chr_rom,
            });
        }
    }

    /// One dot of the sprite fetch window (257-320): slot k = (dot-257)/8 is built over 8 dots.
    pub fn sprite_fetch_dot(&mut self, cart: &mut dyn Mapper) {
        let d = self.dot;
        if !(257..=320).contains(&d) {
            return;
        }
        if self.sprite_limit_removed && d == 320 {
            self.build_extra_sprites(&*cart);
        }
        let k = usize::from((d - 257) / 8);
        match (d - 257) % 8 {
            0 => {
                let v = self.v;
                let nt = self.mem_read(cart, 0x2000 | (v & 0x0FFF));
                if d != 257 {
                    self.fetch_bus = Some(nt);
                }
                self.sprites[k].f_y = self.oam2_byte(k, 0);
            }
            1 => {
                self.sprites[k].f_y = self.oam2_byte(k, 0);
            }
            2 => {
                let v = self.v;
                self.fetch_bus = Some(self.mem_read(cart, 0x2000 | (v & 0x0FFF)));
                self.sprites[k].f_tile = self.oam2_byte(k, 1);
            }
            3 => {
                self.sprites[k].f_attr = self.oam2_byte(k, 2);
            }
            4 => {
                self.sprites[k].x = self.oam2_byte(k, 3);
                let (addr, in_range) = self.sprite_pattern_addr(k);
                let lo = self.mem_read_sprite(cart, addr);
                self.fetch_bus = Some(lo);
                let s = &mut self.sprites[k];
                s.f_in_range = in_range;
                s.f_lo = lo;
            }
            6 => {
                let (addr, _) = self.sprite_pattern_addr(k);
                let hi = self.mem_read_sprite(cart, addr + 8);
                self.fetch_bus = Some(hi);
                let f_attr = self.sprites[k].f_attr;
                if let Some(t) = self.trace.as_deref_mut() {
                    // the CHR-ROM offset behind this fetch, and the attribute
                    // byte in hand. shortcut: MMC5's separate sprite CHR set is looked up through
                    // the background set's bank, so a pack on MMC5 that splits them keys the
                    // background art; upgrade by peeking the set the fetch used if a pack needs it.
                    let rom = cart
                        .chr_rom_offset(addr & 0x1FFF)
                        .and_then(|off| u32::try_from(off).ok());
                    t.spr[k] = super::trace::Source {
                        chr_addr: addr & 0x1FFF,
                        palette: 4 + (f_attr & 3),
                        oam: t.oam2_src[k],
                        attr: f_attr,
                        chr_rom: rom,
                    };
                }
                let is_zero = k == 0 && self.sprite0_in_oam2;
                let s = &mut self.sprites[k];
                s.attr = s.f_attr;
                s.is_zero = is_zero;
                if s.f_in_range {
                    let (mut l, mut h) = (s.f_lo, hi);
                    if s.f_attr & 0x40 != 0 {
                        l = l.reverse_bits();
                        h = h.reverse_bits();
                    }
                    s.lo = l;
                    s.hi = h;
                } else {
                    s.lo = 0;
                    s.hi = 0;
                }
            }
            _ => {}
        }
    }

    /// Dot 339 with rendering enabled: every unit goes back to "counting" mode.
    pub fn sprites_start_counting(&mut self) {
        for s in self.sprites.iter_mut() {
            s.halted = false;
        }
    }

    /// Advance the sprite units for screen column x (visible dot x+1) and return the first
    /// non-transparent sprite pixel: (pixel 1..=3, palette 4..=7, behind background, is sprite 0).
    /// Counters tick even with rendering disabled; the shifters only shift while rendering.
    pub fn sprite_pixel(&mut self, x: usize) -> Option<(u8, u8, bool, bool)> {
        let shifting = self.rendering_enabled();
        let visible = self.mask & 0x10 != 0 && !(x < 8 && self.mask & 0x04 == 0);
        let mut out = None;
        let mut slot = 0;
        // A 9th-or-later sprite has no slot, so its identity is remembered directly.
        let mut extra_src: Option<super::trace::Source> = None;
        for (k, s) in self.sprites.iter_mut().enumerate() {
            if !s.halted {
                if s.x == 0 {
                    s.halted = true;
                } else {
                    s.x -= 1;
                }
            }
            if !s.halted {
                continue;
            }
            let pixel = ((s.hi >> 7) << 1) | (s.lo >> 7);
            if shifting {
                s.lo <<= 1;
                s.hi <<= 1;
            }
            if out.is_none() && visible && pixel != 0 {
                out = Some((pixel, 4 + (s.attr & 0x03), s.attr & 0x20 != 0, s.is_zero));
                slot = k;
            }
        }
        // Sprite-limit removal: only where the eight real slots drew nothing, and never sprite 0.
        if out.is_none() && visible && !self.extra_sprites.is_empty() {
            for e in &self.extra_sprites {
                let c = x.wrapping_sub(usize::from(e.x));
                if c < 8 {
                    let bit = 7 - c;
                    let pixel = (((e.hi >> bit) & 1) << 1) | ((e.lo >> bit) & 1);
                    if pixel != 0 {
                        out = Some((pixel, 4 + (e.attr & 0x03), e.attr & 0x20 != 0, false));
                        extra_src = Some(super::trace::Source {
                            chr_addr: e.chr_addr,
                            palette: 4 + (e.attr & 0x03),
                            oam: e.oam,
                            attr: e.attr,
                            chr_rom: e.chr_rom,
                        });
                        break;
                    }
                }
            }
        }
        if let Some(t) = self.trace.as_deref_mut() {
            t.last_spr = match (extra_src, out) {
                (Some(src), _) => src,
                (None, Some(_)) => t.spr[slot],
                (None, None) => super::trace::Source::default(),
            };
        }
        out
    }
}
