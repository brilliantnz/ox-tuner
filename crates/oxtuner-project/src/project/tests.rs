#![expect(
    clippy::unwrap_used,
    reason = "unit tests may unwrap; a failed call is a failed test"
)]

use std::fs;
use std::path::Path;

use super::open::settings_overrides;
use super::*;

#[test]
fn a_folder_without_projectcfg_is_not_a_project() {
    let path = Path::new("/definitely/not/a/project");
    assert!(matches!(
        Project::detect(path),
        Err(ProjectError::NotAProject { .. })
    ));
}

#[test]
fn a_tune_file_is_detected_case_insensitively() {
    assert_eq!(
        Project::detect(Path::new("/tmp/CurrentTune.MSQ")).unwrap(),
        ProjectKind::TuneFile(PathBuf::from("/tmp/CurrentTune.MSQ"))
    );
}

#[test]
fn settings_suffixes_map_to_booleans() {
    let overrides = settings_overrides(&[
        (
            "enablehardware_test_OFF".to_owned(),
            "enablehardware_test_OFF".to_owned(),
        ),
        ("CELSIUS".to_owned(), "CELSIUS".to_owned()),
    ]);
    assert!(overrides.contains(&("enablehardware_test".to_owned(), false)));
    assert!(overrides.contains(&("CELSIUS".to_owned(), true)));
}

#[test]
fn an_include_cannot_escape_its_directory() {
    let root = std::env::temp_dir().join(format!(
        "oxtuner-include-escape-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = root.join("projectCfg");
    fs::create_dir_all(config.join("engine")).unwrap();
    fs::write(root.join("secret.ini"), "signature = \"secret\"").unwrap();
    fs::write(config.join("real.ini"), "signature = \"real\"").unwrap();
    fs::write(config.join("engine/pins.inc"), "signature = \"pins\"").unwrap();
    // A literal backslash is an ordinary filename byte on Unix. Before
    // normalization this escape vector read the file below and looked like a
    // successful include; a refused resolution is what the assertion expects.
    fs::write(config.join("..\\secret.ini"), "signature = \"secret\"").unwrap();

    assert!(read_include(&config, "real.ini").is_some());
    assert!(
        read_include(&config, "engine\\pins.inc").is_some(),
        "a backslash spelling of a file inside the directory must resolve"
    );
    for name in [
        "../secret.ini",
        "..\\secret.ini",
        "/etc/passwd",
        "\\etc\\passwd",
        "\\\\server\\share\\secret.ini",
        "C:\\secret.ini",
    ] {
        assert_eq!(
            read_include(&config, name),
            None,
            "{name} must stay inside the definition directory"
        );
    }

    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_symlinked_include_cannot_escape_its_directory() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!(
        "oxtuner-include-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config = root.join("projectCfg");
    fs::create_dir_all(&config).unwrap();
    fs::write(root.join("secret.ini"), "signature = \"secret\"").unwrap();
    fs::write(config.join("real.ini"), "signature = \"real\"").unwrap();
    symlink(root.join("secret.ini"), config.join("link.ini")).unwrap();

    assert!(read_include(&config, "real.ini").is_some());
    assert_eq!(
        read_include(&config, "link.ini"),
        None,
        "a symlink that leaves the definition directory must not be read"
    );

    fs::remove_dir_all(&root).unwrap();
}
