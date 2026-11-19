import contextlib
import io
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, ROOT)
sys.path.insert(0, os.path.join(ROOT, 'sutura'))

from sutura_engine import backend  # noqa: E402


class BackendDeprecationTest(unittest.TestCase):
    def setUp(self):
        backend._NOTE_PRINTED = False
        self.old = os.environ.pop(backend.BACKEND_ENV, None)

    def tearDown(self):
        if self.old is None:
            os.environ.pop(backend.BACKEND_ENV, None)
        else:
            os.environ[backend.BACKEND_ENV] = self.old

    def _run(self, value):
        err = io.StringIO()
        out = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(out):
            backend.note_deprecated(value)
            backend.note_deprecated(value)
        return err.getvalue(), out.getvalue()

    def test_python_prints_once_on_stderr(self):
        err, out = self._run('python')
        self.assertEqual(err.count('deprecated'), 1, err)
        self.assertIn('removed in the next release', err)
        self.assertEqual(out, '')

    def test_rust_is_silent(self):
        err, out = self._run('rust')
        self.assertEqual(err, '')
        self.assertEqual(out, '')


if __name__ == '__main__':
    unittest.main()
