//! The pattern a test print puts on a label.
//!
//! One pattern, used by both the settings window and the bench probe. When
//! somebody reports "the test print looks wrong", the thing they saw has to be
//! the thing we can reproduce — two patterns would make that a guess.
//!
//! Nothing here is decorative. Each mark answers one question, and each of
//! those questions is a way this port can be wrong while the printer is fine:
//!
//! - **border** — is the image the size of the label, or wider than the paper
//! - **corner wedge** — is the print direction right, or rotated / mirrored
//! - **diagonal** — is the bit order within a byte right, or staggered
//! - **comb of one-pixel lines** — is the density low enough not to smear
//! - **lone dot** — do indexed rows (six black pixels or fewer) come out at all

use image::{DynamicImage, GrayImage, Luma};

use super::encoder::PrintDirection;

const WHITE: Luma<u8> = Luma([255]);
const BLACK: Luma<u8> = Luma([0]);

/// `across` runs along the printhead, `along` runs with the paper feed.
///
/// Which of those is the image's width is a property of the model, so the
/// caller passes both and this function decides nothing about orientation.
pub fn test_pattern(across: u32, along: u32, direction: PrintDirection) -> DynamicImage {
    let (w, h) = match direction {
        PrintDirection::Top => (across, along),
        // The encoder rotates, so columns come from the height.
        PrintDirection::Left => (along, across),
    };
    let (w, h) = (w.max(16), h.max(16));

    let mut img = GrayImage::from_pixel(w, h, WHITE);
    let mut dot = |x: u32, y: u32| {
        if x < w && y < h {
            img.put_pixel(x, y, BLACK);
        }
    };

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

    let wedge = (w.min(h) / 4).max(8);
    for y in 0..wedge {
        for x in 0..(wedge - y) {
            dot(6 + x, 6 + y);
        }
    }

    for i in 0..w.min(h) {
        dot(i, i);
        dot(i + 1, i);
    }

    let comb_x = w / 2;
    for i in 0..12u32 {
        for y in (h / 4)..(h * 3 / 4) {
            dot(comb_x + i * 4, y);
        }
    }

    dot(w * 3 / 4, h / 2);

    DynamicImage::ImageLuma8(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    #[test]
    fn the_axes_follow_the_print_direction() {
        assert_eq!(
            test_pattern(384, 160, PrintDirection::Top).dimensions(),
            (384, 160)
        );
        assert_eq!(
            test_pattern(384, 160, PrintDirection::Left).dimensions(),
            (160, 384)
        );
    }

    #[test]
    fn the_wedge_sits_in_one_corner_only() {
        // It is the only mark that says which way up the label came out, so it
        // must not be symmetric.
        let img = test_pattern(200, 100, PrintDirection::Top).to_luma8();
        let dark = |x: u32, y: u32| img.get_pixel(x, y).0[0] == 0;
        assert!(dark(10, 10), "top-left is filled");
        assert!(!dark(190, 10), "top-right is not");
        assert!(!dark(10, 90), "bottom-left is not");
    }

    #[test]
    fn a_tiny_label_still_produces_an_image_rather_than_panicking() {
        // Guards the arithmetic: a 6 mm label is 48 dots, and several of the
        // marks are placed by subtraction.
        for (a, b) in [(8u32, 8u32), (1, 400), (400, 1), (16, 16)] {
            let img = test_pattern(a, b, PrintDirection::Top);
            assert!(img.dimensions().0 >= 16 && img.dimensions().1 >= 16);
        }
    }
}
