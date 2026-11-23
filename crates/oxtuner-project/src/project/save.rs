use std::io::ErrorKind;

use oxtuner_tune::{Owner, TuneDiff};

use crate::Msq;
use crate::atomic;
use crate::msq::{EntryKind, EntryPage};
use crate::sidecar::{ProjectLayout, ProjectState};

use super::{Project, ProjectError, SaveOutcome};

impl Project {
    /// Drops the views a reopen can derive from their owner.
    ///
    /// A view is not a value a file carries, but a file that holds a view and
    /// not its owner is the only carrier of those bytes: the loader reads the
    /// owner's value from it. Such a view is kept, because removing it would
    /// lose the value on the next open. `written` names the owners the caller
    /// is about to write; `None` means the caller writes every value, as a
    /// snapshot does.
    fn remove_alias_entries(&self, document: &mut Msq, written: Option<&[String]>) {
        for spec in self.layout().variables() {
            if !self.layout().is_alias(&spec.name) {
                continue;
            }
            let Some(owner) = self.layout().alias_owner(&spec.name) else {
                continue;
            };
            let Some(owner_spec) = self.layout().variable(owner) else {
                continue;
            };
            if owner_spec.no_msq_save {
                // The owner never reaches a file; the view is its carrier.
                continue;
            }
            // A view is always a page constant: `mark_aliases` only looks at
            // a page's variables, so there is no PC entry to remove.
            let Some(page) = spec.ini_page() else {
                continue;
            };
            let page = EntryPage::Numbered(page.to_msq());
            let owner_written = written
                .is_none_or(|names| names.iter().any(|name| name.eq_ignore_ascii_case(owner)));
            if !owner_written && !document.has_entry(page, EntryKind::Constant, owner) {
                continue;
            }
            document.remove_entry(page, EntryKind::Constant, &spec.name);
        }
    }

    /// Writes changed values back to the tune file.
    ///
    /// Only variables whose values differ from the baseline are touched, so an
    /// unedited project keeps its file byte-for-byte apart from one thing: a
    /// constant whose bytes another constant covers is removed, because a view
    /// is not a value a file carries. Unknown elements, comments and entries
    /// this build does not model are never rewritten.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] when the check blocks saving or a file cannot
    /// be written.
    pub fn save(&mut self) -> Result<SaveOutcome, ProjectError> {
        self.save_with_stamp(None)
    }

