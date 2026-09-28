# Real-world samples

Real downloadable meshes used as regression fixtures (no synthetic meshes).
40 meshes: 8 Artec 3D scans + 32 Thingi10K (see ATTRIBUTION.md / docs/ATTRIBUTION.md).
Run the whole corpus with:

```sh
~/.local/share/sutura/venv/bin/python tests/test_real_world_corpus.py
```

The 16 original fixtures:

| File | Type (classifier) | Defects | Repair result |
|---|---|---|---|
| `artec_spanner.stl` | organic* | clean | watertight |
| `artec_pipe-bend.stl` | organic* | clean | watertight |
| `artec_metal-nut.stl` | organic* | 9 holes | watertight (graft) |
| `artec_copper-key.stl` | organic* | 4 holes | watertight |
| `artec_plastic-bolt.stl` | mechanical | clean | watertight |
| `artec_crocodile-statue.stl` | organic | 12 holes | watertight |
| `artec_bovine-heart.stl` | organic | 4 holes | watertight |
| `artec_lobster-hd.stl` | organic | clean | watertight |
| `thingi10k_100045.stl` | unknown | clean | watertight |
| `thingi10k_100077.stl` | mechanical | clean | watertight |
| `thingi10k_100173.stl` | organic | 1 hole | watertight |
| `thingi10k_100827.stl` | unknown | 5 holes | watertight (ftetwild via fidelity fallback) |
| `thingi10k_1038439.stl` | mechanical | 10 holes + self-intersections | watertight (graft, 0.0 deviation) |
| `thingi10k_1038441.stl` | organic | 5 holes + 13 non-manifold | watertight (graft, 0.0 deviation) |
| `thingi10k_224108.stl` | — | non-manifold, open | watertight (graft, 0.0 deviation) |
| `thingi10k_502009.stl` | — | self-intersecting (164) | fully watertight |

\* **Known classifier limitation:** the `artec_*` scans are mechanical parts
(screw, spanner, nut, key, pipe) that the mesh classifier currently reads as
*organic* with high confidence — scan noise reads as gentle curvature. They
are kept as regression fixtures so a future classifier change cannot silently
break the corpus. The decimated Artec files preserve the original defect
counts (see ATTRIBUTION.md).

### Phase 2 additions (2026-09)

24 Thingi10K meshes (CC BY 4.0 / CC0) added to grow the corpus to 40 with a
deliberately hard mix — curved/free-form mechanical parts (spiral pipe, hook,
ball screw, curved gears) and free-form organics. Ground-truth classifier labels and repair results:

| File | Type (classifier) | Repair result |
|---|---|---|
| `thingi10k_55772.stl` (Spiral Panpipes) | mechanical | watertight |
| `thingi10k_71691.stl` (Pirate Hook) | mechanical | watertight (graft, 0.0 deviation) |
| `thingi10k_235725.stl` (Threadless Ball Screw) | mechanical | watertight |
| `thingi10k_81221.stl` (Nautilus Gears) | mechanical | watertight |
| `thingi10k_228302.stl` (Geared Coffee Sleeve) | mechanical | watertight |
| `thingi10k_248395.stl` (HingeBox) | mechanical | watertight |
| `thingi10k_42844.stl` (Ring Adapter) | mechanical | watertight |
| `thingi10k_60916.stl` (Gear O'Clock) | mechanical | watertight |
| `thingi10k_59226.stl` (Exploded Planetary Gear Set) | mechanical | watertight |
| `thingi10k_70561.stl` (Tiny Planetary Gears) | mechanical | watertight |
| `thingi10k_57854.stl` (Panasonic Bracket) | mechanical | watertight |
| `thingi10k_475828.stl` (Fennec Fox Drawer Handle) | mechanical | watertight |
| `thingi10k_145065.stl` (Eiffel Tower) | mechanical | watertight |
| `thingi10k_260537.stl` (Great White Skull) | organic | watertight |
| `thingi10k_313444.stl` (Waving Cat) | organic | watertight |
| `thingi10k_39507.stl` (Roal The Bratty Dragon) | organic | watertight |
| `thingi10k_55280.stl` (Dragon, Artec scan) | organic | watertight |
| `thingi10k_40886.stl` (Fist Sculpture) | organic | watertight (graft, 0.0 deviation) |
| `thingi10k_100281.stl` (Holed Christmas Ornament) | organic | watertight |
| `thingi10k_63785.stl` (Mouse skull, micro-CT) | organic | watertight |
| `thingi10k_136634.stl` (Decorative Cat Bowls) | organic | watertight |
| `thingi10k_1356633.stl` (Gowanus Monster) | organic | watertight |
| `thingi10k_331105.stl` (Angel Candle Holder) | organic | watertight |
| `thingi10k_46012.stl` (Earth Shot) | organic | watertight (graft, 0.0 deviation) |

On these 36 labeled samples the default (experimental) classifier scores
29/36 vs classic 16/36 (mechanical recall 16/20 vs 2/20).

Corpus summary (default auto): **40/40 watertight; final tier stage1 32, graft 7, ftetwild 1**.
Adopted Graft meshes report 0.0 healthy deviation (verbatim hybrid); `thingi10k_100827` falls back to fTetWild via fidelity check (`fidelity_ok=false`, superseded by lower deviation).
