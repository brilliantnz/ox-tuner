//! The settings context that decides `#if` branches.
//!
//! ADR-0006: settings resolution must run **before** protocol-command
//! derivation, because `COMMS_COMPAT` rewrites `burnCommand` and `mcu_stm32`
//! rewrites `blockingFactor`. The context is therefore an input to
//! preprocessing, not a post-processing step.
//!
//! Precedence, highest first (tested in `settings_resolution_follows_precedence`):
//!
//! 1. **Project snapshot / user override** — the `<settings>` block of the
//!    project's `.msq`, or a choice the user just made. An explicit choice
//!    always wins.
//! 2. **INI declaration** — `#set FLAG` (active) or `#unset FLAG` (inactive).
//! 3. **Unmentioned** — inactive. A flag that only appears in `#if` tests
//!    (`CELSIUS`, `LAMBDA` in the Speeduino INI) is off unless something
//!    selects it.

use std::collections::{BTreeMap, BTreeSet};

/// Which symbols are active for one parse.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsContext {
    /// Explicit choices from the project snapshot or the user.
    overrides: BTreeMap<String, bool>,
    /// Flags the INI declared with `#set` / `#unset`, lowercased.
    declared: BTreeMap<String, bool>,
    /// Every flag the file asked about in an `#if`, lowercased.
    tested: BTreeSet<String>,
    /// The declared spelling of every flag seen, keyed by lowercase name.
    ///
    /// Matching stays case-insensitive, but what the context hands back for
    /// writing keeps the definition's own spelling: a settings snapshot that
    /// reads `CELSIUS` must write `CELSIUS` back, not `celsius`. The first
    /// spelling seen wins.
    spelling: BTreeMap<String, String>,
}

impl SettingsContext {
    /// An empty context: every flag is off unless the INI declares it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets explicit choices (project snapshot or user override).
    ///
    /// Keys are compared case-insensitively, the way the dialect treats
    /// directive names; the caller may pass them in either case.
    ///
    /// The `bool` is deliberate rather than an `Off`/`On` enum: the third
    /// state, "no explicit choice", is the key being absent from the map. A
    /// richer flag type earns its keep only if the settings surface grows a
    /// third explicit choice.
    #[must_use]
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (impl Into<String>, bool)>,
    ) -> Self {
        for (name, value) in overrides {
            let name: String = name.into();
            self.set_override(&name, value);
        }
        self
    }

    /// Records an explicit choice.
    pub fn set_override(&mut self, name: impl AsRef<str>, active: bool) {
        let name = name.as_ref();
        self.spelling
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| name.to_owned());
        self.overrides.insert(name.to_ascii_lowercase(), active);
    }

    /// Records what the INI declared. Later declarations win, matching the
    /// "last directive read" behaviour of the dialect.
    pub(crate) fn declare(&mut self, name: &str, active: bool) {
        let trimmed = name.trim_ascii();
        self.spelling
            .entry(trimmed.to_ascii_lowercase())
            .or_insert_with(|| trimmed.to_owned());
        self.declared.insert(trimmed.to_ascii_lowercase(), active);
    }

    /// Records that the file asked whether a flag was set.
    pub(crate) fn note_tested(&mut self, name: &str) {
        let trimmed = name.trim_ascii();
        self.spelling
            .entry(trimmed.to_ascii_lowercase())
            .or_insert_with(|| trimmed.to_owned());
        self.tested.insert(trimmed.to_ascii_lowercase());
    }

    /// Resolves one flag through the precedence order.
    pub fn is_active(&self, name: &str) -> bool {
        let key = name.trim_ascii().to_ascii_lowercase();
        if let Some(value) = self.overrides.get(&key) {
            return *value;
        }
        self.declared.get(&key).copied().unwrap_or(false)
    }

    /// Every flag the file tested, sorted. The Settings UI offers these as
    /// checkboxes; a file that never tests `CELSIUS` has no unit choice to make.
    pub fn tested_flags(&self) -> &BTreeSet<String> {
        &self.tested
    }

    /// Every flag that is active in this parse, in the definition's spelling.
    ///
    /// Resolution goes through [`Self::is_active`], so an override can veto an
    /// active `#set` declaration — the same precedence the parser applied when
    /// it selected `#if` branches. Reporting anything else would make this set
    /// disagree with the definition it was used to produce.
    pub fn active_flags(&self) -> BTreeSet<String> {
        self.declared
            .keys()
            .chain(self.overrides.keys())
            .filter(|name| self.is_active(name))
            .map(|name| {
                self.spelling
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| name.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_resolution_follows_precedence() {
        let mut context = SettingsContext::new().with_overrides([("comms_compat", true)]);
        context.declare("COMMS_COMPAT", false);
        context.declare("MCU_STM32", true);

        // 1. An explicit choice beats the INI's own declaration.
        assert!(context.is_active("COMMS_COMPAT"));
        assert!(
            context.is_active("comms_compat"),
            "names are case-insensitive"
        );

        // 2. With no override, the declaration decides.
        assert!(context.is_active("mcu_stm32"));

        // 3. A flag nobody mentioned is off.
        assert!(!context.is_active("CELSIUS"));
    }

    #[test]
    fn an_unset_declaration_beats_a_silent_default() {
        let mut context = SettingsContext::new();
        context.declare("enablehardware_test", false);

        assert!(!context.is_active("enablehardware_test"));
        assert!(!context.active_flags().contains("enablehardware_test"));
    }

    #[test]
    fn an_override_can_veto_an_active_declaration() {
        let mut context = SettingsContext::new();
        context.declare("celsius", true);
        context.set_override("CELSIUS", false);

        assert!(!context.is_active("celsius"));
        assert!(
            !context.active_flags().contains("celsius"),
            "active_flags must follow the same precedence as is_active"
        );

        // The reverse direction too: an override can activate what the INI unset.
        context.declare("lambda", false);
        context.set_override("lambda", true);
        assert!(context.active_flags().contains("lambda"));
    }

    #[test]
    fn active_flags_keep_the_definition_spelling() {
        let mut context = SettingsContext::new();
        context.declare("CELSIUS", true);
        context.set_override("pressure_bar", true);

        assert!(context.active_flags().contains("CELSIUS"));
        assert!(context.active_flags().contains("pressure_bar"));
        assert!(!context.active_flags().contains("celsius"));
    }

    #[test]
    fn tested_flags_are_remembered_for_the_settings_ui() {
        let mut context = SettingsContext::new();
        context.note_tested("CELSIUS");
        context.note_tested("celsius");

        assert_eq!(context.tested_flags().len(), 1);
        assert!(context.tested_flags().contains("celsius"));
    }
}
