#!/usr/bin/env bash
# Install the Rust (Python-free) Sutura build from a release tarball.
#
#   bash install-rust.sh                    # install the tarball beside this script
#   bash install-rust.sh --from PKG.tar.gz  # install a specific package
#   bash install-rust.sh --dry-run          # show what would happen
#
# The counterpart of `install.sh`, which builds a Python venv. This one installs
# the packaged tree (`Sutura/` with bin/, share/) and touches nothing Python.
#
# Layout written:
#   ~/.local/share/sutura/            the payload (bin/, share/, licences)
#   ~/.local/bin/sutura, sutura-gui   symlinks into the payload
#   ~/.local/share/kio/servicemenus/sutura.desktop   Dolphin right-click menu
#   ~/.local/share/applications/sutura-app.desktop   desktop entry
#   ~/.local/share/icons/hicolor/*/apps/sutura.png   icons
#   data/ inside the payload is never touched, so an upgrade keeps user data.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"
APPLICATIONS_DIR="$HOME/.local/share/applications"
ICON_ROOT="$HOME/.local/share/icons/hicolor"
SERVICE_NAME="sutura.desktop"
APP_NAME="sutura-app.desktop"
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
        -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [ "$(uname -s)" != "Linux" ]; then
    echo "install-rust.sh is the Linux installer; use the .dmg + install-macos-rust.sh on macOS" >&2
    exit 1
fi

# Locate the package: explicit --from, else the newest tarball beside this file.
if [ -z "$PKG" ]; then
    PKG="$(ls -1t "$HERE"/Sutura-rust-v*-x86_64.tar.gz 2>/dev/null | head -1 || true)"
fi
if [ -z "$PKG" ] || [ ! -f "$PKG" ]; then
    echo "no package found: pass --from <Sutura-rust-*.tar.gz>" >&2
    exit 1
fi
say "package: $PKG"

# Verify the published sha256 when one sits beside the package, so a truncated or
# tampered download cannot be installed.
if [ -f "$PKG.sha256" ]; then
    ( cd "$(dirname "$PKG")" && sha256sum -c "$(basename "$PKG").sha256" --quiet ) \
        || { echo "sha256 verification FAILED for $PKG" >&2; exit 1; }
    say "sha256 verified"
else
    say "note: no $PKG.sha256 beside the package; skipping integrity check"
fi

# Unpack into a staging directory first: a failed extract must not leave a
# half-written install.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
run tar -C "$STAGE" -xzf "$PKG"
if [ ! -d "$STAGE/Sutura/bin" ] && [ "$DRY_RUN" != "1" ]; then
    echo "unexpected package layout: no Sutura/bin inside $PKG" >&2
    exit 1
fi

say "installing to $APP_DIR (existing data/ is kept)"
run mkdir -p "$APP_DIR" "$BIN_DIR" "$SERVICE_DIR" "$APPLICATIONS_DIR"
# Copy the payload AROUND data/: never delete or overwrite the user's runtime
# state (history, learning, config) that lives under it.
if [ -d "$APP_DIR/data" ]; then
    KEEP="$STAGE/keep-data"
    run mkdir -p "$KEEP"
    run cp -a "$APP_DIR/data" "$KEEP/data"
fi
run rm -rf "$APP_DIR/bin" "$APP_DIR/share"
if [ -d "$STAGE/Sutura" ]; then
    for entry in "$STAGE/Sutura"/*; do
        base="$(basename "$entry")"
        [ "$base" = "data" ] && continue
        run cp -a "$entry" "$APP_DIR/"
    done
fi
if [ -d "$STAGE/keep-data/data" ]; then
    run rm -rf "$APP_DIR/data"
    run cp -a "$STAGE/keep-data/data" "$APP_DIR/data"
fi

# Symlinks, not copies: the updater replaces $APP_DIR, so the PATH entry follows.
for bin in sutura sutura-gui; do
    if [ -f "$APP_DIR/bin/$bin" ]; then
        run ln -sf "$APP_DIR/bin/$bin" "$BIN_DIR/$bin"
    fi
done

# Dolphin ServiceMenu. The packaged tarball ships it under share/applications,
# but Dolphin only reads kio/servicemenus, so it is installed there explicitly
# with the executable bit KDE 6 requires.
if [ -f "$APP_DIR/share/applications/$SERVICE_NAME" ]; then
    run install -m 0755 "$APP_DIR/share/applications/$SERVICE_NAME" "$SERVICE_DIR/$SERVICE_NAME"
    say "Dolphin: right-click an STL/OBJ/3MF -> Repair with Sutura"
fi
if [ -f "$APP_DIR/share/applications/$APP_NAME" ]; then
    run install -m 0644 "$APP_DIR/share/applications/$APP_NAME" "$APPLICATIONS_DIR/$APP_NAME"
fi

# Icons: install every size the package carries and refresh the caches.
if [ -d "$APP_DIR/share/icons/hicolor" ]; then
    for size_dir in "$APP_DIR/share/icons/hicolor"/*/apps; do
        [ -d "$size_dir" ] || continue
        size="$(basename "$(dirname "$size_dir")")"
        run mkdir -p "$ICON_ROOT/$size/apps"
        run cp -f "$size_dir"/*.png "$ICON_ROOT/$size/apps/" 2>/dev/null || true
    done
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
        gtk-update-icon-cache -f -t "$ICON_ROOT" >/dev/null 2>&1 || true
    fi
fi
if command -v update-desktop-database >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
    update-desktop-database "$APPLICATIONS_DIR" >/dev/null 2>&1 || true
fi
if command -v kbuildsycoca6 >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

# PATH, via the shared helper so install.sh and this script agree.
if [ "$DRY_RUN" != "1" ]; then
    case ":$PATH:" in
        *":$BIN_DIR:"*) : ;;
        *)
            # shellcheck source=/dev/null
            . "$HERE/scripts/install_path.sh" 2>/dev/null || . "$(dirname "$HERE")/scripts/install_path.sh" 2>/dev/null || true
            if command -v add_sutura_path >/dev/null 2>&1; then
                updated="$(add_sutura_path || true)"
                [ -n "$updated" ] && say "added $BIN_DIR to PATH in: $updated"
            fi
            ;;
    esac
fi

# Verify what was installed, by running it.
if [ "$DRY_RUN" != "1" ]; then
    if ! "$BIN_DIR/sutura" --version >/dev/null 2>&1; then
        echo "installed CLI does not run; check $APP_DIR" >&2
        exit 1
    fi
    say "installed: $("$BIN_DIR/sutura" --version)"
fi

say "done"
say "  CLI   : $BIN_DIR/sutura"
say "  GUI   : $BIN_DIR/sutura-gui"
say "  data  : $APP_DIR/data (kept across upgrades)"
say "Remove everything with: bash uninstall-rust.sh"
