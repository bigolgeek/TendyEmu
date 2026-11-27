//! Event model for the event viewer: pure data, no bus hooks here.
//! A later step records bus traffic into an [`EventLog`]; this module only
//! stores, filters, renders and prints it.

/// What happened at a bus address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Read,
    Write,
    Nmi,
    Irq,
    OamDma,
    DmcDma,
}

/// One recorded bus event, positioned both in CPU time (`cycle`, `frame`)
/// and PPU time (`scanline`, `dot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusEvent {
    /// Frame the event happened in (as counted by the recorder).
    pub frame: u64,
    pub scanline: u16,
    pub dot: u16,
    /// CPU cycle at the event.
    pub cycle: u64,
    pub kind: EventKind,
    pub addr: u16,
    /// Data byte for reads/writes, 0 for NMI/IRQ.
    pub val: u8,
}

/// Unbounded log; the caller clears it. Recording is gated on `enabled`, so a disabled log costs
/// only the flag check. Only `examples/events.rs` and `tests/events_bus.rs` switch it on.
#[derive(Clone, Debug, Default)]
pub struct EventLog {
    pub enabled: bool,
    pub events: Vec<BusEvent>,
}

impl EventLog {
    /// Stores the event only when logging is enabled.
    pub fn push(&mut self, e: BusEvent) {
        if self.enabled {
            self.events.push(e);
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// All events of one frame, in recording order.
    pub fn for_frame(&self, frame: u64) -> impl Iterator<Item = &BusEvent> {
        self.events.iter().filter(move |e| e.frame == frame)
    }

    /// "frame,scanline,dot,cycle,kind,addr,val" header + one line per event
    /// (addr as $XXXX, val as $XX).
    pub fn to_csv(&self) -> String {
        let mut s = String::from("frame,scanline,dot,cycle,kind,addr,val\n");
        for e in &self.events {
            s.push_str(&format!(
                "{},{},{},{},{:?},${:04X},${:02X}\n",
                e.frame, e.scanline, e.dot, e.cycle, e.kind, e.addr, e.val
            ));
        }
        s
    }
}

/// Colour for one event: `scanline` is the row (0..=261), `dot` the column
/// (0..=340). Kind wins over address colouring (DMA rows, NMI, IRQ).
fn event_color(e: &BusEvent) -> [u8; 3] {
    match e.kind {
        EventKind::Nmi => [255, 255, 255],
        EventKind::Irq => [255, 0, 255],
        EventKind::OamDma | EventKind::DmcDma => [0, 255, 255],
        EventKind::Read => {
            // $2000-$2007 green, $4000-$4017 blue, everything else green too
            // (only register-space reads are recorded today).
            if (0x4000..=0x4017).contains(&e.addr) {
                [0, 0, 255]
            } else {
                [0, 255, 0]
            }
        }
        EventKind::Write => {
            if (0x2000..=0x2007).contains(&e.addr) {
                [255, 0, 0]
            } else if (0x4000..=0x4017).contains(&e.addr) {
                [0, 0, 255]
            } else if e.addr >= 0x4020 {
                [255, 255, 0]
            } else {
                [255, 0, 0]
            }
        }
    }
}

/// Width of the event map: one column per PPU dot.
pub const EVENT_MAP_WIDTH: usize = 341;
/// Height of an NTSC event map: one row per scanline of `Ppu::lines_per_frame`. PAL and Dendy frames
/// have 312 scanlines and need that many rows (see [`render_event_map`]).
pub const EVENT_MAP_HEIGHT: u16 = 262;

/// `EVENT_MAP_WIDTH` x `lines` RGB image (3 bytes/pixel, row-major) with one coloured dot per event of
/// `frame`: $2000-$2007 writes red, reads green, $4000-$4017 blue, mapper writes ($4020+) yellow, NMI
/// white, IRQ magenta, DMA cyan.
///
/// `lines` is the PPU's `lines_per_frame`: 262 for NTSC ([`EVENT_MAP_HEIGHT`]) and 312 for PAL and
/// Dendy, whose events run up to scanline 311 (Dendy starts vblank at 291). An event outside the map is
/// dropped.
pub fn render_event_map(log: &EventLog, frame: u64, lines: u16) -> Vec<u8> {
    let width = EVENT_MAP_WIDTH;
    let height = usize::from(lines);
    let mut img = vec![0u8; width * height * 3];
    for e in log.for_frame(frame) {
        let (sl, dot) = (usize::from(e.scanline), usize::from(e.dot));
        if sl < height && dot < width {
            let i = (sl * width + dot) * 3;
            img[i..i + 3].copy_from_slice(&event_color(e));
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(
        frame: u64,
        scanline: u16,
        dot: u16,
        cycle: u64,
        kind: EventKind,
        addr: u16,
        val: u8,
    ) -> BusEvent {
        BusEvent {
            frame,
            scanline,
            dot,
            cycle,
            kind,
            addr,
            val,
        }
    }

    #[test]
    fn push_disabled_stores_nothing() {
        let mut log = EventLog::default();
        log.push(ev(0, 0, 0, 0, EventKind::Read, 0x2002, 0));
        assert!(log.events.is_empty());
    }

    #[test]
    fn push_enabled_stores() {
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        log.push(ev(3, 10, 20, 100, EventKind::Write, 0x2001, 0x1E));
        assert_eq!(log.events.len(), 1);
        let kept: Vec<_> = log.for_frame(3).collect();
        assert_eq!(kept.len(), 1);
        assert!(log.for_frame(2).next().is_none());
        let other = ev(9, 0, 0, 0, EventKind::Read, 0, 0);
        log.push(other);
        assert_eq!(log.events.len(), 2);
        log.clear();
        assert!(log.events.is_empty());
    }

    #[test]
    fn csv_header_is_exact() {
        let log = EventLog::default();
        let csv = log.to_csv();
        assert!(csv.starts_with("frame,scanline,dot,cycle,kind,addr,val\n"));
        // header, then nothing
        assert_eq!(csv.lines().count(), 1);
    }

    #[test]
    fn csv_line_format() {
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        log.push(ev(0, 10, 20, 100, EventKind::Write, 0x2001, 0x1E));
        let csv = log.to_csv();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "frame,scanline,dot,cycle,kind,addr,val");
        assert_eq!(lines[1], "0,10,20,100,Write,$2001,$1E");
    }

    #[test]
    fn event_map_shape_and_dot() {
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        log.push(ev(1, 10, 20, 100, EventKind::Write, 0x2001, 0x1E));
        let img = render_event_map(&log, 1, EVENT_MAP_HEIGHT);
        assert_eq!(img.len(), 341 * 262 * 3);
        let i = (10 * 341 + 20) * 3;
        assert_eq!(&img[i..i + 3], &[255, 0, 0]);
        // no other frame's dot is drawn
        let img0 = render_event_map(&log, 0, EVENT_MAP_HEIGHT);
        assert!(img0.iter().all(|&b| b == 0));
    }

    #[test]
    fn event_map_colours() {
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        let cases: [(BusEvent, [u8; 3]); 6] = [
            (ev(0, 1, 1, 0, EventKind::Read, 0x2002, 0x80), [0, 255, 0]),
            (ev(0, 2, 2, 0, EventKind::Read, 0x4015, 0), [0, 0, 255]),
            (ev(0, 3, 3, 0, EventKind::Write, 0x4017, 0), [0, 0, 255]),
            (ev(0, 4, 4, 0, EventKind::Write, 0x8000, 0), [255, 255, 0]),
            (ev(0, 5, 5, 0, EventKind::Nmi, 0, 0), [255, 255, 255]),
            (ev(0, 6, 6, 0, EventKind::Irq, 0, 0), [255, 0, 255]),
        ];
        for (e, rgb) in cases {
            log.events.clear();
            log.events.push(e);
            let img = render_event_map(&log, 0, EVENT_MAP_HEIGHT);
            let i = (usize::from(e.scanline) * 341 + usize::from(e.dot)) * 3;
            assert_eq!(&img[i..i + 3], &rgb, "kind {:?}", e.kind);
        }
        for kind in [EventKind::OamDma, EventKind::DmcDma] {
            log.events.clear();
            log.events.push(ev(0, 7, 7, 0, kind, 0x2003, 0));
            let img = render_event_map(&log, 0, EVENT_MAP_HEIGHT);
            let i = (7 * 341 + 7) * 3;
            assert_eq!(&img[i..i + 3], &[0, 255, 255], "{:?}", kind);
        }
    }

    #[test]
    fn out_of_range_dots_are_ignored() {
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        log.push(ev(0, 300, 400, 0, EventKind::Write, 0x2001, 0));
        assert_eq!(
            render_event_map(&log, 0, EVENT_MAP_HEIGHT).len(),
            341 * 262 * 3
        );
    }

    #[test]
    fn pal_dendy_event_map_keeps_lines_262_to_311() {
        // A PAL or Dendy frame has 312 scanlines. Events on 262..=311 (notably Dendy's vblank start at
        // 291) used to be dropped because the map was fixed at 262 rows.
        let mut log = EventLog {
            enabled: true,
            events: Vec::new(),
        };
        log.push(ev(0, 262, 3, 1, EventKind::Write, 0x2001, 0x1E));
        log.push(ev(0, 291, 1, 2, EventKind::Nmi, 0, 0));
        log.push(ev(0, 311, 340, 3, EventKind::Write, 0x2007, 0x00));
        log.push(ev(0, 312, 0, 4, EventKind::Write, 0x2001, 0x1E)); // off the end of the map
        let img = render_event_map(&log, 0, 312);
        assert_eq!(img.len(), 341 * 312 * 3);
        let at = |sl: usize, dot: usize| {
            let i = (sl * 341 + dot) * 3;
            [img[i], img[i + 1], img[i + 2]]
        };
        assert_eq!(at(262, 3), [255, 0, 0], "line 262 write");
        // Dendy's vblank/NMI at line 291.
        assert_eq!(at(291, 1), [255, 255, 255], "line 291 NMI");
        assert_eq!(at(311, 340), [255, 0, 0], "last scanline");
        assert_eq!(at(0, 0), [0, 0, 0], "line 312 is outside the frame");
    }
}
