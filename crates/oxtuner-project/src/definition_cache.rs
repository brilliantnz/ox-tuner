//! A persistent cache for the parsed ECU definition.
//!
//! Parsing is deterministic: the definition bytes, the `#include` files they
//! pull in and the settings context decide the result and nothing else. The
//! cache is therefore pure memoization — a derived artifact that may be
//! deleted at any time without changing what a project opens with.
//!
//! The envelope is written atomically (`temp` + rename, without the tune file's
//! `fsync`) and every failure is non-fatal: a missing, unreadable, corrupt or
//! incompatible cache falls back to the parser. A read-only project is not
//! special because the cache lives in the application's cache directory, never
//! inside the project.
//!
//! # `TuneLayout`
//!
//! ```text
//! magic[8] | header_len u32-le | header (postcard) | payload (postcard)
//! ```
//!
//! The header carries the whole key, the `#include` fingerprints and an FNV-1a
//! payload checksum, so a stale or corrupt entry is rejected before the
//! definition is deserialized.
//!
//! The payload holds the definition with its option tables lifted out, the
//! distinct tables, and one index per entry. `serde` writes an
//! `Arc<[BitOption]>` by value at every reference, so a table shared by many
//! entries was stored once per entry: Speeduino points nineteen entries at one
//! 65 536 label CAN address list, which made the file 14.4 MiB for 1.2 MiB of
//! data and made a warm open slower than parsing.
//!
//! # Invalidation
//!
//! [`CACHE_FORMAT_VERSION`] covers the envelope layout and the serialized
//! model, [`oxtuner_ini::PARSER_VERSION`] covers the parser's output contract,
//! and [`oxtuner_ini::MAX_SPEC_VERSION`] covers the dialect ceilings this build
//! understands. Any of them changing moves the cache path, so old entries are
//! simply never found again.

use std::{
    cell::RefCell,
    collections::HashMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use oxtuner_ini::{BitOption, Definition, PARSER_VERSION, SettingsContext};
use serde::{Deserialize, Serialize};

use crate::hash;
use crate::project::read_include_canonical;

/// Bump when the envelope layout or the serialized definition model changes.
///
/// The parser has its own [`PARSER_VERSION`] for grammatical changes; this one
/// is about the cache file itself.
pub const CACHE_FORMAT_VERSION: u32 = 2;

/// Identifies a cache file as ours before any byte is interpreted.
const MAGIC: &[u8; 8] = b"OXDEFN01";

/// How many definition entries the cache keeps before pruning older ones.
///
/// A definition is edited many times over a project's life, and each distinct
/// content is a distinct entry. The cap keeps a long-lived cache directory
/// from growing without bound; the cost of a miss is one parse.
const KEEP_ENTRIES: usize = 32;

/// Refuse to interpret a header longer than this.
///
/// A corrupt length prefix must not make the loader allocate before it has
/// read anything; real headers carry a handful of include names.
const MAX_HEADER_BYTES: usize = 1024 * 1024;

/// Refuse to load a cache file larger than this.
///
/// A large definition is a few megabytes at most; the cap only stops a stray
/// or hostile file from being read whole.
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// Where a definition came from on this open.
#[expect(
    clippy::exhaustive_enums,
    reason = "a definition was either parsed or loaded from the cache"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionSource {
    /// Preprocessing and parsing ran for this open.
    Parsed,
    /// A valid cache entry was loaded; parsing was skipped.
    Cache,
}

/// One `#include` input and the fingerprint the parser actually saw.
///
/// `None` records a file that did not resolve: the preprocessor turns that into
/// a diagnostic, so the *absence* is an input too. If the file appears later,
/// the recorded `None` no longer matches and the entry is a miss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IncludeFingerprint {
    pub(crate) name: String,
    pub(crate) sha256: Option<[u8; 32]>,
}

impl IncludeFingerprint {
    /// Fingerprints one resolver answer.
    pub(crate) fn new(name: &str, content: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            sha256: content.map(|text| hash::sha256(text.as_bytes())),
        }
    }

    /// Whether the file still resolves to the same content.
    fn matches(&self, base: &Path) -> bool {
        read_include_canonical(base, &self.name)
            .as_deref()
            .map(|text| hash::sha256(text.as_bytes()))
            == self.sha256
    }
}

