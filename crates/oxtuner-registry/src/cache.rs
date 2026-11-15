//! The on-disk cache: raw downloads under a caller-supplied root.
//!
//! Everything here is a derived artifact. Deleting the root costs downloads
//! and parses, nothing else. Writes are atomic (`temp` + rename, without
//! waiting for the disk) and confined to the root: a path that escapes it is
//! refused before anything is touched.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    definition::{Family, FirmwareId},
    error::{ErrorKind, RegistryError},
    firmware::FirmwareImage,
};

/// How many downloaded files one directory keeps before pruning older ones.
///
/// A miss costs one download, so the cap only keeps a long-lived cache from
/// growing without bound.
pub const KEEP_FILES: usize = 32;

/// The provenance kept next to every downloaded file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The URL the bytes came from.
    pub url: String,
    /// Lowercase hex SHA-256 of the bytes, when it was recorded.
    pub sha256: Option<String>,
    /// How many bytes were stored.
    pub size: u64,
    /// When the bytes were stored, as unix seconds.
    pub fetched_at_unix: Option<u64>,
    /// The server's entity tag, when it sent one.
    pub etag: Option<String>,
}

impl Meta {
    /// Records a fresh download of `url` holding `bytes`.
    pub fn downloaded(url: &str, bytes: &[u8]) -> Self {
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let fetched_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_secs());

        Self {
            url: url.to_owned(),
            sha256: Some(hex_digest(&sha256(bytes))),
            size,
            fetched_at_unix,
            etag: None,
        }
    }
}

/// A cache rooted at one directory the caller owns.
///
/// All paths this store hands out sit under the root, and [`CacheStore::store`]
/// refuses anything else. Two writers in one process never share a temporary
/// because the name carries the process id and a per-write sequence.
#[derive(Debug, Clone)]
pub struct CacheStore {
    root: PathBuf,
}

