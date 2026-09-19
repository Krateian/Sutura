# Real-world sample meshes — attribution & licenses

The `.stl` files in this directory are **third-party data** used only as
test/regression fixtures. They are **not** covered by Sutura's own license
(PolyForm Noncommercial 1.0.0, which applies to the code). Each file keeps
its original license; you are responsible for respecting it if you reuse the
files outside Sutura's test suite.

## Artec 3D scans (CC BY 4.0)

Source: https://www.artec3d.com/3d-models/stl (free STL catalog).

These are **decimated** versions (PyMeshLab quadric edge collapse,
`preservetopology=True`, ~90k faces) of the original Artec scan downloads —
decimation preserves the defect character (holes/non-manifold counts are
reported in `tests/real-world-samples/README.md`). License: **Creative
Commons Attribution 4.0 (CC BY 4.0)** — attribution to *Artec 3D* and a link
back to artec3d.com are required.

| File | Original model |
|---|---|
| `artec_bovine-heart.stl` | Bovine heart — artec3d.com/3d-models/bovine-heart |
| `artec_copper-key.stl` | Copper key — artec3d.com/3d-models/copper-key |
| `artec_crocodile-statue.stl` | Crocodile statue — artec3d.com/3d-models/crocodile-statue |
| `artec_lobster-hd.stl` | Lobster (HD) — artec3d.com/3d-models/lobster-hd |
| `artec_metal-nut.stl` | Metal nut — artec3d.com/3d-models/metal-nut |
| `artec_pipe-bend.stl` | Pipe bend — artec3d.com/3d-models/pipe-bend |
| `artec_plastic-bolt.stl` | Plastic bolt — artec3d.com/3d-models/plastic-bolt |
| `artec_spanner.stl` | Spanner — artec3d.com/3d-models/spanner |

## Thingi10K meshes (varying original licenses)

Source: the Thingi10K dataset (Qingnan Zhou & Alec Jacobson,
https://ten-thousand-models.appspot.com/), mirrored on Hugging Face
(`Thingi10K/Thingi10K`, `raw_meshes/`). The **original licenses vary per
model** (CC0 / CC BY / CC BY-SA etc. as recorded on the original Thingiverse
page); check each model's page before any reuse.

| File | Original model page |
|---|---|
| `thingi10k_100045.stl` | thingiverse.com/thing:100045 |
| `thingi10k_100077.stl` | thingiverse.com/thing:100077 |
| `thingi10k_100173.stl` | thingiverse.com/thing:100173 |
| `thingi10k_100827.stl` | thingiverse.com/thing:100827 |
| `thingi10k_1038439.stl` | thingiverse.com/thing:1038439 |
| `thingi10k_1038441.stl` | thingiverse.com/thing:1038441 |
| `thingi10k_224108.stl` | thingiverse.com/thing:224108 |
| `thingi10k_502009.stl` | thingiverse.com/thing:502009 |