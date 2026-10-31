# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Lightweight CLI progress reporting and cooperative cancellation.

Progress is presentation only: it never touches geometry, work counters or
adoption decisions. Human progress is a single stderr line rewritten only when
stderr is a TTY; JSON mode uses opt-in NDJSON events.
"""
import json
import os
import sys
import threading
import time

STEP_TOTAL = 8


class CancelledError(Exception):
    """Raised at a stage boundary or a work-counter checkpoint."""


class CancelToken:
    def __init__(self):
        self._event = threading.Event()

    def cancel(self):
        self._event.set()

    def is_cancelled(self):
        return self._event.is_set()

    def raise_if_cancelled(self):
        if self._event.is_set():
            raise CancelledError('cancelled')


def raise_if_cancelled(cancel):
    if cancel is not None:
        cancel.raise_if_cancelled()


def _fmt_elapsed(seconds):
    return '%.1fs' % seconds


class _BaseProgress:
    def __init__(self, path, index, total):
        self.path = path
        self.index = index
        self.total = total
        self.t0 = time.perf_counter()

    def start_file(self):
        self.t0 = time.perf_counter()

    def stage(self, step, name, detail=None):
        raise NotImplementedError

    def end_file(self):
        pass

    def cancel(self):
        pass


class HumanProgress(_BaseProgress):
    """One line, rewritten in place; only when stderr is a TTY."""

    def __init__(self, path, index, total):
        super().__init__(path, index, total)
        self._tty = sys.stderr.isatty()
        self._wrote = False

    def stage(self, step, name, detail=None):
        if not self._tty:
            return
        base = os.path.basename(self.path)
        elapsed = _fmt_elapsed(time.perf_counter() - self.t0)
        text = '[%d/%d] %s · %d/%d %s · %s' % (
            self.index, self.total, base, step, STEP_TOTAL, name, elapsed)
        if detail:
            text += ' · %s' % detail
        sys.stderr.write('\r\033[K' + text)
        sys.stderr.flush()
        self._wrote = True

    def end_file(self):
        if self._tty and self._wrote:
            sys.stderr.write('\r\033[K')
            sys.stderr.flush()
            self._wrote = False


class JsonProgress(_BaseProgress):
    """Opt-in NDJSON events on stderr; stdout stays a single JSON document."""

    def _emit(self, event, **kw):
        payload = {
            'event': event,
            'file': os.path.basename(self.path),
            'file_index': self.index,
            'file_total': self.total,
            'step_total': STEP_TOTAL,
            'elapsed_s': round(time.perf_counter() - self.t0, 3),
        }
        payload.update(kw)
        print(json.dumps(payload, ensure_ascii=False), file=sys.stderr, flush=True)

    def start_file(self):
        super().start_file()
        self._emit('start')

    def stage(self, step, name, detail=None):
        payload = {'stage': name, 'step': step}
        if detail:
            payload['detail'] = detail
        self._emit('stage', **payload)

    def end_file(self):
        self._emit('end')

    def cancel(self):
        self._emit('cancel')


def make_progress(path, index, total, mode):
    if mode == 'human':
        return HumanProgress(path, index, total)
    if mode == 'json':
        return JsonProgress(path, index, total)
    return None


def progress_stage(progress, step, name, detail=None):
    if progress is not None:
        progress.stage(step, name, detail)
