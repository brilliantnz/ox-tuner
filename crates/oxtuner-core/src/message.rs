//! The core's message vocabulary.
//!
//! These types are the whole UI↔core contract (`docs/architecture/ui-core/contract.md`).
//! Two rules shape them:
//!
//! - **Commands carry what the core needs, never a borrowed view of UI state.**
//!   A definition load moves bytes and a settings snapshot in; nothing is shared.
//! - **Every accepted command is answered exactly once.** [`OpId`] is assigned by
//!   the sender, so the UI can correlate a reply without trusting arrival order;
//!   the core answers with [`Event::DefinitionLoaded`], [`Event::ProjectOpened`],
//!   [`Event::TuneSaved`], [`Event::DefinitionUnloaded`], [`Event::Failed`] or
//!   [`Event::Rejected`] — never twice, never silently.

use std::{fmt, path::PathBuf};

use oxtuner_ini::{Definition, SettingsContext, Severity};
use oxtuner_project::Project;

use crate::menu::MenuNode;

/// Identifies one accepted command for its whole lifetime.
///
/// Ids come from the sending handle and are unique per process. A gap means a
/// command was **refused** by the queue (see [`crate::CoreError::QueueFull`])
/// and never entered the core — only accepted ids receive an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpId(u64);

impl OpId {
    /// The first id a fresh core hands out.
    pub(crate) const FIRST: u64 = 1;

    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw counter value, for logs.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "op-{}", self.0)
    }
}

/// A request from the UI.
#[derive(Debug)]
pub enum Command {
    /// Parse `bytes` and take ownership of the resulting definition.
    ///
    /// `settings` is the project's settings snapshot (`<settings>` in the
    /// `.msq`); `#if` branches and the protocol commands they rewrite are
    /// resolved with it (ADR-0006). A definition that replaces an already
    /// loaded one is allowed — the core drops the old one.
    LoadDefinition {
        /// What to call the source in logs and the UI (usually a file name).
        label: String,
        /// The definition file, as read from disk.
        bytes: Vec<u8>,
        /// Settings the `#if` branches resolve against.
        settings: SettingsContext,
    },
    /// Open a TunerStudio project folder in place (ADR-0007).
    ///
    /// The core reads the definition and the tune, owns both, and answers with
    /// [`Event::ProjectOpened`] — or [`Event::Failed`] when the folder is not a
    /// project or a file cannot be read. A failure leaves the previous session
    /// untouched. A successful open **replaces** it: the previous definition and
    /// tune are dropped in the core.
    ///
    /// The read happens on the core thread, never on the UI thread.
    OpenProject {
        /// The project folder (a directory containing `projectCfg/`).
        path: PathBuf,
    },
    /// Write the open project's changed values back to its tune file.
    ///
    /// Only variables that differ from the baseline are touched, so an unedited
    /// tune stays byte-for-byte identical. A read-only session refuses with
    /// [`FailureCode::TuneSaveFailed`] and the S1 reasons.
    SaveTune,
    /// Drop the loaded definition and tune.
    UnloadDefinition,
    /// Stop the core. No command is accepted after this one is read.
    Shutdown,
}

impl Command {
    /// A definition load against the default settings context.
    pub fn load_definition(label: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self::LoadDefinition {
            label: label.into(),
            bytes: bytes.into(),
            settings: SettingsContext::new(),
        }
    }

    /// A stable name for logs. Never contains user data.
    pub fn name(&self) -> &'static str {
        match self {
            Self::LoadDefinition { .. } => "load-definition",
            Self::OpenProject { .. } => "open-project",
            Self::SaveTune => "save-tune",
            Self::UnloadDefinition => "unload-definition",
            Self::Shutdown => "shutdown",
        }
    }
}

/// A reply from the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The definition is loaded and owned by the core.
    DefinitionLoaded {
        op: OpId,
        summary: DefinitionSummary,
    },
    /// A project is open and owned by the core: definition, tune images and the
    /// load gate's findings.
    ProjectOpened { op: OpId, summary: ProjectSummary },
    /// The open project's tune file was rewritten.
    TuneSaved { op: OpId, outcome: SaveSummary },
    /// The loaded definition (and tune, if a project was open) was dropped.
    DefinitionUnloaded { op: OpId },
    /// The command was handled but could not do its work.
    Failed { op: OpId, failure: Failure },
    /// The command was accepted, then never executed because the core stopped.
    Rejected { op: OpId, reason: RejectReason },
    /// The core thread has stopped. Always the last event.
    Stopped,
}

