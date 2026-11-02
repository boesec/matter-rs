"""The engine example as device under test: chip-tool and the Python framework against a node
built on the engine facade, with endpoints added and removed while it runs.

The scenarios share one engine DUT and one chip-tool fabric (session fixtures) and run in file
order: commissioning first, the restart in the middle, the Python conformance scripts last.
Every scenario that adds endpoints removes them again, so the conformance scripts see the node
as it started: the aggregator at endpoint 1 and the light at endpoint 2.
"""

from __future__ import annotations

import logging
from pathlib import Path

import pytest

from conftest import CommissionedEngineDut
from harness.chiptool import UNSUPPORTED_ENDPOINT, ChipTool, ChipToolInteractive, attribute_report
from harness.config import INTEROP_ROOT, HarnessConfig
from harness.pythontc import PythonTestRunner
from harness.redact import Redactor

pytestmark = [pytest.mark.docker, pytest.mark.timeout(600)]

ON_OFF_CLUSTER = 0x0006
ON_OFF_ATTRIBUTE = 0x0000
BOOLEAN_STATE_CLUSTER = 0x0045
BRIDGED_DEVICE_CLUSTER = 0x0039
NODE_LABEL = 0x0005
AGGREGATOR = 1
# Endpoints the scenarios add; none of them exists at start.
ADDED_SENSOR = 5
ADDED_LIGHT = 6
WILDCARD_SENSOR = 7
REMOVED_LIGHT = 8

PICS_FILE = INTEROP_ROOT / "pics" / "engine-onoff-node.pics"
# How long a subscription report may take after the change: the subscriptions ask for a
# maximum interval of 10 seconds, so a report within 30 seconds covers one missed interval.
REPORT_TIMEOUT = 30.0


def test_engine_chiptool_commission_and_toggle(
    engine_commissioned_dut: CommissionedEngineDut, engine_chiptool: ChipTool
) -> None:
    """chip-tool commissions the node, toggles the light, and the DUT prints the command event."""
    node, endpoint, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.endpoint, engine_commissioned_dut.dut

    before = engine_chiptool.read_on_off(node, endpoint)
    since = dut.mark()
    engine_chiptool.toggle(node, endpoint)
    event = dut.wait_event(
        rf"^event CommandInvoked endpoint={endpoint} cluster=0x0006 command=0x0002 status=Success", 30.0, since
    )
    assert event is not None
    after = engine_chiptool.read_on_off(node, endpoint)

    assert after is (not before), f"OnOff did not change on toggle: before={before}, after={after}"


def test_engine_write_accept_and_reject(
    engine_commissioned_dut: CommissionedEngineDut, engine_chiptool: ChipTool
) -> None:
    """An accepted write is stored, a refused one leaves the value and reaches chip-tool as a status."""
    node, endpoint, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.endpoint, engine_commissioned_dut.dut

    since = dut.mark()
    engine_chiptool.write("bridgeddevicebasicinformation", "node-label", '"Kitchen"', node, endpoint)
    dut.wait_event(
        rf"^event WriteRequested endpoint={endpoint} cluster=0x0039 attribute=0x0005 status=Success", 30.0, since
    )
    assert (
        engine_chiptool.read_value("bridgeddevicebasicinformation", "node-label", "NodeLabel", node, endpoint)
        == '"Kitchen"'
    )

    since = dut.mark()
    refused = engine_chiptool.write(
        "bridgeddevicebasicinformation", "node-label", '"ThisLabelIsTooLongForTheLight"', node, endpoint
    )
    dut.wait_event(
        rf"^event WriteRequested endpoint={endpoint} cluster=0x0039 attribute=0x0005 status=ConstraintError",
        30.0,
        since,
    )
    refused.expect_pattern(r"CONSTRAINT_ERROR")
    assert (
        engine_chiptool.read_value("bridgeddevicebasicinformation", "node-label", "NodeLabel", node, endpoint)
        == '"Kitchen"'
    )


