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
# Two things the captures must reproduce from a *packaged* install, not a
# development build:
#   * the fTetWild sidecar is present, so the method-health band stays empty and
#     Engines says "installed" -- a document should show what a user sees, and a
#     dev build without the sidecar shows a permanent "#7 skip" warning;
#   * the repair history and settings live in a throwaway HOME, so nothing lands
#     in the real ~/.local or ~/.config (see the history guard in repair_cmd.rs).
#
# Usage:
#   scripts/generate_screenshots_rust.sh [-o OUTDIR] [-b BINARY] [-f FTETWILD]
#
# Defaults: OUTDIR=/tmp/sutura-shots-rust, BINARY=rust/target/release/sutura-gui,
# FTETWILD from SUTURA_FTETWILD_BIN or a packaged/`third_party` build if found.

set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
OUT=${OUT:-/tmp/sutura-shots-rust}
BIN=${BIN:-$REPO/rust/target/release/sutura-gui}
FTETWILD=${FTETWILD:-${SUTURA_FTETWILD_BIN:-}}

while getopts "o:b:f:h" opt; do
  case "$opt" in
    o) OUT=$OPTARG ;;
    b) BIN=$OPTARG ;;
    f) FTETWILD=$OPTARG ;;
    h) sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option" >&2; exit 2 ;;
  esac
done

[ -x "$BIN" ] || { echo "ERROR: no GUI binary at $BIN" >&2; echo "build it: cd rust && cargo build --release -p sutura-gui" >&2; exit 4; }

# The sidecar is what makes the captures look like an installed product. Without
# it every frame carries "1 method(s) unavailable: #7 skip" and Engines reads
# "not found" -- true of this build, false of the release. Search the places
# `find_binary` itself looks, plus a sibling worktree.
if [ -z "$FTETWILD" ]; then
  for candidate in \
    "$REPO/third_party/ftetwild/bin/ftetwild" \
    "$(dirname "$BIN")/ftetwild" \
    "$(dirname "$BIN")/../Resources/bin/ftetwild" \
    /Users/krateian/sutura-wt/ftw-bin/third_party/ftetwild/bin/ftetwild \
    "$HOME"'/.local/share/sutura/bin/ftetwild'
  do
    if [ -x "$candidate" ]; then FTETWILD=$candidate; break; fi
  done
fi

# The sample corpus ships with the benchmark checkout, not the repo: the Artec
# scans are CC BY 4.0 third-party data (see tests/real-world-samples/
# ATTRIBUTION.md) and are not vendored here.
SAMPLES=${SAMPLES:-/Users/krateian/sutura-bench/wt-main/tests/real-world-samples}

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
# `CpuViewer::rasterize`), so the images are still the real UI.
export SUTURA_GUI_CPU=${SUTURA_GUI_CPU:-1}
# A packaged install has the sidecar beside the binary; this is the same lookup
# `find_binary` does, pointed at the build we found above.
if [ -n "$FTETWILD" ]; then
  export SUTURA_FTETWILD_BIN="$FTETWILD"
fi

echo "binary:   $BIN"
echo "  built:  $(date -r "$BIN" '+%Y-%m-%d %H:%M')"
echo "ftetwild: ${FTETWILD:-NOT FOUND (method-health band will show)}"
echo "samples:  $SAMPLES"
echo "output:   $SHOTS"
echo

FAIL=0

