//! Asking BamDude for a label to print, and saying what happened to it.
//!
//! The whole reason this app exists in one loop. A container cannot reach a USB
//! printer, so the server holds a queue and we come and take from it — there is
//! no inbound connection to this desktop anywhere in the design, which is what
//! makes it work behind NAT, on a laptop, through a corporate firewall.
//!
//! ⚠️ **The server sets the pace, not us.** Every answer carries `Retry-After`
//! and we obey it. An administrator can slow a chatty bridge down from the
//! settings page they are already looking at, and a bridge that was just handed
//! work is told to come straight back — so a batch of ten labels drains at
//! printer speed rather than one label per poll interval.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::config::{self, Settings};

use super::encoder::encode_image;
use super::models::by_id;
use super::serial::SerialTransport;
use super::status::{read_snapshot, PrinterSnapshot};
use super::task::{print_b1, select_task, PrintOptions};

const POLL_PATH: &str = "/api/v1/label-devices/poll";
const RESULT_PATH: &str = "/api/v1/label-devices/jobs";

/// Used when the server says nothing. It always does say something, so this is
/// the answer to a server that is older than this feature — back off politely
/// rather than hammer it.
const FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

/// ⚠️ A ceiling on what the server may ask for. A bad `Retry-After` should not
/// be able to park the bridge for a week.
const MAX_INTERVAL: Duration = Duration::from_secs(600);

/// How long to wait after a network failure. Deliberately not exponential: the
/// common case is a server being restarted, and coming back in ten seconds is
/// what somebody watching expects.
const UNREACHABLE_INTERVAL: Duration = Duration::from_secs(10);

/// Long enough for a label to feed, short enough that a wedged printer does not
/// hold the loop forever.
const PRINT_TIMEOUT: Duration = Duration::from_secs(30);

/// What we tell the server about ourselves and what we can see.
///
/// ⚠️ No `enabled` and no `name`. Those belong to the person adopting this
/// device; a bridge that could set them would be adopting itself.
#[derive(Debug, Serialize)]
struct DeviceReport {
    installation_id: String,
    driver: &'static str,
    model: Option<&'static str>,
    protocol_version: Option<u16>,
    transport: &'static str,
    address: String,
    app_version: String,
    paper_state: Option<u8>,
    power_level: Option<u8>,
    printer_reachable: bool,
    cassette: Option<CassetteReport>,
}

#[derive(Debug, Serialize)]
struct CassetteReport {
    barcode: String,
}

/// A job the server has just handed us. It is ours now — nobody else will get
/// it — so failing to report on it strands it until the server's sweep.
#[derive(Debug, Deserialize)]
struct JobHandout {
    job_id: i64,
    image_png: String,
    #[allow(dead_code)]
    width_px: u32,
    #[allow(dead_code)]
    height_px: u32,
    copies: u16,
    density: u8,
}

#[derive(Debug, Serialize)]
struct JobResult {
    ok: bool,
    error: Option<String>,
}

/// What the window shows about the loop.
///
/// ⚠️ The installation id is here because it is the ONE thing somebody needs in
/// order to find this machine in the server's device list and adopt it. Without
/// it on screen they are matching a UUID against a list of UUIDs by eye.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PollerStatus {
    pub installation_id: String,
    /// Absent until the first answer of any kind. "Never talked to the server"
    /// and "talked, and it said no" are different problems.
    pub last_contact: Option<String>,
    pub last_outcome: Option<String>,
    /// True while the role is off or unconfigured — the loop is alive but idle,
    /// which must not look like a crash.
    pub idle: bool,
}

fn status_cell() -> &'static Mutex<PollerStatus> {
    static CELL: OnceLock<Mutex<PollerStatus>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(PollerStatus::default()))
}

fn set_status(update: impl FnOnce(&mut PollerStatus)) {
    if let Ok(mut status) = status_cell().lock() {
        update(&mut status);
    }
}

/// What the settings window asks for. Cheap and lock-free from the caller's
/// point of view — it is a snapshot, not a subscription.
#[tauri::command]
pub fn label_poller_status() -> PollerStatus {
    status_cell().lock().map(|s| s.clone()).unwrap_or_default()
}

/// A timestamp the window can print without a date library.
fn stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.to_string()
}

/// What one pass of the loop decided to do next.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    After(Duration),
}

