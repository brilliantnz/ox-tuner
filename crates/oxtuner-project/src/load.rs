//! Loading `.msq` values into a [`Tune`], and formatting them back.
//!
//! The file stores **user-unit values keyed by name**; the tune stores raw
//! values. This module is the only place that crosses that boundary:
//!
//! - load: user text → [`VariableSpec::to_raw`] → [`Tune`];
//! - save: raw → [`VariableSpec::to_user`] → text.
//!
//! Bit fields are the exception: the file writes their **label**
//! (`"TPS"`), not a number, so the definition's option list is the mapping.
//! A quoted label is always looked up, even when it reads as a number, because
//! Speeduino names options `"1"`, `"30"` and `"2.0"`.
//! Text constants keep their quotes in the file and lose them in the tune.
//!
//! Presentation attributes are expressions over the file's own values, so a
//! scale such as `{fuelLoadRes}` resolves through computed channels, arrays and
//! the definition's channel-offset builtins before the layout is frozen.
//!
//! Every value that cannot be converted is skipped and reported; the working
//! tune keeps its definition default and the original text stays in the
//! document, so a save cannot silently lose it. A `<pageData>` block written by
//! another tool is cross-checked against the decoded constants and reported as
//! a warning, never used as a value source.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use oxtuner_ini::expr::EvalContext;
use oxtuner_ini::{ChannelKind, DataType, Definition};
use oxtuner_tune::{EntryShape, Owner, Tune, TuneLayout, Values, VariableSpec};

use crate::check::{IssueKind, LoadReport, check_page_data};
use crate::msq::{Entry, Msq};

/// A tune loaded from a `.msq`, with its frozen layout and the load report.
#[derive(Debug)]
pub struct LoadedTune {
    /// The working image, values in raw units.
    pub tune: Tune,
    /// The layout every value was interpreted through.
    pub layout: Arc<TuneLayout>,
    /// What the load gate found.
    pub report: LoadReport,
}

/// Why a tune value cannot be written back to text.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum FileValueError {
    /// A bit value has no label in the definition.
    NoLabel {
        /// Variable name.
        name: String,
        /// The raw value with no option.
        raw: f64,
    },
    /// The declared shape and the stored values disagree.
    WrongElementCount {
        /// Variable name.
        name: String,
        /// Values present in the tune.
        found: usize,
        /// Values the shape declares.
        expected: usize,
    },
    /// A text value cannot be written as it stands.
    Text {
        /// Variable name.
        name: String,
        /// Why.
        reason: String,
    },
}

impl fmt::Display for FileValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoLabel { name, raw } => {
                write!(formatter, "`{name}`: no option label for value {raw}")
            }
            Self::WrongElementCount {
                name,
                found,
                expected,
            } => write!(
                formatter,
                "`{name}`: {found} values in the tune, {expected} declared"
            ),
            Self::Text { name, reason } => write!(formatter, "`{name}`: {reason}"),
        }
    }
}

impl std::error::Error for FileValueError {}

/// Loads every value the definition and the file agree about.
pub fn load(msq: &Msq, definition: &Definition) -> LoadedTune {
    let mut report = crate::check::check(msq, definition);

    // Presentation expressions (`{fuelLoadRes}`, `{idleRes}`, …) are resolved
    // against the user-unit values in the file, so collect those before
    // building the layout.
    //
    // Two things the old code missed:
    // - bit fields are stored as labels (`"MAP"`), not numbers, so the
    //   definition's option list is the mapping back to a value;
    // - computed output channels (`fuelLoadRes`, `idleRes`, …) are expressions
    //   over those values, so a scale like `{fuelLoadRes}` needs one level of
    //   recursion, not just the raw file values.
    let values = collect_file_values(msq, definition);
    let resolver = Resolver::new(definition, &values.scalars, &values.arrays);
    let mut context = ExprContext {
        resolver: &resolver,
    };

    let layout = Arc::new(TuneLayout::build_with_context(definition, &mut context));
    if let Some(chain) = resolver.take_too_deep() {
        report.warn(
            IssueKind::Definition,
            format!(
                "computed channel chain `{chain}` is deeper than {MAX_EXPR_DEPTH}; \
                 the value stays unresolved"
            ),
        );
    }
    for issue in layout.issues() {
        report.warn(IssueKind::Definition, issue.to_string());
    }

    let mut tune = Tune::new(Arc::clone(&layout));

    for entry in msq.entries() {
        if entry.is_nameless() {
            continue;
        }
        let Some(index) = layout.index(&entry.name) else {
            // Orphans were reported once by the check layer.
            report.skipped += 1;
            continue;
        };
        let spec = &layout.variables()[index];

        match load_entry(&mut tune, spec, definition, &entry, &mut report) {
            Ok(()) => report.loaded += 1,
            Err(reason) => {
                report.skipped += 1;
                report.warn(
                    IssueKind::ValueSkipped,
                    format!("`{}`: {reason}; the definition default is used", entry.name),
                );
            }
        }
    }

    check_page_data(msq, definition, &layout, &tune, &mut report);

    LoadedTune {
        tune,
        layout,
        report,
    }
}

