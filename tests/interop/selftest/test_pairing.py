"""The manual pairing code the harness treats as a secret must be the one the tools print."""

from __future__ import annotations

import pytest

from harness.pairing import manual_pairing_code, verhoeff_check_digit


def test_manual_pairing_code_matches_the_well_known_development_vector() -> None:
    # Passcode 20202021 with discriminator 3840 is the development default of every Matter SDK;
    # its manual pairing code is printed by all of them.
    assert manual_pairing_code(20202021, 3840) == "34970112332"


def test_verhoeff_check_digit_of_the_development_vector() -> None:
    assert verhoeff_check_digit("3497011233") == "2"


@pytest.mark.parametrize(("passcode", "discriminator"), [(0, 3840), (2**27, 3840), (20202021, 4096), (20202021, -1)])
def test_out_of_range_inputs_are_rejected(passcode: int, discriminator: int) -> None:
    with pytest.raises(ValueError):
        manual_pairing_code(passcode, discriminator)
