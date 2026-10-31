//! Filesystem reads that cannot leave the authorized root.
//!
//! Every path component is opened with `O_NOFOLLOW` relative to the previous
//! directory descriptor, so a symlink swapped in during a scan is refused rather
//! than followed. Only Unix is supported; other platforms report
//! [`std::io::ErrorKind::Unsupported`].
use std::{ffi::OsString, path::Path};

/// One entry of a directory listing, classified without following symlinks.
pub struct DirectoryEntry {
    pub name: OsString,
    pub is_directory: bool,
    pub is_regular_file: bool,
}

/// Lists `relative` (empty for the root itself), sorted by name.
pub fn read_contained_directory(
    root: &Path,
    relative: &Path,
) -> std::io::Result<Vec<DirectoryEntry>> {
    platform::read_directory(root, relative)
}

/// Reads a regular file of at most `max_bytes`; larger files fail with `FileTooLarge`.
pub fn read_contained_file(
    root: &Path,
    relative: &Path,
    max_bytes: u64,
) -> std::io::Result<Vec<u8>> {
    platform::read_file(root, relative, max_bytes)
}

mod listing;
#[cfg(unix)]
mod unix;
#[cfg(not(unix))]
mod unsupported;
#[cfg(unix)]
use unix as platform;
#[cfg(not(unix))]
use unsupported as platform;
