//! The table's two heat-constrained colours.
//!
//! Every other part of the grid chrome reads the application theme. These two
//! cannot: they are read against the heat ramp (see the `heatmap` module),
//! which no theme token describes. The defaults carry the contrast the ramp
//! needs, and a host can replace them through
//! [`Table::set_style`](super::Table::set_style) when a surface needs a
//! different palette.

use gpui_kit::{Hsla, rgb};

/// The table's colours fixed by the heat ramp rather than the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TableStyle {
    /// Text painted over the heat cells.
    ///
    /// Dark in both themes: every ramp stop runs brighter than it, so one
    /// colour reads on all of them and the value never flips as the table is
    /// redrawn.
    pub on_heat: Hsla,
    /// Outline around the selected range.
    ///
    /// Darker than every ramp stop, and its hue sits outside the ramp's
    /// red-to-blue arc. The hue carries the tight cases, red and blue, where
    /// the lightness difference alone is small.
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

    fn luminance(color: Rgba) -> f32 {
        let linear = |channel: f32| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };

        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    #[test]
    fn the_cell_text_reads_on_every_heat_stop() {
        let style = TableStyle::default();
        let text = luminance(Rgba::from(style.on_heat));

        for step in 0..=100 {
            let fill = luminance(Rgba::from(heat_color(0.0, 100.0, f64::from(step))));
            let ratio = (fill + 0.05) / (text + 0.05);

            assert!(
                text < fill,
                "the text must stay darker than the value {step} fill"
            );
            // WCAG large-text tier (3 to 1): the blue stop bottoms out near
            // 3.6 to 1, so normal-text AA (4.5 to 1) is out of reach here.
            assert!(
                ratio >= 3.5,
                "the text reads at {ratio:.2} to 1 on the value {step} fill"
            );
        }
    }

    #[test]
    fn the_selection_frame_stays_dark_and_off_the_heat_hues() {
        let frame = TableStyle::default().selection_frame;
        let frame_color = Rgba::from(frame);

        assert!(frame.a > 0.5, "the outline reads as a frame, not a tint");

        for step in 0..=100 {
            let stop = heat_color(0.0, 100.0, f64::from(step));

            let apart = (frame.h - stop.h).abs();
            let apart = apart.min(1.0 - apart);

            assert!(
                apart >= 0.1,
                "the frame sits {apart:.2} turns from the value {step} stop"
            );
            // The outline is translucent, so measure the pixel it paints over
            // the fill rather than the token on its own.
            assert!(
                luminance(Rgba::from(stop).blend(frame_color)) < luminance(Rgba::from(stop)),
                "the frame reads darker than the value {step} fill after blending"
            );
        }
    }
}
