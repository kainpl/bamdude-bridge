//! The per-model print flow.
//!
//! Ported from `niimbluelib`: `src/print_tasks/B1PrintTask.ts`,
//! `src/print_tasks/AbstractPrintTask.ts`, `src/print_tasks/index.ts` (model →
//! task resolution) and the status polling in `src/packets/abstraction.ts`.
//!
//! Seven flows exist upstream; **one is ported**. That is not a stub — see
//! [`select_task`] for why an unported model is refused rather than routed to
//! the nearest thing.

use std::time::{Duration, Instant};

use super::encoder::EncodedImage;
use super::packet::{cmd, resp, Packet, PacketError};
use super::transport::{Transport, TransportError};

/// Which of the reference's print flows a printer needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    B1,
}

/// What the printer is being asked to do.
#[derive(Debug, Clone, Copy)]
pub struct PrintOptions {
    pub density: u8,
    /// `1` is `WithGaps`, the default for most label rolls.
    pub label_type: u8,
    pub total_pages: u16,
    /// Copies of this one page.
    pub copies: u16,
    /// Print temperature for multicolour media; `0` on ordinary rolls.
    pub color: u8,
    pub packet_timeout: Duration,
    pub status_poll_interval: Duration,
    pub status_timeout: Duration,
}

impl Default for PrintOptions {
    fn default() -> Self {
        // Values from the reference's printOptionsDefaults, except density,
        // which it sets to 2 and BamDude carries per device.
        Self {
            density: 3,
            label_type: 1,
            total_pages: 1,
            copies: 1,
            color: 0,
            packet_timeout: Duration::from_millis(5_000),
            status_poll_interval: Duration::from_millis(300),
            status_timeout: Duration::from_millis(5_000),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Packet(#[from] PacketError),
    #[error("the printer does not support this command")]
    NotSupported,
    #[error("expected a reply to {sent:#04x}, got {got:#04x}")]
    UnexpectedReply { sent: u8, got: u8 },
    #[error("the printer reported error {0} while printing")]
    Printer(u8),
    #[error("the printer never reported the page finished")]
    NeverFinished,
    #[error("model {0} has no ported print flow")]
    UnsupportedModel(String),
}

/// Which flow a printer needs, or `None` if it has not been ported.
///
/// Keyed on the **model id the printer reports**, not on a name: a name is ours
/// to display and never something the device sends.
///
/// Resolution order is the reference's — a (model, protocol version) pair wins
/// over the bare model, because one model on two firmwares can need two
/// different flows.
///
/// ⚠️ **`None` is a feature, not a gap.** A model routed to the wrong flow does
/// not fail — it prints something wrong or jams, and the operator cannot tell
/// which. "This model is not supported" is a sentence they can act on.
pub fn select_task(model_id: u16, protocol_version: Option<u8>) -> Option<TaskKind> {
    // Pairs first. D110_M on protocol 4 needs the D110MV4 flow, which is not
    // ported, so it must NOT fall through to the bare-model match below — that
    // is exactly the wrong-flow case this function exists to prevent.
    if let Some(version) = protocol_version {
        if let (2320, 4) = (model_id, version) {
            return None;
        }
    }

    // Every id the B1 flow covers upstream. Their geometry differs wildly —
    // see `models::by_id`, which is where that lives.
    match model_id {
        4096 | 771 | 775 | 2560 | 2320 | 4608 | 3586 => Some(TaskKind::B1),
        _ => None,
    }
}

/// The B1 print flow.
///
/// ⚠️ `PRINT_END` is sent even when the print failed, mirroring the reference's
/// `finally`. Leaving it out strands the printer mid-job, and the next print
/// then fails for a reason that has nothing to do with itself.
pub async fn print_b1<T: Transport + ?Sized>(
    transport: &mut T,
    image: &EncodedImage,
    opts: &PrintOptions,
) -> Result<(), PrintError> {
    let result = print_b1_body(transport, image, opts).await;

    let ended = expect(
        transport,
        Packet::new(cmd::PRINT_END, vec![1]),
        &[resp::PRINT_END],
        opts.packet_timeout,
    )
    .await;

    match (result, ended) {
        // The failure that happened first is the one worth reporting.
        (Err(e), _) => Err(e),
        (Ok(()), Err(e)) => Err(e),
        (Ok(()), Ok(_)) => Ok(()),
    }
}

async fn print_b1_body<T: Transport + ?Sized>(
    transport: &mut T,
    image: &EncodedImage,
    opts: &PrintOptions,
) -> Result<(), PrintError> {
    // ── init ────────────────────────────────────────────────────────────────
    expect(
        transport,
        Packet::new(cmd::SET_DENSITY, vec![opts.density]),
        &[resp::SET_DENSITY],
        opts.packet_timeout,
    )
    .await?;
    expect(
        transport,
        Packet::new(cmd::SET_LABEL_TYPE, vec![opts.label_type]),
        &[resp::SET_LABEL_TYPE],
        opts.packet_timeout,
    )
    .await?;
    // printStart7b: total pages, four reserved zero bytes, then page colour.
    let mut start = opts.total_pages.to_be_bytes().to_vec();
    start.extend_from_slice(&[0, 0, 0, 0]);
    start.push(opts.color);
    expect(
        transport,
        Packet::new(cmd::PRINT_START, start),
        &[resp::PRINT_START],
        opts.packet_timeout,
    )
    .await?;

    // ── page ────────────────────────────────────────────────────────────────
    expect(
        transport,
        Packet::new(cmd::PAGE_START, vec![1]),
        &[resp::PAGE_START],
        opts.packet_timeout,
    )
    .await?;

    // setPageSize6b: rows, then cols, then copies — in that order.
    let mut size = image.rows.to_be_bytes().to_vec();
    size.extend_from_slice(&image.cols.to_be_bytes());
    size.extend_from_slice(&opts.copies.to_be_bytes());
    expect(
        transport,
        Packet::new(cmd::SET_PAGE_SIZE, size),
        &[resp::SET_PAGE_SIZE],
        opts.packet_timeout,
    )
    .await?;

    // ⚠️ Image rows are one-way. The reference maps them to no response at all,
    // and waiting for an acknowledgement per row would stall on the first one.
    for packet in &image.packets {
        transport.write(&packet.to_bytes()?).await?;
    }

    expect(
        transport,
        Packet::new(cmd::PAGE_END, vec![1]),
        &[resp::PAGE_END],
        opts.packet_timeout,
    )
    .await?;

    wait_until_finished(transport, opts).await
}

/// Poll until the printer says it has done as many pages as we asked for.
async fn wait_until_finished<T: Transport + ?Sized>(
    transport: &mut T,
    opts: &PrintOptions,
) -> Result<(), PrintError> {
    let deadline = opts
        .status_timeout
        .as_millis()
        .div_ceil(opts.status_poll_interval.as_millis().max(1));
    let max_polls = deadline.max(1) as usize;

    for _ in 0..max_polls {
        let status = read_status(transport, opts).await?;
        if status.page >= opts.total_pages {
            return Ok(());
        }
        tokio::time::sleep(opts.status_poll_interval).await;
    }
    Err(PrintError::NeverFinished)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintStatus {
    pub page: u16,
    pub print_progress: u8,
    pub feed_progress: u8,
}

async fn read_status<T: Transport + ?Sized>(
    transport: &mut T,
    opts: &PrintOptions,
) -> Result<PrintStatus, PrintError> {
    let reply = expect(
        transport,
        Packet::new(cmd::PRINT_STATUS, vec![1]),
        &[resp::PRINT_STATUS],
        opts.status_timeout,
    )
    .await?;
    parse_status(&reply.data)
}

/// Status payload: page, print progress, feed progress — and, in the ten-byte
/// form only, an error flag two bytes further on.
pub fn parse_status(data: &[u8]) -> Result<PrintStatus, PrintError> {
    if data.len() < 4 {
        return Err(PrintError::Transport(TransportError::Malformed(format!(
            "status payload of {} bytes, expected at least 4",
            data.len()
        ))));
    }
    if data.len() == 10 && data[6] != 0 {
        return Err(PrintError::Printer(data[6]));
    }
    Ok(PrintStatus {
        page: u16::from_be_bytes([data[0], data[1]]),
        print_progress: data[2],
        feed_progress: data[3],
    })
}

/// Send a packet and wait for one of the answers this command has.
///
/// ⚠️ **Unmatched packets are stepped over, not treated as failures.** A B1
/// answers `PageEnd` with `PrinterCheckLine` *as well as* the reply the command
/// asks for, and rejecting whichever arrived first turns a healthy printer into
/// an error — measured on hardware, and the reference behaves the same way: it
/// keeps listening until a valid id shows up or the timeout runs out.
///
/// The two exceptions are the printer's own refusals, which are answers.
async fn expect<T: Transport + ?Sized>(
    transport: &mut T,
    packet: Packet,
    valid: &[u8],
    timeout: Duration,
) -> Result<Packet, PrintError> {
    // A reply left over from an earlier step would otherwise be read as the
    // answer to this one, and every subsequent reply would be off by one.
    transport.discard_pending().await?;

    let sent = packet.command;
    transport.write(&packet.to_bytes()?).await?;

    let deadline = Instant::now() + timeout;
    let mut stepped_over: Vec<u8> = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(PrintError::UnexpectedReply {
                sent,
                got: stepped_over.last().copied().unwrap_or(0),
            });
        }

        let reply = transport.read_packet(remaining).await?;

        if reply.command == resp::NOT_SUPPORTED {
            return Err(PrintError::NotSupported);
        }
        if reply.command == resp::PRINT_ERROR {
            return Err(PrintError::Printer(
                reply.data.first().copied().unwrap_or(0),
            ));
        }
        if valid.contains(&reply.command) {
            return Ok(reply);
        }
        stepped_over.push(reply.command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::encoder::{encode_png, PrintDirection};
    use crate::label::transport::mock::FakeTransport;

    fn tiny_image() -> EncodedImage {
        let mut img = image::GrayImage::from_pixel(64, 8, image::Luma([255u8]));
        for x in 0..40 {
            img.put_pixel(x, 3, image::Luma([0u8]));
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        encode_png(&buf.into_inner(), 384, PrintDirection::Top).unwrap()
    }

    fn fast() -> PrintOptions {
        PrintOptions {
            status_poll_interval: Duration::from_millis(1),
            status_timeout: Duration::from_millis(50),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_b1_flow_sends_its_commands_in_order() {
        let mut t = FakeTransport::answering_status_after(1);
        print_b1(&mut t, &tiny_image(), &fast()).await.unwrap();

        let sent = t.commands_sent();
        assert_eq!(
            &sent[..3],
            &[cmd::SET_DENSITY, cmd::SET_LABEL_TYPE, cmd::PRINT_START]
        );
        assert_eq!(sent[3], cmd::PAGE_START);
        assert_eq!(sent[4], cmd::SET_PAGE_SIZE);
        assert_eq!(sent.last(), Some(&cmd::PRINT_END));

        let page_end = sent.iter().position(|c| *c == cmd::PAGE_END).unwrap();
        let first_row = sent
            .iter()
            .position(|c| *c == cmd::PRINT_BITMAP_ROW || *c == cmd::PRINT_EMPTY_ROW)
            .unwrap();
        assert!(
            first_row < page_end,
            "rows go out before the page is closed"
        );
    }

    #[tokio::test]
    async fn the_page_size_is_rows_then_columns_then_copies() {
        let mut t = FakeTransport::answering_status_after(1);
        let image = tiny_image();
        let opts = PrintOptions {
            copies: 3,
            ..fast()
        };
        print_b1(&mut t, &image, &opts).await.unwrap();

        let p = t
            .sent
            .iter()
            .find(|p| p.command == cmd::SET_PAGE_SIZE)
            .unwrap();
        let mut expected = image.rows.to_be_bytes().to_vec();
        expected.extend_from_slice(&image.cols.to_be_bytes());
        expected.extend_from_slice(&3u16.to_be_bytes());
        assert_eq!(p.data, expected);
    }

    #[tokio::test]
    async fn print_start_carries_the_page_count_four_zeroes_and_the_colour() {
        let mut t = FakeTransport::answering_status_after(1);
        let opts = PrintOptions {
            total_pages: 2,
            color: 1,
            ..fast()
        };
        print_b1(&mut t, &tiny_image(), &opts).await.unwrap();

        let p = t
            .sent
            .iter()
            .find(|p| p.command == cmd::PRINT_START)
            .unwrap();
        assert_eq!(p.data, vec![0x00, 0x02, 0, 0, 0, 0, 1]);
    }

    #[tokio::test]
    async fn printing_waits_for_the_printer_to_report_the_page_finished() {
        let mut t = FakeTransport::answering_status_after(3);
        print_b1(&mut t, &tiny_image(), &fast()).await.unwrap();
        assert!(t.status_polls >= 3);
    }

    #[tokio::test]
    async fn a_page_end_answered_with_a_check_line_first_still_completes() {
        // Measured on a real B1: closing the page produces PrinterCheckLine
        // (0xd3) as well as In_PageEnd (0xe4). Treating whichever arrived first
        // as *the* answer turns a healthy printer into "expected a reply to
        // 0xe3, got 0xd3" — which is exactly what the first hardware run said.
        let mut t = FakeTransport::like_a_real_b1();
        print_b1(&mut t, &tiny_image(), &fast()).await.unwrap();
        assert!(t.commands_sent().contains(&cmd::PRINT_END));
    }

    #[tokio::test]
    async fn a_printer_that_never_finishes_gives_up_instead_of_hanging() {
        let mut t = FakeTransport::answering_status_after(10_000);
        let err = print_b1(&mut t, &tiny_image(), &fast()).await.unwrap_err();
        assert!(matches!(err, PrintError::NeverFinished));
    }

    #[tokio::test]
    async fn a_transport_that_stops_answering_fails_rather_than_hanging() {
        let mut t = FakeTransport::silent();
        let err = print_b1(&mut t, &tiny_image(), &fast()).await.unwrap_err();
        assert!(matches!(
            err,
            PrintError::Transport(TransportError::Timeout(_))
        ));
    }

    #[tokio::test]
    async fn the_printer_is_released_even_when_the_print_failed() {
        // Otherwise it stays mid-job and the *next* print fails for a reason
        // that has nothing to do with itself.
        let mut t = FakeTransport::reporting_print_error(5);
        let err = print_b1(&mut t, &tiny_image(), &fast()).await.unwrap_err();
        assert!(matches!(err, PrintError::Printer(5)));
        assert!(t.commands_sent().contains(&cmd::PRINT_END));
    }

    #[test]
    fn a_status_payload_shorter_than_four_bytes_is_refused() {
        assert!(parse_status(&[0, 1, 2]).is_err());
    }

    #[test]
    fn the_ten_byte_status_form_carries_an_error_flag() {
        let ok = parse_status(&[0, 1, 50, 50, 0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(ok.page, 1);
        assert!(matches!(
            parse_status(&[0, 1, 50, 50, 0, 0, 7, 0, 0, 0]),
            Err(PrintError::Printer(7))
        ));
    }

    #[test]
    fn the_b1_flow_covers_more_than_the_b1() {
        // B1, B21_C2B (two ids), D101, D110_M, M2_H, N1.
        for id in [4096u16, 771, 775, 2560, 2320, 4608, 3586] {
            assert_eq!(select_task(id, None), Some(TaskKind::B1), "id {id}");
        }
    }

    #[test]
    fn a_model_with_a_protocol_version_beats_the_bare_model() {
        // D110_M (2320) on protocol 4 needs a flow that is not ported. Falling
        // back to B1 here would be the wrong-flow failure select_task exists to
        // prevent.
        assert_eq!(select_task(2320, Some(4)), None);
        assert_eq!(select_task(2320, Some(3)), Some(TaskKind::B1));
        assert_eq!(select_task(2320, None), Some(TaskKind::B1));
    }

    #[test]
    fn an_unported_model_is_refused_rather_than_guessed_at() {
        // D11, D11S, B21, B21_L2B, B21S, D110, H1S and anything unheard of.
        for id in [512u16, 513, 514, 1792, 2304, 3584, 5120, 0, 65535] {
            assert_eq!(select_task(id, None), None, "id {id}");
        }
    }
}
