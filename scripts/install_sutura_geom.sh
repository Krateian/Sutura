#!/usr/bin/env bash
# Install the sutura_geom Rust extension (Graft #13, Exact Refine #6, the
# indirect-predicates tier) into one Python environment.
#
#   scripts/install_sutura_geom.sh <python> [<repo-src-dir>]
#
# The extension is REQUIRED. Order of attempts:
#   1. prebuilt abi3 wheel from the GitHub release matching the Sutura version
#      (falls back to the latest release), SHA-256 checked against the
#      sutura_geom-SHA256SUMS asset published by build-sutura-geom.yml
#      (~0.7 MB download);
#   2. build from rust/sutura-geom with pip + maturin when cargo is on PATH;
#   3. otherwise fail with a named, actionable message.
#
# Environment:
#   SUTURA_GEOM_FROM_SOURCE=1  skip the wheel download, build from source
#   SUTURA_GEOM_WHEEL=<path>   install this local wheel file
#   SUTURA_RELEASE_BASE=<url>  override https://github.com/Krateian/Sutura/releases
#
# SUTURA_NO_GEOM is no longer supported: the extension is mandatory, so setting
# it is an error. SUTURA_GEOM_REQUIRED is accepted for compatibility; a failure
# is always fatal.
set -euo pipefail

PY="${1:?usage: install_sutura_geom.sh <python> [<repo-src-dir>]}"
SRC="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RELEASE_BASE="${SUTURA_RELEASE_BASE:-https://github.com/Krateian/Sutura/releases}"
SUMS_NAME="sutura_geom-SHA256SUMS"

# Toolchain bootstrap home. Scoped to the Sutura install tree (the installers
# export SUTURA_TOOLS_DIR=$APP_DIR/tools) so a Rust toolchain fetched here never
# touches ~/.cargo and is removed with the app; default keeps it next to the
# python environment for standalone runs.
TOOLS_DIR="${SUTURA_TOOLS_DIR:-}"
if [ -z "$TOOLS_DIR" ]; then
    TOOLS_DIR="$(cd "$(dirname "$PY")/.." 2>/dev/null && pwd)/tools"
fi
RUSTUP_HOME="${SUTURA_RUSTUP_HOME:-$TOOLS_DIR/rustup}"
CARGO_HOME="${SUTURA_CARGO_HOME:-$TOOLS_DIR/cargo}"

# Measured sizes (macOS arm64, 2026-11; Linux is comparable). Download figures
# are the compressed components; on-disk figures are `du -sh`. See
# docs/guide/install.md.
SIZE_RUSTUP_DL="~11 MB"
SIZE_CARGO_DL="~90 MB"
SIZE_CARGO_DISK="~460 MB"
SIZE_CMAKE_DL="~54 MB"
SIZE_CMAKE_DISK="~137 MB"

note() { echo "    sutura_geom: $*"; }

cargo_usable() {
    command -v cargo >/dev/null 2>&1 && cargo --version >/dev/null 2>&1
}

# Fetch rustup + a minimal stable toolchain into $TOOLS_DIR (user space only,
# never ~/.cargo, never the shell rc). Only called when cargo is absent.
fetch_rust() {
    local tmp
    command -v curl >/dev/null 2>&1 || return 1
    note "no Rust toolchain found; fetching rustup into $TOOLS_DIR"
    note "  download: rustup $SIZE_RUSTUP_DL + minimal stable toolchain $SIZE_CARGO_DL"
    note "  on disk:  ~$SIZE_CARGO_DISK ($TOOLS_DIR)"
    mkdir -p "$RUSTUP_HOME" "$CARGO_HOME" || return 1
    tmp="$(mktemp "${TMPDIR:-/tmp}/sutura-rustup.XXXXXX")" || return 1
    if ! curl -fsSL -o "$tmp" "https://sh.rustup.rs"; then rm -f "$tmp"; return 1; fi
    if ! RUSTUP_HOME="$RUSTUP_HOME" CARGO_HOME="$CARGO_HOME" \
         sh "$tmp" -y --no-modify-path --profile minimal --default-toolchain stable; then
        rm -f "$tmp"; return 1
    fi
    rm -f "$tmp"
    export RUSTUP_HOME CARGO_HOME
    export PATH="$CARGO_HOME/bin:$PATH"
    cargo_usable
}

# Install cmake from its PyPI wheel into a throwaway tools venv (never into the
# runtime environment). The cmake tools venv is discarded after a successful
# source build.
install_cmake_tools() {
    local venv="$TOOLS_DIR/cmake-venv"
    note "installing cmake ($SIZE_CMAKE_DL download, ~$SIZE_CMAKE_DISK on disk) into a throwaway tools dir"
    "$PY" -m venv "$venv" || return 1
    "$venv/bin/pip" install --quiet --upgrade pip >/dev/null 2>&1 || true
    if ! "$venv/bin/pip" install --quiet cmake; then return 1; fi
    export PATH="$venv/bin:$PATH"
    command -v cmake >/dev/null 2>&1
}

finish_fail() {
    echo "error: sutura_geom is required but could not be installed ($*)." >&2
    echo "       Platform: $(uname -s)/$(uname -m)" >&2
    echo "       No compatible prebuilt wheel is available for this platform," >&2
    echo "       and the automatic source fallback cannot run here. The installer" >&2
    echo "       cannot continue without sutura_geom." >&2
    exit 1
}

import_ok() {
    "$PY" -c 'import sutura_geom' >/dev/null 2>&1
}

case "${SUTURA_NO_GEOM:-}" in
    ''|0) ;;   # unset / empty / 0 mean "not requested"
    *)
        echo "error: sutura_geom is now required; SUTURA_NO_GEOM is no longer supported." >&2
        exit 1
        ;;
esac