/// The definition cache key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheKey {
    definition: [u8; 32],
    settings: [u8; 32],
    source: [u8; 32],
    spec_version: u64,
    parser_version: u32,
    format_version: u32,
}

/// Everything the cache stores before the payload.
#[derive(Debug, Serialize, Deserialize)]
struct CacheHeader {
    key: CacheKey,
    includes: Vec<IncludeFingerprint>,
    /// FNV-1a over the payload. Not a security primitive: it catches the
    /// corruption a crash or a truncated copy leaves behind, and unlike a
    /// cryptographic digest it does not dominate a warm open.
    payload: u64,
}

/// The payload as it is written: the definition with its tables lifted out.
#[derive(Serialize)]
struct CachePayloadRef<'a> {
    definition: &'a Definition,
    tables: &'a [Arc<[BitOption]>],
    constants: &'a [Option<u32>],
    variables: &'a [Option<u32>],
}

/// The payload as it is read back.
#[derive(Deserialize)]
struct CachePayload {
    definition: Definition,
    tables: Vec<Arc<[BitOption]>>,
    constants: Vec<Option<u32>>,
    variables: Vec<Option<u32>>,
}

/// The option tables taken out of a definition before it is written.
struct LiftedTables {
    constants: Vec<Arc<[BitOption]>>,
    variables: Vec<Arc<[BitOption]>>,
}

/// Takes every option table out of `definition`, leaving empty tables behind.
///
/// Taking rather than cloning keeps a definition cache write from copying the
/// whole model; the caller puts the same tables back once the payload is
/// encoded.
fn lift_tables(definition: &mut Definition) -> LiftedTables {
    let mut constants = Vec::with_capacity(definition.constants.len());
    for (_, constant) in definition.constants.iter_mut() {
        constants.push(std::mem::replace(&mut constant.options, Arc::from([])));
    }

    let mut variables = Vec::with_capacity(definition.pc_variables.len());
    for (_, variable) in definition.pc_variables.iter_mut() {
        variables.push(std::mem::replace(&mut variable.options, Arc::from([])));
    }

    LiftedTables {
        constants,
        variables,
    }
}

/// Puts the tables `lift_tables` removed back where they came from.
fn restore_tables(definition: &mut Definition, lifted: LiftedTables) {
    let LiftedTables {
        constants,
        variables,
    } = lifted;

    for ((_, constant), options) in definition.constants.iter_mut().zip(constants) {
        constant.options = options;
    }

    for ((_, variable), options) in definition.pc_variables.iter_mut().zip(variables) {
        variable.options = options;
    }
}

/// The distinct tables of a lifted definition, and one index per entry.
struct InternedTables {
    tables: Vec<Arc<[BitOption]>>,
    constants: Vec<Option<u32>>,
    variables: Vec<Option<u32>>,
}

/// Collects each distinct table once and records which entries use it.
fn intern_tables(lifted: &LiftedTables) -> InternedTables {
    let mut seen: HashMap<*const u8, u32> = HashMap::new();
    let mut tables: Vec<Arc<[BitOption]>> = Vec::new();

    let mut intern = |options: &Arc<[BitOption]>| -> Option<u32> {
        if options.is_empty() {
            return None;
        }

        // The pointer is only used as an identity for this parse: two
        // entries share a table exactly when their `Arc` allocation is the same.
        let key = Arc::as_ptr(options).cast::<u8>();
        if let Some(&index) = seen.get(&key) {
            return Some(index);
        }

        let index =
            u32::try_from(tables.len()).expect("a definition has fewer than u32::MAX tables");
        tables.push(Arc::clone(options));
        seen.insert(key, index);
        Some(index)
    };

    let constants = lifted.constants.iter().map(&mut intern).collect();
    let variables = lifted.variables.iter().map(&mut intern).collect();

    InternedTables {
        tables,
        constants,
        variables,
    }
}

