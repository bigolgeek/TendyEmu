# Super NES front end: parity checklist

The SNES app this repository was merged from is the reference (its final commit, described in
`docs/DESIGN.md` and the source's own README). SPEC §6 says nothing a user could do there may
be lost here. This file walks its README, drawer rows, hotkeys, command-line flags and settings
keys row by row and records where each one lives now.

`yes` means the feature is reachable in this build, and the `where` cell names the exact place.
`gap` means it is not, and the cell names the change that would close it and its size: S is one
small edit, M is a module, L is a subsystem.

`app/tendyemu/tests/snes_parity.rs` parses this table and checks every `yes` row against the
real drawer rows, keys, screen or command line it names, so the table cannot drift from the
code (`all-rows: desc` is the one check that walks every page). Gap rows are checked for their
status word only.

## Opening the app

| feature | status | where / why not |
|---|---|---|
| Open a ROM named on the command line | yes | `cli: file` |
| Open a ROM or an archive from the drawer's file dialog | yes | `drawer: Main menu > Open ROM...` |
| A 512-byte copier header is stripped, not run as code | yes | `tests: crates/tendy-snes/tests/cart.rs::copier_header_is_detected_by_size` |
| LoROM, HiROM and ExHiROM images load | yes | `tests: crates/tendy-snes/tests/cart.rs::header_found_for_each_board` |
| A PAL cartridge runs at NTSC timing with a one-line notice | yes | `tests: crates/tendy-snes/tests/cart.rs::pal_cartridge_loads_with_a_notice` |
| Recent ROMs page: the last ten, a gone file is marked, clear the list | yes | `drawer: Main menu > Recent ROMs` |
| Quit from the drawer | yes | `drawer: Main menu > Quit` |
| A refused coprocessor game shows the ERROR screen, naming the chip | yes | `screen: coprocessor` |
| An unreadable file is refused on the same screen | yes | `screen: unreadable` |
| Esc quits from the ERROR screen | gap | S: in `main.rs`, quit when Esc is pressed on the refusal screen; today it clears the refusal to the idle window |
| `--help` prints usage | gap | S: `main.rs` reads only the ROM argument |
| `--scale N` starts at a given scale | gap | S: same place; the drawer's Scale row covers the setting |
| `--fullscreen` starts fullscreen | gap | S: same place; the drawer's Fullscreen row covers the setting |
| `--no-audio` starts with no sound device | gap | S: same place; there is no device-opening flag today |
| `--frames N` runs headless and prints one checksum line | gap | M: the headless runner and its sweep are #28's (`src/headless.rs`, `scripts/sweep.sh`) |
| `--audio-stats` prints per-second DSP peaks with `--frames` | gap | S: the old measurement printer was not ported; #28's checksum line is its successor |

## The drawer

| feature | status | where / why not |
|---|---|---|
| F11 opens the drawer | gap | S: `main.rs` opens it on Escape, M and F1; add `KeyCode::F11` to that list |
| Esc opens and closes the drawer | yes | `key: Escape` |
| Main menu order: Open ROM, Recent ROMs, Game, Video, Audio, Input, Emulation, Quit | yes | `drawer: Main menu > * Open ROM..., Recent ROMs, Game, Video, Audio, Input, Emulation, Quit` |
| Every row explains itself in the footer | yes | `all-rows: desc` |
| The footer's counter (`n of N`, selectable rows only) | gap | S: `menu.rs`/`draw.rs` draw no counter; old rule counted rows the cursor can land on |

## Game

| feature | status | where / why not |
|---|---|---|
| Pause and resume | yes | `hotkey: P` |
| Reset the console from the drawer | yes | `drawer: Game > Reset` |
| R resets the console | gap | S: `main.rs` does not read R; the drawer row performs the reset |
| Save a state to the selected slot | yes | `hotkey: F5` |
| Load the state from the selected slot | yes | `hotkey: F7` |
| Choose one of five state slots | yes | `drawer: Game > State slot` |
| Each saved slot shows its picture beside the row | yes | `tests: tests/states.rs::a_save_writes_the_state_and_its_thumbnail` |
| Screenshot to a PNG from the drawer | yes | `drawer: Video > Take screenshot` |
| F6 takes a screenshot | gap | S: `main.rs` has no F6 branch; the drawer row performs it |

## Video

| feature | status | where / why not |
|---|---|---|
| Scale row, 1x to 6x | yes | `drawer: Video > Scale` |
| Fullscreen row | yes | `drawer: Video > Fullscreen` |
| Integer-scale row | yes | `drawer: Video > Integer scale` |
| The picture is 256x224 at native size | yes | `tests: tests/display.rs::native_size_snes` |
| The window follows the scale and the bar | yes | `tests: src/display.rs::the_window_is_the_picture_at_the_scale_plus_the_bar` |
| Status bar on/off row | yes | `drawer: Emulation > Status bar` |
| F1 toggles the status bar | gap | S: F1 opens the drawer instead; `main.rs` would flip `status_bar` then resize |
| The bar's title, speed and rate words | yes | `tests: src/status_bar.rs::the_words_run_title_then_words_then_stats` |

## Audio

| feature | status | where / why not |
|---|---|---|
| Mute toggle | gap | S: `App::set_muted` has no caller and no row; M opens the drawer. Add the row in `audio.rs` and the M branch in `main.rs` |
| Volume row | gap | S: `audio.rs` returns no rows; the mixer already honours the value |
| Eight DSP voice toggles | gap | S: `audio.rs` returns no rows; the wrapper already answers `channel_names` and `set_channel_mask` with eight voices |
| Sound output, resampled to the card | yes | `tests: tests/audio.rs::a_tone_survives_every_core_rate` |

## Input

| feature | status | where / why not |
|---|---|---|
| Rebind any of the twelve pad buttons | yes | `drawer: Input > * Up, Down, Left, Right, A, B, X, Y, L, R, Start, Select, Load keys file...` |
| The SNES default layout, physical face diamond kept | yes | `keys: Up=Up, Down=Down, Left=Left, Right=Right, A=X, B=Z, X=S, Y=A, L=Q, R=W, Start=Enter, Select=Backspace` |
| Load a `button = key` keys file | yes | `drawer: Input > Load keys file...` |
| Reset the keys to the built-in table | gap | S: `keys.rs` has the twelve rebind rows and the file row, no reset action |
| An XInput pad is merged with the keyboard | yes | `tests: tests/input.rs::gamepad_merges_with_keys` |
| Gamepad on/off row | gap | S: `gamepad.rs` returns no rows; the `gamepad` key is already known |

## Emulation

| feature | status | where / why not |
|---|---|---|
| Hold Tab to fast-forward | yes | `key: Tab` |
| Fast-forward speed 2x/3x/4x, remembered across runs | gap | S: `ff_speed` is a known key but no row reads it and Tab is fixed at 4x; F2/F3 cycle a speed for the session only |

## Persistence

| feature | status | where / why not |
|---|---|---|
| One config file with a `[snes]` section, per-game overrides | yes | `tests: tests/settings.rs::per_game_overrides_system_section` |
| The battery is written when the game is closed or switched | yes | `tests: tests/shell.rs::battery_written_on_exit` |
| The battery body round-trips the chip's bytes | yes | `tests: tests/snes_front.rs::ram_round_trip` |
| A short battery body fills the front and leaves the rest zero | yes | `tests: tests/snes_front.rs::short_file_loads_into_the_front` |
| A long battery body is truncated to the chip's size | yes | `tests: tests/snes_front.rs::long_file_is_truncated` |
| Saves, states and screenshots live in per-system folders | yes | `tests: src/battery.rs::a_save_lands_in_the_system_folder_under_the_rom_s_stem` |
| First run copies settings, saves and states from a `snes-rust` folder | yes | `firstrun: snes-rust` |
| The recent list survives a restart | yes | `tests: tests/shell.rs::recents_mix_systems_and_reopen_right_core` |
| A battery file held by another program is retried, not lost | yes | `tests: tests/battery.rs::a_locked_battery_file_is_retried_then_reported` |

## Notes

- The source's own drawer counter and per-channel audio rows were the app's, not the core's;
  they are gaps because the shared `audio.rs` and `menu.rs` own those rows now.
- `--scale`, `--fullscreen`, `--no-audio`, `--help` and the missing F-keys are all one-line
  additions to `main.rs`; none needs a module.
