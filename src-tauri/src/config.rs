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

    /// Whether this machine does label printing at all.
    ///
    /// ⚠️ Off by default and separate from everything else. A bridge installed
    /// only to catch plates from the slicer should not open serial ports, and
    /// somebody with no label printer should not be asked about one.
    pub label_enabled: bool,

    /// Serial port the label printer is on, e.g. `COM6`. Chosen from an
    /// enumeration in the window rather than typed, because the name means
    /// nothing without the description beside it.
    pub label_port: String,

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

/// Liveness probe. Unauthenticated, and at the ROOT — not under `/api/v1`.
///
/// ⚠️ **Not `/api/v1/system/health`.** That name is a trap: it scans the
/// application log against a known-issue catalogue for the triage page, and
/// rightly demands `SYSTEM_READ`. Pointing the connection test there made the
/// bridge refuse to admit the server existed until the key was granted
/// `can_read_status` — a scope it never uses for anything.
const HEALTH_PATH: &str = "/health";

/// Probe that answers the only question worth asking: **may this key put a
/// file in the library?**
///
/// It is a scan of a folder id far beyond anything that can exist, on an
/// endpoint guarded by the same `LIBRARY_UPLOAD` permission an upload needs.
/// FastAPI resolves that guard *before* the handler runs, so the two outcomes
/// separate cleanly and nothing is written either way:
///
/// | Answer | Meaning |
/// |---|---|
/// | 404 | permission granted — the folder simply is not there |
/// | 403 | key is real but lacks `can_manage_library` |
/// | 401 | key is not real |
///
/// ⚠️ **`/auth/me` cannot replace this.** It validates a key while requiring no
/// permission — but for an API key it answers with a synthetic admin user
/// carrying all 111 permissions regardless of the key's actual scopes, so
/// reading its `permissions` would confidently report access the key does not
/// have. Verified against a live server before choosing this route.
///
/// ⚠️ The id must stay impossible. A real one would start an actual folder
/// scan, turning a read-only check into work.
const WRITE_PROBE_PATH: &str = "/api/v1/library/folders/2147483647/scan";

/// Checks the address, the key, and — the part that actually matters — whether
/// that key is allowed to add files to the library.
///
/// Anything less is a test that passes and then lets the first real plate fail,
/// which is precisely the failure this app already had once.
#[tauri::command]
pub async fn test_connection(settings: Settings) -> Result<String, String> {
    if settings.server_url.trim().is_empty() {
        return Err(String::from("Fill in the server address."));
    }

    // No key yet: the address is still worth checking on its own, and the
    // unauthenticated probe is the only thing that can check it.
    if settings.api_key.trim().is_empty() {
        let response = reqwest::Client::new()
            .get(settings.endpoint(HEALTH_PATH))
            .send()
            .await
            .map_err(|error| format!("Cannot reach the server: {error}"))?;

        return if response.status().is_success() {
            Ok(String::from(
                "Server is reachable — but no API key is set yet.",
            ))
        } else {
            Err(format!("Server answered {}", response.status()))
        };
    }

    let response = reqwest::Client::new()
        .post(settings.endpoint(WRITE_PROBE_PATH))
        .header("X-API-Key", &settings.api_key)
        .send()
        .await
        .map_err(|error| format!("Cannot reach the server: {error}"))?;

    let status = response.status();
    match status {
        // The guard let us through and only the folder was missing — which is
        // the whole point of asking for one that cannot exist.
        reqwest::StatusCode::NOT_FOUND => Ok(String::from(
            "Connected. This key can add files to the library.",
        )),
        reqwest::StatusCode::UNAUTHORIZED => Err(String::from(
            "The server is there, but this API key is not valid.",
        )),
        // The server names the scope it wanted, so quote it rather than
        // paraphrasing — the message is the fix.
        reqwest::StatusCode::FORBIDDEN => Err(format!(
            "The key is valid but not allowed to add files to the library. {}",
            server_detail(response).await.unwrap_or_default()
        )),
        // Success would mean the impossible id was real and we just started a
        // scan. Permission is proven, but say so plainly.
        status if status.is_success() => Ok(String::from(
            "Connected, and the key can write — though the probe hit a real folder, which it \
             should never do. Worth reporting.",
        )),
        status => Err(format!("Server answered {status}")),
    }
}

async fn server_detail(response: reqwest::Response) -> Option<String> {
    let body = response.text().await.ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    parsed
        .get("detail")?
        .as_str()
        .map(|detail| format!("Server says: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_joins_without_doubling_the_slash() {
        // `..Default::default()` rather than every field: this test is about
        // joining a URL, and it should not need editing every time the struct
        // grows a field that has nothing to do with that.
        let settings = Settings {
            server_url: String::from("http://host:8000/"),
            api_key: String::from("bb_x"),
            ..Default::default()
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
            ..Default::default()
        };
        assert!(!settings.is_complete());
    }

    #[test]
    fn label_printing_is_off_until_somebody_switches_it_on() {
        // A bridge installed only to catch plates from the slicer must not open
        // serial ports, and somebody with no label printer must not be asked
        // about one.
        let fresh = Settings::default();
        assert!(!fresh.label_enabled);
        assert!(fresh.label_port.is_empty());
    }

    #[test]
    fn the_two_roles_are_independent_of_each_other() {
        // Label printing configured, server not: still a valid state, because
        // the printer is reachable and testable without one.
        let labels_only = Settings {
            label_enabled: true,
            label_port: String::from("COM6"),
            ..Default::default()
        };
        assert!(!labels_only.is_complete(), "no server is still no server");
        assert!(labels_only.label_enabled);
    }
}
