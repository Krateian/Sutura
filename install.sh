#!/usr/bin/env bash
# Sutura installer.
#
# Installs the two virtualenvs, the CLI wrapper, the Dolphin service menu
# and the hicolor icons under the user's home.
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

print_python311_instructions() {
    echo "python3.11 not found."
    echo "manifold3d (stage 2) ships wheels only up to Python 3.13."
    echo "On Arch:                    python 3.11 is not in the official repos; unset"
    echo "                            SUTURA_NO_PYTHON_DOWNLOAD to let install.sh fetch"
    echo "                            a standalone build, or provide python3.11 yourself"
    echo "                            (e.g. uv python install 3.11)."
    echo "On Debian/Ubuntu (22.04+):  sudo apt install python3.11 python3.11-venv"
    echo "On Fedora:                  sudo dnf install python3.11"
    echo "Or install the venv311 manually and run install.sh again."
}

# --- find a python3.11 for the manifold3d venv ----------------------------
VENV311_PY="$(command -v python3.11 || true)"
if [ -z "$VENV311_PY" ]; then
    if "$MAIN_PY" -c 'import sys; sys.exit(0 if sys.version_info[:2] == (3, 11) else 1)' 2>/dev/null; then
        VENV311_PY="$MAIN_PY"
    elif [ -x "$APP_DIR/python311/bin/python3.11" ]; then
        # Reuse existing standalone Python 3.11 from a previous install run
        VENV311_PY="$APP_DIR/python311/bin/python3.11"
    else
        if [ "${SUTURA_NO_PYTHON_DOWNLOAD:-0}" = "1" ]; then
            print_python311_instructions
            exit 1
        fi

        for tool in curl tar sha256sum; do
            if ! command -v "$tool" >/dev/null 2>&1; then
                echo "missing tool: $tool"
                print_python311_instructions
                exit 1
            fi
        done

        arch="$(uname -m)"
        case "$arch" in
            x86_64|aarch64) pbs_arch="$arch" ;;
            *)
                echo "automatic python-build-standalone download not supported on $arch"
                print_python311_instructions
                exit 1
                ;;
        esac

        # python-build-standalone release for fallback Python 3.11 runtime.
        # NOTE: PBS_TAG and the Python version must be bumped together with
        # scripts/build_appimage.sh.
        PBS_TAG="20260814"
        PBS_VERSION="3.11.16"
        PBS_BASE="https://github.com/indygreg/python-build-standalone/releases/download/$PBS_TAG"
        PBS_TARBALL="cpython-${PBS_VERSION}+${PBS_TAG}-${pbs_arch}-unknown-linux-gnu-install_only.tar.gz"

        echo "==> downloading standalone Python 3.11 (~30 MB download, ~100 MB installed)"
        dl_dir="$(mktemp -d "${TMPDIR:-/tmp}/sutura-py311.XXXXXX")"
        curl -fL --retry 3 -o "$dl_dir/$PBS_TARBALL" "$PBS_BASE/$PBS_TARBALL"
        curl -fL --retry 3 -o "$dl_dir/SHA256SUMS" "$PBS_BASE/SHA256SUMS"

        # Verify SHA-256 against the SHA256SUMS the project publishes on the same
        # release (FAZ14): the tarball must verify OK, else abort and delete download.
        # See build_appimage.sh for details on --ignore-missing and || true under set -e.
        echo "==> verifying python-build-standalone SHA-256"
        pbs_ok=$(cd "$dl_dir" && sha256sum -c --ignore-missing SHA256SUMS 2>/dev/null | grep -c ': OK') || true
        if [ "$pbs_ok" -ne 1 ]; then
            rm -rf "$dl_dir"
            die "SHA-256 verification failed for python-build-standalone (got $pbs_ok/1 OK)"
        fi

        echo "==> extracting Python 3.11 runtime to $APP_DIR/python311"
        tmp_extract="$(mktemp -d "${TMPDIR:-/tmp}/sutura-extract.XXXXXX")"
        tar xzf "$dl_dir/$PBS_TARBALL" -C "$tmp_extract"
        mkdir -p "$APP_DIR"
        rm -rf "$APP_DIR/python311"
        mv "$tmp_extract/python" "$APP_DIR/python311"
        rm -rf "$tmp_extract" "$dl_dir"

        [ -x "$APP_DIR/python311/bin/python3.11" ] || die "extracted Python 3.11 binary not found at $APP_DIR/python311/bin/python3.11"
        VENV311_PY="$APP_DIR/python311/bin/python3.11"
    fi
