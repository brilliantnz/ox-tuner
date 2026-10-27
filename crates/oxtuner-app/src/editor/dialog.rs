//! The dialog editor: a dialog's scalar constants as an editable list.
//!
//! Tables tune maps; dialogs tune constants — fuel load, injector size, ego
//! limits. Each row names its variable and shows its user-unit value; clicking
//! a writable row edits it in place. Enter commits and moves to the next
//! writable row, so a tuner walks down the list; Escape cancels. The core stays
//! the only owner of tune values: a commit becomes a scalar command and the
//! echoed snapshot clears the optimistic value.

use std::{collections::HashMap, path::PathBuf};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::Button;
use gpui_kit::component::toolbar::Toolbar;
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use oxtuner_core::{DialogFieldKind, DialogSummary, Failure, FailureCode, SaveSummary};

use crate::core::CoreService;
use crate::i18n::t;
use crate::table::{EditOp, parse_edit};

/// An in-progress row edit.
struct RowEditing {
    /// Index into the snapshot's fields.
    index: usize,
    /// Text as typed.
    text: String,
    /// Whether the last commit attempt could not parse the text.
    invalid: bool,
}

/// The dialog editor for the node selected in the navigation tree.
pub(crate) struct DialogEditor {
    focus: FocusHandle,
    /// The menu label, shown until the snapshot arrives and if it never does.
    label: SharedString,
    /// The dialog id the selection asked for.
    requested: Option<String>,
    /// Last seen snapshot; the grid pattern of render-from-status would repaint
    /// under the user's edit, so the snapshot is owned here instead.
    dialog: Option<DialogSummary>,
    /// Last save seen, so its echo refreshes the "changed" marks.
    last_save: Option<SaveSummary>,
    /// The failure that stopped this dialog from appearing or saving.
    failure: Option<Failure>,
    /// Snapshots by dialog id, so a dialog that was open once draws on the next
    /// click instead of waiting for the core to answer again.
    cache: HashMap<String, DialogSummary>,
    /// The project the cache belongs to, so another folder starts clean.
    project_root: Option<PathBuf>,
    /// The tune revision the cache was filled at. An edit or a save elsewhere
    /// can move a value another dialog shows, so the cache drops then too.
    cache_revision: u64,
    /// The row being typed into, by field index.
    editing: Option<RowEditing>,
    /// Optimistic values not yet echoed by the core.
    pending: HashMap<String, f64>,
    _core: Subscription,
}

