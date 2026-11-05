//! The four tune images and the state derived from comparing them (ADR-0010).
//!
//! ```text
//! baseline ── as last persisted (.msq)        ┐
//! working  ── the user's edits               ├─ this crate (file side)
//! ecu.ram  ── last confirmed RAM content     │
//! ecu.flash── last confirmed burned content  ┘
//! ```
//!
//! The ECU-side images are held here as values; only the core may update them,
//! and only from a verified read-back or burn (ADR-0010, ADR-0011). This module
//! never writes anything: it compares images and reports a [`CellState`].
//!
//! # Truth table
//!
//! For one cell, with `w` the working value, `b` the baseline, `r` RAM and `f`
//! flash:
//!
//! | Condition                                  | State         |
//! | ------------------------------------------ | ------------- |
//! | a `WriteFailed` overlay is set             | `WriteFailed` |
//! | a `Conflict` overlay is set                | `Conflict`    |
//! | the ECU side is unknown (lost/never read)  | `Unknown`     |
//! | `w == r` and (`f` unknown or `w == f`)     | `Synced`*     |
//! | `w == r` but `f` differs                   | `InRam`       |
//! | `w != r` and `w != b`                      | `LocalDirty`  |
//! | `w != r` and `w == b`                      | `Conflict`    |
//!
//! \* `f` unknown cannot prove a burn, so `InRam` is reported instead of
//! `Synced` whenever flash was never read. The table's `Synced` row therefore
//! reads "flash known and equal" in practice; a missing flash image falls into
//! the `InRam` row.
//!
//! The last row is the dry-run case that keeps the model honest: the working
//! image equals the file but the ECU's confirmed RAM does not — either the ECU
//! drifted or a write was reverted locally. Either way a write would change the
//! ECU, so the cell must be reviewed, not silently written.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::diff::diff_values;
use crate::{IniPage, Layout, Owner, Tune, TuneDiff, TuneError, Values};

/// The derived state of one cell, one page or the whole tune.
#[expect(
    clippy::exhaustive_enums,
    reason = "the sync state machine has a closed set of outcomes"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CellState {
    /// Working, RAM and flash agree (or the cell was never connected and the
    /// caller treats unknown as synced — the model never does).
    Synced,
    /// The working image differs from the baseline and from RAM: not sent.
    LocalDirty,
    /// The working value is in RAM but not (confirmed) in flash.
    InRam,
    /// The last write to this cell failed. Sticky until a verified read-back.
    WriteFailed,
    /// The ECU side is not known: never read, or the connection was lost.
    Unknown,
    /// The working value equals the file but the ECU holds something else, or a
    /// reconnect found the ECU changed. Needs a decision, never an auto-write.
    Conflict,
    /// A PC-side variable: stored only in the tune file, never sent.
    NotApplicable,
}

impl CellState {
    /// A stable label for logs and UI keys.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "synced",
            Self::LocalDirty => "local-dirty",
            Self::InRam => "in-ram",
            Self::WriteFailed => "write-failed",
            Self::Unknown => "unknown",
            Self::Conflict => "conflict",
            Self::NotApplicable => "not-applicable",
        }
    }
}

impl fmt::Display for CellState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One cell's value, for a conflict report.
#[expect(
    clippy::exhaustive_enums,
    reason = "a cell value is a user number, a raw number or text"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// A numeric value in user units.
    Number(f64),
    /// A numeric value in raw units, because the definition's `scale` or
    /// `translate` is an expression this build could not resolve.
    Raw(f64),
    /// A text value.
    Text(String),
}

/// A cell the ECU and the working image disagree about.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    /// Variable name.
    pub name: String,
    /// Table row (`0` for scalars).
    pub row: usize,
    /// Table column (`0` for scalars).
    pub column: usize,
    /// The value the user is working with.
    pub working: CellValue,
    /// The value read back from the ECU.
    pub ecu: CellValue,
}

