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
# Qt plugin path must be pinned to PySide6's OWN Qt6 plugins. The conda env
# also carries a Qt5 stack (qt-main) whose plugins live under $ENV/plugins;
# when a GUI app is launched from Finder/Spotlight (minimal environment, no
# shell rc sourced) the Qt6 runtime can resolve the plugin search path to the
# Qt5 directory, fail to load its cocoa plugin ("Could not find the Qt platform
# plugin cocoa") and abort. Terminal launches work only because PySide6 happens
# to find its own path first. Pin it so the .app and sutura-gui work everywhere.
QT_PLUGIN_PATH="$(conda run -n "$ENV_NAME" python -c \
    'from PySide6.QtCore import QLibraryInfo; \
     print(QLibraryInfo.path(QLibraryInfo.LibraryPath.PluginsPath))')"

# 7) copy the application files --------------------------------------------
mkdir -p "$APP_DIR" "$BIN_DIR"
for f in repair.py manifold_bridge.py classification.py confidence.py defects.py mesh_classifier.py mesh_classifier_v2.py autorefine.py history.py updater.py gui.py heatmap.py heatmap_render.py before_after_render.py viewer_common.py viewer_data_render.py repair_score.py repair_score_config.json __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/$f"
done
# the importable package layout (for 'from sutura import ...' and __init__)
mkdir -p "$APP_DIR/sutura"
for f in repair.py manifold_bridge.py classification.py confidence.py defects.py mesh_classifier.py mesh_classifier_v2.py autorefine.py history.py updater.py gui.py heatmap.py heatmap_render.py before_after_render.py viewer_common.py viewer_data_render.py repair_score.py repair_score_config.json __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/sutura/$f"
done
install -m 0644 "$REPO_DIR/LICENSE" "$APP_DIR/LICENSE"

# developer/security: when installing from a git checkout, install the
# pre-push secret scan hook (harmless no-op for end-user installs)
if [ -d "$REPO_DIR/.git/hooks" ] && [ -f "$REPO_DIR/scripts/pre-push-security-check.sh" ]; then
    install -m 0755 "$REPO_DIR/scripts/pre-push-security-check.sh" "$REPO_DIR/.git/hooks/pre-push"
fi

# 8) CLI wrapper ------------------------------------------------------------
cat > "$BIN_DIR/sutura" <<EOF
#!/bin/bash
exec "$ENV_PY" "$APP_DIR/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

# 9) GUI launcher -----------------------------------------------------------
cat > "$BIN_DIR/sutura-gui" <<EOF
#!/bin/bash
export QT_PLUGIN_PATH="$QT_PLUGIN_PATH"
exec "$ENV_PY" "$APP_DIR/gui.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura-gui"

# 9b) Native macOS app (Spotlight: Cmd+Space -> "Sutura") -------------------
# Minimal .app wrapper around the sutura-gui launcher: a real .app bundle in
# ~/Applications so Spotlight finds it by name and launches the GUI without a
# terminal. Idempotent (re-running recreates it cleanly).
APPLICATIONS_DIR="$HOME/Applications"
APP="$APPLICATIONS_DIR/Sutura.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cat > "$APP/Contents/MacOS/Sutura" <<EOF
#!/bin/bash
export QT_PLUGIN_PATH="$QT_PLUGIN_PATH"
exec "$BIN_DIR/sutura-gui" "\$@"
EOF
chmod 0755 "$APP/Contents/MacOS/Sutura"
cp "$REPO_DIR/assets/icon/Sutura.icns" "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Sutura</string>
    <key>CFBundleIdentifier</key><string>com.krateian.sutura</string>
    <key>CFBundleName</key><string>Sutura</string>
    <key>CFBundleDisplayName</key><string>Sutura</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$REPO_DIR/sutura/repair.py" | head -1)</string>
</dict>
</plist>
EOF
# refresh the Launch Services + Spotlight index so the app is findable
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP" 2>/dev/null || true
mdimport "$APP" 2>/dev/null || true

# 9c) Finder Quick Action (Dolphin ServiceMenu eşdeğeri) --------------------
# A Finder right-click integration equivalent to the KDE Dolphin ServiceMenu:
# an Automator .workflow installed into ~/Library/Services that appears under
# Finder's "Quick Actions" and calls the BUNDLED sutura-cli inside a
# PyInstaller Sutura.app (independent of this conda dev environment). The
# helper script lives in $APP_DIR (like open.sh) so it can be updated without
# re-installing the workflow; the workflow only points at it. Idempotent:
# re-running removes and re-copies the workflow and refreshes LaunchServices.
install -m 0755 "$REPO_DIR/share/macos-quick-action.sh" "$APP_DIR/macos-quick-action.sh"
SERVICES_DIR="$HOME/Library/Services"
mkdir -p "$SERVICES_DIR"
rm -rf "$SERVICES_DIR/Sutura Quick Action.workflow"
cp -R "$REPO_DIR/share/Sutura Quick Action.workflow" "$SERVICES_DIR/"
# Automator workflow bundles are NOT applications: lsregister refuses them
# (kLSNotAnApplicationErr, -10811) and registers nothing. The Services menu
# is owned by the pbs agent; pbs -update rescans changed Services and applies
# them immediately to all running apps. Idempotent (re-running just rescans).
/System/Library/CoreServices/pbs -update 2>/dev/null || true

# 10) summary ---------------------------------------------------------------
echo
echo "Installed."
echo "  conda env : $ENV_NAME (python $PY_VERSION)"
echo "  python    : $ENV_PY"
echo "  files     : $APP_DIR/"
echo "  CLI       : $BIN_DIR/sutura <file.stl|file.3mf>"
echo "  GUI       : $BIN_DIR/sutura-gui"
echo "  macOS app : $APP  (Spotlight: press Cmd+Space and type Sutura)"
echo
echo "Usage:"
echo "  sutura model.stl --human"
echo "  sutura-gui"