/// The values a `.msq` carries, in user units, keyed by lowercased name.
#[derive(Debug, Default)]
struct FileValues {
    /// Single numeric values and bit fields.
    scalars: HashMap<String, f64>,
    /// Arrays with more than one element, for `arrayValue`.
    arrays: HashMap<String, Vec<f64>>,
}

/// Collects the file values that presentation expressions can read.
///
/// Bit fields are stored as labels, so they are mapped back through the
/// definition's option list, the same way [`load_entry`] maps them on the way
/// into the tune. Multi-element arrays feed `arrayValue()` and `name[index]`;
/// a single-element array is also a scalar and lands in both maps.
fn collect_file_values(msq: &Msq, definition: &Definition) -> FileValues {
    let mut values = FileValues::default();

    for entry in msq.entries() {
        if entry.is_nameless() {
            continue;
        }

        let key = entry.name.to_ascii_lowercase();
        let mut tokens = entry.value.split_whitespace();
        let Some(first) = tokens.next() else {
            continue;
        };
        let second = tokens.next();

        // An array has more than one token and feeds `arrayValue`; a single
        // token is a scalar and takes the cheaper path below.
        if second.is_some() {
            let mut array = Vec::new();
            let mut valid = true;

            for token in std::iter::once(first).chain(second).chain(tokens) {
                if let Ok(value) = token.parse::<f64>() {
                    array.push(value);
                } else {
                    valid = false;
                    break;
                }
            }

            if valid {
                values.arrays.insert(key, array);
            }

            continue;
        }

        let text = unquote(&entry.value);

        if let Ok(value) = text.trim().parse::<f64>() {
            // A scalar number, or a bit field written as a bare number.
            values.scalars.insert(key, value);

            continue;
        }

        let options = match definition.constant(&entry.name) {
            Some(constant) => Some(&constant.options),
            None => definition
                .pc_variables
                .get(&entry.name)
                .map(|variable| &variable.options),
        };

        if let Some(options) = options {
            let trimmed = text.trim();

            if let Some(option) = options.iter().find(|option| option.label.trim() == trimmed) {
                let raw = f64::from(option.value);
                values.scalars.insert(key, raw);
            }
        }
    }

    values
}

/// The deepest computed-channel chain the resolver follows.
///
/// Real definitions chain a scale through one or two channels
/// (`{fuelLoadRes}` -> `algorithm`); the cap keeps a broken definition from
/// recursing without bound. The cycle guard catches a channel that names
/// itself, this cap catches a genuinely long chain.
const MAX_EXPR_DEPTH: usize = 4;

/// The lowercase form of `name`, borrowed when it already is lowercase.
///
/// Definitions spell identifiers in lower case most of the time, so this keeps
/// the common path allocation-free.
fn lower_key<'a>(name: &'a str, buffer: &'a mut String) -> &'a str {
    if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        buffer.clear();
        buffer.extend(name.chars().map(|character| character.to_ascii_lowercase()));
        buffer.as_str()
    } else {
        name
    }
}

/// Files values plus the definition's computed channels, resolved on demand.
struct Resolver<'a> {
    definition: &'a Definition,
    base: &'a HashMap<String, f64>,
    /// File arrays, keyed by lowercased name, for `arrayValue`.
    arrays: &'a HashMap<String, Vec<f64>>,
    /// The channels currently being resolved, outermost first.
    stack: std::cell::RefCell<Vec<String>>,
    /// The first chain that ran past [`MAX_EXPR_DEPTH`], for one warning.
    too_deep: std::cell::RefCell<Option<String>>,
}

impl<'a> Resolver<'a> {
    fn new(
        definition: &'a Definition,
        base: &'a HashMap<String, f64>,
        arrays: &'a HashMap<String, Vec<f64>>,
    ) -> Self {
        Self {
            definition,
            base,
            arrays,
            stack: std::cell::RefCell::new(Vec::new()),
            too_deep: std::cell::RefCell::new(None),
        }
    }

