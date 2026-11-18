//! The wizard's inputs, shared by every step.
//!
//! The steps render from this state and the footer mutates it; the bodies own
//! only presentation.
//!
//! The model is deliberately a shared [`RefCell`] rather than an entity: the
//! dialog owns it for one open, every step borrows it briefly, and nothing
//! outlives the close. Keep each borrow to one statement; a nested borrow is
//! a panic, not a compile error.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::select::SelectState;
use gpui_kit::{App, Entity, Subscription};
use oxtuner_core::DEFAULT_BAUD_RATE;

use crate::platform::paths::AppPaths;

/// The name a fresh project starts with.
pub(super) const DEFAULT_NAME: &str = "MyProject";

/// Where the wizard is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Name,
    Definition,
    Configuration,
    Connection,
    Review,
}

impl Step {
    /// The step before this one, when there is one.
    pub(super) fn previous(self) -> Option<Self> {
        match self {
            Self::Name => None,
            Self::Definition => Some(Self::Name),
            Self::Configuration => Some(Self::Definition),
            Self::Connection => Some(Self::Configuration),
            Self::Review => Some(Self::Connection),
        }
    }

    /// The step after this one. Review is the last step.
    pub(super) fn next(self) -> Option<Self> {
        match self {
            Self::Name => Some(Self::Definition),
            Self::Definition => Some(Self::Configuration),
            Self::Configuration => Some(Self::Connection),
            Self::Connection => Some(Self::Review),
            Self::Review => None,
        }
    }
}

/// Where the definition comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DefinitionMode {
    /// The scan probes the ECU and the registry supplies the file.
    #[default]
    Auto,
    /// The user uploads a file by hand.
    Manual,
}

/// The definition the step offers for `mode`.
///
/// Auto reads the detection's answer; Manual reads the picked file. Each mode
/// owns its source, so toggling never leaks the other side's state into the
/// step, the review, or Create.
pub(super) fn effective_definition(
    mode: DefinitionMode,
    picked: Option<PathBuf>,
    detected: Option<PathBuf>,
) -> Option<PathBuf> {
    match mode {
        DefinitionMode::Auto => detected,
        DefinitionMode::Manual => picked,
    }
}

/// One setting group's picker.
///
/// `values` maps a `Select` row to the option name creation stores; `None` is
/// a boolean group left off.
pub(super) struct GroupSelect {
    pub(super) name: String,
    pub(super) values: Vec<Option<String>>,
    pub(super) state: Entity<SelectState<Vec<String>>>,
}

/// The wizard's inputs, shared by the render pass and every button handler.
pub(super) struct Wizard {
    pub(super) step: Step,
    /// Whether the definition is detected or uploaded.
    pub(super) definition_mode: DefinitionMode,
    /// The folder projects are created inside.
    pub(super) parent: PathBuf,
    pub(super) definition: Option<PathBuf>,
    /// Why the last picked file was refused, if it was not a definition.
    pub(super) definition_error: Option<String>,
    /// Why the name or the target was refused, when it was.
    pub(super) name_error: Option<String>,
    /// One picker per displayed group, built when a preview arrives.
    pub(super) group_selects: Vec<GroupSelect>,
    /// The definition whose preview filled the group pickers.
    pub(super) preview_path: Option<PathBuf>,
    /// The groups the dialog shows: the definition's own, plus the display
    /// choices the application owns but the project records.
    pub(super) display_groups: Vec<oxtuner_core::SettingGroupView>,
    /// The folder a create is building, once Create was pressed.
    pub(super) target: Option<PathBuf>,
    /// The failure serial seen when Create was pressed, so a failure already on
    /// screen is never mistaken for this create's answer.
    pub(super) failure_serial: u64,
    /// The port names the connection picker was built from.
    pub(super) connection_ports: Vec<String>,
    /// The connection port picker, built once the ports arrive.
    pub(super) port_select: Option<Entity<SelectState<Vec<String>>>>,
    /// The baud picker; the baud list never changes, so it is built once.
    pub(super) baud_select: Option<Entity<SelectState<Vec<String>>>>,
    /// The connection type picker; its two options never change either.
    pub(super) connection_type_select: Option<Entity<SelectState<Vec<String>>>>,
    /// The port the picker was last synced to as its default.
    ///
    /// A selection that differs from this is the user's own choice and is
    /// never overwritten by a later detection or rescan.
    pub(super) connection_auto_port: Option<String>,
    /// The baud the picker was last synced to as its default.
    pub(super) connection_auto_baud: u32,
    /// The name and path field subscriptions, kept so the dialog can
    /// unsubscribe them on close instead of leaking a handler per open.
    pub(super) name_subscription: Option<Subscription>,
    pub(super) path_subscription: Option<Subscription>,
    /// What the Test button last launched, if anything.
    ///
    /// A test answer is only shown while it still matches this: changing the
    /// pickers afterwards hides the stale result instead of attributing it to
    /// the new selection.
    pub(super) tested: Option<TestedSelection>,
    /// Whether the dialog closed. Async picker callbacks check this before
    /// touching the model: a file picked after close belongs to no dialog.
    pub(super) closed: bool,
}

