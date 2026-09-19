#!/usr/bin/env bash
# macos-quick-action.sh — Finder Quick Action handler for Sutura.
#
# The Dolphin ServiceMenu equivalent for macOS: called by the
# "Sutura Quick Action.workflow" (a Finder Quick Action that passes the
# selected files as arguments) and runs the BUNDLED sutura-cli inside a
# PyInstaller Sutura.app. It deliberately does NOT use the conda-env python
# from install-macos.sh, so the Quick Action works even when the dev
# environment is not set up.
#
# Behavior:
#   * locates Sutura.app in /Applications or ~/Applications (both layouts:
#     onefile exe or onedir exe subdir)
#   * Gatekeeper guard: if either the .app bundle OR the sutura-cli binary
#     carries com.apple.quarantine, tells the user to right-click -> Open
#     once first (never silently fails, never strips the attribute itself)
#   * filters inputs to .stl / .3mf (skips and reports anything else)
#   * repairs each file with sutura-cli and shows a native macOS notification
#     (single file: Health/Risk/Status; multiple files: one summary)
#   * writes a per-run log to ~/Library/Logs/Sutura/sutura-<timestamp>.log
#
# No windows are opened — feedback is via osascript display notification.

set -u
# No `set -e`: per-file errors are collected and reported, not fatal.

APP_DIR="$HOME/.local/share/sutura"
LOG_DIR="$HOME/Library/Logs/Sutura"
TS="$(date +%Y-%m-%d_%H-%M-%S)"
LOG="$LOG_DIR/sutura-$TS.log"
mkdir -p "$LOG_DIR"

notify() { # $1 = message, $2 = title (default "Sutura — Quick Action")
    osascript -e "display notification \"$1\" with title \"${2:-Sutura — Quick Action}\"" >/dev/null 2>&1
}

log() { printf '%s\n' "$*" >> "$LOG"; }

# --- locate Sutura.app -------------------------------------------------------
APP=""
for cand in "/Applications/Sutura.app" "$HOME/Applications/Sutura.app"; do
    if [ -d "$cand" ]; then APP="$cand"; break; fi
done

CLI=""
if [ -n "$APP" ]; then
    MACOS="$APP/Contents/MacOS"
    for cand in "$MACOS/sutura-cli/sutura-cli" "$MACOS/sutura-cli"; do
        if [ -x "$cand" ]; then CLI="$cand"; break; fi
    done
fi

if [ -z "$APP" ] || [ -z "$CLI" ]; then
    notify "Sutura.app not found in /Applications or ~/Applications. Install it or the bundled CLI is missing." "Sutura — Quick Action"
    log "ERROR: no Sutura.app / sutura-cli found (APP='$APP' CLI='$CLI')"
    # Already-notified condition: exit 0 so Automator does NOT layer its own
    # "Run Shell Script encountered an error" dialog on top of our notification.
    exit 0
fi

# --- Gatekeeper guard -------------------------------------------------------
# Quarantine can sit on the .app bundle root and/or on the inner binary; check
# both. If either is set, the exec may be intercepted by Gatekeeper on a
# downloaded/DMG install. We do NOT strip the attribute ourselves — the user
# must right-click -> Open once.
if xattr -p com.apple.quarantine "$APP" >/dev/null 2>&1 \
   || xattr -p com.apple.quarantine "$CLI" >/dev/null 2>&1; then
    notify "Sutura.app was downloaded from the internet — right-click it in Finder, choose Open once, then run the Quick Action again." "Sutura — Quick Action"
    log "ERROR: quarantine present on $APP or $CLI — user must right-click -> Open once"
    # Already-notified condition: exit 0 (see note above).
    exit 0
fi

# --- filter inputs ----------------------------------------------------------
FILES=()
SKIPPED=()
for f in "$@"; do
    low="$(printf '%s' "$f" | tr '[:upper:]' '[:lower:]')"
    case "$low" in
        *.stl|*.3mf) FILES+=("$f") ;;
        *) SKIPPED+=("$f") ;;
    esac
done

if [ "${#FILES[@]}" -eq 0 ]; then
    msg="No STL/3MF files selected."
    if [ "${#SKIPPED[@]}" -gt 0 ]; then
        msg+=" Skipped: $(basename "${SKIPPED[0]:-}")…"
    fi
    notify "$msg" "Sutura — Quick Action"
    log "INFO: no mesh files selected (skipped: ${SKIPPED[*]:-})"
    exit 0
fi

log "=== Sutura Quick Action $(date) ==="
log "CLI: $CLI"
log "files: ${#FILES[@]}"

# --- repair -----------------------------------------------------------------
FAILED=()
OK=0
HEALTH_LAST=""
for f in "${FILES[@]}"; do
    base="$(basename "$f")"
    log "--- $base ---"
    if out="$("$CLI" "$f" --human 2>&1)"; then
        OK=$((OK + 1))
        health="$(printf '%s\n' "$out" | grep -oE 'Health: [0-9]+/100   Risk: [0-9]+/100   Status: .*' | head -1)"
        if [ -n "$health" ]; then
            # "Health: 94/100   Risk: 18/100   Status: Safe to inspect"
            HEALTH_LAST="$(printf '%s\n' "$health" | sed -E 's#/100#/100#g')"
        fi
        log "OK: $health"
    else
        FAILED+=("$f")
        log "FAILED (exit $?): $(printf '%s\n' "$out" | tail -1)"
    fi
    printf '%s\n' "$out" >> "$LOG"
done

# --- notification -----------------------------------------------------------
total="${#FILES[@]}"
if [ "$total" -eq 1 ]; then
    if [ "${#FAILED[@]}" -eq 0 ]; then
        [ -z "$HEALTH_LAST" ] && HEALTH_LAST="Health: n/a  Risk: n/a  Status: completed"
        notify "$HEALTH_LAST" "Sutura — $(basename "${FILES[0]}")"
    else
        err="$(tail -1 "$LOG")"
        notify "Repair failed: $err — see $LOG" "Sutura — $(basename "${FILES[0]}")"
    fi
else
    nfailed="${#FAILED[@]}"
    nrepaired=$((total - nfailed))
    failed_names=""
    if [ "$nfailed" -gt 0 ]; then
        for f in "${FAILED[@]}"; do failed_names+="$(basename "$f"), "; done
        failed_names="${failed_names%, }"
    fi
    msg="$nrepaired/$total repaired, $nfailed failed"
    [ -n "$failed_names" ] && msg+=" — failed: $failed_names"
    msg+=" — log: $LOG"
    notify "$msg" "Sutura — Quick Action"
fi

log "=== done: $OK/$total ok, ${#FAILED[@]} failed ==="
# Per-file failures were already reported via notification and the log (the
# batch summary or the single-file failure message above). Exit 0 so Automator
# does not add its own error dialog. A non-zero exit is reserved for a genuine
# unexpected crash (e.g. a set -u unbound-variable abort) where no notification
# was shown, so at least something surfaces as a last resort.
exit 0