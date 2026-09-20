# Mesh corpus attribution & licenses

The real-world test corpus used by the mesh classifier (`tests/real-world-samples/`)
contains **third-party meshes** used only as test/regression fixtures. They are
**not** covered by Sutura's own license (PolyForm Noncommercial 1.0.0, which
applies to the code). Each file keeps its original license; you are responsible
for respecting it if you reuse the files outside Sutura's test suite.

- **Artec 3D scans** — 8 meshes, **CC BY 4.0**, source
  https://www.artec3d.com/3d-models/stl (decimated to ~90k faces).
- **Thingi10K** — 32 meshes, per-model **CC BY 4.0 or CC0** (Public Domain),
  source the Thingi10K dataset (Qingnan Zhou & Alec Jacobson), downloaded from
  the Hugging Face mirror `Thingi10K/Thingi10K` `raw_meshes/`. The Phase 2
  additions (2026-09) were restricted to CC BY / CC0 only; the 8 pre-existing
  Thingi10K fixtures include a few CC BY-SA / CC BY-ND models that predate that
  rule and are kept as-is. Files with > ~90k faces are decimated
  (preservetopology) to keep the corpus practical.

The complete per-file attribution table (file, license, model name, author,
Thingiverse source URL) lives in `tests/real-world-samples/ATTRIBUTION.md`.

Corpus composition (40 meshes):

| Group | Count | Files |
|---|---|---|
| Artec scans (CC BY 4.0) | 8 | `artec_*.stl` |
| Thingi10K mechanical (Phase 2, CC BY/CC0) | 13 | `thingi10k_55772/71691/235725/81221/228302/248395/42844/60916/59226/70561/57854/475828/145065` |
| Thingi10K organic (Phase 2, CC BY) | 11 | `thingi10k_260537/313444/39507/55280/40886/100281/63785/136634/1356633/331105/46012` |
| Thingi10K pre-existing fixtures | 8 | `thingi10k_100045/100077/100173/100827/1038439/1038441/224108/502009` |

## Benchmark corpus release (115 meshes)

The large **repair benchmark corpus** — 52 Artec STL scans (CC BY 4.0, from
https://www.artec3d.com/3d-models/stl) + 63 Thingi10K STLs (Zhou & Jacobson;
per-model CC BY 4.0 / CC0, from the Thingi10K dataset / Hugging Face mirror
`Thingi10K/Thingi10K`) — is ~6 GB and is **deliberately not committed to the
git repo** (it would multiply the repo size ~100× and be painful to remove
from history later). It is published instead as a **split `.tar.gz`** on the
GitHub release **`benchmark-corpus-v1`** (two `< 2 GB` parts because GitHub
caps release assets at 2 GB), with a `SHA256SUMS` per-part checksum file.

- **Fetch**: `scripts/fetch_benchmark_corpus.sh [dest_dir]` downloads all
  parts, verifies SHA-256, recombines and extracts (default
  `/tmp/sutura_corpus_100`, the default corpus path used by
  `scripts/benchmark_repair_corpus.py`).
- **Licenses**: the files retain their original licenses (CC BY 4.0 for the
  Artec scans, per-model CC BY 4.0 / CC0 for Thingi10K); reuse outside
  Sutura's benchmark must respect each model's license.
- **How to re-package / re-upload when meshes change**:
  1. Put the new/updated STLs into the corpus directory (keep `*.stl` files
     directly in the directory root).
  2. Re-pack: `cd <corpus_dir> && tar -czf sutura-benchmark-corpus-v1.tar.gz ./*.stl`
     (or a new version tag, e.g. `benchmark-corpus-v2`).
  3. Split into `< 2 GB` parts: `split -b 1900m ... sutura-benchmark-corpus-v1.tar.gz.part-`
     and replace the `PARTS` list + `SHA256SUMS` in `scripts/fetch_benchmark_corpus.sh`.
  4. Compute the new per-part hashes into a `SHA256SUMS` file.
  5. `gh release create <tag> --draft --title "Repair benchmark corpus (115 mesh)" --notes-file <notes>` then `gh release upload <tag> <parts...> SHA256SUMS`.
  6. Re-verify: `scripts/fetch_benchmark_corpus.sh` on a clean machine and re-run
     `scripts/benchmark_repair_corpus.py` to confirm the corpus round-trips.