//! The constants board: a dialog's rows as cards.
//!
//! A dialog is a set of panels, and the definition gives each panel a title.
//! The board draws one card per panel, in document order, so a long engine
//! constants dialog reads as a few named groups instead of one flat list. The
//! placement of those cards in the window belongs to the editor layout; this
//! module only decides what a card and a row look like.
//!
//! What a row holds comes from the core snapshot:
//!
//! - a definition-supplied label list (`bits`) is a select, because the label
//!   is the value a tuner recognizes;
//! - a boolean row is a switch;
//! - a raw number or string is an input;
//! - a note the definition prints is an alert, never an editable row.

mod input;
mod select;
mod switch;

pub(crate) use input::{ConstantInput, InputKind};
pub(crate) use select::{ConstantOption, ConstantSelect};
pub(crate) use switch::control as switch_control;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::alert::Alert;
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, div, px,
};
use oxtuner_core::{DialogField, DialogFieldKind, DialogFieldTone};

use crate::i18n::t;

/// One card: a panel title, then its rows in document order.
///
/// A note is a row too. It stays where the definition put it, so an alert
/// explains the setting right above or below it instead of being collected at
/// the top of the panel.
pub(crate) fn card(
    id: impl Into<ElementId>,
    title: SharedString,
    rows: Vec<AnyElement>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();

    v_flex()
        .id(id)
        .w_full()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.tokens.group_box)
        .overflow_hidden()
        .child(
            v_flex().gap_1().px_4().py_3().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title),
            ),
        )
        .children(rows)
        .into_any_element()
}

/// One field's label column: label, context help, power-cycle mark, metadata.
pub(crate) fn field_label(
    field: &DialogField,
    index: usize,
    available: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let label_color = if available {
        match field.tone {
            DialogFieldTone::Danger => theme.danger,
            DialogFieldTone::Info => theme.info,
            DialogFieldTone::Normal => theme.foreground,
        }
    } else {
        theme.muted_foreground
    };

    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(label_color)
                        .child(field.label.clone()),
                )
                .when(field.power_cycle, |this| this.child(power_cycle(index, cx)))
                .when_some(field.help.as_ref(), |this, help| {
                    this.child(context_help(index, help, cx))
                }),
        )
        .when_some(meta(field), |this, meta| {
            this.child(
                div()
                    .h_4()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(meta),
            )
        })
        .into_any_element()
}

/// A note as a banner, at the row the definition put it.
pub(crate) fn note(index: usize, field: &DialogField, cx: &App) -> AnyElement {
    let text = field.text.clone().unwrap_or_else(|| field.label.clone());
    let alert = match field.tone {
        DialogFieldTone::Danger => Alert::error(("constant-note", index), text),
        DialogFieldTone::Info => Alert::info(("constant-note", index), text),
        DialogFieldTone::Normal => Alert::new(("constant-note", index), text),
    };

    div()
        .px_4()
        .py_2()
        // To drop the first row's top border again, pass the row position and
        // replace this line with:
        // `.when(!first, |this| this.border_t_1().border_color(...))`.
        .border_t_1()
        .border_color(cx.theme().border.opacity(0.5))
        .child(alert.small().w_full())
        .into_any_element()
}

/// The value a numeric input shows, formatted to the definition's digits.
pub(crate) fn value_text(field: &DialogField, value: Option<f64>) -> SharedString {
    value.map_or_else(
        || {
            field
                .text
                .clone()
                .map_or_else(|| t!("app.dialog.no_value"), SharedString::from)
        },
        |value| {
            if field.hex {
                hex_text(value).into()
            } else {
                number_text(value, field.digits).into()
            }
        },
    )
}

/// `0x1F`, for a row with `displayInHex`.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "hex rows are integer constants; the rounded value is displayed as a bit pattern"
)]
pub(crate) fn hex_text(value: f64) -> String {
    format!("0x{:X}", value.round() as i64)
}

/// `14.7`, with the definition's decimal digits.
pub(crate) fn number_text(value: f64, digits: u8) -> String {
    format!("{value:.digits$}", digits = usize::from(digits))
}

/// The raw option value of a numeric constant, when it names one.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the range check keeps the rounded value inside u32 before the cast"
)]
pub(crate) fn option_value(value: Option<f64>) -> Option<u32> {
    let value = value?;

    if !value.is_finite() || value < 0.0 {
        return None;
    }

    let value = value.round();

    if value > f64::from(u32::MAX) {
        return None;
    }

    Some(value as u32)
}

/// The row's accepted display range, when the definition gives one.
fn meta(field: &DialogField) -> Option<SharedString> {
    if field.kind == DialogFieldKind::Note {
        return None;
    }

    // Raw INI names are hidden from the rows for now. Keep the formatter for
    // when a technical view wants them back:
    // let variable = field.index.map_or_else(
    //     || field.variable.clone(),
    //     |index| format!("{}[{index}]", field.variable),
    // );
    // if !variable.is_empty() { parts.push(variable); }
    if matches!(field.kind, DialogFieldKind::Value | DialogFieldKind::Slider) {
        range_hint(field).map(SharedString::from)
    } else {
        None
    }
}

