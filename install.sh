#!/usr/bin/env bash
# Sutura installer.
#
# Installs the virtualenv, the CLI wrapper, the Dolphin service menu,
# the hicolor icons and (by default) the fTetWild fallback tier under the
# user's home.
#
# It can run in two modes:
#   * local  - the repository files are next to this script (git clone)
#   * remote - this script is piped straight into bash; the repository is
#              fetched from GitHub first (curl -fsSL <url> | bash)
#
# Re-running is safe: existing virtualenvs are reused.
set -euo pipefail

APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="$HOME/.local/share/kio/servicemenus"

# The tarball used in remote mode. Override REPO_URL to install from a fork.
REPO_URL="${REPO_URL:-https://github.com/Krateian/Sutura/archive/refs/heads/main.tar.gz}"

MAIN_PY="${PYTHON:-python3}"

die() { echo "error: $*" >&2; exit 1; }

# --- locate the repository source ------------------------------------------
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" 2>/dev/null && pwd)"
if [ -f "$script_dir/requirements.txt" ]; then
    SRC="$script_dir"
    echo "==> local install from $SRC"
else
    SRC="$APP_DIR/repo"
    echo "==> fetching source from GitHub"
    mkdir -p "$SRC"
    curl -fsSL "$REPO_URL" | tar xz --strip-components=1 -C "$SRC"
fi
[ -f "$SRC/requirements.txt" ] || die "could not obtain requirements.txt"

WANT_FTETWILD="${SUTURA_WITH_FTETWILD:-1}"

# pytetwild ships a cp312-abi3 wheel, so it runs on CPython 3.10-3.14. The main
# Sutura venv is built from $MAIN_PY; when that interpreter is in range the tier
# installs straight into the main venv (single environment, the default). Only
# an out-of-range interpreter falls back to a separate standalone Python 3.11
# (venv311), the leftover two-venv layout.
FTETWILD_IN_MAIN=0
if [ "$WANT_FTETWILD" = "1" ] && \
   "$MAIN_PY" -c 'import sys; sys.exit(0 if (3, 10) <= sys.version_info[:2] < (3, 15) else 1)' 2>/dev/null; then
    FTETWILD_IN_MAIN=1
fi

warn_python311() {
    echo "warning: fTetWild fallback tier skipped: python3.11 not found and" >&2
    echo "         $MAIN_PY is outside the pytetwild range (3.10-3.14)." >&2
    echo "         Everything else installs normally; add the tier later with" >&2
    echo "         'sutura ftetwild install'. To prepare a runtime:" >&2
    echo "         Debian/Ubuntu: sudo apt install python3.11 python3.11-venv" >&2
    echo "         Fedora:        sudo dnf install python3.11" >&2
    echo "         Arch:          unset SUTURA_NO_PYTHON_DOWNLOAD to let this" >&2
    echo "                        installer fetch a standalone build." >&2
}

# --- optional fTetWild runtime (only for an out-of-range $MAIN_PY) ----------
# Downloads python-build-standalone 3.11 so the tier has a compatible
# interpreter. Returns non-zero (and the caller warns, never aborts) on any
# failure: a missing fTetWild tier must never break the whole install.
fetch_python311() {
    for tool in curl tar sha256sum; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "missing tool: $tool" >&2
            return 1
        fi
    done

    arch="$(uname -m)"
    case "$arch" in
        x86_64|aarch64) pbs_arch="$arch" ;;
        *)
            echo "automatic python-build-standalone download not supported on $arch" >&2
            return 1
            ;;
    esac

    # python-build-standalone release for the fallback Python 3.11 runtime.
    # NOTE: PBS_TAG and the Python version must be bumped together with
    # scripts/build_appimage.sh.
    PBS_TAG="20260814"
    PBS_VERSION="3.11.16"
    PBS_BASE="https://github.com/indygreg/python-build-standalone/releases/download/$PBS_TAG"
    PBS_TARBALL="cpython-${PBS_VERSION}+${PBS_TAG}-${pbs_arch}-unknown-linux-gnu-install_only.tar.gz"

    echo "==> downloading standalone Python 3.11 (~30 MB download, ~100 MB installed)"
    dl_dir="$(mktemp -d "${TMPDIR:-/tmp}/sutura-py311.XXXXXX")"
    if ! curl -fL --retry 3 -o "$dl_dir/$PBS_TARBALL" "$PBS_BASE/$PBS_TARBALL" \
       || ! curl -fL --retry 3 -o "$dl_dir/SHA256SUMS" "$PBS_BASE/SHA256SUMS"; then
        rm -rf "$dl_dir"
        return 1
    fi

    # Verify SHA-256 against the SHA256SUMS the project publishes on the same
    # release (FAZ14): the tarball must verify OK, else abort and delete download.
    # See build_appimage.sh for details on --ignore-missing and || true under set -e.
    echo "==> verifying python-build-standalone SHA-256"
    pbs_ok=$(cd "$dl_dir" && sha256sum -c --ignore-missing SHA256SUMS 2>/dev/null | grep -c ': OK') || true
    if [ "$pbs_ok" -ne 1 ]; then
        rm -rf "$dl_dir"
        echo "SHA-256 verification failed for python-build-standalone (got $pbs_ok/1 OK)" >&2
        return 1
    fi

    echo "==> extracting Python 3.11 runtime to $APP_DIR/python311"
    tmp_extract="$(mktemp -d "${TMPDIR:-/tmp}/sutura-extract.XXXXXX")"
    if ! tar xzf "$dl_dir/$PBS_TARBALL" -C "$tmp_extract"; then
        rm -rf "$tmp_extract" "$dl_dir"
        return 1
    fi
    mkdir -p "$APP_DIR"
    rm -rf "$APP_DIR/python311"
    mv "$tmp_extract/python" "$APP_DIR/python311"
    rm -rf "$tmp_extract" "$dl_dir"

    [ -x "$APP_DIR/python311/bin/python3.11" ] || return 1
    return 0
}

