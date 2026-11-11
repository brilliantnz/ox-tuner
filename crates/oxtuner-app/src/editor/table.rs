//! The table editor: toolbar, the table widget, and the line under them.
//!
//! This view owns the relationship between the table and the core. The table
//! asks for edits; this sends them as commands and feeds the echoed snapshot
//! back. The Y-axis flip is a workspace view preference, persisted with the
//! panel layout so it survives a restart.

use std::{collections::HashMap, path::PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::separator::Separator;
use gpui_kit::component::toolbar::Toolbar;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div, px,
};
use oxtuner_core::{Failure, FailureCode, SaveReport, TableView};
use oxtuner_ui::curve::{Curve, CurveEvent, CurveSnapshot};
use oxtuner_ui::table::{BulkOp, CellPos, GridAction, Table, TableEvent};

use crate::core::{CoreService, edit_table, read_table};
use crate::i18n::t;
use crate::settings::preferences::AppSettings;
use crate::state::session::{AppState, schedule_save};

/// The table editor for the node selected in the navigation tree.
pub(crate) struct TableEditor {
    table: Entity<Table>,
    /// The plot above the grid when the open table is a curve.
    curve: Entity<Curve>,
    /// The menu label, shown until the snapshot arrives and if it never does.
    label: SharedString,
    /// The table id the selection asked for.
    requested: Option<String>,
    /// Last save seen, so its echo refreshes the "changed" marks.
    last_save: Option<SaveReport>,
    /// The failure that stopped this table from appearing or saving.
    failure: Option<Failure>,
    /// Snapshots by table id, so a table that was open once draws on the next
    /// click instead of waiting for the core to answer again.
    cache: HashMap<String, TableView>,
    /// The project the cache belongs to, so another folder starts clean.
    project_root: Option<PathBuf>,
    /// The definition revision the open grid was read under. A project switch
    /// or a reloaded definition invalidates the numbers on screen, not only
    /// the cache behind them.
    definition_revision: u64,
    /// The tune revision the cache was filled at. An edit or a save elsewhere
    /// can move a value another dialog shows, so the cache drops then too.
    cache_revision: u64,
    flip_y: bool,
    /// Whether the dialog panel that embeds this grid is enabled by its gates.
    ///
    /// A disabled panel keeps its grid visible but refuses edits, the same way
    /// a disabled dialog row stays muted. Dialogs without an embed gate leave
    /// this true.
    embed_enabled: bool,
    _core: Subscription,
    _table: Subscription,
    _curve: Subscription,
    _settings: Subscription,
}

