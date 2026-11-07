//! The diagnostics tab: a bounded live tail of the tracing pipeline.
//!
//! The ring lives in `oxtuner-log`; this view only reads it. It never installs
//! a subscriber and never blocks on the file. While the tab is visible it asks
//! for a repaint a few times a second, then filters and lays out the newest
//! snapshot on the render tick. A paused view keeps the snapshot it paused on
//! and stops asking for repaints; the ring and the file keep running.

use std::{rc::Rc, time::Duration};

use gpui_kit::base::{VirtualListScrollHandle, h_flex, v_flex, v_virtual_list};
use gpui_kit::component::button::{Button, ButtonGroup};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Selectable as _, Sizable as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, ScrollDelta,
    ScrollEvent, ScrollStrategy, Size, Styled as _, Subscription, Task, Window, div, px, size,
};
use oxtuner_log::{LogRecord, LogSnapshot};
use tracing::Level;

use crate::i18n::t;

/// How often the visible view checks the ring for new records.
const REFRESH: Duration = Duration::from_millis(250);

/// Short rows stay compact; only rows whose content wraps take the taller
/// height. The virtual list needs the height before the row is laid out.
const ROW_HEIGHT_ONE: Pixels = px(30.);
const ROW_HEIGHT_TWO: Pixels = px(52.);
const ROW_FIXED_WIDTH: Pixels = px(440.);

/// The workspace crates, in the order the subsystem picker shows them.
const SUBSYSTEMS: [&str; 13] = [
    "core",
    "app",
    "ini",
    "tune",
    "project",
    "protocol",
    "transport",
    "sim",
    "datalog",
    "ui",
    "log",
    "other",
    "deps",
];

/// Which levels the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LevelFilter {
    All,
    AtMost(Level),
}

impl LevelFilter {
    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::AtMost(Level::ERROR),
            2 => Self::AtMost(Level::WARN),
            3 => Self::AtMost(Level::INFO),
            4 => Self::AtMost(Level::DEBUG),
            5 => Self::AtMost(Level::TRACE),
            _ => Self::All,
        }
    }

    fn allows(self, level: Level) -> bool {
        match self {
            Self::All => true,
            Self::AtMost(max) => level <= max,
        }
    }
}

/// Which subsystem the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubsystemFilter {
    All,
    Named(usize),
}

impl SubsystemFilter {
    fn from_index(index: usize) -> Self {
        if index == 0 || index > SUBSYSTEMS.len() {
            Self::All
        } else {
            Self::Named(index - 1)
        }
    }

    fn matches(self, target: &str) -> bool {
        match self {
            Self::All => true,
            Self::Named(index) => subsystem_of(target) == SUBSYSTEMS[index],
        }
    }
}

