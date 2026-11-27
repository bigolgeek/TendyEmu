//! RAM search and RAM watch: find a variable by watching what its byte does, the way
//! FCEUX's RAM Search (https://fceux.com/web/help/RAMSearch.html) and the memory tools in
//! Mesen/RustyNES expose the 2KB work RAM.
//!
//! Both tools sit on the same read-only path: [`SysBus::peek`](crate::cpu::Bus::peek), the bus's
//! documented side-effect-free byte read ("for debugger snapshots and disassembly"). It observes
//! RAM and never clocks the bus, so a frontend can call it between frames without perturbing
//! CPU/PPU/APU timing. (`Bus::read` would be the wrong call here - it advances the machine, which
//! is exactly what a debugger must not do.) Search steps take plain slices, so a frontend can also
//! search a snapshot it kept from an earlier frame.
//!
//! * [`search_between`] / [`narrow_between`] filter addresses by what their byte does: equal to,
//!   below, or above a target value, changed, or unchanged. Start broad (first snapshot, "equal to
//!   the number of lives you can see"), then narrow each later snapshot until one address is left,
//!   the interactive loop FCEUX's RAM Search drives. This is the one RAM search in the crate:
//!   the raw-RAM cheat tools in [`crate::cheats`] drive the same [`Condition`] and the same
//!   narrowing step over a snapshot they kept, instead of a second vocabulary of their own.
//! * [`RamWatch`] holds a list of addresses, re-reads them on every [`RamWatch::poll`] (one poll
//!   per frame) and formats the list as one console line, marking the bytes that moved.
//!
//! Neither tool keeps state inside the emulator: a snapshot is an `&[u8]` the caller owns, and a
//! watch list belongs to the frontend session, so nothing here touches save states.

use std::fmt;

use crate::bus::SysBus;
use crate::cpu::Bus;

/// What a search step asks of one address, comparing the same address across two snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// The address holds this value now (`== target`).
    Equal(u8),
    /// The address holds a value below this one now (`< target`).
    Less(u8),
    /// The address holds a value above this one now (`> target`).
    Greater(u8),
    /// The value differs between the two snapshots.
    Changed,
    /// The value is the same in both snapshots (`not changed`).
    NotChanged,
}

impl Condition {
    /// Whether this condition looks at the earlier snapshot at all. `Equal`, `Less` and `Greater`
    /// do not, so the first step of a search can be run against a single snapshot.
    pub fn needs_previous(self) -> bool {
        matches!(self, Condition::Changed | Condition::NotChanged)
    }

    /// The predicate itself: `prev` is the byte in the earlier snapshot, `now` the later one. The
    /// one comparison match, shared by a full search and every narrowing step.
    fn holds(self, prev: u8, now: u8) -> bool {
        match self {
            Condition::Equal(target) => now == target,
            Condition::Less(target) => now < target,
            Condition::Greater(target) => now > target,
            Condition::Changed => now != prev,
            Condition::NotChanged => now == prev,
        }
    }
}

/// A first or full search step: every address whose byte satisfies `condition` across `prev` and
/// `now`, in ascending address order. The two snapshots are indexed by RAM address (`$0000` onward,
/// i.e. an index into the 2KB page); only the addresses both snapshots cover are searched, so a
/// short synthetic page in a test cannot index out of bounds.
pub fn search_between(prev: &[u8], now: &[u8], condition: Condition) -> Vec<u16> {
    let len = prev.len().min(now.len());
    let mut hits = Vec::new();
    for idx in 0..len {
        if condition.holds(prev[idx], now[idx]) {
            hits.push(idx as u16);
        }
    }
    hits
}

/// A narrowing step: keep the candidates whose byte satisfies `condition` across `prev` and `now`.
/// Candidates keep the order they were given in, so a list built by [`search_between`] (ascending)
/// stays ascending through the whole search. Addresses outside the snapshots are dropped.
pub fn narrow_between(
    prev: &[u8],
    now: &[u8],
    candidates: &[u16],
    condition: Condition,
) -> Vec<u16> {
    let len = prev.len().min(now.len());
    let mut hits = Vec::new();
    for &addr in candidates {
        let idx = usize::from(addr);
        if idx < len && condition.holds(prev[idx], now[idx]) {
            hits.push(addr);
        }
    }
    hits
}

