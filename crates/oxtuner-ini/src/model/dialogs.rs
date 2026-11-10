/// The row kinds the dynamic dialog system defines.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialog grammar has a fixed set of row kinds; unknown text maps to Unknown"
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
            _ => Self::Unmodeled,
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
