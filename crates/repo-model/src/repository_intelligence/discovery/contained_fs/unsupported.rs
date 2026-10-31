//! Other platforms cannot do race-resistant discovery.

use std::{io::Error, path::Path};

use super::DirectoryEntry;

fn unsupported() -> Error {
    Error::new(
        std::io::ErrorKind::Unsupported,
        "race-resistant repository discovery is unsupported on this platform",
    )
}

pub(super) fn read_directory(_: &Path, _: &Path) -> std::io::Result<Vec<DirectoryEntry>> {
    Err(unsupported())
}

pub(super) fn read_file(_: &Path, _: &Path, _: u64) -> std::io::Result<Vec<u8>> {
    Err(unsupported())
}
