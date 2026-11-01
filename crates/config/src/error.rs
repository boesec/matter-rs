//! The error type of the configuration crate.

use std::path::PathBuf;

use crate::diagnostic::Report;

/// Error of the configuration crate.
///
/// Errors of the crates below are wrapped in variants rather than re-exported; the variants of
/// optional crates exist only when their feature is enabled.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The configuration file cannot be read.
    #[error("cannot read the configuration file `{}`", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// Why it cannot be read.
        source: std::io::Error,
    },
    /// The configuration file is invalid. The report holds every error and warning.
    #[error("the configuration file is invalid ({} errors)", .0.errors().count())]
    Invalid(Report),
    /// The operating system cannot supply random numbers for a new passcode or discriminator.
    #[error("the operating system cannot supply random numbers: {0}")]
    Random(getrandom::Error),
    /// An error of the engine facade.
    #[error("engine error")]
    Engine(#[from] matter_rs_engine::Error),
    /// An error of the adapter contract.
    #[error("adapter error")]
    Adapter(#[from] matter_rs_adapter_api::Error),
    /// An error of the bridge role.
    #[cfg(feature = "role-bridge")]
    #[error("bridge error")]
    Bridge(#[from] matter_rs_bridge::Error),
    /// An error of the MQTT adapter.
    #[cfg(feature = "adapter-mqtt")]
    #[error("MQTT adapter error")]
    AdapterMqtt(#[from] matter_rs_adapter_mqtt::Error),
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