    /// Resolves one identifier: a file value first, then a computed channel.
    fn resolve(&self, name: &str) -> Option<f64> {
        let mut lowered = String::new();
        let key = lower_key(name, &mut lowered);

        if let Some(value) = self.base.get(key) {
            return Some(*value);
        }

        {
            let mut stack = self.stack.borrow_mut();

            if stack.iter().any(|seen| seen == key) {
                return None;
            }

            if stack.len() >= MAX_EXPR_DEPTH {
                let chain = stack
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(key))
                    .collect::<Vec<&str>>()
                    .join(" -> ");
                self.too_deep.borrow_mut().get_or_insert(chain);
                return None;
            }

            stack.push(key.to_owned());
        }

        let value = self.definition.channel(name).and_then(|channel| {
            let ChannelKind::Computed(expression) = &channel.kind else {
                return None;
            };

            expression.evaluate_with(&mut ExprContext { resolver: self })
        });

        self.stack.borrow_mut().pop();

        value
    }

    fn take_too_deep(&self) -> Option<String> {
        self.too_deep.borrow_mut().take()
    }

    /// Resolves `arrayValue(array.name, index)` and `name[index]`.
    ///
    /// The dialect prefixes the reference with `array.`; both spellings land on
    /// the same file array. A fraction index or a name the file does not carry
    /// stays unresolved.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "the index is checked non-negative and integral before the cast"
    )]
    fn array_value(&self, name: &str, index: f64) -> Option<f64> {
        if index < 0.0 || index.fract() != 0.0 {
            return None;
        }
        let index = usize::try_from(index as i64).ok()?;

        let mut lowered = String::new();
        let key = lower_key(name, &mut lowered);
        let key = key.strip_prefix("array.").unwrap_or(key);

        self.arrays.get(key)?.get(index).copied()
    }

    /// The `getChannel*ByOffset` builtins.
    ///
    /// A definition uses them so a curve adopts the scaling of whichever load
    /// channel the tuner picked. The value is the metadata of the scalar
    /// channel declared at that byte offset in the realtime block. An offset
    /// with no scalar channel stays unresolved, never guessed.
    ///
    /// Min and max are the channel's encodable physical range, and digits is
    /// zero, because the realtime grammar carries neither.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::as_conversions,
        reason = "the offset is checked non-negative and integral before the cast"
    )]
    fn channel_builtin(&self, name: &str, arguments: &[f64]) -> Option<f64> {
        let [offset] = arguments else {
            return None;
        };
        if *offset < 0.0 || offset.fract() != 0.0 {
            return None;
        }
        let offset = u32::try_from(*offset as i64).ok()?;

        let channel =
            self.definition
                .channels
                .iter()
                .find_map(|(_, channel)| match &channel.kind {
                    ChannelKind::Measured {
                        offset: at,
                        data_type,
                        bits: None,
                        ..
                    } if *at == offset => Some((channel, *data_type)),
                    _ => None,
                })?;
        let (channel, data_type) = channel;

        let scale = channel.scale.literal()?;
        let translate = channel.translate.literal()?;
        let (raw_min, raw_max) = raw_range(data_type)?;
        let (low, high) = ((raw_min + translate) * scale, (raw_max + translate) * scale);

        match name.to_ascii_lowercase().as_str() {
            "getchannelscalebyoffset" => Some(scale),
            "getchanneltranslatebyoffset" => Some(translate),
            "getchannelminbyoffset" => Some(low.min(high)),
            "getchannelmaxbyoffset" => Some(low.max(high)),
            "getchanneldigitsbyoffset" => Some(0.0),
            _ => None,
        }
    }
}

/// An [`EvalContext`] over a [`Resolver`].
struct ExprContext<'a, 'r> {
    resolver: &'r Resolver<'a>,
}

impl EvalContext for ExprContext<'_, '_> {
    fn value(&mut self, name: &str) -> Option<f64> {
        self.resolver.resolve(name)
    }

    fn array_value(&mut self, name: &str, index: f64) -> Option<f64> {
        self.resolver.array_value(name, index)
    }

    fn element(&mut self, name: &str, index: f64) -> Option<f64> {
        self.resolver.array_value(name, index)
    }

    fn function(&mut self, name: &str, arguments: &[f64]) -> Option<f64> {
        self.resolver.channel_builtin(name, arguments)
    }
}

