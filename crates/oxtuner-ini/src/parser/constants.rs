//! `[MegaTune]`, `[TunerStudio]`, `[Constants]` and `[PcVariables]`.
//!
//! `[Constants]` is the hardest section in the dialect: it mixes protocol
//! settings, page declarations, per-page command blocks and calibration entries
//! in one flat list. The parser therefore decides per
//! line whether it is an entry (`scalar, U08, …`) or a setting (`nPages = 15`).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::fields::{CommandTemplate, split_fields, split_fields_preserving_empty};
use crate::model::{
    BitOption, CommsSettings, Constant, ConstantClass, ConstantFlags, DataType, Definition,
    Dimension, Endianness, Number, Offset, Page, PcVariable, Shape, Text,
};
use crate::preprocess::Expander;

use super::SectionParser;

/// Communication keys that are recognised but not modelled yet. Their value
/// is kept verbatim in [`CommsSettings::raw`] rather than being dropped.
const PRESERVED_COMMS_KEYS: &[&str] = &[
    "validatearraybounds",
    "uselegacyftempunits",
    "restrictsquirtrelationship",
    "fastfileserialread",
    "tableblockingfactor",
    "tablewritecommand",
    "tablecrccommand",
    "readsdcompressed",
    "fulltimeturboenabled",
    "timedpagerefresh",
    "triggeredpagerefresh",
    "refreshlocalstoreonactivity",
    "helpmanualdownloadroot",
];

/// `[MegaTune]` and `[TunerStudio]`: identification and file-level attributes.
pub(crate) fn parse_identification(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };

        match key.to_ascii_lowercase().as_str() {
            "signature" => {
                crate::fields::unquote(&value).clone_into(&mut definition.signature);
                definition.signature_section.clone_from(&section.name);
                section.understand(*line);
            }
            "querycommand" => {
                definition.query_command = Some(CommandTemplate::parse(&value));
                section.understand(*line);
            }
            "versioninfo" => {
                definition.version_info = Some(crate::fields::unquote(&value).to_owned());
                section.understand(*line);
            }
            "iniversion" => {
                definition.ini_version = Some(crate::fields::unquote(&value).to_owned());
                section.understand(*line);
            }
            "inispecversion" => {
                match crate::fields::parse_f64(&value) {
                    Some(version) => definition.spec_version = Some(version),
                    None => section.warn(
                        "bad-spec-version",
                        format!("`iniSpecVersion` is not a number: {value}"),
                        *line,
                    ),
                }
                section.understand(*line);
            }
            _ => {
                // Everything else is a tunable the model keeps verbatim.
                definition.comms.raw.insert(key.clone(), value.clone());
                section.understand(*line);
            }
        }
    }
}

/// Where the running offset cursor is on the current page.
#[derive(Debug, Default)]
struct Cursor {
    next: u32,
    last: u32,
}

impl Cursor {
    fn reset(&mut self) {
        self.next = 0;
        self.last = 0;
    }

    fn advance(&mut self, address: Option<u32>, size: Option<u32>) {
        if let Some(address) = address {
            self.last = address;
            if let Some(size) = size {
                self.next = address.saturating_add(size);
            }
        }
    }
}

/// Command template blocks declared inside a single `page = k` block (iniSpec 3.77).
#[derive(Debug, Default, Clone)]
struct PageCommands {
    identifier: Option<CommandTemplate>,
    read: Option<CommandTemplate>,
    burn: Option<CommandTemplate>,
    value_write: Option<CommandTemplate>,
    chunk_write: Option<CommandTemplate>,
    crc: Option<CommandTemplate>,
}

/// The page table as it is collected from `[Constants]`.
#[derive(Debug, Default)]
struct PageTable {
    n_pages: Option<u32>,
    sizes: Vec<u32>,
    identifiers: Vec<CommandTemplate>,
    reads: Vec<CommandTemplate>,
    burns: Vec<CommandTemplate>,
    value_writes: Vec<CommandTemplate>,
    chunk_writes: Vec<CommandTemplate>,
    crcs: Vec<CommandTemplate>,
    start_offsets: Vec<u32>,
    overrides: BTreeMap<u8, PageCommands>,
}

/// Which per-page list a command key belongs to.
#[derive(Clone, Copy)]
enum CommandKind {
    Identifier,
    Read,
    Burn,
    ValueWrite,
    ChunkWrite,
    Crc,
}

