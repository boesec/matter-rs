//! The `[adapters.<id>]` section of kind `mqtt`, the broker URL and the broker password.

use core::fmt;
use std::path::PathBuf;

use matter_rs_engine::Secret;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;

use crate::error::Error;

/// The default instance name: the topic prefix of the bridge's own items on the broker.
pub const DEFAULT_NAME: &str = "matter";
/// The default keep-alive interval in seconds.
pub const DEFAULT_KEEP_ALIVE: u16 = 30;
/// The default first reconnect delay in seconds.
pub const DEFAULT_RECONNECT_INITIAL: f64 = 1.0;
/// The default longest reconnect delay in seconds.
pub const DEFAULT_RECONNECT_MAX: f64 = 60.0;
/// The port of `mqtt://` when the URL names none.
pub const DEFAULT_PORT: u16 = 1883;
/// The port of `mqtts://` when the URL names none.
pub const DEFAULT_TLS_PORT: u16 = 8883;

/// The keys of an `[adapters.<id>]` table with `kind = "mqtt"`.
///
/// Everything beyond the schema (a usable URL, a valid instance name, consistent reconnect
/// delays) is checked by [`Config::check`], which the configuration loader calls to report the
/// problems with their location, and again by the adapter's factory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "An MQTT broker: the bridge publishes its own state on it as an mqtt-smarthome \
                   instance and reads and controls the devices of `[devices]` that name this \
                   adapter."
)]
pub struct Config {
    /// Broker URL: `mqtt://host[:port]` (port 1883 when absent). `mqtts://` is reserved for a
    /// TLS build. The URL may carry `user:password@`, but the `username` and `password` keys
    /// are the recommended place for credentials and win over the URL.
    #[schemars(length(min = 1))]
    pub url: String,
    /// User name for the broker.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub username: Option<String>,
    /// Password for the broker. A secret: write it here, or read it with `{ env = "NAME" }` or
    /// `{ file = "PATH" }`.
    #[serde(default)]
    pub password: Option<SecretText>,
    /// MQTT client id. Default `matter-rs-<id>` with the id of this adapter table.
    #[serde(default)]
    #[schemars(length(min = 1))]
    pub client_id: Option<String>,
    /// The instance name of the bridge on this broker: the first topic level of its own items,
    /// `<name>/connected`, `<name>/info`, `<name>/status/...`. Must not contain `/`, `+` or `#`.
    #[serde(default = "default_name")]
    #[schemars(length(min = 1))]
    pub name: String,
    /// CA certificate file (PEM) for `mqtts://` with a private CA. Reserved for a TLS build.
    #[serde(default)]
    pub tls_ca: Option<PathBuf>,
    /// Publish the bridge's status items as `{ "val": ..., "ts": ..., "lc": ... }` objects
    /// (the mqtt-smarthome default). `false` publishes plain values.
    #[serde(default = "default_true")]
    pub json_payloads: bool,
    /// Publish `set` messages for the devices as `{ "val": ... }` objects instead of plain
    /// values.
    #[serde(default)]
    pub set_json: bool,
    /// Seconds between keep-alive pings when nothing else is sent; `0` switches them off.
    #[serde(default = "default_keep_alive")]
    pub keep_alive: u16,
    /// How the adapter reconnects after the broker connection was lost.
    #[serde(default)]
    pub reconnect: Reconnect,
}

/// Reconnect delays after a lost broker connection: the delay starts at `initial`, doubles with
/// every failed attempt and never exceeds `max`. A successful connection resets it.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reconnect {
    /// The delay before the first reconnect attempt, in seconds.
    #[serde(default = "default_reconnect_initial")]
    #[schemars(range(min = 0.01))]
    pub initial: f64,
    /// The longest delay between two attempts, in seconds.
    #[serde(default = "default_reconnect_max")]
    #[schemars(range(min = 0.01))]
    pub max: f64,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self {
            initial: DEFAULT_RECONNECT_INITIAL,
            max: DEFAULT_RECONNECT_MAX,
        }
    }
}

impl Eq for Reconnect {}

fn default_name() -> String {
    String::from(DEFAULT_NAME)
}

const fn default_true() -> bool {
    true
}

const fn default_keep_alive() -> u16 {
    DEFAULT_KEEP_ALIVE
}

const fn default_reconnect_initial() -> f64 {
    DEFAULT_RECONNECT_INITIAL
}

const fn default_reconnect_max() -> f64 {
    DEFAULT_RECONNECT_MAX
}

/// A text secret as the configuration file writes it: the text itself, or where to read it.
///
/// The configuration loader resolves the `env` and `file` forms into [`SecretText::Plain`]
/// before the adapter sees the configuration; the adapter's factory refuses a configuration
/// whose password is still unresolved. The `Debug` output never shows the text.
#[derive(Clone, PartialEq, Eq)]
pub enum SecretText {
    /// The text itself.
    Plain(Secret<String>),
    /// The name of an environment variable that holds the text.
    Env(String),
    /// The path of a file that holds the text; surrounding whitespace is ignored.
    File(PathBuf),
}

