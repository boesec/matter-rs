//! The configuration reference in Markdown, generated from the JSON schema: one table per
//! section with each key's type, default and description.

use core::fmt::Write as _;

use serde_json::Value;

use crate::schema::config_schema;
use crate::validate::Validator;

/// The order in which the sections are documented; sections not listed follow in schema order.
const SECTION_ORDER: [&str; 6] = ["node", "network", "state", "log", "bridge", "adapters"];

/// The configuration reference as Markdown, with a final newline.
#[must_use]
pub fn reference_markdown() -> String {
    let schema = config_schema();
    let validator = Validator::new(&schema);
    let root = validator.resolve(&schema);
    let empty = serde_json::Map::new();
    let properties = root
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required = required_keys(root);

    let mut out = String::new();
    out.push_str(INTRO);

    let mut names: Vec<&str> = SECTION_ORDER
        .iter()
        .copied()
        .filter(|name| properties.contains_key(*name))
        .collect();
    names.extend(
        properties
            .keys()
            .map(String::as_str)
            .filter(|name| !SECTION_ORDER.contains(name)),
    );

    for name in names {
        let property = &properties[name];
        let section = validator.resolve(property);
        let description = description(property).or_else(|| description(section));
        let is_map = section
            .get("additionalProperties")
            .is_some_and(Value::is_object)
            && section.get("properties").is_none();

        if is_map {
            let _ = writeln!(out, "\n## `[{name}.<id>]`\n");
        } else {
            let _ = writeln!(out, "\n## `[{name}]`\n");
        }
        if let Some(description) = description {
            let _ = writeln!(out, "{description}\n");
        }
        if required.contains(&name) {
            out.push_str("This section is required.\n\n");
        } else {
            out.push_str("This section is optional.\n\n");
        }

        if is_map {
            write_variants(&validator, name, &section["additionalProperties"], &mut out);
        } else {
            write_key_table(&validator, section, &mut out);
        }
    }
    out
}

const INTRO: &str = "# Configuration reference

<!-- Generated from the config types. Do not edit by hand: regenerate with
     `MATTER_RS_BLESS=1 cargo test -p matter-rs-config --all-features --test generated_docs`. -->

matter-rs is configured by one TOML file. `matter-rs init` writes a commented starter file,
`matter-rs validate` checks a file and reports every error and warning with its line and column,
and `matter-rs schema` prints the JSON schema of the file (also in
[`config.schema.json`](config.schema.json)).

The file is found in this order: the `--config <path>` option, the environment variable
`MATTER_RS_CONFIG`, then the platform default, `/etc/matter-rs/config.toml` on Linux and
`%ProgramData%\\matter-rs\\config.toml` on Windows.

Keys that are not listed here are errors, so that a mistyped key cannot be silently ignored.
Relative paths are relative to the directory of the configuration file.

**Secrets** such as the setup passcode accept three forms: the value itself, `{ env = \"NAME\" }`
to read it from an environment variable, or `{ file = \"PATH\" }` to read it from a file
(surrounding whitespace is ignored). `matter-rs validate` checks that the variable or file exists
but never prints a secret. On Unix, a secret written into a configuration file that other users
can read is reported as a warning.
";

/// Writes the table of keys of an object schema.
fn write_key_table(validator: &Validator<'_>, section: &Value, out: &mut String) {
    let Some(properties) = section.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required = required_keys(section);
    out.push_str("| Key | Type | Default | Description |\n|---|---|---|---|\n");
    for (key, property) in properties {
        let resolved = validator.resolve(property);
        let default = if required.contains(&key.as_str()) {
            String::from("required")
        } else {
            property
                .get("default")
                .or_else(|| resolved.get("default"))
                .filter(|value| !value.is_null() && !value.is_object())
                .map_or_else(
                    || String::from("–"),
                    |value| format!("`{}`", toml_literal(value)),
                )
        };
        let description = description(property)
            .or_else(|| description(resolved))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "| `{key}` | {} | {default} | {} |",
            escape(&type_text(validator, property)),
            escape(&description)
        );
    }
}