/// Reattaches the stored tables to a definition read back from the cache.
///
/// Returns `None` when an index is out of range or the counts disagree, which
/// means the payload does not describe this definition; the caller treats that
/// as a miss rather than showing a definition with the wrong option tables.
fn attach_tables(
    definition: &mut Definition,
    tables: &[Arc<[BitOption]>],
    constants: &[Option<u32>],
    variables: &[Option<u32>],
) -> Option<()> {
    if constants.len() != definition.constants.len()
        || variables.len() != definition.pc_variables.len()
    {
        return None;
    }

    for ((_, constant), index) in definition.constants.iter_mut().zip(constants) {
        constant.options = match index {
            Some(index) => Arc::clone(tables.get(usize::try_from(*index).ok()?)?),
            None => Arc::from([]),
        };
    }

    for ((_, variable), index) in definition.pc_variables.iter_mut().zip(variables) {
        variable.options = match index {
            Some(index) => Arc::clone(tables.get(usize::try_from(*index).ok()?)?),
            None => Arc::from([]),
        };
    }

    Some(())
}

/// One cache location and the key this open would look up.
pub(crate) struct DefinitionCache {
    directory: PathBuf,
    path: PathBuf,
    base: PathBuf,
    key: CacheKey,
}

impl DefinitionCache {
    /// Builds the lookup for one definition, or `None` when the key cannot be
    /// encoded (which would mean a bug in a fixed-size key). Used by tests
    /// that hand over a non-canonical path.
    #[cfg(test)]
    pub(crate) fn new(
        directory: &Path,
        base: &Path,
        definition_digest: [u8; 32],
        settings: &SettingsContext,
    ) -> Option<Self> {
        let base = fs::canonicalize(base).unwrap_or_else(|_| base.to_owned());
        Self::with_canonical_base(directory, &base, definition_digest, settings)
    }

    /// Same as [`Self::new`] when the caller already has the canonical base,
    /// so one project open resolves the path only once.
    pub(crate) fn with_canonical_base(
        directory: &Path,
        base: &Path,
        definition_digest: [u8; 32],
        settings: &SettingsContext,
    ) -> Option<Self> {
        let key = CacheKey {
            definition: definition_digest,
            settings: settings_digest(settings)?,
            source: hash::sha256(base.as_os_str().as_encoded_bytes()),
            spec_version: oxtuner_ini::MAX_SPEC_VERSION.to_bits(),
            parser_version: PARSER_VERSION,
            format_version: CACHE_FORMAT_VERSION,
        };
        let digest = postcard::to_allocvec(&key)
            .ok()
            .map(|bytes| hash::sha256(&bytes))?;
        let path = directory.join(format!("{}.defcache", hash::hex(&digest)));

        Some(Self {
            directory: directory.to_owned(),
            path,
            base: base.to_owned(),
            key,
        })
    }