/// The next unused suffix for a cache temporary in this process.
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl CacheStore {
    /// Roots the store at `root`.
    ///
    /// The root is resolved once here. Every path handed back to this store
    /// must come from it ([`CacheStore::root`], [`CacheStore::versions_file`],
    /// [`CacheStore::definition_ini`], [`CacheStore::firmware_image`],
    /// [`CacheStore::meta_for`]); a path built from the caller's original,
    /// unresolved string is not necessarily inside the store.
    ///
    /// A root that does not exist yet is kept as given, and a root carrying
    /// `..` can never be used, because every derived path is refused as
    /// outside the store. Resolve the root before calling this.
    ///
    /// The directory is created on the first store, not here.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_owned();
        let root = fs::canonicalize(&root).unwrap_or(root);

        debug_assert!(
            !root.components().any(|part| part == Component::ParentDir),
            "a cache root must not carry `..`: {}",
            root.display()
        );

        Self { root }
    }

    /// The directory everything in this store lives under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the version listing for `family` is cached.
    ///
    /// The file holds JSON entries, so its name says so.
    pub fn versions_file(&self, family: Family) -> PathBuf {
        self.root
            .join("definition")
            .join("versions")
            .join(family.id())
            .join("versions.json")
    }

    /// Where the definition for `id` is cached.
    pub fn definition_ini(&self, id: &FirmwareId) -> PathBuf {
        let stem = safe_segment(id.version(), 64);
        let suffix = short_hash(id.raw().as_bytes());

        self.root
            .join("definition")
            .join("ini")
            .join(id.family().id())
            .join(format!("{stem}-{suffix}.ini"))
    }

    /// Where the firmware image is cached.
    pub fn firmware_image(&self, image: &FirmwareImage) -> PathBuf {
        let stem = safe_segment(&image.version, 64);
        let suffix = short_hash(image.url.as_bytes());
        let extension = if image.url.to_ascii_lowercase().ends_with(".bin") {
            "bin"
        } else {
            "hex"
        };

        self.root
            .join("firmware")
            .join("image")
            .join(image.family.id())
            .join(format!("{stem}-{suffix}.{extension}"))
    }

    /// The provenance file kept next to a download.
    ///
    /// `data.ini` maps to `data.meta.json` and `202501.7-ab12cd34.ini` to
    /// `202501.7-ab12cd34.meta.json`: the whole stem is kept, dots included,
    /// so two versions that share a leading number never share a sidecar.
    pub fn meta_for(&self, path: &Path) -> PathBuf {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("download");

        path.with_file_name(format!("{stem}.meta.json"))
    }

    /// Stores `bytes` at `path`, atomically.
    ///
    /// Parent directories are created as needed. A path outside the root is
    /// refused before anything is touched.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Cache`] when the path escapes the root or the
    /// write fails.
    pub fn store(&self, path: &Path, bytes: &[u8]) -> Result<(), RegistryError> {
        check_inside(&self.root, path)?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temporary = temporary_path(path);
        let result = fs::write(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(RegistryError::from)
    }

    /// Whether the store holds a readable file at `path`.
    pub fn exists(&self, path: &Path) -> bool {
        check_inside(&self.root, path).is_ok() && path.is_file()
    }

    /// Whether the file at `path` is the one the provenance describes.
    ///
    /// A missing sidecar means the file was not put here by a download, so
    /// there is nothing to check it against and it counts as valid. A
    /// corrupt sidecar counts the same way: the bytes are what matters, and
    /// availability wins over suspicion for a derived artifact.
    ///
    /// The digest is computed while streaming the file, so a large firmware
    /// image is never held in memory just to be checked.
    pub fn verify_file(&self, path: &Path) -> bool {
        if check_inside(&self.root, path).is_err() {
            return false;
        }

        let Some(meta) = self.load_meta(path) else {
            return path.is_file();
        };

        let Some(expected) = meta.sha256.as_deref().and_then(parse_hex_digest) else {
            return path.is_file();
        };

        // A length mismatch is decidable without reading the file: the digest
        // can only match when the sizes do.
        match fs::metadata(path) {
            Ok(found) if found.len() == meta.size => {}
            _ => return false,
        }

        digest_file(path).is_some_and(|found| found == expected)
    }

    /// Reads the file at `path`, or nothing when it is missing or unreadable.
    ///
    /// Every rejection is a normal miss: the caller fetches again and may
    /// overwrite the file. A path outside the root reads as a miss too.
    pub fn load(&self, path: &Path) -> Option<Vec<u8>> {
        if check_inside(&self.root, path).is_err() {
            return None;
        }

        match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(
                        op = "registry-cache-load",
                        %error,
                        path = %path.display(),
                        "the registry cache could not be read",
                    );
                }
                None
            }
        }
    }

    /// Stores provenance for a download.
    ///
    /// A failure is logged and ignored: the bytes are already stored, and a
    /// missing sidecar only means the next open cannot prove where they came
    /// from.
    pub fn store_meta(&self, path: &Path, meta: &Meta) {
        let sidecar = self.meta_for(path);

        match serde_json::to_vec_pretty(meta) {
            Ok(bytes) => {
                if let Err(error) = self.store(&sidecar, &bytes) {
                    tracing::debug!(
                        op = "registry-cache-meta",
                        %error,
                        path = %sidecar.display(),
                        "the registry provenance was not written",
                    );
                }
            }
            Err(error) => {
                tracing::debug!(
                    op = "registry-cache-meta",
                    %error,
                    path = %sidecar.display(),
                    "the registry provenance could not be encoded",
                );
            }
        }
    }

    /// Reads provenance for a download, or nothing when it is missing or bad.
    pub fn load_meta(&self, path: &Path) -> Option<Meta> {
        let bytes = self.load(&self.meta_for(path))?;

        serde_json::from_slice(&bytes).ok()
    }

    /// Removes the oldest files in `dir` past `keep` entries, with their
    /// provenance sidecars.
    ///
    /// Only files with the given extension are counted. Failures are best
    /// effort and ignored; a directory outside the root is left alone.
    pub fn prune(&self, dir: &Path, extension: &str, keep: usize) {
        if check_inside(&self.root, dir).is_err() {
            return;
        }

        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|found| found == extension))
            .filter_map(|path| {
                let modified = fs::metadata(&path).ok()?.modified().ok()?;
                Some((modified, path))
            })
            .collect();

        if files.len() <= keep {
            return;
        }

        files.sort_by_key(|(modified, _)| *modified);
        let excess = files.len() - keep;
        for (_, path) in files.into_iter().take(excess) {
            if fs::remove_file(&path).is_ok() {
                // Same order as `remove`: the bytes go first.
                let _ = fs::remove_file(self.meta_for(&path));
            }
        }
    }

    /// Removes a data file and its provenance sidecar.
    ///
    /// Returns whether a file was removed. A missing file is `false`, not an
    /// error: the caller wanted it gone and it is gone. A path outside the
    /// root is refused.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::Cache`] when the path escapes the root or the file
    /// exists but cannot be removed.
    pub fn remove(&self, path: &Path) -> Result<bool, RegistryError> {
        check_inside(&self.root, path)?;

        let removed = match fs::remove_file(path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(RegistryError::from(error)),
        };

        // The bytes are gone either way, so an orphan sidecar is only dead
        // weight: drop it. A data file whose sidecar vanished by any other
        // route would verify as trusted forever.
        let _ = fs::remove_file(self.meta_for(path));

        Ok(removed)
    }

    /// Whether `bytes` digest to `expected`.
    ///
    /// This answers "is this the file I asked for", not a security question.
    pub fn verify(bytes: &[u8], expected: &[u8; 32]) -> bool {
        &sha256(bytes) == expected
    }
}

