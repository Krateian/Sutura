#!/usr/bin/env bash
# Tests for install-rust.sh / uninstall-rust.sh / the Quick Action CLI lookup.
#
# Everything runs against a throwaway HOME; nothing touches the real one. The
# scripts refuse to run on the "wrong" OS, so this fakes `uname` to exercise the
# Linux path on a Mac (and vice versa when needed).
#
#   bash tests/test_install_rust.sh
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FAIL=0
pass() { printf '  ok   %s\n' "$*"; }
fail() { printf '  FAIL %s\n' "$*"; FAIL=1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
HOME_DIR="$WORK/home"
FAKEBIN="$WORK/bin"
mkdir -p "$HOME_DIR" "$FAKEBIN" "$WORK/pkg"
cat > "$FAKEBIN/uname" <<'EOF'
#!/bin/sh
echo Linux
EOF
chmod +x "$FAKEBIN/uname"

# A package in the layout scripts/package_rust.sh produces.
mkdir -p "$WORK/build/Sutura/bin" "$WORK/build/Sutura/share/applications" \
         "$WORK/build/Sutura/share/icons/hicolor/256x256/apps"
cp "$REPO/rust/target/release/sutura" "$WORK/build/Sutura/bin/" 2>/dev/null || {
    echo "SKIP: build rust/target/release/sutura first"; exit 0; }
cp "$REPO/rust/target/release/sutura-gui" "$WORK/build/Sutura/bin/" 2>/dev/null || true
sed 's#%HOME%/.local/share/sutura/open.sh#sutura#' \
    "$REPO/share/sutura.desktop" > "$WORK/build/Sutura/share/applications/sutura.desktop"
sed 's#%HOME%/.local/share/sutura/venv/bin/python %HOME%/.local/share/sutura/gui.py#sutura-gui#' \
    "$REPO/share/sutura-app.desktop" > "$WORK/build/Sutura/share/applications/sutura-app.desktop"
printf '\x89PNG\r\n\x1a\n' > "$WORK/build/Sutura/share/icons/hicolor/256x256/apps/sutura.png"
echo LICENSE > "$WORK/build/Sutura/LICENSE"
PKG="$WORK/pkg/Sutura-rust-v0.9.9-x86_64.tar.gz"
tar -C "$WORK/build" -czf "$PKG" Sutura
( cd "$WORK/pkg" && shasum -a 256 "$(basename "$PKG")" | awk '{print $1"  "$2}' > "$(basename "$PKG").sha256" )

env_run() { HOME="$HOME_DIR" PATH="$FAKEBIN:$PATH" "$@"; }

echo "install-rust.sh"

# A pre-existing data dir must survive the install.
mkdir -p "$HOME_DIR/.local/share/sutura/data"
echo PRECIOUS > "$HOME_DIR/.local/share/sutura/data/history.jsonl"

if env_run bash "$REPO/install-rust.sh" --from "$PKG" >"$WORK/out.log" 2>&1; then
    pass "install exits 0"
else
    fail "install failed: $(tail -2 "$WORK/out.log")"
fi

# İ6: Dolphin only reads kio/servicemenus, and KDE 6 refuses a non-executable
# menu, so both the path and the mode matter.
SM="$HOME_DIR/.local/share/kio/servicemenus/sutura.desktop"
[ -f "$SM" ] && pass "servicemenu in kio/servicemenus (İ6)" \
             || fail "servicemenu missing from kio/servicemenus (İ6)"
[ -x "$SM" ] && pass "servicemenu is executable (KDE 6)" \
             || fail "servicemenu is not executable"
grep -q '^Type=Service' "$SM" 2>/dev/null && pass "Type=Service" || fail "Type=Service missing"
grep -q '^Exec=sutura %F' "$SM" 2>/dev/null && pass "Exec=sutura %F" || fail "Exec wrong"

APP="$HOME_DIR/.local/share/applications/sutura-app.desktop"
[ -f "$APP" ] && pass "desktop entry installed" || fail "desktop entry missing"
grep -q '^Exec=sutura-gui' "$APP" 2>/dev/null && pass "Exec=sutura-gui" || fail "app Exec wrong"

[ -x "$HOME_DIR/.local/bin/sutura" ] && pass "CLI on PATH dir" || fail "CLI missing"
"$HOME_DIR/.local/bin/sutura" --version >/dev/null 2>&1 \
    && pass "installed CLI runs" || fail "installed CLI does not run"

[ "$(cat "$HOME_DIR/.local/share/sutura/data/history.jsonl" 2>/dev/null)" = "PRECIOUS" ] \
    && pass "user data survived the install" || fail "user data lost on install"

grep -qF "# added by Sutura installer" "$HOME_DIR/.zshrc" 2>/dev/null \
    && pass "PATH line added" || fail "PATH line missing"

echo
echo "uninstall-rust.sh"

env_run bash "$REPO/uninstall-rust.sh" >"$WORK/un.log" 2>&1
[ "$(cat "$HOME_DIR/.local/share/sutura/data/history.jsonl" 2>/dev/null)" = "PRECIOUS" ] \
    && pass "user data kept by default" || fail "user data removed without --purge"
[ ! -e "$HOME_DIR/.local/bin/sutura" ] && pass "CLI removed" || fail "CLI still present"
[ ! -e "$SM" ] && pass "servicemenu removed" || fail "servicemenu still present"
[ ! -e "$APP" ] && pass "desktop entry removed" || fail "desktop entry still present"
grep -qF "# added by Sutura installer" "$HOME_DIR/.zshrc" 2>/dev/null \
    && fail "PATH line not cleaned" || pass "PATH line cleaned"

# --purge removes the data too.
env_run bash "$REPO/install-rust.sh" --from "$PKG" >/dev/null 2>&1
env_run bash "$REPO/uninstall-rust.sh" --purge >/dev/null 2>&1
[ ! -d "$HOME_DIR/.local/share/sutura" ] && pass "--purge removes data" \
                                        || fail "--purge left the install behind"

echo
echo "Quick Action CLI lookup (İ7)"

# The bundle ships Contents/MacOS/sutura; the old script only knew sutura-cli.
APPB="$WORK/apps/Sutura.app/Contents/MacOS"
mkdir -p "$APPB"
cp "$REPO/rust/target/release/sutura" "$APPB/sutura" 2>/dev/null || true
chmod +x "$APPB/sutura"
RESOLVED="$(MACOS="$APPB" bash -c '
CLI_CMD=()
for cand in "$MACOS/sutura" "$MACOS/sutura-cli/sutura-cli" "$MACOS/sutura-cli"; do
    if [ -x "$cand" ]; then CLI_CMD=("$cand"); break; fi
done
printf "%s" "${CLI_CMD[0]:-}"')"
[ "$RESOLVED" = "$APPB/sutura" ] && pass "resolves Contents/MacOS/sutura (İ7)" \
                                || fail "resolved '$RESOLVED' (İ7)"

# The shipped script must contain that candidate, checked in order.
grep -q '"\$MACOS/sutura"' "$REPO/share/macos-quick-action.sh" \
    && pass "macos-quick-action.sh checks Contents/MacOS/sutura" \
    || fail "macos-quick-action.sh does not check Contents/MacOS/sutura"

echo
if [ "$FAIL" -eq 0 ]; then echo "ALL PASS"; else echo "FAILURES"; fi
exit "$FAIL"
