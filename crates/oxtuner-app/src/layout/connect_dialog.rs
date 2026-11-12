//! The connect dialog: source, port, baud, framing, a signature test and the
//! session state.
//!
//! The dialog is built once and **stays open**: the port scan fills the list in
//! place and a test or a connection updates the status block, so a refresh
//! never costs the user their selection or cursor. Its inputs live in a small
//! model next to the builder; the state it reports comes from the core, read
//! live on every frame.
//!
//! Test Connection is a signature-only probe (the definition's `queryCommand`,
//! nothing else). It proves the port, the baud and the framing before a session
//! reads a single page.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IndexPath, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ElementId, FontWeight, IntoElement as _, ParentElement as _,
    Styled as _, Window, div, px,
};
use oxtuner_core::{
    ConnectionView, DeviceView, FailureCode, LinkMode, PortChoice, PortView, SignatureVerdict,
};

use crate::core::{self, CoreService, TestResult};
use crate::i18n::t;

/// The baud rates the dialog offers.
const BAUDS: [u32; 6] = [9_600, 57_600, 115_200, 230_400, 500_000, 1_000_000];

/// Where the bytes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Serial,
    Simulator,
}

/// Which framing the session should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// The definition's own `messageEnvelopeFormat`.
    Auto,
    /// The unframed protocol.
    Legacy,
    /// `msEnvelope_1.0`.
    Envelope,
}

impl Framing {
    fn link_mode(self) -> Option<LinkMode> {
        match self {
            Self::Auto => None,
            Self::Legacy => Some(LinkMode::Legacy),
            Self::Envelope => Some(LinkMode::Envelope),
        }
    }
}

/// The dialog's own inputs, kept across frames and refreshes.
///
/// The baud and framing pickers are `Select` entities created once in [`show`];
/// their values are read from the entities on every frame, so opening the
/// dropdown or refreshing the ports never loses a choice.
struct DialogModel {
    mode: Mode,
    selected_port: Option<String>,
}

impl DialogModel {
    fn new(device: DeviceView) -> Self {
        Self {
            mode: Mode::Serial,
            selected_port: device.last_port,
        }
    }

    /// The chosen port, if the current mode has one.
    fn port_choice(&self, ports: &[PortView], baud: u32) -> Option<PortChoice> {
        match self.mode {
            Mode::Simulator => Some(crate::link::simulator()),
            Mode::Serial => {
                // A remembered or chosen port that is gone leaves no selection
                // rather than falling back to whichever port is first: a cable
                // moving must never point Connect at another board.
                let port = match self.selected_port.as_deref() {
                    Some(name) => ports
                        .iter()
                        .find(|port| port.name == name)
                        .map(|port| port.name.as_str()),
                    None => ports.first().map(|port| port.name.as_str()),
                }?;
                Some(PortChoice::serial(port.to_owned(), baud))
            }
        }
    }
}

