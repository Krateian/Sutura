#!/usr/bin/env bash
# Sutura uninstaller. Removes installed application artifacts on Linux and
# macOS. User data, configs and the conda environment are kept by default.
# --remove-env removes sutura-env only when this installer created it;
# --purge (with confirmation) also removes user data and configs.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
BACKUP_DIR="$HOME/.local/share/sutura.backup"
CONFIG_DIR="$HOME/.config/sutura"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"
APPLICATIONS_DIR="$HOME/Applications"
SERVICES_DIR="$HOME/Library/Services"
LOG_DIR="$HOME/Library/Logs/Sutura"
DRY_RUN="${SUTURA_DRY_RUN:-0}"

PURGE=0
REMOVE_ENV=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1; REMOVE_ENV=1 ;;
        --remove-env) REMOVE_ENV=1 ;;
        -h|--help)
            echo "Usage: uninstall.sh [--remove-env] [--purge]"
            echo "  --remove-env  remove sutura-env only if this installer created it"
            echo "  --purge       also remove user data ($APP_DIR/data), configs ($CONFIG_DIR)"
            echo "                and the Sutura-created sutura-env"
            echo "  Default: application files are removed; data, configs and sutura-env are kept."
            echo "  SUTURA_UNINSTALL_SKIP_CONDA=1 always keeps sutura-env."
            echo "  SUTURA_DRY_RUN=1 prints the actions without removing anything."
            exit 0 ;;
        *) echo "uninstall.sh: unknown option: $arg" >&2; exit 2 ;;
    esac
done

remove_path() {
    if [ "$DRY_RUN" = "1" ]; then
        echo "dry-run: would remove $*"
        return 0
    fi
    rm -rf "$@"
}

if [ "$PURGE" -eq 1 ] && [ "$DRY_RUN" != "1" ] && [ -t 0 ]; then
    printf 'This permanently removes your Sutura user data, configs and the Sutura-created conda environment. Continue? [y/N] '
    read -r reply
    case "$reply" in
        [yY]|[yY][eE][sS]) ;;
        *) echo "Aborted."; exit 1 ;;
    esac
fi

echo "==> removing Sutura application files"
remove_path "$BIN_DIR/sutura" "$BIN_DIR/sutura-gui"
remove_path "$BACKUP_DIR"
remove_path "$APPLICATIONS_DIR/Sutura.app"
remove_path "$SERVICES_DIR/Sutura Quick Action.workflow"
remove_path "$LOG_DIR"
remove_path "$SERVICE_DIR/sutura.desktop"
remove_path "$HOME/.local/share/applications/sutura.desktop"
remove_path "$HOME/.local/share/icons/hicolor/16x16/apps/sutura.png"
remove_path "$HOME/.local/share/icons/hicolor/32x32/apps/sutura.png"
remove_path "$HOME/.local/share/icons/hicolor/48x48/apps/sutura.png"
remove_path "$HOME/.local/share/icons/hicolor/64x64/apps/sutura.png"
remove_path "$HOME/.local/share/icons/hicolor/128x128/apps/sutura.png"
remove_path "$HOME/.local/share/icons/hicolor/256x256/apps/sutura.png"

# Dedicated conda environment (not user repair data). It is removed only when
# the install-ownership marker says this installer created it, and only after
# an interactive confirmation or an explicit --remove-env / --purge.
OWNER_FILE="$APP_DIR/data/.sutura-install-owner"

owner_get() {
    local file="$1" key="$2"
    [ -f "$file" ] || return 1
    sed -n "s/^${key}=//p" "$file" 2>/dev/null | head -n 1
}

find_conda() {
    if [ -n "${SUTURA_CONDA_BIN+x}" ]; then
        if [ -n "${SUTURA_CONDA_BIN:-}" ] && [ -x "$SUTURA_CONDA_BIN" ]; then
            printf '%s\n' "$SUTURA_CONDA_BIN"
            return 0
        fi
        return 1
    fi
    local cand
    for cand in "$(command -v conda 2>/dev/null || true)" \
                "$HOME/miniforge3/bin/conda" "$HOME/miniconda3/bin/conda" \
                "$HOME/anaconda3/bin/conda" \
                "/opt/homebrew/Caskroom/miniforge/base/bin/conda" \
                "/usr/local/Caskroom/miniforge/base/bin/conda" \
                "/opt/homebrew/bin/conda" "/usr/local/bin/conda"; do
        if [ -n "$cand" ] && [ -x "$cand" ]; then
            printf '%s\n' "$cand"
            return 0
        fi
    done
    return 1
}

