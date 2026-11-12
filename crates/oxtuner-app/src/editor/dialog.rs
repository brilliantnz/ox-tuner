//! The dialog editor: a dialog's constants as editable cards.
//!
//! Tables tune maps; dialogs tune constants: fuel load, injector size, ego
//! limits. The definition groups those constants into panels, so each panel is
//! one card and each row is one value. A definition-supplied label list becomes
//! a select, a boolean becomes a switch, a raw number or string stays an input,
//! and printed text becomes an alert banner.
//!
//! The core stays the only owner of tune values. A control reports the value it
//! wants, a scalar command carries it to the core, and the echoed snapshot
//! clears the optimistic value. The editor keeps no tune state of its own.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::component::toolbar::Toolbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px, rems,
};
use oxtuner_core::{
    DialogAction, DialogField, DialogFieldKind, DialogView, Failure, FailureCode, GateState,
    SaveReport,
};
use oxtuner_ui::table::{EditOp, parse_edit};

use crate::core::CoreService;
use crate::i18n::t;
use crate::notifications::{self, NotificationDraft, NotificationKey, NotificationSeverity};
use crate::ui::constant::{self, ConstantInput, ConstantOption, ConstantSelect, InputKind};

/// What a visible row needs as its trailing control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    /// A number input.
    Number,
    /// A text input, read-only until the core can store strings.
    Text,
    /// A definition-supplied option list.
    Select,
}

/// The dialog editor for the node selected in the navigation tree.
pub(crate) struct DialogEditor {
    focus: FocusHandle,
    /// The menu label, shown until the snapshot arrives and if it never does.
    label: SharedString,
    /// The dialog id the selection asked for.
    requested: Option<String>,
    /// Last seen snapshot. The editor owns it instead of reading the status in
    /// render, because a repaint would otherwise replace the row being edited.
    dialog: Option<DialogView>,
    /// Last save seen, so its echo refreshes the "changed" marks.
    last_save: Option<SaveReport>,
    /// The failure that stopped this dialog from appearing or saving.
    failure: Option<Failure>,
    /// Snapshots by dialog id, so a dialog that was open once draws on the next
    /// click instead of waiting for the core to answer again.
    cache: HashMap<String, DialogView>,
    /// The project the cache belongs to, so another folder starts clean.
    project_root: Option<PathBuf>,
    /// The definition revision the open rows were read under. A project switch
    /// or a reloaded definition invalidates the values on screen.
    definition_revision: u64,
    /// The tune revision the cache was filled at.
    cache_revision: u64,
    /// The tune revision the open snapshot was built at.
    dialog_revision: u64,
    /// The failure serial the editor has already acted on. A status change that
    /// carries the same rejection must not overwrite a newer keystroke.
    failure_serial: u64,
    /// Optimistic values not yet echoed by the core.
    pending: HashMap<String, f64>,
    /// Per-row inputs, keyed by field index.
    inputs: HashMap<usize, ConstantInput>,
    /// Per-row option pickers, keyed by field index.
    selects: HashMap<usize, ConstantSelect>,
    /// The subscriptions that turn a control event into an edit.
    subscriptions: HashMap<usize, Subscription>,
    /// Set when the core refused an edit, so the next frame redraws the stored
    /// value even inside a control the user still has focus in.
    force_sync: bool,
    /// Whether this dialog already raised the power-cycle notice for its
    /// current set of changed settings.
    power_cycle_notified: bool,
    /// The dialog the controls were built for, so another dialog starts clean.
    widget_dialog: Option<String>,
    _core: Subscription,
}

