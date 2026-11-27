//! The shared SPC700 + S-DSP through the SNES APU's own entry point (issue #6, DESIGN §4.3).
//!
//! The public `snes-test-roms` suite plays no music, so `[snes/test-rom]`'s audio pin is silence
//! (peak 0.000031) and proves nothing about the DSP's voices or its echo. This probe needs no ROM:
//! it uploads a synthetic SPC700 program through the IPL handshake over the four ports — the exact
//! sequence a cartridge's boot code runs, `$CC` kick, block transfer, entry — then pins what the
//! APU plays for one second of its own clock.
//!
//! Worked example, the way §5.6 defines it: `tendy-pin spc-snes --source <source checkout>`.

mod common;

use common::{AudioHash, Report, fnv1a64, hex, spc_image, SPC_PROGRAM};
use snes::MASTER_CLOCK_HZ;
use snes::apu::Apu;
use snes::state::StateWriter;

/// The run is one second of master clocks, in 32 slices: one slice is about 640 DSP samples, well
/// inside the APU's 2 048-sample staging buffer, so nothing is dropped.
const SLICES: u32 = 32;
const SLICE_TICKS: u32 = MASTER_CLOCK_HZ / SLICES;

fn main() {
    let mut report = Report::new();
    report.run("spc/snes-synthetic");

    let mut apu = Apu::new();
    upload(&mut apu);

    let mut audio = AudioHash::new();
    let mut buffer = [0.0f32; 8192];
    let mut samples = 0u64;
    for _ in 0..SLICES {
        apu.tick(SLICE_TICKS);
        loop {
            let frames = apu.read_samples(&mut buffer);
            if frames == 0 {
                break;
            }
            audio.feed(&buffer[..frames * 2]);
            samples += frames as u64;
            if frames < buffer.len() / 2 {
                break;
            }
        }
    }

    report.pin("audio", hex(audio.value()));
    report.pin("audio_peak", format!("{:.6}", audio.peak()));
    report.pin("samples", samples);
    let mut writer = StateWriter::new();
    apu.save_state(&mut writer);
    let blob = writer.finish();
    report.pin("apu_state", hex(fnv1a64(&blob)));
    report.pin("apu_state_len", blob.len());
    eprintln!(
        "spc/snes-synthetic: peak {:.6} over {samples} stereo frames, {} nonzero",
        audio.peak(),
        audio.nonzero().0
    );
    report.finish();
}

/// The image into APU RAM through the IPL handshake, then the entry command.
fn upload(apu: &mut Apu) {
    let image = spc_image();
    kick(apu, SPC_PROGRAM, 0xCC);
    for (index, byte) in image.iter().enumerate() {
        apu.write_port(1, *byte);
        apu.write_port(0, index as u8);
        await_echo(apu, index as u8);
    }
    // The entry command: `$2141` = 0, the address in `$2142`/`$2143`, then a kick the transfer
    // loop reads as "past the last index".
    let kick_byte = ((image.len().wrapping_add(1) & 0xFF) | 1) as u8;
    apu.write_port(2, SPC_PROGRAM as u8);
    apu.write_port(3, (SPC_PROGRAM >> 8) as u8);
    apu.write_port(1, 0x00);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
    assert_eq!(
        apu.cpu.pc, SPC_PROGRAM,
        "the entry command must leave the SPC700 at the program"
    );
}

/// One command: the destination in `$2142`/`$2143`, `$2141` = 01 (transfer), then the kick.
fn kick(apu: &mut Apu, destination: u16, kick_byte: u8) {
    apu.write_port(2, destination as u8);
    apu.write_port(3, (destination >> 8) as u8);
    apu.write_port(1, 0x01);
    apu.write_port(0, kick_byte);
    await_echo(apu, kick_byte);
}

/// Polls `$2140` the way an uploader does, handing the SPC clocks while it waits: the boot ROM
/// reads the ports from software, so a write is only seen once the SPC runs.
fn await_echo(apu: &mut Apu, want: u8) {
    for _ in 0..200_000 {
        if apu.read_port(0) == want {
            return;
        }
        apu.tick(64); // a couple of SPC cycles: one pass of the ROM's poll loop is 7-9
    }
    panic!(
        "the boot ROM never echoed ${want:02X}; $2140 holds ${:02X}",
        apu.read_port(0)
    );
}
