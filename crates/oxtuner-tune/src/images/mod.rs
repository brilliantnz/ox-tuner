use std::collections::BTreeMap;
use std::sync::Arc;

use crate::diff::diff_values;
use crate::wire::{decode_page, encode_page, patch_page};
use crate::{
    EntryShape, IniPage, Owner, Tune, TuneDiff, TuneError, TuneLayout, Values, VariableSpec,
};

mod conflict;
mod state;

pub use conflict::{CellValue, Conflict, conflict_names};
pub use state::{CellState, PageState, StateTable, TuneStatus};

use conflict::{cell_position, cell_value, diff_writable_values};
use state::aggregate;

/// The tune's four images plus the transient overlays the core maintains.
#[derive(Debug, Clone)]
pub struct TuneImages {
    layout: Arc<TuneLayout>,
    baseline: Tune,
    working: Tune,
    ecu_ram: Option<Tune>,
    ecu_flash: Option<Tune>,
    ecu_unknown: bool,
    overlays: BTreeMap<CellRef, Overlay>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CellRef {
    variable: usize,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    WriteFailed,
    Conflict,
    /// The user chose the working value over the ECU's. The cell is a normal
    /// unsent change until a verified write makes the two agree.
    Superseded,
}

impl TuneImages {
    /// Combines a baseline and a working image.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the images use different
    /// layouts.
    pub fn new(baseline: Tune, working: Tune) -> Result<Self, TuneError> {
        if !baseline.same_layout(&working) {
            return Err(TuneError::LayoutMismatch);
        }
        let layout = Arc::clone(working.layout());
        Ok(Self {
            layout,
            baseline,
            working,
            ecu_ram: None,
            ecu_flash: None,
            ecu_unknown: true,
            overlays: BTreeMap::new(),
        })
    }

    /// Creates images from a freshly loaded tune: baseline == working, no ECU
    /// images yet.
    pub fn from_working(working: Tune) -> Self {
        let baseline = working.snapshot();
        let layout = Arc::clone(working.layout());
        Self {
            layout,
            baseline,
            working,
            ecu_ram: None,
            ecu_flash: None,
            ecu_unknown: true,
            overlays: BTreeMap::new(),
        }
    }

    /// Attaches a confirmed RAM image.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    pub fn with_ecu_ram(mut self, ram: Tune) -> Result<Self, TuneError> {
        self.set_ecu_ram(ram)?;
        Ok(self)
    }

