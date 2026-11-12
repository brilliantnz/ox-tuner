//! Destructive ECU operations: confirmations, the write journal and snapshots.
//!
//! Every dialog here names the object and the verb, states what will happen and
//! offers one explicit commit button. Safety policy still lives in the core
//! (ADR-0011); these dialogs only ask for the intent the core requires.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, FontWeight, IntoElement, ParentElement as _, Styled as _, Window, div, px};

use crate::app::dialog;
use crate::core::{self, CoreService};
use crate::i18n::t;

/// Confirms syncing tune values that differ from ECU RAM.
pub(crate) fn show_write(window: &mut Window, cx: &mut App) {
    let status = CoreService::status(cx);
    let writes_enabled = status.writes_enabled;
    let pending = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.pending_write.cells);
    let halted = status.write_halted.clone();
    let connected = status.connection.is_some();

    dialog::open(window, cx, move |dialog, _, cx| {
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        let ready = writes_enabled && connected && halted.is_none() && pending > 0;
        dialog
            .title(t!("app.write.title"))
            .width(px(540.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .child(t!("app.write.body", cells = pending.to_string())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.write.snapshot")),
                    )
                    .when(!writes_enabled, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.write.writes_off")),
                        )
                    })
                    .when(!connected, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.write.not_connected")),
                        )
                    })
                    .when_some(halted.clone(), |this, reason| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(danger)
                                        .child(t!("app.write.halted", reason = reason)),
                                )
                                .child(
                                    Button::new("write-acknowledge")
                                        .label(t!("app.write.acknowledge"))
                                        .small()
                                        .ghost()
                                        .on_click(|_, window, cx| {
                                            core::acknowledge_write_failure(cx);
                                            window.close_dialog(cx);
                                        }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.write.verify")),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("write-cancel")
                            .label(t!("app.write.cancel"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("write-confirm")
                            .label(t!("app.write.confirm"))
                            .primary()
                            .disabled(!ready)
                            .on_click(|_, window, cx| {
                                core::write_to_ram(cx);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// Confirms burning RAM to flash.
pub(crate) fn show_burn(window: &mut Window, cx: &mut App) {
    let status = CoreService::status(cx);
    let writes_enabled = status.writes_enabled;
    let pending = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.pending_burn.cells);
    let unsent = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.pending_write.cells);
    let connected = status.connection.is_some();
    let halted = status.write_halted.clone();
    let pages = status
        .connection
        .as_ref()
        .map_or(0, |connection| connection.pages);

    dialog::open(window, cx, move |dialog, _, cx| {
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        let ready = writes_enabled && connected && halted.is_none() && unsent == 0;
        dialog
            .title(t!("app.burn.title"))
            .width(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(danger)
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("app.burn.warning")),
                    )
                    .child(div().text_sm().child(t!(
                        "app.burn.body",
                        pages = pages.to_string(),
                        cells = pending.to_string()
                    )))
                    .when(unsent > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.burn.unsent", cells = unsent.to_string())),
                        )
                    })
                    .when(!writes_enabled, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.write.writes_off")),
                        )
                    })
                    .when(!connected, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.write.not_connected")),
                        )
                    })
                    .when_some(halted.clone(), |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.write.halted", reason = reason)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.burn.snapshot")),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("burn-cancel")
                            .label(t!("app.burn.cancel"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("burn-confirm")
                            .label(t!("app.burn.confirm"))
                            .danger()
                            .disabled(!ready)
                            .on_click(|_, window, cx| {
                                core::burn_to_flash(cx);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// Confirms discarding local edits.
pub(crate) fn show_revert(window: &mut Window, cx: &mut App) {
    let unsaved = CoreService::status(cx)
        .tune_status
        .as_ref()
        .map_or(0, |status| status.unsaved.cells);

    dialog::open(window, cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        dialog
            .title(t!("app.revert.title"))
            .width(px(500.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(t!("app.revert.body", cells = unsaved.to_string())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.revert.snapshot")),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("revert-cancel")
                            .label(t!("app.revert.cancel"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("revert-confirm")
                            .label(t!("app.revert.confirm"))
                            .danger()
                            .disabled(unsaved == 0)
                            .on_click(|_, window, cx| {
                                core::revert_tune(cx);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// Explains the write switch and turns it on after an explicit confirmation.
pub(crate) fn show_enable_writes(window: &mut Window, cx: &mut App) {
    let already = CoreService::status(cx).writes_enabled;
    let acknowledged = Rc::new(Cell::new(false));

    dialog::open(window, cx, move |dialog, _, cx| {
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        let checked = acknowledged.get();
        dialog
            .title(if already {
                t!("app.writes.disable_title")
            } else {
                t!("app.writes.enable_title")
            })
            .width(px(560.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(danger)
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("app.writes.warning")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.writes.rules")),
                    )
                    .when(!already, |this| {
                        this.child(
                            Checkbox::new("writes-acknowledge")
                                .label(t!("app.writes.confirm_check"))
                                .checked(checked)
                                .on_click({
                                    let acknowledged = Rc::clone(&acknowledged);
                                    move |checked, _, cx| {
                                        acknowledged.set(*checked);
                                        cx.refresh_windows();
                                    }
                                }),
                        )
                    }),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("writes-cancel")
                            .label(t!("app.writes.cancel"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("writes-confirm")
                            .label(if already {
                                t!("app.writes.disable")
                            } else {
                                t!("app.writes.enable")
                            })
                            .danger()
                            .disabled(!already && !checked)
                            .on_click(move |_, window, cx| {
                                core::set_writes_enabled(!already, cx);
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}

/// Shows the project's write journal, newest last.
pub(crate) fn show_journal(window: &mut Window, cx: &mut App) {
    dialog::open(window, cx, |dialog, _, cx| {
        let journal = CoreService::status(cx).journal.clone();
        let muted = cx.theme().muted_foreground;
        let rows: Vec<_> = journal
            .as_ref()
            .map(|journal| {
                journal
                    .entries
                    .iter()
                    .rev()
                    .take(100)
                    .map(|entry| {
                        let color = if entry.result == "ok" {
                            cx.theme().success
                        } else if entry.result == "failed" {
                            cx.theme().danger
                        } else {
                            muted
                        };
                        h_flex()
                            .gap_2()
                            .py_0p5()
                            .text_xs()
                            .child(div().w(px(180.)).text_color(muted).child(entry.at.clone()))
                            .child(
                                div()
                                    .w(px(70.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(entry.operation.clone()),
                            )
                            .child(div().w(px(70.)).child(entry.phase.clone()))
                            .child(div().w(px(90.)).child(format!("{} cells", entry.cells)))
                            .child(div().flex_1().text_color(color).child(
                                if entry.detail.is_empty() {
                                    entry.result.clone()
                                } else {
                                    format!("{} · {}", entry.result, entry.detail)
                                },
                            ))
                            .into_any_element()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let path = journal
            .as_ref()
            .and_then(|journal| journal.path.clone())
            .map_or_else(
                || t!("app.journal.no_file").to_string(),
                |path| path.display().to_string(),
            );

        dialog
            .title(t!("app.journal.title"))
            .width(px(760.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(muted).child(path))
                    .child(
                        div()
                            .max_h(px(420.))
                            .overflow_y_scrollbar()
                            .id("write-journal-list")
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_2()
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
                            ),
                    ),
            )
            .footer(
                h_flex().justify_end().child(
                    Button::new("journal-close")
                        .label(t!("app.journal.close"))
                        .ghost()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// Lists the project's snapshots with a restore button each.
pub(crate) fn show_snapshots(window: &mut Window, cx: &mut App) {
    dialog::open(window, cx, |dialog, _, cx| {
        let snapshots = CoreService::status(cx).snapshots.clone();
        let muted = cx.theme().muted_foreground;
        let rows: Vec<_> = snapshots
            .iter()
            .rev()
            .map(|path| {
                let restore = path.clone();
                h_flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .py_0p5()
                    .text_xs()
                    .child(div().flex_1().child(path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    )))
                    .child(
                        Button::new(gpui_kit::ElementId::Name(
                            format!("restore-snapshot-{}", path.display()).into(),
                        ))
                        .label(t!("app.snapshots.restore"))
                        .small()
                        .ghost()
                        .on_click(move |_, window, cx| {
                            core::restore_snapshot(&restore, cx);
                            window.close_dialog(cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect();

        dialog
            .title(t!("app.snapshots.title"))
            .width(px(620.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.snapshots.hint")),
                    )
                    .child(
                        div()
                            .max_h(px(420.))
                            .overflow_y_scrollbar()
                            .id("snapshot-list")
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_2()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .when(rows.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(t!("app.snapshots.empty")),
                                        )
                                    })
                                    .children(rows),
                            ),
                    ),
            )
            .footer(
                h_flex().justify_end().child(
                    Button::new("snapshots-close")
                        .label(t!("app.snapshots.close"))
                        .ghost()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    });
}

/// Asks for consent to import a tune with a different signature (S9).
pub(crate) fn show_import_consent(window: &mut Window, cx: &mut App) {
    let Some(path) = CoreService::status(cx).import_consent.clone() else {
        return;
    };

    dialog::open(window, cx, move |dialog, _, cx| {
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        dialog
            .title(t!("app.import.consent_title"))
            .width(px(580.))
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(danger)
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("app.import.consent_warning")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(path.display().to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.import.consent_body")),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("import-consent-cancel")
                            .label(t!("app.import.cancel"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("import-consent-confirm")
                            .label(t!("app.import.consent_confirm"))
                            .danger()
                            .on_click({
                                let path = path.clone();
                                move |_, window, cx| {
                                    core::import_tune_with_consent(&path, cx);
                                    window.close_dialog(cx);
                                }
                            }),
                    ),
            )
    });
}

/// Shows the cells that differ between the working image and a fresh ECU read.
pub(crate) fn show_conflicts(window: &mut Window, cx: &mut App) {
    let Some(read) = CoreService::status(cx).pages_read.clone() else {
        return;
    };
    if read.conflicts.is_empty() {
        return;
    }

    let total = read.conflicts.len();
    let rows: Vec<String> = read
        .conflicts
        .iter()
        .take(8)
        .map(|conflict| {
            format!(
                "{} [{}:{}]  {}  ->  {}",
                conflict.name, conflict.row, conflict.column, conflict.working, conflict.ecu
            )
        })
        .collect();

    dialog::open(window, cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        dialog
            .title(t!("app.conflict.title"))
            .width(px(620.))
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .child(t!("app.conflict.body", count = total.to_string())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.conflict.sample")),
                    )
                    .children(rows.clone().into_iter().map(|row| {
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(row)
                    })),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("conflict-keep")
                            .label(t!("app.conflict.keep"))
                            .ghost()
                            .on_click(|_, window, cx| {
                                core::resolve_conflicts(
                                    oxtuner_core::ConflictResolution::KeepWorking,
                                    cx,
                                );
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("conflict-ecu")
                            .label(t!("app.conflict.use_ecu"))
                            .primary()
                            .on_click(|_, window, cx| {
                                core::resolve_conflicts(
                                    oxtuner_core::ConflictResolution::UseEcu,
                                    cx,
                                );
                                window.close_dialog(cx);
                            }),
                    ),
            )
    });
}
