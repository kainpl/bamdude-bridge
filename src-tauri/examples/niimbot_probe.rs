//! Bench tool for talking to a Niimbot by hand.
//!
//! Not part of the app and not shipped — it exists so the protocol port can be
//! exercised against real hardware without the poller, the server, or a job
//! queue in the way. When a label comes out wrong, this is what narrows it down
//! to the printer, the flow, or the raster.
//!
//! ```text
//! cargo run --example niimbot_probe -- ports
//! cargo run --example niimbot_probe -- probe COM3
//! cargo run --example niimbot_probe -- render out.png      # no printer needed
//! cargo run --example niimbot_probe -- print COM3 [density]
//! ```
//!
//! ⚠️ `print` feeds paper. Everything else is read-only.

use std::time::Duration;

use bamdude_bridge_lib::label::encoder::{encode_image, EncodedImage, PrintDirection};
use bamdude_bridge_lib::label::packet::{cmd, Packet};
use bamdude_bridge_lib::label::serial::{list_ports, SerialTransport};
use bamdude_bridge_lib::label::task::{parse_status, print_b1, select_task, PrintOptions};
use bamdude_bridge_lib::label::transport::Transport;

/// A 50 × 30 mm label at 8 dots/mm. The printhead runs across the 30 mm side,
/// so this is 240 columns — inside a B1's 384.
const LABEL_W_PX: u32 = 400;
const LABEL_H_PX: u32 = 240;
const PRINTHEAD_PX: u16 = 384;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");

    let result = match command {
        "ports" => {
            show_ports();
            Ok(())
        }
        "render" => render_to(args.get(1).map(String::as_str).unwrap_or("probe.png")),
        "probe" => match args.get(1) {
            Some(port) => probe(port).await,
            None => Err("probe needs a port, e.g. `probe COM3`".into()),
        },
        "print" => match args.get(1) {
            Some(port) => {
                let density = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(3);
                print(port, density).await
            }
            None => Err("print needs a port, e.g. `print COM3`".into()),
        },
        _ => {
            eprintln!("{}", USAGE);
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

  ports              list serial ports, most likely printer first
  render [file.png]  write the test image without touching a printer
  probe <PORT>       ask the printer what it is and how it feels
  print <PORT> [1-5] print the test image (this feeds paper)
";

type Failure = Box<dyn std::error::Error>;

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

/// A pattern chosen so a wrong rotation, a wrong bit order and a wrong width are
/// all visible at a glance rather than merely "odd".
fn test_image() -> image::DynamicImage {
    let mut img = image::GrayImage::from_pixel(LABEL_W_PX, LABEL_H_PX, image::Luma([255u8]));
    let black = image::Luma([0u8]);

    // Border: a missing edge means the size is wrong.
    for x in 0..LABEL_W_PX {
        for t in 0..3 {
            img.put_pixel(x, t, black);
            img.put_pixel(x, LABEL_H_PX - 1 - t, black);
        }
    }
    for y in 0..LABEL_H_PX {
        for t in 0..3 {
            img.put_pixel(t, y, black);
            img.put_pixel(LABEL_W_PX - 1 - t, y, black);
        }
    }

    // A solid wedge in ONE corner: the only mark that says which way up it came
    // out. Top-left in the source.
    for y in 10..60 {
        for x in 10..(10 + (60 - y)) {
            img.put_pixel(x, y, black);
        }
    }

    // A diagonal: bit-order mistakes bend or stagger it.
    for x in 0..LABEL_W_PX.min(LABEL_H_PX) {
        for t in 0..2 {
            img.put_pixel(x + t, x, black);
        }
    }

    // A comb of 1px lines: the first thing to smear if density is too high.
    for i in 0..20 {
        let x = 200 + i * 4;
        for y in 80..160 {
            img.put_pixel(x, y, black);
        }
    }

    // A single isolated dot, which forces at least one indexed row.
    img.put_pixel(350, 200, black);

    image::DynamicImage::ImageLuma8(img)
}

fn encoded() -> Result<EncodedImage, Failure> {
    Ok(encode_image(
        &test_image(),
        PRINTHEAD_PX,
        PrintDirection::Left,
    )?)
}

fn render_to(path: &str) -> Result<(), Failure> {
    test_image().save(path)?;
    let enc = encoded()?;
    println!("wrote {path}  ({LABEL_W_PX}×{LABEL_H_PX} px source)");
    describe(&enc);
    Ok(())
}

fn describe(enc: &EncodedImage) {
    let mut empty = 0;
    let mut bitmap = 0;
    let mut indexed = 0;
    for p in &enc.packets {
        match p.command {
            cmd::PRINT_EMPTY_ROW => empty += 1,
            cmd::PRINT_BITMAP_ROW => bitmap += 1,
            cmd::PRINT_BITMAP_ROW_INDEXED => indexed += 1,
            _ => {}
        }
    }
    println!(
        "encoded: {} cols × {} rows  →  {} packets ({bitmap} bitmap, {indexed} indexed, {empty} empty)",
        enc.cols,
        enc.rows,
        enc.packets.len()
    );
    if enc.cols > PRINTHEAD_PX {
        println!(
            "  ⚠️  {} columns is wider than the {PRINTHEAD_PX}-pixel printhead — the label will be clipped",
            enc.cols
        );
    }
}

async fn ask(t: &mut SerialTransport, packet: Packet, label: &str) -> Result<(), Failure> {
    t.discard_pending().await?;
    let command = packet.command;
    t.write(&packet.to_bytes()?).await?;
    match t.read_packet(Duration::from_millis(1500)).await {
        Ok(reply) => {
            println!(
                "  {label:<22} cmd {command:#04x} → {:#04x}  [{}]",
                reply.command,
                reply
                    .data
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if label == "heartbeat(advanced1)" {
                explain_heartbeat(&reply.data);
            }
            if label == "print status" {
                match parse_status(&reply.data) {
                    Ok(s) => println!(
                        "       page {} · print {}% · feed {}%",
                        s.page, s.print_progress, s.feed_progress
                    ),
                    Err(e) => println!("       unparsed: {e}"),
                }
            }
        }
        Err(e) => println!("  {label:<22} cmd {command:#04x} → no answer ({e})"),
    }
    Ok(())
}

/// Best-effort reading of the advanced-1 heartbeat. Field positions depend on
/// the payload length, and the reference keys off exactly that rather than off
/// the model — so this does too.
fn explain_heartbeat(data: &[u8]) {
    let read = |skip: usize| -> Option<&[u8]> { data.get(skip..) };
    match data.len() {
        13 => {
            if let Some(rest) = read(9) {
                println!(
                    "       lid closed: {} · charge: {} · paper in: {} · paper RFID read: {}",
                    rest[0] == 0,
                    rest[1],
                    rest[2] == 0,
                    rest[3] != 0
                );
            }
        }
        10 => {
            if let Some(rest) = read(8) {
                println!("       lid closed: {} · charge: {}", rest[0] == 0, rest[1]);
            }
        }
        n => println!("       payload of {n} bytes — layout not one of the known ones"),
    }
    println!("       ⚠️ some models invert 'lid closed'; trust the physical lid, not this line");
}

async fn probe(port: &str) -> Result<(), Failure> {
    println!("opening {port} …");
    let mut t = SerialTransport::open(port)?;
    println!("open. asking:");

    // Model id, software version, serial number — PrinterInfoType 8, 9, 11.
    for (ty, label) in [
        (8u8, "printer model id"),
        (9, "software version"),
        (11, "serial number"),
    ] {
        ask(&mut t, Packet::new(cmd::PRINTER_INFO, vec![ty]), label).await?;
    }
    // HeartbeatType::Advanced1 = 1, Advanced2 = 4.
    ask(
        &mut t,
        Packet::new(cmd::HEARTBEAT, vec![1]),
        "heartbeat(advanced1)",
    )
    .await?;
    ask(
        &mut t,
        Packet::new(cmd::HEARTBEAT, vec![4]),
        "heartbeat(advanced2)",
    )
    .await?;
    ask(
        &mut t,
        Packet::new(cmd::RFID_INFO, vec![1]),
        "cassette RFID",
    )
    .await?;
    ask(
        &mut t,
        Packet::new(cmd::PRINT_STATUS, vec![1]),
        "print status",
    )
    .await?;

    println!("\nNothing above fed any paper.");
    Ok(())
}

async fn print(port: &str, density: u8) -> Result<(), Failure> {
    let enc = encoded()?;
    describe(&enc);

    println!("\nopening {port} …");
    let mut t = SerialTransport::open(port)?;

    let model = "B1";
    match select_task(model, None) {
        Some(kind) => println!("flow for {model}: {kind:?}"),
        None => return Err(format!("no ported print flow for {model}").into()),
    }

    let opts = PrintOptions {
        density,
        status_timeout: Duration::from_secs(30),
        ..Default::default()
    };
    println!("printing at density {density} …");

    let started = std::time::Instant::now();
    print_b1(&mut t, &enc, &opts).await?;
    println!("done in {:.1}s", started.elapsed().as_secs_f32());
    Ok(())
}
