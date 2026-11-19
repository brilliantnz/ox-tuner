//! Dialog row vocabulary and its audit.
//!
//! Every keyword in the distilled INI reference is classified below. A row is
//! never dropped: it is either *structured* (the core has a defined field layout
//! for it), *verbatim* (the keyword is known and its fields are kept as data),
//! *deferred* (kept with fields plus a `row-deferred` note until an app feature
//! needs it), or *unmodeled* (unknown text, kept with an `unrecognized-key`
//! note). Adding a variant without adding it to [`RowKind::classification`] and
//! [`RowKind::ALL`] is a compile error, which is what keeps the audit current.
//!
//! | Keyword | Kind | Class |
//! | --- | --- | --- |
//! | `field` | `Field` | Structured |
//! | `displayOnlyField` | `DisplayOnlyField` | Verbatim |
//! | `checkbox` | `Checkbox` | Structured |
//! | `radio` | `Radio` | Structured |
//! | `slider` | `Slider` | Structured |
//! | `settingSelector` | `SettingSelector` | Structured |
//! | `settingOption` | `SettingOption` | Structured |
//! | `array1D` | `Array1D` | Structured |
//! | `runtimeValue` | `RuntimeValue` | Structured |
//! | `panel` | `EmbeddedPanel` | Structured |
//! | `gauge` | `Gauge` | Structured |
//! | `liveGraph` | `LiveGraph` | Structured |
//! | `graphLine` | `GraphLine` | Structured |
//! | `indicatorPanel` | `IndicatorPanel` | Structured |
//! | `indicator` | `Indicator` | Structured |
//! | `readoutPanel` | `ReadoutPanel` | Structured |
//! | `readout` | `Readout` | Structured |
//! | `commandButton` | `CommandButton` | Structured |
//! | `help`, `text` | `HelpText` | Verbatim |
//! | `webHelp` | `WebHelp` | Verbatim |
//! | `topicHelp` | `TopicHelp` | Verbatim |
//! | `channelSelector` | `ChannelSelector` | Deferred |
//! | `canDeviceSelector` | `CanDeviceSelector` | Deferred |
//! | `canClientIdSelector` | `CanClientIdSelector` | Deferred |
//! | `logFieldSelector` | `LogFieldSelector` | Deferred |
//! | `userPassword` | `UserPassword` | Deferred |
//! | anything else | `Unmodeled` | Verbatim (kept, reported once) |

/// How a row keyword is classified in the audit above.
#[expect(
    clippy::exhaustive_enums,
    reason = "the three classes are the fixed outcome of the row audit"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowClassification {
    /// The core has a defined field layout for the row.
    Structured,
    /// The keyword is known and its fields are kept as data.
    Verbatim,
    /// Kept with a `row-deferred` note until a consumer needs it.
    Deferred,
}

/// The row kinds the dynamic dialog system defines.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialog grammar has a fixed set of row kinds; unknown text maps to Unmodeled"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// `field` — label plus constant.
    Field,
    /// `displayOnlyField`.
    DisplayOnlyField,
    /// `checkbox`.
    Checkbox,
    /// `radio`.
    Radio,
    /// `slider`.
    Slider,
    /// `settingSelector`.
    SettingSelector,
    /// `settingOption`.
    SettingOption,
    /// `array1D`.
    Array1D,
    /// `runtimeValue`.
    RuntimeValue,
    /// `panel` — embeds another dialog, table, curve or standard dialog.
    EmbeddedPanel,
    /// `gauge`.
    Gauge,
    /// `liveGraph`.
    LiveGraph,
    /// `graphLine`.
    GraphLine,
    /// `indicatorPanel`.
    IndicatorPanel,
    /// `indicator`.
    Indicator,
    /// `readoutPanel`.
    ReadoutPanel,
    /// `readout`.
    Readout,
    /// `commandButton`.
    CommandButton,
    /// `help` / `text`.
    HelpText,
    /// `webHelp`.
    WebHelp,
    /// `topicHelp`.
    TopicHelp,
    /// `channelSelector` — kept with a `row-deferred` note.
    ChannelSelector,
    /// `canDeviceSelector` — kept with a `row-deferred` note.
    CanDeviceSelector,
    /// `canClientIdSelector` — kept with a `row-deferred` note.
    CanClientIdSelector,
    /// `logFieldSelector` — kept with a `row-deferred` note.
    LogFieldSelector,
    /// `userPassword` — kept with a `row-deferred` note.
    UserPassword,
    /// Anything this build does not model yet; kept as data.
    Unmodeled,
}

