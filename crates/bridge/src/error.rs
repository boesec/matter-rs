//! The error type of the bridge role.

use alloc::string::String;

use crate::model::{Attribute, DeviceKey, PartIndex};

/// Error of the bridge role.
///
/// Errors of the engine facade are wrapped in [`Error::Engine`] rather than re-exported. Errors
/// are per call: a failing operation on one device changes nothing on another. Messages name
/// keys, parts and attributes, never values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An error of the engine facade.
    #[error("engine error")]
    Engine(#[from] matter_rs_engine::Error),
    /// The storage failed while the bridge read or wrote its own state.
    #[error("storage error")]
    Storage(#[from] matter_rs_engine::StorageError),
    /// A device key is not acceptable.
    #[error("invalid device key: {reason}")]
    InvalidKey {
        /// What is wrong with the key.
        reason: &'static str,
    },
    /// A device specification is inconsistent and was refused as a whole.
    #[error("invalid specification of device {key}: {reason}")]
    InvalidSpec {
        /// The device.
        key: DeviceKey,
        /// What is wrong.
        reason: String,
    },
    /// A device with this key is added already.
    #[error("device {0} exists already")]
    DuplicateDevice(DeviceKey),
    /// No device with this key is added.
    #[error("unknown device {0}")]
    UnknownDevice(DeviceKey),
    /// The device has no part with this index.
    #[error("device {key} has no part {part}")]
    UnknownPart {
        /// The device.
        key: DeviceKey,
        /// The part index that was named.
        part: PartIndex,
    },
    /// The kind of a part requires an initial value the adapter did not supply.
    #[error("device {key}, part {part}: no initial value for {attribute}")]
    MissingInitialValue {
        /// The device.
        key: DeviceKey,
        /// The part.
        part: PartIndex,
        /// The attribute without a value.
        attribute: Attribute,
    },
    /// A value does not fit its attribute: wrong type, null where not allowed, out of range.
    #[error("device {key}, part {part}: invalid value for {attribute}: {reason}")]
    InvalidValue {
        /// The device.
        key: DeviceKey,
        /// The part.
        part: PartIndex,
        /// The attribute.
        attribute: Attribute,
        /// What is wrong, without the value.
        reason: String,
    },
    /// The part does not serve this attribute.
    #[error("device {key}, part {part} has no {attribute}")]
    UnknownAttribute {
        /// The device.
        key: DeviceKey,
        /// The part.
        part: PartIndex,
        /// The attribute.
        attribute: Attribute,
    },
    /// The part does not produce this kind of event.
    #[error("device {key}, part {part} does not produce this event")]
    UnsupportedEvent {
        /// The device.
        key: DeviceKey,
        /// The part.
        part: PartIndex,
    },
    /// Every endpoint number is reserved; a device must be forgotten before another can be
    /// added.
    #[error("no endpoint number is free")]
    EndpointsExhausted,
    /// A persisted value of the bridge cannot be decoded.
    #[error("corrupt bridge state under storage key {key}: {reason}")]
    CorruptState {
        /// The storage key.
        key: String,
        /// What is wrong.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::Error;
    use crate::model::{Attribute, DeviceKey};

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
    fn messages_name_keys_parts_and_attributes() {
        let key = DeviceKey::new("lamp").unwrap();
        assert_eq!(
            Error::MissingInitialValue {
                key: key.clone(),
                part: 0,
                attribute: Attribute::OnOff,
            }
            .to_string(),
            "device lamp, part 0: no initial value for OnOff"
        );
        assert_eq!(
            Error::UnknownPart { key, part: 3 }.to_string(),
            "device lamp has no part 3"
        );
        assert_eq!(
            Error::EndpointsExhausted.to_string(),
            "no endpoint number is free"
        );
    }
}
