//! `[SettingGroups]`, `[UiDialogs]` / `[UserDefined]`, `[Menu]`,
//! `[SettingContextHelp]` and `[KeyActions]`.
//!
//! The dialog system is the "firmware defines its own UI" part of the dialect.
//! Rows are kept in document order with their keyword, so a row kind this build
//! does not render yet is still data the UI layer can pick up later — never a
//! reason to reject the file.

use crate::fields::split_fields;
use crate::model::{
    Definition, Dialog, DialogRow, KeyAction, Menu, MenuItem, MenuTargetKind, RowKind,
    SettingGroup, SettingOption,
};

use super::SectionParser;

/// `[SettingGroups]`: options become `#if` flags.
pub(crate) fn parse_setting_groups(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut current: Option<String> = None;

    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let fields = split_fields(value);
        let lowered = key.to_ascii_lowercase();

        match lowered.as_str() {
            "settinggroup" => {
                let Some(name) = fields.first().copied() else {
                    section.warn("bad-setting-group", "`settingGroup =` needs a name", *line);
                    continue;
                };
                let label = fields.get(1).map_or_else(
                    || (*name).to_owned(),
                    |field| crate::fields::unquote(field).to_owned(),
                );
                if let Some(existing) = definition.setting_groups.get_mut(name) {
                    // A repeated header continues the group: options collected
                    // under either header are kept in one list.
                    existing.label = label;
                    section.note(
                        "merged-setting-group",
                        format!("setting group `{name}` is declared more than once; options are appended"),
                        *line,
                    );
                } else {
                    definition.setting_groups.insert(
                        name.to_owned(),
                        SettingGroup {
                            name: name.to_owned(),
                            label,
                            options: Vec::new(),
                        },
                    );
                }
                current = Some(name.to_owned());
                section.understand(*line);
            }
            "settingoption" => {
                let Some(name) = fields.first().copied() else {
                    section.warn(
                        "bad-setting-option",
                        "`settingOption =` needs a name",
                        *line,
                    );
                    continue;
                };
                let label = fields.get(1).map_or_else(
                    || (*name).to_owned(),
                    |field| crate::fields::unquote(field).to_owned(),
                );
                let Some(group_name) = current.as_deref() else {
                    section.warn(
                        "orphan-setting-option",
                        "`settingOption` appears before any `settingGroup`",
                        *line,
                    );
                    continue;
                };
                if let Some(group) = definition.setting_groups.get_mut(group_name) {
                    group.options.push(SettingOption {
                        name: name.to_owned(),
                        label,
                    });
                    section.understand(*line);
                }
            }
            _ => section.unrecognized_key(key, *line),
        }
    }
}

/// `[UiDialogs]` and `[UserDefined]`.
pub(crate) fn parse_dialogs(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    // The open container is tracked by name rather than by value: a panel that
    // is declared on its own (`indicatorPanel = name, 2`) has to stay in the
    // same ordered collection as the dialogs that reference it.
    let mut current: Option<String> = None;

    for (line, text) in lines {
        section.count_entry();

        // `dialog = name, "Title", layout` opens a block; every other line is a
        // row or a bare keyword.
        let assignment = crate::fields::split_assignment(text);

        if let Some((key, value)) = assignment
            && key.eq_ignore_ascii_case("dialog")
        {
            let fields = split_fields(value);

            let Some(name) = fields.first().copied() else {
                section.warn("bad-dialog", "`dialog =` needs a name", *line);
                continue;
            };
            let dialog = Dialog {
                name: name.to_owned(),
                title: fields
                    .get(1)
                    .map(|field| crate::fields::unquote(field).to_owned()),
                layout: fields.get(2).map(|field| field.to_ascii_lowercase()),
                rows: Vec::new(),
                line: *line,
            };
            current = Some(name.to_owned());
            if let Some(existing) = definition.dialogs.get_mut(name) {
                // A second header continues the dialog instead of starting a
                // new one, so rows written under either header stay together.
                if dialog.title.is_some() {
                    existing.title = dialog.title;
                }
                if dialog.layout.is_some() {
                    existing.layout = dialog.layout;
                }
                section.note(
                    "merged-dialog",
                    format!("dialog `{name}` is declared more than once; rows are appended"),
                    *line,
                );
            } else {
                definition.dialogs.insert(name, dialog);
            }
            section.understand(*line);
            continue;
        }

        let (keyword, fields) = match assignment {
            Some((key, value)) => (key.trim_ascii().to_owned(), split_fields(value)),
            None => (text.trim_ascii().to_owned(), Vec::new()),
        };
        let kind = RowKind::parse(&keyword);

        // A container keyword always starts its own container: `indicatorPanel =
        // name, 2` defines a reusable panel that dialogs reference with
        // `panel = name`, whether or not a dialog happens to be open.
        if matches!(
            kind,
            RowKind::IndicatorPanel | RowKind::ReadoutPanel | RowKind::LiveGraph
        ) {
            let name = fields
                .first()
                .map_or_else(|| keyword.clone(), |f| (*f).to_owned());
            let container = Dialog {
                name: name.clone(),
                title: fields
                    .get(1)
                    .map(|field| crate::fields::unquote(field).to_owned()),
                layout: Some(keyword.to_ascii_lowercase()),
                rows: Vec::new(),
                line: *line,
            };
            current = Some(name.clone());
            definition.dialogs.insert(name, container);
        }

        let Some(name) = current.as_deref() else {
            section.warn(
                "orphan-dialog-row",
                format!(
                    "`{}` appears before any `dialog =` entry",
                    text.trim_ascii()
                ),
                *line,
            );
            continue;
        };

        if kind == RowKind::Unmodeled {
            // Kept as data; reported once so coverage stays honest.
            section.unrecognized_key(&keyword, *line);
        }
        if let Some(dialog) = definition.dialogs.get_mut(name) {
            dialog.rows.push(DialogRow {
                kind,
                keyword,
                fields: fields.iter().map(|field| (*field).to_owned()).collect(),
                line: *line,
            });
        }
        section.understand(*line);
    }
}

