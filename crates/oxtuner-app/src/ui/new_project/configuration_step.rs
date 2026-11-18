//! Step 3: the definition's setting groups, one choice each.

use std::path::PathBuf;

use gpui_kit::base::v_flex;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, FontWeight, IntoElement as _, ParentElement as _,
    Styled as _, Window, div,
};
use rust_i18n::t;

use super::shared::section_title;
use super::state::{GroupSelect, SharedWizard};

/// A group picker copied out of the model, so its entity can be read while the
/// model is not borrowed.
type GroupSelectSnapshot = (
    String,
    Vec<Option<String>>,
    Entity<SelectState<Vec<String>>>,
);

/// Builds one picker per group the first time a preview arrives.
///
/// The pickers own their selection; the model only mirrors it.
pub(super) fn ensure_pickers(
    model: &SharedWizard,
    definition: Option<&PathBuf>,
    preview: &oxtuner_core::DefinitionPreview,
    window: &mut Window,
    cx: &mut App,
) {
    if model.borrow().preview_path.as_ref() == definition {
        return;
    }

    let display = display_groups(preview);
    let mut wizard = model.borrow_mut();
    wizard.group_selects.clear();
    for group in &display {
        let (values, labels, selected) = group_picker(group);
        let state =
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(selected)), window, cx));
        wizard.group_selects.push(GroupSelect {
            name: group.name.clone(),
            values,
            state,
        });
    }
    wizard.display_groups = display;
    wizard.preview_path = definition.cloned();
}

/// The pickers' current choices, read where they are needed.
///
/// The selects own their selection, so the review and Create read it here
/// instead of keeping a copy the render pass would have to refresh.
pub(super) fn choices(
    model: &SharedWizard,
    cx: &App,
) -> std::collections::BTreeMap<String, Option<String>> {
    let selects: Vec<GroupSelectSnapshot> = model
        .borrow()
        .group_selects
        .iter()
        .map(|select| {
            (
                select.name.clone(),
                select.values.clone(),
                select.state.clone(),
            )
        })
        .collect();

    let mut choices = std::collections::BTreeMap::new();
    for (name, values, state) in selects {
        let index = state
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row);
        choices.insert(name, values.get(index).cloned().flatten());
    }

    choices
}

/// The groups the configuration step shows.
///
/// The derivation rule lives in [`oxtuner_core::DefinitionPreview`], so the
/// step, the review and creation read the same list. Only the text is local:
/// the canonical labels are replaced by the interface language here.
pub(super) fn display_groups(
    preview: &oxtuner_core::DefinitionPreview,
) -> Vec<oxtuner_core::SettingGroupView> {
    preview
        .groups_with_display_choices()
        .into_iter()
        .map(localized)
        .collect()
}

/// Replaces a derived group's canonical labels with the interface language.
fn localized(mut group: oxtuner_core::SettingGroupView) -> oxtuner_core::SettingGroupView {
    match group.name.as_str() {
        "lambda_display" => {
            group.label = t!("app.new_project.lambda_display").to_string();
            for option in &mut group.options {
                option.label = if option.name.eq_ignore_ascii_case("LAMBDA") {
                    t!("app.new_project.lambda").to_string()
                } else {
                    t!("app.new_project.afr").to_string()
                };
            }
        }
        "temperature_display" => {
            group.label = t!("app.new_project.temperature_display").to_string();
            for option in &mut group.options {
                option.label = if option.name.eq_ignore_ascii_case("CELSIUS") {
                    t!("app.new_project.celsius").to_string()
                } else {
                    t!("app.new_project.fahrenheit").to_string()
                };
            }
        }
        _ => {}
    }

    group
}

/// The rows a group's picker offers, their values, and the preselected row.
fn group_picker(
    group: &oxtuner_core::SettingGroupView,
) -> (Vec<Option<String>>, Vec<String>, usize) {
    let suffix = t!("app.new_project.default_suffix").to_string();
    let default = default_choice(group);

    if group.options.is_empty() {
        let off = t!("app.new_project.deactivated").to_string();
        let on = t!("app.new_project.activated").to_string();
        let labels = if group.default_active {
            vec![off, format!("{on}{suffix}")]
        } else {
            vec![format!("{off}{suffix}"), on]
        };

        return (
            vec![None, Some(group.name.clone())],
            labels,
            usize::from(group.default_active),
        );
    }

    let values: Vec<Option<String>> = group
        .options
        .iter()
        .map(|option| Some(option.name.clone()))
        .collect();
    let labels: Vec<String> = group
        .options
        .iter()
        .map(|option| {
            if option.name.eq_ignore_ascii_case("DEFAULT") {
                format!("{}{suffix}", option.label)
            } else {
                option.label.clone()
            }
        })
        .collect();
    let selected = values
        .iter()
        .position(|value| *value == default)
        .unwrap_or(0);

    (values, labels, selected)
}

/// Step 3: the definition's setting groups, one choice each.
pub(super) fn configuration_body(
    model: &SharedWizard,
    preview: Option<&oxtuner_core::DefinitionPreview>,
    inspect_error: Option<&str>,
    cx: &App,
    creating: bool,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;

    if preview.is_none() {
        return v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.new_project.configuration_waiting")),
            )
            .when_some(inspect_error.map(str::to_owned), |this, message| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child(t!("app.new_project.inspect_failed", message = message)),
                )
            })
            .into_any_element();
    }

    let wizard = model.borrow();
    let mut groups = v_flex().gap_3();
    for group in &wizard.display_groups {
        let select = wizard
            .group_selects
            .iter()
            .find(|select| select.name == group.name);
        groups = groups.child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(muted)
                        .child(group.label.clone()),
                )
                .child(select.map_or_else(
                    || div().into_any_element(),
                    |select| {
                        Select::new(&select.state)
                            .w_full()
                            .disabled(creating)
                            .accessibility_label(group.label.clone())
                            .into_any_element()
                    },
                )),
        );
    }
    drop(wizard);

    v_flex()
        .gap_3()
        .child(section_title(
            t!("app.new_project.configuration").to_string(),
            cx,
        ))
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.new_project.configuration_hint")),
        )
        .child(groups)
        .into_any_element()
}

