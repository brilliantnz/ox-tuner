use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::error::Diagnostic;
use crate::fields::CommandTemplate;

use super::{
    Channel, ChannelKind, CommsSettings, Constant, ConstantClass, ControllerCommand, Coverage,
    Curve, DatalogEntry, Dialog, Endianness, Extensions, FrontPage, Gauge, KeyAction, Menu, Number,
    Ordered, Page, PcVariable, ReferenceTable, SettingGroup, Table, UnsupportedSection,
};

/// The frozen result of parsing an INI file.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Definition {
    /// Firmware signature from `[MegaTune]` or `[TunerStudio]`.
    pub signature: String,
    /// Section the signature came from.
    pub signature_section: String,
    /// `iniVersion`, when the file declares one.
    pub ini_version: Option<String>,
    /// `iniSpecVersion`.
    pub spec_version: Option<f64>,
    /// Command template whose reply is the signature.
    pub query_command: Option<CommandTemplate>,
    /// Firmware version text.
    ///
    /// This is the `versionInfo` interrogate command (Speeduino answers
    /// `"S"`), not a description of the firmware.
    pub version_info: Option<String>,
    /// Byte order of calibration data.
    pub endianness: Endianness,
    /// Pages by number, 1-based.
    pub pages: Vec<Page>,
    /// Communication parameters.
    pub comms: CommsSettings,
    /// Calibration constants in load order.
    pub constants: Ordered<Constant>,
    /// PC-side variables in load order.
    pub pc_variables: Ordered<PcVariable>,
    /// Realtime channels in load order.
    pub channels: Ordered<Channel>,
    /// Tables in declaration order.
    pub tables: Ordered<Table>,
    /// Curves in declaration order.
    pub curves: Ordered<Curve>,
    /// Gauge templates, in category order.
    pub gauges: Vec<Gauge>,
    /// Default dashboard.
    pub front_page: FrontPage,
    /// Dialogs in declaration order.
    pub dialogs: Ordered<Dialog>,
    /// Top-level menus.
    pub menus: Vec<Menu>,
    /// Raw controller commands.
    pub controller_commands: Ordered<ControllerCommand>,
    /// Setting groups whose options are `#if` flags.
    pub setting_groups: Ordered<SettingGroup>,
    /// Per-constant help text.
    pub context_help: Ordered<String>,
    /// Datalog fields.
    pub datalog_entries: Vec<DatalogEntry>,
    /// Constant metadata applied after declaration.
    pub extensions: Extensions,
    /// Sensor calibration tables.
    pub reference_tables: Ordered<ReferenceTable>,
    /// Hidden-dialog hotkeys.
    pub key_actions: Vec<KeyAction>,
    /// Sections kept as raw data.
    pub unsupported: Vec<UnsupportedSection>,
    /// `#define` string lists, keyed lowercase.
    pub defines: BTreeMap<String, Vec<String>>,
    /// Flags the file asked about.
    pub tested_flags: BTreeSet<String>,
    /// Flags that were active for this parse.
    pub active_flags: BTreeSet<String>,
    /// Per-section coverage.
    pub coverage: Coverage,
    /// Everything that was adjusted, dropped or inferred.
    pub diagnostics: Vec<Diagnostic>,
}

impl Default for Definition {
    fn default() -> Self {
        Self::empty()
    }
}

/// A numeric PC-variable default converted to the byte the command expects.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "PC variables are byte-valued; the cast matches the dialect's truncation"
)]
fn variable_byte(value: f64) -> u8 {
    value as u8
}

impl Definition {
    /// An empty definition, used as a base by the parser.
    pub fn empty() -> Self {
        Self {
            signature: String::new(),
            signature_section: String::new(),
            ini_version: None,
            spec_version: None,
            query_command: None,
            version_info: None,
            endianness: Endianness::default(),
            pages: Vec::new(),
            comms: CommsSettings::default(),
            constants: Ordered::new(),
            pc_variables: Ordered::new(),
            channels: Ordered::new(),
            tables: Ordered::new(),
            curves: Ordered::new(),
            gauges: Vec::new(),
            front_page: FrontPage::default(),
            dialogs: Ordered::new(),
            menus: Vec::new(),
            controller_commands: Ordered::new(),
            setting_groups: Ordered::new(),
            context_help: Ordered::new(),
            datalog_entries: Vec::new(),
            extensions: Extensions::default(),
            reference_tables: Ordered::new(),
            key_actions: Vec::new(),
            unsupported: Vec::new(),
            defines: BTreeMap::new(),
            tested_flags: BTreeSet::new(),
            active_flags: BTreeSet::new(),
            coverage: Coverage::default(),
            diagnostics: Vec::new(),
        }
    }

    /// The page with this number, if declared.
    pub fn page(&self, number: u8) -> Option<&Page> {
        self.pages.iter().find(|page| page.number == number)
    }

    /// A constant by name.
    pub fn constant(&self, name: &str) -> Option<&Constant> {
        self.constants.get_ignore_case(name)
    }

