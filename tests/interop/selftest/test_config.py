"""Pins and configuration: every image by digest, the DUT swappable through the environment."""

from __future__ import annotations

import re

import pytest

from harness.config import INTEROP_ROOT, HarnessConfig, load_images

DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")


def test_every_image_is_pinned_by_digest() -> None:
    images = load_images()
    assert set(images) >= {"chip", "node"}
    for name, image in images.items():
        assert DIGEST.match(image.digest), f"{name}: digest {image.digest!r} is not a sha256 digest"
        assert image.pinned == f"{image.name}@{image.digest}"
    assert images["chip"].commit is not None and COMMIT.match(images["chip"].commit)
    assert images["chip"].tag == images["chip"].commit, "the certification image tag encodes its commit"


def test_matterjs_dockerfile_uses_the_pinned_node_digest() -> None:
    dockerfile = (INTEROP_ROOT / "containers" / "matterjs" / "Dockerfile").read_text(encoding="utf-8")
    from_line = next(line for line in dockerfile.splitlines() if line.startswith("FROM "))
    node = load_images()["node"]
    assert from_line == f"FROM {node.tagged}@{node.digest}"


def test_matterjs_lockfile_pins_the_packages_exactly() -> None:
    import json

    lock = json.loads((INTEROP_ROOT / "containers" / "matterjs" / "package-lock.json").read_text(encoding="utf-8"))
    packages = lock["packages"]
    assert packages["node_modules/@matter/main"]["version"] == "0.18.0"
    assert packages["node_modules/@matter/nodejs-shell"]["version"] == "0.18.0"
    manifest = json.loads((INTEROP_ROOT / "containers" / "matterjs" / "package.json").read_text(encoding="utf-8"))
    for name, version in manifest["dependencies"].items():
        assert re.match(r"^\d+\.\d+\.\d+$", version), f"{name} is not pinned to an exact version: {version}"


def test_default_dut_is_the_matterjs_device_with_the_configured_parameters() -> None:
    config = HarnessConfig.from_env({"INTEROP_PASSCODE": "12345678", "INTEROP_DISCRIMINATOR": "1234"})
    dut = config.dut()
    assert dut.name == "matterjs-onoff-light"
    assert dut.command[:3] == ("docker", "run", "--rm")
    assert "--passcode=12345678" in dut.command
    assert "--discriminator=1234" in dut.command
    assert dut.container_name == config.container_name("dut")
    assert dut.endpoint == 1


def test_dut_can_be_swapped_through_the_environment() -> None:
    config = HarnessConfig.from_env(
        {
            "INTEROP_DUT_COMMAND": "target/debug/matter-rs --config 'my config.toml'",
            "INTEROP_DUT_READY_PATTERN": "bridge is online",
            "INTEROP_DUT_ENDPOINT": "3",
        }
    )
    dut = config.dut()
    assert dut.name == "custom"
    assert dut.command == ("target/debug/matter-rs", "--config", "my config.toml")
    assert dut.ready_pattern == "bridge is online"
    assert dut.endpoint == 3
    assert dut.container_name is None


def test_dut_command_without_ready_pattern_is_rejected() -> None:
    config = HarnessConfig.from_env({"INTEROP_DUT_COMMAND": "./something"})
    with pytest.raises(ValueError, match="INTEROP_DUT_READY_PATTERN"):
        config.dut()


def test_require_docker_flag() -> None:
    assert HarnessConfig.from_env({}).require_docker is False
    assert HarnessConfig.from_env({"INTEROP_REQUIRE_DOCKER": "0"}).require_docker is False
    assert HarnessConfig.from_env({"INTEROP_REQUIRE_DOCKER": "1"}).require_docker is True
