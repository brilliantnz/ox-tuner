use std::collections::HashMap;

/// A name-keyed, insertion-ordered collection.
///
/// `entries` is the source of truth for order and contents. The two maps are
/// indexes into it: `lookup` answers an exact name, `ci_lookup` answers the
/// trimmed ASCII-lowercase form `get_ci` falls back to. Both are rebuilt by
/// `insert` and nothing else renames or removes an entry, so they cannot drift.
#[derive(Debug, Clone, PartialEq)]
pub struct Ordered<T> {
    pub(super) entries: Vec<(String, T)>,
    lookup: HashMap<String, usize>,
    /// Hash of the folded name -> first entry with that hash.
    ///
    /// A hash rather than a lowercased `String` key: building the index must
    /// not allocate once per entry, because a large definition inserts
    /// hundreds of constants during a parse. A hash collision keeps the first
    /// index and makes `get_ci` fall back to the linear scan, which preserves
    /// the first-declaration rule without ever changing a result.
    ci_lookup: HashMap<u64, usize>,
}

/// FNV-1a over the ASCII-lowercased bytes.
///
/// `eq_ignore_ascii_case` folds only A–Z, and so does this; non-ASCII bytes
/// are hashed unchanged. The name is hashed untrimmed at insertion and the
/// query trimmed at lookup, which is exactly the comparison `get_ci` makes.
fn ci_hash(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in name.as_bytes() {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            lookup: HashMap::new(),
            ci_lookup: HashMap::new(),
        }
    }
}

impl<T> Ordered<T> {
    /// An empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts an entry, replacing an existing one **in place** so definition
    /// order survives a redefinition.
    pub fn insert(&mut self, name: impl Into<String>, value: T) -> Option<T> {
        let name = name.into();
        if let Some(&position) = self.lookup.get(&name) {
            let previous = std::mem::replace(&mut self.entries[position].1, value);
            return Some(previous);
        }

        let position = self.entries.len();
        // The case-insensitive index keeps the first declaration: two names
        // that differ only by case are distinct entries, and a `get_ci` has
        // always returned the earlier one.
        self.ci_lookup.entry(ci_hash(&name)).or_insert(position);
        self.lookup.insert(name.clone(), position);
        self.entries.push((name, value));
        None
    }

    /// Looks an entry up by exact name.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.lookup
            .get(name)
            .map(|position| &self.entries[*position].1)
    }

    /// Looks an entry up case-insensitively, the way references in the dialect
    /// are usually written.
    ///
    /// The exact index is tried first without trimming, then the hash of the
    /// trimmed ASCII-folded form. That is the semantics the previous linear
    /// scan had: candidates were never trimmed, so a name with surrounding
    /// spaces only matched an equally spaced query.
    pub fn get_ci(&self, name: &str) -> Option<&T> {
        if let Some(found) = self.get(name) {
            return Some(found);
        }

        let name = name.trim_ascii();
        match self.ci_lookup.get(&ci_hash(name)) {
            Some(&position) => {
                let (candidate, value) = &self.entries[position];
                if candidate.eq_ignore_ascii_case(name) {
                    return Some(value);
                }

                // A hash collision stored another name first. The linear scan
                // is the oracle for that case; it is unreachable in practice
                // with a 64-bit hash, but keeping it makes a collision a
                // slowdown rather than a wrong answer.
                self.entries
                    .iter()
                    .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value)
            }
            None => None,
        }
    }

    /// Mutably borrows an entry by exact name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut T> {
        let position = *self.lookup.get(name)?;
        Some(&mut self.entries[position].1)
    }

    /// Whether the name is present (exact or case-insensitive).
    pub fn contains(&self, name: &str) -> bool {
        self.get_ci(name).is_some()
    }

    /// Entries in definition order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Values only, in definition order.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|(_, value)| value)
    }

    /// Values mutably, in definition order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut T)> {
        self.entries
            .iter_mut()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Names in definition order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Serializes only the ordered entries; the indexes are an implementation
/// detail and are rebuilt on load.
impl<T: serde::Serialize> serde::Serialize for Ordered<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(&self.entries)
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Ordered<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = <Vec<(String, T)> as serde::Deserialize<'de>>::deserialize(deserializer)?;
        let mut ordered = Self::new();
        for (name, value) in entries {
            ordered.insert(name, value);
        }
        Ok(ordered)
    }
}
