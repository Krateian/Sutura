#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FTETWILD_PATCH="$SCRIPT_DIR/ftetwild.patch"
GEOGRAM_PATCH="$SCRIPT_DIR/geogram.patch"
UPSTREAM_COMMIT="8118f810478e0e65a7bf2d8cecdc5e203a876e97"
UPSTREAM_REPO="https://github.com/wildmeshing/fTetWild.git"

BUILD_ROOT="${1:-${BUILD_DIR:-/tmp/p10-ftw2}}"
SRC_DIR="$BUILD_ROOT/fTetWild"
BUILD_DIR="$BUILD_ROOT/build"
OUT_BIN="${2:-${OUT_BIN:-$SCRIPT_DIR/bin/ftetwild}}"

mkdir -p "$BUILD_ROOT"
mkdir -p "$(dirname "$OUT_BIN")"

echo "=== 1. Checking / Fetching fTetWild upstream ($UPSTREAM_COMMIT) ==="
if [ ! -d "$SRC_DIR/.git" ]; then
    echo "Cloning $UPSTREAM_REPO into $SRC_DIR..."
    git clone "$UPSTREAM_REPO" "$SRC_DIR"
fi

cd "$SRC_DIR"
git fetch origin
git checkout -f "$UPSTREAM_COMMIT"
git reset --hard "$UPSTREAM_COMMIT"
git clean -fd

echo "=== 2. Applying fTetWild determinism patch ==="
if [ ! -f "$FTETWILD_PATCH" ]; then
    echo "ERROR: ftetwild.patch not found at $FTETWILD_PATCH" >&2
    exit 1
fi
git apply --verbose "$FTETWILD_PATCH"

echo "=== 3. Resolving Static GMP and Build Dependencies ==="
STATIC_GMP=""
if [[ "$(uname -s)" == "Darwin" ]]; then
    if command -v brew >/dev/null 2>&1; then
        BREW_GMP="$(brew --prefix gmp 2>/dev/null || true)"
        if [ -n "$BREW_GMP" ] && [ -f "$BREW_GMP/lib/libgmp.a" ]; then
            STATIC_GMP="$BREW_GMP/lib/libgmp.a"
        fi
    fi
fi
if [ -z "$STATIC_GMP" ]; then
    for cand in \
        /opt/homebrew/opt/gmp/lib/libgmp.a \
        /usr/local/opt/gmp/lib/libgmp.a \
        /usr/lib/x86_64-linux-gnu/libgmp.a \
        /usr/lib/aarch64-linux-gnu/libgmp.a \
        /usr/lib/libgmp.a \
        /usr/local/lib/libgmp.a; do
        if [ -f "$cand" ]; then
            STATIC_GMP="$cand"
            break
        fi
    done
fi

if [ -z "$STATIC_GMP" ] || [ ! -f "$STATIC_GMP" ]; then
    echo "ERROR: Static libgmp.a library not found! fTetWild must be statically linked against GMP." >&2
    exit 1
fi
echo "Using static GMP library: $STATIC_GMP"

echo "=== 4. Configuring CMake ==="
GENERATOR="Unix Makefiles"
if command -v ninja >/dev/null 2>&1; then
    GENERATOR="Ninja"
fi

CMAKE_FLAGS=(
    -G "$GENERATOR"
    -DCMAKE_BUILD_TYPE=Release
    -DFLOATTETWILD_BUILD_TESTS=OFF
    -DCMAKE_POLICY_DEFAULT_CMP0169=OLD
    -DFLOAT_TETWILD_ENABLE_TBB=ON
    -DBUILD_SHARED_LIBS=OFF
    -DTBB_TEST=OFF
    -DGMP_LIBRARIES="$STATIC_GMP"
)

GMP_INC="$(dirname "$(dirname "$STATIC_GMP")")/include"
if [ -d "$GMP_INC" ]; then
    CMAKE_FLAGS+=("-DGMP_INCLUDE_DIRS=$GMP_INC")
fi

if command -v ccache >/dev/null 2>&1; then
    CMAKE_FLAGS+=(
        -DCMAKE_C_COMPILER_LAUNCHER=ccache
        -DCMAKE_CXX_COMPILER_LAUNCHER=ccache
    )
fi

# Compiler flags for determinism & exact predicates
EXTRA_CXX_FLAGS="-ffp-contract=off"

if [[ "$(uname -s)" == "Darwin" ]]; then
    SDK_PATH="$(xcrun --show-sdk-path 2>/dev/null || echo '/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk')"
    if [ -d "$SDK_PATH" ]; then
        CMAKE_FLAGS+=("-DCMAKE_OSX_SYSROOT=$SDK_PATH")
        if [ -d "$SDK_PATH/usr/include/c++/v1" ]; then
            EXTRA_CXX_FLAGS="$EXTRA_CXX_FLAGS -cxx-isystem $SDK_PATH/usr/include/c++/v1"
        fi
    fi
