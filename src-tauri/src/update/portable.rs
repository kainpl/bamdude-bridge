//! Updating a portable copy, which the official plugin cannot do.
//!
//! ⚠️ **The plugin runs the INSTALLER.** On Windows that is how it applies an
//! update, so pointing a portable build at it does not fail — it quietly
//! installs the app into `%LOCALAPPDATA%`, relaunches the *installed* copy, and
//! leaves the portable folder sitting there at the old version. It looks like
//! it worked. That silence is why this module exists.
//!
//! ## This is a port
//!
//! The shape is taken from `t8y2/dbx` (`src-tauri/src/commands/update_portable.rs`),
//! which solved the same problem in the same framework. What is copied is the
//! *design*, and every part of it earns its place:
//!
//! - a **marker file** answers "am I portable" — the binary is byte-identical
//!   to the installed one, so it cannot tell from itself;
//! - the asset name is **derived** from version and architecture rather than
//!   read from a manifest, so there is nothing to point us at a stranger's file;
//! - trust is the **same key the plugin uses**, read out of the embedded
//!   config — one root, not two;
//! - a second manifest **inside** the signed archive binds version, arch and a
//!   SHA-256 to the exact executable, so a correctly-signed archive still
//!   cannot deliver a different binary than it claims;
//! - the swap runs in a **detached helper**, because Windows will not let a
//!   running executable overwrite itself.

use std::io::{Cursor, Read};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The config is embedded so the public key cannot be swapped by editing a file
/// next to the executable — which is precisely what somebody attacking a
/// portable install would reach for.
const EMBEDDED_CONFIG: &str = include_str!("../../tauri.conf.json");

const EXECUTABLE_NAME: &str = "bamdude-bridge.exe";
const MANIFEST_NAME: &str = "portable-update.json";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// ⚠️ Its presence beside the exe is the ONLY thing that says "portable". An
/// installed copy is the same binary; without this it would take the portable
/// path and try to overwrite a file in Program Files.
pub const MARKER_NAME: &str = "portable.bamdude";

/// Generous, but not unbounded: a corrupt length field should not become an
/// allocation the size of the disk.
const MAX_EXECUTABLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize)]
struct PortableManifest {
    schema_version: u32,
    version: String,
    arch: String,
    executable: String,
    executable_sha256: String,
}

/// True when this process is running from a portable copy.
pub fn is_portable() -> bool {
    marker_dir().is_some()
}

/// The directory of a portable install, or `None` when this is an installed one.
fn marker_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    dir.join(MARKER_NAME).is_file().then_some(dir)
}

fn parse_version(value: &str, label: &str) -> Result<Version, String> {
    let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
    Version::parse(value).map_err(|error| format!("invalid {label} version: {error}"))
}

fn arch_label(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("x64"),
        "aarch64" => Ok("arm64"),
        other => Err(format!("portable updates are not built for {other}")),
    }
}

/// The asset this version's portable archive is published under.
///
/// ⚠️ Built from the version, never taken from the server's answer. A name that
/// arrived over the network could point anywhere, and the whole point of the
/// signature check is undone if we let it choose the file.
pub fn asset_name(version: &str) -> Result<String, String> {
    let version = parse_version(version, "requested")?;
    let arch = arch_label(std::env::consts::ARCH)?;
    Ok(format!(
        "bamdude-bridge-{version}-windows-{arch}-portable.zip"
    ))
}

/// Refuse anything that is not strictly newer.
///
/// ⚠️ A downgrade is how somebody replays an old, signed release to put a
/// version with a known hole back on the machine. The signature on that
/// archive is perfectly valid.
pub fn ensure_newer(requested: &str, current: &str) -> Result<Version, String> {
    let requested = parse_version(requested, "requested")?;
    let current = parse_version(current, "current")?;
    if requested <= current {
        return Err(format!(
            "{requested} is not newer than the installed {current}"
        ));
    }
    Ok(requested)
}

fn decode_tauri_text(value: &str, label: &str) -> Result<String, String> {
    let decoded = BASE64
        .decode(value.trim())
        .map_err(|error| format!("updater {label} is not valid base64: {error}"))?;
    String::from_utf8(decoded)
        .map_err(|error| format!("updater {label} is not valid UTF-8: {error}"))
}

