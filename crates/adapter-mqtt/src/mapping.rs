//! The `mapping` of a `[devices.<id>]` table whose adapter is of kind `mqtt`, and its resolution
//! into topics and transforms.
//!
//! The mqtt-smarthome convention is the default: a device of source `<adapter>/<device>` reads
//! attribute `x` from `<adapter>/status/<device>/<item>` and controls it through
//! `<adapter>/set/<device>/<item>`, where `<item>` is the attribute's `snake_case` name. Only
//! deviations are spelled out in `items`. Composed devices map every part under `parts`, with
//! the part's index (or a `prefix`) as an item level.

use core::fmt;
use std::collections::BTreeMap;

use matter_rs_bridge::{DeviceKey, DeviceKind, DeviceSpec, PartIndex, PartSpec};
use matter_rs_engine::Value;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value as Json;

use crate::attributes::{self, MappableAttribute, NAME_ITEM};
use crate::config::check_topic_level;
use crate::transform::{Scale, Transform};

/// The `mapping` table of a device on an `mqtt` adapter.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "Where the device lives on the broker: its `source`, the two leading topic \
                   levels `<adapter>/<device>` of an mqtt-smarthome source, and the deviations \
                   from the convention."
)]
pub struct Mapping {
    /// The source adapter instance and the device's name on it, `<adapter>/<device>`: the device
    /// reads `<adapter>/status/<device>/<item>` and writes `<adapter>/set/<device>/<item>`, and
    /// is reachable while `<adapter>/connected` is `2`.
    #[schemars(length(min = 3))]
    pub source: String,
    /// The item that reports whether the device itself is reachable
    /// (`<adapter>/status/<device>/<online>`, a boolean); the device is unreachable while it is
    /// false. Not read when absent.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub online: Option<String>,
    /// Deviations per attribute, by the attribute's name (`on-off`, `level`, `temperature`, ...,
    /// and `name` for the device label). For a composed device only `name` may appear here;
    /// its parts map their attributes in `parts`.
    #[serde(default)]
    pub items: BTreeMap<String, ItemMapping>,
    /// For a composed device: one entry per part, in the order of the device's `parts`. The
    /// default item of attribute `x` of part `i` is `<i>/<x>`, or `<prefix>/<x>`.
    #[serde(default)]
    pub parts: Vec<PartMapping>,
}

/// The mapping of one part of a composed device.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartMapping {
    /// The item level of this part's attributes; the part's index when absent.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub prefix: Option<String>,
    /// Deviations per attribute of this part, as in the device's `items`.
    #[serde(default)]
    pub items: BTreeMap<String, ItemMapping>,
}

/// How one attribute deviates from the convention: another item name or topic, a fixed value,
/// or a transform (see the transforms table of the contract).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemMapping {
    /// The item name under the convention, replacing the attribute's `snake_case` name.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub item: Option<String>,
    /// The full status topic, replacing `<adapter>/status/<device>/<item>`.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub status: Option<String>,
    /// The full `set` topic, replacing `<adapter>/set/<device>/<item>`.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub set: Option<String>,
    /// A fixed value instead of a topic; the attribute is never read from or written to the
    /// broker.
    #[serde(default)]
    pub value: Option<Json>,
    /// The payload range `[min, max]`, mapped linearly onto the attribute's range.
    #[serde(default)]
    pub scale: Option<[f64; 2]>,
    /// Negate a boolean, or reverse the direction of `scale`.
    #[serde(default)]
    pub invert: bool,
    /// Payload text to attribute value, for example `{ "ON" = true, "OFF" = false }`; matched
    /// case-insensitively, the key is published for a `set`.
    #[serde(default)]
    pub map: Option<BTreeMap<String, Json>>,
    /// A JSON pointer into the payload (`/state/on`), instead of the `val` unwrapping.
    #[serde(default)]
    pub path: Option<String>,
    /// Attribute value = payload × factor (`100` turns degrees into hundredths).
    #[serde(default)]
    pub factor: Option<f64>,
}

