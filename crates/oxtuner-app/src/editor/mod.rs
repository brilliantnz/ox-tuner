//! The editor column's views.
//!
//! A node's kind decides which view opens it: tables and curves share the grid
//! (a curve reads as a one-row table), dialogs get the scalar list, and
//! folders, built-in dialogs, help topics and missing targets keep the
//! placeholder.

pub(crate) mod dialog;
pub(crate) mod table;
