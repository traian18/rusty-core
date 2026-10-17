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

#[cfg(unix)]
mod platform {
    use std::{
        ffi::{CStr, CString, OsString},
        io::{Error, ErrorKind, Read, Result},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::ffi::OsStringExt,
        },
        path::{Component, Path},
    };

    use super::DirectoryEntry;

    const DIRECTORY_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    pub(super) fn read_directory(root: &Path, relative: &Path) -> Result<Vec<DirectoryEntry>> {
        let directory = open_directory_at_path(root, relative)?;
        let mut names = list_entry_names(&directory)?;
        names.sort();
        Ok(names
            .into_iter()
            .filter_map(|name| classify_entry(&directory, name))
            .collect())
    }

    pub(super) fn read_file(root: &Path, relative: &Path, max_bytes: u64) -> Result<Vec<u8>> {
        let mut parts = components_as_c_strings(relative)?;
        let file_name = parts
            .pop()
            .ok_or_else(|| Error::other("empty relative path"))?;
        let mut directory = open_root(root)?;
        for part in &parts {
            directory = open_child_directory(&directory, part)?;
        }
        let file = open_regular_file(&directory, &file_name)?;
        read_at_most(file, max_bytes)
    }

    /// Opens the root, then each component of `relative`, refusing symlinks at every step.
    fn open_directory_at_path(root: &Path, relative: &Path) -> Result<OwnedFd> {
        let mut directory = open_root(root)?;
        for part in components_as_c_strings(relative)? {
            directory = open_child_directory(&directory, &part)?;
        }
        Ok(directory)
    }

    fn open_root(root: &Path) -> Result<OwnedFd> {
        let path = CString::new(root.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::other("root path contains NUL"))?;
        // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
        let fd = unsafe { libc::open(path.as_ptr(), DIRECTORY_FLAGS) };
        owned_fd(fd)
    }

    fn open_child_directory(parent: &OwnedFd, name: &CStr) -> Result<OwnedFd> {
        // SAFETY: `parent` is an open descriptor and `name` is NUL-terminated.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIRECTORY_FLAGS) };
        owned_fd(fd)
    }

    fn open_regular_file(parent: &OwnedFd, name: &CStr) -> Result<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        // SAFETY: `parent` is an open descriptor and `name` is NUL-terminated.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        let file = owned_fd(fd)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `file` is open and `stat` is a valid out-pointer.
        if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
            return Err(Error::last_os_error());
        }
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(Error::other("manifest is not a regular file"));
        }
        Ok(file)
    }

    fn owned_fd(fd: libc::c_int) -> Result<OwnedFd> {
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: `fd` was just returned by the OS and is owned by nobody else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn components_as_c_strings(relative: &Path) -> Result<Vec<CString>> {
        relative
            .components()
            .map(|component| match component {
                Component::Normal(name) => CString::new(name.as_encoded_bytes())
                    .map_err(|_| Error::other("path component contains NUL")),
                _ => Err(Error::other("invalid relative path")),
            })
            .collect()
    }

    fn read_at_most(file: OwnedFd, max_bytes: u64) -> Result<Vec<u8>> {
        let mut file = std::fs::File::from(file);
        let mut bytes = Vec::new();
        (&mut file)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(Error::new(
                ErrorKind::FileTooLarge,
                "manifest exceeds configured byte limit",
            ));
        }
        Ok(bytes)
    }

    fn list_entry_names(directory: &OwnedFd) -> Result<Vec<Vec<u8>>> {
        // SAFETY: `directory` is open; the duplicate is handed to `fdopendir`, which owns it.
        let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
        if duplicate < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: `duplicate` is a valid directory descriptor.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            let error = Error::last_os_error();
            // SAFETY: `fdopendir` failed, so the descriptor is still ours to close.
            unsafe { libc::close(duplicate) };
            return Err(error);
        }
        let mut names = Vec::new();
        loop {
            // SAFETY: `stream` is a live directory stream until `closedir` below.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `d_name` is NUL-terminated within the entry.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(name.to_vec());
            }
        }
        // SAFETY: closes the stream (and its descriptor) exactly once.
        unsafe { libc::closedir(stream) };
        Ok(names)
    }

    fn classify_entry(directory: &OwnedFd, name: Vec<u8>) -> Option<DirectoryEntry> {
        let c_name = CString::new(name.clone()).ok()?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid descriptor, NUL-terminated name, and out-pointer; symlinks are not followed.
        let status = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                c_name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status != 0 {
            return None;
        }
        // SAFETY: `fstatat` succeeded, so `stat` is initialized.
        let kind = unsafe { stat.assume_init() }.st_mode & libc::S_IFMT;
        Some(DirectoryEntry {
            name: OsString::from_vec(name),
            is_directory: kind == libc::S_IFDIR,
            is_regular_file: kind == libc::S_IFREG,
        })
    }
}

#[cfg(not(unix))]
mod platform {
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
}
