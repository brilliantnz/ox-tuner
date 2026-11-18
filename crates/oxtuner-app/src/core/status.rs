//! The app-facing status snapshot and the fold from core events into it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use gpui_kit::SharedString;
use oxtuner_core::{
    BurnReport, ConnectionTest, ConnectionView, DefinitionView, DialogView, Failure, FailureCode,
    ImportReport, JournalEntry, MenuNode, OpId, PortChoice, PortView, ProjectView, ReadReport,
    RevertReport, SaveReport, SessionLogReport, SessionLogSnapshot, TableView, TuneStatus,
    WriteReport,
};

/// What the views show about the core. Small and `Clone`: rendering reads it
/// every frame.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the status tracks independent connection and session flags"
)]
#[derive(Debug, Clone, Default)]
pub(crate) struct CoreStatus {
    /// A definition loaded on its own (no tune file).
    pub(crate) definition: Option<DefinitionView>,
    /// The project the core currently owns, when one is open.
    pub(crate) project: Option<ProjectView>,
    /// The project folder the UI asked to open and is waiting for.
    ///
    /// Transient view state, never persisted and never part of the project
    /// model: it only holds the request between the click and the core's
    /// `ProjectOpened`/`Failed` answer.
    pub(crate) pending_project: Option<PendingProject>,
    /// Whether the open project's folder was found missing at the last
    /// lifecycle check. Rendering reads this instead of touching the
    /// filesystem; it starts false because no check has failed yet.
    pub(crate) project_missing: bool,
    /// The last successful save, shown until the next open or failure.
    pub(crate) last_save: Option<SaveReport>,
    /// The table the UI last read, kept until another table replaces it.
    pub(crate) table: Option<TableView>,
    /// The dialog the UI last read, kept until another dialog replaces it.
    pub(crate) dialog: Option<DialogView>,
    /// Definition targets (table or dialog ids) whose last snapshot reported
    /// cells that differ from the file baseline.
    ///
    /// Every read of a target refreshes its entry, so a target edited once and
    /// then left alone keeps its mark while another target is on screen. The
    /// set is cleared by anything that rebases the whole tune: opening a
    /// project, saving, reverting, importing or adopting the ECU image.
    pub(crate) dirty: BTreeSet<String>,
    /// The last command failure, kept until a command succeeds.
    pub(crate) failure: Option<Failure>,
    /// Bumped whenever a failure is stored. A view that is waiting for an
    /// answer can compare this to the value it saw when it asked, so an old
    /// failure still on screen is not mistaken for the new one.
    pub(crate) failure_serial: u64,
    /// Bumped whenever an edit or a save may have moved tune values, so a view
    /// can tell that copies it kept are no longer trustworthy.
    pub(crate) tune_revision: u64,
    /// The ECU session, when one is up.
    pub(crate) connection: Option<ConnectionView>,
    /// Why the link died, until a reconnect succeeds.
    pub(crate) link_lost: Option<String>,
    /// The last complete page read.
    pub(crate) pages_read: Option<ReadReport>,
    /// Whether realtime polling is running.
    pub(crate) realtime: bool,
    /// The last saved session trace.
    pub(crate) last_trace: Option<TraceSaved>,
    /// The serial ports the core last published.
    ///
    /// Both the on-demand answer and the pushed change land here; use
    /// [`crate::core::ports`] to read them, which also tells a machine with no
    /// ports apart from one the core has not sampled yet.
    pub(crate) ports: Vec<PortView>,
    /// How many port snapshots have folded into [`Self::ports`].
    ///
    /// Zero means no sample has arrived yet; a view renders that as still
    /// looking rather than as an empty machine.
    pub(crate) ports_revision: u64,
    /// The port a signature test is running against.
    pub(crate) testing: Option<String>,
    /// The last signature test.
    pub(crate) test: Option<ConnectionTest>,
    /// The connection request that is still being answered.
    pub(crate) pending_connect: Option<PortChoice>,
    /// Whether the global write switch is on.
    pub(crate) writes_enabled: bool,
    /// Why the write path is halted after a failed verification, until the user
    /// acknowledges it.
    pub(crate) write_halted: Option<String>,
    /// The derived write state after the last read, edit or write.
    pub(crate) tune_status: Option<TuneStatus>,
    /// The last verified RAM write.
    pub(crate) last_write: Option<WriteReport>,
    /// The last verified burn.
    pub(crate) last_burn: Option<BurnReport>,
    /// The last import or snapshot restore.
    pub(crate) last_import: Option<ImportReport>,
    /// The last revert.
    pub(crate) last_revert: Option<RevertReport>,
    /// The project's write journal, after `ReadJournal`.
    pub(crate) journal: Option<JournalView>,
    /// The project's snapshots, after `ListSnapshots`.
    pub(crate) snapshots: Vec<PathBuf>,
    /// A tune path waiting for the user's consent to import it.
    pub(crate) import_consent: Option<PathBuf>,
    /// The last stopped session log, for the output panel.
    pub(crate) last_log: Option<SessionLogSnapshot>,
    /// Moves when a definition or project is installed, so views can rebuild
    /// their channel lists.
    pub(crate) definition_revision: u64,
    /// Moves when the core publishes an evaluated menu, without invalidating
    /// the dialog/table snapshots that `definition_revision` governs.
    pub(crate) menu_revision: u64,
}

