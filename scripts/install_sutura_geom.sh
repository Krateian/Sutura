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
set -uo pipefail

PY="${1:?usage: install_sutura_geom.sh <python> [<repo-src-dir>]}"
SRC="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RELEASE_BASE="${SUTURA_RELEASE_BASE:-https://github.com/Krateian/Sutura/releases}"
SUMS_NAME="sutura_geom-SHA256SUMS"

note() { echo "    sutura_geom: $*"; }

finish_fail() {
    echo "error: sutura_geom is required but could not be installed ($*)." >&2
    echo "       Platform: $(uname -s)/$(uname -m)" >&2
    echo "       Install Rust (https://rustup.rs) so the extension can be built from" >&2
    echo "       source, or set SUTURA_GEOM_WHEEL=<path> to a compatible wheel," >&2
    echo "       then re-run the installer." >&2
    exit 1
}

import_ok() {
    "$PY" -c 'import sutura_geom' >/dev/null 2>&1
}

if [ -n "${SUTURA_NO_GEOM:-}" ]; then
    echo "error: sutura_geom is now required; SUTURA_NO_GEOM is no longer supported." >&2
    exit 1
fi

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
    line="$(grep -E "$pat" "$sums" | head -1)"
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

if [ "${SUTURA_GEOM_FROM_SOURCE:-0}" != "1" ]; then
    version="$(sed -n 's/^VERSION = "\(.*\)"/\1/p' "$SRC/sutura/repair.py" 2>/dev/null | head -1)"
    if [ -n "$version" ] && try_wheel "$RELEASE_BASE/download/v$version"; then
        note "installed prebuilt wheel (v$version)"
        exit 0
    fi
    if try_wheel "$RELEASE_BASE/latest/download"; then
        note "installed prebuilt wheel (latest release)"
        exit 0
    fi
fi

# --- 2. build from source ----------------------------------------------------
if [ -f "$SRC/rust/sutura-geom/pyproject.toml" ] && command -v cargo >/dev/null 2>&1; then
    # rustup's cargo shim exists even when no default toolchain is selected
    # (common after a HOME change). Probe it so the failure names the fix
    # instead of a generic build error.
    cargo_probe="$(cargo --version 2>&1)" || true
    if ! cargo --version >/dev/null 2>&1; then
        case "$cargo_probe" in
            *[Nn]o\ default*|*could\ not\ choose*|*no\ toolchain*|*not\ installed*)
                finish_fail "cargo is present but rustup has no default toolchain; run 'rustup default stable' and re-run" ;;
            *)
                finish_fail "cargo is not usable: $cargo_probe" ;;
        esac
    fi
    note "building from rust/sutura-geom (cargo found; ~1 min, pulls maturin ~10 MB)"
    if "$PY" -m pip install --quiet --force-reinstall --no-deps "$SRC/rust/sutura-geom" && import_ok; then
        note "built and installed from source"
        exit 0
    fi
    finish_fail "source build failed"
fi

finish_fail "no prebuilt wheel for $(uname -s)/$(uname -m) and no Rust toolchain (cargo not found)"
