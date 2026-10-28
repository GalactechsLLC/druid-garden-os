//! Descriptor-relative file access beneath a trusted data directory.
use std::ffi::{CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
const RESOLVE_BENEATH: u64 = 0x08;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;

#[derive(Clone, Debug)]
pub struct ConfinedFiles {
    root: PathBuf,
}
impl ConfinedFiles {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn relative(&self, path: &Path) -> Result<PathBuf, Error> {
        let path = path.strip_prefix(&self.root).map_err(|_| denied())?;
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(denied());
        }
        Ok(path.to_path_buf())
    }
    fn anchor(&self) -> Result<File, Error> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.root)
    }
    fn open_relative(&self, path: &Path, flags: i32, mode: u64) -> Result<File, Error> {
        let anchor = self.anchor()?;
        let name = cstring(if path.as_os_str().is_empty() {
            OsStr::new(".")
        } else {
            path.as_os_str()
        })?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (flags | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
            mode,
            resolve: RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                anchor.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd as i32) })
    }
    pub fn directory(&self, path: &Path) -> Result<File, Error> {
        self.open_relative(&self.relative(path)?, libc::O_RDONLY | libc::O_DIRECTORY, 0)
    }
    fn parent(&self, path: &Path) -> Result<(File, CString), Error> {
        let relative = self.relative(path)?;
        let leaf = relative.file_name().ok_or_else(denied)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        Ok((
            self.open_relative(parent, libc::O_RDONLY | libc::O_DIRECTORY, 0)?,
            cstring(leaf)?,
        ))
    }
    pub fn read(&self, path: &Path) -> Result<String, Error> {
        let file = self.open_relative(&self.relative(path)?, libc::O_RDONLY, 0)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(denied());
        }
        if meta.len() > MAX_FILE_BYTES as u64 {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        file.take((MAX_FILE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(too_large());
        }
        String::from_utf8(bytes).map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                "Only UTF-8 text files are supported",
            )
        })
    }
    pub fn create(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        validate_bytes(bytes)?;
        let mut file = self.open_relative(
            &self.relative(path)?,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        file.write_all(bytes)?;
        file.sync_all()
    }
    pub fn update(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        validate_bytes(bytes)?;
        let (parent, name) = self.parent(path)?;
        // The target must already be a regular file. The later rename replaces
        // only this directory entry, never a symlink's destination.
        let current = self.open_relative(&self.relative(path)?, libc::O_RDONLY, 0)?;
        if !current.metadata()?.is_file() || current.metadata()?.nlink() != 1 {
            return Err(denied());
        }
        let temporary = cstring(OsStr::new(&format!(
            ".dgos-{:032x}",
            rand::random::<u128>()
        )))?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            check(unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temporary.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            })?;
            parent.sync_all()
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
            }
        }
        result
    }
    pub fn mkdir(&self, path: &Path) -> Result<(), Error> {
        let (parent, name) = self.parent(path)?;
        check(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })
    }
    pub fn rename(&self, from: &Path, to: &Path) -> Result<(), Error> {
        let (source, from) = self.parent(from)?;
        let (destination, to) = self.parent(to)?;
        check(unsafe {
            libc::renameat2(
                source.as_raw_fd(),
                from.as_ptr(),
                destination.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        })
    }
    pub fn remove(&self, path: &Path) -> Result<(), Error> {
        let (parent, name) = self.parent(path)?;
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()))
            .join(OsStr::from_bytes(name.as_bytes()));
        let metadata = std::fs::symlink_metadata(fd_path)?;
        let flags = if metadata.is_dir() {
            libc::AT_REMOVEDIR
        } else {
            0
        };
        check(unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) })
    }
}
fn validate_bytes(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(too_large());
    }
    std::str::from_utf8(bytes).map_err(|_| {
        Error::new(
            ErrorKind::InvalidData,
            "Only UTF-8 text files are supported",
        )
    })?;
    Ok(())
}
fn cstring(value: &OsStr) -> Result<CString, Error> {
    CString::new(value.as_bytes()).map_err(|_| denied())
}
fn denied() -> Error {
    Error::new(
        ErrorKind::PermissionDenied,
        "File access is restricted to the data directory; links and traversal are not allowed",
    )
}
fn too_large() -> Error {
    Error::new(ErrorKind::InvalidData, "Text files are limited to 1 MiB")
}
fn check(code: i32) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_traversal_symlinks_and_hardlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let files = ConfinedFiles::new(root.path().into());
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert!(files.read(&root.path().join("../secret")).is_err());
        assert!(files.read(&root.path().join("escape/secret")).is_err());
        assert!(files
            .create(&root.path().join("escape/new"), b"bad")
            .is_err());
        assert!(!outside.path().join("new").exists());
        std::fs::hard_link(outside.path().join("secret"), root.path().join("hardlink")).unwrap();
        assert!(files.read(&root.path().join("hardlink")).is_err());
        assert!(files.remove(root.path()).is_err());
    }
    #[test]
    fn bounded_file_lifecycle_and_no_replace_rename() {
        let root = tempfile::tempdir().unwrap();
        let files = ConfinedFiles::new(root.path().into());
        let a = root.path().join("a");
        let b = root.path().join("b");
        files.create(&a, b"one").unwrap();
        files.update(&a, b"two").unwrap();
        assert_eq!(files.read(&a).unwrap(), "two");
        files.create(&b, b"keep").unwrap();
        assert!(files.rename(&a, &b).is_err());
        assert_eq!(files.read(&b).unwrap(), "keep");
        assert!(files.update(&a, &vec![b'x'; MAX_FILE_BYTES + 1]).is_err());
        files.remove(&a).unwrap();
        assert!(!a.exists());
    }
}
