use tendy_nes::cpu::addressing::Operand;
use tendy_nes::cpu::opcodes::Mode;
use tendy_nes::cpu::*;

fn setup(bytes: &[u8]) -> (Cpu, TestBus) {
    let mut bus = TestBus::new();
    bus.mem[0x8000..0x8000 + bytes.len()].copy_from_slice(bytes);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;
    (cpu, bus)
}

#[test]
fn power_on_state() {
    let cpu = Cpu::new();
    assert_eq!(cpu.sp, 0xFD);
    assert_eq!(cpu.p, 0x24);
    assert_eq!(cpu.a, 0);
    assert_eq!(cpu.pc, 0);
    assert_eq!(cpu.cycles, 0);
}

#[test]
fn reset_reads_vector() {
    let (mut cpu, mut bus) = setup(&[]);
    bus.mem[0xFFFC] = 0x34;
    bus.mem[0xFFFD] = 0x12;
    cpu.reset(&mut bus);
    assert_eq!(cpu.pc, 0x1234);
    assert_eq!(cpu.sp, 0xFD);
    assert_eq!(cpu.p, 0x24);
    assert_eq!(cpu.cycles, 7);
}

#[test]
fn set_zn_zero() {
    let mut cpu = Cpu::new();
    cpu.set_zn(0);
    assert!(cpu.flag(FLAG_Z));
    assert!(!cpu.flag(FLAG_N));
}

#[test]
fn set_zn_negative() {
    let mut cpu = Cpu::new();
    cpu.set_zn(0x80);
    assert!(cpu.flag(FLAG_N));
    assert!(!cpu.flag(FLAG_Z));
}

#[test]
fn set_and_clear_flag() {
    let mut cpu = Cpu::new();
    cpu.set_flag(FLAG_C, true);
    assert!(cpu.flag(FLAG_C));
    cpu.set_flag(FLAG_C, false);
    assert!(!cpu.flag(FLAG_C));
    assert_eq!(cpu.p, 0x24);
}

