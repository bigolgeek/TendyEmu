# TendyEmu — Specification v0.1

Status: **approved 2026-11-26** (#1). Changes from here on go through an issue.
Every piece of work below is a GitHub issue; this file says what the pieces are and how they fit.

Where design work proved a fact below wrong, DESIGN.md §3.6 lists the correction (C1-C10) and that
correction governs; this file is amended only when the owner asks for it.

## 1. What TendyEmu is

One desktop application that plays Game Boy, Game Boy Color, Super Game Boy, Game Boy Advance,
NES and Super NES games. The user opens a file; TendyEmu reads it, picks the right emulator core,
and runs it. There is no "choose your system" step.

TendyEmu is built by merging three existing Rust emulators that already share one front-end design
(same windowing, audio, file-picker and archive libraries, same menu shape):

| source project | systems | role in TendyEmu |
|---|---|---|
| gb-rust | DMG, CGB, SGB (incl. SGB sound), GBA | `tendy-gb` and `tendy-gba` cores |
| nes-rust | NES (iNES / NES 2.0, UNIF, FDS / QD, NSF), Vs. System | `tendy-nes` core |
| snes-rust | SNES (ROM, ROM+RAM, ROM+RAM+battery) | `tendy-snes` core |

The source projects stay where they are, unchanged and private. TendyEmu starts a **fresh public
history**: code is imported into it file by file, cleaned of local paths and internal notes.

## 2. Principles

1. **One app, one binary** (`tendyemu`), one settings file, one menu.
2. **The file decides the core** (§4). The user never names the system; a file that matches no
   system is refused with a one-line reason, never guessed.
3. **Import first, improve later.** Moving a core into TendyEmu must not change what it does: every
   imported core produces the same frames, audio and save states as its source commit
   (§8, "inertness"). The one intended difference is SNES save states (#71): the source core wrote
   a 38-byte scaffold, so TendyEmu serialises the whole machine instead. Fixes and features come
   after, each as its own issue.
4. **No copyrighted ROMs in the repo.** Tests build their own ROM bytes in code or use public test
   suites whose licence allows redistribution.
5. **Local and offline.** No accounts, no telemetry, no network access unless a feature the user
   switches on needs it.
6. **Licence:** `MIT OR Apache-2.0`, the same as all three sources.

## 3. Systems in v0.1

| system | file types | notes |
|---|---|---|
| Game Boy / Color | `.gb` `.gbc` | model chosen from the header (§4) and the user's console setting |
| Super Game Boy | `.gb` `.sgb` with the SGB flag | borders, palettes, SGB sound when the user supplies an SGB cartridge file |
| Game Boy Advance | `.gba` `.agb` | |
| NES / Famicom | `.nes` `.unf` `.unif` `.fds` `.qd` `.nsf` | NSF opens the music player |
| Super NES | `.sfc` `.smc` `.swc` `.fig` | coprocessor boards are refused by name, as today |
| any of the above | `.zip` `.7z` | the first entry that detects as a ROM is opened |

Soft patches (`.ips`, `.bps`, `.ups`) next to the ROM keep working for the cores that support them
today, and become shared across cores in a later issue.

## 4. ROM detection — the rule that picks the core

Detection is a pure function in its own crate (`tendy-detect`): bytes + file name in, a `Detected`
answer or a refusal out. It is the most-tested piece of the app.

**Order:**

1. **Archive.** `.zip` / `.7z`: walk entries in archive order; take the first entry whose extension
   is a known ROM type; if none, the first entry whose header (step 2) matches. Directories and
   non-ROM files are skipped. No match → refusal "no ROM inside <archive>".
2. **Header signature (decides when present).**

   | check | offset | value | result |
   |---|---|---|---|
   | iNES / NES 2.0 | `$0000` | `4E 45 53 1A` ("NES␚") | NES (NES 2.0 when byte 7 bits 2-3 = `10`) |
   | UNIF | `$0000` | "UNIF" | NES |
   | FDS (fwNES header) | `$0000` | `46 44 53 1A` ("FDS␚") | NES, disk system |
   | FDS (raw disk) | `$0000` | `01` + "*NINTENDO-HVC*" | NES, disk system |
   | NSF | `$0000` | "NESM" `1A` | NES music player |
   | GBA | `$0004`-`$009F` + `$00B2` | Nintendo logo + fixed `96h` | GBA |
   | Game Boy | `$0104`-`$0133` | Nintendo logo | GB; `$0143` bit 7 → Colour-capable, `$0146` = `03h` → SGB-capable |
   | SNES | `$7FC0` / `$FFC0` / `$40FFC0` (+`$200` when size mod 1024 = 512, a copier header) | internal header scores above the threshold the current SNES loader uses | SNES (LoROM / HiROM / ExHiROM) |

   The SNES row has no magic number, so it runs **after** every magic-number row and only accepts a
   header that scores, using the SNES core's existing scoring code unchanged.
3. **Extension fallback** — only when no signature matched (some Game Boy homebrew and test ROMs
   omit the logo; raw `.qd` images). The table in §3 maps extensions to cores.
4. **Conflict.** If the signature says one system and the extension another, the **signature wins**
   and the status bar says so once ("file named .smc is a Game Boy ROM — opened as Game Boy").
5. **Nothing matched** → refusal screen: `refused: not a recognised ROM (<file>)`. Never guessed.

The Game Boy model (DMG / CGB / SGB) inside the GB core is chosen as today: header flags plus the
user's console setting. GB-on-GBA (running a GB cartridge on the GBA's GB hardware) is not a mode.

Every row above gets a test that builds a minimal image in code, plus the conflict and refusal cases.

## 5. Architecture (target shape — frozen later in `docs/DESIGN.md`)

A Cargo workspace:

```
tendyemu/
├─ crates/
│  ├─ tendy-core     shared contract: the Machine trait, Frame, audio drain, Buttons,
│  │                 save-state container (system tag + version + core blob), CartIdentity
│  ├─ tendy-detect   §4, plus zip / 7z extraction
│  ├─ tendy-spc      SPC700 + S-DSP, used by tendy-snes AND by the SGB sound path in tendy-gb
│  │                 (today the same code lives twice)
│  ├─ tendy-gb       DMG / CGB / SGB core
│  ├─ tendy-gba      GBA core
│  ├─ tendy-nes      NES core (mappers, FDS, NSF, Vs. System)
│  └─ tendy-snes     SNES core
└─ app/tendyemu      the one binary: window, menu, settings, input, audio out, recents,
                     rewind, screenshots, recording, file picker
```

- **The Machine trait** is the union of the three existing `System` traits, with optional methods
  for what only some systems have (SGB border, tilt, shoulder buttons, channel mask, disk swap).
  The front end asks a core which **capabilities** it has and shows only those menu rows.
- **Frame:** one pixel format at the boundary (RGB, converted by each core); the window keeps each
  system's native aspect (160×144, 256×224 SGB, 240×160, 256×240, SNES 256/512 × 224/239).
- **Input:** one logical pad (D-pad, A B X Y, L R, Start Select); each core maps what it has.
  Key and pad bindings are stored **per system**.
- **Save states** carry a system tag so a state from one core is refused by another with a plain
  message, not a crash.
- **Switching systems** in one session: "Open ROM" for a different system saves the running game's
  battery RAM, drops that core and builds the new one.

`docs/DESIGN.md` turns this into exact type and function declarations, the per-issue file split,
and the order the import issues run in. It is reviewed by a second model before any import starts.

## 6. Front end: shared vs per-core

| feature | today in | in TendyEmu v0.1 |
|---|---|---|
| menu / drawer, status bar, recents, file picker, archives | all three | shared, written once |
| settings file, per-game settings | GB, NES | shared, per system section |
| rewind, screenshots, recording | GB, NES | shared |
| gamepad (XInput) | all three | shared |
| cheats | GB, NES (Game Genie, cheat DB) | per core, one menu row |
| SGB border / sound, boot ROM | GB | GB core capability |
| movies, TAS, run-ahead, HD packs, CRT/NTSC filters, shaders, debugger, trace, memory view, RetroAchievements | NES | NES core capability; shared later where it makes sense (separate issues) |
| coprocessor refusal screen | SNES | shared refusal screen |

**Parity rule:** nothing a user can do in one of the three apps today is lost in TendyEmu v0.1.
Features that live in one app only stay with that core in v0.1; making them work for every system
is post-v0.1 work, one issue per feature.

## 7. Files and settings

- One config folder for TendyEmu; saves, states and screenshots in per-system subfolders.
- First run offers to **copy** (never move) battery saves and settings from the three old apps'
  folders if it finds them. Old files are left untouched.

## 8. How the work is proved

| gate | what it checks |
|---|---|
| unit / integration tests | each core's existing suite passes inside the workspace, same counts |
| inertness | for each imported core, a fixed set of test ROMs gives the same frame checksums, audio hashes and save-state bytes as the source commit |
| detector tests | every row of §4, conflict, refusal, archives |
| `cargo fmt --check`, `cargo clippy -D warnings` | whole workspace |
| library sweep (local, maintainers' machine) | per system, the counts of drawing / blank / refused titles match the source project's last sweep; no results containing file paths are committed |
| owner check | the app opens one game of each system from one window |

## 9. Work plan (milestones)

| # | milestone | contents | issues |
|---|---|---|---|
| M0 | Spec | this file, the issue list; **gate: owner approves** | #1 |
| M1 | Design | `docs/DESIGN.md` (exact declarations, file split, order), cross-model review; **gate: review passed** | #2-#3 |
| M2 | Workspace + import | scaffold; import `tendy-spc`, `tendy-gb`, `tendy-gba`, `tendy-nes`, `tendy-snes`, each proved inert | #4-#11 |
| M3 | Detection | `tendy-detect` with the §4 tests | #12-#14 |
| M4 | One app | `app/tendyemu`: shared shell, capabilities, per-system input, system switching, settings import | #15-#24 |
| M5 | Parity | every feature of §6 reachable; parity checklist per old app | #25-#30 |
| M6 | Release 0.1.0 | README, licence notices, Windows + Linux builds, release notes | #31-#36 |
| — | Backlog | open issues carried over from the source projects, and post-v0.1 features | #37-#67 |

## 10. Out of scope for v0.1

New systems; netplay; libretro core; web (WASM) and mobile builds; Lua scripting; localisation; SNES
coprocessors (DSP-n, SA-1, Super FX, S-DD1, CX4, ST01x, OBC1, SPC7110) and MSU-1. Each is an issue
in the Backlog milestone so it is not forgotten.

## 11. Decisions taken in this spec (change any before approving)

1. Fresh public history; the three source repos stay private and unchanged.
2. Workspace of crates, one per core, plus a shared SPC crate (removes today's duplicate).
3. Header signature beats file extension; SNES is detected by header score after all magic numbers.
4. v0.1 = merge with parity, no new emulation features.
5. Old apps' saves are copied on first run, never moved.
