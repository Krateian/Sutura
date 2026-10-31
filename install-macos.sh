#!/usr/bin/env bash
# Sutura macOS installer (Apple Silicon / Intel, conda).
#
# The Linux installer uses a pip-only flow. macOS keeps a single conda
# environment so the pinned PySide6 and the fTetWild extra install
# reproducibly. An existing conda is reused; if it is missing, Miniforge
# (~80 MB) is downloaded and installed in the same run. The repair engine is
# native; PyMeshLab is not installed at runtime.
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: this installer is macOS-only. On Linux use ./install.sh" >&2
    exit 1
fi

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
# shellcheck source=scripts/install_owner.sh
. "$REPO_DIR/scripts/install_owner.sh"
APP_DIR="$HOME/.local/share/sutura"
BIN_DIR="$HOME/.local/bin"
ENV_NAME="sutura-env"
PY_VERSION="3.11"

die() { echo "error: $*" >&2; exit 1; }

if [ "${SUTURA_DRY_RUN:-0}" = "1" ]; then
    echo "dry-run: would install Sutura under $APP_DIR and $BIN_DIR"
    echo "dry-run: would reuse conda or download Miniforge (~80 MB)"
    echo "dry-run: no download, conda env or file change performed"
    exit 0
fi

# 1) conda / Miniforge ------------------------------------------------------
# No Homebrew requirement and no "run this script again": if conda is absent,
# download the official Miniforge installer for this architecture, verify its
# published SHA-256 and install it under ~/miniforge3 in the same run.
CONDA_BIN=""
CONDA_BASE=""
if command -v conda >/dev/null 2>&1; then
    CONDA_BIN="$(command -v conda)"
    CONDA_BASE="$("$CONDA_BIN" info --base 2>/dev/null || true)"
fi
if [ -z "$CONDA_BASE" ] || [ ! -x "$CONDA_BASE/bin/conda" ]; then
    CONDA_BASE="$HOME/miniforge3"
    if [ ! -x "$CONDA_BASE/bin/conda" ]; then
        case "$(uname -m)" in
            arm64) MF_ARCH="arm64" ;;
            x86_64) MF_ARCH="x86_64" ;;
            *) die "unsupported macOS architecture: $(uname -m)" ;;
        esac
        MF_NAME="Miniforge3-MacOSX-${MF_ARCH}.sh"
        MF_URL="https://github.com/conda-forge/miniforge/releases/latest/download/$MF_NAME"
        echo "==> downloading Miniforge (~80 MB) into $CONDA_BASE"
        MF_TMP="$(mktemp -d "${TMPDIR:-/tmp}/sutura-miniforge.XXXXXX")"
        trap 'rm -rf "$MF_TMP"' EXIT
        curl -fL --retry 3 -o "$MF_TMP/$MF_NAME" "$MF_URL" || die "could not download Miniforge"
        curl -fL --retry 3 -o "$MF_TMP/$MF_NAME.sha256" "$MF_URL.sha256" || die "could not download the Miniforge checksum"
        (cd "$MF_TMP" && shasum -a 256 -c "$MF_NAME.sha256") || die "Miniforge SHA-256 verification failed"
        bash "$MF_TMP/$MF_NAME" -b -p "$CONDA_BASE" || die "Miniforge installation failed"
        rm -rf "$MF_TMP"; trap - EXIT
    fi
    CONDA_BIN="$CONDA_BASE/bin/conda"
fi
[ -x "$CONDA_BIN" ] || die "conda is not usable (expected $CONDA_BIN)"
if [ -f "$CONDA_BASE/etc/profile.d/conda.sh" ]; then
    . "$CONDA_BASE/etc/profile.d/conda.sh"
else
    eval "$("$CONDA_BIN" shell.bash hook)"
fi

# 4) create the environment (idempotent) ------------------------------------
ENV_PATH="$CONDA_BASE/envs/$ENV_NAME"
CONDA_ENV_CREATED=0
if ! "$CONDA_BIN" env list | awk '{print $1}' | grep -qx "$ENV_NAME"; then
    echo "==> creating conda env $ENV_NAME (python $PY_VERSION)"
    "$CONDA_BIN" create -y -n "$ENV_NAME" python="$PY_VERSION"
    CONDA_ENV_CREATED=1
