//! Step 4: pick the connection the project starts on.
//!
//! The step never connects: it is a list the user picks from, plus a signature
//! test that proves the choice before Create. The type select leaves room for
//! tomorrow's transports; today it offers serial (USB included) and the
//! simulator. The serial pickers follow the detection while the user has not
//! chosen; once a selection differs from the synced default it is the user's
//! and a later detection or rescan leaves it alone. A rescan keeps a
//! still-present choice by value and drops only a port that vanished, which
//! falls back to the default.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::v_flex;
use gpui_kit::component::button::Button;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IndexPath};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, FontWeight, IntoElement as _, ParentElement as _,
    Styled as _, Window, div,
};
use oxtuner_core::{DEFAULT_BAUD_RATE, PICKABLE_BAUD_RATES, PortChoice};
use rust_i18n::t;

use super::shared::section_title;
use super::state::{ConnectionType, SharedWizard, TestedSelection, Wizard};
use crate::core::{self};

/// Keeps the connection pickers behind the wizard's inputs.
///
/// Call every frame: building and resyncing are both idempotent, and the
/// entities keep the user's dropdown state across frames.
pub(super) fn sync_connection_pickers(
    model: &SharedWizard,
    ports: &[oxtuner_core::PortView],
    detected: Option<(&str, u32)>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut wizard = model.borrow_mut();

    if wizard.connection_type_select.is_none() {
        let labels = vec![
            t!("app.connect.mode_serial").to_string(),
            t!("app.connect.mode_simulator").to_string(),
        ];
        wizard.connection_type_select =
            Some(cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(0)), window, cx)));
    }

    if wizard.baud_select.is_none() {
        let labels: Vec<String> = PICKABLE_BAUD_RATES.iter().map(u32::to_string).collect();
        // DEFAULT_BAUD_RATE is always in the list; the fallback only satisfies the
        // type system and picks the slowest, safest rate.
        let selected = PICKABLE_BAUD_RATES
            .iter()
            .position(|baud| *baud == DEFAULT_BAUD_RATE)
            .unwrap_or(0);
        wizard.baud_select =
            Some(cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(selected)), window, cx)));
        wizard.connection_auto_baud = PICKABLE_BAUD_RATES[selected];
    }

    let names: Vec<String> = ports.iter().map(|port| port.name.clone()).collect();
    if wizard.port_select.is_none() || wizard.connection_ports != names {
        let labels: Vec<String> = ports.iter().map(|port| port.label.clone()).collect();
        let current = wizard
            .port_select
            .as_ref()
            .and_then(|select| select.read(cx).selected_index(cx))
            .and_then(|index| wizard.connection_ports.get(index.row).cloned());
        // A still-present choice survives the rebuild; anything else falls
        // back to the default instead of pointing at another device.
        let want = connection_default_port(&names, detected.map(|(port, _)| port));
        let selected = current.filter(|name| names.contains(name)).or(want);
        wizard.connection_auto_port.clone_from(&selected);
        wizard.port_select = Some(cx.new(|cx| {
            SelectState::new(
                labels,
                selected.as_ref().and_then(|name| {
                    names
                        .iter()
                        .position(|known| known == name)
                        .map(IndexPath::new)
                }),
                window,
                cx,
            )
        }));
        wizard.connection_ports = names;
    }
    drop(wizard);

    follow_detection(model, detected, window, cx);
}

