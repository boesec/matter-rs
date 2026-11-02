//! The error type of the rs-matter engine implementation.

use std::fmt;

/// Error of the rs-matter engine implementation.
///
/// Errors of the engine facade are wrapped in [`Error::Engine`]; errors of rs-matter itself are
/// translated into [`Error::RsMatter`] as text, so that no rs-matter type is visible outside this
/// crate. The facade methods themselves return the facade's error, as the contract demands; this
/// type is for the crate's own entry points, such as [`pairing_info_for`](crate::pairing_info_for).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the engine facade.
    #[error("engine error")]
    Engine(#[from] matter_rs_engine::Error),
    /// rs-matter reported a failure; the text is rs-matter's error code.
    #[error("rs-matter failure: {0}")]
    RsMatter(RsMatterError),
}

/// The text of an rs-matter error, kept as a value of our own so that the rs-matter error type
/// never crosses the crate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsMatterError(String);

impl RsMatterError {
    /// Captures an rs-matter error as text.
    pub(crate) fn new(error: &rs_matter::error::Error) -> Self {
        Self(format!("{:?}", error.code()))
    }
}

impl fmt::Display for RsMatterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<rs_matter::error::Error> for Error {
    fn from(error: rs_matter::error::Error) -> Self {
        Self::RsMatter(RsMatterError::new(&error))
    }
}

/// Converts an rs-matter error into the facade's error, as text.
pub(crate) fn facade_error(error: &rs_matter::error::Error) -> matter_rs_engine::Error {
    matter_rs_engine::Error::Engine(format!("rs-matter: {:?}", error.code()))
}

#[cfg(test)]
mod tests {
    use super::{Error, facade_error};

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

    #[test]
    fn rs_matter_error_is_text_only() {
        let error = Error::from(rs_matter::error::Error::new(
            rs_matter::error::ErrorCode::NoSpace,
        ));
        assert_eq!(error.to_string(), "rs-matter failure: NoSpace");
        let facade = facade_error(&rs_matter::error::Error::new(
            rs_matter::error::ErrorCode::Busy,
        ));
        assert_eq!(facade.to_string(), "engine failure: rs-matter: Busy");
    }
}
