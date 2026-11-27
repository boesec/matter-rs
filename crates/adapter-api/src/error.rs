//! The error type of the adapter contract.

use alloc::string::String;

/// Error of the adapter contract.
///
/// Adapters return it from [`Adapter::run`](crate::Adapter::run) and from their factory; the
/// context returns it for device operations. Errors of the crates below are wrapped in variants
/// rather than re-exported. The bridge variants exist only when the `role-bridge` feature
/// selects the bridge role.
///
/// Messages are read by the operator in logs and health reports. They name what failed, never
/// a value from the configuration or the wire: a password, a token or a passcode belongs in a
/// [`Secret`](matter_rs_engine::Secret), whose text is always the redaction marker.
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
    /// The adapter's configuration cannot be used. A supervisor does not restart an adapter
    /// that fails this way: the configuration does not fix itself.
    #[error("invalid adapter configuration: {0}")]
    Config(String),
    /// The adapter lost or could not open the connection to what it adapts.
    #[error("connection failed: {0}")]
    Connection(String),
    /// Anything else that went wrong inside the adapter.
    #[error("internal adapter error: {0}")]
    Internal(String),
    /// A device key is owned by another adapter instance; keys are unique across the
    /// configuration.
    #[cfg(feature = "role-bridge")]
    #[error("device {key} belongs to adapter {owner}")]
    ForeignDevice {
        /// The device.
        key: matter_rs_bridge::DeviceKey,
        /// The adapter that owns it.
        owner: crate::AdapterId,
    },
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

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

    #[test]
    fn adapter_errors_name_what_failed() {
        assert_eq!(
            Error::Config(String::from("topic is empty")).to_string(),
            "invalid adapter configuration: topic is empty"
        );
        assert_eq!(
            Error::Connection(String::from("broker refused")).to_string(),
            "connection failed: broker refused"
        );
        assert_eq!(
            Error::Internal(String::from("state machine")).to_string(),
            "internal adapter error: state machine"
        );
    }

    #[test]
    fn secrets_in_messages_stay_redacted() {
        let password = matter_rs_engine::Secret::new("hunter2");
        let error = Error::Connection(alloc::format!("broker refused password {password}"));
        let text = error.to_string();
        assert!(text.contains(matter_rs_engine::REDACTED), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
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

    #[cfg(feature = "role-bridge")]
    #[test]
    fn foreign_device_names_key_and_owner() {
        let error = Error::ForeignDevice {
            key: matter_rs_bridge::DeviceKey::new("lamp").unwrap(),
            owner: crate::AdapterId::new("mqtt", "home"),
        };
        assert_eq!(
            error.to_string(),
            "device lamp belongs to adapter mqtt/home"
        );
    }
}