else
    ENV_PY_CHECK="$ENV_PATH/bin/python"
    if ! "$ENV_PY_CHECK" -c 'import sys; sys.exit(0 if sys.version_info[:2] == (3, 11) else 1)' >/dev/null 2>&1; then
        echo "==> recreating $ENV_NAME (its Python is not $PY_VERSION)"
        "$CONDA_BIN" env remove -y -n "$ENV_NAME"
        "$CONDA_BIN" create -y -n "$ENV_NAME" python="$PY_VERSION"
        CONDA_ENV_CREATED=1
    else
        echo "==> conda env $ENV_NAME already exists"
        OWNER_FILE="$(sutura_owner_file "$APP_DIR")"
        if [ -f "$OWNER_FILE" ]; then
            _owner_created="$(sutura_owner_get "$OWNER_FILE" conda_env_created || true)"
            _owner_path="$(sutura_owner_get "$OWNER_FILE" conda_env_path || true)"
            if [ "$_owner_created" = "1" ] && [ "$_owner_path" = "$ENV_PATH" ]; then
                CONDA_ENV_CREATED=1
            fi
        fi
    fi
fi

# 5) install dependencies ---------------------------------------------------
echo "==> installing packages into $ENV_NAME"
conda run -n "$ENV_NAME" pip install PySide6-Essentials==6.11.2
if [ "${SUTURA_WITH_FTETWILD:-1}" = "1" ]; then
    echo "==> fTetWild fallback tier (pytetwild, ~2.3 MB; opt out with SUTURA_WITH_FTETWILD=0)"
    # Older releases installed pyvista/VTK for this tier; drop them if present.
    conda run -n "$ENV_NAME" pip uninstall -y pyvista vtk pyvista-validation >/dev/null 2>&1 || true
    if ! conda run -n "$ENV_NAME" pip install -r "$REPO_DIR/requirements-ftetwild.txt"; then
        echo "warning: fTetWild tier install failed; the rest of Sutura is fine." >&2
    fi
fi

# 5b) required: sutura_geom Rust extension (Graft #13, ~0.7 MB) ------------
# pip-only into the conda env (like Sutura's other optional extras); it never installs a
# conda-forge package, so this does not violate the conda-forge policy. The
# Darwin prebuilt wheel is universal2. ENV_PY is resolved here so the helper
# runs before the verify step. The extension is REQUIRED: a failure aborts the
# install with a named, actionable message from the helper.
ENV_PY="$CONDA_BASE/envs/$ENV_NAME/bin/python"
echo "==> required: sutura_geom Rust extension (Graft #13, ~0.7 MB)"
[ -f "$REPO_DIR/scripts/install_sutura_geom.sh" ] \
    || die "scripts/install_sutura_geom.sh is missing; sutura_geom is required"
bash "$REPO_DIR/scripts/install_sutura_geom.sh" "$ENV_PY" "$REPO_DIR"

# 6) verify ----------------------------------------------------------------
if ! "$ENV_PY" -c \
    "import numpy, sutura_geom; from PySide6 import QtWidgets; print('OK')"; then
    die "import check failed - dependencies not usable in $ENV_NAME"
fi
if ! "$ENV_PY" -c 'import sutura_geom' >/dev/null 2>&1; then
    die "sutura_geom import failed - the required extension is not usable in $ENV_NAME"
fi
echo "    sutura_geom: import OK (Graft #13 + Exact Refine available)"
# Qt plugin path must be pinned to PySide6's OWN Qt6 plugins. The conda env
# also carries a Qt5 stack (qt-main) whose plugins live under $ENV/plugins;
# when a GUI app is launched from Finder/Spotlight (minimal environment, no
# shell rc sourced) the Qt6 runtime can resolve the plugin search path to the
# Qt5 directory, fail to load its cocoa plugin ("Could not find the Qt platform
# plugin cocoa") and abort. Terminal launches work only because PySide6 happens
# to find its own path first. Pin it so the .app and sutura-gui work everywhere.
QT_PLUGIN_PATH="$("$ENV_PY" -c \
    'from PySide6.QtCore import QLibraryInfo; \
     print(QLibraryInfo.path(QLibraryInfo.LibraryPath.PluginsPath))')"

