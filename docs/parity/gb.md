# Game Boy / Game Boy Advance parity

Every feature a player can reach in the old Game Boy app (`gb-rust`, commit `449154a`), and
where it lives in TendyEmu v0.1. The old app is the source of record: its README, its drawer
and its command line. The rule being checked is SPEC §6: nothing a player could do there is
lost here.

`yes` means reachable today, and the third cell names where. `gap` means not reachable, with
the size of the work and what is missing. `app/tendyemu/tests/gb_parity.rs` parses this table
and checks every `yes` cell against the real drawer, the real keys and the real window, so a
row cannot drift away from the code. Locator forms:

- `Page > Row` — a drawer row, by page title and label, present on a Game Boy or an Advance run.
- `key Name` — a window key binding.
- `mouse click` — the window reads a left click for the drawer.
- `argument <file>` — the one command-line argument opens a cartridge.
- `frame WxH` — the machine's picture size.
- `console Name` — the model the machine reports for a cartridge header.
- `bar Label` — the system word on the status bar.
- `cart` — the storage word on the status bar.
- `absent Page > Row` — the row must be missing (Game Boy-only rows on an Advance run).
- `ignored-key` — a bad key in `config.toml` is reported on the Emulation page.

Sizes: S is a few hours in one module, M a day across two, L more than that.

## Opening a game

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Open a cartridge from the file dialog (`.gb`, `.gbc`, `.gba`, `.zip`, `.7z`) | yes | Main menu > Open ROM... |
| Open the cartridge named on the command line | yes | argument <file> |
| Reopen one of the last ten cartridges | yes | Main menu > Recent ROMs |
| Forget the remembered cartridges | yes | Recent ROMs > Clear list |
| The model comes from the cartridge header, not the file name | yes | console CGB |
| An Advance cartridge opens in the same window at 240x160 | yes | frame 240x160 |
| A Game Boy picture is 160x144 | yes | frame 160x144 |

## Session

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Pause and resume | yes | Game > Pause, key P |
| Reset the cartridge | yes | Game > Reset |
| Quit, writing the battery save first | yes | Main menu > Quit |
| Fast-forward while `Tab` is held | yes | key Tab |
| Fast-forward speed, 1x to 4x | yes | key F2, key F3 |
| The status bar under the picture | yes | Emulation > Status bar |
| Open the drawer (also `M` and `F1`) | yes | key M |
| `Escape` goes back one level, then closes the drawer | yes | key Escape |
| Click a row in the drawer | yes | mouse click |
| Hover a row, walk with the wheel, click outside or right click to go back | gap | S: `main.rs` reads a left click on an open drawer only; hovering, the wheel and outside or right clicks are not read. |
| `F1` toggles the status bar | gap | S: `F1` opens the drawer here; the bar has its own row on the Emulation page. |
| `F11` opens and closes the drawer | gap | S: bind `F11` in `main.rs`; `Esc`, `M` and `F1` open it today. |
| `M` mutes and unmutes | gap | S: no audio rows exist, and `M` opens the drawer instead. |
| The status bar says `REW` while rewinding | gap | S: `BarState` has no rewinding field; the fps slot only dashes. |
| `R` resets without the drawer | gap | S: `main.rs` has no `R` binding; Game > Reset works. |

## Picture

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Scale the picture (the old app offered 2x-6x) | yes | Video > Scale |
| Fullscreen | yes | Video > Fullscreen |
| Integer scaling | yes | Video > Integer scale |
| Per-game picture settings | yes | Emulation > Scale (this game) |
| A screenshot of the picture at the machine's own size | yes | Video > Take screenshot |
| Record GIF, WAV, GIF + WAV or MP4 | yes | Video > Record |
| Start and stop a recording (ten-minute cap) | yes | Video > Start recording |
| AVI recording, which the old app did not have | yes | Video > Start AVI recording |
| The three DMG shade sets (green, grey, pocket) | gap | S: the core has the tables (`palette::table`) and `GbMachine` holds a palette, but `GbOptions` carries none and no row chooses one. Add a palette field, a `Video > Palette` row, and pass it in `loader.rs`. |
| The LCD filter (none, grid, grid + ghost, doubler) | gap | M: `display.rs` has the buffer and the scaling hook only. Needs `app/tendyemu/src/gb/filter.rs`, a `Video > Filter` row and the draw call. |
| Frameskip off or auto | gap | M: `timing.rs` paces frames but nothing skips them and there is no row. |
| `F6` takes a screenshot | gap | S: no `F6` binding in `main.rs`. |
| `F9` starts and stops a recording | gap | S: no `F9` binding in `main.rs`. |

