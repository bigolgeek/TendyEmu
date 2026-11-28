# NES parity checklist

What a user can do in the old NES app, one row per feature, and whether TendyEmu offers it
today. This is the SPEC §6 parity walk for the NES core: features that lived only in the old
app stay unbuilt until a later issue, one issue per feature, and the gaps below are that
work list.

How to read a row: **yes** means reachable in TendyEmu today and the note says where; **gap**
means not reachable. Where a yes row is backticked, `tests/nes_parity.rs` opens the drawer,
reads the row or presses the key, so the note cannot drift from the code. `file:nes` and
`file:nsf` mean the loader opens that kind of image.

The old app is the reference for the left column; its README, menu and flags were walked in
full, including the settings that only appeared in its config file.

## Opening and the shell

| Feature | TendyEmu | Note |
|---|---|---|
| Open a ROM from the file dialog | yes | `Main menu > Open ROM...` |
| Recent ROMs, newest first | yes | `Main menu > Recent ROMs`; the old digit keys are not bound |
| Drawer menu, mouse and keyboard | yes | `Esc`, `M`, `F1`; the old F11 is not bound |
| Quit | yes | `Main menu > Quit`; the old Esc quit key opens the drawer now |
| Archives: a .zip or .7z runs its first ROM | yes | `Main menu > Open ROM...`; the detector extracts the entry and the archive stays in recents |
| UNIF board images | gap | the detector names a .unf but the cart reader only takes iNES, so the image is refused at open |
| A ROM path on the command line | yes | `Main menu > Open ROM...`; the first argument takes the same open path |
| A refused ROM keeps the running game and shows the core's reason | yes | `Esc` dismisses the refusal screen |

## Game page

| Feature | TendyEmu | Note |
|---|---|---|
| Pause and resume | yes | `P`, `Game > Pause` |
| Reset | yes | `Game > Reset`; the old R key is not bound |
| Step one frame while paused | gap | the old backtick step key is not bound |
| Save state | yes | `F5`, `Game > Save state` |
| Load state | yes | `F7`, `Game > Load state` |
| Screenshot to a PNG | yes | `Video > Take screenshot`; the old F6 key is not bound |
| Rewind, hold to step back | yes | `F8`, `Game > Rewind (hold F8 or Backspace)` |
| Record the audio to a WAV | yes | `Video > Record`, `Video > Start recording`; pick WAV in the mode row, the old F12 key is not bound |

## Video

| Feature | TendyEmu | Note |
|---|---|---|
| Scale | yes | `Video > Scale` |
| Fullscreen | yes | `Video > Fullscreen` |
| Aspect: square pixels or 4:3 | yes | `Video > Aspect` |
| Integer scale | yes | `Video > Integer scale` |
| Crop top, bottom and side | gap | overscan rows cannot be hidden |
| Palette: built-in, generated NTSC, or a .pal file | gap | no palette row |
| NTSC composite filter | gap | no filter row |
| Sprite limit switch, display only | gap | the core draws eight sprites a line with no override |
| Status bar on or off | yes | `Emulation > Status bar`; the old F1 key opens the drawer now |
| CRT filter and its five detail rows | gap | no filter, scanline, mask, curvature or glow row |
| Pipeline shaders | gap | no shader support |

## Audio

| Feature | TendyEmu | Note |
|---|---|---|
| Mute | gap | no mute row and no key; `M` opens the drawer |
| Per-channel gains, six channels | gap | no mixer rows |
| Per-channel pans, six channels | gap | no stereo placement |
| Three-band equaliser | gap | no EQ |
| Silence the ultrasonic triangle | gap | no fix row |
| Reduce the DMC pop | gap | no fix row |
| Sync: audio clock or video refresh | gap | the app paces on the wall clock only |

## Input