/// Refuses a path that escapes the store root.
fn check_inside(root: &Path, path: &Path) -> Result<(), RegistryError> {
    let lexical =
        path.components().all(|part| part != Component::ParentDir) && path.starts_with(root);

    if lexical {
        Ok(())
    } else {
        Err(RegistryError::new(
            ErrorKind::Cache,
            "the registry only reads and writes under its own root",
        ))
    }
}

/// Parses 64 lowercase hex characters into a digest.
///
/// Only the canonical form the store writes is accepted: uppercase hex
/// counts as damage, not as a digest.
fn parse_hex_digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return None;
    }

    let mut digest = [0u8; 32];
    for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).ok()?;
        digest[index] = u8::from_str_radix(pair, 16).ok()?;
    }

    Some(digest)
}

/// A sibling temporary name no other writer in this process can take.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".{}.{sequence}.tmp", std::process::id()));

    PathBuf::from(name)
}

/// The SHA-256 digest of `data`.
fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The SHA-256 digest of the file at `path`, read in bounded chunks.
///
/// Returns `None` when the file cannot be opened or read; the caller treats
/// that as "not the file the sidecar describes".
fn digest_file(path: &Path) -> Option<[u8; 32]> {
    use std::io::Read as _;

    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Some(hasher.finalize().into())
}

/// The digest as lowercase hexadecimal.
fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut text = String::with_capacity(64);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The first four digest bytes as eight hex characters.
fn short_hash(data: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = sha256(data);
    let mut text = String::with_capacity(8);
    for byte in &digest[..4] {
        let _ = write!(text, "{byte:02x}");
    }

    text
}