impl TableEditor {
    /// An editor with no table loaded yet.
    #[expect(
        clippy::used_underscore_binding,
        reason = "the underscore marks subscriptions kept only for their lifetime"
    )]
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        let table = cx.new(Table::new);
        let curve = cx.new(Curve::new);
        curve.update(cx, |curve, cx| curve.set_title_visible(false, cx));
        let _table = cx.subscribe(&table, |this, _, event, cx| this.on_table(event, cx));
        let _curve = cx.subscribe(&curve, |this, _, event, cx| this.on_curve(event, cx));
        let _core = cx.observe_global::<CoreService>(TableEditor::sync);
        // The Settings page edits a global, so a table that is already open
        // picks a new header preference up without being reopened.
        let _settings = cx.observe_global::<AppSettings>(TableEditor::sync_header_wash);
        let flip_y = cx
            .try_global::<AppState>()
            .is_some_and(|state| state.data().layout.table_low_at_top);
        let header_wash = AppSettings::table_header_wash_for_ranges(cx);
        let (project_root, definition_revision) = {
            let status = CoreService::status(cx);
            (
                status.project.as_ref().map(|project| project.root.clone()),
                status.definition_revision,
            )
        };

        let editor = Self {
            table,
            curve,
            label: SharedString::default(),
            requested: None,
            last_save: None,
            failure: None,
            cache: HashMap::new(),
            project_root,
            definition_revision,
            cache_revision: 0,
            flip_y,
            embed_enabled: true,
            _core,
            _table,
            _curve,
            _settings,
        };
        editor.table.update(cx, |table, cx| {
            table.set_flip_y(flip_y, cx);
            table.set_header_wash_for_ranges(header_wash, cx);
        });
        editor
    }

    /// Points the editor at a selected node.
    ///
    /// `target` is the definition object the node opens; `None` is a menu, a
    /// group or a kind with no editor yet.
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
            self.clear_curve(cx);
            cx.notify();
            return;
        };

        let already_shown = self.requested.as_deref() == Some(name)
            && self
                .table
                .read(cx)
                .model()
                .table()
                .is_some_and(|table| table.id == name);
        if already_shown {
            if let Some(table) = self.table.read(cx).model().table().cloned() {
                self.sync_curve(&table, cx);
            }
            cx.notify();
            return;
        }

        self.requested = Some(name.to_owned());
        self.failure = None;
        let existing = CoreService::status(cx)
            .table
            .as_ref()
            .filter(|table| table.id == name)
            .cloned();
        if let Some(table) = existing.or_else(|| self.cache.get(name).cloned()) {
            self.apply_table(table, cx);
        } else {
            // Do not show the previous table's values while the next snapshot
            // travels through the core; an empty grid plus "Loading" is honest.
            self.table.update(cx, Table::clear_table);
            self.clear_curve(cx);
            read_table(name, cx);
        }
        cx.notify();
    }

    /// Drops the grid a different project or definition invalidates.
    ///
    /// The tab keeps its target name: the definition may still declare it, in
    /// which case the next read draws the new tune's values. A target the new
    /// definition does not declare comes back as a failure, which is the honest
    /// answer for a document that no longer exists.
    fn reload(&mut self, cx: &mut Context<'_, Self>) {
        self.cache.clear();
        self.failure = None;
        self.table.update(cx, Table::clear_table);
        self.clear_curve(cx);

        let Some(name) = self.requested.clone() else {
            return;
        };

        read_table(&name, cx);
    }

    /// Follows core events: a new snapshot, a failure, or a save that makes the
    /// changed marks stale.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let mut snapshot = None;
        let mut failure = None;
        let mut edit_failed = false;
        let mut cleared = false;
        let mut saved = false;
        let mut reload = None;
        let mut table_matches = false;

        // A different project invalidates every snapshot: the values and the
        // changed marks belong to the folder that produced them. The grid on
        // screen is part of that snapshot, so it is cleared and read again
        // rather than left showing the previous tune. An edit or a save moves
        // the tune itself, so cached rows elsewhere are stale too.
        let (root, revision, definition_revision) = {
            let status = CoreService::status(cx);
            (
                status.project.as_ref().map(|project| project.root.clone()),
                status.tune_revision,
                status.definition_revision,
            )
        };

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
                match status.table.as_ref().filter(|table| &table.id == requested) {
                    Some(table) => {
                        table_matches = true;
                        let seen = self.table.read(cx).model().table() == Some(table);
                        if !seen {
                            snapshot = Some(table.clone());
                        }
                    }
                    None => {
                        if status
                            .failure
                            .as_ref()
                            .is_some_and(|failure| failure.code == FailureCode::TableUnavailable)
                        {
                            cleared = true;
                        }
                    }
                }
            }
            if let Some(failure_now) = &status.failure
                && matches!(
                    failure_now.code,
                    FailureCode::TableUnavailable | FailureCode::TableEditRejected
                )
            {
                edit_failed = failure_now.code == FailureCode::TableEditRejected;
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

        if let Some(table) = snapshot {
            self.failure = None;
            self.cache.insert(table.id.clone(), table.clone());
            self.apply_table(table, cx);
        }
        if table_matches {
            // The next successful core state proves the previous table failure
            // stale, even when the snapshot is identical.
            self.failure = None;
        }
        if cleared {
            if let Some(name) = &self.requested {
                self.cache.remove(name);
            }
            self.table.update(cx, Table::clear_table);
            self.clear_curve(cx);
        }
        if failure.is_some() {
            self.failure = failure;
        }
        if edit_failed {
            self.table.update(cx, Table::reject_pending);
            self.curve.update(cx, Curve::reject_pending);
            self.sync_curve_from_model(cx);
        }
        if saved && let Some(name) = reload {
            read_table(&name, cx);
        }
        cx.notify();
    }

    fn on_table(&mut self, event: &TableEvent, cx: &mut Context<'_, Self>) {
        match event {
            TableEvent::EditRequested { name, edits } => {
                edit_table(name, edits.clone(), cx);
            }
            TableEvent::AxisEditRequested { table, axis, edits } => {
                // The table paints a bin edit before the core answers. Feed
                // the same pending values to the plot so the point and its
                // labels do not lag a frame behind the header.
                if matches!(axis, oxtuner_core::AxisKind::X) {
                    self.sync_curve_from_model(cx);
                }
                crate::core::edit_axis(table, *axis, edits.clone(), cx);
            }
            TableEvent::SelectionChanged => {
                if let Some(column) = self.curve_column(cx) {
                    self.curve.update(cx, |curve, cx| curve.select(column, cx));
                }
                cx.notify();
            }
        }
    }

    /// Follows the curve above the grid: writes its edits, mirrors its
    /// selection, and shows a dragged value in the paired row before the echo.
    fn on_curve(&mut self, event: &CurveEvent, cx: &mut Context<'_, Self>) {
        match event {
            CurveEvent::EditRequested { name, edits } => {
                // The preview event just before this one cleared the row;
                // put the committed value back until the core answers.
                if let Some(edit) = edits.first() {
                    self.preview_cell(edit.column, Some(edit.value), cx);
                }
                edit_table(name, edits.clone(), cx);
            }
            CurveEvent::PreviewChanged { index, value } => {
                self.preview_cell(*index, *value, cx);
            }
            CurveEvent::SelectionChanged { index } => {
                self.table.update(cx, |table, cx| {
                    table.select_cell(
                        CellPos {
                            row: 0,
                            column: *index,
                        },
                        cx,
                    );
                });
            }
        }
    }

    fn preview_cell(&self, column: usize, value: Option<f64>, cx: &mut Context<'_, Self>) {
        self.table.update(cx, |table, cx| {
            table.preview_cell(CellPos { row: 0, column }, value, cx);
        });
    }

    /// Whether the open snapshot is a curve.
    fn is_curve(&self, cx: &Context<'_, Self>) -> bool {
        self.table
            .read(cx)
            .model()
            .table()
            .is_some_and(|table| table.curve.is_some())
    }

    /// The grid column the curve selection should mirror, when the open
    /// snapshot is a curve.
    fn curve_column(&self, cx: &Context<'_, Self>) -> Option<usize> {
        let model = self.table.read(cx).model();
        let table = model.table()?;

        table
            .curve
            .is_some()
            .then_some(model.selection().cursor.column)
    }

    /// Whether the embedding dialog panel's gates allow edits.
    ///
    /// The grid stays visible and readable when the panel is disabled; only the
    /// write path closes, and the toolbar shows its read-only banner.
    pub(crate) fn set_embed_enabled(&mut self, enabled: bool, cx: &mut Context<'_, Self>) {
        if self.embed_enabled == enabled {
            return;
        }
        self.embed_enabled = enabled;

        let Some(name) = self.requested.clone() else {
            return;
        };

        let raw = CoreService::status(cx)
            .table
            .clone()
            .filter(|table| table.id == name)
            .or_else(|| self.cache.get(&name).cloned());

        if let Some(table) = raw {
            self.apply_table(table, cx);
        }
        cx.notify();
    }

    /// Sets the grid and the curve from one snapshot, so the two views cannot
    /// disagree about which curve is open.
    fn apply_table(&self, mut table: TableView, cx: &mut Context<'_, Self>) {
        if !self.embed_enabled {
            table.writable = false;
        }

        self.sync_curve(&table, cx);
        self.table.update(cx, |grid, cx| grid.set_table(table, cx));
    }

    /// Feeds the curve from the table model, including a pending bin edit.
    fn sync_curve_from_model(&self, cx: &mut Context<'_, Self>) {
        let model = self.table.read(cx).model();
        let Some(table) = model.table() else {
            return;
        };
        if table.curve.is_none() {
            return;
        }
        let x_values: Vec<f64> = (0..model.axis_len(oxtuner_core::AxisKind::X))
            .map(|index| model.axis_value(oxtuner_core::AxisKind::X, index))
            .collect();
        let Some(snapshot) =
            CurveSnapshot::from_table(table).map(|snapshot| snapshot.with_x_values(x_values))
        else {
            return;
        };

        self.curve
            .update(cx, |curve, cx| curve.set_snapshot(snapshot, cx));
    }

    /// Feeds one snapshot to the curve, or clears it when the open snapshot is
    /// an ordinary table.
    fn sync_curve(&self, table: &TableView, cx: &mut Context<'_, Self>) {
        let snapshot = table
            .curve
            .is_some()
            .then(|| CurveSnapshot::from_table(table))
            .flatten();

        self.curve.update(cx, |curve, cx| match snapshot {
            Some(snapshot) => curve.set_snapshot(snapshot, cx),
            None => curve.clear(cx),
        });
    }

    fn clear_curve(&self, cx: &mut Context<'_, Self>) {
        self.curve.update(cx, Curve::clear);
    }

    fn toggle_flip_y(&mut self, cx: &mut Context<'_, Self>) {
        self.flip_y = !self.flip_y;
        self.table
            .update(cx, |grid, cx| grid.set_flip_y(self.flip_y, cx));
        AppState::update_global(cx, |data| {
            data.layout.table_low_at_top = self.flip_y;
        });
        schedule_save(cx);
        cx.notify();
    }

    /// Follows Settings › Editor: the header wash reaches an open table at once.
    fn sync_header_wash(&mut self, cx: &mut Context<'_, Self>) {
        let wash = AppSettings::table_header_wash_for_ranges(cx);

        self.table
            .update(cx, |table, cx| table.set_header_wash_for_ranges(wash, cx));
    }

    fn retry(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(name) = self.requested.clone() {
            read_table(&name, cx);
            cx.notify();
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one header render lays out the axes, corner and controls together"
    )]
    fn render_header(&self, table: Option<&TableView>, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let title: SharedString =
            table.map_or_else(|| self.label.clone(), |table| table.title.clone().into());
        let power_cycle = t!("app.table.power_cycle");
        let axis_caption = |axis: &oxtuner_core::AxisView| {
            if axis.power_cycle {
                format!("{} ({power_cycle})", axis.caption())
            } else {
                axis.caption()
            }
        };
        let axes = table.map(|table| {
            format!(
                "{} · {}",
                axis_caption(&table.x_axis),
                axis_caption(&table.y_axis)
            )
        });
        let read_only = table.is_some_and(|table| !table.writable);
        let table_power_cycle = table.is_some_and(|table| table.power_cycle);
        let has_table = table.is_some();
        let is_curve = table.is_some_and(|table| table.curve.is_some());

        let copy = self.table.clone();
        let paste = self.table.clone();
        let undo = self.table.clone();
        let redo = self.table.clone();
        let operations = self.table.clone();

        // A menu entry that cannot run is shown disabled: the selection's shape
        // decides, and the operation explains itself in its label.
        let (can_undo, can_redo, bulk, rebin) = {
            let model = self.table.read(cx).model();
            (
                model.can_undo(),
                model.can_redo(),
                [
                    model.bulk_available(BulkOp::InterpolateRows),
                    model.bulk_available(BulkOp::InterpolateColumns),
                    model.bulk_available(BulkOp::Smooth),
                    model.bulk_available(BulkOp::Equalize),
                    model.bulk_available(BulkOp::FillDown),
                    model.bulk_available(BulkOp::FillRight),
                ],
                [
                    model.can_rebin(oxtuner_core::AxisKind::X),
                    model.can_rebin(oxtuner_core::AxisKind::Y),
                ],
            )
        };
        let [
            interpolate_rows,
            interpolate_columns,
            smooth,
            equalize,
            fill_down,
            fill_right,
        ] = bulk;
        let [rebin_x, rebin_y] = rebin;

        let changed = table.map_or(0, oxtuner_core::TableView::changed_cells);
        let operations_button = Button::new("table-operations")
            .label(t!("app.table.operations"))
            .compact()
            .ghost()
            .dropdown_caret(true)
            .disabled(!has_table || read_only)
            .dropdown_menu(move |menu, _, _| {
                menu.item(bulk_item(
                    operations.clone(),
                    t!("app.table.interpolate_rows"),
                    BulkOp::InterpolateRows,
                    !interpolate_rows,
                ))
                .item(bulk_item(
                    operations.clone(),
                    t!("app.table.interpolate_columns"),
                    BulkOp::InterpolateColumns,
                    !interpolate_columns,
                ))
                .item(bulk_item(
                    operations.clone(),
                    t!("app.table.smooth"),
                    BulkOp::Smooth,
                    !smooth,
                ))
                .item(bulk_item(
                    operations.clone(),
                    t!("app.table.equalize"),
                    BulkOp::Equalize,
                    !equalize,
                ))
                .item(bulk_item(
                    operations.clone(),
                    t!("app.table.fill_down"),
                    BulkOp::FillDown,
                    !fill_down,
                ))
                .item(bulk_item(
                    operations.clone(),
                    t!("app.table.fill_right"),
                    BulkOp::FillRight,
                    !fill_right,
                ))
                .separator()
                .item(rebin_item(
                    operations.clone(),
                    t!("app.table.rebin_x"),
                    oxtuner_core::AxisKind::X,
                    !rebin_x,
                ))
                .item(rebin_item(
                    operations.clone(),
                    t!("app.table.rebin_y"),
                    oxtuner_core::AxisKind::Y,
                    !rebin_y,
                ))
            });
        let toolbar = Toolbar::new("table-toolbar")
            .child(
                Button::new("table-undo")
                    .icon(IconName::Undo2)
                    .ghost()
                    .accessibility_label(t!("app.table.undo"))
                    .tooltip(t!("app.table.undo_tooltip"))
                    .disabled(!can_undo)
                    .on_click(move |_, window, cx| {
                        let focus = undo.read(cx).focus_handle(cx);
                        undo.update(cx, oxtuner_ui::Table::undo);
                        window.focus(&focus, cx);
                    }),
            )
            .child(
                Button::new("table-redo")
                    .icon(IconName::Redo2)
                    .ghost()
                    .accessibility_label(t!("app.table.redo"))
                    .tooltip(t!("app.table.redo_tooltip"))
                    .disabled(!can_redo)
                    .on_click(move |_, window, cx| {
                        let focus = redo.read(cx).focus_handle(cx);
                        redo.update(cx, oxtuner_ui::Table::redo);
                        window.focus(&focus, cx);
                    }),
            )
            .content(Separator::vertical().h_5())
            .child(
                Button::new("table-copy")
                    .icon(IconName::Copy)
                    .label(t!("app.table.copy"))
                    .ghost()
                    .tooltip(t!("app.table.copy_tooltip"))
                    .disabled(!has_table)
                    .on_click(move |_, window, cx| {
                        let focus = copy.read(cx).focus_handle(cx);
                        copy.update(cx, oxtuner_ui::Table::copy_selection);
                        window.focus(&focus, cx);
                    }),
            )
            .child(
                Button::new("table-paste")
                    .label(t!("app.table.paste"))
                    .ghost()
                    .tooltip(t!("app.table.paste_tooltip"))
                    .disabled(!has_table || read_only)
                    .on_click(move |_, window, cx| {
                        let focus = paste.read(cx).focus_handle(cx);
                        paste.update(cx, oxtuner_ui::Table::paste_clipboard);
                        window.focus(&focus, cx);
                    }),
            )
            .child(
                Button::new("table-save")
                    .icon(IconName::HardDrive)
                    .label(t!("app.table.save"))
                    .primary()
                    .tooltip(t!("app.table.save_tooltip"))
                    .disabled(!has_table || read_only || changed == 0)
                    .on_click(|_, _, cx| crate::core::save_tune(cx)),
            )
            .content(Separator::vertical().h_5());

        // A curve has one row, so flipping the row axis has nothing to show.
        let toolbar = toolbar.when(!is_curve, |this| {
            this.child(
                Button::new("table-flip-y")
                    .icon(IconName::ChevronsUpDown)
                    .label(t!("app.table.flip_y"))
                    .ghost()
                    .tooltip(t!("app.table.flip_y_tooltip"))
                    .selected(self.flip_y)
                    .disabled(!has_table)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_flip_y(cx);
                        let focus = this.table.read(cx).focus_handle(cx);
                        window.focus(&focus, cx);
                    })),
            )
        });

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
                    .when(table_power_cycle, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(power_cycle.clone()),
                        )
                    })
                    .when_some(axes, |this, axes| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(axes),
                        )
                    })
                    .when(read_only, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(t!("app.table.read_only")),
                        )
                    }),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(operations_button)
                    .child(toolbar),
            )
            .into_any_element()
    }

    fn render_content(
        &self,
        has_table: bool,
        failure: Option<&Failure>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        if has_table {
            // A curve opens as a plot above its one-row table. The plot takes
            // the surplus height and the values stay visible under it.
            if self.is_curve(cx) {
                return v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .p_2()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.table.update(cx, oxtuner_ui::Table::dismiss_focus);
                        }),
                    )
                    .child(div().flex_1().min_h_0().w_full().child(self.curve.clone()))
                    .child(
                        div()
                            .flex_shrink_0()
                            .max_w_full()
                            .overflow_hidden()
                            .child(self.table.clone()),
                    )
                    .into_any_element();
            }

            // The editor owns the surface around the grid: the grid reports
            // its own size, so the surface fits it instead of filling the
            // editor.
            return v_flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .items_start()
                .p_2()
                // Clicks that land on the padding around the grid dismiss
                // the table's focus. Presses on cells and headers stop at
                // the table, so the toolbar and the selection it acts on
                // are unaffected.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.table.update(cx, oxtuner_ui::Table::dismiss_focus);
                    }),
                )
                .child(
                    // The surface fits the table: it is as wide as the grid
                    // and as tall as the grid, up to the editor's height; the
                    // grid scrolls inside once it does not fit.
                    div()
                        .max_w_full()
                        .max_h_full()
                        .overflow_hidden()
                        .child(self.table.clone()),
                )
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
                        t!("app.table.unavailable")
                    } else {
                        t!("app.table.loading")
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
                Button::new("table-retry")
                    .label(t!("app.table.retry"))
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the status line combines cursor, selection, change and failure facts"
    )]
    fn render_status(&self, table: &TableView, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let model = self.table.read(cx).model();
        let cursor = model.selection().cursor;
        // Bins come from the model so an optimistic header edit shows in the
        // same frame the key was pressed, before the core echoes it.
        let x_value = model.axis_value(oxtuner_core::AxisKind::X, cursor.column);
        let y_value = model.axis_value(oxtuner_core::AxisKind::Y, cursor.row);
        let x_text = format!(
            "{} {} {}",
            table.x_axis.label,
            format_value(x_value, table.x_axis.digits),
            table.x_axis.units
        )
        .trim_end()
        .to_owned();
        let y_text = format!(
            "{} {} {}",
            table.y_axis.label,
            format_value(y_value, table.y_axis.digits),
            table.y_axis.units
        )
        .trim_end()
        .to_owned();
        let cell_value = model.value(cursor);
        let value_text = if table.units.is_empty() {
            format_value(cell_value, table.digits)
        } else {
            format!("{} {}", format_value(cell_value, table.digits), table.units)
        };
        let axis_warning = if !model.axis_monotonic(oxtuner_core::AxisKind::X) {
            Some(t!(
                "app.table.axis_order",
                axis = table.x_axis.label.clone()
            ))
        } else if !model.axis_monotonic(oxtuner_core::AxisKind::Y) {
            Some(t!(
                "app.table.axis_order",
                axis = table.y_axis.label.clone()
            ))
        } else {
            None
        };

        let changed = table.changed_cells();
        let action = model.action().map(|action| match action {
            GridAction::Copied { cells } => t!("app.table.copied", count = cells.to_string()),
            GridAction::Pasted { cells } => t!("app.table.pasted", count = cells.to_string()),
            GridAction::PasteRejected => t!("app.table.paste_rejected"),
            GridAction::Interpolated { cells } => {
                t!("app.table.interpolated", count = cells.to_string())
            }
            GridAction::Smoothed { cells } => t!("app.table.smoothed", count = cells.to_string()),
            GridAction::Equalized { cells } => t!("app.table.equalized", count = cells.to_string()),
            GridAction::Filled { cells } => t!("app.table.filled", count = cells.to_string()),
            GridAction::Undone { cells } => t!("app.table.undone", count = cells.to_string()),
            GridAction::Redone { cells } => t!("app.table.redone", count = cells.to_string()),
            GridAction::UndoneAxis { bins } => {
                t!("app.table.undone_axis", count = bins.to_string())
            }
            GridAction::RedoneAxis { bins } => {
                t!("app.table.redone_axis", count = bins.to_string())
            }
            GridAction::TooSmall => t!("app.table.too_small"),
            GridAction::RebinnedAxis { bins } => {
                t!("app.table.rebinned_axis", count = bins.to_string())
            }
        });

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
            .child(
                h_flex()
                    .items_center()
                    .gap_4()
                    .child(t!("app.table.position", x = x_text, y = y_text))
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
            .child(
                h_flex()
                    .items_center()
                    .gap_4()
                    .child(t!(
                        "app.table.selection",
                        count = model.selection().cell_count().to_string()
                    ))
                    .child(t!("app.table.cursor_value", value = value_text))
                    .when(changed > 0, |this| {
                        this.child(
                            div()
                                .text_color(theme.warning)
                                .child(t!("app.table.changed", count = changed.to_string())),
                        )
                    })
                    .when_some(axis_warning, |this, warning| {
                        this.child(div().text_color(theme.danger).child(warning))
                    })
                    .when_some(action, gpui_kit::ParentElement::child),
            )
            .into_any_element()
    }
}

