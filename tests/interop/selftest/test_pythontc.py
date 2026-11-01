"""Parsing of the connectedhomeip framework's Mobly summary."""

from __future__ import annotations

from pathlib import Path

import pytest

from harness.process import HarnessError
from harness.pythontc import find_summary, parse_summary

SUMMARY = """\
Begin Time: 1761990000000
Details: null
End Time: 1761990012000
Extras: null
Result: PASS
Test Class: CommissionDeviceTest
Test Name: test_run_commissioning
Type: Record
...
---
Begin Time: 1761990012000
Details: null
End Time: 1761990020000
Extras: null
Result: PASS
Test Class: TC_CGEN_2_1
Test Name: test_TC_CGEN_2_1
Type: Record
...
---
Error: 0
Executed: 2
Failed: 0
Passed: 2
Requested: 2
Skipped: 0
Type: Summary
...
"""


def test_records_and_counts_are_parsed(tmp_path: Path) -> None:
    path = tmp_path / "test_summary.yaml"
    path.write_text(SUMMARY, encoding="utf-8")

    result = parse_summary(path)

    assert [r.name for r in result.records] == ["test_run_commissioning", "test_TC_CGEN_2_1"]
    assert result.record("test_TC_CGEN_2_1").result == "PASS"
    assert (result.passed, result.failed, result.errors, result.skipped) == (2, 0, 0, 0)
    with pytest.raises(HarnessError):
        result.record("test_missing")


def test_a_failed_record_shows_in_describe(tmp_path: Path) -> None:
    path = tmp_path / "test_summary.yaml"
    path.write_text(
        SUMMARY.replace("Result: PASS\nTest Class: TC_CGEN_2_1", "Result: FAIL\nTest Class: TC_CGEN_2_1")
        .replace("Details: null\nEnd Time: 1761990020000", "Details: Breadcrumb should be 1\nEnd Time: 1761990020000")
        .replace("Failed: 0\nPassed: 2", "Failed: 1\nPassed: 1"),
        encoding="utf-8",
    )

    result = parse_summary(path)

    assert result.failed == 1
    assert "FAIL  test_TC_CGEN_2_1" in result.describe()
    assert "Breadcrumb should be 1" in result.describe()


def test_find_summary_prefers_the_newest_real_file(tmp_path: Path) -> None:
    older = tmp_path / "MatterTest" / "01-01-2026_00-00-00-000"
    newer = tmp_path / "MatterTest" / "01-01-2026_00-10-00-000"
    for directory in (older, newer):
        directory.mkdir(parents=True)
        (directory / "test_summary.yaml").write_text(SUMMARY, encoding="utf-8")
    import os

    os.utime(older / "test_summary.yaml", (1, 1))

    assert find_summary(tmp_path) == newer / "test_summary.yaml"
    assert find_summary(tmp_path / "nothing") is None
