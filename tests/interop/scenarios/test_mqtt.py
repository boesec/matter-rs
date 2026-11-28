"""The MQTT adapter end to end: ``matter-rs run`` with an ``mqtt`` adapter and two devices on a
Mosquitto broker, commissioned by chip-tool. The scenarios play the source adapter with the
broker's own clients and check both directions, the bridge's own topics, reachability and a
broker restart.

The fixtures are local to this module: the broker container, the service on its own port,
discriminator, chip-tool fabric and state directory, next to the other devices under test. The
binary is built here when ``INTEROP_SERVICE_DUT`` does not name one.
"""

from __future__ import annotations

import logging
import shutil
import tempfile
import time
from pathlib import Path

import pytest

from harness.chiptool import ChipTool
from harness.config import HarnessConfig
from harness.docker import remove_volume
from harness.mqtt import (
    ADAPTER_CONNECTED,
    ADAPTER_LOST,
    INSTANCE,
    LAMP_ENDPOINT,
    SOURCE,
    THERMOMETER_ENDPOINT,
    MosquittoBroker,
    MqttServiceDut,
    mqtt_port,
    mqtt_sections,
    mqtt_service_discriminator,
    mqtt_service_port,
    percent_to_level,
)
from harness.process import HarnessError
from harness.redact import Redactor
from harness.servicedut import ServiceBuild, build_service_binary

# The build of the binary runs inside the first scenario's setup.
pytestmark = [pytest.mark.docker, pytest.mark.timeout(1800)]

MQTT_CHIPTOOL_NODE_ID = 31


@pytest.fixture(scope="module")
def mqtt_broker(config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, docker: bool):
    """Mosquitto on the harness port, anonymous, without persistence."""
    broker = MosquittoBroker(config, config.logs_dir / "mosquitto", redactor, harness_logger, port=mqtt_port())
    try:
        broker.start()
    except HarnessError as error:
        if config.require_docker:
            pytest.fail(str(error))
        pytest.skip(str(error))
    yield broker
    broker.remove()


@pytest.fixture(scope="module")
def mqtt_service_build(config: HarnessConfig, harness_logger: logging.Logger, docker: bool) -> ServiceBuild:
    try:
        build = build_service_binary(harness_logger)
    except HarnessError as error:
        if config.require_docker:
            pytest.fail(str(error))
        pytest.skip(str(error))
    if build.built:
        print(f"MQTT service DUT: release build of matter-rs took {build.seconds:.1f} s")
    return build


@pytest.fixture(scope="module")
def mqtt_service_dut(
    config: HarnessConfig,
    redactor: Redactor,
    harness_logger: logging.Logger,
    mqtt_broker: MosquittoBroker,
    mqtt_service_build: ServiceBuild,
):
    """``matter-rs run`` with the MQTT adapter, after the source's retained state is on the broker."""
    # The source adapter is up and the devices have values before the service starts, so that the
    # devices are reachable from the first moment.
    mqtt_broker.publish(f"{SOURCE}/connected", "2", retain=True)
    mqtt_broker.publish(f"{SOURCE}/status/lamp/on_off", "false", retain=True)
    mqtt_broker.publish(f"{SOURCE}/status/lamp/level", "50", retain=True)
    mqtt_broker.publish(f"{SOURCE}/status/thermometer/temperature", '{"val": 21.5, "ts": 1, "lc": 1}', retain=True)
    work_dir = Path(tempfile.mkdtemp(prefix="interop-mqtt-service-"))
    dut = MqttServiceDut(
        mqtt_service_build.binary,
        config,
        work_dir,
        config.logs_dir / "dut",
        redactor,
        harness_logger,
        broker_port=mqtt_broker.port,
        port=mqtt_service_port(),
        discriminator=mqtt_service_discriminator(),
    ).start()
    yield dut
    dut.stop()
    shutil.rmtree(work_dir, ignore_errors=True)


@pytest.fixture(scope="module")
def mqtt_chiptool(
    config: HarnessConfig, redactor: Redactor, harness_logger: logging.Logger, mqtt_service_dut: MqttServiceDut
):
    """A chip-tool with its own fabric for the MQTT service, filtering on its discriminator."""
    tool = ChipTool(
        config,
        redactor,
        harness_logger,
        config.logs_dir / "mqtt-chip-tool",
        component="mqtt-chip-tool",
        discriminator=mqtt_service_dut.discriminator,
    )
    yield tool
    remove_volume(tool.volume)


