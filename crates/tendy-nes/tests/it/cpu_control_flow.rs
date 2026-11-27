use tendy_nes::cpu::*;

fn setup(prog: &[u8]) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.mem[0x8000..0x8000 + prog.len()].copy_from_slice(prog);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;
    (cpu, bus)
}

#[test]
fn bne_not_taken() {
    let (mut cpu, mut bus) = setup(&[0xD0, 0x05]);
    cpu.p = 0x26;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.pc, 0x8002);
}

#[test]
fn bne_taken_same_page() {
    let (mut cpu, mut bus) = setup(&[0xD0, 0x05]);

    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert_eq!(cpu.pc, 0x8007);
}

#[test]
fn beq_taken_across_page() {
    let (mut cpu, mut bus) = setup(&[0xF0, 0x80]);
    cpu.p = 0x26;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 4);
    assert_eq!(cpu.pc, 0x7F82);
}

#[test]
fn bcs_taken() {
    let (mut cpu, mut bus) = setup(&[0xB0, 0x02]);
    cpu.p = 0x25;

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8004);
}

#[test]
fn bcc_taken() {
    let (mut cpu, mut bus) = setup(&[0x90, 0x02]);

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8004);
}

#[test]
fn bpl_not_taken_when_negative() {
    let (mut cpu, mut bus) = setup(&[0x10, 0x02]);
    cpu.p = 0xA4;

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8002);
}

#[test]
fn bmi_taken() {
    let (mut cpu, mut bus) = setup(&[0x30, 0x02]);
    cpu.p = 0xA4;

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8004);
}

#[test]
fn bvc_taken() {
    let (mut cpu, mut bus) = setup(&[0x50, 0x02]);

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8004);
}

#[test]
fn bvs_taken() {
    let (mut cpu, mut bus) = setup(&[0x70, 0x02]);
    cpu.p = 0x64;

    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8004);
}

#[test]
fn jmp_absolute() {
    let (mut cpu, mut bus) = setup(&[0x4C, 0x34, 0x12]);

    let c = cpu.step(&mut bus);
    assert_eq!(c, 3);
    assert_eq!(cpu.pc, 0x1234);
}

#[test]
fn jmp_indirect_page_bug() {
    let (mut cpu, mut bus) = setup(&[0x6C, 0xFF, 0x30]);
    bus.mem[0x30FF] = 0x34;
    bus.mem[0x3000] = 0x12;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 5);
    assert_eq!(cpu.pc, 0x1234);
}

#[test]
fn jsr_then_rts() {
    let (mut cpu, mut bus) = setup(&[0x20, 0x00, 0x90]);
    bus.mem[0x9000] = 0x60;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 6);
    assert_eq!(cpu.pc, 0x9000);
    assert_eq!(cpu.sp, 0xFB);
    assert_eq!(bus.mem[0x01FD], 0x80);
    assert_eq!(bus.mem[0x01FC], 0x02);

    let c1 = cpu.step(&mut bus);
    assert_eq!(c1, 6);
    assert_eq!(cpu.pc, 0x8003);
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn brk_then_rti() {
    let (mut cpu, mut bus) = setup(&[0x00, 0xFF]);
    bus.mem[0xFFFE] = 0x00;
    bus.mem[0xFFFF] = 0x90;
    bus.mem[0x9000] = 0x40;
    cpu.p = 0x20;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 7);
    assert_eq!(cpu.pc, 0x9000);
    assert!(cpu.flag(FLAG_I));
    assert_eq!(bus.mem[0x01FD], 0x80);
    assert_eq!(bus.mem[0x01FC], 0x02);
    assert_eq!(bus.mem[0x01FB], 0x30);
    assert_eq!(cpu.sp, 0xFA);

    let c1 = cpu.step(&mut bus);
    assert_eq!(c1, 6);
    assert_eq!(cpu.pc, 0x8002);
    assert_eq!(cpu.p, 0x20);
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn nmi_is_serviced_first() {
    let (mut cpu, mut bus) = setup(&[0xEA]);
    bus.mem[0xFFFA] = 0x00;
    bus.mem[0xFFFB] = 0xA0;
    cpu.nmi_pending = true;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 7);
    assert_eq!(cpu.pc, 0xA000);
    assert!(!cpu.nmi_pending);
    assert_eq!(bus.mem[0x01FD], 0x80);
    assert_eq!(bus.mem[0x01FC], 0x00);
    assert_eq!(bus.mem[0x01FB], 0x24);
    assert!(cpu.flag(FLAG_I));
}

#[test]
fn irq_ignored_when_i_set() {
    let (mut cpu, mut bus) = setup(&[0xEA]);
    cpu.irq_line = true;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 2);
    assert_eq!(cpu.pc, 0x8001);
}

#[test]
fn irq_taken_when_i_clear() {
    let (mut cpu, mut bus) = setup(&[0xEA]);
    cpu.p = 0x20;
    cpu.irq_line = true;
    bus.mem[0xFFFE] = 0x00;
    bus.mem[0xFFFF] = 0xB0;

    let c = cpu.step(&mut bus);
    assert_eq!(c, 7);
    assert_eq!(cpu.pc, 0xB000);
    assert!(cpu.flag(FLAG_I));
    assert_eq!(bus.mem[0x01FB], 0x20);
}

#[test]
fn jam_halts_cpu() {
    let (mut cpu, mut bus) = setup(&[0x02]);

    cpu.step(&mut bus);
    assert!(cpu.jammed);
    assert_eq!(cpu.pc, 0x8001);

    let c = cpu.step(&mut bus);
    assert_eq!(c, 1);
    assert_eq!(cpu.pc, 0x8001);
}
