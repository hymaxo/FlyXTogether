//! User settings persisted in `settings.toml` in the plugin folder.
//!
//! The session password is deliberately not part of this struct, so it can
//! never be written to disk.

use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Default UDP port for hosting.
pub const DEFAULT_PORT: u16 = 49700;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name shown to the other pilot.
    pub display_name: String,
    /// Last address joined, e.g. `203.0.113.7:49700`.
    pub last_address: String,
    /// Port used when hosting.
    pub port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            display_name: "Pilot".to_owned(),
            last_address: String::new(),
            port: DEFAULT_PORT,
        }
    }
}

impl Settings {
    /// Reads settings; a missing file gives the defaults. Call off the main
    /// thread.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes settings atomically (temp file, then rename). Call off the
    /// main thread.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flyx-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("settings-missing");
        let s = Settings::load(&dir.join("settings.toml")).unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.port, 49700);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn round_trips_and_contains_no_password_field() {
        let dir = temp_dir("settings-roundtrip");
        let path = dir.join("settings.toml");
        let s = Settings {
            display_name: "Alex".into(),
            last_address: "203.0.113.7:49700".into(),
            port: 50000,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("password"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = temp_dir("settings-partial");
        let path = dir.join("settings.toml");
        std::fs::write(&path, "display_name = \"Sam\"\n").unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.display_name, "Sam");
        assert_eq!(s.port, DEFAULT_PORT);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
