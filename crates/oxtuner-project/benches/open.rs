//! MSQ and project benchmarks: XML parse, project open, tune save.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-project
//! ```
//!
//! The fixtures are synthetic and built in a temp dir. Never commit a real
//! vendor INI or `.msq` here.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_project::{Msq, Project};

const INI: &str = "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 288
page = 1
reqFuel = scalar, U16, 0, \"ms\", 0.1, 0, 0, 25.5, 1
rpmBins = array, U08, 2, [16], \"RPM\", 100, 0, 0, 25500, 0
veTable = array, U08, 18, [16x16], \"%\", 1, 0, 0, 255, 0
";

fn msq_bytes() -> Vec<u8> {
    let mut body = String::from(
        "<msq><versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"bench 1\"/><page number=\"0\" size=\"288\">",
    );

    body.push_str("<constant digits=\"1\" name=\"reqFuel\" units=\"ms\">10.0</constant>");

    let bins: Vec<String> = (0..16).map(|index| (index * 500).to_string()).collect();
    body.push_str(&format!(
        "<constant cols=\"16\" name=\"rpmBins\" rows=\"1\">{}</constant>",
        bins.join(" ")
    ));

    let cells: Vec<String> = (0..256).map(|index| (index % 100).to_string()).collect();
    body.push_str(&format!(
        "<constant cols=\"16\" name=\"veTable\" rows=\"16\">{}</constant>",
        cells.join(" ")
    ));

    body.push_str("</page></msq>");

    body.into_bytes()
}

/// A synthetic project folder: `projectCfg/mainController.ini` + `CurrentTune.msq`.
fn project_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("projectCfg");

    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(config.join("mainController.ini"), INI).expect("ini");
    std::fs::write(dir.path().join("CurrentTune.msq"), msq_bytes()).expect("msq");

    dir
}

fn bench_msq_parse(criterion: &mut Criterion) {
    let bytes = msq_bytes();

    criterion.bench_function("project_msq_parse_synthetic", |bencher| {
        bencher.iter(|| black_box(Msq::parse(black_box(&bytes)).expect("parses")));
    });
}

fn bench_project_open(criterion: &mut Criterion) {
    let dir = project_dir();

    criterion.bench_function("project_open_synthetic", |bencher| {
        bencher.iter(|| black_box(Project::open(black_box(dir.path())).expect("opens")));
    });
}

/// Serialising the document, with no filesystem in the way.
fn bench_msq_save(criterion: &mut Criterion) {
    let msq = Msq::parse(&msq_bytes()).expect("parses");

    criterion.bench_function("project_msq_save_synthetic", |bencher| {
        bencher.iter(|| black_box(msq.save().expect("saves")));
    });
}

/// The whole save path. This is IO bound: the file is written to a temp file,
/// flushed with `sync_all`, then renamed, so expect the number to follow the
/// disk rather than the CPU.
fn bench_project_save_noop(criterion: &mut Criterion) {
    let dir = project_dir();

    criterion.bench_function("project_save_unedited_synthetic", |bencher| {
        bencher.iter_with_setup(
            || Project::open(dir.path()).expect("opens"),
            |mut project| black_box(project.save().expect("saves")),
        );
    });
}

criterion_group!(
    benches,
    bench_msq_parse,
    bench_msq_save,
    bench_project_open,
    bench_project_save_noop
);
criterion_main!(benches);