impl PageTable {
    /// A single command declared inside a `page = k` block belongs to that
    /// page; anything else is the parallel list for every page.
    fn set_commands(&mut self, page: u8, kind: CommandKind, value: &str) {
        let commands: Vec<CommandTemplate> = split_fields_preserving_empty(value)
            .iter()
            .map(|field| CommandTemplate::parse(field))
            .collect();

        if page != 0 && commands.len() == 1 {
            let entry = self.overrides.entry(page).or_default();
            let command = commands.into_iter().next();
            match kind {
                CommandKind::Identifier => entry.identifier = command,
                CommandKind::Read => entry.read = command,
                CommandKind::Burn => entry.burn = command,
                CommandKind::ValueWrite => entry.value_write = command,
                CommandKind::ChunkWrite => entry.chunk_write = command,
                CommandKind::Crc => entry.crc = command,
            }
            return;
        }

        let target = match kind {
            CommandKind::Identifier => &mut self.identifiers,
            CommandKind::Read => &mut self.reads,
            CommandKind::Burn => &mut self.burns,
            CommandKind::ValueWrite => &mut self.value_writes,
            CommandKind::ChunkWrite => &mut self.chunk_writes,
            CommandKind::Crc => &mut self.crcs,
        };
        *target = commands;
    }

    fn at(
        list: &[CommandTemplate],
        page: u8,
        per_page: Option<&CommandTemplate>,
    ) -> Option<CommandTemplate> {
        if let Some(command) = per_page {
            return Some(command.clone());
        }
        let index = usize::from(page).checked_sub(1)?;
        list.get(index).cloned()
    }

    fn build(self, highest_page: u8, section: &mut SectionParser) -> Vec<Page> {
        let declared = self.n_pages.unwrap_or(0);
        let highest_override = self.overrides.keys().copied().max().map_or(0, u32::from);
        let listed = u32::try_from(self.sizes.len()).unwrap_or(u32::MAX);
        let count = declared
            .max(u32::from(highest_page))
            .max(listed)
            .max(highest_override);

        if declared != 0 && listed != declared {
            section.note(
                "page-size-count",
                format!(
                    "`nPages` is {declared} but `pageSize` lists {} entries",
                    self.sizes.len()
                ),
                0,
            );
        }

        let mut pages = Vec::new();
        for number in 1..=count {
            let Ok(number) = u8::try_from(number) else {
                break;
            };
            let overrides = self.overrides.get(&number);
            let index = usize::from(number).saturating_sub(1);
            pages.push(Page {
                number,
                size: self.sizes.get(index).copied().unwrap_or(0),
                start_offset: self.start_offsets.get(index).copied().unwrap_or(0),
                identifier: Self::at(
                    &self.identifiers,
                    number,
                    overrides.and_then(|entry| entry.identifier.as_ref()),
                ),
                read_command: Self::at(
                    &self.reads,
                    number,
                    overrides.and_then(|entry| entry.read.as_ref()),
                ),
                burn_command: Self::at(
                    &self.burns,
                    number,
                    overrides.and_then(|entry| entry.burn.as_ref()),
                ),
                value_write: Self::at(
                    &self.value_writes,
                    number,
                    overrides.and_then(|entry| entry.value_write.as_ref()),
                ),
                chunk_write: Self::at(
                    &self.chunk_writes,
                    number,
                    overrides.and_then(|entry| entry.chunk_write.as_ref()),
                ),
                crc_command: Self::at(
                    &self.crcs,
                    number,
                    overrides.and_then(|entry| entry.crc.as_ref()),
                ),
            });
        }
        pages
    }
}

