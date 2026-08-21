//! What the settings window can ask the label role to do.
//!
//! Four things, and no more: list the ports, read the printer, print a test
//! page, and nothing that decides anything. Density, label size and layout are
//! the server's; a control for them here would be a second answer to a question
//! that already has one.

use std::time::Duration;

use serde::Serialize;

use super::encoder::encode_image;
use super::models::by_id;
use super::serial::{list_ports, PortInfo, SerialTransport};
use super::status::{read_snapshot, PrinterSnapshot};
use super::task::{print_b1, select_task, PrintOptions};
use super::testpage::test_pattern;

/// Long enough for a short label, short enough that a wedged printer does not
/// hold the window forever.
const TEST_PRINT_TIMEOUT: Duration = Duration::from_secs(30);

/// A test page proves the printer is alive. It deliberately does **not** need
/// to know the loaded label's size: that fact belongs to the server's catalogue,
/// and inventing one here would put a second size in the product. Short enough
/// that it cannot run onto the next label whatever is loaded.
const TEST_PRINT_LENGTH_MM: f32 = 15.0;

#[derive(Debug, Serialize)]
pub struct PortsResult {
    pub ports: Vec<PortInfo>,
    /// Said plainly because it is the single most common way this fails: the
    /// vendor's app holds the port open and nothing else can have it.
    pub note: &'static str,
}

#[tauri::command]
pub fn label_list_ports() -> PortsResult {
    PortsResult {
        ports: list_ports(),
        note: "Close the NIIMBOT desktop app before connecting — it holds the port exclusively.",
    }
}

#[tauri::command]
pub async fn label_read_status(port: String) -> Result<PrinterSnapshot, String> {
    let mut transport = SerialTransport::open(&port).map_err(explain)?;
    read_snapshot(&mut transport).await.map_err(explain)
}

#[tauri::command]
pub async fn label_test_print(port: String) -> Result<String, String> {
    let mut transport = SerialTransport::open(&port).map_err(explain)?;

    let snapshot = read_snapshot(&mut transport).await.map_err(explain)?;
    let model_id = snapshot
        .model_id
        .ok_or_else(|| "The printer did not say what model it is.".to_string())?;
    let model = by_id(model_id)
        .ok_or_else(|| format!("Model id {model_id} is not one this app can print on yet."))?;
    select_task(model_id, None)
        .ok_or_else(|| format!("No print flow is ported for model id {model_id}."))?;

    let across = model.printhead_pixels as u32;
    let along = (TEST_PRINT_LENGTH_MM * model.dots_per_mm()).round() as u32;
    let image = test_pattern(across, along, model.print_direction);
    let encoded = encode_image(&image, model.printhead_pixels, model.print_direction)
        .map_err(|e| e.to_string())?;

    // Take the loaded consumable's type from the tag rather than assuming a
    // gapped roll: a continuous roll told to look for gaps just feeds.
    let label_type = snapshot
        .cassette
        .as_ref()
        .map(|c| c.consumable_type)
        .unwrap_or(1);

    let options = PrintOptions {
        density: model.density_default,
        label_type,
        status_timeout: TEST_PRINT_TIMEOUT,
        ..Default::default()
    };

    print_b1(&mut transport, &encoded, &options)
        .await
        .map_err(|e| e.to_string())?;

    Ok(format!(
        "Printed a {:.0} mm test label on the {}.",
        TEST_PRINT_LENGTH_MM, model.name
    ))
}

/// Turn the one failure everybody meets into the sentence that fixes it.
fn explain(error: impl std::fmt::Display) -> String {
    let text = error.to_string();
    if text.contains("Access is denied") {
        return format!(
            "{text}\n\nSomething else already has this port — usually the NIIMBOT desktop app. \
             Close it and try again."
        );
    }
    text
}
