//! What the loader needs from the system it runs on: environment variables, secret files, the
//! platform and whether the process is privileged. Behind a trait so that tests and the fuzz
//! target can supply their own.

use std::io;
use std::path::Path;

/// The operating-system family, as far as configuration defaults differ between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Linux and other Unix-like systems.
    Unix,
    /// Windows.
    Windows,
}

impl Platform {
    /// The platform this program was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// The system around the loader.
pub trait Environment {
    /// The value of the environment variable `name`, or `None` when it is not set or is not
    /// valid Unicode.
    fn var(&self, name: &str) -> Option<String>;

    /// Reads a file that holds a secret.
    ///
    /// # Errors
    ///
    /// Any error of reading the file.
    fn read_file(&self, path: &Path) -> io::Result<String>;

    /// The platform whose defaults apply.
    fn platform(&self) -> Platform;

    /// Whether the process runs as root (Unix) or elevated (Windows). Only asked when a default
    /// that depends on it is needed.
    fn is_privileged(&self) -> bool;
}

/// The real system: the process environment, the file system and the running platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemEnvironment;

impl Environment for SystemEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn read_file(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn is_privileged(&self) -> bool {
        is_privileged()
    }
}

/// Whether the process runs as root.
///
/// The owner of `/proc/self` is the effective user of the process, which avoids an unsafe call
/// into the C library. Where `/proc` does not exist the process counts as unprivileged.
#[cfg(unix)]
fn is_privileged() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|metadata| metadata.uid() == 0)
}

/// Whether the process runs elevated.
///
/// Asking the token directly needs unsafe Win32 calls, which this workspace forbids. `whoami
/// /groups` lists the mandatory integrity level of the process token instead: high
/// (`S-1-16-12288`) for an elevated administrator, system (`S-1-16-16384`) for a service running
/// as `LocalSystem`. If `whoami` cannot be run the process counts as not elevated.
#[cfg(windows)]
fn is_privileged() -> bool {
    std::process::Command::new("whoami")
        .args(["/groups", "/fo", "csv", "/nh"])
        .output()
        .is_ok_and(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            output.status.success()
                && (text.contains("S-1-16-12288") || text.contains("S-1-16-16384"))
        })
}

#[cfg(not(any(unix, windows)))]
fn is_privileged() -> bool {
    false
}

#[cfg(test)]
pub(crate) mod fake {
    //! An environment for tests: variables and files from maps, platform and privilege fixed.

    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{Environment, Platform};

    #[derive(Debug, Clone)]
    pub(crate) struct FakeEnvironment {
        pub(crate) vars: BTreeMap<String, String>,
        pub(crate) files: BTreeMap<PathBuf, String>,
        pub(crate) platform: Platform,
        pub(crate) privileged: bool,
    }

    impl Default for FakeEnvironment {
        fn default() -> Self {
            Self {
                vars: BTreeMap::new(),
                files: BTreeMap::new(),
                platform: Platform::Unix,
                privileged: false,
            }
        }
    }

    impl FakeEnvironment {
        pub(crate) fn with_var(mut self, name: &str, value: &str) -> Self {
            self.vars.insert(name.to_owned(), value.to_owned());
            self
        }

        pub(crate) fn with_file(mut self, path: impl Into<PathBuf>, content: &str) -> Self {
            self.files.insert(path.into(), content.to_owned());
            self
        }
    }

    impl Environment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn read_file(&self, path: &Path) -> io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "file not found"))
        }

        fn platform(&self) -> Platform {
            self.platform
        }

        fn is_privileged(&self) -> bool {
            self.privileged
        }
    }
}