/// `[Constants]`.
pub(crate) fn parse_constants(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
    expander: &Expander<'_>,
) {
    let mut cursor = Cursor::default();
    let mut table = PageTable::default();
    let mut option_tables = OptionTables::default();
    let mut current_page = 0u8;
    let mut highest_page = 0u8;

    for (line, text) in lines {
        section.count_entry();

        let Some((key, value)) = crate::fields::split_assignment(text) else {
            section.unrecognized_line(*line, text);
            continue;
        };
        let lowered = key.to_ascii_lowercase();

        if lowered == "page" {
            match crate::fields::parse_u32(value).and_then(|page| u8::try_from(page).ok()) {
                Some(page) => {
                    current_page = page;
                    highest_page = highest_page.max(page);
                    // `nextOffset` / `lastOffset` restart on every page boundary.
                    cursor.reset();
                }
                None => section.warn(
                    "bad-page",
                    format!("`page = {value}` is not a page number"),
                    *line,
                ),
            }
            section.understand(*line);
            continue;
        }

        let raw_fields = split_fields(value);
        let fields = expander.expand(&raw_fields);
        if is_constant_entry(&fields) {
            let entry = Entry {
                line: *line,
                key,
                raw: value,
                raw_fields: &raw_fields,
                fields,
                page: current_page,
            };
            if parse_constant(definition, section, entry, &mut cursor, &mut option_tables) {
                section.understand(*line);
            }
            continue;
        }

        let parsed = parse_comms_key(
            definition,
            &mut table,
            current_page,
            &lowered,
            value,
            section,
            *line,
        );
        if parsed {
            section.understand(*line);
        } else {
            section.unrecognized_key(key, *line);
            definition.comms.raw.insert(lowered, value.to_owned());
        }
    }

    definition.pages = table.build(highest_page, section);
}

/// Whether the expanded field list is a calibration entry rather than a section
/// setting. Both a known class and a known data type are required, so a typo'd
/// key with a scalar value stays a setting instead of becoming a bogus constant.
fn is_constant_entry(fields: &[Cow<'_, str>]) -> bool {
    match (fields.first(), fields.get(1)) {
        (Some(class), Some(data_type)) => {
            ConstantClass::parse(class).is_some() && DataType::parse(data_type).is_some()
        }
        _ => false,
    }
}

/// Handles the communication and page-table keys that appear inside `[Constants]`.
fn parse_comms_key(
    definition: &mut Definition,
    table: &mut PageTable,
    page: u8,
    key: &str,
    value: &str,
    section: &mut SectionParser,
    line: u32,
) -> bool {
    let comms: &mut CommsSettings = &mut definition.comms;
    match key {
        "endianness" => {
            // An unreadable value keeps the dialect default instead of guessing.
            if let Some(endianness) = Endianness::parse(value) {
                definition.endianness = endianness;
            }
        }
        "npages" => table.n_pages = crate::fields::parse_u32(value),
        "pagesize" => table.sizes = number_list(value),
        "parameterstartoffset" => table.start_offsets = number_list(value),
        "pageidentifier" => table.set_commands(page, CommandKind::Identifier, value),
        "pagereadcommand" => table.set_commands(page, CommandKind::Read, value),
        "burncommand" => table.set_commands(page, CommandKind::Burn, value),
        "pagevaluewrite" => table.set_commands(page, CommandKind::ValueWrite, value),
        "pagechunkwrite" => table.set_commands(page, CommandKind::ChunkWrite, value),
        "crc32checkcommand" | "pagecrccommand" => table.set_commands(page, CommandKind::Crc, value),
        "blockingfactor" => comms.blocking_factor = crate::fields::parse_u32(value),
        "interwritedelay" => comms.inter_write_delay_ms = crate::fields::parse_u32(value),
        "writeblocks" | "tswriteblocks" => comms.write_blocks = is_on(value),
        "blockreadtimeout" => comms.block_read_timeout_ms = crate::fields::parse_u32(value),
        "pageactivationdelay" => {
            comms.page_activation_delay_ms = crate::fields::parse_u32(value);
        }
        "delayafterportopen" => comms.delay_after_port_open_ms = crate::fields::parse_u32(value),
        "nocommreaddelay" => comms.no_comm_read_delay = is_on(value),
        "messageenvelopeformat" => {
            comms.envelope = Some(crate::fields::unquote(value).to_owned());
        }
        "forcebigendianprotocol" => comms.force_big_endian_protocol = is_on(value),
        "outputchannelstartoffset" => {
            // An unreadable value keeps the dialect default instead of guessing.
            if let Some(offset) = crate::fields::parse_u32(value) {
                comms.output_channel_start_offset = offset;
            } else {
                section.warn(
                    "bad-number",
                    format!("`{key}` is not a number: {value}; keeping the dialect default"),
                    line,
                );
            }
        }
        "defaultruntimerecordpersec" => {
            comms.default_runtime_record_per_sec = crate::fields::parse_u32(value);
        }
        "maxunusedruntimerange" => {
            // An unreadable value keeps the dialect default instead of guessing.
            if let Some(range) = crate::fields::parse_u32(value) {
                comms.max_unused_runtime_range = range;
            } else {
                section.warn(
                    "bad-number",
                    format!("`{key}` is not a number: {value}; keeping the dialect default"),
                    line,
                );
            }
        }
        "retrieveconfigerror" => comms.retrieve_config_error = Some(CommandTemplate::parse(value)),
        "ochgetcommand" => comms.och_get_command = Some(CommandTemplate::parse(value)),
        "ochblocksize" => comms.och_block_size = crate::fields::parse_u32(value),
        other => {
            if !PRESERVED_COMMS_KEYS.contains(&other) {
                return false;
            }
            comms.raw.insert(other.to_owned(), value.to_owned());
        }
    }
    true
}

fn is_on(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "on" | "true" | "1" | "yes"
    )
}

