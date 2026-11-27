//! Descriptions of what the product puts on the node: device types, clusters and endpoints.
//!
//! A description is data, not behaviour. It tells the engine which ids exist on an endpoint so
//! that it can answer the global attributes (`AttributeList`, `AcceptedCommandList`, ...),
//! route commands and writes to the product and reject paths it does not know. What a cluster
//! means is the product's business.

use alloc::string::String;
use alloc::vec::Vec;

use crate::engine::descriptor;
use crate::id::{AttributeId, ClusterId, CommandId, DeviceTypeId, EndpointId, EventId, VendorId};
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

/// A semantic tag of the Descriptor cluster's `TagList`: what tells two endpoints of the same
/// device type under one parent apart (two sockets of one power strip, two probes of one
/// thermometer).
///
/// A tag is a `tag` within a `namespace`. A standard namespace (`manufacturer_code` `None`) is
/// one of the namespaces of the Matter specification, for example `Common Number` (`0x07`); a
/// manufacturer namespace is defined by the vendor named in `manufacturer_code`. The engine
/// serves the tags as `SemanticTagStruct` entries (see [`SemanticTag::to_value`]); what they
/// mean is the product's business.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SemanticTag {
    /// The vendor that defines the namespace; `None` for a standard namespace.
    pub manufacturer_code: Option<VendorId>,
    /// The namespace id.
    pub namespace: u8,
    /// The tag within the namespace.
    pub tag: u8,
    /// A label, at most [`SemanticTag::MAX_LABEL_LENGTH`] bytes.
    pub label: Option<String>,
}

impl SemanticTag {
    /// The most tags one endpoint may carry (the `TagList` constraint of the Descriptor
    /// cluster).
    pub const MAX_TAGS: usize = 6;

    /// The longest label in bytes (the `Label` constraint of `SemanticTagStruct`).
    pub const MAX_LABEL_LENGTH: usize = 64;

    /// A tag of a standard namespace, without label.
    #[must_use]
    pub const fn standard(namespace: u8, tag: u8) -> Self {
        Self {
            manufacturer_code: None,
            namespace,
            tag,
            label: None,
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The tag as the `SemanticTagStruct` value an engine serves in `TagList`.
    ///
    /// `MfgCode` is always present (null for a standard namespace). `Label` is present when
    /// the tag has one; without one it is null for a manufacturer namespace, where the field is
    /// mandatory, and left out for a standard namespace, where it is optional.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut fields = alloc::vec![
            (
                descriptor::TAG_FIELD_MANUFACTURER_CODE,
                Value::from(self.manufacturer_code.map(VendorId::raw)),
            ),
            (descriptor::TAG_FIELD_NAMESPACE, Value::U8(self.namespace)),
            (descriptor::TAG_FIELD_TAG, Value::U8(self.tag)),
        ];
        match (&self.label, self.manufacturer_code) {
            (Some(label), _) => {
                fields.push((descriptor::TAG_FIELD_LABEL, Value::from(label.as_str())));
            }
            (None, Some(_)) => fields.push((descriptor::TAG_FIELD_LABEL, Value::Null)),
            (None, None) => {}
        }
        Value::Struct(fields)
    }

    /// Why this tag cannot be served, or `None` when it can.
    pub(crate) fn problem(&self) -> Option<String> {
        self.label
            .as_ref()
            .filter(|label| label.len() > Self::MAX_LABEL_LENGTH)
            .map(|label| {
                alloc::format!(
                    "the label of semantic tag {}:{} has {} bytes, at most {} are allowed",
                    self.namespace,
                    self.tag,
                    label.len(),
                    Self::MAX_LABEL_LENGTH
                )
            })
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
///
/// `tags` are the semantic tags the engine serves in the endpoint's Descriptor `TagList`. With
/// tags, the Descriptor lists `TagList` and sets the `TagList` feature (bit 0 of its
/// `FeatureMap`); without tags it does neither. At most [`SemanticTag::MAX_TAGS`] tags are
/// allowed.
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
    /// The semantic tags of the Descriptor `TagList`; empty when the endpoint has none.
    pub tags: Vec<SemanticTag>,
}

impl EndpointDescription {
    /// Adds a semantic tag.
    #[must_use]
    pub fn with_tag(mut self, tag: SemanticTag) -> Self {
        self.tags.push(tag);
        self
    }