@pytest.fixture(scope="module")
def mqtt_commissioned(mqtt_service_dut: MqttServiceDut, mqtt_chiptool: ChipTool) -> ChipTool:
    """The MQTT service on chip-tool's fabric."""
    mqtt_chiptool.pair(MQTT_CHIPTOOL_NODE_ID)
    mqtt_service_dut.running().wait_for(r"joined a fabric", 30.0)
    return mqtt_chiptool


def read_until(tool: ChipTool, cluster: str, attribute: str, label: str, endpoint: int, expected: str) -> str:
    """Reads an attribute up to three times until it shows ``expected``; returns the last value."""
    value = ""
    for _ in range(3):
        value = tool.read_value(cluster, attribute, label, MQTT_CHIPTOOL_NODE_ID, endpoint)
        if value == expected:
            return value
        time.sleep(1.0)
    return value


def test_mqtt_bridge_publishes_its_state(mqtt_service_dut: MqttServiceDut, mqtt_broker: MosquittoBroker) -> None:
    """The bridge is an mqtt-smarthome instance: `connected` 2, `info`, the commissioning items, no secret."""
    assert mqtt_broker.read_retained(f"{INSTANCE}/connected") == "2"
    info = mqtt_broker.read_retained_val(f"{INSTANCE}/info")
    assert isinstance(info, dict), info
    assert info["name"] == "matter-rs"
    assert info["spec"] == "2.0"
    assert info["maintenance"] is False
    assert info["matter"]["vendor_id"] == 0xFFF1
    assert isinstance(info["pid"], int)
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/commissioned") is False
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/window_open") is False
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/fabric_count") == 0
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/devices/lamp/reachable") is True
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/devices/thermometer/reachable") is True
    # Nothing under the instance carries a pairing code or an onboarding payload.
    for topic, payload in mqtt_broker.collect(f"{INSTANCE}/#", 2):
        assert "MT:" not in payload, f"{topic}: {payload}"
        assert "<redacted>" not in payload, f"{topic} carries a redacted secret"


def test_mqtt_status_reaches_the_controller(mqtt_commissioned: ChipTool, mqtt_broker: MosquittoBroker) -> None:
    """A status published by the source shows up as the attribute chip-tool reads."""
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/commissioned") is True
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/fabric_count") == 1

    level = mqtt_commissioned.read_value(
        "levelcontrol", "current-level", "CurrentLevel", MQTT_CHIPTOOL_NODE_ID, LAMP_ENDPOINT
    )
    assert level == str(percent_to_level(50)), f"CurrentLevel from the retained 50 %: {level}"
    mqtt_broker.publish(f"{SOURCE}/status/lamp/level", "75", retain=True)
    level = read_until("levelcontrol", "current-level", "CurrentLevel", LAMP_ENDPOINT, str(percent_to_level(75)))
    assert level == str(percent_to_level(75)), f"CurrentLevel after 75 %: {level}"

    temperature = mqtt_commissioned.read_value(
        "temperaturemeasurement", "measured-value", "MeasuredValue", MQTT_CHIPTOOL_NODE_ID, THERMOMETER_ENDPOINT
    )
    assert temperature == "2150", f"MeasuredValue from 21.5 degrees: {temperature}"
    reachable = mqtt_commissioned.read_value(
        "bridgeddevicebasicinformation", "reachable", "Reachable", MQTT_CHIPTOOL_NODE_ID, LAMP_ENDPOINT
    )
    assert reachable == "TRUE"


def test_mqtt_command_becomes_a_set(mqtt_commissioned: ChipTool, mqtt_broker: MosquittoBroker) -> None:
    """chip-tool's `on` is published as `set/lamp/on_off true`, plain and without a status echo."""
    watcher = mqtt_broker.subscribe(f"{SOURCE}/#", "set-watch")
    try:
        # The retained status items arrive first; everything after the mark is new traffic.
        time.sleep(2.0)
        mark = watcher.mark()
        mqtt_commissioned.run("on", ("onoff", "on", str(MQTT_CHIPTOOL_NODE_ID), str(LAMP_ENDPOINT)), 60.0)
        watcher.wait_for(rf"^{SOURCE}/set/lamp/on_off true$", 30.0, since=mark)
        time.sleep(2.0)
        new_lines = watcher.lines[mark:]
        echoed = [line for line in new_lines if line.startswith(f"{SOURCE}/status/")]
        assert not echoed, f"the adapter echoed a status: {echoed}"
    finally:
        watcher.stop()
    # The bridge serves the target value to the controller meanwhile.
    on_off = mqtt_commissioned.read_on_off(MQTT_CHIPTOOL_NODE_ID, LAMP_ENDPOINT)
    assert on_off is True


