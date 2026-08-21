//! Everything the printer will say about itself and what is loaded in it.
//!
//! Ported from `niimbluelib`, `src/packets/abstraction.ts` — `getPrinterInfo`,
//! `processHeartbeatAdvanced1` and `processRfidInfo`.
//!
//! Lives here rather than in the caller so the settings window and the bench
//! probe read the printer the same way. Two readers of one device that disagree
//! about what it said is a bug that only appears when they disagree.

use std::time::Duration;

use serde::Serialize;

use super::models::{by_id, ModelInfo};
use super::packet::{cmd, Packet};
use super::transport::{Transport, TransportError};

/// How long to wait for any single answer. The printer replies in milliseconds
/// when it is there at all, so this bounds "not there" rather than "slow".
const ASK_TIMEOUT: Duration = Duration::from_millis(1500);

/// `PrinterInfoType` values from the reference. Only the ones worth showing.
mod info {
    pub const MODEL_ID: u8 = 8;
    pub const SOFTWARE_VERSION: u8 = 9;
    pub const SERIAL_NUMBER: u8 = 11;
}

#[derive(Debug, Clone, Serialize)]
pub struct Heartbeat {
    pub lid_closed: Option<bool>,
    pub charge_level: Option<u8>,
    pub paper_inserted: Option<bool>,
    pub tag_read: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cassette {
    pub uuid: String,
    pub barcode: String,
    pub serial: String,
    pub total: i16,
    pub used: i16,
    /// 1 = with gaps, 2 = black mark, 3 = continuous, … — see [`consumable_name`].
    pub consumable_type: u8,
    pub consumable_name: &'static str,
    pub capacity: Option<i16>,
}

/// What the bridge knows about the attached printer at one moment.
///
/// ⚠️ No label size in millimetres anywhere, because the printer does not know
/// it. The tag carries a barcode; turning that into millimetres is the server's
/// catalogue, and putting a guess here would make two answers to one question.
#[derive(Debug, Clone, Serialize)]
pub struct PrinterSnapshot {
    /// The number the printer states. Its name is ours to resolve, never its
    /// to tell us.
    pub model_id: Option<u16>,
    pub model_name: Option<&'static str>,
    /// True when this model has a ported print flow. False means the device is
    /// recognised and still refused — which is a different sentence.
    pub supported: bool,
    pub dpi: Option<u16>,
    pub printhead_pixels: Option<u16>,
    pub density_min: Option<u8>,
    pub density_max: Option<u8>,
    pub density_default: Option<u8>,
    pub firmware: Option<String>,
    pub serial: Option<String>,
    pub heartbeat: Option<Heartbeat>,
    pub cassette: Option<Cassette>,
}

impl PrinterSnapshot {
    fn empty() -> Self {
        Self {
            model_id: None,
            model_name: None,
            supported: false,
            dpi: None,
            printhead_pixels: None,
            density_min: None,
            density_max: None,
            density_default: None,
            firmware: None,
            serial: None,
            heartbeat: None,
            cassette: None,
        }
    }

    fn apply_model(&mut self, id: u16, info: Option<ModelInfo>) {
        self.model_id = Some(id);
        if let Some(m) = info {
            self.model_name = Some(m.name);
            self.supported = true;
            self.dpi = Some(m.dpi);
            self.printhead_pixels = Some(m.printhead_pixels);
            self.density_min = Some(m.density_min);
            self.density_max = Some(m.density_max);
            self.density_default = Some(m.density_default);
        }
    }
}

pub fn consumable_name(kind: u8) -> &'static str {
    match kind {
        1 => "With gaps",
        2 => "Black mark",
        3 => "Continuous",
        4 => "Perforated",
        5 => "Transparent",
        6 => "PVC tag",
        10 => "Black mark gap",
        11 => "Heat-shrink tube",
        _ => "Unknown",
    }
}

/// Send one question and take the answer, or `None` if it does not come.
///
/// A missing answer is not an error here: this is a status read, and a printer
/// that declines one question while answering the others is more useful shown
/// partly filled in than refused wholesale.
async fn ask<T: Transport + ?Sized>(transport: &mut T, packet: Packet) -> Option<Packet> {
    transport.discard_pending().await.ok()?;
    transport.write(&packet.to_bytes().ok()?).await.ok()?;
    transport.read_packet(ASK_TIMEOUT).await.ok()
}

