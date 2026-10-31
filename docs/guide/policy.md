# Policy file

[← Back to README](../../README.md) · [Türkçe](tr/policy.md)

Sutura reads its repair decision rules from one versioned policy file that
ships inside the package. A user file can override the tunable values; the
safety gates cannot be changed.

## Where the files are

- Built-in policy: `sutura/sutura_engine/policy/default_policy.toml`, shipped
  inside the package (there is no path to edit).
- User override: `~/.config/sutura/policy.toml` (create it if it does not
  exist).

The user file is read once at startup and merged over the built-in policy. A
change takes effect on the next run.

## What can be changed

Only data-like values are tunable:

- `[intensity]` — the built-in intensity presets and the default preset.
- `[modes]`, `[stage1_profiles]`, `[stage1_types]`, `[stage1_tuning]` — the
  Stage-1 fixed modes, the named Stage-1 profiles, the mesh-type thresholds and
  the classifier-confidence tuning gates.
- `[ranking]`, `[templates]` — ranking knobs (preference boost, top-N, learning
  priors) and the score-template thresholds and preferred method orders.
- `[tiers.*]` — tier budgets and knobs (flap, tamponade, repeat, splint,
  coaptation, dressing) and the tiers' default-enable switches.
- `[auto]`, `[order]` — the auto-escalation policy and the tier order.

## What cannot be changed

Everything under `[gates.*]`, every `[tiers.<tier>.gates]` table, the whole
`[si_escalation]` section (the auto-SI candidate and its never-worse
volume/distance tolerances) and `auto.generative_max_hausdorff_rel` is a
**locked safety gate**: the shape and never-worse bounds (the adoption guard,
the Tamponade shape and leak triggers, the Flap debris thresholds, the Dressing
adoption bounds, the Hausdorff sample floor and the decimation / fTetWild
margins). An override of a locked key is ignored with a warning; the effective
policy always keeps the built-in gate values.

Unknown keys and malformed files are ignored with a warning (a file that cannot
be parsed at all falls back to the built-in policy and the report records
`policy_error`). The report's `policy` block carries the schema version, a
content hash of the effective policy, whether a user override was applied and
the ignored keys — no filesystem paths.

## Resetting

Delete `~/.config/sutura/policy.toml` to return to the built-in policy.
