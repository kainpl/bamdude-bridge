//! Bench tool for talking to a Niimbot by hand.
//!
//! Not part of the app and not shipped — it exists so the protocol port can be
//! exercised against real hardware without the poller, the server, or a job
//! queue in the way. When a label comes out wrong, this is what narrows it down
//! to the printer, the flow, or the raster.
//!
//! It reads the printer and draws the pattern through the **same library code
//! the settings window uses**, so what it shows here is what the app shows
//! there. Two readers of one device that disagree is a bug that only surfaces
//! when they disagree.
//!
//! ```text
//! cargo run --example niimbot_probe -- ports
//! cargo run --example niimbot_probe -- probe COM6
//! cargo run --example niimbot_probe -- render out.png [WxL]
//! cargo run --example niimbot_probe -- print COM6 [WxL] [density]
//! ```
//!
//! ⚠️ `print` feeds paper. Everything else is read-only.
//!
//! ⚠️ The vendor's own app holds the serial port exclusively. While it is
//! running, opening the port fails with "Access is denied" — close it first.

use std::time::Duration;

use bamdude_bridge_lib::label::encoder::{encode_image, EncodedImage};
use bamdude_bridge_lib::label::models::{by_id, ModelInfo};
use bamdude_bridge_lib::label::packet::cmd;
use bamdude_bridge_lib::label::serial::{list_ports, SerialTransport};
use bamdude_bridge_lib::label::status::{read_snapshot, PrinterSnapshot};
use bamdude_bridge_lib::label::task::{print_b1, select_task, PrintOptions};
use bamdude_bridge_lib::label::testpage::test_pattern;

/// Short enough that it cannot run onto the next label whatever is loaded, for
/// when the cassette size is not given.
const DEFAULT_LENGTH_MM: f32 = 20.0;

type Failure = Box<dyn std::error::Error>;

/// Label size in millimetres: across the printhead, then along the feed.
#[derive(Debug, Clone, Copy)]
struct LabelSize {
    across_mm: f32,
    along_mm: f32,
}