/// One watched address: the byte from the latest [`RamWatch::poll`] and, once the address has been
/// read twice, the byte before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchRow {
    /// The address as displayed, normalised into `$0000-$07FF` (`$0810` and `$0010` are one row).
    addr: u16,
    /// The byte at `addr` in the latest poll; 0 before the first poll.
    value: u8,
    /// The byte at `addr` in the poll before the latest one. `None` until it has been read twice,
    /// so the first reading is never reported as a change.
    previous: Option<u8>,
    /// Whether any poll has read this row yet (the watch may gain addresses mid-session).
    seen: bool,
}

impl WatchRow {
    /// The normalised address (`$0000-$07FF`).
    pub fn addr(&self) -> u16 {
        self.addr
    }

    /// The byte at the address in the latest poll.
    pub fn value(&self) -> u8 {
        self.value
    }

    /// The byte from the previous poll, if this address has been read at least twice.
    pub fn previous(&self) -> Option<u8> {
        self.previous
    }

    /// Whether the latest poll differs from the one before it.
    pub fn changed(&self) -> bool {
        self.previous.is_some_and(|prev| prev != self.value)
    }

    /// Record a fresh reading. The old value moves into `previous` only from the second sample
    /// on, so an address added mid-session never reports its first reading as a change.
    fn sample(&mut self, byte: u8) {
        self.previous = if self.seen { Some(self.value) } else { None };
        self.value = byte;
        self.seen = true;
    }
}

/// A list of watched addresses, re-read on demand. The frontend polls it once per emulated frame
/// and prints it; there is no in-window widget here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RamWatch {
    rows: Vec<WatchRow>,
}

impl RamWatch {
    /// An empty watch list.
    pub fn new() -> RamWatch {
        RamWatch { rows: Vec::new() }
    }

    /// Start watching `addr` (any RAM mirror: `$0010`, `$0810`, `$1010`, `$1810` are the same
    /// byte). Returns true when it was added: false when it was already on the list, or when the
    /// address is outside work RAM (`$0000-$1FFF`), which the shared
    /// [`SysBus::ram_index`](crate::bus::SysBus::ram_index) rejects - masking `$2004` into the 2KB
    /// page would silently watch a different guest address.
    pub fn add(&mut self, addr: u16) -> bool {
        let Some(idx) = SysBus::ram_index(addr) else {
            return false;
        };
        let addr = idx as u16;
        if self.rows.iter().any(|row| row.addr == addr) {
            return false;
        }
        self.rows.push(WatchRow {
            addr,
            value: 0,
            previous: None,
            seen: false,
        });
        true
    }

    /// Stop watching `addr` (in any of its mirrors); returns true when it had been watched.
    pub fn remove(&mut self, addr: u16) -> bool {
        let Some(idx) = SysBus::ram_index(addr) else {
            return false;
        };
        let addr = idx as u16;
        let before = self.rows.len();
        self.rows.retain(|row| row.addr != addr);
        self.rows.len() != before
    }

