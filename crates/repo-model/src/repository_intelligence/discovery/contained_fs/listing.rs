//! Listing a directory by descriptor, without following symlinks.

use std::{
    ffi::{CStr, CString, OsString},
    io::{Error, Result},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::ffi::OsStringExt,
    },
};

use super::DirectoryEntry;

pub(super) fn list_entry_names(directory: &OwnedFd) -> Result<Vec<Vec<u8>>> {
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

pub(super) fn classify_entry(directory: &OwnedFd, name: Vec<u8>) -> Option<DirectoryEntry> {
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
