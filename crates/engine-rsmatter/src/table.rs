//! The endpoint table: the facade's endpoint descriptions as rs-matter node metadata, with the
//! values the product set and a data version per cluster.
//!
//! rs-matter borrows every endpoint's device types and cluster metadata for the node's
//! lifetime, so the table turns each description into `'static` metadata once and keeps it in a
//! cache keyed by shape. A shape is the full set of ids of a cluster or of an endpoint's cluster
//! list, so the leaked memory is bounded by the number of distinct shapes the product uses, not
//! by the number of endpoints added over time. Removing and re-adding an endpoint with the same
//! shape costs nothing new.
//!
//! The table holds the root endpoint (rs-matter's own system clusters), the aggregator and the
//! product's endpoints, each with a Descriptor cluster the engine serves itself.

use std::collections::{BTreeMap, HashMap};

use matter_rs_engine::{
    AttributeId, AttributeValue, ClusterId, EndpointDescription, EndpointId, Error, EventId,
    ROOT_ENDPOINT, Value,
};
use rs_matter::dm::clusters::decl;
use rs_matter::dm::clusters::desc::{ClusterHandler as _, DescHandler};
use rs_matter::dm::devices::DEV_TYPE_AGGREGATOR;
use rs_matter::dm::{
    ACCEPTED_COMMAND_LIST, ATTRIBUTE_LIST, Access, Attribute, CLUSTER_REVISION, Cluster, Command,
    Dataver, DeviceType, Endpoint, Event, FEATURE_MAP, GENERATED_COMMAND_LIST, Quality,
};
use rs_matter::error::ErrorCode;

/// The Descriptor cluster every endpoint carries; the engine serves it.
pub(crate) const DESCRIPTOR: ClusterId = ClusterId::new(0x001D);
/// `Descriptor::DeviceTypeList`.
const DEVICE_TYPE_LIST: AttributeId = AttributeId::new(0x0000);
/// `Descriptor::ServerList`.
const SERVER_LIST: AttributeId = AttributeId::new(0x0001);
/// `Descriptor::ClientList`.
const CLIENT_LIST: AttributeId = AttributeId::new(0x0002);
/// `Descriptor::PartsList`.
const PARTS_LIST: AttributeId = AttributeId::new(0x0003);

/// The root endpoint with rs-matter's system clusters for an Ethernet (IP only) node.
const ROOT: Endpoint<'static> = rs_matter::root_endpoint!(eth);

/// The device types of the aggregator endpoint.
const AGGREGATOR_DEVICE_TYPES: &[DeviceType] = &[DEV_TYPE_AGGREGATOR];

/// The clusters of the aggregator endpoint: the Descriptor only.
const AGGREGATOR_CLUSTERS: &[Cluster<'static>] = &[DescHandler::CLUSTER];

/// Generated cluster metadata rs-matter ships, by cluster id: the access rules and qualities of
/// known attributes, the response ids of known commands. A cluster the product describes with
/// one of these ids gets rs-matter's metadata for the ids it lists.
const KNOWN_CLUSTERS: &[&Cluster<'static>] = &[
    &decl::identify::FULL_CLUSTER,
    &decl::groups::FULL_CLUSTER,
    &decl::on_off::FULL_CLUSTER,
    &decl::level_control::FULL_CLUSTER,
    &decl::basic_information::FULL_CLUSTER,
    &decl::power_source::FULL_CLUSTER,
    &decl::fixed_label::FULL_CLUSTER,
    &decl::user_label::FULL_CLUSTER,
    &decl::boolean_state::FULL_CLUSTER,
    &decl::mode_select::FULL_CLUSTER,
    &decl::bridged_device_basic_information::FULL_CLUSTER,
    &decl::switch::FULL_CLUSTER,
    &decl::air_quality::FULL_CLUSTER,
    &decl::smoke_co_alarm::FULL_CLUSTER,
    &decl::door_lock::FULL_CLUSTER,
    &decl::window_covering::FULL_CLUSTER,
    &decl::thermostat::FULL_CLUSTER,
    &decl::fan_control::FULL_CLUSTER,
    &decl::color_control::FULL_CLUSTER,
    &decl::illuminance_measurement::FULL_CLUSTER,
    &decl::temperature_measurement::FULL_CLUSTER,
    &decl::pressure_measurement::FULL_CLUSTER,
    &decl::flow_measurement::FULL_CLUSTER,
    &decl::relative_humidity_measurement::FULL_CLUSTER,
    &decl::occupancy_sensing::FULL_CLUSTER,
    &decl::carbon_dioxide_concentration_measurement::FULL_CLUSTER,
    &decl::electrical_power_measurement::FULL_CLUSTER,
    &decl::electrical_energy_measurement::FULL_CLUSTER,
];

/// rs-matter's metadata of a cluster id, if it ships one.
pub(crate) fn known_cluster(id: ClusterId) -> Option<&'static Cluster<'static>> {
    KNOWN_CLUSTERS
        .iter()
        .copied()
        .find(|cluster| cluster.id == id.raw())
}

/// Why a path does not reach a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathError {
    Endpoint,
    Cluster,
    Attribute,
    Command,
}

