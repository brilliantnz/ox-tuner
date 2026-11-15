//! The rusEFI definition source.
//!
//! rusEFI publishes no version file. Definitions hang under an Apache
//! autoindex tree shaped
//! `{branch}/{YYYY}/{MM}/{DD}/{board}/{hash}.ini`, so the source walks that
//! tree newest-first and bounded. Every official definition is also derivable
//! from the signature instead: dots become slashes and `.ini` is appended,
//! under one base URL.

use crate::definition::{
    family::{Family, FirmwareId, family_of, parse_signature},
    source::FirmwareSource,
    types::VersionEntry,
};
use crate::error::{ErrorKind, RegistryError};
use crate::fetch::{HttpGet, MAX_LISTING_BYTES};

/// The root every official definition hangs under.
pub const INI_BASE_URL: &str = "https://rusefi.com/online/ini/";

/// The autoindex tree the listing walk starts at.
pub const AUTOINDEX_ROOT: &str = "https://rusefi.com/online/ini/rusefi";

/// How many branches the walk visits, `master` first.
const MAX_BRANCHES: usize = 4;

/// How many newest days per branch the walk visits.
const MAX_DAYS_PER_BRANCH: usize = 1;

/// How many hashes per board the walk visits.
const MAX_HASHES_PER_BOARD: usize = 4;

/// How many entries the walk collects before it stops.
const MAX_ENTRIES: usize = 300;

/// How many index pages one walk may read before it answers with what it has.
///
/// The budget bounds the work on a page that lists far more directories than a
/// real day does, so a hostile or broken index cannot keep the worker busy.
const MAX_PAGES: usize = 400;

/// The definition URL for `signature`, following the lookup rule.
///
/// `rusEFI master.2024.02.02.alphax-4chan.1293678056` becomes
/// `.../rusefi/master/2024/02/02/alphax-4chan/1293678056.ini`: the label is
/// lowercased, the space is a separator, and every dot is a separator too.
pub fn ini_url_for_signature(signature: &str) -> Option<String> {
    let (label, rest) = signature.trim().split_once([' ', '\t'])?;
    let rest = rest.trim();

    if !is_token(label) || rest.is_empty() {
        return None;
    }

    let mut url = String::with_capacity(INI_BASE_URL.len() + signature.len() + 4);
    url.push_str(INI_BASE_URL);
    url.push_str(&label.to_ascii_lowercase());
    url.push('/');
    for part in rest.split('.') {
        if !is_token(part) {
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

        (id.family() == Family::Rusefi).then_some(id)
    }

    fn ini_url(&self, id: &FirmwareId) -> Option<String> {
        if id.family() != Family::Rusefi || family_of(id.raw()) != Family::Rusefi {
            return None;
        }

        ini_url_for_signature(id.raw())
    }

    fn index_hint(&self) -> String {
        AUTOINDEX_ROOT.to_owned()
    }

    fn has_listing(&self) -> bool {
        true
    }

    fn fetch_listing(&self, fetch: &dyn HttpGet) -> Result<Vec<VersionEntry>, RegistryError> {
        crawl_newest(fetch)
    }
}

/// One link of an Apache autoindex page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AutoindexLink {
    /// The `href` exactly as written: `2026/` or `2719445755.ini`.
    href: String,
    /// Whether it points at a directory.
    is_dir: bool,
}

/// Reads the directory and file links of an autoindex page.
///
/// Parent links, sort links, icons and off-site URLs are dropped. Only
/// relative links survive, so every answer stays under the page it came from.
fn parse_autoindex_links(html: &str) -> Vec<AutoindexLink> {
    let mut links = Vec::new();
    let mut cursor = 0;

    while let Some(found) = html[cursor..].find("href=\"") {
        let start = cursor + found + "href=\"".len();
        let Some(end) = html[start..].find('"') else {
            break;
        };
        let href = &html[start..start + end];
        cursor = start + end;

        if href.starts_with("..") || href.starts_with('/') || href.contains("://") {
            continue;
        }

        if href.contains('?') {
            continue;
        }

        if let Some(name) = href.strip_suffix('/') {
            if is_token(name) {
                links.push(AutoindexLink {
                    href: href.to_owned(),
                    is_dir: true,
                });
            }
        } else if href.to_ascii_lowercase().ends_with(".ini") {
            let stem = &href[..href.len() - ".ini".len()];

            if is_token(stem) {
                links.push(AutoindexLink {
                    href: href.to_owned(),
                    is_dir: false,
                });
            }
        }
    }

    links
}

