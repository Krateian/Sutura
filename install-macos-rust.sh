#!/usr/bin/env bash
# Install the Rust (Python-free) Sutura build on macOS.
#
#   bash install-macos-rust.sh                       # .app/.dmg beside this script
#   bash install-macos-rust.sh --from X.dmg|X.app.tar.gz
#   bash install-macos-rust.sh --dry-run
#
# The counterpart of `install-macos.sh`, which builds a conda env. This installs
# the packaged `Sutura.app` and the Finder Quick Action, and touches nothing Python.
#
# Layout written:
#   ~/Applications/Sutura.app                         the GUI bundle + CLI inside it
#   ~/.local/bin/sutura, sutura-gui                   symlinks for the terminal
#   ~/Library/Services/Sutura Quick Action.workflow    Finder right-click action
#   ~/.local/share/sutura/                            Quick Action helper script
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
APPS_DIR="$HOME/Applications"
SERVICES_DIR="$HOME/Library/Services"
DRY_RUN="${SUTURA_DRY_RUN:-0}"
PKG=""
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

say() { printf '==> %s\n' "$*"; }
run() {
    if [ "$DRY_RUN" = "1" ]; then printf '    (dry run) %s\n' "$*"; else "$@"; fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --from) PKG="${2:?--from needs a path}"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [ "$(uname -s)" != "Darwin" ]; then
    echo "install-macos-rust.sh is the macOS installer; use install-rust.sh on Linux" >&2
    exit 1
fi

# Intel Macs were supported by the Python distribution, so detect the arch
# instead of assuming Apple Silicon.
case "$(uname -m)" in
    arm64|aarch64) RU_ARCH="arm64" ;;
    x86_64)        RU_ARCH="x86_64" ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

# Locate the package: explicit --from, else the newest dmg/app.tar.gz for this arch.
if [ -z "$PKG" ]; then
    PKG="$(ls -1t "$HERE"/Sutura-rust-v*-"$RU_ARCH".dmg "$HERE"/Sutura-rust-v*-"$RU_ARCH".app.tar.gz 2>/dev/null | head -1 || true)"
fi
if [ -z "$PKG" ] || [ ! -f "$PKG" ]; then
    echo "no package found: pass --from <Sutura-rust-v*-${RU_ARCH}.dmg|.app.tar.gz>" >&2
    exit 1
fi
say "package: $PKG"

# Verify the published sha256 when present, before mounting anything.
if [ -f "$PKG.sha256" ]; then
    ( cd "$(dirname "$PKG")" && shasum -a 256 -c "$(basename "$PKG").sha256" --quiet ) \
        || { echo "sha256 verification FAILED for $PKG" >&2; exit 1; }
    say "sha256 verified"
else
    say "note: no $PKG.sha256 beside the package; skipping integrity check"
fi

# Extract the .app into a staging directory. A .dmg is mounted read-only and the
# bundle copied out before detaching, so a failed detach cannot cost the payload.
STAGE="$(mktemp -d)"
trap 'test -n "${MNT:-}" && hdiutil detach "$MNT" -force >/dev/null 2>&1 || true; rm -rf "$STAGE"' EXIT
MNT=""
case "$PKG" in
    *.dmg)
        MNT="$STAGE/mnt"
        run mkdir -p "$MNT"
        [ "$DRY_RUN" = "1" ] || hdiutil attach "$PKG" -nobrowse -mountpoint "$MNT" >/dev/null
        APP_SRC="$(ls -d "$MNT"/*.app 2>/dev/null | head -1 || true)"
        if [ -z "$APP_SRC" ] && [ "$DRY_RUN" != "1" ]; then
            echo "no .app inside $PKG" >&2
            exit 1
        fi
        if [ -n "$APP_SRC" ]; then
            run cp -R "$APP_SRC" "$STAGE/Sutura.app"
        fi
        [ "$DRY_RUN" = "1" ] || hdiutil detach "$MNT" -force >/dev/null 2>&1 || true
        MNT=""
        ;;
    *)
        run tar -C "$STAGE" -xzf "$PKG"
        ;;
esac

if [ ! -d "$STAGE/Sutura.app" ] && [ "$DRY_RUN" != "1" ]; then
    echo "unexpected package layout: no Sutura.app extracted from $PKG" >&2
    exit 1
