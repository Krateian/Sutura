#!/usr/bin/env python3
"""Sutura - small GUI frontend.

Pick a mesh file, run the two-stage repair, and read back what was
fixed. The result is always written to a new "_fixed" file.
"""
import tkinter as tk
from tkinter import filedialog, ttk
import subprocess
import threading
import json
import os

SUTURA = os.path.expanduser('~/.local/bin/sutura')


def run_repair(path):
    r = subprocess.run([SUTURA, path], capture_output=True, text=True, timeout=600)
    try:
        return json.loads(r.stdout.strip().splitlines()[-1])
    except Exception:
        return {'error': r.stderr.strip() or r.stdout.strip()}


def format_report(data):
    if data.get('error') and 'stage1' not in data:
        return 'ERROR: %s' % data['error']
    s1 = data.get('stage1', {})
    lines = []
    lines.append('Output file: %s' % data.get('output'))
    lines.append('')
    lines.append('Stage 1 (MeshLab):')
    lines.append('  Holes closed              : %d' % s1.get('holes_closed', 0))
    lines.append('  Holes remaining           : %d' % s1.get('holes_remaining', 0))
    lines.append('  Non-manifold edges fixed  : %d' % s1.get('non_manifold_edges_fixed', 0))
    lines.append('  Faces removed             : %d' % s1.get('faces_removed', 0))
    lines.append('  Connected components      : %d' % s1.get('components', 0))
    lines.append('  Two-manifold              : %s' % ('YES' if s1.get('two_manifold') else 'NO'))
    if 'stage2' in data:
        s2 = data['stage2']
        lines.append('')
        lines.append('Stage 2 (Manifold):')
        if 'error' in s2:
            lines.append('  ERROR: %s' % s2['error'])
        else:
            lines.append('  Input triangles : %d' % s2.get('input_faces', 0))
            lines.append('  Output triangles: %d' % s2.get('output_faces', 0))
            if s2.get('shells_merged'):
                lines.append('  Shells merged   : %d' % s2['shells_merged'])
            lines.append('  Volume (after)  : %.4f' % s2.get('volume_after', 0))
    if 'objects' in data:
        lines.append('')
        lines.append('3MF objects repaired: %d' % data.get('objects', 0))
        for i, rep in enumerate(data.get('object_reports', [])):
            s1o = rep.get('stage1', {})
            ok = s1o.get('two_manifold') and s1o.get('holes_remaining', 0) == 0
            lines.append('  object %d: %s (%d hole(s) remaining, two-manifold=%s)' % (
                i, 'watertight' if ok else 'partial',
                s1o.get('holes_remaining', 0), 'YES' if s1o.get('two_manifold') else 'NO'))
    return '\n'.join(lines)


class App:
    def __init__(self, root):
        self.root = root
        root.title('Sutura')
        root.geometry('640x520')
        root.minsize(560, 420)

        pad = {'padx': 10, 'pady': 6}

        top = ttk.Frame(root)
        top.pack(fill='x', **pad)

        ttk.Label(top, text='File:').pack(side='left')
        self.path_var = tk.StringVar()
        ttk.Entry(top, textvariable=self.path_var, width=52).pack(side='left', padx=6)
        ttk.Button(top, text='Browse…', command=self.browse).pack(side='left')

        mid = ttk.Frame(root)
        mid.pack(fill='x', **pad)
        self.repair_btn = ttk.Button(mid, text='Repair', command=self.repair, state='disabled')
        self.repair_btn.pack(side='left')
        self.progress = ttk.Progressbar(mid, mode='indeterminate', length=180)
        self.status = tk.StringVar(value='Ready')

        ttk.Label(root, textvariable=self.status).pack(fill='x', **pad)

        body = ttk.Frame(root)
        body.pack(fill='both', expand=True, **pad)
        self.text = tk.Text(body, wrap='word', height=18)
        self.text.pack(fill='both', expand=True)
        scroll = ttk.Scrollbar(self.text, command=self.text.yview)
        self.text.configure(yscrollcommand=scroll.set)

        self.path_var.trace_add('write', lambda *_: self.refresh_state())

    def refresh_state(self):
        self.repair_btn.config(state='normal' if self.path_var.get().strip() else 'disabled')

    def browse(self):
        p = filedialog.askopenfilename(
            title='Select a mesh file',
            filetypes=[('Mesh files', '*.stl *.STL *.3mf *.3MF'), ('All files', '*.*')],
        )
        if p:
            self.path_var.set(p)

    def repair(self):
        path = self.path_var.get().strip()
        if not path or not os.path.exists(path):
            self.status.set('File not found')
            return
        self.repair_btn.config(state='disabled')
        self.progress.pack(side='left', padx=10)
        self.progress.start(12)
        self.status.set('Repairing…')
        threading.Thread(target=self._work, args=(path,), daemon=True).start()

    def _work(self, path):
        data = run_repair(path)
        self.root.after(0, lambda: self._done(data))

    def _done(self, data):
        self.progress.stop()
        self.progress.pack_forget()
        self.repair_btn.config(state='normal')
        self.text.delete('1.0', 'end')
        self.text.insert('1.0', format_report(data))
        self.status.set('Done' if 'error' not in data or 'stage1' in data else 'Error')


if __name__ == '__main__':
    import sys
    root = tk.Tk()
    app = App(root)
    if len(sys.argv) > 1:
        app.path_var.set(os.path.abspath(sys.argv[1]))
    root.mainloop()