# 7) copy the application files --------------------------------------------
mkdir -p "$APP_DIR" "$BIN_DIR"
for f in repair.py ftetwild_bridge.py indirect_bridge.py classification.py confidence.py defects.py topology.py mesh_classifier.py mesh_classifier_v2.py autorefine.py local_exact.py triage.py engines.py ftetwild_manager.py methods.py object_analysis.py templates.py closing.py proxy_repair.py repeat_repair.py repeat_picker_render.py mirror_repair.py wall_thickness.py shell_wrap.py history.py updater.py gui.py heatmap.py heatmap_render.py before_after_render.py viewer_common.py viewer_data_render.py repair_score.py diagnostics.py manifest.py self_repair.py self_check.py repair_score_config.json __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/$f"
done
# the importable package layout (for 'from sutura import ...' and __init__)
mkdir -p "$APP_DIR/sutura"
for f in repair.py ftetwild_bridge.py indirect_bridge.py classification.py confidence.py defects.py topology.py mesh_classifier.py mesh_classifier_v2.py autorefine.py local_exact.py triage.py engines.py ftetwild_manager.py methods.py object_analysis.py templates.py closing.py proxy_repair.py repeat_repair.py repeat_picker_render.py mirror_repair.py wall_thickness.py shell_wrap.py history.py updater.py gui.py heatmap.py heatmap_render.py before_after_render.py viewer_common.py viewer_data_render.py repair_score.py diagnostics.py manifest.py self_repair.py self_check.py repair_score_config.json __init__.py; do
    install -m 0644 "$REPO_DIR/sutura/$f" "$APP_DIR/sutura/$f"
done
install -m 0644 "$REPO_DIR/LICENSE" "$APP_DIR/LICENSE"
rm -rf "$APP_DIR/sutura_engine" "$APP_DIR/sutura/sutura_engine"
cp -r "$REPO_DIR/sutura/sutura_engine" "$APP_DIR/sutura_engine"
cp -r "$REPO_DIR/sutura/sutura_engine" "$APP_DIR/sutura/sutura_engine"
# root bootstrap package: source checkouts need it for `import sutura_engine`
[ -f "$APP_DIR/sutura_engine/__init__.py" ] || \
    install -m 0644 "$REPO_DIR/sutura_engine/__init__.py" "$APP_DIR/sutura_engine/__init__.py"

# developer/security: when installing from a git checkout, install the
# pre-push secret scan hook (harmless no-op for end-user installs)
if [ -d "$REPO_DIR/.git/hooks" ] && [ -f "$REPO_DIR/scripts/pre-push-security-check.sh" ]; then
    install -m 0755 "$REPO_DIR/scripts/pre-push-security-check.sh" "$REPO_DIR/.git/hooks/pre-push"
fi

# 8) CLI wrapper ------------------------------------------------------------
cat > "$BIN_DIR/sutura" <<EOF
#!/bin/bash
exec "$CONDA_BIN" run -n "$ENV_NAME" python "$APP_DIR/repair.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura"

# 9) GUI launcher -----------------------------------------------------------
cat > "$BIN_DIR/sutura-gui" <<EOF
#!/bin/bash
export QT_PLUGIN_PATH="$QT_PLUGIN_PATH"
exec "$CONDA_BIN" run -n "$ENV_NAME" python "$APP_DIR/gui.py" "\$@"
EOF
chmod 0755 "$BIN_DIR/sutura-gui"

# 9b) Native macOS app (Spotlight: Cmd+Space -> "Sutura") -------------------
# Minimal .app wrapper around the sutura-gui launcher: a real .app bundle in
# ~/Applications so Spotlight finds it by name and launches the GUI without a
# terminal. Idempotent (re-running recreates it cleanly).
APPLICATIONS_DIR="$HOME/Applications"
APP="$APPLICATIONS_DIR/Sutura.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cat > "$APP/Contents/MacOS/Sutura" <<EOF
#!/bin/bash
export QT_PLUGIN_PATH="$QT_PLUGIN_PATH"
exec "$BIN_DIR/sutura-gui" "\$@"
EOF
chmod 0755 "$APP/Contents/MacOS/Sutura"
cp "$REPO_DIR/assets/icon/Sutura.icns" "$APP/Contents/Resources/AppIcon.icns"
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>Sutura</string>
    <key>CFBundleIdentifier</key><string>com.krateian.sutura</string>
    <key>CFBundleName</key><string>Sutura</string>
    <key>CFBundleDisplayName</key><string>Sutura</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$REPO_DIR/sutura/repair.py" | head -1)</string>