## Sound

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Volume | gap | S: the Audio page has no rows; `app.muted` is set by nothing. |
| Mute, as a row and as `M` | gap | S: same gap as Volume, plus the key. |
| Mute each of the four channels | gap | S: `Capability::ChannelMask` is reported by the core, no row uses it. |
| Sample rate auto / 44.1 / 48 / 96 kHz | gap | S: `audio::negotiate_rate` exists, no row chooses a rate. |
| SGB sound from an SGB cartridge image | gap | M: `Emulation > SGB sound ROM` exists but `loader.rs` passes `sgb_sound_rom: None`. |
| SGB border around the picture | gap | M: `Machine::set_sgb_border` and the 256x224 size exist, but no SGB machine can be built today (`loader.rs` passes no SGB ROM and the Console row is ignored), so the border never shows. |

## Input

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Rebind a button | yes | Input > A |
| Load a `button = key` file | yes | Input > Load keys file... |
| Reset the keys to their defaults | gap | S: `keys.rs` has no reset row. |
| Play with an XInput pad | gap | S: `gamepad.rs` has the backend and no row; #55 covers checking it. |

## Emulation

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| Choose the console (auto, DMG, colour, SGB) | gap | S: `Emulation > Console` is written to `config.toml` and `loader.rs` ignores it (`GbOptions` is always auto). |
| Boot ROM: off, built-in, or a file | gap | M: `Emulation > Boot ROM` is written and ignored; `loader.rs` passes `boot_rom: None`. |
| Keep 30, 60 or 120 seconds of play to rewind | gap | S: `Emulation > Rewind` offers 0/5/10/30/60 s; 120 s is gone. |
| Hold `F8` to walk the picture back | yes | Game > Rewind (hold F8) |
| Step back one snapshot | yes | Game > Step back |
| Link cable role and port | gap | M: the core has `link.rs`; `Level::Link` does not exist and `loader.rs` builds a lone machine. |
| Cheats: Game Genie and GameShark codes from the file, listed, toggled | gap | L: the core has `cheats.rs` and `Capability::Cheats`; there is no row, no list page and no file section. |
| The debugger panel | gap | L: `Capability::Debugger` and `Trace` are reported, and `F3` is speed-down here; #48 covers the panel. |
| Save a state into one of nine slots | yes | Game > Save state, key F5 |
| Load a state | yes | Game > Load state, key F7 |
| Choose the slot | yes | Game > State slot |
| A thumbnail of a saved slot beside the row | yes | Game > State slot |

## Files and settings

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| One settings file, written as it changes | yes | Emulation > File |
| Per-game overrides | yes | Emulation > Scale (this game) |
| The per-game switch that turns every per-game value off at once | gap | S: `settings.rs` writes the game's section whenever a per-game row changes; there is no `per_game` key. |
| A typo in the file is reported, not fatal | yes | ignored-key |
| The battery save is written on quit | yes | Main menu > Quit |
| The cartridge's save chip is named | yes | cart |

## Command line

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| `--help` lists the flags | gap | S: `main.rs` reads one argument and nothing else. |
| An unknown flag is refused by name | gap | S: nothing reads flags, so nothing is refused. |
| Start options: `--scale`, `--fullscreen`, `--no-audio` | gap | S: same; the rows do all three. |
| A headless run: `--frames`, `--screenshot`, `--write-config` | gap | M: the M5 headless lane builds it; until it lands there is no `--frames`. |
| Boot and console overrides: `--boot-rom`, `--cgb`, `--sgb` | gap | S: same; they wait on the Boot ROM and Console rows working. |
| `--link`, `--link-port` | gap | M: they wait on the link UI. |
| `--trace-pc`, `--trace-swi` | gap | M: they wait on the trace panel (#48). |

## Advance only

| Feature | TendyEmu | Where, or what a gap needs |
| --- | --- | --- |
| A Game Boy-only row is hidden while an Advance cartridge runs | yes | absent Emulation > Console |
