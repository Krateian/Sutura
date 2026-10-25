#!/usr/bin/env bash
# Sutura uninstaller. Removes everything installed by install.sh.
# User data (repair history + learning under data/) is kept by default; pass
# --purge to remove it too.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"

PURGE=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1 ;;
        -h|--help)
            echo "Usage: uninstall.sh [--purge]"
            echo "  --purge   also remove user data ($APP_DIR/data)"
            exit 0 ;;
        *) echo "uninstall.sh: unknown option: $arg" >&2; exit 2 ;;
    esac
done

if [ "$PURGE" -eq 1 ]; then
    echo "==> removing $APP_DIR (including user data)"
    if [ -t 0 ]; then
        printf 'This permanently removes your Sutura user data (repair history and learning). Continue? [y/N] '
        read -r reply
        case "$reply" in
            [yY]|[yY][eE][sS]) ;;
            *) echo "Aborted."; exit 1 ;;
        esac
    fi
    rm -rf "$APP_DIR"
else
    echo "==> removing $APP_DIR (keeping user data)"
    if [ -d "$APP_DIR" ]; then
        shopt -s dotglob nullglob
        for entry in "$APP_DIR"/*; do
            base="$(basename "$entry")"
            [ "$base" = "data" ] && continue
            rm -rf "$entry"
        done
        shopt -u dotglob nullglob
    fi
    # Explicitly clear the legacy fTetWild runtime dirs (covered by the loop
    # above; kept named so their removal is intentional, not incidental).
    rm -rf "$APP_DIR/venv311" "$APP_DIR/python311"
    if [ -d "$APP_DIR/data" ]; then
        echo "User data kept at $APP_DIR/data (re-run with --purge to remove it)"
    else
        rmdir "$APP_DIR" 2>/dev/null || true
    fi
fi

echo "Removing $BIN_DIR/sutura"
rm -f "$BIN_DIR/sutura"

# Remove the PATH line the installer may have added (nothing else is touched).
for rc in "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.bash_profile" "$HOME/.profile"; do
    [ -f "$rc" ] && grep -qF "# added by Sutura installer" "$rc" || continue
    grep -vF "# added by Sutura installer" "$rc" > "$rc.sutura-tmp" && cat "$rc.sutura-tmp" > "$rc"
    rm -f "$rc.sutura-tmp"
    echo "Removed the Sutura PATH line from $rc"
done

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