impl From<PathError> for rs_matter::error::Error {
    fn from(error: PathError) -> Self {
        match error {
            PathError::Endpoint => ErrorCode::EndpointNotFound,
            PathError::Cluster => ErrorCode::ClusterNotFound,
            PathError::Attribute => ErrorCode::AttributeNotFound,
            PathError::Command => ErrorCode::CommandNotFound,
        }
        .into()
    }
}

/// The shape of a cluster: everything that decides its metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClusterShape {
    id: u32,
    revision: u16,
    feature_map: u32,
    /// Attribute ids with whether their value is a list.
    attributes: Vec<(u32, bool)>,
    commands: Vec<u32>,
    events: Vec<u32>,
}

/// The leaked, shape-keyed metadata.
#[derive(Default)]
struct ShapeCache {
    clusters: HashMap<ClusterShape, Cluster<'static>>,
    cluster_lists: HashMap<Vec<ClusterShape>, &'static [Cluster<'static>]>,
    device_types: HashMap<Vec<(u16, u16)>, &'static [DeviceType]>,
}

impl ShapeCache {
    fn cluster(&mut self, shape: ClusterShape) -> Cluster<'static> {
        if let Some(cluster) = self.clusters.get(&shape) {
            return cluster.clone();
        }
        let known = known_cluster(ClusterId::new(shape.id));
        let mut attributes: Vec<Attribute> = shape
            .attributes
            .iter()
            .map(|(id, is_list)| attribute_metadata(known, *id, *is_list))
            .collect();
        attributes.extend([
            GENERATED_COMMAND_LIST,
            ACCEPTED_COMMAND_LIST,
            ATTRIBUTE_LIST,
            FEATURE_MAP,
            CLUSTER_REVISION,
        ]);
        let commands: Vec<Command> = shape
            .commands
            .iter()
            .map(|id| command_metadata(known, *id))
            .collect();
        let events: Vec<Event> = shape
            .events
            .iter()
            .map(|id| event_metadata(known, *id))
            .collect();
        let cluster = Cluster::new(
            shape.id,
            shape.revision,
            shape.feature_map,
            Vec::leak(attributes),
            Vec::leak(commands),
            Vec::leak(events),
            |_, _, _| true,
            |_, _, _| true,
            |_, _, _| true,
        );
        self.clusters.insert(shape, cluster.clone());
        cluster
    }

    fn cluster_list(&mut self, shapes: Vec<ClusterShape>) -> &'static [Cluster<'static>] {
        if let Some(list) = self.cluster_lists.get(&shapes) {
            return list;
        }
        let mut clusters = vec![DescHandler::CLUSTER];
        clusters.extend(shapes.iter().cloned().map(|shape| self.cluster(shape)));
        let list: &'static [Cluster<'static>] = Vec::leak(clusters);
        self.cluster_lists.insert(shapes, list);
        list
    }

    fn device_types(&mut self, types: Vec<(u16, u16)>) -> &'static [DeviceType] {
        if let Some(list) = self.device_types.get(&types) {
            return list;
        }
        let list: &'static [DeviceType] = Vec::leak(
            types
                .iter()
                .map(|(dtype, drev)| DeviceType {
                    dtype: *dtype,
                    drev: *drev,
                })
                .collect(),
        );
        self.device_types.insert(types, list);
        list
    }
}

/// The access of an attribute rs-matter has no metadata for: readable and writable by an
/// operator; the product decides on every write.
const DEFAULT_ATTRIBUTE_ACCESS: Access = Access::READ
    .union(Access::WRITE)
    .union(Access::NEED_VIEW)
    .union(Access::NEED_OPERATE);

fn attribute_metadata(known: Option<&Cluster<'static>>, id: u32, is_list: bool) -> Attribute {
    if let Some(attribute) = known.and_then(|c| c.attributes.iter().find(|a| a.id == id)) {
        return attribute.clone();
    }
    let quality = if is_list { Quality::A } else { Quality::NONE };
    Attribute::new(id, DEFAULT_ATTRIBUTE_ACCESS, quality)
}