/// The raw values a realtime storage type can hold.
fn raw_range(data_type: DataType) -> Option<(f64, f64)> {
    Some(match data_type {
        DataType::U08 | DataType::M08 => (0.0, 255.0),
        DataType::S08 => (-128.0, 127.0),
        DataType::U16 => (0.0, 65_535.0),
        DataType::S16 => (-32_768.0, 32_767.0),
        DataType::U32 => (0.0, 4_294_967_295.0),
        DataType::S32 => (-2_147_483_648.0, 2_147_483_647.0),
        DataType::F32 => (f64::from(f32::MIN), f64::from(f32::MAX)),
        DataType::U64 => (0.0, 18_446_744_073_709_551_615.0),
        DataType::S64 => (-9_223_372_036_854_775_808.0, 9_223_372_036_854_775_807.0),
        DataType::Ascii => return None,
    })
}

/// Applies the project's stored PC variable values on top of the loaded tune.
///
/// `pcVariableValues.msq` holds application-level values that live beside the
/// tune. Only names the layout owns as PC variables are applied; a calibration
/// constant written there is ignored, because the tune file is its source.
pub fn apply_pc_overrides(
    tune: &mut Tune,
    layout: &TuneLayout,
    definition: &Definition,
    values: &Msq,
    report: &mut LoadReport,
) {
    for entry in values.entries() {
        if entry.is_nameless() {
            continue;
        }

        let Some(index) = layout.index(&entry.name) else {
            continue;
        };
        let spec = &layout.variables()[index];

        if spec.owner != Owner::Pc {
            continue;
        }

        if let Err(reason) = load_entry(tune, spec, definition, &entry, report) {
            report.warn(
                IssueKind::ValueSkipped,
                format!(
                    "`{}`: the project override was skipped ({reason}); the tune value is kept",
                    entry.name
                ),
            );
        }
    }
}

