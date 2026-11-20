#!/usr/bin/env bash
# Package the Rust binaries for distribution (macOS .app/.dmg, Linux tar.gz).
#
# This is the Python-free counterpart of build_appimage.sh / build-macos.yml:
# no Python runtime, no PyInstaller, no pytetwild wheel. It packages exactly:
#
#   sutura         the CLI (single static-ish binary)
#   sutura-gui     the egui GUI
#   ftetwild       optional, built by third_party/ftetwild/build.sh
#   LICENSE, ATTRIBUTION.md, third-party licences
#   desktop entries, the macOS Quick Action, the icon
#
# It creates NO release, tag or version -- only files under dist/.
#
# Usage:
#   scripts/package_rust.sh --platform macos-arm64
#   scripts/package_rust.sh --platform macos-x86_64
#   scripts/package_rust.sh --platform linux-x86_64
#   scripts/package_rust.sh --platform linux-aarch64
#
# Every platform the Python distribution shipped has a target here. A missing
# one is not just a packaging gap: `install::asset_name` returns None without it,
# which silently disables `--check-update` and `--repair-install` on that
# machine.
#
# Overridable environment:
#   OUT_DIR        output directory (default: <repo>/dist)
#   VERSION        label for the file names (default: read from repair.py)
#   FTETWILD_BIN   prebuilt fTetWild binary to bundle (skipped when unset and
#                  third_party/ftetwild/bin/ftetwild is absent)
#   SKIP_FTETWILD  set to 1 to package without fTetWild
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="${OUT_DIR:-$REPO_ROOT/dist}"

PLATFORM=""
while [ $# -gt 0 ]; do
    case "$1" in
        --platform) PLATFORM="${2:?--platform needs a value}"; shift 2 ;;
        --platform=*) PLATFORM="${1#*=}"; shift ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "error: unknown argument: $1" >&2; exit 2 ;;
    esac
done

die() { echo "error: $*" >&2; exit 1; }

[ -n "$PLATFORM" ] || die "--platform is required (macos-arm64, macos-x86_64, linux-x86_64, linux-aarch64)"
case "$PLATFORM" in
    macos-arm64|macos-x86_64|linux-x86_64|linux-aarch64) ;;
    *) die "unknown platform: $PLATFORM (expected macos-arm64, macos-x86_64, linux-x86_64 or linux-aarch64)" ;;
esac

# The arch label used in the asset name. It is the ONLY thing that separates a
# macOS arm64 package from an Intel one (and the Linux pair likewise), so it must
# match `install::asset_name` exactly or the updater will never find the file.
case "$PLATFORM" in
    macos-arm64|linux-arm64) ARCH="arm64" ;;
    macos-x86_64|linux-x86_64) ARCH="x86_64" ;;
    linux-aarch64) ARCH="aarch64" ;;
esac

# The product version still comes from repair.py while it is the single source
# (see rust/sutura-cli/build.rs); used only in file names, never asserted.
VERSION="${VERSION:-$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$REPO_ROOT/sutura/repair.py" 2>/dev/null | head -1)}"
VERSION="${VERSION:-0.0.0}"

TARGET_DIR="$REPO_ROOT/rust/target/release"
CLI="$TARGET_DIR/sutura"
GUI="$TARGET_DIR/sutura-gui"
[ -x "$CLI" ] || die "missing $CLI -- run: cargo build --release -p sutura-cli"
[ -x "$GUI" ] || die "missing $GUI -- run: cargo build --release -p sutura-gui"

# The fTetWild binary the Python distribution also used, from the SAME pinned
# source: third_party/ftetwild/build.sh pins upstream 8118f810478e0e65a7bf2d8cecdc5e203a876e97
# and applies the determinism patches. Reusing that script is what keeps the
# binary comparable to the one Python shipped (a different fTetWild build
# changed the result on thingi10k_100827), so it is never hand-compiled here.
FTW=""
if [ "${SKIP_FTETWILD:-0}" != "1" ]; then
    if [ -n "${FTETWILD_BIN:-}" ]; then
        FTW="$FTETWILD_BIN"
    elif [ -x "$REPO_ROOT/third_party/ftetwild/bin/ftetwild" ]; then
        FTW="$REPO_ROOT/third_party/ftetwild/bin/ftetwild"
    fi
    if [ -n "$FTW" ] && [ ! -x "$FTW" ]; then
        die "FTETWILD_BIN is set but not executable: $FTW"
    fi
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sutura-pack.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT_DIR"

