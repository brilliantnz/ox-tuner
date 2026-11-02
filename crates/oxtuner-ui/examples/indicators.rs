//! A development window for the indicator strip.
//!
//! Run with:
//!
//! ```text
//! cargo run -p oxtuner-ui --example indicators
//! ```
//!
//! The window is intentionally not part of `oxtuner-app`: the widget is being
//! tuned against the reference design before the application hosts it.
//!
//! The labels come from a real definition, `speeduino-202501.7.ini`. There is
//! no live frame here, so every card is given a demo state from a fixed cycle
//! and all visual states stay visible while the layout is tuned.

use std::path::Path;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::{
    AppContext as _, Bounds, Context, FontWeight, IntoElement, ParentElement as _, Render,
    Styled as _, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use oxtuner_ini::{SettingsContext, parse_bytes};
use oxtuner_ui::indicator::legend::state_legend;
use oxtuner_ui::indicator::{
    IndicatorState, IndicatorStrip, IndicatorStyle, IndicatorTone, IndicatorView,
};

/// A local definition used only to preview real labels.
const INI_PATH: &str = "../../docs/tmp/local/ini/speeduino-202501.7.ini";

/// The card states used by the demo cycle.
const DEMO_STATES: [(IndicatorState, IndicatorTone); 7] = [
    (IndicatorState::Off, IndicatorTone::Ok),
    (IndicatorState::On, IndicatorTone::Ok),
    (IndicatorState::Off, IndicatorTone::Ok),
    (IndicatorState::On, IndicatorTone::Warning),
    (IndicatorState::Off, IndicatorTone::Ok),
    (IndicatorState::On, IndicatorTone::Danger),
    (IndicatorState::Unavailable, IndicatorTone::Ok),
];

struct Preview {
    indicators: Vec<IndicatorView>,
}

impl Render for Preview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = IndicatorStyle::from_theme(cx.theme());
        let page = cx.theme().background;
        let panel = cx.theme().secondary;
        let text = cx.theme().foreground;

        div()
            .size_full()
            .overflow_y_scrollbar()
            .bg(page)
            .p(px(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(text)
                            .child("Tuning indicators"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(style.inactive_text)
                            .child("speeduino-202501.7.ini · [FrontPage] · demo frame"),
                    )
                    .child(
                        div().bg(panel).p(px(12.)).rounded(px(10.)).child(
                            IndicatorStrip::new(self.indicators.clone()).style(style.clone()),
                        ),
                    )
                    .child(state_legend(&style)),
            )
    }
}

fn main() {
    let indicators = load_indicators();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);

            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(120.), px(120.)),
                        size(px(1120.), px(760.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |_, cx| {
                    cx.new(|_| Preview {
                        indicators: indicators.clone(),
                    })
                },
            )
            .expect("open the indicator preview window");

            cx.activate(true);
        });
}

/// Reads the local definition and turns its `[FrontPage]` indicators into demo
/// cards. A missing or broken file falls back to the built-in sample so the
/// example still opens on a fresh checkout.
fn load_indicators() -> Vec<IndicatorView> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(INI_PATH);

    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!(
            "indicator preview: cannot read {}, using the built-in sample",
            path.display()
        );
        return sample_indicators();
    };

    let definition = match parse_bytes(&bytes, &SettingsContext::new()) {
        Ok(definition) => definition,
        Err(error) => {
            eprintln!(
                "indicator preview: cannot parse {}: {error}",
                path.display()
            );
            return sample_indicators();
        }
    };

    let indicators = definition
        .front_page
        .indicators
        .iter()
        .filter(|indicator| !indicator.template_only)
        .enumerate()
        .map(|(index, indicator)| {
            let (state, tone) = DEMO_STATES[index % DEMO_STATES.len()];
            let label = match state {
                IndicatorState::On => &indicator.on_label,
                IndicatorState::Off | IndicatorState::Unavailable => &indicator.off_label,
            };

            IndicatorView::new(label.clone(), state, tone)
        })
        .collect::<Vec<_>>();

    if indicators.is_empty() {
        eprintln!(
            "indicator preview: {} has no [FrontPage] indicators",
            path.display()
        );
        return sample_indicators();
    }

    indicators
}

/// The five visual states, for a checkout without the local definition.
fn sample_indicators() -> Vec<IndicatorView> {
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
