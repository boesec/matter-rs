//! The error type of the configuration crate.

/// Error of the configuration crate.
///
/// Errors of the crates below are wrapped in variants rather than re-exported; the variants of
/// optional crates exist only when their feature is enabled.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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
