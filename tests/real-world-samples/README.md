# Real-world samples

Three broken meshes from the [Thingi10K](https://ten-thousand-models.appspot.com/)
dataset (Zhou & Jacobson), picked as instructive repair cases. Each is a real
downloadable model, not synthetic.

| File | Failure | Repair result (as of the current code) |
|---|---|---|
| `thingi10k_1038439.stl` | non-manifold **and** self-intersecting (955 self-intersections) | two-manifold, 11 residual micro-holes |
| `thingi10k_224108.stl` | non-manifold, open (4 boundary edges) | two-manifold, 15 residual micro-holes |
| `thingi10k_502009.stl` | self-intersecting (164 self-intersections) | fully watertight |

Try them:

```sh
sutura tests/real-world-samples/thingi10k_502009.stl --human
sutura tests/real-world-samples/thingi10k_224108.stl --human
sutura tests/real-world-samples/thingi10k_1038439.stl --human
```

The two-manifold-but-not-watertight results are the documented VCG
limitation on layered/folded geometry (see README, "Known limitations").