    /// Drop every watched address.
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many addresses are watched.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// The watched addresses, in the order they were added.
    pub fn addrs(&self) -> impl Iterator<Item = u16> + '_ {
        self.rows.iter().map(WatchRow::addr)
    }

    /// The current readings, in the order the addresses were added.
    pub fn rows(&self) -> &[WatchRow] {
        &self.rows
    }

    /// Re-read every watched address through the bus's side-effect-free
    /// [`peek`](crate::cpu::Bus::peek) - the public read API a debugger may use between frames.
    /// Call once per frame: that repetition is what makes the list a per-frame view. A row the bus
    /// will not peek keeps its last reading.
    pub fn poll(&mut self, bus: &mut SysBus) -> &[WatchRow] {
        for row in &mut self.rows {
            if let Some(byte) = bus.peek(row.addr) {
                row.sample(byte);
            }
        }
        &self.rows
    }

    /// The same poll over a page indexed by RAM address: a `cheat_snapshot` kept from an earlier
    /// frame, or a synthetic page in a test. Rows the slice does not cover keep their last reading.
    pub fn poll_snapshot(&mut self, page: &[u8]) -> &[WatchRow] {
        for row in &mut self.rows {
            if let Some(&byte) = page.get(usize::from(row.addr)) {
                row.sample(byte);
            }
        }
        &self.rows
    }

    /// Whether any address moved since the previous poll.
    pub fn any_changed(&self) -> bool {
        self.rows.iter().any(WatchRow::changed)
    }
}

