//! Loading a configuration file: parse, check against the schema, deserialize, apply the rules
//! the schema cannot express, resolve secrets and defaults.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use matter_rs_engine::Secret;
use serde::Deserialize;
use serde_json::Value;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::adapters::{AdapterConfig, AdapterKind, adapter_kinds};
use crate::diagnostic::{Collector, Report};
use crate::environment::Environment;
use crate::error::Error;
use crate::model::{BridgeSection, ConfigFile, LogSection, NetworkSection};
use crate::passcode::{TEST_PASSCODE, check_passcode};
use crate::paths::default_state_dir;
use crate::schema::config_schema;
use crate::secret::SecretValue;
use crate::validate::{KeyPath, Validator};

/// A configuration file to load: its text and what is known about where it came from.
#[derive(Debug, Clone, Copy)]
pub struct ConfigSource<'a> {
    /// The file name used in diagnostics, usually the path as the user gave it.
    pub name: &'a str,
    /// The content of the file.
    pub text: &'a str,
    /// The directory of the file. Relative paths in the file (state directory, secret files) are
    /// relative to it; without it they stay as written.
    pub directory: Option<&'a Path>,
    /// The Unix permission bits of the file, if known. A plain secret in a file that others may
    /// read is reported as a warning.
    pub mode: Option<u32>,
}

/// A loaded, valid configuration with secrets and defaults resolved.
///
/// Its [`Debug`](core::fmt::Debug) output contains no secret.
#[derive(Debug, Clone)]
pub struct Config {
    /// Identity and commissioning data of the node.
    pub node: NodeConfig,
    /// Network settings.
    pub network: NetworkSection,
    /// The state directory.
    pub state: StateConfig,
    /// Logging.
    pub log: LogSection,
    /// The bridge role, if the section is present.
    pub bridge: Option<BridgeSection>,
    /// Adapter instances by id.
    pub adapters: BTreeMap<String, AdapterConfig>,
}

/// Identity and commissioning data of the node, with the passcode resolved.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Vendor id.
    pub vendor_id: u16,
    /// Product id.
    pub product_id: u16,
    /// Vendor name.
    pub vendor_name: String,
    /// Product name.
    pub product_name: String,
    /// Node label.
    pub node_label: String,
    /// Serial number, if configured.
    pub serial_number: Option<String>,
    /// Setup passcode.
    pub passcode: Secret<u32>,
    /// Discriminator.
    pub discriminator: u16,
}

/// The effective state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateConfig {
    /// The directory. It is not created by loading.
    pub dir: PathBuf,
    /// Whether it is the platform default rather than configured.
    pub is_default: bool,
}

/// The result of loading a valid file: the configuration, the warnings about it, and which
/// sections the file contains.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The configuration.
    pub config: Config,
    /// Warnings; contains no errors.
    pub report: Report,
    /// The sections present in the file, in file order of their names: `node`, `network`,
    /// `state`, `log`, `bridge`, and `adapters.<id>` per adapter instance.
    pub sections: Vec<String>,
}

/// What the build contains, as far as the configuration cares.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Build {
    pub(crate) role_bridge: bool,
    pub(crate) adapter_kinds: &'static [AdapterKind],
    pub(crate) linux: bool,
}

impl Build {
    pub(crate) fn current() -> Self {
        Self {
            role_bridge: cfg!(feature = "role-bridge"),
            adapter_kinds: adapter_kinds(),
            linux: cfg!(target_os = "linux"),
        }
    }
}

/// Loads a configuration from its text.
///
/// # Errors
///
/// [`Error::Invalid`] with every error and warning when the file is not valid.
pub fn load_source(
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
) -> Result<Loaded, Error> {
    load_with_build(source, environment, &Build::current())
}

