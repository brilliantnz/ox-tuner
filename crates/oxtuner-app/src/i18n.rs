//! Localisation: ox-tuner's own text, in every language it ships.
//!
//! The text lives in `locales/*.yml`, one file per module, each key with its
//! `en` (and, when added, other) side by side. Code asks for a key with `t!`,
//! or `tn!` where a count picks the singular. A key names what the text is for
//! (`app.menu.file`), never the English sentence, and a sentence is always one
//! key: each language has its own word order.
//!
//! The interface language is chosen from the settings before the first window
//! is built, so text is translated where it is shown and a redraw brings a new
//! language. Entities that hand text to gpui-kit to keep (an input's
//! placeholder, a table's columns) give it again when [`UiLocale`] changes.
//!
//! Component strings (dialogs, list search boxes, the settings sidebar) come
//! from gpui-kit's own built-in locales and are not duplicated here.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui_kit::{Global, SharedString};

/// Every language there is text in. The first is the one the text is written
/// in, and what everything starts in until the settings are read.
pub const LOCALES: [&str; 1] = ["en"];

static LOCALE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// A thread's own language, once `isolate_thread` gave it one.
    static THREAD_LOCALE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The language ox-tuner's text is in now, one of [`LOCALES`].
pub fn locale() -> &'static str {
    let index = THREAD_LOCALE
        .with(Cell::get)
        .unwrap_or_else(|| LOCALE.load(Ordering::Relaxed));
    LOCALES[index]
}

/// Put ox-tuner's text, and gpui-kit's own, in `locale`. A language that is not
/// in [`LOCALES`] gives the language the text is written in.
pub fn set_locale(locale: &str) {
    let index = LOCALES
        .iter()
        .position(|known| *known == locale)
        .unwrap_or(0);
    if THREAD_LOCALE.with(|own| own.get().is_some()) {
        THREAD_LOCALE.with(|own| own.set(Some(index)));
        return;
    }
    LOCALE.store(index, Ordering::Relaxed);
    gpui_kit::component::set_locale(LOCALES[index]);
}

/// From now on this thread has a language of its own, which `set_locale`
/// changes without touching any other thread or gpui-kit's text. For UI tests:
/// they run side by side in one process, and one that switches language must
/// not switch the others.
#[doc(hidden)]
pub fn isolate_thread() {
    let current = LOCALES.iter().position(|known| *known == locale());
    THREAD_LOCALE.with(|own| own.set(current));
}

/// The interface language a window was last brought in line with. Entities
/// that keep translated text observe it and give the text again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiLocale(pub &'static str);

impl Global for UiLocale {}

/// The text of `key` in the interface language, with each `%{name}` in it
/// replaced, as a `SharedString`: `t!("app.menu.file")`,
/// `t!("app.about.version", version = "1.0")`.
macro_rules! t {
    ($key:expr $(,)?) => {
        crate::i18n::shared(crate::_rust_i18n_t!($key, locale = crate::i18n::locale()))
    };
    ($key:expr, $($args:tt)+) => {
        crate::i18n::shared(crate::_rust_i18n_t!(
            $key,
            locale = crate::i18n::locale(),
            $($args)+
        ))
    };
}
pub(crate) use t;

/// Text as gpui wants it, without copying what is already `'static`.
#[doc(hidden)]
pub fn shared(text: Cow<'static, str>) -> SharedString {
    match text {
        Cow::Borrowed(text) => SharedString::new_static(text),
        Cow::Owned(text) => text.into(),
    }
}

/// Whether the interface language has text for `key` of its own.
pub fn has(key: &str) -> bool {
    crate::_rust_i18n_backend()
        .translate(locale(), key)
        .is_some()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn messages(locale: &str) -> BTreeSet<String> {
        crate::_rust_i18n_backend()
            .messages_for_locale(locale)
            .unwrap_or_default()
            .into_iter()
            .map(|(key, _)| key.into_owned())
            .collect()
    }

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

    /// The string literal starting at `text[open..]`, where `text[open]` is the
    /// opening quote.
    fn literal(text: &str, open: usize) -> Option<String> {
        let mut literal = String::new();
        let mut escaped = false;
        for character in text[open + 1..].chars() {
            match character {
                _ if escaped => {
                    literal.push(character);
                    escaped = false;
                }
                '\\' => escaped = true,
                '"' => return Some(literal),
                _ => literal.push(character),
            }
        }
        None
    }

    /// Every key the code asks for, from both places a key is written: a `t!`
    /// literal, or a `label` field in the shortcut table.
    ///
    /// Two things keep the scan honest: the marker must not be the tail of a
    /// longer identifier (`_rust_i18n_t!` in this file) nor sit inside a string
    /// literal (this function's own `"t!("`), and the argument must be a string
    /// literal — `t!(self.label)` passes a key from the shortcut table.
    fn used_keys() -> BTreeSet<String> {
        let mut keys: BTreeSet<String> = crate::app::shortcuts::SHORTCUTS
            .iter()
            .map(|shortcut| shortcut.label.to_owned())
            .collect();

        for text in sources() {
            let marker = "t!(";
            let mut from = 0;
            while let Some(found) = text[from..].find(marker) {
                let at = from + found;
                let preceded_by_identifier = text[..at].chars().next_back().is_some_and(|before| {
                    before.is_alphanumeric() || before == '_' || before == '"'
                });
                let after = at + marker.len();
                let rest = &text[after..];
                let start = after + (rest.len() - rest.trim_start().len());
                if !preceded_by_identifier
                    && text[start..].starts_with('"')
                    && let Some(key) = literal(&text, start)
                {
                    keys.insert(key);
                }
                from = after;
            }
        }
        keys
    }

    #[test]
    fn an_unknown_locale_falls_back_to_english_without_touching_the_global() {
        std::thread::spawn(|| {
            isolate_thread();
            set_locale("zz");

            assert_eq!(locale(), "en");
            assert!(
                THREAD_LOCALE.with(Cell::get).is_some(),
                "the thread keeps a locale of its own"
            );
            assert_eq!(
                LOCALE.load(Ordering::Relaxed),
                0,
                "an isolated thread does not write the process locale"
            );
        })
        .join()
        .expect("the test thread runs");
    }

    /// A misspelt key renders the key itself instead of the sentence, and
    /// nothing fails at compile time. This catches it in CI.
    #[test]
    fn every_key_the_code_asks_for_exists() {
        let available = messages(locale());
        let mut missing = Vec::new();

        for key in used_keys() {
            if !available.contains(&key) {
                missing.push(key);
            }
        }

        assert!(missing.is_empty(), "missing locale keys: {missing:#?}");
    }

    /// A key nothing asks for is dead text the next translator keeps
    /// translating.
    #[test]
    fn every_locale_key_is_used() {
        let used = used_keys();
        let unused: Vec<String> = messages(locale())
            .into_iter()
            .filter(|key| !used.contains(key))
            .collect();

        assert!(unused.is_empty(), "unused locale keys: {unused:#?}");
    }

    #[test]
    fn english_is_the_only_locale() {
        assert_eq!(locale(), "en");
    }
}
