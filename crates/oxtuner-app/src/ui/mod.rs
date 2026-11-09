//! Application components shared by the editor views.
//!
//! The views in this crate own their state; the modules here own the way a
//! document is drawn. Keeping the presentation together means two editors that
//! show the same kind of row cannot drift apart.

pub(crate) mod constant;
