//! BamDude Bridge.
//!
//! Two ways this binary starts, and they want opposite things:
//!
//! - **A person opens it** — show the settings window, let them point it at a
//!   server and register the protocol.
//! - **The slicer hands it a file** — Windows starts a fresh process with a
//!   `bambu-farm-client://…` URL in argv. Do the work and say what happened.
//!
//! Everything below exists to keep those two paths from contaminating each
//! other.
//!
//! ⚠️ **Only a plain launch raises the window.** A handover stays quiet unless
//! it fails: the slicer is in front of the user and jumping over it is rude,
//! and the confirmation they actually want — the file appearing in the library
//! — arrives on its own. See [`report`] for where that is decided.
//!
//! ⚠️ **Closing the window does not close the app.** It hides, and the app
//! lives on in the tray. That is not only a convenience: staying resident is
//! what lets a protocol launch reach an already-running instance through
//! single-instance instead of paying a cold start per plate. The only exit is
//! Quit in the tray menu — see [`tray`].

pub mod config;
pub mod farm_client_url;
pub mod label;
#[cfg(windows)]
pub mod registry;
pub mod tray;
pub mod update;
pub mod upload;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};

/// Lets the label poller be stopped.
///
/// ⚠️ A flag rather than aborting the task. Nothing sets it today — the loop
/// lives as long as the app does — but killing the task mid-print would leave a
/// job claimed on the server and a printer half-fed, so the way out has to be
/// one the loop notices between passes.
///
/// Kept in Tauri's state rather than a `static`: a `LazyLock` would raise this
/// crate's minimum Rust past the 1.77 it builds on today.
pub struct LabelPollerStop(pub Arc<AtomicBool>);

/// Event the frontend listens on to report the outcome of a handover.
const EVENT_HANDOVER: &str = "handover";

/// Argument Windows autostart passes so the app comes up in the tray without a
/// window. Defined here rather than beside the registry code because the
/// dispatch that honours it is not Windows-specific.
pub const MINIMIZED_ARG: &str = "--minimized";

/// The last thing that happened to a handover, kept so the window can ask
/// after the fact.
///
/// ⚠️ **An event alone loses the cold-start case.** A handover that starts the
/// process emits its result from `setup()`, which runs before the webview has
/// mounted and subscribed — so the report goes nowhere and the user sees an
/// empty window and assumes it worked. That is precisely how the first real
/// failure looked like a success. Whether the event beats the mount is a race,
/// and a race is not a reporting mechanism.
#[derive(Default)]
pub struct LastHandover(Mutex<Option<HandoverStatus>>);

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum HandoverStatus {
    Started { name: String },
    Succeeded { name: String },
    Failed { name: String, error: String },
}

