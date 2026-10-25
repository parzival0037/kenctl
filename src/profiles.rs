use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One entry in a button remap table. Keys are source key names, e.g.
/// `"BTN_LEFT"` or `"KEY_PLAYPAUSE"`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Forward unchanged.
    Default,
    /// Swallow the event entirely.
    None,
    /// Re-emit as a different button.
    Button { code: String },
    /// Re-emit as a keyboard key, optionally holding modifiers first.
    Key {
        code: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    /// Run a macro on press.
    Macro { id: String },
}

impl Default for Action {
    fn default() -> Self {
        Action::Default
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MacroAction {
    /// Press and release immediately (the original macro behaviour).
    #[default]
    Tap,
    /// Hold the key/button until a later `up` step.
    Down,
    /// Release a key/button held by an earlier `down` step.
    Up,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MacroStep {
    pub code: String,
    #[serde(default)]
    pub delay_ms: u64,
    /// Defaults to `tap`, so macros written before this field still work.
    #[serde(default)]
    pub action: MacroAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Macro {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub steps: Vec<MacroStep>,
}

fn one() -> f64 {
    1.0
}

fn default_name() -> String {
    "Default".to_string()
}

/// A named set of settings applied to the device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(default = "default_name")]
    pub name: String,

    /// Multiplier applied to pointer deltas. 1.0 = device default.
    #[serde(default = "one")]
    pub pointer_speed: f64,

    /// Multiplier applied to scroll deltas. 1.0 = device default.
    #[serde(default = "one")]
    pub scroll_speed: f64,

    #[serde(default)]
    pub invert_pointer_x: bool,
    #[serde(default)]
    pub invert_pointer_y: bool,
    #[serde(default)]
    pub invert_scroll: bool,

    /// Source key name -> action.
    #[serde(default)]
    pub buttons: BTreeMap<String, Action>,

    #[serde(default)]
    pub macros: BTreeMap<String, Macro>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "Default".into(),
            pointer_speed: 1.0,
            scroll_speed: 1.0,
            invert_pointer_x: false,
            invert_pointer_y: false,
            invert_scroll: false,
            buttons: BTreeMap::new(),
            macros: BTreeMap::new(),
        }
    }
}

impl Profile {
    /// Sanitise values coming from the API so a bad payload cannot wedge the
    /// engine (NaN/inf speeds would poison the fractional accumulators).
    pub fn sanitize(&mut self) {
        let clamp = |v: &mut f64| {
            if !v.is_finite() {
                *v = 1.0;
            }
            *v = v.clamp(0.05, 20.0);
        };
        clamp(&mut self.pointer_speed);
        clamp(&mut self.scroll_speed);
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            self.name = "Unnamed".into();
        }
        self.buttons.retain(|k, a| {
            let _ = k;
            !matches!(a, Action::Key { code, .. } if code.trim().is_empty())
        });
    }

    pub fn action_for(&self, key: &str) -> Action {
        self.buttons.get(key).cloned().unwrap_or(Action::Default)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileStore {
    pub profiles: Vec<Profile>,
    pub active: String,
}

impl Default for ProfileStore {
    fn default() -> Self {
        ProfileStore {
            profiles: vec![Profile::default()],
            active: "Default".into(),
        }
    }
}

impl ProfileStore {
    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.name == name)
    }

    pub fn active_profile(&self) -> Profile {
        self.get(&self.active)
            .cloned()
            .unwrap_or_else(Profile::default)
    }

    pub fn upsert(&mut self, mut profile: Profile) -> Result<String> {
        profile.sanitize();
        let original = profile.name.clone();

        // Update in place when the name already exists, otherwise append.
        if let Some(slot) = self.profiles.iter_mut().find(|p| p.name == original) {
            *slot = profile;
        } else {
            self.profiles.push(profile);
        }
        Ok(original)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        let removed = self.profiles.len() != before;
        if removed && self.active == name {
            self.active = self
                .profiles
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "Default".to_string());
            if self.profiles.is_empty() {
                self.profiles.push(Profile::default());
            }
        }
        removed
    }

    pub fn activate(&mut self, name: &str) -> Result<Profile> {
        let profile = self
            .get(name)
            .cloned()
            .with_context(|| format!("no such profile: {name}"))?;
        self.active = name.to_string();
        Ok(profile)
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(ProfileStore::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut store: ProfileStore =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        if store.profiles.is_empty() {
            store.profiles.push(Profile::default());
        }
        if store.get(&store.active).is_none() {
            store.active = store.profiles[0].name.clone();
        }
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Write via temp file + rename so a crash cannot truncate the store.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming {}", tmp.display()))?;
        Ok(())
    }
}

/// `$XDG_CONFIG_HOME/kenctl`, falling back to `~/.config/kenctl`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            home.join(".config")
        });
    base.join("kenctl")
}

pub fn store_path() -> PathBuf {
    config_dir().join("profiles.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_store() {
        let dir = std::env::temp_dir().join(format!("kenctl-test-{}", std::process::id()));
        let path = dir.join("profiles.json");
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        p.name = "Gaming".into();
        p.pointer_speed = 1.5;
        p.buttons
            .insert("BTN_SIDE".into(), Action::Key { code: "KEY_C".into(), modifiers: vec!["KEY_LEFTCTRL".into()] });
        store.upsert(p).unwrap();
        store.activate("Gaming").unwrap();
        store.save(&path).unwrap();

        let loaded = ProfileStore::load(&path).unwrap();
        assert_eq!(loaded.active, "Gaming");
        assert_eq!(loaded.get("Gaming").unwrap().pointer_speed, 1.5);
        assert!(matches!(
            loaded.get("Gaming").unwrap().action_for("BTN_SIDE"),
            Action::Key { .. }
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitize_rejects_nonfinite_speed() {
        let mut p = Profile::default();
        p.pointer_speed = f64::NAN;
        p.scroll_speed = f64::INFINITY;
        p.sanitize();
        assert!(p.pointer_speed.is_finite());
        assert!(p.scroll_speed.is_finite());
    }

    #[test]
    fn legacy_macro_steps_default_to_tap() {
        let step: MacroStep = serde_json::from_str(r#"{"code":"KEY_V","delay_ms":25}"#).unwrap();
        assert_eq!(step.action, MacroAction::Tap);
    }

    #[test]
    fn recorded_macro_round_trips_actions() {
        let step = MacroStep {
            code: "KEY_LEFTALT".into(),
            delay_ms: 40,
            action: MacroAction::Down,
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"action\":\"down\""));
        let back: MacroStep = serde_json::from_str(&json).unwrap();
        assert_eq!(back, step);
    }

    #[test]
    fn removing_active_profile_falls_back() {
        let mut store = ProfileStore::default();
        let mut p = Profile::default();
        p.name = "Second".into();
        store.upsert(p).unwrap();
        store.activate("Second").unwrap();
        assert!(store.remove("Second"));
        assert_eq!(store.active, "Default");
        assert!(store.get("Default").is_some());
    }
}
