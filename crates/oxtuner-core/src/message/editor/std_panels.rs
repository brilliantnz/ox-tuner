//! Built-in `std_*` panels, composed from the loaded definition.
//!
//! Standard panels are application-built, not ini-defined: the definition only
//! names them (`panel = std_injection`, `panel = std_ms3Rtc {rtc_mode}`). This
//! registry maps a panel name to the constants `TunerStudio` shows in it, as
//! `(label, constant)` candidates in display order. Only candidates the loaded
//! definition actually declares become rows, so one registry serves Speeduino,
//! `MegaSquirt` and rusEFI definitions without per-ECU code. A name with no
//! usable candidate resolves to nothing and the caller keeps the
//! [`DialogSummary::unavailable`](super::dialog::DialogSummary::unavailable)
//! name instead.
//!
//! Labels are plain functional words ("Cylinder Count"), not copied artwork.

use oxtuner_project::Project;

/// One composable `std_*` panel: its name, title and candidate rows.
struct StdPanel {
    /// Standard name as written in the definition.
    name: &'static str,
    /// Panel title, used as the rows' section.
    title: &'static str,
    /// `(label, constant)` candidates in display order.
    rows: &'static [(&'static str, &'static str)],
}

/// The panels this build composes.
const PANELS: &[StdPanel] = &[
    StdPanel {
        name: "std_injection",
        title: "Injection Setup",
        rows: &[
            ("Control Algorithm", "algorithm"),
            ("Required Fuel", "reqFuel"),
            ("Cylinder Count", "nCylinders"),
            ("Engine Stroke", "twoStroke"),
            ("Engine Type", "engineType"),
            ("Injector Type", "injType"),
            ("Injector Divider", "divider"),
            ("Injection Timing", "alternate"),
            ("Injector Count", "nInjectors"),
            ("Injector Open Time", "injOpen"),
        ],
    },
    StdPanel {
        name: "std_ms3rtc",
        title: "Real Time Clock",
        rows: &[("RTC Trim", "rtc_trim")],
    },
];

/// The composable panel `name` names, with rows limited to constants the
/// project declares.
///
/// Matching is case-insensitive (`std_ms3Rtc` in the wild). A panel whose
/// rows all miss resolves to nothing: the dialog likely never had that
/// hardware, and an empty section would lie about it.
pub(crate) fn synthesize(
    project: &Project,
    name: &str,
) -> Option<(&'static str, Vec<(String, String)>)> {
    let panel = PANELS
        .iter()
        .find(|panel| panel.name.eq_ignore_ascii_case(name))?;

    let rows: Vec<(String, String)> = panel
        .rows
        .iter()
        .filter(|(_, constant)| project.layout().variable(constant).is_some())
        .map(|(label, constant)| ((*label).to_owned(), (*constant).to_owned()))
        .collect();

    (!rows.is_empty()).then_some((panel.title, rows))
}
