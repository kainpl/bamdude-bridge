//! Handing a sliced plate to the BamDude library.
//!
//! The server side of this needs no new code: `POST /api/v1/library/files`
//! already accepts a multipart upload, and an API key carrying the
//! library-manage scope already satisfies its permission. The bridge exists to
//! reach the FILE, not to teach the server anything.

use crate::config::{self, Settings};
use crate::farm_client_url::UploadRequest;
use tauri::AppHandle;

const LIBRARY_UPLOAD_PATH: &str = "/api/v1/library/files";

/// Suffix appended to the name the slicer sent.
///
/// ⚠️ BambuStudio sends a bare display name — `get_export_gcode_filename` is
/// called with an empty extension — while the bytes are a sliced 3MF carrying
/// G-code. The library decides what a file IS by looking inside it, so a wrong
/// suffix would not break the upload; it would just leave a confusing name in
/// somebody's library forever.
const SLICED_3MF_SUFFIX: &str = ".gcode.3mf";

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("BamDude Bridge is not configured yet — open it and set a server and API key")]
    NotConfigured,

    #[error("cannot read settings: {0}")]
    Settings(String),

    #[error("the slicer's file is gone: {path}")]
    FileMissing { path: String },

    #[error("cannot read {path}: {source}")]
    FileUnreadable { path: String, source: std::io::Error },

    #[error("cannot reach the server: {0}")]
    Unreachable(String),

    #[error("the server rejected the upload ({status}){detail}")]
    Rejected { status: u16, detail: String },
}

/// Reads the temporary 3MF the slicer just wrote and posts it to the library.
pub async fn send_to_library(app: &AppHandle, request: &UploadRequest) -> Result<(), UploadError> {
    let settings = config::load(app).map_err(|error| UploadError::Settings(error.to_string()))?;
    if !settings.is_complete() {
        return Err(UploadError::NotConfigured);
    }

    let bytes = read_slicer_file(&request.path).await?;
    post(&settings, &format!("{}{SLICED_3MF_SUFFIX}", request.name), bytes).await
}

/// ⚠️ Distinguishes "not there" from "there but unreadable" deliberately. The
/// first means we were too slow or the slicer cleaned up — a real possibility
/// worth its own message. The second is a permissions or locking problem, and
/// telling a user "the file is gone" when it is sitting right there sends them
/// looking in the wrong place.
async fn read_slicer_file(path: &str) -> Result<Vec<u8>, UploadError> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(bytes),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(UploadError::FileMissing { path: path.to_owned() })
        }
        Err(source) => Err(UploadError::FileUnreadable { path: path.to_owned(), source }),
    }
}

async fn post(settings: &Settings, filename: &str, bytes: Vec<u8>) -> Result<(), UploadError> {
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename.to_owned())
        .mime_str("application/octet-stream")
        .expect("a literal MIME type cannot fail to parse");

    let response = reqwest::Client::new()
        .post(settings.endpoint(LIBRARY_UPLOAD_PATH))
        .header("X-API-Key", &settings.api_key)
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .map_err(|error| UploadError::Unreachable(error.to_string()))?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    // The library's rejections are written for a person to read ("unsupported
    // file type", "folder is read-only"), so pass the body through instead of
    // flattening everything to a status code.
    let detail = response
        .text()
        .await
        .ok()
        .map(|body| body.trim().to_owned())
        .filter(|body| !body.is_empty())
        .map(|body| format!(": {body}"))
        .unwrap_or_default();

    Err(UploadError::Rejected { status: status.as_u16(), detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_file_is_reported_as_missing_not_as_unreadable() {
        let error = read_slicer_file("C:/definitely/not/here/plate.3mf").await.unwrap_err();
        assert!(matches!(error, UploadError::FileMissing { .. }), "got {error:?}");
    }

    #[test]
    fn the_uploaded_name_gains_the_sliced_suffix() {
        // Mirrors what send_to_library builds, and guards the trap that the
        // slicer's `name` arrives bare.
        let name = "Widget_plate_2";
        assert_eq!(format!("{name}{SLICED_3MF_SUFFIX}"), "Widget_plate_2.gcode.3mf");
    }
}
