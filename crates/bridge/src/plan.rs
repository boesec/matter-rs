//! From a device specification to the endpoints the engine gets.
//!
//! A device with one part becomes one endpoint that carries the part's device type and the
//! Bridged Node device type, with the Bridged Device Basic Information cluster next to the
//! part's clusters. A device with several parts becomes a Bridged Node parent endpoint (Bridged
//! Device Basic Information only) and one child endpoint per part. Children that share a kind, or
//! that have a label, get a semantic tag of the `Common Number` namespace unless the part names
//! its own tags.
//!
//! Every attribute of every cluster gets its initial value here: the value the adapter supplied,
//! else the value a controller wrote earlier and the bridge persisted, else the default the
//! cluster specification names. An attribute with none of the three is an error that names it.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use matter_rs_engine::{
    AttributeId, ClusterDescription, ClusterId, DeviceType, EndpointDescription, EndpointId, Kind,
    Value, bridged_device,
};

use crate::error::Error;
use crate::ids::{
    bridged_device_basic_information as bdbi, color_control, descriptor, device_type,
};
use crate::kinds::{Access, AttributeSpec, ClusterSpec, DeviceKind, Initial};
use crate::model::{
    Attribute, DeviceKey, DeviceSpec, InitialValues, MAX_PARTS, PartIndex, SemanticTag,
};
use crate::numbering::Slot;

/// One endpoint of a bridged device, as the bridge describes it to the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointPlan {
    /// The endpoint number.
    pub endpoint: EndpointId,
    /// The part this endpoint serves, or `None` for the parent endpoint of a composed device.
    pub part: Option<PartIndex>,
    /// The device kind of the part; `None` for the parent endpoint.
    pub kind: Option<DeviceKind>,
    /// The semantic tags of the endpoint's Descriptor `TagList`.
    ///
    /// The engine facade has no way to carry them yet, so they are computed here and exposed, but
    /// do not reach the Descriptor cluster.
    pub tags: Vec<SemanticTag>,
    /// The description the engine receives.
    pub description: EndpointDescription,
}

/// An endpoint with the specifications the bridge keeps for it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlannedEndpoint {
    /// The public view.
    pub plan: EndpointPlan,
    /// The part value of the endpoint's slot, for storage keys.
    pub slot_part: u16,
    /// The clusters with their attribute specifications.
    pub clusters: Vec<ClusterSpec>,
    /// The current values.
    pub values: BTreeMap<(ClusterId, AttributeId), Value>,
}

impl PlannedEndpoint {
    /// The specification of an attribute on this endpoint.
    pub(crate) fn attribute(&self, attribute: Attribute) -> Option<&AttributeSpec> {
        let (cluster, _) = attribute.path();
        self.clusters
            .iter()
            .find(|c| c.id == cluster)
            .and_then(|c| c.attribute(attribute))
    }

    /// The cluster specification of a cluster on this endpoint.
    pub(crate) fn cluster(&self, cluster: ClusterId) -> Option<&ClusterSpec> {
        self.clusters.iter().find(|c| c.id == cluster)
    }
}

/// The endpoint numbers a device was assigned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Numbers {
    /// The parent endpoint of a composed device.
    pub parent: Option<EndpointId>,
    /// One endpoint per part.
    pub parts: Vec<EndpointId>,
}

/// A source of persisted attribute values: the slot part, the attribute, the value if any.
pub(crate) type Persisted<'a> = &'a dyn Fn(u16, Attribute) -> Option<Value>;