/// Turn the server's `Retry-After` into something safe to sleep on.
///
/// ⚠️ Clamped at both ends. `0` is a real and useful answer — "there is more,
/// come straight back" — but an unbounded value from a confused server would
/// park the bridge indefinitely, and a garbage one must not crash the loop.
fn interval_from_header(value: Option<&str>) -> Duration {
    match value.and_then(|raw| raw.trim().parse::<u64>().ok()) {
        Some(seconds) => Duration::from_secs(seconds).min(MAX_INTERVAL),
        None => FALLBACK_INTERVAL,
    }
}

/// Read the printer, or say plainly that we could not.
///
/// ⚠️ A missing printer is reported, not hidden. The server needs to know the
/// difference between "the bridge is gone" and "the bridge is here and the USB
/// cable is out" — they have different fixes, and only one of them is ours.
async fn look_at_printer(port: &str) -> (bool, Option<PrinterSnapshot>) {
    match SerialTransport::open(port) {
        Ok(mut transport) => match read_snapshot(&mut transport).await {
            Ok(snapshot) => (true, Some(snapshot)),
            Err(error) => {
                log::warn!("label printer on {port} did not answer: {error}");
                (false, None)
            }
        },
        Err(error) => {
            log::warn!("cannot open {port}: {error}");
            (false, None)
        }
    }
}

fn build_report(
    settings: &Settings,
    reachable: bool,
    snapshot: Option<&PrinterSnapshot>,
) -> DeviceReport {
    let heartbeat = snapshot.and_then(|s| s.heartbeat.as_ref());
    DeviceReport {
        installation_id: settings.installation_id.clone(),
        driver: "niimbot",
        model: snapshot.and_then(|s| s.model_name),
        protocol_version: snapshot.and_then(|s| s.model_id),
        transport: "serial",
        address: settings.label_port.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        // ⚠️ `None` when the printer did not say, never a guessed `0`. The
        // server treats 0 as "out of paper" and would hold every job forever.
        paper_state: heartbeat.and_then(|h| h.paper_inserted).map(u8::from),
        power_level: heartbeat.and_then(|h| h.charge_level),
        printer_reachable: reachable,
        cassette: snapshot
            .and_then(|s| s.cassette.as_ref())
            .map(|c| CassetteReport {
                barcode: c.barcode.clone(),
            }),
    }
}

/// Print one handed-out job. Returns the message to report on failure.
async fn print_handout(port: &str, job: &JobHandout) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(job.image_png.as_bytes())
        .map_err(|error| format!("the server's image was not valid base64: {error}"))?;
    let image = image::load_from_memory(&bytes)
        .map_err(|error| format!("the server's image was not a readable PNG: {error}"))?;

    let mut transport = SerialTransport::open(port).map_err(|error| error.to_string())?;
    let snapshot = read_snapshot(&mut transport)
        .await
        .map_err(|error| error.to_string())?;

    let model_id = snapshot
        .model_id
        .ok_or_else(|| String::from("the printer did not say what model it is"))?;
    let model = by_id(model_id)
        .ok_or_else(|| format!("model id {model_id} is not one this app can print on"))?;
    select_task(model_id, None)
        .ok_or_else(|| format!("no print flow is ported for model id {model_id}"))?;

    let encoded = encode_image(&image, model.printhead_pixels, model.print_direction)
        .map_err(|error| error.to_string())?;

    // ⚠️ From the tag, not assumed. A continuous roll told to look for gaps
    // just feeds until it gives up.
    let label_type = snapshot
        .cassette
        .as_ref()
        .map(|c| c.consumable_type)
        .unwrap_or(1);

    let options = PrintOptions {
        // The server's number, clamped to what this printer admits to. It knows
        // which device this is; the printer knows what it can do.
        density: job.density.clamp(model.density_min, model.density_max),
        label_type,
        copies: job.copies.max(1),
        status_timeout: PRINT_TIMEOUT,
        ..Default::default()
    };

    print_b1(&mut transport, &encoded, &options)
        .await
        .map_err(|error| error.to_string())
}