    /// Attaches a confirmed flash image.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    pub fn with_ecu_flash(mut self, flash: Tune) -> Result<Self, TuneError> {
        self.set_ecu_flash(flash)?;
        Ok(self)
    }

    /// The layout all four images share.
    pub fn layout(&self) -> &Arc<TuneLayout> {
        &self.layout
    }

    /// The image last persisted to disk.
    pub fn baseline(&self) -> &Tune {
        &self.baseline
    }

    /// The image the user edits.
    pub fn working(&self) -> &Tune {
        &self.working
    }

    /// The image the user edits.
    pub fn working_mut(&mut self) -> &mut Tune {
        &mut self.working
    }

    /// Last confirmed RAM content, when it has been read.
    pub fn ecu_ram(&self) -> Option<&Tune> {
        self.ecu_ram.as_ref()
    }

    /// Last confirmed burned content, when it has been read.
    pub fn ecu_flash(&self) -> Option<&Tune> {
        self.ecu_flash.as_ref()
    }

    /// Whether the ECU side is currently unknown.
    pub fn is_ecu_unknown(&self) -> bool {
        self.ecu_unknown
    }

    /// Records a confirmed RAM image from a partial observation.
    ///
    /// A complete read goes through [`Self::reconcile_read`], which also
    /// compares and clears failure marks; this setter stores the image only.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the image by value lets a read-back hand it over without a clone"
    )]
    pub fn set_ecu_ram(&mut self, ram: Tune) -> Result<(), TuneError> {
        self.check_layout(&ram)?;
        self.ecu_ram = Some(ram.snapshot());
        self.ecu_unknown = false;
        Ok(())
    }

    /// Records a confirmed flash image (a verified burn or a page read).
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the image by value lets a read-back hand it over without a clone"
    )]
    pub fn set_ecu_flash(&mut self, flash: Tune) -> Result<(), TuneError> {
        self.check_layout(&flash)?;
        self.ecu_flash = Some(flash.snapshot());
        Ok(())
    }

    /// Forgets what flash holds without touching the RAM image.
    ///
    /// A partial burn changes some pages and leaves the rest, so the cached
    /// flash image no longer describes the chip.
    pub fn mark_flash_unknown(&mut self) {
        self.ecu_flash = None;
    }

    /// Drops both ECU images and marks every cell unknown (connection lost).
    ///
    /// A write failure survives: it is the one thing the user must still see
    /// when the link comes back. A decision the user already made about a
    /// differing cell survives too, because it is about the working image.
    pub fn mark_unknown(&mut self) {
        self.ecu_ram = None;
        self.ecu_flash = None;
        self.ecu_unknown = true;
        self.overlays
            .retain(|_, overlay| matches!(overlay, Overlay::WriteFailed | Overlay::Superseded));
    }

    /// Records the RAM image a read produced and reports every difference.
    ///
    /// The first read of a session and a later one behave the same: a cell
    /// where the ECU differs from the working image is a conflict waiting for
    /// the user's decision, and a cell the user already decided to overwrite
    /// keeps that decision. A value the user can edit never moves here, so an
    /// unsaved edit is never lost (ADR-0010). The one exception is a
    /// controller-owned value: the ECU always owns it, so the read replaces
    /// the offline copy.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the image by value lets the read hand it over without a clone"
    )]
    pub fn reconcile_read(&mut self, ram: Tune) -> Result<Vec<Conflict>, TuneError> {
        self.check_layout(&ram)?;
        self.ecu_ram = Some(ram.snapshot());
        self.ecu_unknown = false;
        // A complete read observes every cell, so it is the verified read-back
        // a failed write waits for.
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::WriteFailed);

        // A controller-owned value is the ECU's, not the project's: the
        // reference behavior overwrites the offline copy on every connect.
        for (variable, spec) in self.layout.variables().iter().enumerate() {
            if !spec.is_controller_owned() || spec.ini_page().is_none() {
                continue;
            }
            let Some(values) = ram.values().get(variable) else {
                continue;
            };
            for index in 0..spec.element_count() {
                if let Some(raw) = values.number(index) {
                    let _ = self.working.set_image_raw(&spec.name, index, raw);
                }
            }
        }

        let mut conflicts = Vec::new();
        for (variable, spec) in self.layout.variables().iter().enumerate() {
            if !self.is_sendable(variable) {
                continue;
            }
            for index in 0..spec.element_count() {
                let key = CellRef { variable, index };
                let (working_value, ecu_value) = {
                    let working = &self.working.values()[variable];
                    let ecu = &ram.values()[variable];
                    if Self::same_sent_value(spec, working, ecu, index) {
                        // Equal again: a decision about the difference is moot.
                        if matches!(
                            self.overlays.get(&key),
                            Some(Overlay::Conflict | Overlay::Superseded)
                        ) {
                            self.overlays.remove(&key);
                        }
                        continue;
                    }
                    (
                        cell_value(spec, working, index),
                        cell_value(spec, ecu, index),
                    )
                };

                // A failed write and a decision to keep the working value both
                // stand: the difference is known, and neither asks for a new
                // choice.
                if matches!(
                    self.overlays.get(&key),
                    Some(Overlay::WriteFailed | Overlay::Superseded)
                ) {
                    continue;
                }

                self.overlays.insert(key, Overlay::Conflict);
                let (row, column) = cell_position(spec.shape, index);
                conflicts.push(Conflict {
                    name: spec.name.clone(),
                    row,
                    column,
                    working: working_value,
                    ecu: ecu_value,
                });
            }
        }
        Ok(conflicts)
    }

    /// Marks a cell's last write as failed.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable or cell does not exist.
    pub fn mark_write_failed(
        &mut self,
        name: &str,
        row: usize,
        column: usize,
    ) -> Result<(), TuneError> {
        let (variable, index) = self.cell(name, row, column)?;
        self.overlays
            .insert(CellRef { variable, index }, Overlay::WriteFailed);
        Ok(())
    }

    /// Clears every write-failure overlay (after a verified read-back).
    pub fn clear_write_failures(&mut self) {
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::WriteFailed);
    }

    /// Clears the failure marks on the named variables only.
    ///
    /// A verified write observed the pages it wrote; a failure on a page it
    /// did not touch still stands.
    pub fn clear_write_failures_for(&mut self, names: &[String]) {
        for name in names {
            let Some(variable) = self.layout.index(name) else {
                continue;
            };
            self.overlays.retain(|key, overlay| {
                key.variable != variable || *overlay != Overlay::WriteFailed
            });
        }
    }

    /// Clears a conflict overlay once the user has decided about the cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable or cell does not exist.
    pub fn clear_conflict(
        &mut self,
        name: &str,
        row: usize,
        column: usize,
    ) -> Result<(), TuneError> {
        let (variable, index) = self.cell(name, row, column)?;
        let key = CellRef { variable, index };
        if matches!(
            self.overlays.get(&key),
            Some(Overlay::Conflict | Overlay::Superseded)
        ) {
            self.overlays.remove(&key);
        }
        Ok(())
    }

    /// Records the user's decision to keep the working values.
    ///
    /// Every undecided conflict becomes an unsent change: the cell shows the
    /// working value is not in ECU RAM, and a later read keeps the decision
    /// instead of asking again.
    pub fn keep_working(&mut self) {
        for overlay in self.overlays.values_mut() {
            if *overlay == Overlay::Conflict {
                *overlay = Overlay::Superseded;
            }
        }
    }

    /// Clears every conflict and every decision about one.
    pub fn clear_resolutions(&mut self) {
        self.overlays
            .retain(|_, overlay| !matches!(overlay, Overlay::Conflict | Overlay::Superseded));
    }

    /// Clears the decisions a merge changed the value of.
    ///
    /// A file names nearly every constant, so "the file carried it" is not the
    /// same as "the merge moved it". Only a changed cell loses its decision.
    pub(crate) fn clear_resolutions_changed_since(&mut self, before: &Tune, names: &[String]) {
        for name in names {
            let Some(variable) = self.layout.index(name) else {
                continue;
            };
            if self.layout.is_alias_at(variable) {
                continue;
            }
            let spec = &self.layout.variables()[variable];
            let (Some(before_values), Some(after_values)) =
                (before.value(name), self.working.value(name))
            else {
                continue;
            };
            for index in 0..spec.element_count() {
                if Self::same_sent_value(spec, before_values, after_values, index) {
                    continue;
                }
                self.overlays.remove(&CellRef { variable, index });
            }
        }
    }

    /// Clears the decisions about the named variables only.
    ///
    /// A page-scoped write makes its own variables agree with RAM; a decision
    /// about a cell it did not send still stands.
    pub fn clear_resolutions_for(&mut self, names: &[String]) {
        for name in names {
            let Some(variable) = self.layout.index(name) else {
                continue;
            };
            self.overlays.retain(|key, overlay| {
                key.variable != variable
                    || !matches!(overlay, Overlay::Conflict | Overlay::Superseded)
            });
        }
    }

    /// Every currently marked conflict, in layout order.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        for (variable, spec) in self.layout.variables().iter().enumerate() {
            for index in 0..spec.element_count() {
                if !self.conflict_is_live(variable, index) {
                    continue;
                }
                let working = &self.working.values()[variable];
                let ecu = &self
                    .ecu_ram
                    .as_ref()
                    .map_or(working, |ram| &ram.values()[variable]);
                let (row, column) = cell_position(spec.shape, index);
                conflicts.push(Conflict {
                    name: spec.name.clone(),
                    row,
                    column,
                    working: cell_value(spec, working, index),
                    ecu: cell_value(spec, ecu, index),
                });
            }
        }
        conflicts
    }

    /// Accepts the RAM image as the new baseline (after a save).
    pub fn commit_baseline(&mut self) {
        // Save writes the variables that changed, and those values become the
        // chosen tune. An untouched difference stays an open question: saving
        // a different variable is not a decision about it.
        let saved = diff_values(&self.layout, self.baseline.values(), self.working.values());
        self.baseline = self.working.snapshot();

        let Some(ram) = self.ecu_ram.clone() else {
            return;
        };
        for name in &saved.changed_variables {
            let Some(variable) = self.layout.index(name) else {
                continue;
            };
            if !self.is_sendable(variable) {
                continue;
            }
            let spec = &self.layout.variables()[variable];
            for index in 0..spec.element_count() {
                let working = &self.working.values()[variable];
                if Self::same_sent_value(spec, working, &ram.values()[variable], index) {
                    continue;
                }
                let key = CellRef { variable, index };
                if !matches!(self.overlays.get(&key), Some(Overlay::WriteFailed)) {
                    self.overlays.insert(key, Overlay::Superseded);
                }
            }
        }
    }

    /// Discards local edits for the named variables, returning each value to
    /// the file baseline.
    ///
    /// This is the one-document discard: a table's cells or a dialog's
    /// constants go back to the saved tune, and every other variable keeps its
    /// edits. Conflict marks for the reverted variables go with their values,
    /// unless a read or write failed: that variable keeps its mark so the
    /// reader can still see what did not move.
    ///
    /// Returns how many values moved; a text variable counts as one. A variable
    /// the layout does not know, or one whose images disagree about its shape,
    /// is skipped — there is nothing safe to copy for it.
    ///
    /// Each move goes through the normal setter, so the edit log records the
    /// discard's timestamp, not the original edit's. That is deliberate: the
    /// log answers when a value last changed, and a revert did change it.
    #[expect(
        clippy::float_cmp,
        reason = "a cell is skipped only when its stored raw value is identical"
    )]
    pub fn revert_variables(&mut self, names: &[impl AsRef<str>]) -> usize {
        let mut reverted = 0;

        for name in names {
            let name = name.as_ref();
            let Some(spec) = self.layout.variable(name) else {
                continue;
            };
            let mut failed = false;
            let mut moved = false;

            if matches!(spec.shape, EntryShape::Text { .. }) {
                let saved = self.baseline.text(name).ok().map(str::to_owned);
                let current = self.working.text(name).ok().map(str::to_owned);
                if let (Some(saved), Some(current)) = (saved, current) {
                    if saved != current {
                        if self.working.set_text(name, &saved).is_ok() {
                            reverted += 1;
                            moved = true;
                        } else {
                            failed = true;
                        }
                    }
                } else {
                    failed = true;
                }
            } else {
                let count = spec
                    .shape
                    .dimensions()
                    .map_or(1, |(columns, rows)| columns.saturating_mul(rows))
                    .max(1);

                for index in 0..count {
                    let (Ok(saved), Ok(current)) = (
                        self.baseline.raw_at(name, index),
                        self.working.raw_at(name, index),
                    ) else {
                        failed = true;
                        continue;
                    };
                    if saved == current {
                        continue;
                    }

                    if self.working.set_raw_at(name, index, saved).is_err() {
                        failed = true;
                        continue;
                    }

                    reverted += 1;
                    moved = true;
                }
            }

            // A conflict mark asks the reader to review a cell. The values are
            // at the file baseline now, so the mark belongs to the discard
            // too, the way a whole-tune revert drops every conflict. A failed
            // write leaves the mark in place: it would otherwise point at a
            // value that did not move.
            if !failed
                && moved
                && let Some(variable) = self.layout.index(name)
            {
                self.overlays.retain(|key, overlay| {
                    key.variable != variable
                        || !matches!(overlay, Overlay::Conflict | Overlay::Superseded)
                });
            }
        }

        reverted
    }

    /// Discards every local edit.
    pub fn revert_to_baseline(&mut self) {
        self.working = self.baseline.snapshot();
        self.clear_resolutions();
    }

    /// Replaces the working image (an import). The caller owns consent policy.
    ///
    /// The imported tune keeps its own layout allocation, so values are adopted
    /// into this project's layout after a name-and-shape check; the session's
    /// `Arc<TuneLayout>` is never replaced under the views holding it.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the imported tune's values do not match this
    /// layout.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller hands the imported tune over; only its values are kept"
    )]
    pub fn import(&mut self, tune: Tune) -> Result<(), TuneError> {
        self.working.adopt_values(&tune)?;
        self.clear_resolutions();
        self.clear_write_failures();
        Ok(())
    }

    /// Merges one page whose text field overlaps a number.
    ///
    /// Such a page cannot be decoded back without reading the overlap as text,
    /// so the values the file carried are adopted as they are. A view whose
    /// owner the file did not carry still moves the owner's bytes, because the
    /// owner's value in the file's tune is only the definition default.
    fn merge_overlapping_page(
        &self,
        merged: &mut Tune,
        source: &mut Option<Tune>,
        page: IniPage,
        tune: &Tune,
        applied: &[String],
        resolved: &mut Vec<String>,
    ) -> Result<(), TuneError> {
        let names = self.adopt_carried(merged, source, page, tune, applied, resolved)?;

        // A view the file carried without its owner moves the owner's bytes.
        let views: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| self.layout.alias_owner(name).is_some())
            .collect();
        if views.is_empty() {
            return Ok(());
        }

        let base = encode_page(&self.layout, page, merged)?;
        let patched = patch_page(&self.layout, page, &base, tune, &views)?;
        let mut decoded = merged.snapshot();
        decode_page(&self.layout, page, &patched, &mut decoded)?;

        let mut moved = views.clone();
        for name in &views {
            if let Some(owner) = self.layout.alias_owner(name)
                && !moved.contains(&owner)
            {
                moved.push(owner);
            }
        }
        merged.adopt_named(&decoded, &moved)?;

        Ok(())
    }

    /// Adopts the values a file carried on a page the codec cannot rebuild.
    ///
    /// The page keeps the file's values as written: encoding it would let
    /// declaration order pick a winner for the shared bytes. A view the file
    /// did not carry still follows the bytes of the variable that owns them.
    fn adopt_ambiguous_page(
        &self,
        merged: &mut Tune,
        source: &mut Option<Tune>,
        page: IniPage,
        tune: &Tune,
        applied: &[String],
        resolved: &mut Vec<String>,
    ) -> Result<(), TuneError> {
        self.adopt_carried(merged, source, page, tune, applied, resolved)?;

        self.layout.derive_views(page, merged, applied)
    }

    /// Adopts the names the file carried on `page`, views included.
    ///
    /// Returns the names `page_patch_names` kept, which the text-overlap view
    /// patch needs. A page whose values are only adopted ignores them.
    fn adopt_carried<'a>(
        &self,
        merged: &mut Tune,
        source: &mut Option<Tune>,
        page: IniPage,
        tune: &Tune,
        applied: &'a [String],
        resolved: &mut Vec<String>,
    ) -> Result<Vec<&'a str>, TuneError> {
        let names = self.layout.page_patch_names(applied, page);

        let mut moved: Vec<&str> = names.clone();
        // A view the file carried beside its owner: `page_patch_names` dropped
        // it, so the file's own value is kept here. The text-overlap caller
        // stops there; the ambiguous caller lets `derive_views` re-derive the
        // view when the file carried the owner too.
        for name in applied {
            let Some(spec) = self.layout.variable(name) else {
                continue;
            };
            if spec.ini_page() == Some(page)
                && self.layout.alias_owner(name).is_some()
                && !moved.contains(&spec.name.as_str())
            {
                moved.push(spec.name.as_str());
            }
        }
        if moved.is_empty() {
            return Ok(names);
        }

        resolved.extend(names.iter().map(|name| (*name).to_owned()));

        let source = match source {
            Some(source) => source,
            None => source.insert({
                let mut copy = merged.snapshot();
                copy.adopt_values(tune)?;
                copy
            }),
        };
        merged.adopt_named(source, &moved)?;

        Ok(names)
    }

    /// Merges the variables a tune file carried onto the working image.
    ///
    /// A file is not a whole image: it writes the constants it knows, and it
    /// leaves derived ones out. The values it carries are patched into each
    /// page's bytes and the page is decoded back, so constants that share
    /// bytes end up agreeing. A constant the file omits keeps its current
    /// value, which for a derived bit alias is the one the ECU gave it.
    ///
    /// # Errors
    ///
    /// Returns a [`TuneError`] when a page cannot be encoded, patched or
    /// decoded, or when a value's shape does not fit the layout. A file that
    /// states two values the bytes they share cannot both carry is refused
    /// with [`TuneError::ConflictingValues`] before anything is merged.
    pub fn import_applied(&mut self, tune: &Tune, applied: &[String]) -> Result<(), TuneError> {
        // The merge lands in a copy: a page that fails halfway must leave the
        // working image exactly as it was, like the whole-image import does.
        let before = self.working.snapshot();
        let mut merged = self.working.snapshot();
        // Only the variables this file actually moved lose their decision; a
        // difference it never touched is still the user's to resolve.
        let mut resolved: Vec<String> = Vec::new();
        let pages: Vec<IniPage> = self.layout.pages().iter().map(|page| page.page).collect();
        // The file's tune has its own layout allocation, and a value copy needs
        // the destination's layout. This copy holds the file's values under the
        // image's layout; the file does not change while pages are merged.
        let mut source: Option<Tune> = None;

        // A file is judged on what it states, not on what the codec settles
        // afterwards. Two values it carries over the same bytes can only both
        // stand when they agree, and a merge would otherwise pick a winner by
        // the order the file lists them in, so the file is refused instead.
        self.reject_conflicting_values(tune, applied)?;

        for &page in &pages {
            // A page whose numeric constants share bytes with no owner keeps
            // the file's values as written, and its views follow their owners.
            // A file that says nothing about the page leaves it alone, like the
            // regular path below.
            if self.layout.page_has_ambiguous_overlap(page) {
                let mentioned = applied.iter().any(|name| {
                    self.layout
                        .variable(name)
                        .is_some_and(|spec| spec.ini_page() == Some(page))
                });
                if !mentioned {
                    continue;
                }

                self.adopt_ambiguous_page(
                    &mut merged,
                    &mut source,
                    page,
                    tune,
                    applied,
                    &mut resolved,
                )?;

                continue;
            }

            // A page that overlaps text with numbers cannot be decoded back
            // without reading the overlap as text. The values the file carried
            // are adopted as they stand, so nothing the file wrote is dropped.
            if self.layout.page_has_text_overlap(page) {
                self.merge_overlapping_page(
                    &mut merged,
                    &mut source,
                    page,
                    tune,
                    applied,
                    &mut resolved,
                )?;

                continue;
            }

            let names = self.layout.page_patch_names(applied, page);
            if names.is_empty() {
                continue;
            }

            let base = encode_page(&self.layout, page, &merged)?;
            let patched = patch_page(&self.layout, page, &base, tune, &names)?;

            // Decode into a scratch that already holds the image: a value the
            // file did not mention, or one the codec refuses, keeps its value
            // instead of falling to zero.
            let mut decoded = merged.snapshot();
            decode_page(&self.layout, page, &patched, &mut decoded)?;

            // The values the file carried, the owners whose bytes a view wrote,
            // every view on the page, and every constant that shares bytes with
            // any of those follow the decoded result. Moving one constant moves
            // the bytes its neighbours also claim, and a neighbour left behind
            // would hold a value the merge just replaced.
            let mut moved: Vec<&str> = self.layout.names_sharing_bytes_with(&names);
            for name in &names {
                if let Some(owner) = self.layout.alias_owner(name)
                    && !moved.contains(&owner)
                {
                    moved.push(owner);
                }
            }
            if let Some(page_layout) = self.layout.page(page) {
                for index in &page_layout.variables {
                    let spec = &self.layout.variables()[*index];
                    if self.layout.is_alias_at(*index) && !moved.contains(&spec.name.as_str()) {
                        moved.push(&spec.name);
                    }
                }
            }

            merged.adopt_named(&decoded, &moved)?;
            resolved.extend(moved.iter().map(|name| (*name).to_owned()));
        }

        // Settings with no page are not on the wire; they are copied as they
        // are, and a file that omits them keeps the current value.
        for name in applied {
            let Some(spec) = self.layout.variable(name) else {
                continue;
            };
            if spec.ini_page().is_some() {
                continue;
            }
            let Some(values) = tune.value(&spec.name) else {
                continue;
            };

            match values {
                Values::Numbers(numbers) => {
                    for (index, raw) in numbers.iter().enumerate() {
                        merged.set_image_raw(&spec.name, index, *raw)?;
                    }
                }
                Values::Text(text) => {
                    merged.set_image_text(&spec.name, text)?;
                }
            }
            resolved.push(spec.name.clone());
        }

        self.working.adopt_values(&merged)?;
        // The imported values are new; a decision about a value the merge
        // changed does not carry over. A difference it did not touch still
        // stands, even when the file named the variable.
        self.clear_resolutions_changed_since(&before, &resolved);
        Ok(())
    }

    /// Refuses a file that states two values the shared bytes cannot both carry.
    ///
    /// Only the values the file actually carried are judged, and only against
    /// each other. A constant it left out keeps whatever the image holds, which
    /// the merge reconciles with its neighbour, so the file states nothing
    /// about it. A page the file says nothing about is not its business either.
    fn reject_conflicting_values(&self, tune: &Tune, applied: &[String]) -> Result<(), TuneError> {
        let carried = |name: &str| applied.iter().any(|other| other.eq_ignore_ascii_case(name));

        for page in self.layout.pages() {
            let touched = applied.iter().any(|name| {
                self.layout
                    .variable(name)
                    .is_some_and(|spec| spec.ini_page() == Some(page.page))
            });
            if !touched {
                continue;
            }

            for conflict in self.layout.value_conflicts(page.page, tune) {
                if carried(&conflict.first) && carried(&conflict.second) {
                    return Err(TuneError::ConflictingValues { conflict });
                }
            }
        }

        Ok(())
    }

    /// Derived state of one cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable or cell does not exist.
    pub fn cell_state(
        &self,
        name: &str,
        row: usize,
        column: usize,
    ) -> Result<CellState, TuneError> {
        let (variable, index) = self.cell(name, row, column)?;
        Ok(self.state_at(variable, index))
    }

    /// Derived state of a whole page.
    pub fn page_state(&self, page: IniPage) -> Option<CellState> {
        let page = self.layout.page(page)?;
        Some(self.state_of_page(page))
    }

    /// Derived state of the whole tune.
    pub fn overall_state(&self) -> CellState {
        if self.layout.pages().is_empty() {
            return CellState::NotApplicable;
        }
        let states = self
            .layout
            .pages()
            .iter()
            .map(|page| self.state_of_page(page));
        aggregate(states)
    }

    /// Whether two images store the same value at one cell.
    ///
    /// The write plan compares wire bytes, so detection and the derived state
    /// must too: a raw `f64` a page narrows to `f32` is equal to what the page
    /// decodes even when the two `f64` values differ.
    fn same_sent_value(spec: &VariableSpec, left: &Values, right: &Values, index: usize) -> bool {
        match (left.number(index), right.number(index)) {
            (Some(left), Some(right)) => spec.same_wire_value(left, right),
            _ => left.same_at(right, index),
        }
    }

    /// Whether `index` of `variable` is an undecided difference that still exists.
    fn conflict_is_live(&self, variable: usize, index: usize) -> bool {
        let key = CellRef { variable, index };
        if self.overlays.get(&key) != Some(&Overlay::Conflict) {
            return false;
        }
        let (Some(spec), Some(working), Some(ecu)) = (
            self.layout.variables().get(variable),
            self.working.values().get(variable),
            self.ecu_ram
                .as_ref()
                .and_then(|ram| ram.values().get(variable)),
        ) else {
            return false;
        };

        !Self::same_sent_value(spec, working, ecu, index)
    }

    /// The status bar's summary.
    pub fn status(&self) -> TuneStatus {
        let unsaved = diff_values(&self.layout, self.baseline.values(), self.working.values());
        let pending_write = match &self.ecu_ram {
            Some(ram) => diff_writable_values(&self.layout, ram.values(), self.working.values()),
            None => unsaved.clone(),
        };
        let pending_burn = match (&self.ecu_ram, &self.ecu_flash) {
            (Some(ram), Some(flash)) => {
                diff_writable_values(&self.layout, ram.values(), flash.values())
            }
            _ => TuneDiff::default(),
        };
        let conflicts = self
            .overlays
            .keys()
            .filter(|key| self.conflict_is_live(key.variable, key.index))
            .count();

        TuneStatus {
            state: self.overall_state(),
            unsaved,
            pending_write,
            pending_burn,
            conflicts,
            flash_known: self.ecu_flash.is_some(),
        }
    }

    /// Whether the write plan could carry this variable's values on its own.
    ///
    /// Conflict detection covers what a write could carry by itself: PC-side
    /// values, controller-owned values, a value with no page address, a view, a
    /// variable in an ambiguous pair, and a bit field the codec refuses are
    /// never sent, so a difference in one is not something the user can decide
    /// to overwrite. A safe variable beside an ambiguous pair still takes part
    /// in a decision; refusing that page is the write path's job.
    fn is_sendable(&self, variable: usize) -> bool {
        let Some(spec) = self.layout.variables().get(variable) else {
            return false;
        };

        spec.owner != Owner::Pc
            && !spec.is_controller_owned()
            && !spec.no_msq_save
            && spec.address.is_some()
            && spec.scale.is_some()
            && spec.translate.is_some()
            && !self.layout.is_alias_at(variable)
            && !self.layout.is_ambiguous_at(variable)
            && spec.bit_field_round_trips()
    }

    fn check_layout(&self, tune: &Tune) -> Result<(), TuneError> {
        if Arc::ptr_eq(&self.layout, tune.layout()) {
            Ok(())
        } else {
            Err(TuneError::LayoutMismatch)
        }
    }

    fn cell(&self, name: &str, row: usize, column: usize) -> Result<(usize, usize), TuneError> {
        let variable = self
            .layout
            .index(name)
            .ok_or_else(|| TuneError::UnknownVariable(name.to_owned()))?;
        let index = self.layout.variables()[variable]
            .shape
            .offset(row, column)
            .ok_or_else(|| TuneError::CellOutOfRange {
                name: name.to_owned(),
                row,
                column,
            })?;
        Ok((variable, index))
    }

    pub(super) fn state_at(&self, variable: usize, index: usize) -> CellState {
        let Some(spec) = self.layout.variables().get(variable) else {
            return CellState::Unknown;
        };
        if spec.owner == Owner::Pc {
            return CellState::NotApplicable;
        }

        // A view has no state of its own: the element its bytes carry is the
        // cell the user must look at.
        if let Some(owner) = self.layout.alias_owner_at(variable) {
            let Some(owner_spec) = self.layout.variables().get(owner) else {
                return CellState::Unknown;
            };
            let width = usize::try_from(owner_spec.data_type.size()).unwrap_or(0);
            let offset = spec
                .address
                .zip(owner_spec.address)
                .and_then(|(alias, start)| alias.checked_sub(start));

            return match offset.and_then(|offset| offset.checked_div(u32::try_from(width).ok()?)) {
                Some(mapped) => self.state_at(owner, usize::try_from(mapped).unwrap_or(0)),
                None => CellState::Unknown,
            };
        }

        // A value the write plan cannot send has no sync state to show.
        if spec.is_controller_owned()
            || spec.no_msq_save
            || spec.address.is_none()
            || spec.scale.is_none()
            || spec.translate.is_none()
            || !spec.bit_field_round_trips()
        {
            return CellState::NotApplicable;
        }

        if self.overlays.get(&CellRef { variable, index }) == Some(&Overlay::WriteFailed) {
            return CellState::WriteFailed;
        }
        if self.ecu_unknown {
            return CellState::Unknown;
        }

        let working = &self.working.values()[variable];
        let Some(ram) = &self.ecu_ram else {
            return CellState::Unknown;
        };
        let ram = &ram.values()[variable];
        let equal = Self::same_sent_value(spec, working, ram, index);

        if self.overlays.get(&CellRef { variable, index }) == Some(&Overlay::Conflict) && !equal {
            return CellState::Conflict;
        }

        if equal {
            let burned = self.ecu_flash.as_ref().is_some_and(|flash| {
                Self::same_sent_value(spec, working, &flash.values()[variable], index)
            });
            return if burned {
                CellState::Synced
            } else {
                CellState::InRam
            };
        }

        // The working value is not in ECU RAM. Whether it was edited just now
        // or saved earlier does not matter: it is an unsent change, and only
        // the read that found an undecided difference marks a conflict.
        CellState::LocalDirty
    }

    pub(super) fn state_of_page(&self, page: &crate::PageLayout) -> CellState {
        let states = page.variables.iter().flat_map(|variable| {
            let spec = &self.layout.variables()[*variable];
            (0..spec.element_count()).map(move |index| self.state_at(*variable, index))
        });
        aggregate(states)
    }
}

#[cfg(test)]
mod tests;
