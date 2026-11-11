//! The table's two heat-constrained colors.
//!
//! Every other part of the grid chrome reads the application theme. These two
//! cannot: they are read against the heat ramp (see [`super::heatmap`]), whose
//! stops sit at mid lightness in both light and dark themes. The cell text has
//! to stay dark against every stop, and the selection frame has to stay dark
//! while keeping a hue the ramp does not use. No single theme token expresses
//! that without losing the contrast.
//!
//! The defaults carry the constraint, and a host can replace them through
//! `Table::set_style` when a surface needs a different palette.

use gpui_kit::{Hsla, rgb};

/// The table's colors fixed by the heat ramp rather than the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableStyle {
    /// Text painted over the heat cells.
    ///
    /// Dark in both themes: every ramp stop runs brighter than it, so one
    /// color reads on all of them and the value never flips as the table is
    /// redrawn.
    pub on_heat: Hsla,
    /// Outline around the selected range.
    ///
    /// Every ramp stop sits at mid lightness, so the frame goes dark to
    /// separate from all of them, yellow and red included. Purple keeps the
    /// hue outside the ramp's red-to-blue arc at the same time.
    pub selection_frame: Hsla,
}

impl Default for TableStyle {
    fn default() -> Self {
        Self {
            on_heat: Hsla::from(rgb(0x12_13_14)),
            selection_frame: Hsla {
                h: 288.0 / 360.0,
                s: 0.90,
                l: 0.38,
                a: 0.8,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::Rgba;

    use super::*;
    use crate::table::heatmap::heat_color;

    fn luminance(color: Hsla) -> f32 {
        let rgba = Rgba::from(color);
        let linear = |channel: f32| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };

        0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
    }

    #[test]
    fn the_cell_text_reads_on_every_heat_stop() {
        let style = TableStyle::default();
        let text = luminance(style.on_heat);

        for step in 0..=100 {
            let fill = luminance(heat_color(0.0, 100.0, f64::from(step)));
            let ratio = (fill + 0.05) / (text + 0.05);

            assert!(
                text < fill,
                "the text must stay darker than the value {step} fill"
            );
            assert!(
                ratio >= 3.0,
                "the text reads at {ratio:.2} to 1 on the value {step} fill"
            );
        }
    }

    #[test]
    fn the_selection_frame_stays_dark_and_off_the_heat_hues() {
        let frame = TableStyle::default().selection_frame;

        assert!(
            frame.l < 0.5,
            "the frame separates from mid-lightness stops"
        );
        assert!(frame.a > 0.5, "the outline reads as a frame, not a tint");

        for step in 0..=100 {
            let stop = heat_color(0.0, 100.0, f64::from(step));

            assert!(
                stop.h < frame.h,
                "the frame hue must sit outside the ramp at value {step}"
            );
        }
    }
}