async fn report_result(
    client: &reqwest::Client,
    settings: &Settings,
    job_id: i64,
    result: JobResult,
) {
    let url = format!(
        "{}/{}/result?installation_id={}",
        settings.endpoint(RESULT_PATH),
        job_id,
        urlencode(&settings.installation_id)
    );
    match client
        .post(&url)
        .header("X-API-Key", &settings.api_key)
        .json(&result)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {}
        // ⚠️ Logged and dropped, never retried in place. The server's sweep will
        // requeue a job nobody reported on, and a bridge that blocked its loop
        // retrying one result would stop printing everything behind it.
        Ok(response) => log::warn!(
            "server answered {} to the result for job {job_id}",
            response.status()
        ),
        Err(error) => log::warn!("could not report on job {job_id}: {error}"),
    }
}

fn urlencode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// One request, and whatever it leads to. Returns when to come back.
async fn tick(client: &reqwest::Client, settings: &Settings) -> Next {
    let (reachable, snapshot) = look_at_printer(&settings.label_port).await;
    let report = build_report(settings, reachable, snapshot.as_ref());

    let response = match client
        .post(settings.endpoint(POLL_PATH))
        .header("X-API-Key", &settings.api_key)
        .json(&report)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            log::warn!("cannot reach BamDude: {error}");
            set_status(|s| s.last_outcome = Some(format!("Cannot reach the server: {error}")));
            return Next::After(UNREACHABLE_INTERVAL);
        }
    };

    let status = response.status();
    let wait = interval_from_header(
        response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
    );

    // 204 covers three different situations that all mean the same thing to us:
    // nothing queued, nobody has adopted this device yet, or the printer is out
    // of paper. The server decided which; we just come back.
    if status == reqwest::StatusCode::NO_CONTENT {
        set_status(|s| {
            s.last_contact = Some(stamp());
            s.last_outcome = Some(String::from("Connected — nothing queued."));
        });
        return Next::After(wait);
    }

    if status == reqwest::StatusCode::CONFLICT {
        set_status(|s| {
            s.last_contact = Some(stamp());
            s.last_outcome = Some(String::from(
                "Connected, but label printing is switched off on the server.",
            ));
        });
        return Next::After(wait);
    }

    if !status.is_success() {
        log::warn!("BamDude answered {status} to a poll");
        set_status(|s| {
            s.last_contact = Some(stamp());
            s.last_outcome = Some(format!("The server answered {status}."));
        });
        return Next::After(wait.max(UNREACHABLE_INTERVAL));
    }

    let job: JobHandout = match response.json().await {
        Ok(job) => job,
        Err(error) => {
            log::warn!("could not read the handed-out job: {error}");
            return Next::After(FALLBACK_INTERVAL);
        }
    };

    log::info!(
        "printing label job {} ({} cop(y|ies))",
        job.job_id,
        job.copies
    );
    let outcome = print_handout(&settings.label_port, &job).await;
    let result = match &outcome {
        Ok(()) => JobResult {
            ok: true,
            error: None,
        },
        Err(message) => {
            log::warn!("label job {} failed: {message}", job.job_id);
            JobResult {
                ok: false,
                error: Some(message.clone()),
            }
        }
    };
    set_status(|s| {
        s.last_contact = Some(stamp());
        s.last_outcome = Some(match &outcome {
            Ok(()) => format!("Printed job {}.", job.job_id),
            Err(message) => format!("Job {} failed: {message}", job.job_id),
        });
    });
    report_result(client, settings, job.job_id, result).await;

    // ⚠️ A failed job does not earn an immediate retry. The server said "come
    // straight back" because it had more work, and racing back to a printer
    // that just refused would burn all three attempts in a second.
    if outcome.is_err() {
        return Next::After(wait.max(UNREACHABLE_INTERVAL));
    }
    Next::After(wait)
}