impl ItemMapping {
    fn transform(&self) -> Transform {
        Transform {
            path: self.path.clone(),
            map: self
                .map
                .iter()
                .flat_map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())))
                .collect(),
            factor: self.factor,
            scale: self.scale.map(|[min, max]| Scale { min, max }),
            invert: self.invert,
        }
    }

    fn has_transform(&self) -> bool {
        self.scale.is_some()
            || self.invert
            || self.map.is_some()
            || self.path.is_some()
            || self.factor.is_some()
    }
}

/// A device as the configuration defines it for this adapter: its key, label and parts, and
/// its mapping.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceDefinition {
    /// The device key, the id of its `[devices.<id>]` table.
    pub key: DeviceKey,
    /// The label controllers show.
    pub name: String,
    /// The parts; one for a single device.
    pub parts: Vec<PartDefinition>,
    /// The mapping.
    pub mapping: Mapping,
}

/// One part of a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartDefinition {
    /// The kind.
    pub kind: DeviceKind,
    /// The label of the part, for composed devices.
    pub label: Option<String>,
}

/// A problem in a mapping: the key path below `mapping` it concerns (empty for the mapping as a
/// whole) and what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingError {
    /// The keys below `mapping`, for example `["items", "level", "scale"]`.
    pub path: Vec<String>,
    /// What is wrong.
    pub message: String,
}

impl MappingError {
    fn new(path: &[&str], message: impl Into<String>) -> Self {
        Self {
            path: path.iter().map(|key| (*key).to_owned()).collect(),
            message: message.into(),
        }
    }
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "`mapping`: {}", self.message)
        } else {
            write!(f, "`mapping.{}`: {}", self.path.join("."), self.message)
        }
    }
}

/// One attribute of one part, resolved to its topics and transform.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedItem {
    /// The part.
    pub part: PartIndex,
    /// The attribute.
    pub attribute: &'static MappableAttribute,
    /// The topic the value is read from; `None` for a fixed value.
    pub status_topic: Option<String>,
    /// The topic a `set` is published to; `None` for a fixed value.
    pub set_topic: Option<String>,
    /// The fixed value, when the item has one.
    pub constant: Option<Value>,
    /// The transform between payload and attribute value.
    pub transform: Transform,
    /// Whether the bridge requires the attribute when the device is added.
    pub required: bool,
}

/// A device with its mapping resolved: everything the adapter needs to subscribe, translate
/// and publish.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedDevice {
    /// The specification the device is added with.
    pub spec: DeviceSpec,
    /// The source, `<adapter>/<device>`.
    pub source: String,
    /// The `<adapter>/connected` topic of the source adapter.
    pub connected_topic: String,
    /// The topic of the device's own online flag, when configured.
    pub online_topic: Option<String>,
    /// The topic a controller's rename is published to, when configured.
    pub rename_topic: Option<String>,
    /// The attributes.
    pub items: Vec<ResolvedItem>,
}

impl ResolvedDevice {
    /// The device key.
    #[must_use]
    pub fn key(&self) -> &DeviceKey {
        &self.spec.key
    }

    /// The item of a part's attribute, by the attribute's name.
    #[must_use]
    pub fn item(&self, part: PartIndex, name: &str) -> Option<&ResolvedItem> {
        self.items
            .iter()
            .find(|item| item.part == part && item.attribute.name == name)
    }
}