pub fn run() {
    // Before Tauri gets a chance to build a window: this launch may be the
    // elevated helper, whose whole job is one registry write. It must not
    // start a GUI — the user is looking at the un-elevated instance that
    // raised the prompt.
    #[cfg(windows)]
    if std::env::args().any(|arg| arg == registry::INSTALL_MARKER_ARG) {
        std::process::exit(match registry::install_marker() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        });
    }

    tauri::Builder::default()
        .manage(LastHandover::default())
        // Registered first so everything after it can be logged. The file
        // target is the point: a handover happens unattended, and a release
        // build has no console to print to.
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // ⚠️ `target` ADDS to the plugin's defaults rather than
                // replacing them, so without this the app wrote every line
                // twice — once to "BamDude Bridge.log" and once to the file
                // named below. Two identical logs is worse than one badly
                // named: whoever is debugging reads whichever they find and
                // cannot tell it is a duplicate.
                .clear_targets()
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some(String::from("bridge")),
                    },
                ))
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Stdout,
                ))
                .build(),
        )
        // A protocol launch is a NEW process. Without single-instance, every
        // plate sent from the slicer would open another copy of the app; with
        // it, the already-running instance gets the argv and the second
        // process exits.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            log::info!("second instance forwarded {} argument(s)", argv.len());
            dispatch_argv(app, &argv);
        }))
        // The registry half exists only on Windows — everything it touches is
        // a Windows concept — so the command list is spelled twice rather than
        // gated inside the macro, which cannot take a `cfg` per entry.
        .invoke_handler({
            #[cfg(windows)]
            {
                tauri::generate_handler![
                    config::load_settings,
                    config::save_settings,
                    config::test_connection,
                    label::commands::label_list_ports,
                    label::commands::label_read_status,
                    label::commands::label_test_print,
                    label::poller::label_poller_status,
                    update::check_for_update,
                    update::install_update,
                    update::last_update_check,
                    last_handover,
                    app_version,
                    registry::registration_status,
                    registry::register_receiver,
                    registry::unregister_receiver,
                ]
            }
            #[cfg(not(windows))]
            {
                tauri::generate_handler![
                    config::load_settings,
                    config::save_settings,
                    config::test_connection,
                    label::commands::label_list_ports,
                    label::commands::label_read_status,
                    label::commands::label_test_print,
                    label::poller::label_poller_status,
                    update::check_for_update,
                    update::install_update,
                    update::last_update_check,
                    last_handover,
                    app_version,
                ]
            }
        })
        // Hide instead of destroying. Without `prevent_close` the window is
        // gone for good, and the next handover would have nowhere to report
        // its result — the app would still be running, silently.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            // Built before the dispatch below: if the tray cannot be created
            // the app has no way to be quit or reopened, so that must fail
            // loudly at startup rather than after a window is hidden.
            tray::build(app.handle())?;

            // Said once, loudly, at startup — the UI carries the same warning,
            // but a log line survives to be read afterwards by whoever is
            // wondering why plates stopped arriving.
            #[cfg(windows)]
            if registry::running_elevated() {
                log::warn!(
                    "running as administrator — BambuStudio cannot hand files to an elevated \
                     instance, so every send from the slicer will silently do nothing. Quit and \
                     start normally; registration elevates on its own when it needs to."
                );
            }

            // ⚠️ Started unconditionally, and it decides for itself each pass
            // whether the label role is on. Gating it here would mean somebody
            // who switches the role on has to restart the app before anything
            // happens — and this loop is the only thing that would need it.
            let stop = Arc::new(AtomicBool::new(false));
            app.manage(LabelPollerStop(stop.clone()));
            let poller_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                label::poller::run(poller_handle, stop).await;
            });

            update::start_periodic_checks(app.handle().clone());

            // The first launch does not go through the single-instance hook,
            // so the cold-start path needs the same dispatch.
            let argv: Vec<String> = std::env::args().collect();
            dispatch_argv(app.handle(), &argv);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start BamDude Bridge");
}

/// Decides what this launch was for.
fn dispatch_argv(app: &AppHandle, argv: &[String]) {
    match argv.iter().find(|arg| farm_client_url::is_ours(arg)) {
        Some(url) => {
            log::info!("handover URL received ({} bytes)", url.len());
            accept_handover(app, url)
        }
        // Started by Windows at sign-in: be present, be invisible. Showing a
        // window here would put one in front of every user on every boot.
        None if argv.iter().any(|arg| arg == MINIMIZED_ARG) => {
            log::info!("autostart launch — staying in the tray");
        }
        None => {
            log::info!(
                "plain launch — {} argument(s), none of them ours",
                argv.len()
            );
            show_settings(app)
        }
    }
}

/// Brings the window back from wherever it went — hidden by a close, or never
/// shown because the config asks for `visible: false` so a handover launch
/// does not flash a window before it has anything to say.
pub(crate) fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Handles one "here is a sliced plate" URL from the slicer.
///
/// ⚠️ **The file is temporary and not ours.** BambuStudio exports it into its
/// own backup directory and never promised to keep it; it is cleaned up when
/// the slicer exits, and re-slicing the same plate overwrites it in place. So
/// the read happens now, on this launch, not on a queue we drain later.
fn accept_handover(app: &AppHandle, url: &str) {
    let request = match farm_client_url::parse(url) {
        Ok(request) => request,
        Err(error) => {
            // A URL we cannot read is worth surfacing rather than swallowing:
            // it means either a malformed launch or a change on Bambu's side,
            // and both are things somebody needs to see.
            log::error!("could not parse the handover URL: {error} — raw: {url}");
            report(
                app,
                HandoverStatus::Failed {
                    name: String::from("(unparsed)"),
                    error: error.to_string(),
                },
            );
            return;
        }
    };

    log::info!("handover: name={:?} path={:?}", request.name, request.path);
    report(
        app,
        HandoverStatus::Started {
            name: request.name.clone(),
        },
    );

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let status = match upload::send_to_library(&app, &request).await {
            Ok(()) => {
                log::info!("handover complete: {:?} is in the library", request.name);
                HandoverStatus::Succeeded { name: request.name }
            }
            Err(error) => {
                log::error!("handover failed for {:?}: {error}", request.name);
                HandoverStatus::Failed {
                    name: request.name,
                    error: error.to_string(),
                }
            }
        };
        report(&app, status);
    });
}

