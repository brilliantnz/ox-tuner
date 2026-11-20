//! Reading a definition before a project exists.
//!
//! The new-project wizard needs the signature and the setting groups of the
//! file the user picked, but it must not create anything or replace the open
//! session. Parsing therefore stays here, next to creation, so the UI never
//! touches the INI dialect: it receives the few fields the dialog renders and
//! sends the chosen option names back with the create request.

use std::cell::RefCell;
use std::path::Path;

use oxtuner_ini::SettingsContext;

use crate::project::{ProjectError, accepts_definition};

/// What the wizard needs to know about a definition before it creates a
/// project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionPreview {
    /// Firmware signature, trimmed.
    pub signature: String,
    /// Flags the definition asks about in an `#if`, sorted.
    ///
    /// The wizard uses the ones that name a display choice (`CELSIUS`,
    /// `LAMBDA`) to offer the groups `TunerStudio` keeps in its own settings;
    /// a definition that never tests them gets no such choice.
    pub tested_flags: Vec<String>,
    /// The setting groups the definition declares, in declaration order.
    pub groups: Vec<SettingGroupPreview>,
}

/// One `[SettingGroups]` entry as the wizard shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingGroupPreview {
    /// Group id the `.msq` snapshot records.
    pub name: String,
    /// Display name.
    pub label: String,
    /// Options in declaration order.
    ///
    /// Empty for a boolean group: the group's own name is the flag, and the
    /// dialog offers deactivated or activated.
    pub options: Vec<SettingOptionPreview>,
    /// The option selected when the user makes no choice.
    ///
    /// `DEFAULT` when the definition names one, otherwise the first option.
    /// `None` for a boolean group, whose default is [`Self::default_active`].
    pub default: Option<String>,
    /// Whether a boolean group's flag is active by default.
    pub default_active: bool,
}

/// One option of a [`SettingGroupPreview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingOptionPreview {
    /// Flag name the definition's `#if` tests, and what creation stores.
    pub name: String,
    /// Display name.
    pub label: String,
}

/// Parses a definition and returns what the wizard renders.
///
/// Nothing is written and no session is touched. The definition's own defaults
/// decide the preselected option, so the dialog opens on the choice the
/// firmware would use without one.
///
/// # Errors
///
/// Returns [`ProjectError::DefinitionNotIni`] when the path is not a `.ini`
/// file, [`ProjectError::Io`] when it cannot be read, [`ProjectError::Ini`]
/// when it does not parse, and [`ProjectError::DefinitionNeedsIncludes`] when it
/// pulls in `#include` files, because creation copies one file and would build
/// a project that opens with pages missing.
pub fn inspect_definition(path: &Path) -> Result<DefinitionPreview, ProjectError> {
    if !accepts_definition(path) {
        return Err(ProjectError::DefinitionNotIni {
            path: path.to_owned(),
        });
    }

    let bytes = std::fs::read(path).map_err(|source| ProjectError::Io {
        path: path.to_owned(),
        source,
    })?;
    let text = oxtuner_ini::source::decode(&bytes).text;

    // One-shot inspection: the included content is not needed to show the
    // signature and the setting groups, and creation refuses includes anyway.
    let includes = RefCell::new(Vec::new());
    let resolver = |request: oxtuner_ini::IncludeRequest<'_>| {
        includes.borrow_mut().push(request.path.to_owned());
        None
    };
    let definition =
        oxtuner_ini::parse_str_with_includes(&text, &SettingsContext::new(), &resolver)
            .map_err(ProjectError::Ini)?;

    let includes = includes.into_inner();
    if !includes.is_empty() {
        return Err(ProjectError::DefinitionNeedsIncludes { names: includes });
    }

    Ok(DefinitionPreview::of(&definition))
}

