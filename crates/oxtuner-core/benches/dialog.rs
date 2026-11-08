//! The dialog read path, including gate evaluation.
//!
//! The fixture is a synthetic definition with many gated rows. Each read goes
//! through the public command loop, so the number covers the whole path the UI
//! waits on: parse, flatten, evaluate gates, and reply.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-core --bench dialog
//! ```

use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_core::{BlockingEventStream, CoreConfig, CoreHandle, CoreThread, Event, start};
use oxtuner_project::TUNE_FILE;

const WAIT: Duration = Duration::from_secs(30);

fn definition() -> String {
    let mut ini = String::from(
        "[MegaTune]\nsignature = \"bench 1\"\nqueryCommand = \"Q\"\n\n\
         [Constants]\nnPages = 1\npageSize = 160\npage = 1\n",
    );

    for index in 0..64 {
        writeln!(
            ini,
            "flag{index} = scalar, U08, {index}, \"\", 1, 0, 0, 1, 0"
        )
        .expect("writing to a String cannot fail");
    }

    ini.push_str(
        "outputs = array, U08, 64, [64], \"\", 1, 0, 0, 255, 0\n\
         pinLayout = scalar, U08, 128, \"\", 1, 0, 0, 255, 0\n\n\
         [ConstantsExtensions]\ndefaultValue = pinLayout, 1\n\n\
         [OutputChannels]\nnFuelChannels = { arrayValue(array.outputs, pinLayout) }\n\n\
         [UserDefined]\ndialog = benchDialog, \"Bench\", yAxis\n",
    );

    for index in 0..128 {
        let flag = index % 64;
        let gate = match index % 4 {
            0 => format!("flag{flag}"),
            1 => format!("outputs[{flag}] > 0"),
            2 => format!("nFuelChannels >= {flag}"),
            _ => format!("flag{flag} && outputs[{flag}] > 0"),
        };

        writeln!(ini, "  field = \"Row {index}\", flag{flag}, {{ {gate} }}")
            .expect("writing to a String cannot fail");
    }

    ini.push_str("\n[Menu]\nmenuDialog = main\nmenu = \"Bench\"\n");
    for index in 0..64 {
        writeln!(
            ini,
            "  subMenu = benchDialog, \"Menu {index}\", {{ flag{index} }}"
        )
        .expect("writing to a String cannot fail");
    }

    ini
}

fn tune() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n\
     <msq xmlns=\"http://www.msefi.com/:msq\">\n\
     \x20   <bibliography author=\"bench\" tuneComment=\"bench\"/>\n\
     \x20   <versionInfo fileFormat=\"5.0\" firmwareInfo=\"bench\" nPages=\"1\" signature=\"bench 1\"/>\n\
     \x20   <page number=\"0\" size=\"160\">\n\
     \x20       <constant name=\"flag0\">0.0</constant>\n\
     \x20   </page>\n\
     </msq>\n"
}

struct Fixture {
    dir: PathBuf,
    handle: CoreHandle,
    _thread: CoreThread,
    events: BlockingEventStream,
}

impl Fixture {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "oxtuner-dialog-bench-{}-{stamp}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);

        fs::create_dir_all(dir.join("projectCfg")).expect("the bench project is creatable");
        fs::write(dir.join("projectCfg/mainController.ini"), definition())
            .expect("the bench definition is writable");
        fs::write(dir.join(TUNE_FILE), tune()).expect("the bench tune is writable");

        let parts = start(&CoreConfig::default()).expect("the core starts");
        let events = parts.events.into_blocking();
        parts
            .handle
            .open_project(&dir)
            .expect("the project open is queued");

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

    fn read_dialog(&self) {
        let op = self
            .handle
            .read_dialog("benchDialog")
            .expect("the read is queued");

        loop {
            let event = self.events.recv().expect("the core is alive");

            match event {
                Event::DialogRead {
                    op: answered,
                    dialog,
                } if answered == op => {
                    black_box(dialog);
                    return;
                }
                Event::Failed {
                    op: answered,
                    failure,
                } if answered == op => panic!("the dialog read failed: {failure:?}"),
                _ => {}
            }
        }
    }

    /// Edits one scalar and reads the dialog again.
    ///
    /// The read is queued after the edit; the core evaluates the menu after the
    /// edit and before it handles the read, so the round trip includes the menu
    /// refresh without depending on the `MenuChanged` variant existing on the
    /// baseline.
    fn edit_and_read(&self, value: f64) {
        let op = self
            .handle
            .edit_scalar("benchDialog", "flag0", value)
            .expect("the scalar edit is queued");

        loop {
            match self.events.recv().expect("the core is alive") {
                Event::DialogEdited { op: answered, .. } if answered == op => break,
                Event::Failed {
                    op: answered,
                    failure,
                } if answered == op => panic!("the scalar edit failed: {failure:?}"),
                _ => {}
            }
        }

        self.read_dialog();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn bench_dialog_read(criterion: &mut Criterion) {
    let fixture = Fixture::new();
    fixture.read_dialog();

    criterion.bench_function("core_dialog_read_gated_synthetic", |bencher| {
        bencher.iter(|| fixture.read_dialog());
    });
}

fn bench_dialog_edit(criterion: &mut Criterion) {
    let fixture = Fixture::new();
    let mut value = 0.0;

    criterion.bench_function("core_dialog_edit_gated_synthetic", |bencher| {
        bencher.iter(|| {
            value = 1.0 - value;
            fixture.edit_and_read(value);
        });
    });
}

criterion_group!(benches, bench_dialog_read, bench_dialog_edit);
criterion_main!(benches);
