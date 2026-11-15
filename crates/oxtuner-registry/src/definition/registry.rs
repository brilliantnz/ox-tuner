//! The definition sources this build knows.

use std::fmt;
use std::path::PathBuf;

use super::{
    family::Family,
    family::{FirmwareId, family_of},
    rusefi::RusefiSource,
    source::FirmwareSource,
    speeduino::SpeeduinoSource,
    types::{FetchedVersions, ResolvedDefinition, VersionEntry},
};
use crate::{
    cache::CacheStore,
    error::{ErrorKind, RegistryError},
    fetch::{HttpGet, MAX_INI_BYTES, VERSIONS_TTL_SECS},
};

/// The definition sources this build knows.
pub struct Registry {
    sources: Vec<Box<dyn FirmwareSource>>,
}

impl Registry {
    /// An empty registry, for callers that register their own sources.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// The shipped sources: Speeduino and rusEFI.
    pub fn with_defaults() -> Self {
        Self {
            sources: vec![Box::new(SpeeduinoSource), Box::new(RusefiSource)],
        }
    }

    /// Adds one more source. Sources added first are asked first.
    pub fn add(&mut self, source: impl FirmwareSource + 'static) {
        self.sources.push(Box::new(source));
    }

    /// Splits `signature` with the first source that claims it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::InvalidSignature`] when no source serves the text.
    pub fn parse_signature(&self, signature: &str) -> Result<FirmwareId, RegistryError> {
        for source in &self.sources {
            if let Some(id) = source.parse(signature) {
                return Ok(id);
            }
        }

        Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "no source serves this signature",
        ))
    }

    /// The definition URL for `signature`, or nothing when no source serves it.
    pub fn ini_url_for_signature(&self, signature: &str) -> Option<String> {
        let id = self.parse_signature(signature).ok()?;

        self.sources
            .iter()
            .find(|source| source.family() == id.family)?
            .ini_url(&id)
    }

    /// The version listing for `family`, or nothing when it publishes none.
    pub fn versions_url(&self, family: Family) -> Option<String> {
        self.sources
            .iter()
            .find(|source| source.family() == family)?
            .versions_url()
    }

    /// Resolves a signature to its id and download address.
    ///
    /// Pure: no network, no cache reads. This is the check the wizard runs on
    /// the signature the ECU reported before it downloads anything.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::InvalidSignature`] when no source serves the text,
    /// and [`ErrorKind::Unsupported`] when the family publishes no definition
    /// address.
    pub fn resolve(&self, signature: &str) -> Result<ResolvedDefinition, RegistryError> {
        for source in &self.sources {
            if let Some(id) = source.parse(signature) {
                let url = source.ini_url(&id).ok_or_else(|| {
                    RegistryError::new(
                        ErrorKind::Unsupported,
                        "this family has no downloadable definition",
                    )
                })?;

                return Ok(ResolvedDefinition { id, ini_url: url });
            }
        }

        // A known family no source serves yet (FOME today): the text names
        // something real, the registry just cannot fetch it.
        if family_of(signature) != Family::Unknown {
            return Err(RegistryError::new(
                ErrorKind::Unsupported,
                "this family has no downloadable definition",
            ));
        }

        Err(RegistryError::new(
            ErrorKind::InvalidSignature,
            "no source serves this signature",
        ))
    }

    /// The cached definition file for `id`, when the store already holds it.
    pub fn locate(&self, store: &CacheStore, id: &FirmwareId) -> Option<PathBuf> {
        let path = store.definition_ini(id);

        store.exists(&path).then_some(path)
    }

    /// The families with a browsable listing, Speeduino first.
    pub fn listing_families(&self) -> Vec<Family> {
        let mut families: Vec<Family> = self
            .sources
            .iter()
            .filter(|source| source.has_listing())
            .map(|source| source.family())
            .collect();
        families.sort_by_key(|family| u8::from(*family != Family::Speeduino));
        families.dedup();

        families
    }

    /// Lists the family's versions, preferring a fresh cache over the network.
    ///
    /// Runs on a worker thread: a stale or missing cache costs one index read.
    /// When the network fails but a stale listing exists, the stale listing
    /// is served with `stale` set instead of failing.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Unsupported`] when the family publishes no listing,
    /// and the fetch failure when the network cannot answer and no stale
    /// listing exists.
    pub fn fetch_versions(
        &self,
        fetch: &dyn HttpGet,
        store: &CacheStore,
        family: Family,
    ) -> Result<FetchedVersions, RegistryError> {
        let listed = self
            .sources
            .iter()
            .any(|source| source.family() == family && source.has_listing());

        if !listed {
            return Err(RegistryError::new(
                ErrorKind::Unsupported,
                "this family publishes no version list",
            ));
        }

        let path = store.versions_file(family);

        if let Some(entries) = load_cached_listing(store, &path) {
            return Ok(FetchedVersions {
                cached: true,
                stale: false,
                entries,
            });
        }

        match self.fetch_fresh(fetch, store, family, &path) {
            Ok(entries) => Ok(FetchedVersions {
                cached: false,
                stale: false,
                entries,
            }),
            Err(error) => {
                if let Some(entries) = load_stale_listing(store, &path) {
                    tracing::debug!(
                        op = "registry-fetch",
                        family = family.id(),
                        code = error.kind().as_str(),
                        "the listing refresh failed; serving the stale cache"
                    );

                    return Ok(FetchedVersions {
                        cached: true,
                        stale: true,
                        entries,
                    });
                }

                Err(error)
            }
        }
    }

    /// Reads the family's index over the network and caches the entries.
    fn fetch_fresh(
        &self,
        fetch: &dyn HttpGet,
        store: &CacheStore,
        family: Family,
        path: &std::path::Path,
    ) -> Result<Vec<VersionEntry>, RegistryError> {
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.family() == family && source.has_listing())
        else {
            return Err(RegistryError::new(
                ErrorKind::Unsupported,
                "this family publishes no version list",
            ));
        };

        let entries = source.fetch_listing(fetch)?;
        let bytes = serde_json::to_vec(&entries)
            .map_err(|error| RegistryError::new(ErrorKind::Cache, error.to_string()))?;
        store.store(path, &bytes)?;
        store.store_meta(
            path,
            &crate::cache::Meta::downloaded(&source.index_hint(), &bytes),
        );

        Ok(entries)
    }

    /// Downloads the definition into the store, reporting progress.
    ///
    /// Runs on a worker thread, never on the UI. The callback receives
    /// `(downloaded, total)` after every chunk; `total` is the server's
    /// content length when it sent one. The family directory is pruned to
    /// the newest entries afterwards, so repeated downloads of new versions
    /// do not grow the cache without bound.
    ///
    /// # Errors
    ///
    /// Returns the fetch failure, or the cache failure when the bytes cannot
    /// be stored.
    pub fn download_definition(
        &self,
        fetch: &dyn HttpGet,
        store: &CacheStore,
        id: &FirmwareId,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PathBuf, RegistryError> {
        let resolved = self.resolve(&id.raw)?;

        Self::download_resolved(fetch, store, &resolved, progress)
    }

    /// Downloads an already-resolved definition and stores it.
    fn download_resolved(
        fetch: &dyn HttpGet,
        store: &CacheStore,
        resolved: &ResolvedDefinition,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PathBuf, RegistryError> {
        let path = store.definition_ini(&resolved.id);

        let bytes = fetch.download(&resolved.ini_url, MAX_INI_BYTES, progress)?;
        store.store(&path, &bytes)?;
        store.store_meta(
            &path,
            &crate::cache::Meta::downloaded(&resolved.ini_url, &bytes),
        );

        if let Some(dir) = path.parent() {
            store.prune(dir, "ini", crate::cache::KEEP_FILES);
        }

        Ok(path)
    }

    /// Hands out the definition file for `id`, downloading it when needed.
    ///
    /// This is the call the new-project flow makes with the signature the
    /// ECU reported: a verified cache hit answers with no network, a miss or
    /// a tampered file downloads again, and the returned path is what
    /// `oxtuner-project` builds `mainController.ini` from. Progress reports
    /// only on the download path; a hit answers silently.
    ///
    /// # Errors
    ///
    /// Returns the resolve, fetch or cache failure when no usable file can
    /// be produced.
    pub fn ensure_definition(
        &self,
        fetch: &dyn HttpGet,
        store: &CacheStore,
        id: &FirmwareId,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<PathBuf, RegistryError> {
        let resolved = self.resolve(&id.raw)?;
        let path = store.definition_ini(&resolved.id);

        if store.verify_file(&path) {
            tracing::debug!(
                op = "registry-fetch",
                family = resolved.id.family.id(),
                version = resolved.id.version.as_str(),
                "a verified cache hit skipped the download"
            );

            return Ok(path);
        }

        Self::download_resolved(fetch, store, &resolved, progress)
    }
}

/// The cached entries regardless of age, or nothing when unreadable.
fn load_stale_listing(store: &CacheStore, path: &std::path::Path) -> Option<Vec<VersionEntry>> {
    let bytes = store.load(path)?;
    serde_json::from_slice(&bytes).ok()
}

/// The cached entries, or nothing when they are missing, stale or unreadable.
fn load_cached_listing(store: &CacheStore, path: &std::path::Path) -> Option<Vec<VersionEntry>> {
    let meta = store.load_meta(path)?;
    let fetched = meta.fetched_at_unix?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    if now.saturating_sub(fetched) > VERSIONS_TTL_SECS {
        return None;
    }

    load_stale_listing(store, path)
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let families: Vec<Family> = self.sources.iter().map(|source| source.family()).collect();

        formatter
            .debug_struct("Registry")
            .field("sources", &families)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Meta;
    use crate::fetch::HttpGet;

    /// A network that answers from memory and counts its calls.
    #[derive(Default)]
    struct Stub {
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl HttpGet for Stub {
        fn fetch(&self, url: &str, _max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
            self.calls.lock().expect("a test lock").push(url.to_owned());

            match url {
                "https://speeduino.com/fw/versions" => Ok(b"202501.7\nmaster\n".to_vec()),
                _ => Err(RegistryError::new(ErrorKind::NotFound, "no such file")),
            }
        }

        fn download(
            &self,
            url: &str,
            _max_bytes: usize,
            progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<Vec<u8>, RegistryError> {
            self.calls.lock().expect("a test lock").push(url.to_owned());

            let body = b"[MegaTune]\n".to_vec();
            let len = u64::try_from(body.len()).expect("a small test body");
            progress(len, Some(len));

            Ok(body)
        }
    }

    /// A network that always fails.
    struct Failing;

    impl HttpGet for Failing {
        fn fetch(&self, _url: &str, _max_bytes: usize) -> Result<Vec<u8>, RegistryError> {
            Err(RegistryError::new(ErrorKind::Network, "offline"))
        }

        fn download(
            &self,
            _url: &str,
            _max_bytes: usize,
            _progress: &mut dyn FnMut(u64, Option<u64>),
        ) -> Result<Vec<u8>, RegistryError> {
            Err(RegistryError::new(ErrorKind::Network, "offline"))
        }
    }

    fn temp_store(name: &str) -> (tempfile::TempDir, CacheStore) {
        let dir = tempfile::Builder::new()
            .prefix(&format!("oxtuner-registry-{name}-"))
            .tempdir()
            .expect("a temp dir");
        let store = CacheStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn each_family_resolves_to_its_own_url() {
        let registry = Registry::with_defaults();

        assert_eq!(
            registry
                .ini_url_for_signature("speeduino 202501.7")
                .expect("a url"),
            "https://speeduino.com/fw/202501.7.ini"
        );
        assert_eq!(
            registry
                .ini_url_for_signature("rusEFI master.2024.02.02.alphax-4chan.1293678056")
                .expect("a url"),
            "https://rusefi.com/online/ini/rusefi/master/2024/02/02/alphax-4chan/1293678056.ini"
        );
    }

    #[test]
    fn unknown_and_fome_signatures_resolve_to_nothing() {
        let registry = Registry::with_defaults();

        assert_eq!(registry.ini_url_for_signature("something else"), None);
        assert_eq!(
            registry.ini_url_for_signature("rusEFI (FOME) .2026.06.03.proteus_h7.1"),
            None
        );
    }

    #[test]
    fn every_shipped_family_publishes_a_listing() {
        let registry = Registry::with_defaults();

        // Both shipped families list: Speeduino from its versions file,
        // rusEFI by walking its autoindex tree.
        assert_eq!(
            registry.listing_families(),
            vec![Family::Speeduino, Family::Rusefi]
        );
        assert!(registry.versions_url(Family::Speeduino).is_some());
        assert_eq!(registry.versions_url(Family::Rusefi), None);
    }

    #[test]
    fn resolve_names_the_id_and_its_address() {
        let registry = Registry::with_defaults();

        let resolved = registry
            .resolve("rusEFI master.2024.02.02.alphax-4chan.1293678056")
            .expect("a known signature");

        assert_eq!(resolved.id.family, Family::Rusefi);
        assert_eq!(
            resolved.ini_url,
            "https://rusefi.com/online/ini/rusefi/master/2024/02/02/alphax-4chan/1293678056.ini"
        );
    }

    #[test]
    fn resolve_refuses_what_no_source_serves() {
        let registry = Registry::with_defaults();

        let missing = registry.resolve("something else").expect_err("unknown");
        assert_eq!(missing.kind(), ErrorKind::InvalidSignature);

        let unsupported = registry
            .resolve("rusEFI (FOME) .2026.06.03.proteus_h7.1")
            .expect_err("no address");
        assert_eq!(unsupported.kind(), ErrorKind::Unsupported);
    }

    #[test]
    fn fetch_versions_prefers_a_fresh_cache_over_the_network() {
        let (_dir, store) = temp_store("fetch-cache");
        let registry = Registry::with_defaults();
        let stub = Stub::default();

        let fetched = registry
            .fetch_versions(&stub, &store, Family::Speeduino)
            .expect("a listing");
        assert_eq!(fetched.entries.len(), 2);
        assert!(!fetched.cached);
        assert!(!fetched.stale);

        let cached = registry
            .fetch_versions(&stub, &store, Family::Speeduino)
            .expect("a cached listing");
        assert_eq!(cached.entries, fetched.entries);
        assert!(cached.cached);
        assert!(!cached.stale);
        assert_eq!(stub.calls.lock().expect("a test lock").len(), 1);

        let missing = registry.fetch_versions(&stub, &store, Family::Fome);
        assert!(matches!(
            missing,
            Err(error) if error.kind() == ErrorKind::Unsupported
        ));
    }

    #[test]
    fn a_failed_refresh_serves_the_stale_cache() {
        let (_dir, store) = temp_store("fetch-stale");
        let registry = Registry::with_defaults();
        let path = store.versions_file(Family::Speeduino);
        let entries = vec![VersionEntry {
            version: "202501.7".to_owned(),
            stable: true,
        }];
        let bytes = serde_json::to_vec(&entries).expect("entries encode");

        store.store(&path, &bytes).expect("a store succeeds");
        let mut meta = Meta::downloaded("https://speeduino.com/fw/versions", &bytes);
        meta.fetched_at_unix = Some(1);
        store.store_meta(&path, &meta);

        let failing = Failing;
        let served = registry
            .fetch_versions(&failing, &store, Family::Speeduino)
            .expect("stale is better than nothing");

        assert!(served.cached);
        assert!(served.stale);
        assert_eq!(served.entries, entries);
    }

    #[test]
    fn a_network_failure_without_any_cache_still_fails() {
        let (_dir, store) = temp_store("fetch-miss");
        let registry = Registry::with_defaults();

        let missing = registry.fetch_versions(&Failing, &store, Family::Speeduino);
        assert!(matches!(
            missing,
            Err(error) if error.kind() == ErrorKind::Network
        ));
    }

    #[test]
    fn download_stores_the_file_where_locate_finds_it() {
        let (_dir, store) = temp_store("download-cache");
        let registry = Registry::with_defaults();
        let stub = Stub::default();
        let id = registry
            .parse_signature("speeduino 202501.7")
            .expect("parses");

        assert_eq!(registry.locate(&store, &id), None);

        let mut seen = Vec::new();
        let path = registry
            .download_definition(&stub, &store, &id, &mut |downloaded, total| {
                seen.push((downloaded, total));
            })
            .expect("a download");

        assert_eq!(seen.len(), 1, "one chunk reports once");
        assert_eq!(registry.locate(&store, &id), Some(path.clone()));
        assert!(store.verify_file(&path));
        assert_eq!(store.load(&path).expect("stored"), b"[MegaTune]\n");
    }

    #[test]
    fn ensure_skips_the_network_on_a_verified_hit() {
        let (_dir, store) = temp_store("ensure-hit");
        let registry = Registry::with_defaults();
        let stub = Stub::default();
        let id = registry
            .parse_signature("speeduino 202501.7")
            .expect("parses");
        let mut seen = Vec::new();

        let first = registry
            .ensure_definition(&stub, &store, &id, &mut |downloaded, total| {
                seen.push((downloaded, total));
            })
            .expect("a download");
        assert_eq!(seen.len(), 1, "one download reports progress");

        let calls = stub.calls.lock().expect("a test lock").len();
        let second = registry
            .ensure_definition(&stub, &store, &id, &mut |_, _| {
                panic!("a verified hit reports no progress")
            })
            .expect("a hit");

        assert_eq!(first, second);
        assert_eq!(stub.calls.lock().expect("a test lock").len(), calls);
    }

    #[test]
    fn ensure_redownloads_a_tampered_file() {
        let (_dir, store) = temp_store("ensure-tamper");
        let registry = Registry::with_defaults();
        let stub = Stub::default();
        let id = registry
            .parse_signature("speeduino 202501.7")
            .expect("parses");
        let mut noop = |_: u64, _: Option<u64>| {};

        let path = registry
            .ensure_definition(&stub, &store, &id, &mut noop)
            .expect("a download");
        std::fs::write(&path, b"[tampered]\n").expect("a tamper lands");
        assert!(!store.verify_file(&path));

        let calls = stub.calls.lock().expect("a test lock").len();
        let repaired = registry
            .ensure_definition(&stub, &store, &id, &mut noop)
            .expect("a redownload");

        assert_eq!(repaired, path);
        assert!(store.verify_file(&path));
        assert!(stub.calls.lock().expect("a test lock").len() > calls);
    }
}
