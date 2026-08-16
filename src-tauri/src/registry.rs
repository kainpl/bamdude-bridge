//! Making BambuStudio offer the button, and making Windows route the result
//! to us.
//!
//! Two separate registrations, with two different costs:
//!
//! | What | Where | Elevation |
//! |---|---|---|
//! | Marker BambuStudio probes | `HKLM\SOFTWARE\Bambulab\Bambu Farm Manager Client` | **yes** |
//! | Handler Windows routes to | `HKCU\Software\Classes\<scheme>` | no |
//!
//! ⚠️ **The marker cannot live under HKCU.** BambuStudio opens it against
//! `HKEY_LOCAL_MACHINE` explicitly. It does make a second attempt that looks
//! like it covers `HKEY_CLASSES_ROOT`, but that call passes the literal string
//! `"HKEY_CLASSES_ROOT\…"` as a subkey path *under HKLM*, so it resolves to a
//! key that cannot exist and always fails. One key decides, and it needs
//! admin once.
//!
//! ⚠️ **`HKCU\Software\Classes` outranks the machine-wide registration** in
//! the merged `HKCR` view. That is what makes taking over Bambu's own client
//! possible — and why [`status`] reports who holds the scheme before anything
//! is written. Displacing another program's handler is a decision for the
//! person at the keyboard, never a side effect.

use serde::Serialize;
use std::path::Path;
use std::process::Command;
use tauri::Manager;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::RegKey;

use crate::farm_client_url::SCHEME;

/// Subkey BambuStudio probes to decide whether the menu entry exists at all.
const MARKER_KEY: &str = r"SOFTWARE\Bambulab\Bambu Farm Manager Client";

/// Root under which a user-scoped class registration lives.
const CLASSES: &str = r"Software\Classes";

/// Per-user autostart. HKCU rather than HKLM on purpose: this is one person's
/// choice about their own sign-in, and it needs no elevation.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "BamDude Bridge";

/// Where an unpackaged app tells Windows what its notifications are called.
const TOAST_IDENTITY_KEY: &str = r"Software\Classes\AppUserModelId";

/// Icon Windows draws on the toast. Embedded rather than read from the install
/// directory, because the portable build is one loose executable and the
/// installed one keeps its icons inside the binary — neither leaves a PNG
/// lying about for the notification platform to point at.
const TOAST_ICON: &[u8] = include_bytes!("../icons/128x128.png");
const TOAST_ICON_FILE: &str = "toast-icon.png";

/// Argument that puts a fresh, elevated instance into "write the marker and
/// exit" mode. See [`request_elevated_marker`].
pub const INSTALL_MARKER_ARG: &str = "--install-marker";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "owner", rename_all = "kebab-case")]
pub enum Owner {
    /// Nothing handles the scheme; registering displaces nobody.
    Nobody,
    /// We do.
    Us,
    /// ⚠️ Something else does — most likely Bambu Lab's own farm client.
    /// Registering over this takes its files away from it.
    Foreign { command: String, machine_wide: bool },
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Whether BambuStudio will show "Send to Bambu Farm Manager Client".
    pub marker_present: bool,
    /// Who currently receives the URL.
    pub protocol: Owner,
    /// Whether Windows starts us at sign-in, pointing at THIS executable.
    pub autostart: bool,

    /// ⚠️ Whether this process is running elevated — which silently breaks the
    /// one thing the app is for. See [`running_elevated`].
    pub elevated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("cannot determine this executable's own path: {0}")]
    NoExePath(String),

    #[error("access denied — writing this key needs an elevated process")]
    AccessDenied,

    #[error("registry error: {0}")]
    Io(String),

    #[error("could not start the elevated helper: {0}")]
    Elevation(String),
}

fn wrap(error: std::io::Error) -> RegistryError {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => RegistryError::AccessDenied,
        _ => RegistryError::Io(error.to_string()),
    }
}

fn own_exe() -> Result<std::path::PathBuf, RegistryError> {
    std::env::current_exe().map_err(|error| RegistryError::NoExePath(error.to_string()))
}

// --- Inspection ----------------------------------------------------------

/// Reports every registration without changing anything.
pub fn status() -> Result<Status, RegistryError> {
    let exe = own_exe()?;
    Ok(Status {
        marker_present: marker_present(),
        protocol: protocol_owner(&exe),
        autostart: autostart_points_at(&exe),
        elevated: running_elevated(),
    })
}

