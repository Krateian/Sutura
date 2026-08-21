#!/usr/bin/env bash
# Sutura installer.
#
# Installs the two virtualenvs, the CLI wrapper, the Dolphin service menu
# and the hicolor icons under the user's home.
#
# It can run in two modes:
#   * local  - the repository files are next to this script (git clone)
#   * remote - this script is piped straight into bash; the repository is
#              fetched from GitHub first (curl -fsSL <url> | bash)
#
# Re-running is safe: existing virtualenvs are reused.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"

# The tarball used in remote mode. Override REPO_URL to install from a fork.
REPO_URL="${REPO_URL:-https://github.com/Krateian/Sutura/archive/refs/heads/main.tar.gz}"

MAIN_PY="${PYTHON:-python3}"

die() { echo "error: $*" >&2; exit 1; }

# --- locate the repository source ------------------------------------------
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" 2>/dev/null && pwd)"
if [ -f "$script_dir/requirements.txt" ]; then
    SRC="$script_dir"
    echo "==> local install from $SRC"
else
    SRC="$APP_DIR/repo"
    echo "==> fetching source from GitHub"
    mkdir -p "$SRC"
    curl -fsSL "$REPO_URL" | tar xz --strip-components=1 -C "$SRC"
fi
[ -f "$SRC/requirements.txt" ] || die "could not obtain requirements.txt"

# --- find a python3.11 for the manifold3d venv ----------------------------
VENV311_PY="$(command -v python3.11 || true)"
if [ -z "$VENV311_PY" ]; then
    if "$MAIN_PY" -c 'import sys; sys.exit(0 if sys.version_info[:2] == (3, 11) else 1)'; then
        VENV311_PY="$MAIN_PY"
    else
        echo "python3.11 not found."
        echo "manifold3d (stage 2) ships wheels only up to Python 3.13."
        echo "On Arch/CachyOS:            sudo pacman -S python311"
        echo "On Debian/Ubuntu (22.04+):  sudo apt install python3.11 python3.11-venv"
        echo "On Fedora:                  sudo dnf install python3.11"
        echo "Or install the venv311 manually and run install.sh again."
        exit 1
    fi
fi
command -v "$MAIN_PY" >/dev/null || die "python3 not found"

mkdir -p "$APP_DIR" "$BIN_DIR" "$SERVICE_DIR"

echo "==> virtualenv (stage 1: PyMeshLab + GUI) [$MAIN_PY]"
[ -d "$APP_DIR/venv" ] || "$MAIN_PY" -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements.txt"
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements-gui.txt"

echo "==> virtualenv (stage 2: manifold3d) [$VENV311_PY]"
[ -d "$APP_DIR/venv311" ] || "$VENV311_PY" -m venv "$APP_DIR/venv311"
"$APP_DIR/venv311/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv311/bin/pip" install --quiet -r "$SRC/requirements-311.txt"

echo "==> copying application files"
install -m 0755 "$SRC/sutura/repair.py"          "$APP_DIR/repair.py"
install -m 0755 "$SRC/sutura/manifold_bridge.py" "$APP_DIR/manifold_bridge.py"
install -m 0755 "$SRC/sutura/classification.py"  "$APP_DIR/classification.py"
install -m 0755 "$SRC/sutura/defects.py"         "$APP_DIR/defects.py"
install -m 0755 "$SRC/sutura/mesh_classifier.py" "$APP_DIR/mesh_classifier.py"
install -m 0755 "$SRC/sutura/updater.py"         "$APP_DIR/updater.py"
install -m 0755 "$SRC/sutura/gui.py"             "$APP_DIR/gui.py"
install -m 0755 "$SRC/sutura/heatmap.py"         "$APP_DIR/heatmap.py"
install -m 0755 "$SRC/sutura/heatmap_render.py"  "$APP_DIR/heatmap_render.py"
install -m 0755 "$SRC/sutura/before_after_render.py" "$APP_DIR/before_after_render.py"
install -m 0755 "$SRC/sutura/open.sh"            "$APP_DIR/open.sh"

echo "==> CLI wrapper"
cat > "$BIN_DIR/sutura" <<EOF
#!/usr/bin/env bash
exec "\$HOME/.local/share/sutura/venv/bin/python" "\$HOME/.local/share/sutura/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

echo "==> Dolphin service menu"
sed "s|%HOME%|$HOME|g" "$SRC/share/sutura.desktop" > "$SERVICE_DIR/sutura.desktop"
chmod 0755 "$SERVICE_DIR/sutura.desktop"

echo "==> application entry"
APP_ENTRIES="$HOME/.local/share/applications"
mkdir -p "$APP_ENTRIES"
sed "s|%HOME%|$HOME|g" "$SRC/share/sutura-app.desktop" > "$APP_ENTRIES/sutura.desktop"
chmod 0644 "$APP_ENTRIES/sutura.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_ENTRIES" >/dev/null 2>&1 || true
fi

echo "==> icons (hicolor)"
HICON="$HOME/.local/share/icons/hicolor"
if [ -d "$SRC/assets/icon" ]; then
    for s in 16 32 48 64 128 256; do
        install -d "$HICON/${s}x${s}/apps"
        install -m 0644 "$SRC/assets/icon/sutura-${s}.png" "$HICON/${s}x${s}/apps/sutura.png"
    done
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -f -t "$HICON" >/dev/null 2>&1 || true
    fi
else
    echo "    (icons skipped: assets/icon missing in source)"
fi

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

echo
echo "Installed."
echo "  CLI:       $BIN_DIR/sutura <file.stl|file.3mf>"
echo "  GUI:       $APP_DIR/gui.py"
echo "  Dolphin:   right-click an STL/3MF -> Repair with Sutura"