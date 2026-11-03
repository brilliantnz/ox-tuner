//! The channel picker for the live chart.
//!
//! A definition can declare hundreds of channels; the picker filters by name and
//! toggles the chart's selection. The chart itself owns the selection, so the
//! dialog never keeps a second copy that could drift.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use oxtuner_ui::chart::ChartState;

use crate::app::dialog;
use crate::i18n::t;

/// One channel row whose display strings are built once per dialog, not once
/// per repaint. Selection is still read from the chart every frame.
struct ChannelRow {
    name: SharedString,
    label: SharedString,
    id: ElementId,
    search: String,
}

/// Opens the picker for `chart`.
pub(crate) fn show(window: &mut Window, cx: &mut App, chart: Entity<ChartState>) {
    let search =
        cx.new(|cx| InputState::new(window, cx).placeholder(t!("app.realtime.channel_filter")));

    let rows: Vec<ChannelRow> = chart
        .read(cx)
        .history()
        .channels()
        .iter()
        .filter(|channel| !channel.hidden)
        .map(|channel| {
            let name = SharedString::from(channel.name.as_str());
            let label = if channel.units.is_empty() {
                name.clone()
            } else {
                SharedString::from(format!("{} ({})", channel.name, channel.units))
            };
            let id = ElementId::Name(SharedString::from(format!(
                "chart-channel-{}",
                channel.name
            )));
            let search = channel.name.to_lowercase();

            ChannelRow {
                name,
                label,
                id,
                search,
            }
        })
        .collect();

    dialog::open(window, cx, move |dialog, _, cx| {
        let query = search.read(cx).value().to_lowercase();
        let items: Vec<_> = rows
            .iter()
            .filter(|row| query.is_empty() || row.search.contains(&query))
            .take(400)
            .map(|row| {
                let selected = chart.read(cx).is_selected(&row.name);
                let name = row.name.clone();
                let chart = chart.clone();
                Checkbox::new(row.id.clone())
                    .label(row.label.clone())
                    .checked(selected)
                    .on_click(move |_checked, _, cx| {
                        // `toggle_selected` is idempotent in the sense that the
                        // checkbox's checked state is read from the chart on the
                        // next frame; flipping once is what the click means.
                        chart.update(cx, |chart, cx| chart.toggle_selected(&name, cx));
                        cx.refresh_windows();
                    })
                    .into_any_element()
            })
            .collect();

        let muted = cx.theme().muted_foreground;
        let count = chart.read(cx).series(cx).len();

        dialog
            .title(t!("app.realtime.channels_title"))
            .width(px(560.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.realtime.channels_hint", count = count.to_string())),
                    )
                    .child(Input::new(&search).small().w_full())
                    .child(
                        div()
                            .id("chart-channel-list")
                            .max_h(px(360.))
                            .overflow_y_scroll()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_2()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .when(items.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(t!("app.realtime.no_channels")),
                                        )
                                    })
                                    .children(items),
                            ),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        Button::new("chart-channels-default")
                            .label(t!("app.realtime.default_channels"))
                            .small()
                            .ghost()
                            .on_click({
                                let chart = chart.clone();
                                move |_, _, cx| {
                                    chart.update(cx, |chart, cx| {
                                        chart.set_selected(Vec::new(), cx);
                                    });
                                    cx.refresh_windows();
                                }
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(t!(
                                        "app.realtime.channels_count",
                                        count = count.to_string()
                                    )),
                            )
                            .child(
                                Button::new("chart-channels-close")
                                    .label(t!("app.realtime.close"))
                                    .ghost()
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            ),
                    ),
            )
    });
}
