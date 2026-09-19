# Real-world samples

Real downloadable meshes used as regression fixtures (no synthetic meshes).
Run the whole corpus with:

```sh
~/.local/share/sutura/venv/bin/python tests/test_real_world_corpus.py
```

| File | Type (classifier) | Defects | Repair result |
|---|---|---|---|
| `artec_spanner.stl` | organic* | clean | watertight |
| `artec_pipe-bend.stl` | organic* | clean | watertight |
| `artec_metal-nut.stl` | organic* | 9 holes | watertight |
| `artec_copper-key.stl` | organic* | 4 holes | watertight |
| `artec_plastic-bolt.stl` | mechanical | clean | watertight |
| `artec_crocodile-statue.stl` | organic | 12 holes | watertight |
| `artec_bovine-heart.stl` | organic | 4 holes | watertight |
| `artec_lobster-hd.stl` | organic | clean | watertight |
| `thingi10k_100045.stl` | unknown | clean | watertight |
| `thingi10k_100077.stl` | mechanical | clean | watertight |
| `thingi10k_100173.stl` | organic | 1 hole | watertight |
| `thingi10k_100827.stl` | unknown | 5 holes | warning (partial) |
| `thingi10k_1038439.stl` | mechanical | 10 holes + self-intersections | warning (partial) |
| `thingi10k_1038441.stl` | organic | 5 holes + 13 non-manifold | warning (partial) |
| `thingi10k_224108.stl` | — | non-manifold, open | two-manifold, 15 residual micro-holes |
| `thingi10k_502009.stl` | — | self-intersecting (164) | fully watertight |

\* **Known classifier limitation:** the `artec_*` scans are mechanical parts
(screw, spanner, nut, key, pipe) that the mesh classifier currently reads as
*organic* with high confidence — scan noise reads as gentle curvature. They
are kept as regression fixtures so a future classifier change cannot silently
break the corpus. The decimated Artec files preserve the original defect
counts (see ATTRIBUTION.md).

The two-manifold-but-not-watertight results are the documented VCG
limitation on layered/folded geometry (see README, "Known limitations").