fn command_metadata(known: Option<&Cluster<'static>>, id: u32) -> Command {
    if let Some(command) = known.and_then(|c| c.commands.iter().find(|c| c.id == id)) {
        return command.clone();
    }
    Command::new(id, None, Access::WO)
}

fn event_metadata(known: Option<&Cluster<'static>>, id: u32) -> Event {
    if let Some(event) = known.and_then(|c| c.events.iter().find(|e| e.id == id)) {
        return event.clone();
    }
    Event::new(id, Access::RV)
}

/// An endpoint the product added.
pub(crate) struct ProductEndpoint {
    pub(crate) description: EndpointDescription,
    pub(crate) values: BTreeMap<(ClusterId, AttributeId), Value>,
    /// One data version per cluster, the Descriptor included.
    datavers: BTreeMap<ClusterId, Dataver>,
}

/// The endpoint table.
pub(crate) struct Table {
    aggregator: EndpointId,
    capacity: usize,
    /// Every endpoint of the node in id order: root, aggregator and the product's.
    endpoints: Vec<Endpoint<'static>>,
    product: BTreeMap<EndpointId, ProductEndpoint>,
    aggregator_dataver: Dataver,
    shapes: ShapeCache,
}

fn new_dataver() -> Dataver {
    Dataver::new(rand::random())
}

impl Table {
    /// A table with the root endpoint and the aggregator, room for `capacity` product endpoints.
    pub(crate) fn new(aggregator: EndpointId, capacity: usize) -> Self {
        let mut endpoints = vec![
            ROOT,
            Endpoint::new(
                aggregator.raw(),
                AGGREGATOR_DEVICE_TYPES,
                AGGREGATOR_CLUSTERS,
            ),
        ];
        endpoints.sort_by_key(|endpoint| endpoint.id);
        Self {
            aggregator,
            capacity,
            endpoints,
            product: BTreeMap::new(),
            aggregator_dataver: new_dataver(),
            shapes: ShapeCache::default(),
        }
    }

    /// The node's endpoints, for the metadata view.
    pub(crate) fn endpoints(&self) -> &[Endpoint<'static>] {
        &self.endpoints
    }

    /// The ids of the product's endpoints, in order.
    #[cfg(test)]
    pub(crate) fn product_ids(&self) -> Vec<EndpointId> {
        self.product.keys().copied().collect()
    }

    fn validate(&self, description: &EndpointDescription) -> Result<(), Error> {
        let id = description.id;
        let invalid = |reason: String| Error::InvalidDescription {
            endpoint: id,
            reason,
        };
        if id == ROOT_ENDPOINT {
            return Err(invalid(String::from(
                "endpoint 0 is the root endpoint and belongs to the engine",
            )));
        }
        if self.product.contains_key(&id) || self.aggregator == id {
            return Err(Error::DuplicateEndpoint(id));
        }
        if description.device_types.is_empty() {
            return Err(invalid(String::from("no device type")));
        }
        for device_type in &description.device_types {
            if u16::try_from(device_type.id.raw()).is_err() {
                return Err(invalid(format!(
                    "device type {} does not fit the 16 bits this engine supports",
                    device_type.id
                )));
            }
        }
        for (i, cluster) in description.clusters.iter().enumerate() {
            if cluster.id == DESCRIPTOR {
                return Err(invalid(String::from(
                    "the Descriptor cluster is served by the engine and must not be listed",
                )));
            }
            if description.clusters[..i].iter().any(|c| c.id == cluster.id) {
                return Err(invalid(format!("cluster {} is listed twice", cluster.id)));
            }
            for (j, attribute) in cluster.attributes.iter().enumerate() {
                if attribute.is_global() {
                    return Err(invalid(format!(
                        "global attribute {attribute} of cluster {} is derived by the engine and must not be listed",
                        cluster.id
                    )));
                }
                if cluster.attributes[..j].contains(attribute) {
                    return Err(invalid(format!(
                        "attribute {attribute} of cluster {} is listed twice",
                        cluster.id
                    )));
                }
            }
        }
        if let Some(parent) = description.parent
            && self.aggregator != parent
            && !self.product.contains_key(&parent)
        {
            return Err(Error::UnknownEndpoint(parent));
        }
        for (i, (cluster, attribute, _)) in description.initial_values.iter().enumerate() {
            let listed = description
                .cluster(*cluster)
                .is_some_and(|c| c.has_attribute(*attribute));
            if !listed {
                return Err(invalid(format!(
                    "initial value for attribute {attribute} of cluster {cluster}, which is not listed"
                )));
            }
            if description.initial_values[..i]
                .iter()
                .any(|(c, a, _)| c == cluster && a == attribute)
            {
                return Err(invalid(format!(
                    "attribute {attribute} of cluster {cluster} has two initial values"
                )));
            }
        }
        for cluster in &description.clusters {
            for attribute in &cluster.attributes {
                let present = description
                    .initial_values
                    .iter()
                    .any(|(c, a, _)| *c == cluster.id && a == attribute);
                if !present {
                    return Err(invalid(format!(
                        "attribute {attribute} of cluster {} has no initial value",
                        cluster.id
                    )));
                }
            }
        }
        if self.product.len() >= self.capacity {
            return Err(Error::CapacityExceeded { what: "endpoints" });
        }
        Ok(())
    }