/// Check the archive's signature against the key baked into this binary.
pub fn verify_signature(archive: &[u8], encoded_signature: &str) -> Result<(), String> {
    let config: serde_json::Value = serde_json::from_str(EMBEDDED_CONFIG)
        .map_err(|error| format!("embedded updater config is unreadable: {error}"))?;
    let encoded_key = config
        .pointer("/plugins/updater/pubkey")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "embedded updater public key is missing".to_string())?;

    let key = PublicKey::decode(&decode_tauri_text(encoded_key, "public key")?)
        .map_err(|error| format!("cannot read the updater public key: {error}"))?;
    let signature = Signature::decode(&decode_tauri_text(encoded_signature, "signature")?)
        .map_err(|error| format!("cannot read the update signature: {error}"))?;

    key.verify(archive, &signature, true)
        .map_err(|error| format!("the downloaded update is not signed by us: {error}"))
}

/// Pull the executable out of a signed archive, checking it is the one the
/// archive claims to carry.
///
/// ⚠️ The signature says "we built this archive". It does not say *what is in
/// it*, so the manifest inside binds the version, the architecture and the
/// hash. Without this a stale-but-signed archive could be served for a version
/// it is not.
pub fn validated_executable(archive: &[u8], expected: &Version) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|error| format!("the update archive is not a readable ZIP: {error}"))?;

    let manifest: PortableManifest = {
        let mut file = zip
            .by_name(MANIFEST_NAME)
            .map_err(|_| format!("the update archive has no {MANIFEST_NAME}"))?;
        if file.size() > MAX_MANIFEST_BYTES as u64 {
            return Err("the update manifest is implausibly large".to_string());
        }
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read the update manifest: {error}"))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid update manifest: {error}"))?
    };

    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(format!(
            "update manifest schema {} is not one this version understands",
            manifest.schema_version
        ));
    }
    if &parse_version(&manifest.version, "manifest")? != expected {
        return Err(format!(
            "the archive says it is {} but {expected} was asked for",
            manifest.version
        ));
    }
    if manifest.arch != arch_label(std::env::consts::ARCH)? {
        return Err(format!(
            "the archive is built for {}, not for this machine",
            manifest.arch
        ));
    }
    if manifest.executable != EXECUTABLE_NAME {
        return Err(format!(
            "the archive names {} rather than {EXECUTABLE_NAME}",
            manifest.executable
        ));
    }

    let mut file = zip
        .by_name(EXECUTABLE_NAME)
        .map_err(|_| format!("the update archive has no {EXECUTABLE_NAME}"))?;
    if file.size() > MAX_EXECUTABLE_BYTES as u64 {
        return Err("the update executable is implausibly large".to_string());
    }
    let mut executable = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut executable)
        .map_err(|error| format!("cannot extract {EXECUTABLE_NAME}: {error}"))?;

    // "MZ" is the DOS header every Windows executable starts with. Cheap, and
    // it catches a ZIP that carries something else entirely under the name.
    if !executable.starts_with(b"MZ") {
        return Err("the extracted file is not a Windows executable".to_string());
    }
    let digest: String = Sha256::digest(&executable)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if !digest.eq_ignore_ascii_case(manifest.executable_sha256.trim()) {
        return Err("the executable does not match the hash in the signed manifest".to_string());
    }
    Ok(executable)
}

