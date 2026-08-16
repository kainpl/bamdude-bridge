//! Parsing the URL BambuStudio hands us when the user picks
//! "Send to Bambu Farm Manager Client".
//!
//! Shape, as emitted by BambuStudio (read from `Plater.cpp`,
//! `Plater::priv::on_action_send_to_multi_app`, at tag v02.08.02.60):
//!
//! ```text
//! bambu-farm-client://upload-file + curl_easy_escape(
//!     "?version=v1.6.0&path=<absolute path>&name=<export name>")
//! ```
//!
//! Three properties of that one line drive everything below.
//!
//! 1. **The whole query is percent-encoded in a single pass, separators
//!    included.** `?` arrives as `%3F`, `&` as `%26`, `=` as `%3D`. Handing
//!    the raw argument to a URL library finds no query at all: every `/` is
//!    escaped too, so the entire remainder parses as a host component. We do
//!    string work on the raw argument instead, deliberately.
//!
//! 2. **Because the encoding is uniform, a literal `&` inside a value is
//!    indistinguishable from a separator once decoded.** BambuStudio strips
//!    ``<>[]:/\|?*"`` from the project name but not `&`, so `Fish & Chips` is
//!    a name it will happily send. Decode-then-split would cut that name in
//!    half. We locate the parameters on the *encoded* text and decode each
//!    value afterwards.
//!
//! 3. **`name` carries no file extension.** BambuStudio calls
//!    `get_export_gcode_filename` with an empty extension, so what arrives is
//!    `<project>_plate_2`, bare. Callers that need a filename must append the
//!    suffix themselves — see [`UploadRequest::name`].

use percent_encoding::percent_decode_str;

/// The URL scheme Bambu Lab's farm client owns and we register ourselves for.
pub const SCHEME: &str = "bambu-farm-client";

const SCHEME_PREFIX: &str = "bambu-farm-client://";
const ACTION_UPLOAD_FILE: &str = "upload-file";

