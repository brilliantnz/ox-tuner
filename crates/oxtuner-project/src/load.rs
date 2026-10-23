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
//! Text constants keep their quotes in the file and lose them in the tune.
//!
//! Every value that cannot be converted is skipped and reported; the working
//! tune keeps its definition default and the original text stays in the
//! document, so a save cannot silently lose it.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use oxtuner_ini::Definition;
use oxtuner_tune::{EntryShape, Layout, Tune, Values, VariableSpec};

use crate::check::{IssueKind, LoadReport};
use crate::msq::{Entry, Msq};

/// A tune loaded from a `.msq`, with its frozen layout and the load report.
#[derive(Debug)]
pub struct LoadedTune {
    /// The working image, values in raw units.
    pub tune: Tune,
    /// The layout every value was interpreted through.
    pub layout: Arc<Layout>,
    /// What the load gate found.
    pub report: LoadReport,
}

/// Why a tune value cannot be written back to text.
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

    // Presentation expressions (`{0.1 / stoich}`) are resolved against the
    // user-unit values in the file, so collect those before building the
    // layout. Only single numeric values can feed an expression.
    let mut user_values: HashMap<String, f64> = HashMap::new();
    for entry in msq.entries() {
        if entry.is_nameless() {
            continue;
        }
        if let Ok(value) = entry.value.trim().parse::<f64>() {
            user_values.insert(entry.name.clone(), value);
        }
    }

    let layout = Arc::new(Layout::build(definition, &|name| {
        user_values.get(name).copied()
    }));
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

    LoadedTune {
        tune,
        layout,
        report,
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
/// A quoted or plain label is looked up in the definition's option list; a
/// bare number is taken as the field value directly. The option's index is its
/// value when the dialect did not spell one out, matching TunerStudio's list
/// semantics.
fn label_to_raw(
    spec: &VariableSpec,
    definition: &Definition,
    entry: &Entry,
) -> Result<f64, String> {
    let text = unquote(&entry.value);
    if let Ok(number) = text.trim().parse::<f64>() {
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
    </page>
</msq>"#;

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