/// Aggregate state of one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageState {
    /// Page number as written in the INI.
    pub page: IniPage,
    /// The most severe state of any cell on the page.
    pub state: CellState,
    /// Number of cells on the page.
    pub cells: usize,
    /// Number of cells that differ from the baseline.
    pub changed: usize,
}

/// A status bar's worth of information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuneStatus {
    /// Overall state.
    pub state: CellState,
    /// Working vs. the `.msq` baseline.
    pub unsaved: TuneDiff,
    /// Working vs. confirmed RAM.
    pub pending_write: TuneDiff,
    /// Confirmed RAM vs. confirmed flash.
    pub pending_burn: TuneDiff,
    /// Cells marked as conflicts.
    pub conflicts: usize,
}

impl fmt::Display for TuneStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.state)?;
        if !self.unsaved.is_empty() {
            write!(formatter, " · {} unsaved", self.unsaved.cells)?;
        }
        if !self.pending_write.is_empty() {
            write!(formatter, " · {} not sent", self.pending_write.cells)?;
        }
        if !self.pending_burn.is_empty() {
            write!(formatter, " · {} not burned", self.pending_burn.cells)?;
        }
        if self.conflicts > 0 {
            write!(formatter, " · {} conflicts", self.conflicts)?;
        }
        Ok(())
    }
}

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

    /// Replaces the working image (an import). The caller owns consent policy
    /// ([`crate::WriteIntent::needs_consent`]).
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

    fn state_at(&self, variable: usize, index: usize) -> CellState {
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

    fn state_of_page(&self, page: &crate::PageLayout) -> CellState {
        let states = page.variables.iter().flat_map(|variable| {
            let spec = &self.layout.variables()[*variable];
            (0..spec.element_count()).map(move |index| self.state_at(*variable, index))
        });
        aggregate(states)
    }
}

/// The most severe state wins: a page containing one failed write is not
/// "in RAM", even if every other cell is.
fn aggregate(states: impl Iterator<Item = CellState>) -> CellState {
    let mut result = CellState::NotApplicable;
    for state in states {
        let rank = |state: CellState| match state {
            CellState::WriteFailed => 6,
            CellState::Conflict => 5,
            CellState::Unknown => 4,
            CellState::LocalDirty => 3,
            CellState::InRam => 2,
            CellState::Synced => 1,
            CellState::NotApplicable => 0,
        };
        if rank(state) > rank(result) {
            result = state;
        }
    }
    result
}

/// The state of every cell, computed once.
///
/// The table editor renders 32×32 cells per frame; deriving each string
/// comparison per frame would be wasteful. [`TuneImages::cell_state`] is
/// `O(1)` already, but this makes the grid's inner loop a vector index.
#[derive(Debug, Clone)]
pub struct StateTable {
    layout: Arc<Layout>,
    cells: Vec<Vec<CellState>>,
    pages: Vec<PageState>,
    overall: CellState,
}

impl StateTable {
    /// Derives the whole table from the images.
    pub fn build(images: &TuneImages) -> Self {
        let layout = Arc::clone(&images.layout);
        let cells: Vec<Vec<CellState>> = layout
            .variables()
            .iter()
            .enumerate()
            .map(|(variable, spec)| {
                (0..spec.element_count())
                    .map(|index| images.state_at(variable, index))
                    .collect()
            })
            .collect();

        let pages = {
            let diff = diff_values(&layout, images.baseline.values(), images.working.values());
            layout
                .pages()
                .iter()
                .map(|page| PageState {
                    page: page.page,
                    state: images.state_of_page(page),
                    cells: page.cell_count(&layout),
                    changed: diff.pages.get(&page.page).copied().unwrap_or(0),
                })
                .collect::<Vec<_>>()
        };
        let overall = if pages.is_empty() {
            CellState::NotApplicable
        } else {
            aggregate(pages.iter().map(|page| page.state))
        };

        Self {
            layout,
            cells,
            pages,
            overall,
        }
    }

    /// The state of one cell.
    /// Derived state of one cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable or cell does not exist.
    pub fn cell(&self, name: &str, row: usize, column: usize) -> Result<CellState, TuneError> {
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
        Ok(self.cells[variable][index])
    }

