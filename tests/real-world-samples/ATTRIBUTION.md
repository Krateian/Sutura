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

### Phase 2 additions (2026-09, CC BY 4.0 / CC0 only)

Added to grow the corpus to 40 meshes with a deliberately hard mix: curved /
free-form mechanical parts (pipe-bend siblings: spiral pipe, hook, ball screw,
curved gears) and free-form organics. Files with more than ~90k faces were
decimated (PyMeshLab quadric edge collapse, `preservetopology=True`) to keep
the corpus practical, matching the Artec handling. All are **CC BY 4.0** except
`thingi10k_70561` which is **CC0** (Public Domain Dedication). Download:
Thingi10K HF mirror `raw_meshes/<id>.stl`.

| File | License | Model (Thingiverse) | Author | Source |
|---|---|---|---|---|
| `thingi10k_55772.stl` | CC BY | Spiral Panpipes | ranjit | [thing:17020](https://www.thingiverse.com/thing:17020) |
| `thingi10k_71691.stl` | CC BY | Pirate Hook | Emmanuel | [thing:23579](https://www.thingiverse.com/thing:23579) |
| `thingi10k_235725.stl` | CC BY | Threadless Ball Screw | MSollack | [thing:112718](https://www.thingiverse.com/thing:112718) |
| `thingi10k_81221.stl` | CC BY | Nautilus Gears | MishaT | [thing:27233](https://www.thingiverse.com/thing:27233) |
| `thingi10k_228302.stl` | CC BY | Geared Coffee Sleeve | sirmakesalot | [thing:108021](https://www.thingiverse.com/thing:108021) |
| `thingi10k_248395.stl` | CC BY | HingeBox | profhankd | [thing:120179](https://www.thingiverse.com/thing:120179) |
| `thingi10k_42844.stl` | CC BY | Ring Adapter For Lapel Button | degroof | [thing:13143](https://www.thingiverse.com/thing:13143) |
| `thingi10k_60916.stl` | CC BY | Gear O'Clock | Alzibiff | [thing:18959](https://www.thingiverse.com/thing:18959) |
| `thingi10k_59226.stl` | CC BY | Exploded Planetary Gear Set | Thing-O-Fun | [thing:18291](https://www.thingiverse.com/thing:18291) |
| `thingi10k_70561.stl` | CC0 | Tiny Planetary Gears Set | aubenc | [thing:23030](https://www.thingiverse.com/thing:23030) |
| `thingi10k_57854.stl` | CC BY | Panasonic Irrigator Bracket | takuya | [thing:17789](https://www.thingiverse.com/thing:17789) |
| `thingi10k_475828.stl` | CC BY | Fennec Fox Head Drawer Handle | Bluebie | [thing:261218](https://www.thingiverse.com/thing:261218) |
| `thingi10k_260537.stl` | CC BY | Great White Skull | MakerBot | [thing:128112](https://www.thingiverse.com/thing:128112) |
| `thingi10k_313444.stl` | CC BY | Waving Cat | bourbon_and_cigars | [thing:163032](https://www.thingiverse.com/thing:163032) |
| `thingi10k_39507.stl` | CC BY | Roal The Bratty Dragon | SplotchyInk | [thing:12227](https://www.thingiverse.com/thing:12227) |
| `thingi10k_55280.stl` | CC BY | Dragon | artec3d | [thing:16860](https://www.thingiverse.com/thing:16860) |
| `thingi10k_40886.stl` | CC BY | Fist Sculpture | nathan | [thing:12629](https://www.thingiverse.com/thing:12629) |
| `thingi10k_100281.stl` | CC BY | Holed Christmas Ornament | pmoews | [thing:34942](https://www.thingiverse.com/thing:34942) |
| `thingi10k_63785.stl` | CC BY | Mouse Skull (micro-CT) | MarkU | [thing:20200](https://www.thingiverse.com/thing:20200) |
| `thingi10k_136634.stl` | CC BY | Decorative Cat Bowls | pmoews | [thing:52075](https://www.thingiverse.com/thing:52075) |
| `thingi10k_145065.stl` | CC BY | Eiffel Tower | mphardy | [thing:49131](https://www.thingiverse.com/thing:49131) |
| `thingi10k_1356633.stl` | CC BY | Gowanus Monster | boldmachines | [thing:854906](https://www.thingiverse.com/thing:854906) |
| `thingi10k_331105.stl` | CC BY | Angel Candle Holder | tbuser | [thing:174208](https://www.thingiverse.com/thing:174208) |
| `thingi10k_46012.stl` | CC BY | Earth Shot | WilliamAAdams | [thing:14070](https://www.thingiverse.com/thing:14070) |