# Shared payload: the files every platform ships, so the two packages cannot
# drift in what they attribute or document.
copy_payload() { # $1 = destination root
    local root="$1"
    install -m 0644 "$REPO_ROOT/LICENSE" "$root/LICENSE"
    if [ -f "$REPO_ROOT/docs/ATTRIBUTION.md" ]; then
        install -m 0644 "$REPO_ROOT/docs/ATTRIBUTION.md" "$root/ATTRIBUTION.md"
    fi
    # The migration note must travel with the package: a user upgrading from the
    # Python build needs to know where the old install went and how to get back,
    # and they may not have the repository checked out at that moment.
    if [ -f "$REPO_ROOT/MIGRATE.md" ]; then
        install -m 0644 "$REPO_ROOT/MIGRATE.md" "$root/MIGRATE.md"
    fi
    # P10 launchers: Rust CLI/GUI by default, SUTURA_BACKEND=python legacy.
    mkdir -p "$root/share/launchers"
    install -m 0755 "$REPO_ROOT/bin/sutura" "$root/share/launchers/sutura"
    install -m 0755 "$REPO_ROOT/bin/sutura-gui" "$root/share/launchers/sutura-gui"
    # Vendored third-party licences. These are the obligations ATTRIBUTION.md
    # points at, so they ship next to it rather than only in the repo.
    local lic
    for lic in \
        "$REPO_ROOT/rust/sutura-geom/third_party/geogram_delaunay_psm/LICENSE" \
        "$REPO_ROOT/rust/sutura-geom/third_party/meshoptimizer/LICENSE" \
        "$REPO_ROOT/third_party/ftetwild/NOTICE"; do
        [ -f "$lic" ] || continue
        mkdir -p "$root/third_party"
        install -m 0644 "$lic" "$root/third_party/$(basename "$(dirname "$lic")")-$(basename "$lic")"
    done
}