/// Whether `text` fits the upstream file and directory vocabulary.
fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Joins an autoindex `href` onto the page it came from.
fn join_url(base: &str, href: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        href.trim_start_matches('/')
    )
}

/// Fetches one autoindex page and returns its sorted, deduplicated names of
/// directories (`dirs`) or `.ini` files (otherwise).
///
/// One helper keeps the page fetch and parse in one place, so a change to the
/// page handling cannot drift between the two callers.
fn page_names(fetch: &dyn HttpGet, url: &str, dirs: bool) -> Result<Vec<String>, RegistryError> {
    let bytes = fetch.fetch(url, MAX_LISTING_BYTES)?;
    let html = String::from_utf8_lossy(&bytes);

    let mut names: Vec<String> = parse_autoindex_links(&html)
        .into_iter()
        .filter(|link| link.is_dir == dirs)
        .map(|link| {
            let name = link.href.trim_end_matches('/');
            name.to_owned()
        })
        .collect();
    names.sort();
    names.dedup();

    Ok(names)
}

/// Takes one page out of the walk budget.
///
/// The budget bounds remote-driven work: a page listing thousands of empty
/// directories costs one request each, and the entry cap cannot see them.
fn spend(budget: &mut usize) -> bool {
    let Some(left) = budget.checked_sub(1) else {
        return false;
    };

    *budget = left;
    true
}

/// The newest names first. Zero-padded dates sort newest-first this way.
fn newest_first(mut names: Vec<String>, take: usize) -> Vec<String> {
    names.sort();
    names.reverse();
    names.truncate(take);

    names
}