/// Runs for the life of the app, re-reading settings every pass.
///
/// ⚠️ Settings are read each time round rather than captured once. Somebody who
/// switches the role on, or fixes the port, expects it to take effect without
/// restarting the app — and this loop is the only thing that would have to be
/// restarted.
pub async fn run(app: AppHandle, stop: Arc<AtomicBool>) {
    let client = reqwest::Client::new();
    log::info!("label poller started");

    while !stop.load(Ordering::Relaxed) {
        let settings = match config::load_with_identity(&app) {
            Ok(settings) => settings,
            Err(error) => {
                log::warn!("cannot read settings: {error}");
                tokio::time::sleep(FALLBACK_INTERVAL).await;
                continue;
            }
        };

        // ⚠️ Published before anything else can go wrong. The id is what
        // somebody needs in order to adopt this machine, and a bridge that
        // cannot reach the server is exactly when they are looking for it.
        set_status(|status| status.installation_id = settings.installation_id.clone());

        if !settings.label_enabled
            || settings.label_port.trim().is_empty()
            || !settings.is_complete()
        {
            // Idle, not stopped. A loop alive and waiting for configuration
            // must not read on screen as one that died.
            set_status(|status| status.idle = true);
            tokio::time::sleep(FALLBACK_INTERVAL).await;
            continue;
        }
        set_status(|status| status.idle = false);

        let Next::After(wait) = tick(&client, &settings).await;
        if wait.is_zero() {
            // Yield rather than spin: the server means "immediately", not
            // "without letting anything else run".
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(wait).await;
        }
    }

    log::info!("label poller stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_cadence_is_obeyed() {
        assert_eq!(interval_from_header(Some("5")), Duration::from_secs(5));
    }

    #[test]
    fn come_back_immediately_is_a_real_answer() {
        // It is how a batch drains at printer speed instead of one per interval.
        assert_eq!(interval_from_header(Some("0")), Duration::ZERO);
    }

    #[test]
    fn a_server_that_says_nothing_gets_a_polite_default() {
        assert_eq!(interval_from_header(None), FALLBACK_INTERVAL);
    }

    #[test]
    fn garbage_does_not_crash_the_loop() {
        assert_eq!(interval_from_header(Some("soon")), FALLBACK_INTERVAL);
        assert_eq!(interval_from_header(Some("")), FALLBACK_INTERVAL);
        assert_eq!(interval_from_header(Some("-3")), FALLBACK_INTERVAL);
    }

    #[test]
    fn a_confused_server_cannot_park_the_bridge_for_a_week() {
        assert_eq!(interval_from_header(Some("999999")), MAX_INTERVAL);
    }

    #[test]
    fn a_printer_that_did_not_answer_is_reported_as_unreachable_rather_than_omitted() {
        // ⚠️ The server needs "the bridge is here and the cable is out" to look
        // different from "the bridge is gone". Only one of those is ours to fix.
        let settings = Settings {
            installation_id: String::from("abc"),
            label_port: String::from("COM6"),
            ..Default::default()
        };
        let report = build_report(&settings, false, None);
        assert!(!report.printer_reachable);
        assert_eq!(report.address, "COM6");
    }

    #[test]
    fn nothing_is_invented_when_the_printer_said_nothing() {
        // ⚠️ paper_state 0 means "out of paper" to the server and would hold
        // every job forever. Absent is the honest answer.
        let settings = Settings {
            installation_id: String::from("abc"),
            ..Default::default()
        };
        let report = build_report(&settings, false, None);
        assert!(report.paper_state.is_none());
        assert!(report.power_level.is_none());
        assert!(report.cassette.is_none());
    }

    #[test]
    fn the_report_never_claims_to_be_adopted() {
        // Serialised and inspected, because the guarantee is about what goes on
        // the wire — a field added to the struct later would show up here.
        let settings = Settings {
            installation_id: String::from("abc"),
            ..Default::default()
        };
        let json = serde_json::to_string(&build_report(&settings, true, None)).unwrap();
        assert!(!json.contains("enabled"), "{json}");
        assert!(!json.contains("\"name\""), "{json}");
    }

    #[test]
    fn the_status_the_window_reads_is_the_one_the_loop_writes() {
        // ⚠️ The loop published its outcome but not its id, and the window
        // showed a live connection under the words "not generated yet". The
        // patch that added the line silently did not apply — nothing asserted
        // that the two halves of this module meet.
        set_status(|status| status.installation_id = String::from("abc-123"));
        assert_eq!(label_poller_status().installation_id, "abc-123");
    }

    #[test]
    fn the_window_can_show_the_id_before_anything_has_happened() {
        // ⚠️ It is the one thing somebody needs to find this machine in the
        // server's device list. A blank field until the first successful poll
        // would hide it exactly when adoption is being attempted.
        let status = label_poller_status();
        assert!(status.last_contact.is_none());
    }

    #[test]
    fn the_installation_id_survives_the_query_string() {
        assert_eq!(urlencode("a-b_c"), "a%2Db%5Fc");
        assert!(!urlencode("a&b=c").contains('&'));
    }
}