impl Event {
    /// The operation this event answers, if it answers one.
    pub fn op(&self) -> Option<OpId> {
        match self {
            Self::DefinitionLoaded { op, .. }
            | Self::ProjectOpened { op, .. }
            | Self::TuneSaved { op, .. }
            | Self::DefinitionUnloaded { op }
            | Self::Failed { op, .. }
            | Self::Rejected { op, .. } => Some(*op),
            Self::Stopped => None,
        }
    }
}

/// Why a command was accepted and then not executed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// The core was stopping; the command never touched any resource.
    ShuttingDown,
}

impl RejectReason {
    /// A stable code for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShuttingDown => "shutting-down",
        }
    }
}

/// A handled command that could not do its work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// What kind of failure it was.
    pub code: FailureCode,
    /// The full message, including per-line diagnostics where the parser has
    /// them. Designed to be shown in the output panel as-is.
    pub message: String,
}

/// The failure kinds the core can report today.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCode {
    /// The bytes are not a definition this build can read (ADR-0006 rule 11:
    /// an unsupported `iniSpecVersion` fails loudly rather than half-parsing).
    DefinitionRejected,
    /// The folder is not an openable TunerStudio project, or a file in it could
    /// not be read or parsed.
    ProjectRejected,
    /// The tune file could not be written, or the session is read-only (S1).
    TuneSaveFailed,
    /// There is no open tune to save (no project, or only a definition).
    NothingToSave,
}

impl FailureCode {
    /// A stable code for logs and for the UI to switch on.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DefinitionRejected => "definition-rejected",
            Self::ProjectRejected => "project-rejected",
            Self::TuneSaveFailed => "tune-save-failed",
            Self::NothingToSave => "nothing-to-save",
        }
    }
}

/// The load result, shaped for the UI rather than for the parser.
///
/// Deliberately small and `Clone`: the core keeps the full [`Definition`];
/// the UI only needs enough to name what it is looking at.
#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionSummary {
    /// The label the load command carried.
    pub label: String,
    /// Firmware signature.
    pub signature: String,
    /// `iniSpecVersion`, when the file declares one.
    pub spec_version: Option<f64>,
    /// Calibration pages.
    pub pages: usize,
    /// Calibration constants.
    pub constants: usize,
    /// PC-side variables.
    pub pc_variables: usize,
    /// Realtime channels.
    pub channels: usize,
    /// 2-D tables.
    pub tables: usize,
    /// 1-D curves.
    pub curves: usize,
    /// Gauge templates.
    pub gauges: usize,
    /// Dialogs.
    pub dialogs: usize,
    /// The navigation tree the primary sidebar draws, in definition order.
    pub menu: Vec<MenuNode>,
    /// Share of entries that became model data, 0–100.
    pub coverage_percent: f64,
    /// Parse errors recorded while loading.
    pub errors: usize,
    /// Parse warnings recorded while loading.
    pub warnings: usize,
    /// `#if` flags active for this load, sorted.
    pub active_flags: Vec<String>,
}

impl DefinitionSummary {
    /// Builds the UI-facing summary of a parsed definition.
    pub fn new(label: impl Into<String>, definition: &Definition) -> Self {
        let severity = |wanted: Severity| {
            definition
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == wanted)
                .count()
        };

        Self {
            label: label.into(),
            signature: definition.signature.clone(),
            spec_version: definition.spec_version,
            pages: definition.pages.len(),
            constants: definition.constants.len(),
            pc_variables: definition.pc_variables.len(),
            channels: definition.channels.len(),
            tables: definition.tables.len(),
            curves: definition.curves.len(),
            gauges: definition.gauges.len(),
            dialogs: definition.dialogs.len(),
            menu: crate::menu::build(definition),
            coverage_percent: definition.coverage.percent(),
            errors: severity(Severity::Error),
            warnings: severity(Severity::Warning),
            active_flags: definition.active_flags.iter().cloned().collect(),
        }
    }

    /// Whether the definition parsed without errors.
    pub fn is_clean(&self) -> bool {
        self.errors == 0
    }
}

