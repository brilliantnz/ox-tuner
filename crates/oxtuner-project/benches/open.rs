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

use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_project::{DefinitionSource, Msq, Project};

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

/// Fifteen pages and 818 constants: the reference project's shape, used to
/// show what the definition cache actually skips.
fn reference_ini() -> String {
    const PAGES: usize = 15;
    const CONSTANTS: usize = 818;
    const TABLES: usize = 8;

    let scalars_per_page = (CONSTANTS - TABLES) / PAGES;
    let mut text = String::from(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 15
",
    );

    for _ in 0..PAGES {
        text.push_str("pageSize = 288\n");
    }

    for page in 1..=PAGES {
        let _ = writeln!(text, "page = {page}");

        for index in 0..scalars_per_page {
            let _ = writeln!(
                text,
                "scalar_{page}_{index} = scalar, U08, {}, \"\", 1, 0, 0, 255, 0",
                index % 240
            );
        }

        if page <= TABLES {
            let shape = if page == 1 { "[32x32]" } else { "[16x16]" };
            let _ = writeln!(
                text,
                "table_{page} = array, U08, 256, {shape}, \"\", 1, 0, 0, 255, 0"
            );
        }
    }

    text.push_str(
        "\
[OutputChannels]
rpm = scalar, U16, 0, \"RPM\", 1.0, 0.0
map = scalar, U16, 2, \"kPa\", 0.1, 0.0
clt = scalar, U16, 4, \"C\", 0.1, 0.0
afr = scalar, U16, 6, \"AFR\", 0.1, 0.0

[Menu]
menu = \"Fuel\"
subMenu = table_1, \"VE Table\"
menu = \"Ignition\"
subMenu = table_2, \"Advance\"
",
    );

    text
}

fn reference_msq() -> Vec<u8> {
    let mut body = String::from(
        "<msq><versionInfo fileFormat=\"5.0\" nPages=\"15\" signature=\"speeduino 202501\"/>",
    );

    for page in 0..15 {
        let _ = write!(body, "<page number=\"{page}\" size=\"288\">");
        if page == 0 {
            body.push_str("<constant name=\"scalar_1_0\">10.0</constant>");
        }
        body.push_str("</page>");
    }

    body.push_str("</msq>");
    body.into_bytes()
}

fn msq_bytes() -> Vec<u8> {
    let mut body = String::from(
        "<msq><versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"bench 1\"/><page number=\"0\" size=\"288\">",
    );

    body.push_str("<constant digits=\"1\" name=\"reqFuel\" units=\"ms\">10.0</constant>");

    let bins: Vec<String> = (0..16).map(|index| (index * 500).to_string()).collect();
    let _ = write!(
        body,
        "<constant cols=\"16\" name=\"rpmBins\" rows=\"1\">{}</constant>",
        bins.join(" ")
    );

    let cells: Vec<String> = (0..256).map(|index| (index % 100).to_string()).collect();
    let _ = write!(
        body,
        "<constant cols=\"16\" name=\"veTable\" rows=\"16\">{}</constant>",
        cells.join(" ")
    );

    body.push_str("</page></msq>");

    body.into_bytes()
}

/// A project whose definition and tune carry scaled PC variables, the shape a
/// real project uses for its AFR bins.
fn pc_variables_project() -> tempfile::TempDir {
    const VARIABLES: usize = 100;

    let mut ini = String::from(
        "\
[MegaTune]
signature = \"bench 1\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 288
page = 1
reqFuel = scalar, U16, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[PcVariables]
",
    );

    let mut body = String::from(
        "<msq><versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"bench 1\"/><page>",
    );

    for index in 0..VARIABLES {
        let _ = writeln!(
            ini,
            "bins_{index} = array, S16, [10], \"AFR\", 0.1, 0.0, -4.0, 4.0, 1"
        );
        let _ = write!(
            body,
            "<pcVariable cols=\"1\" digits=\"1\" name=\"bins_{index}\" rows=\"10\">\
             -1.5 -1.2 -1.0 -0.8 -0.6 -0.4 -0.2 -0.1 0.0 0.1</pcVariable>"
        );
    }

    body.push_str(
        "</page><page number=\"0\" size=\"288\"><constant name=\"reqFuel\">10.0</constant></page></msq>",
    );

    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("projectCfg");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(config.join("mainController.ini"), ini).expect("ini");
    std::fs::write(dir.path().join("CurrentTune.msq"), body).expect("msq");

    dir
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

/// The reference-shape project, with the definition cache kept outside it.
fn reference_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("projectCfg");

    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(config.join("mainController.ini"), reference_ini()).expect("ini");
    std::fs::write(dir.path().join("CurrentTune.msq"), reference_msq()).expect("msq");

    dir
}

fn cache_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("cache dir")
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

/// Scaled PC variables: their presentation attributes are resolved and their
/// values loaded, the path the AFR-bin fix changed.
fn bench_project_open_pc_variables(criterion: &mut Criterion) {
    let dir = pc_variables_project();

    criterion.bench_function("project_open_scaled_pc_variables", |bencher| {
        bencher
            .iter(|| black_box(Project::open(black_box(dir.path())).expect("the project opens")));
    });
}

/// The reference-shape definition with no cache: parse and resolve every time.
fn bench_project_open_reference(criterion: &mut Criterion) {
    let dir = reference_project();

    criterion.bench_function("project_open_reference_shape", |bencher| {
        bencher.iter(|| {
            black_box(Project::open(black_box(dir.path())).expect("the reference project opens"))
        });
    });
}

/// The same project with a valid cache entry: the definition is deserialized
/// instead of preprocessed and parsed.
fn bench_project_open_reference_warm(criterion: &mut Criterion) {
    let dir = reference_project();
    let cache = cache_dir();

    let first = Project::open_with_cache(dir.path(), Some(cache.path()))
        .expect("the reference project opens");
    assert_eq!(
        first.definition_source(),
        DefinitionSource::Parsed,
        "a cold open must parse the definition"
    );
    drop(first);

    let warm = Project::open_with_cache(dir.path(), Some(cache.path()))
        .expect("the reference project opens");
    assert_eq!(
        warm.definition_source(),
        DefinitionSource::Cache,
        "the warm bench must measure a hit"
    );
    drop(warm);

    criterion.bench_function("project_open_reference_shape_warm", |bencher| {
        bencher.iter(|| {
            black_box(
                Project::open_with_cache(black_box(dir.path()), Some(cache.path()))
                    .expect("the cached project opens"),
            )
        });
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
    bench_project_open_reference,
    bench_project_open_reference_warm,
    bench_project_open_pc_variables,
    bench_project_save_noop
);
criterion_main!(benches);
