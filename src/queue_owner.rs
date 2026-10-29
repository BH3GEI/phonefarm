//! Storage-initialization helpers; no task owner or acquisition protocol lives here.
use std::fs::File;
#[cfg(test)]
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::{Duration, Instant};
#[cfg(test)]
use std::os::unix::fs::MetadataExt;
#[cfg(test)]
pub(crate) const QUEUE_MUTEX_BUSY: &str = "queue mutex exists; existing state preserved";

// The shell's FIFO uses this directory, not queue-owner.lock. Never wait for or
// steal it while holding bridge + queue-owner; remove only our own empty inode.
#[cfg(test)]
pub(crate) struct QueueMutex {
    path: PathBuf,
    device: u64,
    inode: u64,
    owned: bool,
}
#[cfg(test)]
impl QueueMutex {
    pub(crate) fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join("devlock.qmutex");
        crate::shared_access::create_dir(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                QUEUE_MUTEX_BUSY.to_owned()
            } else {
                e.to_string()
            }
        })?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            owned: true,
        })
    }
    pub(crate) fn release(&mut self) -> Result<(), String> {
        if !self.owned {
            return Ok(());
        }
        let metadata = fs::symlink_metadata(&self.path).map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.dev() != self.device || metadata.ino() != self.inode {
            return Err("queue mutex identity changed; preserved".into());
        }
        fs::remove_dir(&self.path).map_err(|e| e.to_string())?;
        self.owned = false;
        Ok(())
    }
}
#[cfg(test)]
impl Drop for QueueMutex {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub(crate) fn valid_instance(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn lock(root: &Path) -> Result<File, String> {
    crate::shared_access::create_dir_all(root).map_err(|e| e.to_string())?;
    let file = crate::shared_access::lock_file(&root.join("queue-owner.lock"), 0o600)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("queue metadata mutex is not a file".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(format!("queue metadata mutex unavailable: {e}")),
        }
    }
}
