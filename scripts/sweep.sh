#!/usr/bin/env bash
# The library sweep (DESIGN §8; issue #28): run every ROM in one folder through detection and its
# core with no window and no audio device, and print one row per file plus a tally per system —
# draws / blank / refused.
#
#   usage: bash scripts/sweep.sh <rom-dir> [frames] [blank-frames] [--sample]
#
# Example: one system's library
#   bash scripts/sweep.sh /mnt/roms/snes 600 1800
#   bash scripts/sweep.sh ../gb-library 1200 1300 --sample
#
# <rom-dir> is an argument and nothing is baked in: the tree is public, a cartridge collection is
# not. Rows name files relative to the folder, never the folder itself, and the sweep writes
# nothing — results with file paths are not committed, so the counts are all that enter the tree.
#
# Frames: the sample frame every row is judged on (default 600), and the longer horizon a row that
# is one colour at the sample is given before it is called blank (default 1800). `--sample` judges
# only those two frames, the way the Game Boy and Advance source sweeps did; without it, a
# cartridge that drew at any frame of the run counts as drawing, the Super NES sweep's rule.
#
# The runner is `headless::cli`, the code behind `tendyemu --headless --frames N <folder>`. Until
# main.rs takes the flag, this script reaches the same function through its integration test; the
# test is #[ignore]d, so the ordinary test run never sweeps a library.
#
# A verdict is one of:
#   draws     the frame at the sample has more than one colour, or (without --sample) the run drew
#             at some frame
#   blank     one colour on every frame of the run it was given
#   refused:  detection or the core said no; the rest of the cell is the loader's own reason
#   crash:    the core panicked; one cartridge is one row, the sweep carries on
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ $# -lt 1 ]; then
  echo "usage: $0 <rom-dir> [frames] [blank-frames] [--sample]" >&2
  exit 2
fi

ROM_DIR="$1"
shift
if [ ! -d "$ROM_DIR" ]; then
  echo "sweep: $ROM_DIR is not a directory" >&2
  exit 2
fi

FRAMES=600
BLANK_FRAMES=1800
SAMPLE=0
if [ $# -ge 1 ] && [ "$1" != "--sample" ]; then
  FRAMES="$1"
  shift
fi
if [ $# -ge 1 ] && [ "$1" != "--sample" ]; then
  BLANK_FRAMES="$1"
  shift
fi
if [ $# -ge 1 ] && [ "$1" = "--sample" ]; then
  SAMPLE=1
  shift
fi
if [ $# -ne 0 ]; then
  echo "sweep: unexpected argument: $1" >&2
  exit 2
fi

echo "sweep: $ROM_DIR, $FRAMES frames (one-colour rows to $BLANK_FRAMES)" >&2

cd "$ROOT" || exit 1
TENDY_SWEEP_DIR="$ROM_DIR" \
TENDY_SWEEP_FRAMES="$FRAMES" \
TENDY_SWEEP_BLANK_FRAMES="$BLANK_FRAMES" \
TENDY_SWEEP_SAMPLE="$SAMPLE" \
  cargo test --release -p tendyemu --test headless sweep_a_folder -- --exact --ignored --nocapture