/// True when this process holds administrator rights.
///
/// ⚠️ **Running elevated silently breaks every handover**, which is why this is
/// worth detecting at all. BambuStudio runs unelevated; when it launches our
/// URL, Windows starts an unelevated process that must hand the arguments to
/// the instance already in the tray. If that instance is elevated, UIPI blocks
/// the message across the integrity boundary — the second process exits, the
/// first never hears, and **nothing is logged anywhere** because no code of
/// ours ever runs. Observed exactly once, and it cost an hour.
///
/// "Run as administrator" is the obvious thing to try when a feature mentions
/// admin rights, so this is a mistake users will make. Registration does not
/// need it: it elevates a separate short-lived process for the one key.
///
/// Detected by asking for write access to a hive only administrators can
/// write. Opening a key changes nothing, and the answer is exactly the
/// privilege boundary that matters here — no extra dependency needed to learn
/// it a more formal way.
pub fn running_elevated() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags("SOFTWARE", winreg::enums::KEY_WRITE)
        .is_ok()
}

/// True only when the autostart entry points at **this** executable.
///
/// ⚠️ Same staleness trap as the protocol handler: the entry stores whatever
/// path wrote it, so an entry left behind by a build that has since moved is
/// worse than none — Windows would try to launch something that is not there,
/// and the UI would happily claim autostart is on. Reporting "not us" makes
/// the fix one click of Register.
fn autostart_points_at(exe: &Path) -> bool {
    let Ok(run) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
    else {
        return false;
    };
    let Ok(command) = run.get_value::<String, _>(RUN_VALUE) else {
        return false;
    };
    command_target(&command).is_some_and(|target| same_file(&target, exe))
}

pub fn marker_present() -> bool {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(MARKER_KEY, KEY_READ)
        .is_ok()
}

/// Resolves who effectively receives the scheme. Checks the per-user branch
/// first because that is the one Windows prefers.
fn protocol_owner(exe: &Path) -> Owner {
    let candidates = [
        (RegKey::predef(HKEY_CURRENT_USER), CLASSES, false),
        (RegKey::predef(HKEY_LOCAL_MACHINE), CLASSES, true),
    ];

    for (root, base, machine_wide) in candidates {
        let Some(command) = read_handler_command(&root, base) else {
            continue;
        };

        return match command_target(&command) {
            Some(target) if same_file(&target, exe) => Owner::Us,
            _ => Owner::Foreign {
                command,
                machine_wide,
            },
        };
    }

    Owner::Nobody
}

fn read_handler_command(root: &RegKey, base: &str) -> Option<String> {
    let path = format!(r"{base}\{SCHEME}\shell\open\command");
    let value: String = root
        .open_subkey_with_flags(path, KEY_READ)
        .ok()?
        .get_value("")
        .ok()?;
    (!value.trim().is_empty()).then_some(value)
}

/// Pulls the executable out of a `shell\open\command` value.
///
/// Handles the quoted form Windows expects — `"C:\Program Files\x.exe" "%1"` —
/// and the unquoted single-token form that older installers still write.
pub fn command_target(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    if let Some(rest) = command.strip_prefix('"') {
        let (inside, _) = rest.split_once('"')?;
        return (!inside.is_empty()).then(|| inside.to_owned());
    }

    // Unquoted: everything up to the first space. A path containing spaces
    // registered without quotes is already broken for Windows itself, so
    // guessing further would only invent a different wrong answer.
    command.split_whitespace().next().map(str::to_owned)
}

/// Windows paths are case-insensitive and the registry may hold a different
/// casing (or a short 8.3 form) than `current_exe` reports. Canonicalising
/// both resolves that; if either path cannot be canonicalised — the registered
/// program was uninstalled, say — fall back to a case-insensitive compare
/// rather than claiming ownership we cannot prove.
fn same_file(registered: &str, ours: &Path) -> bool {
    let registered = Path::new(registered);
    match (registered.canonicalize(), ours.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => registered
            .as_os_str()
            .eq_ignore_ascii_case(ours.as_os_str()),
    }
}

// --- Writing -------------------------------------------------------------