/// The diagnostics category.
#[expect(
    clippy::struct_excessive_bools,
    reason = "active, paused, follow and follow-pending are independent view states"
)]
pub(crate) struct DiagnosticsView {
    active: bool,
    paused: bool,
    follow: bool,
    query: Entity<InputState>,
    level: Entity<SelectState<Vec<String>>>,
    subsystem: Entity<SelectState<Vec<String>>>,
    scroll: VirtualListScrollHandle,
    records: Vec<LogRecord>,
    #[expect(
        clippy::rc_buffer,
        reason = "the virtual list takes a shared Vec of measured row sizes"
    )]
    row_sizes: Rc<Vec<Size<Pixels>>>,
    last_width: Pixels,
    last_revision: u64,
    clear_after: Option<u64>,
    paused_snapshot: Option<LogSnapshot>,
    follow_pending: bool,
    ring_total: usize,
    ring_dropped: u64,
    file_dropped: u64,
    force: bool,
    tick: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DiagnosticsView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("app.diagnostics.filter_placeholder"))
        });

        let levels = vec![
            t!("app.diagnostics.filter_all").to_string(),
            "ERROR".to_string(),
            "WARN".to_string(),
            "INFO".to_string(),
            "DEBUG".to_string(),
            "TRACE".to_string(),
        ];
        let level = cx.new(|cx| SelectState::new(levels, Some(IndexPath::new(0)), window, cx));

        let subsystems = std::iter::once(t!("app.diagnostics.filter_all").to_string())
            .chain(SUBSYSTEMS.iter().map(|name| (*name).to_string()))
            .collect::<Vec<_>>();
        let subsystem =
            cx.new(|cx| SelectState::new(subsystems, Some(IndexPath::new(0)), window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.invalidate(cx);
                }
            }),
            cx.observe(&level, |this, _, cx| this.invalidate(cx)),
            cx.observe(&subsystem, |this, _, cx| this.invalidate(cx)),
        ];

        Self {
            active: false,
            paused: false,
            follow: true,
            query,
            level,
            subsystem,
            scroll: VirtualListScrollHandle::new(),
            records: Vec::new(),
            row_sizes: Rc::new(Vec::new()),
            last_width: px(0.),
            last_revision: 0,
            clear_after: None,
            paused_snapshot: None,
            follow_pending: true,
            ring_total: 0,
            ring_dropped: 0,
            file_dropped: 0,
            force: true,
            tick: None,
            _subscriptions: subscriptions,
        }
    }

    /// Starts or stops the repaint timer as the tab becomes visible.
    pub(crate) fn set_active(&mut self, active: bool, cx: &mut Context<'_, Self>) {
        if self.active == active {
            return;
        }

        self.active = active;
        if active {
            self.force = true;
            self.start_tick(cx);
            cx.notify();
        } else {
            self.tick = None;
        }
    }

    /// Shows the records around one filter, with every other filter cleared.
    ///
    /// The caller has already made the tab visible. Follow and pause are reset
    /// so the target is at the live end of the list, and the level and
    /// subsystem pickers go back to All so a filter from an earlier search
    /// cannot hide the record the notification points at.
    pub(crate) fn reveal(
        &mut self,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.paused = false;
        self.paused_snapshot = None;
        self.follow = true;
        self.follow_pending = true;
        self.clear_after = None;
        self.force = true;
        self.last_revision = 0;

        self.query.update(cx, |query, cx| {
            query.set_value(filter.unwrap_or_default(), window, cx);
        });
        self.level.update(cx, |level, cx| {
            level.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });
        self.subsystem.update(cx, |subsystem, cx| {
            subsystem.set_selected_index(Some(IndexPath::new(0)), window, cx);
        });

        if self.active {
            self.start_tick(cx);
        }

        cx.notify();
    }

    fn start_tick(&mut self, cx: &mut Context<'_, Self>) {
        self.tick = None;
        if !self.active || self.paused {
            return;
        }

        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;

                let keep = this
                    .update(cx, |this, cx| {
                        if !this.active || this.paused {
                            return false;
                        }

                        cx.notify();
                        true
                    })
                    .unwrap_or(false);

                if !keep {
                    break;
                }
            }
        }));
    }

    fn invalidate(&mut self, cx: &mut Context<'_, Self>) {
        self.force = true;
        cx.notify();
    }

    fn toggle_paused(&mut self, cx: &mut Context<'_, Self>) {
        self.paused = !self.paused;

        if self.paused {
            self.paused_snapshot = Some(snapshot(cx));
            self.tick = None;
        } else {
            self.paused_snapshot = None;
            self.start_tick(cx);
        }

        self.force = true;
        cx.notify();
    }

    fn clear_view(&mut self, cx: &mut Context<'_, Self>) {
        let latest = crate::platform::logging::handle(cx)
            .map_or(self.last_revision, |handle| handle.store().revision());

        self.clear_after = Some(latest);
        self.force = true;
        cx.notify();
    }

    fn copy(&self, cx: &mut App) {
        let text = self
            .records
            .iter()
            .map(record_text)
            .collect::<Vec<_>>()
            .join("\n");

        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn open_file(cx: &App) {
        let Some(path) = crate::platform::logging::handle(cx).and_then(|handle| handle.path())
        else {
            return;
        };

        let url = format!("file://{}", path.display().to_string().replace(' ', "%20"));
        cx.open_url(&url);
    }

    /// A manual scroll away from the tail turns Follow off, so the next
    /// record cannot yank the view back to the bottom.
    fn stop_following_if_scrolled_away(&mut self) {
        if !self.follow || self.follow_pending || self.records.is_empty() {
            return;
        }

        let handle = self.scroll.base_handle();
        let offset = handle.offset().y;
        let bottom = -handle.max_offset().y;
        if offset > bottom + px(1.0) {
            self.follow = false;
        }
    }

    fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        if !self.active {
            return;
        }

        let snapshot = if self.paused {
            let snapshot = self.paused_snapshot.clone().unwrap_or_default();
            if !self.force && snapshot.revision == self.last_revision {
                return;
            }

            self.last_revision = snapshot.revision;
            snapshot
        } else {
            let Some(handle) = crate::platform::logging::handle(cx) else {
                self.records.clear();
                self.row_sizes = Rc::new(Vec::new());
                self.ring_total = 0;
                self.ring_dropped = 0;
                self.file_dropped = 0;
                self.force = false;
                return;
            };

            let snapshot = handle.store().snapshot();
            if !self.force && snapshot.revision == self.last_revision {
                return;
            }

            self.last_revision = snapshot.revision;
            self.file_dropped = handle.file_dropped();
            snapshot
        };

        self.rebuild(snapshot, cx);
        self.follow_pending = self.follow;
        self.force = false;
    }

    fn rebuild(&mut self, snapshot: LogSnapshot, cx: &App) {
        let query = self.query.read(cx).value().to_lowercase();
        let level = LevelFilter::from_index(
            self.level
                .read(cx)
                .selected_index(cx)
                .map_or(0, |index| index.row),
        );
        let subsystem = SubsystemFilter::from_index(
            self.subsystem
                .read(cx)
                .selected_index(cx)
                .map_or(0, |index| index.row),
        );
        let total = snapshot.records.len();
        let dropped = snapshot.dropped;

        self.records = snapshot
            .records
            .into_iter()
            .filter(|record| {
                self.clear_after
                    .is_none_or(|sequence| record.sequence > sequence)
            })
            .filter(|record| level.allows(record.level))
            .filter(|record| subsystem.matches(&record.target))
            .filter(|record| query.is_empty() || record_matches(record, &query))
            .collect();

        let width = if self.last_width > px(0.) {
            self.last_width
        } else {
            px(700.)
        };
        self.row_sizes = Rc::new(
            self.records
                .iter()
                .map(|record| size(px(0.), row_height(record, width)))
                .collect(),
        );
        self.ring_total = total;
        self.ring_dropped = dropped;
    }

    fn toolbar_row(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        let has_file = crate::platform::logging::handle(cx)
            .and_then(|handle| handle.path())
            .is_some();

        h_flex()
            .w_full()
            .flex_none()
            .justify_between()
            .gap_3()
            .items_center()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.query).small().w_full()),
                    )
                    .child(
                        div().w(px(110.)).child(
                            Select::new(&self.level)
                                .small()
                                .w_full()
                                .title_prefix(format!("{}: ", t!("app.diagnostics.level")))
                                .accessibility_label(t!("app.diagnostics.level")),
                        ),
                    )
                    .child(
                        div().w(px(140.)).child(
                            Select::new(&self.subsystem)
                                .small()
                                .w_full()
                                .title_prefix(format!("{}: ", t!("app.diagnostics.subsystem")))
                                .accessibility_label(t!("app.diagnostics.subsystem")),
                        ),
                    ),
            )
            .child(
                ButtonGroup::new("diagnostics-actions")
                    .small()
                    .child(
                        Button::new("diagnostics-pause")
                            .label(if self.paused {
                                t!("app.diagnostics.resume")
                            } else {
                                t!("app.diagnostics.pause")
                            })
                            .selected(self.paused)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_paused(cx))),
                    )
                    .child(
                        Button::new("diagnostics-follow")
                            .label(t!("app.diagnostics.follow"))
                            .selected(self.follow)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.follow = !this.follow;
                                this.follow_pending = this.follow;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diagnostics-clear")
                            .label(t!("app.diagnostics.clear"))
                            .disabled(self.records.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.clear_view(cx))),
                    )
                    .child(
                        Button::new("diagnostics-copy")
                            .label(t!("app.diagnostics.copy"))
                            .disabled(self.records.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                    )
                    .child(
                        Button::new("diagnostics-open")
                            .label(t!("app.diagnostics.open"))
                            .disabled(!has_file)
                            .on_click(cx.listener(|_, _, _, cx| Self::open_file(cx))),
                    ),
            )
            .into_any_element()
    }

    fn status_info(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let path = crate::platform::logging::handle(cx)
            .and_then(|handle| handle.path())
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_string());

        h_flex()
            .flex_none()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child(t!(
                "app.diagnostics.count",
                shown = self.records.len().to_string(),
                total = self.ring_total.to_string(),
            ))
            .when(self.ring_dropped > 0, |this| {
                this.child(t!(
                    "app.diagnostics.ring_dropped",
                    count = self.ring_dropped.to_string(),
                ))
            })
            .when(self.file_dropped > 0, |this| {
                this.child(t!(
                    "app.diagnostics.file_dropped",
                    count = self.file_dropped.to_string(),
                ))
            })
            .when(self.paused, |this| this.child(t!("app.diagnostics.paused")))
            .when_some(path, |this, path| {
                this.child(div().text_color(muted).child(path))
            })
            .into_any_element()
    }

    fn empty(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let (title, hint) = if crate::platform::logging::handle(cx).is_none() {
            (
                t!("app.diagnostics.unavailable"),
                t!("app.diagnostics.unavailable_hint"),
            )
        } else if self.records.is_empty() && self.ring_total > 0 {
            (
                t!("app.diagnostics.empty_filtered"),
                t!("app.diagnostics.empty_filtered_hint"),
            )
        } else {
            (
                t!("app.diagnostics.empty"),
                t!("app.diagnostics.empty_hint"),
            )
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().text_xs().text_color(muted).child(hint))
            .into_any_element()
    }

    fn row(&self, index: usize, cx: &App) -> AnyElement {
        let Some(record) = self.records.get(index) else {
            return div().into_any_element();
        };

        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let level_color = match record.level {
            Level::ERROR => theme.danger,
            Level::WARN => theme.warning,
            Level::INFO => theme.foreground,
            Level::DEBUG | Level::TRACE => theme.muted_foreground,
        };
        let text = record_body(record);

        h_flex()
            .w_full()
            .gap_2()
            .px_1()
            .py_0p5()
            .items_start()
            .when(index % 2 == 1, |this| this.bg(theme.tokens.list_even))
            .text_size(theme.mono_font_size)
            .font_family(theme.mono_font_family.clone())
            .child(
                div()
                    .w(px(110.))
                    .truncate()
                    .text_color(muted)
                    .child(time_label(&record.local_timestamp).to_string()),
            )
            .child(
                div()
                    .w(px(50.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(level_color)
                    .child(record.level.to_string()),
            )
            .child(
                div()
                    .w(px(140.))
                    .whitespace_normal()
                    .line_clamp(2)
                    .child(subsystem_label(&record.target)),
            )
            .child(
                div()
                    .w(px(100.))
                    .whitespace_normal()
                    .line_clamp(2)
                    .text_color(muted)
                    .child(record.thread.as_deref().unwrap_or("-").to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .line_clamp(2)
                    .child(text),
            )
            .into_any_element()
    }
}

impl Render for DiagnosticsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if self.active {
            let width = self.scroll.base_handle().bounds().size.width;
            if width > px(0.) && width != self.last_width {
                self.last_width = width;
                self.force = true;
            }

            self.stop_following_if_scrolled_away();
            self.refresh(cx);
        }

        let body = if self.records.is_empty() {
            self.empty(cx)
        } else {
            let list = v_virtual_list(
                cx.entity(),
                "diagnostics-list",
                Rc::clone(&self.row_sizes),
                move |this, range, _, cx| {
                    range
                        .map(|index| this.row(index, cx))
                        .collect::<Vec<AnyElement>>()
                },
            )
            .track_scroll(&self.scroll)
            .size_full();

            if self.follow && self.follow_pending {
                // `scroll_to_bottom` uses the item count from the previous
                // layout. With a record just added that count is one short, so
                // the newest row stops just below the viewport. Naming the
                // newest row directly makes the deferred scroll use this
                // frame's sizes.
                self.scroll
                    .scroll_to_item(self.records.len().saturating_sub(1), ScrollStrategy::Top);
                self.follow_pending = false;
            }

            list.into_any_element()
        };

        v_flex()
            .size_full()
            .w_full()
            .min_h_0()
            .gap_2()
            .child(self.toolbar_row(cx))
            .child(header(cx, self.status_info(cx)))
            .child(
                div()
                    .id("diagnostics-list-scroll")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .pr_1()
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                        let upward = match event.delta {
                            ScrollDelta::Pixels(delta) => delta.y > px(0.),
                            ScrollDelta::Lines(delta) => delta.y > 0.,
                        };

                        if upward && this.follow {
                            this.follow = false;
                            cx.notify();
                        }
                    }))
                    .child(body)
                    // A virtual list owns its scroll and virtualization, so
                    // it takes the scrollbar directly instead of the
                    // `Scrollable` wrapper.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .w(px(6.))
                            .child(
                                Scrollbar::vertical(&self.scroll)
                                    .id("diagnostics-list-scrollbar")
                                    .mode(ScrollbarMode::Scrolling)
                                    .viewport_from_layout(),
                            ),
                    ),
            )
    }
}