/// One "send this plate" request from the slicer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRequest {
    /// Protocol version the slicer declared, e.g. `v1.6.0`. Informational:
    /// BambuStudio announces it, it does not negotiate, and there is no
    /// version we have a reason to refuse. Kept so a future change is visible
    /// in logs rather than silent.
    pub version: Option<String>,

    /// Absolute path to the temporary 3MF the slicer just exported, with
    /// forward slashes (BambuStudio normalises `\` before encoding; Windows
    /// accepts both, so we pass it through untouched).
    pub path: String,

    /// Display name for the plate — **without an extension**. The file itself
    /// is a sliced 3MF, so a caller building a filename wants
    /// `format!("{name}.gcode.3mf")`.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("not a {SCHEME} URL")]
    ForeignScheme,

    #[error("unsupported action `{0}`")]
    UnknownAction(String),

    #[error("required parameter `{0}` is missing")]
    MissingParameter(&'static str),

    #[error("parameters are out of order — `name` precedes `path`")]
    ParametersOutOfOrder,

    #[error("parameter `{0}` is not valid UTF-8 once decoded")]
    NotUtf8(&'static str),
}

/// Parses one raw command-line argument into an upload request.
pub fn parse(raw: &str) -> Result<UploadRequest, ParseError> {
    let rest = strip_scheme(raw).ok_or(ParseError::ForeignScheme)?;
    let (action, query) = split_action(rest);

    if !action.eq_ignore_ascii_case(ACTION_UPLOAD_FILE) {
        return Err(ParseError::UnknownAction(action.to_owned()));
    }

    parse_upload_query(query)
}

/// True when `raw` is addressed to us at all. Cheap enough to call on every
/// argv entry before deciding whether this process launch is a handover.
pub fn is_ours(raw: &str) -> bool {
    strip_scheme(raw).is_some()
}

fn strip_scheme(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.len() < SCHEME_PREFIX.len() {
        return None;
    }
    let (head, tail) = trimmed.split_at(SCHEME_PREFIX.len());
    head.eq_ignore_ascii_case(SCHEME_PREFIX).then_some(tail)
}

/// Splits `upload-file%3Fversion%3D…` into the action and the query that
/// follows it. Accepts the literal `?` form too, so the URL stays testable
/// by hand.
fn split_action(rest: &str) -> (&str, &str) {
    if let Some(at) = find_ci(rest, "%3F") {
        (&rest[..at], &rest[at + 3..])
    } else if let Some(at) = rest.find('?') {
        (&rest[..at], &rest[at + 1..])
    } else {
        (rest, "")
    }
}

/// Where one parameter sits in the encoded query.
struct Param {
    /// Index of the separator that introduces it (0 for the leading one).
    sep_at: usize,
    /// Index just past `key=`, where the value begins.
    val_at: usize,
}

fn parse_upload_query(query: &str) -> Result<UploadRequest, ParseError> {
    let path = find_param(query, "path", 0).ok_or(ParseError::MissingParameter("path"))?;

    // Search for `name` only in the tail that follows `path`'s value. Looking
    // from the left of that tail (rather than from the end of the whole query)
    // keeps the PATH intact when a project name contains something that looks
    // like an anchor — and the path is the half we must not corrupt, because
    // we open it. A name pathological enough to contain a literal `&name=`
    // loses its tail; a path we truncate loses the file.
    let name = match find_param(query, "name", path.val_at) {
        Some(found) => found,
        None => {
            // Nothing after `path`. Absent, or the slicer reordered its
            // format string — worth distinguishing, because the second means
            // our reading of the format has gone stale and the message should
            // say so rather than blame a missing parameter.
            return Err(match find_param(query, "name", 0) {
                Some(_) => ParseError::ParametersOutOfOrder,
                None => ParseError::MissingParameter("name"),
            });
        }
    };

    // `version` is the leading parameter, so it carries no separator of its
    // own: the query begins with it directly. Absent is fine.
    let version = find_leading_param(query, "version")
        .map(|v| decode(&query[v.val_at..path.sep_at], "version"))
        .transpose()?;

    Ok(UploadRequest {
        version,
        path: decode(&query[path.val_at..name.sep_at], "path")?,
        name: decode(&query[name.val_at..], "name")?,
    })
}

/// Finds `&key=` — or its encoded twin `%26key%3D` — at or after `from`.
///
/// ⚠️ **The first parameter of a query carries no separator**, because the `?`
/// that introduced it was consumed along with the action. `version` is
/// optional, so `path` is routinely the leading one; a search that only looks
/// for the separator form finds nothing at all in that very common case.
fn find_param(query: &str, key: &str, from: usize) -> Option<Param> {
    let tail = query.get(from..)?;

    if from == 0 {
        if let Some(leading) = match_leading(tail, key) {
            return Some(leading);
        }
    }

    for (sep, eq) in [("%26", "%3D"), ("&", "=")] {
        let pattern = format!("{sep}{key}{eq}");
        if let Some(at) = find_ci(tail, &pattern) {
            let sep_at = from + at;
            return Some(Param {
                sep_at,
                val_at: sep_at + pattern.len(),
            });
        }
    }
    None
}

/// Finds `key=` / `key%3D` sitting at the very start of `query`.
///
/// Used on its own for `version`, which is only ever the leading parameter:
/// accepting it anywhere would let a `version` that follows `path` produce a
/// backwards slice.
fn find_leading_param(query: &str, key: &str) -> Option<Param> {
    match_leading(query, key)
}

fn match_leading(text: &str, key: &str) -> Option<Param> {
    for eq in ["%3D", "="] {
        let pattern = format!("{key}{eq}");
        // `get` rather than indexing: the lenient unencoded form can carry
        // multi-byte UTF-8, and slicing into the middle of a character panics.
        if text
            .get(..pattern.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&pattern))
        {
            return Some(Param {
                sep_at: 0,
                val_at: pattern.len(),
            });
        }
    }
    None
}