VENV311_PY=""
if [ "$WANT_FTETWILD" = "1" ] && [ "$FTETWILD_IN_MAIN" = "0" ]; then
    VENV311_PY="$(command -v python3.11 || true)"
    if [ -z "$VENV311_PY" ] && [ -x "$APP_DIR/python311/bin/python3.11" ]; then
        # Reuse existing standalone Python 3.11 from a previous install run
        VENV311_PY="$APP_DIR/python311/bin/python3.11"
    fi
    if [ -z "$VENV311_PY" ] && [ "${SUTURA_NO_PYTHON_DOWNLOAD:-0}" != "1" ]; then
        if fetch_python311; then
            VENV311_PY="$APP_DIR/python311/bin/python3.11"
        fi
    fi
    [ -n "$VENV311_PY" ] || warn_python311
fi
command -v "$MAIN_PY" >/dev/null || die "python3 not found"

mkdir -p "$APP_DIR" "$BIN_DIR" "$SERVICE_DIR"

echo "==> virtualenv (native stage 1 + GUI) [$MAIN_PY]"
[ -d "$APP_DIR/venv" ] || "$MAIN_PY" -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements.txt"
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements-gui.txt"

if [ "$WANT_FTETWILD" = "1" ]; then
    if [ "$FTETWILD_IN_MAIN" = "1" ]; then
        echo "==> fTetWild fallback tier (pytetwild, ~3 MB) [$APP_DIR/venv]"
        # Upgrading from the old two-venv layout: drop the stale separate env.
        rm -rf "$APP_DIR/venv311" "$APP_DIR/python311"
        if ! "$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements-ftetwild.txt"; then
            echo "warning: fTetWild tier install failed; the rest of Sutura is fine." >&2
        fi
    elif [ -n "$VENV311_PY" ]; then
        echo "==> fTetWild fallback tier (pytetwild, ~3 MB) [$VENV311_PY]"
        [ -d "$APP_DIR/venv311" ] || "$VENV311_PY" -m venv "$APP_DIR/venv311" || true
        "$APP_DIR/venv311/bin/pip" install --quiet --upgrade pip >/dev/null 2>&1 || true
        "$APP_DIR/venv311/bin/pip" install --quiet -r "$SRC/requirements-ftetwild.txt" \
            || echo "warning: fTetWild tier install failed; the rest of Sutura is fine." >&2
    fi
fi

# The sutura_geom extension (Graft #13, Exact Refine #6, indirect tier) is
# imported from the main venv (graft/cast/availability) AND from
# indirect_bridge.py, which may run under venv311, so both venvs get it.
# The extension is REQUIRED: a failure aborts the install with a named,
# actionable message from the helper.
echo "==> required: sutura_geom Rust extension (Graft #13, ~0.7 MB)"
[ -f "$SRC/scripts/install_sutura_geom.sh" ] \
    || die "scripts/install_sutura_geom.sh is missing; sutura_geom is required"
