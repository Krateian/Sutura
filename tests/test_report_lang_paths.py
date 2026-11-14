#!/usr/bin/env python3
"""TR localisation path-safety regression (P10 lead review C2).

Values may be translated inside labelled lines; paths and file names must not
be rewritten by the broad word replacements.
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
sys.path.insert(0, SUTURA)

from report_lang import translate  # noqa: E402


def test_path_values_are_not_translated():
    text = ('Input : /models/High volume  kept/confidence/home.stl\n'
            'Output: /tmp/High/Low/kept.obj\n')
    out = translate(text)
    assert '/models/High volume  kept/confidence/home.stl' in out, out
    assert '/tmp/High/Low/kept.obj' in out, out


def test_labelled_values_still_translate():
    text = ('Confidence: High\n'
            'Budget: within budget\n'
            '  - balloon: adopted\n')
    out = translate(text)
    assert 'Güven: Yüksek' in out, out
    assert 'Bütçe: bütçe içinde' in out, out
    assert 'benimsendi' in out, out


if __name__ == '__main__':
    test_path_values_are_not_translated()
    test_labelled_values_still_translate()
    print('report_lang path safety passed')
