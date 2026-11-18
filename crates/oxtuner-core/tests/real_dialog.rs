//! Reads the real Speeduino and rusEFI definitions through the core, when
//! available locally.
//!
//! The INIs are third-party files that ox-tuner does not bundle, so they are
//! never checked in. The tests look in the git-ignored study folder
//! (`docs/tmp/local/ini/`) or at the paths in `OXTUNER_INI_SPEEDUINO` and
//! `OXTUNER_INI_RUSEFI`. When a fixture is absent the test reports that it was
//! skipped. Set `OXTUNER_INI_REQUIRED=1` to turn a missing fixture into a hard
//! failure.

#![expect(
    clippy::print_stdout,
    reason = "the integration test prints a readable report for the developer running it"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, DefinitionView, DialogFieldKind,
    DialogFieldTone, DialogView, Event, ProjectView, start,
};
use oxtuner_ini::{SettingsContext, parse_bytes};

mod support;

const WAIT: Duration = Duration::from_secs(60);

fn locate(env: &str, fallback: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(env) {
        let path = PathBuf::from(path);

        if path.is_file() {
            return Some(path);
        }
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/tmp/local/ini")
        .join(fallback);

    fallback.is_file().then_some(fallback)
}

fn require(env: &str, fallback: &str) -> Option<(PathBuf, Vec<u8>)> {
    let path = locate(env, fallback)?;

    let required = std::env::var_os("OXTUNER_INI_REQUIRED").is_some_and(|value| value == "1");

    let Ok(bytes) = fs::read(&path) else {
        assert!(
            !required,
            "OXTUNER_INI_REQUIRED=1 but `{}` is unreadable",
            path.display()
        );
        return None;
    };

    Some((path, bytes))
}

fn require_ini() -> Option<(PathBuf, Vec<u8>)> {
    require("OXTUNER_INI_SPEEDUINO", "speeduino-202501.7.ini")
}

fn require_rusefi() -> Option<(PathBuf, Vec<u8>)> {
    require("OXTUNER_INI_RUSEFI", "rusefi.ini")
}

struct Harness {
    handle: CoreHandle,
    _thread: CoreThread,
    events: BlockingEventStream,
}

impl Harness {
    fn start() -> Self {
        let parts = start(&CoreConfig::default()).expect("the core starts");

        Self {
            handle: parts.handle,
            _thread: parts.thread,
            events: parts.events.into_blocking(),
        }
    }

    fn load_definition(&self, bytes: &[u8]) -> DefinitionView {
        self.handle
            .load_definition("speeduino.ini", bytes)
            .expect("the load is queued");
        // The core pushes its port snapshot on its own, so the answer is read
        // past those publications.
        let event = support::recv_non_port(&self.events, WAIT).expect("an event arrives");

        let Event::DefinitionLoaded { view: summary, .. } = event else {
            panic!("expected a definition load, got {event:?}");
        };

        summary
    }
}

struct RealProject {
    dir: PathBuf,
    handle: CoreHandle,
    _thread: CoreThread,
    events: BlockingEventStream,
    summary: ProjectView,
}

impl RealProject {
    fn open(bytes: &[u8], label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "oxtuner-core-real-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        fs::create_dir_all(dir.join("projectCfg")).expect("temporary project is creatable");
        fs::write(dir.join("projectCfg/mainController.ini"), bytes)
            .expect("the definition is writable");

        let definition =
            parse_bytes(bytes, &SettingsContext::new()).expect("the definition parses");
        let tune = format!(
            "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n\
             <msq xmlns=\"http://www.msefi.com/:msq\">\n\
             \x20   <bibliography author=\"test\" tuneComment=\"empty\"/>\n\
             \x20   <versionInfo fileFormat=\"5.0\" firmwareInfo=\"test\" nPages=\"{}\" signature=\"{}\"/>\n\
             </msq>\n",
            definition.pages.len(),
            definition.signature,
        );
        fs::write(dir.join("CurrentTune.msq"), tune).expect("the tune is writable");

        let parts = start(&CoreConfig::default()).expect("the core starts");
        let events = parts.events.into_blocking();
        parts.handle.open_project(&dir).expect("the open is queued");

        // The core pushes its port snapshot on its own, so the answer is read
        // past those publications.
        let event = support::recv_non_port(&events, WAIT).expect("an event arrives");
        let Event::ProjectOpened { view: summary, .. } = event else {
            panic!("expected a project open, got {event:?}");
        };

        Self {
            dir,
            handle: parts.handle,
            _thread: parts.thread,
            events,
            summary,
        }
    }

    fn summary(&self) -> &ProjectView {
        &self.summary
    }

    fn read_dialog(&self, name: &str) -> DialogView {
        self.handle.read_dialog(name).expect("the read is queued");
        // The core pushes its port snapshot on its own, so the answer is read
        // past those publications.
        let event = support::recv_non_port(&self.events, WAIT).expect("an event arrives");

        let Event::DialogRead { dialog, .. } = event else {
            panic!("expected a dialog read, got {event:?}");
        };

        dialog
    }
}

impl Drop for RealProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_real_engine_constants_dialog_flattens_every_panel() {
    let Some((path, bytes)) = require_ini() else {
        println!("skipped: no real Speeduino INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let project = RealProject::open(&bytes, "engine-constants");
    let dialog = project.read_dialog("engine_constants");

    let mut sections: Vec<&str> = dialog
        .fields
        .iter()
        .map(|field| field.section.as_str())
        .collect();
    sections.sort_unstable();
    sections.dedup();
    println!("sections: {sections:?}");

    for section in ["Speeduino Board", "Injection Setup", "Oddfire Angles"] {
        assert!(
            sections.contains(&section),
            "the {section} panel flattens into the dialog"
        );
    }

    assert!(
        dialog.unavailable.is_empty(),
        "every panel resolves or composes: {:?}",
        dialog.unavailable
    );
    assert_eq!(dialog.layout.as_deref(), Some("border"));

    let warning = dialog
        .fields
        .iter()
        .find(|field| field.kind == DialogFieldKind::Note && field.tone == DialogFieldTone::Danger)
        .expect("a conditional warning note is carried");
    assert_eq!(
        warning.visibility_expression.as_deref(),
        Some("injLayout == 3 && !sequentialFuelAvailable"),
        "`{{}}, {{}}, {{visible}}` keeps the visible expression in the visible slot: {warning:?}"
    );
    assert_eq!(warning.enable_expression, None);
    assert!(
        !warning.gates.visible,
        "the default injLayout is 0, so the sequential warning is hidden"
    );
    assert!(!warning.gates.unresolved);

    let layout = dialog
        .fields
        .iter()
        .find(|field| field.variable == "pinLayout")
        .expect("the board selector is present");
    assert!(
        !layout.options.is_empty(),
        "a bits row carries its select options"
    );

    let pairing = dialog
        .fields
        .iter()
        .find(|field| field.variable == "inj4CylPairing")
        .expect("the injector pairing row is present");
    assert_eq!(
        pairing.visibility_expression.as_deref(),
        Some("injLayout != 0 && nCylinders == 4"),
        "`{{}}, {{visible}}` is visibility, not enablement"
    );
    assert_eq!(pairing.enable_expression, None);
    assert!(
        !pairing.gates.visible,
        "the default injLayout is 0, so the pairing row is hidden"
    );
    assert!(!pairing.gates.unresolved);
    assert!(
        pairing.power_cycle,
        "`requiresPowerCycle = inj4CylPairing` marks the row"
    );
    assert!(
        layout.power_cycle,
        "`requiresPowerCycle = pinLayout` marks the row"
    );

    let stoich = dialog
        .fields
        .iter()
        .find(|field| field.variable == "stoich")
        .expect("the stoichiometric row is present");
    assert!(
        !stoich.power_cycle,
        "a constant absent from requiresPowerCycle stays unflagged"
    );

    println!(
        "fields: {}, tables: {}, curves: {}, widgets: {}, commands: {}, presets: {}",
        dialog.fields.len(),
        dialog.tables.len(),
        dialog.curves.len(),
        dialog.widgets.len(),
        dialog.actions.len(),
        dialog.presets.len(),
    );
}

#[test]
fn a_real_speeduino_dialog_resolves_an_array_value_gate() {
    let Some((path, bytes)) = require_ini() else {
        println!("skipped: no real Speeduino INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let project = RealProject::open(&bytes, "speeduino-array-gate");

    // `nFuelChannels` is a computed channel over `arrayValue`, so this gate
    // proves the array path on a real definition instead of failing open.
    let trim = project.read_dialog("inj_trim_enable");
    let enabled = trim
        .fields
        .iter()
        .find(|field| field.variable == "fuelTrimEnabled")
        .expect("the fuel trim enable row is present");

    assert!(
        !enabled.gates.enabled,
        "the default injLayout is not sequential"
    );
    assert!(enabled.gates.visible);
    assert!(
        !enabled.gates.unresolved,
        "`nFuelChannels` resolves through the computed channel and arrayValue"
    );
}

#[test]
fn a_real_speeduino_project_evaluates_menu_gates() {
    let Some((path, bytes)) = require_ini() else {
        println!("skipped: no real Speeduino INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let project = RealProject::open(&bytes, "speeduino-menu-gates");
    let menu = &project.summary().definition.menu;

    let trim = find_menu_node(menu, "Sequential fuel trim (5-8)")
        .expect("the sequential trim menu item is present");
    assert!(
        !trim.gates.enabled,
        "the default two-channel board cannot run channels 5-8"
    );
    assert!(trim.gates.visible);
    assert!(!trim.gates.unresolved, "nFuelChannels resolves");

    let can = find_menu_node(menu, "CAN Broadcasting menu")
        .expect("the CAN broadcasting item is present");
    assert!(!can.gates.enabled, "CAN is off by default");
    assert!(!can.gates.unresolved, "CANisAvailable resolves");

    let rtc =
        find_menu_node(menu, "Setup realtime clock").expect("the realtime clock item is present");
    assert!(
        !rtc.gates.enabled,
        "the default board layout does not carry an RTC"
    );
    assert!(
        !rtc.gates.unresolved,
        "boardHasRTC resolves through arrayValue"
    );
}

#[test]
fn a_real_speeduino_blank_row_reads_as_a_note() {
    let Some((path, bytes)) = require_ini() else {
        println!("skipped: no real Speeduino INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let project = RealProject::open(&bytes, "speeduino-note");

    let fan = project.read_dialog("fanSettings");
    let warning = fan
        .fields
        .iter()
        .find(|field| field.label == "No PWM Fan available on MCU")
        .expect("the blank display-only row reads as a note");

    assert_eq!(warning.kind, DialogFieldKind::Note);
    assert_eq!(warning.tone, DialogFieldTone::Danger);
}

#[test]
fn a_real_definition_reports_its_gauges_and_slots() {
    let Some((path, bytes)) = require_ini() else {
        println!("skipped: no real Speeduino INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let core = Harness::start();
    let summary = core.load_definition(&bytes);

    assert!(!summary.gauges.is_empty(), "gauge templates are served");
    assert!(
        summary
            .gauges
            .iter()
            .all(|gauge| !gauge.name.is_empty() && !gauge.channel.is_empty()),
        "every template names its channel"
    );
    assert!(
        !summary.front_page_gauges.is_empty(),
        "dashboard slots are served"
    );
    println!(
        "gauges: {}, slots: {}, unsupported: {:?}",
        summary.gauges.len(),
        summary.front_page_gauges.len(),
        summary.unsupported,
    );
}

fn find_menu_node<'tree>(
    nodes: &'tree [oxtuner_core::MenuNode],
    label: &str,
) -> Option<&'tree oxtuner_core::MenuNode> {
    for node in nodes {
        if node.label == label {
            return Some(node);
        }

        if let Some(found) = find_menu_node(&node.children, label) {
            return Some(found);
        }
    }

    None
}

#[test]
fn a_real_rusefi_definition_keeps_visibility_gates_and_widget_labels() {
    let Some((path, bytes)) = require_rusefi() else {
        println!("skipped: no real rusEFI INI (see the module docs)");
        return;
    };

    println!("definition: {}", path.display());
    let core = Harness::start();
    let summary = core.load_definition(&bytes);

    assert!(!summary.gauges.is_empty(), "gauge templates are served");

    // rusEFI writes `0, {visible}`; the zero is the empty enable placeholder.
    let protection = find_menu_node(&summary.menu, "Lambda protection")
        .expect("the lambda protection menu item is present");
    assert_eq!(
        protection.visibility_expression.as_deref(),
        Some("isInjectionEnabled"),
        "the second slot is visibility: {protection:?}"
    );
    assert_eq!(protection.enable_expression, None);

    // A `runtimeValue` labels its row from the first field.
    let project = RealProject::open(&bytes, "rusefi-runtime");
    let monitor = project.read_dialog("aePumpMonitor");
    let runtime = monitor
        .widgets
        .iter()
        .find(|widget| widget.kind == oxtuner_core::DialogWidgetKind::RuntimeValue)
        .expect("the monitor dialog carries a runtime value");
    assert_eq!(runtime.label, "Accumulated enrichment (ms/fraction)");
    assert_eq!(runtime.target, "accumulatedValue");
}