def test_engine_restart_keeps_fabric(engine_commissioned_dut: CommissionedEngineDut, engine_chiptool: ChipTool) -> None:
    """After a restart on the same state directory chip-tool reads the node without commissioning again."""
    node, endpoint, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.endpoint, engine_commissioned_dut.dut

    before = engine_chiptool.read_on_off(node, endpoint)
    dut.restart()
    ready = dut.process.expect_pattern(r"onoff_node: ready .*commissioned=(true|false)")
    assert ready.group(1) == "true", f"the restarted node does not know its fabric: {ready.group(0)}"

    assert engine_chiptool.read_on_off(node, endpoint) is before, "the light's state did not survive the restart"
    engine_chiptool.toggle(node, endpoint)
    assert engine_chiptool.read_on_off(node, endpoint) is (not before)


def test_engine_subscription_reports_set_attribute(
    engine_commissioned_dut: CommissionedEngineDut,
    engine_chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    """A value the product sets reaches a subscriber within the subscription's maximum interval."""
    node, endpoint, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.endpoint, engine_commissioned_dut.dut

    session = ChipToolInteractive(
        engine_chiptool, config, redactor, harness_logger, config.logs_dir / "engine-chip-tool", label="onoff"
    )
    try:
        session.start()
        since = session.subscribe_on_off(node, endpoint)
        current = engine_chiptool.read_on_off(node, endpoint)
        target = "false" if current else "true"
        dut.set(endpoint, ON_OFF_CLUSTER, ON_OFF_ATTRIBUTE, target)
        session.wait_for(rf"OnOff:\s*{target.upper()}", REPORT_TIMEOUT, since)
    finally:
        session.close()
    assert engine_chiptool.read_on_off(node, endpoint) is (target == "true")


def test_engine_runtime_added_endpoint_is_served(
    engine_commissioned_dut: CommissionedEngineDut, engine_chiptool: ChipTool
) -> None:
    """An endpoint added while commissioned is served, listed by the aggregator and bumps ConfigurationVersion."""
    node, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.dut

    version_before = engine_chiptool.read_configuration_version(node)
    parts_before = engine_chiptool.read_parts_list(node, AGGREGATOR)
    assert ADDED_SENSOR not in parts_before
    dut.add(ADDED_SENSOR, "contact-sensor")
    try:
        assert engine_chiptool.read_value("booleanstate", "state-value", "StateValue", node, ADDED_SENSOR) == "FALSE"
        dut.set(ADDED_SENSOR, BOOLEAN_STATE_CLUSTER, 0x0000, "true")
        assert engine_chiptool.read_value("booleanstate", "state-value", "StateValue", node, ADDED_SENSOR) == "TRUE"
        parts_after = engine_chiptool.read_parts_list(node, AGGREGATOR)
        assert set(parts_after) == set(parts_before) | {ADDED_SENSOR}, (
            f"PartsList {parts_after} after adding {ADDED_SENSOR}"
        )
        root_parts = engine_chiptool.read_parts_list(node, 0)
        assert ADDED_SENSOR in root_parts, f"root PartsList {root_parts}"
        version_after = engine_chiptool.read_configuration_version(node)
        assert version_after > version_before, f"ConfigurationVersion {version_before} -> {version_after}"
    finally:
        dut.remove(ADDED_SENSOR)
    assert ADDED_SENSOR not in engine_chiptool.read_parts_list(node, AGGREGATOR)


def test_engine_wildcard_subscription_sees_added_endpoint(
    engine_commissioned_dut: CommissionedEngineDut,
    engine_chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    """A wildcard subscriber receives a report for an endpoint added after it subscribed."""
    node, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.dut

    session = ChipToolInteractive(
        engine_chiptool, config, redactor, harness_logger, config.logs_dir / "engine-chip-tool", label="wildcard"
    )
    try:
        session.start()
        since = session.subscribe_all(node)
        dut.add(WILDCARD_SENSOR, "contact-sensor")
        try:
            session.wait_for(attribute_report(WILDCARD_SENSOR, BOOLEAN_STATE_CLUSTER), REPORT_TIMEOUT, since)
        finally:
            dut.remove(WILDCARD_SENSOR)
    finally:
        session.close()


def test_engine_removed_endpoint_is_unsupported(
    engine_commissioned_dut: CommissionedEngineDut,
    engine_chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    """After a removal the endpoint answers UnsupportedEndpoint, leaves the PartsList, and its subscriber is told."""
    node, dut = engine_commissioned_dut.node_id, engine_commissioned_dut.dut

    dut.add(REMOVED_LIGHT, "on-off-light")
    session = ChipToolInteractive(
        engine_chiptool, config, redactor, harness_logger, config.logs_dir / "engine-chip-tool", label="removed"
    )
    try:
        session.start()
        assert engine_chiptool.read_on_off(node, REMOVED_LIGHT) is False
        since = session.subscribe_on_off(node, REMOVED_LIGHT)
        dut.remove(REMOVED_LIGHT)

        failure = engine_chiptool.read_failure("onoff", "on-off", node, REMOVED_LIGHT)
        assert UNSUPPORTED_ENDPOINT in failure, failure
        assert REMOVED_LIGHT not in engine_chiptool.read_parts_list(node, AGGREGATOR)
        # The subscription learns about the removal: the next report carries the status, or
        # the subscription ends; either shows in the session's output.
        session.wait_for(
            rf"{UNSUPPORTED_ENDPOINT}|Subscription (?:terminated|ended|closed)|OnError|Error: ", REPORT_TIMEOUT, since
        )
    finally:
        session.close()
        if REMOVED_LIGHT in engine_chiptool.read_parts_list(node, AGGREGATOR):
            dut.remove(REMOVED_LIGHT)


def run_framework_scripts(
    runner: PythonTestRunner,
    scripts: list[tuple[str, tuple[str, ...], tuple[str, ...]]],
    code: str,
    logger: logging.Logger,
) -> list[str]:
    """Run the scripts in order (the first commissions with ``code``), return every problem.

    A problem is a failed or errored record, a required record that did not pass, or a non-zero
    exit of the framework. Skipped records are logged with the framework's reason.
    """
    problems: list[str] = []
    skipped: list[str] = []
    for index, (script, required, extra_args) in enumerate(scripts):
        process, result = runner.run(
            script, code if index == 0 else None, Path(PICS_FILE), extra_args=extra_args, timeout=900.0
        )
        for record in result.records:
            if record.result == "SKIP":
                skipped.append(f"{script}: {record.name} skipped: {record.details}")
        if result.failed or result.errors:
            problems.append(f"{script}: {result.describe()}\nexit code {process.returncode}\n{process.tail()}")
            continue
        for name in required:
            record = result.record(name)
            if record.result != "PASS":
                problems.append(f"{script}: {name} is {record.result}: {record.details}")
        if process.returncode != 0:
            problems.append(f"{script}: framework exit code {process.returncode}\n{process.tail()}")
    for line in skipped:
        logger.info("framework skip: %s", line)
    if problems and skipped:
        problems.append("skipped:\n" + "\n".join(skipped))
    return problems


@pytest.mark.timeout(1500)
def test_engine_python_conformance(
    engine_commissioned_dut: CommissionedEngineDut,
    engine_chiptool: ChipTool,
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
) -> None:
    """The connectedhomeip bridged-device and composition scripts pass against the node with its PICS.

    `TC_BRBINFO_2_1` commissions the node through the framework as another fabric;
    `TC_DeviceBasicComposition` (with `TC_DESC_2_1`, `TC_DESC_2_2` and `TC_IDM_10_1`) then runs
    on that fabric over CASE. Steps the framework skips are reported with its reason, not hidden.

    This scenario runs last on the engine DUT: the framework's cleanup removes every fabric
    that was not in its baseline, chip-tool's included, so nothing may need chip-tool's fabric
    on this node afterwards. The order also keeps the composition script off the PASE session
    the framework reuses when it commissioned the node itself (see the README).
    """
    assert PICS_FILE.is_file(), f"PICS file missing: {PICS_FILE}"
    node, endpoint = engine_commissioned_dut.node_id, engine_commissioned_dut.endpoint

    code = engine_chiptool.open_commissioning_window(node, (config.engine_discriminator + 2) & 0xFFF)
    runner = PythonTestRunner(
        config, redactor, harness_logger, config.logs_dir / "engine-python-tc", component="engine-python-tc"
    )
    problems = run_framework_scripts(
        runner,
        [
            ("TC_BRBINFO_2_1.py", ("test_TC_BRBINFO_2_1",), ("--endpoint", str(endpoint))),
            ("TC_DeviceBasicComposition.py", ("test_TC_DESC_2_1", "test_TC_DESC_2_2", "test_TC_IDM_10_1"), ()),
        ],
        code,
        harness_logger,
    )
    assert not problems, "\n\n".join(problems)
