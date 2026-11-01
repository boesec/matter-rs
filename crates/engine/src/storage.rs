//! The storage contract: where the engine and the product keep state across restarts.
//!
//! One [`Storage`] serves both sides. The engine keeps its own state (fabrics, counters,
//! resumption data) under keys that start with `engine/`; the product keeps its own (for
//! example the endpoint numbering of a bridge) under keys that start with `bridge/`. Neither side
//! touches the other's keys. A key is a non-empty string; everything else about its shape is the
//! owner's choice.
//!
//! The contract is synchronous and `Send + Sync` so that it can be driven from the engine
//! thread and from product tasks alike; a back-end that needs to wait does so inside the call.

use alloc::string::String;
use alloc::vec::Vec;

/// A failure of a [`Storage`] back-end.
///
/// Messages name keys, never values: the values may be key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// The key is empty or cannot be represented by this back-end.
    #[error("invalid storage key {key:?}")]
    InvalidKey {
        /// The offending key.
        key: String,
    },
    /// The back-end failed to read or write.
    #[error("storage back-end failed: {message}")]
    Backend {
        /// What went wrong, in the back-end's words.
        message: String,
    },
}

/// Persistent key-value storage shared by the engine and the product.
///
/// Values are opaque byte strings. A `store` replaces any earlier value under the same key; a
/// `remove` of a key that does not exist succeeds; a `load` of a key that does not exist is
/// `Ok(None)`, never an error. Implementations must write durably before `store` returns, so
/// that a crash right after a commissioning step does not lose the fabric.
pub trait Storage: Send + Sync {
    /// Loads the value stored under `key`, or `None` if there is none.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for a key this back-end cannot represent,
    /// [`StorageError::Backend`] when the back-end fails.
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError>;

    /// Stores `value` under `key`, replacing an earlier value.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for a key this back-end cannot represent,
    /// [`StorageError::Backend`] when the back-end fails.
    fn store(&self, key: &str, value: &[u8]) -> Result<(), StorageError>;

    /// Removes the value stored under `key`; succeeds when there is none.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidKey`] for a key this back-end cannot represent,
    /// [`StorageError::Backend`] when the back-end fails.
    fn remove(&self, key: &str) -> Result<(), StorageError>;
}

#[cfg(feature = "std")]
pub use self::std_backends::{DirectoryStorage, MemoryStorage};

#[cfg(feature = "std")]
mod std_backends {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;
    use std::fs;
    use std::io::{self, Write as _};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    use super::{Storage, StorageError};

    /// The longest file name a storage file may have; most file systems allow 255 bytes.
    const MAX_FILE_NAME_LENGTH: usize = 255;

    /// Storage that keeps every value in memory and forgets it when dropped.
    ///
    /// For tests and for nodes that must not persist anything.
    #[derive(Debug, Default)]
    pub struct MemoryStorage {
        values: Mutex<BTreeMap<String, Vec<u8>>>,
    }

    impl MemoryStorage {
        /// Creates an empty storage.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// The keys currently stored, in order.
        #[must_use]
        pub fn keys(&self) -> Vec<String> {
            self.values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .cloned()
                .collect()
        }
    }

    impl Storage for MemoryStorage {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
            check_key(key)?;
            Ok(self
                .values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(key)
                .cloned())
        }

