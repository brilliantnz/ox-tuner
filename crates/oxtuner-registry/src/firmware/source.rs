//! Where one family's firmware images come from.
//!
//! No implementation ships yet: binary download URLs are still unverified for
//! the `https://` policy, and the application has no flash worker to consume
//! them. The trait records the shape implementations will take, so the cache
//! layout and the image type do not need to change when they arrive.

use crate::{definition::Family, firmware::types::FirmwareImage};

/// Builds firmware image URLs for one family.
pub trait FirmwareImageSource: Send + Sync {
    /// The family this source serves.
    fn family(&self) -> Family;

    /// The download URL for `image`, or nothing when the board is unknown.
    fn image_url(&self, image: &FirmwareImage) -> Option<String>;
}