env_path_listed() {
    local path="$1" base="$2"
    if [ -n "$CONDA_BIN" ] && [ -x "$CONDA_BIN" ]; then
        if "$CONDA_BIN" env list 2>/dev/null | awk '{print $NF}' \
                | grep -Fxq "$path"; then
            return 0
        fi
    fi
    if [ -n "$base" ] && [ "$path" = "$base/envs/sutura-env" ] \
            && [ -d "$path" ]; then
        return 0
    fi
    return 1
}

valid_owned_env() {
    local path="$1" name="$2" created="$3" base="$4"
    [ "$created" = "1" ] || return 1
    [ "$name" = "sutura-env" ] || return 1
    [ -n "$path" ] || return 1
    case "$path" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$path" in
        */envs/sutura-env) ;;
        *) return 1 ;;
    esac
    case "$path" in
        *..*) return 1 ;;
    esac
    [ ! -L "$path" ] || return 1
    [ -d "$path" ] || return 1
    env_path_listed "$path" "$base" || return 1
    return 0
}

manual_remove_hint() {
    local path="$1"
    if [ -n "$path" ]; then
        printf '  To remove it manually: conda env remove -p %q  (or rm -rf %q)\n' \
            "$path" "$path"
    else
        printf '  To remove it manually: conda env remove -n sutura-env\n'
    fi
}

remove_owned_env() {
    local path="$1"
    if [ -n "$CONDA_BIN" ] && [ -x "$CONDA_BIN" ]; then
        "$CONDA_BIN" env remove -y -p "$path" >/dev/null 2>&1 || true
    fi
    if [ -d "$path" ]; then
        rm -rf "$path" || true
    fi
    if [ -d "$path" ]; then
        echo "warning: could not remove $path" >&2
        manual_remove_hint "$path" >&2
        return 1
    fi
    rm -f "$OWNER_FILE" 2>/dev/null || true
    echo "Removed conda env $path"
    return 0
}

CONDA_BIN="$(find_conda || true)"
if [ "${SUTURA_UNINSTALL_SKIP_CONDA:-0}" = "1" ]; then
    echo "==> keeping conda env sutura-env (SUTURA_UNINSTALL_SKIP_CONDA=1)"