/// `[Menu]`.
pub(crate) fn parse_menu(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut dialog: Option<String> = None;
    let mut index: Option<usize> = None;
    // Index in the current menu's `items` of the `groupMenu` that later
    // `groupChildMenu` entries belong to. A `subMenu` or a separator closes it.
    let mut group: Option<usize> = None;

    for (line, text) in lines {
        section.count_entry();
        let Some((key, value)) = crate::fields::split_assignment(text) else {
            // `std_separator` is a bare keyword.
            if text.trim_ascii().eq_ignore_ascii_case("std_separator")
                && let Some(menu) = index.and_then(|index| definition.menus.get_mut(index))
            {
                menu.items.push(MenuItem {
                    kind: MenuTargetKind::Separator,
                    target: String::new(),
                    label: String::new(),
                    conditions: Vec::new(),
                    group: None,
                    line: *line,
                });
                group = None;
                section.understand(*line);
                continue;
            }
            section.unrecognized_line(*line, text);
            continue;
        };

        let lowered = key.to_ascii_lowercase();
        let fields = split_fields(value);

        match lowered.as_str() {
            "menudialog" => {
                dialog = fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned());
                section.understand(*line);
            }
            "menu" => {
                index = Some(open_menu(definition, &fields, dialog.as_deref()));
                group = None;
                section.understand(*line);
            }
            "submenu" | "groupmenu" | "groupchildmenu" => {
                let (target, label, conditions) = parse_menu_item(&fields);
                // Speeduino writes a toolbar/menu index after the label
                // (`subMenu = target, "Label", 9, { gate }`). It is metadata,
                // not a gate slot; a bare `0` is the empty enable placeholder
                // rusEFI uses and stays for the gate walker.
                let conditions = conditions
                    .into_iter()
                    .filter(|condition| !is_non_zero_number(condition))
                    .collect();
                let kind = match lowered.as_str() {
                    "groupmenu" => MenuTargetKind::Group,
                    _ if target.starts_with("std_") => MenuTargetKind::Standard,
                    _ => MenuTargetKind::DefinitionTarget,
                };
                let owner = if lowered == "groupchildmenu" {
                    group
                } else {
                    None
                };
                let Some(menu) = index.and_then(|index| definition.menus.get_mut(index)) else {
                    section.warn(
                        "orphan-menu-item",
                        format!("`{key}` appears before any `menu =` entry"),
                        *line,
                    );
                    continue;
                };
                let position = menu.items.len();
                menu.items.push(MenuItem {
                    kind,
                    target,
                    label,
                    conditions,
                    group: owner,
                    line: *line,
                });
                // A group header opens a group and a child keeps it open; a
                // plain subMenu ends it.
                group = match lowered.as_str() {
                    "groupmenu" => Some(position),
                    "groupchildmenu" => group,
                    _ => None,
                };
                section.understand(*line);
            }
            "removesubmenu" | "updatesubmenulabel" | "replacesubmenu" => {
                // Menu overrides (iniSpec 3.78) are recognised and preserved;
                // applying them belongs to the UI layer.
                definition
                    .comms
                    .raw
                    .insert(format!("menu.{key}"), value.to_owned());
                section.understand(*line);
            }
            _ => section.unrecognized_key(key, *line),
        }
    }
}

