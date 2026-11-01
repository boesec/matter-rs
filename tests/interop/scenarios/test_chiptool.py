"""chip-tool commissions the device under test and toggles its On/Off cluster."""

from __future__ import annotations

import pytest

from conftest import CommissionedDut
from harness.chiptool import ChipTool

pytestmark = [pytest.mark.docker, pytest.mark.timeout(360)]


def test_chiptool_commission_and_toggle(commissioned_dut: CommissionedDut, chiptool: ChipTool) -> None:
    node, endpoint = commissioned_dut.node_id, commissioned_dut.endpoint

    before = chiptool.read_on_off(node, endpoint)
    chiptool.toggle(node, endpoint)
    after = chiptool.read_on_off(node, endpoint)

    assert after is (not before), f"OnOff did not change on toggle: before={before}, after={after}"
