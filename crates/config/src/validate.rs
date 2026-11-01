//! Checks a parsed TOML document against the JSON schema of the config types and reports every
//! violation with its location.
//!
//! Deserializing with serde stops at the first error. Walking the spanned document against the
//! schema instead finds all of them in one run, and because the schema is generated from the
//! same types that serde fills afterwards, the two cannot drift apart. The walker understands
//! the subset of JSON schema that schemars produces for the config types: `$ref`, `type`,
//! `properties`, `required`, `additionalProperties`, `const`, `enum`, `oneOf`, `anyOf`,
//! `minimum`, `maximum`, `minLength`, `maxLength` and `items`.
//!
//! Messages name keys and expected values but never echo a value from the file, so that a
//! mistyped secret cannot end up in the output.

use core::fmt::Write as _;
use core::ops::Range;

use serde_json::{Map, Value};
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diagnostic::Collector;

/// A dotted key path such as `node.passcode`, for messages.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyPath(Vec<String>);

impl KeyPath {
    pub(crate) fn child(&self, key: &str) -> Self {
        let mut keys = self.0.clone();
        keys.push(key.to_owned());
        Self(keys)
    }

    pub(crate) fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    fn dotted(&self) -> String {
        self.0
            .iter()
            .map(|key| {
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    key.clone()
                } else {
                    format!("{key:?}")
                }
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    /// `` `node.port` `` for a value, or "the file" at the root.
    pub(crate) fn value_name(&self) -> String {
        if self.is_root() {
            String::from("the file")
        } else {
            format!("`{}`", self.dotted())
        }
    }

    /// `` in `[node]` `` for a table, or nothing at the root.
    pub(crate) fn table_suffix(&self) -> String {
        if self.is_root() {
            String::new()
        } else {
            format!(" in `[{}]`", self.dotted())
        }
    }
}

/// Walks documents against one root schema.
pub(crate) struct Validator<'s> {
    root: &'s Value,
}

impl<'s> Validator<'s> {
    pub(crate) fn new(root: &'s Value) -> Self {
        Self { root }
    }

    /// Follows `$ref` to the definition it names.
    pub(crate) fn resolve(&self, schema: &'s Value) -> &'s Value {
        let mut schema = schema;
        for _ in 0..32 {
            let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
                break;
            };
            match reference
                .strip_prefix("#/$defs/")
                .and_then(|name| self.root.get("$defs")?.get(name))
            {
                Some(target) => schema = target,
                None => break,
            }
        }
        schema
    }

    /// The schema of the root table's property `key`, if it has one.
    pub(crate) fn property(&self, schema: &'s Value, key: &str) -> Option<&'s Value> {
        self.resolve(schema).get("properties")?.get(key)
    }

    /// Checks `value` against `schema`.
    pub(crate) fn validate(
        &self,
        schema: &'s Value,
        value: &Spanned<DeValue<'_>>,
        path: &KeyPath,
        out: &mut Collector<'_>,
    ) {
        let schema = self.resolve(schema);
        let object = match schema {
            Value::Bool(true) => return,
            Value::Object(object) => object,
            _ => {
                out.error(
                    &value.span(),
                    format!("{} is not allowed", path.value_name()),
                );
                return;
            }
        };

        if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
            self.validate_alternatives(branches, value, path, out);
            return;
        }
        if let Some(branches) = object.get("anyOf").and_then(Value::as_array) {
            self.validate_alternatives(branches, value, path, out);
            return;
        }

        if let Some(expected) = object.get("type")
            && !type_matches(expected, value.get_ref())
        {
            out.error(
                &value.span(),
                format!(
                    "{} must be {}, found {}",
                    path.value_name(),
                    self.describe(schema),
                    value_kind(value.get_ref())
                ),
            );
            return;
        }

        if let Some(constant) = object.get("const")
            && !value_equals(value.get_ref(), constant)
        {
            out.error(
                &value.span(),
                format!(
                    "{} must be {}",
                    path.value_name(),
                    describe_literal(constant)
                ),
            );
            return;
        }
        if let Some(Value::Array(options)) = object.get("enum")
            && !options
                .iter()
                .any(|option| value_equals(value.get_ref(), option))
        {
            out.error(
                &value.span(),
                format!(
                    "{} must be one of {}",
                    path.value_name(),
                    join_literals(options)
                ),
            );
            return;
        }

        match value.get_ref() {
            DeValue::Integer(integer) => {
                let number = i128::from_str_radix(integer.as_str(), integer.radix()).ok();
                check_range(object, number, &value.span(), path, out);
            }
            DeValue::String(text) => check_length(object, text, &value.span(), path, out),
            DeValue::Table(table) => {
                self.validate_table(object, table, &value.span(), path, &[], out);
            }
            DeValue::Array(items) => {
                if let Some(item_schema) = object.get("items") {
                    for (index, item) in items.iter().enumerate() {
                        self.validate(item_schema, item, &path.child(&index.to_string()), out);
                    }
                }
            }
            DeValue::Float(_) | DeValue::Boolean(_) | DeValue::Datetime(_) => {}
        }
    }