impl SecretText {
    /// The resolved text, or `None` for a source that the loader has not resolved yet.
    #[must_use]
    pub fn plain(&self) -> Option<&Secret<String>> {
        match self {
            Self::Plain(secret) => Some(secret),
            Self::Env(_) | Self::File(_) => None,
        }
    }
}

impl fmt::Debug for SecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(secret) => f.debug_tuple("Plain").field(secret).finish(),
            Self::Env(name) => f.debug_tuple("Env").field(name).finish(),
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
        }
    }
}

/// The three forms of a secret in the file.
#[derive(Deserialize)]
#[serde(untagged)]
enum SecretForm {
    Plain(String),
    Env(EnvSource),
    File(FileSource),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvSource {
    env: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSource {
    file: PathBuf,
}

impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match SecretForm::deserialize(deserializer)? {
            SecretForm::Plain(text) => Self::Plain(Secret::new(text)),
            SecretForm::Env(EnvSource { env }) => Self::Env(env),
            SecretForm::File(FileSource { file }) => Self::File(file),
        })
    }
}

impl JsonSchema for SecretText {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SecretText".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A secret text: the text itself, `{ env = \"NAME\" }` to read it from \
                            an environment variable, or `{ file = \"PATH\" }` to read it from a \
                            file (surrounding whitespace is ignored).",
            "anyOf": [
                { "type": "string" },
                {
                    "type": "object",
                    "properties": {
                        "env": {
                            "type": "string",
                            "description": "Name of the environment variable that holds the secret."
                        }
                    },
                    "required": ["env"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "properties": {
                        "file": {
                            "type": "string",
                            "description": "Path of the file that holds the secret. A relative \
                                            path is relative to the directory of the \
                                            configuration file."
                        }
                    },
                    "required": ["file"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// A problem with a configuration that parsed: the key it concerns (as spelled in the file) and
/// what is wrong. The message never repeats a value from the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// The key of the `[adapters.<id>]` table, for example `url`.
    pub key: &'static str,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}`: {}", self.key, self.message)
    }
}

impl Config {
    /// The checks the schema cannot express. Empty when the configuration is usable.
    #[must_use]
    pub fn check(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();
        let issue = |key, message: String| ConfigIssue { key, message };
        match BrokerUrl::parse(&self.url) {
            Ok(url) if url.tls => issues.push(issue(
                "url",
                String::from(
                    "`mqtts://` needs TLS, which this build does not include; use `mqtt://`",
                ),
            )),
            Ok(_) => {}
            Err(error) => issues.push(issue("url", error.to_string())),
        }
        if self.tls_ca.is_some() {
            issues.push(issue(
                "tls_ca",
                String::from("TLS is not included in this build; remove the key"),
            ));
        }
        if let Err(reason) = check_topic_level(&self.name) {
            issues.push(issue("name", format!("the instance name {reason}")));
        }
        if self.client_id.as_deref().is_some_and(str::is_empty) {
            issues.push(issue("client_id", String::from("must not be empty")));
        }
        if self.username.as_deref().is_some_and(str::is_empty) {
            issues.push(issue("username", String::from("must not be empty")));
        }
        if !(self.reconnect.initial > 0.0 && self.reconnect.initial.is_finite()) {
            issues.push(issue(
                "reconnect",
                String::from("`initial` must be a positive number of seconds"),
            ));
        }
        if !(self.reconnect.max > 0.0 && self.reconnect.max.is_finite()) {
            issues.push(issue(
                "reconnect",
                String::from("`max` must be a positive number of seconds"),
            ));
        } else if self.reconnect.initial > self.reconnect.max {
            issues.push(issue(
                "reconnect",
                String::from("`initial` must not exceed `max`"),
            ));
        }
        issues
    }

    /// The client id: the configured one, or `matter-rs-<instance>`.
    #[must_use]
    pub fn client_id(&self, instance: &str) -> String {
        self.client_id
            .clone()
            .unwrap_or_else(|| format!("matter-rs-{instance}"))
    }
}

/// Checks that `text` is usable as one topic level (and so as an instance or item name).
pub(crate) fn check_topic_level(text: &str) -> Result<(), &'static str> {
    if text.is_empty() {
        return Err("must not be empty");
    }
    if text.contains(['/', '+', '#']) {
        return Err("must not contain `/`, `+` or `#`");
    }
    if text.contains('\0') {
        return Err("must not contain a NUL character");
    }
    Ok(())
}

/// A broker URL, parsed. Its [`Display`](fmt::Display) output carries no credentials, so it can
/// be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerUrl {
    /// Whether the URL asks for TLS (`mqtts://`).
    pub tls: bool,
    /// The host name or address, without brackets around an IPv6 address.
    pub host: String,
    /// The port.
    pub port: u16,
    /// The user name from the URL's `user:password@` part.
    pub username: Option<String>,
    /// The password from the URL's `user:password@` part.
    pub password: Option<Secret<String>>,
}

/// Why a broker URL is not usable; carried by [`Error::Url`]. The texts never repeat the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UrlProblem {
    /// The URL has no `mqtt://` or `mqtts://` scheme.
    Scheme,
    /// The URL has no host.
    Host,
    /// The port is not a number from 1 to 65535.
    Port,
    /// The URL carries a path or a query.
    Path,
    /// The `user:password@` part is not valid percent-encoded text.
    Credentials,
}

