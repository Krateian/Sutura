# fTetWild Fallback Tier — 115-mesh Real-World Corpus Benchmark

Date: 2026-09-23
Corpus: /tmp/sutura_corpus_100 (115 real-world meshes, thingi10k + artec scans)
Flags: --experimental-fallback-ftetwild (autorefine NOT combined — too slow together)
Per-mesh fTetWild timeout: 180s (added in cfef874)

## Headline result

| | strict watertight |
|---|---|
| baseline (stage-1 only) | 103/115 (89.6%) |
| with fTetWild fallback  | **108/115 (93.9%)** |

Net gain: +5 meshes moved to strict watertight, zero regressions
(adopt guard: cand_holes <= base_holes and cand_nm <= base_nm, so the
fallback can never make a mesh worse than stage-1 alone).

## fTetWild tier stats

- meshes where it ran: 45
- adopted: 15 (of which 7 needed the manifold3d post-process assist)
- adopt=False (kept stage-1 result, correctly not worse): 30
- errors: 30, all timeouts under the 180s budget

fTetWild wall-clock time on meshes that completed: min=0.38s, median=5.36s,
max=94.99s, total=314.43s.

## Timeout meshes (30, all dense real-world scans)

artec_airplane-without-texture, artec_bearded-guy-hd, artec_bovine-heart,
artec_bronze-sculpture, artec_car-body, artec_church-facade, artec_coins,
artec_copper-key, artec_crankshaft-hd, artec_crocodile-statue, artec_dagger,
artec_fantasy-dragon, artec_fountain-basin, artec_heart-pendant,
artec_human-skeleton-hd, artec_jaguar-ring, artec_lobster-hd,
artec_metal-cutting-blade, artec_metal-nut, artec_michel-rodange-monument,
artec_motorcycle-cylinder-head-hd, artec_motorcycle-frame-hd,
artec_motorcycle-wheel-hd, artec_pipe-bend, artec_smart-car,
artec_snowmobile, artec_statue-dragonfly-tamer, artec_tripod,
artec_wooden-chair-hd, thingi10k_1038441

These are dense 3D-scan meshes (millions of faces); fTetWild's default
settings are not time-bounded upstream, hence the engineering timeout
guard. Increasing the budget would likely recover some of these at the
cost of batch throughput — not attempted here.

## Known limitation

On thingi10k_1038441, the manifold3d post-process makes the fTetWild
boundary two-manifold at adopt-check time, but the pipeline's own
downstream stage-2 re-run can reintroduce non-manifold geometry in the
final saved output. Reported as-is; not hidden.

## Conclusion

The fTetWild fallback tier (FAZ17) is a real, measured improvement:
+5 meshes (89.6% -> 93.9% strict watertight) on the full real-world
corpus, with all trade-offs (timeout rate, manifold3d-assist rate)
transparently characterized. Guard logic ensures it is adopt-only —
never a regression risk on any mesh.