| Feature | TendyEmu | Note |
|---|---|---|
| Keyboard layout on the console pad | yes | `Input > A`; arrows, X A, Z B, Enter Start, Right Shift Select |
| Rebind a button from the drawer | yes | `Input > A`, `Input > Load keys file...` |
| A keys file per system | yes | `Input > Load keys file...` |
| Gamepad on player 1 | yes | `gamepad`; XInput pad 1 merges onto the keyboard's pad |
| Gamepad on player 2 | gap | a second XInput pad is not read; ports 2 to 4 are not built |
| Rebind a pad button from a keys file | gap | the pad layout is fixed; only keys are rebindable |
| Player 2 on the keyboard | gap | one keyboard layout only |
| Four Score, players 3 and 4 | gap | no adapter and no ports 2 to 4 |
| Auto-fire A and B on two keys | gap | no turbo |
| Zapper light gun | gap | no device selection |
| Paddle | gap | no device selection |
| Power Pad | gap | no device selection |
| Family BASIC keyboard | gap | no expansion-port device |

## Emulation

| Feature | TendyEmu | Note |
|---|---|---|
| Speed: 1x, 2x, 3x, 4x | yes | `F2`, `F3` |
| Speeds 0.25x and 0.5x, and the bracket keys | gap | the ladder starts at 1x and wraps |
| Fast-forward while held | yes | `Tab` |
| Rewind window in seconds | yes | `Emulation > Rewind`; choices 0, 5, 10, 30 and 60 seconds against the old 0 to 3600 |
| Run-ahead, 0 to 4 frames | gap | no speculative frames |
| Session history and the scrub bar | gap | no input log, keyframes or `H` bar |
| RAM init at power-on | gap | no power-on options |
| Power-on alignment | gap | no power-on options |
| Reset alignment | gap | reset is fixed |
| Random mapper and seed | gap | no power-on options |
| Overclock lines | gap | no overclock |
| Hardware quirks and console presets | gap | no quirk rows or preset picker |
| Vs. System arcade boards | gap | the core boots a Vs. cartridge but the coin and service keys and the DIP switch setting are missing |

## Tools and overlays

| Feature | TendyEmu | Note |
|---|---|---|
| Input overlay | gap | no overlay |
| APU viewer | gap | no channel panel |
| PPU viewer | gap | no pattern, palette or OAM viewer |
| Memory viewer | gap | no viewer |
| Hide the background or the sprites | gap | no layer keys |
| Trace log with symbols | gap | no instruction log |
| Code/data log | gap | no CDL output |
| RAM watch | gap | no watch list |
| Lag log | gap | no per-frame lag CSV |
| Cycle profiler | gap | no profiler |

## Movies, TAS and cheats

| Feature | TendyEmu | Note |
|---|---|---|
| Record and play FM2 movies | gap | no recorder or player |
| Import a BizHawk .bk2 | gap | no movie import |
| TAS editor model | gap | the old app kept the model unwired, so nothing was reachable there either |
| Game Genie codes at launch | gap | no code entry |
| Cheats folder of RetroArch lists | gap | no cheat database read |
| RetroAchievements | gap | the old app had the offline engine only and never wired it to a frame, so nothing was reachable there either |
| HD packs | gap | no replacement tiles |

## Files, disks and recording

| Feature | TendyEmu | Note |
|---|---|---|
| Battery save in the game's folder | yes | `Main menu > Open ROM...`; written while playing and on quit, adopted from beside the ROM |
| AVI recording with sound | yes | `Video > Start AVI recording` |
| FDS games: BIOS choice and loading | yes | `Emulation > FDS BIOS`; a .fds, a .qd, or a zip holding one opens on side A, and a disk with no BIOS refuses in one line that names the row. A disksys.rom beside the image is used when the row is empty |
| FDS side swap | gap | the disk mounts on side A and stays there; the old F8 side key is rewind now |
| IPS and BPS soft patches | gap | a later lane of this milestone adds them |
| NSF playback | yes | `file:nsf`; bare or inside an archive |
| NSF title screen, song list and starting song flag | gap | the tune plays with no title, no Up and Down song keys and no --song |
| Display, audio and power-on command-line flags | gap | most settings have a drawer row or a config key, but the startup flags are not read |

A note on what is not a gap: archive support, battery saves and the AVI writer are shared
shell work from earlier issues, not NES-specific code. Per-game display settings are an
addition TendyEmu has and the old app did not.