    /// Checks the keys of a table. Keys listed in `skip` are left to the caller, including when
    /// they are required.
    pub(crate) fn validate_table(
        &self,
        schema: &'s Map<String, Value>,
        table: &DeTable<'_>,
        span: &Range<usize>,
        path: &KeyPath,
        skip: &[&str],
        out: &mut Collector<'_>,
    ) {
        let empty = Map::new();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        for (key, value) in table {
            let name: &str = key.get_ref();
            if skip.contains(&name) {
                continue;
            }
            if let Some(property) = properties.get(name) {
                self.validate(property, value, &path.child(name), out);
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    let mut message = format!("unknown key `{name}`{}", path.table_suffix());
                    if properties.is_empty() {
                        message.push_str("; no keys are allowed here");
                    } else {
                        let known: Vec<&str> = properties.keys().map(String::as_str).collect();
                        let _ = write!(message, "; expected one of `{}`", known.join("`, `"));
                    }
                    out.error(&key.span(), message);
                }
                Some(additional @ Value::Object(_)) => {
                    self.validate(additional, value, &path.child(name), out);
                }
                _ => {}
            }
        }
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                if skip.contains(&key) || table.iter().any(|(name, _)| name.get_ref() == key) {
                    continue;
                }
                let is_table = properties
                    .get(key)
                    .map(|property| self.resolve(property))
                    .and_then(|property| property.get("type"))
                    .is_some_and(|kind| kind == "object");
                let what = if is_table { "table" } else { "key" };
                let shown = if is_table && path.is_root() {
                    format!("[{key}]")
                } else {
                    key.to_owned()
                };
                out.error(
                    span,
                    format!("missing required {what} `{shown}`{}", path.table_suffix()),
                );
            }
        }
    }

    /// `oneOf` and `anyOf`: enumerations of constants, unions with a discriminating key, or
    /// alternatives that are tried one by one.
    fn validate_alternatives(
        &self,
        branches: &'s [Value],
        value: &Spanned<DeValue<'_>>,
        path: &KeyPath,
        out: &mut Collector<'_>,
    ) {
        let resolved: Vec<&Value> = branches.iter().map(|branch| self.resolve(branch)).collect();

        // An enumeration: every branch is a constant.
        let constants: Option<Vec<&Value>> = resolved.iter().map(|b| b.get("const")).collect();
        if let Some(constants) = constants {
            if !constants.iter().any(|c| value_equals(value.get_ref(), c)) {
                let options: Vec<Value> = constants.into_iter().cloned().collect();
                out.error(
                    &value.span(),
                    format!(
                        "{} must be one of {}",
                        path.value_name(),
                        join_literals(&options)
                    ),
                );
            }
            return;
        }

        // A tagged union: every branch fixes the same key to a constant.
        if let Some(tag) = discriminator(&resolved) {
            let DeValue::Table(table) = value.get_ref() else {
                out.error(
                    &value.span(),
                    format!(
                        "{} must be a table, found {}",
                        path.value_name(),
                        value_kind(value.get_ref())
                    ),
                );
                return;
            };
            let Some((_, tag_value)) = table.iter().find(|(key, _)| key.get_ref() == tag) else {
                out.error(
                    &value.span(),
                    format!("missing required key `{tag}`{}", path.table_suffix()),
                );
                return;
            };
            let chosen = resolved.iter().find(|branch| {
                value_equals(tag_value.get_ref(), &branch["properties"][tag]["const"])
            });
            match chosen {
                Some(branch) => self.validate(branch, value, path, out),
                None => {
                    let options: Vec<Value> = resolved
                        .iter()
                        .map(|branch| branch["properties"][tag]["const"].clone())
                        .collect();
                    out.error(
                        &tag_value.span(),
                        format!(
                            "{} must be one of {}",
                            path.child(tag).value_name(),
                            join_literals(&options)
                        ),
                    );
                }
            }
            return;
        }

        // Plain alternatives: valid if any branch accepts the value.
        let mut scratch_errors = Vec::new();
        for branch in &resolved {
            let mark = out.len();
            self.validate(branch, value, path, out);
            let errors = out.split_off(mark);
            if errors.is_empty() {
                return;
            }
            scratch_errors.push(errors);
        }

        // None does. If exactly one branch fits the shape of the value, its errors are the
        // helpful ones; otherwise say which shapes are accepted.
        let candidates: Vec<usize> = resolved
            .iter()
            .enumerate()
            .filter(|(_, branch)| fits_shape(branch, value.get_ref()))
            .map(|(index, _)| index)
            .collect();
        if let [only] = candidates.as_slice() {
            out.extend(scratch_errors.swap_remove(*only));
        } else {
            let shapes: Vec<String> = resolved.iter().map(|b| self.describe(b)).collect();
            out.error(
                &value.span(),
                format!("{} must be {}", path.value_name(), join_or(&shapes)),
            );
        }
    }

    /// A short description of what a schema accepts, for messages and the reference.
    pub(crate) fn describe(&self, schema: &'s Value) -> String {
        let schema = self.resolve(schema);
        if let Some(constant) = schema.get("const") {
            return describe_literal(constant);
        }
        if let Some(Value::Array(options)) = schema.get("enum") {
            return format!("one of {}", join_literals(options));
        }
        for keyword in ["oneOf", "anyOf"] {
            if let Some(Value::Array(branches)) = schema.get(keyword) {
                let resolved: Vec<&Value> = branches.iter().map(|b| self.resolve(b)).collect();
                let constants: Option<Vec<Value>> =
                    resolved.iter().map(|b| b.get("const").cloned()).collect();
                if let Some(constants) = constants {
                    return format!("one of {}", join_literals(&constants));
                }
                if let Some(tag) = discriminator(&resolved) {
                    return format!("a table with `{tag}`");
                }
                let shapes: Vec<String> = resolved.iter().map(|b| self.describe(b)).collect();
                return join_or(&shapes);
            }
        }
        match schema.get("type").and_then(Value::as_str) {
            Some("object") => {
                let required: Vec<&str> = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|keys| keys.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if required.is_empty() {
                    String::from("a table")
                } else {
                    format!("a table with `{}`", required.join("`, `"))
                }
            }
            Some("integer") => String::from("an integer"),
            Some("number") => String::from("a number"),
            Some("string") => String::from("a string"),
            Some("boolean") => String::from("a boolean"),
            Some("array") => String::from("an array"),
            _ => String::from("a value"),
        }
    }
}