/// Follows the detection while the user has not chosen.
///
/// A selection that still matches the last synced default is not the user's
/// own, so a later detection moves it along. Anything else is left alone.
fn follow_detection(
    model: &SharedWizard,
    detected: Option<(&str, u32)>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut wizard = model.borrow_mut();
    let want_port = detected
        .map(|(port, _)| port.to_owned())
        .or_else(|| wizard.connection_ports.first().cloned());
    let want_baud = detected.map_or(DEFAULT_BAUD_RATE, |(_, baud)| baud);

    let current_port = wizard
        .port_select
        .as_ref()
        .and_then(|select| select.read(cx).selected_index(cx))
        .and_then(|index| wizard.connection_ports.get(index.row).cloned());
    if current_port == wizard.connection_auto_port
        && current_port.as_deref() != want_port.as_deref()
    {
        set_port_selection(&mut wizard, want_port.as_deref(), window, cx);
        wizard.connection_auto_port = want_port;
    }

    let current_baud = wizard
        .baud_select
        .as_ref()
        .and_then(|select| select.read(cx).selected_index(cx))
        .and_then(|index| PICKABLE_BAUD_RATES.get(index.row).copied());
    // A selection that still matches the last synced default is not the
    // user's own, so a later detection moves it along. Anything else is left
    // alone.
    let follows = current_baud == Some(wizard.connection_auto_baud);
    if follows && current_baud != Some(want_baud) {
        set_baud_selection(&mut wizard, want_baud, window, cx);
        wizard.connection_auto_baud = want_baud;
    }
}

/// Points the port picker at `name`, clearing it when the port is gone.
fn set_port_selection(wizard: &mut Wizard, name: Option<&str>, window: &mut Window, cx: &mut App) {
    let index = name.and_then(|name| {
        wizard
            .connection_ports
            .iter()
            .position(|known| known == name)
            .map(IndexPath::new)
    });
    if let Some(select) = wizard.port_select.as_ref() {
        select.update(cx, |state, cx| {
            state.set_selected_index(index, window, cx);
        });
    }
}

/// Points the baud picker at `baud`.
fn set_baud_selection(wizard: &mut Wizard, baud: u32, window: &mut Window, cx: &mut App) {
    let index = PICKABLE_BAUD_RATES
        .iter()
        .position(|known| *known == baud)
        .map(IndexPath::new);
    if let Some(select) = wizard.baud_select.as_ref() {
        select.update(cx, |state, cx| {
            state.set_selected_index(index, window, cx);
        });
    }
}

/// The default port: the detection when it answered, else the first port.
fn connection_default_port(ports: &[String], detected: Option<&str>) -> Option<String> {
    detected
        .map(str::to_owned)
        .or_else(|| ports.first().cloned())
}

/// The connection type the user picked; serial until they say otherwise.
pub(super) fn connection_type(model: &SharedWizard, cx: &App) -> ConnectionType {
    let serial = model
        .borrow()
        .connection_type_select
        .as_ref()
        .and_then(|select| select.read(cx).selected_index(cx))
        .is_none_or(|index| index.row == 0);

    if serial {
        ConnectionType::Serial
    } else {
        ConnectionType::Simulator
    }
}

/// The connection the project is created with: the pickers' selection.
///
/// With no serial ports there is nothing to pair, so the baud falls back to
/// zero and keeps the definition's own rate. The simulator link cannot be
/// stored in the project, so it creates the same port-less project and the
/// link is chosen after creation, from the project's own dialog.
pub(super) fn effective_connection(model: &SharedWizard, cx: &App) -> (Option<String>, u32) {
    if connection_type(model, cx) == ConnectionType::Simulator {
        return (None, 0);
    }

    let wizard = model.borrow();
    let port = wizard
        .port_select
        .as_ref()
        .and_then(|select| select.read(cx).selected_index(cx))
        .and_then(|index| wizard.connection_ports.get(index.row).cloned());
    let baud = wizard
        .baud_select
        .as_ref()
        .and_then(|select| select.read(cx).selected_index(cx))
        .and_then(|index| PICKABLE_BAUD_RATES.get(index.row).copied())
        .unwrap_or(DEFAULT_BAUD_RATE);

    match port {
        Some(port) => (Some(port), baud),
        None => (None, 0),
    }
}

