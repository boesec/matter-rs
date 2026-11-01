//! The JSON schema of the configuration file, generated from the config types.

use schemars::Schema;
use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use serde_json::Value;

use crate::model::ConfigFile;

/// The JSON schema (draft 2020-12) of the configuration file, as a JSON value.
///
/// TOML has no null, so the `null` that schemars adds for optional fields is removed: an
/// optional key is one that is not required.
#[must_use]
pub fn config_schema() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .for_deserialize()
        .with_transform(RecursiveTransform(remove_null))
        .into_generator();
    generator.into_root_schema_for::<ConfigFile>().to_value()
}

/// The JSON schema as pretty-printed text with a final newline, as committed to the repository.
#[must_use]
pub fn json_schema() -> String {
    let mut text =
        serde_json::to_string_pretty(&config_schema()).unwrap_or_else(|_| String::from("{}"));
    text.push('\n');
    text
}

/// Removes `null` from `type` lists and from `anyOf` branches.
fn remove_null(schema: &mut Schema) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if object.get("default").is_some_and(Value::is_null) {
        object.remove("default");
    }
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|kind| kind != "null");
        if types.len() == 1 {
            let only = types.remove(0);
            object.insert(String::from("type"), only);
        }
    }
    if let Some(Value::Array(branches)) = object.get_mut("anyOf") {
        branches.retain(|branch| branch.get("type").is_none_or(|kind| kind != "null"));
        if branches.len() == 1 {
            let Some(Value::Object(only)) = branches.pop() else {
                return;
            };
            object.remove("anyOf");
            for (key, value) in only {
                object.entry(key).or_insert(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::config_schema;

    #[test]
    fn schema_has_no_null() {
        let text = config_schema().to_string();
        assert!(!text.contains("\"null\""), "{text}");
    }

    #[test]
    fn schema_forbids_unknown_keys_in_every_section() {
        let schema = config_schema();
        assert_eq!(schema["additionalProperties"], false);
        for section in [
            "NodeSection",
            "NetworkSection",
            "StateSection",
            "LogSection",
        ] {
            assert_eq!(
                schema["$defs"][section]["additionalProperties"], false,
                "{section}"
            );
        }
    }
}
