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

    #[test]
    fn every_placeholder_is_filled() {
        assert!(TEMPLATE.contains("{{passcode}}"));
        let text = starter_config(13_572_468, 3_840);
        assert!(!text.contains("{{"), "{text}");
        assert!(text.contains("passcode = 13572468"));
        assert!(text.contains("discriminator = 3840"));
    }
}
