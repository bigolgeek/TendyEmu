//! Symbol import: ca65 `.dbg` and FCEUX/ASM6 `.nl` label files.
//!
//! A `.dbg` file (cc65 `ld65 --dbgfile`) is not XML: one `record<TAB>key=val,key=val,...` per
//! line, numbers in hex. A `sym` (a label) is kept only when `type=lab` and `val=` is present,
//! `type=imp` (it carries `exp=` and no `val`) and `type=equ` are skipped, and every other record
//! family (`seg`, `info`, `version`, ...) is ignored. `val` is the resolved absolute linked
//! address and is used as the address directly; `seg=` is informational only and never enters the
//! address math. The source of truth is cc65's `src/ld65/dbgsyms.c` (`PrintDbgSyms`), which emits
//! `val=0x%lX` from `GetDbgSymVal` — the fully-resolved value — and writes `seg=<id>` purely as an
//! attribute of that symbol, so interpreting `val` as segment-relative misdecodes real builds
//! (a WRAM label `val=0x6010` in a segment starting at 0x6000 is at 0x6010, not 0xC010).
//! A `.nl` file is one `$AAAA#NAME#comment` line per label: the comment is dropped, and lines not
//! starting with `$`, labels with an empty name, and blanks are skipped. asm6f's `-n` option
//! writes this same FCEUX format (`<rom>.0.nl`, `<rom>.ram.nl`), so its output loads unchanged.
//! [`Symbols`] is sorted by
//! address and deduped (first record wins), so [`Symbols::name_at`] is a binary search.
use std::path::PathBuf;

/// A sorted, deduped address -> label table.
#[derive(Debug, Clone, Default)]
pub struct Symbols {
    pub labels: Vec<(u16, String)>,
}

impl Symbols {
    /// Parses `.dbg` text. A bad line is skipped with a stderr warning `<dbg>:<line>: ...`.
    pub fn parse_dbg(text: &str) -> Result<Symbols, String> {
        let (s, w) = parse_dbg_named("<dbg>", text);
        report(&w);
        Ok(s)
    }

    /// Parses `.nl` text. A bad line is skipped with a stderr warning `<nl>:<line>: ...`.
    pub fn parse_nl(text: &str) -> Result<Symbols, String> {
        let (s, w) = parse_nl_named("<nl>", text);
        report(&w);
        Ok(s)
    }

    /// Loads `.dbg`/`.nl` files by extension and merges them: append, sort, dedupe. Only an
    /// unknown extension or an unreadable file is an `Err`; bad lines warn and are skipped.
    pub fn load(files: &[PathBuf]) -> Result<Symbols, String> {
        let mut labels = Vec::new();
        for path in files {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !matches!(ext.as_str(), "dbg" | "nl") {
                return Err(format!(
                    "{name}: unknown symbol file extension, expected .dbg or .nl"
                ));
            }
            let text = std::fs::read_to_string(path).map_err(|e| format!("{name}: {e}"))?;
            let (part, warnings) = if ext == "dbg" {
                parse_dbg_named(&name, &text)
            } else {
                parse_nl_named(&name, &text)
            };
            report(&warnings);
            labels.extend(part.labels);
        }
        Ok(Symbols {
            labels: sorted(labels),
        })
    }

    /// The label exactly at `addr`, if one is defined there.
    pub fn name_at(&self, addr: u16) -> Option<&str> {
        self.labels
            .binary_search_by_key(&addr, |(a, _)| *a)
            .ok()
            .map(|i| self.labels[i].1.as_str())
    }
}

/// Stable sort by address, then drop repeats so the first record of an address keeps its name.
fn sorted(mut labels: Vec<(u16, String)>) -> Vec<(u16, String)> {
    labels.sort_by_key(|(a, _)| *a);
    labels.dedup_by_key(|(a, _)| *a);
    labels
}

/// `0x8000` or a bare `8000` as a number.
fn hex(s: &str) -> Option<u32> {
    let s = s.trim();
    u32::from_str_radix(
        s.strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s),
        16,
    )
    .ok()
}

/// The `key=val` pairs of one record, with spaces trimmed.
fn fields(rest: &str) -> Vec<(&str, &str)> {
    rest.split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
}

fn get<'a>(fields: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    fields.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Most distinct per-line warnings printed per parse; the rest collapse into "and N more".
const MAX_WARNINGS: usize = 10;