fi

# Install the bundle. Replace atomically-ish: copy next to the target, then swap,
# so an interrupted copy cannot leave a broken app.
say "installing to $APPS_DIR/Sutura.app"
run mkdir -p "$APPS_DIR" "$BIN_DIR" "$APP_DIR" "$SERVICES_DIR"
if [ -d "$STAGE/Sutura.app" ]; then
    run rm -rf "$STAGE/swap.app"
    run cp -R "$STAGE/Sutura.app" "$STAGE/swap.app"
    if [ "$DRY_RUN" != "1" ]; then
        rm -rf "$APPS_DIR/Sutura.app"
        mv "$STAGE/swap.app" "$APPS_DIR/Sutura.app"
    fi
fi

# Terminal entry points. The bundle carries both binaries, so the PATH commands
# and the Quick Action resolve to the same build.
for bin in sutura sutura-gui ftetwild; do
    if [ -x "$APPS_DIR/Sutura.app/Contents/MacOS/$bin" ]; then
        run ln -sf "$APPS_DIR/Sutura.app/Contents/MacOS/$bin" "$BIN_DIR/$bin"
    fi
done

# Finder Quick Action. The helper script lives in APP_DIR (not in the bundle) so
# it can be updated without re-installing the workflow, exactly as
# install-macos.sh does.
WF_SRC=""
for cand in "$STAGE/Sutura.app/Contents/Resources/Sutura Quick Action.workflow" \
            "$HERE/share/Sutura Quick Action.workflow" \
            "$(dirname "$HERE")/share/Sutura Quick Action.workflow"; do
    if [ -d "$cand" ]; then WF_SRC="$cand"; break; fi
done
HELPER=""
for cand in "$STAGE/Sutura.app/Contents/Resources/macos-quick-action.sh" \
            "$HERE/share/macos-quick-action.sh" \
            "$(dirname "$HERE")/share/macos-quick-action.sh"; do
    if [ -f "$cand" ]; then HELPER="$cand"; break; fi
done

if [ -n "$HELPER" ]; then
    run install -m 0755 "$HELPER" "$APP_DIR/macos-quick-action.sh"
    say "Quick Action helper: $APP_DIR/macos-quick-action.sh"
fi
if [ -n "$WF_SRC" ]; then
    run rm -rf "$SERVICES_DIR/Sutura Quick Action.workflow"
    run cp -R "$WF_SRC" "$SERVICES_DIR/"
    # Keep the workflow's version in sync with the bundle's.
    if [ "$DRY_RUN" != "1" ]; then
        VER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
               "$APPS_DIR/Sutura.app/Contents/Info.plist" 2>/dev/null || true)"
        WF_PLIST="$SERVICES_DIR/Sutura Quick Action.workflow/Contents/Info.plist"
        if [ -n "$VER" ] && [ -f "$WF_PLIST" ]; then
            /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $VER" "$WF_PLIST" 2>/dev/null || true
        fi
    fi
    # Automator workflow bundles are not applications: lsregister refuses them
    # (kLSNotAnApplicationErr). The Services menu belongs to pbs.
    if [ "$DRY_RUN" != "1" ]; then
        /System/Library/CoreServices/pbs -update 2>/dev/null || true
    fi
    say "Finder: right-click an STL/OBJ/3MF -> Quick Actions -> Sutura"
fi

# The bundle's CLI must run; a broken install should say so now, not later.
if [ "$DRY_RUN" != "1" ]; then
    if ! "$BIN_DIR/sutura" --version >/dev/null 2>&1; then
        echo "installed CLI does not run; check $APPS_DIR/Sutura.app" >&2
        exit 1
    fi
    say "installed: $("$BIN_DIR/sutura" --version)"
    # Gatekeeper: an unsigned/ad-hoc bundle carried through a browser may keep
    # its quarantine flag, which blocks first launch.
    if xattr -p com.apple.quarantine "$APPS_DIR/Sutura.app" >/dev/null 2>&1; then
        say "note: the app is quarantined; open it once with right-click -> Open"
    fi
fi

say "done"
say "  app   : $APPS_DIR/Sutura.app"
say "  CLI   : $BIN_DIR/sutura"
say "Remove everything with: bash uninstall-rust.sh"
