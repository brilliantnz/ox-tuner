//! Whether a definition's visibility and enable expressions let an item appear.

/// The evaluated gates a dialog row, preset, command or widget carries.
///
/// Both gates are open when the definition states none. A gate that cannot be
/// evaluated stays open and sets [`Self::unresolved`], so a gap in the evaluator
/// never hides a setting the user can otherwise edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateState {
    /// The item is shown.
    pub visible: bool,
    /// The item accepts edits or activation.
    pub enabled: bool,
    /// At least one gate could not be evaluated.
    pub unresolved: bool,
}

impl GateState {
    /// Both gates open, with nothing unresolved.
    #[must_use]
    pub const fn open() -> Self {
        Self {
            visible: true,
            enabled: true,
            unresolved: false,
        }
    }
}

impl Default for GateState {
    fn default() -> Self {
        Self::open()
    }
}