/// The connection a Test click ran against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TestedSelection {
    pub(super) kind: ConnectionType,
    pub(super) port: Option<String>,
    pub(super) baud: u32,
}

impl Wizard {
    pub(super) fn new(cx: &App) -> Self {
        Self {
            step: Step::Name,
            definition_mode: DefinitionMode::Auto,
            parent: AppPaths::get(cx).projects_dir().to_path_buf(),
            definition: None,
            definition_error: None,
            name_error: None,
            group_selects: Vec::new(),
            preview_path: None,
            display_groups: Vec::new(),
            target: None,
            failure_serial: 0,
            connection_ports: Vec::new(),
            port_select: None,
            baud_select: None,
            connection_type_select: None,
            connection_auto_port: None,
            connection_auto_baud: DEFAULT_BAUD_RATE,
            name_subscription: None,
            path_subscription: None,
            tested: None,
            closed: false,
        }
    }

    /// Starts over on the active definition.
    ///
    /// A new file is a new attempt: nothing from the previous inspect or
    /// create may linger into it. The group pickers go with the groups, so the
    /// next preview cannot be rendered into a picker built for the old file.
    pub(super) fn retry_definition(&mut self) {
        self.definition_error = None;
        self.preview_path = None;
        self.display_groups.clear();
        self.group_selects.clear();
        self.target = None;
    }

    /// Points the wizard at another target folder.
    ///
    /// A new folder is a new attempt: the target goes, and the failure serial
    /// moves to the current one. Resetting it to zero would let a failure that
    /// is still on screen count as the new attempt's answer.
    pub(super) fn retarget(&mut self, failure_serial: u64) {
        self.target = None;
        self.failure_serial = failure_serial;
    }
}

/// How the project talks to the ECU.
///
/// A select today, so tomorrow's transports (`TCP`, `WiFi`, `Bluetooth`) slot
/// in as options without restructuring the step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ConnectionType {
    /// A serial device, USB included.
    #[default]
    Serial,
    /// The built-in simulator.
    Simulator,
}

pub(super) type SharedWizard = Rc<RefCell<Wizard>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_reads_the_detection_and_ignores_the_picked_file() {
        let picked = Some(PathBuf::from("/tmp/manual.ini"));
        let detected = Some(PathBuf::from("/tmp/auto.ini"));

        assert_eq!(
            effective_definition(DefinitionMode::Auto, picked, detected.clone()),
            detected
        );
        assert_eq!(
            effective_definition(DefinitionMode::Auto, None, detected.clone()),
            detected
        );
        assert_eq!(effective_definition(DefinitionMode::Auto, None, None), None);
    }

    #[test]
    fn manual_reads_the_picked_file_and_ignores_the_detection() {
        let picked = Some(PathBuf::from("/tmp/manual.ini"));
        let detected = Some(PathBuf::from("/tmp/auto.ini"));

        assert_eq!(
            effective_definition(DefinitionMode::Manual, picked.clone(), detected),
            picked
        );
        assert_eq!(
            effective_definition(DefinitionMode::Manual, picked.clone(), None),
            picked
        );
        assert_eq!(
            effective_definition(DefinitionMode::Manual, None, None),
            None
        );
    }

    #[gpui_kit::test]
    fn retrying_the_definition_clears_the_previous_attempt(cx: &mut gpui_kit::TestAppContext) {
        let root = std::env::temp_dir().join(format!("oxtuner-wizard-test-{}", std::process::id()));
        cx.update(|cx| {
            cx.set_global(crate::platform::paths::AppPaths {
                config_dir: root.join("config"),
                cache_dir: root.join("cache"),
                logs_dir: root.join("logs"),
                projects_dir: root.join("OxTunerProjects"),
            });
        });
        let model: SharedWizard =
            std::rc::Rc::new(std::cell::RefCell::new(cx.update(|cx| Wizard::new(cx))));
        {
            let mut wizard = model.borrow_mut();
            wizard.definition_error = Some("refused".to_owned());
            wizard.preview_path = Some(PathBuf::from("/tmp/old.ini"));
            wizard.target = Some(PathBuf::from("/tmp/Old"));
        }

        model.borrow_mut().retry_definition();

        let wizard = model.borrow();
        assert!(wizard.definition_error.is_none());
        assert!(wizard.preview_path.is_none());
        assert_eq!(wizard.display_groups.len(), 0);
        assert!(wizard.target.is_none());
    }
}
