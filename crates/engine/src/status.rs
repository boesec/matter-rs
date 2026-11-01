//! The Interaction Model status codes a response may carry.

use core::fmt;

/// A general status code of the Matter Interaction Model.
///
/// The product answers a command or a write with one of these; the engine puts it on the wire
/// unchanged. The numeric codes are those of the Matter Core Specification (Interaction Model,
/// "Status Code Table"); deprecated and reserved codes are left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Status {
    /// The operation succeeded (`0x00`).
    Success,
    /// A general failure without a more specific code (`0x01`).
    Failure,
    /// The subscription id is unknown (`0x7D`).
    InvalidSubscription,
    /// The request lacks the privilege for the path (`0x7E`).
    UnsupportedAccess,
    /// The endpoint does not exist (`0x7F`).
    UnsupportedEndpoint,
    /// The action is not valid for the current state of the interaction (`0x80`).
    InvalidAction,
    /// The cluster does not support the command (`0x81`).
    UnsupportedCommand,
    /// The command payload is malformed (`0x85`).
    InvalidCommand,
    /// The cluster does not support the attribute (`0x86`).
    UnsupportedAttribute,
    /// The value violates a constraint of the attribute or field (`0x87`).
    ConstraintError,
    /// The attribute cannot be written (`0x88`).
    UnsupportedWrite,
    /// The node has no resources left for the request (`0x89`).
    ResourceExhausted,
    /// The named item does not exist (`0x8B`).
    NotFound,
    /// The attribute cannot be reported (`0x8C`).
    UnreportableAttribute,
    /// The data type of the value does not match the attribute (`0x8D`).
    InvalidDataType,
    /// The attribute cannot be read (`0x8F`).
    UnsupportedRead,
    /// The data version in the request does not match (`0x92`).
    DataVersionMismatch,
    /// The operation timed out (`0x94`).
    Timeout,
    /// The node is busy; the request may be retried (`0x9C`).
    Busy,
    /// Access is restricted by an access restriction list (`0x9D`).
    AccessRestricted,
    /// The endpoint does not support the cluster (`0xC3`).
    UnsupportedCluster,
    /// The subscription has no upstream subscription (`0xC5`).
    NoUpstreamSubscription,
    /// The request needs a timed interaction (`0xC6`).
    NeedsTimedInteraction,
    /// The cluster does not support the event (`0xC7`).
    UnsupportedEvent,
    /// The request names too many paths (`0xC8`).
    PathsExhausted,
    /// The timed request does not match (`0xC9`).
    TimedRequestMismatch,
    /// The operation needs an armed fail-safe (`0xCA`).
    FailsafeRequired,
    /// The operation is invalid in the current state of the cluster (`0xCB`).
    InvalidInState,
    /// The command produced no response (`0xCC`).
    NoCommandResponse,
    /// The value violates a constraint that depends on the current state (`0xCF`).
    DynamicConstraintError,
    /// The item exists already (`0xD0`).
    AlreadyExists,
    /// The transport type is not valid for the operation (`0xD1`).
    InvalidTransportType,
    /// The write was rejected because it could not be persisted (`0xD2`).
    PersistFailReject,
    /// The write was accepted although it could not be persisted (`0xD3`).
    PersistFailAccept,
}

impl Status {
    /// Every status code in the order of the specification table.
    const ALL: [Status; 34] = [
        Self::Success,
        Self::Failure,
        Self::InvalidSubscription,
        Self::UnsupportedAccess,
        Self::UnsupportedEndpoint,
        Self::InvalidAction,
        Self::UnsupportedCommand,
        Self::InvalidCommand,
        Self::UnsupportedAttribute,
        Self::ConstraintError,
        Self::UnsupportedWrite,
        Self::ResourceExhausted,
        Self::NotFound,
        Self::UnreportableAttribute,
        Self::InvalidDataType,
        Self::UnsupportedRead,
        Self::DataVersionMismatch,
        Self::Timeout,
        Self::Busy,
        Self::AccessRestricted,
        Self::UnsupportedCluster,
        Self::NoUpstreamSubscription,
        Self::NeedsTimedInteraction,
        Self::UnsupportedEvent,
        Self::PathsExhausted,
        Self::TimedRequestMismatch,
        Self::FailsafeRequired,
        Self::InvalidInState,
        Self::NoCommandResponse,
        Self::DynamicConstraintError,
        Self::AlreadyExists,
        Self::InvalidTransportType,
        Self::PersistFailReject,
        Self::PersistFailAccept,
    ];