/// A project open the UI is waiting for.
///
/// The op id correlates the completion event: a slow answer to an earlier
/// request must not clear the loading state of a newer request. `None` only
/// exists between setting the state and handing the command to the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingProject {
    /// The folder the user chose.
    pub(crate) path: PathBuf,
    /// Folder name for the title bar and the opening splash, resolved once.
    pub(crate) name: SharedString,
    /// The core operation that will answer this request, once accepted.
    ///
    /// Stored as the raw id only for equality; the UI never inspects it.
    pub(crate) op: Option<u64>,
}

/// The journal file and its newest entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalView {
    /// The file, when the session has one.
    pub(crate) path: Option<PathBuf>,
    /// Newest last.
    pub(crate) entries: Vec<JournalEntry>,
}

/// A saved session trace, for the output panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TraceSaved {
    /// Where it was written.
    pub(crate) path: PathBuf,
    /// Frames recorded.
    pub(crate) frames: usize,
}

impl CoreStatus {
    /// The definition to name, whether it came from a project or on its own.
    pub(crate) fn definition(&self) -> Option<&DefinitionView> {
        self.project
            .as_ref()
            .map(|project| &project.definition)
            .or(self.definition.as_ref())
    }

    /// Marks a folder as being opened, before the command is sent.
    pub(crate) fn begin_pending_project(&mut self, path: &Path) {
        self.pending_project = Some(PendingProject {
            path: path.to_owned(),
            name: crate::project::display_name(path).into(),
            op: None,
        });
    }

    /// Records the op the core accepted for the pending open.
    pub(crate) fn pending_project_sent(&mut self, op: OpId) {
        if let Some(pending) = &mut self.pending_project {
            pending.op = Some(op.get());
        }
    }

    /// Clears the pending open if `op` is its answer.
    ///
    /// The caller repaints on every answer, so the result is not returned.
    /// [`Self::finish_pending_project_at`] keeps it for the tests.
    pub(crate) fn finish_pending_project(&mut self, op: Option<OpId>) {
        self.finish_pending_project_at(op.map(OpId::get));
    }

    fn finish_pending_project_at(&mut self, op: Option<u64>) -> bool {
        let Some(pending) = &self.pending_project else {
            return false;
        };
        if pending.op.is_some() && pending.op != op {
            return false;
        }
        tracing::debug!(
            path = %pending.path.display(),
            "the pending project open finished"
        );
        self.pending_project = None;
        true
    }

    /// Clears the pending open after a command that never reached the core.
    pub(crate) fn cancel_pending_project(&mut self) {
        self.pending_project = None;
    }

    /// Clears the pending open when its answer never arrived.
    ///
    /// Returns whether this call cleared the state. Only the request with the
    /// same op expires, so a newer open keeps its splash.
    pub(crate) fn expire_pending_project(&mut self, op: OpId) -> bool {
        self.expire_pending_project_at(op.get())
    }