pip_install() {
    "$PY" -m pip install --quiet --force-reinstall --no-deps "$@"
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

# --- 0. explicit local wheel ------------------------------------------------
if [ -n "${SUTURA_GEOM_WHEEL:-}" ]; then
    pip_install "$SUTURA_GEOM_WHEEL" && import_ok && { note "installed from $SUTURA_GEOM_WHEEL"; exit 0; }
    finish_fail "local wheel $SUTURA_GEOM_WHEEL did not install"
fi

# --- 1. prebuilt wheel from the release --------------------------------------
try_wheel() {
    local base="$1" dl sums pat line name want got
    command -v curl >/dev/null 2>&1 || return 1
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64)   pat='manylinux[^ ]*x86_64\.whl$' ;;
        Linux/aarch64)  pat='manylinux[^ ]*aarch64\.whl$' ;;
        Darwin/*)       pat='macosx[^ ]*universal2\.whl$' ;;
        *) return 1 ;;
    esac
    dl="$(mktemp -d "${TMPDIR:-/tmp}/sutura-geom.XXXXXX")" || return 1
    sums="$dl/$SUMS_NAME"
    if ! curl -fsSL --retry 2 -o "$sums" "$base/$SUMS_NAME" 2>/dev/null; then
        rm -rf "$dl"; return 1
    fi
    line="$(grep -E "$pat" "$sums" | head -1 || true)"
    [ -n "$line" ] || { rm -rf "$dl"; return 1; }
    want="$(echo "$line" | awk '{print $1}')"
    name="$(echo "$line" | awk '{print $2}' | sed 's/^\*//')"
    note "downloading $name (~0.7 MB)"
    if ! curl -fsSL --retry 2 -o "$dl/$name" "$base/$name"; then
        rm -rf "$dl"; return 1
    fi
    got="$(sha256_of "$dl/$name")"
    if [ "$got" != "$want" ]; then
        note "SHA-256 mismatch for $name, discarding download"
        rm -rf "$dl"; return 1
    fi
    if pip_install "$dl/$name" && import_ok; then
        rm -rf "$dl"; return 0
    fi
    rm -rf "$dl"; return 1
}

try_wheel_retry() {
    local base="$1" attempt
    for attempt in 1 2 3 4 5; do
        if try_wheel "$base"; then
            return 0
        fi
        [ "$attempt" -lt 5 ] && sleep 8
    done
    return 1
}

if [ "${SUTURA_GEOM_FROM_SOURCE:-0}" != "1" ]; then
    version="$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$SRC/sutura/repair.py" 2>/dev/null | head -1)"
    if [ -n "$version" ] && try_wheel_retry "$RELEASE_BASE/download/v$version"; then
        note "installed prebuilt wheel (v$version)"
        exit 0
    fi
    if try_wheel_retry "$RELEASE_BASE/latest/download"; then
        note "installed prebuilt wheel (latest release)"
        exit 0
    fi
fi

# --- 2. build from source ----------------------------------------------------
# When no prebuilt wheel exists the extension is built from rust/sutura-geom.
# Everything the build needs is fetched automatically into the install tree
# (user space only, never ~/.cargo); set SUTURA_NO_TOOLCHAIN_FETCH=1 to refuse.
if [ -f "$SRC/rust/sutura-geom/pyproject.toml" ]; then
    if ! cargo_usable; then
        if [ "${SUTURA_NO_TOOLCHAIN_FETCH:-0}" = "1" ]; then
            finish_fail "no Rust toolchain and SUTURA_NO_TOOLCHAIN_FETCH=1 (install Rust from https://rustup.rs, or set SUTURA_GEOM_WHEEL)"
        fi
        note "no prebuilt wheel for $(uname -s)/$(uname -m): building from source"
        fetch_rust || finish_fail "could not bootstrap the Rust toolchain"
    fi
    if ! command -v cmake >/dev/null 2>&1; then
        install_cmake_tools || finish_fail "could not install cmake (the source build needs it)"
    fi
    # A C++20 compiler is the one prerequisite we cannot install in user space.
    if [ -z "${CXX:-}" ]; then
        if ! { command -v c++ >/dev/null 2>&1 || command -v g++ >/dev/null 2>&1 \
               || command -v clang++ >/dev/null 2>&1; }; then
            echo "error: no C++ compiler found; the source build needs a C++20 compiler." >&2
            case "$(uname -s)" in
                Darwin) echo "       Run: xcode-select --install   (macOS shows its installer dialog)" >&2 ;;
                *)      echo "       Install a C++ compiler (e.g. build-essential / gcc-c++), then re-run." >&2 ;;
            esac
            finish_fail "missing C++20 compiler"
        fi
    fi
    # rustup's cargo shim exists even when no default toolchain is selected
    # (common after a HOME change). Probe it so the failure names the fix
    # instead of a generic build error.
    cargo_probe="$(cargo --version 2>&1)" || true
    if ! cargo_usable; then
        case "$cargo_probe" in
            *[Nn]o\ default*|*could\ not\ choose*|*no\ toolchain*|*not\ installed*)
                finish_fail "cargo has no default toolchain; source fallback unavailable" ;;
            *)
                finish_fail "cargo is not usable: $cargo_probe" ;;
        esac
    fi
    note "building from rust/sutura-geom (cargo found; ~1 min, pulls maturin ~10 MB)"
    if "$PY" -m pip install --quiet --force-reinstall --no-deps "$SRC/rust/sutura-geom" && import_ok; then
        rm -rf "$TOOLS_DIR/cmake-venv"   # throwaway cmake tools dir
        note "built and installed from source"
        exit 0
    fi
    finish_fail "source build failed"
fi

finish_fail "no prebuilt wheel for $(uname -s)/$(uname -m) and rust/sutura-geom is missing from the source; cannot build"