/// Step 4: the connection the project starts on.
///
/// Labels sit above their fields, the way the configuration step does it. The
/// test proves the choice reads a signature; connecting itself happens after
/// Create, from the project's own dialog.
pub(super) fn connection_body(
    model: &SharedWizard,
    status: &crate::core::CoreStatus,
    creating: bool,
    cx: &App,
) -> AnyElement {
    let connected = status.connection.is_some();
    let busy = status.testing.is_some() || connected || creating;
    let kind = connection_type(model, cx);
    let (port, baud) = effective_connection(model, cx);
    // A test answer belongs to the pickers' state when Test was pressed, not
    // to whatever they show now: changing the selection afterwards hides the
    // stale result instead of attributing it to the new target.
    let show_test =
        model.borrow().tested.as_ref().is_some_and(|tested| {
            tested.kind == kind && tested.port == port && tested.baud == baud
        });
    let test_model = Rc::clone(model);

    let mut body = v_flex()
        .gap_3()
        .child(section_title(
            t!("app.new_project.connection").to_string(),
            cx,
        ))
        .child(connection_select(
            &t!("app.new_project.connection_type"),
            model.borrow().connection_type_select.clone(),
            cx,
            creating,
        ));

    if kind == ConnectionType::Serial {
        body = body
            .child(connection_select(
                &t!("app.new_project.port"),
                model.borrow().port_select.clone(),
                cx,
                creating,
            ))
            .child(connection_select(
                &t!("app.new_project.baud"),
                model.borrow().baud_select.clone(),
                cx,
                creating,
            ));
    }

    body = body.child(
        Button::new("new-project-test-connection")
            .label(if let Some(testing) = status.testing.as_ref() {
                t!("app.connect.testing", port = testing.port.clone())
            } else {
                t!("app.connect.test")
            })
            .icon(IconName::Search)
            .outline()
            .disabled(busy || (kind == ConnectionType::Serial && port.is_none()))
            .on_click(move |_, _, cx| {
                let choice = match kind {
                    ConnectionType::Serial => {
                        port.clone().map(|port| PortChoice::serial(port, baud))
                    }
                    ConnectionType::Simulator => Some(crate::link::simulator()),
                };
                if let Some(choice) = choice {
                    test_model.borrow_mut().tested = Some(TestedSelection {
                        kind,
                        port: port.clone(),
                        baud,
                    });
                    core::test_connection(choice, None, cx);
                }
            }),
    );

    if show_test && let Some(test) = &status.test {
        body = body.child(match &test.outcome {
            Ok(view) => div().text_xs().child(t!(
                "app.new_project.test_signature",
                signature = view.signature.clone(),
                port = test.port.clone()
            )),
            Err(message) => div()
                .text_xs()
                .text_color(cx.theme().danger)
                .child(message.clone()),
        });
    }

    body.into_any_element()
}

/// One labelled picker: the label above, the field full width below.
fn connection_select(
    label: &str,
    state: Option<Entity<SelectState<Vec<String>>>>,
    cx: &App,
    creating: bool,
) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(cx.theme().muted_foreground)
                .child(label.to_owned()),
        )
        .child(state.map_or_else(
            || div().into_any_element(),
            |state| {
                Select::new(&state)
                    .w_full()
                    .disabled(creating)
                    .accessibility_label(label.to_owned())
                    .into_any_element()
            },
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ports() -> Vec<String> {
        vec!["/dev/cu.first".to_owned(), "/dev/cu.second".to_owned()]
    }

    #[test]
    fn a_detection_sets_the_default_port() {
        assert_eq!(
            connection_default_port(&ports(), Some("/dev/cu.second")),
            Some("/dev/cu.second".to_owned())
        );
    }

    #[test]
    fn without_a_detection_the_first_port_wins() {
        assert_eq!(
            connection_default_port(&ports(), None),
            Some("/dev/cu.first".to_owned())
        );
    }

    #[test]
    fn without_ports_there_is_nothing_to_default_to() {
        assert_eq!(connection_default_port(&[], None), None);
    }
}