    fn expire_pending_project_at(&mut self, op: u64) -> bool {
        let waiting = self
            .pending_project
            .as_ref()
            .is_some_and(|pending| pending.op == Some(op));
        if waiting {
            self.pending_project = None;
        }
        waiting
    }
}
pub(super) enum Update {
    Loaded(DefinitionView),
    Tested(ConnectionView),
    /// A project opened; boxed because its summary is much larger than the
    /// other updates.
    Opened(Box<ProjectView>),
    Saved(SaveReport),
    Table {
        table: Box<TableView>,
        edited: bool,
    },
    Dialog {
        dialog: DialogView,
        edited: bool,
    },
    Menu(Vec<MenuNode>),
    Connected(ConnectionView),
    Reconnected(ConnectionView),
    LinkLost(String),
    Disconnected,
    PagesRead(ReadReport),
    EcuAdopted,
    Realtime(bool),
    Trace {
        saved: TraceSaved,
    },
    Written(WriteReport),
    Burned(BurnReport),
    ControllerActionRan,
    ConflictsResolved,
    Imported(ImportReport),
    Reverted(RevertReport),
    TuneStatus(TuneStatus),
    WriteAccess {
        enabled: bool,
        connection: Option<ConnectionView>,
    },
    Acknowledged,
    Journal(JournalView),
    Snapshots(Vec<PathBuf>),
    Log {
        running: bool,
        report: Option<SessionLogReport>,
    },
    Unloaded,
    Failed(Failure),
    Nothing,
}

