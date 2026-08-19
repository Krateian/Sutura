# /// script
# requires-python = ">=3.12"
# dependencies = []
#
# [tool.orcaslicer.plugin]
# name = "Sutura Repair"
# description = "EXPERIMENTAL: runs the Sutura CLI on a mesh file in a background thread and shows the result."
# author = "Krateian"
# version = "0.1.0"
# ///
"""Sutura Repair — OrcaSlicer script plugin.

Calls the installed Sutura CLI (`~/.local/bin/sutura`) as a subprocess from a
background thread and reports the result through the host UI. It deliberately
does NOT bundle pymeshlab/manifold3d into OrcaSlicer's embedded Python — it
just shells out to the separately-installed CLI.

EXPERIMENTAL — untested in a real OrcaSlicer. The plugin system needs a build
newer than 2.4.2; the author has not run this in a real instance, only
validated logic/API usage against the documented API. See the README for the
full notes.

Limitations:
  * Does NOT repair the currently selected model — `execute()` takes no
    arguments and there is no API for the selection; this repairs a fixed /
    configured target (SUTURA_TARGET env).
  * Linux only: depends on the install.sh CLI path (~/.local/bin/sutura).
    No Windows support; macOS uses a different layout. If made cross-platform,
    the SUTURA_CLI default should detect the path per platform.
"""

import os
import subprocess
import threading

import orca

# The Sutura CLI. Use the absolute path: a GUI-launched app may not have
# ~/.local/bin on PATH. Overridable via the SUTURA_CLI env var.
SUTURA_CLI = os.environ.get('SUTURA_CLI', os.path.expanduser('~/.local/bin/sutura'))

# Target mesh file to repair. Script `execute()` takes no arguments and there
# is no documented per-selection file API yet, so for this prototype the file
# is configured here (or via the SUTURA_TARGET env var). This is the main
# open question that needs a real OrcaSlicer to answer.
TARGET = os.environ.get('SUTURA_TARGET', '')


class SuturaRepair(orca.script.ScriptPluginCapabilityBase):
    def get_name(self):
        return "Repair mesh with Sutura"

    def _worker(self, target):
        """Run Sutura in a background thread and report via the host UI.

        Runs entirely off the UI thread so a long repair does not freeze the
        slicer. The child process is a separate process, so its writes (the
        `_fixed` output next to the input) bypass OrcaSlicer's audit hook."""
        try:
            if not os.path.exists(target):
                orca.host.ui.message(
                    "Sutura Repair: target file not found:\n%s" % target,
                    title="Sutura Repair", buttons="ok", icon="error")
                return
            proc = subprocess.run(
                [SUTURA_CLI, target, '--human'],
                capture_output=True, text=True, timeout=300)
            output = (proc.stdout or '') + ('\n' + proc.stderr if proc.stderr else '')
            if proc.returncode == 0:
                fixed = target.rsplit('.', 1)
                fixed_path = fixed[0] + '_fixed' + ('.' + fixed[1] if len(fixed) > 1 else '')
                orca.host.ui.message(
                    "Sutura repaired:\n%s\n\nOutput written to:\n%s\n\n%s"
                    % (target, fixed_path, output),
                    title="Sutura Repair", buttons="ok", icon="info")
            else:
                orca.host.ui.message(
                    "Sutura failed (exit %d):\n%s\n\n%s"
                    % (proc.returncode, target, output),
                    title="Sutura Repair", buttons="ok", icon="error")
        except subprocess.TimeoutExpired:
            orca.host.ui.message(
                "Sutura Repair timed out on:\n%s" % target,
                title="Sutura Repair", buttons="ok", icon="error")
        except FileNotFoundError:
            orca.host.ui.message(
                "Sutura CLI not found at:\n%s\nInstall it with install.sh." % SUTURA_CLI,
                title="Sutura Repair", buttons="ok", icon="error")
        except Exception as exc:  # never crash the worker silently
            orca.host.ui.message(
                "Sutura Repair error:\n%s" % exc,
                title="Sutura Repair", buttons="ok", icon="error")

    def execute(self):
        # `execute()` runs on the main/UI thread; a slow call freezes the UI.
        # Return immediately and do the work in a daemon thread, which reports
        # back via orca.host.ui (marshals to the main thread).
        target = TARGET or os.path.expanduser('~/.local/share/sutura/sample.stl')
        threading.Thread(target=self._worker, args=(target,), daemon=True).start()
        return orca.ExecutionResult.success(
            "Sutura Repair started on: %s" % target)


@orca.plugin
class SuturaRepairPlugin(orca.base):
    def register_capabilities(self):
        orca.register_capability(SuturaRepair)
