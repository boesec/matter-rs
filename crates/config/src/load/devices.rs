//! The `[devices.<id>]` tables: their structure against the schema and the mapping schema of
//! their adapter's kind, and their resolution into typed device configurations.

use std::collections::BTreeMap;
use std::ops::Range;

use serde_json::Value;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{is_valid_id, span_at, value_at};
use crate::adapters::mapping_schema;
use crate::diagnostic::Collector;
use crate::validate::{KeyPath, Validator};

/// The structural check of every `[devices.<id>]` table: a valid id, the keys of the section,
/// exactly one of `kind` and `parts` with known kinds, an adapter that exists, and a `mapping`
/// that fits the adapter's kind.
pub(super) fn check_devices(
    validator: &Validator<'_>,
    schema: &Value,
    document: &Spanned<DeTable<'_>>,
    value: &Spanned<DeValue<'_>>,
    out: &mut Collector<'_>,
) {
    let path = KeyPath::default().child("devices");
    let DeValue::Table(devices) = value.get_ref() else {
        out.error(
            &value.span(),
            "`devices` must be a table of devices, `[devices.<id>]`",
        );
        return;
    };
    let device_schema = validator
        .property(schema, "devices")
        .and_then(|devices| validator.resolve(devices).get("additionalProperties"));

    for (id, device) in devices {
        let device_path = path.child(id.get_ref());
        if !is_valid_id(id.get_ref()) {
            out.error(
                &id.span(),
                format!(
                    "device id `{}` is not valid; use lower-case letters, digits, `-` and `_`, \
                     starting with a letter or digit",
                    id.get_ref()
                ),
            );
        }
        let DeValue::Table(table) = device.get_ref() else {
            out.error(
                &device.span(),
                format!("{} must be a table", device_path.value_name()),
            );
            continue;
        };
        if let Some(device_schema) = device_schema {
            validator.validate(device_schema, device, &device_path, out);
        }
        check_kinds(table, &device.span(), &device_path, out);

        let adapter = table.iter().find(|(key, _)| key.get_ref() == "adapter");
        let Some((_, adapter_id)) = adapter else {
            continue;
        };
        let DeValue::String(adapter_id) = adapter_id.get_ref() else {
            continue;
        };
        let adapter_kind = value_at(document, &["adapters", adapter_id, "kind"]);
        let Some(adapter_kind) = adapter_kind else {
            let known: Vec<String> = match value_at(document, &["adapters"]).map(Spanned::get_ref) {
                Some(DeValue::Table(adapters)) => adapters
                    .keys()
                    .map(|key| key.get_ref().to_string())
                    .collect(),
                _ => Vec::new(),
            };
            let hint = if known.is_empty() {
                String::from("the file has no `[adapters.<id>]` table")
            } else {
                format!("known adapters are `{}`", known.join("`, `"))
            };
            out.error(
                &adapter.map_or_else(|| device.span(), |(_, value)| value.span()),
                format!(
                    "{} names no `[adapters.<id>]` table; {hint}",
                    device_path.child("adapter").value_name()
                ),
            );
            continue;
        };
        let DeValue::String(kind) = adapter_kind.get_ref() else {
            continue;
        };
        let Some((_, mapping)) = table.iter().find(|(key, _)| key.get_ref() == "mapping") else {
            continue;
        };
        let Some(known) = crate::adapters::adapter_kinds()
            .iter()
            .find(|known| known.kind == kind.as_ref())
        else {
            // Reported at the adapter table already.
            continue;
        };
        if !known.compiled_in {
            continue;
        }
        match mapping_schema(kind) {
            Some(mapping_schema) => {
                let mapping_validator = Validator::new(&mapping_schema);
                mapping_validator.validate(
                    &mapping_schema,
                    mapping,
                    &device_path.child("mapping"),
                    out,
                );
            }
            None => out.error(
                &mapping.span(),
                format!("adapter `{adapter_id}` is of kind `{kind}`, which feeds no devices",),
            ),
        }
    }
}

/// Exactly one of `kind` and `parts`, and every kind a known one.
fn check_kinds(
    table: &DeTable<'_>,
    span: &Range<usize>,
    device_path: &KeyPath,
    out: &mut Collector<'_>,
) {
    let kind = table.iter().find(|(key, _)| key.get_ref() == "kind");
    let parts = table.iter().find(|(key, _)| key.get_ref() == "parts");
    match (kind, parts) {
        (None, None) => out.error(
            span,
            format!(
                "missing `kind` (or `parts` for a composed device){}",
                device_path.table_suffix()
            ),
        ),
        (Some(_), Some((parts_key, _))) => out.error(
            &parts_key.span(),
            format!(
                "`kind` and `parts` exclude each other{}; a composed device lists its kinds in \
                 `parts` only",
                device_path.table_suffix()
            ),
        ),
        (Some((_, kind)), None) => check_kind_name(kind, &device_path.child("kind"), out),
        (None, Some((_, parts))) => {
            if let DeValue::Array(items) = parts.get_ref() {
                if items.is_empty() {
                    out.error(
                        &parts.span(),
                        format!(
                            "{} must list at least one part",
                            device_path.child("parts").value_name()
                        ),
                    );
                }
                for (index, item) in items.iter().enumerate() {
                    if let DeValue::Table(part) = item.get_ref()
                        && let Some((_, kind)) =
                            part.iter().find(|(key, _)| key.get_ref() == "kind")
                    {
                        check_kind_name(
                            kind,
                            &device_path
                                .child("parts")
                                .child(&index.to_string())
                                .child("kind"),
                            out,
                        );
                    }
                }
            }
        }
    }
}

