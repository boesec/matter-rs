"""One connectedhomeip Python test case runs through its framework against the device under test."""

from __future__ import annotations

import logging
from pathlib import Path

import pytest

from conftest import CommissionedDut
from harness.chiptool import ChipTool
from harness.config import INTEROP_ROOT, HarnessConfig
from harness.pythontc import PythonTestRunner
from harness.redact import Redactor

pytestmark = [pytest.mark.docker, pytest.mark.timeout(720)]

# General Commissioning attributes: a short test without PIXITs that still commissions the
# device through the framework and exercises reads and a write.
TEST_SCRIPT = "TC_CGEN_2_1.py"
TEST_CASE = "test_TC_CGEN_2_1"
PICS_FILE = INTEROP_ROOT / "pics" / "matterjs-onoff.pics"


def test_python_tc_smoke(
    commissioned_dut: CommissionedDut,
    chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    assert PICS_FILE.is_file(), f"PICS file missing: {PICS_FILE}"
    node = commissioned_dut.node_id

    code = chiptool.open_commissioning_window(node, (config.discriminator + 2) & 0xFFF)

    runner = PythonTestRunner(config, redactor, harness_logger, config.logs_dir / "python-tc")
    process, result = runner.run(TEST_SCRIPT, code, Path(PICS_FILE))

    assert result.failed == 0 and result.errors == 0, result.describe()
    assert result.record(TEST_CASE).result == "PASS", result.describe()
    assert result.passed >= 1, result.describe()
    assert process.returncode == 0, f"framework exit code {process.returncode}\n{process.tail()}"