</dict>
</plist>
EOF
# refresh the Launch Services + Spotlight index so the app is findable
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP" 2>/dev/null || true
mdimport "$APP" 2>/dev/null || true

# 9c) Finder Quick Action (Dolphin ServiceMenu eşdeğeri) --------------------
# A Finder right-click integration equivalent to the KDE Dolphin ServiceMenu:
# an Automator .workflow installed into ~/Library/Services that appears under
# Finder's "Quick Actions" and uses a bundled sutura-cli (.dmg app) or the
# installed ~/.local/bin/sutura (conda install). The
# helper script lives in $APP_DIR (like open.sh) so it can be updated without
# re-installing the workflow; the workflow only points at it. Idempotent:
# re-running removes and re-copies the workflow and refreshes LaunchServices.
install -m 0755 "$REPO_DIR/share/macos-quick-action.sh" "$APP_DIR/macos-quick-action.sh"
SERVICES_DIR="$HOME/Library/Services"
mkdir -p "$SERVICES_DIR"
rm -rf "$SERVICES_DIR/Sutura Quick Action.workflow"
cp -R "$REPO_DIR/share/Sutura Quick Action.workflow" "$SERVICES_DIR/"
# Keep the installed workflow version in sync with the app (the source plist
# is a template; a new release must not leave a stale Quick Action version).
WF_PLIST="$SERVICES_DIR/Sutura Quick Action.workflow/Contents/Info.plist"
WF_VER="$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$REPO_DIR/sutura/repair.py" | head -1)"
if [ -n "$WF_VER" ] && [ -f "$WF_PLIST" ]; then
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $WF_VER" "$WF_PLIST" 2>/dev/null || true
fi
# Automator workflow bundles are NOT applications: lsregister refuses them
# (kLSNotAnApplicationErr, -10811) and registers nothing. The Services menu
# is owned by the pbs agent; pbs -update rescans changed Services and applies
# them immediately to all running apps. Idempotent (re-running just rescans).
/System/Library/CoreServices/pbs -update 2>/dev/null || true

# 10) summary ---------------------------------------------------------------
# --- make sure ~/.local/bin is on PATH -------------------------------------
# Write the marker to the file the user's shell actually reads. The current
# installer process cannot change the parent shell's PATH, so also print the
# absolute working path below.
PATH_NOTE=""
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        # shellcheck source=scripts/install_path.sh
        . "$REPO_DIR/scripts/install_path.sh"
        added="$(add_sutura_path)"
        if command -v fish >/dev/null 2>&1; then
            fish -c 'fish_add_path -U $HOME/.local/bin' >/dev/null 2>&1 || true
        fi
        if [ -n "$added" ]; then
            PATH_NOTE="Added ~/.local/bin to PATH in:$added"
        fi
        ;;
esac

if ! write_sutura_owner "$APP_DIR" "install-macos.sh" "$CONDA_ENV_CREATED" \
        "$ENV_NAME" "$ENV_PATH" "$CONDA_BASE"; then
    echo "warning: could not write the install ownership marker under $APP_DIR/data" >&2
fi

echo
echo "Installed."
echo "  conda env : $ENV_NAME (python $PY_VERSION)"
echo "  python    : $ENV_PY"
echo "  files     : $APP_DIR/"
echo "  CLI       : $BIN_DIR/sutura <file.stl|file.3mf>   (works now)"
echo "  GUI       : $BIN_DIR/sutura-gui"
echo "  macOS app : $APP  (Spotlight: press Cmd+Space and type Sutura)"
echo
echo "Usage:"
echo "  sutura model.stl --human"
echo "  sutura-gui"

if [ -n "$PATH_NOTE" ]; then
    echo
    echo "$PATH_NOTE"
    echo "Current shell: $BIN_DIR/sutura"
else
    echo
    echo "Current shell: $BIN_DIR/sutura"
fi
