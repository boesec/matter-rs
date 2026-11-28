//! The error type of the MQTT adapter.

use crate::config::UrlProblem;

/// Error of the MQTT adapter.
///
/// Errors of the adapter contract are wrapped in [`Error::Adapter`] rather than re-exported.
/// The adapter's run and factory report through the contract's error type; this type covers
/// what the crate does on its own, such as parsing a broker URL. Messages never carry a value
/// from the configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the adapter contract.
    #[error("adapter error")]
    Adapter(#[from] matter_rs_adapter_api::Error),
    /// The broker URL is not usable.
    #[error("the broker URL {0}")]
    Url(UrlProblem),
}

#[cfg(test)]
mod tests {
    use super::{Error, UrlProblem};

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

    #[test]
    fn url_errors_name_the_part_without_the_text() {
        assert_eq!(
            Error::Url(UrlProblem::Scheme).to_string(),
            "the broker URL must start with `mqtt://` or `mqtts://`"
        );
    }
}