    /// The file this key maps to, for logs and tests.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Loads a valid entry, or `None` for any miss reason.
    ///
    /// Every rejection is a normal miss: the caller parses and may overwrite
    /// the file. Nothing here is allowed to make opening a project fail.
    pub(crate) fn load(&self) -> Option<Definition> {
        // One open and one bounded read instead of a metadata probe followed
        // by an unbounded read: a stray large file is never held in memory.
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(
                        %error,
                        path = %self.path.display(),
                        "the definition cache could not be read"
                    );
                }
                return None;
            }
        };
        let mut bytes = Vec::with_capacity(4 * 1024 * 1024);
        if file
            .take(MAX_CACHE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            tracing::debug!(path = %self.path.display(), "the definition cache could not be read");
            return None;
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CACHE_BYTES {
            tracing::debug!(
                path = %self.path.display(),
                bytes = bytes.len(),
                "the definition cache file is implausibly large"
            );
            return None;
        }

        let Some((header, payload)) = decode(&bytes) else {
            tracing::debug!(path = %self.path.display(), "the definition cache is malformed");
            return None;
        };

        if header.key != self.key {
            tracing::debug!(
                path = %self.path.display(),
                "the definition cache key does not match"
            );
            return None;
        }

        if !self.includes_match(&header.includes) {
            tracing::debug!(
                path = %self.path.display(),
                "an included definition file changed"
            );
            return None;
        }

        if checksum(payload) != header.payload {
            tracing::debug!(
                path = %self.path.display(),
                "the definition cache payload is corrupt"
            );
            return None;
        }

        let decoded: CachePayload = match postcard::from_bytes(payload) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::debug!(
                    %error,
                    path = %self.path.display(),
                    "the definition cache payload is undecodable"
                );
                return None;
            }
        };

        let CachePayload {
            mut definition,
            tables,
            constants,
            variables,
        } = decoded;

        if attach_tables(&mut definition, &tables, &constants, &variables).is_none() {
            tracing::debug!(
                path = %self.path.display(),
                "the definition cache payload does not match the model it was written from"
            );
            return None;
        }

        if !definition.is_usable() {
            tracing::debug!(
                path = %self.path.display(),
                "the definition cache payload failed validation"
            );
            return None;
        }

        Some(definition)
    }

    /// Stores a freshly parsed definition, best effort.
    ///
    /// A read-only cache directory, a full disk or a serialization failure is
    /// logged at debug level and ignored: the project is already open.
    pub(crate) fn store(&self, definition: &mut Definition, includes: &[IncludeFingerprint]) {
        if let Err(error) = self.write(definition, includes) {
            tracing::debug!(
                error = %error,
                path = %self.path.display(),
                "the definition cache was not written"
            );
            return;
        }

        tracing::debug!(path = %self.path.display(), "the definition cache was written");
        prune(&self.directory);
    }

    fn write(
        &self,
        definition: &mut Definition,
        includes: &[IncludeFingerprint],
    ) -> Result<(), String> {
        // The tables are lifted out for the encode and put back afterwards, so
        // a shared table is written once and the caller keeps the definition
        // it already parsed.
        let lifted = lift_tables(definition);
        let interned = intern_tables(&lifted);
        let payload = postcard::to_allocvec(&CachePayloadRef {
            definition,
            tables: &interned.tables,
            constants: &interned.constants,
            variables: &interned.variables,
        });
        restore_tables(definition, lifted);
        let payload = payload.map_err(|error| error.to_string())?;

        let header = CacheHeader {
            key: self.key.clone(),
            includes: includes.to_vec(),
            payload: checksum(&payload),
        };
        let encoded = encode(&header, &payload).ok_or_else(|| {
            "the cache header is larger than the envelope format allows".to_owned()
        })?;

        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        crate::atomic::write_derived(&self.path, &encoded).map_err(|error| error.to_string())
    }

    fn includes_match(&self, includes: &[IncludeFingerprint]) -> bool {
        includes.iter().all(|include| include.matches(&self.base))
    }
}

/// A deterministic fingerprint of everything in the settings context.
///
/// The context is a `BTreeMap`/`BTreeSet` struct, so postcard emits its keys in
/// sorted order regardless of insertion order. Two contexts that resolve the
/// same flags therefore hash the same, and changing any override or INI
/// declaration changes the digest.
fn settings_digest(settings: &SettingsContext) -> Option<[u8; 32]> {
    postcard::to_allocvec(settings)
        .ok()
        .map(|bytes| hash::sha256(&bytes))
}

/// FNV-1a over the payload, for corruption detection.
fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn encode(header: &CacheHeader, payload: &[u8]) -> Option<Vec<u8>> {
    let header = postcard::to_allocvec(header).ok()?;
    let header_len = u32::try_from(header.len()).ok()?;

    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + header.len() + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&header_len.to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(payload);
    Some(bytes)
}

/// The header length field as an index into the cache bytes.
#[expect(
    clippy::as_conversions,
    reason = "a u32 header length fits usize on every supported target"
)]
fn header_length(bytes: [u8; 4]) -> usize {
    u32::from_le_bytes(bytes) as usize
}

/// Splits a cache file into its header and payload, rejecting bad framing.
fn decode(bytes: &[u8]) -> Option<(CacheHeader, &[u8])> {
    let length_at = MAGIC.len();
    let header_at = length_at + 4;
    if bytes.len() < header_at || bytes.get(..length_at)? != MAGIC {
        return None;
    }

    let length = header_length(bytes[length_at..header_at].try_into().ok()?);
    if length == 0 || length > MAX_HEADER_BYTES {
        return None;
    }
    let header_end = header_at.checked_add(length)?;
    if header_end > bytes.len() {
        return None;
    }

    let header = postcard::from_bytes(&bytes[header_at..header_end]).ok()?;
    Some((header, &bytes[header_end..]))
}

