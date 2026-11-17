#!/usr/bin/env bash
# Remove the Rust (Python-free) Sutura install, on Linux or macOS.
#
#   bash uninstall-rust.sh            # remove the program, KEEP user data
#   bash uninstall-rust.sh --purge    # also remove history/learning and config
#   SUTURA_DRY_RUN=1 bash uninstall-rust.sh   # show what would be removed
#
# The counterpart of `uninstall.sh`. That script also has to unwind a conda env;
# this one removes a plain directory tree, so it has no environment step. It keeps
# `data/` by default for the same reason `uninstall.sh` does: history and the
# learning counters are the user's, not the installer's.
#
# Both platforms are handled unconditionally: each `remove_path` is a no-op when
# the path does not exist, so one script serves a Linux install, a macOS install,
# or a machine that has had both.
set -uo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
CONFIG_DIR="$HOME/.config/sutura"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"
APPLICATIONS_DIR="$HOME/.local/share/applications"
ICON_ROOT="$HOME/.local/share/icons/hicolor"
DRY_RUN="${SUTURA_DRY_RUN:-0}"
PURGE=0

say() { printf '==> %s\n' "$*"; }
remove_path() {
    local p="$1"
    [ -e "$p" ] || [ -L "$p" ] || return 0
    if [ "$DRY_RUN" = "1" ]; then
        printf '    (dry run) remove %s\n' "$p"
    else
        rm -rf "$p"
    fi
}

while [ $# -gt 0 ]; do
    case "$1" in
        --purge) PURGE=1; shift ;;
        -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

say "removing the Rust Sutura install"

# Programs and integration first; user data is decided last.
remove_path "$BIN_DIR/sutura"
remove_path "$BIN_DIR/sutura-gui"
# macOS: the bundle and its Finder integration (no-ops elsewhere).
remove_path "$HOME/Applications/Sutura.app"
remove_path "$HOME/Library/Services/Sutura Quick Action.workflow"
remove_path "$HOME/Library/Logs/Sutura"
remove_path "$SERVICE_DIR/sutura.desktop"
remove_path "$APPLICATIONS_DIR/sutura-app.desktop"
for size_dir in "$ICON_ROOT"/*/apps/sutura.png; do
    [ -e "$size_dir" ] && remove_path "$size_dir"
done

if [ "$PURGE" = "1" ]; then
    say "removing $APP_DIR (including user data) because of --purge"
    remove_path "$APP_DIR"
    remove_path "$CONFIG_DIR"
else
    say "removing $APP_DIR (keeping user data)"
    # Remove everything except data/, so history and learning survive.
    if [ -d "$APP_DIR" ]; then
        for entry in "$APP_DIR"/* "$APP_DIR"/.[!.]*; do
            [ -e "$entry" ] || continue
            [ "$(basename "$entry")" = "data" ] && continue
            remove_path "$entry"
        done
    fi
    if [ -d "$APP_DIR/data" ]; then
        say "user data kept at $APP_DIR/data (use --purge to remove it)"
    fi
fi

# PATH: strip the marked line that install.sh/install-rust.sh added. Written
# through a temp file and moved back so a symlinked rc file stays a symlink.
for rc in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile"; do
    [ -f "$rc" ] || continue
    grep -qF "# added by Sutura installer" "$rc" 2>/dev/null || continue
    if [ "$DRY_RUN" = "1" ]; then
        printf '    (dry run) strip PATH line from %s\n' "$rc"
        continue
    fi
    tmp="$(mktemp)"
    grep -vF "# added by Sutura installer" "$rc" > "$tmp" || true
    cat "$tmp" > "$rc"
    rm -f "$tmp"
    say "cleaned PATH in $rc"
done

# Refresh the desktop caches so the removed entries disappear immediately.
if command -v update-desktop-database >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
    update-desktop-database "$APPLICATIONS_DIR" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
    gtk-update-icon-cache -f -t "$ICON_ROOT" >/dev/null 2>&1 || true
fi
if command -v kbuildsycoca6 >/dev/null 2>&1 && [ "$DRY_RUN" != "1" ]; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi
if [ "$(uname -s)" = "Darwin" ] && [ "$DRY_RUN" != "1" ]; then
    # The Services menu belongs to pbs; it refuses workflow bundles via
    # lsregister, so this is the correct refresh on macOS.
    /System/Library/CoreServices/pbs -update 2>/dev/null || true
fi

if [ "$PURGE" = "1" ]; then
    say "removed, including user data"
else
    say "removed; user data kept (--purge also removes it)"
fi
