//! Finished glosses kept on disk, so that restarting the server does not
//! re-translate what it already translated.
//!
//! [`GlossCache`](crate::GlossCache) alone lives and dies with the process,
//! and a translation costs on the order of a third of a second per paragraph
//! (`docs/model-runtime-notes.md`). Reopening yesterday's file would pay that
//! again for every comment in it. This is the layer that stops it: one file
//! per gloss, named by the [`GlossKey`], holding the finished Japanese.
//!
//! The key already hashes the model version and both languages, so entries
//! from another engine cannot be served after a swap - they simply hash
//! elsewhere. The stale files are removed by [`GlossStore::open`] once the
//! directory grows past its capacity.
//!
//! IMPORTANT: nothing here is allowed to fail loudly. A read-only home
//! directory, a full disk, a file someone edited by hand - each of them has to
//! come out as a cache miss, because a cache is an optimisation and an editor
//! that stops working when one is unavailable is worse than a slow one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::GlossKey;

/// Suffix of the file a gloss is being written into, before it is renamed into
/// place.
///
/// The rename is what makes a half-written gloss impossible to read: a reader
/// either sees the previous file or the whole new one, never a truncated
/// answer that would then be served as a translation.
const PENDING_SUFFIX: &str = ".pending";

/// A directory of glosses.
///
/// Cloning is cheap and gives another handle on the same directory; the store
/// holds no state beyond the path, so concurrent readers and writers need no
/// coordination beyond what the filesystem already gives.
#[derive(Debug, Clone)]
pub struct GlossStore {
    directory: PathBuf,
}

impl GlossStore {
    /// Opens (creating if needed) the directory, and prunes it to `capacity`
    /// entries.
    ///
    /// Pruning happens here rather than on every insert because it costs a
    /// directory listing: once per process start is enough to keep a cache
    /// that is written to for years from growing without a bound, and it is
    /// off the path of every request.
    ///
    /// The oldest entries go first, by the time they were written. Reading a
    /// gloss deliberately does not touch its file: an editor reads far more
    /// often than it writes, and a cache that rewrites metadata on every hover
    /// is a cache that keeps a disk awake.
    pub fn open(directory: impl Into<PathBuf>, capacity: usize) -> io::Result<Self> {
        let directory = directory.into();
        fs::create_dir_all(&directory)?;
        let store = Self { directory };
        store.prune(capacity)?;
        Ok(store)
    }

    /// Where the glosses are kept.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The gloss stored under `key`, if there is one.
    pub fn get(&self, key: &GlossKey) -> Option<String> {
        fs::read_to_string(self.path(key)).ok()
    }

    /// Whether `key` has a gloss on disk.
    pub fn contains(&self, key: &GlossKey) -> bool {
        self.path(key).is_file()
    }