/// The choice a group starts on when the dialog opens.
fn default_choice(group: &oxtuner_core::SettingGroupView) -> Option<String> {
    if group.options.is_empty() {
        // A boolean group's own name is its flag; `None` means off.
        group.default_active.then(|| group.name.clone())
    } else {
        group.default.clone()
    }
}

/// The display text of one group's choice.
pub(super) fn choice_label(group: &oxtuner_core::SettingGroupView, choice: Option<&str>) -> String {
    if group.options.is_empty() {
        return if choice.is_some() {
            t!("app.new_project.activated").to_string()
        } else {
            t!("app.new_project.deactivated").to_string()
        };
    }

    let option = choice
        .and_then(|name| group.options.iter().find(|option| option.name == name))
        .or_else(|| {
            group
                .default
                .as_deref()
                .and_then(|name| group.options.iter().find(|option| option.name == name))
        });

    option.map_or_else(String::new, |option| option.label.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, label: &str, options: &[(&str, &str)]) -> oxtuner_core::SettingGroupView {
        oxtuner_core::SettingGroupView {
            name: name.to_owned(),
            label: label.to_owned(),
            options: options
                .iter()
                .map(|(name, label)| oxtuner_core::SettingOptionView {
                    name: (*name).to_owned(),
                    label: (*label).to_owned(),
                })
                .collect(),
            default: Some("DEFAULT".to_owned()),
            default_active: false,
        }
    }

    fn preview_with_flags(flags: &[&str]) -> oxtuner_core::DefinitionPreview {
        preview_with_groups(
            flags,
            vec![group(
                "pressure_units",
                "Pressure Display",
                &[("DEFAULT", "PSI")],
            )],
        )
    }

    fn preview_with_groups(
        flags: &[&str],
        groups: Vec<oxtuner_core::SettingGroupView>,
    ) -> oxtuner_core::DefinitionPreview {
        oxtuner_core::DefinitionPreview {
            signature: "speeduino 202501".to_owned(),
            tested_flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
            groups,
        }
    }

    #[test]
    fn a_group_starts_on_the_default_the_definition_names() {
        let group = oxtuner_core::SettingGroupView {
            name: "pressure_units".to_owned(),
            label: "Pressure Display".to_owned(),
            options: vec![
                oxtuner_core::SettingOptionView {
                    name: "DEFAULT".to_owned(),
                    label: "PSI".to_owned(),
                },
                oxtuner_core::SettingOptionView {
                    name: "pressure_bar".to_owned(),
                    label: "BAR".to_owned(),
                },
            ],
            default: Some("DEFAULT".to_owned()),
            default_active: false,
        };

        assert_eq!(default_choice(&group).as_deref(), Some("DEFAULT"));
    }

    #[test]
    fn a_definition_that_tests_lambda_and_celsius_gets_both_choices() {
        let groups = display_groups(&preview_with_flags(&["lambda", "celsius"]));

        assert_eq!(groups.len(), 3, "two display choices plus the INI group");
        assert_eq!(groups[0].name, "lambda_display");
        assert_eq!(groups[0].options[1].name, "LAMBDA");
        assert_eq!(groups[0].default.as_deref(), Some("DEFAULT"));
        assert_eq!(groups[1].name, "temperature_display");
        assert_eq!(groups[1].options[1].name, "CELSIUS");
        assert_eq!(groups[2].name, "pressure_units");
    }

    #[test]
    fn an_ini_group_with_the_lambda_choice_keeps_its_own_labels() {
        // The definition names the group and its options; those labels win
        // over the application's built-in row.
        let preview = preview_with_groups(
            &["lambda"],
            vec![group(
                "lambdaSensor",
                "Oxygen Sensor / Display",
                &[
                    ("NARROW_BAND_EGO", "Narrowband Sensor - Volts"),
                    ("LAMBDA", "Wideband - Lambda"),
                    ("DEFAULT", "WideBand - AFR"),
                ],
            )],
        );

        let groups = display_groups(&preview);

        assert_eq!(groups.len(), 1, "no application row is added");
        assert_eq!(groups[0].name, "lambdaSensor");
        assert_eq!(groups[0].label, "Oxygen Sensor / Display");
    }

    #[test]
    fn an_ini_group_with_a_celsius_option_keeps_its_own_labels() {
        let preview = preview_with_groups(
            &["celsius"],
            vec![group(
                "temperature",
                "Coolant Display",
                &[("DEFAULT", "Fahrenheit"), ("CELSIUS", "Celsius")],
            )],
        );

        let groups = display_groups(&preview);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "temperature");
        assert_eq!(groups[0].label, "Coolant Display");
    }

    #[test]
    fn a_definition_that_tests_neither_gets_no_display_choice() {
        let groups = display_groups(&preview_with_flags(&[]));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "pressure_units");
    }

    #[test]
    fn a_boolean_group_starts_on_its_declared_state() {
        let group = |active| oxtuner_core::SettingGroupView {
            name: "enablehardware_test".to_owned(),
            label: "Enable Hardware Test Page".to_owned(),
            options: Vec::new(),
            default: None,
            default_active: active,
        };

        assert_eq!(default_choice(&group(false)), None);
        assert_eq!(
            default_choice(&group(true)).as_deref(),
            Some("enablehardware_test")
        );
    }
}
