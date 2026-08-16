//! Where the bridge is pointed and what it authenticates with.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Base URL of the BamDude server, e.g. `http://192.168.1.10:8000`.
    /// Stored without a trailing slash; [`Settings::endpoint`] adds the path.
    pub server_url: String,

    /// API key (`bb_…`) with the library-upload scope.
    ///
    /// ⚠️ **Stored in plaintext in the config file today.** The right home on
    /// Windows is the Credential Manager, and moving it there is a change of
    /// storage only — the field stays. Recorded here rather than in a tracker
    /// because whoever reads this struct is exactly who needs to know.
    pub api_key: String,
}

impl Settings {
    /// True when there is enough here to attempt an upload at all.
    pub fn is_complete(&self) -> bool {
        !self.server_url.trim().is_empty() && !self.api_key.trim().is_empty()
    }

    /// Absolute URL of one API path, joined without doubling the slash.
    pub fn endpoint(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.server_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot locate a config directory for this user: {0}")]
    NoConfigDir(String),

    #[error("cannot read settings: {0}")]
    Read(String),

    #[error("cannot write settings: {0}")]
    Write(String),

    #[error("settings file is not valid JSON: {0}")]
    Malformed(String),
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, ConfigError> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|error| ConfigError::NoConfigDir(error.to_string()))?;
    Ok(dir.join(FILE_NAME))
}

/// Reads settings, treating "never configured" as empty rather than as an
/// error — a first run is not a failure.
pub fn load(app: &AppHandle) -> Result<Settings, ConfigError> {
    let path = settings_path(app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }

    let raw =
        std::fs::read_to_string(&path).map_err(|error| ConfigError::Read(error.to_string()))?;
    serde_json::from_str(&raw).map_err(|error| ConfigError::Malformed(error.to_string()))
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), ConfigError> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| ConfigError::Write(error.to_string()))?;
    }

    let encoded = serde_json::to_string_pretty(settings)
        .map_err(|error| ConfigError::Write(error.to_string()))?;
    std::fs::write(&path, encoded).map_err(|error| ConfigError::Write(error.to_string()))
}

// --- Commands reachable from the settings window -------------------------

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<Settings, String> {
    load(&app).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    save(&app, &settings).map_err(|error| error.to_string())
}

/// Checks that the server answers and that the key is accepted, so the user
/// finds out here rather than when a plate silently fails to arrive.
#[tauri::command]
pub async fn test_connection(settings: Settings) -> Result<String, String> {
    if !settings.is_complete() {
        return Err(String::from(
            "Fill in both the server address and an API key.",
        ));
    }

    let response = reqwest::Client::new()
        .get(settings.endpoint("/api/v1/system/health"))
        .header("X-API-Key", &settings.api_key)
        .send()
        .await
        .map_err(|error| format!("Cannot reach the server: {error}"))?;

    if response.status().is_success() {
        Ok(String::from("Connected."))
    } else {
        Err(format!("Server answered {}", response.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_joins_without_doubling_the_slash() {
        let settings = Settings {
            server_url: String::from("http://host:8000/"),
            api_key: String::from("bb_x"),
        };
        assert_eq!(
            settings.endpoint("/api/v1/library/files"),
            "http://host:8000/api/v1/library/files"
        );
    }

    #[test]
    fn a_blank_key_counts_as_unconfigured() {
        let settings = Settings {
            server_url: String::from("http://host:8000"),
            api_key: String::from("   "),
        };
        assert!(!settings.is_complete());
    }
}