impl DialogEditor {
    /// An editor with no dialog loaded yet.
    #[expect(
        clippy::used_underscore_binding,
        reason = "the underscore marks a subscription kept only for its lifetime"
    )]
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        let _core = cx.observe_global::<CoreService>(DialogEditor::sync);
        let (project_root, definition_revision, failure_serial) = {
            let status = CoreService::status(cx);
            (
                status.project.as_ref().map(|project| project.root.clone()),
                status.definition_revision,
                status.failure_serial,
            )
        };

        Self {
            focus: cx.focus_handle(),
            label: SharedString::default(),
            requested: None,
            dialog: None,
            last_save: None,
            failure: None,
            cache: HashMap::new(),
            project_root,
            definition_revision,
            cache_revision: 0,
            dialog_revision: 0,
            failure_serial,
            pending: HashMap::new(),
            inputs: HashMap::new(),
            selects: HashMap::new(),
            subscriptions: HashMap::new(),
            force_sync: false,
            power_cycle_notified: false,
            widget_dialog: None,
            _core,
        }
    }

    /// Points the editor at a selected node.
    pub(crate) fn show(
        &mut self,
        label: SharedString,
        target: Option<&str>,
        cx: &mut Context<'_, Self>,
    ) {
        self.label = label;

        let Some(name) = target else {
            self.requested = None;
            self.failure = None;
            cx.notify();
            return;
        };

        let status = CoreService::status(cx);
        let tune_revision = status.tune_revision;
        let stale = self.dialog_revision != tune_revision;
        let already_shown = self.requested.as_deref() == Some(name)
            && self.dialog.as_ref().is_some_and(|dialog| dialog.id == name);

        if already_shown && !stale {
            cx.notify();
            return;
        }

        self.requested = Some(name.to_owned());
        self.failure = None;
        self.pending.clear();
        self.power_cycle_notified = false;

        let existing = status
            .dialog
            .as_ref()
            .filter(|dialog| dialog.id == name)
            .cloned();

        if stale {
            self.dialog = None;
            crate::core::read_dialog(name, cx);
        } else if let Some(dialog) = existing.or_else(|| self.cache.get(name).cloned()) {
            self.dialog = Some(dialog);
            self.dialog_revision = tune_revision;
        } else {
            self.dialog = None;
            crate::core::read_dialog(name, cx);
        }

        self.publish_power_cycle_notice(cx);
        cx.notify();
    }

    /// The gate of the table or curve the open dialog embeds, when carried.
    ///
    /// A dialog that wraps a grid reads its snapshot even while the grid is in
    /// front, so the panel can decide whether the definition still offers that
    /// grid. `None` means the snapshot has not arrived or the dialog carries no
    /// such embed; callers keep the grid available then.
    pub(crate) fn embed_state(&self, target: &str) -> Option<GateState> {
        let cached = self
            .requested
            .as_deref()
            .and_then(|name| self.cache.get(name));
        let dialog = self.dialog.as_ref().or(cached)?;

        dialog
            .tables
            .iter()
            .chain(dialog.curves.iter())
            .find(|embed| embed.id.eq_ignore_ascii_case(target))
            .map(|embed| embed.gates)
    }

    /// Drops the rows a different project or definition invalidates.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        self.cache.clear();
        self.failure = None;
        self.dialog = None;
        self.dialog_revision = 0;
        self.pending.clear();
        self.power_cycle_notified = false;
        self.drop_controls();

        let Some(name) = self.requested.clone() else {
            return;
        };

        crate::core::read_dialog(&name, cx);
    }

    /// Drops every control and the subscription that keeps it alive.
    fn drop_controls(&mut self) {
        self.inputs.clear();
        self.selects.clear();
        self.subscriptions.clear();
        self.force_sync = false;
        self.widget_dialog = None;
    }

    /// Raises one notice while any visible setting still needs a power cycle.
    ///
    /// The notice lives in the notification surface, not in the board, so it
    /// can never push the cards down when it appears.
    fn publish_power_cycle_notice(&mut self, cx: &mut Context<'_, Self>) {
        let power_cycle_changed = self.dialog.as_ref().is_some_and(|dialog| {
            dialog
                .fields
                .iter()
                .any(|field| field.gates.visible && field.power_cycle && field.changed)
        });

        if power_cycle_changed && !self.power_cycle_notified {
            self.power_cycle_notified = true;
            notifications::publish(
                NotificationDraft::new(
                    NotificationSeverity::Warning,
                    t!("app.dialog.power_cycle").to_string(),
                    t!("app.dialog.power_cycle_changed").to_string(),
                )
                .key(NotificationKey::named("dialog-power-cycle")),
                cx,
            );
        } else if !power_cycle_changed {
            self.power_cycle_notified = false;
        }
    }

    /// Follows core events: a new snapshot, a failure, or a save that makes the
    /// changed marks stale.
    #[expect(
        clippy::too_many_lines,
        reason = "one observer folds the whole core status into the editor"
    )]
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let mut snapshot = None;
        let mut failure = None;
        let mut edit_failed = false;
        let mut cleared = false;
        let mut saved = false;
        let mut reload = None;
        let mut dialog_matches = false;

        // A different project invalidates every snapshot. An edit or a save
        // moves the tune itself, so cached rows elsewhere are stale too.
        let (root, revision, definition_revision, failure_serial) = {
            let status = CoreService::status(cx);
            (
                status.project.as_ref().map(|project| project.root.clone()),
                status.tune_revision,
                status.definition_revision,
                status.failure_serial,
            )
        };
        let new_failure = failure_serial != self.failure_serial;
        self.failure_serial = failure_serial;

        if root != self.project_root || definition_revision != self.definition_revision {
            self.project_root = root;
            self.definition_revision = definition_revision;
            self.cache_revision = revision;
            self.reload(cx);
        } else if revision != self.cache_revision {
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
                edit_failed = new_failure && failure_now.code == FailureCode::DialogEditRejected;
                failure = Some(failure_now.clone());
            }
            if status.last_save != self.last_save {
                self.last_save.clone_from(&status.last_save);
                if self.last_save.is_some() {
                    saved = true;
                    reload.clone_from(&self.requested);
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
            self.dialog_revision = revision;
        }

        if dialog_matches {
            self.failure = None;
        }
        if edit_failed {
            // The core refused the value, so the editor must not keep showing
            // it as if it were pending. The next frame also redraws the stored
            // value inside a control that still has focus.
            self.pending.clear();
            self.force_sync = true;
        }
        if cleared {
            if let Some(name) = &self.requested {
                self.cache.remove(name);
            }
            self.dialog = None;
            self.dialog_revision = 0;
            self.pending.clear();
            self.drop_controls();
        }
        self.publish_power_cycle_notice(cx);
        if failure.is_some() {
            self.failure = failure;
        }
        if saved && let Some(name) = reload {
            crate::core::read_dialog(&name, cx);
        }
        cx.notify();
    }

    /// The displayed value of one field, optimistic edits first.
    fn value(&self, dialog: &DialogView, index: usize) -> Option<f64> {
        let field = dialog.fields.get(index)?;

        self.pending.get(&field.variable).copied().or(field.value)
    }

    /// Builds or refreshes the controls for the rows on screen.
    ///
    /// Widget entities need a window, and the editor learns of a snapshot in an
    /// observer, so the controls are made on the first render after the data
    /// arrives. Every later frame only syncs values the user is not typing into.
    fn ensure_widgets(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };

        if self.widget_dialog.as_deref() != Some(dialog.id.as_str()) {
            self.inputs.clear();
            self.selects.clear();
            self.subscriptions.clear();
            self.force_sync = false;
            self.widget_dialog = Some(dialog.id.clone());
        }

        let force = std::mem::take(&mut self.force_sync);

        for (index, field) in dialog.fields.iter().enumerate() {
            if !field.gates.visible {
                continue;
            }

            match widget_kind(field) {
                None => {
                    self.inputs.remove(&index);
                    self.selects.remove(&index);
                    self.subscriptions.remove(&index);
                }
                Some(WidgetKind::Select) => {
                    self.inputs.remove(&index);

                    if !self.selects.contains_key(&index) {
                        let selected = constant::option_value(self.value(dialog, index));
                        let select = ConstantSelect::new(window, cx, &field.options, selected);
                        let subscription = subscribe_select(cx, index, select.state(), window);

                        self.selects.insert(index, select);
                        self.subscriptions.insert(index, subscription);
                    }

                    let selected = constant::option_value(self.value(dialog, index));

                    if let Some(select) = self.selects.get_mut(&index)
                        && (force || !select.is_focused(window, cx))
                    {
                        select.set_selected(selected, window, cx);
                    }
                }
                Some(kind @ (WidgetKind::Number | WidgetKind::Text)) => {
                    self.selects.remove(&index);

                    if !self.inputs.contains_key(&index) {
                        let text = constant::value_text(field, self.value(dialog, index));
                        let input_kind = if kind == WidgetKind::Number {
                            InputKind::Number
                        } else {
                            InputKind::Text
                        };
                        let input = ConstantInput::new(window, cx, text, input_kind);
                        let subscription = subscribe_input(cx, index, input.state(), window);

                        self.inputs.insert(index, input);
                        self.subscriptions.insert(index, subscription);
                    }

                    if let Some(input) = self.inputs.get(&index)
                        && (force || !input.is_focused(window, cx))
                    {
                        let text = constant::value_text(field, self.value(dialog, index));

                        input.set_text(text, window, cx);
                    }
                }
            }
        }
    }

    /// Commits a typed value after Enter or a focus change.
    fn commit_input(
        &mut self,
        index: usize,
        state: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let Some(field) = dialog.fields.get(index) else {
            return;
        };
        if !row_editable(field) || field.value.is_none() {
            return;
        }

        let text = state.read(cx).value().to_string();
        let Some(current) = self.value(dialog, index) else {
            return;
        };
        let Some(value) = edited_value_with_hex(current, &text, field.hex) else {
            return;
        };

        let value = clamp(value, field.minimum, field.maximum);
        let digits = field.digits;

        state.update(cx, |state, cx| {
            state.set_value(constant::number_text(value, digits), window, cx);
        });
        self.commit_value(index, value, cx);
    }

    /// Commits a picked option.
    fn commit_select(&mut self, index: usize, value: u32, cx: &mut Context<'_, Self>) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let Some(field) = dialog.fields.get(index) else {
            return;
        };
        if !row_editable(field) || field.kind != DialogFieldKind::Select {
            return;
        }

        self.commit_value(index, f64::from(value), cx);
    }

    /// Commits a switch position.
    fn set_toggle(&mut self, index: usize, checked: bool, cx: &mut Context<'_, Self>) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let Some(field) = dialog.fields.get(index) else {
            return;
        };
        if !row_editable(field) || field.kind != DialogFieldKind::Toggle {
            return;
        }

        let value = toggle_value(field.flip, checked);

        self.commit_value(index, value, cx);
    }

    /// Sends one value and marks it optimistic until the core echoes it.
    fn commit_value(&mut self, index: usize, value: f64, cx: &mut Context<'_, Self>) {
        let Some(dialog) = self.dialog.as_ref() else {
            return;
        };
        let Some(field) = dialog.fields.get(index) else {
            return;
        };
        if !row_editable(field) {
            return;
        }
        let Some(current) = self.value(dialog, index) else {
            return;
        };
        if constant::number_text(current, field.digits)
            == constant::number_text(value, field.digits)
        {
            return;
        }

        let variable = field.variable.clone();
        let dialog_id = dialog.id.clone();

        self.pending.insert(variable.clone(), value);
        self.failure = None;
        crate::core::edit_scalar(&dialog_id, &variable, field.index, value, cx);
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(name) = self.requested.clone() {
            crate::core::read_dialog(&name, cx);
            cx.notify();
        }
    }

    fn render_header(&self, dialog: Option<&DialogView>, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let title: SharedString =
            dialog.map_or_else(|| self.label.clone(), |dialog| dialog.title.clone().into());
        let writable = dialog.is_some_and(|dialog| dialog.writable);
        let dirty = dialog.is_some_and(|dialog| dialog.changed > 0 || !self.pending.is_empty());

        let toolbar = Toolbar::new("dialog-toolbar").child(
            Button::new("dialog-save")
                .label(t!("app.dialog.save"))
                .tooltip(t!("app.dialog.save_tooltip"))
                .primary()
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

    /// The panel cards, in document order.
    fn render_board(&self, dialog: &DialogView, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let groups = sections(&dialog.title, &dialog.fields);

        let mut board = v_flex()
            .debug_selector(|| "constant-board".to_string())
            .id("constant-board")
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_4()
            .gap_4();

        // The power-cycle notice is a notification now, so it does not move the
        // cards when it appears.

        for (ordinal, group) in groups.iter().enumerate() {
            let rows = group
                .items
                .iter()
                .map(|index| self.render_field(dialog, *index, &dialog.fields[*index], cx))
                .collect();

            board = board.child(constant::card(
                ("constant-card", ordinal),
                group.title.clone().into(),
                // The definition id used to sit under the card title.
                // dialog.id.clone().into(),
                rows,
                cx,
            ));
        }

        if let Some(actions) = command_card(dialog, cx) {
            board = board.child(actions);
        }

        if let Some(help) = help_card(dialog, cx) {
            board = board.child(help);
        }

        if !dialog.unmodeled.is_empty() {
            let names = dialog
                .unmodeled
                .iter()
                .map(|row| row.keyword.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            board = board.child(
                div()
                    .px_4()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!("app.dialog.unmodeled", names = names)),
            );
        }

        if !dialog.unavailable.is_empty() {
            board = board.child(
                div()
                    .px_4()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t!(
                        "app.dialog.unavailable_panels",
                        names = dialog.unavailable.join(", ")
                    )),
            );
        }

        board.into_any_element()
    }

    /// One row: label and metadata on the leading edge, control on the trailing.
    fn render_field(
        &self,
        dialog: &DialogView,
        index: usize,
        field: &DialogField,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let available = row_available(field);
        let value = self.value(dialog, index);
        let changed = field.changed || self.pending.contains_key(&field.variable);

        let control = match field.kind {
            DialogFieldKind::Note => return constant::note(index, field, cx),
            DialogFieldKind::Toggle => {
                let raw = value.unwrap_or(0.0);
                let checked = if field.flip { raw == 0.0 } else { raw != 0.0 };

                constant::switch_control(
                    ("constant-switch", index),
                    checked,
                    row_editable(field),
                    field.label.clone().into(),
                    cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_toggle(index, *checked, cx);
                    }),
                )
                .into_any_element()
            }
            DialogFieldKind::Select => self.selects.get(&index).map_or_else(
                || fallback_value(field, value, cx),
                |select| select.render(row_editable(field), field.label.clone().into()),
            ),
            DialogFieldKind::Value | DialogFieldKind::Slider => {
                self.inputs.get(&index).map_or_else(
                    || fallback_value(field, value, cx),
                    |input| {
                        input.render(
                            field.units.clone().into(),
                            available,
                            row_editable(field),
                            cx,
                        )
                    },
                )
            }
        };

        let mut lane = div()
            .id(format!(
                "constant-control-{}-{}-{}",
                dialog.id, index, field.variable
            ))
            .w_48()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .gap_2();

        if !available {
            let reason = t!("app.dialog.unavailable_for_settings").to_string();

            lane = lane.tooltip(move |window, cx| {
                Tooltip::new(reason.clone()).text_xs().build(window, cx)
            });
        }

        h_flex()
            .debug_selector(move || format!("constant-row-{index}"))
            .h(rems(3.5))
            .items_center()
            .gap_4()
            .px_4()
            .py_2()
            // To drop the first row's top border again, replace these two
            // lines with `.when(!first, |this| this.border_t_1()...)`.
            .border_t_1()
            .border_color(theme.border.opacity(0.5))
            .child(constant::field_label(field, index, available, cx))
            .child(
                lane.child(constant::changed_mark(changed, cx))
                    .child(div().flex_1().min_w_0().flex().justify_end().child(control)),
            )
            .into_any_element()
    }

    fn render_content(
        &self,
        dialog: Option<&DialogView>,
        failure: Option<&Failure>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();

        if let Some(dialog) = dialog {
            let has_visible = dialog.fields.iter().any(|field| field.gates.visible)
                || dialog.actions.iter().any(|command| command.gates.visible);

            if !has_visible && dialog.unavailable.is_empty() {
                let message = if dialog.fields.is_empty() {
                    t!("app.dialog.empty")
                } else {
                    t!("app.dialog.no_visible_fields")
                };

                return div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .into_any_element();
            }

            return div()
                .flex_1()
                .min_h_0()
                .child(self.render_board(dialog, cx))
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
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        }
    }

    fn render_status(&self, dialog: &DialogView, cx: &mut Context<'_, Self>) -> AnyElement {
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

/// The `commandButton` rows the definition declares, as one card.
///
/// A raw controller action is never run on open, close or edit; every button
/// is a separate click. A button is muted when the definition does not declare
/// the command, its gate is off, the session is read-only, or there is no
/// connection or no write access to carry it.
fn command_card(dialog: &DialogView, cx: &mut Context<'_, DialogEditor>) -> Option<AnyElement> {
    let theme = cx.theme().clone();
    let status = CoreService::status(cx);
    let can_reach_ecu = status
        .connection
        .as_ref()
        .is_some_and(|connection| !connection.read_only);
    let writes_enabled = status.writes_enabled;

    let rows: Vec<AnyElement> = dialog
        .actions
        .iter()
        .filter(|command| command.gates.visible)
        .map(|command| {
            let blocked =
                command_block_reason(command, dialog.writable, can_reach_ecu, writes_enabled);
            let message = show_message(command);
            let dialog_id = dialog.id.clone();
            let command_name = command.command.clone();

            let mut button = Button::new(format!(
                "dialog-command-{}-{}-{}",
                dialog.id, command.command, command.label
            ))
            .label(t!("app.dialog.run"))
            .ghost();

            match blocked {
                Some(reason) => {
                    button = button.disabled(true).tooltip(reason);
                }
                None => {
                    button = button.on_click(move |_, _, cx| {
                        send_command(&dialog_id, &command_name, message.clone(), cx);
                    });
                }
            }

            h_flex()
                .h(rems(3.5))
                .items_center()
                .justify_between()
                .gap_4()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(theme.border.opacity(0.5))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .child(command.label.clone()),
                )
                .child(button)
                .into_any_element()
        })
        .collect();

    if rows.is_empty() {
        return None;
    }

    Some(constant::card(
        ("constant-actions", 0usize),
        t!("app.dialog.actions"),
        rows,
        cx,
    ))
}

