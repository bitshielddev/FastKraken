//! Descriptor-relative operations used only for the files ref backend. A moved
//! worktree or Git directory can never redirect a write or lock cleanup.
use super::{Result, changed, unavailable, unsupported};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub(crate) struct Directory(Arc<File>);
#[derive(Debug, Clone)]
pub(crate) struct Pins {
    pub(super) root: Directory,
    pub(super) git: Directory,
    pub(super) common: Directory,
}
type Identity = [(u64, u64); 3];

fn component(name: &OsStr) -> io::Result<CString> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    CString::new(bytes).map_err(|_| io::ErrorKind::InvalidInput.into())
}
impl Pins {
    pub(crate) fn open(paths: [&Path; 3]) -> io::Result<Self> {
        let directory = |p: &Path| -> io::Result<_> {
            let fd = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(p)?;
            Ok(Directory(Arc::new(fd)))
        };
        Ok(Self {
            root: directory(paths[0])?,
            git: directory(paths[1])?,
            common: directory(paths[2])?,
        })
    }
    pub(crate) fn identity(&self) -> io::Result<Identity> {
        let id = |dir: &Directory| -> io::Result<_> {
            let m = dir.0.metadata()?;
            Ok((m.dev(), m.ino()))
        };
        Ok([id(&self.root)?, id(&self.git)?, id(&self.common)?])
    }
}
impl Directory {
    fn sync(&self) -> Result<()> {
        match self.0.sync_all() {
            Ok(()) => Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => Ok(()),
            Err(_) => Err(unavailable()),
        }
    }
    fn open(&self, name: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
        let named = component(name)?;
        let fd = unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                named.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    pub(super) fn subdirs(&self, path: &Path, create: bool) -> Result<Self> {
        let mut dir = self.clone();
        for part in path.components() {
            let Component::Normal(name) = part else {
                return Err(unavailable());
            };
            let open = || dir.open(name, libc::O_RDONLY | libc::O_DIRECTORY, 0);
            let next = match open() {
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                    let name = component(name).map_err(|_| unavailable())?;
                    if unsafe { libc::mkdirat(dir.0.as_raw_fd(), name.as_ptr(), 0o777) } < 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                    {
                        return Err(unavailable());
                    }
                    dir.sync()?;
                    open().map_err(|_| unavailable())?
                }
                other => other.map_err(|_| unavailable())?,
            };
            dir = Self(Arc::new(next));
        }
        Ok(dir)
    }
    pub(super) fn read(&self, name: &OsStr, limit: usize) -> Result<Option<Vec<u8>>> {
        let file = match self.open(name, libc::O_RDONLY, 0) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other.map_err(|_| unavailable())?,
        };
        if !file.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unavailable());
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unavailable())?;
        if bytes.len() > limit {
            return Err(unsupported(
                "The repository metadata exceeds the local commit limit.",
            ));
        }
        Ok(Some(bytes))
    }
    pub(super) fn exists_path(&self, path: &Path) -> Result<bool> {
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(unavailable());
        }
        let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| unavailable())?;
        let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe {
            libc::fstatat(
                self.0.as_raw_fd(),
                name.as_ptr(),
                info.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            return if io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(unavailable())
            };
        }
        Ok(true)
    }
    pub(super) fn contains(&self, name: &OsStr) -> Result<bool> {
        match self.open(name, libc::O_RDONLY, 0) {
            Ok(f) if f.metadata().map_err(|_| unavailable())?.is_file() => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            _ => Err(unavailable()),
        }
    }
    pub(super) fn append_log(&self, name: &OsStr, line: &[u8]) -> Result<()> {
        let mut f = self
            .open(name, libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT, 0o666)
            .map_err(|_| unavailable())?;
        if !f.metadata().map_err(|_| unavailable())?.is_file() {
            return Err(unavailable());
        }
        f.write_all(line)
            .and_then(|_| f.sync_data())
            .map_err(|_| unavailable())?;
        self.sync()
    }
    pub(super) fn lock(&self, name: &OsStr) -> Result<Lock> {
        let mut lockname = name.as_bytes().to_vec();
        lockname.extend_from_slice(b".lock");
        let lockname = OsString::from_vec(lockname);
        let file = self
            .open(
                &lockname,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                0o666,
            )
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    super::staged::busy()
                } else {
                    unavailable()
                }
            })?;
        Ok(Lock {
            dir: self.clone(),
            file,
            name: Some(lockname),
        })
    }
    pub(super) fn install_object(&self, name: &OsStr, src: &Path) -> Result<()> {
        if self.contains(name)? {
            return Ok(());
        }
        let mut scratch = OsString::from("agent-object-");
        scratch.push(name);
        // Different volumes are supported. Publish a complete, synced loose
        // object with linkat (no replacement) and remove its temporary name.
        let mut lock = self.lock(&scratch)?;
        let mut input = File::open(src).map_err(|_| unavailable())?;
        io::copy(&mut input, &mut lock.file).map_err(|_| unavailable())?;
        lock.file
            .set_permissions(fs::Permissions::from_mode(0o444))
            .map_err(|_| unavailable())?;
        lock.file.sync_all().map_err(|_| unavailable())?;
        let from = component(lock.name.as_deref().unwrap()).map_err(|_| unavailable())?;
        let to = component(name).map_err(|_| unavailable())?;
        if unsafe {
            libc::linkat(
                self.0.as_raw_fd(),
                from.as_ptr(),
                self.0.as_raw_fd(),
                to.as_ptr(),
                0,
            )
        } < 0
        {
            if io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                || !self.contains(name)?
            {
                return Err(unavailable());
            }
        }
        self.sync()
    }
}
pub(super) struct Lock {
    dir: Directory,
    file: File,
    name: Option<OsString>,
}
impl Lock {
    fn owns(&self) -> bool {
        self.name
            .as_deref()
            .and_then(|name| self.dir.open(name, libc::O_RDONLY, 0).ok())
            .and_then(|f| f.metadata().ok())
            .zip(self.file.metadata().ok())
            .is_some_and(|(a, b)| (a.dev(), a.ino()) == (b.dev(), b.ino()))
    }
    pub(super) fn fill(&mut self, bytes: &[u8]) -> Result<()> {
        self.file
            .write_all(bytes)
            .and_then(|_| self.file.sync_all())
            .map_err(|_| unavailable())
    }
    pub(super) fn publish(&mut self, name: &OsStr) -> Result<()> {
        self.dir.contains(name)?; // refuse symlink, directory or device destinations
        if !self.owns() {
            return Err(changed());
        }
        let from = component(self.name.as_deref().unwrap()).map_err(|_| unavailable())?;
        let to = component(name).map_err(|_| unavailable())?;
        if unsafe {
            libc::renameat(
                self.dir.0.as_raw_fd(),
                from.as_ptr(),
                self.dir.0.as_raw_fd(),
                to.as_ptr(),
            )
        } < 0
        {
            return Err(unavailable());
        }
        self.name = None;
        self.dir.sync().map_err(|_| super::GitError {code:"outcome_unknown", message:"The branch moved, but its directory flush failed. Reconcile the commit in local history."})
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if self.owns()
            && let Some(name) = self.name.as_deref().and_then(|n| component(n).ok())
        {
            unsafe {
                libc::unlinkat(self.dir.0.as_raw_fd(), name.as_ptr(), 0);
            }
        }
    }
}
