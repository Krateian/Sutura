# Copyright (C) 2026 Sutura Authors
# SPDX-License-Identifier: Apache-2.0
"""Test extensible plugin architecture: RepairMethod protocol, registration, and templates."""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO)

import numpy as np
from sutura_engine.methods.protocol import (
    RepairContext,
    MethodResult,
    RepairMethod,
    RepairMethodProtocol,
)
from sutura_engine.methods import (
    all_methods,
    get_method,
    register_method,
    unregister_method,
)
from sutura_engine.analysis import (
    Template,
    register_template,
    all_templates,
    by_id,
)


def test_builtin_methods_conform_to_protocol():
    methods = all_methods()
    assert len(methods) >= 12
    for m in methods:
        assert isinstance(m, RepairMethodProtocol)
        ok, reason = m.available()
        assert isinstance(ok, bool)


def test_custom_plugin_registration():
    class CustomMethodPlugin:
        num = 999
        id = "custom_test"
        name = "Custom Test Plugin"
        display_name = "Custom Test Plugin"
        family = "custom"
        description = "A custom repair plugin"
        invents_geometry = False

        def available(self):
            return True, None

        def repair(self, ctx: RepairContext) -> MethodResult:
            return MethodResult(ok=True, verts=ctx.verts, tris=ctx.tris, report={"custom": True})

    plugin = CustomMethodPlugin()
    assert isinstance(plugin, RepairMethodProtocol)

    register_method(plugin)
    try:
        retrieved = get_method(999)
        assert retrieved is plugin
        assert retrieved in all_methods()
        assert retrieved.name == "Custom Test Plugin"

        # Test execution through repair protocol
        ctx = RepairContext(verts=np.zeros((3, 3)), tris=np.zeros((1, 3)))
        res = retrieved.repair(ctx)
        assert res.ok is True
        assert res.report.get("custom") is True
    finally:
        unregister_method(999)

    assert get_method(999) is None


def test_custom_template_registration():
    t_id = "test_custom_template"
    custom_tmpl = Template(
        id=t_id,
        name="Custom Test Template",
        preferred=(1, 5),
        confidence_fn=lambda a: 0.95,
    )
    register_template(custom_tmpl)
    retrieved = by_id(t_id)
    assert retrieved is not None
    assert retrieved.name == "Custom Test Template"
    assert retrieved.preferred == (1, 5)
    assert any(t.id == t_id for t in all_templates())


if __name__ == '__main__':
    test_builtin_methods_conform_to_protocol()
    print('ok test_builtin_methods_conform_to_protocol')
    test_custom_plugin_registration()
    print('ok test_custom_plugin_registration')
    test_custom_template_registration()
    print('ok test_custom_template_registration')
    print('all plugin tests passed')
