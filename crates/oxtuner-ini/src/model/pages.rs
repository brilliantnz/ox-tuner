use crate::fields::CommandTemplate;

use super::{Number, Text};

/// One calibration page.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Page {
    /// Page number as written, 1-based.
    pub number: u8,
    /// Page size in bytes.
    pub size: u32,
    /// Base address added to every constant offset on this page.
    pub start_offset: u32,
    /// Two-byte page identifier used by `%2i`.
    pub identifier: Option<CommandTemplate>,
    /// Command template that reads part of this page.
    pub read_command: Option<CommandTemplate>,
    /// Command template that burns this page. `None` (or an empty command) means the
    /// page cannot be burned.
    pub burn_command: Option<CommandTemplate>,
    /// Command template that writes a single value.
    pub value_write: Option<CommandTemplate>,
    /// Command template that writes a chunk.
    pub chunk_write: Option<CommandTemplate>,
    /// Command template that returns the page's CRC32.
    pub crc_command: Option<CommandTemplate>,
}

impl Page {
    /// Whether the page can be burned at all.
    pub fn is_burnable(&self) -> bool {
        self.burn_command
            .as_ref()
            .is_some_and(|command| !command.is_empty())
    }

    /// Whether the page can be written at all.
    pub fn is_writable(&self) -> bool {
        let usable = |command: &Option<CommandTemplate>| {
            command.as_ref().is_some_and(|command| !command.is_empty())
        };
        usable(&self.value_write) || usable(&self.chunk_write)
    }
}

/// One axis of a table: an array constant plus the live channel that moves the
/// cursor.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    /// Array constant holding the axis values.
    pub array: String,
    /// Channel that tracks the current position.
    pub channel: Option<String>,
    /// Whether the axis is read-only in this view.
    pub read_only: bool,
}

/// A 2-D table from `[TableEditor]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Table {
    /// View id (also names the `.table` snapshot).
    pub id: String,
    /// 3-D dashboard id (also names the `.dash` file).
    pub map_id: Option<String>,
    /// Window title.
    pub title: Text,
    /// Page the cell array lives on, when declared.
    pub page: Option<u8>,
    /// X axis.
    pub x_bins: Option<Axis>,
    /// Y axis.
    pub y_bins: Option<Axis>,
    /// Cell array.
    pub z_bins: Option<String>,
    /// Axis label overrides.
    pub xy_labels: Vec<Text>,
    /// Wording for the 3-D vertical axis.
    pub up_down_label: Vec<Text>,
    /// 3-D camera height.
    pub grid_height: Option<Number>,
    /// 3-D camera angles (yaw, pitch, roll).
    pub grid_orient: Option<[Number; 3]>,
    /// Help topic.
    pub topic_help: Option<String>,
    /// Source line of the `table =` entry.
    pub line: u32,
}

/// A 1-D curve from `[CurveEditor]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Curve {
    /// Curve name.
    pub name: String,
    /// Window title.
    pub title: Text,
    /// Column labels.
    pub column_labels: Vec<Text>,
    /// X axis range (min, max, divisions).
    pub x_axis: Vec<Number>,
    /// Y axis range (min, max, divisions).
    pub y_axis: Vec<Number>,
    /// X bins, with optional tracking channel.
    pub x_bins: Option<Axis>,
    /// One entry per plotted curve.
    pub y_bins: Vec<Axis>,
    /// Per-curve line labels.
    pub line_labels: Vec<Text>,
    /// Optional gauge to draw.
    pub gauge: Option<String>,
    /// Whether text values are shown next to the graph.
    pub show_text_values: bool,
    /// Whether the graph is suppressed (table-only display).
    pub suppress_graph: bool,
    /// Source line.
    pub line: u32,
}