bash "$SRC/scripts/install_sutura_geom.sh" "$APP_DIR/venv/bin/python" "$SRC"
if [ -x "$APP_DIR/venv311/bin/python" ]; then
    bash "$SRC/scripts/install_sutura_geom.sh" "$APP_DIR/venv311/bin/python" "$SRC"
fi

echo "==> copying application files"
# __init__.py is deliberately not copied: the installed layout is flat (no
# package import), so repair.py/gui.py resolve their flat imports from APP_DIR
# itself. The AppImage/macOS package layouts copy it separately.
install -m 0755 "$SRC/sutura/repair.py"          "$APP_DIR/repair.py"
install -m 0755 "$SRC/sutura/ftetwild_bridge.py" "$APP_DIR/ftetwild_bridge.py"
install -m 0755 "$SRC/sutura/indirect_bridge.py"  "$APP_DIR/indirect_bridge.py"
install -m 0755 "$SRC/sutura/classification.py"  "$APP_DIR/classification.py"
install -m 0755 "$SRC/sutura/confidence.py"      "$APP_DIR/confidence.py"
install -m 0755 "$SRC/sutura/defects.py"         "$APP_DIR/defects.py"
install -m 0755 "$SRC/sutura/topology.py"        "$APP_DIR/topology.py"
install -m 0755 "$SRC/sutura/mesh_classifier.py" "$APP_DIR/mesh_classifier.py"
install -m 0755 "$SRC/sutura/mesh_classifier_v2.py" "$APP_DIR/mesh_classifier_v2.py"
install -m 0755 "$SRC/sutura/autorefine.py"       "$APP_DIR/autorefine.py"
install -m 0755 "$SRC/sutura/local_exact.py"      "$APP_DIR/local_exact.py"
install -m 0755 "$SRC/sutura/triage.py"           "$APP_DIR/triage.py"
install -m 0755 "$SRC/sutura/engines.py"          "$APP_DIR/engines.py"
install -m 0755 "$SRC/sutura/ftetwild_manager.py" "$APP_DIR/ftetwild_manager.py"
install -m 0755 "$SRC/sutura/methods.py"          "$APP_DIR/methods.py"
install -m 0755 "$SRC/sutura/object_analysis.py"  "$APP_DIR/object_analysis.py"
install -m 0755 "$SRC/sutura/templates.py"        "$APP_DIR/templates.py"
install -m 0755 "$SRC/sutura/closing.py"          "$APP_DIR/closing.py"
install -m 0755 "$SRC/sutura/proxy_repair.py"     "$APP_DIR/proxy_repair.py"
install -m 0755 "$SRC/sutura/repeat_repair.py"    "$APP_DIR/repeat_repair.py"
install -m 0755 "$SRC/sutura/repeat_picker_render.py" "$APP_DIR/repeat_picker_render.py"
install -m 0755 "$SRC/sutura/mirror_repair.py"     "$APP_DIR/mirror_repair.py"
install -m 0755 "$SRC/sutura/wall_thickness.py"    "$APP_DIR/wall_thickness.py"
install -m 0755 "$SRC/sutura/shell_wrap.py"        "$APP_DIR/shell_wrap.py"
install -m 0755 "$SRC/sutura/history.py"         "$APP_DIR/history.py"
install -m 0755 "$SRC/sutura/repair_score.py"     "$APP_DIR/repair_score.py"
install -m 0644 "$SRC/sutura/repair_score_config.json" "$APP_DIR/repair_score_config.json"
install -m 0755 "$SRC/sutura/updater.py"         "$APP_DIR/updater.py"
install -m 0755 "$SRC/sutura/gui.py"             "$APP_DIR/gui.py"
install -m 0755 "$SRC/sutura/heatmap.py"         "$APP_DIR/heatmap.py"
install -m 0755 "$SRC/sutura/heatmap_render.py"  "$APP_DIR/heatmap_render.py"
install -m 0755 "$SRC/sutura/before_after_render.py" "$APP_DIR/before_after_render.py"
install -m 0755 "$SRC/sutura/viewer_common.py"       "$APP_DIR/viewer_common.py"
install -m 0755 "$SRC/sutura/viewer_data_render.py"  "$APP_DIR/viewer_data_render.py"
install -m 0755 "$SRC/sutura/diagnostics.py"         "$APP_DIR/diagnostics.py"
install -m 0755 "$SRC/sutura/manifest.py"            "$APP_DIR/manifest.py"
install -m 0755 "$SRC/sutura/self_repair.py"         "$APP_DIR/self_repair.py"
install -m 0755 "$SRC/sutura/self_check.py"          "$APP_DIR/self_check.py"
install -m 0755 "$SRC/sutura/open.sh"            "$APP_DIR/open.sh"
install -m 0644 "$SRC/LICENSE"                   "$APP_DIR/LICENSE"