fn number_list(value: &str) -> Vec<u32> {
    split_fields(value)
        .iter()
        .filter_map(|field| crate::fields::parse_u32(field))
        .collect()
}

/// One calibration entry, bundled so the parser keeps a readable signature.
struct Entry<'a> {
    line: u32,
    key: &'a str,
    raw: &'a str,
    /// The entry's fields as written, before `$define` expansion. Only the
    /// option list needs them: it is what identifies a shared table.
    raw_fields: &'a [String],
    fields: Vec<Cow<'a, str>>,
    page: u8,
}

/// Parses one calibration entry. Returns whether it became model data.
fn parse_constant(
    definition: &mut Definition,
    section: &mut SectionParser,
    entry: Entry<'_>,
    cursor: &mut Cursor,
    option_tables: &mut OptionTables,
) -> bool {
    let Entry {
        line,
        key,
        raw,
        raw_fields,
        fields,
        page,
    } = entry;
    let Some(class) = fields.first().and_then(|field| ConstantClass::parse(field)) else {
        unreachable!("callers check the class first");
    };
    let Some(data_type) = fields.get(1).and_then(|field| DataType::parse(field)) else {
        unreachable!("callers check the data type first");
    };

    // A `string` entry may omit its offset entirely (`string, ASCII, 20`), so
    // the offset decision is made per class.
    let (offset, index) = if class == ConstantClass::String && fields.len() == 3 {
        (Offset::Next, 2usize)
    } else {
        match fields.get(2) {
            Some(field) if looks_like_offset(field) => {
                (parse_offset(section, line, key, field), 3usize)
            }
            _ => (Offset::Next, 2usize),
        }
    };

    let mut context = AttributeContext {
        fields: &fields,
        raw_fields,
        key,
        line,
        index,
        flags: ConstantFlags::default(),
    };
    let Some(attributes) = parse_entry_attributes(class, &mut context, option_tables, section)
    else {
        return false;
    };
    let EntryAttributes {
        shape,
        options,
        units,
        scale,
        translate,
        minimum,
        maximum,
        digits,
        flags,
    } = attributes;

    let address = match &offset {
        Offset::Fixed(value) => Some(*value),
        Offset::Next => Some(cursor.next),
        Offset::Last => Some(cursor.last),
        // An expression resolves at load time; an unknown offset has none.
        // Both must stay address-less so they can never be mistaken for a
        // real (and therefore writable) ECU address.
        Offset::Expression(_) | Offset::Unknown => None,
    };

    if matches!(offset, Offset::Expression(_)) {
        section.note(
            "dynamic-offset",
            format!("`{key}` has an offset expression; it resolves at load time"),
            line,
        );
    }

    let constant = Constant {
        name: key.to_owned(),
        class,
        data_type,
        page,
        offset,
        address,
        shape,
        units,
        scale,
        translate,
        minimum,
        maximum,
        digits,
        options,
        flags,
        line,
        raw: raw.to_owned(),
    };

    let size = constant.byte_size();
    cursor.advance(address, size);

    if definition.constants.insert(key, constant).is_some() {
        section.error(
            "duplicate-name",
            format!("`{key}` is defined more than once; the later entry wins"),
            line,
        );
    }
    true
}

/// The class-specific attributes of one calibration entry.
struct EntryAttributes {
    shape: Shape,
    options: Arc<[BitOption]>,
    units: Text,
    scale: Option<Number>,
    translate: Option<Number>,
    minimum: Option<Number>,
    maximum: Option<Number>,
    digits: Option<Number>,
    flags: ConstantFlags,
}