/// Plans the endpoints of a device.
///
/// # Errors
///
/// [`Error::InvalidSpec`] for an inconsistent specification, [`Error::UnknownPart`] and
/// [`Error::UnknownAttribute`] for an initial value that addresses nothing,
/// [`Error::MissingInitialValue`] for a required value that is missing, [`Error::InvalidValue`]
/// for a value that does not fit its attribute.
pub(crate) fn plan_device(
    spec: &DeviceSpec,
    initial: &InitialValues,
    numbers: &Numbers,
    persisted: Persisted<'_>,
) -> Result<Vec<PlannedEndpoint>, Error> {
    validate_spec(spec)?;
    debug_assert_eq!(numbers.parts.len(), spec.parts.len());
    for (part, attribute, _) in initial.iter() {
        let part_spec = spec
            .parts
            .get(usize::from(part))
            .ok_or(Error::UnknownPart {
                key: spec.key.clone(),
                part,
            })?;
        let known = part_spec
            .kind
            .clusters()
            .iter()
            .chain(extra_cluster_specs(part_spec).iter())
            .any(|cluster| cluster.attribute(canonical(attribute)).is_some());
        if !known {
            return Err(Error::UnknownAttribute {
                key: spec.key.clone(),
                part,
                attribute,
            });
        }
    }

    let mut endpoints = Vec::with_capacity(spec.parts.len() + 1);
    let composed = spec.is_composed();
    if let Some(parent) = numbers.parent {
        let clusters = alloc::vec![bdbi_cluster(spec, initial)];
        let values = resolve_values(spec, None, Slot::PARENT, &clusters, initial, persisted)?;
        endpoints.push(assemble(
            parent,
            None,
            None,
            Slot::PARENT,
            alloc::vec![bridged_node()],
            None,
            Vec::new(),
            clusters,
            values,
        ));
    }
    for (index, part) in spec.parts.iter().enumerate() {
        // Parts are bounded by `MAX_PARTS`, so the index fits.
        let part_index = PartIndex::try_from(index).unwrap_or(PartIndex::MAX);
        let mut clusters = part.kind.clusters();
        clusters.extend(extra_cluster_specs(part));
        let mut device_types = alloc::vec![part.kind.device_type()];
        if !composed {
            clusters.push(bdbi_cluster(spec, initial));
            device_types.push(bridged_node());
        }
        let values = resolve_values(
            spec,
            Some(part_index),
            part_index,
            &clusters,
            initial,
            persisted,
        )?;
        let tags = if composed {
            tags_of(spec, index)
        } else {
            Vec::new()
        };
        endpoints.push(assemble(
            numbers.parts[index],
            Some(part_index),
            Some(part.kind),
            part_index,
            device_types,
            numbers.parent,
            tags,
            clusters,
            values,
        ));
    }
    Ok(endpoints)
}

fn validate_spec(spec: &DeviceSpec) -> Result<(), Error> {
    let invalid = |reason: &str| Error::InvalidSpec {
        key: spec.key.clone(),
        reason: String::from(reason),
    };
    if spec.parts.is_empty() {
        return Err(invalid("a device needs at least one part"));
    }
    if spec.parts.len() > MAX_PARTS {
        return Err(invalid("a device has at most 64 parts"));
    }
    if spec.name.chars().count() > bdbi::MAX_LABEL_LENGTH {
        return Err(invalid("the name is longer than 32 characters"));
    }
    for (index, part) in spec.parts.iter().enumerate() {
        if part
            .label
            .as_ref()
            .is_some_and(|label| label.chars().count() > bdbi::MAX_LABEL_LENGTH)
        {
            return Err(Error::InvalidSpec {
                key: spec.key.clone(),
                reason: alloc::format!("the label of part {index} is longer than 32 characters"),
            });
        }
        let own: Vec<ClusterId> = part.kind.clusters().iter().map(|c| c.id).collect();
        for (i, extra) in part.extra_clusters.iter().enumerate() {
            let reserved = extra.id == descriptor::CLUSTER
                || extra.id == bridged_device::CLUSTER
                || own.contains(&extra.id);
            if reserved {
                return Err(Error::InvalidSpec {
                    key: spec.key.clone(),
                    reason: alloc::format!(
                        "part {index} lists cluster {} as an extra cluster, but the bridge serves it",
                        extra.id
                    ),
                });
            }
            if part.extra_clusters[..i].iter().any(|c| c.id == extra.id) {
                return Err(Error::InvalidSpec {
                    key: spec.key.clone(),
                    reason: alloc::format!("part {index} lists extra cluster {} twice", extra.id),
                });
            }
        }
    }
    Ok(())
}

/// Replaces a raw attribute that names a typed one with the typed one.
pub(crate) fn canonical(attribute: Attribute) -> Attribute {
    match attribute {
        Attribute::Raw(cluster, id) => Attribute::from_path(cluster, id),
        typed => typed,
    }
}