    /// Adds an endpoint. Returns the endpoints whose metadata changed: the new one, its
    /// ancestors up to the aggregator, and the root.
    ///
    /// # Errors
    ///
    /// As [`matter_rs_engine::Running::add_endpoint`].
    pub(crate) fn add(
        &mut self,
        description: EndpointDescription,
    ) -> Result<Vec<EndpointId>, Error> {
        self.validate(&description)?;
        let shapes: Vec<ClusterShape> = description
            .clusters
            .iter()
            .map(|cluster| {
                let mut attributes: Vec<(u32, bool)> = cluster
                    .attributes
                    .iter()
                    .map(|attribute| {
                        let is_list = description.initial_values.iter().any(|(c, a, v)| {
                            *c == cluster.id && a == attribute && matches!(v, Value::List(_))
                        });
                        (attribute.raw(), is_list)
                    })
                    .collect();
                attributes.sort_unstable();
                let mut commands: Vec<u32> =
                    cluster.accepted_commands.iter().map(|c| c.raw()).collect();
                commands.sort_unstable();
                commands.dedup();
                let mut events: Vec<u32> = cluster.events.iter().map(|e| e.raw()).collect();
                events.sort_unstable();
                events.dedup();
                ClusterShape {
                    id: cluster.id.raw(),
                    revision: cluster.revision,
                    feature_map: cluster.feature_map,
                    attributes,
                    commands,
                    events,
                }
            })
            .collect();
        let device_types: Vec<(u16, u16)> = description
            .device_types
            .iter()
            .map(|device_type| {
                let dtype = u16::try_from(device_type.id.raw()).expect("validated above");
                (dtype, device_type.revision)
            })
            .collect();
        let clusters = self.shapes.cluster_list(shapes);
        let device_types = self.shapes.device_types(device_types);

        let id = description.id;
        let endpoint = Endpoint::new(id.raw(), device_types, clusters);
        let position = self
            .endpoints
            .iter()
            .position(|e| e.id > id.raw())
            .unwrap_or(self.endpoints.len());
        self.endpoints.insert(position, endpoint);

        let values = description
            .initial_values
            .iter()
            .map(|(c, a, v)| ((*c, *a), v.clone()))
            .collect();
        let mut datavers: BTreeMap<ClusterId, Dataver> = description
            .clusters
            .iter()
            .map(|cluster| (cluster.id, new_dataver()))
            .collect();
        datavers.insert(DESCRIPTOR, new_dataver());
        let parent = description.parent;
        self.product.insert(
            id,
            ProductEndpoint {
                description,
                values,
                datavers,
            },
        );

        let mut changed = vec![id];
        changed.extend(self.ancestors(parent));
        Ok(changed)
    }

    /// The chain from `parent` up to the aggregator, then the root. `None` means directly
    /// under the aggregator.
    fn ancestors(&self, parent: Option<EndpointId>) -> Vec<EndpointId> {
        let mut chain = Vec::new();
        let mut current = parent;
        while let Some(id) = current {
            if id == self.aggregator || chain.contains(&id) {
                break;
            }
            chain.push(id);
            current = self.product.get(&id).and_then(|e| e.description.parent);
        }
        chain.push(self.aggregator);
        chain.push(ROOT_ENDPOINT);
        chain
    }

    /// Removes an endpoint and every endpoint composed under it. Returns the endpoints whose
    /// metadata changed: the removed ones, the ancestors of the removed endpoint, and the root.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownEndpoint`] when the product did not add `id`.
    pub(crate) fn remove(&mut self, id: EndpointId) -> Result<Vec<EndpointId>, Error> {
        let Some(removed) = self.product.get(&id) else {
            return Err(Error::UnknownEndpoint(id));
        };
        let parent = removed.description.parent;
        let mut doomed = vec![id];
        loop {
            let more: Vec<EndpointId> = self
                .product
                .values()
                .filter(|e| {
                    e.description.parent.is_some_and(|p| doomed.contains(&p))
                        && !doomed.contains(&e.description.id)
                })
                .map(|e| e.description.id)
                .collect();
            if more.is_empty() {
                break;
            }
            doomed.extend(more);
        }
        for doomed_id in &doomed {
            self.product.remove(doomed_id);
            self.endpoints.retain(|e| e.id != doomed_id.raw());
        }
        let mut changed = doomed;
        changed.extend(self.ancestors(parent));
        Ok(changed)
    }

