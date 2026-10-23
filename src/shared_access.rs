//! Opt-in permissions for declared PF output and persistent lease directories.
//! Existing objects are checked, never migrated or recursively chmodded.
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy)]
pub(crate) struct Policy {
    gid: u32,
}

fn error(message: impl Into<String>) -> std::io::Error {
    std::io::Error::other(message.into())
}
fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => out.push(component.as_os_str()),
            Component::CurDir => {}
            _ => return Err(error("shared path must not contain traversal")),
        }
    }
    // Resolve directory aliases (e.g. /var on macOS), retaining the final
    // component so its later O_NOFOLLOW open still rejects a symlink.
    let mut parent = out.parent().unwrap_or(&out);
    let mut tail = Vec::new();
    while !parent.exists() {
        tail.push(
            parent
                .file_name()
                .ok_or_else(|| error("shared path parent missing"))?,
        );
        parent = parent
            .parent()
            .ok_or_else(|| error("shared path parent missing"))?;
    }
    let mut resolved = parent.canonicalize()?;
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    if let Some(name) = out.file_name() {
        resolved.push(name);
    }
    Ok(resolved)
}

impl Policy {
    pub(crate) fn from_env() -> std::io::Result<Option<Self>> {
        let Some(value) = std::env::var_os("PHONEFARM_SHARED_GID") else {
            return Ok(None);
        };
        let value = value
            .to_str()
            .ok_or_else(|| error("PHONEFARM_SHARED_GID must be decimal"))?;
        let gid = value
            .parse::<u32>()
            .ok()
            .filter(|v| *v > 0 && *v != u32::MAX)
            .filter(|_| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| error("PHONEFARM_SHARED_GID must be a positive group ID"))?;
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut groups = vec![0 as libc::gid_t; count as usize];
        if unsafe { libc::getgroups(count, groups.as_mut_ptr()) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::getegid() } != gid && !groups.contains(&gid) {
            return Err(error("process does not belong to PHONEFARM_SHARED_GID"));
        }
        Ok(Some(Self { gid }))
    }
    pub(crate) fn apply_new(&self, file: &File, mode: u32) -> std::io::Result<()> {
        // fchown may clear mode bits: always establish the final mode afterwards.
        if unsafe { libc::fchown(file.as_raw_fd(), !0 as libc::uid_t, self.gid) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    pub(crate) fn check(&self, file: &File, mode: u32) -> std::io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.gid() != self.gid
            || metadata.mode() & mode != mode
            || metadata.mode() & 0o007 != 0
        {
            return Err(error("existing PF shared object has incompatible group/permissions; explicit offline migration is required"));
        }
        Ok(())
    }
}

/// Generic writers share only explicitly declared roots; callers never gain
/// permissions on inputs, node metadata, installations or arbitrary HOME files.
pub(crate) fn for_path(path: &Path) -> std::io::Result<Option<Policy>> {
    let Some(policy) = Policy::from_env()? else {
        return Ok(None);
    };
    let path = absolute(path)?;
    for key in [
        "PF_JOB_OUT",
        "PHONEFARM_STATE_DIR",
        "DEVLOCK_DIR",
        "PHONEFARM_LEASE_DIR",
    ] {
        if let Some(root) = std::env::var_os(key) {
            let root = PathBuf::from(root);
            if !root.is_absolute() {
                return Err(error(format!("{key} must be absolute in shared mode")));
            }
            let root = absolute(&root)?;
            if path.starts_with(&root) {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                    .open(&root)?;
                policy.check(&file, 0o2770)?;
                return Ok(Some(policy));
            }
        }
    }
    Ok(None)
}

pub(crate) fn check_directory(path: &Path) -> std::io::Result<()> {
    if let Some(policy) = for_path(path)? {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)?;
        policy.check(&file, 0o2770)?;
    }
    Ok(())
}
pub(crate) fn check_existing(path: &Path, writable: bool) -> std::io::Result<()> {
    if let Some(policy) = for_path(path)? {
        check_directory(
            path.parent()
                .ok_or_else(|| error("shared file parent missing"))?,
        )?;
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => {
                if !file.metadata()?.is_file() {
                    return Err(error("shared record is not regular"));
                }
                policy.check(&file, if writable { 0o660 } else { 0o640 })?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
pub(crate) fn created_file(path: &Path, file: &File, writable: bool) -> std::io::Result<()> {
    if let Some(policy) = for_path(path)? {
        check_directory(
            path.parent()
                .ok_or_else(|| error("shared file parent missing"))?,
        )?;
        policy.apply_new(file, if writable { 0o660 } else { 0o640 })?;
    }
    Ok(())
}
pub(crate) fn create_dir(path: &Path) -> std::io::Result<()> {
    let Some(policy) = for_path(path)? else {
        return fs::create_dir(path);
    };
    let parent = path
        .parent()
        .ok_or_else(|| error("shared directory parent missing"))?;
    check_directory(parent)?;
    fs::DirBuilder::new().mode(0o700).create(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(path)?;
    policy.apply_new(&file, 0o2770)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()
}
pub(crate) fn create_dir_all(path: &Path) -> std::io::Result<()> {
    if for_path(path)?.is_none() {
        return fs::create_dir_all(path);
    }
    match fs::symlink_metadata(path) {
        Ok(_) => return check_directory(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let parent = path
        .parent()
        .ok_or_else(|| error("shared directory parent missing"))?;
    if !parent.exists() {
        if for_path(parent)?.is_none() {
            return Err(error("shared root parent must already exist"));
        }
        create_dir_all(parent)?;
    }
    match create_dir(path) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => check_directory(path),
        result => result,
    }
}

/// Do not chmod an existing stable lock: every UID must open the same inode.
pub(crate) fn lock_file(path: &Path, default_mode: u32) -> std::io::Result<File> {
    let policy = for_path(path)?;
    if policy.is_some() {
        check_directory(path.parent().ok_or_else(|| error("lock parent missing"))?)?;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .mode(default_mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    match options.create_new(true).open(path) {
        Ok(file) => {
            if let Some(policy) = policy {
                policy.apply_new(&file, 0o660)?;
                file.sync_all()?;
            }
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = options.create_new(false).open(path)?;
            if !file.metadata()?.is_file() {
                return Err(error("lock is not a regular file"));
            }
            if let Some(policy) = policy {
                policy.check(&file, 0o660)?;
            }
            Ok(file)
        }
        Err(e) => Err(e),
    }
}

pub(crate) fn append_file(path: &Path) -> std::io::Result<File> {
    let Some(policy) = for_path(path)? else {
        return OpenOptions::new().create(true).append(true).open(path);
    };
    check_directory(
        path.parent()
            .ok_or_else(|| error("append parent missing"))?,
    )?;
    let mut options = OpenOptions::new();
    options
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    match options.create_new(true).open(path) {
        Ok(file) => {
            policy.apply_new(&file, 0o660)?;
            Ok(file)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = options.create_new(false).open(path)?;
            if !file.metadata()?.is_file() {
                return Err(error("append target is not regular"));
            }
            policy.check(&file, 0o660)?;
            Ok(file)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn new_shared_inode_survives_atomic_replace_and_private_state_is_not_repaired() {
        let path = std::env::temp_dir().join(format!(
            "pf-shared-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let policy = Policy {
            gid: unsafe { libc::getegid() },
        };
        let dir = File::open(&path).unwrap();
        policy.apply_new(&dir, 0o2770).unwrap();
        let old = path.join("record");
        fs::write(&old, b"old").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(policy.check(&File::open(&old).unwrap(), 0o640).is_err());
        assert_eq!(fs::metadata(&old).unwrap().mode() & 0o777, 0o600);
        let tmp = path.join("new");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .unwrap();
        policy.apply_new(&file, 0o640).unwrap();
        file.sync_all().unwrap();
        fs::rename(&tmp, &old).unwrap();
        policy.check(&File::open(&old).unwrap(), 0o640).unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
