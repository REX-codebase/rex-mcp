use crate::error::ProviderError;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

/// Credential storage. Keys are write-only from the caller's perspective:
/// they can be stored, read back for an API call inside the backend, and
/// cleared. Nothing upstream ever serializes them.
pub trait SecretStore: Send + Sync {
    fn get_key(&self, provider: &str) -> Result<Option<String>, ProviderError>;
    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError>;
    fn clear_key(&self, provider: &str) -> Result<(), ProviderError>;
    fn has_key(&self, provider: &str) -> bool {
        matches!(self.get_key(provider), Ok(Some(_)))
    }
}

/// In-memory store. Used by tests and by the dev sidecar when no config
/// directory is available. Keys vanish when the process exits.
#[derive(Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn get_key(&self, provider: &str) -> Result<Option<String>, ProviderError> {
        let map = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        Ok(map.get(provider).cloned())
    }
    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError> {
        let mut map = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        map.insert(provider.to_string(), key.to_string());
        Ok(())
    }
    fn clear_key(&self, provider: &str) -> Result<(), ProviderError> {
        let mut map = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        map.remove(provider);
        Ok(())
    }
}

/// File-backed store: one JSON map at `<dir>/secrets.json`, permissions
/// 0600, directory 0700. Keys never enter logs, and the file lives outside
/// the frontend bundle and the repo. This is a v1 store; an OS-keychain
/// backend can replace it behind the same trait.
pub struct FileSecretStore {
    path: PathBuf,
    inner: Mutex<()>,
}

impl FileSecretStore {
    pub fn new(dir: PathBuf) -> Result<Self, ProviderError> {
        fs::create_dir_all(&dir).map_err(|e| ProviderError::Store(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| ProviderError::Store(e.to_string()))?;
        }
        Ok(Self {
            path: dir.join("secrets.json"),
            inner: Mutex::new(()),
        })
    }

    fn read_map(&self) -> Result<HashMap<String, String>, ProviderError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| ProviderError::Store(e.to_string()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(ProviderError::Store(e.to_string())),
        }
    }

    fn write_map(&self, map: &HashMap<String, String>) -> Result<(), ProviderError> {
        let text = serde_json::to_string(map).map_err(|e| ProviderError::Store(e.to_string()))?;
        // A temp file that is 0600 from creation, synced, then renamed over
        // the store: a crash mid-write keeps the old keys instead of leaving
        // a broken file, and the keys are never in a file others could
        // read. Hermes saves credentials the same way (`atomic_json_write`
        // with `mode=0o600`, `utils.py`).
        let tmp = self
            .path
            .with_file_name(format!(".secrets.{}.tmp", std::process::id()));
        let written = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut f = options.open(&tmp)?;
            #[cfg(unix)]
            {
                // a leftover temp file keeps its old mode; reset it
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            std::io::Write::write_all(&mut f, text.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &self.path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written.map_err(|e| ProviderError::Store(e.to_string()))?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn get_key(&self, provider: &str) -> Result<Option<String>, ProviderError> {
        let _guard = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        Ok(self.read_map()?.get(provider).cloned())
    }
    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError> {
        let _guard = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        let mut map = self.read_map()?;
        map.insert(provider.to_string(), key.to_string());
        self.write_map(&map)
    }
    fn clear_key(&self, provider: &str) -> Result<(), ProviderError> {
        let _guard = self
            .inner
            .lock()
            .map_err(|e| ProviderError::Store(e.to_string()))?;
        let mut map = self.read_map()?;
        map.remove(provider);
        self.write_map(&map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn keys_round_trip_and_the_store_stays_private() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(tmp.path().join("s")).unwrap();
        let path = tmp.path().join("s").join("secrets.json");
        // an old store left readable, and a leftover temp file from a crash
        fs::write(&path, "{\"old\":\"k0\"}").unwrap();
        let leftover = path.with_file_name(format!(".secrets.{}.tmp", std::process::id()));
        fs::write(&leftover, "stale").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(&leftover, fs::Permissions::from_mode(0o644)).unwrap();
        }
        store.set_key("a", "k1").unwrap();
        // the first save reused the readable leftover; it is private now
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        store.set_key("b", "k2").unwrap();
        store.clear_key("old").unwrap();
        assert_eq!(store.get_key("a").unwrap().as_deref(), Some("k1"));
        assert_eq!(store.get_key("b").unwrap().as_deref(), Some("k2"));
        assert_eq!(store.get_key("old").unwrap(), None);
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        let names: Vec<String> = fs::read_dir(tmp.path().join("s"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["secrets.json"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_rename_removes_the_temp_file() {
        // the write is stopped at the rename by a folder in the way
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let store = FileSecretStore::new(dir.clone()).unwrap();
        let leftover = dir.join(format!(".secrets.{}.tmp", std::process::id()));
        fs::write(&leftover, "stale").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&leftover, fs::Permissions::from_mode(0o644)).unwrap();
        }
        // secrets.json as a non-empty folder makes the rename fail
        fs::create_dir(dir.join("secrets.json")).unwrap();
        fs::write(dir.join("secrets.json").join("x"), "").unwrap();
        let fresh = FileSecretStore {
            path: dir.join("secrets.json"),
            inner: Mutex::new(()),
        };
        let mut map = HashMap::new();
        map.insert("a".to_string(), "k1".to_string());
        assert!(fresh.write_map(&map).is_err());
        // the failed write removed its temp file
        assert!(!leftover.exists());
        drop(store);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_keeps_the_old_keys() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let store = FileSecretStore::new(dir.clone()).unwrap();
        store.set_key("a", "k1").unwrap();
        // no new files can be made in the folder
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let failed = store.set_key("b", "k2").is_err();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(failed);
        assert_eq!(store.get_key("a").unwrap().as_deref(), Some("k1"));
        assert_eq!(store.get_key("b").unwrap(), None);
    }
}
