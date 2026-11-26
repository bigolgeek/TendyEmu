# Decisions

Owner decisions that change what the spec or the design says. Newest first. Each row names where it landed, so
the document and the code behind it can be checked against this list.

| Date | Decision | Landed in |
|---|---|---|
| 2026-11-26 | **Per-ROM files live in per-system folders** under the config folder (`<system>/saves/`, `states/`, `screenshots/`, `recordings/`, `cheats/`, `movies/`), not beside the ROMs. Saves the old apps wrote beside a ROM are adopted on first write. | SPEC §7; DESIGN §6.3, §6.5 row 4, Q7 |
| 2026-11-26 | **Super Game Boy is manual only.** The Game Boy core never reads the cartridge's SGB flag, so detection never selects SGB; it stays a setting. | DESIGN §3.6 C1, Q14 |
| 2026-11-26 | **SNES high-resolution (512-wide) and 239-line output go to the backlog.** The SNES core draws 256x224; the rest is new work, not part of the merge. | DESIGN §3.6 C7; backlog issue |

Standing answers taken from the design's recommendations (no owner objection raised, recorded so the choice is
visible): Q1 keep the Game Boy copy of the SPC if a shared crate cannot hold both outputs byte-identical; Q2 SNES
signature threshold 8 of 13; Q3 RGBA8888 at the frame boundary; Q4 `tendy-gba` may depend on `tendy-gb`; Q5 port
the system-only front features in #25-#27; Q8 a "Find the NES app folder" button on first run; Q9 the update check
is a manual menu row; Q13 64 MiB archive entry cap; Q15 new save states are containers the old apps cannot read.
