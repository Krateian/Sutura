#!/usr/bin/env bash
# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# Regenerate the Rust GUI screenshots from the real GUI.
#
# The Rust counterpart of scripts/generate_screenshots.py: it drives the actual
# binary offscreen and captures the same moments, so the images cannot drift from
# the UI. There is no Python here at all -- the repair runs in-process, so the
# script only has to launch `sutura-gui` with the flags that open each view.
#
# The English UI is forced regardless of the system locale (the standing rule for
# documentation screenshots): `--lang en` plus LANG/LC_ALL, because egui's
# `System` language reads the environment.
#
# Nothing is written outside the output directory and a throwaway HOME: the GUI
# persists settings (`~/.config/sutura/gui.json`), writes a crash log and can
# append repair history, so `HOME` and `SUTURA_DATA_DIR` are both redirected.
# That is not optional -- see the history guard in `repair_cmd.rs`.
#
# Usage:
#   scripts/generate_screenshots_rust.sh [-o OUTDIR] [-b BINARY]
#
# Defaults: OUTDIR=/tmp/sutura-shots-rust, BINARY=rust/target/release/sutura-gui

set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT=${OUT:-/tmp/sutura-shots-rust}
BIN=${BIN:-$REPO/rust/target/release/sutura-gui}

while getopts "o:b:h" opt; do
  case "$opt" in
    o) OUT=$OPTARG ;;
    b) BIN=$OPTARG ;;
    h) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option" >&2; exit 2 ;;
  esac
done

[ -x "$BIN" ] || { echo "ERROR: no GUI binary at $BIN" >&2; echo "build it: cd rust && cargo build --release -p sutura-gui" >&2; exit 4; }

FIX=$REPO/rust/sutura-geom/tests/fixtures
SHOTS="$OUT"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/sutura-shots-XXXXXX")
HOME_TMP="$TMP/home"
DATA_TMP="$TMP/data"
mkdir -p "$SHOTS" "$HOME_TMP" "$DATA_TMP"
trap 'rm -rf "$TMP"' EXIT

# English UI whatever the system locale says.
export LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 LANGUAGE=en

# Redirect everything the GUI can persist. `HOME` alone is not enough: the
# installers and the data path resolve independently, and a missing
# `SUTURA_DATA_DIR` is what let a gate append to the real history file twice.
export HOME="$HOME_TMP"
export SUTURA_DATA_DIR="$DATA_TMP"
export XDG_CONFIG_HOME="$HOME_TMP/.config"
# Software rendering: a headless CI machine has no GPU surface, and the wgpu path
# can block waiting for one. The CPU preview draws the same mesh (one renderer,
# ), so the images are still the real UI.
export SUTURA_GUI_CPU=${SUTURA_GUI_CPU:-1}

# Screenshots are compared by eye, so a stale build is the one failure mode that
# looks like a UI bug. Report what produced the images.
echo "binary: $BIN"
echo "  built: $(date -r "$BIN" '+%Y-%m-%d %H:%M')"
echo "output: $SHOTS"
echo

SHOT_HEIGHT=${SHOT_HEIGHT:-720}

# Capture one view. Arguments after `--` are passed to the GUI.
#
# `--screenshot` grabs three frames in and then closes, so `--exit-after` is what
# makes this terminate; without it the run would hang on a windowless machine.
capture() {
  local name=$1; shift
  local out="$SHOTS/$name.png"
  local rc
  # Each frame gets its own directory, and the `_fixed` output is removed
  # first: a file left over from an earlier frame made the GUI stop on its
  # "Output already exists" prompt, which is a screenshot of a dialog rather
  # than of the view.
  local work="$TMP/$name"
  mkdir -p "$work"
  local args=()
  for a in "$@"; do
    case "$a" in
      *.stl|*.obj|*.ply|*.3mf)
        cp "$a" "$work/" 2>/dev/null
        args+=("$work/$(basename "$a")") ;;
      *) args+=("$a") ;;
    esac
  done
  ( cd "$work" && timeout 300 "$BIN" "${args[@]}" --lang en \
      --screenshot "$out" --exit-after >"$TMP/$name.log" 2>&1 )
  rc=$?
  if [ -s "$out" ]; then
    printf '  %-24s %8d bytes\n' "$name.png" "$(stat -f%z "$out" 2>/dev/null || stat -c%s "$out")"
  else
    printf '  %-24s FAILED (rc=%d)\n' "$name.png" "$rc"
    tail -3 "$TMP/$name.log" | sed 's/^/      /'
    FAIL=$((FAIL + 1))
  fi
}

