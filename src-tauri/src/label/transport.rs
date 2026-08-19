//! How bytes reach the printer.
//!
//! The print flow is written against this trait and nothing else, so the whole
//! protocol can be exercised without a printer plugged in — which matters,
//! because CI has no hardware and never will.

use std::time::Duration;

use super::packet::Packet;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the printer did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the connection to the printer is gone")]
    Disconnected,
    #[error("i/o: {0}")]
    Io(String),
    #[error("the printer sent something that is not a packet: {0}")]
    Malformed(String),
}

#[async_trait::async_trait]
pub trait Transport: Send {
    /// Put bytes on the wire. Does not wait for anything — several commands in
    /// the print flow are one-way by design.
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError>;

    /// Read one packet, or fail with [`TransportError::Timeout`].
    async fn read_packet(&mut self, timeout: Duration) -> Result<Packet, TransportError>;

    /// Drop anything already buffered. Called before a command whose reply
    /// matters, so a stale packet from an earlier step cannot be mistaken for
    /// the answer to this one.
    async fn discard_pending(&mut self) -> Result<(), TransportError>;
}

#[cfg(test)]
pub mod mock {
    //! A printer that exists only in the test binary.

    use std::collections::VecDeque;
    use std::time::Duration;

    use super::{Transport, TransportError};
    use crate::label::packet::{cmd, resp, Packet};

    /// Records everything sent and answers the way a printer would.
    pub struct FakeTransport {
        pub sent: Vec<Packet>,
        pub status_polls: usize,
        /// How many status polls before the printer claims the page is done.
        polls_until_done: usize,
        /// Pages the printer will eventually report. Learned from the
        /// PRINT_START it is given, the way a real one would, so a test that
        /// changes the page count does not also have to configure the double.
        pages: u16,
        /// Answer nothing at all, to exercise the timeout path.
        silent: bool,
        queued: VecDeque<Packet>,
        /// Non-zero makes the status packet carry a printer-side error.
        status_error: u8,
    }

    impl FakeTransport {
        pub fn answering_status_after(polls: usize) -> Self {
            Self {
                sent: Vec::new(),
                status_polls: 0,
                polls_until_done: polls,
                pages: 1,
                silent: false,
                queued: VecDeque::new(),
                status_error: 0,
            }
        }

        pub fn silent() -> Self {
            Self {
                silent: true,
                ..Self::answering_status_after(1)
            }
        }

        pub fn reporting_print_error(code: u8) -> Self {
            Self {
                status_error: code,
                ..Self::answering_status_after(1)
            }
        }

        pub fn commands_sent(&self) -> Vec<u8> {
            self.sent.iter().map(|p| p.command).collect()
        }

        fn reply_for(&mut self, packet: &Packet) -> Option<Packet> {
            let command = packet.command;
            if command == cmd::PRINT_START && packet.data.len() >= 2 {
                self.pages = u16::from_be_bytes([packet.data[0], packet.data[1]]);
            }
            let response = match command {
                cmd::SET_DENSITY => resp::SET_DENSITY,
                cmd::SET_LABEL_TYPE => resp::SET_LABEL_TYPE,
                cmd::SET_PAGE_SIZE => resp::SET_PAGE_SIZE,
                cmd::PRINT_START => resp::PRINT_START,
                cmd::PAGE_START => resp::PAGE_START,
                cmd::PAGE_END => resp::PAGE_END,
                cmd::PRINT_END => resp::PRINT_END,
                cmd::PRINT_STATUS => {
                    self.status_polls += 1;
                    let page: u16 = if self.status_polls >= self.polls_until_done {
                        self.pages
                    } else {
                        0
                    };
                    let mut data = page.to_be_bytes().to_vec();
                    data.push(50); // print progress
                    data.push(50); // feed progress
                    if self.status_error != 0 {
                        // The ten-byte form carries an error flag at index 6.
                        data.extend_from_slice(&[0, 0]);
                        data.push(self.status_error);
                        data.extend_from_slice(&[0, 0, 0]);
                    }
                    return Some(Packet::new(resp::PRINT_STATUS, data));
                }
                // Image rows are one-way; a printer sends nothing back.
                cmd::PRINT_BITMAP_ROW | cmd::PRINT_BITMAP_ROW_INDEXED | cmd::PRINT_EMPTY_ROW => {
                    return None
                }
                _ => return None,
            };
            Some(Packet::new(response, vec![1]))
        }
    }

    #[async_trait::async_trait]
    impl Transport for FakeTransport {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
            // Parse back what the flow produced, so the test double also proves
            // every frame it was handed is well formed.
            let mut offset = 0;
            while offset < bytes.len() {
                let (packet, used) = Packet::parse(&bytes[offset..])
                    .map_err(|e| TransportError::Malformed(e.to_string()))?;
                if !self.silent {
                    if let Some(reply) = self.reply_for(&packet) {
                        self.queued.push_back(reply);
                    }
                }
                self.sent.push(packet);
                offset += used;
            }
            Ok(())
        }

        async fn read_packet(&mut self, timeout: Duration) -> Result<Packet, TransportError> {
            self.queued
                .pop_front()
                .ok_or(TransportError::Timeout(timeout))
        }

        async fn discard_pending(&mut self) -> Result<(), TransportError> {
            self.queued.clear();
            Ok(())
        }
    }
}