def test_mqtt_reachability_follows_the_source(mqtt_commissioned: ChipTool, mqtt_broker: MosquittoBroker) -> None:
    """`source/connected` 0 makes the devices unreachable; 2 brings them back."""
    mqtt_broker.publish(f"{SOURCE}/connected", "0", retain=True)
    reachable = read_until("bridgeddevicebasicinformation", "reachable", "Reachable", LAMP_ENDPOINT, "FALSE")
    assert reachable == "FALSE"
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/devices/lamp/reachable") is False
    mqtt_broker.publish(f"{SOURCE}/connected", "2", retain=True)
    reachable = read_until("bridgeddevicebasicinformation", "reachable", "Reachable", THERMOMETER_ENDPOINT, "TRUE")
    assert reachable == "TRUE"
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/devices/thermometer/reachable") is True


def test_mqtt_broker_restart_recovers(
    mqtt_commissioned: ChipTool, mqtt_broker: MosquittoBroker, mqtt_service_dut: MqttServiceDut
) -> None:
    """After the broker container is stopped and started, the adapter reconnects, publishes anew and reads again."""
    service = mqtt_service_dut.running()
    mark = service.mark()
    mqtt_broker.stop_container()
    service.wait_for(ADAPTER_LOST.format(port=mqtt_broker.port), 30.0, since=mark)
    # The devices keep their last state while the broker is away.
    reachable = mqtt_commissioned.read_value(
        "bridgeddevicebasicinformation", "reachable", "Reachable", MQTT_CHIPTOOL_NODE_ID, LAMP_ENDPOINT
    )
    assert reachable == "TRUE"

    mqtt_broker.start_container()
    service.wait_for(ADAPTER_CONNECTED.format(port=mqtt_broker.port), 60.0, since=mark)
    # The broker lost its retained state with the restart: `connected` and `info` are there
    # again only because the adapter published them anew.
    deadline = time.monotonic() + 30.0
    while mqtt_broker.read_retained(f"{INSTANCE}/connected", 2.0) != "2":
        assert time.monotonic() < deadline, "matter/connected was not published after the reconnect"
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/info") is not None
    assert mqtt_broker.read_retained_val(f"{INSTANCE}/status/commissioning/commissioned") is True
    # Values flow again: the subscriptions were made anew.
    mqtt_broker.publish(f"{SOURCE}/connected", "2", retain=True)
    mqtt_broker.publish(f"{SOURCE}/status/lamp/level", "10", retain=True)
    level = read_until("levelcontrol", "current-level", "CurrentLevel", LAMP_ENDPOINT, str(percent_to_level(10)))
    assert level == str(percent_to_level(10)), f"CurrentLevel after the restart: {level}"


def test_mqtt_sections_are_complete() -> None:
    """The appended configuration names the adapter, the broker port and both devices (no Docker needed)."""
    import tomllib

    parsed = tomllib.loads(mqtt_sections(1884))
    assert parsed["adapters"]["home"]["kind"] == "mqtt"
    assert parsed["adapters"]["home"]["url"] == "mqtt://127.0.0.1:1884"
    assert parsed["adapters"]["home"]["name"] == INSTANCE
    assert parsed["devices"]["lamp"]["kind"] == "dimmable-light"
    assert parsed["devices"]["lamp"]["mapping"]["source"] == f"{SOURCE}/lamp"
    assert parsed["devices"]["lamp"]["mapping"]["items"]["level"]["scale"] == [0, 100]
    assert parsed["devices"]["thermometer"]["mapping"]["items"]["temperature"]["factor"] == 100
    assert percent_to_level(0) == 1
    assert percent_to_level(100) == 254
    assert percent_to_level(50) == 128
    assert mqtt_port({}) == 1884
    assert mqtt_service_port({}) == 5543
    assert mqtt_service_discriminator({"INTEROP_MQTT_DISCRIMINATOR": "0xF04"}) == 0xF04