    /// Writes the gloss for `key`, replacing whatever was there.
    ///
    /// Errors are returned rather than ignored so that a caller which wants to
    /// report them can; the cache above deliberately does not.
    pub fn insert(&self, key: &GlossKey, value: &str) -> io::Result<()> {
        let path = self.path(key);
        let mut pending = path.clone().into_os_string();
        pending.push(PENDING_SUFFIX);

        fs::write(&pending, value)?;
        match fs::rename(&pending, &path) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A rename that failed leaves the temporary behind, and a
                // directory slowly filling with those is worse than the failed
                // write itself.
                let _ = fs::remove_file(&pending);
                Err(error)
            }
        }
    }

    /// How many glosses are stored.
    pub fn len(&self) -> usize {
        self.entries().map_or(0, |entries| entries.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Deletes the oldest entries until at most `capacity` are left. Returns
    /// how many were deleted.
    pub fn prune(&self, capacity: usize) -> io::Result<usize> {
        let mut entries = self.entries()?;
        if entries.len() <= capacity {
            return Ok(0);
        }

        // Oldest first, so the excess is the head of the list.
        entries.sort_by_key(|(written, _)| *written);
        let excess = entries.len() - capacity;
        let mut removed = 0;
        for (_, path) in entries.into_iter().take(excess) {
            if fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn path(&self, key: &GlossKey) -> PathBuf {
        self.directory.join(key.to_hex())
    }

    /// Every gloss file, with the time it was written.
    ///
    /// Anything that is not named like a key is left alone: the directory may
    /// be one a person also keeps notes in, and a cache has no business
    /// deleting files it did not write.
    fn entries(&self) -> io::Result<Vec<(SystemTime, PathBuf)>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_key(name) {
                continue;
            }
            let written = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((written, entry.path()));
        }
        Ok(entries)
    }
}

/// Whether `name` is what [`GlossKey::to_hex`] produces.
fn is_key(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use crate::CommentRules;

    use super::*;

    fn key(text: &str) -> GlossKey {
        GlossKey::new(CommentRules::Fenced, "fugumt-en-ja@1", "en", "ja", text)
    }

    /// A directory of this test's own, removed when the guard is dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "codegloss-store-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn what_was_written_comes_back() {
        let scratch = Scratch::new();
        let store = GlossStore::open(&scratch.0, 16).expect("the directory is writable");

        assert!(store.get(&key("a")).is_none());
        assert!(!store.contains(&key("a")));

        store
            .insert(&key("a"), "キャッシュされたユーザーを返す。")
            .expect("the gloss is written");

        assert_eq!(
            store.get(&key("a")).as_deref(),
            Some("キャッシュされたユーザーを返す。")
        );
        assert!(store.contains(&key("a")));
        assert_eq!(store.len(), 1);
    }

    /// The point of the whole file: a second process finds what the first one
    /// translated.
    #[test]
    fn another_store_over_the_same_directory_sees_the_same_glosses() {
        let scratch = Scratch::new();
        GlossStore::open(&scratch.0, 16)
            .expect("the directory is writable")
            .insert(&key("a"), "訳")
            .expect("the gloss is written");

        let reopened = GlossStore::open(&scratch.0, 16).expect("the directory is writable");
        assert_eq!(reopened.get(&key("a")).as_deref(), Some("訳"));
    }

    /// Entries produced by another engine hash elsewhere and are never served.
    #[test]
    fn a_different_model_version_does_not_hit() {
        let scratch = Scratch::new();
        let store = GlossStore::open(&scratch.0, 16).expect("the directory is writable");
        store
            .insert(
                &GlossKey::new(CommentRules::Fenced, "passthrough-1", "en", "ja", "text"),
                "old",
            )
            .expect("the gloss is written");

        assert!(
            store
                .get(&GlossKey::new(
                    CommentRules::Fenced,
                    "fugumt-en-ja@1",
                    "en",
                    "ja",
                    "text"
                ))
                .is_none()
        );
    }

    #[test]
    fn opening_prunes_the_oldest_entries() {
        let scratch = Scratch::new();
        let store = GlossStore::open(&scratch.0, 8).expect("the directory is writable");
        for index in 0..6 {
            store
                .insert(&key(&index.to_string()), &index.to_string())
                .expect("the gloss is written");
            // Coarse filesystem timestamps would otherwise make the order
            // undefined, and this test is about the order.
            filetime(&store.path(&key(&index.to_string())), index);
        }
        assert_eq!(store.len(), 6);

        let reopened = GlossStore::open(&scratch.0, 2).expect("the directory is writable");
        assert_eq!(reopened.len(), 2);
        assert!(reopened.contains(&key("4")));
        assert!(reopened.contains(&key("5")));
        assert!(!reopened.contains(&key("0")));
    }

    /// A file nobody here wrote is not a gloss and is not deleted.
    #[test]
    fn foreign_files_are_left_alone() {
        let scratch = Scratch::new();
        let store = GlossStore::open(&scratch.0, 1).expect("the directory is writable");
        fs::write(scratch.0.join("notes.txt"), "mine").expect("the directory is writable");
        for index in 0..4 {
            store
                .insert(&key(&index.to_string()), "訳")
                .expect("the gloss is written");
        }

        store.prune(0).expect("the directory is readable");
        assert_eq!(store.len(), 0);
        assert!(scratch.0.join("notes.txt").is_file());
    }

    #[test]
    fn a_directory_that_cannot_be_created_is_an_error_and_not_a_panic() {
        let scratch = Scratch::new();
        fs::create_dir_all(&scratch.0).expect("the directory is writable");
        let file = scratch.0.join("a-file");
        fs::write(&file, "not a directory").expect("the directory is writable");

        assert!(GlossStore::open(file.join("glosses"), 16).is_err());
    }

    #[test]
    fn only_key_shaped_names_are_glosses() {
        assert!(is_key(
            &GlossKey::new(CommentRules::Fenced, "m", "en", "ja", "text").to_hex()
        ));
        assert!(!is_key("notes.txt"));
        assert!(!is_key(&"z".repeat(64)));
        assert!(!is_key(&"0".repeat(63)));
    }

    /// Sets a file's modification time to a known, increasing instant.
    fn filetime(path: &Path, order: u64) {
        let when = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + order);
        let file = fs::File::options()
            .write(true)
            .open(path)
            .expect("the gloss is writable");
        file.set_modified(when).expect("the time can be set");
    }
}