/// Sends a command button's action and shows its declared message.
fn send_command(dialog: &str, command: &str, message: Option<String>, cx: &mut App) {
    if crate::core::run_controller_command(dialog, command, cx)
        && let Some(message) = message
    {
        notifications::publish(
            NotificationDraft::new(
                NotificationSeverity::Info,
                t!("app.dialog.command_sent").to_string(),
                message,
            )
            .key(NotificationKey::named(format!(
                "dialog-command-{dialog}-{command}"
            ))),
            cx,
        );
    }
}

/// Help links declared by the dialog, as one card.
fn help_card(dialog: &DialogView, cx: &mut Context<'_, DialogEditor>) -> Option<AnyElement> {
    let mut rows = Vec::new();

    if let Some(topic) = dialog.topic_help.as_deref() {
        rows.push(help_row(
            "topic",
            t!("app.dialog.topic_help").to_string(),
            topic,
            cx,
        ));
    }

    for (index, link) in dialog.help_links.iter().enumerate() {
        let label = if link.title.trim().is_empty() {
            link.topic.clone()
        } else {
            link.title.clone()
        };
        rows.push(help_row(&format!("link-{index}"), label, &link.topic, cx));
    }

    if rows.is_empty() {
        return None;
    }

    Some(constant::card(
        ("constant-help", 0usize),
        t!("app.dialog.help"),
        rows,
        cx,
    ))
}

