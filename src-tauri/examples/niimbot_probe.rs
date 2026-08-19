//! Bench tool for talking to a Niimbot by hand.
//!
//! Not part of the app and not shipped — it exists so the protocol port can be
//! exercised against real hardware without the poller, the server, or a job
//! queue in the way. When a label comes out wrong, this is what narrows it down
//! to the printer, the flow, or the raster.
//!
//! ```text
//! cargo run --example niimbot_probe -- ports
//! cargo run --example niimbot_probe -- probe COM6
//! cargo run --example niimbot_probe -- render out.png [length_mm]
//! cargo run --example niimbot_probe -- print COM6 [length_mm] [density]
//! ```
//!
//! ⚠️ `print` feeds paper. Everything else is read-only.
//!
//! ⚠️ The vendor's own app holds the serial port exclusively. While it is
//! running, opening the port fails with "Access is denied" — close it first.

use std::time::Duration;

use bamdude_bridge_lib::label::encoder::{encode_image, EncodedImage, PrintDirection};
use bamdude_bridge_lib::label::models::{by_id, ModelInfo};
use bamdude_bridge_lib::label::packet::{cmd, Packet};
use bamdude_bridge_lib::label::serial::{list_ports, SerialTransport};
use bamdude_bridge_lib::label::task::{parse_status, print_b1, select_task, PrintOptions};
use bamdude_bridge_lib::label::transport::Transport;

/// Deliberately shorter than any cassette we might meet. On a gap-sensed roll
/// the printer feeds to the next gap regardless, so a short image wastes
/// nothing and cannot run over onto the following label — which is the right
/// default when the loaded size is unknown.
const DEFAULT_LENGTH_MM: f32 = 20.0;

