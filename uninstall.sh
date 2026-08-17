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

echo "Removing $HOME/.local/share/applications/sutura.desktop"
rm -f "$HOME/.local/share/applications/sutura.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true
fi

echo "Removing hicolor icons"
HICON="$HOME/.local/share/icons/hicolor"
for s in 16 32 48 64 128 256; do
    rm -f "$HICON/${s}x${s}/apps/sutura.png"
    rmdir "$HICON/${s}x${s}/apps" 2>/dev/null || true
    rmdir "$HICON/${s}x${s}" 2>/dev/null || true
done

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$HICON" >/dev/null 2>&1 || true
fi

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

echo "Done."