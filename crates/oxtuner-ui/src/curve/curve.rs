//! The reusable curve editor.
//!
//! A curve is a one-row table drawn as a line: one point per bin, with the
//! tuned value on the Y axis. Dragging a point vertically changes its value,
//! and the arrow keys nudge the selected point. A committed change becomes a
//! [`CurveEvent::EditRequested`], the owner sends it to the core, and the
//! echoed snapshot clears the optimistic value. That keeps the core the only
//! owner of the tune while the point still moves in the same frame as the
//! pointer (ADR-0020).
//!
//! The widget owns only view state: the snapshot, the selected point, the
//! hover, and the drag. It reads its chrome from the application theme, so it
//! carries no second style owner.
//!
//! Interaction:
//!
//! - click a point to select it, drag it up or down to change its value;
//! - left and right move the selection, up and down nudge it by the step the
//!   definition's display digits imply, with Shift for ten steps;
//! - Home and End jump to the first and last point, Escape abandons a drag.
//!
//! The curve is built to sit above the tune table for the same curve, so a
//! drag and a cell edit write the same variable and the two views can share a
//! selection. [`CurveEvent::PreviewChanged`] carries the in-flight value for a
//! paired table that wants to show it before the write lands.

#![expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "GPUI geometry uses f32 while the tune model stores f64 and element indexes"
)]

use std::{cell::Cell, rc::Rc};

use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::ThemeStyled as _;
use gpui_kit::component::theme::Theme;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, Context, CursorStyle, ElementId, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PathBuilder, Pixels, Point,
    Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    Window, canvas, div, point, px, relative, size,
};
use oxtuner_core::CellEdit;

use crate::caption::y_caption;

use super::model::{CurveModel, CurveSnapshot, curve_ticks, format_tick, tick_step};

/// Width of the Y tick label column, in pixels.
const Y_AXIS_WIDTH: f32 = 36.0;
/// Width of the stacked Y caption column, in pixels.
const Y_CAPTION_WIDTH: f32 = 20.0;
/// Smallest height the plot keeps, in pixels.
const MIN_PLOT_HEIGHT: f32 = 140.0;
/// Height of the X tick label row, in pixels.
const X_AXIS_HEIGHT: f32 = 18.0;
/// Height of the axis caption made of ordinary text, in pixels.
const CAPTION_HEIGHT: f32 = 18.0;
/// Side of the square that holds one point and its halo, in pixels.
const POINT_BOX: f32 = 24.0;
/// Diameter of a point, matching the component chart's dot.
const POINT_DOT: f32 = 8.0;
/// Diameter of the halo behind an active point, matching the chart's hover halo.
const POINT_HALO: f32 = 20.0;
/// Width of the series line, matching the component chart's stroke.
const LINE_WIDTH: f32 = 2.0;
/// Font size of an axis tick label, matching the plot's default text size.
const AXIS_TEXT: f32 = 10.0;
/// How far from a point's center a press still grabs it, in pixels.
const HIT_RADIUS: f32 = 10.0;
/// Padding that centers a tick label over its tick, in pixels.
const LABEL_WIDTH: f32 = 52.0;
/// Half a text line, used to center a Y tick label on its tick.
const LABEL_OFFSET: f32 = 7.0;

/// What the curve asks its owner to do.
#[expect(
    clippy::exhaustive_enums,
    reason = "the curve's owner handles a closed set of interactions"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum CurveEvent {
    /// The user committed values for `name` (the curve's value array).
    EditRequested {
        /// The value array a curve edit writes to.
        name: String,
        /// The new values, one per point, in user units.
        edits: Vec<CellEdit>,
    },
    /// The selected point changed.
    SelectionChanged {
        /// The selected point.
        index: usize,
    },
    /// A drag changed a point but has not committed it. A paired table can
    /// show the value until the write echoes; `None` clears it.
    PreviewChanged {
        /// The point being dragged.
        index: usize,
        /// The in-flight value, or `None` when the drag ended.
        value: Option<f64>,
    },
}

impl EventEmitter<CurveEvent> for Curve {}