    /// Checks that `endpoint` lists `attribute` in `cluster`.
    fn check_attribute(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
    ) -> Result<(), Error> {
        let Some(e) = self.product.get(&endpoint) else {
            return Err(Error::UnknownEndpoint(endpoint));
        };
        let listed = e
            .description
            .cluster(cluster)
            .is_some_and(|c| c.has_attribute(attribute));
        if listed {
            Ok(())
        } else {
            Err(Error::UnknownAttribute {
                endpoint,
                cluster,
                attribute,
            })
        }
    }

    /// Checks that `endpoint` lists `event` in `cluster`.
    pub(crate) fn check_event(
        &self,
        endpoint: EndpointId,
        cluster: ClusterId,
        event: EventId,
    ) -> Result<(), Error> {
        let Some(e) = self.product.get(&endpoint) else {
            return Err(Error::UnknownEndpoint(endpoint));
        };
        let listed = e
            .description
            .cluster(cluster)
            .is_some_and(|c| c.has_event(event));
        if listed {
            Ok(())
        } else {
            Err(Error::UnknownEvent {
                endpoint,
                cluster,
                event,
            })
        }
    }

    /// Sets attribute values, all or none.
    ///
    /// # Errors
    ///
    /// As [`matter_rs_engine::Running::set_attributes`].
    pub(crate) fn set_values(&mut self, values: &[AttributeValue]) -> Result<(), Error> {
        for value in values {
            self.check_attribute(value.endpoint, value.cluster, value.attribute)?;
        }
        for value in values {
            self.store(
                value.endpoint,
                value.cluster,
                value.attribute,
                value.value.clone(),
            );
        }
        Ok(())
    }

    /// Stores a value the path of which was checked already.
    pub(crate) fn store(
        &mut self,
        endpoint: EndpointId,
        cluster: ClusterId,
        attribute: AttributeId,
        value: Value,
    ) {
        if let Some(e) = self.product.get_mut(&endpoint) {
            e.values.insert((cluster, attribute), value);
        }
    }

    /// The data version of a cluster on an endpoint the engine serves (aggregator or product).
    pub(crate) fn dataver(&self, endpoint: u16, cluster: u32) -> Result<u32, PathError> {
        if endpoint == self.aggregator.raw() {
            return if cluster == DESCRIPTOR.raw() {
                Ok(self.aggregator_dataver.get())
            } else {
                Err(PathError::Cluster)
            };
        }
        let e = self
            .product
            .get(&EndpointId::new(endpoint))
            .ok_or(PathError::Endpoint)?;
        e.datavers
            .get(&ClusterId::new(cluster))
            .map(Dataver::get)
            .ok_or(PathError::Cluster)
    }

    /// Bumps the data versions matching the context: a cluster on an endpoint, or every
    /// cluster of every endpoint when both are unspecified.
    pub(crate) fn bump(&self, endpoint: Option<u16>, cluster: Option<u32>) {
        let matches = |e: u16, c: u32| {
            endpoint.is_none_or(|wanted| wanted == e) && cluster.is_none_or(|wanted| wanted == c)
        };
        if matches(self.aggregator.raw(), DESCRIPTOR.raw()) {
            self.aggregator_dataver.changed();
        }
        for (id, e) in &self.product {
            for (cluster_id, dataver) in &e.datavers {
                if matches(id.raw(), cluster_id.raw()) {
                    dataver.changed();
                }
            }
        }
    }

    /// The value the engine reports for an attribute of a served endpoint: a Descriptor
    /// attribute is computed, everything else comes from the value map.
    pub(crate) fn read(
        &self,
        endpoint: u16,
        cluster: u32,
        attribute: u32,
    ) -> Result<Value, PathError> {
        let endpoint_id = EndpointId::new(endpoint);
        let cluster_id = ClusterId::new(cluster);
        let attribute_id = AttributeId::new(attribute);
        if endpoint == self.aggregator.raw() {
            if cluster_id != DESCRIPTOR {
                return Err(PathError::Cluster);
            }
            return self.descriptor(endpoint_id, attribute_id);
        }
        let e = self.product.get(&endpoint_id).ok_or(PathError::Endpoint)?;
        if cluster_id == DESCRIPTOR {
            return self.descriptor(endpoint_id, attribute_id);
        }
        if e.description.cluster(cluster_id).is_none() {
            return Err(PathError::Cluster);
        }
        e.values
            .get(&(cluster_id, attribute_id))
            .cloned()
            .ok_or(PathError::Attribute)
    }