impl Render for TableEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // One small snapshot clone per frame keeps the borrow of the table out
        // of the render helpers, which need `cx` mutably for their buttons.
        let snapshot = self.table.read(cx).model().table().cloned();
        let has_table = snapshot.is_some();
        let failure = self.failure.as_ref();
        let header = self.render_header(snapshot.as_ref(), cx);
        let content = self.render_content(has_table, failure, cx);
        let status = snapshot.as_ref().map(|table| self.render_status(table, cx));

        v_flex()
            .size_full()
            .min_h_0()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(header)
            .child(content)
            .when_some(status, gpui_kit::ParentElement::child)
    }
}

fn bulk_item(
    grid: Entity<Table>,
    label: SharedString,
    op: BulkOp,
    disabled: bool,
) -> PopupMenuItem {
    PopupMenuItem::new(label)
        .disabled(disabled)
        .on_click(move |_, window, cx| {
            let focus = grid.read(cx).focus_handle(cx);
            grid.update(cx, |grid, cx| grid.bulk(op, cx));
            window.focus(&focus, cx);
        })
}

fn rebin_item(
    grid: Entity<Table>,
    label: SharedString,
    axis: oxtuner_core::AxisKind,
    disabled: bool,
) -> PopupMenuItem {
    PopupMenuItem::new(label)
        .disabled(disabled)
        .on_click(move |_, window, cx| {
            let focus = grid.read(cx).focus_handle(cx);
            grid.update(cx, |grid, cx| grid.rebin(axis, cx));
            window.focus(&focus, cx);
        })
}