FAIL=0

# A realistic curved part with small holes: enough healthy surface that the
# report has a measured Fidelity and a low Risk, unlike the box fixture's
# "shape check unavailable" worst case.
PART="$FIX/case3_broken_cup.stl"
# Copies, not the fixtures themselves: a repair writes `<stem>_fixed.<ext>`
# beside the input, and the repo must stay clean.
cp "$PART" "$TMP/curved_part.stl" 2>/dev/null
PART="$TMP/curved_part.stl"
# A box with one face missing: the honest warning case (holes + unit warning).
BOX="$FIX/case5_box_missing_face.stl"
cp "$BOX" "$TMP/box_missing_face.stl" 2>/dev/null
BOX="$TMP/box_missing_face.stl"
CLEAN="$FIX/case1_open_cup.stl"
# Note: the GUI has no `-o` flag -- Python's did. It always writes
# `<stem>_fixed.<ext>` beside the input (`fixed_path`, and that is deliberate:
# keeping the extension is what stops a multi-object 3MF being flattened to STL).
# So the captures run on copies in $TMP and the originals stay pristine.

echo "--- main window and file list"
# The batch is the point of the screenshot: a repaired file shows the per-file
# decision column, the report and the analysis panel together.
capture main-window --repair --heatmap "$PART"
capture file-list --repair "$PART" "$BOX"

echo "--- settings panel, scrolled to the lower half"
capture settings-panel "$PART"
# The panel is taller than any window; the method list and the analysis table
# only appear below the fold.
capture settings-panel-lower --settings-scroll 780 "$PART"

echo "--- methods and per-file method selection"
capture methods-panel "$PART" --settings-scroll 960
capture file-methods "$PART" --settings-scroll 780

echo "--- analysis panel"
capture analysis-panel "$PART" --settings-scroll 1030

echo "--- 3MF object list"
THREE_MF="$TMP/three_objects.3mf"
if python3 "$REPO/tests/make_layered_multiobject_3mf.py" "$THREE_MF" >/dev/null 2>&1; then
  # The object list lives under the Analysis section; scroll it into view.
  capture objects-3mf "$THREE_MF" --settings-scroll 1000
else
  echo "  objects-3mf              SKIPPED (fixture generator failed)"
fi

echo "--- heatmap (3D) and defect colours"
capture heatmap "$PART" --heatmap
cp "$FIX/case3_broken_cup.stl" "$TMP/defect_kinds.stl" 2>/dev/null
capture defect-colors "$TMP/defect_kinds.stl" --defect-colors

echo "--- static before/after pair"
capture before-after --repair --open-compare --wait-frames 30 "$PART"

echo "--- repeat picker"
capture repeat-picker "$PART" --open-repeat-picker

echo "--- update dialog"
capture update-dialog "$PART" --updates

echo "--- engines and changelog"
capture engines-panel "$PART" --settings-scroll 930
capture changelog "$PART" --open-changelog

echo
if [ "$FAIL" -gt 0 ]; then
  echo "$FAIL capture(s) failed" >&2
  exit 1
fi
echo "done: $(find "$SHOTS" -name '*.png' -newermt '-10 minutes' | wc -l | tr -d ' ') PNG(s) in $SHOTS"
