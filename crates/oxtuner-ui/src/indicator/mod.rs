//! Tuning indicators: the `[FrontPage]` state strip as a reusable widget.
//!
//! The widget is display-only. It takes labels, states and tones from the
//! host; it never evaluates an INI expression and never reads the definition.
//! The colours are a token set, like [`crate::chart::ChartStyle`], so the host
//! owns the theme and the widget can be embedded in a panel or a dialog.
//!
//! The reference is the indicator strip design in the repository's local
//! design notes: compact cards with a label on the left and a status dot on the
//! right. Off, ok, warning and danger are distinct colour states, not one green
//! "on" state. The strip paints only the cards; the host supplies the panel
//! background, padding and radius around it.

pub mod legend;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, BoxShadow, FontWeight, Hsla, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div, px, rgb,
};

/// What one indicator is doing in the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorState {
    /// The expression evaluated to zero.
    Off,
    /// The expression evaluated to a non-zero value.
    On,
    /// The expression could not be evaluated this frame.
    Unavailable,
}

/// The visual meaning of an active indicator.
///
/// The definition does not say whether "on" is good or bad, so the host picks
/// the tone. Off indicators use the inactive colours and ignore the tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndicatorTone {
    /// Normal operation, for example "Running" or "SD OK".
    #[default]
    Ok,
    /// A condition the user should watch, for example "Engine Protect".
    Warning,
    /// A fault, for example "Reset Lock" or "Disconnected".
    Danger,
}

/// One indicator to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorView {
    /// The label for the current state.
    pub label: SharedString,
    /// Whether it is off, on or cannot be evaluated.
    pub state: IndicatorState,
    /// The active tone. Ignored while the indicator is off.
    pub tone: IndicatorTone,
}

impl IndicatorView {
    /// A card with no extra detail.
    pub fn new(label: impl Into<SharedString>, state: IndicatorState, tone: IndicatorTone) -> Self {
        Self {
            label: label.into(),
            state,
            tone,
        }
    }
}

/// The colour and sizing tokens the strip paints with.
///
/// The defaults are the dark reference palette. A host with a light theme or a
/// different brand passes its own values; the widget never reads a global
/// theme.
#[derive(Debug, Clone, PartialEq)]
pub struct IndicatorStyle {
    /// Inactive card background.
    pub inactive_bg: Hsla,
    /// Inactive card border.
    pub inactive_border: Hsla,
    /// Inactive label text.
    pub inactive_text: Hsla,
    /// Inactive status dot.
    pub inactive_dot: Hsla,
    /// Good/active card background.
    pub ok_bg: Hsla,
    /// Good/active card border.
    pub ok_border: Hsla,
    /// Good/active label text.
    pub ok_text: Hsla,
    /// Good/active status dot.
    pub ok_dot: Hsla,
    /// Warning card background.
    pub warning_bg: Hsla,
    /// Warning card border.
    pub warning_border: Hsla,
    /// Warning label text.
    pub warning_text: Hsla,
    /// Warning status dot.
    pub warning_dot: Hsla,
    /// Danger card background.
    pub danger_bg: Hsla,
    /// Danger card border.
    pub danger_border: Hsla,
    /// Danger label text.
    pub danger_text: Hsla,
    /// Danger status dot.
    pub danger_dot: Hsla,
    /// Card corner radius.
    pub card_radius: gpui_kit::Pixels,
    /// Gap between cards.
    pub gap: gpui_kit::Pixels,
    /// Minimum card width in the wrapping mode. Cards grow above it to fill
    /// the row.
    pub min_width: gpui_kit::Pixels,
    /// Card padding, horizontal.
    pub card_padding_x: gpui_kit::Pixels,
    /// Card padding, vertical.
    pub card_padding_y: gpui_kit::Pixels,
    /// Label size.
    pub label_size: gpui_kit::Pixels,
    /// Status dot size.
    pub dot_size: gpui_kit::Pixels,
    /// Gap between the label and the dot.
    pub dot_gap: gpui_kit::Pixels,
}

impl Default for IndicatorStyle {
    fn default() -> Self {
        let color = |hex| Hsla::from(rgb(hex));

        Self {
            inactive_bg: color(0x2a2a2a),
            inactive_border: color(0x333333),
            inactive_text: color(0x888888),
            inactive_dot: color(0x444444),
            ok_bg: color(0x143222),
            ok_border: color(0x22c55e),
            ok_text: color(0x86efac),
            ok_dot: color(0x22c55e),
            warning_bg: color(0x3b2f18),
            warning_border: color(0xf59e0b),
            warning_text: color(0xfcd34d),
            warning_dot: color(0xf59e0b),
            danger_bg: color(0x3b1818),
            danger_border: color(0xef4444),
            danger_text: color(0xfca5a5),
            danger_dot: color(0xef4444),
            card_radius: px(5.),
            gap: px(8.),
            min_width: px(160.),
            card_padding_x: px(8.),
            card_padding_y: px(4.),
            label_size: px(10.),
            dot_size: px(7.),
            dot_gap: px(5.),
        }
    }
}

