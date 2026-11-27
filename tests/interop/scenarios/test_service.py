"""The service binary as device under test: ``matter-rs run`` commissioned by chip-tool, stopped
with ``SIGTERM`` and restarted on the same state directory.

The fixtures are local to this module: the service gets its own port, discriminator, chip-tool
fabric and state directory next to the other devices under test. The binary is built here when
``INTEROP_SERVICE_DUT`` does not name one (``cargo build --release -p matter-rs``).
"""

from __future__ import annotations

import logging
import shutil
import tempfile
import tomllib
from pathlib import Path

import pytest

from harness.chiptool import ChipTool
from harness.config import HarnessConfig
from harness.docker import remove_volume
from harness.process import HarnessError
from harness.redact import Redactor
from harness.servicedut import (
    STOP_TIMEOUT,
    ServiceBuild,
    ServiceDut,
    build_service_binary,
    service_config_text,
    service_discriminator,
    service_port,
)

# The build of the binary runs inside the first scenario's setup.
pytestmark = [pytest.mark.docker, pytest.mark.timeout(1800)]

SERVICE_CHIPTOOL_NODE_ID = 21
AGGREGATOR = 1


@pytest.fixture(scope="module")
def service_build(config: HarnessConfig, harness_logger: logging.Logger, docker: bool) -> ServiceBuild:
    """The service binary, built if ``INTEROP_SERVICE_DUT`` does not name one."""
    try:
        build = build_service_binary(harness_logger)
    except HarnessError as error:
        if config.require_docker:
            pytest.fail(str(error))
        pytest.skip(str(error))
    if build.built:
        print(f"service DUT: release build of matter-rs took {build.seconds:.1f} s")
    return build


@pytest.fixture(scope="module")
def service_dut(config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, service_build: ServiceBuild):
    """``matter-rs run`` on a fresh configuration and state directory (outside ``logs/``)."""
    work_dir = Path(tempfile.mkdtemp(prefix="interop-service-"))
    dut = ServiceDut(
        service_build.binary,
        config,
        work_dir,
        config.logs_dir / "dut",
        redactor,
        harness_logger,
        port=service_port(),
        discriminator=service_discriminator(),
    ).start()
    yield dut
    dut.stop()
    shutil.rmtree(work_dir, ignore_errors=True)


@pytest.fixture(scope="module")
def service_chiptool(
    config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, service_dut: ServiceDut
):
    """A chip-tool with its own fabric for the service DUT, filtering on its discriminator."""
    tool = ChipTool(
        config,
        redactor,
        harness_logger,
        config.logs_dir / "service-chip-tool",
        component="service-chip-tool",
        discriminator=service_dut.discriminator,
    )
    yield tool
    remove_volume(tool.volume)


def test_service_commission_restart_keeps_fabric(service_dut: ServiceDut, service_chiptool: ChipTool) -> None:
    """chip-tool commissions the service; after SIGTERM and a restart it reads without commissioning again."""
    first = service_dut.running()
    first.expect_pattern(r"node is running .*commissioned=false")
    first.expect_pattern(r"matter-rs pairing-info")

    service_chiptool.pair(SERVICE_CHIPTOOL_NODE_ID)
    # The bridge runs without adapters: the aggregator lists no bridged device.
    assert service_chiptool.read_parts_list(SERVICE_CHIPTOOL_NODE_ID, AGGREGATOR) == []
    first.wait_for(r"joined a fabric", 30.0)

    code, seconds, lines = service_dut.terminate()
    output = "\n".join(lines)
    assert code == 0, f"matter-rs run exited with {code} after SIGTERM:\n{output[-3000:]}"
    assert seconds < STOP_TIMEOUT, f"the stop took {seconds:.1f} s"
    assert "stop signal received, shutting down" in output
    assert "matter-rs stopped" in output

    second = service_dut.start().running()
    ready = second.expect_pattern(r"node is running .*commissioned=(true|false)")
    assert ready.group(1) == "true", f"the restarted node does not know its fabric: {ready.group(0)}"

    assert service_chiptool.read_parts_list(SERVICE_CHIPTOOL_NODE_ID, AGGREGATOR) == []
    label = service_chiptool.read_value("basicinformation", "node-label", "NodeLabel", SERVICE_CHIPTOOL_NODE_ID, 0)
    assert label == "matter-rs-interop", f"NodeLabel after the restart: {label}"


def test_service_config_text_is_complete() -> None:
    """The generated configuration carries the harness passcode, the DUT's port and the bridge role."""
    config = HarnessConfig.from_env({"INTEROP_ENGINE_INTERFACE": "eth0"})
    state_dir = Path(tempfile.gettempdir()) / "interop-service-state"
    parsed = tomllib.loads(service_config_text(config, state_dir, 5542, 3842))
    assert parsed["node"] == {
        "passcode": config.passcode,
        "discriminator": 3842,
        "node_label": "matter-rs-interop",
    }
    assert parsed["network"] == {"interface": "eth0", "port": 5542}
    assert Path(parsed["state"]["dir"]) == state_dir
    assert parsed["log"] == {"level": "info", "format": "text"}
    assert parsed["bridge"] == {}