else
    OWNER_CREATED=""
    OWNER_NAME=""
    OWNER_PATH=""
    OWNER_BASE=""
    if [ -f "$OWNER_FILE" ]; then
        OWNER_CREATED="$(owner_get "$OWNER_FILE" conda_env_created || true)"
        OWNER_NAME="$(owner_get "$OWNER_FILE" conda_env_name || true)"
        OWNER_PATH="$(owner_get "$OWNER_FILE" conda_env_path || true)"
        OWNER_BASE="$(owner_get "$OWNER_FILE" conda_base || true)"
    fi
    ENV_PATH=""
    if [ "$OWNER_CREATED" = "1" ] && [ "$OWNER_NAME" = "sutura-env" ]; then
        ENV_PATH="$OWNER_PATH"
    fi
    if [ "$OWNER_CREATED" = "1" ] && [ -n "$OWNER_PATH" ] \
            && [ ! -e "$OWNER_PATH" ]; then
        echo "==> conda env already absent ($OWNER_PATH)"
        rm -f "$OWNER_FILE" 2>/dev/null || true
    elif valid_owned_env "$ENV_PATH" "$OWNER_NAME" "$OWNER_CREATED" \
            "$OWNER_BASE"; then
        SIZE="$(du -sh "$ENV_PATH" 2>/dev/null | awk 'NR==1{print $1}')"
        [ -n "$SIZE" ] || SIZE="unknown"
        if [ "$DRY_RUN" = "1" ]; then
            if [ "$REMOVE_ENV" -eq 1 ]; then
                echo "dry-run: would remove conda env $ENV_PATH ($SIZE)"
            else
                echo "dry-run: would keep conda env $ENV_PATH ($SIZE); use --remove-env"
            fi
        elif [ "$REMOVE_ENV" -eq 1 ]; then
            remove_owned_env "$ENV_PATH" || true
        elif [ -t 0 ]; then
            printf 'Sutura created the conda environment at %s (%s). Remove it? [y/N] ' \
                "$ENV_PATH" "$SIZE"
            reply=""
            if ! read -r reply; then reply=""; fi
            case "$reply" in
                [yY]|[yY][eE][sS]) remove_owned_env "$ENV_PATH" || true ;;
                *) echo "Keeping conda env $ENV_PATH." ;;
            esac
        else
            echo "==> keeping conda env $ENV_PATH ($SIZE); use --remove-env to remove it"
        fi
    else
        HINT_PATH=""
        if [ -n "$CONDA_BIN" ] && [ -x "$CONDA_BIN" ]; then
            HINT_PATH="$("$CONDA_BIN" env list 2>/dev/null \
                | awk '$1=="sutura-env"{print $NF; exit}')"
        fi
        if [ -z "$HINT_PATH" ]; then
            for cand in "$HOME/miniforge3/envs/sutura-env" \
                        "$HOME/miniconda3/envs/sutura-env" \
                        "$HOME/anaconda3/envs/sutura-env" \
                        "/opt/homebrew/Caskroom/miniforge/base/envs/sutura-env" \
                        "/usr/local/Caskroom/miniforge/base/envs/sutura-env"; do
                if [ -d "$cand" ]; then HINT_PATH="$cand"; break; fi
            done
        fi
        if [ -n "$OWNER_PATH" ] && [ -d "$OWNER_PATH" ]; then
            HINT_PATH="$OWNER_PATH"
        fi
        if [ -n "$HINT_PATH" ]; then
            SIZE="$(du -sh "$HINT_PATH" 2>/dev/null | awk 'NR==1{print $1}')"
            [ -n "$SIZE" ] || SIZE="unknown"
            echo "==> keeping conda env sutura-env at $HINT_PATH ($SIZE) (ownership not confirmed)"
            manual_remove_hint "$HINT_PATH"
        else
            echo "==> keeping conda env sutura-env (ownership not confirmed)"
            manual_remove_hint ""
        fi
    fi
fi

if [ "$PURGE" -eq 1 ]; then
    remove_path "$APP_DIR" "$CONFIG_DIR"
else
    echo "==> removing $APP_DIR (keeping user data)"
    if [ -d "$APP_DIR" ] && [ "$DRY_RUN" != "1" ]; then
        shopt -s dotglob nullglob
        for entry in "$APP_DIR"/*; do
            base="$(basename "$entry")"
            [ "$base" = "data" ] && continue
            rm -rf "$entry"
        done
        shopt -u dotglob nullglob
    elif [ "$DRY_RUN" = "1" ]; then
        echo "dry-run: would remove $APP_DIR except data/"
    fi
    remove_path "$APP_DIR/venv311" "$APP_DIR/python311"
    if [ -d "$APP_DIR/data" ]; then
        echo "User data kept at $APP_DIR/data (use --purge to remove it)"
    else
        [ "$DRY_RUN" = "1" ] || rmdir "$APP_DIR" 2>/dev/null || true
    fi
fi

# Remove the PATH marker from every file the installers may have touched.
for rc in "$HOME/.zshrc" "$HOME/.zprofile" "$HOME/.bashrc" \
          "$HOME/.bash_profile" "$HOME/.profile"; do
    [ -f "$rc" ] || continue
    grep -qF "# added by Sutura installer" "$rc" || continue
    if [ "$DRY_RUN" = "1" ]; then
        echo "dry-run: would remove the Sutura PATH line from $rc"
        continue
    fi
    tmp="$rc.sutura-tmp.$$"
    grep -vF "# added by Sutura installer" "$rc" > "$tmp"
    mv "$tmp" "$rc"
    echo "Removed the Sutura PATH line from $rc"
done

if [ "$DRY_RUN" != "1" ]; then
    command -v update-desktop-database >/dev/null 2>&1 && \
        update-desktop-database "$HOME/.local/share/applications" >/dev/null 2>&1 || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && \
        gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true
    command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 >/dev/null 2>&1 || true
    [ "$(uname -s)" = "Darwin" ] && /System/Library/CoreServices/pbs -update >/dev/null 2>&1 || true
fi

if [ "$PURGE" -eq 1 ]; then
    echo "Removed application files, user data and configs."
else
    echo "Removed application files. User data and configs were kept:"
    echo "  $APP_DIR/data"
    echo "  $CONFIG_DIR"
fi
