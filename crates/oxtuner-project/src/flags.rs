//! The `_ON` / `_OFF` suffix convention a `.msq` settings snapshot carries.
//!
//! `TunerStudio` writes `enablehardware_test_OFF` for a disabled boolean and
//! `CELSIUS` for an enabled choice. The snapshot is read when a project opens
//! and written back after a save, so both directions share this parser: a
//! suffix that is recognised on read but not on write would make the file and
//! the definition disagree about the same flag.

/// Strips `suffix` from `text`, ignoring ASCII case.
pub(crate) fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let start = text.len().checked_sub(suffix.len())?;
    let tail = text.get(start..)?;
    tail.eq_ignore_ascii_case(suffix).then(|| &text[..start])
}

/// Splits a setting name into the flag it selects and its state.
pub(crate) fn state(name: &str) -> (String, bool) {
    if let Some(stem) = strip_suffix_ci(name, "_OFF") {
        return (stem.to_owned(), false);
    }
    if let Some(stem) = strip_suffix_ci(name, "_ON") {
        return (stem.to_owned(), true);
    }
    (name.to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suffix_is_recognised_in_every_case() {
        for name in [
            "enablehardware_test_OFF",
            "enablehardware_test_off",
            "enablehardware_test_Off",
            "enablehardware_test_oFf",
        ] {
            assert_eq!(
                state(name),
                ("enablehardware_test".to_owned(), false),
                "{name}"
            );
        }
        assert_eq!(
            [
                state("CELSIUS_ON"),
                state("celsius_On"),
                state("CELSIUS_on")
            ],
            [
                ("CELSIUS".to_owned(), true),
                ("celsius".to_owned(), true),
                ("CELSIUS".to_owned(), true),
            ]
        );
    }
}
