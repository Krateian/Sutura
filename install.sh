#!/usr/bin/env bash
# Sutura installer.
#
# Installs the two virtualenvs, the CLI wrapper, and the Dolphin
# service menu under the user's home. Re-running is safe.
set -euo pipefail

SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"

MAIN_PY="${PYTHON:-python3}"

die() { echo "error: $*" >&2; exit 1; }

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

echo "==> virtualenv (stage 1: PyMeshLab) [$MAIN_PY]"
[ -d "$APP_DIR/venv" ] || "$MAIN_PY" -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements.txt"

echo "==> virtualenv (stage 2: manifold3d) [$VENV311_PY]"
[ -d "$APP_DIR/venv311" ] || "$VENV311_PY" -m venv "$APP_DIR/venv311"
"$APP_DIR/venv311/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv311/bin/pip" install --quiet -r "$SRC/requirements-311.txt"

echo "==> copying application files"
install -m 0755 "$SRC/sutura/repair.py"        "$APP_DIR/repair.py"
install -m 0755 "$SRC/sutura/manifold_bridge.py" "$APP_DIR/manifold_bridge.py"
install -m 0755 "$SRC/sutura/gui.py"           "$APP_DIR/gui.py"
install -m 0755 "$SRC/sutura/open.sh"          "$APP_DIR/open.sh"

echo "==> CLI wrapper"
cat > "$BIN_DIR/sutura" <<EOF
#!/usr/bin/env bash
exec "\$HOME/.local/share/sutura/venv/bin/python" "\$HOME/.local/share/sutura/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

echo "==> Dolphin service menu"
sed "s|%HOME%|$HOME|g" "$SRC/share/sutura.desktop" > "$SERVICE_DIR/sutura.desktop"
chmod 0755 "$SERVICE_DIR/sutura.desktop"
if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

echo
echo "Installed."
echo "  CLI:       $BIN_DIR/sutura <file.stl|file.3mf>"
echo "  GUI:       $APP_DIR/gui.py"
echo "  Dolphin:   right-click an STL/3MF -> Repair with Sutura"