    fn descriptor(&self, endpoint: EndpointId, attribute: AttributeId) -> Result<Value, PathError> {
        let node_endpoint = self
            .endpoints
            .iter()
            .find(|e| e.id == endpoint.raw())
            .ok_or(PathError::Endpoint)?;
        let value = match attribute {
            DEVICE_TYPE_LIST => Value::List(
                node_endpoint
                    .device_types
                    .iter()
                    .map(|device_type| {
                        Value::Struct(vec![
                            (0, Value::U32(u32::from(device_type.dtype))),
                            (1, Value::U16(device_type.drev)),
                        ])
                    })
                    .collect(),
            ),
            SERVER_LIST => Value::List(
                node_endpoint
                    .clusters
                    .iter()
                    .map(|cluster| Value::U32(cluster.id))
                    .collect(),
            ),
            CLIENT_LIST => Value::List(Vec::new()),
            PARTS_LIST => {
                let parts: Vec<Value> = if endpoint == self.aggregator {
                    self.product.keys().map(|id| Value::U16(id.raw())).collect()
                } else {
                    self.product
                        .values()
                        .filter(|e| e.description.parent == Some(endpoint))
                        .map(|e| Value::U16(e.description.id.raw()))
                        .collect()
                };
                Value::List(parts)
            }
            _ => return Err(PathError::Attribute),
        };
        Ok(value)
    }

    /// Checks that a write may reach the product: the endpoint is the product's and lists the
    /// attribute.
    pub(crate) fn check_write(
        &self,
        endpoint: u16,
        cluster: u32,
        attribute: u32,
    ) -> Result<(), PathError> {
        let e = self
            .product
            .get(&EndpointId::new(endpoint))
            .ok_or(PathError::Endpoint)?;
        let cluster_id = ClusterId::new(cluster);
        if cluster_id == DESCRIPTOR {
            return Err(PathError::Attribute);
        }
        let c = e
            .description
            .cluster(cluster_id)
            .ok_or(PathError::Cluster)?;
        if c.has_attribute(AttributeId::new(attribute)) {
            Ok(())
        } else {
            Err(PathError::Attribute)
        }
    }

    /// Checks that a command may reach the product and returns the id of its response command
    /// when rs-matter's metadata knows one.
    pub(crate) fn check_command(
        &self,
        endpoint: u16,
        cluster: u32,
        command: u32,
    ) -> Result<Option<u32>, PathError> {
        let e = self
            .product
            .get(&EndpointId::new(endpoint))
            .ok_or(PathError::Endpoint)?;
        let cluster_id = ClusterId::new(cluster);
        let c = e
            .description
            .cluster(cluster_id)
            .ok_or(PathError::Cluster)?;
        if !c.accepts_command(matter_rs_engine::CommandId::new(command)) {
            return Err(PathError::Command);
        }
        let response = known_cluster(cluster_id)
            .and_then(|known| known.commands.iter().find(|k| k.id == command))
            .and_then(|k| k.resp_id);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::{
        AttributeId, ClusterDescription, ClusterId, CommandId, DeviceType, DeviceTypeId,
        EndpointDescription, EndpointId, Error, EventId, Value, bridged_device,
    };
    use rs_matter::dm::Metadata as _;

    use super::{DESCRIPTOR, PathError, ROOT, Table, known_cluster};

    const AGGREGATOR: EndpointId = EndpointId::new(1);
    const LIGHT: EndpointId = EndpointId::new(2);
    const ON_OFF: ClusterId = ClusterId::new(0x0006);
    const ON_OFF_ATTRIBUTE: AttributeId = AttributeId::new(0);
    const TOGGLE: CommandId = CommandId::new(2);
    const VENDOR: ClusterId = ClusterId::new(0xFFF1_FC01);

    fn light(id: EndpointId) -> EndpointDescription {
        EndpointDescription {
            id,
            device_types: vec![DeviceType {
                id: DeviceTypeId::new(0x0100),
                revision: 3,
            }],
            clusters: vec![
                ClusterDescription {
                    id: ON_OFF,
                    revision: 6,
                    feature_map: 0,
                    attributes: vec![ON_OFF_ATTRIBUTE],
                    accepted_commands: vec![CommandId::new(0), CommandId::new(1), TOGGLE],
                    generated_commands: vec![],
                    events: vec![],
                },
                ClusterDescription {
                    id: VENDOR,
                    revision: 1,
                    feature_map: 0,
                    attributes: vec![AttributeId::new(1), AttributeId::new(2)],
                    accepted_commands: vec![CommandId::new(7)],
                    generated_commands: vec![],
                    events: vec![EventId::new(3)],
                },
            ],
            parent: None,
            initial_values: vec![
                (ON_OFF, ON_OFF_ATTRIBUTE, Value::Bool(false)),
                (VENDOR, AttributeId::new(1), Value::U8(1)),
                (
                    VENDOR,
                    AttributeId::new(2),
                    Value::List(vec![Value::U8(1), Value::U8(2)]),
                ),
            ],
        }
    }

    #[test]
    fn root_and_aggregator_are_present() {
        let table = Table::new(AGGREGATOR, 4);
        let ids: Vec<u16> = table.endpoints().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![0, 1]);
        assert_eq!(ROOT.id, 0);
        assert_eq!(table.read(1, DESCRIPTOR.raw(), 3), Ok(Value::List(vec![])));
        assert_eq!(table.read(1, ON_OFF.raw(), 0), Err(PathError::Cluster));
        assert_eq!(table.read(5, ON_OFF.raw(), 0), Err(PathError::Endpoint));
    }

