//! The error type of the MQTT adapter.

/// Error of the MQTT adapter.
///
/// Errors of the adapter contract are wrapped in [`Error::Adapter`] rather than re-exported.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the adapter contract.
    #[error("adapter error")]
    Adapter(#[from] matter_rs_adapter_api::Error),
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn error_is_core_error() {
        fn assert_error<E: core::error::Error + Send + Sync + 'static>() {}
        assert_error::<Error>();
    }

    #[test]
    fn adapter_error_is_wrapped_as_source() {
        let adapter = matter_rs_adapter_api::Error::from(matter_rs_engine::Error::Unsupported {
            operation: "nothing",
        });
        let error = Error::from(adapter);
        let source = core::error::Error::source(&error).expect("wrapped error is the source");
        assert_eq!(source.to_string(), "engine error");
    }
}