type Failure = Box<dyn std::error::Error>;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    let mm = |i: usize| args.get(i).and_then(|v| v.parse::<f32>().ok());
    let density = |i: usize| args.get(i).and_then(|v| v.parse::<u8>().ok());

    let result: Result<(), Failure> = match command {
        "ports" => {
            show_ports();
            Ok(())
        }
        "render" => render_to(
            args.get(1).map(String::as_str).unwrap_or("probe.png"),
            mm(2).unwrap_or(DEFAULT_LENGTH_MM),
        ),
        "probe" => match args.get(1) {
            Some(port) => probe(port).await,
            None => Err("probe needs a port, e.g. `probe COM6`".into()),
        },
        "print" => match args.get(1) {
            Some(port) => print(port, mm(2).unwrap_or(DEFAULT_LENGTH_MM), density(3)).await,
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
  render <file.png> [mm]    write the test image without touching a printer
  probe <PORT>              ask the printer what it is and what is loaded
  print <PORT> [mm] [1-5]   print the test image (this feeds paper)

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

// ── the test pattern ────────────────────────────────────────────────────────

/// `across` is the printhead direction, `along` is the feed direction. Which of
/// those is the image's width depends on the model, so the caller passes both
/// and this decides nothing.
fn test_image(across: u32, along: u32, direction: PrintDirection) -> image::DynamicImage {
    let (w, h) = match direction {
        // cols come from the width
        PrintDirection::Top => (across, along),
        // cols come from the height — the encoder rotates
        PrintDirection::Left => (along, across),
    };

    let mut img = image::GrayImage::from_pixel(w, h, image::Luma([255u8]));
    let black = image::Luma([0u8]);
    let mut dot = |x: u32, y: u32| {
        if x < w && y < h {
            img.put_pixel(x, y, black);
        }
    };

    // Border: a missing edge means the size is wrong.
    for x in 0..w {
        for t in 0..3 {
            dot(x, t);
            dot(x, h - 1 - t);
        }
    }
    for y in 0..h {
        for t in 0..3 {
            dot(t, y);
            dot(w - 1 - t, y);
        }
    }

    // A wedge in ONE corner — the only mark that says which way up it came out.
    let wedge = (w.min(h) / 4).max(8);
    for y in 0..wedge {
        for x in 0..(wedge - y) {
            dot(6 + x, 6 + y);
        }
    }

    // A diagonal: bit-order mistakes bend or stagger it.
    for i in 0..w.min(h) {
        dot(i, i);
        dot(i + 1, i);
    }

    // A comb of one-pixel lines: the first thing to smear at too high a density.
    let comb_x = w / 2;
    for i in 0..12u32 {
        let x = comb_x + i * 4;
        for y in (h / 4)..(h * 3 / 4) {
            dot(x, y);
        }
    }

    // An isolated dot, which forces at least one indexed row.
    dot(w * 3 / 4, h / 2);

    image::DynamicImage::ImageLuma8(img)
}

fn encode_for(model: &ModelInfo, length_mm: f32) -> Result<(EncodedImage, u32, u32), Failure> {
    let across = model.printhead_pixels as u32;
    let along = (length_mm * model.dots_per_mm()).round() as u32;
    let img = test_image(across, along, model.print_direction);
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

fn render_to(path: &str, length_mm: f32) -> Result<(), Failure> {
    // No printer to ask, so assume the one on the bench.
    let model = by_id(4096).expect("B1");
    let (enc, across, along) = encode_for(&model, length_mm)?;
    let img = test_image(across, along, model.print_direction);
    img.save(path)?;
    println!(
        "wrote {path}  — {} at {} dpi, {:.0} mm across × {length_mm:.0} mm along",
        model.name,
        model.dpi,
        model.max_width_mm()
    );
    describe(&enc, &model);
    Ok(())
}

// ── talking to the printer ──────────────────────────────────────────────────

async fn ask(t: &mut SerialTransport, packet: Packet) -> Result<Option<Packet>, Failure> {
    t.discard_pending().await?;
    t.write(&packet.to_bytes()?).await?;
    Ok(t.read_packet(Duration::from_millis(1500)).await.ok())
}

fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

async fn read_model_id(t: &mut SerialTransport) -> Result<Option<u16>, Failure> {
    // PrinterInfoType::PrinterModelId = 8.
    let reply = ask(t, Packet::new(cmd::PRINTER_INFO, vec![8])).await?;
    Ok(reply.and_then(|p| (p.data.len() >= 2).then(|| u16::from_be_bytes([p.data[0], p.data[1]]))))
}

/// Cassette tag, laid out exactly as the reference reads it.
struct Rfid {
    uuid: String,
    barcode: String,
    serial: String,
    all_paper: i16,
    used_paper: i16,
    consumables_type: u8,
    capacity: Option<i16>,
}

fn parse_rfid(data: &[u8]) -> Option<Rfid> {
    if data.len() <= 1 {
        return None; // no tag present
    }
    let mut at = 0usize;
    let take = |at: &mut usize, n: usize| -> Option<Vec<u8>> {
        let end = at.checked_add(n)?;
        let slice = data.get(*at..end)?.to_vec();
        *at = end;
        Some(slice)
    };
    let uuid = hex(&take(&mut at, 8)?).replace(' ', "");

    let vstring = |at: &mut usize| -> Option<String> {
        let len = *data.get(*at)? as usize;
        *at += 1;
        let bytes = data.get(*at..*at + len)?.to_vec();
        *at += len;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    let barcode = vstring(&mut at)?;
    let serial = vstring(&mut at)?;

    let i16_at = |at: &mut usize| -> Option<i16> {
        let b = data.get(*at..*at + 2)?;
        *at += 2;
        Some(i16::from_be_bytes([b[0], b[1]]))
    };
    let all_paper = i16_at(&mut at)?;
    let used_paper = i16_at(&mut at)?;
    let consumables_type = *data.get(at)?;
    at += 1;
    let capacity = i16_at(&mut at);

    Some(Rfid {
        uuid,
        barcode,
        serial,
        all_paper,
        used_paper,
        consumables_type,
        capacity,
    })
}

fn label_type_name(t: u8) -> &'static str {
    match t {
        1 => "WithGaps",
        2 => "Black",
        3 => "Continuous",
        4 => "Perforated",
        5 => "Transparent",
        6 => "PvcTag",
        10 => "BlackMarkGap",
        11 => "HeatShrinkTube",
        _ => "unknown",
    }
}

async fn probe(port: &str) -> Result<(), Failure> {
    println!("opening {port} …");
    let mut t = SerialTransport::open(port)?;
    println!("open.\n");

    match read_model_id(&mut t).await? {
        Some(id) => match by_id(id) {
            Some(m) => println!(
                "model id {id} → {} · {} dpi ({:.1} dots/mm) · head {} px ({:.0} mm) · direction {:?} · density {}-{}",
                m.name, m.dpi, m.dots_per_mm(), m.printhead_pixels, m.max_width_mm(),
                m.print_direction, m.density_min, m.density_max
            ),
            None => println!("model id {id} → not a model with a ported print flow"),
        },
        None => println!("model id → no answer"),
    }

    for (ty, label) in [(9u8, "software version"), (11, "serial number")] {
        if let Some(p) = ask(&mut t, Packet::new(cmd::PRINTER_INFO, vec![ty])).await? {
            let text = String::from_utf8_lossy(&p.data);
            let printable = text.chars().all(|c| c.is_ascii_graphic());
            println!(
                "{label:<18} {}",
                if printable {
                    text.into_owned()
                } else {
                    hex(&p.data)
                }
            );
        }
    }

    if let Some(p) = ask(&mut t, Packet::new(cmd::HEARTBEAT, vec![1])).await? {
        // The 13-byte form is the B1's; field offsets follow the payload
        // length, not the model, exactly as the reference does it.
        if p.data.len() == 13 {
            println!(
                "heartbeat          lid closed {} · charge {} · paper in {} · tag read {}",
                p.data[9] == 0,
                p.data[10],
                p.data[11] == 0,
                p.data[12] != 0
            );
        } else {
            println!(
                "heartbeat          {} bytes: {}",
                p.data.len(),
                hex(&p.data)
            );
        }
    }

    if let Some(p) = ask(&mut t, Packet::new(cmd::RFID_INFO, vec![1])).await? {
        match parse_rfid(&p.data) {
            Some(r) => {
                println!(
                    "cassette           barcode {} · serial {}",
                    r.barcode, r.serial
                );
                println!(
                    "                   {} of {} used · capacity {} · type {} ({})",
                    r.used_paper,
                    r.all_paper,
                    r.capacity
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "—".into()),
                    r.consumables_type,
                    label_type_name(r.consumables_type)
                );
                println!("                   uuid {}", r.uuid);
                println!(
                    "\n⚠️ The tag carries no size in millimetres — that comes from the barcode."
                );
            }
            None => println!("cassette           no tag present"),
        }
    }

    if let Some(p) = ask(&mut t, Packet::new(cmd::PRINT_STATUS, vec![1])).await? {
        match parse_status(&p.data) {
            Ok(s) => println!(
                "print status       page {} · print {}% · feed {}%",
                s.page, s.print_progress, s.feed_progress
            ),
            Err(e) => println!("print status       unparsed: {e}"),
        }
    }

    println!("\nNothing above fed any paper.");
    Ok(())
}

async fn print(port: &str, length_mm: f32, density: Option<u8>) -> Result<(), Failure> {
    println!("opening {port} …");
    let mut t = SerialTransport::open(port)?;

    let model_id = read_model_id(&mut t)
        .await?
        .ok_or("the printer did not say what model it is")?;
    let model = by_id(model_id).ok_or(format!("model id {model_id} has no ported print flow"))?;
    select_task(model_id, None).ok_or(format!("no print flow for model id {model_id}"))?;

    // Take the loaded consumable's type from the tag rather than assuming a
    // gapped roll; a continuous roll told to look for gaps feeds and feeds.
    let label_type = match ask(&mut t, Packet::new(cmd::RFID_INFO, vec![1])).await? {
        Some(p) => parse_rfid(&p.data).map(|r| r.consumables_type).unwrap_or(1),
        None => 1,
    };

    let (enc, across, along) = encode_for(&model, length_mm)?;
    println!(
        "{} · {} dpi · head {} px · direction {:?} · label type {} ({})",
        model.name,
        model.dpi,
        model.printhead_pixels,
        model.print_direction,
        label_type,
        label_type_name(label_type)
    );
    println!("image {across} × {along} px  ({length_mm:.0} mm along the feed)");
    describe(&enc, &model);

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
