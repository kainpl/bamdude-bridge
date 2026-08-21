//! Checking for a new version, and applying it whichever way this copy needs.
//!
//! Two paths behind one pair of commands, because the person pressing the
//! button does not care which they are on:
//!
//! - **installed** — the official `tauri-plugin-updater`. It downloads the NSIS
//!   installer, runs it and exits us; `installMode: passive` shows a progress
//!   bar and asks nothing.
//! - **portable** — [`portable`], ours, because the plugin would *install* a
//!   portable copy and silently leave the folder behind.
//!
//! ⚠️ **Both read the same `latest.json` and trust the same key.** The portable
//! path derives its own asset name from the version rather than adding a second
//! manifest to maintain — a release cannot then be half-published.

pub mod portable;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Where the portable archive and its signature live for a given version.
///
/// ⚠️ Built here, not fetched. `latest.json` names the installer, and letting a
/// downloaded document choose which file we execute would undo the signature
/// check it is meant to enable.
fn portable_urls(version: &str) -> Result<(String, String), String> {
    let asset = portable::asset_name(version)?;
    let base = format!("https://github.com/kainpl/bamdude-bridge/releases/download/v{version}");
    Ok((format!("{base}/{asset}"), format!("{base}/{asset}.sig")))
}

/// What the window shows when it asks whether there is anything new.
#[derive(Debug, Serialize)]
pub struct UpdateCheck {
    pub available: bool,
    pub current_version: String,
    pub version: Option<String>,
    /// The release body, as written on GitHub. `tauri-action` copies it into
    /// the manifest, which is why the notes are the real ones rather than a
    /// second changelog somebody has to remember to write.
    pub notes: Option<String>,
    pub date: Option<String>,
    /// True when this copy will replace its own executable rather than run an
    /// installer. Worth saying out loud: the two feel different.
    pub portable: bool,
}

#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> Result<UpdateCheck, String> {
    let current = app.package_info().version.to_string();
    let updater = app
        .updater()
        .map_err(|error| format!("cannot reach the update service: {error}"))?;

    match updater.check().await {
        Ok(Some(update)) => Ok(UpdateCheck {
            available: true,
            current_version: current,
            version: Some(update.version.clone()),
            notes: update.body.clone(),
            date: update.date.map(|date| date.to_string()),
            portable: portable::is_portable(),
        }),
        Ok(None) => Ok(UpdateCheck {
            available: false,
            current_version: current,
            version: None,
            notes: None,
            date: None,
            portable: portable::is_portable(),
        }),
        // ⚠️ Said plainly rather than swallowed. "No update" and "could not ask"
        // look identical on screen otherwise, and the second one is the state
        // somebody needs to know about — it is usually a proxy or no network.
        Err(error) => Err(format!("could not check for updates: {error}")),
    }
}

/// Download and apply. The app exits either way; the new one starts itself.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|error| format!("cannot reach the update service: {error}"))?;
    let update = updater
        .check()
        .await
        .map_err(|error| format!("could not check for updates: {error}"))?
        .ok_or_else(|| "there is no newer version to install".to_string())?;

    if !portable::is_portable() {
        // The plugin's own path: it downloads, runs the installer and exits us.
        update
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|error| format!("the update could not be installed: {error}"))?;
        return Ok(());
    }

    let current = app.package_info().version.to_string();
    let version = portable::ensure_newer(&update.version, &current)?;
    let (archive_url, signature_url) = portable_urls(&version.to_string())?;

    let client = reqwest::Client::new();
    let signature = client
        .get(&signature_url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("cannot fetch the update signature: {error}"))?
        .text()
        .await
        .map_err(|error| format!("cannot read the update signature: {error}"))?;

    let archive = client
        .get(&archive_url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("cannot download the update: {error}"))?
        .bytes()
        .await
        .map_err(|error| format!("cannot read the downloaded update: {error}"))?;

    // ⚠️ Signature first, contents second. Nothing is unpacked, hashed or
    // written until we know we made the archive.
    portable::verify_signature(&archive, &signature)?;
    portable::apply(&archive, &version)?;

    // The helper is waiting for this process to go before it can replace the
    // file — so leaving is part of applying, not something after it.
    app.exit(0);
    Ok(())
}
