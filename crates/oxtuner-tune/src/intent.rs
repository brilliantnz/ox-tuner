//! Write intents: what the user asked the core to do, as data.
//!
//! Nothing in this crate performs an intent. `oxtuner-tune` has no transport and
//! no filesystem (ADR-0010/0011): the core receives an intent, runs the S1–S10
//! guardrails, executes the conversation and reports back a read-back-verified
//! image. Keeping the request a value is what makes "no auto-burn" structural —
//! there is no code path from editing to a write without an explicit intent.

use std::fmt;

use crate::{IniPage, Layout, Tune};

/// Which calibration pages an operation covers.
#[expect(
    clippy::exhaustive_enums,
    reason = "a page set is either every page or an explicit list"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageSet {
    /// Every page the definition declares.
    All,
    /// A sorted, de-duplicated set of pages.
    Pages(Vec<IniPage>),
}

impl PageSet {
    /// A set of explicit pages. Duplicates are removed and the order is fixed,
    /// so two equivalent requests compare equal.
    pub fn pages(pages: impl IntoIterator<Item = IniPage>) -> Self {
        let mut pages: Vec<IniPage> = pages.into_iter().collect();
        pages.sort_unstable();
        pages.dedup();
        Self::Pages(pages)
    }

    /// Whether the set covers `page`.
    pub fn contains(&self, page: IniPage) -> bool {
        match self {
            Self::All => true,
            Self::Pages(pages) => pages.contains(&page),
        }
    }

    /// Whether the set names no page at all (an empty explicit set).
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Pages(pages) if pages.is_empty())
    }

    /// Checks every named page against a layout.
    ///
    /// # Errors
    ///
    /// Returns [`IntentError::NoPages`] for an empty explicit set and
    /// [`IntentError::UnknownPage`] for a page the layout does not declare.
    pub fn validate(&self, layout: &Layout) -> Result<(), IntentError> {
        let Self::Pages(pages) = self else {
            return Ok(());
        };
        if pages.is_empty() {
            return Err(IntentError::NoPages);
        }
        for page in pages {
            if layout.page(*page).is_none() {
                return Err(IntentError::UnknownPage(*page));
            }
        }
        Ok(())
    }

    /// A human-readable description for logs and confirmations.
    pub fn describe(&self) -> String {
        match self {
            Self::All => "all pages".to_owned(),
            Self::Pages(pages) => {
                let numbers: Vec<String> = pages.iter().map(ToString::to_string).collect();
                if numbers.is_empty() {
                    "no pages".to_owned()
                } else {
                    format!("pages {}", numbers.join(", "))
                }
            }
        }
    }
}

/// What the user asked for.
#[expect(
    clippy::exhaustive_enums,
    reason = "the set of user-initiated write operations is closed"
)]
#[derive(Debug, Clone)]
pub enum WriteIntent {
    /// Send the working image's pages to ECU RAM (takes effect immediately).
    WriteToRam {
        /// Pages to write.
        pages: PageSet,
    },
    /// Burn confirmed RAM content to flash (persists across power cycles).
    BurnToFlash {
        /// Pages to burn, or every burnable page.
        pages: Option<PageSet>,
    },
    /// Discard local edits and return to the baseline image.
    RevertToBaseline,
    /// Replace the working image with another tune.
    ImportFrom {
        /// The imported tune.
        tune: Tune,
        /// Whether the user explicitly approved the import (ADR-0011: an import
        /// whose signature differs requires consent).
        consented: bool,
    },
}

impl WriteIntent {
    /// Whether the intent changes something on the ECU.
    pub const fn touches_ecu(&self) -> bool {
        matches!(self, Self::WriteToRam { .. } | Self::BurnToFlash { .. })
    }

    /// Whether the intent changes the working image.
    pub const fn changes_working(&self) -> bool {
        matches!(self, Self::RevertToBaseline | Self::ImportFrom { .. })
    }

    /// Whether the user still has to approve before the core may execute.
    pub const fn needs_consent(&self) -> bool {
        matches!(
            self,
            Self::ImportFrom {
                consented: false,
                ..
            }
        )
    }

    /// The pages the intent names, when it names any.
    pub const fn pages(&self) -> Option<&PageSet> {
        match self {
            Self::WriteToRam { pages } => Some(pages),
            Self::BurnToFlash { pages } => pages.as_ref(),
            Self::RevertToBaseline | Self::ImportFrom { .. } => None,
        }
    }

