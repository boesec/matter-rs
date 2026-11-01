"""Matter onboarding payload helpers.

Only the manual pairing code is needed here: the harness computes it from the configured passcode
and discriminator so that it can treat the code as a secret before any tool prints it.
"""

from __future__ import annotations

# Verhoeff check digit tables (multiplication, permutation and inverse).
_D = (
    (0, 1, 2, 3, 4, 5, 6, 7, 8, 9),
    (1, 2, 3, 4, 0, 6, 7, 8, 9, 5),
    (2, 3, 4, 0, 1, 7, 8, 9, 5, 6),
    (3, 4, 0, 1, 2, 8, 9, 5, 6, 7),
    (4, 0, 1, 2, 3, 9, 5, 6, 7, 8),
    (5, 9, 8, 7, 6, 0, 4, 3, 2, 1),
    (6, 5, 9, 8, 7, 1, 0, 4, 3, 2),
    (7, 6, 5, 9, 8, 2, 1, 0, 4, 3),
    (8, 7, 6, 5, 9, 3, 2, 1, 0, 4),
    (9, 8, 7, 6, 5, 4, 3, 2, 1, 0),
)
_P = (
    (0, 1, 2, 3, 4, 5, 6, 7, 8, 9),
    (1, 5, 7, 6, 2, 8, 3, 0, 9, 4),
    (5, 8, 0, 3, 7, 9, 6, 1, 4, 2),
    (8, 9, 1, 6, 0, 4, 3, 5, 2, 7),
    (9, 4, 5, 3, 1, 2, 6, 8, 7, 0),
    (4, 2, 8, 6, 5, 7, 3, 9, 0, 1),
    (2, 7, 9, 3, 8, 0, 6, 4, 1, 5),
    (7, 0, 4, 6, 9, 1, 3, 2, 5, 8),
)
_INV = (0, 4, 3, 2, 1, 5, 6, 7, 8, 9)


def verhoeff_check_digit(digits: str) -> str:
    """Return the Verhoeff check digit for a string of decimal digits."""
    c = 0
    for i, ch in enumerate(reversed(digits)):
        c = _D[c][_P[(i + 1) % 8][int(ch)]]
    return str(_INV[c])


def manual_pairing_code(passcode: int, discriminator: int) -> str:
    """Return the 11-digit manual pairing code for a passcode and a 12-bit discriminator.

    Layout (Matter core specification, manual pairing code): digit 1 carries the
    vendor/product-present flag and the top two bits of the discriminator, digits 2 to 6 carry the
    next two discriminator bits and the low 14 bits of the passcode, digits 7 to 10 the remaining
    13 passcode bits, digit 11 is the Verhoeff check digit.
    """
    if not 0 <= discriminator <= 0xFFF:
        raise ValueError(f"discriminator out of range: {discriminator}")
    if not 0 < passcode < 2**27:
        raise ValueError(f"passcode out of range: {passcode}")
    digit1 = (discriminator >> 10) & 0x3
    digits_2_6 = ((discriminator >> 8) & 0x3) << 14 | (passcode & 0x3FFF)
    digits_7_10 = passcode >> 14
    body = f"{digit1:01d}{digits_2_6:05d}{digits_7_10:04d}"
    return body + verhoeff_check_digit(body)