    #[test]
    fn add_builds_metadata_and_notifies_the_chain() {
        let mut table = Table::new(AGGREGATOR, 4);
        let changed = table.add(light(LIGHT)).unwrap();
        assert_eq!(changed, vec![LIGHT, AGGREGATOR, EndpointId::new(0)]);
        let ids: Vec<u16> = table.endpoints().iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![0, 1, 2]);

        let endpoint = &table.endpoints()[2];
        let clusters: Vec<u32> = endpoint.clusters.iter().map(|c| c.id).collect();
        assert_eq!(clusters, vec![DESCRIPTOR.raw(), ON_OFF.raw(), VENDOR.raw()]);
        let on_off = endpoint.cluster(ON_OFF.raw()).unwrap();
        // The known On/Off attribute carries rs-matter's metadata; the globals follow.
        let on_off_attrs: Vec<u32> = on_off.attributes.iter().map(|a| a.id).collect();
        assert_eq!(
            on_off_attrs,
            vec![0, 0xFFF8, 0xFFF9, 0xFFFB, 0xFFFC, 0xFFFD]
        );
        assert!(
            !on_off.attributes[0]
                .access
                .contains(rs_matter::dm::Access::WRITE)
        );
        let vendor = endpoint.cluster(VENDOR.raw()).unwrap();
        assert!(
            vendor.attributes[0]
                .access
                .contains(rs_matter::dm::Access::WRITE)
        );
        assert!(
            vendor.attributes[1]
                .quality
                .contains(rs_matter::dm::Quality::A)
        );
        assert_eq!(vendor.commands.len(), 1);
        assert_eq!(vendor.events.len(), 1);