pub(crate) fn load_with_build(
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    build: &Build,
) -> Result<Loaded, Error> {
    let mut out = Collector::new(source.name, source.text);

    let (document, syntax_errors) = DeTable::parse_recoverable(source.text);
    if !syntax_errors.is_empty() {
        for error in syntax_errors {
            let span = error.span().unwrap_or(0..0);
            out.error(&span, error.message().trim_end().to_owned());
        }
        return Err(Error::Invalid(out.into_report()));
    }

    let schema = config_schema();
    check_structure(&document, &schema, build, &mut out);

    // Only a structurally valid document can be deserialized. The rules below still run on an
    // invalid one, as far as the values they need are readable, so that one run reports all
    // errors.
    let file: Option<ConfigFile> = if out.error_count() == 0 {
        match toml::from_str(source.text) {
            Ok(file) => Some(file),
            Err(error) => {
                // The schema check passed, so this is a mismatch between schema and types;
                // still report it with its location rather than failing without one.
                let span = error.span().unwrap_or(0..0);
                out.error(&span, error.message().trim_end().to_owned());
                None
            }
        }
    } else {
        None
    };

    let resolved = check_rules(
        file.as_ref(),
        &document,
        source,
        environment,
        build,
        &mut out,
    );
    let report = out.into_report();
    match (file, resolved) {
        (
            Some(file),
            Resolved {
                passcode: Some(passcode),
                state: Some(state),
            },
        ) if !report.has_errors() => Ok(Loaded {
            config: build_config(file, passcode, state),
            report,
            sections: sections(&document),
        }),
        _ => Err(Error::Invalid(report)),
    }
}

/// The schema check of the whole document, with the top-level keys that need more than the
/// schema handled here.
fn check_structure(
    document: &Spanned<DeTable<'_>>,
    schema: &Value,
    build: &Build,
    out: &mut Collector<'_>,
) {
    let validator = Validator::new(schema);
    let root = validator.resolve(schema);
    let Some(root_object) = root.as_object() else {
        return;
    };
    validator.validate_table(
        root_object,
        document.get_ref(),
        &document.span(),
        &KeyPath::default(),
        &["bridge", "adapters", "devices"],
        out,
    );

    for (key, value) in document.get_ref() {
        match key.get_ref().as_ref() {
            "devices" => out.error(
                &key.span(),
                "`devices` is reserved for device definitions, which this version does not \
                 support yet",
            ),
            "bridge" if !build.role_bridge => out.error(
                &key.span(),
                "the `[bridge]` section needs the Cargo feature `role-bridge`, which is not \
                 compiled into this build",
            ),
            "bridge" => {
                if let Some(property) = validator.property(schema, "bridge") {
                    validator.validate(property, value, &KeyPath::default().child("bridge"), out);
                }
            }
            "adapters" => check_adapters(&validator, schema, value, build, out),
            _ => {}
        }
    }
}

/// Checks every `[adapters.<id>]` table: a valid id, a known and compiled-in kind, and the keys
/// of that kind.
fn check_adapters(
    validator: &Validator<'_>,
    schema: &Value,
    value: &Spanned<DeValue<'_>>,
    build: &Build,
    out: &mut Collector<'_>,
) {
    let path = KeyPath::default().child("adapters");
    let DeValue::Table(instances) = value.get_ref() else {
        out.error(
            &value.span(),
            "`adapters` must be a table of adapter instances, `[adapters.<id>]`",
        );
        return;
    };
    let instance_schema = validator
        .property(schema, "adapters")
        .and_then(|adapters| validator.resolve(adapters).get("additionalProperties"));

    for (id, instance) in instances {
        let instance_path = path.child(id.get_ref());
        if !is_valid_id(id.get_ref()) {
            out.error(
                &id.span(),
                format!(
                    "adapter id `{}` is not valid; use lower-case letters, digits, `-` and `_`, \
                     starting with a letter or digit",
                    id.get_ref()
                ),
            );
        }
        let DeValue::Table(table) = instance.get_ref() else {
            out.error(
                &instance.span(),
                format!("{} must be a table", instance_path.value_name()),
            );
            continue;
        };
        let Some((_, kind_value)) = table.iter().find(|(key, _)| key.get_ref() == "kind") else {
            out.error(
                &instance.span(),
                format!(
                    "missing required key `kind`{}; it selects the adapter",
                    instance_path.table_suffix()
                ),
            );
            continue;
        };
        let DeValue::String(kind) = kind_value.get_ref() else {
            out.error(
                &kind_value.span(),
                format!(
                    "{} must be a string",
                    instance_path.child("kind").value_name()
                ),
            );
            continue;
        };
        let Some(known) = build
            .adapter_kinds
            .iter()
            .find(|known| known.kind == kind.as_ref())
        else {
            let known: Vec<&str> = build.adapter_kinds.iter().map(|known| known.kind).collect();
            let hint = if known.is_empty() {
                String::from("this version knows no adapter kinds")
            } else {
                format!("known kinds are `{}`", known.join("`, `"))
            };
            out.error(
                &kind_value.span(),
                format!(
                    "unknown adapter kind `{kind}`{}; {hint}",
                    instance_path.table_suffix()
                ),
            );
            continue;
        };
        if !known.compiled_in {
            out.error(
                &kind_value.span(),
                format!(
                    "adapter kind `{kind}` is not compiled into this build; it needs the Cargo \
                     feature `{}`",
                    known.feature
                ),
            );
            continue;
        }
        if let Some(instance_schema) = instance_schema {
            validator.validate(instance_schema, instance, &instance_path, out);
        }
    }
}