/// `40x20` — the way the cassette is labelled.
///
/// ⚠️ A label is **not** the printhead. Printing the full head width onto a
/// narrower label puts the edges of the image past the edges of the paper,
/// where they are simply not there — which reads as "the corners are missing"
/// rather than as "the image is too wide".
fn parse_size(text: &str) -> Option<LabelSize> {
    let (w, l) = text.split_once(['x', 'X', '*', '×'])?;
    Some(LabelSize {
        across_mm: w.trim().parse().ok()?,
        along_mm: l.trim().parse().ok()?,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let size = |i: usize| args.get(i).and_then(|v| parse_size(v));
    let density = |i: usize| args.get(i).and_then(|v| v.parse::<u8>().ok());

    let result: Result<(), Failure> = match command {
        "ports" => {
            show_ports();
            Ok(())
        }
        "render" => render_to(
            args.get(1).map(String::as_str).unwrap_or("probe.png"),
            size(2),
        ),
        "probe" => match args.get(1) {
            Some(port) => probe(port).await,
            None => Err("probe needs a port, e.g. `probe COM6`".into()),
        },
        "print" => match args.get(1) {
            Some(port) => print(port, size(2), density(3)).await,
            None => Err("print needs a port, e.g. `print COM6`".into()),
        },
        _ => {
            eprintln!("{USAGE}");
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("\n  failed: {e}");
        std::process::exit(1);
    }
}

const USAGE: &str = "\
niimbot_probe — bench tool for the label printer port

  ports                     list serial ports, most likely printer first
  render <file.png> [WxL]   write the test image without touching a printer
  probe <PORT>              ask the printer what it is and what is loaded
  print <PORT> [WxL] [1-5]  print the test image (this feeds paper)

WxL is the cassette size in millimetres, e.g. 40x20 — across the head first.
The vendor app holds the port exclusively — close it before using these.
";

fn show_ports() {
    let ports = list_ports();
    if ports.is_empty() {
        println!("no serial ports at all — is the printer plugged in and switched on?");
        return;
    }
    println!("{:<10} {:<6} DESCRIPTION", "PORT", "BUS");
    for p in ports {
        println!(
            "{:<10} {:<6} {}",
            p.name,
            if p.usb { "usb" } else { "other" },
            p.description
        );
    }
    println!("\nUSB ports are listed first; Bluetooth ones answer nothing on this path.");
}

// ── the image ───────────────────────────────────────────────────────────────

fn encode_for(
    model: &ModelInfo,
    size: Option<LabelSize>,
) -> Result<(EncodedImage, u32, u32), Failure> {
    let dpmm = model.dots_per_mm();
    let across = match size {
        Some(s) => (s.across_mm * dpmm).round() as u32,
        None => model.printhead_pixels as u32,
    };
    let along = match size {
        Some(s) => (s.along_mm * dpmm).round() as u32,
        None => (DEFAULT_LENGTH_MM * dpmm).round() as u32,
    };
    if across > model.printhead_pixels as u32 {
        return Err(format!(
            "{:.0} mm is wider than this printhead can reach ({:.0} mm)",
            across as f32 / dpmm,
            model.max_width_mm()
        )
        .into());
    }
    let img = test_pattern(across, along, model.print_direction);
    let enc = encode_image(&img, model.printhead_pixels, model.print_direction)?;
    Ok((enc, across, along))
}

fn describe(enc: &EncodedImage, model: &ModelInfo) {
    let (mut empty, mut bitmap, mut indexed) = (0, 0, 0);
    for p in &enc.packets {
        match p.command {
            cmd::PRINT_EMPTY_ROW => empty += 1,
            cmd::PRINT_BITMAP_ROW => bitmap += 1,
            cmd::PRINT_BITMAP_ROW_INDEXED => indexed += 1,
            _ => {}
        }
    }
    println!(
        "encoded: {} cols × {} rows → {} packets ({bitmap} bitmap, {indexed} indexed, {empty} empty)",
        enc.cols,
        enc.rows,
        enc.packets.len()
    );
    if enc.cols > model.printhead_pixels {
        println!(
            "  ⚠️  {} columns exceeds the {}-pixel printhead — the label would be clipped",
            enc.cols, model.printhead_pixels
        );
    }
}

/// Draw the image into the terminal you are already standing in.
///
/// A file is no use on a bench: comparing a label in your hand against one
/// means finding a viewer, and the thing being checked — which corner the wedge
/// is in — survives being squashed to text perfectly well.
fn ascii_preview(img: &image::DynamicImage, width: u32) {
    use image::GenericImageView;
    let (w, h) = img.dimensions();
    let cols = width.min(w);
    // Terminal cells are about twice as tall as they are wide.
    let rows = ((h as f32 / w as f32) * cols as f32 / 2.0).round().max(1.0) as u32;
    let gray = img.to_luma8();

    println!("┌{}┐", "─".repeat(cols as usize));
    for r in 0..rows {
        print!("│");
        for c in 0..cols {
            let x0 = c * w / cols;
            let x1 = ((c + 1) * w / cols).max(x0 + 1).min(w);
            let y0 = r * h / rows;
            let y1 = ((r + 1) * h / rows).max(y0 + 1).min(h);
            let (mut dark, mut total) = (0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    total += 1;
                    if gray.get_pixel(x, y).0[0] != 255 {
                        dark += 1;
                    }
                }
            }
            let ratio = if total == 0 {
                0.0
            } else {
                dark as f32 / total as f32
            };
            print!(
                "{}",
                match ratio {
                    0.0 => ' ',
                    r if r < 0.25 => '░',
                    r if r < 0.6 => '▒',
                    _ => '█',
                }
            );
        }
        println!("│");
    }
    println!("└{}┘", "─".repeat(cols as usize));
}

fn render_to(path: &str, size: Option<LabelSize>) -> Result<(), Failure> {
    // No printer to ask, so assume the one on the bench.
    let model = by_id(4096).expect("B1");
    let (enc, across, along) = encode_for(&model, size)?;
    let img = test_pattern(across, along, model.print_direction);
    img.save(path)?;
    println!(
        "wrote {path}  — {} at {} dpi, {:.1} × {:.1} mm",
        model.name,
        model.dpi,
        across as f32 / model.dots_per_mm(),
        along as f32 / model.dots_per_mm()
    );
    describe(&enc, &model);
    ascii_preview(&img, 76);
    Ok(())
}

// ── talking to the printer ──────────────────────────────────────────────────

fn show_snapshot(s: &PrinterSnapshot) {
    match (s.model_id, s.model_name) {
        (Some(id), Some(name)) => println!(
            "model              {name} (id {id}) · {} dpi · head {} px · density {}-{}",
            s.dpi.unwrap_or(0),
            s.printhead_pixels.unwrap_or(0),
            s.density_min.unwrap_or(0),
            s.density_max.unwrap_or(0)
        ),
        (Some(id), None) => println!("model              id {id} — no ported print flow"),
        _ => println!("model              no answer"),
    }
    if let Some(v) = &s.firmware {
        println!("firmware           {v}");
    }
    if let Some(v) = &s.serial {
        println!("serial             {v}");
    }
    if let Some(h) = &s.heartbeat {
        println!(
            "state              lid closed {:?} · charge {:?} · paper in {:?} · tag read {:?}",
            h.lid_closed, h.charge_level, h.paper_inserted, h.tag_read
        );
    }
    match &s.cassette {
        Some(c) => {
            println!(
                "cassette           barcode {} · {}",
                c.barcode, c.consumable_name
            );
            println!(
                "                   {} of {} used · capacity {:?} · serial {}",
                c.used, c.total, c.capacity, c.serial
            );
            println!("\n⚠️ The tag carries no size in millimetres — that comes from the barcode.");
        }
        None => println!("cassette           no tag present"),
    }
}

async fn probe(port: &str) -> Result<(), Failure> {
    println!("opening {port} …");
    let mut t = SerialTransport::open(port)?;
    println!("open.\n");
    let snapshot = read_snapshot(&mut t).await?;
    show_snapshot(&snapshot);
    println!("\nNothing above fed any paper.");
    Ok(())
}

async fn print(port: &str, size: Option<LabelSize>, density: Option<u8>) -> Result<(), Failure> {
    println!("opening {port} …");
    let mut t = SerialTransport::open(port)?;

    let snapshot = read_snapshot(&mut t).await?;
    let model_id = snapshot
        .model_id
        .ok_or("the printer did not say what model it is")?;
    let model = by_id(model_id).ok_or(format!("model id {model_id} has no ported print flow"))?;
    select_task(model_id, None).ok_or(format!("no print flow for model id {model_id}"))?;

    let label_type = snapshot
        .cassette
        .as_ref()
        .map(|c| c.consumable_type)
        .unwrap_or(1);

    let (enc, across, along) = encode_for(&model, size)?;
    println!(
        "{} · {} dpi · head {} px · direction {:?} · label type {}",
        model.name, model.dpi, model.printhead_pixels, model.print_direction, label_type
    );
    println!(
        "image {across} × {along} px  ({:.1} × {:.1} mm)",
        across as f32 / model.dots_per_mm(),
        along as f32 / model.dots_per_mm()
    );
    if (across as u16) < model.printhead_pixels {
        println!(
            "  note: {} of the head's {} columns are used — the head aligns to its edge",
            across, model.printhead_pixels
        );
    }
    describe(&enc, &model);
    ascii_preview(&test_pattern(across, along, model.print_direction), 76);

    let requested = density.unwrap_or(model.density_default);
    let clamped = model.clamp_density(requested);
    if clamped != requested {
        println!(
            "density {requested} is outside this model's {}-{} — using {clamped}",
            model.density_min, model.density_max
        );
    }

    let opts = PrintOptions {
        density: clamped,
        label_type,
        status_timeout: Duration::from_secs(30),
        ..Default::default()
    };

    println!("\nprinting at density {clamped} …");
    let started = std::time::Instant::now();
    print_b1(&mut t, &enc, &opts).await?;
    println!("done in {:.1}s", started.elapsed().as_secs_f32());
    Ok(())
}