fi

CMAKE_FLAGS+=("-DCMAKE_CXX_FLAGS_RELEASE=-O3 -DNDEBUG $EXTRA_CXX_FLAGS")

# Initial configure (downloads FetchContent dependencies)
set +e
cmake -B "$BUILD_DIR" -S "$SRC_DIR" "${CMAKE_FLAGS[@]}"
CMAKE_RC=$?
set -e
if [ $CMAKE_RC -ne 0 ]; then
    EIGEN_CMAKE="$BUILD_DIR/_deps/libigl-src/cmake/recipes/external/eigen.cmake"
    if [ -f "$EIGEN_CMAKE" ]; then
        python3 -c "
path = '$EIGEN_CMAKE'
with open(path, 'r') as f:
    c = f.read()
if 'cmake_policy(SET CMP0169 OLD)' not in c:
    c = 'cmake_policy(SET CMP0169 OLD)\n' + c
    with open(path, 'w') as f:
        f.write(c)
"
        echo "Patched eigen.cmake with CMP0169 OLD for CMake 4.x compatibility."
        cmake -B "$BUILD_DIR" -S "$SRC_DIR" "${CMAKE_FLAGS[@]}"
    else
        echo "ERROR: CMake configuration failed with exit code $CMAKE_RC" >&2
        exit $CMAKE_RC
    fi
fi

echo "=== 5. Applying Geogram BRIO determinism patch ==="
GEO_DIR="$BUILD_DIR/_deps/geogram-src"
if [ ! -d "$GEO_DIR" ]; then
    echo "ERROR: Geogram source directory not found at $GEO_DIR" >&2
    exit 1
fi
if [ ! -f "$GEOGRAM_PATCH" ]; then
    echo "ERROR: geogram.patch not found at $GEOGRAM_PATCH" >&2
    exit 1
fi

cd "$GEO_DIR"
if git apply --check "$GEOGRAM_PATCH" >/dev/null 2>&1; then
    git apply --verbose "$GEOGRAM_PATCH"
    echo "Successfully applied Geogram BRIO determinism patch."
elif git apply --reverse --check "$GEOGRAM_PATCH" >/dev/null 2>&1; then
    echo "Geogram BRIO patch is already applied."
else
    echo "ERROR: Geogram BRIO patch cannot be applied cleanly to $GEO_DIR!" >&2
    exit 1
fi

echo "=== 6. Building FloatTetwild_bin ==="
NUM_JOBS=4
if command -v nproc >/dev/null 2>&1; then
    NUM_JOBS="$(nproc)"
elif command -v sysctl >/dev/null 2>&1; then
    NUM_JOBS="$(sysctl -n hw.ncpu 2>/dev/null || echo 4)"
fi

cmake --build "$BUILD_DIR" --target FloatTetwild_bin -j "$NUM_JOBS"

BUILT_BIN="$BUILD_DIR/FloatTetwild_bin"
if [ ! -f "$BUILT_BIN" ]; then
    echo "ERROR: Built binary not found at $BUILT_BIN" >&2
    exit 1
fi

echo "=== 7. Installing & Stripping binary to $OUT_BIN ==="
cp "$BUILT_BIN" "$OUT_BIN"
chmod +x "$OUT_BIN"
if command -v strip >/dev/null 2>&1; then
    if [[ "$(uname -s)" == "Darwin" ]]; then
        strip -u -r "$OUT_BIN" 2>/dev/null || strip "$OUT_BIN" 2>/dev/null || true
    else
        strip -s "$OUT_BIN" 2>/dev/null || strip "$OUT_BIN" 2>/dev/null || true
    fi
fi

echo "=== 8. Verifying Shared Library Linkage ==="
if [[ "$(uname -s)" == "Darwin" ]]; then
    echo "otool -L $OUT_BIN:"
    otool -L "$OUT_BIN"
    NON_SYSTEM=$(otool -L "$OUT_BIN" | grep -v "$OUT_BIN:" | grep -v "/usr/lib/libc++.1.dylib" | grep -v "/usr/lib/libSystem.B.dylib" || true)
    if [ -n "$NON_SYSTEM" ]; then
        echo "ERROR: Built binary contains disallowed non-system dynamic dependencies:" >&2
        echo "$NON_SYSTEM" >&2
        exit 1
    fi
    echo "Verification OK: binary links ONLY to macOS system libraries."
elif command -v ldd >/dev/null 2>&1; then
    echo "ldd $OUT_BIN:"
    ldd "$OUT_BIN"
fi

echo "=== Done: $OUT_BIN built successfully ($(du -h "$OUT_BIN" | cut -f1)) ==="