/// Removes the oldest entries once the directory holds more than the cap.
fn prune(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "defcache")
        })
        .filter_map(|path| {
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();

    if files.len() <= KEEP_ENTRIES {
        return;
    }

    files.sort_by_key(|(modified, _)| *modified);
    let excess = files.len() - KEEP_ENTRIES;
    for (_, path) in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

/// Collects the `#include` answers while the preprocessor resolves them.
pub(crate) struct IncludeRecorder {
    base: PathBuf,
    includes: RefCell<Vec<IncludeFingerprint>>,
}

impl IncludeRecorder {
    /// The caller passes an already-canonical base, so one project open
    /// resolves the directory only once.
    pub(crate) fn new(base: &Path) -> Self {
        Self {
            base: base.to_owned(),
            includes: RefCell::new(Vec::new()),
        }
    }

    /// Resolves one include and records what it saw.
    pub(crate) fn resolve(&self, name: &str) -> Option<String> {
        let content = read_include_canonical(&self.base, name);
        let fingerprint = IncludeFingerprint::new(name, content.as_deref());

        let mut includes = self.includes.borrow_mut();
        // A file included twice is one input; keeping the first record is
        // enough because both resolutions saw the same bytes.
        if !includes.iter().any(|include| include.name == name) {
            includes.push(fingerprint);
        }

        content
    }

    pub(crate) fn finish(self) -> Vec<IncludeFingerprint> {
        self.includes.into_inner()
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-defcache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn settings() -> SettingsContext {
        SettingsContext::new().with_overrides([("CELSIUS", true)])
    }

    fn definition() -> Definition {
        oxtuner_ini::parse_str(
            "[MegaTune]\nsignature = \"cache test\"\nqueryCommand = \"Q\"\n\n[Constants]\nnPages = 1\npageSize = 8\npage = 1\nreqFuel = scalar, U08, 0, \"\", 1, 0, 0, 255, 0\n",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn a_stored_definition_round_trips() {
        let root = temp_dir("roundtrip");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");

        assert!(cache.load().is_none(), "a first open is a miss");

        let mut definition = definition();
        cache.store(&mut definition, &[]);
        let loaded = cache.load().expect("a second open is a hit");

        assert_eq!(loaded, definition);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_different_parser_or_format_version_is_a_miss() {
        let root = temp_dir("versions");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");
        cache.store(&mut definition(), &[]);

        let mut changed = CacheKey {
            definition: cache.key.definition,
            settings: cache.key.settings,
            source: cache.key.source,
            spec_version: cache.key.spec_version,
            parser_version: cache.key.parser_version + 1,
            format_version: cache.key.format_version,
        };
        assert_ne!(changed, cache.key);

        changed.parser_version = cache.key.parser_version;
        changed.format_version = cache.key.format_version + 1;
        assert_ne!(changed, cache.key);

        let mut header = CacheHeader {
            key: cache.key.clone(),
            includes: Vec::new(),
            payload: checksum(&postcard::to_allocvec(&definition()).unwrap()),
        };
        header.key = changed;
        let payload = postcard::to_allocvec(&definition()).unwrap();
        let bytes = encode(&header, &payload).expect("encodes");
        fs::write(cache.path(), bytes).unwrap();

        assert!(
            cache.load().is_none(),
            "an incompatible cache must not be served"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_a_miss_and_is_replaced() {
        let root = temp_dir("corrupt");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");
        cache.store(&mut definition(), &[]);

        fs::write(cache.path(), b"not a cache").unwrap();
        assert!(cache.load().is_none());

        cache.store(&mut definition(), &[]);
        assert_eq!(cache.load(), Some(definition()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_changed_include_is_a_miss() {
        let root = temp_dir("include");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("extra.inc"), "included = 1\n").unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n#include extra.inc\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");

        let canonical_base = fs::canonicalize(&base).expect("canonical base");
        let recorder = IncludeRecorder::new(&canonical_base);
        assert!(recorder.resolve("extra.inc").is_some());
        assert!(recorder.resolve("missing.inc").is_none());
        let includes = recorder.finish();
        cache.store(&mut definition(), &includes);
        assert!(cache.load().is_some());

        fs::write(base.join("extra.inc"), "included = 2\n").unwrap();
        assert!(cache.load().is_none(), "an include change must invalidate");

        fs::write(base.join("extra.inc"), "included = 1\n").unwrap();
        fs::write(base.join("missing.inc"), "appeared = 1\n").unwrap();
        assert!(
            cache.load().is_none(),
            "an include that was missing is an input too"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_oldest_entries_are_pruned() {
        let root = temp_dir("prune");
        for index in 0..KEEP_ENTRIES + 4 {
            let path = root.join(format!("{index}.defcache"));
            fs::write(&path, b"x").unwrap();
            let seconds = u64::try_from(index).unwrap_or(u64::MAX);
            let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        }

        prune(&root);

        let remaining = fs::read_dir(&root).unwrap().count();
        assert_eq!(remaining, KEEP_ENTRIES);
        fs::remove_dir_all(&root).unwrap();
    }

    fn shared_definition() -> Definition {
        oxtuner_ini::parse_str(
            "[MegaTune]\nsignature = \"cache test\"\nqueryCommand = \"Q\"\n\n[Constants]\nnPages = 1\npageSize = 8\npage = 1\n#define opts = \"A\", \"B\"\nfirst = bits, U08, 0, [0:1], $opts\nsecond = bits, U08, 1, [0:1], $opts\n",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn a_shared_option_table_is_written_once_and_comes_back_shared() {
        let root = temp_dir("shared-checksum");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");

        let mut definition = shared_definition();
        let first = Arc::clone(&definition.constant("first").expect("first").options);
        let second = Arc::clone(&definition.constant("second").expect("second").options);
        assert!(Arc::ptr_eq(&first, &second), "the parser shares the table");

        cache.store(&mut definition, &[]);
        let loaded = cache.load().expect("a stored definition is a hit");

        let first = Arc::clone(&loaded.constant("first").expect("first").options);
        let second = Arc::clone(&loaded.constant("second").expect("second").options);
        assert!(
            Arc::ptr_eq(&first, &second),
            "the cache must not duplicate a table that the parser shared"
        );
        assert_eq!(
            &*first,
            &*definition.constant("first").expect("first").options
        );
        assert_eq!(loaded, definition, "the rest of the model is unchanged");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_payload_that_does_not_describe_the_model_is_a_miss() {
        let root = temp_dir("mismatched-payload");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");

        let mut definition = shared_definition();
        let lifted = lift_tables(&mut definition);
        let interned = intern_tables(&lifted);

        // One index short: the payload no longer describes this definition.
        let constants = interned.constants[..interned.constants.len() - 1].to_vec();
        let payload = postcard::to_allocvec(&CachePayloadRef {
            definition: &definition,
            tables: &interned.tables,
            constants: &constants,
            variables: &interned.variables,
        })
        .expect("encodes");
        restore_tables(&mut definition, lifted);

        let header = CacheHeader {
            key: cache.key.clone(),
            includes: Vec::new(),
            payload: checksum(&payload),
        };
        let encoded = encode(&header, &payload).expect("encodes");
        fs::write(cache.path(), encoded).unwrap();

        assert!(
            cache.load().is_none(),
            "a payload whose indices do not match the model must not be served"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_payload_in_the_previous_format_is_a_miss() {
        // The payload gained the table list and the per-entry indices. An entry
        // written before that has a valid header and checksum but a payload
        // that ends where the definition ends, so decoding it fails and the
        // open falls back to parsing.
        let root = temp_dir("old-format");
        let base = root.join("projectCfg");
        fs::create_dir_all(&base).unwrap();
        let bytes = b"[MegaTune]\nsignature = \"cache test\"\n";
        let cache =
            DefinitionCache::new(&root, &base, hash::sha256(bytes), &settings()).expect("key");

        let definition = definition();
        let payload = postcard::to_allocvec(&definition).expect("encodes");
        let header = CacheHeader {
            key: cache.key.clone(),
            includes: Vec::new(),
            payload: checksum(&payload),
        };
        let encoded = encode(&header, &payload).expect("encodes");
        fs::write(cache.path(), encoded).unwrap();

        assert!(
            cache.load().is_none(),
            "an entry in the previous format must not be served"
        );

        fs::remove_dir_all(&root).unwrap();
    }
}
