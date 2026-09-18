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
        let map = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
        Ok(map.get(provider).cloned())
    }
    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError> {
        let mut map = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
        map.insert(provider.to_string(), key.to_string());
        Ok(())
    }
    fn clear_key(&self, provider: &str) -> Result<(), ProviderError> {
        let mut map = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
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
        Ok(Self { path: dir.join("secrets.json"), inner: Mutex::new(()) })
    }

    fn read_map(&self) -> Result<HashMap<String, String>, ProviderError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| ProviderError::Store(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(ProviderError::Store(e.to_string())),
        }
    }

    fn write_map(&self, map: &HashMap<String, String>) -> Result<(), ProviderError> {
        let text = serde_json::to_string(map).map_err(|e| ProviderError::Store(e.to_string()))?;
        fs::write(&self.path, text).map_err(|e| ProviderError::Store(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .map_err(|e| ProviderError::Store(e.to_string()))?;
        }
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn get_key(&self, provider: &str) -> Result<Option<String>, ProviderError> {
        let _guard = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
        Ok(self.read_map()?.get(provider).cloned())
    }
    fn set_key(&self, provider: &str, key: &str) -> Result<(), ProviderError> {
        let _guard = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
        let mut map = self.read_map()?;
        map.insert(provider.to_string(), key.to_string());
        self.write_map(&map)
    }
    fn clear_key(&self, provider: &str) -> Result<(), ProviderError> {
        let _guard = self.inner.lock().map_err(|e| ProviderError::Store(e.to_string()))?;
        let mut map = self.read_map()?;
        map.remove(provider);
        self.write_map(&map)
    }
}