/// The key that every branch of a union fixes to a constant, if there is one.
fn discriminator<'a>(branches: &[&'a Value]) -> Option<&'a str> {
    let first = branches.first()?.get("properties")?.as_object()?;
    first
        .iter()
        .filter(|(_, property)| property.get("const").is_some())
        .map(|(key, _)| key.as_str())
        .find(|key| {
            branches
                .iter()
                .all(|branch| branch["properties"][key].get("const").is_some())
        })
}

/// Whether a branch could be meant for the value: same type and, for tables, all required keys
/// present.
fn fits_shape(branch: &Value, value: &DeValue<'_>) -> bool {
    if let Some(expected) = branch.get("type")
        && !type_matches(expected, value)
    {
        return false;
    }
    match (value, branch.get("required")) {
        (DeValue::Table(table), Some(Value::Array(required))) => required
            .iter()
            .filter_map(Value::as_str)
            .all(|key| table.iter().any(|(name, _)| name.get_ref() == key)),
        _ => true,
    }
}

fn check_range(
    schema: &Map<String, Value>,
    number: Option<i128>,
    span: &Range<usize>,
    path: &KeyPath,
    out: &mut Collector<'_>,
) {
    let minimum = schema.get("minimum").and_then(as_i128);
    let maximum = schema.get("maximum").and_then(as_i128);
    let in_range = number.is_some_and(|number| {
        minimum.is_none_or(|minimum| number >= minimum)
            && maximum.is_none_or(|maximum| number <= maximum)
    });
    if in_range || (number.is_some() && minimum.is_none() && maximum.is_none()) {
        return;
    }
    let bounds = match (minimum, maximum) {
        (Some(minimum), Some(maximum)) => format!("between {minimum} and {maximum}"),
        (Some(minimum), None) => format!("at least {minimum}"),
        (None, Some(maximum)) => format!("at most {maximum}"),
        (None, None) => String::from("a smaller number"),
    };
    out.error(span, format!("{} must be {bounds}", path.value_name()));
}

