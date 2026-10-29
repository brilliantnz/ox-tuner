//! The visual tokens the table paints with.
//!
//! Like [`crate::chart::ChartStyle`], the widget crate does not read a global
//! theme: the host supplies the colours, so `oxtuner-ui` stays a custom widget
//! layer over GPUI rather than a second theme owner.

use gpui_kit::{Hsla, Pixels, px};

/// The table's palette, supplied by the host from its theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TableStyle {
    /// Window background behind the grid.
    pub background: Hsla,
    /// Primary text.
    pub foreground: Hsla,
    /// Header bands and the corner cell.
    pub surface: Hsla,
    /// Secondary text, such as the corner caption.
    pub muted: Hsla,
    /// The grid line between cells and the scrollbar thumbs.
    pub border: Hsla,
    /// Write failure and conflict marks.
    pub danger: Hsla,
    /// Unsaved-change marks.
    pub warning: Hsla,
    /// In-RAM and realtime marks.
    pub info: Hsla,
    /// Focus outline around the grid.
    pub accent: Hsla,
    /// The editor frame's corner radius.
    pub radius: Pixels,
}

impl Default for TableStyle {
    fn default() -> Self {
        Self {
            background: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.10,
                a: 1.0,
            },
            foreground: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.93,
                a: 1.0,
            },
            surface: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.135,
                a: 1.0,
            },
            muted: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.58,
                a: 1.0,
            },
            border: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.24,
                a: 1.0,
            },
            danger: Hsla {
                h: 0.00,
                s: 0.72,
                l: 0.58,
                a: 1.0,
            },
            warning: Hsla {
                h: 0.10,
                s: 0.86,
                l: 0.58,
                a: 1.0,
            },
            info: Hsla {
                h: 0.55,
                s: 0.80,
                l: 0.56,
                a: 1.0,
            },
            accent: Hsla {
                h: 0.55,
                s: 0.80,
                l: 0.56,
                a: 1.0,
            },
            radius: px(8.),
        }
    }
}