/// Selects the menu with this title, creating it when it is new.
fn open_menu(definition: &mut Definition, fields: &[&str], dialog: Option<&str>) -> usize {
    let title = fields
        .first()
        .map(|field| crate::fields::unquote(field).to_owned())
        .unwrap_or_default();

    let conditions = fields
        .get(1..)
        .unwrap_or_default()
        .iter()
        .map(|field| (*field).to_owned())
        .collect();

    if let Some(position) = definition
        .menus
        .iter()
        .position(|menu| menu.title.eq_ignore_ascii_case(&title))
    {
        let menu = &mut definition.menus[position];

        if menu.conditions.is_empty() {
            menu.conditions = conditions;
        }

        return position;
    }

    definition.menus.push(Menu {
        title,
        conditions,
        items: Vec::new(),
        dialog: dialog.map(str::to_owned),
    });

    definition.menus.len() - 1
}

/// `subMenu = target, "Label"[, cond…]`, tolerating the space-separated form
/// some generators emit (`subMenu = target "Label"[, cond…]`).
///
/// The space-separated form is detected by the quote in the first field, not by
/// the field count: with a trailing condition it splits into two fields, and
/// reading those as target and label would turn the condition into the label.
fn is_non_zero_number(text: &str) -> bool {
    text.trim_ascii()
        .parse::<f64>()
        .is_ok_and(|value| value != 0.0)
}

fn parse_menu_item(fields: &[&str]) -> (String, String, Vec<String>) {
    if let Some(first) = fields.first()
        && let Some(start) = first.find('"')
    {
        let target = first[..start].trim_ascii().to_owned();
        let label = crate::fields::unquote(first[start..].trim_ascii()).to_owned();
        return (
            target,
            label,
            fields[1..]
                .iter()
                .map(|field| (*field).to_owned())
                .collect(),
        );
    }

    if fields.len() >= 2 {
        return (
            fields[0].to_owned(),
            crate::fields::unquote(fields[1]).to_owned(),
            fields[2..]
                .iter()
                .map(|field| (*field).to_owned())
                .collect(),
        );
    }

    (
        fields
            .first()
            .map_or_else(String::new, |field| (*field).to_owned()),
        String::new(),
        Vec::new(),
    )
}

/// `[SettingContextHelp]`: per-constant help text.
pub(crate) fn parse_context_help(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        definition
            .context_help
            .insert(key, crate::fields::unquote(value).to_owned());
        section.understand(*line);
    }
}

/// `[KeyActions]`: `showPanel = keys, dialog`.
pub(crate) fn parse_key_actions(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        if !key.eq_ignore_ascii_case("showPanel") {
            section.unrecognized_key(key, *line);
            continue;
        }
        let fields = split_fields(value);
        let Some(dialog) = fields.last().copied() else {
            section.warn("bad-key-action", "`showPanel` needs a dialog name", *line);
            continue;
        };
        // `showPanel = C, T, dialogName`: every field but the last is a key.
        let keys: Vec<&str> = fields[..fields.len() - 1]
            .iter()
            .map(|field| crate::fields::unquote(field))
            .collect();
        definition.key_actions.push(KeyAction {
            keys: keys.join(","),
            dialog: crate::fields::unquote(dialog).to_owned(),
            line: *line,
        });
        section.understand(*line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Severity;
    use crate::settings::SettingsContext;

    fn parse(text: &str) -> Definition {
        crate::parse_str(text, &SettingsContext::new()).expect("parses")
    }

    #[test]
    fn a_setting_group_keeps_its_options_in_order() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[SettingGroups]\nsettingGroup = mcu, \"Controller\"\nsettingOption = DEFAULT, \"Mega\"\nsettingOption = mcu_stm32, \"STM32\"\n",
        );

        let group = definition.setting_groups.get("mcu").expect("group");
        assert_eq!(group.label, "Controller");
        assert_eq!(group.options.len(), 2);
        assert_eq!(group.options[1].name, "mcu_stm32");
    }

    #[test]
    fn a_repeated_setting_group_header_merges_its_options() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"

