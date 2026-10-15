//! Handle-based source scanning for Unix.
//!
//! Every directory is opened with `openat(O_DIRECTORY | O_NOFOLLOW)` relative to its already-open
//! parent, and every file with `openat(O_NOFOLLOW)` relative to its directory handle. A symlink
//! swapped in after listing is therefore rejected by the kernel instead of being followed, and the
//! scan can never leave the opened root. Identity (device and inode) is re-checked after the open
//! and the file's size and modification time after the read.

use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use tokio_util::sync::CancellationToken;

use crate::source::{hash_bytes, is_source, Limits, OpenHook, SourceDiscovery, SourceFile};
use crate::{CallGraphError, Diagnostic};

struct Scan<'a> {
    limits: &'a Limits,
    max_files: usize,
    cancel: &'a CancellationToken,
    hook: Option<&'a OpenHook>,
    files: Vec<SourceFile>,
    diagnostics: Vec<Diagnostic>,
    total_bytes: usize,
    file_limit_reported: bool,
}

pub(crate) fn scan(
    root: &Path,
    limits: &Limits,
    max_files: usize,
    cancel: &CancellationToken,
    hook: Option<&OpenHook>,
) -> Result<SourceDiscovery, CallGraphError> {
    let root_fd = open_dir_at(libc::AT_FDCWD, &path_cstring(root)?, false)
        .map_err(|error| CallGraphError::DependencyFailure(format!("cannot open root: {error}")))?;
    let mut scan = Scan {
        limits,
        max_files,
        cancel,
        hook,
        files: Vec::new(),
        diagnostics: Vec::new(),
        total_bytes: 0,
        file_limit_reported: false,
    };
    walk(&root_fd, "", 0, &mut scan)?;
    scan.files.sort_by(|a, b| a.path.cmp(&b.path));
    scan.diagnostics
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.message.cmp(&b.message)));
    Ok(SourceDiscovery {
        files: scan.files,
        diagnostics: scan.diagnostics,
        limits: limits.describe(max_files),
    })
}

fn path_cstring(path: &Path) -> Result<CString, CallGraphError> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| CallGraphError::InvalidRequest("path contains a NUL byte".into()))
}

fn open_dir_at(dir: libc::c_int, name: &CStr, nofollow: bool) -> std::io::Result<OwnedFd> {
    let mut flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    if nofollow {
        flags |= libc::O_NOFOLLOW;
    }
    // SAFETY: `name` is a valid NUL-terminated string and `dir` is a valid descriptor or AT_FDCWD.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by a successful `openat` and is owned by no one else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn list_names(dir: &OwnedFd) -> std::io::Result<Vec<Vec<u8>>> {
    // SAFETY: `dup` of a valid descriptor; `fdopendir` takes ownership of the duplicate.
    let duplicate = unsafe { libc::dup(dir.as_raw_fd()) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `duplicate` is a valid directory descriptor.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so the duplicate is still ours to close.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is a valid, open directory stream.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: `d_name` is a NUL-terminated string inside the entry returned by `readdir`.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    }
    // SAFETY: `stream` is open and closed exactly once (this also closes the duplicate).
    unsafe { libc::closedir(stream) };
    names.sort();
    Ok(names)
}

fn stat_at(dir: &OwnedFd, name: &CStr) -> std::io::Result<libc::stat> {
    // SAFETY: zeroed `stat` is a valid output buffer; `name` is NUL-terminated.
    let mut buffer: libc::stat = unsafe { std::mem::zeroed() };
    let status = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut buffer,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buffer)
}

fn fstat(fd: &OwnedFd) -> std::io::Result<libc::stat> {
    // SAFETY: zeroed `stat` is a valid output buffer and `fd` is open.
    let mut buffer: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut buffer) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buffer)
}

fn same_identity(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}

fn unchanged(a: &libc::stat, b: &libc::stat) -> bool {
    same_identity(a, b)
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
}

fn diagnostic(scan: &mut Scan<'_>, path: &str, message: impl Into<String>) {
    scan.diagnostics.push(Diagnostic {
        path: Some(path.to_owned()),
        message: message.into(),
    });
}

