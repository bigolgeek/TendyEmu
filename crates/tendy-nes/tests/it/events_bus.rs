use tendy_nes::events::EventKind;
use tendy_nes::nes::Nes;

/// 16 KB NROM: SEI; LDA #$80; STA $2000; LDA #$1E; STA $2001; JMP self. NMI handler is a lone RTI.
fn tiny_rom() -> Vec<u8> {
    let mut v = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut prg = vec![0xEAu8; 0x4000];
    let code = [
        0x78, 0xA9, 0x80, 0x8D, 0x00, 0x20, 0xA9, 0x1E, 0x8D, 0x01, 0x20, 0x4C, 0x0B, 0x80,
    ];
    prg[..code.len()].copy_from_slice(&code);
    prg[0x100] = 0x40; // RTI at $8100
    prg[0x3FFA] = 0x00; // NMI -> $8100
    prg[0x3FFB] = 0x81;
    prg[0x3FFC] = 0x00; // RESET -> $8000
    prg[0x3FFD] = 0x80;
    v.extend(prg);
    v.extend(vec![0u8; 0x2000]);
    v
}

#[test]
fn records_writes_and_nmi() {
    let mut nes = Nes::new(&tiny_rom()).unwrap();
    nes.bus.events.enabled = true;
    nes.run_frame();
    nes.run_frame();
    let ev = &nes.bus.events.events;
    let w2000 = ev
        .iter()
        .position(|e| e.kind == EventKind::Write && e.addr == 0x2000 && e.val == 0x80);
    let w2001 = ev
        .iter()
        .position(|e| e.kind == EventKind::Write && e.addr == 0x2001 && e.val == 0x1E);
    assert!(w2000.unwrap() < w2001.unwrap());
    assert!(ev.windows(2).all(|w| w[0].cycle <= w[1].cycle));
    assert!(
        ev.iter()
            .any(|e| e.kind == EventKind::Nmi && e.scanline == 241)
    );
}

#[test]
fn disabled_records_nothing() {
    let mut nes = Nes::new(&tiny_rom()).unwrap();
    nes.run_frame();
    nes.run_frame();
    assert!(nes.bus.events.events.is_empty());
}
