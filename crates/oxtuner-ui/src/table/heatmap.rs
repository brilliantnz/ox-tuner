//! The cell heatmap and the write-state marks.
//!
//! These are data colours, not interface chrome: the value is the datum and its
//! colour is the encoding, so the ramp stays stable across light and dark
//! themes while everything around it follows the application theme. The marks
//! take their colours as arguments for the same reason: the caller reads the
//! theme, this module only decides which colour a state means.

use gpui_kit::{Hsla, rgb};

#[cfg(test)]
use gpui_kit::Rgba;

use oxtuner_core::CellState;

/// The heatmap's data palette: a saturated blue → cyan → green → yellow →
/// orange → red ramp.
///
/// The stops are picked for chroma rather than pastel tints so adjacent values
/// stay distinguishable, and the blend between them is linear — easing pushed
/// the midpoints towards the same washed-out middle and made neighbours look
/// alike.
const HEAT_STOPS: [u32; 7] = [
    0x2563EB, // blue
    0x0891B2, // cyan
    0x16A34A, // green
    0x84CC16, // lime
    0xEAB308, // yellow
    0xF97316, // orange
    0xDC2626, // red
];

pub(crate) fn heat_color(minimum: f64, maximum: f64, value: f64) -> Hsla {
    let span = maximum - minimum;
    if span <= f64::EPSILON {
        // Every cell is the same value: show the middle of the scale.
        return Hsla::from(rgb(HEAT_STOPS[HEAT_STOPS.len() / 2]));
    }

    let t = ((value - minimum) / span).clamp(0.0, 1.0) as f32;
    let scaled = t * (HEAT_STOPS.len() - 1) as f32;
    let index = (scaled.floor() as usize).min(HEAT_STOPS.len() - 2);
    let ratio = scaled - index as f32;
    let low = Hsla::from(rgb(HEAT_STOPS[index]));
    let high = Hsla::from(rgb(HEAT_STOPS[index + 1]));
    low.blend(high.opacity(ratio))
}

// Black or white text, whichever reads on the heat colour.
// pub(crate) fn contrast_text(fill: Hsla) -> Hsla {
//     let rgba = Rgba::from(fill);
//     let luminance = 0.299 * rgba.r + 0.587 * rgba.g + 0.114 * rgba.b;
//     if luminance > 0.55 {
//         Hsla::from(rgb(0x111418))
//     } else {
//         Hsla::from(rgb(0xFFFFFF))
//     }
// }
// Unused while cell text is fixed at #121314 in the table. Kept around in case
// the palette changes and per-cell contrast is needed again.

/// The write-state dot for one cell, or `None` when the cell needs no mark.
pub(crate) fn decoration(
    state: CellState,
    changed: bool,
    danger: Hsla,
    warning: Hsla,
    info: Hsla,
) -> Option<Hsla> {
    match state {
        CellState::WriteFailed | CellState::Conflict => Some(danger),
        CellState::InRam => Some(info),
        CellState::LocalDirty => Some(warning),
        CellState::Synced | CellState::Unknown | CellState::NotApplicable => {
            changed.then_some(warning)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_of(color: Hsla) -> (f32, f32, f32) {
        let rgba = Rgba::from(color);
        (rgba.r, rgba.g, rgba.b)
    }

    #[test]
    fn the_heatmap_follows_the_saturated_gradient() {
        let blue = rgb_of(heat_color(0.0, 100.0, 0.0));
        assert!((blue.0 - 0x25 as f32 / 255.0).abs() < 1e-3);
        assert!((blue.1 - 0x63 as f32 / 255.0).abs() < 1e-3);
        assert!((blue.2 - 0xEB as f32 / 255.0).abs() < 1e-3);
        let red = rgb_of(heat_color(0.0, 100.0, 100.0));
        assert!((red.0 - 0xDC as f32 / 255.0).abs() < 1e-3);
        assert!((red.1 - 0x26 as f32 / 255.0).abs() < 1e-3);
        assert!((red.2 - 0x26 as f32 / 255.0).abs() < 1e-3);
        let middle = rgb_of(heat_color(0.0, 100.0, 50.0));
        assert!(
            (middle.0 - 0x84 as f32 / 255.0).abs() < 1e-3
                && (middle.1 - 0xCC as f32 / 255.0).abs() < 1e-3
                && (middle.2 - 0x16 as f32 / 255.0).abs() < 1e-3,
            "halfway sits on the middle stop, got {middle:?}"
        );
        let constant = rgb_of(heat_color(5.0, 5.0, 5.0));
        assert!(
            (constant.0 - middle.0).abs() < 1e-6
                && (constant.1 - middle.1).abs() < 1e-6
                && (constant.2 - middle.2).abs() < 1e-6,
            "a constant table sits on the middle stop, got {constant:?}"
        );
    }

    #[test]
    fn the_heatmap_stays_saturated_between_stops() {
        // A blend between two stops must not collapse to grey: the strongest
        // channel and the weakest must stay far apart across the whole scale.
        for step in 0..=100 {
            let color = rgb_of(heat_color(0.0, 100.0, f64::from(step)));
            let high = color.0.max(color.1).max(color.2);
            let low = color.0.min(color.1).min(color.2);
            assert!(
                high - low > 0.18,
                "value {step} blends to a flat colour {color:?}"
            );
        }
    }

    #[test]
    fn neighbouring_shades_change_gradually() {
        let step = 2.0;
        let mut previous = rgb_of(heat_color(0.0, 100.0, 0.0));
        let mut value = step;
        while value <= 100.0 {
            let current = rgb_of(heat_color(0.0, 100.0, value));
            let jump = (current.0 - previous.0).abs()
                + (current.1 - previous.1).abs()
                + (current.2 - previous.2).abs();
            assert!(
                jump < 0.45,
                "a {step} point step at {value} jumps {jump:.3}, which reads as banding"
            );
            previous = current;
            value += step;
        }
    }

    // #[test]
    // fn text_contrast_flips_at_the_bright_end_of_the_scale() {
    //     let on_blue = Rgba::from(contrast_text(heat_color(0.0, 100.0, 0.0)));
    //     assert!(on_blue.r > 0.9 && on_blue.g > 0.9 && on_blue.b > 0.9);
    //     let on_yellow = Rgba::from(contrast_text(heat_color(0.0, 100.0, 60.0)));
    //     assert!(on_yellow.r < 0.2 && on_yellow.g < 0.2);
    // }
}
