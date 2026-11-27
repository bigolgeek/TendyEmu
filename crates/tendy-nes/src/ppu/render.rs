use super::Ppu;
use super::memory::{attribute_palette, palette_index};
use super::trace::{Layer, PixelTrace, Source};
use crate::cart::Mapper;

#[cfg(feature = "ppu-profile")]
use super::profile::{Bucket, PixelProbe};

/// Times one sub-bucket of the pixel path when the `ppu-profile` feature is on, and compiles to the
/// bare expression — no probe, no clock, no branch, and no reference to the probe or the bucket —
/// when it is off (docs/ppu-pixel.md §2).
#[cfg(feature = "ppu-profile")]
macro_rules! probe {
    ($sub:expr, $bucket:ident, $body:expr) => {
        $sub.time(Bucket::$bucket, || $body)
    };
}

#[cfg(not(feature = "ppu-profile"))]
macro_rules! probe {
    ($sub:expr, $bucket:ident, $body:expr) => {
        $body
    };
}

/// Background fetch latches and the 16-bit shift registers that feed one pixel per dot.
#[derive(Clone, Debug, Default)]
pub struct Background {
    pub nt: u8,
    pub at: u8,
    pub lo: u8,
    pub hi: u8,
    pub pat_lo: u16,
    pub pat_hi: u16,
    pub attr_lo: u16,
    pub attr_hi: u16,
}

/// A pixel inside the two-dot output pipeline: the screen column it belongs to and
/// the contributions it was produced with. Greyscale, emphasis and the D3/D1 background gate are all
/// applied when it leaves, with the PPUMASK in force two dots after the pixel's own dot.
///
/// Seven bytes of payload and no `Source` fields: where the pixel came from is [`LagSource`], which
/// only the tracer writes. Keeping the descriptors out of this struct halves what the per-dot path
/// copies twice — and it is `save_state`'s shape that pins the field set, because the seven
/// serialised bytes are written in this order (`LagPixel::save_state`, src/ppu/state.rs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LagPixel {
    pub x: u8,
    /// Background pixel (0..=3) from the shifters before the D3/D1 gate, and the colour it produces.
    pub bg: u8,
    pub bg_raw: u8,
    /// Backdrop colour ($3F00) on the pixel's own dot: what it shows when the background is gated off.
    pub backdrop: u8,
    /// Sprite pixel (1..=3, 0 = none) and the colour it produces. Sprites keep the live D4.
    pub spr: u8,
    pub spr_raw: u8,
    /// The sprite pixel is behind the background.
    pub behind: bool,
}

/// Where a queued pixel came from, entry for entry with `Ppu::lag_pipe`.
///
/// Written only while the tracer is on — it is trace state, not machine state: never saved, never
/// loaded, and nothing in emulation reads it — so the untraced per-dot path never touches it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LagSource {
    pub bg: Source,
    pub spr: Source,
}

impl Ppu {
    /// True when background or sprite rendering is on (PPUMASK bits 3 or 4).
    pub fn rendering_enabled(&self) -> bool {
        self.mask & 0x18 != 0
    }

    /// PPUMASK's emphasis bits as the index of the one NTSC-order table: bit 0 red, bit 1 green,
    /// bit 2 blue. The 2C07 (PAL) and Dendy swap red and green — NESdev PPU registers, `$2001`:
    /// "Emphasize red (green on PAL/Dendy)" / "Emphasize green (red on PAL/Dendy)" — so there the
    /// two low bits are swapped before the table is indexed. Blue (bit 2) is never swapped.
    #[inline]
    pub fn emphasis_bits(&self) -> u8 {
        let e = self.mask >> 5;
        if self.emphasis_swap {
            (e & 4) | ((e & 1) << 1) | ((e & 2) >> 1)
        } else {
            e
        }
    }

