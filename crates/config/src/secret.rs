//! Secrets in the configuration file: written in place, or read from an environment variable or
//! a file when the configuration is loaded.

use core::fmt;
use core::str::FromStr;
use std::path::{Path, PathBuf};

use matter_rs_engine::Secret;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::environment::Environment;

/// A secret as it is written in the configuration file.
///
/// Every secret field accepts three forms:
///
/// ```toml
/// passcode = 20202021                        # the value itself
/// passcode = { env = "MATTER_RS_PASSCODE" }  # read from an environment variable
/// passcode = { file = "/run/secrets/code" }  # read from a file; surrounding whitespace is ignored
/// ```
///
/// Loading resolves all three into the same [`Secret`]. [`Debug`](fmt::Debug) never shows a
/// value written in place.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(
    description = "A secret: the value itself, `{ env = \"NAME\" }` to read it from an environment \
                   variable, or `{ file = \"PATH\" }` to read it from a file (surrounding \
                   whitespace is ignored)."
)]
pub enum SecretValue<T> {
    /// The secret written into the configuration file itself.
    Plain(T),
    /// The secret is read from an environment variable.
    Env(EnvSource),
    /// The secret is read from a file.
    File(FileSource),
}

/// Names the environment variable that holds a secret.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvSource {
    /// Name of the environment variable that holds the secret.
    pub env: String,
}

/// Names the file that holds a secret.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileSource {
    /// Path of the file that holds the secret. A relative path is relative to the directory of
    /// the configuration file.
    pub file: PathBuf,
}

impl<T> fmt::Debug for SecretValue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(_) => f
                .debug_tuple("Plain")
                .field(&matter_rs_engine::Secret::new(()))
                .finish(),
            Self::Env(source) => f.debug_tuple("Env").field(source).finish(),
            Self::File(source) => f.debug_tuple("File").field(source).finish(),
        }
    }
}

impl<T> SecretValue<T> {
    /// Whether the secret is written into the configuration file itself.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        matches!(self, Self::Plain(_))
    }
}

/// Why a secret could not be resolved. The messages name where the secret was looked for, never
/// the secret itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveError {
    /// The environment variable is not set.
    MissingVariable(String),
    /// The environment variable is set but does not hold a valid value.
    InvalidVariable(String),
    /// The file cannot be read.
    UnreadableFile(PathBuf, String),
    /// The file does not hold a valid value.
    InvalidFile(PathBuf),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => {
                write!(f, "the environment variable `{name}` is not set")
            }
            Self::InvalidVariable(name) => write!(
                f,
                "the environment variable `{name}` does not hold a valid value"
            ),
            Self::UnreadableFile(path, reason) => {
                write!(
                    f,
                    "the secret file `{}` cannot be read: {reason}",
                    path.display()
                )
            }
            Self::InvalidFile(path) => write!(
                f,
                "the secret file `{}` does not hold a valid value",
                path.display()
            ),
        }
    }
}

impl<T: FromStr + Clone> SecretValue<T> {
    /// Resolves the secret. A relative file path is taken relative to `directory`.
    pub(crate) fn resolve(
        &self,
        environment: &dyn Environment,
        directory: Option<&Path>,
    ) -> Result<Secret<T>, ResolveError> {
        match self {
            Self::Plain(value) => Ok(Secret::new(value.clone())),
            Self::Env(EnvSource { env }) => {
                let text = environment
                    .var(env)
                    .ok_or_else(|| ResolveError::MissingVariable(env.clone()))?;
                text.trim()
                    .parse()
                    .map(Secret::new)
                    .map_err(|_| ResolveError::InvalidVariable(env.clone()))
            }
            Self::File(FileSource { file }) => {
                let path = match directory {
                    Some(directory) if file.is_relative() => directory.join(file),
                    _ => file.clone(),
                };
                let text = environment.read_file(&path).map_err(|error| {
                    ResolveError::UnreadableFile(path.clone(), error.to_string())
                })?;
                text.trim()
                    .parse()
                    .map(Secret::new)
                    .map_err(|_| ResolveError::InvalidFile(path))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EnvSource, SecretValue};

    #[test]
    fn debug_hides_a_plain_value_but_names_the_source() {
        let plain: SecretValue<u32> = SecretValue::Plain(12_348_765);
        let output = format!("{plain:?}");
        assert!(!output.contains("12348765"), "{output}");
        assert!(output.contains("[REDACTED]"), "{output}");

        let env: SecretValue<u32> = SecretValue::Env(EnvSource {
            env: String::from("PASSCODE"),
        });
        assert!(format!("{env:?}").contains("PASSCODE"));
    }
}
