//! USB serial transport.
//!
//! The `serialport` crate is blocking, and this trait is async, so every read
//! and write hands the port to a blocking thread and takes it back. That is
//! cheaper than it looks — a print is a few hundred small writes, not a hot
//! loop — and it avoids running a second thread with a channel protocol of its
//! own for the lifetime of the app.

use std::time::{Duration, Instant};

use serialport::SerialPort;

use super::packet::{Packet, PacketError};
use super::transport::{Transport, TransportError};

/// What `niimprint` uses, and what a Niimbot's USB CDC endpoint expects.
const BAUD: u32 = 115_200;
/// Per-read timeout inside the loop; the caller's timeout bounds the whole wait.
const READ_CHUNK_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PortInfo {
    pub name: String,
    pub description: String,
    /// USB ports rank above Bluetooth ones — see [`rank_candidates`].
    pub usb: bool,
}

/// Every serial port the machine can see.
pub fn list_ports() -> Vec<PortInfo> {
    let ports = serialport::available_ports().unwrap_or_default();
    let mut out: Vec<PortInfo> = ports
        .into_iter()
        .map(|p| {
            let (description, usb) = match &p.port_type {
                serialport::SerialPortType::UsbPort(info) => {
                    let name = info
                        .product
                        .clone()
                        .or_else(|| info.manufacturer.clone())
                        .unwrap_or_else(|| "USB serial".to_string());
                    (name, true)
                }
                serialport::SerialPortType::BluetoothPort => {
                    ("Bluetooth serial".to_string(), false)
                }
                serialport::SerialPortType::PciPort => ("PCI serial".to_string(), false),
                serialport::SerialPortType::Unknown => ("Serial port".to_string(), false),
            };
            PortInfo {
                name: p.port_name,
                description,
                usb,
            }
        })
        .collect();
    out = rank_candidates(out);
    out
}

/// Most-likely-printer first.
///
/// A Windows machine paired with anything at all carries "Standard Serial over
/// Bluetooth link" ports that answer nothing. Offering one of those as the first
/// choice sends the operator to debug a cable that is fine.
pub fn rank_candidates(mut ports: Vec<PortInfo>) -> Vec<PortInfo> {
    ports.sort_by(|a, b| b.usb.cmp(&a.usb).then_with(|| a.name.cmp(&b.name)));
    ports
}

pub struct SerialTransport {
    port: Option<Box<dyn SerialPort>>,
    /// Bytes read but not yet consumed by a complete packet.
    buf: Vec<u8>,
}

impl SerialTransport {
    pub fn open(port_name: &str) -> Result<Self, TransportError> {
        let port = serialport::new(port_name, BAUD)
            .timeout(READ_CHUNK_TIMEOUT)
            .open()
            .map_err(|e| TransportError::Io(format!("{port_name}: {e}")))?;
        Ok(Self {
            port: Some(port),
            buf: Vec::new(),
        })
    }

    fn take(&mut self) -> Result<Box<dyn SerialPort>, TransportError> {
        self.port.take().ok_or(TransportError::Disconnected)
    }

    /// Pull one complete packet out of the buffer, resynchronising past junk.
    fn drain_packet(&mut self) -> Option<Packet> {
        loop {
            match Packet::parse(&self.buf) {
                Ok((packet, used)) => {
                    self.buf.drain(..used);
                    return Some(packet);
                }
                Err(PacketError::TooShort) => return None,
                // ⚠️ Resynchronise rather than give up. A serial line can hand
                // us the tail of something we were not listening for; dropping
                // the whole buffer would take a valid packet with it.
                Err(_) if !self.buf.is_empty() => {
                    self.buf.remove(0);
                }
                Err(_) => return None,
            }
        }
    }
}

