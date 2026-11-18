//! Opens a project **`TunerStudio` created**, when one is available locally.
//!
//! A real project is the compatibility benchmark: it was written by the other
//! application, so the folder layout, the definition, the `.msq` bytes and the
//! Java-properties file are all exactly what a user already has. The project is
//! third-party data and is never checked in (ADR-0006), so this test looks for
//! `OXTUNER_PROJECT` and reports a skip when it is unset:
//!
//! ```sh
//! OXTUNER_PROJECT="$HOME/Documents/OxTunerProjects/<project>" \
//!     cargo nextest run -p oxtuner-project --test real_project --no-capture
//! ```
//!
//! `OXTUNER_PROJECT_REQUIRED=1` turns a missing folder into a failure, for a
//! compatibility run that must not pass by testing nothing.

#![expect(
    clippy::print_stdout,
    reason = "the integration test prints a readable report for the developer running it"
)]

use std::path::PathBuf;

use oxtuner_project::{IssueKind, Msq, Project};

/// The folder named by `OXTUNER_PROJECT`, when it exists.
fn project_root() -> Option<PathBuf> {
    let root = PathBuf::from(std::env::var_os("OXTUNER_PROJECT")?);

    let required = std::env::var_os("OXTUNER_PROJECT_REQUIRED").is_some_and(|value| value == "1");
    assert!(
        !required || root.is_dir(),
        "OXTUNER_PROJECT_REQUIRED=1 but `{}` is not a folder; point OXTUNER_PROJECT at a \
         TunerStudio project (`CurrentTune.msq` + `projectCfg/`) or unset the variable",
        root.display()
    );

    root.is_dir().then_some(root)
}

fn skip() {
    println!("skipped: set OXTUNER_PROJECT to a project TunerStudio created");
}

#[test]
fn a_project_written_by_tunerstudio_opens_as_tunable_data() {
    let Some(root) = project_root() else {
        skip();
        return;
    };

    let project = Project::open(&root).expect("a TunerStudio project opens");
    let report = project.report();
    let tune_bytes = std::fs::read(project.tune_path()).expect("the tune is readable");
    let tune = Msq::parse(&tune_bytes).expect("the tune parses");
    let tune_signature = tune
        .version_info()
        .map_or_else(|| "(none)".to_owned(), |info| info.signature);

    println!("\n== {} ==", root.display());
    println!("in project     : {}", project.is_in_project());
    println!("definition     : {}", project.definition_path().display());
    println!("signature      : {}", project.definition().signature);
    println!("tune signature : {tune_signature}");
    println!("read only      : {}", project.is_read_only());
    println!("loaded         : {}", report.loaded);
    println!("skipped        : {}", report.skipped);
    println!("warnings       : {}", report.warnings.len());
    for warning in &report.warnings {
        println!("  warning {:?}: {}", warning.kind, warning.message);
    }
    println!("active flags   : {:?}", project.definition().active_flags);

    assert!(
        project.is_in_project(),
        "the folder is a project, not a bare tune"
    );
    assert!(
        project
            .definition_path()
            .starts_with(root.join("projectCfg")),
        "the definition is the one the project names: {}",
        project.definition_path().display()
    );
    assert!(project.definition_path().is_file());

    // When the project names its definition in `project.properties`, that is
    // the file that must have been loaded.
    let properties = std::fs::read(root.join("projectCfg/project.properties"))
        .ok()
        .map(|bytes| oxtuner_encoding::decode_latin1(&bytes));
    // Java properties lets a later duplicate key win, so the last one counts.
    let named = properties.as_deref().and_then(|properties| {
        properties
            .lines()
            .filter_map(|line| {
                line.split_once('=')
                    .filter(|(key, _)| key.trim().eq_ignore_ascii_case("ecuConfigFile"))
                    .map(|(_, value)| value.trim().to_owned())
            })
            .next_back()
    });
    if let Some(name) = named {
        assert_eq!(
            project
                .definition_path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some(name.as_str()),
            "the loaded definition is the one project.properties names"
        );
    }
    assert!(
        !project.is_read_only(),
        "the tune and the definition share a signature: {:?}",
        report.blockers
    );
    assert!(report.loaded > 0, "the tune carries values");

    // Every skipped value must be one the definition does not declare. A value
    // the definition does declare has to load, or the file and the INI disagree
    // in a way the user would see as a silently changed tune.
    let skipped_for_a_reason: Vec<&str> = report
        .warnings
        .iter()
        .filter(|issue| issue.kind == IssueKind::ValueSkipped)
        .map(|issue| issue.message.as_str())
        .collect();
    assert!(
        skipped_for_a_reason.is_empty(),
        "values the definition declares must load: {skipped_for_a_reason:?}"
    );
}