fi
command -v "$MAIN_PY" >/dev/null || die "python3 not found"

mkdir -p "$APP_DIR" "$BIN_DIR" "$SERVICE_DIR"

echo "==> virtualenv (stage 1: PyMeshLab + GUI) [$MAIN_PY]"
[ -d "$APP_DIR/venv" ] || "$MAIN_PY" -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements.txt"
"$APP_DIR/venv/bin/pip" install --quiet -r "$SRC/requirements-gui.txt"

echo "==> virtualenv (stage 2: manifold3d) [$VENV311_PY]"
[ -d "$APP_DIR/venv311" ] || "$VENV311_PY" -m venv "$APP_DIR/venv311"
"$APP_DIR/venv311/bin/pip" install --quiet --upgrade pip
"$APP_DIR/venv311/bin/pip" install --quiet -r "$SRC/requirements-311.txt"
if [ "${SUTURA_WITH_FTETWILD:-0}" = "1" ]; then
    echo "==> optional: fTetWild fallback tier (pytetwild + pyvista/VTK, ~1.1 GB installed)"
    "$APP_DIR/venv311/bin/pip" install --quiet -r "$SRC/requirements-ftetwild.txt"
fi

echo "==> copying application files"
# __init__.py is deliberately not copied: the installed layout is flat (no
# package import), so repair.py/gui.py resolve their flat imports from APP_DIR
# itself. The AppImage/macOS package layouts copy it separately.
install -m 0755 "$SRC/sutura/repair.py"          "$APP_DIR/repair.py"
install -m 0755 "$SRC/sutura/manifold_bridge.py" "$APP_DIR/manifold_bridge.py"
install -m 0755 "$SRC/sutura/ftetwild_bridge.py" "$APP_DIR/ftetwild_bridge.py"
install -m 0755 "$SRC/sutura/indirect_bridge.py"  "$APP_DIR/indirect_bridge.py"
install -m 0755 "$SRC/sutura/classification.py"  "$APP_DIR/classification.py"
install -m 0755 "$SRC/sutura/confidence.py"      "$APP_DIR/confidence.py"
install -m 0755 "$SRC/sutura/defects.py"         "$APP_DIR/defects.py"
install -m 0755 "$SRC/sutura/mesh_classifier.py" "$APP_DIR/mesh_classifier.py"
install -m 0755 "$SRC/sutura/mesh_classifier_v2.py" "$APP_DIR/mesh_classifier_v2.py"
install -m 0755 "$SRC/sutura/autorefine.py"       "$APP_DIR/autorefine.py"
install -m 0755 "$SRC/sutura/triage.py"           "$APP_DIR/triage.py"
install -m 0755 "$SRC/sutura/engines.py"          "$APP_DIR/engines.py"
install -m 0755 "$SRC/sutura/ftetwild_manager.py" "$APP_DIR/ftetwild_manager.py"
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
install -m 0755 "$SRC/sutura/open.sh"            "$APP_DIR/open.sh"
install -m 0644 "$SRC/LICENSE"                   "$APP_DIR/LICENSE"

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

echo
echo "Installed."
echo "  CLI:       $BIN_DIR/sutura <file.stl|file.obj|file.3mf>"
echo "  GUI:       $APP_DIR/gui.py"
echo "  Dolphin:   right-click an STL/OBJ/3MF -> Repair with Sutura"