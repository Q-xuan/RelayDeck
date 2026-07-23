use std::{fs, path::{Path, PathBuf}, sync::Arc};

use tokio::sync::{Mutex, RwLock};

use crate::{models::AppConfig, RelayError};

const KEYRING_SERVICE: &str = "com.relaydeck.desktop";

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    write_guard: Arc<Mutex<()>>,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self { path, write_guard: Arc::new(Mutex::new(())) }
    }

    pub fn load(path: &Path) -> Result<AppConfig, RelayError> {
        if !path.exists() {
            return Ok(AppConfig::default());
        }
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn save(&self, config: &RwLock<AppConfig>) -> Result<(), RelayError> {
        let _guard = self.write_guard.lock().await;
        let snapshot = config.read().await.clone();
        let bytes = serde_json::to_vec_pretty(&snapshot)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("json.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

pub fn save_secret(id: &str, secret: &str) -> Result<(), RelayError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, id)?;
    entry.set_password(secret)?;
    Ok(())
}

pub fn load_secret(id: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, id).ok()?.get_password().ok()
}

pub fn delete_secret(id: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, id) {
        let _ = entry.delete_credential();
    }
}
