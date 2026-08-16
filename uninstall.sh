#!/usr/bin/env bash
# Sutura uninstaller. Removes everything installed by install.sh.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"

echo "Removing $APP_DIR"
rm -rf "$APP_DIR"

echo "Removing $BIN_DIR/sutura"
rm -f "$BIN_DIR/sutura"

echo "Removing $SERVICE_DIR/sutura.desktop"
rm -f "$SERVICE_DIR/sutura.desktop"

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

echo "Done."