/// Opens the connect dialog and starts a port scan.
#[expect(
    clippy::too_many_lines,
    reason = "the connect dialog builds its mode, port, test and status sections in one place"
)]
pub(crate) fn show(window: &mut Window, cx: &mut App) {
    // The device is per project: each project remembers the ECU it talks to.
    let device = CoreService::status(cx)
        .project
        .as_ref()
        .map(|project| project.device.clone())
        .unwrap_or_default();
    let last_port = device.last_port.clone();
    // The pickers own their selection; the dialog reads it each frame.
    let baud_index = device
        .baud_override
        .and_then(|baud| BAUDS.iter().position(|rate| *rate == baud))
        .unwrap_or(2);
    let model = Rc::new(RefCell::new(DialogModel::new(device)));
    core::refresh_ports(cx);

    let baud_state = cx.new(|cx| {
        SelectState::new(
            BAUDS.iter().map(u32::to_string).collect::<Vec<_>>(),
            Some(IndexPath::new(baud_index)),
            window,
            cx,
        )
    });
    let framing_state = cx.new(|cx| {
        SelectState::new(
            vec![
                t!("app.connect.framing_auto").to_string(),
                t!("app.connect.framing_legacy").to_string(),
                t!("app.connect.framing_envelope").to_string(),
            ],
            Some(IndexPath::new(0)),
            window,
            cx,
        )
    });

    crate::app::dialog::open(window, cx, move |dialog, _, cx| {
        let status = CoreService::status(cx);
        let ports = status.ports.clone();
        let scanning = status.scanning;
        let testing = status.testing.clone();
        let test = status.test.clone();
        let connection = status.connection.clone();
        let link_lost = status.link_lost.clone();
        let connecting = status.pending_connect.as_ref().map(PortChoice::label);
        let connect_failure = status
            .failure
            .as_ref()
            .filter(|failure| failure.code == FailureCode::ConnectFailed)
            .map(|failure| failure.message.clone());
        let has_definition = status.definition().is_some();

        // Read the inputs once; the closures below mutate a clone of the handle
        // or the select entity when the user chooses something.
        let baud = baud_state
            .read(cx)
            .selected_index(cx)
            .and_then(|index| BAUDS.get(index.row).copied())
            .unwrap_or(115_200);
        let framing = match framing_state
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row)
        {
            1 => Framing::Legacy,
            2 => Framing::Envelope,
            _ => Framing::Auto,
        };
        let (mode, chosen) = {
            let model = model.borrow();
            (model.mode, model.port_choice(&ports, baud))
        };
        // The remembered device is a hint, never a constraint: when the port is
        // not on this machine the dialog says so instead of silently picking
        // another ECU's port.
        let port_hint = last_port.as_ref().map(|last| {
            let present = ports.iter().any(|port| port.name == *last);
            (last.clone(), present)
        });

        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let success = cx.theme().success;
        let busy = testing.is_some() || connecting.is_some();
        let connected = connection.is_some();
        let ready = chosen.is_some() && has_definition;

        let mode_picker = h_flex()
            .gap_5()
            .child(
                Checkbox::new("connect-mode-serial")
                    .label(t!("app.connect.mode_serial"))
                    .checked(mode == Mode::Serial)
                    .on_change({
                        let model = Rc::clone(&model);
                        move |checked, _, cx| {
                            // One mode is always chosen: unchecking the active
                            // one does nothing, checking the other switches.
                            if *checked {
                                model.borrow_mut().mode = Mode::Serial;
                                cx.refresh_windows();
                            }
                        }
                    }),
            )
            .child(
                Checkbox::new("connect-mode-simulator")
                    .label(t!("app.connect.mode_simulator"))
                    .checked(mode == Mode::Simulator)
                    .on_change({
                        let model = Rc::clone(&model);
                        move |checked, _, cx| {
                            if *checked {
                                model.borrow_mut().mode = Mode::Simulator;
                                cx.refresh_windows();
                            }
                        }
                    }),
            );

        let port_section = if mode == Mode::Serial {
            let rows: Vec<AnyElement> = ports
                .iter()
                .map(|port| {
                    let name = port.name.clone();
                    let checked = chosen
                        .as_ref()
                        .is_some_and(|choice| choice.label() == port.name);
                    // Keyed by device path, not list position: a rescan that
                    // reorders or removes ports must not move a check mark to
                    // a different device.
                    let id = ElementId::Name(format!("connect-port-{}", port.name).into());
                    div()
                        .py_0p5()
                        .child(
                            Checkbox::new(id)
                                .label(port.label.clone())
                                .checked(checked)
                                .on_change({
                                    let model = Rc::clone(&model);
                                    move |checked, _, cx| {
                                        if *checked {
                                            model.borrow_mut().selected_port = Some(name.clone());
                                            cx.refresh_windows();
                                        }
                                    }
                                }),
                        )
                        .into_any_element()
                })
                .collect();

            let empty = if scanning {
                t!("app.connect.scanning").to_string()
            } else {
                t!("app.connect.no_ports").to_string()
            };

            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(muted)
                                .child(t!("app.connect.ports", count = ports.len().to_string())),
                        )
                        .child(
                            Button::new("connect-refresh")
                                .label(t!("app.connect.refresh"))
                                .icon(IconName::RefreshCw)
                                .small()
                                .ghost()
                                .disabled(busy)
                                .on_click(|_, _, cx| {
                                    // The dialog stays where it is; the list
                                    // fills in when the scan answers.
                                    core::refresh_ports(cx);
                                    cx.refresh_windows();
                                }),
                        ),
                )
                .child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().border)
                        .p_2()
                        .max_h(px(160.))
                        .overflow_y_scrollbar()
                        .id("connect-port-list")
                        .child(
                            v_flex()
                                .gap_1()
                                .when(rows.is_empty(), |this| {
                                    this.child(div().text_xs().text_color(muted).child(empty))
                                })
                                .children(rows),
                        ),
                )
                .when_some(port_hint, |this, (last, present)| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(if present { muted } else { danger })
                            .child(if present {
                                t!("app.connect.project_port", port = last)
                            } else {
                                t!("app.connect.project_port_missing", port = last)
                            }),
                    )
                })
                .into_any_element()
        } else {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("app.connect.simulator_hint")),
                )
                .into_any_element()
        };

        let baud_section = if mode == Mode::Serial {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(muted)
                        .child(t!("app.connect.baud")),
                )
                .child(
                    Select::new(&baud_state)
                        .w_full()
                        .disabled(busy)
                        .accessibility_label(t!("app.connect.baud")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(t!("app.connect.baud_hint")),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let framing_section = v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(muted)
                    .child(t!("app.connect.framing")),
            )
            .child(
                Select::new(&framing_state)
                    .w_full()
                    .disabled(busy)
                    .accessibility_label(t!("app.connect.framing")),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.connect.framing_hint")),
            );

        let status_block = session_status(
            cx,
            connection.as_ref(),
            link_lost.as_deref(),
            testing.as_deref(),
            test.as_ref(),
            connecting.as_deref(),
            muted,
            danger,
            success,
        );

        let test_button = Button::new("connect-test")
            .label(if testing.is_some() {
                t!("app.connect.testing")
            } else {
                t!("app.connect.test")
            })
            .icon(IconName::Search)
            .outline()
            .disabled(!ready || busy || connected);

        let test_button = match chosen.clone() {
            Some(choice) => test_button.on_click({
                let framing = framing.link_mode();
                move |_, _, cx| {
                    core::test_connection(choice.clone(), framing, cx);
                    cx.refresh_windows();
                }
            }),
            None => test_button,
        };

        let connect_button = if connected {
            Button::new("connect-disconnect")
                .label(t!("app.connect.disconnect"))
                .icon(IconName::CircleX)
                .outline()
                .on_click(|_, _, cx| {
                    core::disconnect(cx);
                    cx.refresh_windows();
                })
                .into_any_element()
        } else {
            let label = if connecting.is_some() {
                t!("app.connect.connecting")
            } else {
                t!("app.connect.connect")
            };
            let button = Button::new("connect-connect")
                .label(label)
                .icon(IconName::Play)
                .primary()
                .disabled(!ready || busy);
            match chosen {
                Some(choice) => button
                    .on_click({
                        let framing = framing.link_mode();
                        move |_, _, cx| {
                            core::connect(choice.clone(), framing, cx);
                            cx.refresh_windows();
                        }
                    })
                    .into_any_element(),
                None => button.into_any_element(),
            }
        };

        dialog
            .title(t!("app.connect.title"))
            .width(px(560.))
            .child(
                v_flex()
                    .gap_4()
                    .child(section(
                        t!("app.connect.mode").to_string(),
                        mode_picker.into_any_element(),
                        muted,
                    ))
                    .child(section(
                        t!("app.connect.port").to_string(),
                        port_section,
                        muted,
                    ))
                    .child(baud_section)
                    .child(framing_section)
                    .when_some(connect_failure, |this, message| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.connect.connect_failed", message = message)),
                        )
                    })
                    .when(!has_definition, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(t!("app.connect.no_definition")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.connect.test_hint")),
                    )
                    .child(status_block),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("connect-close")
                            .label(t!("app.connect.close"))
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(test_button)
                    .child(connect_button),
            )
    });
}