/// Prints the per-line warnings of one parse to stderr: each distinct line once, capped.
fn report(warnings: &[String]) {
    for w in warnings.iter().take(MAX_WARNINGS) {
        eprintln!("warning: {w}; skipped");
    }
    if warnings.len() > MAX_WARNINGS {
        eprintln!(
            "warning: and {} more bad symbol lines skipped",
            warnings.len() - MAX_WARNINGS
        );
    }
}

/// One `.dbg` `sym` record: `Ok(None)` for imports/equates, `Err` for a line that must be skipped.
fn dbg_label(name: &str, n: usize, f: &[(&str, &str)]) -> Result<Option<(u16, String)>, String> {
    match get(f, "type").unwrap_or("") {
        "lab" => {}
        "imp" | "equ" => return Ok(None),
        other => return Err(format!("{name}:{n}: sym with unknown type {other:?}")),
    }
    let val = get(f, "val")
        .and_then(hex)
        .ok_or_else(|| format!("{name}:{n}: sym without val"))?;
    let label = get(f, "name").ok_or_else(|| format!("{name}:{n}: sym without name"))?;
    // `val` is the resolved absolute linked address; `seg=` is only an attribute. An address
    // the 6502 cannot reach is skipped, not a label truncated onto another instruction.
    let addr = u16::try_from(val)
        .map_err(|_| format!("{name}:{n}: sym address {val:#x} does not fit in 16 bits"))?;
    Ok(Some((addr, label.trim_matches('"').to_string())))
}

/// Parses `.dbg` text; bad lines are collected as warnings and skipped, never an error.
fn parse_dbg_named(name: &str, text: &str) -> (Symbols, Vec<String>) {
    let (mut labels, mut warnings) = (Vec::new(), Vec::new());
    for (i, line) in text.lines().enumerate() {
        let Some((rec, rest)) = line.split_once('\t') else {
            continue;
        };
        if rec != "sym" {
            continue;
        }
        match dbg_label(name, i + 1, &fields(rest)) {
            Ok(Some(l)) => labels.push(l),
            Ok(None) => {}
            Err(w) => warnings.push(w),
        }
    }
    (
        Symbols {
            labels: sorted(labels),
        },
        warnings,
    )
}

fn nl_label(name: &str, n: usize, line: &str) -> Result<Option<(u16, String)>, String> {
    let addr = line
        .get(1..5)
        .and_then(hex)
        .ok_or_else(|| format!("{name}:{n}: .nl address is not 4 hex digits"))?;
    let label = line
        .get(5..)
        .and_then(|s| s.strip_prefix('#'))
        .ok_or_else(|| format!("{name}:{n}: .nl address is not followed by '#'"))?;
    let label = label.split('#').next().unwrap().trim();
    Ok((!label.is_empty()).then(|| (addr as u16, label.to_string())))
}

