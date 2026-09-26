# Sutura External Repair Engines

Sutura supports integrating third-party repair engines installed by the user. This allows incorporating external meshing or repair tools into Sutura's repair pipeline.

> [!IMPORTANT]
> **Sutura never downloads engines.** Users install and manage external binaries themselves.
> **Safety invariant:** Every mesh produced by an external engine is strictly validated by Sutura's core watertightness checks and Hausdorff surface-deviation guard in `repair.py`. External engines cannot disable or bypass these safety checks.

---

## Configuration Location

Engine definitions are stored as individual TOML files in:

```text
${XDG_CONFIG_HOME:-~/.config}/sutura/engines/*.toml
```

- Each file defines one engine.
- Engine names must be unique. If two files specify the same `name`, the duplicate is skipped with a warning.
- Malformed TOML files or configurations with missing required fields are skipped with a warning and will never crash Sutura.

---

## Engine Configuration Schema

Each engine TOML file supports the following fields:

| Field | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `name` | string | *(required)* | Unique identifier for the engine. |
| `command` | list[string] | *(required)* | Command argv list. Must contain `{input}` and `{output}` placeholders. May optionally contain `{tmpdir}`. |
| `input_format` | string | `"stl"` | Input mesh format (`"stl"`, `"obj"`, `"ply"`). |
| `output_format`| string | `"stl"` | Output mesh format (`"stl"`, `"obj"`, `"ply"`). |
| `timeout` | float | `120.0` | Maximum execution time in seconds. Subprocess group is terminated if exceeded. |
| `placement` | string | `"after_stage1"` | Pipeline insertion point: `before_stage1`, `after_stage1`, `after_stage2`, `replace_ftetwild`, or `final_fallback`. |
| `enabled` | boolean | `true` | Whether the engine is active. |
| `cwd` | string | `null` | Optional working directory for the process. |
| `env` | table | `{}` | Optional dictionary of environment variables passed to the process. |

### Placeholder Tokens

- `{input}`: Substituted with the path to the input mesh file.
- `{output}`: Substituted with the path to the expected output mesh file.
- `{tmpdir}`: (Optional) Substituted with an isolated temporary directory created for this run and cleaned up afterwards.

Unknown `{...}` placeholder tokens are rejected during configuration loading.

---

## Placements in the Default Chain

When no custom `chain.toml` is present, enabled engines are slotted into Sutura's default repair sequence according to their `placement`:

1. `before_stage1`: Runs before PyMeshLab VCG filter chain.
2. **`stage1`**: PyMeshLab VCG filter chain (built-in).
3. `after_stage1`: Runs after Stage 1, before Stage 2 manifold rebuild.
4. **`stage2`**: Manifold3d solid rebuild (built-in).
5. `after_stage2`: Runs after Stage 2.
6. **`deep_repair`**: Built-in deep repair ladder.
7. `replace_ftetwild`: Replaces fTetWild fallback; if none registered, built-in **`ftetwild`** runs.
8. `final_fallback`: Runs as the final fallback tier.

---

## Custom Chain Configuration (`chain.toml`)

Users can define an explicit pipeline order by creating `chain.toml` in the engines directory:

```toml
# ~/.config/sutura/engines/chain.toml
stages = [
    "stage1",
    "meshfix",
    "stage2",
    "ftetwild"
]
```

### Built-in Stage Names
- `stage1`
- `stage2`
- `deep_repair`
- `ftetwild`

> [!WARNING]
> If `chain.toml` contains any unknown stage or engine name, or if it is malformed, the entire custom chain is rejected with a warning, and Sutura automatically falls back to the default chain.

---

## Example Configurations

### Example 1: MeshFix CLI Engine

`~/.config/sutura/engines/meshfix.toml`:
```toml
name = "meshfix"
command = ["meshfix", "{input}", "{output}", "-u", "20"]
input_format = "stl"
output_format = "stl"
timeout = 60.0
placement = "after_stage1"
enabled = true

[env]
OMP_NUM_THREADS = "4"
```

### Example 2: Custom Python Tool with Temporary Working Directory

`~/.config/sutura/engines/custom_tool.toml`:
```toml
name = "custom_tool"
command = [
    "python3",
    "/usr/local/bin/repair_script.py",
    "--in", "{input}",
    "--out", "{output}",
    "--scratch", "{tmpdir}"
]
input_format = "stl"
output_format = "obj"
timeout = 90.0
placement = "before_stage1"
enabled = true
```

---

## Execution Security & Process Isolation

- **No shell expansion:** Commands are executed as argument lists via `subprocess.Popen` with `shell=False`.
- **Process group isolation:** Commands run with `start_new_session=True`. On timeout, the entire process group is sent `SIGKILL` to prevent orphaned child processes.
- **Fail-safe outputs:** Missing or empty output files result in a structured failure report; an external failure never raises an unhandled exception or interrupts batch repairs.