impl fmt::Display for UrlProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Scheme => "must start with `mqtt://` or `mqtts://`",
            Self::Host => "has no host",
            Self::Port => "has an invalid port; use a number from 1 to 65535",
            Self::Path => "must not carry a path or a query",
            Self::Credentials => "has invalid percent-encoding in its `user:password@` part",
        })
    }
}

impl BrokerUrl {
    /// Parses `mqtt://[user[:password]@]host[:port]` and the `mqtts://` form.
    ///
    /// # Errors
    ///
    /// [`Error::Url`] for anything else; it names the part, never the text.
    pub fn parse(text: &str) -> Result<Self, Error> {
        Self::parse_inner(text).map_err(Error::Url)
    }

    fn parse_inner(text: &str) -> Result<Self, UrlProblem> {
        let text = text.trim();
        let (scheme, rest) = text.split_once("://").ok_or(UrlProblem::Scheme)?;
        let tls = match scheme.to_ascii_lowercase().as_str() {
            "mqtt" | "tcp" => false,
            "mqtts" | "ssl" => true,
            _ => return Err(UrlProblem::Scheme),
        };
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        if !(tail.is_empty() || tail == "/") {
            return Err(UrlProblem::Path);
        }
        let (credentials, host_port) = match authority.rfind('@') {
            Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
            None => (None, authority),
        };
        let (username, password) = match credentials {
            None => (None, None),
            Some(credentials) => {
                let (user, password) = match credentials.split_once(':') {
                    Some((user, password)) => (user, Some(password)),
                    None => (credentials, None),
                };
                let user = percent_decode(user).ok_or(UrlProblem::Credentials)?;
                let password = password
                    .map(|password| percent_decode(password).ok_or(UrlProblem::Credentials))
                    .transpose()?;
                (
                    (!user.is_empty()).then_some(user),
                    password.map(Secret::new),
                )
            }
        };
        let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
            let (host, after) = rest.split_once(']').ok_or(UrlProblem::Host)?;
            let port = match after {
                "" => None,
                port => Some(port.strip_prefix(':').ok_or(UrlProblem::Port)?),
            };
            (host, port)
        } else {
            match host_port.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (host_port, None),
            }
        };
        if host.is_empty() {
            return Err(UrlProblem::Host);
        }
        let port = match port {
            None => {
                if tls {
                    DEFAULT_TLS_PORT
                } else {
                    DEFAULT_PORT
                }
            }
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|port| *port != 0)
                .ok_or(UrlProblem::Port)?,
        };
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
            username,
            password,
        })
    }
}

impl fmt::Display for BrokerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.tls { "mqtts" } else { "mqtt" };
        if self.host.contains(':') {
            write!(f, "{scheme}://[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{scheme}://{}:{}", self.host, self.port)
        }
    }
}