/// Records an outcome and decides how loudly to say it.
///
/// ⚠️ **Success is deliberately silent.** Raising the window on every plate
/// meant the app leapt in front of the slicer mid-workflow, which is exactly
/// what nobody asked for — and the confirmation the user actually wants shows
/// up on its own, because the file appears in the BamDude library live. A
/// failure is the opposite: rare, and useless if unseen, so it takes the
/// window.
fn report(app: &AppHandle, status: HandoverStatus) {
    // Stored first, then emitted: a listener that is already attached gets it
    // live, and one that attaches later can still ask.
    if let Some(state) = app.try_state::<LastHandover>() {
        if let Ok(mut slot) = state.0.lock() {
            *slot = Some(status.clone());
        }
    }

    // Passive signal — visible on hover, interrupts nobody, and works
    // everywhere. Kept even though a toast is nicer, because a toast can be
    // suppressed by Focus Assist or refused outright on a machine where this
    // app has no Start Menu identity, and then the tooltip is all there is.
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(match &status {
            HandoverStatus::Started { name } => format!("BamDude Bridge — sending {name}…"),
            HandoverStatus::Succeeded { name } => format!("BamDude Bridge — sent {name}"),
            HandoverStatus::Failed { name, .. } => format!("BamDude Bridge — {name} FAILED"),
        }));
    }

    match &status {
        // The one moment worth a pop-up: it happened, it is done, and the
        // window would have been an interruption.
        HandoverStatus::Succeeded { name } => {
            notify(
                app,
                "Sent to BamDude",
                &format!("{name} is in your library."),
            );
        }
        HandoverStatus::Failed { .. } => show_settings(app),
        HandoverStatus::Started { .. } => {}
    }

    let _ = app.emit(EVENT_HANDOVER, status);
}

/// Pops a Windows toast, and says in the log whether it managed to.
///
/// ⚠️ **A toast is best-effort and must stay that way.** Windows routes it
/// through an AppUserModelID that normally comes from a Start Menu shortcut —
/// which the installer creates and a portable copy does not — and Focus Assist
/// can swallow it regardless. So the outcome is logged rather than assumed:
/// "the notification did not appear" is otherwise indistinguishable from "the
/// upload never happened", which is the exact confusion this app already cost
/// somebody once. The tray tooltip carries the same news unconditionally.
///
/// ⚠️ **The AppUserModelID only works because registration declares it.**
///
/// Windows draws a toast only for an AUMID it knows, and handing it an unknown
/// one is the worst possible outcome: the call **succeeds** and nothing is ever
/// drawn. That is not hypothetical — it happened here, logging a cheerful
/// "handed to Windows" at an empty screen. What makes ours known is
/// `registry::install_toast_identity`, written during registration; a Start
/// Menu shortcut carrying the id would do the same job with far more work.
///
/// ⚠️ **Windows caches the name on the id's first use.** Send a toast under an
/// id before declaring it and the sender reads `top.bamdude.bridge` forever
/// after, whatever the registry says later. Nothing here may send a toast on a
/// path that registration has not already passed through — which holds today
/// only because a handover cannot happen before the protocol is registered.
#[cfg(windows)]
fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_winrt_notification::{Duration, Toast};

    // Windows offers exactly two lengths — 7 seconds and 25 — and no way to
    // ask for a number in between. Short is the default and goes past before
    // you have looked up from the slicer, which is the whole reason this
    // exists, so Long it is. (`Scenario::Reminder` would pin it on screen
    // until dismissed; that is right for an alarm and wrong for "your file
    // arrived", which nobody should have to dismiss once per plate.)
    let toast = Toast::new(&app.config().identifier)
        .title(title)
        .text1(body)
        .duration(Duration::Long);

    match toast.show() {
        // Info rather than debug: this is the line that answers "did the
        // notification fail, or did the upload?", and the log level is Info.
        Ok(()) => log::info!("toast handed to Windows: {title}"),
        Err(error) => log::warn!("could not show a toast ({error}) — tray tooltip still updated"),
    }
}

#[cfg(not(windows))]
fn notify(_app: &AppHandle, _title: &str, _body: &str) {}

/// What the window asks on mount, to catch a handover that finished before it
/// was listening.
#[tauri::command]
fn last_handover(state: tauri::State<'_, LastHandover>) -> Option<HandoverStatus> {
    state.0.lock().ok().and_then(|slot| slot.clone())
}

/// The build's own version, for the window to show.
///
/// Taken from the crate rather than passed in from the frontend, because
/// `package.json` is not what gets shipped — the binary is. `scripts/set_version.js`
/// keeps the two in step, and the release workflow refuses to build when the
/// tag disagrees, so a version shown here is one that was really built.
#[tauri::command]
fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
