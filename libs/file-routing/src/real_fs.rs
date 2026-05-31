//! A real-filesystem [`DirTree`] implementation.
//!
//! The core scanner reads the directory tree exclusively through the injectable
//! [`DirTree`] trait so the pipeline stays pure and unit-testable with in-memory
//! `MemTree` fixtures. [`RealFsDirTree`] is the production adapter that backs
//! that trait with `std::fs`: it is rooted at a base directory on disk and maps
//! the tree-relative, `/`-separated paths the scanner asks for onto real
//! directory listings.
//!
//! This is what a host (a CLI, a build step, the NAPI binding) hands to
//! [`crate::generate_routing`] to run file routing against an actual project on
//! disk, rather than a fixture. See [[rust-core-ts-shim-layering]].
//!
//! # Trait contract
//!
//! [`DirTree::entries`] is total — it never errors and never panics. A path that
//! is missing, is a file rather than a directory, or cannot be read (permissions,
//! I/O error) yields an empty `Vec`, exactly as a missing key does for the
//! in-memory fixture. This lets the scanner treat "no `routes/` here" and "I/O
//! error reading `routes/`" identically: both simply contribute nothing.
//!
//! # Determinism
//!
//! `std::fs::read_dir` returns entries in an unspecified, platform-dependent
//! order. The scanner re-sorts children for determinism, but this adapter *also*
//! sorts by entry name so that [`RealFsDirTree`] is deterministic on its own —
//! its output does not depend on filesystem iteration order, which keeps direct
//! tests of the adapter stable and makes the type safe to use anywhere a
//! reproducible listing is expected.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{DirTree, Entry};

/// A [`DirTree`] backed by the real filesystem, rooted at a base directory.
///
/// Construct with [`RealFsDirTree::new`], passing the project root that
/// *contains* the configured `routes/` and `api/` directories. The scanner then
/// asks for tree-relative paths (e.g. `"routes/blog"`); each is resolved against
/// the base and listed with `std::fs::read_dir`.
#[derive(Debug, Clone)]
pub struct RealFsDirTree {
    /// Absolute or relative base directory the tree is rooted at. Tree-relative
    /// paths from the scanner are joined onto this.
    base: PathBuf,
}

impl RealFsDirTree {
    /// Create a tree rooted at `base`. `base` is the directory that contains the
    /// `routes/` and `api/` folders (i.e. the project root); it is not required
    /// to exist — a non-existent base simply makes every listing empty.
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// The base directory this tree is rooted at.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Resolve a tree-relative, `/`-separated `rel` path against [`Self::base`].
    ///
    /// The empty string denotes the tree root and resolves to the base itself.
    /// Splitting on `/` (never the OS separator) is correct because the scanner
    /// always speaks `/`-separated tree paths regardless of platform; each
    /// component is pushed individually so the result uses the platform's native
    /// separator. Empty components (from a leading/trailing/doubled `/`) are
    /// skipped so a stray separator never resolves to the wrong directory.
    fn resolve(&self, rel: &str) -> PathBuf {
        let mut path = self.base.clone();
        for part in rel.split('/') {
            if !part.is_empty() {
                path.push(part);
            }
        }
        path
    }
}