fn header(cx: &App, status: AnyElement) -> AnyElement {
    let muted = cx.theme().muted_foreground;

    h_flex()
        .w_full()
        .flex_none()
        .justify_between()
        .gap_3()
        .px_1()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(muted)
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(div().w(px(110.)).child(t!("app.diagnostics.column_time")))
                .child(div().w(px(50.)).child(t!("app.diagnostics.column_level")))
                .child(
                    div()
                        .w(px(140.))
                        .child(t!("app.diagnostics.column_subsystem")),
                )
                .child(div().w(px(100.)).child(t!("app.diagnostics.column_thread")))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(t!("app.diagnostics.column_message")),
                ),
        )
        .child(status)
        .into_any_element()
}

fn snapshot(cx: &App) -> LogSnapshot {
    crate::platform::logging::handle(cx)
        .map_or_else(LogSnapshot::default, |handle| handle.store().snapshot())
}

/// The date is the same for a session, so the table shows the clock part.
/// The full RFC 3339 timestamp stays on the record for copy and export.
fn time_label(timestamp: &str) -> &str {
    let time = timestamp
        .split_once('T')
        .map_or(timestamp, |(_, time)| time);
    let end = time.find(['Z', '+', '-']).unwrap_or(time.len());
    let time = &time[..end];

    time.get(..12).unwrap_or(time)
}

