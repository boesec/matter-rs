//! The error type of the engine facade.

use alloc::string::String;

use crate::engine::Correlation;
use crate::id::{AttributeId, ClusterId, EndpointId, EventId, FabricIndex};
use crate::storage::StorageError;

/// Error of the engine facade.
///
/// Every library crate of the workspace has exactly one public error type like this one: a
/// `#[non_exhaustive]` enum that implements [`core::error::Error`] and [`core::fmt::Display`]
/// without the standard library. Crates above this one wrap it in a variant of their own error
/// instead of re-exporting it.
///
/// The variants grow with the facade. New variants are not a breaking change for callers because
/// the enum is non-exhaustive. Messages never contain attribute values or secrets, only ids.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The engine behind the facade does not support the named operation.
    ///
    /// Engines differ in what they implement; the facade reports the gap instead of pretending.
    #[error("the engine does not support {operation}")]
    Unsupported {
        /// Short, stable name of the operation, for diagnostics.
        operation: &'static str,
    },
    /// The node is not running: it was stopped, or the operation needs a started node.
    #[error("the node is not running")]
    NotRunning,
    /// The engine already runs a node; stop it before starting another.
    #[error("the engine is already running a node")]
    AlreadyRunning,
    /// The identity or network configuration cannot be used to start a node.
    #[error("invalid node identity: {reason}")]
    InvalidIdentity {
        /// What is wrong, without the offending value when the value is a secret.
        reason: String,
    },
    /// No endpoint with this id exists.
    #[error("unknown endpoint {0}")]
    UnknownEndpoint(EndpointId),
    /// An endpoint with this id exists already.
    #[error("endpoint {0} exists already")]
    DuplicateEndpoint(EndpointId),
    /// The endpoint description is inconsistent and was refused as a whole.
    #[error("invalid description of endpoint {endpoint}: {reason}")]
    InvalidDescription {
        /// The endpoint the description is for.
        endpoint: EndpointId,
        /// What is wrong, naming the cluster and attribute where one is involved.
        reason: String,
    },
    /// The endpoint exists but does not list this cluster or this attribute.
    #[error("endpoint {endpoint} has no attribute {attribute} in cluster {cluster}")]
    UnknownAttribute {
        /// The endpoint.
        endpoint: EndpointId,
        /// The cluster that was named.
        cluster: ClusterId,
        /// The attribute that was named.
        attribute: AttributeId,
    },
    /// The endpoint exists but does not list this cluster or this event.
    #[error("endpoint {endpoint} has no event {event} in cluster {cluster}")]
    UnknownEvent {
        /// The endpoint.
        endpoint: EndpointId,
        /// The cluster that was named.
        cluster: ClusterId,
        /// The event that was named.
        event: EventId,
    },
    /// No fabric with this index exists.
    #[error("unknown fabric {0}")]
    UnknownFabric(FabricIndex),
    /// The engine has no room for another of the named resource.
    #[error("capacity exceeded: no room for another of {what}")]
    CapacityExceeded {
        /// The resource, for example `"endpoints"`.
        what: &'static str,
    },
    /// Nothing waits for a response with this correlation.
    #[error("no pending request with correlation {0}")]
    UnknownCorrelation(Correlation),
    /// The storage failed.
    #[error("storage error")]
    Storage(#[from] StorageError),
    /// An implementation-specific failure, as text. Never a foreign type, so that the facade
    /// stays free of any engine's types.
    #[error("engine failure: {0}")]
    Engine(String),
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::Error;
    use crate::engine::Correlation;
    use crate::id::{AttributeId, ClusterId, EndpointId, FabricIndex};
    use crate::storage::StorageError;

    #[test]
    fn error_is_core_error() {
        fn assert_error<E: core::error::Error + Send + Sync + 'static>() {}
        assert_error::<Error>();
    }

    #[test]
    fn unsupported_names_the_operation() {
        let error = Error::Unsupported {
            operation: "group messaging",
        };
        assert_eq!(
            error.to_string(),
            "the engine does not support group messaging"
        );
    }

    #[test]
    fn messages_name_ids() {
        assert_eq!(
            Error::UnknownEndpoint(EndpointId::new(5)).to_string(),
            "unknown endpoint 5"
        );
        assert_eq!(
            Error::DuplicateEndpoint(EndpointId::new(5)).to_string(),
            "endpoint 5 exists already"
        );
        assert_eq!(
            Error::UnknownAttribute {
                endpoint: EndpointId::new(5),
                cluster: ClusterId::new(6),
                attribute: AttributeId::new(0),
            }
            .to_string(),
            "endpoint 5 has no attribute 0x0000 in cluster 0x0006"
        );
        assert_eq!(
            Error::UnknownFabric(FabricIndex::new(2)).to_string(),
            "unknown fabric 2"
        );
        assert_eq!(
            Error::CapacityExceeded { what: "endpoints" }.to_string(),
            "capacity exceeded: no room for another of endpoints"
        );
        assert_eq!(
            Error::UnknownCorrelation(Correlation::new(3)).to_string(),
            "no pending request with correlation #3"
        );
        assert_eq!(
            Error::Engine(String::from("socket closed")).to_string(),
            "engine failure: socket closed"
        );
    }

    #[test]
    fn storage_error_is_wrapped_with_source() {
        let error = Error::from(StorageError::Backend {
            message: String::from("disk full"),
        });
        assert_eq!(error.to_string(), "storage error");
        let source = core::error::Error::source(&error).expect("has a source");
        assert_eq!(source.to_string(), "storage back-end failed: disk full");
    }
}
