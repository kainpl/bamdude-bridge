//! What a printer is, by the number it reports.
//!
//! Ported from `niimbluelib`, `src/printer_models.ts` — the entries for the
//! models whose print flow is ported, and no others.
//!
//! ⚠️ **A printer identifies itself with a number, not a name.** Asking it for
//! `PrinterInfoType::PrinterModelId` returns e.g. 4096; the name is ours to
//! print, never the device's to tell us. Matching on a name string would be
//! matching on something no printer ever sends.
//!
//! ⚠️ **Sharing a print flow does not mean sharing geometry.** All six models
//! below run the B1 flow, and among them the print direction, the printhead
//! width, the resolution and the density range all differ — a D110_M is 96
//! pixels across where a B1 is 384, and an M2_H is 300 dpi where everything
//! else is 203. Taking any of these from a default is how a label comes out
//! sideways, clipped, or at the wrong scale on hardware that is working fine.

use super::encoder::PrintDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    /// Display name. Ours, for the operator — the printer never says it.
    pub name: &'static str,
    pub dpi: u16,
    pub print_direction: PrintDirection,
    pub printhead_pixels: u16,
    pub density_min: u8,
    pub density_max: u8,
    pub density_default: u8,
}

impl ModelInfo {
    /// Dots per millimetre, which is what a label size in millimetres has to be
    /// multiplied by. 203 dpi is the familiar 8; 300 dpi is not.
    pub fn dots_per_mm(&self) -> f32 {
        self.dpi as f32 / 25.4
    }

    /// Widest label this printer can put down, in millimetres.
    pub fn max_width_mm(&self) -> f32 {
        self.printhead_pixels as f32 / self.dots_per_mm()
    }

    /// Keep a requested density inside what this model accepts.
    pub fn clamp_density(&self, requested: u8) -> u8 {
        requested.clamp(self.density_min, self.density_max)
    }
}

/// Only the models whose print flow is ported. An id absent from here is a
/// printer we refuse rather than guess at — see `task::select_task`.
const MODELS: &[(&[u16], ModelInfo)] = &[
    (
        &[4096],
        ModelInfo {
            name: "B1",
            dpi: 203,
            print_direction: PrintDirection::Top,
            printhead_pixels: 384,
            density_min: 1,
            density_max: 5,
            density_default: 3,
        },
    ),
    (
        &[771, 775],
        ModelInfo {
            name: "B21_C2B",
            dpi: 203,
            print_direction: PrintDirection::Top,
            printhead_pixels: 384,
            density_min: 1,
            density_max: 5,
            density_default: 3,
        },
    ),
    (
        &[2560],
        ModelInfo {
            name: "D101",
            dpi: 203,
            print_direction: PrintDirection::Left,
            printhead_pixels: 192,
            density_min: 1,
            density_max: 3,
            density_default: 2,
        },
    ),
    (
        &[2320],
        ModelInfo {
            name: "D110_M",
            dpi: 203,
            print_direction: PrintDirection::Left,
            printhead_pixels: 96,
            density_min: 1,
            density_max: 5,
            density_default: 3,
        },
    ),
    (
        &[4608],
        ModelInfo {
            name: "M2_H",
            dpi: 300,
            print_direction: PrintDirection::Top,
            printhead_pixels: 567,
            density_min: 1,
            density_max: 5,
            density_default: 3,
        },
    ),
    (
        &[3586],
        ModelInfo {
            name: "N1",
            dpi: 203,
            print_direction: PrintDirection::Left,
            printhead_pixels: 96,
            density_min: 1,
            density_max: 3,
            density_default: 2,
        },
    ),
];

/// What the printer with this reported id is, or `None` if it is not ported.
pub fn by_id(model_id: u16) -> Option<ModelInfo> {
    MODELS
        .iter()
        .find(|(ids, _)| ids.contains(&model_id))
        .map(|(_, info)| *info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_b1_reports_itself_as_four_thousand_and_ninety_six() {
        // Measured on real hardware: `PrinterInfo(8)` answered `10 00`.
        let b1 = by_id(4096).expect("B1");
        assert_eq!(b1.name, "B1");
        assert_eq!(b1.printhead_pixels, 384);
        assert_eq!(b1.print_direction, PrintDirection::Top);
    }

    #[test]
    fn the_b1_does_not_use_the_libraries_default_direction() {
        // The reference's encoder defaults to Left. The B1 is Top. Taking the
        // default prints every label rotated ninety degrees on working
        // hardware, which reads as a rendering bug.
        assert_eq!(by_id(4096).unwrap().print_direction, PrintDirection::Top);
        assert_eq!(PrintDirection::default(), PrintDirection::Left);
    }

    #[test]
    fn models_sharing_a_print_flow_do_not_share_geometry() {
        // All six run the B1 flow. Nothing else about them agrees.
        let b1 = by_id(4096).unwrap();
        let d110m = by_id(2320).unwrap();
        let m2h = by_id(4608).unwrap();

        assert_ne!(b1.printhead_pixels, d110m.printhead_pixels);
        assert_ne!(b1.print_direction, d110m.print_direction);
        assert_ne!(b1.dpi, m2h.dpi);
    }

    #[test]
    fn two_hundred_and_three_dpi_is_the_familiar_eight_dots_per_millimetre() {
        assert!((by_id(4096).unwrap().dots_per_mm() - 8.0).abs() < 0.01);
    }

    #[test]
    fn three_hundred_dpi_is_not_eight_dots_per_millimetre() {
        // The assumption "every Niimbot is 8 px/mm" is false inside the set of
        // models the ported flow already covers.
        let m2h = by_id(4608).unwrap();
        assert!((m2h.dots_per_mm() - 11.8).abs() < 0.1);
    }

    #[test]
    fn the_widest_label_follows_from_the_printhead_and_the_resolution() {
        // A B1 is sold for 50 mm media and can put down 48 of it.
        let b1 = by_id(4096).unwrap();
        assert!((b1.max_width_mm() - 48.0).abs() < 0.2);
    }

    #[test]
    fn density_is_clamped_to_what_the_model_accepts() {
        // 5 is fine on a B1 and impossible on a D101; sending it anyway is a
        // command the printer answers by doing something else.
        assert_eq!(by_id(4096).unwrap().clamp_density(5), 5);
        assert_eq!(by_id(2560).unwrap().clamp_density(5), 3);
        assert_eq!(by_id(2560).unwrap().clamp_density(0), 1);
    }

    #[test]
    fn an_unported_model_id_is_unknown_rather_than_approximated() {
        for id in [512u16, 513, 2304, 1792, 0, 65535] {
            assert!(by_id(id).is_none(), "{id} must not resolve");
        }
    }
}