fn record_matches(record: &LogRecord, query: &str) -> bool {
    if record.message.to_lowercase().contains(query) {
        return true;
    }

    if record.target.to_lowercase().contains(query) {
        return true;
    }

    if record
        .thread
        .as_deref()
        .is_some_and(|thread| thread.to_lowercase().contains(query))
    {
        return true;
    }

    record
        .fields
        .iter()
        .any(|(name, value)| field_matches(name, value, query))
}

fn field_matches(name: &str, value: &str, query: &str) -> bool {
    // A notification links to a record with `op=op-42`. The field is stored
    // as a name and a value, so matching them separately would never see the
    // pair the user searched for.
    let name = name.to_lowercase();
    let value = value.to_lowercase();

    name.contains(query) || value.contains(query) || format!("{name}={value}").contains(query)
}

fn record_text(record: &LogRecord) -> String {
    let thread = record.thread.as_deref().unwrap_or("-");

    format!(
        "{} {:5} {} {} {}",
        record.local_timestamp,
        record.level,
        record.target,
        thread,
        record_body(record),
    )
}

fn record_body(record: &LogRecord) -> String {
    let fields = record
        .fields
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(" ");

    if fields.is_empty() {
        record.message.to_string()
    } else {
        format!("{} {fields}", record.message)
    }
}