/// Writes one subsection per variant of a tagged union (the adapter kinds).
fn write_variants(validator: &Validator<'_>, name: &str, schema: &Value, out: &mut String) {
    let schema = validator.resolve(schema);
    let branches: Vec<&Value> = schema
        .get("oneOf")
        .and_then(Value::as_array)
        .map(|branches| branches.iter().map(|b| validator.resolve(b)).collect())
        .or_else(|| schema.get("properties").map(|_| vec![schema]))
        .unwrap_or_default();
    if branches.is_empty() {
        out.push_str("No kind is available yet.\n");
        return;
    }
    for branch in branches {
        let kind = branch["properties"]["kind"]["const"]
            .as_str()
            .unwrap_or("?");
        let _ = writeln!(out, "### `{name}` of kind `\"{kind}\"`\n");
        if let Some(description) = description(branch) {
            let _ = writeln!(out, "{description}\n");
        }
        write_key_table(validator, branch, out);
        out.push('\n');
    }
}

/// The type of a key, with its constraints, for the reference table.
fn type_text(validator: &Validator<'_>, schema: &Value) -> String {
    let schema = validator.resolve(schema);
    for keyword in ["oneOf", "anyOf"] {
        if let Some(Value::Array(branches)) = schema.get(keyword) {
            let all_constant = branches
                .iter()
                .all(|branch| validator.resolve(branch).get("const").is_some());
            if all_constant {
                return validator.describe(schema);
            }
            let parts: Vec<String> = branches
                .iter()
                .map(|branch| type_text(validator, branch))
                .collect();
            return parts.join(", or ");
        }
    }
    let mut text = match schema.get("type").and_then(Value::as_str) {
        Some("integer") => String::from("integer"),
        Some("string") => String::from("string"),
        Some("boolean") => String::from("boolean"),
        Some("number") => String::from("number"),
        Some("array") => String::from("array"),
        // Tables and anything else are described in words.
        _ => return validator.describe(schema),
    };
    match (
        schema.get("minimum").and_then(Value::as_i64),
        schema.get("maximum").and_then(Value::as_i64),
    ) {
        (Some(minimum), Some(maximum)) => {
            let _ = write!(text, ", {minimum} to {maximum}");
        }
        (Some(minimum), None) => {
            let _ = write!(text, ", at least {minimum}");
        }
        (None, Some(maximum)) => {
            let _ = write!(text, ", at most {maximum}");
        }
        (None, None) => {}
    }
    match (
        schema.get("minLength").and_then(Value::as_u64),
        schema.get("maxLength").and_then(Value::as_u64),
    ) {
        (Some(minimum), Some(maximum)) => {
            let _ = write!(text, ", {minimum} to {maximum} characters");
        }
        (Some(1), None) => text.push_str(", not empty"),
        (Some(minimum), None) => {
            let _ = write!(text, ", at least {minimum} characters");
        }
        (None, Some(maximum)) => {
            let _ = write!(text, ", at most {maximum} characters");
        }
        (None, None) => {}
    }
    text
}

fn required_keys(schema: &Value) -> Vec<&str> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|keys| keys.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// The description of a schema with its lines joined, as one paragraph.
fn description(schema: &Value) -> Option<String> {
    let text = schema.get("description")?.as_str()?;
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A JSON default written as TOML.
fn toml_literal(value: &Value) -> String {
    match value {
        Value::String(text) => format!("{text:?}"),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(toml_literal).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

/// Escapes a table cell.
fn escape(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::reference_markdown;

    #[test]
    fn reference_documents_every_section_and_key() {
        let text = reference_markdown();
        for needle in [
            "## `[node]`",
            "## `[network]`",
            "## `[state]`",
            "## `[log]`",
            "## `[bridge]`",
            "## `[adapters.<id>]`",
            "| `passcode` |",
            "| `discriminator` | integer, 0 to 4095 | required |",
            "| `port` | integer, 1 to 65535 | `5540` |",
            "| `level` | one of",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }

    #[test]
    fn reference_lists_the_kinds_of_the_build() {
        // Unit tests build the test adapter kind, so it shows up with its keys.
        let text = reference_markdown();
        assert!(text.contains("### `adapters` of kind `\"test\"`"), "{text}");
        assert!(text.contains("| `topic` | string | required |"), "{text}");
    }
}