#[async_trait::async_trait]
impl Transport for SerialTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let mut port = self.take()?;
        let owned = bytes.to_vec();
        let (port, result) = tokio::task::spawn_blocking(move || {
            let r = std::io::Write::write_all(&mut port, &owned)
                .and_then(|_| std::io::Write::flush(&mut port));
            (port, r)
        })
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;

        self.port = Some(port);
        result.map_err(|e| TransportError::Io(e.to_string()))
    }

    async fn read_packet(&mut self, timeout: Duration) -> Result<Packet, TransportError> {
        let deadline = Instant::now() + timeout;

        loop {
            if let Some(packet) = self.drain_packet() {
                return Ok(packet);
            }
            if Instant::now() >= deadline {
                return Err(TransportError::Timeout(timeout));
            }

            let mut port = self.take()?;
            let (port, chunk) = tokio::task::spawn_blocking(move || {
                let mut scratch = [0u8; 512];
                let read = std::io::Read::read(&mut port, &mut scratch);
                let chunk = match read {
                    Ok(n) => Ok(scratch[..n].to_vec()),
                    // A timeout on a chunk is normal: the printer simply has
                    // nothing to say yet, and the caller's deadline decides.
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
                    Err(e) => Err(e),
                };
                (port, chunk)
            })
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;

            self.port = Some(port);
            self.buf
                .extend_from_slice(&chunk.map_err(|e| TransportError::Io(e.to_string()))?);
        }
    }

    async fn discard_pending(&mut self) -> Result<(), TransportError> {
        self.buf.clear();
        let port = self.take()?;
        let (port, result) = tokio::task::spawn_blocking(move || {
            let r = port.clear(serialport::ClearBuffer::Input);
            (port, r)
        })
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;

        self.port = Some(port);
        result.map_err(|e| TransportError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, description: &str, usb: bool) -> PortInfo {
        PortInfo {
            name: name.into(),
            description: description.into(),
            usb,
        }
    }

    #[test]
    fn listing_ports_does_not_panic_on_a_machine_with_none() {
        let _ = list_ports();
    }

    #[test]
    fn a_usb_serial_port_ranks_above_a_bluetooth_one() {
        let ranked = rank_candidates(vec![
            port("COM5", "Standard Serial over Bluetooth link", false),
            port("COM3", "USB-SERIAL CH340", true),
        ]);
        assert_eq!(ranked[0].name, "COM3");
    }

    #[test]
    fn ports_of_equal_rank_keep_a_stable_order() {
        // Otherwise the list reshuffles between openings of the settings window
        // and the operator picks a different port than the one they saw.
        let ranked = rank_candidates(vec![
            port("COM9", "USB-SERIAL CH340", true),
            port("COM3", "USB Serial Device", true),
        ]);
        assert_eq!(
            ranked.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["COM3", "COM9"]
        );
    }

    #[test]
    fn a_stray_byte_before_a_frame_does_not_swallow_it() {
        let mut t = SerialTransport {
            port: None,
            buf: Vec::new(),
        };
        t.buf.push(0x00); // junk from something we were not listening for
        t.buf.extend(
            Packet::new(super::super::packet::cmd::HEARTBEAT, vec![1])
                .to_bytes()
                .unwrap(),
        );

        let packet = t.drain_packet().expect("resynchronised past the junk");
        assert_eq!(packet.command, super::super::packet::cmd::HEARTBEAT);
        assert!(t.buf.is_empty());
    }

    #[test]
    fn a_partial_frame_is_kept_for_the_next_read() {
        let mut t = SerialTransport {
            port: None,
            buf: Vec::new(),
        };
        let full = Packet::new(super::super::packet::cmd::HEARTBEAT, vec![1, 2, 3])
            .to_bytes()
            .unwrap();
        t.buf.extend_from_slice(&full[..4]);
        assert!(t.drain_packet().is_none());
        assert_eq!(
            t.buf.len(),
            4,
            "nothing is thrown away while it may still complete"
        );

        t.buf.extend_from_slice(&full[4..]);
        assert!(t.drain_packet().is_some());
    }
}