    /// The state of one page.
    pub fn page(&self, page: IniPage) -> Option<&PageState> {
        self.pages.iter().find(|state| state.page == page)
    }

    /// Every page, in page order.
    pub fn pages(&self) -> &[PageState] {
        &self.pages
    }

    /// The overall state.
    pub fn overall(&self) -> CellState {
        self.overall
    }

    /// Cells in a given state, for the status tooltip.
    pub fn count(&self, state: CellState) -> usize {
        self.cells
            .iter()
            .flat_map(|cells| cells.iter())
            .filter(|cell| **cell == state)
            .count()
    }
}

fn cell_value(spec: &crate::VariableSpec, values: &Values, index: usize) -> CellValue {
    match values {
        Values::Numbers(numbers) => {
            let raw = numbers.get(index).copied().unwrap_or(0.0);
            match spec.to_user(raw) {
                Ok(user) => CellValue::Number(user),
                Err(_) => CellValue::Raw(raw),
            }
        }
        Values::Text(text) => CellValue::Text(text.clone()),
    }
}

/// The `(row, column)` a flat index addresses.
fn cell_position(shape: crate::EntryShape, index: usize) -> (usize, usize) {
    match shape {
        crate::EntryShape::Array { cols, .. } if cols > 0 => (index / cols, index % cols),
        _ => (0, 0),
    }
}

/// Names that have any conflict overlay. Kept for callers that only need the
/// set (the save path and the dialog both do).
pub fn conflict_names(images: &TuneImages) -> BTreeSet<String> {
    images
        .conflicts()
        .into_iter()
        .map(|conflict| conflict.name)
        .collect()
}

