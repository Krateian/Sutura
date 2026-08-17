#!/usr/bin/env bash
# Sutura macOS installer (Apple Silicon / Intel, conda).
#
# The Linux installer uses two Python virtualenvs and a pip-only flow. On
# macOS, pymeshlab has no PyPI wheel for Apple Silicon, so it must come from
# conda-forge; everything else lives in one conda environment. This script
# automates that setup.
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: this installer is macOS-only. On Linux use ./install.sh" >&2
    exit 1
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
ENV_NAME="sutura-env"
PY_VERSION="3.11"

die() { echo "error: $*" >&2; exit 1; }

# 1) Homebrew ---------------------------------------------------------------
if ! command -v brew >/dev/null 2>&1; then
    echo "Homebrew is not installed."
    echo "Install it from the official site: https://brew.sh"
    echo "(it may prompt for sudo), then run this script again."
    exit 1
fi

# 2) Miniforge / conda ------------------------------------------------------
if ! command -v conda >/dev/null 2>&1; then
    echo "conda not found - installing Miniforge via Homebrew..."
    brew install miniforge
    echo "Miniforge installed. Restart your terminal (or run 'conda init' first),"
    echo "then run this script again."
    exit 1
fi

# 3) conda init must let us activate in a non-interactive shell --------------
if ! conda activate "$ENV_NAME" >/dev/null 2>&1; then
    # try init if not done yet
    if ! command -v conda >/dev/null 2>&1 || ! conda shell.bash hook >/dev/null 2>&1; then
        echo "conda is not initialized for this shell."
        echo "Run:  conda init \"\$(basename \"\$SHELL\")\""
        echo "then close and reopen your terminal, and run this script again."
        exit 1
    fi
    eval "$(conda shell.bash hook)"
fi

# 4) create the environment (idempotent) ------------------------------------
if ! conda env list | awk '{print $1}' | grep -qx "$ENV_NAME"; then
    echo "==> creating conda env $ENV_NAME (python $PY_VERSION)"
    conda create -y -n "$ENV_NAME" python="$PY_VERSION"
else
    echo "==> conda env $ENV_NAME already exists"
fi

# 5) install dependencies ---------------------------------------------------
echo "==> installing packages into $ENV_NAME"
conda install -y -n "$ENV_NAME" -c conda-forge pymeshlab
conda run -n "$ENV_NAME" pip install manifold3d trimesh PySide6-Essentials

# 6) verify ----------------------------------------------------------------
if ! conda run -n "$ENV_NAME" python -c \
    "import pymeshlab, manifold3d, trimesh; from PySide6 import QtWidgets; print('OK')"; then
    die "import check failed - dependencies not usable in $ENV_NAME"
fi
ENV_PY="$(conda run -n "$ENV_NAME" which python)"

# 7) copy the application files --------------------------------------------
mkdir -p "$APP_DIR" "$BIN_DIR"
for f in repair.py manifold_bridge.py gui.py __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/$f"
done
# the importable package layout (for 'from sutura import ...' and __init__)
mkdir -p "$APP_DIR/sutura"
for f in repair.py manifold_bridge.py gui.py __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/sutura/$f"
done

# 8) CLI wrapper ------------------------------------------------------------
cat > "$BIN_DIR/sutura" <<EOF
#!/bin/bash
exec "$ENV_PY" "$APP_DIR/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

# 9) GUI launcher -----------------------------------------------------------
cat > "$BIN_DIR/sutura-gui" <<EOF
#!/bin/bash
exec "$ENV_PY" "$APP_DIR/gui.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura-gui"

# 10) summary ---------------------------------------------------------------
echo
echo "Installed."
echo "  conda env : $ENV_NAME (python $PY_VERSION)"
echo "  python    : $ENV_PY"
echo "  files     : $APP_DIR/"
echo "  CLI       : $BIN_DIR/sutura <file.stl|file.3mf>"
echo "  GUI       : $BIN_DIR/sutura-gui"
echo
echo "Usage:"
echo "  sutura model.stl --human"
echo "  sutura-gui"