/// Parses `.nl` text; bad lines are collected as warnings and skipped, never an error.
fn parse_nl_named(name: &str, text: &str) -> (Symbols, Vec<String>) {
    let (mut labels, mut warnings) = (Vec::new(), Vec::new());
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if !line.starts_with('$') {
            continue;
        }
        match nl_label(name, i + 1, line) {
            Ok(Some(l)) => labels.push(l),
            Ok(None) => {}
            Err(w) => warnings.push(w),
        }
    }
    (
        Symbols {
            labels: sorted(labels),
        },
        warnings,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &Symbols) -> Vec<(u16, &str)> {
        s.labels.iter().map(|(a, n)| (*a, n.as_str())).collect()
    }

    #[test]
    fn records_parse_in_any_key_order() {
        let a = Symbols::parse_dbg(
            "seg\tid=1,start=0x8000\nsym\tid=1,name=\"main\",val=0x8010,seg=1,type=lab\n",
        )
        .unwrap();
        let b = Symbols::parse_dbg(
            "seg\tstart=0x8000,id=1\nsym\ttype=lab,seg=1,val=0x8010,name=\"main\"\n",
        )
        .unwrap();
        assert_eq!(names(&a), vec![(0x8010, "main")]);
        assert_eq!(names(&b), names(&a));
    }

    #[test]
    fn imports_and_equates_are_skipped() {
        let text = "sym\tid=4,name=\"extern_font\",type=imp,exp=9\nsym\tid=5,name=\"SCREEN\",val=0x8000,type=equ\nsym\tid=6,name=\"main\",val=0x8000,type=lab\n";
        assert_eq!(
            names(&Symbols::parse_dbg(text).unwrap()),
            vec![(0x8000, "main")]
        );
    }

    /// Regression: cc65 emits `val` as the linked address, so a WRAM segment start is
    /// not an addend. `start=0x6000` + `val=0x6010` must resolve to 0x6010, never 0xC010.
    #[test]
    fn wram_segment_value_is_the_address_itself() {
        let text = "seg\tid=3,name=\"WRAM\",start=0x6000,size=0x2000,addrsize=abs,type=rw\n\
                    sym\tid=7,name=\"player_x\",addrsize=abs,size=1,scope=1,val=0x6010,seg=3,type=lab\n";
        let s = Symbols::parse_dbg(text).unwrap();
        assert_eq!(names(&s), vec![(0x6010, "player_x")]);
        assert_eq!(
            s.name_at(0xC010),
            None,
            "seg.start + val would misdecode this"
        );
    }

    /// The same regression in a low RAM segment: it must not be added twice either.
    #[test]
    fn ram_segment_value_is_not_double_counted() {
        let text = "seg\tid=4,name=\"RAM\",start=0x0200,size=0x0100,addrsize=abs,type=rw\n\
                    sym\tid=8,name=\"frame_ready\",addrsize=abs,size=1,scope=1,val=0x0220,seg=4,type=lab\n";
        let s = Symbols::parse_dbg(text).unwrap();
        assert_eq!(names(&s), vec![(0x0220, "frame_ready")]);
        assert_eq!(
            s.name_at(0x0440),
            None,
            "seg.start + val would misdecode this"
        );
    }

    #[test]
    fn zeropage_segment_and_absolute_values() {
        let text = "seg\tid=1,start=0x8000,addrsize=abs\nseg\tid=2,start=0x0000,addrsize=zeropage\nsym\tname=\"screen_x\",val=0x0010,seg=2,type=lab\nsym\tname=\"reset\",val=FFFC,seg=1,type=lab\n";
        assert_eq!(
            names(&Symbols::parse_dbg(text).unwrap()),
            vec![(0x0010, "screen_x"), (0xFFFC, "reset")]
        );
    }

    #[test]
    fn nl_drops_comments_bad_lines_and_repeats() {
        let text = "$8000#main#\n$8020#irq_handler#called every vblank\nnot a label\n$0010##\n$8000#again#\n";
        assert_eq!(
            names(&Symbols::parse_nl(text).unwrap()),
            vec![(0x8000, "main"), (0x8020, "irq_handler")]
        );
    }

    #[test]
    fn unknown_extension_is_an_error() {
        let e = Symbols::load(&[PathBuf::from("labels.txt")]).unwrap_err();
        assert!(e.contains("unknown symbol file extension"), "{e}");
    }

    #[test]
    fn missing_val_is_skipped_with_its_line() {
        let (s, w) = parse_dbg_named(
            "t.dbg",
            "seg\tid=1,start=0x8000\nsym\tid=5,name=\"broken\",seg=1,type=lab\n",
        );
        assert!(s.labels.is_empty());
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("t.dbg:2: sym without val"), "{w:?}");
    }

    /// `val as u16` put a label at 0x18000 on the instruction at 0x8000. The
    /// label is skipped with a warning, never truncated, and the parse still succeeds.
    #[test]
    fn address_above_ffff_warns_and_is_skipped() {
        let text = "sym\tid=1,name=\"bank2_main\",val=0x18000,type=lab\nsym\tid=2,name=\"main\",val=0x8000,type=lab\n";
        let (_, w) = parse_dbg_named("t.dbg", text);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains(":1: sym address 0x18000 does not fit in 16 bits"),
            "{w:?}"
        );
        assert_eq!(
            names(&Symbols::parse_dbg(text).unwrap()),
            vec![(0x8000, "main")]
        );
    }

    /// Two good symbols around one bad address: both resolve, the bad one is absent.
    #[test]
    fn mixed_file_keeps_good_labels_and_drops_the_bad_one() {
        let text = "sym\tid=1,name=\"reset\",val=0x8000,type=lab\n\
                    sym\tid=2,name=\"bank2_main\",val=0x18000,type=lab\n\
                    sym\tid=3,name=\"nmi\",val=0x8100,type=lab\n";
        let s = Symbols::parse_dbg(text).expect("a bad line is not an error");
        assert_eq!(s.name_at(0x8000), Some("reset"));
        assert_eq!(s.name_at(0x8100), Some("nmi"));
        assert!(
            s.labels.iter().all(|(_, n)| n != "bank2_main"),
            "{:?}",
            s.labels
        );
        assert_eq!(s.labels.len(), 2);
    }
}
