//! Definition-derived command matching: what the virtual ECU understands.
//!
//! The simulator must not know Speeduino's command letters. It builds its
//! message table from the same [`Definition`] the host encodes with — the
//! signature query, each page's read/write/burn/CRC commands, and the realtime
//! command — and matches incoming bytes against those templates. A definition
//! for another board therefore simulates without a code change, as long as its
//! commands use the dialect's placeholders.

use std::collections::HashMap;

use oxtuner_ini::{ChannelKind, CommandTemplate, Definition};
use oxtuner_protocol::CommandPattern;

mod controller;
mod pages;

pub(crate) use pages::PageEcu;

use crate::SimError;

/// One page's commands.
#[derive(Debug, Clone)]

pub(crate) struct RealtimeEcu {
    pub(crate) pattern: CommandPattern,
    pub(crate) block_size: usize,
    pub(crate) start_offset: u16,
    /// Where a `burnPending` status bit lives, when the definition declares
    /// one. A queued burn sets it so a client can wait for its flash write.
    pub(crate) burn_pending: Option<(usize, u8)>,
}

/// A request the bytes turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request<'a> {
    Signature,
    ReadPage {
        page: u8,
        offset: u16,
        count: u16,
    },
    WritePage {
        page: u8,
        offset: u16,
        values: &'a [u8],
    },
    Burn {
        page: u8,
    },
    Crc {
        page: u8,
        offset: u16,
        count: u16,
    },
    Realtime {
        offset: u16,
        count: u16,
    },
    /// One `[ControllerCommands]` action.
    ControllerCommand,
}

/// The message set of one definition.
#[derive(Debug, Clone)]
pub(crate) struct SimEcu {
    pub(crate) signature: Vec<u8>,
    query: Option<CommandPattern>,
    realtime: Option<RealtimeEcu>,
    /// `[ControllerCommands]` command strings, in definition order.
    controller: Vec<CommandPattern>,
    pub(crate) pages: Vec<PageEcu>,
}

/// One command pattern with the definition's variables resolved.
pub(super) fn pattern_for(
    command: &CommandTemplate,
    variables: &HashMap<String, u8>,
    force_big_endian: bool,
) -> Option<CommandPattern> {
    CommandPattern::new(command, force_big_endian, &|name| {
        variables.get(&name.to_ascii_lowercase()).copied()
    })
}

impl SimEcu {
    /// Builds the message set, using `signature` as the reply text.
    pub(crate) fn new(definition: &Definition, signature: String) -> Result<Self, SimError> {
        let variables = variables(definition);
        let force_big_endian = definition.comms.force_big_endian_protocol;
        let pattern =
            |command: &CommandTemplate| pattern_for(command, &variables, force_big_endian);

        let query = definition
            .query_command
            .as_ref()
            .filter(|command| !command.is_empty())
            .and_then(pattern);
        let realtime = definition
            .comms
            .och_get_command
            .as_ref()
            .and_then(|command| {
                let pattern = pattern(command)?;
                let block_size = definition
                    .comms
                    .och_block_size
                    .and_then(|size| usize::try_from(size).ok())?;
                Some(RealtimeEcu {
                    pattern,
                    block_size,
                    start_offset: u16::try_from(definition.comms.output_channel_start_offset)
                        .unwrap_or(0),
                    burn_pending: definition.channels.iter().find_map(|(name, channel)| {
                        if !name.eq_ignore_ascii_case("burnPending") {
                            return None;
                        }
                        let ChannelKind::Measured {
                            offset,
                            bits: Some(bits),
                            ..
                        } = &channel.kind
                        else {
                            return None;
                        };
                        let offset = usize::try_from(*offset).ok()?;
                        (offset < block_size).then_some((offset, bits.low))
                    }),
                })
            });

        let pages = pages::build_pages(definition, &variables, force_big_endian);

        if query.is_none() && pages.iter().all(|page| page.read.is_none()) {
            return Err(SimError {
                message: format!(
                    "`{}` declares neither a queryCommand nor a page read command, so there is \
                     nothing to simulate",
                    definition.signature
                ),
            });
        }

        let controller = controller::build_controller(definition, &variables, force_big_endian);

        Ok(Self {
            signature: signature.into_bytes(),
            query,
            realtime,
            controller,
            pages,
        })
    }

