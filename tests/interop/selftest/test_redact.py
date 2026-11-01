"""Redaction: configured secrets, captured secrets and code-shaped values never survive."""

from __future__ import annotations

from pathlib import Path

from harness.redact import MASK, Redactor


def test_configured_secrets_are_masked() -> None:
    redactor = Redactor(["20202021", "34970112332"])
    line = "pairing onnetwork-long 1 20202021 3840 and code 34970112332"
    assert redactor.redact(line) == f"pairing onnetwork-long 1 {MASK} 3840 and code {MASK}"


def test_onboarding_payload_and_code_shaped_numbers_are_masked_without_configuration() -> None:
    redactor = Redactor()
    assert redactor.redact("QR code: MT:Y.K9042C00KA0648G00") == f"QR code: {MASK}"
    assert redactor.redact("Manual pairing code: [36177160937]") == f"Manual pairing code: [{MASK}]"
    assert redactor.redact("long code 749701123365521327694") == f"long code {MASK}"


def test_numbers_that_are_not_code_shaped_stay() -> None:
    redactor = Redactor()
    assert redactor.redact("node 1 endpoint 1 at 1761990000000 ms") == "node 1 endpoint 1 at 1761990000000 ms"


def test_capture_takes_the_value_and_masks_it_from_then_on() -> None:
    redactor = Redactor()
    capture = redactor.capture(r"Manual pairing code: \[(\d+)\]")
    assert redactor.redact("[TOO] Manual pairing code: [36177160937]") == f"[TOO] Manual pairing code: [{MASK}]"
    assert capture.value == "36177160937"
    assert "36177160937" in redactor.secrets
    assert redactor.redact("commission pair 2 --pairing-code 36177160937") == f"commission pair 2 --pairing-code {MASK}"


def test_redact_tree_rewrites_files_in_place(tmp_path: Path) -> None:
    redactor = Redactor(["20202021"])
    (tmp_path / "a").mkdir()
    text = tmp_path / "a" / "log.txt"
    text.write_text("passcode 20202021\nnothing here\n", encoding="utf-8")
    binary = tmp_path / "blob.bin"
    binary.write_bytes(b"\xff\xfe20202021")

    seen = redactor.redact_tree(tmp_path)

    assert seen == 2
    assert text.read_text(encoding="utf-8") == f"passcode {MASK}\nnothing here\n"
    assert binary.read_bytes() == b"\xff\xfe20202021"
