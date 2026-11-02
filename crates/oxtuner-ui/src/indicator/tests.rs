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
            .child(IndicatorStrip::new(sample()))
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
fn the_theme_supplies_the_card_colours() {
    let theme = gpui_kit::component::theme::Theme::default();
    let style = IndicatorStyle::from_theme(&theme);

    assert_eq!(style.inactive_bg, theme.secondary);
    assert_eq!(style.inactive_text, theme.muted_foreground);
    assert_eq!(style.ok_dot, theme.success);
    assert_eq!(style.warning_dot, theme.warning);
    assert_eq!(style.danger_dot, theme.danger);
    assert_eq!(style.gap, IndicatorStyle::default().gap);
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
fn an_ini_on_colour_picks_the_card_tone() {
    assert_eq!(
        IndicatorTone::from_colour_name("red"),
        IndicatorTone::Danger
    );
    assert_eq!(
        IndicatorTone::from_colour_name("darkRed"),
        IndicatorTone::Danger
    );
    assert_eq!(
        IndicatorTone::from_colour_name("yellow"),
        IndicatorTone::Warning
    );
    assert_eq!(IndicatorTone::from_colour_name("green"), IndicatorTone::Ok);
    assert_eq!(IndicatorTone::from_colour_name("white"), IndicatorTone::Ok);

    let view =
        IndicatorView::from_colour_name("Battery Voltage Warning", IndicatorState::On, "red");
    assert_eq!(view.tone, IndicatorTone::Danger);
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
