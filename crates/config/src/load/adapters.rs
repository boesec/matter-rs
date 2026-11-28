//! The rules of the adapter sections beyond their schema, and the resolution of their secrets.

use std::collections::BTreeMap;

use toml::Spanned;
use toml::de::DeTable;

#[cfg(feature = "adapter-mqtt")]
use super::span_at;
use crate::adapters::AdapterConfig;
use crate::diagnostic::Collector;
use crate::environment::Environment;
use crate::load::ConfigSource;

/// Checks every adapter of a valid file against the rules of its kind and resolves its secrets.
/// `None` when something failed; everything is reported.
///
/// Only the MQTT adapter has rules and secrets of its own so far; a build without it keeps the
/// adapters as they were parsed.
#[cfg_attr(not(feature = "adapter-mqtt"), allow(unused_variables))]
pub(super) fn resolve_adapters(
    file: &crate::model::ConfigFile,
    document: &Spanned<DeTable<'_>>,
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    out: &mut Collector<'_>,
) -> Option<BTreeMap<String, AdapterConfig>> {
    #[cfg(not(feature = "adapter-mqtt"))]
    {
        Some(file.adapters.clone())
    }
    #[cfg(feature = "adapter-mqtt")]
    {
        let mut adapters = BTreeMap::new();
        let mut complete = true;
        for (id, adapter) in &file.adapters {
            let resolved = match adapter {
                AdapterConfig::Mqtt(config) => {
                    resolve_mqtt(id, config, document, source, environment, out)
                        .map(AdapterConfig::Mqtt)
                }
                #[allow(unreachable_patterns)]
                other => Some(other.clone()),
            };
            match resolved {
                Some(resolved) => {
                    adapters.insert(id.clone(), resolved);
                }
                None => complete = false,
            }
        }
        complete.then_some(adapters)
    }
}

/// The MQTT adapter's own checks, each at its key, and the password resolved into its plain
/// form.
#[cfg(feature = "adapter-mqtt")]
fn resolve_mqtt(
    id: &str,
    config: &matter_rs_adapter_mqtt::Config,
    document: &Spanned<DeTable<'_>>,
    source: &ConfigSource<'_>,
    environment: &dyn Environment,
    out: &mut Collector<'_>,
) -> Option<matter_rs_adapter_mqtt::Config> {
    use matter_rs_adapter_mqtt::SecretText;
    use matter_rs_engine::Secret;

    let mut ok = true;
    for issue in config.check() {
        out.error(
            &span_at(document, &["adapters", id, issue.key]),
            format!("`adapters.{id}.{}`: {}", issue.key, issue.message),
        );
        ok = false;
    }
    let span = span_at(document, &["adapters", id, "password"]);
    let key = format!("`adapters.{id}.password`");
    let password = match &config.password {
        None => None,
        Some(SecretText::Plain(secret)) => {
            if let Some(mode) = source.mode
                && mode & 0o004 != 0
            {
                out.warning(
                    &span,
                    format!(
                        "{key} is written into a file that other users can read (mode {:o}); \
                         restrict the file (chmod 600) or read the password with \
                         `{{ env = \"NAME\" }}` or `{{ file = \"PATH\" }}`",
                        mode & 0o777
                    ),
                );
            }
            Some(SecretText::Plain(secret.clone()))
        }
        Some(SecretText::Env(name)) => match environment.var(name) {
            Some(value) => Some(SecretText::Plain(Secret::new(value))),
            None => {
                out.error(
                    &span,
                    format!("{key}: the environment variable `{name}` is not set"),
                );
                ok = false;
                None
            }
        },
        Some(SecretText::File(file)) => {
            let path = match source.directory {
                Some(directory) if file.is_relative() => directory.join(file),
                _ => file.clone(),
            };
            match environment.read_file(&path) {
                Ok(text) => Some(SecretText::Plain(Secret::new(text.trim().to_owned()))),
                Err(error) => {
                    out.error(
                        &span,
                        format!(
                            "{key}: the secret file `{}` cannot be read: {error}",
                            path.display()
                        ),
                    );
                    ok = false;
                    None
                }
            }
        }
    };
    ok.then(|| matter_rs_adapter_mqtt::Config {
        password,
        ..config.clone()
    })
}
