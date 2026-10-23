# Tiers Registry

Trigger: Method numbers/slugs/aliases, registry, adding a method, Tamponade/Flap registry, repeat/transplant.

Source: `origin/main:AGENTS.md` @ 5e28b78, ranges 75-103, 154-166, 220-246, 247-299.

- **Repair method registry (`sutura_engine.methods`, v0.7.0).** Nineteen methods with
  stable user-facing numbers and canonical slugs (1 `quick_clean`, 2 `local_mend`,
  3 `full_mend`, 4 `join`, 5 `autorefine`, 6 `exact_refine`, 7 `ftetwild`,
  8 `balloon`, 9 `backplate`, 10 `scaffold`, 11 `transplant`,
  12 `transplant_plus`, 13 `graft`, 14 `mirror_complete`, 15 `wall_thicken`,
  16 `dressing`, 17 `tamponade`, 18 `splint`, 19 `coaptation`).
  Method #13 `graft` is the morphology shell-wrap tier (`sutura_engine/graft.py`);
  #16 `dressing` is the variable-viscosity volumetric-skinning tier
  (`sutura_engine/dressing.py`, opt-in); #17 `tamponade` is the on-by-default
  interior hole plug; #18 `splint` is the opt-in exact solid rebuild; #19
  `coaptation` is the on-by-default seam-weld union. #12
  `transplant_plus` retains `repeat_manual` and `transplant+` aliases.
  `--list-methods`/`--analyze`/`--methods` are the CLI
  surface; the GUI exposes the same via the file list's **Method** column and a
  right-click menu (*Analyze*, *Recommended methods*, *Use method*, *External
  engines*, *Clear tags*). `process_file` routes through
  `methods.repair_with_methods` (`methods=None` = auto: the default pipeline
  first, ranked fallbacks only when the baseline is not strict-watertight;
  `_auto_escalation_allowed` keeps `--deep-repair off|local` / no-fTetWild
  runs non-escalating). Geometry-inventing methods are guarded — #7
  output→input Hausdorff, #8/#9/#10/#14 input→output, #11/#12 self-guarded by
  `hausdorff_outside`, #15 input→output and excluded from the ranking
  (`needs_user_input`) so a thicken is always an explicit choice, #16
  self-guarded by its own coat→input fidelity gate and also excluded from the
  ranking (`needs_user_input`, opt-in until measured on the corpus) — and
  `_evaluate` judges every candidate on the
  reload-equivalent weld. `sutura_engine.diagnosis` (with the
  `object_analysis.py` + `templates.py` shims) drives
  `--analyze` and the ranking; third-party engines are listed separately and
  never mixed into it. New runtime dep: trimesh (~4.6 MB); SciPy is not required.

- **Repeated-element repair (`sutura/repeat_repair.py` + `sutura_geom.boolean_transplant`,
  v0.6.0).** Methods #11/#12. Detection is segmentation-free (surface sampling,
  PCA + canonical axes, rotational orders 2–64 + knurl counts 72–120, 2D/3D
  translational lattices, multi-step verification, fundamental-order
  de-aliasing); `align` supports similarity (scale) and mirror; `extract_outer_shell`
  unions a multi-component assembly into one outer shell; missing material and
  bumps are both detected (forward/reverse deviation); the cavity cut is shrunk
  0.1 % so donor/cavity overlap (avoids the coplanar boundary that collapses to
  nm edges on float32 STL). The CSG transplant runs in the mandatory Rust
  core (`sutura_geom.boolean_transplant`); the Python `csg_bridge.py` and its
  Python 3.11 subprocess were removed. It is a deliberate geometry change,
  so it stays behind the method flags; adopt-only-if-watertight + no-worse +
  `hausdorff_outside`.

### Method naming

Each built-in method module is named after its user-facing slug; `num` (1-19), the old
slug and the new slug are all accepted by `get_method` / `--methods` / saved GUI tags.
`display_name` is what the GUI and CLI print.