    /// A channel by name.
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get_ignore_case(name)
    }

    /// Whether a definition that did not just come from the parser is still
    /// structurally usable.
    ///
    /// This is the gate for a cache hit: deserialization proves the bytes are
    /// a `Definition`, not that they are a *sane* one. The checks are the
    /// invariants every parse enforces and every consumer relies on, not a
    /// second full validation pass — a definition that fails them is rebuilt
    /// from its source instead of shown.
    pub fn is_usable(&self) -> bool {
        !self.signature.is_empty()
            && self
                .spec_version
                .is_none_or(|version| version <= crate::MAX_SPEC_VERSION)
    }

    /// A table by id.
    pub fn table(&self, id: &str) -> Option<&Table> {
        self.tables.get_ignore_case(id)
    }

    /// A raw value substituted for `$name` in a command.
    ///
    /// PC variables are byte-valued; a `bits` variable's value is the index of
    /// its selected option, which is how `tsCanId` becomes `0x00`.
    pub fn command_variable(&self, name: &str) -> Option<u8> {
        let variable = self.pc_variables.get_ignore_case(name)?;
        if let Some(value) = self
            .extensions
            .defaults
            .get_ignore_case(name)
            .and_then(Number::literal)
        {
            return Some(variable_byte(value));
        }
        match variable.class {
            ConstantClass::Bits => variable
                .options
                .iter()
                .find(|option| !option.hidden)
                .and_then(|option| u8::try_from(option.value).ok()),
            _ => Some(0),
        }
    }

    /// Diagnostics at [`crate::error::Severity::Error`] or above.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Error)
    }

    /// A stable, human-readable rendering of the parsed model.
    ///
    /// Golden tests compare this text, so a change in what the parser
    /// understands shows up as a reviewable diff rather than a silently
    /// different count. Order is the definition's own order, never a map's.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        self.write_identity(&mut out);
        self.write_pages(&mut out);
        self.write_constants(&mut out);
        self.write_channels(&mut out);
        self.write_tables(&mut out);
        self.write_tail(&mut out);
        out
    }

    fn write_identity(&self, out: &mut String) {
        let _ = writeln!(out, "signature|{}", self.signature);
        let _ = writeln!(out, "signature_section|{}", self.signature_section);
        let _ = writeln!(out, "spec|{:?}", self.spec_version);
        let _ = writeln!(out, "endianness|{}", self.endianness);
        let flags = self
            .active_flags
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        let _ = writeln!(out, "flags|{flags}");
    }

    fn write_pages(&self, out: &mut String) {
        let _ = writeln!(out, "pages|{}", self.pages.len());
        for page in &self.pages {
            let _ = writeln!(
                out,
                "page|{}|size={}|burn={}|read={}|writable={}",
                page.number,
                page.size,
                command_text(page.burn_command.as_ref()),
                command_text(page.read_command.as_ref()),
                page.is_writable()
            );
        }
    }

    fn write_constants(&self, out: &mut String) {
        let _ = writeln!(out, "constants|{}", self.constants.len());
        for (name, constant) in self.constants.iter() {
            let _ = writeln!(
                out,
                "constant|{name}|{:?}|{}|page={}|addr={:?}|shape={}|units={}|scale={}|translate={}",
                constant.class,
                constant.data_type,
                constant.page,
                constant.address,
                constant.shape,
                constant.units,
                constant
                    .scale
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                constant
                    .translate
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            );
        }
        let _ = writeln!(out, "pc_variables|{}", self.pc_variables.len());
        for (name, variable) in self.pc_variables.iter() {
            let _ = writeln!(
                out,
                "pc_variable|{name}|{:?}|{}|{}|units={}|scale={}|translate={}|min={}|max={}|\
                 digits={}|flags={:?}",
                variable.class,
                variable.data_type,
                variable.shape,
                variable.units,
                variable
                    .scale
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                variable
                    .translate
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                variable
                    .minimum
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                variable
                    .maximum
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                variable
                    .digits
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                variable.flags,
            );
        }
    }

    fn write_channels(&self, out: &mut String) {
        let _ = writeln!(out, "channels|{}", self.channels.len());
        for (name, channel) in self.channels.iter() {
            let kind = match &channel.kind {
                ChannelKind::Computed(_) => "computed".to_owned(),
                ChannelKind::Measured {
                    data_type,
                    offset,
                    bits,
                    ..
                } => match bits {
                    Some(bits) => {
                        format!("bits{}({}..{})@{offset}", bits.width(), bits.low, bits.high)
                    }
                    None => format!("{data_type}@{offset}"),
                },
            };
            let _ = writeln!(out, "channel|{name}|{kind}|units={}", channel.units);
        }
    }

    fn write_tables(&self, out: &mut String) {
        let _ = writeln!(out, "tables|{}", self.tables.len());
        for (id, table) in self.tables.iter() {
            let _ = writeln!(
                out,
                "table|{id}|x={}|y={}|z={}|title={}",
                table.x_bins.as_ref().map_or("", |axis| axis.array.as_str()),
                table.y_bins.as_ref().map_or("", |axis| axis.array.as_str()),
                table.z_bins.as_deref().unwrap_or(""),
                table.title
            );
        }
    }

    fn write_tail(&self, out: &mut String) {
        let _ = writeln!(out, "curves|{}", self.curves.len());
        let _ = writeln!(out, "gauges|{}", self.gauges.len());
        let _ = writeln!(out, "dialogs|{}", self.dialogs.len());
        let _ = writeln!(out, "menus|{}", self.menus.len());
        let _ = writeln!(out, "commands|{}", self.controller_commands.len());
        let _ = writeln!(out, "datalog|{}", self.datalog_entries.len());
        let _ = writeln!(out, "unsupported|{}", self.unsupported.len());
        for section in &self.unsupported {
            let _ = writeln!(out, "unsupported_section|{}", section.name);
        }
        let _ = writeln!(out, "coverage|{:.2}", self.coverage.percent());
        let errors = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Error)
            .count();
        let warnings = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Warning)
            .count();
        let _ = writeln!(out, "errors|{errors}");
        let _ = writeln!(out, "warnings|{warnings}");
    }
}

fn command_text(command: Option<&CommandTemplate>) -> String {
    match command {
        Some(command) if !command.is_empty() => command.raw.clone(),
        Some(_) => "<empty>".to_owned(),
        None => "<none>".to_owned(),
    }
}