    /// The numeric status code as it appears on the wire.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0x00,
            Self::Failure => 0x01,
            Self::InvalidSubscription => 0x7D,
            Self::UnsupportedAccess => 0x7E,
            Self::UnsupportedEndpoint => 0x7F,
            Self::InvalidAction => 0x80,
            Self::UnsupportedCommand => 0x81,
            Self::InvalidCommand => 0x85,
            Self::UnsupportedAttribute => 0x86,
            Self::ConstraintError => 0x87,
            Self::UnsupportedWrite => 0x88,
            Self::ResourceExhausted => 0x89,
            Self::NotFound => 0x8B,
            Self::UnreportableAttribute => 0x8C,
            Self::InvalidDataType => 0x8D,
            Self::UnsupportedRead => 0x8F,
            Self::DataVersionMismatch => 0x92,
            Self::Timeout => 0x94,
            Self::Busy => 0x9C,
            Self::AccessRestricted => 0x9D,
            Self::UnsupportedCluster => 0xC3,
            Self::NoUpstreamSubscription => 0xC5,
            Self::NeedsTimedInteraction => 0xC6,
            Self::UnsupportedEvent => 0xC7,
            Self::PathsExhausted => 0xC8,
            Self::TimedRequestMismatch => 0xC9,
            Self::FailsafeRequired => 0xCA,
            Self::InvalidInState => 0xCB,
            Self::NoCommandResponse => 0xCC,
            Self::DynamicConstraintError => 0xCF,
            Self::AlreadyExists => 0xD0,
            Self::InvalidTransportType => 0xD1,
            Self::PersistFailReject => 0xD2,
            Self::PersistFailAccept => 0xD3,
        }
    }

    /// The status for a numeric code; `None` for codes the table does not define.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.code() == code)
    }

    /// Whether the status is [`Status::Success`].
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?} (0x{:02X})", self.code())
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::Status;

    #[test]
    fn codes_round_trip() {
        for status in Status::ALL {
            assert_eq!(Status::from_code(status.code()), Some(status));
        }
    }

    #[test]
    fn codes_are_unique() {
        for (i, a) in Status::ALL.iter().enumerate() {
            for b in &Status::ALL[i + 1..] {
                assert_ne!(a.code(), b.code(), "{a:?} and {b:?} share a code");
            }
        }
    }

    #[test]
    fn well_known_codes_match_the_specification() {
        assert_eq!(Status::Success.code(), 0x00);
        assert_eq!(Status::Failure.code(), 0x01);
        assert_eq!(Status::UnsupportedEndpoint.code(), 0x7F);
        assert_eq!(Status::UnsupportedCluster.code(), 0xC3);
        assert_eq!(Status::UnsupportedAttribute.code(), 0x86);
        assert_eq!(Status::UnsupportedCommand.code(), 0x81);
        assert_eq!(Status::ConstraintError.code(), 0x87);
        assert_eq!(Status::Busy.code(), 0x9C);
        assert_eq!(Status::Timeout.code(), 0x94);
    }

    #[test]
    fn undefined_codes_are_none() {
        assert_eq!(Status::from_code(0x82), None);
        assert_eq!(Status::from_code(0xFF), None);
    }

    #[test]
    fn display_shows_name_and_code() {
        assert_eq!(
            format!("{}", Status::UnsupportedAttribute),
            "UnsupportedAttribute (0x86)"
        );
        assert!(Status::Success.is_success());
        assert!(!Status::Failure.is_success());
    }
}