package_macos() {
    local app="$WORK/Sutura.app"
    local macos="$app/Contents/MacOS"
    local res="$app/Contents/Resources"
    mkdir -p "$macos" "$res"

    install -m 0755 "$GUI" "$macos/sutura-gui"
    install -m 0755 "$CLI" "$macos/sutura"
    if [ -n "$FTW" ]; then
        install -m 0755 "$FTW" "$macos/ftetwild"
        echo "==> bundled fTetWild: $(du -h "$FTW" | cut -f1)"
    else
        echo "==> fTetWild not bundled (tier will report unavailable)"
    fi

    # `sutura-gui` is the bundle's main executable; the CLI sits beside it for
    # the Quick Action and for terminal use.
    cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Sutura</string>
  <key>CFBundleDisplayName</key><string>Sutura</string>
  <key>CFBundleIdentifier</key><string>app.sutura.rust</string>
  <key>CFBundleExecutable</key><string>sutura-gui</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleIconFile</key><string>Sutura.icns</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

    if [ -f "$REPO_ROOT/assets/icon/Sutura.icns" ]; then
        install -m 0644 "$REPO_ROOT/assets/icon/Sutura.icns" "$res/Sutura.icns"
    fi
    copy_payload "$res"

    # The Quick Action helper is installed by the user (it lives in ~/Library/
    # Services), so ship it inside the bundle as well.
    if [ -f "$REPO_ROOT/share/macos-quick-action.sh" ]; then
        install -m 0755 "$REPO_ROOT/share/macos-quick-action.sh" "$res/macos-quick-action.sh"
    fi
    if [ -d "$REPO_ROOT/share/Sutura Quick Action.workflow" ]; then
        cp -R "$REPO_ROOT/share/Sutura Quick Action.workflow" "$res/"
    fi

    # Ad-hoc signature, INSIDE OUT: every executable in the bundle first, then
    # the bundle itself. Deliberately **no** `--deep`: Apple deprecated it, and
    # on the macOS 15 Intel runner it fails with
    #
    #   Contents/MacOS/sutura-gui: code object is not signed at all
    #   In subcomponent: Contents/MacOS/ftetwild
    #
    # which is `--deep` refusing to sign an outer item while a nested
    # executable is unsigned. Signing each binary explicitly and then the
    # bundle seals the same way without depending on traversal order.
    # No notarization: that needs an Apple Developer ID, which this project does
    # not have (the Python .dmg was unsigned for the same reason).
    if command -v codesign >/dev/null 2>&1; then
        local signed=0
        for bin in "$macos"/*; do
            [ -f "$bin" ] || continue
            [ -x "$bin" ] || continue
            codesign --force --sign - "$bin" >/dev/null || {
                echo "error: codesign failed for $bin" >&2; return 1
            }
            signed=$((signed + 1))
        done
        codesign --force --sign - "$app" >/dev/null || {
            echo "error: codesign failed for $app" >&2; return 1
        }
        echo "==> ad-hoc signed ($signed executable(s) + bundle)"
    else
        echo "==> codesign unavailable; leaving the bundle unsigned"
    fi

    if command -v hdiutil >/dev/null 2>&1; then
        local dmg="$OUT_DIR/Sutura-rust-v${VERSION}-${ARCH}.dmg"
        rm -f "$dmg"
        hdiutil create -volname "Sutura" -srcfolder "$app" \
            -ov -format UDZO "$dmg" >/dev/null
        echo "==> $dmg ($(du -h "$dmg" | cut -f1))"
    else
        # No hdiutil (e.g. Linux CI): keep the .app tree for inspection.
        local out="$OUT_DIR/Sutura-rust-v${VERSION}-${ARCH}.app.tar.gz"
        tar -C "$WORK" -czf "$out" "Sutura.app"
        echo "==> $out ($(du -h "$out" | cut -f1))"
    fi
}

package_linux() {
    local root="$WORK/Sutura"
    mkdir -p "$root/bin" "$root/share/applications" "$root/share/icons/hicolor/256x256/apps"
    install -m 0755 "$CLI" "$root/bin/sutura"
    install -m 0755 "$GUI" "$root/bin/sutura-gui"
    if [ -n "$FTW" ]; then
        install -m 0755 "$FTW" "$root/bin/ftetwild"
        echo "==> bundled fTetWild: $(du -h "$FTW" | cut -f1)"
    else
        echo "==> fTetWild not bundled (tier will report unavailable)"
    fi
    copy_payload "$root"

    # Same entries the Python installer wrote (share/sutura.desktop and
    # share/sutura-app.desktop), with the venv python replaced by the binaries.
    sed 's#%HOME%/.local/share/sutura/open.sh#sutura#' \
        "$REPO_ROOT/share/sutura.desktop" > "$root/share/applications/sutura.desktop"
    sed -e 's#%HOME%/.local/bin/sutura-gui#sutura-gui#' \
        "$REPO_ROOT/share/sutura-app.desktop" > "$root/share/applications/sutura-app.desktop"
    if [ -f "$REPO_ROOT/assets/icon/sutura-256.png" ]; then
        install -m 0644 "$REPO_ROOT/assets/icon/sutura-256.png" \
            "$root/share/icons/hicolor/256x256/apps/sutura.png"
    fi

    local out="$OUT_DIR/Sutura-rust-v${VERSION}-${ARCH}.tar.gz"
    rm -f "$out"
    tar -C "$WORK" -czf "$out" "Sutura"
    echo "==> $out ($(du -h "$out" | cut -f1))"
}

echo "==> packaging $PLATFORM (version $VERSION, arch $ARCH)"
case "$PLATFORM" in
    macos-arm64|macos-x86_64) package_macos ;;
    linux-x86_64|linux-aarch64) package_linux ;;
esac
echo "==> done; artifacts in $OUT_DIR"