impl DirTree for RealFsDirTree {
    /// List the direct children of the tree-relative directory `path`.
    ///
    /// Returns each child as an [`Entry`] tagged file or dir. Per the trait
    /// contract this is total: a missing path, a path that is a file, or any I/O
    /// error yields an empty `Vec`. Entries whose name is not valid UTF-8, or
    /// whose file type cannot be determined, are skipped (the routing
    /// conventions are defined over UTF-8 names). The result is sorted by name
    /// for determinism independent of filesystem iteration order.
    fn entries(&self, path: &str) -> Vec<Entry> {
        let dir = self.resolve(path);
        let Ok(read) = fs::read_dir(&dir) else {
            // Missing dir, a file rather than a dir, or an I/O / permission
            // error: contribute nothing, exactly like an absent fixture key.
            return Vec::new();
        };

        let mut entries: Vec<Entry> = read
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                // Use the entry's own file-type probe (no extra stat on most
                // platforms); on the rare error, fall back to a `metadata` call
                // so a symlinked directory is still classified, and only then
                // give up on the entry.
                let is_dir = match entry.file_type() {
                    Ok(ft) if ft.is_symlink() => fs::metadata(entry.path())
                        .map(|m| m.is_dir())
                        .unwrap_or(false),
                    Ok(ft) => ft.is_dir(),
                    Err(_) => entry.path().is_dir(),
                };
                Some(if is_dir {
                    Entry::dir(name)
                } else {
                    Entry::file(name)
                })
            })
            .collect();

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EntryKind;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// A unique, process-local temp directory under the OS temp dir. Created
    /// fresh per test and removed on drop so the unit tests touch a real
    /// filesystem without leaking artifacts or colliding across runs.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            // Derive a stable-per-call-site but unique-per-process name from the
            // tag, pid, and a monotonic-ish hash of time, avoiding extra deps.
            let mut hasher = DefaultHasher::new();
            tag.hash(&mut hasher);
            std::process::id().hash(&mut hasher);
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .hash(&mut hasher);
            let path = std::env::temp_dir()
                .join(format!("treaty_file_routing_real_fs_{tag}_{:x}", hasher.finish()));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn touch(&self, rel: &str) {
            let p = self.path.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("create parent dirs");
            }
            fs::write(&p, b"").expect("write temp file");
        }

        fn mkdir(&self, rel: &str) {
            fs::create_dir_all(self.path.join(rel)).expect("create temp subdir");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn lists_files_and_dirs_sorted_by_name() {
        let tmp = TempDir::new("listing");
        // Create out of lexicographic order to prove the adapter sorts.
        tmp.touch("zebra.ts");
        tmp.mkdir("alpha");
        tmp.touch("mango.treaty");
        tmp.mkdir("beta");

        let tree = RealFsDirTree::new(&tmp.path);
        let entries = tree.entries("");

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "mango.treaty", "zebra.ts"]);

        let kinds: Vec<EntryKind> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![EntryKind::Dir, EntryKind::Dir, EntryKind::File, EntryKind::File]
        );
    }

    #[test]
    fn resolves_nested_relative_paths() {
        let tmp = TempDir::new("nested");
        tmp.touch("routes/blog/index.treaty");
        tmp.mkdir("routes/blog/[slug]");

        let tree = RealFsDirTree::new(&tmp.path);
        let entries = tree.entries("routes/blog");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["[slug]", "index.treaty"]);
        assert!(entries.iter().find(|e| e.name == "[slug]").unwrap().is_dir());
        assert!(entries.iter().find(|e| e.name == "index.treaty").unwrap().is_file());
    }

    #[test]
    fn empty_string_path_is_the_root() {
        let tmp = TempDir::new("root");
        tmp.touch("only.ts");
        let tree = RealFsDirTree::new(&tmp.path);
        let entries = tree.entries("");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "only.ts");
    }

    #[test]
    fn missing_directory_yields_empty() {
        let tmp = TempDir::new("missing");
        let tree = RealFsDirTree::new(&tmp.path);
        // Never created.
        assert!(tree.entries("does/not/exist").is_empty());
    }

    #[test]
    fn nonexistent_base_yields_empty() {
        let base = std::env::temp_dir().join("treaty_file_routing_definitely_absent_qwerty");
        // Guard against a stale dir from a prior aborted run.
        let _ = fs::remove_dir_all(&base);
        let tree = RealFsDirTree::new(&base);
        assert!(tree.entries("").is_empty());
        assert!(tree.entries("routes").is_empty());
    }

    #[test]
    fn path_to_a_file_is_not_a_directory_yields_empty() {
        let tmp = TempDir::new("fileaspath");
        tmp.touch("index.ts");
        let tree = RealFsDirTree::new(&tmp.path);
        // Asking for the children of a *file* must be empty, not an error.
        assert!(tree.entries("index.ts").is_empty());
    }

    #[test]
    fn ignores_leading_trailing_and_doubled_slashes() {
        let tmp = TempDir::new("slashes");
        tmp.touch("routes/index.treaty");
        let tree = RealFsDirTree::new(&tmp.path);
        // All of these denote the same `routes/` directory.
        for p in ["routes", "/routes", "routes/", "routes//"] {
            let names: Vec<String> = tree.entries(p).iter().map(|e| e.name.clone()).collect();
            assert_eq!(names, vec!["index.treaty".to_string()], "path {p:?}");
        }
    }

    #[test]
    fn base_accessor_returns_root() {
        let tmp = TempDir::new("accessor");
        let tree = RealFsDirTree::new(&tmp.path);
        assert_eq!(tree.base(), tmp.path.as_path());
    }
}