/// A filename stem drawn from free-form text.
///
/// Anything outside `[a-z0-9._-]` becomes a dash, runs are kept as they fall,
/// and the result is capped at `max` characters so one long version string
/// cannot make an awkward path. An empty result becomes `unnamed`.
fn safe_segment(text: &str, max: usize) -> String {
    let limit = max.max(1);
    let mut stem = String::with_capacity(text.len().min(limit));

    for byte in text.bytes() {
        if stem.len() >= limit {
            break;
        }

        let lower = byte.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == b'.' || lower == b'_' || lower == b'-' {
            stem.push(char::from(lower));
        } else {
            stem.push('-');
        }
    }

    let trimmed = stem.trim_matches(['.', '-']);

    if trimmed.is_empty() {
        // The fallback is a name too, so it obeys the same length budget.
        "unnamed"[..limit.min("unnamed".len())].to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-registry-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&path).expect("a test root is created");

        // The store canonicalizes its root, and the OS temp dir may sit behind
        // a symlink, so the test compares against the same resolved path.
        fs::canonicalize(&path).unwrap_or(path)
    }

    fn speeduino_id() -> FirmwareId {
        crate::definition::family::parse_signature("speeduino 202501.7").expect("a known signature")
    }

    #[test]
    fn downloads_land_in_the_planned_layout() {
        let root = temp_root("layout");
        let store = CacheStore::new(&root);
        let id = speeduino_id();

        assert_eq!(
            store.versions_file(Family::Speeduino),
            root.join("definition/versions/speeduino/versions.json")
        );

        let ini = store.definition_ini(&id);
        assert!(ini.starts_with(root.join("definition/ini/speeduino")));
        assert_eq!(ini.extension().and_then(|ext| ext.to_str()), Some("ini"));

        // Two versions can share a leading number, so their sidecars must not.
        // `202501.7-ab12cd34.ini` keeps its whole stem.
        let ini = root.join("definition/ini/speeduino/202501.7-ab12cd34.ini");
        assert_eq!(
            store
                .meta_for(&ini)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("202501.7-ab12cd34.meta.json")
        );

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_stored_file_round_trips_with_its_provenance() {
        let root = temp_root("roundtrip");
        let store = CacheStore::new(&root);
        let path = store.definition_ini(&speeduino_id());
        let meta = Meta::downloaded("https://speeduino.com/fw/202501.7.ini", b"[MegaTune]\n");

        store
            .store(&path, b"[MegaTune]\n")
            .expect("a store succeeds");
        store.store_meta(&path, &meta);

        assert_eq!(store.load(&path).expect("a hit"), b"[MegaTune]\n");
        assert_eq!(store.load_meta(&path).expect("a meta hit"), meta);

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_missing_file_is_a_miss_not_an_error() {
        let root = temp_root("miss");
        let store = CacheStore::new(&root);

        assert!(store.load(&root.join("definition/ini/nope.ini")).is_none());
        assert!(
            store
                .load_meta(&root.join("definition/ini/nope.ini"))
                .is_none()
        );

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn paths_outside_the_root_are_refused() {
        let root = temp_root("escape");
        let store = CacheStore::new(&root);
        let outside = root.join("..").join("outside.ini");

        assert!(store.store(&outside, b"x").is_err());
        assert!(store.load(&outside).is_none());

        store.prune(&root, "ini", 1);

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn pruning_keeps_the_newest_files() {
        let root = temp_root("prune");
        let store = CacheStore::new(&root);
        let dir = root.join("definition/ini/speeduino");

        for index in 0..KEEP_FILES + 4 {
            let path = dir.join(format!("{index}.ini"));
            store.store(&path, b"x").expect("a store succeeds");
            let stamp = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(u64::try_from(index).unwrap_or(u64::MAX));
            fs::File::options()
                .write(true)
                .open(&path)
                .expect("the file opens")
                .set_modified(stamp)
                .expect("the stamp is set");
        }

        store.prune(&dir, "ini", KEEP_FILES);

        let remaining = fs::read_dir(&dir).expect("the dir reads").count();
        assert_eq!(remaining, KEEP_FILES);

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn verification_compares_the_digest() {
        let digest = sha256(b"abc");

        assert!(CacheStore::verify(b"abc", &digest));
        assert!(!CacheStore::verify(b"abd", &digest));
    }

    #[test]
    fn pruning_takes_the_sidecar_with_its_data() {
        let root = temp_root("prune-sidecar");
        let store = CacheStore::new(&root);
        let dir = root.join("definition/ini/speeduino");

        for index in 0..2 {
            let path = dir.join(format!("{index}.ini"));
            let bytes = b"x";
            store.store(&path, bytes).expect("a store succeeds");
            store.store_meta(
                &path,
                &Meta::downloaded("https://example.invalid/x.ini", bytes),
            );
        }

        store.prune(&dir, "ini", 1);

        let left: Vec<String> = fs::read_dir(&dir)
            .expect("the dir reads")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        let has = |suffix: &str| left.iter().any(|name| name.ends_with(suffix));

        assert_eq!(left.len(), 2, "one data file and its sidecar: {left:?}");
        assert!(has(".ini"), "{left:?}");
        assert!(has(".meta.json"), "{left:?}");
        assert!(
            left.iter().any(|name| name.ends_with(".meta.json")),
            "{left:?}"
        );

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_stored_file_verifies_against_its_sidecar() {
        let root = temp_root("verify-file");
        let store = CacheStore::new(&root);
        let path = store.definition_ini(&speeduino_id());
        let bytes = b"[MegaTune]\n";

        store.store(&path, bytes).expect("a store succeeds");
        store.store_meta(
            &path,
            &Meta::downloaded("https://example.invalid/x.ini", bytes),
        );

        assert!(store.verify_file(&path));

        fs::write(&path, b"[tampered]\n").expect("a tamper lands");
        assert!(!store.verify_file(&path));

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn two_versions_sharing_a_prefix_keep_their_own_provenance() {
        let root = temp_root("sidecar-collision");
        let store = CacheStore::new(&root);
        let first = root.join("definition/ini/speeduino/202501.7-aaaa1111.ini");
        let second = root.join("definition/ini/speeduino/202501.8-bbbb2222.ini");

        store.store(&first, b"first").expect("a store succeeds");
        store.store_meta(
            &first,
            &Meta::downloaded("https://example.invalid/1.ini", b"first"),
        );
        store.store(&second, b"second").expect("a store succeeds");
        store.store_meta(
            &second,
            &Meta::downloaded("https://example.invalid/2.ini", b"second"),
        );

        // The bug this guards: both stems start with `202501`, so a sidecar
        // named from the first dot would give one file the other's digest.
        assert!(store.verify_file(&first));
        assert!(store.verify_file(&second));
        assert_ne!(store.meta_for(&first), store.meta_for(&second));

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_large_file_is_verified_without_holding_it_in_memory() {
        let root = temp_root("verify-large");
        let store = CacheStore::new(&root);
        let path = root.join("definition/ini/speeduino/202501.7-cccc3333.ini");
        let bytes = vec![b'x'; 3 * 1024 * 1024];

        store.store(&path, &bytes).expect("a store succeeds");
        store.store_meta(
            &path,
            &Meta::downloaded("https://example.invalid/big.ini", &bytes),
        );

        assert!(store.verify_file(&path));
        assert_eq!(digest_file(&path), Some(sha256(&bytes)));

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_path_outside_the_root_never_verifies() {
        let root = temp_root("verify-outside");
        let store = CacheStore::new(&root);
        let outside = root.join("..").join("outside.ini");

        fs::write(&outside, b"x").expect("a file lands outside");

        assert!(!store.verify_file(&outside));
        assert!(store.remove(&outside).is_err());

        let _ = fs::remove_file(&outside);
        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn removing_drops_the_data_and_its_sidecar() {
        let root = temp_root("remove");
        let store = CacheStore::new(&root);
        let path = store.definition_ini(&speeduino_id());

        store.store(&path, b"x").expect("a store succeeds");
        store.store_meta(
            &path,
            &Meta::downloaded("https://example.invalid/x.ini", b"x"),
        );

        assert!(store.remove(&path).expect("a removal"));
        assert!(!path.exists());
        assert!(!store.meta_for(&path).exists());
        assert!(!store.remove(&path).expect("a second removal is quiet"));

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn a_file_without_provenance_counts_as_valid() {
        let root = temp_root("verify-nometa");
        let store = CacheStore::new(&root);
        let path = root.join("hand.ini");

        fs::write(&path, b"x").expect("a hand-placed file lands");

        assert!(store.verify_file(&path));
        assert!(!store.verify_file(&root.join("missing.ini")));

        fs::remove_dir_all(&root).expect("the test root is removed");
    }

    #[test]
    fn odd_hex_is_not_a_digest() {
        assert!(parse_hex_digest(&"0".repeat(64)).is_some());
        assert_eq!(parse_hex_digest(&"0".repeat(64)), Some([0u8; 32]));
        assert!(parse_hex_digest("abc").is_none());
        assert!(parse_hex_digest(&"z".repeat(64)).is_none());
        assert!(parse_hex_digest(&"AB".repeat(32)).is_none());
    }

    #[test]
    fn odd_text_becomes_a_plain_stem() {
        assert_eq!(safe_segment("202501.7", 64), "202501.7");
        assert_eq!(safe_segment("AlphaX 4chan!", 64), "alphax-4chan");
        assert_eq!(safe_segment("...---...", 64), "unnamed");
        assert_eq!(safe_segment("v", 1), "v");
    }

    #[test]
    fn a_stem_never_escapes_the_directory() {
        use proptest::prelude::*;

        proptest!(|(text in "\\PC{0,200}", max in 1usize..80)| {
            let stem = safe_segment(&text, max);

            prop_assert!(!stem.is_empty());
            prop_assert!(!stem.contains(['/', '\\']));
            prop_assert!(stem != "." && stem != "..");
            prop_assert!(stem.len() <= max);
        });
    }

    #[test]
    fn confinement_refuses_anything_outside_the_root() {
        let root = PathBuf::from("/cache/registry");

        assert!(check_inside(&root, &root).is_ok());
        assert!(check_inside(&root, &root.join("definition/x.ini")).is_ok());

        for outside in [
            PathBuf::from("/cache/registry-evil/x.ini"),
            PathBuf::from("/cache/registry/../x.ini"),
            PathBuf::from("/tmp/x.ini"),
            PathBuf::new(),
        ] {
            assert!(
                check_inside(&root, &outside).is_err(),
                "should be refused: {outside:?}"
            );
        }
    }
}