impl DialogEditor {
    /// An editor with no dialog loaded yet.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let _core = cx.observe_global::<CoreService>(|this, cx| this.sync(cx));
        Self {
            focus: cx.focus_handle(),
            label: SharedString::default(),
            requested: None,
            dialog: None,
            last_save: None,
            failure: None,
            cache: HashMap::new(),
            project_root: None,
            cache_revision: 0,
            editing: None,
            pending: HashMap::new(),
            _core,
        }
    }

    /// Points the editor at a selected node.
    pub(crate) fn show(
        &mut self,
        label: SharedString,
        target: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        self.label = label;
        let Some(name) = target else {
            self.requested = None;
            self.failure = None;
            cx.notify();
            return;
        };

        let already_shown = self.requested.as_deref() == Some(name)
            && self.dialog.as_ref().is_some_and(|dialog| dialog.id == name);
        if already_shown {
            cx.notify();
            return;
        }

        self.requested = Some(name.to_owned());
        self.failure = None;
        self.editing = None;
        self.pending.clear();
        let existing = CoreService::status(cx)
            .dialog
            .as_ref()
            .filter(|dialog| dialog.id == name)
            .cloned();
        if let Some(dialog) = existing.or_else(|| self.cache.get(name).cloned()) {
            self.dialog = Some(dialog);
        } else {
            self.dialog = None;
            crate::core::read_dialog(name, cx);
        }
        cx.notify();
    }

    /// Follows core events: a new snapshot, a failure, or a save that makes the
    /// changed marks stale.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let mut snapshot = None;
        let mut failure = None;
        let mut edit_failed = false;
        let mut cleared = false;
        let mut saved = false;
        let mut reload = None;
        let mut dialog_matches = false;

        // A different project invalidates every snapshot: the values and the
        // changed marks belong to the folder that produced them. An edit or a
        // save moves the tune itself, so cached rows elsewhere are stale too.
        let status = CoreService::status(cx);
        let root = status.project.as_ref().map(|project| project.root.clone());
        let revision = status.tune_revision;
        if root != self.project_root || revision != self.cache_revision {
            self.project_root = root;
            self.cache_revision = revision;
            self.cache.clear();
        }

        {
            let status = CoreService::status(cx);
            if let Some(requested) = &self.requested {
                match status
                    .dialog
                    .as_ref()
                    .filter(|dialog| &dialog.id == requested)
                {
                    Some(dialog) => {
                        dialog_matches = true;
                        if self.dialog.as_ref() != Some(dialog) {
                            snapshot = Some(dialog.clone());
                        }
                    }
                    None => {
                        if status
                            .failure
                            .as_ref()
                            .is_some_and(|failure| failure.code == FailureCode::DialogUnavailable)
                        {
                            cleared = true;
                        }
                    }
                }
            }
            if let Some(failure_now) = &status.failure
                && matches!(
                    failure_now.code,
                    FailureCode::DialogUnavailable | FailureCode::DialogEditRejected
                )
            {
                edit_failed = failure_now.code == FailureCode::DialogEditRejected;
                failure = Some(failure_now.clone());
            }
            if status.last_save != self.last_save {
                self.last_save = status.last_save.clone();
                if self.last_save.is_some() {
                    saved = true;
                    reload = self.requested.clone();
                }
            }
        }

        if let Some(dialog) = snapshot {
            self.failure = None;
            // An echo confirms only the values it carries; anything still
            // different stays optimistic.
            self.pending
                .retain(|variable, pending| field_value(&dialog, variable) != Some(*pending));
            self.cache.insert(dialog.id.clone(), dialog.clone());
            self.dialog = Some(dialog);
        }
        if dialog_matches {
            self.failure = None;
        }
        if cleared {
            if let Some(name) = &self.requested {
                self.cache.remove(name);
            }
            self.dialog = None;
            self.editing = None;
            self.pending.clear();
        }
        if failure.is_some() {
            self.failure = failure;
        }
        if edit_failed {
            self.pending.clear();
            if let Some(editing) = self.editing.as_mut() {
                editing.invalid = true;
            }
        }
        if saved && let Some(name) = reload {
            crate::core::read_dialog(&name, cx);
        }
        cx.notify();
    }

    /// The displayed value of one field, optimistic edits first.
    fn value(&self, dialog: &DialogSummary, index: usize) -> Option<f64> {
        let field = dialog.fields.get(index)?;
        self.pending.get(&field.variable).copied().or(field.value)
    }

    /// Starts editing a row, seeded with its current value.
    fn begin_edit(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let writable = self
            .dialog
            .as_ref()
            .and_then(|dialog| dialog.fields.get(index))
            .is_some_and(|field| field.writable);
        if !writable {
            return;
        }
        let text = self
            .dialog
            .as_ref()
            .and_then(|dialog| self.value(dialog, index))
            .map(|value| {
                let digits = self
                    .dialog
                    .as_ref()
                    .and_then(|dialog| dialog.fields.get(index))
                    .map_or(0, |field| field.digits);
                format!("{value:.digits$}", digits = usize::from(digits))
            })
            .unwrap_or_default();
        self.editing = Some(RowEditing {
            index,
            text,
            invalid: false,
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Flips a checkbox or radio row.
    ///
    /// A toggle has two states, so a click is the whole edit; there is no text
    /// to type and no reason to open an input for it.
    fn toggle_field(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let Some(field) = dialog.fields.get(index) else {
            return;
        };
        if !field.writable || field.kind != DialogFieldKind::Toggle {
            return;
        }
        let current = self.value(dialog, index).unwrap_or(0.0);
        let next = if current == 0.0 { 1.0 } else { 0.0 };
        let (dialog_id, variable) = (dialog.id.clone(), field.variable.clone());
        self.pending.insert(variable.clone(), next);
        self.failure = None;
        crate::core::edit_scalar(&dialog_id, &variable, next, cx);
        cx.notify();
    }

    /// Commits the in-progress edit; empty text skips the row.
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.as_ref() else {
            return;
        };
        let index = editing.index;
        let text = editing.text.clone();
        let operation = match parse_edit(&text) {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                // Nothing typed: leave the row, keep walking.
                self.editing = None;
                self.edit_next(index, cx);
                cx.notify();
                return;
            }
            Err(_) => {
                self.editing = Some(RowEditing {
                    index,
                    text,
                    invalid: true,
                });
                cx.notify();
                return;
            }
        };
        let (dialog_id, variable, current) = match self.dialog.as_ref().and_then(|dialog| {
            dialog.fields.get(index).map(|field| {
                (
                    dialog.id.clone(),
                    field.variable.clone(),
                    self.value(dialog, index),
                )
            })
        }) {
            Some(found) => found,
            None => {
                self.editing = None;
                cx.notify();
                return;
            }
        };
        let Some(current) = current else {
            self.editing = None;
            cx.notify();
            return;
        };
        let (operation, amount) = operation;
        let value = match operation {
            EditOp::Set => amount,
            EditOp::Add => current + amount,
            EditOp::Multiply => current * amount,
            EditOp::Divide => current / amount,
            EditOp::Percent => current * (1.0 + amount / 100.0),
        };
        if !value.is_finite() {
            self.editing = Some(RowEditing {
                index,
                text,
                invalid: true,
            });
            cx.notify();
            return;
        }
        self.pending.insert(variable.clone(), value);
        self.editing = None;
        crate::core::edit_scalar(&dialog_id, &variable, value, cx);
        self.edit_next(index, cx);
        cx.notify();
    }

    /// Moves the edit to the next writable row, if there is one.
    fn edit_next(&mut self, from: usize, cx: &mut Context<Self>) {
        let next = self.dialog.as_ref().and_then(|dialog| {
            dialog
                .fields
                .iter()
                .enumerate()
                .skip(from + 1)
                .find(|(_, field)| field.writable)
                .map(|(index, _)| index)
        });
        if let Some(index) = next {
            let text = self
                .dialog
                .as_ref()
                .and_then(|dialog| self.value(dialog, index))
                .map(|value| {
                    let digits = self
                        .dialog
                        .as_ref()
                        .and_then(|dialog| dialog.fields.get(index))
                        .map_or(0, |field| field.digits);
                    format!("{value:.digits$}", digits = usize::from(digits))
                })
                .unwrap_or_default();
            self.editing = Some(RowEditing {
                index,
                text,
                invalid: false,
            });
        }
        let _ = cx;
    }

    /// Moves the edit one writable row up or down, committing first.
    fn commit_and_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let from = self.editing.as_ref().map(|editing| editing.index);
        self.commit_edit(cx);
        // `commit_edit` already advanced forward; step back when going up.
        if delta < 0 {
            let _ = from;
            self.editing = None;
            if let Some(from) = from {
                let previous = self.dialog.as_ref().and_then(|dialog| {
                    dialog
                        .fields
                        .iter()
                        .enumerate()
                        .take(from)
                        .rev()
                        .find(|(_, field)| field.writable)
                        .map(|(index, _)| index)
                });
                if let Some(index) = previous {
                    let text = self
                        .dialog
                        .as_ref()
                        .and_then(|dialog| self.value(dialog, index))
                        .map(|value| {
                            let digits = self
                                .dialog
                                .as_ref()
                                .and_then(|dialog| dialog.fields.get(index))
                                .map_or(0, |field| field.digits);
                            format!("{value:.digits$}", digits = usize::from(digits))
                        })
                        .unwrap_or_default();
                    self.editing = Some(RowEditing {
                        index,
                        text,
                        invalid: false,
                    });
                }
            }
        }
        cx.notify();
    }

    /// The keyboard contract: Enter commits and advances, Escape cancels.
    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.editing.is_none() {
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" | "tab" => self.commit_edit(cx),
            "escape" => {
                self.editing = None;
                cx.notify();
            }
            "backspace" => {
                if let Some(editing) = self.editing.as_mut() {
                    editing.text.pop();
                    editing.invalid = false;
                }
                cx.notify();
            }
            "up" => self.commit_and_move(-1, cx),
            "down" => self.commit_and_move(1, cx),
            _ => {
                if let Some(text) = &event.keystroke.key_char {
                    for character in text.chars() {
                        let editable = character.is_ascii_digit()
                            || matches!(character, '.' | '-' | '+' | '*' | '/' | '=' | 'e' | 'E');
                        if editable && let Some(editing) = self.editing.as_mut() {
                            editing.text.push(character);
                            editing.invalid = false;
                        }
                    }
                    cx.notify();
                }
            }
        }
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = self.requested.clone() {
            crate::core::read_dialog(&name, cx);
            cx.notify();
        }
    }

    fn render_header(&self, dialog: Option<&DialogSummary>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let title: SharedString =
            dialog.map_or_else(|| self.label.clone(), |dialog| dialog.title.clone().into());
        let writable = dialog.is_some_and(|dialog| dialog.writable);
        let dirty = dialog.is_some_and(|dialog| dialog.changed > 0 || !self.pending.is_empty());

        let toolbar = Toolbar::new("dialog-toolbar").child(
            Button::new("dialog-save")
                .label(t!("app.dialog.save"))
                .tooltip(t!("app.dialog.save_tooltip"))
                .disabled(!writable || !dirty)
                .on_click(|_, _, cx| crate::core::save_tune(cx)),
        );

        h_flex()
            .h_10()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(title),
                    )
                    .when(!writable && dialog.is_some(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(t!("app.dialog.read_only")),
                        )
                    }),
            )
            .child(toolbar)
            .into_any_element()
    }

    fn render_rows(&self, dialog: &DialogSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut rows = Vec::new();
        let mut section = "";

        for (index, field) in dialog.fields.iter().enumerate() {
            if field.section != section && !field.section.is_empty() {
                section = &field.section;
                rows.push(
                    div()
                        .px_4()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(field.section.clone())
                        .into_any_element(),
                );
            }

            // Text the definition prints between values is not a row to edit.
            if field.kind == DialogFieldKind::Note {
                let color = if field.warning {
                    theme.danger
                } else {
                    theme.muted_foreground
                };
                rows.push(
                    div()
                        .px_4()
                        .py_1()
                        .text_sm()
                        .text_color(color)
                        .child(field.label.clone())
                        .into_any_element(),
                );
                continue;
            }

            let value = self.value(dialog, index);
            let text = value.map_or_else(
                || {
                    field
                        .text
                        .clone()
                        .unwrap_or_else(|| t!("app.dialog.no_value").to_string())
                },
                |value| {
                    if field.kind == DialogFieldKind::Toggle {
                        return if value == 0.0 {
                            t!("app.dialog.toggle_off").to_string()
                        } else {
                            t!("app.dialog.toggle_on").to_string()
                        };
                    }
                    let rendered = format!("{value:.digits$}", digits = usize::from(field.digits));
                    if field.units.is_empty() {
                        rendered
                    } else {
                        format!("{rendered} {}", field.units)
                    }
                },
            );
            let editing = self
                .editing
                .as_ref()
                .filter(|editing| editing.index == index);
            let invalid = editing.as_ref().is_some_and(|editing| editing.invalid);
            let shown: SharedString = editing.map_or_else(
                || text.clone().into(),
                |editing| editing.text.clone().into(),
            );
            let toggle = field.kind == DialogFieldKind::Toggle;

            rows.push(
                h_flex()
                    .min_h(px(32.))
                    .items_center()
                    .gap_3()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(field.label.clone()),
                    )
                    .child(
                        div()
                            .id(("dialog-field", index))
                            .w(px(160.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .text_sm()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .when(field.writable, |this| {
                                this.hover(|this| this.bg(theme.secondary_hover))
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                            if toggle {
                                                this.toggle_field(index, cx);
                                            } else {
                                                this.begin_edit(index, window, cx);
                                            }
                                        }),
                                    )
                            })
                            .when(!field.writable, |this| {
                                this.text_color(theme.muted_foreground)
                            })
                            .when_some(editing, |this, _| {
                                this.border_1().border_color(if invalid {
                                    theme.danger
                                } else {
                                    theme.info
                                })
                            })
                            .child(shown),
                    )
                    .into_any_element(),
            );
        }

        if !dialog.unavailable.is_empty() {
            rows.push(
                div()
                    .px_4()
                    .pt_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!(
                        "app.dialog.unavailable_panels",
                        names = dialog.unavailable.join(", ")
                    ))
                    .into_any_element(),
            );
        }

        v_flex()
            .id("dialog-fields")
            .flex_1()
            .min_h_0()
            .overflow_scroll()
            .py_2()
            .children(rows)
            .into_any_element()
    }

    fn render_content(
        &self,
        dialog: Option<&DialogSummary>,
        failure: Option<&Failure>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        if let Some(dialog) = dialog {
            if dialog.fields.is_empty() && dialog.unavailable.is_empty() {
                return div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t!("app.dialog.empty"))
                    .into_any_element();
            }
            return div()
                .flex_1()
                .min_h_0()
                .child(self.render_rows(dialog, cx))
                .into_any_element();
        }

        let body =
            v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .px_6()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(
                    if failure.is_some() {
                        t!("app.dialog.unavailable")
                    } else {
                        t!("app.dialog.loading")
                    },
                ));

        if let Some(failure) = failure {
            body.child(
                div()
                    .max_w(px(440.))
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(failure.message.clone()),
            )
            .child(
                Button::new("dialog-retry")
                    .label(t!("app.dialog.retry"))
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        }
    }

    fn render_status(&self, dialog: &DialogSummary, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let changed = dialog.changed + self.pending.len();

        h_flex()
            .h_8()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_4()
            .px_3()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(t!("app.dialog.hint"))
            .child(
                h_flex()
                    .items_center()
                    .gap_4()
                    .when(changed > 0, |this| {
                        this.child(
                            div()
                                .text_color(theme.warning)
                                .child(t!("app.dialog.changed", count = changed.to_string())),
                        )
                    })
                    .when_some(self.failure.as_ref(), |this, failure| {
                        this.child(
                            div()
                                .max_w(px(360.))
                                .truncate()
                                .text_color(theme.danger)
                                .child(failure.message.clone()),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Focusable for DialogEditor {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DialogEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let dialog = self.dialog.clone();
        let failure = self.failure.clone();
        let header = self.render_header(dialog.as_ref(), cx);
        let content = self.render_content(dialog.as_ref(), failure.as_ref(), cx);
        let status = dialog.as_ref().map(|dialog| self.render_status(dialog, cx));

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key(event, cx)),
            )
            .child(header)
            .child(content)
            .when_some(status, |this, status| this.child(status))
    }
}

/// The snapshot value of one variable, for echo reconciliation.
fn field_value(dialog: &DialogSummary, variable: &str) -> Option<f64> {
    dialog
        .fields
        .iter()
        .find(|field| field.variable == variable)
        .and_then(|field| field.value)
}
