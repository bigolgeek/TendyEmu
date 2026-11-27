use tendy_nes::cpu::*;

fn setup(prog: &[u8]) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.mem[0x8000..0x8000 + prog.len()].copy_from_slice(prog);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;
    (cpu, bus)
}

#[test]
fn lda_imm_zero() {
    let (mut cpu, mut bus) = setup(&[0xA9, 0x00]);
    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.a, 0);
    assert!(cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_N));
    assert_eq!(cpu.pc, 0x8002);
}

#[test]
fn lda_imm_negative() {
    let (mut cpu, mut bus) = setup(&[0xA9, 0x80]);
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80);
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_Z));
}

#[test]
fn lda_abs() {
    let (mut cpu, mut bus) = setup(&[0xAD, 0x00, 0x02]);
    bus.mem[0x0200] = 0x42;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 4);
    assert_eq!(cpu.a, 0x42);
}

#[test]
fn lda_abx_page_cross_costs_one() {
    let (mut cpu, mut bus) = setup(&[0xBD, 0xFF, 0x02]);
    cpu.x = 1;
    bus.mem[0x0300] = 7;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 5);
    assert_eq!(cpu.a, 7);
}

#[test]
fn ldx_zpy() {
    let (mut cpu, mut bus) = setup(&[0xB6, 0x10]);
    cpu.y = 2;
    bus.mem[0x12] = 0x33;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 4);
    assert_eq!(cpu.x, 0x33);
}

#[test]
fn ldy_imm() {
    let (mut cpu, mut bus) = setup(&[0xA0, 0x7F]);
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x7F);
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn sta_zp_keeps_flags() {
    let (mut cpu, mut bus) = setup(&[0x85, 0x10]);
    cpu.a = 0x55;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert_eq!(bus.mem[0x10], 0x55);
    assert_eq!(cpu.p, 0x24);
}

#[test]
fn sta_abx_has_no_penalty() {
    let (mut cpu, mut bus) = setup(&[0x9D, 0xFF, 0x02]);
    cpu.x = 1;
    cpu.a = 9;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 5);
    assert_eq!(bus.mem[0x0300], 9);
}

#[test]
fn stx_and_sty() {
    let (mut cpu, mut bus) = setup(&[0x86, 0x20, 0x84, 0x21]);
    cpu.x = 1;
    cpu.y = 2;
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x20], 1);
    assert_eq!(bus.mem[0x21], 2);
}

#[test]
fn tax_sets_zero() {
    let (mut cpu, mut bus) = setup(&[0xAA]);
    cpu.a = 0;
    cpu.x = 5;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0);
    assert!(cpu.flag(FLAG_Z));
}

#[test]
fn txs_sets_no_flags() {
    let (mut cpu, mut bus) = setup(&[0x9A]);
    cpu.x = 0;
    cpu.step(&mut bus);
    assert_eq!(cpu.sp, 0);
    assert_eq!(cpu.p, 0x24);
}

#[test]
fn tsx_reads_sp() {
    let (mut cpu, mut bus) = setup(&[0xBA]);
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0xFD);
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn pha_then_pla() {
    let (mut cpu, mut bus) = setup(&[0x48, 0xA9, 0x00, 0x68]);
    cpu.a = 0x80;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);

    cpu.step(&mut bus);
    assert!(cpu.flag(FLAG_Z));

    let c = cpu.step(&mut bus);
    assert_eq!(c, 4);
    assert_eq!(cpu.a, 0x80);
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_Z));
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn php_pushes_b_and_u() {
    let (mut cpu, mut bus) = setup(&[0x08]);
    cpu.p = 0;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert_eq!(bus.mem[0x01FD], 0x30);
    assert_eq!(cpu.sp, 0xFC);
}

#[test]
fn plp_clears_b_sets_u() {
    let (mut cpu, mut bus) = setup(&[0x28]);
    cpu.sp = 0xFC;
    bus.mem[0x01FD] = 0xFF;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 4);
    assert_eq!(cpu.p, 0xEF);
}

#[test]
fn set_flags() {
    let (mut cpu, mut bus) = setup(&[0x38, 0x78, 0xF8]);
    cpu.p = 0x20;
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.p, 0x2D);
}

#[test]
fn clear_flags() {
    let (mut cpu, mut bus) = setup(&[0x18, 0x58, 0xD8]);
    cpu.p = 0x2D;
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.p, 0x20);
}

#[test]
fn clv_clears_overflow() {
    let (mut cpu, mut bus) = setup(&[0xB8]);
    cpu.p = 0x64;
    cpu.step(&mut bus);
    assert_eq!(cpu.p, 0x24);
}

#[test]
fn inc_zp_wraps() {
    let (mut cpu, mut bus) = setup(&[0xE6, 0x10]);
    bus.mem[0x10] = 0xFF;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 5);
    assert_eq!(bus.mem[0x10], 0);
    assert!(cpu.flag(FLAG_Z));
}

#[test]
fn dec_abs_wraps() {
    let (mut cpu, mut bus) = setup(&[0xCE, 0x00, 0x02]);
    bus.mem[0x0200] = 0;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 6);
    assert_eq!(bus.mem[0x0200], 0xFF);
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn inx_and_dey_wrap() {
    let (mut cpu, mut bus) = setup(&[0xE8, 0x88]);
    cpu.x = 0xFF;
    cpu.y = 0;
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0);
    assert_eq!(cpu.y, 0xFF);
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_Z));
}

#[test]
fn and_imm() {
    let (mut cpu, mut bus) = setup(&[0x29, 0x0F]);
    cpu.a = 0xF3;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x03);
}

#[test]
fn ora_imm() {
    let (mut cpu, mut bus) = setup(&[0x09, 0x80]);
    cpu.a = 0x01;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x81);
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn eor_imm() {
    let (mut cpu, mut bus) = setup(&[0x49, 0xFF]);
    cpu.a = 0xFF;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0);
    assert!(cpu.flag(FLAG_Z));
}

#[test]
fn bit_sets_z_v_n() {
    let (mut cpu, mut bus) = setup(&[0x24, 0x10]);
    cpu.a = 0x01;
    bus.mem[0x10] = 0xC0;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert!(cpu.flag(FLAG_Z));
    assert!(cpu.flag(FLAG_V));
    assert!(cpu.flag(FLAG_N));
    assert_eq!(cpu.a, 0x01);
}

#[test]
fn bit_clears_z_v_n() {
    let (mut cpu, mut bus) = setup(&[0x24, 0x10]);
    cpu.a = 0xFF;
    cpu.p = 0xE6;
    bus.mem[0x10] = 0x01;
    cpu.step(&mut bus);
    assert!(!cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_V));
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn nop() {
    let (mut cpu, mut bus) = setup(&[0xEA]);
    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.pc, 0x8001);
}

#[test]
fn cycles_accumulate() {
    let (mut cpu, mut bus) = setup(&[0xA9, 0x01, 0xEA]);
    cpu.step(&mut bus);
    cpu.step(&mut bus);
    assert_eq!(cpu.cycles, 4);
}