impl fmt::Display for RamWatch {
    /// One line, `$0043=07 $0044=2A*`: `$` and the address in uppercase hex, the value as two hex
    /// digits, and a `*` on every byte that moved since the previous poll.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, row) in self.rows.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            let mark = if row.changed() { '*' } else { ' ' };
            write!(f, "${:04X}={:02X}{mark}", row.addr, row.value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic 16-byte page: index = address, so a test can name the byte it changes.
    fn page(bytes: [u8; 16]) -> Vec<u8> {
        bytes.to_vec()
    }

    fn watched(watch: &RamWatch) -> Vec<(u16, u8)> {
        watch
            .rows()
            .iter()
            .map(|row| (row.addr(), row.value()))
            .collect()
    }

    #[test]
    fn equal_search_returns_every_address_holding_the_value() {
        let now = page([5, 0, 5, 1, 5, 0, 5, 1, 5, 0, 5, 1, 5, 0, 5, 1]);
        assert_eq!(
            search_between(&now, &now, Condition::Equal(5)),
            vec![0, 2, 4, 6, 8, 10, 12, 14]
        );
        assert_eq!(
            search_between(&now, &now, Condition::Equal(1)),
            vec![3, 7, 11, 15]
        );
        assert!(search_between(&now, &now, Condition::Equal(9)).is_empty());
    }

    #[test]
    fn changed_search_returns_only_the_bytes_that_moved() {
        let before = page([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let after = page([0, 9, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(search_between(&before, &after, Condition::Changed), vec![1]);
        // 9 -> 9 did not move even though the value is 9 in both.
        assert_eq!(
            search_between(&before, &after, Condition::NotChanged),
            vec![0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn a_search_narrows_in_steps_like_the_interactive_loop() {
        // A lives counter at $06 counts down 3 -> 2 -> 1 while a score at $0C climbs and a
        // frame timer at $0A flips every frame.
        let first = page([0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 7, 0, 0, 0, 0, 0]);
        let second = page([0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 8, 0, 0, 0, 0, 0]);
        let third = page([0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 9, 0, 0, 0, 0, 0]);

        let candidates = search_between(&first, &first, Condition::Equal(3));
        assert_eq!(candidates, vec![6]);
        let candidates = narrow_between(&first, &second, &candidates, Condition::Changed);
        assert_eq!(candidates, vec![6]);
        let candidates = narrow_between(&second, &third, &candidates, Condition::Equal(1));
        assert_eq!(candidates, vec![6]);
    }

    #[test]
    fn narrow_keeps_the_candidate_order_and_drops_bad_addresses() {
        let before = page([7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7]);
        let after = page([1, 7, 2, 7, 3, 7, 4, 7, 5, 7, 6, 7, 7, 7, 8, 7]);
        // Candidates deliberately out of address order, plus two outside the snapshot.
        let candidates = [9u16, 2, 0, 40, 6, 4];
        assert_eq!(
            narrow_between(&before, &after, &candidates, Condition::Changed),
            vec![2, 0, 6, 4]
        );
        // `not changed` keeps the one that stayed at 7 - including the out-of-range 40? No:
        // an address the snapshot cannot cover is dropped whatever the condition.
        assert_eq!(
            narrow_between(&before, &after, &candidates, Condition::NotChanged),
            vec![9]
        );
    }

    #[test]
    fn mismatched_snapshot_lengths_search_the_common_prefix() {
        let before = vec![1u8, 2, 3, 4];
        let after = vec![1u8, 9, 3];
        assert_eq!(search_between(&before, &after, Condition::Changed), vec![1]);
        assert_eq!(
            search_between(&before, &after, Condition::Equal(3)),
            vec![2]
        );
        // Address 3 exists only in the longer snapshot.
        assert_eq!(
            narrow_between(&before, &after, &[3], Condition::NotChanged),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn only_changed_and_not_changed_need_an_earlier_snapshot() {
        assert!(!Condition::Equal(0).needs_previous());
        assert!(!Condition::Less(0).needs_previous());
        assert!(!Condition::Greater(0).needs_previous());
        assert!(Condition::Changed.needs_previous());
        assert!(Condition::NotChanged.needs_previous());
    }

    /// The three target relations the cheat search used to own: one `Condition`, one
    /// predicate, over the whole snapshot.
    #[test]
    fn equal_less_and_greater_pin_every_relation() {
        // Synthetic snapshot: ram[a] = a & 0xFF makes every relation easy to pin.
        let ram: Vec<u8> = (0..0x100u32).map(|a| a as u8).collect();
        // == 0x20: only one address.
        assert_eq!(
            search_between(&ram, &ram, Condition::Equal(0x20)),
            vec![0x0020]
        );
        // < 0x03: the first three addresses, nothing between them.
        assert_eq!(
            search_between(&ram, &ram, Condition::Less(0x03)),
            vec![0x0000, 0x0001, 0x0002]
        );
        // > 0xFD: the top two.
        assert_eq!(
            search_between(&ram, &ram, Condition::Greater(0xFD)),
            vec![0x00FE, 0x00FF]
        );
        // The target itself sits in neither open interval, so 0x20 is in neither list.
        let less = search_between(&ram, &ram, Condition::Less(0x20));
        let greater = search_between(&ram, &ram, Condition::Greater(0x20));
        assert!(!less.contains(&0x0020));
        assert!(!greater.contains(&0x0020));
        assert_eq!(less.len(), 0x20);
        assert_eq!(greater.len(), 0xFF - 0x20);
        // An impossible value leaves the candidate list empty (a valid search step).
        assert_eq!(
            search_between(&[0u8; 0x0800], &[0u8; 0x0800], Condition::Equal(0x01)),
            Vec::<u16>::new()
        );
    }

    /// A target relation narrows a candidate list between snapshots, like every other condition.
    #[test]
    fn a_target_relation_narrows_a_candidate_list_between_snapshots() {
        // Frame N: three addresses hold the live value.
        let frame_n = [0x00, 0x40, 0x40, 0x40, 0xFF];
        let candidates = search_between(&frame_n, &frame_n, Condition::Equal(0x40));
        assert_eq!(candidates, vec![1, 2, 3]);
        // Frame N+1: the real live value moved on; only one candidate followed it.
        let frame_n1 = [0x00, 0x41, 0x09, 0x40, 0xFF];
        let kept = narrow_between(&frame_n, &frame_n1, &candidates, Condition::Equal(0x41));
        assert_eq!(kept, vec![1]);
        // A further Less step narrows again with the same list shape.
        assert_eq!(
            narrow_between(&frame_n1, &frame_n1, &kept, Condition::Less(0x80)),
            vec![1]
        );
    }

    /// An address the snapshots cannot cover is dropped whatever the condition.
    #[test]
    fn narrowing_drops_addresses_outside_the_snapshot() {
        let ram = [0x00u8; 4];
        let hits = narrow_between(&ram, &ram, &[0, 1, 0x0800], Condition::Equal(0x00));
        assert_eq!(hits, vec![0, 1]);
    }

    #[test]
    fn adding_mirrors_watches_one_row() {
        let mut watch = RamWatch::new();
        assert!(watch.add(0x0010));
        assert!(!watch.add(0x0810));
        assert!(!watch.add(0x1010));
        assert!(!watch.add(0x1810));
        assert_eq!(watch.addrs().collect::<Vec<_>>(), vec![0x0010]);
        assert_eq!(watch.len(), 1);
    }

    #[test]
    fn addresses_outside_work_ram_are_refused_rather_than_masked() {
        let mut watch = RamWatch::new();
        // $2004 must not fold onto $0004: that is the PPU's OAMDATA, not RAM.
        assert!(!watch.add(0x2004));
        assert!(!watch.add(0x4014));
        assert!(!watch.add(0x8000));
        assert!(watch.is_empty());
        assert!(!watch.remove(0x2004));
    }

    #[test]
    fn the_first_poll_reports_no_change() {
        let mut watch = RamWatch::new();
        watch.add(0x0004);
        let page = [0, 0, 0, 0, 0x2A, 0, 0, 0];
        watch.poll_snapshot(&page);
        let row = watch.rows()[0];
        assert_eq!(row.value(), 0x2A);
        assert_eq!(row.previous(), None);
        assert!(!row.changed());
        assert!(!watch.any_changed());
    }

    #[test]
    fn later_polls_flag_only_the_bytes_that_moved() {
        let mut watch = RamWatch::new();
        watch.add(0x0001);
        watch.add(0x0002);
        watch.poll_snapshot(&[0, 0x11, 0x22, 0]);
        watch.poll_snapshot(&[0, 0x11, 0x23, 0]);
        assert!(!watch.rows()[0].changed());
        assert!(watch.rows()[1].changed());
        assert_eq!(watch.rows()[1].previous(), Some(0x22));
        assert_eq!(watch.rows()[1].value(), 0x23);
        assert!(watch.any_changed());
        // A poll with no movement clears the mark.
        watch.poll_snapshot(&[0, 0x11, 0x23, 0]);
        assert!(!watch.any_changed());
    }

    #[test]
    fn display_marks_moved_bytes_and_pads_clean_ones() {
        let mut watch = RamWatch::new();
        watch.add(0x0043);
        watch.add(0x0044);
        watch.poll_snapshot(&[0; 0x100]);
        watch.poll_snapshot(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // Unchanged bytes keep their trailing slot, so the columns line up frame to frame.
        assert_eq!(watch.to_string(), "$0043=00  $0044=00 ");
    }

    #[test]
    fn display_shows_the_new_value_with_a_star() {
        let mut watch = RamWatch::new();
        watch.add(0x0001);
        let mut page = vec![0u8; 8];
        watch.poll_snapshot(&page);
        page[1] = 0x2A;
        watch.poll_snapshot(&page);
        assert_eq!(watch.to_string(), "$0001=2A*");
    }

    #[test]
    fn rows_added_after_the_first_poll_start_unmarked() {
        let mut watch = RamWatch::new();
        watch.add(0x0000);
        let mut page = vec![0u8; 8];
        watch.poll_snapshot(&page);
        page[3] = 0x55;
        watch.poll_snapshot(&page);
        watch.add(0x0003);
        // A brand-new row has not been read twice, so it cannot be a change yet.
        assert!(!watch.any_changed());
        watch.poll_snapshot(&page);
        assert!(!watch.any_changed());
        page[3] = 0x56;
        watch.poll_snapshot(&page);
        assert!(watch.rows()[1].changed());
    }

    #[test]
    fn removing_and_clearing_addresses_works_through_mirrors() {
        let mut watch = RamWatch::new();
        watch.add(0x0004);
        watch.add(0x0006);
        assert!(watch.remove(0x0804));
        assert!(!watch.remove(0x0804));
        assert_eq!(watch.addrs().collect::<Vec<_>>(), vec![0x0006]);
        watch.clear();
        assert!(watch.is_empty());
        assert_eq!(watched(&watch), Vec::new());
    }
}
