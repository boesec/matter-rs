//! Rules for the setup passcode and the discriminator, and fresh random values for a new
//! configuration.
//!
//! The rules follow the Matter Core Specification, section 5.1.7 "Setup Passcode": a passcode is
//! a number from 1 to 99 999 998, and the trivial values listed there are not allowed.

use core::fmt;

/// The largest valid setup passcode.
pub const PASSCODE_MAX: u32 = 99_999_998;

/// The largest valid discriminator (12 bits).
pub const DISCRIMINATOR_MAX: u16 = 0x0FFF;

/// The passcode that Matter examples and test setups use. It is valid, but anybody who knows it
/// can commission the node, so it is accepted with a warning only.
pub const TEST_PASSCODE: u32 = 20_202_021;

/// Passcodes the Matter specification forbids because they are trivial to guess.
const FORBIDDEN_PASSCODES: [u32; 12] = [
    0, 11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777,
    88_888_888, 99_999_999, 12_345_678, 87_654_321,
];

/// Why a passcode is not allowed. The message never contains the passcode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasscodeError {
    /// Outside 1..=99 999 998.
    OutOfRange,
    /// One of the trivial values the specification forbids.
    Forbidden,
}

impl fmt::Display for PasscodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange => write!(f, "the passcode must be between 1 and {PASSCODE_MAX}"),
            Self::Forbidden => f.write_str(
                "the passcode is one of the trivial values the Matter specification forbids",
            ),
        }
    }
}

/// Checks a setup passcode against the rules of the specification. The test passcode
/// [`TEST_PASSCODE`] passes; callers warn about it separately.
///
/// # Errors
///
/// [`PasscodeError`] when the passcode is out of range or forbidden.
pub fn check_passcode(passcode: u32) -> Result<(), PasscodeError> {
    if FORBIDDEN_PASSCODES.contains(&passcode) {
        Err(PasscodeError::Forbidden)
    } else if !(1..=PASSCODE_MAX).contains(&passcode) {
        Err(PasscodeError::OutOfRange)
    } else {
        Ok(())
    }
}

/// A random valid passcode from the operating system's random number generator. It is never the
/// test passcode.
///
/// # Errors
///
/// When the operating system cannot supply random numbers.
pub fn random_passcode() -> Result<u32, getrandom::Error> {
    // Rejection sampling keeps the distribution uniform: values at or above the largest multiple
    // of the range that fits into a u32 are drawn again.
    let range = PASSCODE_MAX;
    let limit = u32::MAX - u32::MAX % range;
    loop {
        let value = getrandom::u32()?;
        if value >= limit {
            continue;
        }
        let passcode = value % range + 1;
        if check_passcode(passcode).is_ok() && passcode != TEST_PASSCODE {
            return Ok(passcode);
        }
    }
}

/// A random discriminator from the operating system's random number generator.
///
/// # Errors
///
/// When the operating system cannot supply random numbers.
pub fn random_discriminator() -> Result<u16, getrandom::Error> {
    let value = getrandom::u32()? & u32::from(DISCRIMINATOR_MAX);
    Ok(u16::try_from(value).unwrap_or(DISCRIMINATOR_MAX))
}

#[cfg(test)]
mod tests {
    use super::{
        DISCRIMINATOR_MAX, PASSCODE_MAX, PasscodeError, TEST_PASSCODE, check_passcode,
        random_discriminator, random_passcode,
    };

    #[test]
    fn every_forbidden_passcode_is_rejected() {
        for passcode in [
            0, 11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777,
            88_888_888, 99_999_999, 12_345_678, 87_654_321,
        ] {
            assert_eq!(
                check_passcode(passcode),
                Err(PasscodeError::Forbidden),
                "{passcode}"
            );
        }
    }

    #[test]
    fn range_is_one_to_max() {
        assert_eq!(check_passcode(1), Ok(()));
        assert_eq!(check_passcode(PASSCODE_MAX), Ok(()));
        assert_eq!(
            check_passcode(PASSCODE_MAX + 2),
            Err(PasscodeError::OutOfRange)
        );
        assert_eq!(check_passcode(u32::MAX), Err(PasscodeError::OutOfRange));
        assert_eq!(check_passcode(TEST_PASSCODE), Ok(()));
    }

    #[test]
    fn error_message_does_not_contain_the_passcode() {
        let message = PasscodeError::Forbidden.to_string();
        assert!(!message.contains("11111111"), "{message}");
    }

    #[test]
    fn random_values_are_valid() {
        for _ in 0..1000 {
            let passcode = random_passcode().unwrap();
            assert_eq!(check_passcode(passcode), Ok(()));
            assert_ne!(passcode, TEST_PASSCODE);
            assert!(random_discriminator().unwrap() <= DISCRIMINATOR_MAX);
        }
    }
}
