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
        let fields = split_fields(&value);
        let lowered = key.to_ascii_lowercase();

        match lowered.as_str() {
            "settinggroup" => {
                let Some(name) = fields.first() else {
                    section.warn("bad-setting-group", "`settingGroup =` needs a name", *line);
                    continue;
                };
                let label = fields
                    .get(1)
                    .map(|field| crate::fields::unquote(field).to_owned())
                    .unwrap_or_else(|| name.clone());
                definition.setting_groups.insert(
                    name,
                    SettingGroup {
                        name: name.clone(),
                        label,
                        options: Vec::new(),
                    },
                );
                current = Some(name.clone());
                section.understand(*line);
            }
            "settingoption" => {
                let Some(name) = fields.first() else {
                    section.warn(
                        "bad-setting-option",
                        "`settingOption =` needs a name",
                        *line,
                    );
                    continue;
                };
                let label = fields
                    .get(1)
                    .map(|field| crate::fields::unquote(field).to_owned())
                    .unwrap_or_else(|| name.clone());
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
                        name: name.clone(),
                        label,
                    });
                    section.understand(*line);
                }
            }
            _ => section.unrecognized_key(&key, *line),
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
        let is_dialog = assignment
            .as_ref()
            .is_some_and(|(key, _)| key.eq_ignore_ascii_case("dialog"));

        if is_dialog {
            let (_, value) = assignment.unwrap_or_default();
            let fields = split_fields(value);

            let Some(name) = fields.first() else {
                section.warn("bad-dialog", "`dialog =` needs a name", *line);
                continue;
            };
            let dialog = Dialog {
                name: name.clone(),
                title: fields
                    .get(1)
                    .map(|field| crate::fields::unquote(field).to_owned()),
                layout: fields.get(2).map(|field| field.to_ascii_lowercase()),
                rows: Vec::new(),
                line: *line,
            };
            current = Some(name.clone());
            if definition.dialogs.insert(name, dialog).is_some() {
                section.error(
                    "duplicate-name",
                    format!("dialog `{name}` is declared more than once; the later wins"),
                    *line,
                );
            }
            section.understand(*line);
            continue;
        }

        let (keyword, fields) = match assignment {
            Some((key, value)) => (key.trim().to_owned(), split_fields(value)),
            None => (text.trim().to_owned(), Vec::new()),
        };
        let kind = RowKind::parse(&keyword);

        // A container keyword always starts its own container: `indicatorPanel =
        // name, 2` defines a reusable panel that dialogs reference with
        // `panel = name`, whether or not a dialog happens to be open.
        if matches!(
            kind,
            RowKind::IndicatorPanel | RowKind::ReadoutPanel | RowKind::LiveGraph
        ) {
            let name = fields.first().cloned().unwrap_or_else(|| keyword.clone());
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
                format!("`{}` appears before any `dialog =` entry", text.trim()),
                *line,
            );
            continue;
        };

        if kind == RowKind::Unknown {
            // Kept as data; reported once so coverage stays honest.
            section.unrecognized_key(&keyword, *line);
        }
        if let Some(dialog) = definition.dialogs.get_mut(name) {
            dialog.rows.push(DialogRow {
                kind,
                keyword,
                fields,
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

    for (line, text) in lines {
        section.count_entry();
        let Some((key, value)) = crate::fields::split_assignment(text) else {
            // `std_separator` is a bare keyword.
            if text.trim().eq_ignore_ascii_case("std_separator")
                && let Some(menu) = index.and_then(|index| definition.menus.get_mut(index))
            {
                menu.items.push(MenuItem {
                    kind: MenuTargetKind::Separator,
                    target: String::new(),
                    label: String::new(),
                    conditions: Vec::new(),
                    line: *line,
                });
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
                let title = fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned())
                    .unwrap_or_default();
                let existing = definition
                    .menus
                    .iter()
                    .position(|menu| menu.title.eq_ignore_ascii_case(&title));
                index = Some(match existing {
                    Some(position) => position,
                    None => {
                        definition.menus.push(Menu {
                            title,
                            items: Vec::new(),
                            dialog: dialog.clone(),
                        });
                        definition.menus.len() - 1
                    }
                });
                section.understand(*line);
            }
            "submenu" | "groupmenu" | "groupchildmenu" => {
                let (target, label, conditions) = parse_menu_item(&fields);
                let kind = match lowered.as_str() {
                    "groupmenu" => MenuTargetKind::SubMenu,
                    "groupchildmenu" => MenuTargetKind::SubMenu,
                    _ if target.starts_with("std_") => MenuTargetKind::Standard,
                    _ => MenuTargetKind::Panel,
                };
                let Some(menu) = index.and_then(|index| definition.menus.get_mut(index)) else {
                    section.warn(
                        "orphan-menu-item",
                        format!("`{key}` appears before any `menu =` entry"),
                        *line,
                    );
                    continue;
                };
                menu.items.push(MenuItem {
                    kind,
                    target,
                    label,
                    conditions,
                    line: *line,
                });
                section.understand(*line);
            }
            "removeSubMenu" | "updateSubMenuLabel" | "replaceSubMenu" => {
                // Menu overrides (iniSpec 3.78) are recognised and preserved;
                // applying them belongs to the UI layer.
                definition
                    .protocol
                    .raw
                    .insert(format!("menu.{key}"), value.to_owned());
                section.understand(*line);
            }
            _ => section.unrecognized_key(key, *line),
        }
    }
}

/// `subMenu = target, "Label"[, cond…]`, tolerating the space-separated form
/// some generators emit (`subMenu = target "Label"`).
fn parse_menu_item(fields: &[String]) -> (String, String, Vec<String>) {
    if fields.len() >= 2 {
        return (
            fields[0].clone(),
            crate::fields::unquote(&fields[1]).to_owned(),
            fields[2..].to_vec(),
        );
    }
    let joined = fields.first().cloned().unwrap_or_default();
    match joined.find('"') {
        Some(start) => {
            let target = joined[..start].trim().to_owned();
            let label = joined[start..].trim();
            (target, crate::fields::unquote(label).to_owned(), Vec::new())
        }
        None => (joined, String::new(), Vec::new()),
    }
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
            .insert(&key, crate::fields::unquote(&value).to_owned());
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
            section.unrecognized_key(&key, *line);
            continue;
        }
        let fields = split_fields(&value);
        let Some(dialog) = fields.last() else {
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
    fn dialog_rows_keep_their_keyword_and_order() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[UserDefined]\ndialog = veTableDialog, \"VE Table\", border\npanel = veTable1Tbl, Center\nfield = \"RPM\", rpm\n",
        );

        let dialog = definition.dialogs.get("veTableDialog").expect("dialog");
        assert_eq!(dialog.title.as_deref(), Some("VE Table"));
        assert_eq!(dialog.layout.as_deref(), Some("border"));
        assert_eq!(dialog.rows.len(), 2);
        assert_eq!(dialog.rows[0].kind, RowKind::Panel);
        assert_eq!(dialog.rows[1].kind, RowKind::Field);
    }

    #[test]
    fn an_unknown_row_kind_is_kept_as_data() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[UserDefined]\ndialog = d, \"D\"\nbrandNewRow = a, b\n",
        );

        let dialog = definition.dialogs.get("d").expect("dialog");
        assert_eq!(dialog.rows[0].kind, RowKind::Unknown);
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
        assert_eq!(menu.items[0].kind, MenuTargetKind::Panel);
        assert_eq!(menu.items[1].kind, MenuTargetKind::Standard);
        assert_eq!(menu.items[2].kind, MenuTargetKind::Separator);
    }

    #[test]
    fn a_space_separated_menu_item_still_yields_a_target_and_label() {
        let (target, label, _) = parse_menu_item(&["veTable1Tbl \"VE Table\"".to_owned()]);

        assert_eq!(target, "veTable1Tbl");
        assert_eq!(label, "VE Table");
    }
}