        // Descriptor values are computed.
        assert_eq!(
            table.read(2, DESCRIPTOR.raw(), 0),
            Ok(Value::List(vec![Value::Struct(vec![
                (0, Value::U32(0x0100)),
                (1, Value::U16(3))
            ])]))
        );
        assert_eq!(
            table.read(2, DESCRIPTOR.raw(), 1),
            Ok(Value::List(vec![
                Value::U32(DESCRIPTOR.raw()),
                Value::U32(ON_OFF.raw()),
                Value::U32(VENDOR.raw())
            ]))
        );
        assert_eq!(
            table.read(1, DESCRIPTOR.raw(), 3),
            Ok(Value::List(vec![Value::U16(2)]))
        );
        assert_eq!(table.read(2, ON_OFF.raw(), 0), Ok(Value::Bool(false)));
        assert_eq!(table.read(2, ON_OFF.raw(), 9), Err(PathError::Attribute));
        assert_eq!(table.check_command(2, ON_OFF.raw(), 2), Ok(None));
        assert_eq!(
            table.check_command(2, ON_OFF.raw(), 9),
            Err(PathError::Command)
        );
        assert_eq!(
            table.check_write(2, DESCRIPTOR.raw(), 1),
            Err(PathError::Attribute)
        );
        assert!(table.check_write(2, VENDOR.raw(), 1).is_ok());
    }

    #[test]
    fn shapes_are_shared_between_endpoints() {
        let mut table = Table::new(AGGREGATOR, 8);
        table.add(light(LIGHT)).unwrap();
        table.add(light(EndpointId::new(3))).unwrap();
        let a = table.endpoints()[2].clusters.as_ptr();
        let b = table.endpoints()[3].clusters.as_ptr();
        assert_eq!(a, b, "the same shape leaks one cluster list");
        table.remove(LIGHT).unwrap();
        table.add(light(LIGHT)).unwrap();
        assert_eq!(table.endpoints()[2].clusters.as_ptr(), a);
        assert_eq!(table.shapes.cluster_lists.len(), 1);
        assert_eq!(table.shapes.clusters.len(), 2);
    }

    #[test]
    fn composed_endpoints_list_their_parts_and_go_together() {
        let mut table = Table::new(AGGREGATOR, 8);
        table.add(light(LIGHT)).unwrap();
        let part = EndpointId::new(3);
        let changed = table
            .add(EndpointDescription {
                parent: Some(LIGHT),
                ..light(part)
            })
            .unwrap();
        assert_eq!(changed, vec![part, LIGHT, AGGREGATOR, EndpointId::new(0)]);
        assert_eq!(
            table.read(2, DESCRIPTOR.raw(), 3),
            Ok(Value::List(vec![Value::U16(3)]))
        );
        assert_eq!(
            table.read(1, DESCRIPTOR.raw(), 3),
            Ok(Value::List(vec![Value::U16(2), Value::U16(3)]))
        );
        let changed = table.remove(LIGHT).unwrap();
        assert_eq!(changed, vec![LIGHT, part, AGGREGATOR, EndpointId::new(0)]);
        assert!(table.product_ids().is_empty());
        assert_eq!(table.endpoints().len(), 2);
        assert_eq!(table.remove(LIGHT), Err(Error::UnknownEndpoint(LIGHT)));
    }

    #[test]
    fn descriptions_are_validated() {
        let mut table = Table::new(AGGREGATOR, 1);
        assert_eq!(
            table.add(light(AGGREGATOR)),
            Err(Error::DuplicateEndpoint(AGGREGATOR))
        );
        let mut with_descriptor = light(LIGHT);
        with_descriptor.clusters.push(ClusterDescription {
            id: DESCRIPTOR,
            revision: 2,
            feature_map: 0,
            attributes: vec![],
            accepted_commands: vec![],
            generated_commands: vec![],
            events: vec![],
        });
        assert!(matches!(
            table.add(with_descriptor),
            Err(Error::InvalidDescription { reason, .. }) if reason.contains("Descriptor")
        ));
        let mut wide = light(LIGHT);
        wide.device_types[0].id = DeviceTypeId::new(0x1_0000);
        assert!(matches!(
            table.add(wide),
            Err(Error::InvalidDescription { reason, .. }) if reason.contains("16 bits")
        ));
        table.add(light(LIGHT)).unwrap();
        assert_eq!(
            table.add(light(EndpointId::new(3))),
            Err(Error::CapacityExceeded { what: "endpoints" })
        );
    }

    #[test]
    fn datavers_bump_by_context() {
        let mut table = Table::new(AGGREGATOR, 4);
        table.add(light(LIGHT)).unwrap();
        let before = table.dataver(2, ON_OFF.raw()).unwrap();
        let descriptor_before = table.dataver(2, DESCRIPTOR.raw()).unwrap();
        table.bump(Some(2), Some(ON_OFF.raw()));
        assert_eq!(
            table.dataver(2, ON_OFF.raw()).unwrap(),
            before.wrapping_add(1)
        );
        assert_eq!(
            table.dataver(2, DESCRIPTOR.raw()).unwrap(),
            descriptor_before
        );
        let aggregator_before = table.dataver(1, DESCRIPTOR.raw()).unwrap();
        table.bump(None, None);
        assert_eq!(
            table.dataver(1, DESCRIPTOR.raw()).unwrap(),
            aggregator_before.wrapping_add(1)
        );
        assert_eq!(
            table.dataver(2, DESCRIPTOR.raw()).unwrap(),
            descriptor_before.wrapping_add(1)
        );
        assert_eq!(table.dataver(9, 0), Err(PathError::Endpoint));
    }

    #[test]
    fn metadata_view_sees_the_table() {
        let mut table = Table::new(AGGREGATOR, 4);
        table.add(light(LIGHT)).unwrap();
        let view = crate::handler::TableMetadata(std::rc::Rc::new(std::cell::RefCell::new(table)));
        let count = view.access(|node| node.endpoints.len());
        assert_eq!(count, 3);
    }

    #[test]
    fn known_clusters_carry_rs_matters_response_ids() {
        assert!(known_cluster(ON_OFF).is_some());
        assert!(known_cluster(bridged_device::CLUSTER).is_some());
        assert!(known_cluster(VENDOR).is_none());
    }
}
