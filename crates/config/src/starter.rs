//! The commented starter configuration that `matter-rs init` writes.

use crate::error::Error;
use crate::passcode::{random_discriminator, random_passcode};

/// The template; `{{passcode}}`, `{{discriminator}}` and `{{bridge}}` are filled in.
const TEMPLATE: &str = include_str!("starter.toml");

/// The `[bridge]` block of a build with the bridge role.
const BRIDGE_ENABLED: &str = "[bridge]
# The bridge role: exposes the devices of the adapters as bridged Matter devices. Remove the
# section, or set `enabled = false`, to switch the role off.
enabled = true
";

/// The `[bridge]` block of a build without the bridge role.
const BRIDGE_ABSENT: &str = "# [bridge]
# The bridge role needs a build with the Cargo feature `role-bridge`.
";

/// The starter configuration with the given passcode and discriminator.
///
/// The `[bridge]` section is active only when this build has the bridge role, so that the result
/// is valid for the build that wrote it.
#[must_use]
pub fn starter_config(passcode: u32, discriminator: u16) -> String {
    let bridge = if cfg!(feature = "role-bridge") {
        BRIDGE_ENABLED
    } else {
        BRIDGE_ABSENT
    };
    TEMPLATE
        .replace("{{passcode}}", &passcode.to_string())
        .replace("{{discriminator}}", &discriminator.to_string())
        .replace("{{bridge}}", bridge)
}

/// The starter configuration with a fresh random passcode and discriminator.
///
/// # Errors
///
/// [`Error::Random`] when the operating system cannot supply random numbers.
pub fn new_starter_config() -> Result<String, Error> {
    let passcode = random_passcode().map_err(Error::Random)?;
    let discriminator = random_discriminator().map_err(Error::Random)?;
    Ok(starter_config(passcode, discriminator))
}

#[cfg(test)]
mod tests {
    use super::{TEMPLATE, starter_config};

    /// The marker line after which the commented example follows.
    const EXAMPLE_MARKER: &str = "remove the leading `# `";

    #[test]
    fn starter_config_validates() {
        let text = starter_config(13_572_468, 3_840);
        let source = crate::ConfigSource {
            name: "starter.toml",
            text: &text,
            directory: None,
            mode: None,
        };
        let environment =
            crate::environment::fake::FakeEnvironment::default().with_var("HOME", "/home/someone");
        let loaded = crate::load_source(&source, &environment)
            .unwrap_or_else(|error| panic!("the starter file does not load: {error}"));
        assert!(loaded.config.adapters.is_empty());
    }

    #[test]
    #[cfg(all(feature = "role-bridge", feature = "adapter-mqtt"))]
    fn starter_config_mqtt_example_validates() {
        let text = starter_config(13_572_468, 3_840);
        let marker = text
            .lines()
            .position(|line| line.contains(EXAMPLE_MARKER))
            .expect("the example marker exists");
        let uncommented: Vec<String> = text
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index > marker {
                    line.strip_prefix("# ")
                        .or_else(|| line.strip_prefix('#'))
                        .unwrap_or(line)
                        .to_owned()
                } else {
                    line.to_owned()
                }
            })
            .collect();
        let text = uncommented.join("\n");
        let source = crate::ConfigSource {
            name: "starter.toml",
            text: &text,
            directory: None,
            mode: None,
        };
        let environment =
            crate::environment::fake::FakeEnvironment::default().with_var("HOME", "/home/someone");
        let loaded = match crate::load_source(&source, &environment) {
            Ok(loaded) => loaded,
            Err(crate::Error::Invalid(report)) => panic!(
                "the example does not load:
{report}"
            ),
            Err(error) => panic!("{error}"),
        };
        assert_eq!(loaded.config.adapters["home"].kind(), "mqtt");
        assert_eq!(loaded.config.devices.len(), 2);
        assert_eq!(loaded.config.mqtt_devices("home").len(), 2);
        assert!(
            loaded.report.warnings().next().is_none(),
            "{}",
            loaded.report
        );
    }

    #[test]
    fn every_placeholder_is_filled() {
        assert!(TEMPLATE.contains("{{passcode}}"));
        let text = starter_config(13_572_468, 3_840);
        assert!(!text.contains("{{"), "{text}");
        assert!(text.contains("passcode = 13572468"));
        assert!(text.contains("discriminator = 3840"));
    }
}
