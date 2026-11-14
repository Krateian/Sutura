#!/usr/bin/env python3
"""Unit tests for ftetwild_bridge command generation and parameter mappings.

Validates that build_binary_cmd strictly aligns with fTetWild CLI flags:
  -a, --la: absolute ideal edge length
  -l, --lr: relative ideal edge length
  -e, --epsr: relative envelope epsilon
  --max-its: maximum optimization iterations
  --skip-simplify: skip preprocessing
  --max-threads: single-threaded determinism (1)
"""
import os
import sys
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUTURA = os.path.join(REPO, 'sutura')
if SUTURA not in sys.path:
    sys.path.insert(0, SUTURA)

from ftetwild_bridge import build_binary_cmd, tetrahedralize_params


class TestFTetWildBridgeCmd(unittest.TestCase):
    def setUp(self):
        self.bin = '/mock/bin/ftetwild'
        self.src = '/mock/input.obj'
        self.dst = '/mock/output.msh'

    def test_default_command(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst)
        self.assertEqual(cmd[0], self.bin)
        self.assertIn('-i', cmd)
        self.assertIn(os.path.abspath(self.src), cmd)
        self.assertIn('-o', cmd)
        self.assertIn(os.path.abspath(self.dst), cmd)
        self.assertIn('--max-threads', cmd)
        self.assertEqual(cmd[cmd.index('--max-threads') + 1], '1')
        self.assertIn('-q', cmd)
        self.assertIn('--no-color', cmd)

    def test_optimize_false_maps_to_max_its_0(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst, {'optimize': False})
        self.assertIn('--max-its', cmd)
        self.assertEqual(cmd[cmd.index('--max-its') + 1], '0')

    def test_explicit_max_its(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst, {'max_its': 12})
        self.assertIn('--max-its', cmd)
        self.assertEqual(cmd[cmd.index('--max-its') + 1], '12')

    def test_simplify_false_maps_to_skip_simplify(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst, {'simplify': False})
        self.assertIn('--skip-simplify', cmd)

    def test_skip_simplify_true(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst, {'skip_simplify': True})
        self.assertIn('--skip-simplify', cmd)

    def test_stop_energy(self):
        cmd = build_binary_cmd(self.bin, self.src, self.dst, {'stop_energy': 15.5})
        self.assertIn('--stop-energy', cmd)
        self.assertEqual(cmd[cmd.index('--stop-energy') + 1], '15.5')

    def test_absolute_edge_length_maps_to_flag_a(self):
        # ideal_edge_length / edge_length_abs / la must map to -a, NEVER -e
        cmd1 = build_binary_cmd(self.bin, self.src, self.dst, {'edge_length_abs': 0.75})
        self.assertIn('-a', cmd1)
        self.assertEqual(cmd1[cmd1.index('-a') + 1], '0.75')
        self.assertNotIn('-e', cmd1)

        cmd2 = build_binary_cmd(self.bin, self.src, self.dst, {'ideal_edge_length': 1.25})
        self.assertIn('-a', cmd2)
        self.assertEqual(cmd2[cmd2.index('-a') + 1], '1.25')
        self.assertNotIn('-e', cmd2)

        cmd3 = build_binary_cmd(self.bin, self.src, self.dst, {'la': 0.5})
        self.assertIn('-a', cmd3)
        self.assertEqual(cmd3[cmd3.index('-a') + 1], '0.5')
        self.assertNotIn('-e', cmd3)

    def test_relative_edge_length_maps_to_flag_l(self):
        # lr / edge_length_fac must map to -l
        cmd1 = build_binary_cmd(self.bin, self.src, self.dst, {'lr': 0.08})
        self.assertIn('-l', cmd1)
        self.assertEqual(cmd1[cmd1.index('-l') + 1], '0.08')

        cmd2 = build_binary_cmd(self.bin, self.src, self.dst, {'edge_length_fac': 0.04})
        self.assertIn('-l', cmd2)
        self.assertEqual(cmd2[cmd2.index('-l') + 1], '0.04')

    def test_epsilon_maps_to_flag_e(self):
        # epsilon / epsr must map to -e (relative envelope epsilon), NEVER --eps
        cmd1 = build_binary_cmd(self.bin, self.src, self.dst, {'epsilon': 0.002})
        self.assertIn('-e', cmd1)
        self.assertEqual(cmd1[cmd1.index('-e') + 1], '0.002')
        self.assertNotIn('--eps', cmd1)

        cmd2 = build_binary_cmd(self.bin, self.src, self.dst, {'epsr': 0.005})
        self.assertIn('-e', cmd2)
        self.assertEqual(cmd2[cmd2.index('-e') + 1], '0.005')
        self.assertNotIn('--eps', cmd2)

    def test_combined_parameters(self):
        params = {
            'optimize': False,
            'simplify': False,
            'stop_energy': 20.0,
            'edge_length_abs': 0.5,
            'epsilon': 0.001,
        }
        cmd = build_binary_cmd(self.bin, self.src, self.dst, params)
        self.assertIn('--max-its', cmd)
        self.assertEqual(cmd[cmd.index('--max-its') + 1], '0')
        self.assertIn('--skip-simplify', cmd)
        self.assertIn('--stop-energy', cmd)
        self.assertEqual(cmd[cmd.index('--stop-energy') + 1], '20.0')
        self.assertIn('-a', cmd)
        self.assertEqual(cmd[cmd.index('-a') + 1], '0.5')
        self.assertIn('-e', cmd)
        self.assertEqual(cmd[cmd.index('-e') + 1], '0.001')
        self.assertNotIn('--eps', cmd)


if __name__ == '__main__':
    unittest.main()
