//! Session log counters and the activity the core reports.
//!
//! The panel keeps the last outcome of each operation rather than a full
//! timeline; the write journal supplies the timeline when the project has one.
//! Reading the journal stays an explicit command, so this tab never sends work
//! to the core just because it became visible.

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use crate::core::CoreService;
use crate::i18n::t;

/// The log tab.
#[derive(IntoElement)]
pub(super) struct Log;

impl RenderOnce for Log {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let status = CoreService::status(cx);
        let log = CoreService::log(cx);
        let muted = cx.theme().muted_foreground;
        let success = cx.theme().success;
        let danger = cx.theme().danger;

        let session = match log.path.as_ref() {
            Some(path) => v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(if log.running { success } else { muted })
                        .child(if log.running {
                            t!(
                                "app.bottom_panel.log_recording",
                                frames = log.frames.to_string(),
                                dropped = log.dropped.to_string(),
                                bytes = log.bytes.to_string(),
                            )
                        } else {
                            t!(
                                "app.bottom_panel.log_saved",
                                frames = log.frames.to_string(),
                                dropped = log.dropped.to_string(),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(path.display().to_string()),
                )
                .into_any_element(),
            None => div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.bottom_panel.log_none"))
                .into_any_element(),
        };

        let mut activity: Vec<AnyElement> = Vec::new();

        if let Some(trace) = status.last_trace.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.trace_saved",
                        frames = trace.frames.to_string(),
                        path = trace.path.display().to_string(),
                    ))
                    .into_any_element(),
            );
        }

        if let Some(save) = status.last_save.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.saved",
                        cells = save.cells.to_string(),
                        tune = save.path.display().to_string(),
                    ))
                    .into_any_element(),
            );
        }

        if let Some(write) = status.last_write.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.write_done",
                        pages = write.pages.to_string(),
                        cells = write.cells.to_string(),
                    ))
                    .into_any_element(),
            );
        }

        if let Some(burn) = status.last_burn.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.burn_done",
                        pages = burn.pages.to_string(),
                    ))
                    .into_any_element(),
            );
        }

        if let Some(import) = status.last_import.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.import_done",
                        cells = import.cells.to_string(),
                        path = import.path.display().to_string(),
                    ))
                    .into_any_element(),
            );
        }

        if let Some(revert) = status.last_revert.as_ref() {
            activity.push(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!(
                        "app.bottom_panel.revert_done",
                        cells = revert.cells.to_string(),
                    ))
                    .into_any_element(),
            );
        }

        let journal = status.journal.as_ref().map(|journal| {
            let rows: Vec<AnyElement> =
                journal
                    .entries
                    .iter()
                    .rev()
                    .take(100)
                    .map(|entry| {
                        let color = if entry.result == "ok" {
                            success
                        } else if entry.result == "failed" {
                            danger
                        } else {
                            muted
                        };

                        h_flex()
                            .gap_2()
                            .py_0p5()
                            .text_xs()
                            .child(div().w_48().text_color(muted).child(entry.at.clone()))
                            .child(
                                div()
                                    .w_20()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(entry.operation.clone()),
                            )
                            .child(div().w_20().child(entry.phase.clone()))
                            .child(div().w_24().child(format!("{} cells", entry.cells)))
                            .child(div().flex_1().text_color(color).child(
                                if entry.detail.is_empty() {
                                    entry.result.clone()
                                } else {
                                    format!("{} · {}", entry.result, entry.detail)
                                },
                            ))
                            .into_any_element()
                    })
                    .collect();

            let path = journal.path.as_ref().map_or_else(
                || t!("app.journal.no_file").to_string(),
                |path| path.display().to_string(),
            );

            v_flex()
                .gap_1()
                .child(heading(t!("app.journal.title")))
                .child(div().text_xs().text_color(muted).child(path))
                .child(
                    v_flex()
                        .gap_1()
                        .when(rows.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(t!("app.journal.empty")),
                            )
                        })
                        .children(rows),
                )
                .into_any_element()
        });

        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(heading(t!("app.bottom_panel.section_session_log")))
                    .child(session),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(heading(t!("app.bottom_panel.section_activity")))
                    .when(activity.is_empty(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(t!("app.bottom_panel.log_empty")),
                        )
                    })
                    .children(activity),
            )
            .when_some(journal, |this, journal| this.child(journal))
    }
}

/// One titled group of log lines.
fn heading(title: SharedString) -> impl IntoElement {
    div().text_xs().font_weight(FontWeight::MEDIUM).child(title)
}
