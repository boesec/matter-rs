//! Configuration of matter-rs (layer 5).
//!
//! The service is configured by one TOML file. This crate owns the config model, its validation
//! and its schema, and keeps configuration apart from runtime state. It assembles the sections of
//! the roles and adapters that the binary's features select, which is why the same `role-*` and
//! `adapter-*` features exist here and are forwarded to the crates they name.
//!
//! - [`ConfigFile`] and its section types define the file format. The JSON schema
//!   ([`json_schema`]) and the Markdown reference ([`reference_markdown`]) are generated from
//!   them.
//! - [`load`] reads a file, checks it and returns a [`Config`] with secrets and defaults
//!   resolved, or a [`Report`] with every error and warning, each with file, line and column.
//! - [`SecretValue`] is the form of a secret in the file; loading turns it into the engine's
//!   [`Secret`](matter_rs_engine::Secret).
//! - [`new_starter_config`] produces the commented starter file with a fresh passcode and
//!   discriminator.
//!
//! The crate needs the standard library (files, environment variables).

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

mod adapters;
mod diagnostic;
mod environment;
mod error;
mod load;
mod markdown;
mod model;
mod passcode;
mod paths;
mod schema;
mod secret;
mod starter;
mod validate;

use std::path::Path;

pub use adapters::{AdapterConfig, AdapterKind, adapter_kinds};
pub use diagnostic::{Diagnostic, Location, Report, Severity};
pub use environment::{Environment, Platform, SystemEnvironment};
pub use error::Error;
pub use load::{Config, ConfigSource, Loaded, NodeConfig, StateConfig, load_source};
pub use markdown::reference_markdown;
pub use model::{
    BridgeSection, ConfigFile, DEFAULT_PORT, DEFAULT_PRODUCT_ID, DEFAULT_VENDOR_ID, LogFormat,
    LogLevel, LogSection, NetworkSection, NodeSection, StateSection,
};
pub use passcode::{
    DISCRIMINATOR_MAX, PASSCODE_MAX, PasscodeError, TEST_PASSCODE, check_passcode,
    random_discriminator, random_passcode,
};
pub use paths::{
    CONFIG_ENV, ConfigPathSource, default_config_path, default_state_dir, resolve_config_path,
};
pub use schema::{config_schema, json_schema};
pub use secret::{EnvSource, FileSource, SecretValue};
pub use starter::{new_starter_config, starter_config};

/// Reads and loads the configuration file at `path`.
///
/// Diagnostics name the file as `path` displays. Relative paths in the file are taken relative to
/// the file's directory. On Unix the file's permission bits are checked for plain secrets.
///
/// # Errors
///
/// [`Error::Read`] when the file cannot be read, [`Error::Invalid`] with every error and warning
/// when it is not valid.
pub fn load(path: &Path, environment: &dyn Environment) -> Result<Loaded, Error> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    let name = path.display().to_string();
    let source = ConfigSource {
        name: &name,
        text: &text,
        directory: path.parent(),
        mode: file_mode(path),
    };
    load_source(&source, environment)
}

/// The Unix permission bits of a file.
#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode())
}

/// Windows has no Unix permission bits.
#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}
