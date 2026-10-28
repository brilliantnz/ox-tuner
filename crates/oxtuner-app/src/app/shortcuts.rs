//! Keyboard shortcuts: one table that both binds the keys and documents them.
//!
//! The bindings are **fixed**. ox-tuner has no shortcut editor and does not
//! persist overrides: a shortcut table that nobody can change needs no
//! override merging, no schema field and no "reset to defaults" state. If
//! reassignment is ever wanted, it becomes a stored override map layered on
//! this same table.
//!
//! GPUI cannot remove a single binding from its keymap, only clear the whole
//! thing — and that would drop gpui-kit's own bindings (text fields, dialogs,
//! lists), which cannot be registered again. So [`keep_kit_bindings`] takes a
//! copy of gpui-kit's bindings first, and [`apply_shortcuts`] rebuilds the
//! keymap from that copy plus [`SHORTCUTS`].
//!
//! The table is also the Settings › Shortcuts page's source of truth, so the
//! keys a user reads there cannot drift from the keys the app binds.

use std::rc::Rc;

use gpui_kit::component::kbd::Kbd;
use gpui_kit::{
    Action, App, Global, KeyBinding, KeyBindingContextPredicate, Keystroke, Modifiers,
    OperatingSystem, SharedString,
};

use super::actions::{
    CloseProject, ConnectEcu, DisconnectEcu, ManageProjects, OpenProject, Quit, SaveTune, Settings,
    ShowCharts, ShowTune, ToggleBottomPanel, TogglePrimarySidebar, ToggleSecondarySidebar,
};
use crate::i18n::t;

/// A shortcut's category. Kept on each entry for ordering and future filtering;
/// the settings page shows one flat list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutGroup {
    General,
    View,
}

/// One shortcut: what it does, what it is called, and the keys on each
/// platform family.
pub struct Shortcut {
    /// Stable identifier, also the element id used by UI that shows it.
    pub id: &'static str,
    /// Locale key for the command's name.
    pub label: &'static str,
    pub group: ShortcutGroup,
    /// Where it works: anywhere, or in these key contexts.
    contexts: &'static [&'static str],
    /// The binding on macOS.
    pub mac: &'static str,
    /// The binding everywhere else.
    pub other: &'static str,
    action: fn() -> Box<dyn Action>,
}

impl Shortcut {
    /// The binding for this platform. Empty means "not bound".
    pub fn keys(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            self.mac
        } else {
            self.other
        }
    }

    /// The command's name in the interface language.
    pub fn title(&self) -> SharedString {
        t!(self.label)
    }

    /// The action this shortcut dispatches.
    pub fn action(&self) -> Box<dyn Action> {
        (self.action)()
    }
}

macro_rules! shortcut {
    ($id:literal, $label:literal, $group:ident, $contexts:expr, $mac:literal, $other:literal, $action:ident) => {
        Shortcut {
            id: $id,
            label: $label,
            group: ShortcutGroup::$group,
            contexts: $contexts,
            mac: $mac,
            other: $other,
            action: || Box::new($action),
        }
    };
}

/// Every shortcut, in the order Settings › Shortcuts lists them.
///
/// The panel toggles the primary sidebar on the main modifier
/// plus `B`, the bottom panel plus `J`, and the secondary sidebar on the
/// neighbouring `Alt+B` so the three read as a family.
pub static SHORTCUTS: [Shortcut; 13] = [
    shortcut!(
        "open-settings",
        "app.shortcut.open_settings",
        General,
        &[],
        "cmd-,",
        "ctrl-,",
        Settings
    ),
    shortcut!(
        "open-project",
        "app.shortcut.open_project",
        General,
        &[],
        "cmd-o",
        "ctrl-o",
        OpenProject
    ),
    shortcut!(
        "manage-projects",
        "app.shortcut.manage_projects",
        General,
        &[],
        "cmd-shift-o",
        "ctrl-shift-o",
        ManageProjects
    ),
    shortcut!(
        "close-project",
        "app.shortcut.close_project",
        General,
        &[],
        "cmd-shift-w",
        "ctrl-shift-w",
        CloseProject
    ),
    shortcut!(
        "save-tune",
        "app.shortcut.save_tune",
        General,
        &[],
        "cmd-s",
        "ctrl-s",
        SaveTune
    ),
    shortcut!(
        "connect",
        "app.shortcut.connect",
        General,
        &[],
        "cmd-k",
        "ctrl-k",
        ConnectEcu
    ),
    shortcut!(
        "disconnect",
        "app.shortcut.disconnect",
        General,
        &[],
        "cmd-shift-k",
        "ctrl-shift-k",
        DisconnectEcu
    ),
    shortcut!(
        "quit",
        "app.shortcut.quit",
        General,
        &[],
        "cmd-q",
        "ctrl-q",
        Quit
    ),
    shortcut!(
        "show-tune",
        "app.shortcut.show_tune",
        View,
        &[],
        "cmd-1",
        "ctrl-1",
        ShowTune
    ),
    shortcut!(
        "show-charts",
        "app.shortcut.show_charts",
        View,
        &[],
        "cmd-2",
        "ctrl-2",
        ShowCharts
    ),
    shortcut!(
        "toggle-primary-sidebar",
        "app.shortcut.toggle_primary_sidebar",
        View,
        &[],
        "cmd-b",
        "ctrl-b",
        TogglePrimarySidebar
    ),
    shortcut!(
        "toggle-secondary-sidebar",
        "app.shortcut.toggle_secondary_sidebar",
        View,
        &[],
        "cmd-alt-b",
        "ctrl-alt-b",
        ToggleSecondarySidebar
    ),
    shortcut!(
        "toggle-bottom-panel",
        "app.shortcut.toggle_bottom_panel",
        View,
        &[],
        "cmd-j",
        "ctrl-j",
        ToggleBottomPanel
    ),
];

