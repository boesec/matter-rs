//! The error type of the adapter contract.

/// Error of the adapter contract.
///
/// Errors of the crates below are wrapped in variants rather than re-exported. The bridge variant
/// exists only when the `role-bridge` feature selects the bridge role.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the engine facade.
    #[error("engine error")]
    Engine(#[from] matter_rs_engine::Error),
    /// An error of the bridge role.
    #[cfg(feature = "role-bridge")]
    #[error("bridge error")]
    Bridge(#[from] matter_rs_bridge::Error),
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
    fn engine_error_is_wrapped_as_source() {
        let error = Error::from(matter_rs_engine::Error::Unsupported {
            operation: "nothing",
        });
        let source = core::error::Error::source(&error).expect("wrapped error is the source");
        assert_eq!(source.to_string(), "the engine does not support nothing");
    }

    #[cfg(feature = "role-bridge")]
    #[test]
    fn bridge_error_is_wrapped_as_source() {
        let bridge = matter_rs_bridge::Error::from(matter_rs_engine::Error::Unsupported {
            operation: "nothing",
        });
        let error = Error::from(bridge.clone());
        assert_eq!(error, Error::Bridge(bridge));
        assert_eq!(error.to_string(), "bridge error");
        let source = core::error::Error::source(&error).expect("wrapped error is the source");
        assert_eq!(source.to_string(), "engine error");
    }
}
