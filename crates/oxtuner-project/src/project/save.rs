use oxtuner_tune::TuneDiff;

use crate::atomic;
use crate::load::{self};
use crate::msq::{EntryKind, EntryPage};
use crate::sidecar::{ProjectState, UiState};

use super::{Project, ProjectError, SaveOutcome};

impl Project {
    /// Writes changed values back to the tune file.
    ///
    /// Only variables whose values differ from the baseline are touched, so an
    /// unedited project keeps its file byte-for-byte; unknown elements,
    /// comments and entries this build does not model are never rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the check blocks saving or a file cannot
    /// be written.
    pub fn save(&mut self) -> Result<SaveOutcome, ProjectError> {
        if self.is_read_only() {
            return Err(ProjectError::ReadOnly {
                reasons: self
                    .report
                    .read_only_reasons()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            });
        }

        let diff = TuneDiff::between(self.images.baseline(), self.images.working())?;
        // Edits land in a copy: a write that fails (or a value that cannot be
        // formatted) must leave the session's document exactly as it was, so a
        // later revert cannot save text from the failed attempt.
        let mut document = self.msq.clone();
        let mut written: Vec<String> = Vec::new();
        let mut cells = 0;
        for name in &diff.changed_variables {
            let Some(spec) = self.layout().variable(name) else {
                continue;
            };
            if spec.no_msq_save {
                continue;
            }
            let Some(values) = self.tune().value(name) else {
                continue;
            };
            let Some(baseline) = self.images.baseline().value(name) else {
                continue;
            };
            // Counted before the write: `values` borrows the images and the
            // document copy is mutated next.
            let changed = baseline.changed_cells(values);
            let text = load::format_values(spec, &self.definition, values).map_err(|source| {
                ProjectError::Unrepresentable {
                    name: name.clone(),
                    source,
                }
            })?;

            let (page, kind) = match spec.ini_page() {
                Some(page) => (EntryPage::Numbered(page.to_msq()), EntryKind::Constant),
                None => (EntryPage::Pc, EntryKind::PcVariable),
            };
            document.put_entry_text(page, kind, name, &text)?;

            // Only the cells that actually reached the file are reported; a
            // `noMsqSave` variable is skipped above and must not be counted.
            cells += changed;
            written.push(name.clone());
        }

        document.set_settings(&self.settings, &self.definition.tested_flags)?;

        let bytes = document.save()?;
        atomic::write(&self.tune_path, &bytes).map_err(|source| ProjectError::Io {
            path: self.tune_path.clone(),
            source,
        })?;

        // The file holds these values now; keep the session's copy in step even
        // if the sidecar below fails, so a retry cannot write an older tune.
        self.msq = document;

        if self.in_folder {
            self.state.set_definition(
                super::sidecar_definition_path(&self.root, &self.definition_path),
                self.definition_digest.clone(),
                self.definition.signature.clone(),
            );
            self.persist_state()?;
        }

        self.images.commit_baseline();

        Ok(SaveOutcome {
            path: self.tune_path.clone(),
            cells,
            variables: written,
        })
    }

    /// Renders the **working** image as a complete `.msq` document.
    ///
    /// Unlike [`Self::save`], which touches only changed values in the file it
    /// owns, this writes every representable variable into a copy of the
    /// document. It is the snapshot used before a risky operation (ADR-0011 S4)
    /// and is never written over the project's tune. A value the file cannot
    /// represent keeps the text the document already had: a variable with no
    /// label, a broken text constant, or a `scale` expression this build could
    /// not evaluate (Speeduino's `{idleRes}`). The last case is safe because
    /// such a value is never loaded from the file nor edited, so the tune holds
    /// no change worth snapshotting.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the tune cannot be serialized.
    pub fn snapshot_msq(&self) -> Result<Vec<u8>, ProjectError> {
        let mut document = self.msq.clone();
        for spec in self.layout().variables() {
            if spec.no_msq_save {
                continue;
            }
            let Some(values) = self.tune().value(&spec.name) else {
                continue;
            };
            if values.is_empty() {
                continue;
            }
            let Ok(text) = load::format_values(spec, &self.definition, values) else {
                continue;
            };
            let (page, kind) = match spec.ini_page() {
                Some(page) => (EntryPage::Numbered(page.to_msq()), EntryKind::Constant),
                None => (EntryPage::Pc, EntryKind::PcVariable),
            };
            document.put_entry_text(page, kind, &spec.name, &text)?;
        }
        document.set_settings(&self.settings, &self.definition.tested_flags)?;
        document.save().map_err(ProjectError::Msq)
    }

    /// The project's persisted state: definition reference, device and UI.
    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    /// Remembers which device the project uses and persists it.
    ///
    /// Only the device section changes; the definition reference and the UI
    /// layout the file already holds are kept.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Sidecar`] when the state file cannot be
    /// written. A standalone tune keeps the device in memory only, and a
    /// sidecar from a newer build is left untouched.
    pub fn set_device(
        &mut self,
        port: Option<String>,
        baud: Option<u32>,
    ) -> Result<(), ProjectError> {
        self.state.device.last_port = port;
        self.state.device.baud_override = baud;
        self.persist_state()
    }

    /// Replaces the project's panel layout and persists it.
    ///
    /// The layout is per project on purpose: the same tune opens with the
    /// tables and panels the reader arranged for it, whatever another project
    /// last looked like.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Sidecar`] when the state file cannot be
    /// written. A standalone tune keeps the layout in memory only.
    pub fn set_ui(&mut self, ui: UiState) -> Result<(), ProjectError> {
        self.state.ui = ui;
        self.persist_state()
    }

    /// Writes the state file, unless there is nowhere or nothing to write.
    ///
    /// A standalone tune keeps its state in memory, and a sidecar from a
    /// newer build stays untouched; both are an `Ok` with a debug line, not
    /// an error, because the state is a convenience next to the tune.
    fn persist_state(&self) -> Result<(), ProjectError> {
        if !self.in_folder {
            return Ok(());
        }
        if !self.state_writable {
            tracing::debug!(
                root = %self.root.display(),
                "the project state needs a newer build and stays untouched"
            );
            return Ok(());
        }
        self.state
            .write(&self.root)
            .map(|_| ())
            .map_err(|source| ProjectError::Sidecar {
                path: self.root.join(crate::sidecar::SIDECAR_DIR),
                source,
            })
    }
}
