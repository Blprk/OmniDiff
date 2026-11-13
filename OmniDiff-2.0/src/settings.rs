//! Persistent user settings, stored as JSON in `~/Library/Application
//! Support/OmniDiff/settings.json`. Loaded at startup, saved on every change.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which color scheme the UI uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the macOS system appearance.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Auto, Theme::Dark, Theme::Light];
    pub const LABELS: [&'static str; 3] = ["Follow system", "Dark", "Light"];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Theme {
        Theme::ALL.get(i).copied().unwrap_or_default()
    }

    /// Resolve against the system's current appearance.
    pub fn resolve(self, system_dark: bool) -> bool {
        match self {
            Theme::Auto => system_dark,
            Theme::Dark => true,
            Theme::Light => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    /// Skip Blake3 hashing: compare by size + modification date only.
    pub fast_compare: bool,
    /// Give copied files the source's modification date (photo libraries
    /// keep sorting by "date taken" after a sync).
    pub preserve_mtime: bool,
    /// Open the Insights panel automatically after each comparison.
    pub auto_insights: bool,
    /// Pre-fill the last used source/destination on launch.
    pub remember_folders: bool,
    pub last_source: String,
    pub last_dest: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::Auto,
            fast_compare: false,
            preserve_mtime: true,
            auto_insights: false,
            remember_folders: true,
            last_source: String::new(),
            last_dest: String::new(),
        }
    }
}

/// Where settings live on this machine.
pub fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library/Application Support/OmniDiff/settings.json")
}

pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, s: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_every_field() {
        let path = std::env::temp_dir().join(format!("omnidiff2_settings_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut s = Settings::default();
        s.theme = Theme::Light;
        s.fast_compare = true;
        s.preserve_mtime = false;
        s.auto_insights = true;
        s.remember_folders = false;
        s.last_source = "/tmp/a".into();
        s.last_dest = "/tmp/b".into();
        save(&path, &s).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded, s);
        assert_eq!(loaded.theme, Theme::Light);
        assert!(loaded.fast_compare);
        assert!(!loaded.preserve_mtime);

        // A file missing fields (older version) still loads via #[serde(default)].
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let partial = load(&path);
        assert_eq!(partial.theme, Theme::Dark);
        assert_eq!(partial.preserve_mtime, Settings::default().preserve_mtime);

        // A corrupt file falls back to defaults instead of crashing.
        std::fs::write(&path, "not json at all {").unwrap();
        assert_eq!(load(&path), Settings::default());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn theme_resolution() {
        assert_eq!(Theme::Auto.resolve(true), true);
        assert_eq!(Theme::Auto.resolve(false), false);
        assert_eq!(Theme::Dark.resolve(false), true);
        assert_eq!(Theme::Light.resolve(true), false);
        assert_eq!(Theme::from_index(2), Theme::Light);
        assert_eq!(Theme::from_index(99), Theme::Auto);
    }
}