/// One help row: a URL opens in the browser, a topic id stays read-only.
fn help_row(id: &str, label: String, topic: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let opens = topic.starts_with("http://") || topic.starts_with("https://");

    let mut row = h_flex()
        .h(rems(3.5))
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(theme.border.opacity(0.5))
        .child(div().min_w_0().flex_1().truncate().text_sm().child(label));

    if opens {
        let url = topic.to_owned();
        row = row.child(
            Button::new(SharedString::from(format!("dialog-help-{id}")))
                .label(t!("app.dialog.open_help"))
                .ghost()
                .on_click(move |_, _, cx| cx.open_url(&url)),
        );
    } else {
        row = row.child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(topic.to_owned()),
        );
    }

    row.into_any_element()
}

/// One card's rows, in document order.
struct SectionGroup {
    title: String,
    items: Vec<usize>,
}

/// Groups visible rows into consecutive panels.
///
/// The definition stores a panel title on every row, and a row's position is
/// the reading order the definition chose. A title that appears twice with
/// another panel between them is two cards, not one, so the grouping follows
/// runs of equal titles instead of collecting by title.
fn sections(dialog_title: &str, fields: &[DialogField]) -> Vec<SectionGroup> {
    let mut groups: Vec<SectionGroup> = Vec::new();
    let mut current: Option<&str> = None;

    for (index, field) in fields.iter().enumerate() {
        if !field.gates.visible {
            continue;
        }

        if current != Some(field.section.as_str()) {
            current = Some(field.section.as_str());
            groups.push(SectionGroup {
                title: if field.section.is_empty() {
                    dialog_title.to_owned()
                } else {
                    field.section.clone()
                },
                items: Vec::new(),
            });
        }

        if let Some(group) = groups.last_mut() {
            group.items.push(index);
        }
    }

    groups
}

