use std::collections::HashMap;

use oxtuner_ini::{CommandTemplate, Definition};
use oxtuner_protocol::{CommandPattern, WireOrder, page_identifier};

use super::pattern_for;
use crate::SimError;

/// One page's commands.
#[derive(Debug, Clone)]
pub(crate) struct PageEcu {
    pub(crate) number: u8,
    pub(crate) size: usize,
    /// The raw two-byte `%2i` identifier this page answers to.
    pub(crate) identifier: [u8; 2],
    pub(crate) read: Option<CommandPattern>,
    pub(crate) write: Option<CommandPattern>,
    pub(crate) burn: Option<CommandPattern>,
    pub(crate) crc: Option<CommandPattern>,
}

/// Every page's commands, in definition order.
pub(super) fn build_pages(
    definition: &Definition,
    variables: &HashMap<String, u8>,
    wire_order: WireOrder,
) -> Result<Vec<PageEcu>, SimError> {
    let pattern = |command: &CommandTemplate| pattern_for(command, variables, wire_order);

    definition
        .pages
        .iter()
        .map(|page| {
            Ok(PageEcu {
                number: page.number,
                size: usize::try_from(page.size).unwrap_or(0),
                identifier: page_identifier(page.number, page.identifier.as_ref(), &|name| {
                    variables.get(&name.to_ascii_lowercase()).copied()
                })
                .map_err(|error| SimError {
                    message: error.to_string(),
                })?,
                read: page
                    .read_command
                    .as_ref()
                    .filter(|command| !command.is_empty())
                    .and_then(pattern),
                write: page
                    .chunk_write
                    .as_ref()
                    .filter(|command| !command.is_empty())
                    .or(page
                        .value_write
                        .as_ref()
                        .filter(|command| !command.is_empty()))
                    .and_then(pattern),
                burn: page
                    .burn_command
                    .as_ref()
                    .filter(|command| !command.is_empty())
                    .and_then(pattern),
                crc: page
                    .crc_command
                    .as_ref()
                    .filter(|command| !command.is_empty())
                    .and_then(pattern),
            })
        })
        .collect()
}

/// Whether the command's raw `%2i` pair is this page's identifier.
pub(super) fn names_page(matched: &oxtuner_protocol::PatternMatch<'_>, page: &PageEcu) -> bool {
    matched
        .identifier
        .is_none_or(|identifier| identifier == page.identifier)
}