/// The wrapping indicator strip.
///
/// The cards use the definition's labels and the host's tones. The strip has no
/// internal state and emits no events; a click does nothing because the
/// indicator is a reading, not a command.
#[derive(IntoElement)]
pub struct IndicatorStrip {
    indicators: Vec<IndicatorView>,
    style: IndicatorStyle,
    columns: Option<usize>,
}

impl IndicatorStrip {
    /// A strip for the given indicators.
    pub fn new(indicators: impl IntoIterator<Item = IndicatorView>) -> Self {
        Self {
            indicators: indicators.into_iter().collect(),
            style: IndicatorStyle::default(),
            columns: None,
        }
    }

    /// Supplies the host's colours and spacing.
    pub fn style(mut self, style: IndicatorStyle) -> Self {
        self.style = style;
        self
    }

    /// Fixes the column count, for a dialog's `indicatorPanel`. Without it the
    /// strip wraps fixed-width cards and leaves a short last row empty.
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = Some(columns.max(1));
        self
    }
}

/// How the strip places its cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardLayout {
    /// Cards share each row and grow to fill it.
    ///
    /// GPUI has no CSS-style auto-fill grid: a flex line only knows how many
    /// cards fit after it wraps. So a full row is distributed evenly, and a
    /// short last row grows its cards across the remaining width. The host
    /// that needs a fixed grid passes `columns(n)`.
    Wrap,
    /// Equal columns that share the host width. Used by `columns(n)`.
    Grid,
}

impl RenderOnce for IndicatorStrip {
    fn render(self, _window: &mut Window, _cx: &mut gpui_kit::App) -> impl IntoElement {
        let style = self.style;
        let layout = match self.columns {
            Some(_) => CardLayout::Grid,
            None => CardLayout::Wrap,
        };
        let cards = self
            .indicators
            .into_iter()
            .map(|indicator| card(indicator, &style, layout))
            .collect::<Vec<_>>();

        let strip = div().w_full().gap(style.gap);

        match self.columns {
            Some(columns) => strip.grid().grid_cols(columns as u16).children(cards),
            None => strip.flex().flex_wrap().children(cards),
        }
    }
}

fn card(indicator: IndicatorView, style: &IndicatorStyle, layout: CardLayout) -> AnyElement {
    let (background, border, text, dot) = colors(indicator.state, indicator.tone, style);
    let hollow = indicator.state == IndicatorState::Unavailable;

    let dot = div()
        .w(style.dot_size)
        .h(style.dot_size)
        .rounded_full()
        .flex_shrink_0()
        .when(hollow, |this| {
            this.border_1().border_color(style.inactive_dot)
        })
        .when(indicator.state == IndicatorState::On, |this| {
            this.bg(dot).shadow(vec![
                BoxShadow::new(px(0.), px(0.), dot.opacity(0.55)).blur_radius(style.dot_size),
            ])
        })
        .when(indicator.state == IndicatorState::Off, |this| this.bg(dot));

    let card = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(style.dot_gap)
        .px(style.card_padding_x)
        .py(style.card_padding_y)
        .rounded(style.card_radius)
        .border_1()
        .border_color(border)
        .bg(background)
        .child(
            div()
                .line_clamp(2)
                .text_size(style.label_size)
                .font_weight(FontWeight::MEDIUM)
                .text_color(text)
                .child(indicator.label.clone()),
        )
        .child(dot);

    match layout {
        CardLayout::Wrap => card.flex_1().min_w(style.min_width),
        // A fixed grid shares the host width; a narrow column lets the label
        // wrap instead of overlapping the next card.
        CardLayout::Grid => card.min_w(px(0.)),
    }
    .into_any_element()
}

fn colors(
    state: IndicatorState,
    tone: IndicatorTone,
    style: &IndicatorStyle,
) -> (Hsla, Hsla, Hsla, Hsla) {
    if state != IndicatorState::On {
        return (
            style.inactive_bg,
            style.inactive_border,
            style.inactive_text,
            style.inactive_dot,
        );
    }

    match tone {
        IndicatorTone::Ok => (style.ok_bg, style.ok_border, style.ok_text, style.ok_dot),
        IndicatorTone::Warning => (
            style.warning_bg,
            style.warning_border,
            style.warning_text,
            style.warning_dot,
        ),
        IndicatorTone::Danger => (
            style.danger_bg,
            style.danger_border,
            style.danger_text,
            style.danger_dot,
        ),
    }
}

#[cfg(test)]
mod tests;
