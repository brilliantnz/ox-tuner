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
    /// The directory is created on the first store, not here.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_owned();
        let root = fs::canonicalize(&root).unwrap_or(root);

        Self { root }
    }

    /// The directory everything in this store lives under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the version listing for `family` is cached.
    pub fn versions_file(&self, family: Family) -> PathBuf {
        self.root
            .join("definition")
            .join("versions")
            .join(family.id())
            .join("versions.txt")
    }

    /// Where the definition for `id` is cached.
    pub fn definition_ini(&self, id: &FirmwareId) -> PathBuf {
        let stem = safe_segment(&id.version, 64);
        let suffix = short_hash(id.raw.as_bytes());

        self.root
            .join("definition")
            .join("ini")
            .join(id.family.id())
            .join(format!("{stem}-{suffix}.ini"))
    }

    /// Where the release notes for `id` are cached.
    pub fn notes_file(&self, id: &FirmwareId) -> PathBuf {
        let stem = safe_segment(&id.version, 64);
        let suffix = short_hash(id.raw.as_bytes());

        self.root
            .join("definition")
            .join("notes")
            .join(id.family.id())
            .join(format!("{stem}-{suffix}.json"))
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
    /// `data.ini` maps to `data.meta.json`, whatever the data extension is.
    pub fn meta_for(&self, path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("download");
        let stem = name.split('.').next().unwrap_or(name);

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

    /// Removes the oldest files in `dir` past `keep` entries.
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
            let _ = fs::remove_file(path);
        }
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
    hex_digest(&sha256(data))[..8].to_owned()
}

/// A filename stem drawn from free-form text.
///
/// Anything outside `[a-z0-9._-]` becomes a dash, runs are kept as they fall,
/// and the result is capped at `max` characters so one long version string
/// cannot make an awkward path. An empty result becomes `unnamed`.
fn safe_segment(text: &str, max: usize) -> String {
    let mut stem = String::with_capacity(text.len());

    for byte in text.bytes() {
        let lower = byte.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == b'.' || lower == b'_' || lower == b'-' {
            stem.push(char::from(lower));
        } else {
            stem.push('-');
        }

        if stem.len() >= max {
            break;
        }
    }

    let trimmed = stem.trim_matches(['.', '-']).to_owned();

    if trimmed.is_empty() {
        "unnamed".to_owned()
    } else {
        trimmed
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
        FirmwareId {
            family: Family::Speeduino,
            version: "202501.7".to_owned(),
            board: None,
            hash: None,
            raw: "speeduino 202501.7".to_owned(),
        }
    }

    #[test]
    fn downloads_land_in_the_planned_layout() {
        let root = temp_root("layout");
        let store = CacheStore::new(&root);
        let id = speeduino_id();

        assert_eq!(
            store.versions_file(Family::Speeduino),
            root.join("definition/versions/speeduino/versions.txt")
        );

        let ini = store.definition_ini(&id);
        assert!(ini.starts_with(root.join("definition/ini/speeduino")));
        assert_eq!(ini.extension().and_then(|ext| ext.to_str()), Some("ini"));

        let sidecar = store.meta_for(&ini);
        assert_eq!(
            sidecar.extension().and_then(|ext| ext.to_str()),
            Some("json")
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
    fn odd_text_becomes_a_plain_stem() {
        assert_eq!(safe_segment("202501.7", 64), "202501.7");
        assert_eq!(safe_segment("AlphaX 4chan!", 64), "alphax-4chan");
        assert_eq!(safe_segment("...---...", 64), "unnamed");
        assert_eq!(safe_segment("v", 1), "v");
    }
}