impl Focusable for DialogEditor {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DialogEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.ensure_widgets(window, cx);

        let theme = cx.theme().clone();
        let header = self.render_header(self.dialog.as_ref(), cx);
        let content = self.render_content(self.dialog.as_ref(), self.failure.as_ref(), cx);
        let status = self
            .dialog
            .as_ref()
            .map(|dialog| self.render_status(dialog, cx));

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.background)
            .text_color(theme.foreground)
            .track_focus(&self.focus)
            .child(header)
            .child(content)
            .when_some(status, gpui_kit::ParentElement::child)
    }
}

/// The control a visible row needs, or none for a note or switch.
fn widget_kind(field: &DialogField) -> Option<WidgetKind> {
    match field.kind {
        DialogFieldKind::Note | DialogFieldKind::Toggle => None,
        // A `bits` list with no usable label is a raw number in disguise; the
        // definition gives the tuner nothing to recognize, so the input shows
        // the value instead of an empty picker.
        DialogFieldKind::Select
            if field
                .options
                .iter()
                .any(|option| !option.label.trim().is_empty()) =>
        {
            Some(WidgetKind::Select)
        }
        _ if field.value.is_some() => Some(WidgetKind::Number),
        _ => Some(WidgetKind::Text),
    }
}

/// Whether the definition currently shows a row and lets it accept edits.
fn row_available(field: &DialogField) -> bool {
    field.gates.visible && field.gates.enabled
}

/// Whether this session may start an edit on a row.
fn row_editable(field: &DialogField) -> bool {
    field.writable && row_available(field)
}

/// Why a command button cannot run, or `None` when it can.
fn command_block_reason(
    command: &DialogAction,
    writable: bool,
    can_reach_ecu: bool,
    writes_enabled: bool,
) -> Option<SharedString> {
    if !command.known {
        return Some(t!("app.dialog.command_unknown").to_string().into());
    }
    if !command.gates.enabled {
        return Some(t!("app.dialog.command_disabled").to_string().into());
    }
    if !writable {
        return Some(t!("app.dialog.read_only").to_string().into());
    }
    if !can_reach_ecu {
        return Some(t!("app.dialog.command_needs_connection").to_string().into());
    }
    if !writes_enabled {
        return Some(t!("app.dialog.command_needs_writes").to_string().into());
    }

    None
}

/// The message a `showMessageOnClick` flag carries, when the row has one.
fn show_message(command: &DialogAction) -> Option<String> {
    let (index, _) = command
        .flags
        .iter()
        .enumerate()
        .find(|(_, flag)| flag.eq_ignore_ascii_case("showMessageOnClick"))?;
    let message = command.flags.get(index + 1)?.trim();

    (!message.is_empty()).then(|| message.to_owned())
}

/// Keeps a typed value inside the range the definition accepts.
fn clamp(value: f64, minimum: Option<f64>, maximum: Option<f64>) -> f64 {
    let mut value = value;

    if let Some(minimum) = minimum {
        value = value.max(minimum);
    }
    if let Some(maximum) = maximum {
        value = value.min(maximum);
    }

    value
}