fn load_entry(
    tune: &mut Tune,
    spec: &VariableSpec,
    definition: &Definition,
    entry: &Entry,
    report: &mut LoadReport,
) -> Result<(), String> {
    match spec.shape {
        EntryShape::Text { len } => {
            let text = unquote(&entry.value);
            if text.chars().count() > len {
                return Err(format!(
                    "the text is {} characters, the definition allows {len}",
                    text.chars().count()
                ));
            }
            tune.set_text(&spec.name, &text)
                .map_err(|error| error.to_string())
        }
        EntryShape::Bits { .. } => {
            let raw = label_to_raw(spec, definition, entry)?;
            tune.set_raw_at(&spec.name, 0, raw)
                .map_err(|error| error.to_string())
        }
        EntryShape::Scalar | EntryShape::Array { .. } => {
            let tokens = entry.tokens();
            let expected = spec.element_count();
            if tokens.len() != expected {
                return Err(format!(
                    "the file has {} values, the definition declares {expected}",
                    tokens.len()
                ));
            }
            for (index, token) in tokens.iter().enumerate() {
                let user = token
                    .parse::<f64>()
                    .map_err(|_| format!("`{token}` is not a number"))?;
                let conversion = spec.to_raw(user).map_err(|error| error.to_string())?;
                if conversion.is_out_of_range() {
                    report.warn(
                        IssueKind::ValueOutOfRange,
                        format!(
                            "`{}` = {user} is outside the declared range; it was kept, not clamped",
                            spec.name
                        ),
                    );
                }
                tune.set_raw_at(&spec.name, index, conversion.raw)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
    }
}

/// The bit value a label stands for.
///
/// A **quoted** label is always looked up in the definition's option list, even
/// when it reads as a number: Speeduino names options `"1"`, `"30"` and `"2.0"`,
/// and their values are list positions, not the text. A bare number is taken as
/// the field value directly. The option's index is its value when the dialect
/// did not spell one out, matching `TunerStudio`'s list semantics.
fn label_to_raw(
    spec: &VariableSpec,
    definition: &Definition,
    entry: &Entry,
) -> Result<f64, String> {
    let written = entry.value.trim();
    let text = unquote(written);

    if !is_quoted(written)
        && let Ok(number) = text.trim().parse::<f64>()
    {
        return Ok(number);
    }

    let options = match definition.constant(&spec.name) {
        Some(constant) => &constant.options,
        None => match definition.pc_variables.get(&spec.name) {
            Some(variable) => &variable.options,
            None => return Err("the definition has no bit options for this value".to_owned()),
        },
    };

    options
        .iter()
        .find(|option| option.label.trim() == text.trim())
        .map(|option| f64::from(option.value))
        .ok_or_else(|| format!("`{text}` is not one of the definition's labels"))
}

/// Whether the value text is wrapped in double quotes.
fn is_quoted(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"'
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        trimmed[1..trimmed.len() - 1].to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Formats a variable's stored values as the text a `.msq` carries.
///
/// # Errors
///
/// Returns [`FileValueError`] when text is too long or quoted, a bit value has
/// no label, or the shape and stored values disagree.
#[expect(
    clippy::float_cmp,
    reason = "a bit option matches its exact integer raw value"
)]
pub fn format_values(
    spec: &VariableSpec,
    definition: &Definition,
    values: &Values,
) -> Result<String, FileValueError> {
    match (&spec.shape, values) {
        (EntryShape::Text { len }, Values::Text(text)) => {
            if text.chars().count() > *len {
                return Err(FileValueError::Text {
                    name: spec.name.clone(),
                    reason: format!(
                        "the text is {} characters, the definition allows {len}",
                        text.chars().count()
                    ),
                });
            }
            if text.contains('"') {
                return Err(FileValueError::Text {
                    name: spec.name.clone(),
                    reason: "a quote cannot be represented in a .msq string".to_owned(),
                });
            }
            Ok(format!("\"{text}\""))
        }
        (EntryShape::Bits { .. }, Values::Numbers(numbers)) => {
            let raw = numbers.first().copied().unwrap_or(0.0);
            let options = definition
                .constant(&spec.name)
                .map(|constant| &constant.options)
                .or_else(|| {
                    definition
                        .pc_variables
                        .get(&spec.name)
                        .map(|variable| &variable.options)
                });
            let label = options.and_then(|options| {
                options
                    .iter()
                    .find(|option| f64::from(option.value) == raw)
                    .map(|option| option.label.as_str())
            });
            match label {
                Some(label) => Ok(format!("\"{label}\"")),
                None => Err(FileValueError::NoLabel {
                    name: spec.name.clone(),
                    raw,
                }),
            }
        }
        (shape, Values::Numbers(numbers)) => {
            let expected = shape.element_count();
            if numbers.len() != expected {
                return Err(FileValueError::WrongElementCount {
                    name: spec.name.clone(),
                    found: numbers.len(),
                    expected,
                });
            }
            let mut tokens = Vec::with_capacity(numbers.len());
            for raw in numbers {
                let text = spec
                    .format_file(*raw)
                    .map_err(|error| FileValueError::Text {
                        name: spec.name.clone(),
                        reason: error.to_string(),
                    })?;
                tokens.push(text);
            }
            if shape.is_scalar() {
                Ok(tokens.into_iter().next().unwrap_or_default())
            } else {
                let mut text = String::from("\n         ");
                for (index, token) in tokens.iter().enumerate() {
                    if index > 0 {
                        text.push_str("\n         ");
                    }
                    text.push_str(token);
                }
                text.push_str("\n      ");
                Ok(text)
            }
        }
        (_, Values::Text(_)) => Err(FileValueError::Text {
            name: spec.name.clone(),
            reason: "the definition expects a number".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use std::sync::OnceLock;

    use oxtuner_ini::{SettingsContext, parse_str};
    use oxtuner_tune::{CellState, TuneImages};

    use super::*;

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
aeMode = bits, U08, 1, [0:7], "TPS", "MAP", "Alpha-N"
note = string, ASCII, 8
table = array, U08, 4, [2x2], "", 1, 0, 0, 255, 0
canAddress = bits, U16, 10, [0:15], 256="0x100", 300="0x12C"
numbered = bits, U08, 12, [0:7], "1", "2", "3"

[PcVariables]
rpmhigh = scalar, U16, 0, "rpm", 1, 0, 0, 20000, 0
"#;

    const MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page>
        <pcVariable name="rpmhigh">12000.0</pcVariable>
    </page>
    <page number="0" size="64">
        <constant name="reqFuel" units="ms">15.4</constant>
        <constant name="aeMode">"MAP"</constant>
        <constant name="note">"NO2C"</constant>
        <constant cols="2" rows="2" name="table">1.0 2.0 3.0 4.0</constant>
        <constant name="canAddress">"0x12C"</constant>
        <constant name="numbered">"2"</constant>
    </page>
</msq>"#;

    /// A definition whose scale is a computed channel, so the layout can only
    /// resolve it from the file's own values.
    const CHAINED_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
algorithm = bits, U08, 0, [0:2], "MAP", "TPS", "MAF"
fuelLoadBins = array, U08, 1, [4], "kPa", {fuelLoadRes}, 0.0, 0.0, {fuelLoadMax}, 0

[OutputChannels]
ochGetCommand = "r\$tsCanId\x30%2o%2c"
ochBlockSize = 16
fuelLoadMax = { (algorithm == 0 || algorithm == 2) ? 511 : 100.0 }
fuelLoadRes = { ((algorithm == 0) || (algorithm == 2)) ? 2.000 : 0.500 }
"#;

    const CHAINED_MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="algorithm">"MAP"</constant>
        <constant cols="1" rows="4" name="fuelLoadBins">20 26 30 36</constant>
    </page>
</msq>"#;

    fn chained_definition() -> &'static Definition {
        static PARSED: OnceLock<Definition> = OnceLock::new();
        PARSED.get_or_init(|| {
            parse_str(CHAINED_DEFINITION, &SettingsContext::new()).expect("definition parses")
        })
    }

    #[test]
    fn a_bit_label_feeds_the_expressions_that_read_it() {
        // The file stores `"MAP"`, not `0`. Without the option-list mapping the
        // channel expressions see nothing and every axis bin falls back to its
        // definition default.
        let loaded = load(
            &Msq::parse(CHAINED_MSQ.as_bytes()).unwrap(),
            chained_definition(),
        );

        assert_eq!(loaded.tune.scalar_raw("algorithm").unwrap(), 0.0);
        assert!(
            loaded
                .report
                .warnings
                .iter()
                .all(|issue| { !issue.message.contains("presentation attribute") }),
            "the scale must resolve from the file values: {:?}",
            loaded.report.warnings
        );
    }

    #[test]
    fn a_computed_channel_scale_resolves_one_level_deep() {
        // `fuelLoadBins` is scaled by `{fuelLoadRes}`, which is a computed
        // channel over `algorithm`, which is a file value. The user value
        // `20` is raw `20 / 2 = 10`.
        let loaded = load(
            &Msq::parse(CHAINED_MSQ.as_bytes()).unwrap(),
            chained_definition(),
        );

        assert_eq!(loaded.tune.raw_at("fuelLoadBins", 0).unwrap(), 10.0);
        assert_eq!(loaded.tune.raw_at("fuelLoadBins", 1).unwrap(), 13.0);
        assert_eq!(loaded.tune.raw_at("fuelLoadBins", 3).unwrap(), 18.0);
    }

    #[test]
    fn a_computed_channel_scale_follows_the_file_choice() {
        let msq = CHAINED_MSQ.replace("\"MAP\"", "\"TPS\"");
        let loaded = load(&Msq::parse(msq.as_bytes()).unwrap(), chained_definition());

        assert_eq!(loaded.tune.scalar_raw("algorithm").unwrap(), 1.0);
        // TPS selects the 0.5 scale, so user `20` is raw `40`.
        assert_eq!(loaded.tune.raw_at("fuelLoadBins", 0).unwrap(), 40.0);
    }

    const DEEP_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
deep = scalar, U08, 0, "", {chain0}, 0, 0, 255, 0
selfRef = scalar, U08, 1, "", {loop}, 0, 0, 255, 0

[OutputChannels]
ochGetCommand = "r\$tsCanId\x30%2o%2c"
ochBlockSize = 16
chain0 = { chain1 }
chain1 = { chain2 }
chain2 = { chain3 }
chain3 = { chain4 }
chain4 = { 1 }
loop = { loop + 1 }
"#;

    const DEEP_MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="deep">1</constant>
        <constant name="selfRef">1</constant>
    </page>
</msq>"#;

    fn deep_definition() -> &'static Definition {
        static PARSED: OnceLock<Definition> = OnceLock::new();
        PARSED.get_or_init(|| {
            parse_str(DEEP_DEFINITION, &SettingsContext::new()).expect("definition parses")
        })
    }

    fn deep_loaded() -> LoadedTune {
        load(&Msq::parse(DEEP_MSQ.as_bytes()).unwrap(), deep_definition())
    }

    #[test]
    fn a_chain_past_the_depth_cap_is_reported_by_name() {
        let loaded = deep_loaded();

        let warning = loaded
            .report
            .warnings
            .iter()
            .find(|issue| issue.message.contains("deeper than"))
            .unwrap_or_else(|| panic!("no depth warning: {:?}", loaded.report.warnings));

        assert!(
            warning
                .message
                .contains("chain0 -> chain1 -> chain2 -> chain3 -> chain4"),
            "{}",
            warning.message
        );
    }

    #[test]
    fn a_self_referential_channel_is_unresolved_not_a_hang() {
        let loaded = deep_loaded();

        assert!(
            loaded.report.warnings.iter().any(|issue| {
                issue.message.contains("selfRef")
                    && issue.message.contains("presentation attribute")
            }),
            "{:?}",
            loaded.report.warnings
        );
    }

    const OFFSET_DEFINITION: &str = r#"
[MegaTune]
signature = "rusEFI test"

[Constants]
nPages = 1
pageSize = 64
page = 1
scaled = scalar, U08, 0, "", { getChannelScaleByOffset(6) * 2 }, 0, 0, 255, 0

[OutputChannels]
ochGetCommand = "r\$tsCanId\x30%2o%2c"
ochBlockSize = 16
map = scalar, U16, 6, "kPa", 2.000, 0.000
"#;

    const OFFSET_MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="rusEFI test"/>
    <page number="0" size="64">
        <constant name="scaled">8</constant>
    </page>
</msq>"#;

    #[test]
    fn a_channel_offset_builtin_adopts_the_channel_scaling() {
        // A generic curve asks the definition which scaling the load channel at
        // that offset uses. Here that is scale 2, doubled by the expression.
        let definition = parse_str(OFFSET_DEFINITION, &SettingsContext::new()).expect("parses");
        let loaded = load(&Msq::parse(OFFSET_MSQ.as_bytes()).unwrap(), &definition);
        assert_eq!(loaded.tune.raw_at("scaled", 0).unwrap(), 2.0);
        assert!(
            loaded
                .report
                .warnings
                .iter()
                .all(|issue| !issue.message.contains("presentation attribute")),
            "{:?}",
            loaded.report.warnings
        );
    }

    #[test]
    fn a_channel_offset_builtin_with_no_channel_stays_unresolved() {
        // Offset 6 resolves; an offset the definition never declares must not
        // invent a scale, so the constant keeps its definition default.
        let definition = parse_str(
            &OFFSET_DEFINITION.replace("getChannelScaleByOffset(6)", "getChannelScaleByOffset(99)"),
            &SettingsContext::new(),
        )
        .expect("parses");
        let loaded = load(&Msq::parse(OFFSET_MSQ.as_bytes()).unwrap(), &definition);

        assert!(
            loaded
                .report
                .warnings
                .iter()
                .any(|issue| issue.message.contains("presentation attribute")),
            "{:?}",
            loaded.report.warnings
        );
    }

    const ARRAY_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
alpha = array, U08, 0, [3], "", 1, 0, 0, 255, 0
viaFunction = array, U08, 4, [3], "", { arrayValue(array.alpha, 0) }, 0, 0, 255, 0
viaIndex = array, U08, 8, [3], "", { alpha[1] }, 0, 0, 255, 0
"#;

    const ARRAY_MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant cols="1" rows="3" name="alpha">2 5 6</constant>
        <constant cols="1" rows="3" name="viaFunction">10 10 10</constant>
        <constant cols="1" rows="3" name="viaIndex">10 10 10</constant>
    </page>
</msq>"#;

    fn array_loaded() -> LoadedTune {
        let definition = parse_str(ARRAY_DEFINITION, &SettingsContext::new()).expect("parses");
        load(&Msq::parse(ARRAY_MSQ.as_bytes()).unwrap(), &definition)
    }

    #[test]
    fn an_array_value_scale_reads_the_file_array() {
        let loaded = array_loaded();

        // `arrayValue(array.alpha, 0)` is 2, so user `10` is raw `5`.
        assert_eq!(loaded.tune.raw_at("viaFunction", 0).unwrap(), 5.0);
        // `alpha[1]` is 5, so user `10` is raw `2`.
        assert_eq!(loaded.tune.raw_at("viaIndex", 0).unwrap(), 2.0);
        assert!(
            loaded
                .report
                .warnings
                .iter()
                .all(|issue| !issue.message.contains("presentation attribute")),
            "{:?}",
            loaded.report.warnings
        );
    }

    /// The page bytes the chained fixture's constants decode to.
    fn chained_page() -> Vec<u8> {
        let mut page = vec![0u8; 64];
        page[1..5].copy_from_slice(&[10, 13, 15, 18]);
        page
    }

    fn with_page_data(msq: &str, bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02x}");
        }

        msq.replace(
            "<page number=\"0\" size=\"64\">",
            &format!("<pageData page=\"0\">{hex}</pageData><page number=\"0\" size=\"64\">"),
        )
    }

    #[test]
    fn a_page_data_block_that_agrees_is_silent() {
        let msq = with_page_data(CHAINED_MSQ, &chained_page());
        let definition = parse_str(CHAINED_DEFINITION, &SettingsContext::new()).expect("parses");
        let loaded = load(&Msq::parse(msq.as_bytes()).unwrap(), &definition);

        assert!(
            loaded
                .report
                .warnings
                .iter()
                .all(|issue| issue.kind != IssueKind::PageDataMismatch),
            "{:?}",
            loaded.report.warnings
        );
    }

    #[test]
    fn a_page_data_block_that_disagrees_names_the_values() {
        let mut bytes = chained_page();
        bytes[1] = 99;

        let msq = with_page_data(CHAINED_MSQ, &bytes);
        let definition = parse_str(CHAINED_DEFINITION, &SettingsContext::new()).expect("parses");
        let loaded = load(&Msq::parse(msq.as_bytes()).unwrap(), &definition);

        let warning = loaded
            .report
            .warnings
            .iter()
            .find(|issue| issue.kind == IssueKind::PageDataMismatch)
            .unwrap_or_else(|| panic!("no mismatch warning: {:?}", loaded.report.warnings));

        assert!(
            warning.message.contains("fuelLoadBins"),
            "{}",
            warning.message
        );
    }

    fn definition() -> &'static Definition {
        static PARSED: OnceLock<Definition> = OnceLock::new();
        PARSED.get_or_init(|| {
            parse_str(DEFINITION, &SettingsContext::new()).expect("definition parses")
        })
    }

    fn loaded() -> LoadedTune {
        load(&Msq::parse(MSQ.as_bytes()).unwrap(), definition())
    }

    #[test]
    fn user_units_become_raw_values() {
        let loaded = loaded();
        assert!(
            loaded.report.blockers.is_empty(),
            "{:?}",
            loaded.report.blockers
        );

        assert_eq!(loaded.tune.scalar_raw("reqFuel").unwrap(), 154.0);
        assert_eq!(loaded.tune.text("note").unwrap(), "NO2C");
        assert_eq!(loaded.tune.scalar_raw("rpmhigh").unwrap(), 12_000.0);
        assert_eq!(loaded.tune.raw_at("table", 3).unwrap(), 4.0);
    }

    #[test]
    fn a_bit_label_maps_through_the_option_list() {
        let loaded = loaded();
        assert_eq!(loaded.tune.scalar_raw("aeMode").unwrap(), 1.0);
    }

    #[test]
    fn a_label_past_the_first_byte_keeps_its_value() {
        // `bits` option values are not bytes: Speeduino's CAN address fields
        // use `[0:15]` with a 65 536-label list, so `0x12C` is value 300.
        let loaded = loaded();
        assert_eq!(loaded.tune.scalar_raw("canAddress").unwrap(), 300.0);

        let spec = loaded.layout.variable("canAddress").unwrap();
        let values = loaded.tune.value("canAddress").unwrap();
        assert_eq!(
            format_values(spec, definition(), values).unwrap(),
            "\"0x12C\""
        );
    }

    #[test]
    fn a_quoted_numeric_label_is_a_list_position_not_a_number() {
        // The file writes `"2"`, which is the second option of `"1", "2",
        // "3"`, so its value is 1. Reading the text as a number would pick 2.
        let loaded = loaded();

        assert_eq!(loaded.tune.scalar_raw("numbered").unwrap(), 1.0);
    }

    #[test]
    fn a_bare_number_is_the_field_value() {
        let msq = MSQ.replace(
            "<constant name=\"numbered\">\"2\"",
            "<constant name=\"numbered\">2",
        );
        let loaded = load(&Msq::parse(msq.as_bytes()).unwrap(), definition());

        assert_eq!(loaded.tune.scalar_raw("numbered").unwrap(), 2.0);
    }

    #[test]
    fn an_unknown_label_is_skipped_and_reported() {
        let msq = MSQ.replace("\"MAP\"", "\"Nope\"");
        let loaded = load(&Msq::parse(msq.as_bytes()).unwrap(), definition());

        assert_eq!(loaded.tune.scalar_raw("aeMode").unwrap(), 0.0);
        assert!(loaded.report.warnings.iter().any(|issue| {
            issue.kind == IssueKind::ValueSkipped && issue.message.contains("aeMode")
        }));
    }

    #[test]
    fn formatting_round_trips_every_shape() {
        let loaded = loaded();
        for name in ["reqFuel", "aeMode", "note", "table"] {
            let spec = loaded.layout.variable(name).unwrap();
            let values = loaded.tune.value(name).unwrap();
            let text = format_values(spec, definition(), values).unwrap();
            assert!(!text.is_empty(), "{name}");
        }
    }

    #[test]
    fn a_loaded_tune_is_the_new_baseline_and_has_no_ecu_state() {
        let loaded = loaded();
        let images = TuneImages::from_working(loaded.tune);
        assert_eq!(images.overall_state(), CellState::Unknown);
        assert!(images.status().unsaved.is_empty());
    }
}