/// gpui-kit's own bindings, kept before the first rebuild.
struct KitBindings(Vec<KeyBinding>);

impl Global for KitBindings {}

/// Whether the keymap already holds exactly [`SHORTCUTS`].
struct AppliedShortcuts;

impl Global for AppliedShortcuts {}

/// Copies gpui-kit's bindings aside. Call once, before anything rebuilds the
/// keymap.
pub(super) fn keep_kit_bindings(cx: &mut App) {
    if cx.try_global::<KitBindings>().is_some() {
        return;
    }
    let bindings: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(KitBindings(bindings));
}

/// Binds gpui-kit's own keys, then [`SHORTCUTS`]. Does nothing when they are
/// already in place.
pub fn apply_shortcuts(cx: &mut App) {
    if cx.try_global::<AppliedShortcuts>().is_some() {
        return;
    }

    let mut bindings = cx
        .try_global::<KitBindings>()
        .map(|kit| kit.0.clone())
        .unwrap_or_default();
    bindings.extend(key_bindings(cx));

    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    cx.set_global(AppliedShortcuts);
}

/// The bindings [`SHORTCUTS`] describes, for this platform.
pub(super) fn key_bindings(cx: &App) -> Vec<KeyBinding> {
    SHORTCUTS
        .iter()
        .filter_map(|shortcut| {
            let keys = shortcut.keys();
            if keys.is_empty() {
                return None;
            }
            // `KeyBinding::new` wants a concrete action type; the table hands
            // out `Box<dyn Action>`, which `load` accepts.
            let predicate = shortcut
                .contexts
                .first()
                .and_then(|context| KeyBindingContextPredicate::parse(context).ok())
                .map(Rc::new);
            KeyBinding::load(keys, shortcut.action(), predicate, false, None, cx).ok()
        })
        .collect()
}

/// The keys of `keys` as separate caps: `["⌘", "B"]` on macOS,
/// `["Ctrl", "B"]` elsewhere. Empty when the shortcut is unbound.
pub fn key_caps(keys: &str) -> Vec<SharedString> {
    if keys.is_empty() {
        return Vec::new();
    }
    let os = if cfg!(target_os = "macos") {
        OperatingSystem::Mac
    } else if cfg!(target_os = "windows") {
        OperatingSystem::Windows
    } else {
        OperatingSystem::Linux
    };
    let Ok(keystroke) = Keystroke::parse_with_os(keys, os) else {
        return Vec::new();
    };
    let modifiers = keystroke.modifiers;
    let mac = cfg!(target_os = "macos");
    let mut caps: Vec<SharedString> = [
        (modifiers.control, "⌃", "Ctrl"),
        (modifiers.alt, "⌥", "Alt"),
        (modifiers.shift, "⇧", "Shift"),
        (modifiers.platform, "⌘", "Win"),
    ]
    .into_iter()
    .filter(|(held, _, _)| *held)
    .map(|(_, symbol, name)| SharedString::from(if mac { symbol } else { name }))
    .collect();
    caps.push(
        Kbd::format(&Keystroke {
            modifiers: Modifiers::default(),
            key: keystroke.key,
            key_char: None,
        })
        .into(),
    );
    caps
}

/// The shortcut with `id`, if the table has one.
pub fn find(id: &str) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
}

/// The keys of `keys` as one label: `⌘B` on macOS, `Ctrl+B` elsewhere.
pub fn key_text(keys: &str) -> String {
    let separator = if cfg!(target_os = "macos") { "" } else { "+" };
    key_caps(keys).join(separator)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_shortcut_is_bound_on_both_platform_families() {
        for shortcut in &SHORTCUTS {
            assert!(
                !shortcut.mac.is_empty(),
                "{} has no macOS binding",
                shortcut.id
            );
            assert!(
                !shortcut.other.is_empty(),
                "{} has no Windows/Linux binding",
                shortcut.id
            );
        }
    }

    /// Two commands on one key means one of them can never fire.
    #[test]
    fn no_two_shortcuts_share_a_binding() {
        for family in [
            SHORTCUTS
                .iter()
                .map(|shortcut| shortcut.mac)
                .collect::<Vec<_>>(),
            SHORTCUTS
                .iter()
                .map(|shortcut| shortcut.other)
                .collect::<Vec<_>>(),
        ] {
            let mut seen = BTreeSet::new();
            for keys in family {
                assert!(seen.insert(keys), "{keys} is bound twice");
            }
        }
    }

    #[test]
    fn shortcut_ids_are_unique() {
        let mut seen = BTreeSet::new();
        for shortcut in &SHORTCUTS {
            assert!(seen.insert(shortcut.id), "{} repeats", shortcut.id);
        }
    }

    /// Every shortcut names a locale key that exists, or the page shows the
    /// raw key instead of a name.
    #[test]
    fn every_shortcut_label_resolves() {
        for shortcut in &SHORTCUTS {
            assert_ne!(
                shortcut.title().as_ref(),
                shortcut.label,
                "{} has no locale text",
                shortcut.id
            );
        }
    }

    #[test]
    fn keys_render_as_caps() {
        // `cmd-b` is one modifier and one key, whatever the platform calls them.
        assert_eq!(key_caps("cmd-b").len(), 2);
        assert_eq!(key_caps(""), Vec::<SharedString>::new());
        assert_eq!(key_text(""), "");
        assert!(!key_text("cmd-b").is_empty());
    }
}