    /// Saves, stamping the tune's `writeDate` when a stamp is supplied.
    ///
    /// The core passes a fresh stamp on every user save, so a file does not keep
    /// claiming it was saved at creation time. `None` leaves the header exactly
    /// as the file had it, which is what a byte-identical unedited save and the
    /// tests use.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] for the same reasons as [`Self::save`].
    pub fn save_with_stamp(
        &mut self,
        write_stamp: Option<&str>,
    ) -> Result<SaveOutcome, ProjectError> {
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
        if let Some(stamp) = write_stamp {
            document.set_write_date(stamp);
        }
        self.remove_alias_entries(&mut document, Some(&diff.changed_variables));
        let mut written: Vec<String> = Vec::new();
        let mut cells = 0;
        for name in &diff.changed_variables {
            let Some(spec) = self.layout().variable(name) else {
                continue;
            };
            if spec.no_msq_save {
                continue;
            }
            if spec.scale.is_none() || spec.translate.is_none() {
                // The file's own text is its only representation; the loader
                // skipped it and no edit can change it.
                continue;
            }
            if self.layout().is_alias(name) {
                // Already removed from the document above; nothing to write.
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
            let (page, kind) = match spec.ini_page() {
                Some(page) => (EntryPage::Numbered(page.to_msq()), EntryKind::Constant),
                None => (EntryPage::Pc, EntryKind::PcVariable),
            };
            let text = match document.format_entry(page, kind, spec, &self.definition, values) {
                Ok(text) => text,
                // The ECU owns this value and the file cannot express it; the
                // file's own text stays rather than wedging every later save.
                Err(error) if spec.is_controller_owned() => {
                    tracing::debug!(%name, %error, "a controller-owned value stays as the file had it");
                    continue;
                }
                Err(source) => {
                    return Err(ProjectError::Unrepresentable {
                        name: name.clone(),
                        source,
                    });
                }
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

        self.save_pc_values(&diff)?;

        if self.in_folder {
            self.state.set_definition(
                super::sidecar_definition_path(&self.root, &self.definition_path),
                self.definition_digest.clone(),
                self.definition.signature.clone(),
            );
            self.persist_state()?;
        }

        self.images.commit_baseline();
        tracing::debug!(
            path = %self.tune_path.display(),
            cells,
            variables = written.len(),
            "tune saved"
        );

        Ok(SaveOutcome {
            path: self.tune_path.clone(),
            cells,
            variables: written,
        })
    }

    /// Writes the changed PC variables where the project reads them from.
    ///
    /// A PC value lives in `pcVariableValues.msq` and overrides the tune page
    /// on open; saving the tune alone would lose the edit on the next open.
    /// Only changed PC values are touched, so an unedited project keeps its
    /// sidecar byte-for-byte.
    fn save_pc_values(&self, diff: &TuneDiff) -> Result<(), ProjectError> {
        if !self.in_folder {
            return Ok(());
        }
        let names: Vec<&str> = diff
            .changed_variables
            .iter()
            .filter_map(|name| {
                let spec = self.layout().variable(name)?;
                (spec.owner == Owner::Pc).then_some(name.as_str())
            })
            .collect();
        if names.is_empty() {
            return Ok(());
        }

        let path = self
            .root
            .join(super::CONFIG_DIR)
            .join(super::PC_VALUES_FILE);
        let mut document = match std::fs::read(&path) {
            Ok(bytes) => Msq::parse(&bytes).map_err(ProjectError::Msq)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let template = format!(
                    "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<msq xmlns=\"http://www.msefi.com/:msq\">\n    <versionInfo fileFormat=\"4.0\" signature=\"{}\"/>\n    <page number=\"-1\"/>\n</msq>\n",
                    crate::xml::escape_attribute(&self.definition.signature)
                );
                Msq::parse(template.as_bytes()).map_err(ProjectError::Msq)?
            }
            Err(source) => {
                return Err(ProjectError::Io { path, source });
            }
        };

        for name in names {
            let Some(spec) = self.layout().variable(name) else {
                continue;
            };
            if spec.is_controller_owned() || spec.scale.is_none() || spec.translate.is_none() {
                // The ECU owns it, or the definition has no presentation for
                // it; the sidecar keeps its text rather than wedging a save.
                continue;
            }
            let Some(values) = self.tune().value(name) else {
                continue;
            };
            let text = document
                .format_entry(
                    EntryPage::Pc,
                    EntryKind::PcVariable,
                    spec,
                    &self.definition,
                    values,
                )
                .map_err(|source| ProjectError::Unrepresentable {
                    name: name.to_owned(),
                    source,
                })?;
            document
                .put_entry_text(EntryPage::Pc, EntryKind::PcVariable, name, &text)
                .map_err(ProjectError::Msq)?;
        }

        let bytes = document.save().map_err(ProjectError::Msq)?;
        atomic::write(&path, &bytes).map_err(|source| ProjectError::Io { path, source })?;
        Ok(())
    }

    /// Renders the **working** image as a complete `.msq` document.
    ///
    /// Unlike [`Self::save`], which touches only changed values in the file it
    /// owns, this writes every representable variable into a copy of the
    /// document, except a constant whose bytes another constant covers: that
    /// entry is removed, and a reload on a page without a text overlap derives
    /// it from the owner. It is the snapshot used before a risky operation
    /// (ADR-0011 S4)
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
        self.remove_alias_entries(&mut document, None);
        for spec in self.layout().variables() {
            if spec.no_msq_save || self.layout().is_alias(&spec.name) {
                continue;
            }
            let Some(values) = self.tune().value(&spec.name) else {
                continue;
            };
            if values.is_empty() {
                continue;
            }
            let (page, kind) = match spec.ini_page() {
                Some(page) => (EntryPage::Numbered(page.to_msq()), EntryKind::Constant),
                None => (EntryPage::Pc, EntryKind::PcVariable),
            };
            let Ok(text) = document.format_entry(page, kind, spec, &self.definition, values) else {
                continue;
            };
            document.put_entry_text(page, kind, &spec.name, &text)?;
        }
        document.set_settings(&self.settings, &self.definition.tested_flags)?;
        document.save().map_err(ProjectError::Msq)
    }

    /// The project's persisted state: definition reference, connection and UI.
    pub fn state(&self) -> &ProjectState {
        &self.state
    }

    /// Remembers which connection the project uses and persists it.
    ///
    /// Only the connection section changes; the definition reference and the UI
    /// layout the file already holds are kept.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::Sidecar`] when the state file cannot be
    /// written. A standalone tune keeps the connection in memory only, and a
    /// sidecar from a newer build is left untouched.
    pub fn set_connection(
        &mut self,
        port: Option<String>,
        baud: Option<u32>,
    ) -> Result<(), ProjectError> {
        self.state.connection.last_port = port;
        self.state.connection.baud_override = baud;
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
    pub fn set_layout(&mut self, layout: ProjectLayout) -> Result<(), ProjectError> {
        self.state.layout = layout;
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
