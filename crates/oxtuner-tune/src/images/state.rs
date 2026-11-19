use std::fmt;
use std::sync::Arc;

use crate::diff::diff_values;
use crate::{IniPage, TuneDiff, TuneError, TuneLayout};

use super::TuneImages;

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
    /// A PC-side variable, or a value the write plan cannot send: it has no
    /// sync state to show.
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

/// Aggregate state of one page.
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// Whether ECU flash is confirmed in this session.
    ///
    /// A full verified burn is the only confirmation; an empty `pending_burn`
    /// means "nothing to burn" only when this is true.
    pub flash_known: bool,
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

/// The most severe state wins: a page containing one failed write is not
/// "in RAM", even if every other cell is.
pub(super) fn aggregate(states: impl Iterator<Item = CellState>) -> CellState {
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
    layout: Arc<TuneLayout>,
    /// Every cell state, in one buffer with one region per variable. A
    /// definition with hundreds of variables otherwise pays a heap allocation
    /// per variable, and the grid reads the cells in an inner loop.
    cells: Vec<CellState>,
    /// Where each variable's region starts, plus one final end offset.
    offsets: Vec<usize>,
    pages: Vec<PageState>,
    overall: CellState,
}

impl StateTable {
    /// Derives the whole table from the images.
    pub fn build(images: &TuneImages) -> Self {
        let layout = Arc::clone(&images.layout);
        let variables = layout.variables();

        let mut offsets = Vec::with_capacity(variables.len() + 1);
        let mut total = 0usize;
        for spec in variables {
            offsets.push(total);
            total += spec.element_count();
        }
        offsets.push(total);

        let mut cells = Vec::with_capacity(total);
        for (variable, spec) in variables.iter().enumerate() {
            for index in 0..spec.element_count() {
                cells.push(images.state_at(variable, index));
            }
        }

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
            offsets,
            pages,
            overall,
        }
    }

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
        // Both vectors are built from the same `variables()` pass, and `index`
        // comes from the shape's own bounds check, so the offset arithmetic
        // cannot leave the buffer.
        let offset = self.offsets[variable];
        Ok(self.cells[offset + index])
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
        self.cells.iter().filter(|cell| **cell == state).count()
    }
}