    /// The realtime block, if the definition declares one.
    pub(crate) fn realtime(&self) -> Option<&RealtimeEcu> {
        self.realtime.as_ref()
    }

    /// Recognises one complete command.
    pub(crate) fn interpret<'a>(&self, bytes: &'a [u8]) -> Option<Request<'a>> {
        if let Some(pattern) = &self.query
            && pattern.matches(bytes)
        {
            return Some(Request::Signature);
        }

        if let Some(realtime) = &self.realtime
            && let Some(matched) = realtime.pattern.capture(bytes)
        {
            return Some(Request::Realtime {
                offset: matched.offset,
                count: matched.count,
            });
        }

        for page in &self.pages {
            if let Some(pattern) = &page.read
                && let Some(matched) = pattern.capture(bytes)
                && pages::names_page(&matched, page)
            {
                return Some(Request::ReadPage {
                    page: page.number,
                    offset: matched.offset,
                    count: matched.count,
                });
            }
            if let Some(pattern) = &page.write
                && let Some(matched) = pattern.capture(bytes)
                && pages::names_page(&matched, page)
            {
                return Some(Request::WritePage {
                    page: page.number,
                    offset: matched.offset,
                    values: matched.values,
                });
            }
            if let Some(pattern) = &page.burn
                && let Some(matched) = pattern.capture(bytes)
                && pages::names_page(&matched, page)
            {
                return Some(Request::Burn { page: page.number });
            }
            if let Some(pattern) = &page.crc
                && let Some(matched) = pattern.capture(bytes)
                && pages::names_page(&matched, page)
            {
                return Some(Request::Crc {
                    page: page.number,
                    offset: matched.offset,
                    count: matched.count,
                });
            }
        }

        for pattern in &self.controller {
            if pattern.matches(bytes) {
                return Some(Request::ControllerCommand);
            }
        }

        None
    }
}

pub(super) fn variables(definition: &Definition) -> HashMap<String, u8> {
    let mut variables = HashMap::with_capacity(definition.pc_variables.len());
    for (name, _) in definition.pc_variables.iter() {
        if let Some(value) = definition.command_variable(name) {
            variables.insert(name.to_ascii_lowercase(), value);
        }
    }
    variables
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::SettingsContext;

    use super::*;

    fn definition() -> Definition {
        Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 2
pageSize = 8, 4
page = 1
pageIdentifier = \"\\x00\\x00\", \"\\x00\\x01\"
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\"
pageValueWrite = \"M%2i%2o%2c%v\", \"M%2i%2o%2c%v\"
burnCommand = \"b%2i\", \"\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
page = 2
baro = scalar, U08, 0, \"kpa\", 1.0, 0.0

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0

[ControllerCommands]
cmdPing = \"z\"
cmdOff = \"y\\x01\"
",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn the_signature_query_is_recognised() {
        let ecu = SimEcu::new(&definition(), "speeduino 202501".to_owned()).expect("builds");

        assert_eq!(ecu.interpret(b"Q"), Some(Request::Signature));
    }

    #[test]
    fn a_controller_command_is_recognised() {
        let ecu = SimEcu::new(&definition(), "speeduino 202501".to_owned()).expect("builds");

        assert_eq!(ecu.interpret(b"z"), Some(Request::ControllerCommand));
        assert_eq!(ecu.interpret(b"y\x01"), Some(Request::ControllerCommand));
        assert_eq!(
            ecu.interpret(b"x"),
            None,
            "an undeclared action stays unknown"
        );
    }

    #[test]
    fn a_page_read_reports_the_page_from_the_definition_identifier() {
        let ecu = SimEcu::new(&definition(), "speeduino 202501".to_owned()).expect("builds");

        let request = ecu
            .interpret(&[b'p', 0x00, 0x01, 0x02, 0x00, 0x03, 0x00])
            .expect("recognised");

        assert_eq!(
            request,
            Request::ReadPage {
                page: 2,
                offset: 2,
                count: 3
            }
        );
    }

    #[test]
    fn a_write_carries_its_payload() {
        let ecu = SimEcu::new(&definition(), "speeduino 202501".to_owned()).expect("builds");

        let request = ecu
            .interpret(&[b'M', 0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0xAA, 0xBB])
            .expect("recognised");

        assert_eq!(
            request,
            Request::WritePage {
                page: 1,
                offset: 1,
                values: &[0xAA, 0xBB]
            }
        );
    }
}