#[test]
fn push_then_pop() {
    let (mut cpu, mut bus) = setup(&[]);
    cpu.push(&mut bus, 0x42);
    assert_eq!(bus.mem[0x01FD], 0x42);
    assert_eq!(cpu.sp, 0xFC);
    assert_eq!(cpu.pop(&mut bus), 0x42);
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn push16_order() {
    let (mut cpu, mut bus) = setup(&[]);
    cpu.push16(&mut bus, 0xABCD);
    assert_eq!(bus.mem[0x01FD], 0xAB);
    assert_eq!(bus.mem[0x01FC], 0xCD);
    assert_eq!(cpu.sp, 0xFB);
    assert_eq!(cpu.pop16(&mut bus), 0xABCD);
}

#[test]
fn stack_wraps() {
    let (mut cpu, mut bus) = setup(&[]);
    cpu.sp = 0x00;
    cpu.push(&mut bus, 1);
    assert_eq!(bus.mem[0x0100], 1);
    assert_eq!(cpu.sp, 0xFF);
}

#[test]
fn fetch_advances_pc() {
    let (mut cpu, mut bus) = setup(&[0xA9, 0x05]);
    assert_eq!(cpu.fetch(&mut bus), 0xA9);
    assert_eq!(cpu.pc, 0x8001);
}

#[test]
fn mode_imp_reads_nothing() {
    let (mut cpu, mut bus) = setup(&[0x99]);
    let op = cpu.operand(&mut bus, Mode::Imp);
    assert_eq!(
        op,
        Operand {
            addr: 0,
            page_crossed: false
        }
    );
    assert_eq!(cpu.pc, 0x8000);
}

#[test]
fn mode_imm() {
    let (mut cpu, mut bus) = setup(&[0x05]);
    let op = cpu.operand(&mut bus, Mode::Imm);
    assert_eq!(
        op,
        Operand {
            addr: 0x8000,
            page_crossed: false
        }
    );
    assert_eq!(cpu.pc, 0x8001);
}

#[test]
fn mode_zp() {
    let (mut cpu, mut bus) = setup(&[0x10]);
    let op = cpu.operand(&mut bus, Mode::Zp);
    assert_eq!(
        op,
        Operand {
            addr: 0x0010,
            page_crossed: false
        }
    );
}

#[test]
fn mode_zpx_wraps() {
    let (mut cpu, mut bus) = setup(&[0xF0]);
    cpu.x = 0x20;
    let op = cpu.operand(&mut bus, Mode::Zpx);
    assert_eq!(
        op,
        Operand {
            addr: 0x0010,
            page_crossed: false
        }
    );
}

#[test]
fn mode_zpy() {
    let (mut cpu, mut bus) = setup(&[0x10]);
    cpu.y = 5;
    let op = cpu.operand(&mut bus, Mode::Zpy);
    assert_eq!(
        op,
        Operand {
            addr: 0x0015,
            page_crossed: false
        }
    );
}

#[test]
fn mode_abs() {
    let (mut cpu, mut bus) = setup(&[0x34, 0x12]);
    let op = cpu.operand(&mut bus, Mode::Abs);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
    assert_eq!(cpu.pc, 0x8002);
}

#[test]
fn mode_abx_same_page() {
    let (mut cpu, mut bus) = setup(&[0x00, 0x12]);
    cpu.x = 1;
    let op = cpu.operand(&mut bus, Mode::Abx);
    assert_eq!(
        op,
        Operand {
            addr: 0x1201,
            page_crossed: false
        }
    );
}

#[test]
fn mode_abx_crosses_page() {
    let (mut cpu, mut bus) = setup(&[0xFF, 0x12]);
    cpu.x = 1;
    let op = cpu.operand(&mut bus, Mode::Abx);
    assert_eq!(
        op,
        Operand {
            addr: 0x1300,
            page_crossed: true
        }
    );
}

#[test]
fn mode_aby_crosses_page() {
    let (mut cpu, mut bus) = setup(&[0xF8, 0x12]);
    cpu.y = 0x10;
    let op = cpu.operand(&mut bus, Mode::Aby);
    assert_eq!(
        op,
        Operand {
            addr: 0x1308,
            page_crossed: true
        }
    );
}

#[test]
fn mode_ind() {
    let (mut cpu, mut bus) = setup(&[0x00, 0x30]);
    bus.mem[0x3000] = 0x34;
    bus.mem[0x3001] = 0x12;
    let op = cpu.operand(&mut bus, Mode::Ind);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
}

#[test]
fn mode_ind_page_bug() {
    let (mut cpu, mut bus) = setup(&[0xFF, 0x30]);
    bus.mem[0x30FF] = 0x34;
    bus.mem[0x3000] = 0x12;
    bus.mem[0x3100] = 0x99;
    let op = cpu.operand(&mut bus, Mode::Ind);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
}

#[test]
fn mode_izx() {
    let (mut cpu, mut bus) = setup(&[0x20]);
    cpu.x = 4;
    bus.mem[0x24] = 0x34;
    bus.mem[0x25] = 0x12;
    let op = cpu.operand(&mut bus, Mode::Izx);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
}

#[test]
fn mode_izx_pointer_wraps() {
    let (mut cpu, mut bus) = setup(&[0xFE]);
    cpu.x = 1;
    bus.mem[0xFF] = 0x34;
    bus.mem[0x00] = 0x12;
    let op = cpu.operand(&mut bus, Mode::Izx);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
}

#[test]
fn mode_izy_crosses_page() {
    let (mut cpu, mut bus) = setup(&[0x40]);
    cpu.y = 0x10;
    bus.mem[0x40] = 0xF8;
    bus.mem[0x41] = 0x12;
    let op = cpu.operand(&mut bus, Mode::Izy);
    assert_eq!(
        op,
        Operand {
            addr: 0x1308,
            page_crossed: true
        }
    );
}

#[test]
fn mode_izy_pointer_wraps() {
    let (mut cpu, mut bus) = setup(&[0xFF]);
    bus.mem[0xFF] = 0x34;
    bus.mem[0x00] = 0x12;
    let op = cpu.operand(&mut bus, Mode::Izy);
    assert_eq!(
        op,
        Operand {
            addr: 0x1234,
            page_crossed: false
        }
    );
}

#[test]
fn mode_rel_forward() {
    let (mut cpu, mut bus) = setup(&[0x10]);
    let op = cpu.operand(&mut bus, Mode::Rel);
    assert_eq!(
        op,
        Operand {
            addr: 0x8011,
            page_crossed: false
        }
    );
    assert_eq!(cpu.pc, 0x8001);
}

#[test]
fn mode_rel_backward_crosses_page() {
    let (mut cpu, mut bus) = setup(&[0x80]);
    let op = cpu.operand(&mut bus, Mode::Rel);
    assert_eq!(
        op,
        Operand {
            addr: 0x7F81,
            page_crossed: true
        }
    );
}