[SettingGroups]
settingGroup = mcu, \"First\"
settingOption = first, \"First\"
settingGroup = mcu, \"Second\"
settingOption = second, \"Second\"
",
        );

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "merged-setting-group")
            .expect("the repeated header is reported");
        assert_eq!(diagnostic.line, Some(7));
        assert_eq!(diagnostic.severity, Severity::Note);

        let group = definition.setting_groups.get("mcu").expect("group");
        assert_eq!(group.label, "Second");
        assert_eq!(group.options.len(), 2);
        assert_eq!(group.options[0].name, "first");
        assert_eq!(group.options[1].name, "second");
    }

    #[test]
    fn dialog_rows_keep_their_keyword_and_order() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[UserDefined]\ndialog = veTableDialog, \"VE Table\", border\npanel = veTable1Tbl, Center\nfield = \"RPM\", rpm\n",
        );

        let dialog = definition.dialogs.get("veTableDialog").expect("dialog");
        assert_eq!(dialog.title.as_deref(), Some("VE Table"));
        assert_eq!(dialog.layout.as_deref(), Some("border"));
        assert_eq!(dialog.rows.len(), 2);
        assert_eq!(dialog.rows[0].kind, RowKind::EmbeddedPanel);
        assert_eq!(dialog.rows[1].kind, RowKind::Field);
    }

    #[test]
    fn an_unknown_row_kind_is_kept_as_data() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[UserDefined]\ndialog = d, \"D\"\nbrandNewRow = a, b\n",
        );

        let dialog = definition.dialogs.get("d").expect("dialog");
        assert_eq!(dialog.rows[0].kind, RowKind::Unmodeled);
        assert_eq!(dialog.rows[0].keyword, "brandNewRow");
        assert_eq!(dialog.rows[0].fields, vec!["a", "b"]);
    }

    #[test]
    fn menu_items_attach_to_the_current_menu_and_mark_standard_targets() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[Menu]\nmenuDialog = main\nmenu = \"Fuel\"\nsubMenu = veTable1Tbl, \"VE Table\"\nsubMenu = std_realtime, \"Realtime\"\nstd_separator\n",
        );

        let menu = definition.menus.first().expect("menu");
        assert_eq!(menu.title, "Fuel");
        assert_eq!(menu.items.len(), 3);
        assert_eq!(menu.items[0].kind, MenuTargetKind::DefinitionTarget);
        assert_eq!(menu.items[1].kind, MenuTargetKind::Standard);
        assert_eq!(menu.items[2].kind, MenuTargetKind::Separator);
    }

    #[test]
    fn menu_override_keys_survive_as_raw_comms_entries() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[Menu]\nremoveSubMenu = \"Old\"\nupdateSubMenuLabel = \"Old\", \"New\"\nreplaceSubMenu = \"Old\", veTable1Tbl\n",
        );

        assert_eq!(
            definition.comms.raw.get("menu.removeSubMenu"),
            Some(&"\"Old\"".to_owned()),
            "the override keeps its declared spelling and value"
        );
        assert_eq!(
            definition.comms.raw.get("menu.updateSubMenuLabel"),
            Some(&"\"Old\", \"New\"".to_owned())
        );
        assert_eq!(
            definition.comms.raw.get("menu.replaceSubMenu"),
            Some(&"\"Old\", veTable1Tbl".to_owned())
        );
    }

    #[test]
    fn a_group_menu_owns_the_group_child_entries_that_follow_it() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[Menu]\nmenu = \"Fuel\"\nsubMenu = before, \"Before\"\ngroupMenu = \"Protection\"\ngroupChildMenu = cut, \"Cut\"\ngroupChildMenu = limiter, \"Limiter\"\nsubMenu = after, \"After\"\n",
        );

        let menu = definition.menus.first().expect("menu");
        assert_eq!(menu.items[1].kind, MenuTargetKind::Group);
        assert_eq!(menu.items[1].label, "Protection");
        assert_eq!(menu.items[2].group, Some(1));
        assert_eq!(menu.items[3].group, Some(1));
        assert_eq!(
            menu.items[4].group, None,
            "a plain subMenu closes the group"
        );
        assert_eq!(menu.items[4].kind, MenuTargetKind::DefinitionTarget);
    }

    #[test]
    fn a_menu_index_after_the_label_is_not_a_gate_slot() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[Menu]\nmenu = \"Fuel\"\nsubMenu = trim, \"Trim\", 9, { nFuelChannels >= 5 }\nsubMenu = lambda, \"Lambda\", 0, { enabled }\n",
        );

        let menu = definition.menus.first().expect("menu");
        assert_eq!(
            menu.items[0].conditions,
            vec!["{ nFuelChannels >= 5 }"],
            "a non-zero menu index is metadata"
        );
        assert_eq!(
            menu.items[1].conditions,
            vec!["0", "{ enabled }"],
            "a bare zero keeps the empty enable slot"
        );
    }

    #[test]
    fn a_space_separated_menu_item_still_yields_a_target_and_label() {
        let (target, label, conditions) = parse_menu_item(&["veTable1Tbl \"VE Table\""]);

        assert_eq!(target, "veTable1Tbl");
        assert_eq!(label, "VE Table");
        assert!(
            conditions.is_empty(),
            "unexpected conditions: {conditions:?}"
        );
    }

    #[test]
    fn a_space_separated_menu_item_keeps_its_trailing_condition() {
        // The real Speeduino INI writes this form; splitting on the comma must
        // not turn `{ useDwellMap }` into the label.
        let fields = ["dwell_tblMap \"Dwell Map\"", "{ useDwellMap }"];
        let (target, label, conditions) = parse_menu_item(&fields);

        assert_eq!(target, "dwell_tblMap");
        assert_eq!(label, "Dwell Map");
        assert_eq!(conditions, vec!["{ useDwellMap }"]);
    }
}