/// Case-insensitive substring search that returns an index valid in the
/// ORIGINAL string. Safe because `to_ascii_lowercase` maps only `A-Z`, leaving
/// every other byte — including multi-byte UTF-8 — the same length.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn decode(raw: &str, what: &'static str) -> Result<String, ParseError> {
    percent_decode_str(raw)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| ParseError::NotUtf8(what))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what BambuStudio emits: every separator escaped, drive letter
    /// and slashes escaped, nothing left readable.
    const REAL: &str = "bambu-farm-client://upload-file\
        %3Fversion%3Dv1.6.0\
        %26path%3DC%3A%2FUsers%2Fkain%2FAppData%2FRoaming%2FBambuStudio%2Fbackup\
%2F1234%2FMetadata%2F.5678.0.3mf\
        %26name%3DWidget_plate_2";

    #[test]
    fn parses_what_bambustudio_actually_sends() {
        let got = parse(REAL).expect("the real shape must parse");

        assert_eq!(got.version.as_deref(), Some("v1.6.0"));
        assert_eq!(
            got.path,
            "C:/Users/kain/AppData/Roaming/BambuStudio/backup/1234/Metadata/.5678.0.3mf"
        );
        assert_eq!(got.name, "Widget_plate_2");
    }

    #[test]
    fn the_name_arrives_without_an_extension() {
        // Guards the trap documented on `UploadRequest::name`: if this ever
        // starts carrying `.3mf`, the callers appending a suffix are wrong.
        let got = parse(REAL).unwrap();
        assert!(
            !got.name.contains('.'),
            "name unexpectedly carries an extension: {}",
            got.name
        );
    }

    #[test]
    fn an_ampersand_in_the_project_name_survives() {
        // BambuStudio filters `<>[]:/\|?*"` out of a project name — but not
        // `&`. Decoding before splitting would truncate this to "Fish ".
        let url = "bambu-farm-client://upload-file\
            %3Fversion%3Dv1.6.0%26path%3DC%3A%2Ftmp%2Fa.3mf%26name%3DFish%20%26%20Chips";

        let got = parse(url).unwrap();

        assert_eq!(got.path, "C:/tmp/a.3mf");
        assert_eq!(got.name, "Fish & Chips");
    }

    #[test]
    fn a_non_ascii_name_round_trips() {
        // %D0%9A%D1%83%D0%B1 == "Куб"
        let url = "bambu-farm-client://upload-file\
            %3Fpath%3DC%3A%2Ftmp%2Fa.3mf%26name%3D%D0%9A%D1%83%D0%B1_plate_1";

        assert_eq!(parse(url).unwrap().name, "Куб_plate_1");
    }

    #[test]
    fn a_hand_written_unencoded_url_also_parses() {
        // Not a shape BambuStudio emits — a shape a human types while testing.
        let url = "bambu-farm-client://upload-file?version=v1.6.0&path=C:/tmp/a.3mf&name=Widget";

        let got = parse(url).unwrap();

        assert_eq!(got.path, "C:/tmp/a.3mf");
        assert_eq!(got.name, "Widget");
    }

    #[test]
    fn the_scheme_is_matched_case_insensitively() {
        // Windows does not promise us the case it hands back.
        let url = "BAMBU-FARM-CLIENT://upload-file%3Fpath%3DC%3A%2Fa.3mf%26name%3DX";
        assert!(parse(url).is_ok());
    }

    #[test]
    fn version_is_optional() {
        let url = "bambu-farm-client://upload-file%3Fpath%3DC%3A%2Fa.3mf%26name%3DX";
        assert_eq!(parse(url).unwrap().version, None);
    }

    #[test]
    fn a_leading_path_is_found_even_though_it_has_no_separator() {
        // Regression. The first parameter of a query carries no separator —
        // the `?` went with the action — and `version` is optional, so a
        // leading `path` is the ordinary case rather than an exotic one.
        // Searching only for the `%26path%3D` form found nothing at all here
        // and reported the path missing, which took five tests down at once.
        let url = "bambu-farm-client://upload-file%3Fpath%3DC%3A%2Ftmp%2Fa.3mf%26name%3DX";
        assert_eq!(parse(url).unwrap().path, "C:/tmp/a.3mf");
    }

    #[test]
    fn a_foreign_scheme_is_refused() {
        assert_eq!(
            parse("https://example.com/upload-file"),
            Err(ParseError::ForeignScheme)
        );
        assert!(!is_ours("https://example.com"));
        assert!(is_ours(REAL));
    }

    #[test]
    fn an_unknown_action_is_refused_by_name() {
        // If Bambu ever adds a second verb we want the log to say which one,
        // not "malformed".
        let err = parse("bambu-farm-client://delete-everything%3Fpath%3Da").unwrap_err();
        assert_eq!(err, ParseError::UnknownAction("delete-everything".into()));
    }

    #[test]
    fn a_missing_path_is_an_error_not_an_empty_string() {
        let err = parse("bambu-farm-client://upload-file%3Fname%3DX").unwrap_err();
        assert_eq!(err, ParseError::MissingParameter("path"));
    }

    #[test]
    fn a_missing_name_is_an_error() {
        let err = parse("bambu-farm-client://upload-file%3Fpath%3DC%3A%2Fa.3mf").unwrap_err();
        assert_eq!(err, ParseError::MissingParameter("name"));
    }

    #[test]
    fn reversed_parameters_are_refused_rather_than_panicking() {
        // Slicing path..name backwards would panic on an out-of-order range;
        // this is the guard that keeps a malformed URL from taking the app down.
        let url = "bambu-farm-client://upload-file%3Fname%3DX%26path%3DC%3A%2Fa.3mf";
        assert_eq!(parse(url), Err(ParseError::ParametersOutOfOrder));
    }
}