    pub(super) fn inc_coarse_x(&mut self) {
        if self.v & 0x001F == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    pub(super) fn inc_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    fn copy_x(&mut self) {
        self.v = (self.v & !0x041F) | (self.t & 0x041F);
    }

    fn copy_y(&mut self) {
        self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
    }

    fn load_shifters(&mut self) {
        let b = &mut self.bg;
        b.pat_lo = (b.pat_lo & 0xFF00) | u16::from(b.lo);
        b.pat_hi = (b.pat_hi & 0xFF00) | u16::from(b.hi);
        b.attr_lo = (b.attr_lo & 0xFF00) | if b.at & 1 != 0 { 0xFF } else { 0 };
        b.attr_hi = (b.attr_hi & 0xFF00) | if b.at & 2 != 0 { 0xFF } else { 0 };
        let at = b.at & 3;
        if let Some(t) = self.trace.as_deref_mut() {
            // The tile that was in the low byte is in the high byte now: it is the one on screen.
            t.bg_cur = t.bg_next;
            t.bg_next = Source {
                chr_addr: t.bg_fetch_addr,
                palette: at,
                oam: 0,
                attr: 0,
                chr_rom: t.bg_fetch_rom,
            };
        }
    }

    pub(super) fn shift(&mut self) {
        let b = &mut self.bg;
        b.pat_lo <<= 1;
        b.pat_hi = (b.pat_hi << 1) | 1;
        b.attr_lo <<= 1;
        b.attr_hi <<= 1;
    }

    fn ale_override(&mut self, addr: u16) -> u16 {
        match self.ale_latch.take() {
            Some((d, l)) if d == self.dot => (addr & 0xFF00) | u16::from(l),
            _ => addr,
        }
    }

    fn bg_pattern_addr(&self) -> u16 {
        let table = if self.ctrl & 0x10 != 0 { 0x1000 } else { 0 };
        table + u16::from(self.bg.nt) * 16 + ((self.v >> 12) & 0x07)
    }

    /// One dot of background fetching for a visible or pre-render line (call only while rendering is enabled).
    pub fn background_dot(&mut self, cart: &mut dyn Mapper) {
        let dot = self.dot;
        if (2..=257).contains(&dot) || (321..=337).contains(&dot) {
            self.shift();
            match (dot - 1) % 8 {
                0 => {
                    self.load_shifters();
                    let addr = 0x2000 | (self.v & 0x0FFF);
                    self.bg.nt = self.mem_read(cart, addr);
                    if let Some((e, old_v)) = self.hyb.take()
                        && e == dot
                    {
                        let save = self.v;
                        self.v = old_v;
                        // The pending read always carried HYB_DELAY (3) dots, always >= 2, so the increment is unconditional.
                        self.inc_coarse_x();
                        let ov = self.v;
                        self.v = save;
                        let new_v = self.t;
                        let a = 0x2000 | (new_v & 0x0F00) | (ov & 0x00FF);
                        self.bg.nt = self.mem_read(cart, a);
                        self.v = new_v;
                    }
                    self.fetch_bus = Some(self.bg.nt);
                }
                2 => {
                    let v = self.v;
                    let addr = 0x23C0 | (v & 0x0C00) | ((v >> 4) & 0x38) | ((v >> 2) & 0x07);
                    let at = self.mem_read(cart, addr);
                    self.fetch_bus = Some(at);
                    self.bg.at =
                        attribute_palette(at, usize::from(v & 0x1F), usize::from((v >> 5) & 0x1F));
                }
                4 => {
                    let addr = self.bg_pattern_addr();
                    if let Some(t) = self.trace.as_deref_mut() {
                        t.bg_fetch_addr = addr & 0x1FFF;
                        // the ROM offset behind that pattern address, at this fetch.
                        t.bg_fetch_rom = cart
                            .chr_rom_offset(addr & 0x1FFF)
                            .and_then(|off| u32::try_from(off).ok());
                    }
                    let addr = self.ale_override(addr);
                    self.bg.lo = self.mem_read(cart, addr);
                    self.fetch_bus = Some(self.bg.lo);
                }
                6 => {
                    let addr = self.bg_pattern_addr() + 8;
                    let addr = self.ale_override(addr);
                    self.bg.hi = self.mem_read(cart, addr);
                    self.fetch_bus = Some(self.bg.hi);
                }
                7 => self.inc_coarse_x(),
                _ => {}
            }
        }
        if dot == 339 {
            // Second dummy nametable fetch.
            let a = 0x2000 | (self.v & 0x0FFF);
            self.fetch_bus = Some(self.mem_read(cart, a));
        }
        if dot == 256 {
            self.inc_y();
        }
        if dot == 257 {
            self.load_shifters();
            self.copy_x();
        }
        if self.scanline == self.pre_render_line && (280..=304).contains(&dot) {
            self.copy_y();
        }
    }

    /// The background pixel (0..=3) and palette (0..=3) under the shifters, **before** the PPUMASK
    /// D3 (enable) and D1 (left clip) gate: those two bits are applied two dots later, when the
    /// pixel leaves the output pipeline, so this reads the shifters only.
    pub fn bg_pixel(&self) -> (u8, u8) {
        let mux = 0x8000u16 >> self.fine_x;
        let p0 = u8::from(self.bg.pat_lo & mux != 0);
        let p1 = u8::from(self.bg.pat_hi & mux != 0);
        let a0 = u8::from(self.bg.attr_lo & mux != 0);
        let a1 = u8::from(self.bg.attr_hi & mux != 0);
        ((p1 << 1) | p0, (a1 << 1) | a0)
    }

    /// True when PPUMASK lets the background show at column x: D3 enables it, D1 masks its left
    /// eight columns. Read at the dot the pixel leaves the pipeline, i.e. `x + 3`.
    fn bg_enabled(&self, x: usize) -> bool {
        self.mask & 0x08 != 0 && !(x < 8 && self.mask & 0x02 == 0)
    }

    /// The colour a (pixel, palette) pair selects from the current palette, pixel 0 being backdrop.
    fn palette_colour(&self, pixel: u8, pal: u8) -> u8 {
        // Pixel 0 is entry 0 whatever the palette, and for pixels 1-3 the low two bits of
        // `(pal << 2) | pixel` are never zero, so palette RAM's $10/$14/$18/$1C fold
        // (`palette_index`) cannot fire here: index the 32 bytes directly. The `& 0x1F` also
        // proves the index in range, so the array bounds check goes.
        // Callers keep the contract: `bg_pixel` gives palette 0-3, `sprite_source` gives 4-7.
        debug_assert!(
            pixel <= 3 && pal <= 7,
            "palette index is 5 bits: pixel 0-3, palette 0-7"
        );
        let i = if pixel == 0 {
            0
        } else {
            usize::from((pal << 2) | pixel) & 0x1F
        };
        self.palette[i]
    }

    /// The colour the backdrop shows on this dot: palette entry 0, except during forced blank.
    ///
    /// "Backdrop override" (NESdev, PPU palettes § Backdrop override): palette RAM has one address
    /// input, which is normally the one the rendering hardware produces, but while the CPU owns it
    /// the PPU draws whatever that address holds. So when both background and sprite rendering are
    /// disabled and `v` points into palette RAM ($3F00-$3FFF), the screen shows
    /// `palette[v & 0x1F]` instead of entry 0 - the side effect `full_nes_palette` and other
    /// mid-scanline palette demos are built on, and the reason a game that ends vblank with `v` in
    /// palette space flashes that colour in its bottom border rows (usually hidden by overscan).
    ///
    /// The rule applies only during forced blank: with either layer enabled the rendering
    /// pipeline supplies the address, so the override is not consulted.
    fn backdrop_colour(&self) -> u8 {
        if !self.rendering_enabled() && self.v & 0x3F00 == 0x3F00 {
            self.palette[palette_index(self.v)]
        } else {
            self.palette_colour(0, 0)
        }
    }

    /// Produce the pixel for the current dot (visible lines, dots 1..=256) and push its contributions
    /// into the two-dot output pipeline. Nothing is final here: `finalize_lag_pixel` composes it with
    /// the PPUMASK in force two dots later, where the greyscale, emphasis and D3/D1 bits land.
    pub fn output_pixel(&mut self) {
        let x = usize::from(self.dot - 1);
        // One `self.trace` test for the whole dot: the per-pixel `Source` work at the bottom runs
        // only while tracing, and it is the same test `finalize_lag_pixel` would make again.
        let traced = self.trace.is_some();
        #[cfg(feature = "ppu-profile")]
        let sub = PixelProbe::arm(self.dot, self.scanline, &Bucket::BODY);
        let (bg, bg_pal) = probe!(sub, BgPixel, self.bg_pixel());
        let bg_raw = probe!(sub, BgPalette, self.palette_colour(bg, bg_pal));
        let hit = probe!(sub, SpritePixel, self.sprite_pixel(x));
        let (spr, spr_raw, behind, is_zero) = match hit {
            Some((sp, sp_pal, behind, is_zero)) => (
                sp,
                probe!(sub, SprPalette, self.palette_colour(sp, sp_pal)),
                behind,
                is_zero,
            ),
            None => (0, 0, false, false),
        };
        // Sprite 0 hit is still decided on the pixel's own dot, with the background gate then in force.
        if is_zero && bg != 0 && self.bg_enabled(x) && x != 255 {
            self.status |= 0x40;
        }
        // The front entry is two dots old exactly when the pipeline is full, i.e. when a newer
        // pixel is already queued behind it: that is pixel x's dot x+3, where its mask bits land.
        if self.lag_len == 2 {
            self.finalize_lag_pixel_inner(traced);
        }
        let at = usize::from(self.lag_len);
        probe!(
            sub,
            LagPush,
            self.lag_push(LagPixel {
                x: x as u8,
                bg,
                bg_raw,
                backdrop: self.backdrop_colour(),
                spr,
                spr_raw,
                behind
            })
        );
        if traced {
            let src = self.pixel_source(spr);
            self.lag_src[at] = src;
        }
    }

    /// The background and sprite descriptors attached to the pixel just queued for column `x`:
    /// the one piece of the per-dot path that only exists while tracing.
    #[inline]
    fn pixel_source(&self, spr: u8) -> LagSource {
        let t = match self.trace.as_deref() {
            Some(t) => t,
            None => return LagSource::default(),
        };
        let bg = if u16::from(self.fine_x) + (self.dot - 1) % 8 < 8 {
            t.bg_cur
        } else {
            t.bg_next
        };
        LagSource {
            bg,
            spr: if spr != 0 {
                t.last_spr
            } else {
                Source::default()
            },
        }
    }

    /// Write the oldest queued pixel to the frame buffer with the PPUMASK of the current dot: the
    /// D3/D1 background gate and greyscale (bit 0) here, emphasis is `mask >> 5`. Pixel `x` is
    /// composed with the mask of dot `x + 3`, so a mid-line `$2001` write that clears D3 blanks the
    /// two columns already queued behind it (the `scanline` test area 1).
    /// `output_pixel` calls this once the entry is two dots old, and `tick` calls it on dots 257-258,
    /// where pixels 254 and 255 finally reach dot x+3.
    pub fn finalize_lag_pixel(&mut self) {
        let traced = self.trace.is_some();
        self.finalize_lag_pixel_inner(traced);
    }

    /// The body of [`Ppu::finalize_lag_pixel`], with the day's `self.trace` answer already in hand:
    /// the callers (`output_pixel`, and `tick` on dots 257-258) each test it once per dot.
    #[inline]
    fn finalize_lag_pixel_inner(&mut self, traced: bool) {
        if self.lag_len == 0 {
            return;
        }
        #[cfg(feature = "ppu-profile")]
        let sub = PixelProbe::arm(self.dot, self.scanline, &Bucket::FINALIZE);
        let p = self.lag_pipe[0];
        // The compose is the timed bucket; the trace-plane write at the bottom is not part of it.
        let (bg, spr) = probe!(sub, Finalize, {
            self.lag_pipe[0] = self.lag_pipe[1];
            self.lag_len -= 1;
            let bg = if self.bg_enabled(usize::from(p.x)) {
                p.bg
            } else {
                0
            };
            // Hidden layers are dropped here, after sprite 0 hit was decided from the real pixels.
            let bg = if self.layers.hide_bg { 0 } else { bg };
            let spr = if self.layers.hide_sprites { 0 } else { p.spr };
            let mut color = match (bg, spr) {
                (0, 0) => p.backdrop,
                (0, _) => p.spr_raw,
                (_, 0) => p.bg_raw,
                (_, _) if p.behind => p.bg_raw,
                _ => p.spr_raw,
            };
            if self.mask & 0x01 != 0 {
                color &= 0x30;
            }
            let i = usize::from(self.scanline) * 256 + usize::from(p.x);
            self.framebuffer[i] = color;
            self.emphasis[i] = self.emphasis_bits();
            (bg, spr)
        });
        if traced {
            let src = self.lag_src[0];
            self.lag_src[0] = self.lag_src[1];
            let x = usize::from(p.x);
            // the background identity is kept wherever the layer was allowed to draw,
            // transparent pixel or not, and lost where the PPUMASK gate or the hidden layer stopped it.
            let bg_named = self.bg_enabled(x) && !self.layers.hide_bg;
            if let Some(t) = self.trace.as_deref_mut() {
                let (bg_chr_addr, bg_palette, bg_rom) = if bg_named {
                    (src.bg.chr_addr, src.bg.palette, src.bg.chr_rom)
                } else {
                    (0, 0xFF, None)
                };
                let bg_px = PixelTrace {
                    layer: Layer::Background,
                    chr_addr: src.bg.chr_addr,
                    palette: src.bg.palette,
                    pixel: bg,
                    attr: 0,
                    chr_rom: src.bg.chr_rom,
                    bg_chr_addr,
                    bg_palette,
                    bg_chr_rom: bg_rom,
                };
                let spr_px = PixelTrace {
                    layer: Layer::Sprite(src.spr.oam),
                    chr_addr: src.spr.chr_addr,
                    palette: src.spr.palette,
                    pixel: spr,
                    attr: src.spr.attr,
                    chr_rom: src.spr.chr_rom,
                    bg_chr_addr,
                    bg_palette,
                    bg_chr_rom: bg_rom,
                };
                let i = usize::from(self.scanline) * 256 + x;
                t.plane[i] = match (bg, spr) {
                    (0, 0) => PixelTrace {
                        layer: Layer::Backdrop,
                        bg_chr_addr,
                        bg_palette,
                        bg_chr_rom: bg_rom,
                        ..PixelTrace::NONE
                    },
                    (0, _) => spr_px,
                    (_, 0) => bg_px,
                    (_, _) if p.behind => bg_px,
                    _ => spr_px,
                };
            }
        }
    }

    /// Append a freshly produced pixel to the (at most two entry) output pipeline.
    fn lag_push(&mut self, p: LagPixel) {
        self.lag_pipe[usize::from(self.lag_len)] = p;
        self.lag_len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::load;

    /// NROM image with 8 KB of CHR-RAM (CHR bank count 0).
    fn chr_ram_rom() -> Vec<u8> {
        let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        v.resize(16 + 16384, 0);
        v
    }

    /// A PPU standing on dot 1 of a visible line, rendering on, no greyscale or emphasis, backdrop
    /// colour 0x16: every pixel it produces is the backdrop.
    fn line_start() -> (Ppu, Box<dyn Mapper>) {
        let cart = load(&chr_ram_rom()).unwrap();
        let mut ppu = Ppu::new();
        ppu.palette[0] = 0x16;
        ppu.scanline = 0;
        ppu.dot = 1;
        ppu.mask = 0x0A;
        (ppu, cart)
    }

    // greyscale and emphasis come from the PPUMASK in force on dot x+3, so a write made
    // while pixel 0 is still in the pipeline still governs pixel 0.
    #[test]
    fn greyscale_and_emphasis_apply_two_dots_late() {
        let (mut ppu, mut cart) = line_start();
        ppu.tick(cart.as_mut());
        ppu.tick(cart.as_mut()); // dots 1-2: pixel 0 is queued, not yet in the picture
        assert_eq!(ppu.framebuffer[0], 0x00);
        assert_eq!(ppu.emphasis[0], 0);
        ppu.mask = 0x2B; // greyscale + red emphasis, written just before dot 3
        ppu.tick(cart.as_mut()); // dot 3 finalizes pixel 0
        assert_eq!(ppu.framebuffer[0], 0x10); // 0x16 & 0x30
        assert_eq!(ppu.emphasis[0], 1);
    }

    // The last two pixels of a line cannot wait for an x+3 dot inside dots 1-256, so dots 257 and
    // 258 finalize them.
    #[test]
    fn the_last_two_pixels_finalize_on_dots_257_and_258() {
        let (mut ppu, mut cart) = line_start();
        for _ in 0..256 {
            ppu.tick(cart.as_mut()); // dots 1..=256
        }
        assert_eq!(ppu.framebuffer[253], 0x16);
        assert_eq!(ppu.framebuffer[254], 0x00);
        assert_eq!(ppu.framebuffer[255], 0x00);
        ppu.mask = 0x2B;
        ppu.tick(cart.as_mut()); // dot 257
        assert_eq!(ppu.framebuffer[254], 0x10);
        assert_eq!(ppu.framebuffer[255], 0x00);
        ppu.tick(cart.as_mut()); // dot 258
        assert_eq!(ppu.framebuffer[255], 0x10);
        assert_eq!(ppu.emphasis[255], 1);
        assert_eq!(ppu.lag_len, 0);
    }

    // The `scanline` test area 1 clears D3 with a dot 201/202/203 write while the star
    // tile starts at x = 200. Pixel x is composed with the mask of dot x + 3, so a dot-202 write
    // already blanks x = 199 and a dot-203 write blanks x = 200: the two columns that leaked before
    // (200 and 201) stay backdrop, and nothing to their left is touched.
    #[test]
    fn a_mid_line_bg_disable_blanks_the_pixels_still_in_the_pipeline() {
        for w in [202u16, 203] {
            let (mut ppu, _cart) = line_start(); // PPUMASK $0A: D1 + D3 on, backdrop 0x16
            ppu.palette[0x03] = 0x30; // BG pixel 3, palette 0: the star colour
            for dot in 1..=256u16 {
                ppu.dot = dot;
                // The fetch side is not clocked here: hold the shifter output at pixel 3.
                ppu.bg.pat_lo = 0xFFFF;
                ppu.bg.pat_hi = 0xFFFF;
                if dot == w {
                    ppu.mask = 0x02; // `STA $2001` with D3 cleared, the area-1 dispatch
                }
                ppu.output_pixel();
            }
            ppu.dot = 257;
            ppu.finalize_lag_pixel();
            ppu.dot = 258;
            ppu.finalize_lag_pixel();
            let first_blank = w as usize - 3;
            for (x, &px) in ppu.framebuffer.iter().enumerate().take(first_blank) {
                assert_eq!(px, 0x30, "w={w} x={x} must stay lit");
            }
            for (x, &px) in ppu
                .framebuffer
                .iter()
                .enumerate()
                .take(256)
                .skip(first_blank)
            {
                assert_eq!(px, 0x16, "w={w} x={x} must be backdrop");
            }
            assert_eq!(ppu.lag_len, 0);
        }
    }

    // The same 2-dot rule in the other direction: a D3 0 -> 1 write lights the background from
    // x = w - 3. This is why `bg_pixel` must not gate the background on the mask of the pixel's own
    // dot: the pixel is composed two dots later, when D3 is on.
    #[test]
    fn a_mid_line_bg_enable_lights_the_pixels_still_in_the_pipeline() {
        for w in [202u16, 203] {
            let (mut ppu, _cart) = line_start();
            ppu.mask = 0x02; // D1 on, D3 off: the background is hidden
            ppu.palette[0x03] = 0x30;
            for dot in 1..=256u16 {
                ppu.dot = dot;
                ppu.bg.pat_lo = 0xFFFF;
                ppu.bg.pat_hi = 0xFFFF;
                if dot == w {
                    ppu.mask = 0x0A; // `STA $2001` with D3 set again
                }
                ppu.output_pixel();
            }
            ppu.dot = 257;
            ppu.finalize_lag_pixel();
            ppu.dot = 258;
            ppu.finalize_lag_pixel();
            let first_lit = w as usize - 3;
            for (x, &px) in ppu.framebuffer.iter().enumerate().take(first_lit) {
                assert_eq!(px, 0x16, "w={w} x={x} must stay backdrop");
            }
            for (x, &px) in ppu.framebuffer.iter().enumerate().take(256).skip(first_lit) {
                assert_eq!(px, 0x30, "w={w} x={x} must be lit");
            }
            assert_eq!(ppu.lag_len, 0);
        }
    }

    /// The direct index must pick the same palette byte as the `$3Fxx` address through
    /// `palette_index` for every pixel and palette the renderer can produce (pixel 0..=3, palette
    /// 0..=7: background uses 0..=3, sprites 4..=7).
    #[test]
    fn palette_colour_indexes_the_byte_palette_ram_would() {
        let (mut ppu, _cart) = line_start();
        for (i, v) in ppu.palette.iter_mut().enumerate() {
            *v = 0x40 | i as u8;
        }
        for pal in 0..=7u8 {
            for pixel in 0..=3u8 {
                let addr = if pixel == 0 {
                    0x3F00
                } else {
                    0x3F00 | u16::from((pal << 2) | pixel)
                };
                assert_eq!(
                    ppu.palette_colour(pixel, pal),
                    ppu.palette[palette_index(addr)],
                    "pixel {pixel}, palette {pal}"
                );
            }
        }
    }
}