#[test]
fn the_tune_bytes_tunerstudio_wrote_survive_a_save_unchanged() {
    let Some(root) = project_root() else {
        skip();
        return;
    };

    let path = root.join("CurrentTune.msq");
    let bytes = std::fs::read(&path).expect("the tune is readable");

    let parsed = Msq::parse(&bytes).expect("the tune parses");
    let saved = parsed.save().expect("the tune serializes as ISO-8859-1");

    println!(
        "\n== {} ==\nbytes in: {} bytes out: {}",
        path.display(),
        bytes.len(),
        saved.len()
    );
    assert_eq!(
        saved, bytes,
        "an untouched tune must save byte for byte, or opening and saving would rewrite it"
    );
}

#[test]
fn a_snapshot_of_an_untouched_project_reloads_to_the_same_tune() {
    let Some(root) = project_root() else {
        skip();
        return;
    };

    let project = Project::open(&root).expect("a TunerStudio project opens");
    let snapshot = project.snapshot_msq().expect("the tune snapshots");
    let msq = Msq::parse(&snapshot).expect("the snapshot parses");
    let reloaded = oxtuner_project::load(&msq, project.definition());

    let values_skipped: Vec<&str> = reloaded
        .report
        .warnings
        .iter()
        .filter(|issue| issue.kind == IssueKind::ValueSkipped)
        .map(|issue| issue.message.as_str())
        .collect();
    println!(
        "\n== snapshot reload of {} ==\nloaded={} skipped={} warnings={} values_skipped={values_skipped:?}",
        root.display(),
        reloaded.report.loaded,
        reloaded.report.skipped,
        reloaded.report.warnings.len()
    );

    assert!(
        reloaded.report.blockers.is_empty(),
        "{:?}",
        reloaded.report.blockers
    );
    assert!(
        values_skipped.is_empty(),
        "a saved value must load again: {values_skipped:?}"
    );

    let mut compared = 0usize;
    for spec in project.layout().variables() {
        if spec.no_msq_save {
            continue;
        }
        let before = project.tune().value(&spec.name);
        let after = reloaded.tune.value(&spec.name);
        assert_eq!(
            before, after,
            "`{}` changed after a save, so editing the project rewrites it",
            spec.name
        );
        compared += 1;
    }

    println!("values compared: {compared}");
    assert!(compared > 0, "the definition declares values");
}

#[test]
fn the_settings_a_tunerstudio_project_names_are_active() {
    let Some(root) = project_root() else {
        skip();
        return;
    };

    let properties_path = root.join("projectCfg/project.properties");
    let Ok(bytes) = std::fs::read(&properties_path) else {
        println!("skipped: {} does not exist", properties_path.display());
        return;
    };
    let properties = oxtuner_encoding::decode_latin1(&bytes);

    let project = Project::open(&root).expect("a TunerStudio project opens");
    let settings = project.settings();

    let mut checked = 0usize;
    for line in properties.lines() {
        let line = line.trim_start();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("ecuSettings") {
            continue;
        }

        for item in value.split('|') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (name, active) = match item.to_ascii_lowercase() {
                lowered if lowered.ends_with("_off") => (&item[..item.len() - 4], false),
                _ => (item, true),
            };
            assert_eq!(
                settings.is_active(name),
                active,
                "`{name}` is listed in {properties_path:?} and must be {active}"
            );
            checked += 1;
        }
    }

    println!("\n== {properties_path:?} ==\necuSettings flags checked: {checked}");
    assert!(checked > 0, "the project names at least one flag");
}
