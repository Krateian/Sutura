#!/usr/bin/env bash
# Safe installer-helper tests: no real install, no downloads.
#
# DEFAULT DENY: this helper may run only with a throwaway HOME and with
# SUTURA_UNINSTALL_SKIP_CONDA=1 explicitly set. The uninstall test must never
# be allowed to see the real conda installation.
set -euo pipefail
case "${HOME:-}" in
    /tmp/*|/private/tmp/*|/var/folders/*|/var/tmp/*) ;;
    *)
        echo "refuse: HOME must point to a throwaway directory (/tmp, /private/tmp, /var/folders, /var/tmp)" >&2
        exit 2
        ;;
esac
if [ "${SUTURA_UNINSTALL_SKIP_CONDA:-0}" != "1" ]; then
    echo "refuse: set SUTURA_UNINSTALL_SKIP_CONDA=1 explicitly (default deny)" >&2
    exit 2
fi
export SUTURA_UNINSTALL_SKIP_CONDA=1
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/sutura-install-test.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
# shellcheck source=scripts/install_owner.sh
. "$REPO/scripts/install_owner.sh"

make_owned_env() {
    local H="$1"
    mkdir -p "$H/.local/share/sutura/data" "$H/.config/sutura" \
             "$H/.local/bin" "$H/miniforge3/envs/sutura-env/bin"
    echo keep > "$H/.local/share/sutura/data/keep"
    echo '{}' > "$H/.config/sutura/config.json"
    echo x > "$H/.local/bin/sutura"
    echo x > "$H/miniforge3/envs/sutura-env/bin/python"
    write_sutura_owner "$H/.local/share/sutura" install-macos.sh 1 \
        sutura-env "$H/miniforge3/envs/sutura-env" "$H/miniforge3"
}

run_uninstall() {
    local H="$1"; shift
    HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" \
        SUTURA_UNINSTALL_SKIP_CONDA=0 SUTURA_CONDA_BIN=/nonexistent \
        "$REPO/uninstall.sh" "$@" < /dev/null
}

# PATH helper: zsh and bash files, idempotent.
mkdir -p "$TMP/zsh" "$TMP/bash"
: > "$TMP/zsh/.zprofile"
HOME="$TMP/zsh" SHELL=/bin/zsh bash -c ". '$REPO/scripts/install_path.sh'; add_sutura_path >/dev/null; add_sutura_path >/dev/null"
grep -qF '# added by Sutura installer' "$TMP/zsh/.zshrc"
grep -qF '# added by Sutura installer' "$TMP/zsh/.zprofile"
test "$(grep -cF '# added by Sutura installer' "$TMP/zsh/.zshrc")" -eq 1
HOME="$TMP/bash" SHELL=/bin/bash bash -c ". '$REPO/scripts/install_path.sh'; add_sutura_path >/dev/null"
grep -qF '# added by Sutura installer' "$TMP/bash/.bashrc"
grep -qF '# added by Sutura installer' "$TMP/bash/.bash_profile"
echo "ok  PATH helper (zsh/bash, idempotent)"

# Installers: dry-run performs no side effects.
out="$(HOME="$TMP/dry" SUTURA_DRY_RUN=1 "$REPO/install.sh")"
grep -q 'dry-run' <<<"$out"
out="$(HOME="$TMP/dry" SUTURA_DRY_RUN=1 "$REPO/install-macos.sh")"
grep -q 'dry-run' <<<"$out"
test ! -e "$TMP/dry/.local"
echo "ok  installer dry-runs"

# Uninstall: keep data/config by default; --purge removes them.
H="$TMP/uninstall"
mkdir -p "$H/.local/share/sutura/data" "$H/.config/sutura" "$H/.local/bin" \
         "$H/Applications/Sutura.app" "$H/Library/Services/Sutura Quick Action.workflow" \
         "$H/Library/Logs/Sutura" "$H/.local/share/sutura.backup"
echo keep > "$H/.local/share/sutura/data/keep"
echo '{}' > "$H/.config/sutura/config.json"
echo x > "$H/.local/bin/sutura"
out="$(HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 SUTURA_DRY_RUN=1 \
    "$REPO/uninstall.sh")"
grep -q 'dry-run' <<<"$out"
HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 "$REPO/uninstall.sh"
test ! -e "$H/.local/bin/sutura"
test ! -e "$H/Applications/Sutura.app"
test ! -e "$H/Library/Services/Sutura Quick Action.workflow"
test ! -e "$H/.local/share/sutura.backup"
test -f "$H/.local/share/sutura/data/keep"
test -f "$H/.config/sutura/config.json"
HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 "$REPO/uninstall.sh" --purge
test ! -e "$H/.local/share/sutura/data"
test ! -e "$H/.config/sutura"
echo "ok  uninstall keeps data/config, --purge removes them"

# Uninstall with an rc file that holds only the installer marker, and a
# symlinked rc, must succeed and remove the marker without replacing the link.
H="$TMP/uninstall-rc"
mkdir -p "$H/.local/share/sutura/data" "$H/.config/sutura" "$H/dotfiles"
echo keep > "$H/.local/share/sutura/data/keep"
echo '{}' > "$H/.config/sutura/config.json"
printf 'export PATH="$HOME/.local/bin:$PATH"  # added by Sutura installer\n' > "$H/dotfiles/zshrc"
chmod 640 "$H/dotfiles/zshrc"
ln -s "$H/dotfiles/zshrc" "$H/.zshrc"
printf 'export PATH="$HOME/.local/bin:$PATH"  # added by Sutura installer\n' > "$H/.bashrc"
HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 "$REPO/uninstall.sh"
test -L "$H/.zshrc"
test ! -s "$H/dotfiles/zshrc"
! grep -qF "# added by Sutura installer" "$H/.zshrc"
test ! -s "$H/.bashrc"
test -f "$H/.local/share/sutura/data/keep"
test -f "$H/.config/sutura/config.json"
mode="$(stat -f '%Lp' "$H/dotfiles/zshrc" 2>/dev/null || stat -c '%a' "$H/dotfiles/zshrc" 2>/dev/null || true)"
[ -z "$mode" ] || test "$mode" = "640"
echo "ok  only-marker rc and symlinked rc are cleaned safely"

# A dry-run piped to grep -q closes stdout early; the producer must still exit 0.
H="$TMP/dry-pipe"
mkdir -p "$H/.local/share/sutura/data" "$H/.config/sutura"
HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 SUTURA_DRY_RUN=1 \
    "$REPO/uninstall.sh" | grep -q 'would remove'
echo "ok  uninstall dry-run survives a closed stdout"

# Install-ownership marker helper.
H="$TMP/owner"
mkdir -p "$H"
write_sutura_owner "$H" install-macos.sh 1 sutura-env \
    "$H/miniforge3/envs/sutura-env" "$H/miniforge3"
OWNER="$H/data/.sutura-install-owner"
test -f "$OWNER"
grep -qx 'owner=sutura' "$OWNER"
grep -qx 'conda_env_created=1' "$OWNER"
grep -qx "conda_env_path=$H/miniforge3/envs/sutura-env" "$OWNER"
mode="$(stat -f '%Lp' "$OWNER" 2>/dev/null || stat -c '%a' "$OWNER" 2>/dev/null || true)"
[ -z "$mode" ] || test "$mode" = "600"
echo "ok  install ownership marker"

# Ownership marker + --remove-env removes only the env, keeps data/config.
H="$TMP/owned-remove"
make_owned_env "$H"
test -d "$H/miniforge3/envs/sutura-env"
out="$(run_uninstall "$H" --remove-env)"
grep -q 'Removed conda env' <<<"$out"
test ! -e "$H/miniforge3/envs/sutura-env"
test ! -e "$H/.local/share/sutura/data/.sutura-install-owner"
test -f "$H/.local/share/sutura/data/keep"
test -f "$H/.config/sutura/config.json"
echo "ok  uninstall --remove-env removes the owned env"

# Marker + no flag + non-TTY keeps the env.
H="$TMP/owned-keep"
make_owned_env "$H"
out="$(run_uninstall "$H")"
grep -q 'use --remove-env' <<<"$out"
test -d "$H/miniforge3/envs/sutura-env"
test -f "$H/.local/share/sutura/data/.sutura-install-owner"
echo "ok  non-interactive uninstall keeps the owned env"

# Marker + --purge removes env, data and config.
H="$TMP/owned-purge"
make_owned_env "$H"
out="$(run_uninstall "$H" --purge)"
grep -q 'Removed conda env' <<<"$out"
test ! -e "$H/miniforge3/envs/sutura-env"
test ! -e "$H/.local/share/sutura/data"
test ! -e "$H/.config/sutura"
echo "ok  uninstall --purge removes the owned env and data"

# Marker + dry-run prints path/size and removes nothing.
H="$TMP/owned-dry"
make_owned_env "$H"
out="$(HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=0 \
    SUTURA_CONDA_BIN=/nonexistent SUTURA_DRY_RUN=1 "$REPO/uninstall.sh" --remove-env < /dev/null)"
grep -q "would remove conda env $H/miniforge3/envs/sutura-env" <<<"$out"
test -d "$H/miniforge3/envs/sutura-env"
echo "ok  dry-run reports the owned env, removes nothing"

# Skip always wins over --remove-env.
H="$TMP/owned-skip"
make_owned_env "$H"
out="$(HOME="$H" PATH="/usr/bin:/bin:/usr/sbin:/sbin" SUTURA_UNINSTALL_SKIP_CONDA=1 \
    SUTURA_CONDA_BIN=/nonexistent "$REPO/uninstall.sh" --remove-env < /dev/null)"
grep -q 'SUTURA_UNINSTALL_SKIP_CONDA=1' <<<"$out"
test -d "$H/miniforge3/envs/sutura-env"
echo "ok  SUTURA_UNINSTALL_SKIP_CONDA wins over --remove-env"

# No marker: old install keeps the env and prints the manual command.
H="$TMP/no-marker"
mkdir -p "$H/.local/share/sutura" "$H/miniforge3/envs/sutura-env/bin"
echo x > "$H/miniforge3/envs/sutura-env/bin/python"
out="$(run_uninstall "$H" --remove-env)"
grep -q 'ownership not confirmed' <<<"$out"
grep -q "conda env remove -p" <<<"$out"
test -d "$H/miniforge3/envs/sutura-env"
echo "ok  old install keeps the env and prints the manual command"

# Unsafe marker path is rejected.
H="$TMP/unsafe-marker"
mkdir -p "$H/.local/share/sutura/data" "$H/miniforge3/envs/sutura-env" "$H/not-an-env"
write_sutura_owner "$H/.local/share/sutura" install-macos.sh 1 \
    sutura-env "$H/not-an-env" "$H/miniforge3"
out="$(run_uninstall "$H" --remove-env)"
grep -q 'ownership not confirmed' <<<"$out"
test -d "$H/not-an-env"
test -d "$H/miniforge3/envs/sutura-env"
echo "ok  unsafe marker path is not removed"

# Quick Action dry-run selects the installed CLI when no bundled app exists.
H="$TMP/qa"
mkdir -p "$H/.local/bin"
printf '#!/bin/sh\nexit 0\n' > "$H/.local/bin/sutura"
chmod +x "$H/.local/bin/sutura"
out="$(HOME="$H" SUTURA_QA_APP=/nonexistent SUTURA_QA_DRY_RUN=1 \
    "$REPO/share/macos-quick-action.sh" 2>/dev/null)"
printf '%s\n' "$out" | grep -q "CLI=.*/.local/bin/sutura"
echo "ok  Quick Action dry-run resolves installed CLI"
