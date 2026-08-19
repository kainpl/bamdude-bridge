//! Turn a 1-bit image into the rows a Niimbot expects.
//!
//! Ported from `niimbluelib`: `src/image_encoder.ts` (row building, pixel
//! indexing) and `src/packets/packet_generator.ts` (`writeImageData`,
//! `printBitmapRow`, `printBitmapRowIndexed`, `printEmptySpace`) together with
//! `countPixelsForBitmapPacket` from `src/utils.ts`.
//!
//! This is the piece where a port goes subtly wrong, and where being wrong is
//! silent: the printer prints *something*, slightly off, and the search starts
//! in the transport. Hence the fixed vectors in the tests.

use image::GenericImageView;

use super::packet::{cmd, Packet, PacketError};

/// ⚠️ The reference's default is `Left`, which **rotates the image 90°
/// clockwise** — the printhead runs across the roll, not along it. Feeding a
/// label straight through as `Top` gives a picture rotated the wrong way, which
/// looks like a rendering bug rather than a direction one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintDirection {
    #[default]
    Left,
    Top,
}

/// A run of identical rows, before it becomes packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub kind: RowKind,
    pub row_number: u16,
    pub repeat: u16,
    pub black_pixels: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// Entirely white — sent as empty space, with no pixel data at all.
    Void,
    Pixels(Vec<u8>),
    /// A bookkeeping marker every 200 rows. ⚠️ Only turned into a packet when
    /// the caller asks: the reference gates it behind `enableCheckLine`, and
    /// sending it unasked is a command the printer did not expect.
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedImage {
    pub cols: u16,
    pub rows: u16,
    pub packets: Vec<Packet>,
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("could not decode the image: {0}")]
    Decode(#[from] image::ImageError),
    #[error("image is {0}×{1}; each side must fit a u16")]
    TooLarge(u32, u32),
    #[error("frame could not be built: {0}")]
    Packet(#[from] PacketError),
}

/// A row with this many black pixels or fewer goes out as indexes.
///
/// ⚠️ Not an optimisation. The reference notes the printer **powers off** when
/// an indexed packet carries more than six, which is why the threshold is a
/// hard boundary and not a tuning knob.
const INDEXED_MAX_BLACK: u32 = 6;

/// `repeat` occupies one byte on the wire.
const MAX_REPEAT: u16 = u8::MAX as u16;

pub fn encode_png(
    png: &[u8],
    printhead_pixels: u16,
    direction: PrintDirection,
) -> Result<EncodedImage, EncodeError> {
    let img = image::load_from_memory(png)?;
    encode_image(&img, printhead_pixels, direction)
}

pub fn encode_image(
    img: &image::DynamicImage,
    printhead_pixels: u16,
    direction: PrintDirection,
) -> Result<EncodedImage, EncodeError> {
    let (src_w, src_h) = img.dimensions();
    if src_w > u16::MAX as u32 || src_h > u16::MAX as u32 {
        return Err(EncodeError::TooLarge(src_w, src_h));
    }

    let (original_cols, rows) = match direction {
        PrintDirection::Left => (src_h, src_w),
        PrintDirection::Top => (src_w, src_h),
    };
    // Pad to a whole number of bytes; the extra columns are white and the
    // printer ignores them.
    let cols = original_cols.div_ceil(8) * 8;

    let gray = img.to_luma8();
    let is_black = |col: u32, row: u32| -> bool {
        let (sx, sy) = match direction {
            PrintDirection::Left => (row, src_h - 1 - col),
            PrintDirection::Top => (col, row),
        };
        gray.get_pixel(sx, sy).0[0] != 255
    };

    let image_rows = build_rows(cols, original_cols, rows, is_black);
    let packets = rows_to_packets(&image_rows, printhead_pixels)?;

    Ok(EncodedImage {
        cols: cols as u16,
        rows: rows as u16,
        packets,
    })
}

/// Build the run-length row list. Split out so the packing can be tested
/// without an image decoder in the way.
pub fn build_rows(
    cols: u32,
    original_cols: u32,
    rows: u32,
    is_black: impl Fn(u32, u32) -> bool,
) -> Vec<ImageRow> {
    let mut out: Vec<ImageRow> = Vec::new();

    for row in 0..rows {
        let mut data = vec![0u8; (cols / 8) as usize];
        let mut black = 0u32;

        for octet in 0..(cols / 8) {
            let mut bits = 0u8;
            for bit in 0..8u32 {
                let col = octet * 8 + bit;
                if col < original_cols && is_black(col, row) {
                    // Most significant bit is the leftmost pixel.
                    bits |= 1 << (7 - bit);
                    black += 1;
                }
            }
            data[octet as usize] = bits;
        }

        let kind = if black == 0 {
            RowKind::Void
        } else {
            RowKind::Pixels(data)
        };
        let fresh = ImageRow {
            kind,
            row_number: row as u16,
            repeat: 1,
            black_pixels: black,
        };

        match out.last_mut() {
            // ⚠️ The run also breaks at MAX_REPEAT. The reference does not do
            // this because JavaScript numbers do not overflow — but `repeat` is
            // one byte on the wire, so a 300-row blank area would be written as
            // 44 and the label would come out short, with nothing to indicate
            // it. A tall label is exactly where that bites.
            Some(last) if last.kind == fresh.kind && last.repeat < MAX_REPEAT => {
                last.repeat += 1;
            }
            _ => out.push(fresh),
        }

        // Every 200th row, for every row but the very first. ⚠️ The condition
        // is "the list is not empty", NOT "the list has more than one entry":
        // on a mostly-blank label every row collapses into a single run, so a
        // length test would skip the marker exactly where the label is longest.
        // The reference expresses this by placing the check inside the branch
        // it takes when the list was already non-empty.
        if row % 200 == 199 && !out.is_empty() {
            out.push(ImageRow {
                kind: RowKind::Check,
                row_number: row as u16,
                repeat: 0,
                black_pixels: 0,
            });
        }
    }

    out
}

/// Which packet each row becomes. `check` rows are dropped — see [`RowKind::Check`].
pub fn rows_to_packets(
    rows: &[ImageRow],
    printhead_pixels: u16,
) -> Result<Vec<Packet>, PacketError> {
    let mut out = Vec::with_capacity(rows.len());

    for row in rows {
        match &row.kind {
            RowKind::Void => {
                let mut data = row.row_number.to_be_bytes().to_vec();
                data.push(row.repeat as u8);
                out.push(Packet::new(cmd::PRINT_EMPTY_ROW, data));
            }
            RowKind::Pixels(bits) => {
                let counts = count_pixels(bits, printhead_pixels);
                let mut data = row.row_number.to_be_bytes().to_vec();
                data.extend_from_slice(&counts.parts);
                data.push(row.repeat as u8);

                if row.black_pixels <= INDEXED_MAX_BLACK {
                    data.extend_from_slice(&index_pixels(bits));
                    out.push(Packet::new(cmd::PRINT_BITMAP_ROW_INDEXED, data));
                } else {
                    data.extend_from_slice(bits);
                    out.push(Packet::new(cmd::PRINT_BITMAP_ROW, data));
                }
            }
            RowKind::Check => {}
        }
    }

    Ok(out)
}

pub struct PixelCounts {
    pub parts: [u8; 3],
    pub total: u32,
}

/// Per-third black-pixel counts the printer wants ahead of the row data.
///
/// The row is only split into thirds when it fits three chunks of the
/// printhead's width; otherwise the parts stay zero and only the total is
/// meaningful — which is what the reference's `"auto"` mode decides.
pub fn count_pixels(bits: &[u8], printhead_pixels: u16) -> PixelCounts {
    let chunk = (printhead_pixels as usize) / 8 / 3;
    let split = chunk > 0 && bits.len() <= chunk * 3;

    let mut parts = [0u32; 3];
    let mut total = 0u32;

    for (byte_n, value) in bits.iter().enumerate() {
        let ones = value.count_ones();
        total += ones;
        if split {
            let idx = byte_n / chunk;
            if idx < 3 {
                parts[idx] += ones;
            }
        }
    }

    PixelCounts {
        parts: [
            parts[0].min(u8::MAX as u32) as u8,
            parts[1].min(u8::MAX as u32) as u8,
            parts[2].min(u8::MAX as u32) as u8,
        ],
        total,
    }
}

/// Positions of the black pixels, two bytes each, big endian.
pub fn index_pixels(bits: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (byte_pos, value) in bits.iter().enumerate() {
        for bit_pos in 0..8u32 {
            if value & (1 << (7 - bit_pos)) != 0 {
                let index = (byte_pos as u32 * 8 + bit_pos) as u16;
                out.extend_from_slice(&index.to_be_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD_PX: u16 = 384;

    fn png_of(width: u32, height: u32, black: &[(u32, u32)]) -> Vec<u8> {
        let mut img = image::GrayImage::from_pixel(width, height, image::Luma([255u8]));
        for (x, y) in black {
            img.put_pixel(*x, *y, image::Luma([0u8]));
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn rows_of(cols: u32, rows: u32, black: &[(u32, u32)]) -> Vec<ImageRow> {
        let owned: Vec<(u32, u32)> = black.to_vec();
        build_rows(cols, cols, rows, move |c, r| owned.contains(&(c, r)))
    }

    #[test]
    fn the_default_direction_rotates_ninety_degrees_clockwise() {
        // A 40-wide, 240-tall label is fed to a printhead that is 240 across.
        let enc = encode_png(&png_of(40, 240, &[]), HEAD_PX, PrintDirection::Left).unwrap();
        assert_eq!(enc.cols, 240, "columns come from the source height");
        assert_eq!(enc.rows, 40, "rows come from the source width");
    }

    #[test]
    fn top_direction_leaves_the_axes_alone() {
        let enc = encode_png(&png_of(40, 240, &[]), HEAD_PX, PrintDirection::Top).unwrap();
        assert_eq!((enc.cols, enc.rows), (40, 240));
    }

    #[test]
    fn a_width_that_is_not_a_whole_byte_is_padded_up_with_white() {
        let enc = encode_png(&png_of(60, 8, &[]), HEAD_PX, PrintDirection::Top).unwrap();
        assert_eq!(enc.cols, 64);
    }

    #[test]
    fn an_all_white_image_sends_only_empty_space() {
        let enc = encode_png(&png_of(64, 4, &[]), HEAD_PX, PrintDirection::Top).unwrap();
        assert!(enc
            .packets
            .iter()
            .all(|p| p.command == cmd::PRINT_EMPTY_ROW));
    }

    #[test]
    fn identical_consecutive_rows_collapse_into_one_repeat() {
        let rows = rows_of(64, 4, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repeat, 4);
    }

    #[test]
    fn a_run_longer_than_a_byte_is_split_rather_than_silently_truncated() {
        // 300 identical rows. One packet would write repeat=300 into one byte
        // and the printer would advance 44 rows.
        let rows = rows_of(64, 300, &[]);
        assert_eq!(rows.iter().filter(|r| r.kind == RowKind::Void).count(), 2);
        let total: u32 = rows
            .iter()
            .filter(|r| r.kind == RowKind::Void)
            .map(|r| r.repeat as u32)
            .sum();
        assert_eq!(total, 300, "no row is lost by the split");
        assert!(rows.iter().all(|r| r.repeat <= MAX_REPEAT));
    }

    #[test]
    fn a_row_with_six_black_pixels_or_fewer_goes_out_as_indexes() {
        let black: Vec<(u32, u32)> = (0..6).map(|x| (x * 3, 0u32)).collect();
        let enc = encode_png(&png_of(64, 1, &black), HEAD_PX, PrintDirection::Top).unwrap();
        assert!(enc
            .packets
            .iter()
            .any(|p| p.command == cmd::PRINT_BITMAP_ROW_INDEXED));
    }

    #[test]
    fn a_seventh_black_pixel_switches_to_the_bitmap_form() {
        // The boundary matters: the reference says the printer powers off if an
        // indexed packet carries more than six.
        let black: Vec<(u32, u32)> = (0..7).map(|x| (x * 3, 0u32)).collect();
        let enc = encode_png(&png_of(64, 1, &black), HEAD_PX, PrintDirection::Top).unwrap();
        assert!(enc
            .packets
            .iter()
            .any(|p| p.command == cmd::PRINT_BITMAP_ROW));
        assert!(!enc
            .packets
            .iter()
            .any(|p| p.command == cmd::PRINT_BITMAP_ROW_INDEXED));
    }

    #[test]
    fn indexes_are_two_bytes_big_endian() {
        // Bit 258 lives in byte 32, bit 2 — big endian 0x0102.
        let mut bits = vec![0u8; 64];
        bits[32] = 0b0010_0000;
        assert_eq!(index_pixels(&bits), vec![0x01, 0x02]);
    }

    #[test]
    fn the_leftmost_pixel_is_the_most_significant_bit() {
        let rows = rows_of(64, 1, &[(0, 0)]);
        let RowKind::Pixels(bits) = &rows[0].kind else {
            panic!("expected pixels");
        };
        assert_eq!(bits[0], 0b1000_0000);
    }

    #[test]
    fn a_bitmap_row_is_position_counts_repeat_then_data() {
        let black: Vec<(u32, u32)> = (0..10).map(|x| (x, 0u32)).collect();
        let enc = encode_png(&png_of(64, 1, &black), HEAD_PX, PrintDirection::Top).unwrap();
        let p = enc
            .packets
            .iter()
            .find(|p| p.command == cmd::PRINT_BITMAP_ROW)
            .expect("bitmap row");
        assert_eq!(&p.data[0..2], &[0x00, 0x00], "row 0, big endian");
        assert_eq!(
            p.data.len(),
            2 + 3 + 1 + 8,
            "pos + counts + repeat + 8 bytes"
        );
        assert_eq!(p.data[5], 1, "repeat");
        assert_eq!(
            p.data[6], 0b1111_1111,
            "first pixel byte follows the header"
        );
    }

    #[test]
    fn an_empty_row_packet_is_position_then_repeat() {
        let enc = encode_png(&png_of(64, 3, &[]), HEAD_PX, PrintDirection::Top).unwrap();
        let p = &enc.packets[0];
        assert_eq!(p.command, cmd::PRINT_EMPTY_ROW);
        assert_eq!(p.data, vec![0x00, 0x00, 3]);
    }

    #[test]
    fn a_blank_tall_label_still_records_its_check_rows() {
        // Regression on the port: with every row collapsing into one run, a
        // "more than one entry" test would record no marker at all here.
        let rows = rows_of(64, 401, &[]);
        assert_eq!(rows.iter().filter(|r| r.kind == RowKind::Check).count(), 2);
    }

    #[test]
    fn check_rows_are_recorded_but_not_sent_unasked() {
        // The reference gates them behind an option that is off by default.
        let rows = rows_of(64, 401, &[(0, 5), (0, 250)]);
        assert!(rows.iter().any(|r| r.kind == RowKind::Check));
        let packets = rows_to_packets(&rows, HEAD_PX).unwrap();
        assert!(packets.iter().all(|p| p.command != cmd::PRINTER_CHECK_LINE));
    }

    #[test]
    fn counts_are_split_in_thirds_when_the_row_fits_the_printhead() {
        // 384 px head → chunk = 16 bytes, so a 48-byte row splits evenly.
        let mut bits = vec![0u8; 48];
        bits[0] = 0xff; // first third
        bits[20] = 0xff; // second third
        let counts = count_pixels(&bits, 384);
        assert_eq!(counts.parts, [8, 8, 0]);
        assert_eq!(counts.total, 16);
    }

    #[test]
    fn a_row_wider_than_the_printhead_reports_only_a_total() {
        let mut bits = vec![0u8; 200];
        bits[0] = 0xff;
        let counts = count_pixels(&bits, 384);
        assert_eq!(counts.parts, [0, 0, 0]);
        assert_eq!(counts.total, 8);
    }

    #[test]
    fn every_packet_the_encoder_produces_can_actually_be_framed() {
        // A row is 2 + 3 + 1 + cols/8 bytes and the length field is one byte,
        // so a wide enough label would produce a packet that cannot be sent.
        // 384 px is the widest printhead in the family: 48 + 6 = 54 bytes.
        let black: Vec<(u32, u32)> = (0..300).map(|x| (x, 0u32)).collect();
        let enc = encode_png(&png_of(384, 3, &black), HEAD_PX, PrintDirection::Top).unwrap();
        for p in &enc.packets {
            p.to_bytes()
                .expect("every produced packet must be sendable");
        }
    }
}