fn diff_writable_values(layout: &Layout, left: &[Values], right: &[Values]) -> TuneDiff {
    let mut diff = TuneDiff::default();
    for (index, variable) in layout.variables().iter().enumerate() {
        if variable.is_controller_owned() || variable.address.is_none() {
            continue;
        }
        let (Some(left), Some(right)) = (left.get(index), right.get(index)) else {
            continue;
        };
        let changed = left.changed_cells(right);
        if changed == 0 {
            continue;
        }
        diff.changed_variables.push(variable.name.clone());
        diff.cells += changed;
        if let Some(page) = variable.ini_page() {
            *diff.pages.entry(page).or_default() += changed;
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::float_cmp,
        reason = "unit tests may unwrap and compare model values exactly"
    )]
    use std::sync::OnceLock;

    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
nPages = 1
pageSize = 288
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
b = scalar, U08, 1, "", 1, 0, 0, 255, 0

[PcVariables]
pc = scalar, U08, 1, "", 1, 0, 0, 255, 0
"#;

    fn tune() -> Tune {
        // Every tune in these tests must share one `Arc<Layout>`: the images
        // reject anything else, exactly as production does when the ECU image
        // is read through the same definition as the working image.
        static LAYOUT: OnceLock<Arc<Layout>> = OnceLock::new();
        let layout = LAYOUT.get_or_init(|| {
            let definition =
                parse_str(DEFINITION, &SettingsContext::new()).expect("definition parses");
            Arc::new(Layout::build(&definition, &|_| None))
        });
        Tune::new(Arc::clone(layout))
    }

    fn images() -> TuneImages {
        TuneImages::from_working(tune())
    }

    #[test]
    fn without_an_ecu_read_everything_is_unknown() {
        let images = images();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Unknown);
        assert_eq!(
            images.cell_state("pc", 0, 0).unwrap(),
            CellState::NotApplicable
        );
        assert_eq!(images.overall_state(), CellState::Unknown);
    }

    #[test]
    fn a_confirmed_equal_ram_and_flash_is_synced() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(images.status().state, CellState::Synced);
    }

    #[test]
    fn a_local_edit_is_local_dirty_until_written_to_ram() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 42.0).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::LocalDirty);
        assert_eq!(images.status().unsaved.cells, 1);
        assert_eq!(images.status().pending_write.cells, 1);
    }

    #[test]
    fn a_verified_write_is_in_ram_and_burn_clears_it() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 42.0).unwrap();

        let mut ram = tune();
        ram.set_user("a", 42.0).unwrap();
        images.set_ecu_ram(ram.clone()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
        assert_eq!(images.status().pending_write.cells, 0);
        assert_eq!(images.status().pending_burn.cells, 1);

        images.set_ecu_flash(ram).unwrap();
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Synced);
    }

    #[test]
    fn the_truth_table_is_exhaustive_over_image_equalities() {
        // One cell, four images, each either 0 or 1. `ecu_unknown` is covered
        // by the "no RAM read" case, and both overlays are tested separately.
        for working in [0.0, 1.0] {
            for baseline in [0.0, 1.0] {
                for ram in [0.0, 1.0] {
                    for flash in [0.0, 1.0] {
                        let mut base = tune();
                        base.set_raw_at("a", 0, baseline).unwrap();

                        let mut work = tune();
                        work.set_raw_at("a", 0, working).unwrap();

                        let mut ecu = tune();
                        ecu.set_raw_at("a", 0, ram).unwrap();

                        let mut burned = tune();
                        burned.set_raw_at("a", 0, flash).unwrap();

                        let images = TuneImages::new(base, work)
                            .unwrap()
                            .with_ecu_ram(ecu)
                            .unwrap()
                            .with_ecu_flash(burned)
                            .unwrap();

                        let expected = if working == ram {
                            if working == flash {
                                CellState::Synced
                            } else {
                                CellState::InRam
                            }
                        } else if working == baseline {
                            CellState::Conflict
                        } else {
                            CellState::LocalDirty
                        };

                        assert_eq!(
                            images.cell_state("a", 0, 0).unwrap(),
                            expected,
                            "w={working} b={baseline} r={ram} f={flash}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_flash_turns_a_match_into_in_ram_not_synced() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();

        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::InRam);
    }

    #[test]
    fn a_failed_write_is_sticky_and_more_severe_than_unknown() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.mark_write_failed("a", 0, 0).unwrap();
        images.mark_unknown();

        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::WriteFailed
        );
        assert_eq!(images.overall_state(), CellState::WriteFailed);
    }

    #[test]
    fn ecu_drift_without_a_local_edit_is_a_conflict() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();

        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        let conflicts = images.reconnect(ram).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "a");
        assert_eq!(conflicts[0].working, CellValue::Number(0.0));
        assert_eq!(conflicts[0].ecu, CellValue::Number(9.0));
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);

        images.clear_conflict("a", 0, 0).unwrap();
        assert_eq!(
            images.cell_state("a", 0, 0).unwrap(),
            CellState::Conflict,
            "the underlying images still differ; only an overlay was cleared"
        );
    }

    #[test]
    fn reverting_one_variable_leaves_the_others_edited() {
        let mut images = images();
        images.working_mut().set_user("a", 3.0).unwrap();
        images.working_mut().set_user("b", 4.0).unwrap();

        let reverted = images.revert_variables(&["a".to_owned()]);

        assert_eq!(reverted, 1);
        assert_eq!(images.working().scalar_raw("a").unwrap(), 0.0);
        assert_ne!(
            images.working().scalar_raw("b").unwrap(),
            0.0,
            "the variable the caller did not name keeps its edit"
        );
    }

    #[test]
    fn reverting_one_variable_clears_its_conflict_mark() {
        let mut images = images();
        let mut ram = tune();
        ram.set_user("a", 9.0).unwrap();
        images.reconnect(ram).unwrap();
        images.working_mut().set_user("a", 5.0).unwrap();

        images.revert_variables(&["a".to_owned()]);

        assert!(
            images.conflicts().is_empty(),
            "the reverted variable carries no conflict mark"
        );
        // The cell still reads Conflict: the ECU's value really does differ
        // from the file. That is the value-based state, not a stale overlay.
        assert_eq!(images.cell_state("a", 0, 0).unwrap(), CellState::Conflict);
    }

    #[test]
    fn reverting_restores_the_baseline_and_clears_conflicts() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.working_mut().set_user("a", 3.0).unwrap();
        images.working_mut().set_user("b", 4.0).unwrap();

        images.revert_to_baseline();

        assert_eq!(images.working().scalar_raw("a").unwrap(), 0.0);
        assert_eq!(images.status().unsaved.cells, 0);
    }

    #[test]
    fn committing_the_baseline_marks_the_tune_saved() {
        let mut images = images();
        images.working_mut().set_user("a", 1.0).unwrap();
        assert_eq!(images.status().unsaved.cells, 1);

        images.commit_baseline();

        assert_eq!(images.status().unsaved.cells, 0);
    }

    #[test]
    fn the_state_table_matches_cell_lookups() {
        let mut images = images();
        images.set_ecu_ram(tune()).unwrap();
        images.set_ecu_flash(tune()).unwrap();
        images.working_mut().set_user("a", 7.0).unwrap();

        let table = StateTable::build(&images);

        assert_eq!(table.cell("a", 0, 0).unwrap(), CellState::LocalDirty);
        assert_eq!(table.cell("b", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(table.cell("pc", 0, 0).unwrap(), CellState::NotApplicable);
        assert_eq!(table.count(CellState::LocalDirty), 1);
        assert_eq!(table.overall(), CellState::LocalDirty);
        assert_eq!(
            table.page(IniPage::FIRST).unwrap().state,
            CellState::LocalDirty
        );
        assert_eq!(table.page(IniPage::FIRST).unwrap().changed, 1);
        assert_eq!(table.page(IniPage::new(9).unwrap()), None);
    }

    const SCALED_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
note = string, ASCII, 8
"#;

    fn scaled_images() -> TuneImages {
        let definition =
            parse_str(SCALED_DEFINITION, &SettingsContext::new()).expect("definition parses");
        let layout = Arc::new(Layout::build(&definition, &|_| None));
        TuneImages::from_working(Tune::new(layout))
    }

    #[test]
    fn conflict_values_are_reported_in_user_units() {
        let mut images = scaled_images();
        let mut ram = Tune::new(Arc::clone(images.layout()));
        ram.set_user("reqFuel", 12.3).unwrap();

        let conflicts = images.reconnect(ram).unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].name, "reqFuel");
        assert_eq!(conflicts[0].working, CellValue::Number(0.0));
        let CellValue::Number(ecu) = conflicts[0].ecu else {
            panic!("the ECU value should be a user-unit number");
        };
        assert!((ecu - 12.3).abs() < 1e-9, "got {ecu}");
    }

    #[test]
    fn a_text_value_is_one_cell_not_its_reserved_length() {
        let mut images = scaled_images();
        let ram = Tune::new(Arc::clone(images.layout()));
        images.set_ecu_ram(ram.clone()).unwrap();
        images.set_ecu_flash(ram).unwrap();

        assert_eq!(images.cell_state("note", 0, 0).unwrap(), CellState::Synced);
        assert_eq!(images.page_state(IniPage::FIRST), Some(CellState::Synced));

        let table = StateTable::build(&images);
        assert_eq!(table.page(IniPage::FIRST).unwrap().cells, 2);
        assert_eq!(table.count(CellState::Synced), 2);

        images.working_mut().set_text("note", "ABC").unwrap();
        let diff = TuneDiff::between(images.baseline(), images.working()).unwrap();
        assert_eq!(diff.cells, 1, "an edited string is one changed cell");
    }

    #[test]
    fn images_from_different_definitions_are_rejected() {
        let other = Tune::from_definition(
            &parse_str(
                "[MegaTune]\nsignature = \"other\"\n[Constants]\nnPages = 1\npageSize = 1\npage = 1\nx = scalar, U08, 0, \"\", 1, 0, 0, 1, 0\n",
                &SettingsContext::new(),
            )
            .expect("parses"),
        );

        assert!(matches!(
            TuneImages::new(tune(), other),
            Err(TuneError::LayoutMismatch)
        ));
    }
}
