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
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div, px,
};

use crate::app::dialog;
use crate::core::{self, CoreService};
use crate::i18n::t;

/// The reasons a write or burn cannot start, in one place.
///
/// Both dialogs name the same blockers; only the conflict sentence differs, so
/// the caller passes it already translated.
fn safety_warnings(
    cx: &App,
    conflict: Option<SharedString>,
    writes_enabled: bool,
    connected: bool,
    halted: Option<String>,
) -> impl IntoElement {
    let danger = cx.theme().danger;
    v_flex()
        .gap_1()
        .when_some(conflict, |this, text| {
            this.child(div().text_xs().text_color(danger).child(text))
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
        .when_some(halted, |this, reason| {
            this.child(
                div()
                    .text_xs()
                    .text_color(danger)
                    .child(t!("app.write.halted", reason = reason)),
            )
        })
}

/// Confirms syncing tune values that differ from ECU RAM.
pub(crate) fn show_write(window: &mut Window, cx: &mut App) {
    let status = CoreService::status(cx);
    let writes_enabled = status.writes_enabled;
    let pending = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.pending_write.cells);
    let conflicts = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.conflicts);
    let halted = status.write_halted.clone();
    let connected = status.connection.is_some();

    dialog::open(window, cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let ready =
            writes_enabled && connected && halted.is_none() && conflicts == 0 && pending > 0;
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
                    .child(safety_warnings(
                        cx,
                        (conflicts > 0)
                            .then(|| t!("app.write.conflicts", cells = conflicts.to_string())),
                        writes_enabled,
                        connected,
                        halted.clone(),
                    ))
                    .when_some(halted.clone(), |this, _| {
                        this.child(
                            Button::new("write-acknowledge")
                                .label(t!("app.write.acknowledge"))
                                .small()
                                .ghost()
                                .on_click(|_, window, cx| {
                                    core::acknowledge_write_failure(cx);
                                    window.close_dialog(cx);
                                }),
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

/// The burn confirmation's main sentence.
fn burn_body(flash_known: bool, pending: usize, pages: usize) -> SharedString {
    if flash_known {
        t!(
            "app.burn.body",
            pages = pages.to_string(),
            cells = pending.to_string()
        )
    } else {
        t!("app.burn.body_unknown")
    }
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
    let conflicts = status
        .tune_status
        .as_ref()
        .map_or(0, |status| status.conflicts);
    let flash_known = status
        .tune_status
        .as_ref()
        .is_some_and(|status| status.flash_known);
    let connected = status.connection.is_some();
    let halted = status.write_halted.clone();
    let pages = status
        .connection
        .as_ref()
        .map_or(0, |connection| connection.pages);

    dialog::open(window, cx, move |dialog, _, cx| {
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;
        let ready =
            writes_enabled && connected && halted.is_none() && conflicts == 0 && unsent == 0;
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
                    .child(
                        div()
                            .text_sm()
                            .child(burn_body(flash_known, pending, pages)),
                    )
                    .child(safety_warnings(
                        cx,
                        (conflicts > 0)
                            .then(|| t!("app.burn.conflicts", cells = conflicts.to_string())),
                        writes_enabled,
                        connected,
                        halted.clone(),
                    ))
                    .when(unsent > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.burn.unsent", cells = unsent.to_string())),
                        )
                    })
                    .when(connected && !flash_known, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(t!("app.burn.flash_unknown")),
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

/// The project-versus-ECU comparison table.
fn conflict_table(
    cx: &App,
    rows: Vec<oxtuner_core::ConflictReport>,
    hidden: usize,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let border = cx.theme().border;
    let mono = cx.theme().mono_font_family.clone();

    v_flex()
        .border_1()
        .border_color(border)
        .rounded(cx.theme().radius)
        .overflow_hidden()
        .child(
            h_flex()
                .gap_3()
                .px_2()
                .py_1()
                .text_xs()
                .text_color(muted)
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .w(px(200.))
                        .min_w_0()
                        .truncate()
                        .child(t!("app.conflict.column_variable")),
                )
                .child(
                    div()
                        .w(px(64.))
                        .min_w_0()
                        .truncate()
                        .child(t!("app.conflict.column_cell")),
                )
                .child(div().flex_1().child(t!("app.conflict.column_project")))
                .child(div().flex_1().child(t!("app.conflict.column_ecu"))),
        )
        .children(rows.into_iter().map(move |row| {
            h_flex()
                .gap_3()
                .px_2()
                .py_1()
                .text_sm()
                .child(div().w(px(200.)).min_w_0().child(row.name))
                .child(div().w(px(64.)).text_xs().text_color(muted).child(
                    if row.row == 0 && row.column == 0 {
                        "—".to_owned()
                    } else {
                        format!("[{}:{}]", row.row, row.column)
                    },
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .child(row.working),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(mono.clone())
                        .child(row.ecu),
                )
        }))
        .when(hidden > 0, |this| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.conflict.more", count = hidden.to_string())),
            )
        })
}

/// Opens the queued review prompt, if one is still owed.
pub(crate) fn retry_pending_review(cx: &mut App) {
    if !CoreService::status(cx).review_pending {
        return;
    }
    let Some(window) = crate::app::window::main(cx) else {
        return;
    };
    window
        .update(cx, |_, window, cx| show_conflicts(window, cx))
        .ok();
}

/// Shows the cells that differ between the project tune and the ECU tune.
#[expect(
    clippy::too_many_lines,
    reason = "the dialog reads its live gates, table and two choices in one builder"
)]
pub(crate) fn show_conflicts(window: &mut Window, cx: &mut App) {
    let status = CoreService::status(cx);
    let Some(read) = status.pages_read.clone() else {
        CoreService::status_mut(cx).review_pending = false;
        return;
    };
    // A dialog for a difference the core has already decided would offer
    // choices that no longer apply; every operation that decides clears the
    // list. The status can lag the read by one refresh, so it is not consulted.
    if read.conflicts.is_empty() {
        CoreService::status_mut(cx).review_pending = false;
        return;
    }

    // A read-only project cannot save either choice, so the dialog would be a
    // modal with both buttons disabled; the status bar carries the reason.
    let blocked = status
        .project
        .as_ref()
        .is_some_and(|project| project.read_only)
        || status.project_missing;
    if blocked {
        CoreService::status_mut(cx).review_pending = false;
        return;
    }

    // The review is the only path that satisfies the core's write gate. It
    // replaces whatever dialog is in the way instead of being dropped.
    if window.has_active_dialog(cx) {
        window.close_dialog(cx);
    }

    let total = read.conflicts.len();
    let rows: Vec<oxtuner_core::ConflictReport> = read.conflicts.iter().take(10).cloned().collect();
    let hidden = total.saturating_sub(rows.len());

    let opened = dialog::open(window, cx, move |dialog, _, cx| {
        let status = CoreService::status(cx);
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let connected = status.connection.is_some() && status.link_lost.is_none();
        let read_only = status
            .project
            .as_ref()
            .is_some_and(|project| project.read_only)
            || status.project_missing;
        let signature_ok = status.connection.as_ref().is_some_and(|connection| {
            !connection.read_only && connection.verdict == oxtuner_core::SignatureVerdict::Match
        });
        let ready = status.writes_enabled
            && connected
            && signature_ok
            && !read_only
            && status.write_halted.is_none();
        let ecu_ready = connected && !read_only;

        dialog
            .title(t!("app.conflict.title"))
            .width(px(680.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(if total == 1 {
                        t!("app.conflict.body_one")
                    } else {
                        t!("app.conflict.body", count = total.to_string())
                    }))
                    .child(conflict_table(cx, rows.clone(), hidden))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.conflict.keep_hint")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.conflict.use_ecu_hint")),
                    )
                    .when(read_only, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.conflict.read_only")),
                        )
                    })
                    .when(!read_only, |this| {
                        this.child(safety_warnings(
                            cx,
                            None,
                            status.writes_enabled,
                            connected,
                            status.write_halted.clone(),
                        ))
                    })
                    .when(connected && !signature_ok, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.conflict.signature")),
                        )
                    }),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("conflict-project")
                            .label(t!("app.conflict.keep"))
                            .outline()
                            .disabled(!ready)
                            .on_click(|_, window, cx| {
                                core::resolve_conflicts(
                                    oxtuner_core::ConflictResolution::UseProject,
                                    cx,
                                );
                                window.close_dialog(cx);
                            }),
                    )
                    .child(
                        Button::new("conflict-ecu")
                            .label(t!("app.conflict.use_ecu"))
                            .primary()
                            .disabled(!ecu_ready)
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

    CoreService::status_mut(cx).review_pending = !opened;
}