fn ascii(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    text.chars()
        .all(|c| c.is_ascii_graphic())
        .then(|| text.into_owned())
}

pub async fn read_snapshot<T: Transport + ?Sized>(
    transport: &mut T,
) -> Result<PrinterSnapshot, TransportError> {
    let mut snapshot = PrinterSnapshot::empty();

    if let Some(p) = ask(
        transport,
        Packet::new(cmd::PRINTER_INFO, vec![info::MODEL_ID]),
    )
    .await
    {
        if p.data.len() >= 2 {
            let id = u16::from_be_bytes([p.data[0], p.data[1]]);
            snapshot.apply_model(id, by_id(id));
        }
    }
    if let Some(p) = ask(
        transport,
        Packet::new(cmd::PRINTER_INFO, vec![info::SOFTWARE_VERSION]),
    )
    .await
    {
        snapshot.firmware = Some(format_version(&p.data));
    }
    if let Some(p) = ask(
        transport,
        Packet::new(cmd::PRINTER_INFO, vec![info::SERIAL_NUMBER]),
    )
    .await
    {
        snapshot.serial = ascii(&p.data);
    }
    if let Some(p) = ask(transport, Packet::new(cmd::HEARTBEAT, vec![1])).await {
        snapshot.heartbeat = Some(parse_heartbeat(&p.data));
    }
    if let Some(p) = ask(transport, Packet::new(cmd::RFID_INFO, vec![1])).await {
        snapshot.cassette = parse_cassette(&p.data);
    }

    Ok(snapshot)
}

/// Two bytes, shown the way the vendor writes it. Not parsed into a number —
/// it is a label, and inventing a scheme for it would be inventing a fact.
fn format_version(data: &[u8]) -> String {
    match data.len() {
        2 => format!("{}.{}", data[0], data[1]),
        _ => data
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(""),
    }
}

/// ⚠️ Field offsets follow the **payload length**, not the model — which is how
/// the reference does it, and the only way that survives meeting a model whose
/// length nobody wrote down. The 13-byte form is the B1's, measured.
pub fn parse_heartbeat(data: &[u8]) -> Heartbeat {
    let at = |i: usize| data.get(i).copied();
    match data.len() {
        13 => Heartbeat {
            lid_closed: at(9).map(|v| v == 0),
            charge_level: at(10),
            paper_inserted: at(11).map(|v| v == 0),
            tag_read: at(12).map(|v| v != 0),
        },
        10 => Heartbeat {
            lid_closed: at(8).map(|v| v == 0),
            charge_level: at(9),
            paper_inserted: None,
            tag_read: None,
        },
        19 => Heartbeat {
            lid_closed: at(15).map(|v| v == 0),
            charge_level: at(16),
            paper_inserted: at(17).map(|v| v == 0),
            tag_read: at(18).map(|v| v != 0),
        },
        20 => Heartbeat {
            lid_closed: None,
            charge_level: None,
            paper_inserted: at(18).map(|v| v == 0),
            tag_read: at(19).map(|v| v != 0),
        },
        _ => Heartbeat {
            lid_closed: None,
            charge_level: None,
            paper_inserted: None,
            tag_read: None,
        },
    }
}