/// The raw value a switch position stores, honouring a flipped row.
fn toggle_value(flip: bool, checked: bool) -> f64 {
    if flip == checked { 0.0 } else { 1.0 }
}

/// The value a typed edit asks for, from the row's current value.
///
/// A bare number replaces the value; `+2`, `*3`, `/4` and `+10%` are the
/// relative forms the table grid also accepts. `None` means the text is not an
/// edit and the row keeps its value.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "hex rows are integer constants; the parsed value is a small bit pattern"
)]
fn edited_value_with_hex(current: f64, text: &str, hex: bool) -> Option<f64> {
    if hex {
        let trimmed = text.trim();
        if let Some(hex) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            && let Ok(value) = i64::from_str_radix(hex, 16)
        {
            return Some(value as f64);
        }
    }

    edited_value(current, text)
}

fn edited_value(current: f64, text: &str) -> Option<f64> {
    let (operation, amount) = parse_edit(text).ok().flatten()?;
    let value = match operation {
        EditOp::Set => amount,
        EditOp::Add => current + amount,
        EditOp::Multiply => current * amount,
        EditOp::Divide => current / amount,
        EditOp::Percent => current * (1.0 + amount / 100.0),
    };

    value.is_finite().then_some(value)
}

/// A read-only value when the control for a row is not available.
fn fallback_value(field: &DialogField, value: Option<f64>, cx: &App) -> AnyElement {
    div()
        .w_full()
        .truncate()
        .text_right()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(constant::value_text(field, value))
        .into_any_element()
}

/// The subscription behind one numeric or text row.
fn subscribe_input(
    cx: &mut Context<'_, DialogEditor>,
    index: usize,
    state: &Entity<InputState>,
    window: &mut Window,
) -> Subscription {
    cx.subscribe_in(
        state,
        window,
        move |this, state, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_input(index, state, window, cx);
            }
        },
    )
}

/// The subscription behind one select row.
fn subscribe_select(
    cx: &mut Context<'_, DialogEditor>,
    index: usize,
    state: &Entity<SelectState<Vec<ConstantOption>>>,
    window: &mut Window,
) -> Subscription {
    cx.subscribe_in(
        state,
        window,
        move |this, _, event: &SelectEvent<Vec<ConstantOption>>, _, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.commit_select(index, *value, cx);
            }
        },
    )
}