/// Write the new executable in and restart, from a process that is not us.
///
/// ⚠️ **Windows will not let a running executable be overwritten**, which is
/// the whole reason for the helper, the wait and the retry loop. The old binary
/// is moved aside rather than deleted so a failure at any point can put it
/// back — the worst outcome must be "still on the old version", never "no
/// application at all".
#[cfg(windows)]
pub fn apply(archive: &[u8], version: &Version) -> Result<(), String> {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let current_exe =
        std::env::current_exe().map_err(|error| format!("cannot locate myself: {error}"))?;
    let dir = marker_dir().ok_or_else(|| {
        format!("this is not a portable copy — {MARKER_NAME} is not beside the executable")
    })?;

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("the system clock is unusable: {error}"))?
        .as_nanos();
    let id = format!("{}-{stamp}", std::process::id());

    // ⚠️ Asked BEFORE anything is downloaded into place: a portable copy in
    // Program Files, or on a read-only share, must fail here with a sentence
    // somebody can act on rather than halfway through replacing itself.
    let probe = dir.join(format!(".update-{id}.probe"));
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .map_err(|error| {
            format!("this folder is not writable, so the update cannot be applied here: {error}")
        })?;
    fs::remove_file(&probe).map_err(|error| format!("cannot clean up the write check: {error}"))?;

    let staging = std::env::temp_dir().join(format!("bamdude-bridge-update-{id}"));
    fs::create_dir(&staging).map_err(|error| format!("cannot create a staging folder: {error}"))?;

    let result = (|| -> Result<(), String> {
        let executable = validated_executable(archive, version)?;
        let staged = staging.join("bamdude-bridge.exe.new");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
            .map_err(|error| format!("cannot stage the new executable: {error}"))?;
        file.write_all(&executable)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot write the new executable: {error}"))?;

        let script = staging.join("apply-update.ps1");
        fs::write(&script, APPLY_SCRIPT)
            .map_err(|error| format!("cannot write the update helper: {error}"))?;

        Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script)
            .arg("-ParentProcessId")
            .arg(std::process::id().to_string())
            .arg("-SourceExe")
            .arg(&staged)
            .arg("-TargetExe")
            .arg(&current_exe)
            .arg("-BackupExe")
            .arg(dir.join(format!(".bamdude-bridge-{id}.old.exe")))
            .arg("-StagingDir")
            .arg(&staging)
            // No console flash, and the helper must outlive us — we are about
            // to exit so it can take our place.
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
            .spawn()
            .map_err(|error| format!("cannot start the update helper: {error}"))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(not(windows))]
pub fn apply(_archive: &[u8], _version: &Version) -> Result<(), String> {
    Err("portable updates are a Windows path".to_string())
}

/// ⚠️ Every failure branch here ends with the old executable back in place and
/// running. A half-applied update on somebody's USB stick is the one outcome
/// worse than not updating.
#[cfg(windows)]
const APPLY_SCRIPT: &str = r#"param(
    [Parameter(Mandatory = $true)][int]$ParentProcessId,
    [Parameter(Mandatory = $true)][string]$SourceExe,
    [Parameter(Mandatory = $true)][string]$TargetExe,
    [Parameter(Mandatory = $true)][string]$BackupExe,
    [Parameter(Mandatory = $true)][string]$StagingDir
)

$ErrorActionPreference = 'Stop'
Remove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue
try { Wait-Process -Id $ParentProcessId -Timeout 120 -ErrorAction SilentlyContinue } catch {}

$installed = $false
for ($attempt = 0; $attempt -lt 120; $attempt++) {
    try {
        if (Test-Path -LiteralPath $TargetExe) {
            if (Test-Path -LiteralPath $BackupExe) { Remove-Item -LiteralPath $BackupExe -Force }
            Move-Item -LiteralPath $TargetExe -Destination $BackupExe -Force
        }
        if (-not (Test-Path -LiteralPath $BackupExe)) { throw 'the old executable could not be set aside' }
        Move-Item -LiteralPath $SourceExe -Destination $TargetExe -Force
        $installed = $true
        break
    } catch {
        if (-not (Test-Path -LiteralPath $TargetExe) -and (Test-Path -LiteralPath $BackupExe)) {
            try { Copy-Item -LiteralPath $BackupExe -Destination $TargetExe -Force } catch {}
        }
        Start-Sleep -Seconds 1
    }
}

if (-not $installed) {
    if (-not (Test-Path -LiteralPath $TargetExe) -and (Test-Path -LiteralPath $BackupExe)) {
        try { Copy-Item -LiteralPath $BackupExe -Destination $TargetExe -Force } catch {}
    }
    exit 1
}

try {
    Start-Process -FilePath $TargetExe -WorkingDirectory (Split-Path -Parent $TargetExe)
} catch {
    try {
        if (Test-Path -LiteralPath $TargetExe) { Remove-Item -LiteralPath $TargetExe -Force }
        if (Test-Path -LiteralPath $BackupExe) { Move-Item -LiteralPath $BackupExe -Destination $TargetExe -Force }
        Start-Process -FilePath $TargetExe -WorkingDirectory (Split-Path -Parent $TargetExe)
    } catch {}
    exit 1
}