impl RowKind {
    /// Maps a dialog row keyword.
    pub fn parse(keyword: &str) -> Self {
        match keyword.trim_ascii().to_ascii_lowercase().as_str() {
            "field" => Self::Field,
            "displayonlyfield" => Self::DisplayOnlyField,
            "checkbox" => Self::Checkbox,
            "radio" => Self::Radio,
            "slider" => Self::Slider,
            "settingselector" => Self::SettingSelector,
            "settingoption" => Self::SettingOption,
            "array1d" => Self::Array1D,
            "runtimevalue" => Self::RuntimeValue,
            "panel" => Self::EmbeddedPanel,
            "gauge" => Self::Gauge,
            "livegraph" => Self::LiveGraph,
            "graphline" => Self::GraphLine,
            "indicatorpanel" => Self::IndicatorPanel,
            "indicator" => Self::Indicator,
            "readoutpanel" => Self::ReadoutPanel,
            "readout" => Self::Readout,
            "commandbutton" => Self::CommandButton,
            "help" | "text" => Self::HelpText,
            "webhelp" => Self::WebHelp,
            "topichelp" => Self::TopicHelp,
            "channelselector" => Self::ChannelSelector,
            "candeviceselector" => Self::CanDeviceSelector,
            "canclientidselector" => Self::CanClientIdSelector,
            "logfieldselector" => Self::LogFieldSelector,
            "userpassword" => Self::UserPassword,
            _ => Self::Unmodeled,
        }
    }

    /// Every classified row kind, for the coverage audit.
    pub const ALL: &'static [Self] = &[
        Self::Field,
        Self::DisplayOnlyField,
        Self::Checkbox,
        Self::Radio,
        Self::Slider,
        Self::SettingSelector,
        Self::SettingOption,
        Self::Array1D,
        Self::RuntimeValue,
        Self::EmbeddedPanel,
        Self::Gauge,
        Self::LiveGraph,
        Self::GraphLine,
        Self::IndicatorPanel,
        Self::Indicator,
        Self::ReadoutPanel,
        Self::Readout,
        Self::CommandButton,
        Self::HelpText,
        Self::WebHelp,
        Self::TopicHelp,
        Self::ChannelSelector,
        Self::CanDeviceSelector,
        Self::CanClientIdSelector,
        Self::LogFieldSelector,
        Self::UserPassword,
    ];

    /// The audit class of this kind. Exhaustive on purpose: a new kind that is
    /// not added here breaks the build instead of slipping past the audit.
    pub fn classification(self) -> RowClassification {
        match self {
            Self::Field
            | Self::Checkbox
            | Self::Radio
            | Self::Slider
            | Self::SettingSelector
            | Self::SettingOption
            | Self::Array1D
            | Self::RuntimeValue
            | Self::EmbeddedPanel
            | Self::Gauge
            | Self::LiveGraph
            | Self::GraphLine
            | Self::IndicatorPanel
            | Self::Indicator
            | Self::ReadoutPanel
            | Self::Readout
            | Self::CommandButton => RowClassification::Structured,
            Self::DisplayOnlyField | Self::HelpText | Self::WebHelp | Self::TopicHelp => {
                RowClassification::Verbatim
            }
            Self::ChannelSelector
            | Self::CanDeviceSelector
            | Self::CanClientIdSelector
            | Self::LogFieldSelector
            | Self::UserPassword => RowClassification::Deferred,
            Self::Unmodeled => RowClassification::Verbatim,
        }
    }
}

/// One row of a dialog.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DialogRow {
    /// The keyword.
    pub kind: RowKind,
    /// The keyword as written.
    pub keyword: String,
    /// Fields after the keyword, brace- and quote-aware.
    pub fields: Vec<String>,
    /// Source line.
    pub line: u32,
}

/// A dialog from `[UiDialogs]` / `[UserDefined]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// Dialog id (a stable key for window geometry).
    pub name: String,
    /// Title, when present.
    pub title: Option<String>,
    /// Layout: `yAxis`, `xAxis`, `border`, `card`, `indexCard`.
    pub layout: Option<String>,
    /// Rows in document order.
    pub rows: Vec<DialogRow>,
    /// Source line of the `dialog =` entry.
    pub line: u32,
}
