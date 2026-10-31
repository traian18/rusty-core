//! Contained reads on Unix: every path component is opened with `O_NOFOLLOW` relative to the
//! previous directory descriptor.

use std::{
    ffi::{CStr, CString},
    io::{Error, ErrorKind, Read, Result},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Component, Path},
};

use super::DirectoryEntry;

use super::listing::{classify_entry, list_entry_names};

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
    // SAFETY: `libc::stat` is plain data, for which all-zero bytes are a valid value.
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
