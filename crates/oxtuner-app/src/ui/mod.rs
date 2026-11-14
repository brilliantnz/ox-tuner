//! Application components shared by the editor views.
//!
//! The views in this crate own their state; the modules here own the way a
//! document is drawn. Keeping the presentation together means two editors that
//! show the same kind of row cannot drift apart.

use gpui_kit::{App, MouseButton, MouseDownEvent, Window};

pub(crate) mod constant;

/// Releases window focus when a left press lands on the background.
///
/// Capture runs before the press reaches its target, so blurring now still
/// lets a control take focus afterwards: inputs focus on press, buttons and
/// pickers stop the event and focus themselves. Anything else leaves the
/// window unfocused. Blurring before delivery also disarms popover focus
/// restore, which would otherwise pull focus back to a dismissed dropdown's
/// trigger and make a compare-after-delivery lie.
pub(crate) fn release_focus_on_background()
-> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    |event, window, cx| {
        if event.button != MouseButton::Left {
            return;
        }

        if window.focused(cx).is_some() {
            window.blur(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::scroll::ScrollableElement as _;
    use gpui_kit::test::{TestEventExt as _, TestWindowExt};
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
        IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render, Styled as _,
        TestAppContext, VisualTestContext, Window, WindowBounds, WindowOptions, div, px, size,
    };
    use oxtuner_core::DialogSelectOption;

    use super::constant::{ConstantInput, ConstantSelect, InputKind};

    /// A background with a real dialog input row and a real dialog dropdown
    /// row, the way the dialog editor owns them.
    struct PressHost {
        input: ConstantInput,
        select: ConstantSelect,
    }

    impl Render for PressHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            div()
                .size_full()
                .capture_any_mouse_down(super::release_focus_on_background())
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_4()
                        .child(
                            div()
                                .id("real-input")
                                .debug_selector(|| "real-input".to_string())
                                .w(px(300.))
                                .h(px(40.))
                                .child(self.input.render("".into(), true, true, cx)),
                        )
                        .child(
                            div()
                                .id("real-select")
                                .debug_selector(|| "real-select".to_string())
                                .w(px(300.))
                                .h(px(40.))
                                .child(self.select.render(true, "select".into())),
                        ),
                )
        }
    }

    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<PressHost>) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(1200.), px(800.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    let input = ConstantInput::new(window, cx, "0".into(), InputKind::Number);
                    let select = ConstantSelect::new(
                        window,
                        cx,
                        &[
                            DialogSelectOption {
                                value: 0,
                                label: "Zero".to_owned(),
                            },
                            DialogSelectOption {
                                value: 1,
                                label: "One".to_owned(),
                            },
                        ],
                        Some(0),
                    );

                    cx.new(|_| PressHost { input, select })
                },
            )
            .expect("open test window")
        })
    }

    fn press(handle: AnyWindowHandle, at: Point<Pixels>, cx: &mut TestAppContext) {
        cx.update_window(handle, |_, window, cx| {
            window.pointer_down(at, MouseButton::Left, cx);
        })
        .expect("dispatch the press");
        cx.run_until_parked();
    }

    fn input_focused(
        handle: AnyWindowHandle,
        host: &Entity<PressHost>,
        cx: &mut TestAppContext,
    ) -> bool {
        cx.update_window(handle, |_, window, cx| {
            host.read(cx).input.is_focused(window, cx)
        })
        .expect("read input focus")
    }

    fn select_focused(
        handle: AnyWindowHandle,
        host: &Entity<PressHost>,
        cx: &mut TestAppContext,
    ) -> bool {
        cx.update_window(handle, |_, window, cx| {
            host.read(cx).select.is_focused(window, cx)
        })
        .expect("read select focus")
    }

    // `debug_bounds` is only recorded when the renderer is built with debug
    // assertions, so a release test build has nothing to assert here.
    #[cfg(debug_assertions)]
    #[gpui_kit::test]
    fn presses_on_controls_keep_focus_while_background_releases_it(cx: &mut TestAppContext) {
        let (handle, host) = open(cx);

        let mut visual = VisualTestContext::from_window(handle, cx);
        visual.update(TestWindowExt::render_frame);
        let input = visual
            .debug_bounds("real-input")
            .expect("the input draws")
            .center();
        let select = visual
            .debug_bounds("real-select")
            .expect("the select draws")
            .center();

        // A press on the input focuses it and the background listener leaves
        // that focus alone: inputs must stay clickable.
        press(handle, input, cx);
        assert!(
            input_focused(handle, &host, cx),
            "a press on the input keeps the focus it took"
        );

        // A press on the background releases it.
        press(handle, gpui_kit::Point::new(px(1000.), px(700.)), cx);
        assert!(
            !input_focused(handle, &host, cx),
            "a press on the background releases the input"
        );

        // With nothing focused the background press is a harmless no-op.
        press(handle, gpui_kit::Point::new(px(1000.), px(700.)), cx);
        assert!(
            !input_focused(handle, &host, cx) && !select_focused(handle, &host, cx),
            "a background press with no focus changes nothing"
        );

        // A press on the dropdown focuses it and the background listener
        // leaves that focus alone.
        press(handle, select, cx);
        assert!(
            select_focused(handle, &host, cx),
            "a press on the dropdown keeps the focus it took"
        );

        // A press on the background releases it too.
        press(handle, gpui_kit::Point::new(px(1000.), px(700.)), cx);
        assert!(
            !select_focused(handle, &host, cx),
            "a press on the background releases the dropdown"
        );
    }
}