/// The fields and cursor shared while reading one entry's attributes.
struct AttributeContext<'a, 'b> {
    fields: &'a [Cow<'b, str>],
    raw_fields: &'a [String],
    key: &'a str,
    line: u32,
    index: usize,
    flags: ConstantFlags,
}

/// Reads the shape, units and numeric attributes for one entry class.
fn parse_entry_attributes(
    class: ConstantClass,
    context: &mut AttributeContext<'_, '_>,
    option_tables: &mut OptionTables,
    section: &mut SectionParser,
) -> Option<EntryAttributes> {
    let mut attributes = EntryAttributes {
        shape: Shape::Scalar,
        options: Arc::from([]),
        units: Text::Literal(String::new()),
        scale: None,
        translate: None,
        minimum: None,
        maximum: None,
        digits: None,
        flags: ConstantFlags::default(),
    };

    match class {
        ConstantClass::Bits => {
            let Some(raw_shape) = context.fields.get(context.index) else {
                section.warn(
                    "missing-shape",
                    format!("`{}` has no bit range", context.key),
                    context.line,
                );
                return None;
            };
            context.index += 1;
            let Some(shape) = parse_bit_shape(raw_shape) else {
                section.warn(
                    "bad-bit-range",
                    format!("`{}` has bit range {raw_shape:?}", context.key),
                    context.line,
                );
                return None;
            };
            attributes.shape = shape;
            attributes.options = option_tables.table(
                shared_define(context.raw_fields, context.index),
                &context.fields[context.index..],
            );
        }
        ConstantClass::Array | ConstantClass::OddArray => {
            let Some(raw_shape) = context.fields.get(context.index) else {
                section.warn(
                    "missing-shape",
                    format!("`{}` has no array shape", context.key),
                    context.line,
                );
                return None;
            };
            context.index += 1;
            let Some(shape) = parse_array_shape(raw_shape) else {
                section.warn(
                    "bad-shape",
                    format!("`{}` has array shape {raw_shape:?}", context.key),
                    context.line,
                );
                return None;
            };
            let (units, numbers) =
                split_numbers(&context.fields[context.index..], &mut context.flags);
            attributes.shape = shape;
            attributes.units = units;
            attributes.scale = numbers.first().cloned();
            attributes.translate = numbers.get(1).cloned();
            attributes.minimum = numbers.get(2).cloned();
            attributes.maximum = numbers.get(3).cloned();
            attributes.digits = numbers.get(4).cloned();
        }
        ConstantClass::String => {
            let tail = &context.fields[context.index..];
            let (shape, _) = parse_string_shape(tail);
            for field in tail {
                if is_keyword(field) {
                    apply_keyword(&mut context.flags, field);
                }
            }
            attributes.shape = shape;
        }
        ConstantClass::Scalar => {
            let (units, numbers) =
                split_numbers(&context.fields[context.index..], &mut context.flags);
            attributes.units = units;
            attributes.scale = numbers.first().cloned();
            attributes.translate = numbers.get(1).cloned();
            attributes.minimum = numbers.get(2).cloned();
            attributes.maximum = numbers.get(3).cloned();
            attributes.digits = numbers.get(4).cloned();
        }
    }

    attributes.flags = context.flags;
    Some(attributes)
}