| # | module | slug (`id`) | display name | accepted aliases |
|---|--------|-------------|--------------|------------------|
| 1 | `quick_clean.py` | `quick_clean` | Quick Clean | `fast`, `quick`, `m01`, `1` |
| 2 | `local_mend.py` | `local_mend` | Local Mend | `deep_local`, `local`, `m02`, `2` |
| 3 | `full_mend.py` | `full_mend` | Full Mend | `deep_full`, `full`, `deep`, `m03`, `3` |
| 4 | `join.py` | `join` | Join | `join_components`, `m04`, `4` |
| 5 | `autorefine.py` | `autorefine` | Autorefine | `m05`, `5` |
| 6 | `exact_refine.py` | `exact_refine` | Exact Refine | `indirect_autorefine`, `exact`, `m06`, `6` |
| 7 | `ftetwild.py` | `ftetwild` | fTetWild | `m07`, `7` |
| 8 | `balloon.py` | `balloon` | Balloon | `poisson_close`, `poisson`, `m08`, `8` |
| 9 | `backplate.py` | `backplate` | Backplate | `flat_back_close`, `flat_back`, `relief`, `m09`, `9` |
| 10 | `scaffold.py` | `scaffold` | Scaffold | `proxy_template`, `proxy`, `m10`, `10` |
| 11 | `transplant.py` | `transplant` | Transplant | `repeat_auto`, `repeat`, `m11`, `11` |
| 12 | `transplant.py` | `transplant_plus` | Transplant+ | `repeat_manual`, `transplant+`, `m12`, `12` |
| 13 | `graft.py` | `graft` | Graft | `shell_wrap`, `wrap`, `m13`, `13` |
| 14 | `mirror_complete.py` | `mirror_complete` | Mirror Complete | `mirror`, `m14`, `14` |
| 15 | `wall_thicken.py` | `wall_thicken` | Wall Thicken | `wall`, `thicken`, `m15`, `15` |
| 16 | `dressing.py` | `dressing` | Dressing | `coat`, `dipcoat`, `m16`, `16` |
| 17 | `tamponade.py` | `tamponade` | Tamponade | `lava`, `m17`, `17` |
| 18 | `splint.py` | `splint` | Splint | `m18`, `18` |
| 19 | `coaptation.py` | `coaptation` | Coaptation | `seam_weld`, `coapt`, `m19`, `19` |

## How to add a repair method

Sutura's repair engine is designed to be easily extensible. Any new repair method can be added without modifying the core pipeline or triage logic.

### 1. Implement `RepairMethodProtocol`
A repair method is a callable class or object conforming to `RepairMethodProtocol` (defined in `sutura_engine.methods.protocol`):
```python
from dataclasses import dataclass, field
from typing import Optional, Tuple
import numpy as np
from sutura_engine.methods.protocol import (
    RepairContext, MethodResult, RepairMethod
)

@dataclass(frozen=True)
class MyCustomMethod(RepairMethod):
    num: int = 16
    id: str = "my_custom"
    name: str = "My Custom"
    display_name: str = "My Custom Repair"
    description: str = "Repairs custom defects using specialised algorithms."
    family: str = "custom"
    invents_geometry: bool = False

    def available(self) -> Tuple[bool, Optional[str]]:
        """Return (is_available, failure_reason)."""
        return True, None

    def repair(self, ctx: RepairContext) -> MethodResult:
        """Execute repair on ctx.verts, ctx.tris and return MethodResult."""
        repaired_v, repaired_t = my_algorithm(ctx.verts, ctx.tris)
        return MethodResult(
            ok=True,
            verts=repaired_v,
            tris=repaired_t,
            report={"custom_metric": 42},
        )
```

### 2. Auto-Discovery via `builtin/` or Manual Registration
- **Built-in method:** Place your module as `sutura_engine/methods/builtin/<slug>.py`. Export an instance named `METHOD` (or `METHODS` for two variants; or a subclass of `RepairMethod`). When `sutura_engine.methods` initializes, `discover_builtin_methods()` scans the `builtin/` package and registers any discovered methods automatically. Name the module after its user-facing slug and set a `display_name`.
- **Dynamic / Plugin registration:** Register at runtime using:
  ```python
  from sutura_engine.methods import register_method
  register_method(MyCustomMethod())
  ```
- **Custom Templates:** Custom geometric templates can also be registered pluggably via:
  ```python
  from sutura_engine.diagnosis import register_template
  register_template("my_template", my_detection_function)
  ```
The new method immediately participates in `--list-methods`, `--analyze` recommendations, GUI method column selection, and budget-governed triage ranking.

