use crate::devices::FoundDevice;
use crate::hardware::HardwareStatus;
use crate::profiles::{Profile, ProfileStore};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

/// Process-wide shared state handed to the web server and the engine.
///
/// Every field is behind an `Arc` so the engine threads can be handed a slice
/// of this state without taking the whole `AppState` with them.
pub struct AppState {
    /// Persisted profile collection + which one is active.
    pub store: Arc<Mutex<ProfileStore>>,
    /// The compiled profile the engine threads read on every batch.
    pub active: Arc<RwLock<Arc<Profile>>>,
    /// Snapshot of discovered devices, refreshed by the supervisor.
    pub devices: Arc<Mutex<Vec<FoundDevice>>>,
    /// Result of probing the vendor HID channel.
    pub hardware: Arc<Mutex<HardwareStatus>>,
    /// Where the store is persisted.
    pub store_path: PathBuf,
}

use std::path::PathBuf;

impl AppState {
    pub fn new(store: ProfileStore, store_path: PathBuf) -> Arc<Self> {
        let active = Arc::new(store.active_profile());
        Arc::new(AppState {
            store: Arc::new(Mutex::new(store)),
            active: Arc::new(RwLock::new(active)),
            devices: Arc::new(Mutex::new(Vec::new())),
            hardware: Arc::new(Mutex::new(HardwareStatus::probe())),
            store_path,
        })
    }

    /// Persist the store and publish the newly active profile to the engine.
    ///
    /// Order matters: write to disk first, then swap the engine's view, so a
    /// failed write never leaves RAM and disk disagreeing silently.
    pub fn commit(&self) -> anyhow::Result<()> {
        let profile = {
            let store = self.store.lock().expect("store lock poisoned");
            store.save(&self.store_path)?;
            store.active_profile()
        };
        let mut active = self.active.write().expect("active lock poisoned");
        *active = Arc::new(profile);
        Ok(())
    }

    pub fn active_snapshot(&self) -> Profile {
        self.active
            .read()
            .expect("active lock poisoned")
            .as_ref()
            .clone()
    }
}

pub fn load_store(path: &Path) -> ProfileStore {
    ProfileStore::load(path).unwrap_or_else(|e| {
        tracing::warn!("could not load {}: {e}; starting fresh", path.display());
        ProfileStore::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_publishes_the_active_profile_to_the_engine_slot() {
        let dir = std::env::temp_dir().join(format!("kenctl-state-{}", std::process::id()));
        let path = dir.join("profiles.json");
        let state = AppState::new(ProfileStore::default(), path.clone());

        let mut p = Profile::default();
        p.name = "Edited".into();
        p.scroll_speed = 2.0;
        state.store.lock().unwrap().upsert(p).unwrap();
        state.store.lock().unwrap().activate("Edited").unwrap();

        state.commit().unwrap();
        assert_eq!(state.active_snapshot().name, "Edited");
        assert_eq!(state.active_snapshot().scroll_speed, 2.0);
        assert!(path.exists(), "commit must persist before publishing");
        std::fs::remove_dir_all(&dir).ok();
    }
}
