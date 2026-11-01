use super::*;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Bounds, Context, IntoElement, Render, TestAppContext, Window, WindowBounds,
    WindowOptions, div, px, size,
};

fn sample() -> Vec<IndicatorView> {
    vec![
        IndicatorView::new("Off", IndicatorState::Off, IndicatorTone::Ok),
        IndicatorView::new("Ok", IndicatorState::On, IndicatorTone::Ok),
        IndicatorView::new("Warning", IndicatorState::On, IndicatorTone::Warning),
        IndicatorView::new("Danger", IndicatorState::On, IndicatorTone::Danger),
        IndicatorView::new(
            "Unavailable",
            IndicatorState::Unavailable,
            IndicatorTone::Ok,
        ),
    ]
}

struct Host;

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = IndicatorStyle::default();

        div()
            .size_full()
            .p(px(20.))
            .child(IndicatorStrip::new(sample()).style(style.clone()))
            .child(
                IndicatorStrip::new(sample())
                    .style(style.clone())
                    .columns(3),
            )
            .child(legend::state_legend(&style))
    }
}

#[gpui_kit::test]
fn the_strip_renders_every_state(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);

    let (handle, _) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Default::default(),
                    size(px(800.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            |_, cx| cx.new(|_| Host),
        )
        .expect("open test window")
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
    })
    .expect("render the indicator strip");
}

#[test]
fn off_uses_inactive_colors_even_with_an_active_tone() {
    let style = IndicatorStyle::default();

    assert_eq!(
        colors(IndicatorState::Off, IndicatorTone::Danger, &style),
        (
            style.inactive_bg,
            style.inactive_border,
            style.inactive_text,
            style.inactive_dot,
        )
    );
}

#[test]
fn on_uses_the_tone_colors() {
    let style = IndicatorStyle::default();

    assert_eq!(
        colors(IndicatorState::On, IndicatorTone::Warning, &style),
        (
            style.warning_bg,
            style.warning_border,
            style.warning_text,
            style.warning_dot,
        )
    );
}
