//! Descriptions of what the product puts on the node: device types, clusters and endpoints.
//!
//! A description is data, not behaviour. It tells the engine which ids exist on an endpoint so
//! that it can answer the global attributes (`AttributeList`, `AcceptedCommandList`, ...),
//! route commands and writes to the product and reject paths it does not know. What a cluster
//! means is the product's business.

use alloc::vec::Vec;

use crate::id::{AttributeId, ClusterId, CommandId, DeviceTypeId, EndpointId, EventId};
use crate::value::Value;

/// A device type with the revision of its definition in the Matter device library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceType {
    /// The device type id.
    pub id: DeviceTypeId,
    /// The revision of the device type definition the endpoint conforms to.
    pub revision: u16,
}

/// A cluster server on an endpoint: its id, revision, feature map and the ids it supports.
///
/// `attributes` lists the attributes the product serves. The global attributes
/// (`ClusterRevision`, `FeatureMap`, `AttributeList`, `AcceptedCommandList`,
/// `GeneratedCommandList`, ...) are derived from this description by the engine and must not be
/// listed; see [`AttributeId::is_global`]. Every listed attribute needs an initial value in
/// [`EndpointDescription::initial_values`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterDescription {
    /// The cluster id.
    pub id: ClusterId,
    /// The value of the global `ClusterRevision` attribute.
    pub revision: u16,
    /// The value of the global `FeatureMap` attribute.
    pub feature_map: u32,
    /// The attributes the product serves, without the global ones.
    pub attributes: Vec<AttributeId>,
    /// The commands the cluster accepts; each one reaches the product as a command event.
    pub accepted_commands: Vec<CommandId>,
    /// The commands the cluster generates as responses.
    pub generated_commands: Vec<CommandId>,
    /// The events the product may emit on this cluster.
    pub events: Vec<EventId>,
}

impl ClusterDescription {
    /// Whether `attribute` is listed in this description.
    #[must_use]
    pub fn has_attribute(&self, attribute: AttributeId) -> bool {
        self.attributes.contains(&attribute)
    }

    /// Whether `command` is listed as an accepted command.
    #[must_use]
    pub fn accepts_command(&self, command: CommandId) -> bool {
        self.accepted_commands.contains(&command)
    }

    /// Whether `event` is listed in this description.
    #[must_use]
    pub fn has_event(&self, event: EventId) -> bool {
        self.events.contains(&event)
    }
}

/// An endpoint the product adds to the node.
///
/// The product chooses the endpoint id; it must be unique on the node, must not be the root
/// endpoint and must not be the aggregator endpoint the engine creates for a bridge. `parent`
/// expresses a composed device: `None` puts the endpoint directly under the aggregator (or, on a
/// node without one, at the top level), `Some(id)` makes it a part of the endpoint `id`, which
/// must exist already.
///
/// `initial_values` holds one value for every attribute of every cluster listed in `clusters`;
/// the engine answers reads from these values until the product changes them. A description
/// that lists an attribute without a value, or a value for an attribute that is not listed, is
/// rejected: the engine never invents a value.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointDescription {
    /// The endpoint id, chosen by the product.
    pub id: EndpointId,
    /// The device types of the endpoint, at least one.
    pub device_types: Vec<DeviceType>,
    /// The cluster servers on the endpoint.
    pub clusters: Vec<ClusterDescription>,
    /// The endpoint this one is a part of, for composed devices.
    pub parent: Option<EndpointId>,
    /// The initial value of every listed attribute.
    pub initial_values: Vec<(ClusterId, AttributeId, Value)>,
}

impl EndpointDescription {
    /// The description of `cluster` on this endpoint, if listed.
    #[must_use]
    pub fn cluster(&self, cluster: ClusterId) -> Option<&ClusterDescription> {
        self.clusters.iter().find(|c| c.id == cluster)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::{ClusterDescription, EndpointDescription};
    use crate::id::{AttributeId, ClusterId, CommandId, EndpointId, EventId};

    fn on_off() -> ClusterDescription {
        ClusterDescription {
            id: ClusterId::new(0x0006),
            revision: 6,
            feature_map: 0,
            attributes: vec![AttributeId::new(0)],
            accepted_commands: vec![CommandId::new(0), CommandId::new(1)],
            generated_commands: vec![],
            events: vec![EventId::new(7)],
        }
    }

    #[test]
    fn cluster_description_answers_membership() {
        let cluster = on_off();
        assert!(cluster.has_attribute(AttributeId::new(0)));
        assert!(!cluster.has_attribute(AttributeId::new(1)));
        assert!(cluster.accepts_command(CommandId::new(1)));
        assert!(!cluster.accepts_command(CommandId::new(2)));
        assert!(cluster.has_event(EventId::new(7)));
        assert!(!cluster.has_event(EventId::new(8)));
    }

    #[test]
    fn endpoint_description_finds_its_cluster() {
        let endpoint = EndpointDescription {
            id: EndpointId::new(3),
            device_types: vec![],
            clusters: vec![on_off()],
            parent: None,
            initial_values: vec![],
        };
        assert_eq!(endpoint.cluster(ClusterId::new(0x0006)), Some(&on_off()));
        assert_eq!(endpoint.cluster(ClusterId::new(0x0008)), None);
    }
}
