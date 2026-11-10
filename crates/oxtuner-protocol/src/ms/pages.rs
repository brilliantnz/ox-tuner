use std::collections::HashMap;

use oxtuner_ini::{CommandTemplate, Definition};

use crate::behavior::{BurnPlan, CrcReply, EcuBehavior, PageSwitch};

/// What the definition declares about one page.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent page capability, not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSpec {
    /// INI page number, 1-based.
    pub number: u8,
    /// Page size in bytes.
    pub size: u32,
    /// Base address added to every constant offset.
    pub start_offset: u32,
    /// The page can be read.
    pub readable: bool,
    /// The page can be written.
    pub writable: bool,
    /// The page can be burned.
    pub burnable: bool,
    /// The page can report a CRC-32.
    pub crc: bool,
}

/// The defined commands of one page.
#[derive(Debug, Clone)]
pub(super) struct PageCommands {
    pub(super) spec: PageSpec,
    /// The two bytes `%2i` expands to, from `pageIdentifier` when declared.
    pub(super) identifier: [u8; 2],
    /// What the firmware needs before this page may be used.
    pub(super) switch: PageSwitch,
    pub(super) read: Option<CommandTemplate>,
    pub(super) write: Option<CommandTemplate>,
    pub(super) burn: Option<CommandTemplate>,
    pub(super) crc: Option<CommandTemplate>,
    /// How a CRC reply from this page is encoded.
    pub(super) crc_reply: Option<CrcReply>,
}

pub(super) fn build_pages(
    definition: &Definition,
    behavior: &dyn EcuBehavior,
    variables: &HashMap<String, u8>,
) -> Vec<PageCommands> {
    definition
        .pages
        .iter()
        .map(|page| {
            let number = page.number;
            let burnable = matches!(behavior.burn(definition, number), BurnPlan::Immediate);
            let crc = behavior.crc(definition, number);

            PageCommands {
                switch: behavior.switch_page(definition, number),
                spec: PageSpec {
                    number,
                    size: page.size,
                    start_offset: page.start_offset,
                    readable: page.read_command.is_some(),
                    writable: page.is_writable(),
                    burnable,
                    crc: crc.is_some(),
                },
                identifier: crate::pattern::page_identifier(
                    page.number,
                    page.identifier.as_ref(),
                    &|name| variables.get(&name.to_ascii_lowercase()).copied(),
                ),
                read: page
                    .read_command
                    .clone()
                    .filter(|command| !command.is_empty()),
                write: page
                    .chunk_write
                    .clone()
                    .filter(|command| !command.is_empty())
                    .or_else(|| {
                        page.value_write
                            .clone()
                            .filter(|command| !command.is_empty())
                    }),
                burn: burnable
                    .then(|| page.burn_command.clone())
                    .flatten()
                    .filter(|command| !command.is_empty()),
                crc: page
                    .crc_command
                    .clone()
                    .filter(|command| !command.is_empty()),
                crc_reply: crc.map(|crc| crc.reply),
            }
        })
        .collect()
}