        fn store(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
            check_key(key)?;
            self.values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key.to_owned(), value.to_vec());
            Ok(())
        }

        fn remove(&self, key: &str) -> Result<(), StorageError> {
            check_key(key)?;
            self.values
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(key);
            Ok(())
        }
    }

    fn check_key(key: &str) -> Result<(), StorageError> {
        if key.is_empty() {
            return Err(StorageError::InvalidKey {
                key: key.to_owned(),
            });
        }
        Ok(())
    }

    /// Storage that keeps one file per key in a directory.
    ///
    /// The directory is created on open with mode `0o700` and every file is written with mode
    /// `0o600` on Unix, because the values include fabric credentials. On other platforms the
    /// permissions are the platform's defaults for the user's files.
    ///
    /// Keys are mapped to file names by [`DirectoryStorage::file_name`]: lower-case letters,
    /// digits, `-` and `_` stay as they are, every other byte becomes `%XX`. The mapping is
    /// reversible, never produces a path separator or a `.`, and distinguishes keys that differ
    /// only in case, so no key can name a file outside the directory and no two keys share a
    /// file on a case-insensitive file system.
    ///
    /// A value is written to a temporary file and renamed into place, so a reader never sees a
    /// partially written value.
    #[derive(Debug)]
    pub struct DirectoryStorage {
        directory: PathBuf,
    }

    impl DirectoryStorage {
        /// Opens the storage in `directory`, creating it (and its parents) if needed.
        ///
        /// # Errors
        ///
        /// [`StorageError::Backend`] when the directory cannot be created or is not a directory.
        pub fn open(directory: impl Into<PathBuf>) -> Result<Self, StorageError> {
            let directory = directory.into();
            create_private_directory(&directory).map_err(|error| backend(&directory, &error))?;
            if !directory.is_dir() {
                return Err(StorageError::Backend {
                    message: format!("{} is not a directory", directory.display()),
                });
            }
            Ok(Self { directory })
        }

        /// The directory the values live in.
        #[must_use]
        pub fn directory(&self) -> &Path {
            &self.directory
        }

        /// The file name a key is stored under.
        ///
        /// # Errors
        ///
        /// [`StorageError::InvalidKey`] when the key is empty or its file name would be longer
        /// than a file system allows.
        pub fn file_name(key: &str) -> Result<String, StorageError> {
            check_key(key)?;
            let mut name = String::with_capacity(key.len());
            for byte in key.bytes() {
                if byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'_'
                {
                    name.push(char::from(byte));
                } else {
                    // Writing to a `String` cannot fail.
                    let _ = write!(name, "%{byte:02X}");
                }
            }
            if name.len() > MAX_FILE_NAME_LENGTH {
                return Err(StorageError::InvalidKey {
                    key: key.to_owned(),
                });
            }
            Ok(name)
        }

        fn path(&self, key: &str) -> Result<PathBuf, StorageError> {
            Ok(self.directory.join(Self::file_name(key)?))
        }
    }

    impl Storage for DirectoryStorage {
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
            let path = self.path(key)?;
            match fs::read(&path) {
                Ok(value) => Ok(Some(value)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(backend(&path, &error)),
            }
        }

        fn store(&self, key: &str, value: &[u8]) -> Result<(), StorageError> {
            let path = self.path(key)?;
            let mut temporary = path.clone().into_os_string();
            temporary.push(".tmp");
            let temporary = PathBuf::from(temporary);
            write_private_file(&temporary, value)
                .and_then(|()| fs::rename(&temporary, &path))
                .map_err(|error| {
                    // Best effort: a stale temporary file is harmless and is overwritten next time.
                    let _ = fs::remove_file(&temporary);
                    backend(&path, &error)
                })
        }

        fn remove(&self, key: &str) -> Result<(), StorageError> {
            let path = self.path(key)?;
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(backend(&path, &error)),
            }
        }
    }

    fn backend(path: &Path, error: &io::Error) -> StorageError {
        StorageError::Backend {
            message: format!("{}: {error}", path.display()),
        }
    }

    #[cfg(unix)]
    fn create_private_directory(directory: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
    }

    #[cfg(not(unix))]
    fn create_private_directory(directory: &Path) -> io::Result<()> {
        fs::create_dir_all(directory)
    }

    fn write_private_file(path: &Path, value: &[u8]) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(value)?;
        file.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::StorageError;

    #[test]
    fn storage_error_is_core_error() {
        fn assert_error<E: core::error::Error + Send + Sync + 'static>() {}
        assert_error::<StorageError>();
    }

    #[test]
    fn storage_error_names_the_key_not_the_value() {
        let error = StorageError::InvalidKey {
            key: "".to_string(),
        };
        assert_eq!(error.to_string(), "invalid storage key \"\"");
    }
}