fn row_height(record: &LogRecord, viewport: Pixels) -> Pixels {
    row_height_for(
        &record_body(record),
        &subsystem_label(&record.target),
        record.thread.as_deref().unwrap_or("-"),
        viewport,
    )
}

fn row_height_for(message: &str, subsystem: &str, thread: &str, viewport: Pixels) -> Pixels {
    if content_wraps(message, subsystem, thread, viewport) {
        ROW_HEIGHT_TWO
    } else {
        ROW_HEIGHT_ONE
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "a positive pixel width divided by the glyph width is a row count"
)]
fn content_wraps(message: &str, subsystem: &str, thread: &str, viewport: Pixels) -> bool {
    // A small over-estimate of glyph width keeps a row that actually wraps
    // from being laid out at the one-line height and clipping.
    let per_line = |width: Pixels| (width / px(8.0)).max(1.0) as usize;
    let message_width = (viewport - ROW_FIXED_WIDTH).max(px(120.));

    message.chars().count() > per_line(message_width)
        || subsystem.chars().count() > per_line(px(140.))
        || thread.chars().count() > per_line(px(100.))
}

fn subsystem_of(target: &str) -> &'static str {
    let Some(rest) = target.strip_prefix("oxtuner_") else {
        return "deps";
    };

    match rest.split("::").next().unwrap_or(rest) {
        "core" => "core",
        "app" => "app",
        "ini" => "ini",
        "tune" => "tune",
        "project" => "project",
        "protocol" => "protocol",
        "transport" => "transport",
        "sim" => "sim",
        "datalog" => "datalog",
        "ui" => "ui",
        "log" => "log",
        _ => "other",
    }
}