fn is_valid_id(id: &str) -> bool {
    id.chars()
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// What the rules resolved: the passcode and the state directory, each `None` when it failed or
/// could not be read.
struct Resolved {
    passcode: Option<Secret<u32>>,
    state: Option<StateConfig>,
}

/// The rules the schema cannot express, secret resolution and defaults.
///
/// The rules read their values from the document, so that they also run when the structure check
/// failed; a value that is missing or of the wrong type is left to the structure check. `file` is
/// the deserialized file when the structure is valid.
fn check_rules(
    file: Option<&ConfigFile>,
    document: &Spanned<DeTable<'_>>,
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    build: &Build,
    out: &mut Collector<'_>,
) -> Resolved {
    let span_of = |path: &[&str]| span_at(document, path);

    let ipv4 = bool_at(document, &["network", "ipv4"], true);
    let ipv6 = bool_at(document, &["network", "ipv6"], true);
    if ipv4 == Some(false) && ipv6 == Some(false) {
        out.error(
            &span_of(&["network", "ipv6"]),
            "`network.ipv4` and `network.ipv6` are both false; at least one must be enabled",
        );
    }

    if bool_at(document, &["log", "journald"], false) == Some(true) && !build.linux {
        out.warning(
            &span_of(&["log", "journald"]),
            "`log.journald` only works on Linux and is ignored on this platform",
        );
    }

    Resolved {
        passcode: check_passcode_rules(file, document, source, environment, out),
        state: resolve_state_dir(document, source, environment, out),
    }
}

/// Resolves the passcode, checks it against the rules of the specification and warns about the
/// test passcode and about a plain passcode in a file others can read.
fn check_passcode_rules(
    file: Option<&ConfigFile>,
    document: &Spanned<DeTable<'_>>,
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    out: &mut Collector<'_>,
) -> Option<Secret<u32>> {
    let span = span_at(document, &["node", "passcode"]);
    let secret: SecretValue<u32> = match file {
        Some(file) => file.node.passcode.clone(),
        // Without a valid file, read the value on its own: it is an inline value whose span
        // covers exactly its text.
        None => {
            let value = value_at(document, &["node", "passcode"])?;
            let text = source.text.get(value.span())?;
            let deserializer = toml::de::ValueDeserializer::parse(text).ok()?;
            SecretValue::deserialize(deserializer).ok()?
        }
    };

    if secret.is_plain()
        && let Some(mode) = source.mode
        && mode & 0o004 != 0
    {
        out.warning(
            &span,
            format!(
                "`node.passcode` is written into a file that other users can read (mode {:o}); \
                 restrict the file (chmod 600) or read the passcode with `{{ env = \"NAME\" }}` \
                 or `{{ file = \"PATH\" }}`",
                mode & 0o777
            ),
        );
    }

    match secret.resolve(environment, source.directory) {
        Ok(passcode) => {
            let value = *passcode.expose();
            match check_passcode(value) {
                Ok(()) if value == TEST_PASSCODE => out.warning(
                    &span,
                    "`node.passcode` is the well-known test passcode; anybody who knows it can \
                     commission this node",
                ),
                Ok(()) => {}
                Err(error) => {
                    out.error(&span, format!("`node.passcode`: {error}"));
                    return None;
                }
            }
            Some(passcode)
        }
        Err(error) => {
            out.error(&span, format!("`node.passcode`: {error}"));
            None
        }
    }
}

/// The configured state directory, relative to the configuration file, or the platform default.
fn resolve_state_dir(
    document: &Spanned<DeTable<'_>>,
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    out: &mut Collector<'_>,
) -> Option<StateConfig> {
    match value_at(document, &["state", "dir"]).map(Spanned::get_ref) {
        Some(DeValue::String(dir)) => {
            let dir = PathBuf::from(dir.as_ref());
            Some(StateConfig {
                dir: match source.directory {
                    Some(directory) if dir.is_relative() => directory.join(dir),
                    _ => dir,
                },
                is_default: false,
            })
        }
        Some(_) => None,
        None => {
            let dir = default_state_dir(environment);
            if dir.is_none() {
                out.error(
                    &(0..0),
                    "the default state directory cannot be determined because the environment \
                     variables it is derived from are not set; set `[state] dir`",
                );
            }
            dir.map(|dir| StateConfig {
                dir,
                is_default: true,
            })
        }
    }
}

/// The configuration from a valid file and the values the rules resolved.
fn build_config(file: ConfigFile, passcode: Secret<u32>, state: StateConfig) -> Config {
    let node = file.node;
    Config {
        node: NodeConfig {
            vendor_id: node.vendor_id,
            product_id: node.product_id,
            vendor_name: node.vendor_name,
            product_name: node.product_name,
            node_label: node.node_label,
            serial_number: node.serial_number,
            passcode,
            discriminator: node.discriminator,
        },
        network: file.network,
        state,
        log: file.log,
        bridge: file.bridge,
        adapters: file.adapters,
    }
}

/// The value at `path`, if every table on the way and the value exist.
fn value_at<'d, 'i>(
    document: &'d Spanned<DeTable<'i>>,
    path: &[&str],
) -> Option<&'d Spanned<DeValue<'i>>> {
    let (last, tables) = path.split_last()?;
    let mut table = document.get_ref();
    for key in tables {
        let (_, value) = table.iter().find(|(name, _)| name.get_ref() == key)?;
        let DeValue::Table(inner) = value.get_ref() else {
            return None;
        };
        table = inner;
    }
    table
        .iter()
        .find(|(name, _)| name.get_ref() == last)
        .map(|(_, value)| value)
}

/// The boolean at `path`: `default` when absent, `None` when it is not a boolean.
fn bool_at(document: &Spanned<DeTable<'_>>, path: &[&str], default: bool) -> Option<bool> {
    match value_at(document, path).map(Spanned::get_ref) {
        None => Some(default),
        Some(DeValue::Boolean(value)) => Some(*value),
        Some(_) => None,
    }
}

/// The span of the value at `path`, or of the nearest table above it that exists.
fn span_at(document: &Spanned<DeTable<'_>>, path: &[&str]) -> Range<usize> {
    let mut span = document.span();
    let mut table = document.get_ref();
    for key in path {
        let Some((_, value)) = table.iter().find(|(name, _)| name.get_ref() == key) else {
            break;
        };
        span = value.span();
        match value.get_ref() {
            DeValue::Table(inner) => table = inner,
            _ => break,
        }
    }
    span
}

/// The sections present in the document.
fn sections(document: &Spanned<DeTable<'_>>) -> Vec<String> {
    let mut sections = Vec::new();
    for (key, value) in document.get_ref() {
        let key: &str = key.get_ref();
        match (key, value.get_ref()) {
            ("adapters", DeValue::Table(instances)) => {
                sections.extend(
                    instances
                        .iter()
                        .map(|(id, _)| format!("adapters.{}", id.get_ref())),
                );
            }
            _ => sections.push(key.to_owned()),
        }
    }
    sections
}

#[cfg(test)]
mod tests;
