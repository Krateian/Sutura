#!/usr/bin/env bash
# fetch_benchmark_corpus.sh — download, verify and extract the 115-mesh repair
# benchmark corpus from the Krateian/Sutura GitHub release `benchmark-corpus-v1`.
#
# The corpus (52 Artec STL scans + 63 Thingi10K STLs, ~6 GB uncompressed) is
# intentionally NOT in the git repo (it would multiply the repo size ~100x). It
# is published as a split `.tar.gz` (GitHub caps release assets at 2 GB) on the
# release above. This script downloads the parts, verifies SHA-256, recombines
# and extracts them, so any machine (e.g. a future Linux box) can reproduce the
# strict-watertight benchmark without re-scraping Thingi10K.
#
# Usage:
#   scripts/fetch_benchmark_corpus.sh [dest_dir]
#   dest_dir defaults to /tmp/sutura_corpus_100 -- the default corpus location
#   used by scripts/benchmark_repair_corpus.py.
#
# NOTE: the release is created as a GitHub DRAFT; the public download URLs below
# only respond once it is published (non-draft). Publish it (e.g.
# `gh release edit benchmark-corpus-v1 --draft=false`) before relying on this
# script on another machine.
#
# Requires: curl, and sha256sum (GNU coreutils; on macOS `shasum -a 256`).
set -euo pipefail

REPO="Krateian/Sutura"
TAG="benchmark-corpus-v1"
BASE="https://github.com/$REPO/releases/download/$TAG"
DEST="${1:-/tmp/sutura_corpus_100}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# The two split parts of sutura-benchmark-corpus-v1.tar.gz, plus the checksum
# file, as published on the release. If the corpus is re-packaged (new meshes),
# update this list and re-upload the new parts + SHA256SUMS (see
# docs/ATTRIBUTION.md -> "Benchmark corpus release").
PARTS=(
  "sutura-benchmark-corpus-v1.tar.gz.part-aa"
  "sutura-benchmark-corpus-v1.tar.gz.part-ab"
)
SHA_SUMS_URL="$BASE/SHA256SUMS"

# sha256sum on Linux, `shasum -a 256` on macOS.
if command -v sha256sum >/dev/null 2>&1; then
  SHA="sha256sum"
else
  SHA="shasum -a 256"
fi

echo "Fetching benchmark corpus from $BASE (parts: ${#PARTS[@]}, ~3.6 GB)."
for part in "${PARTS[@]}"; do
  echo "  downloading $part ..."
  curl -fL --retry 3 -o "$TMP/$part" "$BASE/$part"
done
echo "  downloading SHA256SUMS ..."
curl -fL --retry 3 -o "$TMP/SHA256SUMS" "$SHA_SUMS_URL"

echo "Verifying SHA-256 of the downloaded parts ..."
(cd "$TMP" && $SHA -c SHA256SUMS)

echo "Recombining and extracting to $DEST ..."
mkdir -p "$DEST"
cat "$TMP"/sutura-benchmark-corpus-v1.tar.gz.part-* > "$TMP/corpus.tar.gz"
tar -xzf "$TMP/corpus.tar.gz" -C "$DEST"

count=$(ls "$DEST"/*.stl 2>/dev/null | wc -l | tr -d ' ')
echo "Done: $DEST ($count STL files)."
echo "Next: ~/.local/share/sutura/venv/bin/python scripts/benchmark_repair_corpus.py $DEST out.json"