    /// Validates page numbers against a layout.
    ///
    /// # Errors
    ///
    /// Returns [`IntentError`] when an explicit page set is empty or names a
    /// page the layout does not declare.
    pub fn validate(&self, layout: &Layout) -> Result<(), IntentError> {
        match self.pages() {
            Some(pages) => pages.validate(layout),
            None => Ok(()),
        }
    }

    /// A description suitable for a confirmation dialog.
    pub fn describe(&self) -> String {
        match self {
            Self::WriteToRam { pages } => format!("write {} to ECU RAM", pages.describe()),
            Self::BurnToFlash { pages } => match pages {
                Some(pages) => format!("burn {} to flash", pages.describe()),
                None => "burn every page to flash".to_owned(),
            },
            Self::RevertToBaseline => "discard local edits".to_owned(),
            Self::ImportFrom { .. } => "import a tune".to_owned(),
        }
    }
}

/// Why an intent cannot be executed.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentError {
    /// An explicit page set named no pages.
    NoPages,
    /// A page is not in the layout.
    UnknownPage(IniPage),
}

impl fmt::Display for IntentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPages => formatter.write_str("the intent names no pages"),
            Self::UnknownPage(page) => write!(formatter, "page {page} is not in the definition"),
        }
    }
}

impl std::error::Error for IntentError {}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    fn layout() -> Layout {
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 8, 8
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
page = 2
b = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        Layout::build(&definition, &|_| None)
    }

    #[test]
    fn a_page_set_is_sorted_and_de_duplicated() {
        let set = PageSet::pages([
            IniPage::new(2).unwrap(),
            IniPage::new(1).unwrap(),
            IniPage::new(2).unwrap(),
        ]);

        assert_eq!(
            set,
            PageSet::Pages(vec![IniPage::new(1).unwrap(), IniPage::new(2).unwrap()])
        );
        assert_eq!(set.describe(), "pages 1, 2");
    }

    #[test]
    fn all_contains_every_page_and_an_empty_set_contains_none() {
        assert!(PageSet::All.contains(IniPage::new(9).unwrap()));

        let empty = PageSet::pages([]);
        assert!(empty.is_empty());
        assert!(!empty.contains(IniPage::FIRST));
        assert_eq!(empty.describe(), "no pages");
    }

    #[test]
    fn validation_rejects_an_empty_set_and_unknown_pages() {
        let layout = layout();

        assert_eq!(
            PageSet::pages([]).validate(&layout),
            Err(IntentError::NoPages)
        );
        assert_eq!(
            PageSet::pages([IniPage::new(9).unwrap()]).validate(&layout),
            Err(IntentError::UnknownPage(IniPage::new(9).unwrap()))
        );
        assert_eq!(PageSet::All.validate(&layout), Ok(()));
        assert_eq!(PageSet::pages([IniPage::FIRST]).validate(&layout), Ok(()));
    }

    #[test]
    fn intents_describe_themselves_and_never_need_consent_for_writes() {
        let write = WriteIntent::WriteToRam {
            pages: PageSet::All,
        };
        let burn = WriteIntent::BurnToFlash { pages: None };
        let revert = WriteIntent::RevertToBaseline;

        assert!(write.touches_ecu());
        assert!(!write.needs_consent());
        assert_eq!(write.describe(), "write all pages to ECU RAM");
        assert_eq!(burn.describe(), "burn every page to flash");
        assert!(revert.changes_working());
        assert!(!revert.touches_ecu());
    }

    #[test]
    fn an_unconsented_import_asks_for_consent() {
        let tuned = Tune::from_definition(
            &parse_str(
                "[MegaTune]\nsignature = \"speeduino 202501\"\n[Constants]\nnPages = 1\npageSize = 8\npage = 1\na = scalar, U08, 0, \"\", 1, 0, 0, 255, 0\n",
                &SettingsContext::new(),
            )
            .expect("parses"),
        );

        let pending = WriteIntent::ImportFrom {
            tune: tuned.clone(),
            consented: false,
        };
        assert!(pending.needs_consent());

        let approved = WriteIntent::ImportFrom {
            tune: tuned,
            consented: true,
        };
        assert!(!approved.needs_consent());
        assert!(approved.changes_working());
    }
}
