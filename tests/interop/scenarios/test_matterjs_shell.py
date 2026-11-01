"""The matter.js shell joins the same device as a second administrator and toggles it."""

from __future__ import annotations

import logging

import pytest

from conftest import CommissionedDut
from harness.chiptool import ChipTool
from harness.config import HarnessConfig
from harness.redact import Redactor
from harness.shell import MatterShell

pytestmark = [pytest.mark.docker, pytest.mark.timeout(600)]

SHELL_NODE_ID = 2


def test_matterjs_shell_commission_and_toggle(
    commissioned_dut: CommissionedDut,
    chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    node, endpoint = commissioned_dut.node_id, commissioned_dut.endpoint

    # The first admin (chip-tool) opens an enhanced commissioning window; its fresh pairing code
    # goes to the second admin without ever being logged.
    code = chiptool.open_commissioning_window(node, (config.discriminator + 1) & 0xFFF)

    shell = MatterShell(config, redactor, harness_logger, config.logs_dir / "matterjs-shell")
    try:
        shell.start()
        shell.pair(SHELL_NODE_ID, code)
        before = shell.read_on_off(SHELL_NODE_ID, endpoint)
        shell.toggle(SHELL_NODE_ID, endpoint)
        after = shell.read_on_off(SHELL_NODE_ID, endpoint)
    finally:
        shell.close()

    assert after is (not before), f"OnOff did not change on toggle via the shell: before={before}, after={after}"
    # Both fabrics see the same device state.
    assert chiptool.read_on_off(node, endpoint) is after, "chip-tool reads a different OnOff than the shell wrote"