/// `10–18 AFR`, `≤ 250 kPa`, or just the units when the definition gives no
/// bounds.
fn range_hint(field: &DialogField) -> Option<String> {
    let units = field.units.trim();

    match (field.minimum, field.maximum) {
        (Some(min), Some(max)) => Some(with_units(
            format!(
                "{} - {}",
                number_text(min, field.digits),
                number_text(max, field.digits)
            ),
            units,
        )),
        (Some(min), None) => Some(with_units(
            format!("≥ {}", number_text(min, field.digits)),
            units,
        )),
        (None, Some(max)) => Some(with_units(
            format!("≤ {}", number_text(max, field.digits)),
            units,
        )),
        (None, None) if !units.is_empty() => Some(units.to_owned()),
        (None, None) => None,
    }
}

fn with_units(text: String, units: &str) -> String {
    if units.is_empty() {
        text
    } else {
        format!("{text} {units}")
    }
}

/// The unsaved mark, in a fixed slot before the control so the control does
/// not move when the mark appears or disappears.
pub(crate) fn changed_mark(changed: bool, cx: &App) -> AnyElement {
    div()
        .w_2()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .when(changed, |this| {
            this.child(div().size(px(7.)).rounded_full().bg(cx.theme().warning))
        })
        .into_any_element()
}

/// The power-cycle mark: a circle-power icon with the reason on hover.
fn power_cycle(index: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let message = t!("app.dialog.power_cycle_help");

    HoverCard::new(("constant-power", index))
        .p_1()
        .trigger(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    Icon::new(IconName::CirclePower)
                        .xsmall()
                        .text_color(theme.warning),
                ),
        )
        .child(
            v_flex()
                .w_72()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t!("app.dialog.power_cycle")),
                )
                .child(help_body(&message)),
        )
        .into_any_element()
}

/// The body of a definition's help text: real lines, never a literal `\n`.
fn help_body(help: &str) -> AnyElement {
    v_flex()
        .w_72()
        .gap_1()
        .text_xs()
        .children(
            help_lines(help)
                .into_iter()
                .map(|line| div().child(SharedString::from(line))),
        )
        .into_any_element()
}

/// Splits help text into display lines.
///
/// Definitions write line breaks either as a real newline or as the two
/// characters `\n` inside a quoted value. Both mean one line break, and a run
/// of blank lines is not worth a gap in a hover card.
fn help_lines(text: &str) -> Vec<String> {
    text.replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The question-mark trigger that shows the definition's context help.
fn context_help(index: usize, help: &str, cx: &App) -> AnyElement {
    HoverCard::new(("constant-help", index))
        .p_1()
        .trigger(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .cursor_pointer()
                .child(
                    Icon::new(IconName::CircleQuestionMark)
                        .xsmall()
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .child(help_body(help))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtuner_core::GateState;
    use std::sync::Arc;

    fn field(kind: DialogFieldKind, value: Option<f64>) -> DialogField {
        DialogField {
            label: String::new(),
            variable: "reqFuel".to_owned(),
            value,
            text: None,
            units: "ms".to_owned(),
            digits: 2,
            changed: false,
            display_only: false,
            writable: true,
            kind,
            section: String::new(),
            tone: DialogFieldTone::Normal,
            options: Arc::default(),
            visibility_expression: None,
            enable_expression: None,
            gates: GateState::open(),
            help: None,
            minimum: Some(0.0),
            maximum: Some(25.5),
            index: None,
            hex: false,
            flip: false,
            power_cycle: false,
        }
    }

    #[test]
    fn a_numeric_row_advertises_its_range() {
        let row = field(DialogFieldKind::Value, Some(12.3));

        assert_eq!(meta(&row).as_deref(), Some("0.00 - 25.50 ms"));
        assert_eq!(value_text(&row, Some(12.3)), "12.30");
    }

    #[test]
    fn a_select_row_has_no_technical_metadata() {
        let row = field(DialogFieldKind::Select, Some(2.0));

        assert_eq!(meta(&row), None);
    }

    #[test]
    fn help_text_breaks_literal_and_real_newlines() {
        assert_eq!(
            help_lines("One\\nTwo\nThree"),
            vec!["One", "Two", "Three"],
            "both the escape and a real newline start a line"
        );
        assert_eq!(help_lines("One\\r\\nTwo"), vec!["One", "Two"]);
        assert_eq!(help_lines("  \n "), Vec::<String>::new());
    }

    #[test]
    fn an_option_value_rounds_a_stored_bits_number() {
        assert_eq!(option_value(Some(3.0)), Some(3));
        assert_eq!(option_value(Some(3.4)), Some(3));
        assert_eq!(option_value(Some(-1.0)), None);
        assert_eq!(option_value(None), None);
    }
}