/// The specifications of the extra clusters of a part: every attribute raw and required.
fn extra_cluster_specs(part: &crate::model::PartSpec) -> Vec<ClusterSpec> {
    part.extra_clusters
        .iter()
        .map(|cluster| ClusterSpec {
            id: cluster.id,
            revision: cluster.revision,
            feature_map: cluster.feature_map,
            attributes: cluster
                .attributes
                .iter()
                .map(|attribute| AttributeSpec::raw(Attribute::Raw(cluster.id, *attribute)))
                .collect(),
            commands: cluster.accepted_commands.clone(),
            events: cluster.events.clone(),
        })
        .collect()
}

fn bridged_node() -> DeviceType {
    let (id, revision) = device_type::BRIDGED_NODE;
    DeviceType { id, revision }
}

/// The Bridged Device Basic Information cluster of a device's top endpoint.
fn bdbi_cluster(spec: &DeviceSpec, initial: &InitialValues) -> ClusterSpec {
    ClusterSpec {
        id: bdbi::CLUSTER,
        revision: bdbi::REVISION,
        feature_map: 0,
        attributes: alloc::vec![
            AttributeSpec {
                attribute: Attribute::Raw(bdbi::CLUSTER, bdbi::NODE_LABEL),
                kind: Some(Kind::Utf8),
                nullable: false,
                range: None,
                initial: Initial::Default(Value::from(spec.name.as_str())),
                access: Access::Persisted,
            },
            AttributeSpec {
                attribute: Attribute::Raw(bdbi::CLUSTER, bdbi::REACHABLE),
                kind: Some(Kind::Bool),
                nullable: false,
                range: None,
                initial: Initial::Default(Value::Bool(initial.is_reachable())),
                access: Access::ReadOnly,
            },
            AttributeSpec {
                attribute: Attribute::Raw(bdbi::CLUSTER, bdbi::UNIQUE_ID),
                kind: Some(Kind::Utf8),
                nullable: false,
                range: None,
                initial: Initial::Default(Value::Utf8(unique_id(&spec.key))),
                access: Access::ReadOnly,
            },
        ],
        commands: Vec::new(),
        events: alloc::vec![bridged_device::REACHABLE_CHANGED],
    }
}