#[cfg(all(test, feature = "std"))]
mod std_tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::{DirectoryStorage, MemoryStorage, Storage, StorageError};

    /// A fresh directory under the system temporary directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(test: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = format!(
                "matter-rs-engine-{test}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            // A left-over from an aborted run must not make the test see old values.
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn memory_storage_round_trip() {
        let storage = MemoryStorage::new();
        assert_eq!(storage.load("engine/a").unwrap(), None);
        storage.store("engine/a", b"1").unwrap();
        storage.store("bridge/b", b"2").unwrap();
        assert_eq!(storage.load("engine/a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(storage.keys(), vec!["bridge/b", "engine/a"]);
        storage.store("engine/a", b"3").unwrap();
        assert_eq!(storage.load("engine/a").unwrap(), Some(b"3".to_vec()));
        storage.remove("engine/a").unwrap();
        storage.remove("engine/a").unwrap();
        assert_eq!(storage.load("engine/a").unwrap(), None);
        assert!(matches!(
            storage.load(""),
            Err(StorageError::InvalidKey { .. })
        ));
    }

    #[test]
    fn directory_storage_round_trip() {
        let temp = TempDir::new("round-trip");
        let storage = DirectoryStorage::open(temp.join("state")).unwrap();
        assert_eq!(storage.directory(), temp.join("state"));

        storage.store("engine/fabrics/1", b"first").unwrap();
        assert_eq!(
            storage.load("engine/fabrics/1").unwrap(),
            Some(b"first".to_vec())
        );
        storage.store("engine/fabrics/1", b"second").unwrap();
        assert_eq!(
            storage.load("engine/fabrics/1").unwrap(),
            Some(b"second".to_vec())
        );
        storage.remove("engine/fabrics/1").unwrap();
        assert_eq!(storage.load("engine/fabrics/1").unwrap(), None);
        // Removing a missing key succeeds.
        storage.remove("engine/fabrics/1").unwrap();

        // A second instance on the same directory sees what the first wrote.
        storage.store("bridge/endpoints", b"map").unwrap();
        let reopened = DirectoryStorage::open(temp.join("state")).unwrap();
        assert_eq!(
            reopened.load("bridge/endpoints").unwrap(),
            Some(b"map".to_vec())
        );
    }

    #[test]
    fn directory_storage_missing_key() {
        let temp = TempDir::new("missing-key");
        let storage = DirectoryStorage::open(temp.join("state")).unwrap();
        assert_eq!(storage.load("engine/missing").unwrap(), None);
    }

    #[test]
    fn directory_storage_rejects_path_escape() {
        let temp = TempDir::new("path-escape");
        let storage = DirectoryStorage::open(temp.join("state")).unwrap();
        for key in [
            "../escaped",
            "..\\escaped",
            "/etc/escaped",
            "C:\\escaped",
            "..",
            ".",
            "a/../../escaped",
        ] {
            storage.store(key, b"x").unwrap();
            assert_eq!(storage.load(key).unwrap(), Some(b"x".to_vec()), "{key}");
            let name = DirectoryStorage::file_name(key).unwrap();
            assert!(!name.contains(['/', '\\', '.']), "{key} -> {name}");
            assert!(temp.join("state").join(&name).is_file(), "{key} -> {name}");
        }
        // Nothing landed next to the storage directory.
        let siblings: Vec<_> = std::fs::read_dir(&temp.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(siblings, vec![std::ffi::OsString::from("state")]);
        assert!(!temp.join("escaped").exists());
    }

    #[test]
    fn directory_storage_file_names_are_reversible_and_case_sensitive() {
        assert_eq!(
            DirectoryStorage::file_name("engine/fabrics/1").unwrap(),
            "engine%2Ffabrics%2F1"
        );
        assert_ne!(
            DirectoryStorage::file_name("engine/A").unwrap(),
            DirectoryStorage::file_name("engine/a").unwrap()
        );
        assert_eq!(DirectoryStorage::file_name("é").unwrap(), "%C3%A9");
        assert!(matches!(
            DirectoryStorage::file_name(""),
            Err(StorageError::InvalidKey { .. })
        ));
        let long = "k".repeat(256);
        assert!(matches!(
            DirectoryStorage::file_name(&long),
            Err(StorageError::InvalidKey { key }) if key == long
        ));
    }

    #[test]
    fn directory_storage_reports_a_file_in_the_way() {
        let temp = TempDir::new("file-in-the-way");
        std::fs::create_dir_all(&temp.0).unwrap();
        std::fs::write(temp.join("file"), b"").unwrap();
        let error = DirectoryStorage::open(temp.join("file")).unwrap_err();
        assert!(matches!(error, StorageError::Backend { .. }), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn directory_storage_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempDir::new("permissions");
        let storage = DirectoryStorage::open(temp.join("state")).unwrap();
        storage.store("engine/key", b"secret").unwrap();

        let directory = std::fs::metadata(temp.join("state")).unwrap();
        assert_eq!(directory.permissions().mode() & 0o777, 0o700);
        let file = std::fs::metadata(temp.join("state").join("engine%2Fkey")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
    }
}