fn check_kind_name(kind: &Spanned<DeValue<'_>>, path: &KeyPath, out: &mut Collector<'_>) {
    let DeValue::String(name) = kind.get_ref() else {
        return;
    };
    if matter_rs_bridge::DeviceKind::from_name(name).is_none() {
        let known: Vec<&str> = matter_rs_bridge::DeviceKind::ALL
            .iter()
            .map(|kind| kind.name())
            .collect();
        out.error(
            &kind.span(),
            format!(
                "{} is not a device kind; known kinds are `{}`",
                path.value_name(),
                known.join("`, `")
            ),
        );
    }
}

/// Turns the device sections of a valid file into typed configurations; mapping problems are
/// reported at the key they concern.
pub(super) fn resolve_devices(
    file: &crate::model::ConfigFile,
    document: &Spanned<DeTable<'_>>,
    out: &mut Collector<'_>,
) -> Option<BTreeMap<String, super::DeviceConfig>> {
    let mut devices = BTreeMap::new();
    let mut complete = true;
    for (id, section) in &file.devices {
        let parts: Vec<super::PartConfig> = match (&section.kind, section.parts.is_empty()) {
            (Some(kind), true) => vec![super::PartConfig {
                kind: matter_rs_bridge::DeviceKind::from_name(kind)?,
                label: None,
            }],
            (None, false) => section
                .parts
                .iter()
                .map(|part| {
                    Some(super::PartConfig {
                        kind: matter_rs_bridge::DeviceKind::from_name(&part.kind)?,
                        label: part.label.clone(),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            // Reported by the structure check.
            _ => return None,
        };
        let name = section.name.clone().unwrap_or_else(|| id.clone());
        let adapter = file.adapters.get(&section.adapter)?;
        let Some(mapping) = resolve_mapping(id, &name, &parts, adapter, section, document, out)
        else {
            complete = false;
            continue;
        };
        devices.insert(
            id.clone(),
            super::DeviceConfig {
                adapter: section.adapter.clone(),
                name,
                parts,
                mapping,
            },
        );
    }
    complete.then_some(devices)
}

/// The typed mapping of one device for its adapter's kind.
fn resolve_mapping(
    id: &str,
    name: &str,
    parts: &[super::PartConfig],
    adapter: &crate::adapters::AdapterConfig,
    section: &crate::model::DeviceSection,
    document: &Spanned<DeTable<'_>>,
    out: &mut Collector<'_>,
) -> Option<super::DeviceMapping> {
    let mapping_span = span_at(document, &["devices", id, "mapping"]);
    match adapter {
        #[cfg(feature = "adapter-mqtt")]
        crate::adapters::AdapterConfig::Mqtt(_) => {
            let mapping: matter_rs_adapter_mqtt::Mapping =
                match serde_json::from_value(Value::Object(section.mapping.clone())) {
                    Ok(mapping) => mapping,
                    Err(error) => {
                        out.error(&mapping_span, format!("`mapping`: {error}"));
                        return None;
                    }
                };
            let key = match matter_rs_bridge::DeviceKey::new(id) {
                Ok(key) => key,
                Err(error) => {
                    out.error(
                        &span_at(document, &["devices", id]),
                        format!("device id `{id}`: {error}"),
                    );
                    return None;
                }
            };
            let definition = matter_rs_adapter_mqtt::DeviceDefinition {
                key,
                name: name.to_owned(),
                parts: parts
                    .iter()
                    .map(|part| matter_rs_adapter_mqtt::PartDefinition {
                        kind: part.kind,
                        label: part.label.clone(),
                    })
                    .collect(),
                mapping: mapping.clone(),
            };
            match matter_rs_adapter_mqtt::mapping::resolve(&definition) {
                Ok(_) => Some(super::DeviceMapping::Mqtt(mapping)),
                Err(errors) => {
                    for error in errors {
                        let mut path = vec!["devices", id, "mapping"];
                        path.extend(error.path.iter().map(String::as_str));
                        out.error(&span_at(document, &path), error.to_string());
                    }
                    None
                }
            }
        }
        #[allow(unreachable_patterns)]
        _ => {
            let _ = (name, parts, section);
            out.error(
                &mapping_span,
                format!("adapter `{}` feeds no devices", adapter.kind()),
            );
            None
        }
    }
}