fn format_value(value: f64, digits: u8) -> String {
    format!("{value:.digits$}", digits = usize::from(digits))
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;
    use oxtuner_core::{AxisView, CellState, TableView};

    use super::*;

    fn summary() -> TableView {
        TableView {
            id: "veTableTbl".to_owned(),
            name: "veTable".to_owned(),
            title: "VE Table".to_owned(),
            cols: 2,
            rows: 2,
            digits: 0,
            units: "%".to_owned(),
            cells: vec![10.0, 20.0, 30.0, 40.0],
            states: vec![CellState::Unknown; 4],
            changed: vec![false; 4],
            x_axis: AxisView {
                name: "rpmBins".to_owned(),
                label: "RPM".to_owned(),
                units: "rpm".to_owned(),
                channel: None,
                digits: 0,
                minimum: Some(0.0),
                maximum: Some(10_000.0),
                power_cycle: false,
                values: vec![1000.0, 2000.0],
            },
            y_axis: AxisView {
                name: "mapBins".to_owned(),
                label: "MAP".to_owned(),
                units: "kPa".to_owned(),
                channel: None,
                digits: 0,
                minimum: Some(0.0),
                maximum: Some(255.0),
                power_cycle: false,
                values: vec![50.0, 100.0],
            },
            minimum: 10.0,
            maximum: 40.0,
            cell_minimum: Some(0.0),
            cell_maximum: Some(255.0),
            writable: true,
            power_cycle: false,
            curve: None,
        }
    }

    #[gpui_kit::test]
    fn a_disabled_embed_keeps_the_grid_readable_and_closes_its_writes(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);
            CoreService::show_table(cx, summary());
        });

        let editor = cx.update(|cx| cx.new(TableEditor::new));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.show(SharedString::from("VE Table"), Some("veTableTbl"), cx);
            });
        });
        assert!(
            cx.update(|cx| editor.read(cx).table.read(cx).model().is_writable()),
            "an ungated embed starts writable"
        );

        cx.update(|cx| {
            editor.update(cx, |editor, cx| editor.set_embed_enabled(false, cx));
        });

        let (writable, cells) = cx.update(|cx| {
            let editor = editor.read(cx);
            let model = editor.table.read(cx).model();

            (
                model.is_writable(),
                model.table().map(|table| table.cells.clone()),
            )
        });
        assert!(
            !writable,
            "a disabled panel leaves the grid readable but not writable"
        );
        assert_eq!(
            cells,
            Some(vec![10.0, 20.0, 30.0, 40.0]),
            "the values stay on screen"
        );

        cx.update(|cx| {
            editor.update(cx, |editor, cx| editor.set_embed_enabled(true, cx));
        });
        assert!(
            cx.update(|cx| editor.read(cx).table.read(cx).model().is_writable()),
            "enabling the panel restores the write path"
        );
    }

    #[gpui_kit::test]
    fn a_new_definition_drops_the_previous_grid(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            CoreService::install_without_core(cx);
            CoreService::show_table(cx, summary());
        });

        let editor = cx.update(|cx| cx.new(TableEditor::new));
        cx.update(|cx| {
            editor.update(cx, |editor, cx| {
                editor.show(SharedString::from("VE Table"), Some("veTableTbl"), cx);
            });
        });

        assert_eq!(
            cx.update(|cx| {
                editor
                    .read(cx)
                    .table
                    .read(cx)
                    .model()
                    .table()
                    .map(|table| table.cells.clone())
            }),
            Some(vec![10.0, 20.0, 30.0, 40.0]),
            "the grid starts on the snapshot it was opened with"
        );

        cx.update(CoreService::next_definition);
        cx.run_until_parked();

        assert!(
            cx.update(|cx| editor.read(cx).table.read(cx).model().table().is_none()),
            "another definition must not leave the previous tune on screen"
        );
    }
}
