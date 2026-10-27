//! The simulated memory: RAM per page, plus the burned (flash) image.

use crate::SimError;

/// One page's two images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMemory {
    /// INI page number, 1-based.
    pub number: u8,
    /// What the ECU currently runs from — reads and writes touch this.
    pub ram: Vec<u8>,
    /// What a burn last copied to EEPROM.
    pub flash: Vec<u8>,
}

/// Every page the definition declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pages: Vec<PageMemory>,
}

impl Memory {
    /// Builds zeroed images for `sizes` (page number, byte count).
    pub fn zeroed(sizes: &[(u8, u32)]) -> Self {
        Self {
            pages: sizes
                .iter()
                .map(|(number, size)| PageMemory {
                    number: *number,
                    ram: vec![0; usize::try_from(*size).unwrap_or(0)],
                    flash: vec![0; usize::try_from(*size).unwrap_or(0)],
                })
                .collect(),
        }
    }

    /// Builds images filled with a deterministic pattern.
    ///
    /// The pattern makes an offset or endianness bug visible: every byte of a
    /// page differs from its neighbours, and every page differs from the next.
    pub fn patterned(sizes: &[(u8, u32)], seed: u64) -> Self {
        let mut memory = Self::zeroed(sizes);
        for page in &mut memory.pages {
            let mut state = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(u64::from(page.number));
            for byte in &mut page.ram {
                // xorshift64*, so the sequence is stable across platforms.
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                *byte = u8::try_from((state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) & 0xFF)
                    .unwrap_or(0);
            }
            page.flash.copy_from_slice(&page.ram);
        }
        memory
    }

    /// The page with this number.
    pub fn page(&self, number: u8) -> Option<&PageMemory> {
        self.pages.iter().find(|page| page.number == number)
    }

    /// The page with this number, mutably.
    pub fn page_mut(&mut self, number: u8) -> Option<&mut PageMemory> {
        self.pages.iter_mut().find(|page| page.number == number)
    }

    /// A page's RAM contents.
    pub fn ram(&self, number: u8) -> Option<&[u8]> {
        self.page(number).map(|page| page.ram.as_slice())
    }

    /// A page's burned contents.
    pub fn flash(&self, number: u8) -> Option<&[u8]> {
        self.page(number).map(|page| page.flash.as_slice())
    }

    /// Replaces a page's RAM, for tests and scenario preload.
    pub fn set_ram(&mut self, number: u8, bytes: &[u8]) -> Result<(), SimError> {
        let page = self.page_mut(number).ok_or_else(|| SimError {
            message: format!("the definition has no page {number}"),
        })?;
        if bytes.len() > page.ram.len() {
            return Err(SimError {
                message: format!(
                    "page {number} holds {} byte(s); {} do not fit",
                    page.ram.len(),
                    bytes.len()
                ),
            });
        }
        page.ram[..bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    /// Writes into a page's RAM at an offset.
    pub fn write(&mut self, number: u8, offset: usize, bytes: &[u8]) -> Option<usize> {
        let page = self.page_mut(number)?;
        let end = offset.checked_add(bytes.len())?;
        if end > page.ram.len() {
            return None;
        }
        page.ram[offset..end].copy_from_slice(bytes);
        Some(bytes.len())
    }

    /// Copies a page's RAM to its burned image. Returns the page size.
    pub fn burn(&mut self, number: u8) -> Option<usize> {
        let page = self.page_mut(number)?;
        page.flash.copy_from_slice(&page.ram);
        Some(page.ram.len())
    }

    /// The page numbers, in definition order.
    pub fn numbers(&self) -> impl Iterator<Item = u8> + '_ {
        self.pages.iter().map(|page| page.number)
    }

    /// Number of pages.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether the definition declared no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burn_copies_ram_to_flash_and_nothing_else_does() {
        let mut memory = Memory::zeroed(&[(1, 4)]);
        memory.write(1, 0, &[1, 2, 3, 4]);
        assert_eq!(memory.flash(1), Some(&[0, 0, 0, 0][..]));

        memory.burn(1);

        assert_eq!(memory.flash(1), Some(&[1, 2, 3, 4][..]));
    }

    #[test]
    fn a_write_past_the_page_end_is_refused() {
        let mut memory = Memory::zeroed(&[(1, 4)]);

        assert_eq!(memory.write(1, 3, &[1, 2]), None);
        assert_eq!(memory.ram(1), Some(&[0, 0, 0, 0][..]));
    }

    #[test]
    fn the_fill_pattern_is_deterministic_and_page_specific() {
        let first = Memory::patterned(&[(1, 8), (2, 8)], 7);
        let second = Memory::patterned(&[(1, 8), (2, 8)], 7);

        assert_eq!(first, second);
        assert_ne!(first.ram(1), first.ram(2));
    }
}