/// The `UniqueID` of a device: the key itself when it fits the attribute (at most 32 bytes of
/// letters, digits, `-`, `_`, `.` and `:`), otherwise the 64-bit FNV-1a hash of the key as 16
/// hexadecimal digits. Both are stable for the lifetime of the key.
#[must_use]
pub fn unique_id(key: &DeviceKey) -> String {
    let text = key.as_str();
    let plain = text.len() <= bdbi::MAX_LABEL_LENGTH
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if plain {
        return String::from(text);
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    alloc::format!("{hash:016x}")
}

/// The initial value of every attribute of every cluster of an endpoint.
fn resolve_values(
    spec: &DeviceSpec,
    part: Option<PartIndex>,
    slot_part: u16,
    clusters: &[ClusterSpec],
    initial: &InitialValues,
    persisted: Persisted<'_>,
) -> Result<BTreeMap<(ClusterId, AttributeId), Value>, Error> {
    let mut values = BTreeMap::new();
    for cluster in clusters {
        for attribute in &cluster.attributes {
            let path = attribute.attribute.path();
            let supplied = part.and_then(|part| {
                initial
                    .iter()
                    .find(|(p, a, _)| *p == part && canonical(*a) == attribute.attribute)
                    .map(|(_, _, value)| value.clone())
            });
            let stored = if attribute.access == Access::Persisted {
                persisted(slot_part, attribute.attribute)
            } else {
                None
            };
            let value = match (stored, supplied, &attribute.initial) {
                (Some(stored), _, _) => stored,
                (None, Some(supplied), _) => supplied,
                (None, None, Initial::Default(default)) => default.clone(),
                (None, None, Initial::Required) => {
                    return Err(Error::MissingInitialValue {
                        key: spec.key.clone(),
                        part: part.unwrap_or(0),
                        attribute: attribute.attribute,
                    });
                }
            };
            let value = attribute
                .check(&value)
                .map_err(|reason| Error::InvalidValue {
                    key: spec.key.clone(),
                    part: part.unwrap_or(0),
                    attribute: attribute.attribute,
                    reason,
                })?;
            values.insert(path, value);
        }
    }
    // `CoupleColorTempToLevelMinMireds` is derived from the physical minimum.
    if let Some(min) = values
        .get(&(
            color_control::CLUSTER,
            color_control::COLOR_TEMP_PHYSICAL_MIN_MIREDS,
        ))
        .cloned()
    {
        values.insert(
            (
                color_control::CLUSTER,
                color_control::COUPLE_COLOR_TEMP_TO_LEVEL_MIN_MIREDS,
            ),
            min,
        );
    }
    Ok(values)
}

/// The semantic tags of a child endpoint of a composed device.
fn tags_of(spec: &DeviceSpec, index: usize) -> Vec<SemanticTag> {
    let part = &spec.parts[index];
    if !part.tags.is_empty() {
        return part.tags.clone();
    }
    let same_kind: Vec<usize> = spec
        .parts
        .iter()
        .enumerate()
        .filter(|(_, other)| other.kind == part.kind)
        .map(|(i, _)| i)
        .collect();
    if same_kind.len() < 2 && part.label.is_none() {
        return Vec::new();
    }
    let ordinal = same_kind.iter().position(|i| *i == index).unwrap_or(0) + 1;
    let tag = u8::try_from(ordinal)
        .unwrap_or(descriptor::MAX_NUMBER_TAG)
        .min(descriptor::MAX_NUMBER_TAG);
    let mut tag = SemanticTag::standard(descriptor::NAMESPACE_COMMON_NUMBER, tag);
    tag.label = part.label.clone();
    alloc::vec![tag]
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    endpoint: EndpointId,
    part: Option<PartIndex>,
    kind: Option<DeviceKind>,
    slot_part: u16,
    device_types: Vec<DeviceType>,
    parent: Option<EndpointId>,
    tags: Vec<SemanticTag>,
    clusters: Vec<ClusterSpec>,
    values: BTreeMap<(ClusterId, AttributeId), Value>,
) -> PlannedEndpoint {
    let description = EndpointDescription {
        id: endpoint,
        device_types,
        clusters: clusters
            .iter()
            .map(|cluster| ClusterDescription {
                id: cluster.id,
                revision: cluster.revision,
                feature_map: cluster.feature_map,
                attributes: cluster
                    .attributes
                    .iter()
                    .map(|a| a.attribute.path().1)
                    .collect(),
                accepted_commands: cluster.commands.clone(),
                generated_commands: Vec::new(),
                events: cluster.events.clone(),
            })
            .collect(),
        parent,
        initial_values: values
            .iter()
            .map(|((cluster, attribute), value)| (*cluster, *attribute, value.clone()))
            .collect(),
    };
    PlannedEndpoint {
        plan: EndpointPlan {
            endpoint,
            part,
            kind,
            tags,
            description,
        },
        slot_part,
        clusters,
        values,
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use matter_rs_engine::{AttributeId, ClusterDescription, ClusterId, EndpointId, Value};

    use super::{Numbers, PlannedEndpoint, plan_device, unique_id};
    use crate::error::Error;
    use crate::ids::{bridged_device_basic_information as bdbi, color_control, device_type};
    use crate::kinds::DeviceKind;
    use crate::model::{Attribute, DeviceKey, DeviceSpec, InitialValues, PartSpec, SemanticTag};

    fn key(name: &str) -> DeviceKey {
        DeviceKey::new(name).unwrap()
    }

    fn none(_: u16, _: Attribute) -> Option<Value> {
        None
    }

    fn single(kind: DeviceKind, initial: InitialValues) -> Result<Vec<PlannedEndpoint>, Error> {
        plan_device(
            &DeviceSpec::single(key("dev"), "Device", kind),
            &initial,
            &Numbers {
                parent: None,
                parts: vec![EndpointId::new(2)],
            },
            &none,
        )
    }

    #[test]
    fn single_part_device_is_one_endpoint_with_bridged_node() {
        let endpoints = single(
            DeviceKind::OnOffLight,
            InitialValues::new().value(Attribute::OnOff, true),
        )
        .unwrap();
        assert_eq!(endpoints.len(), 1);
        let light = &endpoints[0];
        assert_eq!(light.plan.endpoint, EndpointId::new(2));
        assert_eq!(light.plan.part, Some(0));
        assert_eq!(light.plan.kind, Some(DeviceKind::OnOffLight));
        assert!(light.plan.tags.is_empty());
        assert_eq!(light.plan.description.parent, None);
        assert_eq!(
            light.plan.description.device_types,
            vec![
                DeviceKind::OnOffLight.device_type(),
                matter_rs_engine::DeviceType {
                    id: device_type::BRIDGED_NODE.0,
                    revision: device_type::BRIDGED_NODE.1
                }
            ]
        );
        assert!(light.cluster(bdbi::CLUSTER).is_some());
        assert_eq!(
            light.values.get(&(bdbi::CLUSTER, bdbi::NODE_LABEL)),
            Some(&Value::from("Device"))
        );
        assert_eq!(
            light.values.get(&(bdbi::CLUSTER, bdbi::UNIQUE_ID)),
            Some(&Value::from("dev"))
        );
        assert_eq!(
            light.values.get(&Attribute::OnOff.path()),
            Some(&Value::Bool(true))
        );
        // Every listed attribute has a value and every value is listed.
        let description = &light.plan.description;
        for cluster in &description.clusters {
            for attribute in &cluster.attributes {
                assert!(
                    description
                        .initial_values
                        .iter()
                        .any(|(c, a, _)| *c == cluster.id && a == attribute)
                );
            }
        }
        assert_eq!(
            description.initial_values.len(),
            description
                .clusters
                .iter()
                .map(|c| c.attributes.len())
                .sum::<usize>()
        );
    }

    #[test]
    fn missing_required_value_names_the_attribute() {
        let error = single(
            DeviceKind::DimmableLight,
            InitialValues::new().value(Attribute::OnOff, false),
        )
        .unwrap_err();
        assert_eq!(
            error,
            Error::MissingInitialValue {
                key: key("dev"),
                part: 0,
                attribute: Attribute::CurrentLevel,
            }
        );
    }

    #[test]
    fn wrong_or_unknown_values_are_rejected() {
        let error = single(
            DeviceKind::OnOffLight,
            InitialValues::new().value(Attribute::OnOff, Value::U8(1)),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidValue {
                attribute: Attribute::OnOff,
                ..
            }
        ));

        let error = single(
            DeviceKind::OnOffLight,
            InitialValues::new()
                .value(Attribute::OnOff, true)
                .value(Attribute::Temperature, Value::I16(1)),
        )
        .unwrap_err();
        assert_eq!(
            error,
            Error::UnknownAttribute {
                key: key("dev"),
                part: 0,
                attribute: Attribute::Temperature
            }
        );

        let error = single(
            DeviceKind::OnOffLight,
            InitialValues::new()
                .value(Attribute::OnOff, true)
                .with(1, Attribute::OnOff, true),
        )
        .unwrap_err();
        assert_eq!(
            error,
            Error::UnknownPart {
                key: key("dev"),
                part: 1
            }
        );
    }

    #[test]
    fn raw_initial_values_address_typed_attributes_too() {
        let (cluster, attribute) = Attribute::OnOff.path();
        let endpoints = single(
            DeviceKind::OnOffLight,
            InitialValues::new().value(Attribute::Raw(cluster, attribute), true),
        )
        .unwrap();
        assert_eq!(
            endpoints[0].values.get(&(cluster, attribute)),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn persisted_values_win_over_supplied_and_defaults() {
        let persisted = |slot: u16, attribute: Attribute| {
            assert_eq!(slot, 0);
            match attribute {
                Attribute::OnLevel => Some(Value::U8(77)),
                Attribute::Raw(c, a) if (c, a) == (bdbi::CLUSTER, bdbi::NODE_LABEL) => {
                    Some(Value::from("Renamed"))
                }
                _ => None,
            }
        };
        let endpoints = plan_device(
            &DeviceSpec::single(key("dev"), "Device", DeviceKind::DimmableLight),
            &InitialValues::new()
                .value(Attribute::OnOff, false)
                .value(Attribute::CurrentLevel, Value::U8(10))
                .value(Attribute::OnLevel, Value::U8(5)),
            &Numbers {
                parent: None,
                parts: vec![EndpointId::new(2)],
            },
            &persisted,
        )
        .unwrap();
        let values = &endpoints[0].values;
        assert_eq!(values.get(&Attribute::OnLevel.path()), Some(&Value::U8(77)));
        assert_eq!(
            values.get(&(bdbi::CLUSTER, bdbi::NODE_LABEL)),
            Some(&Value::from("Renamed"))
        );
        // A read-only attribute is never taken from the storage.
        assert_eq!(
            values.get(&Attribute::CurrentLevel.path()),
            Some(&Value::U8(10))
        );
    }

    #[test]
    fn couple_color_temp_follows_the_physical_minimum() {
        let endpoints = single(
            DeviceKind::ColorTemperatureLight,
            InitialValues::new()
                .value(Attribute::OnOff, true)
                .value(Attribute::CurrentLevel, Value::U8(100))
                .value(Attribute::ColorTemperatureMireds, Value::U16(300))
                .value(Attribute::ColorTempPhysicalMinMireds, Value::U16(153))
                .value(Attribute::ColorTempPhysicalMaxMireds, Value::U16(500)),
        )
        .unwrap();
        assert_eq!(
            endpoints[0].values.get(&(
                color_control::CLUSTER,
                color_control::COUPLE_COLOR_TEMP_TO_LEVEL_MIN_MIREDS
            )),
            Some(&Value::U16(153))
        );
    }

    fn composed() -> DeviceSpec {
        DeviceSpec {
            key: key("strip"),
            name: String::from("Power strip"),
            parts: vec![
                PartSpec::new(DeviceKind::OnOffPlugInUnit),
                PartSpec::new(DeviceKind::OnOffPlugInUnit).with_label("Lamp"),
                PartSpec::new(DeviceKind::TemperatureSensor),
                PartSpec::new(DeviceKind::ContactSensor).with_tag(SemanticTag::standard(0x08, 1)),
            ],
        }
    }

    fn composed_initial() -> InitialValues {
        InitialValues::new()
            .with(0, Attribute::OnOff, false)
            .with(1, Attribute::OnOff, true)
            .with(2, Attribute::Temperature, Value::I16(2000))
            .with(3, Attribute::StateValue, true)
    }

    #[test]
    fn composed_device_has_a_parent_and_one_child_per_part() {
        let endpoints = plan_device(
            &composed(),
            &composed_initial(),
            &Numbers {
                parent: Some(EndpointId::new(5)),
                parts: vec![
                    EndpointId::new(6),
                    EndpointId::new(7),
                    EndpointId::new(8),
                    EndpointId::new(9),
                ],
            },
            &none,
        )
        .unwrap();
        assert_eq!(endpoints.len(), 5);
        let parent = &endpoints[0];
        assert_eq!(parent.plan.endpoint, EndpointId::new(5));
        assert_eq!(parent.plan.part, None);
        assert_eq!(parent.plan.kind, None);
        assert_eq!(parent.plan.description.parent, None);
        assert_eq!(parent.plan.description.device_types.len(), 1);
        assert_eq!(
            parent.plan.description.device_types[0].id,
            device_type::BRIDGED_NODE.0
        );
        assert_eq!(parent.plan.description.clusters.len(), 1);
        assert_eq!(parent.plan.description.clusters[0].id, bdbi::CLUSTER);
        for (i, child) in endpoints[1..].iter().enumerate() {
            assert_eq!(
                child.plan.endpoint,
                EndpointId::new(6 + u16::try_from(i).unwrap())
            );
            assert_eq!(child.plan.part, Some(u16::try_from(i).unwrap()));
            assert_eq!(child.plan.description.parent, Some(EndpointId::new(5)));
            assert_eq!(child.plan.description.device_types.len(), 1);
            assert!(child.cluster(bdbi::CLUSTER).is_none());
        }
    }

    #[test]
    fn same_kind_siblings_and_labelled_parts_get_number_tags() {
        let endpoints = plan_device(
            &composed(),
            &composed_initial(),
            &Numbers {
                parent: Some(EndpointId::new(5)),
                parts: vec![
                    EndpointId::new(6),
                    EndpointId::new(7),
                    EndpointId::new(8),
                    EndpointId::new(9),
                ],
            },
            &none,
        )
        .unwrap();
        assert_eq!(endpoints[1].plan.tags, vec![SemanticTag::standard(0x07, 1)]);
        assert_eq!(
            endpoints[2].plan.tags,
            vec![SemanticTag::standard(0x07, 2).with_label("Lamp")]
        );
        // A lone kind without label has no tag.
        assert!(endpoints[3].plan.tags.is_empty());
        // Explicit tags are taken as given.
        assert_eq!(endpoints[4].plan.tags, vec![SemanticTag::standard(0x08, 1)]);
    }

    #[test]
    fn invalid_specifications_are_refused() {
        let numbers = Numbers {
            parent: None,
            parts: vec![EndpointId::new(2)],
        };
        let mut spec = DeviceSpec::single(key("dev"), "Device", DeviceKind::OnOffLight);
        spec.parts.clear();
        assert!(matches!(
            plan_device(&spec, &InitialValues::new(), &numbers, &none),
            Err(Error::InvalidSpec { reason, .. }) if reason.contains("at least one part")
        ));

        let spec = DeviceSpec::single(key("dev"), "n".repeat(33), DeviceKind::OnOffLight);
        assert!(matches!(
            plan_device(&spec, &InitialValues::new(), &numbers, &none),
            Err(Error::InvalidSpec { reason, .. }) if reason.contains("32 characters")
        ));

        let on_off = ClusterDescription {
            id: Attribute::OnOff.path().0,
            revision: 6,
            feature_map: 0,
            attributes: vec![],
            accepted_commands: vec![],
            generated_commands: vec![],
            events: vec![],
        };
        let mut spec = DeviceSpec::single(key("dev"), "Device", DeviceKind::OnOffLight);
        spec.parts[0].extra_clusters.push(on_off);
        assert!(matches!(
            plan_device(&spec, &InitialValues::new(), &numbers, &none),
            Err(Error::InvalidSpec { reason, .. }) if reason.contains("bridge serves it")
        ));
    }

    #[test]
    fn extra_clusters_need_raw_initial_values() {
        let vendor = ClusterId::new(0xFFF1_FC01);
        let extra = ClusterDescription {
            id: vendor,
            revision: 1,
            feature_map: 0,
            attributes: vec![AttributeId::new(0)],
            accepted_commands: vec![],
            generated_commands: vec![],
            events: vec![],
        };
        let mut spec = DeviceSpec::single(key("dev"), "Device", DeviceKind::OnOffLight);
        spec.parts[0].extra_clusters.push(extra);
        let numbers = Numbers {
            parent: None,
            parts: vec![EndpointId::new(2)],
        };
        let error = plan_device(
            &spec,
            &InitialValues::new().value(Attribute::OnOff, true),
            &numbers,
            &none,
        )
        .unwrap_err();
        assert_eq!(
            error,
            Error::MissingInitialValue {
                key: key("dev"),
                part: 0,
                attribute: Attribute::Raw(vendor, AttributeId::new(0)),
            }
        );
        let endpoints = plan_device(
            &spec,
            &InitialValues::new()
                .value(Attribute::OnOff, true)
                .value(Attribute::Raw(vendor, AttributeId::new(0)), Value::U32(9)),
            &numbers,
            &none,
        )
        .unwrap();
        assert_eq!(
            endpoints[0].values.get(&(vendor, AttributeId::new(0))),
            Some(&Value::U32(9))
        );
    }

    #[test]
    fn unique_id_is_the_key_when_it_fits_and_a_hash_otherwise() {
        assert_eq!(
            unique_id(&key("lamp-1.living:room_A")),
            "lamp-1.living:room_A"
        );
        let spaced = unique_id(&key("living room lamp"));
        assert_eq!(spaced.len(), 16);
        assert!(spaced.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(spaced, unique_id(&key("living room lamp")));
        let long = unique_id(&key(&"k".repeat(40)));
        assert_eq!(long.len(), 16);
        assert_ne!(long, spaced);
    }
}
