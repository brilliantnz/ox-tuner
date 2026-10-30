//! Build steps for the application crate.

fn main() {
    // `rust_i18n::i18n!` reads `../../locales` while compiling, which the macro
    // does not tell Cargo about; without this, editing a locale file does not
    // trigger a rebuild.
    println!("cargo:rerun-if-changed=../../locales");
}
