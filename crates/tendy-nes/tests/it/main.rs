//! The integration tests as one binary: the library is linked once, and libtest runs every
//! test across all cores. Each former `tests/<name>.rs` is a module here.
//!
//! `jit_perf`, `ppu_perf`, `ntsc_alloc` and `save_state_alloc` stay their own binaries in
//! `tests/`: the first two judge wall-clock time and the last two count heap allocations
//! through a `#[global_allocator]` (one per binary at most), so a shared, parallel process
//! would make their numbers meaningless.

mod common;

mod accuracycoin;
mod achievements;
mod alignment_matrix;
mod apu_frame_counter;
mod apu_state_layout;
mod audio_mixer;
mod blargg_more;
mod blargg_vbl_apu;
mod bus_map;
mod cart_hook_flags;
mod clocked_bus;
mod console_quirks;
mod controller_input;
mod cpu_alu;
mod cpu_control_flow;
mod cpu_core;
mod cpu_instructions;
mod cpu_jam;
mod cpu_opcodes;
mod cpu_reset;
mod cpu_roms;
mod determinism;
mod emphasis;
mod events_bus;
mod expansion_audio;
mod expansion_port;
mod four_score;
mod game_snapshots;
mod genie_apply;
mod hdpack;
mod history;
mod joypad_ports;
mod mapper111;
mod machine;
mod mapper228;
mod mapper30;
mod mapper_hooks;
mod mapper_mmc2_mmc4;
mod mapper_short_prg;
mod mappers_core;
mod mappers_discrete;
mod mappers_tranche_b;
mod mmc1_surom;
mod mmc3;
mod mmc3_old_roms;
mod mmc3_roms;
mod movie;
mod nmi_race;
mod nsf;
mod overclock;
mod power_pad;
mod ppu_background;
mod ppu_core;
mod ppu_emphasis;
mod ppu_glitches;
mod ppu_sprites;
mod profiler;
mod ram_watch;
mod raw_ram_cheats;
mod reset_align;
mod runahead;
mod save_state;
mod screen_text;
mod shared_helpers;
mod sst_cpu;
mod state_mappers;
mod state_roundtrip;
mod submappers;
mod symbols;
mod tasedit;
mod tasvideos_movies;
mod trace;
mod trace_log;
mod trace_peek;
mod vs_system;
mod zapper;