fn walk(dir: &OwnedFd, rel: &str, depth: usize, scan: &mut Scan<'_>) -> Result<(), CallGraphError> {
    if scan.cancel.is_cancelled() {
        return Err(CallGraphError::Cancelled);
    }
    if depth > scan.limits.max_depth {
        diagnostic(
            scan,
            rel,
            format!(
                "source traversal depth limit ({}) skipped directory",
                scan.limits.max_depth
            ),
        );
        return Ok(());
    }
    let names = match list_names(dir) {
        Ok(names) => names,
        Err(error) if depth > 0 => {
            diagnostic(scan, rel, format!("cannot list directory: {error}"));
            return Ok(());
        }
        Err(error) => return Err(CallGraphError::DependencyFailure(error.to_string())),
    };
    for raw in names {
        if scan.cancel.is_cancelled() {
            return Err(CallGraphError::Cancelled);
        }
        let Ok(name) = String::from_utf8(raw.clone()) else {
            let shown = String::from_utf8_lossy(&raw).into_owned();
            diagnostic(scan, &join(rel, &shown), "non-UTF-8 file name skipped");
            continue;
        };
        let child = join(rel, &name);
        let Ok(c_name) = CString::new(raw) else {
            continue;
        };
        let before = match stat_at(dir, &c_name) {
            Ok(stat) => stat,
            Err(error) => {
                diagnostic(scan, &child, format!("cannot inspect source: {error}"));
                continue;
            }
        };
        let kind = before.st_mode & libc::S_IFMT;
        if kind == libc::S_IFLNK {
            diagnostic(scan, &child, "symbolic link skipped");
            continue;
        }
        if kind == libc::S_IFDIR {
            match open_dir_at(dir.as_raw_fd(), &c_name, true) {
                Ok(sub) => match fstat(&sub) {
                    Ok(opened) if same_identity(&before, &opened) => {
                        walk(&sub, &child, depth + 1, scan)?;
                    }
                    _ => diagnostic(
                        scan,
                        &child,
                        "directory changed while it was being opened; directory skipped",
                    ),
                },
                Err(error) => diagnostic(
                    scan,
                    &child,
                    format!("directory rejected (changed or unreadable): {error}"),
                ),
            }
            continue;
        }
        if kind != libc::S_IFREG || !is_source(Path::new(&name)) {
            continue;
        }
        if scan.files.len() >= scan.max_files {
            if !scan.file_limit_reported {
                scan.file_limit_reported = true;
                diagnostic(
                    scan,
                    &child,
                    format!(
                        "source file count limit ({}) skipped remaining entries",
                        scan.max_files
                    ),
                );
            }
            continue;
        }
        if before.st_size as u64 > scan.limits.max_bytes as u64 {
            diagnostic(
                scan,
                &child,
                format!(
                    "source file size limit ({}) exceeded",
                    scan.limits.max_bytes
                ),
            );
            continue;
        }
        if let Some(hook) = scan.hook {
            hook(&child);
        }
        read_file(dir, &c_name, &child, &before, scan);
    }
    Ok(())
}

fn read_file(dir: &OwnedFd, name: &CStr, rel: &str, before: &libc::stat, scan: &mut Scan<'_>) {
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and `dir` is an open directory descriptor.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        diagnostic(scan, rel, format!("source read rejected: {error}"));
        return;
    }
    // SAFETY: `fd` was just returned by a successful `openat`.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    let opened = match fstat(&owned) {
        Ok(stat) => stat,
        Err(error) => {
            diagnostic(scan, rel, format!("source read rejected: {error}"));
            return;
        }
    };
    if opened.st_mode & libc::S_IFMT != libc::S_IFREG || !same_identity(before, &opened) {
        diagnostic(
            scan,
            rel,
            "source changed while it was being opened; file skipped",
        );
        return;
    }
    let limit = scan.limits.max_bytes;
    let mut bytes = Vec::with_capacity((opened.st_size as usize).min(limit));
    let mut file = std::fs::File::from(owned);
    if let Err(error) = (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes) {
        diagnostic(scan, rel, format!("source read rejected: {error}"));
        return;
    }
    if bytes.len() > limit {
        diagnostic(
            scan,
            rel,
            format!("source file size limit ({limit}) exceeded"),
        );
        return;
    }
    let after = match fstat_file(&file) {
        Ok(stat) => stat,
        Err(error) => {
            diagnostic(
                scan,
                rel,
                format!("source changed while being read: {error}"),
            );
            return;
        }
    };
    if !unchanged(&opened, &after) {
        diagnostic(
            scan,
            rel,
            "source changed while it was being read; file skipped",
        );
        return;
    }
    if scan.total_bytes.saturating_add(bytes.len()) > scan.limits.max_total_bytes {
        diagnostic(
            scan,
            rel,
            format!(
                "aggregate source byte limit ({}) reached; file skipped",
                scan.limits.max_total_bytes
            ),
        );
        return;
    }
    scan.total_bytes += bytes.len();
    scan.files.push(SourceFile {
        path: rel.to_owned(),
        hash: hash_bytes(&bytes),
        bytes,
    });
}

fn fstat_file(file: &std::fs::File) -> std::io::Result<libc::stat> {
    // SAFETY: zeroed `stat` is a valid output buffer and the descriptor is open.
    let mut buffer: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut buffer) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(buffer)
}

fn join(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_owned()
    } else {
        format!("{rel}/{name}")
    }
}