/// The snapshot value of one variable, for echo reconciliation.
fn field_value(dialog: &DialogView, variable: &str) -> Option<f64> {
    dialog
        .fields
        .iter()
        .find(|field| field.variable == variable)
        .and_then(|field| field.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AppContext as _, Bounds, Entity, IntoElement, Point, Render, TestAppContext, Window,
        WindowBounds, WindowOptions, div, size,
    };
    use oxtuner_core::{DialogFieldTone, GateState};

    fn row(kind: DialogFieldKind, value: Option<f64>, options: usize) -> DialogField {
        DialogField {
            label: String::new(),
            variable: String::new(),
            value,
            text: None,
            units: String::new(),
            digits: 0,
            changed: false,
            display_only: false,
            writable: true,
            kind,
            section: String::new(),
            tone: DialogFieldTone::Normal,
            options: vec![
                oxtuner_core::DialogSelectOption {
                    value: 0,
                    label: String::new(),
                };
                options
            ]
            .into(),
            visibility_expression: None,
            enable_expression: None,
            gates: GateState::open(),
            help: None,
            minimum: None,
            maximum: None,
            index: None,
            hex: false,
            flip: false,
            power_cycle: false,
        }
    }

    #[test]
    fn definition_labels_choose_a_select_and_raw_values_choose_an_input() {
        let mut labelled = row(DialogFieldKind::Select, Some(2.0), 0);
        labelled.options = vec![oxtuner_core::DialogSelectOption {
            value: 2,
            label: "On".to_owned(),
        }]
        .into();

        assert_eq!(widget_kind(&labelled), Some(WidgetKind::Select));
        assert_eq!(
            widget_kind(&row(DialogFieldKind::Value, Some(2.0), 0)),
            Some(WidgetKind::Number)
        );
        assert_eq!(
            widget_kind(&row(DialogFieldKind::Value, None, 0)),
            Some(WidgetKind::Text)
        );
        assert_eq!(
            widget_kind(&row(DialogFieldKind::Toggle, Some(1.0), 0)),
            None
        );
        assert_eq!(widget_kind(&row(DialogFieldKind::Note, None, 0)), None);
        assert_eq!(
            widget_kind(&row(DialogFieldKind::Select, Some(2.0), 3)),
            Some(WidgetKind::Number),
            "an option list without labels is shown as the raw number"
        );
    }

    fn command(known: bool, enabled: bool, flags: &[&str]) -> DialogAction {
        DialogAction {
            label: "Reboot".to_owned(),
            command: "cmdReboot".to_owned(),
            visibility_expression: None,
            enable_expression: None,
            gates: GateState {
                visible: true,
                enabled,
                unresolved: false,
            },
            known,
            flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
        }
    }

    #[test]
    fn a_command_button_names_why_it_cannot_run() {
        assert!(
            command_block_reason(&command(false, true, &[]), true, true, true)
                .is_some_and(|reason| reason.contains("does not declare"))
        );
        assert!(
            command_block_reason(&command(true, false, &[]), true, true, true)
                .is_some_and(|reason| reason.contains("disabled"))
        );
        assert!(
            command_block_reason(&command(true, true, &[]), false, true, true)
                .is_some_and(|reason| reason.contains("Read-only"))
        );
        assert!(
            command_block_reason(&command(true, true, &[]), true, false, true)
                .is_some_and(|reason| reason.contains("Connect"))
        );
        assert!(
            command_block_reason(&command(true, true, &[]), true, true, false)
                .is_some_and(|reason| reason.contains("Enable writes"))
        );
        assert_eq!(
            command_block_reason(&command(true, true, &[]), true, true, true),
            None
        );
    }

    #[test]
    fn a_message_flag_carries_the_text_after_it() {
        let row = command(true, true, &["showMessageOnClick", "Controller restarted"]);

        assert_eq!(show_message(&row).as_deref(), Some("Controller restarted"));
        assert_eq!(show_message(&command(true, true, &[])), None);
        assert_eq!(
            show_message(&command(true, true, &["closeDialogOnClick"])),
            None
        );
    }

    #[test]
    fn hex_rows_display_and_parse_their_value() {
        let mut row = row(DialogFieldKind::Value, Some(31.0), 0);
        row.hex = true;
        assert_eq!(
            crate::ui::constant::value_text(&row, Some(31.0)).as_ref(),
            "0x1F"
        );
        assert_eq!(edited_value_with_hex(0.0, "0x1F", true), Some(31.0));
        assert_eq!(edited_value_with_hex(10.0, "+2", true), Some(12.0));
    }

    #[test]
    fn a_typed_value_stays_inside_the_definition_range() {
        assert_eq!(clamp(30.0, Some(0.0), Some(25.5)), 25.5);
        assert_eq!(clamp(-1.0, Some(0.0), None), 0.0);
        assert_eq!(clamp(12.3, None, None), 12.3);
    }

    #[test]
    fn a_typed_edit_applies_its_operation() {
        assert_eq!(edited_value(10.0, "12"), Some(12.0));
        assert_eq!(edited_value(10.0, "+2"), Some(12.0));
        assert_eq!(edited_value(10.0, "*3"), Some(30.0));
        assert_eq!(edited_value(10.0, "/4"), Some(2.5));
        assert_eq!(edited_value(10.0, ""), None);
        assert_eq!(edited_value(10.0, "nope"), None);
        assert_eq!(
            edited_value(10.0, "/0"),
            None,
            "division by zero is refused"
        );

        let percent = edited_value(10.0, "+10%").expect("a percentage is an edit");
        assert!((percent - 11.0).abs() < 1e-9);
    }

    #[test]
    fn a_flipped_switch_stores_its_inverse() {
        // A flipped row shows checked on raw zero and stores zero on checked.
        assert_eq!(toggle_value(true, true), 0.0);
        assert_eq!(toggle_value(true, false), 1.0);
        assert_eq!(toggle_value(false, true), 1.0);
        assert_eq!(toggle_value(false, false), 0.0);
    }

    #[test]
    fn panels_keep_document_order_and_split_repeated_titles() {
        let mut first = row(DialogFieldKind::Value, Some(1.0), 0);
        first.section = "Alpha".to_owned();
        let mut note = row(DialogFieldKind::Note, None, 0);
        note.section = "Alpha".to_owned();
        let mut second = row(DialogFieldKind::Value, Some(2.0), 0);
        second.section = "Beta".to_owned();
        let mut hidden = row(DialogFieldKind::Value, Some(3.0), 0);
        hidden.section = "Alpha".to_owned();
        hidden.gates.visible = false;
        let mut third = row(DialogFieldKind::Value, Some(4.0), 0);
        third.section = "Alpha".to_owned();

        let groups = sections("Title", &[first, note, second, hidden, third]);

        assert_eq!(groups.len(), 3, "a repeated title starts a new card");
        assert_eq!(groups[0].title, "Alpha");
        assert_eq!(groups[0].items, vec![0, 1]);
        assert_eq!(groups[1].title, "Beta");
        assert_eq!(groups[2].title, "Alpha");
        assert_eq!(groups[2].items, vec![4], "the hidden row is left out");
    }

    #[test]
    fn rows_at_the_dialog_top_take_the_dialog_title() {
        let mut top = row(DialogFieldKind::Value, Some(1.0), 0);
        top.section = String::new();

        let groups = sections("Engine Constants", &[top]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].title, "Engine Constants");
        assert_eq!(groups[0].items, vec![0]);
    }

    #[gpui_kit::test]
    fn the_embed_gate_comes_from_the_open_dialog(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);

            let mut dialog = summary();
            dialog.tables.push(oxtuner_core::DialogEmbed {
                id: "veTable1Tbl".to_owned(),
                section: String::new(),
                visibility_expression: Some("useVVT".to_owned()),
                enable_expression: None,
                gates: GateState {
                    visible: false,
                    enabled: true,
                    unresolved: false,
                },
            });
            CoreService::show_dialog(cx, dialog);
        });

        let editor = cx.update(|cx| cx.new(DialogEditor::new));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.show(
                    SharedString::from("Engine Constants"),
                    Some("engine_constants"),
                    cx,
                );
            });
        });

        let gates = cx
            .update(|cx| editor.read(cx).embed_state("vetable1tbl"))
            .expect("the embed is found by id, case-insensitively");
        assert!(!gates.visible, "the hidden panel withdraws its grid");
        assert!(
            cx.update(|cx| editor.read(cx).embed_state("other"))
                .is_none(),
            "an embed the dialog does not carry keeps the grid available"
        );
    }

    #[gpui_kit::test]
    fn a_rejected_edit_drops_the_optimistic_value(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);
            CoreService::show_dialog(cx, summary());
        });

        let editor = cx.update(|cx| cx.new(DialogEditor::new));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.show(
                    SharedString::from("Engine Constants"),
                    Some("engine_constants"),
                    cx,
                );
                editor.pending.insert("reqFuel".to_owned(), 99.0);
            });
        });

        cx.update(|cx| CoreService::fail_for_test(cx, FailureCode::DialogEditRejected));
        cx.run_until_parked();

        assert!(
            cx.update(|cx| editor.read(cx).pending.is_empty()),
            "a refused edit must not stay optimistic"
        );
        assert!(
            cx.update(|cx| editor.read(cx).force_sync),
            "the next frame redraws the stored value"
        );

        // A later status change still carries the same rejection. It must not
        // overwrite a keystroke the user made after that rejection.
        cx.update(|cx| {
            editor.update(cx, |editor, _| {
                editor.force_sync = false;
                editor.pending.insert("reqFuel".to_owned(), 42.0);
            });
        });
        cx.update(|cx| CoreService::show_dialog(cx, summary()));
        cx.run_until_parked();

        assert_eq!(
            cx.update(|cx| editor.read(cx).pending.get("reqFuel").copied()),
            Some(42.0),
            "a stale rejection must not clear a newer edit"
        );
        assert!(
            !cx.update(|cx| editor.read(cx).force_sync),
            "a stale rejection must not force another sync"
        );
    }

    #[gpui_kit::test]
    fn a_changed_power_cycle_setting_raises_a_notification(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);
            notifications::init(cx);
        });

        let dialog = power_summary();
        cx.update(|cx| CoreService::show_dialog(cx, dialog));

        let editor = cx.update(|cx| cx.new(DialogEditor::new));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.show(
                    SharedString::from("Engine Constants"),
                    Some("engine_constants"),
                    cx,
                );
            });
        });
        cx.run_until_parked();

        cx.update(|cx| CoreService::show_dialog(cx, power_summary()));
        cx.run_until_parked();

        let records = cx.update(|cx| notifications::records(cx));

        assert_eq!(records.len(), 1, "the dialog raises one notice");
        assert_eq!(records[0].severity(), NotificationSeverity::Warning);
        assert_eq!(records[0].title(), "Power cycle");
        assert_eq!(records[0].count(), 1, "a repeated status does not stack");
    }

    // `debug_bounds` is only recorded when the renderer is built with debug
    // assertions, so a release test build has nothing to assert here.
    #[cfg(debug_assertions)]
    #[gpui_kit::test]
    fn a_dialog_renders_a_card_for_each_panel(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);
            CoreService::show_dialog(cx, summary());
        });

        let (handle, _) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(1200.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |_window, cx| {
                    let editor = cx.new(|cx| {
                        let mut editor = DialogEditor::new(cx);
                        editor.show(
                            SharedString::from("Engine Constants"),
                            Some("engine_constants"),
                            cx,
                        );
                        editor
                    });

                    cx.new(|_| Host { editor })
                },
            )
            .expect("open test window")
        });

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("render the dialog frame");
        cx.run_until_parked();

        let mut visual = gpui_kit::VisualTestContext::from_window(handle, cx);
        visual.update(gpui_kit::test::TestWindowExt::render_frame);

        assert!(
            visual.debug_bounds("constant-board").is_some(),
            "the constants board scrolls the cards"
        );
        assert!(
            visual.debug_bounds("constant-row-0").is_some(),
            "the numeric row draws"
        );
        assert!(
            visual.debug_bounds("constant-row-1").is_none(),
            "the note draws as an alert, not as a row"
        );
        assert!(
            visual.debug_bounds("constant-row-2").is_some(),
            "the select row draws"
        );
        assert!(
            visual.debug_bounds("constant-row-3").is_some(),
            "the second panel's switch row draws"
        );
    }

    struct Host {
        editor: Entity<DialogEditor>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            div().size_full().child(self.editor.clone())
        }
    }

    fn option(value: u32, label: &str) -> oxtuner_core::DialogSelectOption {
        oxtuner_core::DialogSelectOption {
            value,
            label: label.to_owned(),
        }
    }

    fn summary() -> DialogView {
        let mut fuel = row(DialogFieldKind::Value, Some(12.3), 0);
        fuel.label = "Required fuel".to_owned();
        fuel.variable = "reqFuel".to_owned();
        fuel.units = "ms".to_owned();
        fuel.digits = 1;
        fuel.section = "Injection".to_owned();
        fuel.minimum = Some(0.0);
        fuel.maximum = Some(25.5);
        fuel.help = Some("Fuel delivered per cycle.".to_owned());

        let mut note = row(DialogFieldKind::Note, None, 0);
        note.label = "Outputs will not work if the wrong board is selected.".to_owned();
        note.text = Some(note.label.clone());
        note.tone = DialogFieldTone::Danger;
        note.section = "Injection".to_owned();

        let mut mode = row(DialogFieldKind::Select, Some(1.0), 0);
        mode.label = "Injection mode".to_owned();
        mode.variable = "injMode".to_owned();
        mode.section = "Injection".to_owned();
        mode.options = vec![
            option(0, "Off"),
            option(1, "Sequential"),
            option(2, "Batch"),
        ]
        .into();

        let mut sensor = row(DialogFieldKind::Toggle, Some(1.0), 0);
        sensor.label = "Flex fuel sensor".to_owned();
        sensor.variable = "flexEnabled".to_owned();
        sensor.section = "Flex fuel".to_owned();

        DialogView {
            id: "engine_constants".to_owned(),
            title: "Engine Constants".to_owned(),
            layout: None,
            fields: vec![fuel, note, mode, sensor],
            changed: 0,
            writable: true,
            unavailable: Vec::new(),
            topic_help: None,
            help_links: Vec::new(),
            presets: Vec::new(),
            actions: Vec::new(),
            widgets: Vec::new(),
            tables: Vec::new(),
            curves: Vec::new(),
            unmodeled: Vec::new(),
        }
    }

    fn power_summary() -> DialogView {
        let mut dialog = summary();
        dialog.fields[0].power_cycle = true;
        dialog.fields[0].changed = true;
        dialog
    }
}