fn subsystem_label(target: &str) -> String {
    target
        .strip_prefix("oxtuner_")
        .unwrap_or(target)
        .replace("::", " · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Point;
    use gpui_kit::test::TestWindowExt as _;

    #[test]
    fn time_labels_keep_the_clock_part() {
        assert_eq!(time_label("2026-11-01T05:20:55.123456Z"), "05:20:55.123");
        assert_eq!(time_label("2026-11-01T13:20:55+08:00"), "13:20:55");
    }

    #[test]
    fn a_filter_can_match_a_whole_field_pair() {
        assert!(field_matches("op", "op-42", "op=op-42"));
        assert!(field_matches("op", "op-42", "op"));
        assert!(!field_matches("op", "op-42", "op=op-7"));
    }

    #[test]
    fn row_heights_follow_wrapped_content() {
        let viewport = px(775.);
        let long = "x".repeat(200);

        assert_eq!(
            row_height_for("short", "core", "main", viewport),
            ROW_HEIGHT_ONE
        );
        assert_eq!(
            row_height_for(&long, "core", "main", viewport),
            ROW_HEIGHT_TWO
        );
        assert_eq!(
            row_height_for("short", "app · state · session", "main", viewport),
            ROW_HEIGHT_TWO
        );
    }

    #[test]
    fn a_level_filter_keeps_the_more_severe_records() {
        let info = LevelFilter::AtMost(Level::INFO);

        assert!(info.allows(Level::ERROR));
        assert!(info.allows(Level::WARN));
        assert!(info.allows(Level::INFO));
        assert!(!info.allows(Level::DEBUG));
        assert!(!info.allows(Level::TRACE));
        assert!(LevelFilter::All.allows(Level::TRACE));
    }

    #[test]
    fn subsystem_mapping_separates_workspace_crates_from_dependencies() {
        assert_eq!(subsystem_of("oxtuner_core::service"), "core");
        assert_eq!(subsystem_of("oxtuner_app::platform::logging"), "app");
        assert_eq!(subsystem_of("oxtuner_log::init"), "log");
        assert_eq!(subsystem_of("oxtuner_unknown::thing"), "other");
        assert_eq!(subsystem_of("serial2::port"), "deps");
    }

    #[test]
    fn subsystem_labels_strip_the_workspace_prefix() {
        assert_eq!(subsystem_label("oxtuner_core::service"), "core · service");
        assert_eq!(subsystem_label("serial2::port"), "serial2 · port");
    }

    #[test]
    fn a_subsystem_pick_matches_only_its_own_crate() {
        let core = SubsystemFilter::from_index(1);
        let deps = SubsystemFilter::from_index(SUBSYSTEMS.len());

        assert!(core.matches("oxtuner_core::service"));
        assert!(!core.matches("oxtuner_app::core"));
        assert!(deps.matches("serial2::port"));
        assert!(!deps.matches("oxtuner_core::service"));
    }

    struct Host {
        view: Entity<DiagnosticsView>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            div().size_full().child(self.view.clone())
        }
    }

    fn open(
        cx: &mut gpui_kit::TestAppContext,
    ) -> (
        gpui_kit::AnyWindowHandle,
        Entity<Host>,
        Entity<DiagnosticsView>,
    ) {
        use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};

        cx.update(gpui_kit::init);

        cx.update(|cx| {
            let (handle, host) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(900.), px(600.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let view = cx.new(|cx| DiagnosticsView::new(window, cx));
                    cx.new(|_| Host { view })
                },
            )
            .expect("open test window");

            let view = host.read(cx).view.clone();
            (handle, host, view)
        })
    }

    /// The tab has to build from a real window: the text filter and both
    /// pickers are stateful components. Without a logging handle it falls back
    /// to the unavailable state rather than failing to render.
    #[gpui_kit::test]
    fn the_view_renders_without_logging(cx: &mut gpui_kit::TestAppContext) {
        let (handle, _host, _view) = open(cx);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
        })
        .expect("render the diagnostics tab");
    }

    /// A wheel event must move the virtual list, not just repaint it.
    #[gpui_kit::test]
    fn the_view_renders_and_scrolls_records_from_the_ring(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::InputEvent as _;
        use gpui_kit::{ScrollDelta, point};
        use oxtuner_log::{FilePolicy, LogConfig};

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "oxtuner-diagnostics-scroll-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create the log directory");

        let handle = oxtuner_log::init(&LogConfig::new(&directory).with_file(FilePolicy::Off))
            .expect("install logging");
        cx.update(|cx| crate::platform::logging::install(cx, handle));

        for index in 0..100 {
            tracing::info!(
                target: "oxtuner_core",
                "record {index}"
            );
        }

        let (window, _host, view) = open(cx);
        cx.update(|cx| {
            view.update(cx, |view, cx| view.set_active(true, cx));
        });

        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("render the populated diagnostics tab");

        let count = view.read_with(cx, |view, _| view.records.len());
        assert!(count >= 100, "expected at least 100 records, got {count}");

        let (offset, max) = view.read_with(cx, |view, _| {
            let handle = view.scroll.base_handle();
            (handle.offset().y, handle.max_offset().y)
        });
        assert_eq!(
            offset, -max,
            "follow must start pinned to the newest record"
        );

        tracing::info!(target: "oxtuner_core", "record after follow");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .expect("render the new record");
        let (offset, max) = view.read_with(cx, |view, _| {
            let handle = view.scroll.base_handle();
            (handle.offset().y, handle.max_offset().y)
        });
        assert_eq!(
            offset, -max,
            "follow must stay pinned while the diagnostics tab stays open"
        );

        let (before, position) = view.read_with(cx, |view, _| {
            let handle = view.scroll.base_handle();
            (handle.offset().y, handle.bounds().center())
        });
        cx.update_window(window, |_, window, cx| {
            window.dispatch_event(
                gpui_kit::ScrollEvent {
                    position,
                    delta: ScrollDelta::Pixels(point(px(0.), px(120.))),
                    modifiers: window.modifiers(),
                    ..Default::default()
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .expect("scroll the diagnostics list");
        let (after, follow) = view.read_with(cx, |view, _| {
            (view.scroll.base_handle().offset().y, view.follow)
        });

        assert!(
            after > before,
            "the wheel must move the list up: {before} -> {after}"
        );
        assert!(!follow, "a manual scroll away must turn Follow off");

        let _ = std::fs::remove_dir_all(&directory);
    }
}
