//! The rusEFI definition source.
//!
//! rusEFI publishes no version list. Every official definition is derived
//! from the signature instead: dots become slashes and `.ini` is appended,
//! under one base URL.

use crate::definition::{
    family::{Family, FirmwareId, family_of, parse_signature},
    source::FirmwareSource,
};

/// The root every official definition hangs under.
pub const INI_BASE_URL: &str = "https://rusefi.com/online/ini/";

/// The definition URL for `signature`, following the lookup rule.
///
/// `rusEFI master.2024.02.02.alphax-4chan.1293678056` becomes
/// `.../rusefi/master/2024/02/02/alphax-4chan/1293678056.ini`: the label is
/// lowercased, the space is a separator, and every dot is a separator too.
pub fn ini_url_for_signature(signature: &str) -> Option<String> {
    let (label, rest) = signature.trim().split_once([' ', '\t'])?;
    let rest = rest.trim();

    if label.is_empty() || rest.is_empty() {
        return None;
    }

    let mut url = String::with_capacity(INI_BASE_URL.len() + signature.len() + 4);
    url.push_str(INI_BASE_URL);
    url.push_str(&label.to_ascii_lowercase());
    url.push('/');
    for part in rest.split('.') {
        if part.is_empty() {
            return None;
        }
        url.push_str(part);
        url.push('/');
    }
    url.pop();
    url.push_str(".ini");

    Some(url)
}

/// Serves the rusEFI definitions.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RusefiSource;

impl FirmwareSource for RusefiSource {
    fn family(&self) -> Family {
        Family::Rusefi
    }

    fn parse(&self, signature: &str) -> Option<FirmwareId> {
        let id = parse_signature(signature).ok()?;

        (id.family == Family::Rusefi).then_some(id)
    }

    fn ini_url(&self, id: &FirmwareId) -> Option<String> {
        if id.family != Family::Rusefi || family_of(&id.raw) != Family::Rusefi {
            return None;
        }

        ini_url_for_signature(&id.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_claims_only_its_own_signatures() {
        let source = RusefiSource;
        let signature = "rusEFI master.2024.02.02.alphax-4chan.1293678056";

        assert!(source.parse(signature).is_some());
        assert!(source.parse("speeduino 202501.7").is_none());
        assert!(source.parse("").is_none());
    }

    #[test]
    fn the_lookup_rule_builds_the_documented_url() {
        let source = RusefiSource;
        let signature = "rusEFI master.2024.02.02.alphax-4chan.1293678056";
        let id = source.parse(signature).expect("parses");

        assert_eq!(
            source.ini_url(&id).expect("a url"),
            "https://rusefi.com/online/ini/rusefi/master/2024/02/02/alphax-4chan/1293678056.ini"
        );
    }

    #[test]
    fn broken_tails_build_no_url() {
        assert_eq!(ini_url_for_signature("rusEFI"), None);
        assert_eq!(ini_url_for_signature("rusEFI master..x"), None);
        assert_eq!(ini_url_for_signature("  "), None);
    }
}