Remove-Item -LiteralPath $BackupExe -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $StagingDir -Recurse -Force -ErrorAction SilentlyContinue
exit 0
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(version: &str, arch: &str, exe: &[u8], hash: Option<&str>) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let digest: String = Sha256::digest(exe)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let manifest = serde_json::json!({
            "schema_version": MANIFEST_SCHEMA_VERSION,
            "version": version,
            "arch": arch,
            "executable": EXECUTABLE_NAME,
            "executable_sha256": hash.map(ToOwned::to_owned).unwrap_or(digest),
        });
        zip.start_file(MANIFEST_NAME, options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.start_file(EXECUTABLE_NAME, options).unwrap();
        zip.write_all(exe).unwrap();
        zip.finish().unwrap().into_inner()
    }

    fn here() -> &'static str {
        arch_label(std::env::consts::ARCH).unwrap()
    }

    #[test]
    fn the_asset_name_is_built_from_the_version_not_taken_from_anyone() {
        assert_eq!(
            asset_name("0.2.0").unwrap(),
            format!("bamdude-bridge-0.2.0-windows-{}-portable.zip", here())
        );
    }

    #[test]
    fn a_version_that_is_not_a_version_is_refused_before_anything_is_fetched() {
        assert!(asset_name("../../../etc/passwd").is_err());
        assert!(asset_name("").is_err());
    }

    #[test]
    fn a_downgrade_is_refused() {
        // ⚠️ Replaying an older signed release is how a known hole comes back,
        // and its signature is perfectly valid.
        assert!(ensure_newer("0.1.0", "0.2.0").is_err());
        assert!(ensure_newer("0.2.0", "0.2.0").is_err());
        assert!(ensure_newer("0.2.1", "0.2.0").is_ok());
    }

    #[test]
    fn a_leading_v_is_tolerated_on_either_side() {
        assert!(ensure_newer("v0.3.0", "0.2.0").is_ok());
    }

    #[test]
    fn the_executable_bound_by_the_manifest_comes_out() {
        let zip = archive("0.2.0", here(), b"MZreal", None);
        assert_eq!(
            validated_executable(&zip, &Version::parse("0.2.0").unwrap()).unwrap(),
            b"MZreal"
        );
    }

    #[test]
    fn an_archive_for_another_version_is_refused() {
        // A correctly signed archive is still the wrong archive if it is stale.
        let zip = archive("0.1.0", here(), b"MZold", None);
        let error = validated_executable(&zip, &Version::parse("0.2.0").unwrap()).unwrap_err();
        assert!(error.contains("was asked for"), "{error}");
    }

    #[test]
    fn an_executable_that_does_not_match_its_hash_is_refused() {
        let zip = archive("0.2.0", here(), b"MZreal", Some(&"0".repeat(64)));
        let error = validated_executable(&zip, &Version::parse("0.2.0").unwrap()).unwrap_err();
        assert!(error.contains("hash"), "{error}");
    }

    #[test]
    fn something_that_is_not_an_executable_is_refused() {
        let zip = archive("0.2.0", here(), b"just text", None);
        assert!(validated_executable(&zip, &Version::parse("0.2.0").unwrap()).is_err());
    }

    #[test]
    fn an_archive_built_for_another_machine_is_refused() {
        let other = if here() == "x64" { "arm64" } else { "x64" };
        let zip = archive("0.2.0", other, b"MZreal", None);
        let error = validated_executable(&zip, &Version::parse("0.2.0").unwrap()).unwrap_err();
        assert!(error.contains("not for this machine"), "{error}");
    }

    #[test]
    fn an_unsigned_archive_does_not_pass_as_ours() {
        assert!(verify_signature(b"anything", "").is_err());
    }

    #[test]
    fn the_public_key_is_actually_embedded() {
        // ⚠️ If the config ever loses the key, every portable update would fail
        // at verification with a confusing message. Better to know here.
        let config: serde_json::Value = serde_json::from_str(EMBEDDED_CONFIG).unwrap();
        let key = config
            .pointer("/plugins/updater/pubkey")
            .and_then(|v| v.as_str());
        assert!(
            key.is_some_and(|k| !k.is_empty()),
            "no updater pubkey in tauri.conf.json"
        );
    }
}
