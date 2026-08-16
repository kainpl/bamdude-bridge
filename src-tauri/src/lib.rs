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
//! ⚠️ **Both paths raise the window**, because a person who just clicked
//! "send" needs to learn whether it worked, and a hidden window cannot tell
//! them. The better answer for the handover path is a tray notification — it
//! is not built yet, and until it is, a window is better than silence.
//!
//! ⚠️ **Closing the window does not close the app.** It hides, and the app
//! lives on in the tray. That is not only a convenience: staying resident is
//! what lets a protocol launch reach an already-running instance through
//! single-instance instead of paying a cold start per plate. The only exit is
//! Quit in the tray menu — see [`tray`].

pub mod config;
pub mod farm_client_url;
#[cfg(windows)]
pub mod registry;
pub mod tray;
pub mod upload;

use tauri::{AppHandle, Emitter, Manager};

/// Event the frontend listens on to report the outcome of a handover.
const EVENT_HANDOVER: &str = "handover";

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
        // A protocol launch is a NEW process. Without single-instance, every
        // plate sent from the slicer would open another copy of the app; with
        // it, the already-running instance gets the argv and the second
        // process exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
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
        Some(url) => accept_handover(app, url),
        None => show_settings(app),
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

    show_settings(app);
    report(
        app,
        HandoverStatus::Started {
            name: request.name.clone(),
        },
    );

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let status = match upload::send_to_library(&app, &request).await {
            Ok(()) => HandoverStatus::Succeeded { name: request.name },
            Err(error) => HandoverStatus::Failed {
                name: request.name,
                error: error.to_string(),
            },
        };
        report(&app, status);
    });
}

fn report(app: &AppHandle, status: HandoverStatus) {
    let _ = app.emit(EVENT_HANDOVER, status);
}