/// Declares this executable as the handler for the scheme, for this user.
///
/// ⚠️ Callers must have shown the user the current [`Owner`] first. This
/// function does not refuse to displace a foreign handler — refusing here
/// would put the decision in the wrong place — it simply does what it is told.
pub fn install_protocol_handler() -> Result<(), RegistryError> {
    let exe = own_exe()?;
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(CLASSES, winreg::enums::KEY_WRITE | KEY_READ)
        .map_err(wrap)?;

    let (scheme_key, _) = classes.create_subkey(SCHEME).map_err(wrap)?;
    // The default value is shown to the user by some shells; "URL Protocol"
    // is the flag that makes Windows treat the key as a scheme at all, and
    // its value is required to be empty.
    scheme_key
        .set_value("", &format!("URL:{SCHEME}"))
        .map_err(wrap)?;
    scheme_key.set_value("URL Protocol", &"").map_err(wrap)?;

    let (command_key, _) = classes
        .create_subkey(format!(r"{SCHEME}\shell\open\command"))
        .map_err(wrap)?;
    command_key
        .set_value("", &format_command(&exe))
        .map_err(wrap)
}

/// Removes our per-user registration. Leaves any machine-wide handler alone —
/// if Bambu's client is installed, this is what hands the scheme back to it.
pub fn remove_protocol_handler() -> Result<(), RegistryError> {
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(CLASSES, winreg::enums::KEY_WRITE | KEY_READ)
        .map_err(wrap)?;
    match classes.delete_subkey_all(SCHEME) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(wrap(error)),
    }
}

/// Asks Windows to start us at sign-in, in the tray.
///
/// The `--minimized` flag is the whole difference between "be ready" and "be
/// in the way": without it every boot would open a window nobody asked for.
pub fn install_autostart() -> Result<(), RegistryError> {
    let exe = own_exe()?;
    let run = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(RUN_KEY)
        .map_err(wrap)?
        .0;
    run.set_value(
        RUN_VALUE,
        &format!("\"{}\" {}", exe.display(), crate::MINIMIZED_ARG),
    )
    .map_err(wrap)
}

pub fn remove_autostart() -> Result<(), RegistryError> {
    let run = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_WRITE | KEY_READ)
        .map_err(wrap)?;
    match run.delete_value(RUN_VALUE) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(wrap(error)),
    }
}

/// Teaches Windows what this app's notifications are called.
///
/// Without this, a toast sent under our own AppUserModelID is **not drawn at
/// all** — the call succeeds and the screen stays empty, which is the single
/// most misleading failure in this whole app. With it, the id is valid and the
/// toast carries a name and an icon instead of a raw reverse-DNS string.
///
/// ⚠️ **Windows caches this on the AUMID's first use and never re-reads it.**
/// Register the identity BEFORE anything sends a toast under that id, or the
/// sender is stuck showing `top.bamdude.bridge` forever, no matter what the
/// key says afterwards. That ordering is why this lives in
/// [`register_receiver`]: registration necessarily precedes the first
/// handover, because there are no handovers until the protocol is registered.
///
/// The icon is best-effort — a missing one costs a picture, while failing the
/// whole registration over it would cost the feature.
pub fn install_toast_identity(
    aumid: &str,
    display_name: &str,
    icon: Option<&Path>,
) -> Result<(), RegistryError> {
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(format!(r"{TOAST_IDENTITY_KEY}\{aumid}"))
        .map_err(wrap)?
        .0;

    key.set_value("DisplayName", &display_name).map_err(wrap)?;

    if let Some(icon) = icon {
        let _ = key.set_value("IconUri", &icon.display().to_string());
    }
    Ok(())
}

pub fn remove_toast_identity(aumid: &str) -> Result<(), RegistryError> {
    let classes = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(TOAST_IDENTITY_KEY, winreg::enums::KEY_WRITE | KEY_READ)
        .map_err(wrap)?;
    match classes.delete_subkey_all(aumid) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(wrap(error)),
    }
}

/// Drops the embedded icon somewhere the notification platform can read it,
/// and answers with the path. Best-effort by design — see
/// [`install_toast_identity`].
fn write_toast_icon(config_dir: &Path) -> Option<std::path::PathBuf> {
    let path = config_dir.join(TOAST_ICON_FILE);
    if path.exists() {
        return Some(path);
    }
    std::fs::create_dir_all(config_dir).ok()?;
    std::fs::write(&path, TOAST_ICON).ok()?;
    Some(path)
}

