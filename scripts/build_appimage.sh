#!/usr/bin/env bash
# Build the Sutura AppImage.
#
# Bundles two relocatable python-build-standalone runtimes:
#   * Python 3.14 -> usr/lib/sutura/venv     (pymeshlab + PySide6: stage 1 + GUI)
#   * Python 3.11 -> usr/lib/sutura/venv311  (manifold3d: stage 2)
# plus the application modules, an AppRun GUI/CLI dispatch entry point, and a
# .desktop + icon, then packages the whole AppDir with appimagetool.
#
# Run from anywhere; the repository root is resolved relative to this script.
# The build happens in a temporary directory that is removed on exit; the
# only artifact left behind is the AppImage in <repo>/dist/.
#
# Overridable environment:
#   PBS_TAG        python-build-standalone release tag (default: 20260814)
#   OUT_DIR        output directory (default: <repo>/dist)
#   APPIMAGE_NAME  output file name (default: Sutura-x86_64.AppImage)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

PBS_TAG="${PBS_TAG:-20260814}"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/dist}"
APPIMAGE_NAME="${APPIMAGE_NAME:-Sutura-x86_64.AppImage}"

PBS_BASE="https://github.com/indygreg/python-build-standalone/releases/download/$PBS_TAG"
PBS_314="$PBS_BASE/cpython-3.14.7+$PBS_TAG-x86_64-unknown-linux-gnu-install_only.tar.gz"
PBS_311="$PBS_BASE/cpython-3.11.16+$PBS_TAG-x86_64-unknown-linux-gnu-install_only.tar.gz"
APPIMAGETOOL_URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"

die() { echo "error: $*" >&2; exit 1; }

[ "$(uname -m)" = "x86_64" ] || die "only x86_64 is supported (got $(uname -m))"
for t in curl tar; do command -v "$t" >/dev/null || die "missing tool: $t"; done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sutura-appimage.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
DL="$WORK/downloads"
APP_DIR="$WORK/AppDir"
LIB="$APP_DIR/usr/lib/sutura"
mkdir -p "$DL" "$LIB" "$APP_DIR/usr/bin" "$OUT_DIR"

echo "==> downloading python-build-standalone ($PBS_TAG)"
curl -fL --retry 3 -o "$DL/pbs314.tar.gz" "$PBS_314"
curl -fL --retry 3 -o "$DL/pbs311.tar.gz" "$PBS_311"

echo "==> installing Python 3.14 runtime (stage 1 + GUI)"
tar xzf "$DL/pbs314.tar.gz" -C "$WORK"
mv "$WORK/python" "$LIB/venv"

echo "==> installing Python 3.11 runtime (stage 2)"
tar xzf "$DL/pbs311.tar.gz" -C "$WORK"
mv "$WORK/python" "$LIB/venv311"

echo "==> pip install: stage 1 (pymeshlab + PySide6)"
"$LIB/venv/bin/python" -m pip install --quiet --upgrade pip
"$LIB/venv/bin/python" -m pip install --quiet \
    -r "$REPO_ROOT/requirements.txt" -r "$REPO_ROOT/requirements-gui.txt"

echo "==> pip install: stage 2 (manifold3d)"
"$LIB/venv311/bin/python" -m pip install --quiet --upgrade pip
"$LIB/venv311/bin/python" -m pip install --quiet -r "$REPO_ROOT/requirements-311.txt"

echo "==> copying application modules"
for f in repair.py gui.py classification.py defects.py mesh_classifier.py \
         manifold_bridge.py updater.py heatmap.py heatmap_render.py open.sh __init__.py; do
    install -m 0755 "$REPO_ROOT/sutura/$f" "$LIB/"
done
install -m 0644 "$REPO_ROOT/requirements.txt" \
    "$REPO_ROOT/requirements-gui.txt" "$REPO_ROOT/requirements-311.txt" "$LIB/"

echo "==> writing AppRun + CLI wrapper"
cat > "$APP_DIR/AppRun" <<'EOF'
#!/usr/bin/env bash
# Sutura AppImage entry point. The AppImage runtime sets ARGV0 to the
# AppImage file itself, so GUI/CLI selection is done on the arguments:
#   * no args          -> GUI
#   * --gui            -> GUI (explicit)
#   * --cli            -> CLI (explicit)
#   * any other args   -> CLI (repair.py reports file-not-found itself;
#                         AppRun must not guess whether a path exists)
set -euo pipefail

SUTURA_LIB="$APPDIR/usr/lib/sutura"
export SUTURA_DIR="$SUTURA_LIB"          # repair.py:29 reads this
export SUTURA="$APPDIR/usr/bin/sutura"   # gui.py:140 reads this

if [ "$#" -eq 0 ]; then
    exec "$SUTURA_LIB/venv/bin/python" "$SUTURA_LIB/gui.py"
fi

case "$1" in
    --gui) shift; exec "$SUTURA_LIB/venv/bin/python" "$SUTURA_LIB/gui.py" "$@";;
    *)     exec "$SUTURA_LIB/venv/bin/python" "$SUTURA_LIB/repair.py" "$@";;
esac
EOF
cat > "$APP_DIR/usr/bin/sutura" <<'EOF'
#!/usr/bin/env bash
# Sutura CLI wrapper inside the AppImage. Used by the GUI's shell-out
# ($SUTURA) and by scripts; bypasses AppRun's GUI/CLI dispatch.
set -euo pipefail

SUTURA_LIB="$(cd "$(dirname "$(readlink -f "$0")")/../lib/sutura" && pwd)"
export SUTURA_DIR="$SUTURA_LIB"
exec "$SUTURA_LIB/venv/bin/python" "$SUTURA_LIB/repair.py" "$@"
EOF
chmod 0755 "$APP_DIR/AppRun" "$APP_DIR/usr/bin/sutura"

echo "==> desktop entry + icon"
cat > "$APP_DIR/sutura.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=Sutura
Comment=Repair STL/3MF meshes
Exec=sutura
Icon=sutura
Terminal=false
Categories=Graphics;3DGraphics;
StartupNotify=true
EOF
chmod 0755 "$APP_DIR/sutura.desktop"
cp "$REPO_ROOT/assets/icon/sutura-256.png" "$APP_DIR/sutura.png"

echo "==> downloading appimagetool"
curl -fL --retry 3 -o "$DL/appimagetool.AppImage" "$APPIMAGETOOL_URL"
chmod +x "$DL/appimagetool.AppImage"

echo "==> building AppImage"
mkdir -p "$WORK/build"
(
    cd "$WORK/build"
    "$DL/appimagetool.AppImage" --appimage-extract-and-run \
        "$APP_DIR" "$OUT_DIR/$APPIMAGE_NAME"
)

echo "==> done: $OUT_DIR/$APPIMAGE_NAME"