impl DefinitionPreview {
    fn of(definition: &oxtuner_ini::Definition) -> Self {
        let groups = definition
            .setting_groups
            .values()
            .map(|group| {
                let options: Vec<SettingOptionPreview> = group
                    .options
                    .iter()
                    .map(|option| SettingOptionPreview {
                        name: option.name.clone(),
                        label: option.label.clone(),
                    })
                    .collect();

                // `DEFAULT` is the option that writes no flag; a group without
                // one starts on its first option, the way the reference dialog
                // shows it.
                let default = group
                    .options
                    .iter()
                    .find(|option| option.name.eq_ignore_ascii_case("DEFAULT"))
                    .or_else(|| group.options.first())
                    .map(|option| option.name.clone());

                let default_active = group.options.is_empty()
                    && definition
                        .active_flags
                        .iter()
                        .any(|flag| flag.eq_ignore_ascii_case(&group.name));

                SettingGroupPreview {
                    name: group.name.clone(),
                    label: group.label.clone(),
                    options,
                    default,
                    default_active,
                }
            })
            .collect();

        Self {
            signature: definition.signature.trim().to_owned(),
            tested_flags: definition.tested_flags.iter().cloned().collect(),
            groups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("oxtuner-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("definition");
        path
    }

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[SettingGroups]
settingGroup = pressure_units, "Pressure Display"
settingOption = DEFAULT, "PSI"
settingOption = pressure_bar, "BAR"
settingGroup = resetcontrol_group, "Reset Control Features"
settingOption = resetcontrol_standard, "Basic Options Only"
settingOption = resetcontrol_adv, "Advanced"
settingGroup = enablehardware_test, "Enable Hardware Test Page"

[Constants]
nPages = 1
pageSize = 64
page = 1

#if CELSIUS
celsius_only = scalar, U08, 8, "", 1, 0, 0, 255, 0
#endif
"#;

    #[test]
    fn the_preview_names_the_groups_and_their_defaults() {
        let path = write("groups.ini", DEFINITION);

        let preview = inspect_definition(&path).expect("the definition is inspectable");

        assert_eq!(preview.signature, "speeduino 202501");
        assert_eq!(preview.groups.len(), 3);
        assert!(
            preview.tested_flags.iter().any(|flag| flag == "celsius"),
            "the definition tests CELSIUS: {:?}",
            preview.tested_flags
        );

        let pressure = &preview.groups[0];
        assert_eq!(pressure.name, "pressure_units");
        assert_eq!(pressure.label, "Pressure Display");
        assert_eq!(pressure.default.as_deref(), Some("DEFAULT"));
        assert_eq!(pressure.options.len(), 2);

        let reset = &preview.groups[1];
        assert_eq!(
            reset.default.as_deref(),
            Some("resetcontrol_standard"),
            "a group without DEFAULT starts on its first option"
        );

        let hardware = &preview.groups[2];
        assert!(
            hardware.options.is_empty(),
            "a boolean group has no options"
        );
        assert!(!hardware.default_active);
    }

    #[test]
    fn a_boolean_group_that_the_definition_sets_starts_active() {
        let path = write(
            "active.ini",
            &DEFINITION.replace("[Constants]", "#set enablehardware_test\n\n[Constants]"),
        );

        let preview = inspect_definition(&path).expect("the definition is inspectable");

        assert!(preview.groups[2].default_active);
    }

    #[test]
    fn a_file_that_is_not_a_definition_is_refused() {
        let path = write("note.txt", "[MegaTune]\nsignature = \"x\"\n");

        let error = inspect_definition(&path).expect_err("the extension is refused");

        assert!(matches!(error, ProjectError::DefinitionNotIni { .. }));
    }

    #[test]
    fn a_definition_with_includes_is_refused_like_creation_refuses_it() {
        let path = write(
            "includes.ini",
            &format!("{DEFINITION}\n#include \"extra.ini\"\n"),
        );

        let error = inspect_definition(&path).expect_err("includes are refused");

        assert!(
            matches!(error, ProjectError::DefinitionNeedsIncludes { .. }),
            "{error:?}"
        );
    }
}
