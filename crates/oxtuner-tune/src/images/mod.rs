use std::collections::BTreeMap;
use std::sync::Arc;

use crate::diff::diff_values;
use crate::{IniPage, Layout, Owner, Tune, TuneDiff, TuneError};

mod conflict;
mod state;

pub use conflict::{CellValue, Conflict, conflict_names};
pub use state::{CellState, PageState, StateTable, TuneStatus};

use conflict::{cell_position, cell_value, diff_writable_values};
use state::aggregate;

/// The tune's four images plus the transient overlays the core maintains.
#[derive(Debug, Clone)]
pub struct TuneImages {
    layout: Arc<Layout>,
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
    pub fn layout(&self) -> &Arc<Layout> {
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

    /// Records a confirmed RAM image (verified write or a page read).
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
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::WriteFailed);
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

    /// Drops both ECU images and marks every cell unknown (connection lost).
    ///
    /// A write failure survives: it is the one thing the user must still see
    /// when the link comes back.
    pub fn mark_unknown(&mut self) {
        self.ecu_ram = None;
        self.ecu_flash = None;
        self.ecu_unknown = true;
        self.overlays
            .retain(|_, overlay| *overlay == Overlay::WriteFailed);
    }

    /// Replaces the RAM image after a reconnect read and reports conflicts.
    ///
    /// Every cell where the ECU differs from the working image is marked, for
    /// the conflict dialog (ADR-0010: "on reconnect, compare the ECU with
    /// `working` → conflict dialog; no automatic write").
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the image uses another
    /// layout.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the image by value lets the reconnect read hand it over without a clone"
    )]
    pub fn reconnect(&mut self, ram: Tune) -> Result<Vec<Conflict>, TuneError> {
        self.check_layout(&ram)?;
        self.ecu_ram = Some(ram.snapshot());
        self.ecu_unknown = false;
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::WriteFailed);

        let mut conflicts = Vec::new();
        for (variable, spec) in self.layout.variables().iter().enumerate() {
            if spec.owner == Owner::Pc {
                continue;
            }
            for index in 0..spec.element_count() {
                let (working_value, ecu_value) = {
                    let working = &self.working.values()[variable];
                    let ecu = &ram.values()[variable];
                    if working.same_at(ecu, index) {
                        continue;
                    }
                    (
                        cell_value(spec, working, index),
                        cell_value(spec, ecu, index),
                    )
                };

                self.overlays
                    .insert(CellRef { variable, index }, Overlay::Conflict);
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
        if self.overlays.get(&key) == Some(&Overlay::Conflict) {
            self.overlays.remove(&key);
        }
        Ok(())
    }

    /// Every currently marked conflict, in layout order.
    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut conflicts = Vec::new();
        for (variable, spec) in self.layout.variables().iter().enumerate() {
            for index in 0..spec.element_count() {
                let key = CellRef { variable, index };
                if self.overlays.get(&key) != Some(&Overlay::Conflict) {
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
        self.baseline = self.working.snapshot();
    }

    /// Discards local edits for the named variables, returning each value to
    /// the file baseline.
    ///
    /// This is the one-document discard: a table's cells or a dialog's
    /// constants go back to the saved tune, and every other variable keeps its
    /// edits. Conflict marks for the reverted variables go with their values.
    /// Returns how many cells moved. A variable the layout does not know, or
    /// one whose images disagree about its shape, is skipped — there is nothing
    /// safe to copy for it.
    #[expect(
        clippy::float_cmp,
        reason = "a cell is skipped only when its stored raw value is identical"
    )]
    pub fn revert_variables(&mut self, names: &[String]) -> usize {
        let mut reverted = 0;

        for name in names {
            let Some(spec) = self.layout.variable(name) else {
                continue;
            };
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
                    continue;
                };
                if saved == current {
                    continue;
                }

                if self.working.set_raw_at(name, index, saved).is_err() {
                    continue;
                }

                reverted += 1;
            }

            // A conflict mark asks the reader to review a cell. The values are
            // at the file baseline now, so the mark belongs to the discard
            // too, the way a whole-tune revert drops every conflict.
            if let Some(variable) = self.layout.index(name) {
                self.overlays.retain(|key, overlay| {
                    key.variable != variable || *overlay != Overlay::Conflict
                });
            }
        }

        reverted
    }

    /// Discards every local edit.
    pub fn revert_to_baseline(&mut self) {
        self.working = self.baseline.snapshot();
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::Conflict);
    }

    /// Replaces the working image (an import). The caller owns consent policy.
    ///
    /// The imported tune keeps its own layout allocation, so values are adopted
    /// into this project's layout after a name-and-shape check; the session's
    /// `Arc<Layout>` is never replaced under the views holding it.
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
        self.overlays
            .retain(|_, overlay| *overlay != Overlay::Conflict);
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
            .values()
            .filter(|overlay| **overlay == Overlay::Conflict)
            .count();

        TuneStatus {
            state: self.overall_state(),
            unsaved,
            pending_write,
            pending_burn,
            conflicts,
        }
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

        match self.overlays.get(&CellRef { variable, index }) {
            Some(Overlay::WriteFailed) => return CellState::WriteFailed,
            Some(Overlay::Conflict) => return CellState::Conflict,
            None => {}
        }
        if self.ecu_unknown {
            return CellState::Unknown;
        }

        let working = &self.working.values()[variable];
        let baseline = &self.baseline.values()[variable];
        let Some(ram) = &self.ecu_ram else {
            return CellState::Unknown;
        };
        let ram = &ram.values()[variable];

        if working.same_at(ram, index) {
            let burned = self
                .ecu_flash
                .as_ref()
                .is_some_and(|flash| working.same_at(&flash.values()[variable], index));
            return if burned {
                CellState::Synced
            } else {
                CellState::InRam
            };
        }

        if working.same_at(baseline, index) {
            CellState::Conflict
        } else {
            CellState::LocalDirty
        }
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
