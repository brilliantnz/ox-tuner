// Vendored from gpui-component 0.9.0; migrated to 0.11.0 (Apache-2.0).
// See NOTICE in this crate.

#![expect(
    missing_docs,
    missing_debug_implementations,
    clippy::all,
    clippy::pedantic,
    reason = "vendored gpui-component code stays close to upstream"
)]

mod tab;
mod tab_bar;

pub use tab::*;
pub use tab_bar::*;