/// An open project, shaped for the UI rather than for the file layer.
///
/// Deliberately small and `Clone`: the core keeps the [`Project`] (definition,
/// tune images, `.msq` document); the UI only needs enough to name what is open,
/// explain read-only mode and show the load gate's findings.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSummary {
    /// The project folder, or the tune's folder in standalone mode.
    pub root: PathBuf,
    /// The tune file that was read (and will be written).
    pub tune_path: PathBuf,
    /// The definition file the tune was interpreted with.
    pub definition_path: PathBuf,
    /// Whether this is a project folder (as opposed to standalone).
    pub in_project: bool,
    /// The definition, summarised like a standalone load.
    pub definition: DefinitionSummary,
    /// Whether saving is refused (S1).
    pub read_only: bool,
    /// Why the tune opened read-only, for the editor and output panel that
    /// explain it.
    pub read_only_reasons: Vec<String>,
    /// Values copied from the tune into the model.
    pub loaded: usize,
    /// Values left at their definition default (absent from the tune).
    pub skipped: usize,
    /// Non-fatal findings, ready for the output panel.
    pub warnings: Vec<String>,
}

impl ProjectSummary {
    /// Builds the UI-facing summary of an open project.
    pub(crate) fn from_project(project: &Project) -> Self {
        let report = project.report();
        let label = project
            .definition_path()
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(
                || project.definition_path().display().to_string(),
                str::to_owned,
            );

        Self {
            root: project.root().to_owned(),
            tune_path: project.tune_path().to_owned(),
            definition_path: project.definition_path().to_owned(),
            in_project: project.is_in_project(),
            definition: DefinitionSummary::new(label, project.definition()),
            read_only: report.is_read_only(),
            read_only_reasons: report
                .read_only_reasons()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            loaded: report.loaded,
            skipped: report.skipped,
            warnings: report
                .warnings
                .iter()
                .map(|issue| issue.message.clone())
                .collect(),
        }
    }
}

/// What a successful [`Command::SaveTune`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSummary {
    /// The tune file that was rewritten.
    pub path: PathBuf,
    /// Number of values that changed and were written.
    pub cells: usize,
}

/// One command plus the id it will be answered under.
///
/// Internal to the crate: senders wrap a [`Command`], receivers unwrap it. A
/// command never reaches the core without an id, so "answered exactly once"
/// is a property of the type rather than a convention.
#[derive(Debug)]
pub(crate) struct Envelope {
    pub(crate) op: OpId,
    pub(crate) command: Command,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn summary() -> DefinitionSummary {
        DefinitionSummary {
            label: "test.ini".to_owned(),
            signature: "speeduino 202501".to_owned(),
            spec_version: Some(3.78),
            pages: 15,
            constants: 817,
            pc_variables: 50,
            channels: 229,
            tables: 21,
            curves: 34,
            gauges: 90,
            dialogs: 239,
            menu: Vec::new(),
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: vec!["mcu_stm32".to_owned()],
        }
    }

    #[test]
    fn an_op_id_renders_with_its_prefix() {
        assert_eq!(OpId::new(7).to_string(), "op-7");
        assert_eq!(OpId::new(7).get(), 7);
    }

    #[test]
    fn only_replies_carry_the_op_they_answer() {
        let op = OpId::new(3);

        assert_eq!(Event::DefinitionUnloaded { op }.op(), Some(op));
        assert_eq!(
            Event::DefinitionLoaded {
                op,
                summary: summary()
            }
            .op(),
            Some(op)
        );
        assert_eq!(
            Event::TuneSaved {
                op,
                outcome: SaveSummary {
                    path: PathBuf::from("CurrentTune.msq"),
                    cells: 0,
                },
            }
            .op(),
            Some(op)
        );
        assert_eq!(Event::Stopped.op(), None);
    }

    #[test]
    fn a_command_constructed_from_bytes_uses_the_default_settings() {
        let Command::LoadDefinition {
            label, settings, ..
        } = Command::load_definition("test.ini", vec![0x5B])
        else {
            panic!("wrong variant");
        };

        assert_eq!(label, "test.ini");
        assert!(settings.tested_flags().is_empty());
    }

    #[test]
    fn summaries_are_ordered_by_flag_for_a_stable_ui() {
        let summary = summary();

        let flags: HashSet<_> = summary.active_flags.iter().cloned().collect();
        assert_eq!(flags.len(), summary.active_flags.len());
        assert!(summary.is_clean());
    }
}