/// uuid · barcode · serial · total · used · consumable type · optional capacity.
///
/// A one-byte payload means no tag is present at all, which is a real answer
/// rather than a failure to parse.
pub fn parse_cassette(data: &[u8]) -> Option<Cassette> {
    if data.len() <= 1 {
        return None;
    }
    let mut at = 0usize;

    let uuid_bytes = data.get(at..at + 8)?;
    let uuid: String = uuid_bytes.iter().map(|b| format!("{b:02x}")).collect();
    at += 8;

    let vstring = |at: &mut usize| -> Option<String> {
        let len = *data.get(*at)? as usize;
        *at += 1;
        let bytes = data.get(*at..*at + len)?;
        *at += len;
        Some(String::from_utf8_lossy(bytes).into_owned())
    };
    let barcode = vstring(&mut at)?;
    let serial = vstring(&mut at)?;

    let i16_at = |at: &mut usize| -> Option<i16> {
        let b = data.get(*at..*at + 2)?;
        *at += 2;
        Some(i16::from_be_bytes([b[0], b[1]]))
    };
    let total = i16_at(&mut at)?;
    let used = i16_at(&mut at)?;
    let consumable_type = *data.get(at)?;
    at += 1;
    let capacity = i16_at(&mut at);

    Some(Cassette {
        uuid,
        barcode,
        serial,
        total,
        used,
        consumable_type,
        consumable_name: consumable_name(consumable_type),
        capacity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes a real B1 sent on 2026-08-19.
    const REAL_RFID: &[u8] = &[
        0x88, 0x1d, 0xbd, 0xa5, 0xed, 0x96, 0x00, 0x00, 0x0d, 0x36, 0x39, 0x37, 0x31, 0x35, 0x30,
        0x31, 0x32, 0x32, 0x37, 0x37, 0x34, 0x33, 0x10, 0x50, 0x43, 0x30, 0x46, 0x43, 0x32, 0x31,
        0x33, 0x30, 0x32, 0x30, 0x30, 0x38, 0x32, 0x31, 0x39, 0x01, 0x80, 0x00, 0x3f, 0x01, 0x01,
        0x40, 0x88, 0x1d, 0xbd, 0xa5, 0xed, 0x96, 0x00, 0x00,
    ];
    const REAL_HEARTBEAT: &[u8] = &[
        0x1f, 0x10, 0x00, 0x75, 0x00, 0x75, 0x00, 0x00, 0x4f, 0x00, 0x03, 0x00, 0x01,
    ];

    #[test]
    fn a_real_cassette_tag_decodes_field_for_field() {
        let c = parse_cassette(REAL_RFID).expect("tag present");
        assert_eq!(c.barcode, "6971501227743");
        assert_eq!(c.serial, "PC0FC21302008219");
        assert_eq!(c.uuid, "881dbda5ed960000");
        assert_eq!((c.total, c.used), (384, 63));
        assert_eq!(c.consumable_type, 1);
        assert_eq!(c.consumable_name, "With gaps");
        assert_eq!(c.capacity, Some(320));
    }

    #[test]
    fn the_tag_says_nothing_about_millimetres() {
        // Pinned deliberately: the day someone "finds" a size field here is the
        // day the catalogue quietly stops being the answer. There is no such
        // field — checked byte by byte against real hardware.
        let c = parse_cassette(REAL_RFID).unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("width"));
        assert!(!json.contains("height"));
        assert!(!json.contains("_mm"));
    }

    #[test]
    fn an_empty_tag_response_means_no_cassette_rather_than_a_parse_failure() {
        assert!(parse_cassette(&[0x00]).is_none());
        assert!(parse_cassette(&[]).is_none());
    }

    #[test]
    fn a_truncated_tag_is_refused_instead_of_read_past_the_end() {
        for cut in [4usize, 9, 20, 30, 40] {
            let _ = parse_cassette(&REAL_RFID[..cut]);
        }
    }

    #[test]
    fn a_real_heartbeat_decodes_by_its_length() {
        let h = parse_heartbeat(REAL_HEARTBEAT);
        assert_eq!(h.lid_closed, Some(true));
        assert_eq!(h.charge_level, Some(3));
        assert_eq!(h.paper_inserted, Some(true));
        assert_eq!(h.tag_read, Some(true));
    }

    #[test]
    fn an_unknown_heartbeat_length_says_nothing_rather_than_something_wrong() {
        let h = parse_heartbeat(&[1, 2, 3]);
        assert!(h.lid_closed.is_none() && h.charge_level.is_none());
    }

    #[test]
    fn a_recognised_but_unported_model_is_not_reported_as_supported() {
        let mut s = PrinterSnapshot::empty();
        s.apply_model(512, by_id(512));
        assert_eq!(s.model_id, Some(512));
        assert!(!s.supported);
        assert!(s.model_name.is_none());
    }

    #[test]
    fn a_supported_model_carries_the_capabilities_the_server_needs() {
        let mut s = PrinterSnapshot::empty();
        s.apply_model(4096, by_id(4096));
        assert!(s.supported);
        assert_eq!(s.model_name, Some("B1"));
        assert_eq!(s.dpi, Some(203));
        assert_eq!(s.printhead_pixels, Some(384));
        assert_eq!((s.density_min, s.density_max), (Some(1), Some(5)));
    }

    #[test]
    fn a_two_byte_version_is_shown_the_way_the_vendor_writes_it() {
        assert_eq!(format_version(&[5, 0x1a]), "5.26");
    }
}
