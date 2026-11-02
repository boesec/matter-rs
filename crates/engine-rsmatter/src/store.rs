//! rs-matter's key-value blob store over the facade's [`Storage`].
//!
//! rs-matter persists fabrics, basic-information settings, counters and the rest of its state
//! as blobs under `u16` keys. Each one lands in the facade storage under
//! `engine/rsmatter/<key>` (decimal), next to the product's own keys, so that one state
//! directory holds everything with the permissions the directory storage sets.
//!
//! The store keeps an index of the keys it has written under `engine/rsmatter/index`, because
//! the facade storage cannot list keys; a factory reset removes every indexed key and the index.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use matter_rs_engine::{Storage, StorageError};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::persist::KvBlobStore;

/// The prefix of every key rs-matter stores through the facade.
pub(crate) const KEY_PREFIX: &str = "engine/rsmatter/";

/// The key of the index of stored keys.
const INDEX_KEY: &str = "engine/rsmatter/index";

/// The facade storage key of an rs-matter blob key.
pub(crate) fn key_name(key: u16) -> String {
    format!("{KEY_PREFIX}{key}")
}

/// The index of rs-matter keys present in the storage, shared between the store and the
/// factory reset.
pub(crate) struct KeyIndex {
    storage: Arc<dyn Storage>,
    keys: Mutex<BTreeSet<u16>>,
}

impl KeyIndex {
    /// Loads the index from `storage`.
    ///
    /// # Errors
    ///
    /// [`StorageError`] when the storage cannot be read.
    pub(crate) fn load(storage: Arc<dyn Storage>) -> Result<Self, StorageError> {
        let keys = match storage.load(INDEX_KEY)? {
            Some(bytes) => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect(),
            None => BTreeSet::new(),
        };
        Ok(Self {
            storage,
            keys: Mutex::new(keys),
        })
    }

    /// The keys currently indexed.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<u16> {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    fn save(&self, keys: &BTreeSet<u16>) -> Result<(), StorageError> {
        let bytes: Vec<u8> = keys.iter().flat_map(|key| key.to_le_bytes()).collect();
        self.storage.store(INDEX_KEY, &bytes)
    }

    fn record_store(&self, key: u16) -> Result<(), StorageError> {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        if keys.insert(key) {
            self.save(&keys)?;
        }
        Ok(())
    }

    fn record_remove(&self, key: u16) -> Result<(), StorageError> {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        if keys.remove(&key) {
            self.save(&keys)?;
        }
        Ok(())
    }

    /// Removes every indexed key and the index itself. Everything is attempted; the first
    /// failure is returned.
    ///
    /// # Errors
    ///
    /// [`StorageError`] when a key could not be removed.
    pub(crate) fn remove_all(&self) -> Result<(), StorageError> {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        let mut result = Ok(());
        for key in std::mem::take(&mut *keys) {
            let removed = self.storage.remove(&key_name(key));
            if removed.is_err() {
                keys.insert(key);
            }
            result = result.and(removed);
        }
        let index = if keys.is_empty() {
            self.storage.remove(INDEX_KEY)
        } else {
            self.save(&keys)
        };
        result.and(index)
    }
}

/// The rs-matter blob store over the facade storage.
pub(crate) struct FacadeKvStore {
    storage: Arc<dyn Storage>,
    index: Arc<KeyIndex>,
}

impl FacadeKvStore {
    /// A store over `storage` with the given key index.
    pub(crate) fn new(storage: Arc<dyn Storage>, index: Arc<KeyIndex>) -> Self {
        Self { storage, index }
    }
}

fn storage_failure(operation: &str, key: u16, error: &StorageError) -> Error {
    // The message names the key, never the value, as the storage contract promises.
    log::error!("storage {operation} of rs-matter key {key} failed: {error}");
    ErrorCode::StdIoError.into()
}

impl KvBlobStore for FacadeKvStore {
    fn load<'a>(&mut self, key: u16, buf: &'a mut [u8]) -> Result<Option<&'a [u8]>, Error> {
        match self.storage.load(&key_name(key)) {
            Ok(None) => Ok(None),
            Ok(Some(value)) => {
                let len = value.len();
                if len > buf.len() {
                    log::error!(
                        "rs-matter key {key} holds {len} bytes, more than the {} byte scratch buffer",
                        buf.len()
                    );
                    return Err(ErrorCode::BufferTooSmall.into());
                }
                buf[..len].copy_from_slice(&value);
                Ok(Some(&buf[..len]))
            }
            Err(error) => Err(storage_failure("load", key, &error)),
        }
    }

    fn store(&mut self, key: u16, data: &[u8], _buf: &mut [u8]) -> Result<(), Error> {
        self.storage
            .store(&key_name(key), data)
            .and_then(|()| self.index.record_store(key))
            .map_err(|error| storage_failure("store", key, &error))
    }

    fn remove(&mut self, key: u16, _buf: &mut [u8]) -> Result<(), Error> {
        self.storage
            .remove(&key_name(key))
            .and_then(|()| self.index.record_remove(key))
            .map_err(|error| storage_failure("remove", key, &error))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use matter_rs_engine::{MemoryStorage, Storage};
    use rs_matter::persist::KvBlobStore;

    use super::{FacadeKvStore, KeyIndex, key_name};

    #[test]
    fn keys_are_prefixed_and_indexed() {
        let storage = Arc::new(MemoryStorage::new());
        let index = Arc::new(KeyIndex::load(storage.clone()).unwrap());
        let mut store = FacadeKvStore::new(storage.clone(), index.clone());
        let mut scratch = [0_u8; 64];

        assert_eq!(store.load(1, &mut scratch).unwrap(), None);
        store.store(1, b"fabric", &mut scratch).unwrap();
        store.store(0x1000, b"vendor", &mut scratch).unwrap();
        assert_eq!(store.load(1, &mut scratch).unwrap(), Some(&b"fabric"[..]));
        assert_eq!(key_name(1), "engine/rsmatter/1");
        assert_eq!(
            storage.keys(),
            vec![
                "engine/rsmatter/1",
                "engine/rsmatter/4096",
                "engine/rsmatter/index"
            ]
        );
        assert_eq!(index.keys(), vec![1, 0x1000]);

        store.remove(1, &mut scratch).unwrap();
        store.remove(1, &mut scratch).unwrap();
        assert_eq!(index.keys(), vec![0x1000]);

        // A second index on the same storage sees the keys.
        let reloaded = KeyIndex::load(storage.clone()).unwrap();
        assert_eq!(reloaded.keys(), vec![0x1000]);

        reloaded.remove_all().unwrap();
        assert!(storage.keys().is_empty());
    }

    #[test]
    fn a_value_larger_than_the_scratch_buffer_is_an_error() {
        let storage = Arc::new(MemoryStorage::new());
        storage.store("engine/rsmatter/7", &[0_u8; 10]).unwrap();
        let index = Arc::new(KeyIndex::load(storage.clone()).unwrap());
        let mut store = FacadeKvStore::new(storage, index);
        let mut scratch = [0_u8; 4];
        assert!(store.load(7, &mut scratch).is_err());
    }
}