impl CoreStatus {
    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive match folds every core update into the status"
    )]
    pub(super) fn apply(&mut self, update: Update) {
        match update {
            Update::Loaded(summary) => {
                self.definition = Some(summary);
                self.project = None;
                self.last_save = None;
                self.table = None;
                self.dialog = None;
                self.dirty.clear();
                self.failure = None;
                self.tune_status = None;
                self.definition_revision = self.definition_revision.wrapping_add(1);
                self.menu_revision = self.menu_revision.wrapping_add(1);
            }
            Update::Opened(project) => {
                self.project = Some(*project);
                self.definition = None;
                self.project_missing = false;
                self.last_save = None;
                self.table = None;
                self.dialog = None;
                self.dirty.clear();
                self.failure = None;
                self.tune_status = None;
                self.journal = None;
                self.snapshots.clear();
                self.definition_revision = self.definition_revision.wrapping_add(1);
                self.menu_revision = self.menu_revision.wrapping_add(1);
            }
            Update::Saved(outcome) => {
                self.last_save = Some(outcome);
                self.dirty.clear();
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Table { table, edited } => {
                if table.changed.iter().any(|changed| *changed) {
                    self.dirty.insert(table.id.clone());
                } else {
                    self.dirty.remove(&table.id);
                }

                self.table = Some(*table);
                // A successful table operation also proves the earlier failure
                // is stale.
                self.failure = None;
                if edited {
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
            }
            Update::Dialog { dialog, edited } => {
                if dialog.changed > 0 {
                    self.dirty.insert(dialog.id.clone());
                } else {
                    self.dirty.remove(&dialog.id);
                }

                self.dialog = Some(dialog);
                self.failure = None;
                if edited {
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
            }
            Update::Menu(menu) => {
                if let Some(project) = &mut self.project {
                    project.definition.menu = menu;
                } else if let Some(definition) = &mut self.definition {
                    definition.menu = menu;
                }
                self.menu_revision = self.menu_revision.wrapping_add(1);
            }
            Update::Tested(summary) => {
                let port = summary.port.clone();
                self.test = Some(ConnectionTest {
                    port,
                    outcome: Ok(summary),
                });
                self.testing = None;
                self.failure = None;
            }
            Update::Connected(connection) => {
                self.connection = Some(connection);
                self.link_lost = None;
                self.pages_read = None;
                self.realtime = false;
                self.failure = None;
                self.pending_connect = None;
                self.testing = None;
                self.test = None;
            }
            Update::Reconnected(connection) => {
                self.connection = Some(connection);
                self.link_lost = None;
                self.pages_read = None;
                self.realtime = false;
                self.failure = None;
            }
            Update::LinkLost(reason) => {
                self.link_lost = Some(reason);
                self.realtime = false;
                self.pending_connect = None;
            }
            Update::Disconnected => {
                self.connection = None;
                self.link_lost = None;
                self.pending_connect = None;
                // The last read still describes what the tune image holds.
                self.realtime = false;
            }
            Update::PagesRead(summary) => {
                self.pages_read = Some(summary);
                self.failure = None;
                // The ECU image moved: a table kept on screen can no longer
                // trust the write-state marks it drew from the old one.
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::EcuAdopted => {
                self.failure = None;
                // The adopted image is the new starting point for targets the
                // core has not re-read; anything still marked is stale.
                self.dirty.clear();
                // The working tune changed under every open table and dialog.
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Realtime(running) => self.realtime = running,
            Update::Trace { saved } => {
                self.last_trace = Some(saved);
                self.failure = None;
            }
            Update::Written(summary) => {
                self.last_write = Some(summary);
                self.write_halted = None;
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Burned(summary) => {
                self.last_burn = Some(summary);
                self.write_halted = None;
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::ConflictsResolved => {
                if let Some(read) = &mut self.pages_read {
                    read.conflicts.clear();
                }
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::ControllerActionRan => {
                self.failure = None;
                // A raw command bypassed memory synchronization, so the ECU
                // image is stale; open tables must read their states again.
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Imported(summary) => {
                self.last_import = Some(summary);
                self.import_consent = None;
                self.dirty.clear();
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::Reverted(summary) => {
                self.last_revert = Some(summary);
                self.dirty.clear();
                self.failure = None;
                self.tune_revision = self.tune_revision.wrapping_add(1);
            }
            Update::TuneStatus(status) => self.tune_status = Some(status),
            Update::WriteAccess {
                enabled,
                connection,
            } => {
                self.writes_enabled = enabled;
                if let Some(connection) = connection {
                    self.connection = Some(connection);
                }
                self.failure = None;
            }
            Update::Acknowledged => self.write_halted = None,
            Update::Journal(view) => self.journal = Some(view),
            Update::Snapshots(paths) => self.snapshots = paths,
            Update::Log { running, report } => {
                if let Some(report) = report {
                    self.last_log = Some(SessionLogSnapshot::finished(report));
                }
                if running {
                    self.last_log = Some(SessionLogSnapshot::running(
                        self.last_log.as_ref().and_then(|log| log.path.clone()),
                    ));
                }
            }
            Update::Unloaded => {
                let writes_enabled = self.writes_enabled;
                *self = Self {
                    writes_enabled,
                    ..Self::default()
                };
            }
            Update::Failed(failure) => {
                // A table or dialog that could not be read must not stay on
                // screen as if it were still the selection; an edit failure
                // keeps it, since the user needs to see what was changing.
                if failure.code == FailureCode::TableUnavailable {
                    self.table = None;
                }
                if failure.code == FailureCode::DialogUnavailable {
                    self.dialog = None;
                }
                // A failed test must not leave the dialog stuck on "working".
                // The message stays for the dialog to show.
                if failure.code == FailureCode::TestFailed {
                    let port = self.testing.take().unwrap_or_default();
                    self.test = Some(ConnectionTest {
                        port,
                        outcome: Err(failure.message.clone()),
                    });
                }
                if failure.code == FailureCode::ConnectFailed {
                    self.pending_connect = None;
                }
                if matches!(
                    failure.code,
                    FailureCode::WriteFailed | FailureCode::BurnFailed
                ) {
                    self.write_halted = Some(failure.message.clone());
                    // The core reloaded the ECU image from the read-back and
                    // marked the changed cells; a table on screen must refetch
                    // its states instead of showing the pre-write snapshot.
                    self.tune_revision = self.tune_revision.wrapping_add(1);
                }
                self.failure_serial = self.failure_serial.wrapping_add(1);
                self.failure = Some(failure);
            }
            Update::Nothing => {}
        }
    }
}

#[cfg(test)]
mod tests;