/// Splits the trailing fields of a scalar/array entry into units and the numeric
/// attributes, collecting storage keywords as it goes.
fn split_numbers(fields: &[Cow<'_, str>], flags: &mut ConstantFlags) -> (Text, Vec<Number>) {
    let mut units = Text::Literal(String::new());
    let mut numbers = Vec::new();
    let mut seen_units = false;

    for field in fields {
        if is_keyword(field) {
            apply_keyword(flags, field);
            continue;
        }
        if !seen_units && (field.starts_with('"') || crate::fields::is_expression_field(field)) {
            units = Text::parse(field);
            seen_units = true;
            continue;
        }
        numbers.push(Number::parse(field));
    }

    (units, numbers)
}

fn looks_like_offset(field: &str) -> bool {
    let field = field.trim();
    !field.starts_with('"') && !field.starts_with('[') && !field.starts_with('{')
}

fn parse_offset(section: &mut SectionParser, line: u32, key: &str, field: &str) -> Offset {
    let lowered = field.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "nextoffset" => return Offset::Next,
        "lastoffset" => return Offset::Last,
        _ => {}
    }
    if let Some(value) = crate::fields::parse_u32(field) {
        return Offset::Fixed(value);
    }
    match crate::expr::Expr::parse(crate::fields::expression_body(field).unwrap_or(field)) {
        Ok(expression) => Offset::Expression(expression),
        Err(error) => {
            // The offset decides where this constant lives in ECU memory, so a
            // failed parse must never become a fabricated — and therefore
            // writable — address. `Offset::Unknown` marks the constant's
            // layout as unresolved; the warning names the constant so
            // consumers can filter for definitions with unknown layout.
            section.warn(
                "bad-offset",
                format!(
                    "`{key}` has offset {field:?}: {error}; the constant's memory location is \
                     recorded as unknown"
                ),
                line,
            );
            Offset::Unknown
        }
    }
}

fn parse_bit_shape(field: &str) -> Option<Shape> {
    let inner = field.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (range, shift) = match inner.split_once('+') {
        Some((range, shift)) => (range, shift.trim().parse::<u8>().ok()?),
        None => (inner, 0),
    };
    let (low, high) = range.split_once(':')?;
    let low: u8 = low.trim().parse().ok()?;
    let high: u8 = high.trim().parse().ok()?;
    (low <= high).then_some(Shape::Bits { low, high, shift })
}

fn parse_array_shape(field: &str) -> Option<Shape> {
    let inner = field.trim().strip_prefix('[')?.strip_suffix(']')?;
    match inner.split_once(['x', 'X']) {
        Some((cols, rows)) => Some(Shape::Table {
            cols: parse_dimension(cols)?,
            rows: parse_dimension(rows)?,
        }),
        None => Some(Shape::List(parse_dimension(inner)?)),
    }
}

fn parse_dimension(field: &str) -> Option<Dimension> {
    let field = field.trim();
    if let Some(value) = crate::fields::parse_u32(field) {
        return Some(Dimension::Fixed(value));
    }
    let source = crate::fields::expression_body(field).unwrap_or(field);
    crate::expr::Expr::parse(source)
        .ok()
        .map(Dimension::Expression)
}

