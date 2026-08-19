//! Niimbot packet framing.
//!
//! Ported from `niimbluelib`, `src/packets/packet.ts` and `src/packets/commands.ts`.
//!
//! The frame is identical across every model and protocol version — only the
//! *contents* of start-of-print and page-size differ by generation, which is why
//! the model-specific part lives elsewhere and not here.

/// Every frame starts with these two bytes.
const HEAD: [u8; 2] = [0x55, 0x55];
/// Every frame ends with these two bytes.
const TAIL: [u8; 2] = [0xaa, 0xaa];

/// Head + command + length + checksum + tail, with an empty payload.
const MIN_FRAME_LEN: usize = HEAD.len() + 1 + 1 + 1 + TAIL.len();

/// Commands the bridge sends. Values from `RequestCommandId` in the reference;
/// only the ones this role needs are listed, so an unported command is a
/// compile error rather than a guess.
pub mod cmd {
    /// ⚠️ The only command whose *entire frame* is prefixed with `0x03`.
    pub const CONNECT: u8 = 0xc1;
    pub const HEARTBEAT: u8 = 0xdc;
    pub const PAGE_START: u8 = 0x03;
    pub const PAGE_END: u8 = 0xe3;
    pub const PRINT_START: u8 = 0x01;
    pub const PRINT_END: u8 = 0xf3;
    pub const PRINT_STATUS: u8 = 0xa3;
    pub const PRINT_CLEAR: u8 = 0x20;
    pub const PRINT_BITMAP_ROW: u8 = 0x85;
    /// Sent instead of [`PRINT_BITMAP_ROW`] when the row has 6 black pixels or
    /// fewer. Not an optimisation — see the encoder for why the printer cares.
    pub const PRINT_BITMAP_ROW_INDEXED: u8 = 0x83;
    pub const PRINT_EMPTY_ROW: u8 = 0x84;
    pub const PRINTER_CHECK_LINE: u8 = 0x86;
    pub const RFID_INFO: u8 = 0x1a;
    pub const SET_DENSITY: u8 = 0x21;
    pub const SET_LABEL_TYPE: u8 = 0x23;
    pub const SET_PAGE_SIZE: u8 = 0x13;
    pub const PRINTER_INFO: u8 = 0x40;
}

/// Responses the bridge reads. Values from `ResponseCommandId`.
pub mod resp {
    pub const NOT_SUPPORTED: u8 = 0x00;
    /// The printer refusing the job outright, as its own packet.
    pub const PRINT_ERROR: u8 = 0xdb;
    /// ⚠️ Some printers — the B1 among them, measured — send this after
    /// `PageEnd` **in addition to** the reply that command asks for. It is
    /// noise to step over, not an answer and not a failure.
    pub const PRINTER_CHECK_LINE: u8 = 0xd3;
    pub const CONNECT: u8 = 0xc2;
    pub const PAGE_START: u8 = 0x04;
    pub const PAGE_END: u8 = 0xe4;
    pub const PRINT_END: u8 = 0xf4;
    pub const PRINT_STATUS: u8 = 0xb3;
    pub const PRINT_START: u8 = 0x02;
    pub const SET_DENSITY: u8 = 0x31;
    pub const SET_LABEL_TYPE: u8 = 0x33;
    pub const SET_PAGE_SIZE: u8 = 0x14;
    pub const HEARTBEAT_ADVANCED1: u8 = 0xdd;
    pub const HEARTBEAT_BASIC: u8 = 0xde;
    pub const HEARTBEAT_ADVANCED2: u8 = 0xd9;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub command: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PacketError {
    /// Not an error in a stream — it means "read more and try again".
    #[error("need more bytes")]
    TooShort,
    #[error("frame does not start with 55 55")]
    BadHead,
    #[error("frame does not end with aa aa")]
    BadTail,
    #[error("checksum mismatch: frame says {found:#04x}, payload gives {expected:#04x}")]
    BadChecksum { found: u8, expected: u8 },
    /// A payload longer than a `u8` cannot state its own length.
    #[error("payload of {0} bytes does not fit a one-byte length field")]
    PayloadTooLong(usize),
}

impl Packet {
    pub fn new(command: u8, data: Vec<u8>) -> Self {
        Self { command, data }
    }

