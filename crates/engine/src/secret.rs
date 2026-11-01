//! A wrapper that keeps secrets out of logs and error messages.

use core::fmt;

/// The text that [`Secret`] prints in place of its value.
pub const REDACTED: &str = "[REDACTED]";

/// A value that must never appear in logs, error messages, panics or user-visible output.
///
/// Setup passcodes, keys and broker passwords are wrapped in this type as early as possible. Its
/// [`Debug`](fmt::Debug) and [`Display`](fmt::Display) output is the fixed marker [`REDACTED`],
/// which reveals neither the value nor its length, so that a secret cannot leak by being
/// formatted by accident. The value itself is reachable only through [`Secret::expose`], which
/// makes every use of the secret visible in the code.
///
/// Equality is derived so that two secrets can be compared; the comparison is not constant-time.
///
/// ```
/// use matter_rs_engine::Secret;
///
/// let passcode = Secret::new(20_202_021_u32);
/// assert_eq!(format!("{passcode:?}"), "[REDACTED]");
/// assert_eq!(*passcode.expose(), 20_202_021);
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns the secret value.
    ///
    /// Every call site that needs the value says so by name. The reference must not be formatted,
    /// logged or stored in a type that formats it.
    #[must_use]
    pub const fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;

    use super::{REDACTED, Secret};

    /// A value whose text and whose length are both easy to spot in formatted output.
    const VALUE: &str = "hunter2";

    fn assert_redacted(output: &str) {
        assert_eq!(output, REDACTED);
        assert!(
            !output.contains(VALUE),
            "output contains the value: {output}"
        );
        let length = format!("{}", VALUE.len());
        assert!(
            !output.contains(&length),
            "output contains the length {length}: {output}"
        );
    }

    #[test]
    fn secret_debug_redacts() {
        let secret = Secret::new(String::from(VALUE));
        assert_redacted(&format!("{secret:?}"));
        assert_redacted(&format!("{secret:#?}"));
    }

    #[test]
    fn secret_display_redacts() {
        let secret = Secret::new(String::from(VALUE));
        assert_redacted(&format!("{secret}"));
    }

    #[test]
    fn secret_expose_returns_value() {
        let secret = Secret::new(String::from(VALUE));
        assert_eq!(secret.expose(), VALUE);
    }

    #[test]
    fn secret_clone_and_eq_compare_values() {
        let secret = Secret::new(42_u32);
        assert_eq!(secret.clone(), secret);
        assert_ne!(secret, Secret::new(43_u32));
    }

    #[test]
    fn secret_in_a_struct_stays_redacted() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Credentials {
            user: &'static str,
            password: Secret<&'static str>,
        }
        let credentials = Credentials {
            user: "bridge",
            password: Secret::new(VALUE),
        };
        let output = format!("{credentials:?}");
        assert!(output.contains("bridge"));
        assert!(output.contains(REDACTED));
        assert!(
            !output.contains(VALUE),
            "output contains the value: {output}"
        );
    }
}