/// Decodes `%XX` escapes; `None` for a malformed escape or invalid UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = core::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::{REDACTED, Secret};

    use super::{BrokerUrl, Config, SecretText, UrlProblem, check_topic_level};
    use crate::error::Error;

    fn config(text: &str) -> Config {
        toml::from_str(text).expect("the section parses")
    }

    #[test]
    fn defaults_are_the_decided_ones() {
        let config = config("url = \"mqtt://broker.local\"\n");
        assert_eq!(config.name, "matter");
        assert_eq!(config.keep_alive, 30);
        assert!(config.json_payloads);
        assert!(!config.set_json);
        assert_eq!(config.reconnect.initial, 1.0);
        assert_eq!(config.reconnect.max, 60.0);
        assert_eq!(config.client_id("home"), "matter-rs-home");
        assert!(config.check().is_empty(), "{:?}", config.check());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error = toml::from_str::<Config>("url = \"mqtt://b\"\nqos = 1\n").unwrap_err();
        assert!(error.to_string().contains("qos"), "{error}");
    }

    #[test]
    fn password_forms_parse_and_never_print() {
        let plain = config("url = \"mqtt://b\"\npassword = \"hunter2\"\n");
        let text = format!("{plain:?}");
        assert!(text.contains(REDACTED), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
        assert_eq!(
            plain.password.as_ref().and_then(SecretText::plain),
            Some(&Secret::new(String::from("hunter2")))
        );

        let env = config("url = \"mqtt://b\"\npassword = { env = \"MQTT_PASSWORD\" }\n");
        assert_eq!(env.password, Some(SecretText::Env("MQTT_PASSWORD".into())));
        assert!(env.password.as_ref().unwrap().plain().is_none());

        let file = config("url = \"mqtt://b\"\npassword = { file = \"/run/secret\" }\n");
        assert_eq!(file.password, Some(SecretText::File("/run/secret".into())));

        let error = toml::from_str::<Config>("url = \"mqtt://b\"\npassword = { envv = \"X\" }\n");
        assert!(error.is_err());
    }

    #[test]
    fn checks_report_url_name_tls_and_reconnect() {
        let config = config(
            "url = \"ftp://b\"\nname = \"a/b\"\ntls_ca = \"ca.pem\"\nreconnect = { initial = 5, max = 1 }\n",
        );
        let keys: Vec<&str> = config.check().iter().map(|issue| issue.key).collect();
        assert_eq!(keys, ["url", "tls_ca", "name", "reconnect"]);

        let tls = super::Config {
            url: String::from("mqtts://b"),
            ..config.clone()
        };
        let messages: Vec<String> = tls.check().iter().map(ToString::to_string).collect();
        assert!(messages[0].contains("TLS"), "{messages:?}");
    }

    #[test]
    fn urls_parse_with_defaults_credentials_and_ipv6() {
        let url = BrokerUrl::parse("mqtt://broker.local").unwrap();
        assert_eq!(
            (url.host.as_str(), url.port, url.tls),
            ("broker.local", 1883, false)
        );
        assert_eq!(url.to_string(), "mqtt://broker.local:1883");

        let url = BrokerUrl::parse("mqtts://broker.local/").unwrap();
        assert_eq!((url.port, url.tls), (8883, true));

        let url = BrokerUrl::parse("mqtt://me:p%40ss@10.0.0.5:1884").unwrap();
        assert_eq!(url.username.as_deref(), Some("me"));
        assert_eq!(url.password, Some(Secret::new(String::from("p@ss"))));
        assert_eq!(url.to_string(), "mqtt://10.0.0.5:1884");
        assert!(!format!("{url:?}").contains("p@ss"));

        let url = BrokerUrl::parse("mqtt://[fd00::1]:1883").unwrap();
        assert_eq!(url.host, "fd00::1");
        assert_eq!(url.to_string(), "mqtt://[fd00::1]:1883");
        let url = BrokerUrl::parse("tcp://[fd00::1]").unwrap();
        assert_eq!(url.port, 1883);
    }

    #[test]
    fn bad_urls_name_the_part() {
        assert_eq!(
            BrokerUrl::parse("broker.local"),
            Err(Error::Url(UrlProblem::Scheme))
        );
        assert_eq!(
            BrokerUrl::parse("http://broker"),
            Err(Error::Url(UrlProblem::Scheme))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://"),
            Err(Error::Url(UrlProblem::Host))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://user@"),
            Err(Error::Url(UrlProblem::Host))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://b:0"),
            Err(Error::Url(UrlProblem::Port))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://b:99999"),
            Err(Error::Url(UrlProblem::Port))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://b:x"),
            Err(Error::Url(UrlProblem::Port))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://b/topic"),
            Err(Error::Url(UrlProblem::Path))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://b?x=1"),
            Err(Error::Url(UrlProblem::Path))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://u:p%zz@b"),
            Err(Error::Url(UrlProblem::Credentials))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://[fd00::1"),
            Err(Error::Url(UrlProblem::Host))
        );
        assert_eq!(
            BrokerUrl::parse("mqtt://[fd00::1]x"),
            Err(Error::Url(UrlProblem::Port))
        );
    }

    #[test]
    fn topic_levels_are_checked() {
        assert!(check_topic_level("matter").is_ok());
        assert!(check_topic_level("").is_err());
        assert!(check_topic_level("a/b").is_err());
        assert!(check_topic_level("a+").is_err());
        assert!(check_topic_level("#").is_err());
    }

    #[test]
    fn garbage_urls_never_panic() {
        for text in [
            "",
            "://",
            "mqtt:/",
            "mqtt://@",
            "mqtt://:@",
            "mqtt://[",
            "mqtt://]",
            "mqtt://[]",
            "mqtt://a:b:c",
            "mqtt://%",
            "mqtt://%2@h",
            "mqtt://\u{0}",
            "MQTT://H:1",
        ] {
            let _ = BrokerUrl::parse(text);
        }
    }
}
