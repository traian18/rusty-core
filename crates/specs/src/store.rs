//! The files the specs live in, behind a trait so formats and checks run without a disk.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError {
    pub message: String,
}

impl SpecError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SpecError {}

/// A repository's files. Paths are relative to its root, use `/`, and never contain `..`.
pub trait SpecStore {
    /// The file's text, or `None` when it is missing or not text.
    fn read(&self, path: &str) -> Option<String>;
    /// Creates parent folders as needed and replaces the file.
    fn write(&self, path: &str, text: &str) -> Result<(), SpecError>;
    /// Names of the sub-folders of `path`, sorted. Missing folder: empty.
    fn dirs(&self, path: &str) -> Vec<String>;
    /// Names of the files directly in `path`, sorted. Missing folder: empty.
    fn files(&self, path: &str) -> Vec<String>;
    /// Whether a file or folder exists (also files that are not text).
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_some() || !self.files(path).is_empty() || !self.dirs(path).is_empty()
    }
    /// Moves a folder with everything in it. Refuses a target that exists.
    fn move_dir(&self, from: &str, to: &str) -> Result<(), SpecError>;
}

fn safe(path: &str) -> Result<(), SpecError> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|part| part == ".." || part.is_empty());
    if bad {
        Err(SpecError::new(format!(
            "'{path}' is not a path inside the repository"
        )))
    } else {
        Ok(())
    }
}

/// A repository on disk.
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        safe(path).ok().map(|_| self.root.join(Path::new(path)))
    }
    fn names(&self, path: &str, want_dirs: bool) -> Vec<String> {
        let Some(dir) = self.resolve(path) else {
            return Vec::new();
        };
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_type()
                    .is_ok_and(|t| if want_dirs { t.is_dir() } else { t.is_file() })
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }
}

impl SpecStore for FsStore {
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.resolve(path)?).ok()
    }
    fn write(&self, path: &str, text: &str) -> Result<(), SpecError> {
        safe(path)?;
        let target = self.root.join(Path::new(path));
        let io = |error: std::io::Error| SpecError::new(format!("{path}: {error}"));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let temporary = target.with_extension("md.tmp");
        std::fs::write(&temporary, text).map_err(io)?;
        std::fs::rename(&temporary, &target).map_err(io)
    }
    fn dirs(&self, path: &str) -> Vec<String> {
        self.names(path, true)
    }
    fn files(&self, path: &str) -> Vec<String> {
        self.names(path, false)
    }
    fn exists(&self, path: &str) -> bool {
        self.resolve(path).is_some_and(|full| full.exists())
    }
    fn move_dir(&self, from: &str, to: &str) -> Result<(), SpecError> {
        safe(from)?;
        safe(to)?;
        let (source, target) = (
            self.root.join(Path::new(from)),
            self.root.join(Path::new(to)),
        );
        let io = |error: std::io::Error| SpecError::new(format!("{from}: {error}"));
        if target.exists() {
            return Err(SpecError::new(format!("{to} already exists")));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        std::fs::rename(source, target).map_err(io)
    }
}

/// Files in memory, for tests.
#[derive(Default)]
pub struct MemoryStore {
    files: Mutex<BTreeMap<String, String>>,
}

impl MemoryStore {
    pub fn insert(&self, path: &str, text: &str) {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_owned(), text.to_owned());
    }
    pub fn paths(&self) -> Vec<String> {
        self.files.lock().unwrap().keys().cloned().collect()
    }
    /// The names one level below `path`: sub-folders when `want_dirs`, else files.
    fn children(&self, path: &str, want_dirs: bool) -> Vec<String> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut names: Vec<String> = self
            .files
            .lock()
            .unwrap()
            .keys()
            .filter_map(|key| key.strip_prefix(&prefix))
            .filter_map(|rest| match rest.split_once('/') {
                Some((dir, _)) if want_dirs => Some(dir.to_owned()),
                None if !want_dirs => Some(rest.to_owned()),
                _ => None,
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

impl SpecStore for MemoryStore {
    fn read(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }
    fn write(&self, path: &str, text: &str) -> Result<(), SpecError> {
        safe(path)?;
        self.insert(path, text);
        Ok(())
    }
    fn dirs(&self, path: &str) -> Vec<String> {
        self.children(path, true)
    }
    fn files(&self, path: &str) -> Vec<String> {
        self.children(path, false)
    }
    fn move_dir(&self, from: &str, to: &str) -> Result<(), SpecError> {
        safe(from)?;
        safe(to)?;
        let (old, new) = (
            format!("{}/", from.trim_end_matches('/')),
            format!("{}/", to.trim_end_matches('/')),
        );
        let mut files = self.files.lock().unwrap();
        if files.keys().any(|key| key.starts_with(&new)) {
            return Err(SpecError::new(format!("{to} already exists")));
        }
        let moved: Vec<String> = files
            .keys()
            .filter(|key| key.starts_with(&old))
            .cloned()
            .collect();
        for key in moved {
            let text = files.remove(&key).unwrap_or_default();
            files.insert(format!("{new}{}", &key[old.len()..]), text);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_disk_list_the_same_way() {
        let memory = MemoryStore::default();
        let directory = tempfile::tempdir().unwrap();
        let disk = FsStore::new(directory.path());
        for store in [&memory as &dyn SpecStore, &disk] {
            store.write("specs/changes/0001-a/spec.md", "a").unwrap();
            store.write("specs/changes/0002-b/spec.md", "b").unwrap();
            store.write("specs/project.md", "p").unwrap();
            assert_eq!(store.dirs("specs/changes"), ["0001-a", "0002-b"]);
            assert_eq!(store.files("specs"), ["project.md"]);
            assert_eq!(
                store.read("specs/changes/0002-b/spec.md").as_deref(),
                Some("b")
            );
            assert_eq!(store.read("specs/none.md"), None);
            assert!(store.dirs("nothing").is_empty());
        }
    }

    #[test]
    fn a_folder_moves_with_everything_in_it_and_never_onto_another() {
        let memory = MemoryStore::default();
        let directory = tempfile::tempdir().unwrap();
        let disk = FsStore::new(directory.path());
        for store in [&memory as &dyn SpecStore, &disk] {
            store.write("specs/changes/0001-a/spec.md", "a").unwrap();
            store
                .write("specs/changes/0001-a/tasks/T1-x.md", "t")
                .unwrap();
            store
                .move_dir("specs/changes/0001-a", "specs/archive/2026-10-30-0001-a")
                .unwrap();
            assert!(store.dirs("specs/changes").is_empty());
            assert_eq!(
                store
                    .read("specs/archive/2026-10-30-0001-a/tasks/T1-x.md")
                    .as_deref(),
                Some("t")
            );
            store
                .write("specs/changes/0001-a/spec.md", "again")
                .unwrap();
            assert!(store
                .move_dir("specs/changes/0001-a", "specs/archive/2026-10-30-0001-a")
                .is_err());
        }
    }

    #[test]
    fn paths_outside_the_repository_are_refused() {
        let store = MemoryStore::default();
        for path in ["../x.md", "/abs.md", "a//b.md", "a\\b.md", ""] {
            assert!(store.write(path, "x").is_err(), "{path}");
        }
        let directory = tempfile::tempdir().unwrap();
        assert!(FsStore::new(directory.path())
            .write("../x.md", "x")
            .is_err());
    }
}
