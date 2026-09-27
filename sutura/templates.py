"""Built-in object templates for Sutura's method-recommendation engine (P0).

A template is a coarse, readable object profile ("this looks like a mechanical
part / a single-sided scan / ...") that combines with a method's own score to
drive ``methods.rank_methods``. It is NOT a classifier replacement: the mesh
type still comes from ``mesh_classifier_v2`` and the template only adds the
shape-of-damage signal (open area, boundary-loop size, self-intersection load,
component count).

Every threshold is a named constant so the heuristics can be tuned without
touching the logic. Templates are pure functions of an
``object_analysis.ObjectAnalysis`` (duck typed: any object exposing the fields
works), stdlib-only, no numpy needed.
"""

# Thresholds (documented, deliberately simple).
OPEN_AREA_SCAN = 0.02      # >2% of the surface missing reads as "open"
OPEN_AREA_RELIEF = 0.08    # full-confidence open fraction
LARGE_LOOP_RATIO = 0.25    # a boundary loop spanning >25% of the bbox diagonal
MANY_LOOPS = 4             # more boundary loops than this stops looking like a plate/scan
SI_MODERATE = 20           # self-intersecting faces worth the SI methods
SI_HEAVY = 200             # self-intersection load of a dense scan


def _a(analysis, name, default=0.0):
    value = getattr(analysis, name, default)
    return default if value is None else value


def _clamp01(value):
    try:
        value = float(value)
    except (TypeError, ValueError):
        return 0.0
    return max(0.0, min(1.0, value))


class Template:
    """One object profile: an id, a preferred method order and a confidence."""

    __slots__ = ('id', 'name', 'preferred', '_confidence_fn')

    def __init__(self, id, name, preferred, confidence_fn):
        self.id = id
        self.name = name
        self.preferred = tuple(int(x) for x in preferred)
        self._confidence_fn = confidence_fn

    def confidence(self, analysis):
        """Confidence (0..1) that this profile fits the object."""
        try:
            return _clamp01(self._confidence_fn(analysis))
        except Exception:  # noqa: BLE001 - a template never crashes a caller
            return 0.0


def _mechanical(a):
    if _a(a, 'mesh_type') == 'mechanical':
        return _clamp01(_a(a, 'type_confidence'))
    return 0.0


def _organic(a):
    if _a(a, 'mesh_type') == 'organic':
        return _clamp01(_a(a, 'type_confidence'))
    return 0.0


def _single_side_scan(a):
    open_ratio = _a(a, 'open_area_ratio')
    loops = _a(a, 'boundary_loops')
    largest = _a(a, 'largest_loop_ratio')
    if open_ratio < OPEN_AREA_SCAN or loops > MANY_LOOPS:
        return 0.0
    score = min(1.0, open_ratio / OPEN_AREA_RELIEF)
    if largest >= LARGE_LOOP_RATIO:
        score = min(1.0, score + 0.2)
    return score


def _relief(a):
    open_ratio = _a(a, 'open_area_ratio')
    loops = _a(a, 'boundary_loops')
    if open_ratio < OPEN_AREA_SCAN or loops > 2:
        return 0.0
    return min(1.0, open_ratio / OPEN_AREA_RELIEF)


def _repeated_pattern(a):
    # repetition_score is a P5 stub today (always 0.0), so this template stays
    # quiet until the repetition detector lands.
    return _clamp01(_a(a, 'repetition_score'))


def _dense_scan_heavy_si(a):
    si = _a(a, 'self_intersections')
    if si < SI_HEAVY:
        return 0.0
    return min(1.0, si / (4.0 * SI_HEAVY))


TEMPLATES = (
    Template('mechanical', 'Mechanical part', (1, 3, 4, 5, 2), _mechanical),
    Template('organic', 'Organic surface', (1, 2, 3, 7, 5), _organic),
    Template('single_side_scan', 'Single-sided scan', (9, 7, 3, 2),
             _single_side_scan),
    Template('relief', 'Relief / shell', (9, 10, 3, 8), _relief),
    Template('repeated_pattern', 'Repeated pattern', (11, 12, 1),
             _repeated_pattern),
    Template('dense_scan_heavy_si', 'Dense scan / heavy self-intersections',
             (5, 6, 7, 3), _dense_scan_heavy_si),
)
SCORE_TEMPLATES = TEMPLATES


def by_id(template_id):
    for template in TEMPLATES:
        if template.id == template_id:
            return template
    return None