    /// The `TagList` value an engine serves for this endpoint, or `None` when the endpoint has
    /// no tags and the Descriptor does not list `TagList`.
    #[must_use]
    pub fn tag_list(&self) -> Option<Value> {
        (!self.tags.is_empty())
            .then(|| Value::List(self.tags.iter().map(SemanticTag::to_value).collect()))
    }

    /// Why the tags of this description cannot be served, or `None` when they can. Engines
    /// refuse such a description with [`Error::InvalidDescription`](crate::Error).
    #[must_use]
    pub fn tag_problem(&self) -> Option<String> {
        if self.tags.len() > SemanticTag::MAX_TAGS {
            return Some(alloc::format!(
                "{} semantic tags, at most {} are allowed",
                self.tags.len(),
                SemanticTag::MAX_TAGS
            ));
        }
        self.tags.iter().find_map(SemanticTag::problem)
    }

    /// The description of `cluster` on this endpoint, if listed.
    #[must_use]
    pub fn cluster(&self, cluster: ClusterId) -> Option<&ClusterDescription> {
        self.clusters.iter().find(|c| c.id == cluster)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use alloc::string::String;

    use super::{ClusterDescription, EndpointDescription, SemanticTag};
    use crate::id::{AttributeId, ClusterId, CommandId, EndpointId, EventId, VendorId};
    use crate::value::Value;

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
            tags: vec![],
        };
        assert_eq!(endpoint.cluster(ClusterId::new(0x0006)), Some(&on_off()));
        assert_eq!(endpoint.cluster(ClusterId::new(0x0008)), None);
    }

    fn bare(id: u16) -> EndpointDescription {
        EndpointDescription {
            id: EndpointId::new(id),
            device_types: vec![],
            clusters: vec![],
            parent: None,
            initial_values: vec![],
            tags: vec![],
        }
    }

    #[test]
    fn semantic_tag_encodes_as_semantic_tag_struct() {
        // Field tags of `SemanticTagStruct`: MfgCode 0, NamespaceID 1, Tag 2, Label 3.
        assert_eq!(
            SemanticTag::standard(0x07, 1).to_value(),
            Value::Struct(vec![(0, Value::Null), (1, Value::U8(7)), (2, Value::U8(1))])
        );
        assert_eq!(
            SemanticTag::standard(0x07, 2).with_label("Left").to_value(),
            Value::Struct(vec![
                (0, Value::Null),
                (1, Value::U8(7)),
                (2, Value::U8(2)),
                (3, Value::from("Left")),
            ])
        );
        let vendor = SemanticTag {
            manufacturer_code: Some(VendorId::new(0xFFF1)),
            namespace: 0x80,
            tag: 3,
            label: None,
        };
        // A manufacturer namespace makes `Label` mandatory; without a label it is null.
        assert_eq!(
            vendor.to_value(),
            Value::Struct(vec![
                (0, Value::U16(0xFFF1)),
                (1, Value::U8(0x80)),
                (2, Value::U8(3)),
                (3, Value::Null),
            ])
        );
    }

    #[test]
    fn tag_list_is_absent_without_tags() {
        assert_eq!(bare(2).tag_list(), None);
        let tagged = bare(2)
            .with_tag(SemanticTag::standard(7, 1))
            .with_tag(SemanticTag::standard(7, 2));
        assert_eq!(
            tagged.tag_list(),
            Some(Value::List(vec![
                SemanticTag::standard(7, 1).to_value(),
                SemanticTag::standard(7, 2).to_value(),
            ]))
        );
    }

    #[test]
    fn tag_problems_name_the_constraint() {
        assert_eq!(bare(2).tag_problem(), None);
        let mut six = bare(2);
        for tag in 0..6 {
            six = six.with_tag(SemanticTag::standard(7, tag));
        }
        assert_eq!(six.tag_problem(), None);
        let seven = six.with_tag(SemanticTag::standard(7, 6));
        assert!(seven.tag_problem().unwrap().contains("at most 6"));

        let fits = bare(2).with_tag(SemanticTag::standard(7, 1).with_label("x".repeat(64)));
        assert_eq!(fits.tag_problem(), None);
        let long: String = "x".repeat(65);
        let too_long = bare(2).with_tag(SemanticTag::standard(7, 1).with_label(long));
        assert!(too_long.tag_problem().unwrap().contains("65 bytes"));
    }
}
