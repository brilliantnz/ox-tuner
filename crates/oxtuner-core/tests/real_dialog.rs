//! Reads the real Speeduino definition through the core, when available locally.
//!
//! The INI is a third-party file that ox-tuner does not bundle, so it is never
//! checked in. The test looks for it in the git-ignored study folder
//! (`docs/tmp/local/ini/`) or at the path in `OXTUNER_INI_SPEEDUINO`. When
//! neither exists the test reports that it was skipped. Set
//! `OXTUNER_INI_REQUIRED=1` to turn a missing fixture into a hard failure.

#![expect(
    clippy::print_stdout,
    reason = "the integration test prints a readable report for the developer running it"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, DefinitionSummary, DialogFieldKind,
    DialogSummary, Event, start,
};
use oxtuner_ini::{SettingsContext, parse_bytes};

const WAIT: Duration = Duration::from_secs(60);

fn locate_ini() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("OXTUNER_INI_SPEEDUINO") {
        let path = PathBuf::from(path);

        if path.is_file() {
            return Some(path);
        }
    }

    let fallback = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/tmp/local/ini/speeduino-202501.7.ini");

    fallback.is_file().then_some(fallback)
}

fn require_ini() -> Option<(PathBuf, Vec<u8>)> {
    let path = locate_ini()?;

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

    fn load_definition(&self, bytes: &[u8]) -> DefinitionSummary {
        self.handle
            .load_definition("speeduino.ini", bytes)
            .expect("the load is queued");
        let event = self.events.recv_timeout(WAIT).expect("an event arrives");

        let Event::DefinitionLoaded { summary, .. } = event else {
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

        let event = events.recv_timeout(WAIT).expect("an event arrives");
        assert!(
            matches!(event, Event::ProjectOpened { .. }),
            "expected a project open, got {event:?}"
        );

        Self {
            dir,
            handle: parts.handle,
            _thread: parts.thread,
            events,
        }
    }

    fn read_dialog(&self, name: &str) -> DialogSummary {
        self.handle.read_dialog(name).expect("the read is queued");
        let event = self.events.recv_timeout(WAIT).expect("an event arrives");

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
        .find(|field| field.kind == DialogFieldKind::Note && field.warning)
        .expect("a conditional warning note is carried");
    assert!(
        warning.enabled.is_some(),
        "the warning keeps its gate: {warning:?}"
    );

    let layout = dialog
        .fields
        .iter()
        .find(|field| field.variable == "pinLayout")
        .expect("the board selector is present");
    assert!(
        !layout.options.is_empty(),
        "a bits row carries its select options"
    );

    println!(
        "fields: {}, tables: {:?}, curves: {:?}, widgets: {}, commands: {}, presets: {}",
        dialog.fields.len(),
        dialog.tables,
        dialog.curves,
        dialog.widgets.len(),
        dialog.commands.len(),
        dialog.presets.len(),
    );
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
