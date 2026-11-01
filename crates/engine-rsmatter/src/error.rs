//! The error type of the rs-matter engine implementation.

/// Error of the rs-matter engine implementation.
///
/// Errors of the engine facade are wrapped in [`Error::Engine`]; errors of rs-matter itself will
/// be translated into facade or implementation errors here, so that no rs-matter type is visible
/// outside this crate.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the engine facade.
    #[error("engine error")]
    Engine(#[from] matter_rs_engine::Error),
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
    fn engine_error_is_wrapped_as_source() {
        let error = Error::from(matter_rs_engine::Error::Unsupported {
            operation: "nothing",
        });
        let source = core::error::Error::source(&error).expect("wrapped error is the source");
        assert_eq!(source.to_string(), "the engine does not support nothing");
    }
}