echo "==> copying sutura_engine package"
rm -rf "$APP_DIR/sutura_engine" "$APP_DIR/sutura/sutura_engine"
cp -r "$SRC/sutura/sutura_engine" "$APP_DIR/sutura_engine"
mkdir -p "$APP_DIR/sutura"
cp -r "$SRC/sutura/sutura_engine" "$APP_DIR/sutura/sutura_engine"
# root bootstrap package: source checkouts need it for `import sutura_engine`
[ -f "$APP_DIR/sutura_engine/__init__.py" ] || \
    install -m 0644 "$SRC/sutura_engine/__init__.py" "$APP_DIR/sutura_engine/__init__.py"

# developer/security: when installing from a git checkout, install the
# pre-push secret scan hook (harmless no-op for end-user installs)
if [ -d "$SRC/.git/hooks" ] && [ -f "$SRC/scripts/pre-push-security-check.sh" ]; then
    install -m 0755 "$SRC/scripts/pre-push-security-check.sh" "$SRC/.git/hooks/pre-push"
fi

echo "==> CLI wrapper"
cat > "$BIN_DIR/sutura" <<EOF
#!/usr/bin/env bash
exec "\$HOME/.local/share/sutura/venv/bin/python" "\$HOME/.local/share/sutura/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

echo "==> Dolphin service menu"
sed "s|%HOME%|$HOME|g" "$SRC/share/sutura.desktop" > "$SERVICE_DIR/sutura.desktop"
chmod 0755 "$SERVICE_DIR/sutura.desktop"

echo "==> application entry"
APP_ENTRIES="$HOME/.local/share/applications"
mkdir -p "$APP_ENTRIES"
sed "s|%HOME%|$HOME|g" "$SRC/share/sutura-app.desktop" > "$APP_ENTRIES/sutura.desktop"
chmod 0644 "$APP_ENTRIES/sutura.desktop"
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APP_ENTRIES" >/dev/null 2>&1 || true
fi

echo "==> icons (hicolor)"
HICON="$HOME/.local/share/icons/hicolor"
if [ -d "$SRC/assets/icon" ]; then
    for s in 16 32 48 64 128 256; do
        install -d "$HICON/${s}x${s}/apps"
        install -m 0644 "$SRC/assets/icon/sutura-${s}.png" "$HICON/${s}x${s}/apps/sutura.png"
    done
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -f -t "$HICON" >/dev/null 2>&1 || true
    fi
else
    echo "    (icons skipped: assets/icon missing in source)"
fi

if command -v kbuildsycoca6 >/dev/null 2>&1; then
    kbuildsycoca6 >/dev/null 2>&1 || true
fi

# --- make sure ~/.local/bin is on PATH -------------------------------------
# Many desktops (some Fedora/Ubuntu setups, macOS) do not put
# ~/.local/bin on PATH, so a fresh `sutura` would be "command not found".
PATH_NOTE=""
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        PATH_LINE='export PATH="$HOME/.local/bin:$PATH"  # added by Sutura installer'
        added=""
        for rc in "$HOME/.bashrc" "$HOME/.zshrc"; do
            [ -f "$rc" ] || continue
            if ! grep -qF "# added by Sutura installer" "$rc"; then
                [ -n "$(tail -c1 "$rc")" ] && echo >> "$rc"
                printf '%s\n' "$PATH_LINE" >> "$rc"
            fi
            added="$added $rc"
        done
        if [ -z "$added" ]; then
            printf '%s\n' "$PATH_LINE" >> "$HOME/.profile"
            added=" $HOME/.profile"
        fi
        if command -v fish >/dev/null 2>&1; then
            fish -c 'fish_add_path -U $HOME/.local/bin' >/dev/null 2>&1 || true
        fi
        PATH_NOTE="Added ~/.local/bin to PATH in:$added - open a new terminal to use 'sutura'."
        ;;
esac

echo
echo "Installed."
echo "  CLI:       $BIN_DIR/sutura <file.stl|file.obj|file.3mf>"
echo "  GUI:       $APP_DIR/gui.py"
echo "  Dolphin:   right-click an STL/OBJ/3MF -> Repair with Sutura"

if [ -n "$PATH_NOTE" ]; then echo; echo "$PATH_NOTE"; fi
