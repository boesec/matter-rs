//! The error type of the engine facade.

/// Error of the engine facade.
///
/// Every library crate of the workspace has exactly one public error type like this one: a
/// `#[non_exhaustive]` enum that implements [`core::error::Error`] and [`core::fmt::Display`]
/// without the standard library. Crates above this one wrap it in a variant of their own error
/// instead of re-exporting it.
///
/// The variants grow with the facade. New variants are not a breaking change for callers because
/// the enum is non-exhaustive.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The engine behind the facade does not support the named operation.
    ///
    /// Engines differ in what they implement; the facade reports the gap instead of pretending.
    #[error("the engine does not support {operation}")]
    Unsupported {
        /// Short, stable name of the operation, for diagnostics.
        operation: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::Error;

    #[test]
    fn error_is_core_error() {
        fn assert_error<E: core::error::Error + Send + Sync + 'static>() {}
        assert_error::<Error>();
    }

    #[test]
    fn unsupported_names_the_operation() {
        let error = Error::Unsupported {
            operation: "group messaging",
        };
        assert_eq!(
            error.to_string(),
            "the engine does not support group messaging"
        );
    }
}
