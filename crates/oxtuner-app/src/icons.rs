//! Icon bundle guard.
//!
//! The icon enum names the full Lucide catalog at compile time, but the
//! embedded `Assets` bundle only ships gpui-kit's default subset. Naming an
//! icon outside that subset compiles and then logs "could not find asset" at
//! runtime, rendering blank. This test scans the app's own sources for every
//! icon variant it uses and loads each one from the bundle, so a wrong icon
//! fails `cargo test` instead of a user's session.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Every `.rs` file under `src/`, with its text.
    fn sources() -> Vec<String> {
        fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).expect("source directory") {
                let path = entry.expect("source entry").path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push(path);
                }
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        files
            .into_iter()
            .filter_map(|path| fs::read_to_string(path).ok())
            .collect()
    }

    /// Every icon variant the sources mention.
    fn used_variants() -> Vec<String> {
        let mut variants = Vec::new();
        for text in sources() {
            let marker = "IconName::";
            let mut from = 0;
            while let Some(found) = text[from..].find(marker) {
                let mut variant = String::new();
                for character in text[from + found + marker.len()..].chars() {
                    if character.is_ascii_alphanumeric() {
                        variant.push(character);
                    } else {
                        break;
                    }
                }
                if !variant.is_empty()
                    && variant
                        .chars()
                        .next()
                        .is_some_and(|first| first.is_uppercase())
                {
                    variants.push(variant);
                }
                from += found + marker.len();
            }
        }
        variants.sort();
        variants.dedup();
        variants
    }

    /// `HardDrive` to `hard-drive`. A digit run takes a `-` prefix too, so
    /// `Settings2` also tries `settings-2`, which is how the bundle spells it.
    fn candidates(variant: &str) -> Vec<String> {
        let mut plain = String::new();
        for (index, character) in variant.chars().enumerate() {
            if character.is_ascii_uppercase() && index > 0 {
                plain.push('-');
            }
            plain.push(character.to_ascii_lowercase());
        }
        let mut dashed = String::new();
        let mut in_digits = false;
        for character in variant.chars() {
            if character.is_ascii_digit() {
                if !in_digits && !dashed.is_empty() {
                    dashed.push('-');
                }
                in_digits = true;
                dashed.push(character);
            } else {
                in_digits = false;
                if character.is_ascii_uppercase() && !dashed.is_empty() {
                    dashed.push('-');
                }
                dashed.push(character.to_ascii_lowercase());
            }
        }
        if dashed == plain {
            vec![plain]
        } else {
            vec![plain, dashed]
        }
    }

    /// An icon the app names must load from the embedded bundle.
    #[test]
    fn every_named_icon_is_in_the_embedded_bundle() {
        let variants = used_variants();
        assert!(!variants.is_empty(), "the scan found no icons at all");

        let mut missing = Vec::new();
        for variant in &variants {
            let found = candidates(variant).iter().any(|name| {
                gpui_kit::assets::Assets::get(format!("icons/{name}.svg").as_str()).is_some()
            });
            if !found {
                missing.push(variant.clone());
            }
        }

        assert!(
            missing.is_empty(),
            "icons that compile but are not in the embedded bundle: {missing:#?}"
        );
    }

    #[test]
    fn variant_spelling_covers_the_bundle() {
        assert_eq!(candidates("HardDrive"), vec!["hard-drive".to_owned()]);
        assert!(candidates("Settings2").contains(&"settings-2".to_owned()));
        assert!(candidates("PanelLeftClose").contains(&"panel-left-close".to_owned()));
    }
}