/// A labelled section.
fn section(title: String, body: AnyElement, muted: gpui_kit::Hsla) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(title),
        )
        .child(body)
        .into_any_element()
}

/// The session/test block at the bottom of the dialog.
#[expect(
    clippy::too_many_arguments,
    reason = "the status block renders the connection, link and test facts together"
)]
fn session_status(
    cx: &App,
    connection: Option<&ConnectionView>,
    link_lost: Option<&str>,
    testing: Option<&str>,
    test: Option<&TestResult>,
    connecting: Option<&str>,
    muted: gpui_kit::Hsla,
    danger: gpui_kit::Hsla,
    success: gpui_kit::Hsla,
) -> AnyElement {
    let border = cx.theme().border;
    let background = cx.theme().muted;

    v_flex()
        .gap_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(background)
        .p_3()
        .child(match connection {
            Some(connection) => h_flex()
                .gap_2()
                .items_center()
                .child(div().size(px(8.)).rounded_full().bg(success))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(t!(
                    "app.connect.status_connected",
                    signature = connection.signature.clone()
                )))
                .when(connection.read_only, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(danger)
                            .child(t!("app.connect.status_read_only")),
                    )
                })
                .into_any_element(),
            None if testing.is_some() => h_flex()
                .gap_2()
                .items_center()
                .child(div().size(px(8.)).rounded_full().bg(muted))
                .child(div().text_sm().child(t!(
                    "app.connect.status_testing",
                    port = testing.unwrap_or_default().to_owned()
                )))
                .into_any_element(),
            None if connecting.is_some() => h_flex()
                .gap_2()
                .items_center()
                .child(div().size(px(8.)).rounded_full().bg(muted))
                .child(div().text_sm().child(t!(
                    "app.connect.status_connecting",
                    port = connecting.unwrap_or_default().to_owned()
                )))
                .into_any_element(),
            None => h_flex()
                .gap_2()
                .items_center()
                .child(div().size(px(8.)).rounded_full().bg(muted))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("app.connect.status_not_connected")),
                )
                .into_any_element(),
        })
        .when_some(link_lost, |this, reason| {
            this.child(div().text_xs().text_color(danger).child(t!(
                "app.connect.status_link_lost",
                reason = reason.to_owned()
            )))
        })
        .when_some(test, |this, test| {
            let line: AnyElement = match &test.outcome {
                Ok(summary) if summary.verdict == SignatureVerdict::Match => h_flex()
                    .gap_2()
                    .child(div().text_xs().text_color(success).child(t!(
                        "app.connect.test_match",
                        signature = summary.signature.clone()
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(t!("app.connect.test_port", port = summary.port.clone())),
                    )
                    .into_any_element(),
                Ok(summary) => div()
                    .text_xs()
                    .text_color(danger)
                    .child(t!(
                        "app.connect.test_mismatch",
                        signature = summary.signature.clone(),
                        expected = summary.definition_signature.clone()
                    ))
                    .into_any_element(),
                Err(message) => div()
                    .text_xs()
                    .text_color(danger)
                    .child(t!(
                        "app.connect.test_failed",
                        port = test.port.clone(),
                        message = message.clone()
                    ))
                    .into_any_element(),
            };
            this.child(line)
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ports() -> Vec<PortView> {
        vec![
            PortView {
                name: "/dev/cu.first".to_owned(),
                label: "first".to_owned(),
                vid: Some(0x2341),
                pid: Some(0x0042),
                serial: Some("A".to_owned()),
            },
            PortView {
                name: "/dev/cu.second".to_owned(),
                label: "second".to_owned(),
                vid: Some(0x1A86),
                pid: Some(0x7523),
                serial: None,
            },
        ]
    }

    fn serial_model(selected: Option<&str>) -> DialogModel {
        DialogModel {
            mode: Mode::Serial,
            selected_port: selected.map(str::to_owned),
        }
    }

    #[test]
    fn a_remembered_port_is_kept_when_the_scan_still_lists_it() {
        let choice = serial_model(Some("/dev/cu.second"))
            .port_choice(&ports(), 115_200)
            .expect("a port");

        assert!(matches!(
            choice,
            PortChoice::Serial { ref port, baud } if port == "/dev/cu.second" && baud == 115_200
        ));
    }

    #[test]
    fn a_remembered_port_that_is_gone_leaves_no_selection() {
        // Falling back to the first scanned port would point Connect at a board
        // the user never chose, which is what a moved cable would trigger.
        let choice = serial_model(Some("/dev/cu.missing")).port_choice(&ports(), 57_600);

        assert!(choice.is_none());
    }

    #[test]
    fn no_scanned_port_means_no_serial_choice() {
        assert!(serial_model(None).port_choice(&[], 115_200).is_none());
        assert!(
            serial_model(Some("/dev/cu.first"))
                .port_choice(&[], 115_200)
                .is_none()
        );
    }

    #[test]
    fn simulator_mode_ignores_the_scanned_ports() {
        let model = DialogModel {
            mode: Mode::Simulator,
            selected_port: None,
        };

        let choice = model.port_choice(&[], 115_200).expect("the simulator link");

        assert!(!choice.is_serial());
        assert_eq!(choice.label(), crate::link::simulator().label());
    }
}