fn parse_string_shape(fields: &[Cow<'_, str>]) -> (Shape, usize) {
    let length = fields
        .first()
        .and_then(|field| parse_dimension(field))
        .unwrap_or(Dimension::Fixed(0));
    let columns = fields.get(1).and_then(|field| parse_dimension(field));
    (Shape::Text { length, columns }, fields.len().min(2))
}

fn is_keyword(field: &str) -> bool {
    matches!(
        field.trim().to_ascii_lowercase().as_str(),
        "nomsqsave" | "controllerpriority" | "readonly"
    )
}

fn apply_keyword(flags: &mut ConstantFlags, field: &str) {
    match field.trim().to_ascii_lowercase().as_str() {
        "nomsqsave" => flags.no_msq_save = true,
        "controllerpriority" | "readonly" => flags.controller_priority = true,
        _ => {}
    }
}

/// Bit-option tables, shared between entries that use the same `$define`.
///
/// A `bits` entry's option list can be very large: Speeduino spells out all
/// 65 536 CAN addresses in `$CAN_ADDRESS_HEX`, and twenty entries reference
/// it. Parsing the list once per define keeps both the time and the memory
/// proportional to the definition instead of to the number of entries using
/// it, and the [`Arc`] makes the sharing cheap for a [`Definition`] clone.
#[derive(Debug, Default)]
struct OptionTables {
    by_define: BTreeMap<String, Arc<[BitOption]>>,
}

impl OptionTables {
    fn table(&mut self, define: Option<String>, fields: &[Cow<'_, str>]) -> Arc<[BitOption]> {
        let Some(define) = define else {
            return parse_bit_options(fields);
        };
        if let Some(table) = self.by_define.get(&define) {
            return Arc::clone(table);
        }
        let table = parse_bit_options(fields);
        self.by_define.insert(define, Arc::clone(&table));
        table
    }
}

/// The define a bit-option list consists of, when it is exactly one `$name`
/// field — the case worth sharing.
///
/// `index` is where the option list starts in the expanded fields; the raw
/// fields before it expand one-to-one for every entry this applies to, so
/// `raw.len() == index + 1` means the list is the expansion of the last field
/// and nothing else.
fn shared_define(raw_fields: &[String], index: usize) -> Option<String> {
    if raw_fields.len() != index.checked_add(1)? {
        return None;
    }
    let name = raw_fields.last()?.trim().strip_prefix('$')?.trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_ascii_lowercase())
}

fn parse_bit_options(fields: &[Cow<'_, str>]) -> Arc<[BitOption]> {
    let mut options = Vec::new();
    let mut next_value: u32 = 0;

    for field in fields {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        let (value, label) = match field.split_once('=') {
            Some((value, label)) => (
                value.trim().parse::<u32>().ok(),
                crate::fields::unquote(label).to_owned(),
            ),
            None => (None, crate::fields::unquote(field).to_owned()),
        };
        let assigned = value.unwrap_or(next_value);
        next_value = assigned.saturating_add(1);
        options.push(BitOption {
            value: assigned,
            hidden: label.eq_ignore_ascii_case("INVALID"),
            label,
        });
    }

    options.into()
}

/// `[PcVariables]`: the same syntax as constants, without an offset.
pub(crate) fn parse_pc_variables(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
    expander: &Expander<'_>,
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let fields = expander.expand(&split_fields(&value));
        if fields.is_empty() {
            continue;
        }

        let lowered = fields[0].trim().to_ascii_lowercase();
        if lowered == "channelvalueonconnect" || lowered == "continuouschannelvalue" {
            let channel = fields
                .get(1)
                .map(|field| crate::fields::unquote(field).to_owned())
                .unwrap_or_default();
            let is_connect = lowered == "channelvalueonconnect";
            let variable = PcVariable {
                name: key.clone(),
                class: ConstantClass::Scalar,
                data_type: DataType::F32,
                shape: Shape::Scalar,
                units: Text::Literal(String::new()),
                options: Arc::from([]),
                channel_value_on_connect: is_connect.then(|| channel.clone()),
                continuous_channel_value: (!is_connect).then_some(channel),
                line: *line,
                raw: value.clone(),
            };
            insert_variable(definition, section, variable, *line);
            section.understand(*line);
            continue;
        }

        let Some(class) = ConstantClass::parse(&lowered) else {
            section.warn(
                "unknown-class",
                format!(
                    "`{key}` starts with {:?}, which is not a PC variable class",
                    fields[0]
                ),
                *line,
            );
            continue;
        };
        let Some(data_type) = fields.get(1).and_then(|field| DataType::parse(field)) else {
            section.warn(
                "unknown-type",
                format!(
                    "`{key}` has data type {:?}",
                    fields.get(1).map_or("", AsRef::as_ref)
                ),
                *line,
            );
            continue;
        };

        let mut index = 2usize;
        let shape = match class {
            ConstantClass::Bits => {
                let shape = fields.get(index).and_then(|field| parse_bit_shape(field));
                index += 1;
                shape.unwrap_or(Shape::Scalar)
            }
            ConstantClass::Array | ConstantClass::OddArray => {
                let shape = fields.get(index).and_then(|field| parse_array_shape(field));
                index += 1;
                shape.unwrap_or(Shape::Scalar)
            }
            _ => Shape::Scalar,
        };

        let options: Arc<[BitOption]> = if class == ConstantClass::Bits {
            parse_bit_options(&fields[index..])
        } else {
            Arc::from([])
        };
        let units = if class == ConstantClass::Bits {
            Text::Literal(String::new())
        } else {
            fields
                .get(index)
                .map_or(Text::Literal(String::new()), |field| Text::parse(field))
        };

        let variable = PcVariable {
            name: key.clone(),
            class,
            data_type,
            shape,
            units,
            options,
            channel_value_on_connect: None,
            continuous_channel_value: None,
            line: *line,
            raw: value.clone(),
        };
        insert_variable(definition, section, variable, *line);
        section.understand(*line);
    }
}

fn insert_variable(
    definition: &mut Definition,
    section: &mut SectionParser,
    variable: PcVariable,
    line: u32,
) {
    let name = variable.name.clone();
    if definition.pc_variables.insert(&name, variable).is_some() {
        section.error(
            "duplicate-name",
            format!("`{name}` is defined more than once; the later entry wins"),
            line,
        );
    }
}