    /// XOR over command, length and payload — the frame bytes are not covered.
    fn checksum(&self) -> u8 {
        let mut sum = self.command ^ (self.data.len() as u8);
        for b in &self.data {
            sum ^= b;
        }
        sum
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, PacketError> {
        if self.data.len() > u8::MAX as usize {
            return Err(PacketError::PayloadTooLong(self.data.len()));
        }

        let mut out = Vec::with_capacity(MIN_FRAME_LEN + self.data.len());
        // ⚠️ Connect, and only Connect, carries an extra 0x03 ahead of the
        // whole frame. It is not part of the checksum and not part of any other
        // command; the reference special-cases it in exactly this place.
        if self.command == cmd::CONNECT {
            out.push(0x03);
        }
        out.extend_from_slice(&HEAD);
        out.push(self.command);
        out.push(self.data.len() as u8);
        out.extend_from_slice(&self.data);
        out.push(self.checksum());
        out.extend_from_slice(&TAIL);
        Ok(out)
    }

    /// Parse one frame from the front of `buf`.
    ///
    /// Returns the packet and how many bytes it consumed, so a caller reading a
    /// serial stream can advance and keep going. Replies never carry the
    /// `Connect` prefix, so parsing does not look for one.
    pub fn parse(buf: &[u8]) -> Result<(Packet, usize), PacketError> {
        if buf.len() < MIN_FRAME_LEN {
            return Err(PacketError::TooShort);
        }
        if buf[0..2] != HEAD {
            return Err(PacketError::BadHead);
        }

        let command = buf[2];
        let len = buf[3] as usize;
        let total = HEAD.len() + 2 + len + 1 + TAIL.len();
        if buf.len() < total {
            return Err(PacketError::TooShort);
        }

        let data = buf[4..4 + len].to_vec();
        let found = buf[4 + len];
        let packet = Packet { command, data };
        let expected = packet.checksum();
        if found != expected {
            return Err(PacketError::BadChecksum { found, expected });
        }
        if buf[total - 2..total] != TAIL {
            return Err(PacketError::BadTail);
        }
        Ok((packet, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packet_is_head_command_length_data_checksum_tail() {
        let bytes = Packet::new(cmd::HEARTBEAT, vec![0x01]).to_bytes().unwrap();
        assert_eq!(
            bytes,
            vec![0x55, 0x55, 0xdc, 0x01, 0x01, 0xdc ^ 0x01 ^ 0x01, 0xaa, 0xaa]
        );
    }

    #[test]
    fn the_checksum_covers_command_length_and_data_but_not_the_frame() {
        let bytes = Packet::new(cmd::PRINT_START, vec![0x0a, 0x0b])
            .to_bytes()
            .unwrap();
        let expected = 0x01u8 ^ 0x02 ^ 0x0a ^ 0x0b;
        assert_eq!(bytes[bytes.len() - 3], expected);
    }

    #[test]
    fn connect_and_only_connect_is_prefixed_with_three() {
        // The reference special-cases this one command; sending the prefix on
        // anything else, or omitting it here, is a frame the printer ignores.
        let connect = Packet::new(cmd::CONNECT, vec![0x01]).to_bytes().unwrap();
        assert_eq!(connect[0], 0x03);
        assert_eq!(&connect[1..3], &[0x55, 0x55]);

        let other = Packet::new(cmd::HEARTBEAT, vec![0x01]).to_bytes().unwrap();
        assert_eq!(&other[0..2], &[0x55, 0x55]);
    }

    #[test]
    fn the_connect_prefix_is_outside_the_checksum() {
        let connect = Packet::new(cmd::CONNECT, vec![0x01]).to_bytes().unwrap();
        assert_eq!(connect[connect.len() - 3], 0xc1 ^ 0x01 ^ 0x01);
    }

    #[test]
    fn parse_round_trips_what_to_bytes_produced() {
        let original = Packet::new(cmd::SET_PAGE_SIZE, vec![1, 2, 3, 4, 5, 6]);
        let bytes = original.to_bytes().unwrap();
        let (parsed, consumed) = Packet::parse(&bytes).unwrap();
        assert_eq!(parsed, original);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn parse_reports_how_much_it_consumed_so_a_stream_can_continue() {
        let mut buf = Packet::new(cmd::HEARTBEAT, vec![1]).to_bytes().unwrap();
        buf.extend(Packet::new(cmd::RFID_INFO, vec![2]).to_bytes().unwrap());

        let (first, consumed) = Packet::parse(&buf).unwrap();
        assert_eq!(first.command, cmd::HEARTBEAT);
        let (second, _) = Packet::parse(&buf[consumed..]).unwrap();
        assert_eq!(second.command, cmd::RFID_INFO);
    }

    #[test]
    fn a_corrupted_payload_is_rejected_rather_than_believed() {
        let mut bytes = Packet::new(cmd::HEARTBEAT, vec![1]).to_bytes().unwrap();
        let payload_byte = bytes.len() - 4;
        bytes[payload_byte] ^= 0xff;
        assert!(matches!(
            Packet::parse(&bytes),
            Err(PacketError::BadChecksum { .. })
        ));
    }

    #[test]
    fn a_truncated_frame_asks_for_more_instead_of_failing() {
        let bytes = Packet::new(cmd::HEARTBEAT, vec![1, 2, 3])
            .to_bytes()
            .unwrap();
        assert_eq!(Packet::parse(&bytes[..5]), Err(PacketError::TooShort));
        // Long enough to hold a minimal frame, but not this one.
        assert_eq!(Packet::parse(&bytes[..7]), Err(PacketError::TooShort));
    }

    #[test]
    fn a_frame_that_does_not_start_with_the_head_is_refused() {
        let mut bytes = Packet::new(cmd::HEARTBEAT, vec![1]).to_bytes().unwrap();
        bytes[0] = 0x56;
        assert_eq!(Packet::parse(&bytes), Err(PacketError::BadHead));
    }

    #[test]
    fn a_payload_too_long_to_state_its_own_length_is_refused_not_truncated() {
        // The length field is one byte. Silently sending the low 8 bits would
        // produce a frame the printer reads as a shorter, valid one.
        let err = Packet::new(cmd::PRINT_BITMAP_ROW, vec![0; 256])
            .to_bytes()
            .unwrap_err();
        assert_eq!(err, PacketError::PayloadTooLong(256));
    }

    #[test]
    fn an_empty_payload_still_frames() {
        let bytes = Packet::new(cmd::PRINT_END, vec![]).to_bytes().unwrap();
        assert_eq!(bytes.len(), MIN_FRAME_LEN);
        let (parsed, _) = Packet::parse(&bytes).unwrap();
        assert!(parsed.data.is_empty());
        assert_eq!(parsed.command, cmd::PRINT_END);
    }
}
