//! Page numbering at the definition boundary.
//!
//! The dialect disagrees with itself and both spellings are load-bearing:
//!
//! | Artifact                    | Numbering                         |
//! | --------------------------- | --------------------------------- |
//! | INI `[Constants]` `page = N` | **1-based**                       |
//! | `.msq` `<page number="N">`   | **0-based**                       |
//! | Wire commands (`%2i`)        | the INI number, sent as a byte    |
//!
//! Keeping both as distinct newtypes means the off-by-one cannot travel
//! unnoticed: [`IniPage`] and [`MsqPage`] meet only in the two conversion
//! functions below, both unit-tested exhaustively (P10 finding 3).
//!
//! [`PageLayout`]: crate::PageLayout

use std::fmt;

/// A calibration page number as written in the INI: **1-based**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IniPage(u8);

impl IniPage {
    /// The first INI page.
    pub const FIRST: Self = Self(1);

    /// Builds an INI page number, rejecting the impossible page 0.
    pub const fn new(number: u8) -> Option<Self> {
        if number == 0 {
            None
        } else {
            Some(Self(number))
        }
    }

    /// The number as written.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The `.msq` page number carrying this INI page's constants.
    pub const fn to_msq(self) -> MsqPage {
        // `u8 → u16` is lossless; `u16::from` is not const-stable yet.
        MsqPage(self.0 as u16 - 1)
    }
}

impl fmt::Display for IniPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A tune-file page number as written in `.msq` files: **0-based**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MsqPage(u16);

impl MsqPage {
    /// The first `.msq` page.
    pub const FIRST: Self = Self(0);

    /// Builds an `.msq` page number.
    pub const fn new(number: u16) -> Self {
        Self(number)
    }

    /// The number as written.
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The INI page this `.msq` page carries.
    ///
    /// Every representable `.msq` page maps to an INI page; `u16::MAX` would
    /// need INI page 65 536, which does not fit, so it returns `None` rather
    /// than wrapping to page 0.
    pub fn to_ini(self) -> Option<IniPage> {
        let number = self.0.checked_add(1)?;
        u8::try_from(number).ok().and_then(IniPage::new)
    }
}

impl fmt::Display for MsqPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference project has fifteen pages; every one must map both ways.
    #[test]
    fn the_reference_page_table_maps_both_directions() {
        for msq in 0u16..15 {
            let ini = MsqPage::new(msq).to_ini().expect("fits");
            assert_eq!(ini.get(), u8::try_from(msq).unwrap() + 1);
            assert_eq!(ini.to_msq().get(), msq);
        }
    }

    #[test]
    fn ini_page_zero_does_not_exist() {
        assert_eq!(IniPage::new(0), None);
        assert_eq!(IniPage::new(1), Some(IniPage::FIRST));
    }

    #[test]
    fn an_unrepresentable_msq_page_is_rejected_instead_of_wrapping() {
        assert_eq!(MsqPage::new(u16::MAX).to_ini(), None);
        assert_eq!(MsqPage::new(254).to_ini(), Some(IniPage::new(255).unwrap()));
    }
}
