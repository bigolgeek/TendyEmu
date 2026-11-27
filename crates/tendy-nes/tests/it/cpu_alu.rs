use tendy_nes::cpu::*;

fn setup(prog: &[u8]) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.mem[0x8000..0x8000 + prog.len()].copy_from_slice(prog);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;
    (cpu, bus)
}

#[test]
fn adc_simple() {
    let (mut cpu, mut bus) = setup(&[0x69, 0x20]);
    cpu.a = 0x10;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.a, 0x30);
    assert!(!cpu.flag(FLAG_C));
    assert!(!cpu.flag(FLAG_V));
    assert!(!cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn adc_adds_carry_in() {
    let (mut cpu, mut bus) = setup(&[0x69, 0x20]);
    cpu.a = 0x10;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x31);
    assert!(!cpu.flag(FLAG_C));
}

#[test]
fn adc_carry_out_to_zero() {
    let (mut cpu, mut bus) = setup(&[0x69, 0x01]);
    cpu.a = 0xFF;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.flag(FLAG_C));
    assert!(cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_V));
}

#[test]
fn adc_positive_overflow() {
    let (mut cpu, mut bus) = setup(&[0x69, 0x01]);
    cpu.a = 0x7F;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80);
    assert!(cpu.flag(FLAG_V));
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_C));
}

#[test]
fn adc_negative_overflow() {
    let (mut cpu, mut bus) = setup(&[0x69, 0xFF]);
    cpu.a = 0x80;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x7F);
    assert!(cpu.flag(FLAG_C));
    assert!(cpu.flag(FLAG_V));
}

#[test]
fn sbc_simple() {
    let (mut cpu, mut bus) = setup(&[0xE9, 0x20]);
    cpu.a = 0x50;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x30);
    assert!(cpu.flag(FLAG_C));
    assert!(!cpu.flag(FLAG_V));
}

#[test]
fn sbc_borrow() {
    let (mut cpu, mut bus) = setup(&[0xE9, 0x01]);
    cpu.a = 0x00;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xFF);
    assert!(!cpu.flag(FLAG_C));
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn sbc_overflow() {
    let (mut cpu, mut bus) = setup(&[0xE9, 0x01]);
    cpu.a = 0x80;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x7F);
    assert!(cpu.flag(FLAG_V));
    assert!(cpu.flag(FLAG_C));
}

#[test]
fn sbc_without_carry_takes_one_more() {
    let (mut cpu, mut bus) = setup(&[0xE9, 0x20]);
    cpu.a = 0x50;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x2F);
}

#[test]
fn cmp_equal() {
    let (mut cpu, mut bus) = setup(&[0xC9, 0x40]);
    cpu.a = 0x40;
    cpu.step(&mut bus);
    assert!(cpu.flag(FLAG_Z));
    assert!(cpu.flag(FLAG_C));
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn cmp_less() {
    let (mut cpu, mut bus) = setup(&[0xC9, 0x20]);
    cpu.a = 0x10;
    cpu.step(&mut bus);
    assert!(!cpu.flag(FLAG_C));
    assert!(!cpu.flag(FLAG_Z));
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn cpx_greater() {
    let (mut cpu, mut bus) = setup(&[0xE0, 0x20]);
    cpu.x = 0x30;
    cpu.step(&mut bus);
    assert!(cpu.flag(FLAG_C));
    assert!(!cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn cpy_zero_page() {
    let (mut cpu, mut bus) = setup(&[0xC4, 0x10]);
    cpu.y = 5;
    bus.mem[0x10] = 5;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert!(cpu.flag(FLAG_Z));
    assert!(cpu.flag(FLAG_C));
}

#[test]
fn asl_accumulator() {
    let (mut cpu, mut bus) = setup(&[0x0A]);
    cpu.a = 0x81;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.a, 0x02);
    assert!(cpu.flag(FLAG_C));
}

#[test]
fn asl_zero_page() {
    let (mut cpu, mut bus) = setup(&[0x06, 0x10]);
    bus.mem[0x10] = 0x40;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 5);
    assert_eq!(bus.mem[0x10], 0x80);
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_C));
}

#[test]
fn lsr_accumulator() {
    let (mut cpu, mut bus) = setup(&[0x4A]);
    cpu.a = 0x01;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0);
    assert!(cpu.flag(FLAG_C));
    assert!(cpu.flag(FLAG_Z));
}

#[test]
fn rol_accumulator_with_carry() {
    let (mut cpu, mut bus) = setup(&[0x2A]);
    cpu.a = 0x80;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x01);
    assert!(cpu.flag(FLAG_C));
}

#[test]
fn ror_accumulator_with_carry() {
    let (mut cpu, mut bus) = setup(&[0x6A]);
    cpu.a = 0x01;
    cpu.p = 0x25;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80);
    assert!(cpu.flag(FLAG_C));
    assert!(cpu.flag(FLAG_N));
}

#[test]
fn ror_absolute() {
    let (mut cpu, mut bus) = setup(&[0x6E, 0x00, 0x02]);
    bus.mem[0x0200] = 0x02;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 6);
    assert_eq!(bus.mem[0x0200], 0x01);
    assert!(!cpu.flag(FLAG_C));
}

#[test]
fn rol_abx_has_no_penalty() {
    let (mut cpu, mut bus) = setup(&[0x3E, 0xFF, 0x02]);
    cpu.x = 1;
    bus.mem[0x0300] = 0x01;
    let c = cpu.step(&mut bus);
    assert_eq!(c, 7);
    assert_eq!(bus.mem[0x0300], 0x02);
}