/// Resolves a device definition against the convention.
///
/// # Errors
///
/// Every problem found, each with its key path, so that the configuration report can show them
/// all with their locations.
pub fn resolve(definition: &DeviceDefinition) -> Result<ResolvedDevice, Vec<MappingError>> {
    let mapping = &definition.mapping;
    let mut errors = Vec::new();

    let (source_adapter, source_device) = match split_source(&mapping.source) {
        Ok(parts) => parts,
        Err(message) => {
            errors.push(MappingError::new(&["source"], message));
            ("", "")
        }
    };
    let status_prefix = format!("{source_adapter}/status/{source_device}");
    let set_prefix = format!("{source_adapter}/set/{source_device}");

    if definition.parts.is_empty() {
        errors.push(MappingError::new(&[], "the device has no part"));
    }
    let composed = definition.parts.len() > 1;
    if composed {
        if !mapping.parts.is_empty() && mapping.parts.len() != definition.parts.len() {
            errors.push(MappingError::new(
                &["parts"],
                format!(
                    "the device has {} parts, the mapping {}; list one entry per part or none",
                    definition.parts.len(),
                    mapping.parts.len()
                ),
            ));
        }
        for name in mapping.items.keys().filter(|name| *name != NAME_ITEM) {
            errors.push(MappingError::new(
                &["items", name],
                "a composed device maps the attributes of its parts in `parts`; only `name` \
                 belongs here",
            ));
        }
    } else if !mapping.parts.is_empty() {
        errors.push(MappingError::new(
            &["parts"],
            "`parts` is for composed devices; map a single device's attributes in `items`",
        ));
    }

    if let Some(online) = &mapping.online
        && let Err(reason) = check_topic_level(online)
    {
        errors.push(MappingError::new(
            &["online"],
            format!("the item name {reason}"),
        ));
    }

    let mut items = Vec::new();
    let mut rename_topic = None;
    let empty = BTreeMap::new();
    for (index, part) in definition.parts.iter().enumerate() {
        let part_index = PartIndex::try_from(index).unwrap_or(PartIndex::MAX);
        let (overrides, item_prefix, path_prefix): (
            &BTreeMap<String, ItemMapping>,
            String,
            Vec<String>,
        ) = if composed {
            let part_mapping = mapping.parts.get(index);
            let prefix = part_mapping
                .and_then(|part| part.prefix.clone())
                .unwrap_or_else(|| index.to_string());
            if let Err(reason) = check_topic_level(&prefix) {
                errors.push(MappingError::new(
                    &["parts", &index.to_string(), "prefix"],
                    format!("the prefix {reason}"),
                ));
            }
            (
                part_mapping.map_or(&empty, |part| &part.items),
                format!("{prefix}/"),
                vec![
                    String::from("parts"),
                    index.to_string(),
                    String::from("items"),
                ],
            )
        } else {
            (&mapping.items, String::new(), vec![String::from("items")])
        };
        let path = |name: &str, key: Option<&str>| -> Vec<String> {
            let mut path = path_prefix.clone();
            path.push(name.to_owned());
            path.extend(key.map(str::to_owned));
            path
        };

        if part.kind == DeviceKind::GenericSwitch {
            errors.push(MappingError::new(
                &[],
                format!(
                    "part {index} is a generic switch, which the MQTT adapter does not support \
                     yet (its presses are events, not values)"
                ),
            ));
            continue;
        }
        let mut known: Vec<&str> = attributes::of_kind(part.kind).map(|a| a.name).collect();
        if !composed {
            known.push(NAME_ITEM);
        }
        for name in overrides.keys() {
            if !known.contains(&name.as_str()) {
                errors.push(MappingError {
                    path: path(name, None),
                    message: format!(
                        "`{name}` is not an attribute of kind `{}`; known: `{}`",
                        part.kind.name(),
                        known.join("`, `")
                    ),
                });
            }
        }
        let required = part.kind.required_attributes();
        for attribute in attributes::of_kind(part.kind) {
            let item = overrides.get(attribute.name);
            let item_path = |key: Option<&str>| path(attribute.name, key);
            let is_required = required.contains(&attribute.attribute);
            let Some(item) = item else {
                if attribute.by_default {
                    items.push(ResolvedItem {
                        part: part_index,
                        attribute,
                        status_topic: Some(format!(
                            "{status_prefix}/{item_prefix}{}",
                            attribute.item
                        )),
                        set_topic: Some(format!("{set_prefix}/{item_prefix}{}", attribute.item)),
                        constant: None,
                        transform: Transform::default(),
                        required: is_required,
                    });
                } else if is_required {
                    errors.push(MappingError {
                        path: path_prefix.clone(),
                        message: format!(
                            "the required attribute `{}` of part {index} (`{}`) has no default \
                             item; map it with `{} = {{ status = \"<topic>\" }}` or give it a \
                             fixed `{} = {{ value = ... }}`",
                            attribute.name,
                            part.kind.name(),
                            attribute.name,
                            attribute.name
                        ),
                    });
                }
                continue;
            };
            if let Some(name) = &item.item
                && let Err(reason) = check_topic_level(name)
            {
                errors.push(MappingError {
                    path: item_path(Some("item")),
                    message: format!("the item name {reason}"),
                });
            }
            for (key, topic) in [("status", &item.status), ("set", &item.set)] {
                if let Some(topic) = topic
                    && let Err(reason) = check_topic(topic)
                {
                    errors.push(MappingError {
                        path: item_path(Some(key)),
                        message: format!("the topic {reason}"),
                    });
                }
            }
            if let Some(value) = &item.value {
                if item.item.is_some() || item.status.is_some() || item.set.is_some() {
                    errors.push(MappingError {
                        path: item_path(Some("value")),
                        message: String::from("a fixed value excludes `item`, `status` and `set`"),
                    });
                }
                if item.has_transform() {
                    errors.push(MappingError {
                        path: item_path(Some("value")),
                        message: String::from("a fixed value takes no transform"),
                    });
                }
                match attribute.value_type.constant(value) {
                    Ok(constant) => items.push(ResolvedItem {
                        part: part_index,
                        attribute,
                        status_topic: None,
                        set_topic: None,
                        constant: Some(constant),
                        transform: Transform::default(),
                        required: is_required,
                    }),
                    Err(reason) => errors.push(MappingError {
                        path: item_path(Some("value")),
                        message: format!("the value {reason}"),
                    }),
                }
                continue;
            }
            let transform = item.transform();
            if let Err(message) = transform.validate(attribute.value_type) {
                let key = ["scale", "invert", "map", "path", "factor"]
                    .into_iter()
                    .find(|key| message.contains(&format!("`{key}`")));
                errors.push(MappingError {
                    path: item_path(key),
                    message,
                });
            }
            let item_name = item.item.as_deref().unwrap_or(attribute.item);
            items.push(ResolvedItem {
                part: part_index,
                attribute,
                status_topic: Some(
                    item.status
                        .clone()
                        .unwrap_or_else(|| format!("{status_prefix}/{item_prefix}{item_name}")),
                ),
                set_topic: Some(
                    item.set
                        .clone()
                        .unwrap_or_else(|| format!("{set_prefix}/{item_prefix}{item_name}")),
                ),
                constant: None,
                transform,
                required: is_required,
            });
        }
    }

    if let Some(name) = mapping.items.get(NAME_ITEM) {
        let path = |key: &str| {
            vec![
                String::from("items"),
                String::from(NAME_ITEM),
                key.to_owned(),
            ]
        };
        if name.status.is_some() || name.value.is_some() || name.has_transform() {
            errors.push(MappingError {
                path: path("set"),
                message: String::from(
                    "`name` is the device label a controller sets; it takes only `item` or `set`",
                ),
            });
        }
        if let Some(topic) = &name.set
            && let Err(reason) = check_topic(topic)
        {
            errors.push(MappingError {
                path: path("set"),
                message: format!("the topic {reason}"),
            });
        }
        let item_name = name.item.as_deref().unwrap_or(NAME_ITEM);
        rename_topic = Some(
            name.set
                .clone()
                .unwrap_or_else(|| format!("{set_prefix}/{item_name}")),
        );
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    let spec = DeviceSpec {
        key: definition.key.clone(),
        name: definition.name.clone(),
        parts: definition
            .parts
            .iter()
            .map(|part| {
                let spec = PartSpec::new(part.kind);
                match &part.label {
                    Some(label) => spec.with_label(label.clone()),
                    None => spec,
                }
            })
            .collect(),
    };
    Ok(ResolvedDevice {
        spec,
        source: mapping.source.clone(),
        connected_topic: format!("{source_adapter}/connected"),
        online_topic: mapping
            .online
            .as_ref()
            .map(|online| format!("{status_prefix}/{online}")),
        rename_topic,
        items,
    })
}

/// Splits `<adapter>/<device>` into its two levels.
fn split_source(source: &str) -> Result<(&str, &str), String> {
    let Some((adapter, device)) = source.split_once('/') else {
        return Err(String::from(
            "must name the source adapter and the device as `<adapter>/<device>`",
        ));
    };
    for (what, level) in [("adapter", adapter), ("device", device)] {
        check_topic_level(level).map_err(|reason| format!("the {what} level {reason}"))?;
    }
    Ok((adapter, device))
}

/// Checks a full topic for publishing and subscribing: no wildcards, no empty levels.
fn check_topic(topic: &str) -> Result<(), &'static str> {
    if topic.is_empty() {
        return Err("must not be empty");
    }
    if topic.contains(['+', '#']) {
        return Err("must not contain the wildcards `+` or `#`");
    }
    if topic.contains('\0') {
        return Err("must not contain a NUL character");
    }
    if topic.split('/').any(str::is_empty) {
        return Err("must not have empty levels");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use matter_rs_bridge::{DeviceKey, DeviceKind};
    use matter_rs_engine::Value;

    use super::{DeviceDefinition, Mapping, MappingError, PartDefinition, resolve};

    fn definition(kind: DeviceKind, mapping: &str) -> DeviceDefinition {
        DeviceDefinition {
            key: DeviceKey::new("lamp").unwrap(),
            name: String::from("Lamp"),
            parts: vec![PartDefinition { kind, label: None }],
            mapping: toml::from_str(mapping).expect("the mapping parses"),
        }
    }

    fn messages(errors: &[MappingError]) -> Vec<String> {
        errors.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn convention_gives_three_line_devices() {
        let resolved = resolve(&definition(
            DeviceKind::DimmableLight,
            "source = \"hue/lamp\"",
        ))
        .expect("resolves");
        assert_eq!(resolved.connected_topic, "hue/connected");
        assert_eq!(resolved.online_topic, None);
        assert_eq!(resolved.rename_topic, None);
        let on_off = resolved.item(0, "on-off").unwrap();
        assert_eq!(
            on_off.status_topic.as_deref(),
            Some("hue/status/lamp/on_off")
        );
        assert_eq!(on_off.set_topic.as_deref(), Some("hue/set/lamp/on_off"));
        assert!(on_off.required);
        let level = resolved.item(0, "level").unwrap();
        assert_eq!(level.status_topic.as_deref(), Some("hue/status/lamp/level"));
        assert_eq!(resolved.items.len(), 2);
        assert_eq!(resolved.spec.parts.len(), 1);
    }

    #[test]
    fn overrides_change_items_topics_and_transforms() {
        let mapping = "
source = \"tasmota/plug\"
online = \"online\"
[items.on-off]
item = \"power\"
map = { \"ON\" = true, \"OFF\" = false }
[items.level]
status = \"zigbee/lamp/brightness\"
scale = [0, 100]
[items.name]
";
        let resolved = resolve(&definition(DeviceKind::DimmableLight, mapping)).expect("resolves");
        assert_eq!(
            resolved.online_topic.as_deref(),
            Some("tasmota/status/plug/online")
        );
        assert_eq!(
            resolved.rename_topic.as_deref(),
            Some("tasmota/set/plug/name")
        );
        let on_off = resolved.item(0, "on-off").unwrap();
        assert_eq!(
            on_off.status_topic.as_deref(),
            Some("tasmota/status/plug/power")
        );
        assert_eq!(on_off.set_topic.as_deref(), Some("tasmota/set/plug/power"));
        assert_eq!(on_off.transform.map.len(), 2);
        let level = resolved.item(0, "level").unwrap();
        assert_eq!(
            level.status_topic.as_deref(),
            Some("zigbee/lamp/brightness")
        );
        assert_eq!(level.set_topic.as_deref(), Some("tasmota/set/plug/level"));
        assert!(level.transform.scale.is_some());
    }

    #[test]
    fn fixed_values_replace_topics() {
        let mapping = "
source = \"hue/lamp\"
[items.color-temperature-min]
value = 153
[items.color-temperature-max]
value = 500
";
        let resolved =
            resolve(&definition(DeviceKind::ColorTemperatureLight, mapping)).expect("resolves");
        let min = resolved.item(0, "color-temperature-min").unwrap();
        assert_eq!(min.constant, Some(Value::U16(153)));
        assert_eq!(min.status_topic, None);
        assert!(min.required);
        assert_eq!(resolved.items.len(), 5);
    }

    #[test]
    fn required_attribute_without_default_is_an_error() {
        let errors = resolve(&definition(
            DeviceKind::ColorTemperatureLight,
            "source = \"hue/lamp\"",
        ))
        .unwrap_err();
        let messages = messages(&errors);
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].contains("`color-temperature-min`"),
            "{messages:?}"
        );
        assert!(messages[0].starts_with("`mapping.items`"), "{messages:?}");
        assert!(
            messages[1].contains("`color-temperature-max`"),
            "{messages:?}"
        );
    }

    #[test]
    fn errors_carry_their_key_path() {
        let mapping = "
source = \"hue\"
online = \"a/b\"
[items.on-off]
scale = [0, 1]
[items.brightness]
item = \"x\"
[items.level]
value = 5
scale = [0, 1]
";
        let errors = resolve(&definition(DeviceKind::DimmableLight, mapping)).unwrap_err();
        let paths: Vec<String> = errors.iter().map(|e| e.path.join(".")).collect();
        assert_eq!(
            paths,
            [
                "source",
                "online",
                "items.brightness",
                "items.on-off.scale",
                "items.level.value",
            ]
        );
        assert!(
            errors[2].message.contains("`on-off`, `level`, `name`"),
            "{}",
            errors[2]
        );
    }

    #[test]
    fn composed_devices_map_per_part() {
        let strip = DeviceDefinition {
            key: DeviceKey::new("strip").unwrap(),
            name: String::from("Strip"),
            parts: vec![
                PartDefinition {
                    kind: DeviceKind::OnOffPlugInUnit,
                    label: Some(String::from("Left")),
                },
                PartDefinition {
                    kind: DeviceKind::TemperatureSensor,
                    label: None,
                },
            ],
            mapping: toml::from_str(
                "source = \"strip/one\"\nparts = [{ prefix = \"left\" }, { items = { temperature = { factor = 100 } } }]\n[items.name]\n",
            )
            .unwrap(),
        };
        let resolved = resolve(&strip).expect("resolves");
        assert_eq!(
            resolved.item(0, "on-off").unwrap().status_topic.as_deref(),
            Some("strip/status/one/left/on_off")
        );
        assert_eq!(
            resolved
                .item(1, "temperature")
                .unwrap()
                .status_topic
                .as_deref(),
            Some("strip/status/one/1/temperature")
        );
        assert_eq!(
            resolved.item(1, "temperature").unwrap().transform.factor,
            Some(100.0)
        );
        assert_eq!(resolved.rename_topic.as_deref(), Some("strip/set/one/name"));
        assert_eq!(resolved.spec.parts[0].label.as_deref(), Some("Left"));

        let wrong = DeviceDefinition {
            mapping: toml::from_str("source = \"strip/one\"\n[items.on-off]\ninvert = true\n")
                .unwrap(),
            ..strip.clone()
        };
        let errors = resolve(&wrong).unwrap_err();
        assert!(errors[0].to_string().contains("composed"), "{}", errors[0]);

        let count = DeviceDefinition {
            mapping: toml::from_str("source = \"strip/one\"\nparts = [{}]\n").unwrap(),
            ..strip
        };
        let errors = resolve(&count).unwrap_err();
        assert!(errors[0].to_string().contains("2 parts"), "{}", errors[0]);
    }

    #[test]
    fn single_devices_take_no_parts_and_no_switches() {
        let errors = resolve(&definition(
            DeviceKind::OnOffLight,
            "source = \"a/b\"\nparts = [{}]\n",
        ))
        .unwrap_err();
        assert_eq!(errors[0].path, ["parts"]);
        let errors =
            resolve(&definition(DeviceKind::GenericSwitch, "source = \"a/b\"")).unwrap_err();
        assert!(errors[0].message.contains("generic switch"));
    }

    #[test]
    fn name_item_takes_only_a_topic() {
        let errors = resolve(&definition(
            DeviceKind::OnOffLight,
            "source = \"a/b\"\n[items.name]\nvalue = 1\n",
        ))
        .unwrap_err();
        assert_eq!(errors[0].path, ["items", "name", "set"]);
        let resolved = resolve(&definition(
            DeviceKind::OnOffLight,
            "source = \"a/b\"\n[items.name]\nset = \"x/y/label\"\n",
        ))
        .unwrap();
        assert_eq!(resolved.rename_topic.as_deref(), Some("x/y/label"));
    }

    #[test]
    fn unknown_mapping_keys_are_rejected() {
        assert!(toml::from_str::<Mapping>("source = \"a/b\"\ntopic = \"x\"\n").is_err());
        assert!(toml::from_str::<Mapping>("source = \"a/b\"\n[items.level]\nmin = 1\n").is_err());
    }
}