/// The retained curve view.
pub struct Curve {
    model: CurveModel,
    focus: FocusHandle,
    /// Whether the header repeats the curve's title. A host whose own header
    /// already names the curve turns this off.
    title_visible: bool,
    /// The plot area in window coordinates, recorded each frame. Pointer
    /// positions are measured against it, so a press picks the point under the
    /// cursor rather than the one nearest a label gutter.
    plot_bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl std::fmt::Debug for Curve {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Curve")
            .field("model", &self.model)
            .field("title_visible", &self.title_visible)
            .finish_non_exhaustive()
    }
}

impl Focusable for Curve {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Curve {
    /// An empty curve with no snapshot.
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            model: CurveModel::new(),
            focus: cx.focus_handle().tab_stop(true),
            title_visible: true,
            plot_bounds: Rc::new(Cell::new(Bounds::new(
                point(px(0.), px(0.)),
                size(px(0.), px(0.)),
            ))),
        }
    }

    /// The model, for the owner's toolbar and for tests.
    pub fn model(&self) -> &CurveModel {
        &self.model
    }

    /// Replaces the snapshot and repaints.
    ///
    /// The same curve id keeps the selection and clears the optimistic values
    /// the snapshot confirms. A different curve starts at its first point.
    pub fn set_snapshot(&mut self, snapshot: CurveSnapshot, cx: &mut Context<'_, Self>) {
        let dragging = self.model.drag_index();
        self.model.set_snapshot(snapshot);
        self.drop_preview(dragging, cx);
        cx.notify();
    }

    /// Drops the curve.
    pub fn clear(&mut self, cx: &mut Context<'_, Self>) {
        let dragging = self.model.drag_index();
        self.model.clear();
        self.drop_preview(dragging, cx);
        cx.notify();
    }

    /// Shows or hides the title in the curve header.
    ///
    /// The default is visible, which suits a standalone plot. A host whose
    /// toolbar already names the curve turns it off to avoid a second title.
    pub fn set_title_visible(&mut self, visible: bool, cx: &mut Context<'_, Self>) {
        self.title_visible = visible;
        cx.notify();
    }

    /// Selects one point without telling the owner, so an owner can mirror a
    /// paired table's selection without echoing the event back.
    pub fn select(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if self.model.select(index) {
            self.model.set_hovered(None);
            cx.notify();
        }
    }

    /// Drops optimistic values after the core rejected an edit.
    pub fn reject_pending(&mut self, cx: &mut Context<'_, Self>) {
        let dragging = self.model.drag_index();
        self.model.reject_pending();
        self.drop_preview(dragging, cx);
        cx.notify();
    }

    /// Clears a paired view's preview when the drag it described is gone.
    fn drop_preview(&mut self, dragging: Option<usize>, cx: &mut Context<'_, Self>) {
        if let Some(index) = dragging
            && !self.model.is_dragging()
        {
            cx.emit(CurveEvent::PreviewChanged { index, value: None });
        }
    }

    /// The keyboard contract, in one place so tests can drive it through real
    /// events.
    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<'_, Self>) {
        let key = event.keystroke.key.as_str();
        let coarse = event.keystroke.modifiers.shift;

        match key {
            "left" => self.move_selection(-1, cx),
            "right" => self.move_selection(1, cx),
            "up" => self.nudge(if coarse { 10 } else { 1 }, cx),
            "down" => self.nudge(if coarse { -10 } else { -1 }, cx),
            "pageup" => self.nudge(10, cx),
            "pagedown" => self.nudge(-10, cx),
            "home" => self.move_to_first(cx),
            "end" => self.move_to_last(cx),
            "escape" => self.cancel_drag(cx),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<'_, Self>) {
        if self.model.select_relative(delta) {
            self.model.set_hovered(None);
            cx.emit(CurveEvent::SelectionChanged {
                index: self.model.selected(),
            });
            cx.notify();
        }
    }

    fn move_to_first(&mut self, cx: &mut Context<'_, Self>) {
        self.move_to_index(0, cx);
    }

    fn move_to_last(&mut self, cx: &mut Context<'_, Self>) {
        let Some(last) = self.model.point_count().checked_sub(1) else {
            return;
        };
        self.move_to_index(last, cx);
    }

    fn move_to_index(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if self.model.select(index) {
            self.model.set_hovered(None);
            cx.emit(CurveEvent::SelectionChanged { index });
            cx.notify();
        }
    }

    fn nudge(&mut self, steps: i32, cx: &mut Context<'_, Self>) {
        if let Some((index, value)) = self.model.nudge(steps) {
            self.emit_edit(index, value, cx);
            cx.notify();
        }
    }

    fn cancel_drag(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(index) = self.model.cancel_drag() {
            cx.emit(CurveEvent::PreviewChanged { index, value: None });
            cx.notify();
        }
    }

    fn finish_drag(&mut self, cx: &mut Context<'_, Self>) {
        let dragging = self.model.drag_index();
        let finished = self.model.finish_drag();

        if let Some(index) = dragging {
            cx.emit(CurveEvent::PreviewChanged { index, value: None });
        }
        if let Some((index, value)) = finished {
            self.emit_edit(index, value, cx);
        }

        cx.notify();
    }

    fn emit_edit(&mut self, index: usize, value: f64, cx: &mut Context<'_, Self>) {
        let Some(name) = self
            .model
            .snapshot()
            .map(|snapshot| snapshot.name().to_owned())
        else {
            return;
        };

        cx.emit(CurveEvent::EditRequested {
            name,
            edits: vec![CellEdit {
                row: 0,
                column: index,
                value,
            }],
        });
    }

    /// The pointer as fractions of the plot, `x` across and `y` down.
    fn plot_fraction(&self, position: Point<Pixels>) -> Option<(f64, f64)> {
        let bounds = self.plot_bounds.get();
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);

        if width <= 0.0 || height <= 0.0 {
            return None;
        }

        let x = f64::from(f32::from(position.x - bounds.origin.x) / width);
        let y = f64::from(f32::from(position.y - bounds.origin.y) / height);

        Some((x, y))
    }

    fn point_at(&self, x: f64, y: f64) -> Option<usize> {
        let bounds = self.plot_bounds.get();

        self.model.nearest(
            x,
            y,
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            HIT_RADIUS,
        )
    }

    fn render_empty(cx: &mut Context<'_, Self>) -> AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from("No curve loaded"))
            .into_any_element()
    }

    fn render_header(
        &self,
        snapshot: &CurveSnapshot,
        active: Option<usize>,
        theme: &Theme,
    ) -> AnyElement {
        let readout = active.and_then(|index| self.readout(snapshot, index));
        let count = self.model.point_count();
        let position = active.map(|index| format!("Point {} of {}", index + 1, count));

        h_flex()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .when(self.title_visible, |this| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(snapshot.title().to_owned())),
                        )
                    })
                    .when(!snapshot.is_writable(), |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(SharedString::from("Read-only")),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .when_some(position, |this, position| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(position)),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.foreground)
                            .child(SharedString::from(
                                readout.unwrap_or_else(|| "—".to_owned()),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn readout(&self, snapshot: &CurveSnapshot, index: usize) -> Option<String> {
        let x = self.model.x(index)?;
        let y = self.model.value(index)?;

        Some(format!(
            "{} · {}",
            value_with_units(x, snapshot.x_digits(), snapshot.x_units()),
            value_with_units(y, snapshot.digits(), snapshot.units()),
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one visual pass draws the header, grid, curve and handles together"
    )]
    fn render_curve(&self, snapshot: &CurveSnapshot, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let count = self.model.point_count();
        let selected = self.model.selected().min(count.saturating_sub(1));
        let hovered = self.model.hovered();
        let dragging = self.model.drag_index();
        let active = dragging.or(hovered).or((count > 0).then_some(selected));
        let writable = snapshot.is_writable();
        let x_range = self.model.x_range();
        let y_range = self.model.y_range();
        let x_span = x_range.1 - x_range.0;
        let y_span = y_range.1 - y_range.0;
        let x_ticks = curve_ticks(x_range, 5);
        // The definition gives intervals, while the tick helper takes a
        // target count. One extra line keeps a 4-division axis on 50-unit
        // steps instead of collapsing it to 100.
        let y_ticks = curve_ticks(y_range, snapshot.divisions().clamp(2, 12) + 1);
        let x_step = tick_step(&x_ticks);
        let y_step = tick_step(&y_ticks);
        let x_fractions: Vec<f32> = x_ticks
            .iter()
            .map(|tick| ((tick - x_range.0) / x_span) as f32)
            .collect();
        let y_fractions: Vec<f32> = y_ticks
            .iter()
            .map(|tick| ((y_range.1 - tick) / y_span) as f32)
            .collect();

        let x_labels: Vec<AnyElement> = x_ticks
            .iter()
            .zip(&x_fractions)
            .map(|(tick, fraction)| {
                x_axis_label(
                    *fraction,
                    format_tick(*tick, x_step),
                    theme.muted_foreground,
                )
            })
            .collect();
        let y_labels: Vec<AnyElement> = y_ticks
            .iter()
            .zip(&y_fractions)
            .map(|(tick, fraction)| {
                y_axis_label(
                    *fraction,
                    format_tick(*tick, y_step),
                    theme.muted_foreground,
                )
            })
            .collect();

        let points: Vec<Point<f32>> = (0..count)
            .filter_map(|index| {
                Some(point(
                    self.model.x_fraction(index)? as f32,
                    self.model.y_fraction(index)? as f32,
                ))
            })
            .collect();
        let active_x = active
            .and_then(|index| self.model.x_fraction(index))
            .map(|fraction| fraction as f32);
        let paint_x = x_fractions;
        let paint_y = y_fractions;
        let paint_points = points.clone();
        let grid_color = theme.chart_grid;
        let line_color = theme.chart_1;
        let guide_color = theme.chart_1.opacity(0.45);
        let slot = Rc::clone(&self.plot_bounds);

        let canvas = canvas(
            move |bounds, _, _| {
                slot.set(bounds);

                CanvasMetrics {
                    origin: bounds.origin,
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                }
            },
            move |_bounds, metrics, window, _| {
                paint_grid(window, metrics, &paint_x, &paint_y, grid_color);

                if let Some(fraction) = active_x {
                    paint_guide(window, metrics, fraction, guide_color);
                }

                paint_curve(window, metrics, &paint_points, line_color);
            },
        )
        .size_full();

        let dots: Vec<AnyElement> = points
            .iter()
            .enumerate()
            .map(|(index, at)| {
                let is_selected = index == selected;
                let is_hovered = hovered == Some(index);
                let mut dot = div()
                    .id(ElementId::from(("curve-point", index)))
                    .test_support()
                    .absolute()
                    .left(relative(at.x))
                    .top(relative(at.y))
                    .ml(px(-POINT_BOX / 2.0))
                    .mt(px(-POINT_BOX / 2.0))
                    .size(px(POINT_BOX))
                    .flex()
                    .items_center()
                    .justify_center();

                if is_selected || is_hovered {
                    // The halo sits behind the dot in the same square. It is
                    // positioned rather than laid out, so two circles stack on
                    // one center instead of sitting side by side.
                    let inset = (POINT_BOX - POINT_HALO) / 2.0;
                    dot = dot.child(
                        div()
                            .id(ElementId::from(("curve-point-halo", index)))
                            .test_support()
                            .absolute()
                            .top(px(inset))
                            .left(px(inset))
                            .size(px(POINT_HALO))
                            .rounded_full()
                            .bg(if is_selected {
                                theme.primary.opacity(0.20)
                            } else {
                                theme.chart_1.opacity(0.22)
                            }),
                    );
                }

                dot.child(
                    div()
                        .id(ElementId::from(("curve-point-dot", index)))
                        .test_support()
                        .size(px(POINT_DOT))
                        .rounded_full()
                        .bg(if is_selected {
                            theme.primary
                        } else {
                            theme.chart_1
                        })
                        .border_1()
                        .border_color(theme.background),
                )
                .into_any_element()
            })
            .collect();

        let chip = dragging.and_then(|index| {
            let x = self.model.x_fraction(index)? as f32;
            let y = self.model.y_fraction(index)? as f32;
            let value = value_with_units(
                self.model.value(index)?,
                snapshot.digits(),
                snapshot.units(),
            );

            Some(drag_value_chip(x, y, value, cx))
        });

        let plot = div()
            .id(ElementId::from("curve-plot"))
            .test_support()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h(px(MIN_PLOT_HEIGHT))
            .cursor(if writable {
                CursorStyle::Crosshair
            } else {
                CursorStyle::Arrow
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);

                    let Some((x, y)) = this.plot_fraction(event.position) else {
                        return;
                    };
                    let Some(index) = this.point_at(x, y) else {
                        return;
                    };

                    let changed = this.model.select(index);
                    if this.model.begin_drag(index, y)
                        && let Some(value) = this.model.value(index)
                    {
                        cx.emit(CurveEvent::PreviewChanged {
                            index,
                            value: Some(value),
                        });
                    }
                    if changed {
                        cx.emit(CurveEvent::SelectionChanged { index });
                    }

                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some((x, y)) = this.plot_fraction(event.position) else {
                    return;
                };
                let inside = (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y);
                let hovered = if inside { this.point_at(x, y) } else { None };

                if this.model.hovered() != hovered {
                    this.model.set_hovered(hovered);
                    cx.notify();
                }

                if event.pressed_button == Some(MouseButton::Left)
                    && let Some((index, value)) = this.model.drag_to(y)
                {
                    cx.emit(CurveEvent::PreviewChanged {
                        index,
                        value: Some(value),
                    });
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_drag(cx)),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && this.model.hovered().is_some() {
                    this.model.set_hovered(None);
                    cx.notify();
                }
            }))
            .child(canvas)
            .children(dots)
            .when_some(chip, gpui_kit::ParentElement::child);

        let x_caption = caption_of(snapshot.x_label(), snapshot.x_units());
        let y_label = snapshot.y_label();
        let y_units = snapshot.units();
        let has_y_caption = !y_label.is_empty() || !y_units.is_empty();
        let gutter = if has_y_caption {
            Y_CAPTION_WIDTH + Y_AXIS_WIDTH
        } else {
            Y_AXIS_WIDTH
        };
        let aria = match active.and_then(|index| self.readout(snapshot, index)) {
            Some(readout) => format!(
                "{} curve, {} points, point {} of {}, {}",
                snapshot.title(),
                count,
                active.unwrap_or(selected) + 1,
                count,
                readout
            ),
            None => format!("{} curve, {} points", snapshot.title(), count),
        };

        v_flex()
            .id("curve")
            .size_full()
            .min_h_0()
            .gap_2()
            .text_color(theme.foreground)
            .track_focus(&self.focus)
            .role(Role::Document)
            .aria_label(SharedString::from(aria))
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key(event, cx)),
            )
            .child(self.render_header(snapshot, active, &theme))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .when(has_y_caption, |this| {
                        this.child(y_caption(
                            y_label,
                            y_units,
                            Y_CAPTION_WIDTH,
                            theme.muted_foreground,
                        ))
                    })
                    .child(
                        div()
                            .relative()
                            .w(px(Y_AXIS_WIDTH))
                            .flex_shrink_0()
                            .pr_2()
                            .text_xs()
                            .children(y_labels),
                    )
                    .child(plot),
            )
            .child(
                h_flex()
                    .h(px(X_AXIS_HEIGHT))
                    .flex_shrink_0()
                    .w_full()
                    .child(div().w(px(gutter)).flex_shrink_0())
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .text_xs()
                            .children(x_labels),
                    ),
            )
            .when(!x_caption.is_empty(), |this| {
                this.child(
                    h_flex()
                        .h(px(CAPTION_HEIGHT))
                        .flex_shrink_0()
                        .w_full()
                        .child(div().w(px(gutter)).flex_shrink_0())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_center()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(x_caption)),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for Curve {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(snapshot) = self.model.snapshot() else {
            return Self::render_empty(cx);
        };

        self.render_curve(snapshot, cx)
    }
}

/// A Y tick label pinned to its tick instead of spread between the edges.
fn y_axis_label(fraction: f32, text: String, color: Hsla) -> AnyElement {
    div()
        .absolute()
        .left_0()
        .right_3()
        .top(relative(fraction))
        .mt(px(-LABEL_OFFSET))
        .whitespace_nowrap()
        .text_right()
        .text_size(px(AXIS_TEXT))
        .text_color(color)
        .child(SharedString::from(text))
        .into_any_element()
}

/// An X tick label centered on its tick, flush at either end.
fn x_axis_label(fraction: f32, text: String, color: Hsla) -> AnyElement {
    let mut label = div()
        .absolute()
        .top_0()
        .h(px(X_AXIS_HEIGHT))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .text_size(px(AXIS_TEXT))
        .text_color(color)
        .child(SharedString::from(text));

    if fraction <= 0.001 {
        label = label.left_0();
    } else if fraction >= 0.999 {
        label = label.right_0();
    } else {
        label = label
            .left(relative(fraction))
            .ml(px(-LABEL_WIDTH / 2.0))
            .w(px(LABEL_WIDTH))
            .justify_center();
    }

    label.into_any_element()
}

#[derive(Clone, Copy)]
struct CanvasMetrics {
    origin: Point<Pixels>,
    width: f32,
    height: f32,
}

fn paint_grid(
    window: &mut Window,
    metrics: CanvasMetrics,
    x_ticks: &[f32],
    y_ticks: &[f32],
    color: Hsla,
) {
    let mut builder = PathBuilder::stroke(px(1.));

    for fraction in y_ticks {
        let y = metrics.origin.y + px(fraction * metrics.height);
        builder.move_to(point(metrics.origin.x, y));
        builder.line_to(point(metrics.origin.x + px(metrics.width), y));
    }
    for fraction in x_ticks {
        let x = metrics.origin.x + px(fraction * metrics.width);
        builder.move_to(point(x, metrics.origin.y));
        builder.line_to(point(x, metrics.origin.y + px(metrics.height)));
    }

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_guide(window: &mut Window, metrics: CanvasMetrics, fraction: f32, color: Hsla) {
    let x = metrics.origin.x + px(fraction * metrics.width);
    let mut builder = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(3.)]);

    builder.move_to(point(x, metrics.origin.y));
    builder.line_to(point(x, metrics.origin.y + px(metrics.height)));

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_curve(window: &mut Window, metrics: CanvasMetrics, points: &[Point<f32>], color: Hsla) {
    if points.len() < 2 {
        return;
    }

    let map = |at: &Point<f32>| {
        point(
            metrics.origin.x + px(at.x * metrics.width),
            metrics.origin.y + px(at.y * metrics.height),
        )
    };
    let mut builder = PathBuilder::stroke(px(LINE_WIDTH));
    builder.move_to(map(&points[0]));

    for at in &points[1..] {
        builder.line_to(map(at));
    }

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// A value label beside a point while it is dragged, placed on the side that
/// keeps it inside the plot.
fn drag_value_chip(x: f32, y: f32, text: String, cx: &App) -> AnyElement {
    let mut chip = div()
        .id("curve-drag-value")
        .test_support()
        .absolute()
        .p_2()
        .text_xs()
        .popover_style(cx)
        .child(SharedString::from(text));

    chip = if y > 0.25 {
        chip.bottom(relative(1.0 - y)).mb(px(12.))
    } else {
        chip.top(relative(y)).mt(px(12.))
    };
    chip = if x < 0.5 {
        chip.left(relative(x)).ml(px(12.))
    } else {
        chip.right(relative(1.0 - x)).mr(px(12.))
    };

    chip.into_any_element()
}

fn caption_of(label: &str, units: &str) -> String {
    match (label.is_empty(), units.is_empty()) {
        (true, true) => String::new(),
        (false, true) => label.to_owned(),
        (true, false) => units.to_owned(),
        (false, false) => format!("{label} ({units})"),
    }
}

fn value_with_units(value: f64, digits: u8, units: &str) -> String {
    let text = format_value(value, digits);

    if units.is_empty() {
        text
    } else {
        format!("{text} {units}")
    }
}

fn format_value(value: f64, digits: u8) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }

    format!("{value:.digits$}", digits = usize::from(digits.min(6)))
}
