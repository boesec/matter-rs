//! Where the configuration file and the state directory are when nothing names them.

use std::path::PathBuf;

use crate::environment::{Environment, Platform};

/// The environment variable that names the configuration file when `--config` is not given.
pub const CONFIG_ENV: &str = "MATTER_RS_CONFIG";

/// The directory name used under the platform's configuration and state directories.
const DIRECTORY_NAME: &str = "matter-rs";

/// Where a configuration path came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPathSource {
    /// Given on the command line.
    CommandLine,
    /// Taken from the environment variable [`CONFIG_ENV`].
    Environment,
    /// The platform default.
    PlatformDefault,
}

/// The platform default of the configuration file: `/etc/matter-rs/config.toml` on Unix,
/// `%ProgramData%\matter-rs\config.toml` on Windows. `None` when the Windows variable
/// `ProgramData` is not set.
#[must_use]
pub fn default_config_path(environment: &dyn Environment) -> Option<PathBuf> {
    match environment.platform() {
        Platform::Unix => Some(
            PathBuf::from("/etc")
                .join(DIRECTORY_NAME)
                .join("config.toml"),
        ),
        Platform::Windows => non_empty_var(environment, "ProgramData")
            .map(|base| PathBuf::from(base).join(DIRECTORY_NAME).join("config.toml")),
    }
}

/// The configuration file to use: `command_line` if given, else the environment variable
/// [`CONFIG_ENV`], else the platform default. `None` when none of the three is available.
#[must_use]
pub fn resolve_config_path(
    command_line: Option<PathBuf>,
    environment: &dyn Environment,
) -> Option<(PathBuf, ConfigPathSource)> {
    if let Some(path) = command_line {
        return Some((path, ConfigPathSource::CommandLine));
    }
    if let Some(path) = non_empty_var(environment, CONFIG_ENV) {
        return Some((PathBuf::from(path), ConfigPathSource::Environment));
    }
    default_config_path(environment).map(|path| (path, ConfigPathSource::PlatformDefault))
}

/// The state directory used when `[state] dir` is not set.
///
/// - Unix as root: `/var/lib/matter-rs`.
/// - Unix otherwise: `$XDG_STATE_HOME/matter-rs`, or `~/.local/state/matter-rs` when
///   `XDG_STATE_HOME` is unset or not absolute (as the XDG base directory specification says).
/// - Windows elevated: `%ProgramData%\matter-rs`.
/// - Windows otherwise: `%LOCALAPPDATA%\matter-rs`.
///
/// `None` when the variables a default needs are not set; then the directory must be configured.
#[must_use]
pub fn default_state_dir(environment: &dyn Environment) -> Option<PathBuf> {
    let base = match (environment.platform(), environment.is_privileged()) {
        (Platform::Unix, true) => PathBuf::from("/var/lib"),
        (Platform::Unix, false) => non_empty_var(environment, "XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() || path.starts_with("/"))
            .or_else(|| {
                non_empty_var(environment, "HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })?,
        (Platform::Windows, true) => PathBuf::from(non_empty_var(environment, "ProgramData")?),
        (Platform::Windows, false) => PathBuf::from(non_empty_var(environment, "LOCALAPPDATA")?),
    };
    Some(base.join(DIRECTORY_NAME))
}

fn non_empty_var(environment: &dyn Environment, name: &str) -> Option<String> {
    environment.var(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CONFIG_ENV, ConfigPathSource, default_config_path, default_state_dir, resolve_config_path,
    };
    use crate::environment::Platform;
    use crate::environment::fake::FakeEnvironment;

    fn unix(privileged: bool) -> FakeEnvironment {
        FakeEnvironment {
            privileged,
            ..FakeEnvironment::default()
        }
    }

    fn windows(privileged: bool) -> FakeEnvironment {
        FakeEnvironment {
            platform: Platform::Windows,
            privileged,
            ..FakeEnvironment::default()
        }
        .with_var("ProgramData", r"C:\ProgramData")
        .with_var("LOCALAPPDATA", r"C:\Users\someone\AppData\Local")
    }

    #[test]
    fn state_dir_on_unix_as_root_is_var_lib() {
        let environment = unix(true).with_var("HOME", "/root");
        assert_eq!(
            default_state_dir(&environment),
            Some(PathBuf::from("/var/lib/matter-rs"))
        );
    }

    #[test]
    fn state_dir_on_unix_as_user_prefers_xdg_state_home() {
        let environment = unix(false)
            .with_var("HOME", "/home/someone")
            .with_var("XDG_STATE_HOME", "/srv/state");
        assert_eq!(
            default_state_dir(&environment),
            Some(PathBuf::from("/srv/state/matter-rs"))
        );
    }

    #[test]
    fn state_dir_on_unix_as_user_falls_back_to_home() {
        let environment = unix(false)
            .with_var("HOME", "/home/someone")
            .with_var("XDG_STATE_HOME", "relative/ignored");
        assert_eq!(
            default_state_dir(&environment),
            Some(PathBuf::from("/home/someone/.local/state/matter-rs"))
        );
    }

    #[test]
    fn state_dir_without_any_variable_is_unknown() {
        assert_eq!(default_state_dir(&unix(false)), None);
    }

    #[test]
    fn state_dir_on_windows_depends_on_elevation() {
        assert_eq!(
            default_state_dir(&windows(true)),
            Some(PathBuf::from(r"C:\ProgramData").join("matter-rs"))
        );
        assert_eq!(
            default_state_dir(&windows(false)),
            Some(PathBuf::from(r"C:\Users\someone\AppData\Local").join("matter-rs"))
        );
    }

    #[test]
    fn config_path_defaults_per_platform() {
        assert_eq!(
            default_config_path(&unix(false)),
            Some(PathBuf::from("/etc/matter-rs/config.toml"))
        );
        assert_eq!(
            default_config_path(&windows(false)),
            Some(
                PathBuf::from(r"C:\ProgramData")
                    .join("matter-rs")
                    .join("config.toml")
            )
        );
    }

    #[test]
    fn config_path_lookup_order_is_command_line_environment_default() {
        let environment = unix(false).with_var(CONFIG_ENV, "/from/env.toml");
        assert_eq!(
            resolve_config_path(Some(PathBuf::from("/from/cli.toml")), &environment),
            Some((
                PathBuf::from("/from/cli.toml"),
                ConfigPathSource::CommandLine
            ))
        );
        assert_eq!(
            resolve_config_path(None, &environment),
            Some((
                PathBuf::from("/from/env.toml"),
                ConfigPathSource::Environment
            ))
        );
        assert_eq!(
            resolve_config_path(None, &unix(false)),
            Some((
                PathBuf::from("/etc/matter-rs/config.toml"),
                ConfigPathSource::PlatformDefault
            ))
        );
    }
}