# Capture one view. Arguments are passed to the GUI.
#
# `--screenshot` grabs a few frames in and then closes, so `--exit-after` is what
# makes this terminate; without it the run would hang on a windowless machine.
capture() {
  local name=$1; shift
  local out="$SHOTS/$name.png"
  local rc
  # Each frame gets its own directory: a `_fixed` file left over from an earlier
  # frame made the GUI stop on its "Output already exists" prompt, which is a
  # screenshot of a dialog rather than of the view.
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

# A real scanned part, not a 12-triangle toy: the documentation has to show what
# a user's mesh looks like. `copper-key` is the one the Deck review picked: it
# reads at preview size (a recognisable object, not a blob) and has real defects
# (4 boundary loops, 5 components). Falls back to the repo fixture when the
# benchmark checkout is absent, so the script still produces a full set on a
# bare clone.
if [ -f "$SAMPLES/artec_copper-key.stl" ]; then
  HERO="$SAMPLES/artec_copper-key.stl"
else
  HERO="$FIX/case3_broken_cup.stl"
  echo "  (no sample corpus; hero mesh falls back to the repo fixture)"
fi

# The small fixtures stay for the views that need a dense defect list or a
# multi-object archive; the hero mesh is for the frames a reader looks at.
BOX="$FIX/case5_box_missing_face.stl"

echo "--- main window and file list"
# Before view, not After-with-heatmap: the heatmap's deviation ramp needs a
# reference, and without one it paints the whole part one flat colour, which
# reads as a rendering bug in a README.
# The main frame is the Before view exactly as a launch shows it (no scroll),
# which is why it is the only frame without `--settings-scroll`.
capture main-window "$HERO"
# , not : the capture waits for the *selected* file under
# , so a second file still repairing kept the window from closing and
# the frame timed out. Batch repair is what a two-file frame shows anyway.
capture file-list --repair-all "$HERO" "$BOX"

echo "--- settings panel, scrolled to the lower half"
capture settings-panel --settings-scroll 200 "$HERO"
# The panel is taller than any window; the method list and the analysis table
# only appear below the fold. ~380 px is the first screenful past the tiers.
capture settings-panel-lower --settings-scroll 380 "$HERO"

echo "--- methods and per-file method selection"
capture methods-panel "$HERO" --settings-scroll 520
# The editor itself has to be open with a hand-picked list, or this frame is a
# duplicate of `settings-panel-lower` (it was: same md5). `--file-methods` is the
# hook that turns "Use the panel selection" off and ticks a few methods.
capture file-methods --file-methods 1,3,10 --settings-scroll 420 "$HERO"

echo "--- analysis panel"
capture analysis-panel "$HERO" --settings-scroll 650

echo "--- 3MF object list"
THREE_MF="$TMP/three_objects.3mf"
if python3 "$REPO/tests/make_layered_multiobject_3mf.py" "$THREE_MF" >/dev/null 2>&1; then
  # The object list lives under the Analysis section; scroll it into view.
  capture objects-3mf "$THREE_MF" --settings-scroll 1000
else
  echo "  objects-3mf              SKIPPED (fixture generator failed)"
fi

echo "--- heatmap (3D) and defect colours"
# Same real mesh, so the two colourings can be compared in the docs.
# After, not Before: the deviation ramp is computed against the original, so it
# only exists on the repaired side -- a Before+Heatmap frame shows no colouring
# at all and would document nothing.
capture heatmap --repair --show-after --heatmap "$HERO"
capture defect-colors "$FIX/case3_broken_cup.stl" --defect-colors

echo "--- static before/after pair"
capture before-after --repair --open-compare --wait-frames 30 "$HERO"

echo "--- repeat picker"
capture repeat-picker "$HERO" --open-repeat-picker

echo "--- update dialog"
capture update-dialog "$HERO" --updates

echo "--- engines and changelog"
capture engines-panel "$HERO" --settings-scroll 760
capture changelog "$HERO" --open-changelog

# A duplicated frame looks like a pass and shows nothing: `file-methods` was a
# byte-for-byte copy of `settings-panel-lower` because the editor never opened.
echo
echo "--- duplicate check"
DUPES=$(cd "$SHOTS" && md5 -q *.png 2>/dev/null | sort | uniq -d)
if [ -n "$DUPES" ]; then
  echo "  DUPLICATE frames (identical bytes):" >&2
  for h in $DUPES; do
    echo -n "    " >&2
    md5 "$SHOTS"/*.png 2>/dev/null | awk -v h="$h" '$4==h {printf "%s ", $2}' >&2
    echo >&2
  done
  FAIL=$((FAIL + 1))
else
  echo "  all frames differ"
fi

echo
if [ "$FAIL" -gt 0 ]; then
  echo "$FAIL problem(s)" >&2
  exit 1
fi
echo "done: $(ls "$SHOTS"/*.png 2>/dev/null | wc -l | tr -d ' ') PNG(s) in $SHOTS"
