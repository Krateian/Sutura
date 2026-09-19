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