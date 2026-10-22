//! Infrastructure shared by the whole application.
//!
//! Things that are not features and not views: the platform-specific
//! filesystem layout and process-wide logging.

pub mod logging;
pub mod paths;