fn check_length(
    schema: &Map<String, Value>,
    text: &str,
    span: &Range<usize>,
    path: &KeyPath,
    out: &mut Collector<'_>,
) {
    let length = text.chars().count();
    if let Some(minimum) = schema.get("minLength").and_then(Value::as_u64)
        && (length as u64) < minimum
    {
        let message = if minimum == 1 {
            format!("{} must not be empty", path.value_name())
        } else {
            format!(
                "{} must be at least {minimum} characters long",
                path.value_name()
            )
        };
        out.error(span, message);
    }
    if let Some(maximum) = schema.get("maxLength").and_then(Value::as_u64)
        && (length as u64) > maximum
    {
        out.error(
            span,
            format!(
                "{} must be at most {maximum} characters long",
                path.value_name()
            ),
        );
    }
}

fn as_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

/// Whether a TOML value has one of the JSON schema types in `expected`.
fn type_matches(expected: &Value, value: &DeValue<'_>) -> bool {
    let matches = |kind: &str| match value {
        DeValue::String(_) => kind == "string",
        DeValue::Integer(_) => kind == "integer" || kind == "number",
        DeValue::Float(_) => kind == "number",
        DeValue::Boolean(_) => kind == "boolean",
        DeValue::Array(_) => kind == "array",
        DeValue::Table(_) => kind == "object",
        DeValue::Datetime(_) => false,
    };
    match expected {
        Value::String(kind) => matches(kind),
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).any(matches),
        _ => true,
    }
}

/// What kind of TOML value this is, for messages.
fn value_kind(value: &DeValue<'_>) -> &'static str {
    match value {
        DeValue::String(_) => "a string",
        DeValue::Integer(_) => "an integer",
        DeValue::Float(_) => "a float",
        DeValue::Boolean(_) => "a boolean",
        DeValue::Datetime(_) => "a date-time",
        DeValue::Array(_) => "an array",
        DeValue::Table(_) => "a table",
    }
}

/// Whether a TOML scalar equals a JSON constant.
fn value_equals(value: &DeValue<'_>, constant: &Value) -> bool {
    match (value, constant) {
        (DeValue::String(text), Value::String(expected)) => text.as_ref() == expected,
        (DeValue::Boolean(flag), Value::Bool(expected)) => flag == expected,
        (DeValue::Integer(integer), Value::Number(expected)) => {
            i128::from_str_radix(integer.as_str(), integer.radix()).ok() == as_i128(constant)
                && expected.is_i64() | expected.is_u64()
        }
        _ => false,
    }
}

fn describe_literal(value: &Value) -> String {
    match value {
        Value::String(text) => format!("`\"{text}\"`"),
        other => format!("`{other}`"),
    }
}

fn join_literals(values: &[Value]) -> String {
    values
        .iter()
        .map(describe_literal)
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_or(items: &[String]) -> String {
    match items {
        [] => String::from("nothing"),
        [only] => only.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}