/// Whether `text` is `width` ASCII digits.
fn is_dated(text: &str, width: usize) -> bool {
    text.len() == width && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// Crawls the autoindex tree newest-first and bounded.
///
/// `master` goes first, then the remaining branches newest-name-first, one
/// newest day each with all its boards. The caps keep the walk to a few
/// hundred small requests at most; the caller caches the result for a day.
fn crawl_newest(fetch: &dyn HttpGet) -> Result<Vec<VersionEntry>, RegistryError> {
    let mut budget = MAX_PAGES;

    let mut branches = page_names(fetch, AUTOINDEX_ROOT, true)?;
    branches.retain(|branch| branch != "master");
    branches.sort();
    branches.reverse();

    let mut ordered = vec!["master".to_owned()];
    ordered.extend(branches);
    ordered.truncate(MAX_BRANCHES);

    let mut entries = Vec::new();

    for branch in ordered {
        if entries.len() >= MAX_ENTRIES {
            break;
        }

        crawl_branch(fetch, &branch, &mut entries, &mut budget)?;
    }

    // A listed family always publishes something. An empty walk means the
    // index did not answer as expected, and caching an empty answer as fresh
    // would replace a good listing for a whole TTL.
    if entries.is_empty() {
        return Err(RegistryError::new(
            ErrorKind::Network,
            "the listing walk found no entries",
        ));
    }

    Ok(entries)
}

/// Crawls one branch's newest day into `entries`.
fn crawl_branch(
    fetch: &dyn HttpGet,
    branch: &str,
    entries: &mut Vec<VersionEntry>,
    budget: &mut usize,
) -> Result<(), RegistryError> {
    let base = join_url(AUTOINDEX_ROOT, branch);

    if !spend(budget) {
        return Ok(());
    }

    let years: Vec<String> = newest_first(
        page_names(fetch, &base, true)?
            .into_iter()
            .filter(|name| is_dated(name, 4))
            .collect(),
        1,
    );

    let Some(year) = years.into_iter().next() else {
        return Ok(());
    };

    if !spend(budget) {
        return Ok(());
    }

    let months: Vec<String> = newest_first(
        page_names(fetch, &join_url(&base, &year), true)?
            .into_iter()
            .filter(|name| is_dated(name, 2))
            .collect(),
        1,
    );

    let Some(month) = months.into_iter().next() else {
        return Ok(());
    };

    if !spend(budget) {
        return Ok(());
    }

    let days: Vec<String> = newest_first(
        page_names(fetch, &join_url(&join_url(&base, &year), &month), true)?
            .into_iter()
            .filter(|name| is_dated(name, 2))
            .collect(),
        MAX_DAYS_PER_BRANCH,
    );

    for day in days {
        if entries.len() >= MAX_ENTRIES || !spend(budget) {
            break;
        }

        let day_url = join_url(&join_url(&base, &year), &format!("{month}/{day}"));
        let boards = page_names(fetch, &day_url, true)?;

        for board in boards {
            if entries.len() >= MAX_ENTRIES || !spend(budget) {
                break;
            }

            let hashes = match page_names(fetch, &join_url(&day_url, &board), false) {
                Ok(names) => newest_first(names, MAX_HASHES_PER_BOARD),
                Err(error) if matches!(error.kind(), ErrorKind::Network | ErrorKind::NotFound) => {
                    // One board that answers nothing must not cost the rest of
                    // the listing: skip it and keep walking.
                    tracing::debug!(
                        op = "registry-listing",
                        board,
                        code = error.kind().as_str(),
                        "board page skipped"
                    );

                    continue;
                }
                Err(error) => return Err(error),
            };

            for hash in hashes {
                let Some(hash) = hash
                    .strip_suffix(".ini")
                    .or_else(|| hash.strip_suffix(".INI"))
                else {
                    continue;
                };

                entries.push(VersionEntry {
                    version: format!("rusEFI {branch}.{year}.{month}.{day}.{board}.{hash}"),
                    stable: branch == "master",
                });

                if entries.len() >= MAX_ENTRIES {
                    break;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::error::RegistryError;

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
        assert_eq!(ini_url_for_signature("rusEFI master/../../etc.1"), None);
        assert_eq!(ini_url_for_signature("  "), None);
    }

    #[test]
    fn an_autoindex_page_yields_only_relative_entries() {
        let html = concat!(
            "<html><head><title>Index of /online/ini/rusefi/master/2026/11/15/uaefi/</title></head><body>",
            "<h1>Index of /online/ini/rusefi/master/2026/11/15/uaefi/</h1>",
            "<table><tr><td><a href=\"?C=N;O=D\">Name</a></td></tr>",
            "<tr><td><a href=\"/icons/blank.gif\">[ICO]</a></td></tr>",
            "<tr><td><a href=\"../\">Parent Directory</a></td></tr>",
            "<tr><td><a href=\"2719445755.ini\">2719445755.ini</a></td></tr>",
            "<tr><td><a href=\"https://elsewhere.example/x.ini\">x.ini</a></td></tr>",
            "</table></body></html>",
        );

        let links = parse_autoindex_links(html);

        assert_eq!(
            links,
            vec![AutoindexLink {
                href: "2719445755.ini".to_owned(),
                is_dir: false,
            }]
        );
    }

    #[test]
    fn branch_and_board_names_must_fit_the_upstream_vocabulary() {
        assert!(is_token("alphax-4chan"));
        assert!(is_token("hellenNA8_96"));
        assert!(!is_token(""));
        assert!(!is_token("bo ard"));
        assert!(!is_token("board.ini"));
        assert!(is_dated("2026", 4));
        assert!(!is_dated("26", 4));
        assert!(!is_dated("202x", 4));
    }

    /// A stub tree shaped like the server, to crawl without a network.
    struct TreeStub {
        pages: std::collections::HashMap<String, String>,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl TreeStub {
        fn page(names: &[&str]) -> String {
            use std::fmt::Write as _;

            let mut html = String::from("<html><body><table>");
            html.push_str("<tr><td><a href=\"../\">Parent Directory</a></td></tr>");
            for name in names {
                let _ = write!(html, "<tr><td><a href=\"{name}\">{name}</a></td></tr>");
            }
            html.push_str("</table></body></html>");
            html
        }
    }

    impl HttpGet for TreeStub {
        fn fetch(&self, url: &str, _max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
            self.calls.lock().expect("a test lock").push(url.to_owned());

            self.pages
                .get(url)
                .cloned()
                .map(String::into_bytes)
                .ok_or_else(|| RegistryError::new(ErrorKind::NotFound, "no such test page"))
        }

        fn download(
            &self,
            _url: &str,
            _max_bytes: usize,
            _progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<Vec<u8>, RegistryError> {
            Err(RegistryError::new(
                ErrorKind::Unsupported,
                "the listing test never downloads",
            ))
        }
    }

    #[test]
    fn one_unreadable_board_page_does_not_fail_the_listing() {
        use std::collections::HashMap;

        let mut pages = HashMap::new();
        pages.insert(AUTOINDEX_ROOT.to_owned(), TreeStub::page(&["master/"]));
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master"),
            TreeStub::page(&["2026/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026"),
            TreeStub::page(&["11/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11"),
            TreeStub::page(&["15/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15"),
            TreeStub::page(&["broken/", "uaefi/"]),
        );
        // `broken` has no page in the stub, so it answers NotFound.
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15/uaefi"),
            TreeStub::page(&["111.ini"]),
        );

        let stub = TreeStub {
            pages,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let entries = RusefiSource.fetch_listing(&stub).expect("a listing");

        assert_eq!(entries.len(), 1, "the readable board still lists");
        assert!(entries[0].version.ends_with("uaefi.111"));
    }

    #[test]
    fn a_walk_where_every_board_fails_is_an_error_not_an_empty_listing() {
        use std::collections::HashMap;

        let mut pages = HashMap::new();
        pages.insert(AUTOINDEX_ROOT.to_owned(), TreeStub::page(&["master/"]));
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master"),
            TreeStub::page(&["2026/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026"),
            TreeStub::page(&["11/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11"),
            TreeStub::page(&["15/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15"),
            TreeStub::page(&["broken/"]),
        );

        let stub = TreeStub {
            pages,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let error = RusefiSource
            .fetch_listing(&stub)
            .expect_err("an empty walk is a failure");

        assert_eq!(error.kind(), ErrorKind::Network);
    }

    #[test]
    fn the_page_budget_stops_a_walk_that_keeps_reading() {
        let mut budget = 2;

        assert!(spend(&mut budget));
        assert!(spend(&mut budget));
        assert!(!spend(&mut budget), "the budget is spent, not negative");
        assert_eq!(budget, 0);
    }

    #[test]
    fn an_exhausted_page_budget_returns_what_it_collected() {
        use std::collections::HashMap;

        let boards: Vec<String> = (0..MAX_PAGES + 20)
            .map(|index| format!("b{index}/"))
            .collect();
        let board_refs: Vec<&str> = boards.iter().map(String::as_str).collect();

        let mut pages = HashMap::new();
        pages.insert(AUTOINDEX_ROOT.to_owned(), TreeStub::page(&["master/"]));
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master"),
            TreeStub::page(&["2026/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026"),
            TreeStub::page(&["11/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11"),
            TreeStub::page(&["15/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15"),
            TreeStub::page(&board_refs),
        );
        // Only the first board answers; the rest are absent and skipped.
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15/b0"),
            TreeStub::page(&["111.ini"]),
        );

        let stub = TreeStub {
            pages,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let entries = RusefiSource
            .fetch_listing(&stub)
            .expect("a partial listing");

        assert_eq!(entries.len(), 1);
        let calls = stub.calls.lock().expect("a test lock").len();
        assert!(
            calls <= MAX_PAGES + 5,
            "the walk stops near its budget, got {calls} requests"
        );
    }

    #[test]
    fn the_crawl_walks_newest_first_and_capped() {
        use std::collections::HashMap;

        let mut pages = HashMap::new();
        pages.insert(
            AUTOINDEX_ROOT.to_owned(),
            TreeStub::page(&["master/", "older/", "zzz/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master"),
            TreeStub::page(&["2024/", "2026/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026"),
            TreeStub::page(&["01/", "11/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11"),
            TreeStub::page(&["14/", "15/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15"),
            TreeStub::page(&["uaefi/", "mre_f4/"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15/uaefi"),
            TreeStub::page(&["111.ini", "222.ini"]),
        );
        pages.insert(
            join_url(AUTOINDEX_ROOT, "master/2026/11/15/mre_f4"),
            TreeStub::page(&["333.ini"]),
        );
        pages.insert(join_url(AUTOINDEX_ROOT, "zzz"), TreeStub::page(&["2026/"]));
        pages.insert(join_url(AUTOINDEX_ROOT, "zzz/2026"), TreeStub::page(&[]));
        pages.insert(
            join_url(AUTOINDEX_ROOT, "older"),
            TreeStub::page(&["2024/"]),
        );
        pages.insert(join_url(AUTOINDEX_ROOT, "older/2024"), TreeStub::page(&[]));

        let stub = TreeStub {
            pages,
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let entries = RusefiSource.fetch_listing(&stub).expect("a listing");

        assert_eq!(entries.len(), 3, "two hashes plus one, stable master");
        assert!(
            entries.iter().all(|entry| entry.stable),
            "master builds are stable"
        );
        assert_eq!(entries[0].version, "rusEFI master.2026.11.15.mre_f4.333");
    }
}
