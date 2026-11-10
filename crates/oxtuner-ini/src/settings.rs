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
    #[must_use]
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (impl Into<String>, bool)>,
    ) -> Self {
        self.overrides = overrides
            .into_iter()
            .map(|(name, value)| (name.into().to_ascii_lowercase(), value))
            .collect();
        self
    }

    /// Records an explicit choice.
    pub fn set_override(&mut self, name: impl AsRef<str>, active: bool) {
        self.overrides
            .insert(name.as_ref().to_ascii_lowercase(), active);
    }

    /// Records what the INI declared. Later declarations win, matching the
    /// "last directive read" behaviour of the dialect.
    pub(crate) fn declare(&mut self, name: &str, active: bool) {
        self.declared
            .insert(name.trim_ascii().to_ascii_lowercase(), active);
    }

    /// Records that the file asked whether a flag was set.
    pub(crate) fn note_tested(&mut self, name: &str) {
        self.tested.insert(name.trim_ascii().to_ascii_lowercase());
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

    /// Every flag that is active in this parse.
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
            .cloned()
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
    fn tested_flags_are_remembered_for_the_settings_ui() {
        let mut context = SettingsContext::new();
        context.note_tested("CELSIUS");
        context.note_tested("celsius");

        assert_eq!(context.tested_flags().len(), 1);
        assert!(context.tested_flags().contains("celsius"));
    }
}