/// Writes the HKLM marker. **Fails with [`RegistryError::AccessDenied`] unless
/// this process is elevated** — see [`request_elevated_marker`].
pub fn install_marker() -> Result<(), RegistryError> {
    let (_key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey(MARKER_KEY)
        .map_err(wrap)?;
    Ok(())
}

/// Command string Windows will run for an incoming URL.
///
/// `%1` must be quoted separately: the URL arrives percent-encoded but can
/// still be long, and an unquoted `%1` would split at the first space if a
/// future BambuStudio ever stops escaping.
fn format_command(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

// --- Elevation -----------------------------------------------------------

/// Re-launches this executable elevated, with [`INSTALL_MARKER_ARG`], so it
/// can write the one key that needs admin. Returns as soon as the prompt has
/// been raised; the caller should re-read [`status`] afterwards rather than
/// trust a return value.
///
/// A process cannot elevate itself in place on Windows — the only route is
/// starting a new one with the `runas` verb. PowerShell's `Start-Process` is
/// used for that rather than a direct `ShellExecuteW`, which would mean taking
/// on a Win32 binding crate for a single call.
pub fn request_elevated_marker() -> Result<(), RegistryError> {
    let exe = own_exe()?;
    let quoted = exe.display().to_string().replace('\'', "''");

    // CREATE_NO_WINDOW: without it a console flashes over the app every time.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    use std::os::windows::process::CommandExt;

    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!("Start-Process -FilePath '{quoted}' -ArgumentList '{INSTALL_MARKER_ARG}' -Verb RunAs -Wait"),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| RegistryError::Elevation(error.to_string()))?;

    Ok(())
}

// --- Commands reachable from the settings window -------------------------

#[tauri::command]
pub fn registration_status() -> Result<Status, String> {
    status().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn register_receiver(app: tauri::AppHandle) -> Result<Status, String> {
    install_protocol_handler().map_err(|error| error.to_string())?;

    // Autostart comes with being the receiver rather than as a separate
    // switch: a receiver that is not running when the slicer sends a plate
    // still works — Windows starts it — but it pays a cold start every time
    // and cannot be found in the tray in between.
    install_autostart().map_err(|error| error.to_string())?;

    // ⚠️ Must happen here, and not lazily before the first toast: Windows
    // caches an AUMID's name on first use and never looks again. Registering
    // is the last moment that is still guaranteed to be earlier than any
    // handover, because a handover cannot happen until the line above ran.
    let aumid = app.config().identifier.clone();
    let icon = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|dir| write_toast_icon(&dir));
    install_toast_identity(&aumid, "BamDude Bridge", icon.as_deref())
        .map_err(|error| error.to_string())?;

    if !marker_present() {
        request_elevated_marker().map_err(|error| error.to_string())?;
    }
    status().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn unregister_receiver(app: tauri::AppHandle) -> Result<Status, String> {
    remove_protocol_handler().map_err(|error| error.to_string())?;
    // Symmetry: nothing left to be ready for.
    remove_autostart().map_err(|error| error.to_string())?;
    let _ = remove_toast_identity(&app.config().identifier);
    status().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quoted_form_windows_expects_yields_the_exe() {
        let command = r#""C:\Program Files\BamDude Bridge\bamdude-bridge.exe" "%1""#;
        assert_eq!(
            command_target(command).as_deref(),
            Some(r"C:\Program Files\BamDude Bridge\bamdude-bridge.exe")
        );
    }

    #[test]
    fn an_unquoted_registration_still_yields_something() {
        assert_eq!(
            command_target(r"C:\apps\other.exe %1").as_deref(),
            Some(r"C:\apps\other.exe")
        );
    }

    #[test]
    fn an_empty_or_blank_command_owns_nothing() {
        assert_eq!(command_target(""), None);
        assert_eq!(command_target("   "), None);
        // An opening quote with nothing inside is malformed, not ownership.
        assert_eq!(command_target("\"\" \"%1\""), None);
    }

    #[test]
    fn the_command_we_write_quotes_both_halves() {
        // A path with spaces and an unquoted %1 are the two classic ways this
        // value gets written wrong.
        let command = format_command(Path::new(
            r"C:\Program Files\BamDude Bridge\bamdude-bridge.exe",
        ));
        assert_eq!(
            command,
            r#""C:\Program Files\BamDude Bridge\bamdude-bridge.exe" "%1""#
        );
        assert_eq!(
            command_target(&command).as_deref(),
            Some(r"C:\Program Files\BamDude Bridge\bamdude-bridge.exe")
        );
    }
}
