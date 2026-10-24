//! Versioned paths within one lock domain. The root bridge/mutex never move.
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const RECORD: &str = "phonefarm/lease-domain@2";
const FENCE: &str = "phonefarm/lease-domain-fence@2";
const LIMIT: u64 = 64 * 1024;
#[derive(Clone, Debug)]
pub(crate) struct Paths {
    pub queue_dir: PathBuf,
    pub layout_version: u32,
    pub domain_id: Option<String>,
    pub device_lease_dir: Option<PathBuf>,
}
impl Paths {
    pub(crate) fn require_device_root(&self, actual: &Path) -> Result<(), String> {
        if let Some(expected) = &self.device_lease_dir {
            if &crate::lease_storage::absolute(actual)? != expected {
                return Err(
                    "domain device_lease_dir differs from configured device root; preserved".into(),
                );
            }
        }
        Ok(())
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: String,
    domain_id: String,
    devlock_dir: PathBuf,
    device_lease_dir: PathBuf,
    queue_dir: PathBuf,
    initialized_at: String,
}
#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Fence {
    schema: String,
    domain_id: String,
    devlock_dir: PathBuf,
}
impl Record {
    fn paths(&self) -> Paths {
        Paths {
            queue_dir: self.queue_dir.clone(),
            layout_version: 2,
            domain_id: Some(self.domain_id.clone()),
            device_lease_dir: Some(self.device_lease_dir.clone()),
        }
    }
    fn fence(&self) -> Fence {
        Fence {
            schema: FENCE.into(),
            domain_id: self.domain_id.clone(),
            devlock_dir: self.devlock_dir.clone(),
        }
    }
}
fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
fn directory(path: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err(format!(
            "not a regular domain directory: {}",
            path.display()
        ));
    }
    crate::shared_access::check_directory(path).map_err(|e| e.to_string())
}
fn bounded(path: &Path) -> Result<Option<Vec<u8>>, String> {
    if !exists(path)? {
        return Ok(None);
    }
    crate::shared_access::check_existing(path, false).map_err(|e| e.to_string())?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.len() > LIMIT {
        return Err(format!(
            "not a bounded regular domain record: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("domain record exceeds 64 KiB".into());
    }
    Ok(Some(bytes))
}
fn read_record(root: &Path) -> Result<Option<Record>, String> {
    let Some(bytes) = bounded(&root.join("domain.json"))? else {
        return Ok(None);
    };
    let r: Record =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid domain record: {e}"))?;
    if r.schema != RECORD
        || !crate::queue_owner::valid_instance(&r.domain_id)
        || r.devlock_dir != root
        || r.queue_dir != root.join("v2/global")
        || crate::lease_storage::absolute(&r.device_lease_dir)? != r.device_lease_dir
        || chrono::DateTime::parse_from_rfc3339(&r.initialized_at).is_err()
    {
        return Err("domain record identity/path binding is invalid; preserved".into());
    }
    Ok(Some(r))
}
fn exact_fence(root: &Path, record: &Record) -> Result<bool, String> {
    let Some(bytes) = bounded(&root.join("devlock.d"))? else {
        return Ok(false);
    };
    let fence: Fence =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid domain fence: {e}"))?;
    if fence != record.fence() {
        return Err("domain fence differs from its record; preserved".into());
    }
    Ok(true)
}
fn legacy_quiet(root: &Path) -> Result<(), String> {
    for name in ["phonefarm-lease.d", "devlock.qmutex"] {
        if exists(&root.join(name))? {
            return Err(format!(
                "old domain contains {name}; explicit reconciliation is required"
            ));
        }
    }
    if let Some(bytes) = bounded(&root.join("devlock.queue"))? {
        if !std::str::from_utf8(&bytes)
            .map_err(|e| e.to_string())?
            .trim()
            .is_empty()
        {
            return Err("old domain queue is not empty; preserved".into());
        }
    }
    Ok(())
}
fn entries(path: &Path, allowed: &[&str]) -> Result<(), String> {
    directory(path)?;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|n| allowed.contains(&n))
        {
            return Err(format!(
                "domain directory contains an unknown entry: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}
fn layout_dirs(root: &Path, partial: bool) -> Result<(), String> {
    let v2 = root.join("v2");
    let global = v2.join("global");
    if partial && !exists(&v2)? {
        return Ok(());
    }
    entries(&v2, &["global"])?;
    if partial && !exists(&global)? {
        return Ok(());
    }
    entries(
        &global,
        if partial {
            &[]
        } else {
            &[
                "devlock.d",
                "phonefarm-lease.d",
                "devlock.queue",
                "devlock.queue.tmp",
                "devlock.seen",
                "devlock.seen.tmp",
                "devlock.log",
                "devlock.qmutex",
            ]
        },
    )?;
    for entry in fs::read_dir(&global).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let directory = matches!(
            entry.file_name().to_str(),
            Some("devlock.d" | "phonefarm-lease.d" | "devlock.qmutex")
        );
        if if directory {
            !kind.is_dir()
        } else {
            !kind.is_file()
        } {
            return Err(format!(
                "domain entry has an unknown file type: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}
/// Read only. Once a record exists, no error can fall back to the old layout.
pub(crate) fn inspect(root: &Path) -> Result<Paths, String> {
    let root = crate::lease_storage::absolute(root)?;
    if exists(&root)? {
        directory(&root)?;
    }
    let Some(record) = read_record(&root)? else {
        if exists(&root.join("v2"))? {
            return Err("unregistered v2 directory; preserved".into());
        }
        if exists(&root.join("devlock.d"))? {
            let m = fs::symlink_metadata(root.join("devlock.d")).map_err(|e| e.to_string())?;
            if !m.is_dir() {
                return Err(
                    "domain fence/unknown old slot exists without a domain record; preserved"
                        .into(),
                );
            }
        }
        return Ok(Paths {
            queue_dir: root,
            layout_version: 1,
            domain_id: None,
            device_lease_dir: None,
        });
    };
    if !exact_fence(&root, &record)? {
        return Err(format!(
            "domain migration required: {} has no matching fence; rerun explicit domain-init",
            record.domain_id
        ));
    }
    legacy_quiet(&root)?;
    layout_dirs(&root, false)?;
    Ok(record.paths())
}
fn sync(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
fn create_directory(path: &Path) -> Result<(), String> {
    if exists(path)? {
        directory(path)?;
    } else {
        crate::shared_access::create_dir(path).map_err(|e| e.to_string())?;
    }
    sync(path)?;
    sync(path.parent().ok_or("domain directory parent missing")?)
}
fn publish_fence(root: &Path, record: &Record) -> Result<(), String> {
    if exact_fence(root, record)? {
        return sync(root);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temp = root.join(format!(".domain-fence-{}-{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        crate::shared_access::created_file(&temp, &file, false).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(&record.fence()).map_err(|e| e.to_string())?)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::hard_link(&temp, root.join("devlock.d"))
            .map_err(|e| format!("old owner/fence slot changed; preserved: {e}"))?;
        sync(root)
    })();
    let removed = fs::remove_file(&temp).map_err(|e| e.to_string());
    result?;
    removed?;
    sync(root)
}
/// Explicit offline metadata upgrade. A complete domain is an idempotent read,
/// including while its v2 owner is active; only partial completion requires idle.
pub(crate) fn init(root: &Path, device_root: &Path) -> Result<Paths, String> {
    let root = crate::lease_storage::absolute(root)?;
    let device_root = crate::lease_storage::absolute(device_root)?;
    if let Ok(paths) = inspect(&root) {
        paths.require_device_root(&device_root)?;
        if paths.layout_version == 2 {
            return Ok(paths);
        }
    }
    crate::shared_access::create_dir_all(&root).map_err(|e| e.to_string())?;
    directory(&root)?;
    let bridge = crate::shared_access::lock_file(&root.join("phonefarm-bridge.lock"), 0o600)
        .map_err(|e| e.to_string())?;
    bridge
        .try_lock()
        .map_err(|e| format!("domain bridge is held/unavailable: {e}"))?;
    let _owner = crate::queue_owner::lock(&root)?;
    // Another initializer may have finished while we acquired the stable locks.
    if let Ok(paths) = inspect(&root) {
        paths.require_device_root(&device_root)?;
        if paths.layout_version == 2 {
            return Ok(paths);
        }
    }
    legacy_quiet(&root)?;
    let record = match read_record(&root)? {
        Some(record) => {
            record.paths().require_device_root(&device_root)?;
            record
        }
        None => {
            if exists(&root.join("devlock.d"))? || exists(&root.join("v2"))? {
                return Err(
                    "old owner/fence or unregistered v2 directory exists; preserved".into(),
                );
            }
            let mut bytes = [0u8; 16];
            File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut bytes))
                .map_err(|e| e.to_string())?;
            let record = Record {
                schema: RECORD.into(),
                domain_id: bytes.iter().map(|b| format!("{b:02x}")).collect(),
                devlock_dir: root.clone(),
                device_lease_dir: device_root,
                queue_dir: root.join("v2/global"),
                initialized_at: chrono::Utc::now().to_rfc3339(),
            };
            crate::devlease::durable_replace(
                &root.join("domain.json"),
                &serde_json::to_vec(&record).map_err(|e| e.to_string())?,
            )?;
            record
        }
    };
    // Existing old owner directories are not a partial fence belonging to us.
    exact_fence(&root, &record)?;
    layout_dirs(&root, true)?;
    create_directory(&root.join("v2"))?;
    create_directory(&record.queue_dir)?;
    legacy_quiet(&root)?;
    publish_fence(&root, &record)?;
    let paths = inspect(&root)?;
    paths.require_device_root(&record.device_lease_dir)?;
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!("pf-domain-{}-{stamp}", std::process::id())))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn upgrade_keeps_root_inodes_identity_and_active_domain_readback() {
        let s = Scratch::new();
        assert_eq!(inspect(&s.0).unwrap().layout_version, 1);
        assert!(!s.0.exists());
        fs::create_dir(&s.0).unwrap();
        let bridge = File::create(s.0.join("phonefarm-bridge.lock")).unwrap();
        let mutex = File::create(s.0.join("queue-owner.lock")).unwrap();
        fs::write(s.0.join("device-registry.json"), b"original registry").unwrap();
        let paths = init(&s.0, &s.0.join("devices")).unwrap();
        assert_eq!(paths.layout_version, 2);
        assert!(s.0.join("devlock.d").is_file());
        assert_eq!(
            fs::metadata(s.0.join("phonefarm-bridge.lock"))
                .unwrap()
                .ino(),
            bridge.metadata().unwrap().ino()
        );
        assert_eq!(
            fs::metadata(s.0.join("queue-owner.lock")).unwrap().ino(),
            mutex.metadata().unwrap().ino()
        );
        bridge.try_lock().unwrap();
        fs::create_dir(paths.queue_dir.join("devlock.d")).unwrap();
        assert_eq!(
            init(&s.0, &s.0.join("devices")).unwrap().domain_id,
            paths.domain_id
        );
        assert_eq!(
            fs::read(s.0.join("device-registry.json")).unwrap(),
            b"original registry"
        );
        assert!(init(&s.0, &s.0.join("other-devices")).is_err());
    }
    #[test]
    fn record_without_fence_resumes_same_id_but_never_claims_unknown_entries() {
        let s = Scratch::new();
        let first = init(&s.0, &s.0.join("devices")).unwrap();
        let record = fs::read(s.0.join("domain.json")).unwrap();
        fs::remove_file(s.0.join("devlock.d")).unwrap();
        assert!(inspect(&s.0).unwrap_err().contains("migration required"));
        fs::write(first.queue_dir.join("unrecognized"), b"keep").unwrap();
        assert!(init(&s.0, &s.0.join("devices")).is_err());
        assert_eq!(fs::read(s.0.join("domain.json")).unwrap(), record);
        fs::remove_file(first.queue_dir.join("unrecognized")).unwrap();
        assert_eq!(
            init(&s.0, &s.0.join("devices")).unwrap().domain_id,
            first.domain_id
        );
        assert_eq!(fs::read(s.0.join("domain.json")).unwrap(), record);
        fs::write(s.0.join("devlock.d"), b"{}").unwrap();
        assert!(inspect(&s.0).is_err());
        assert!(init(&s.0, &s.0.join("devices")).is_err());
        assert_eq!(fs::read(s.0.join("devlock.d")).unwrap(), b"{}");
    }
    #[test]
    fn upgrade_and_v2_inspection_refuse_old_owners_queue_and_unknown_layout() {
        let s = Scratch::new();
        fs::create_dir(&s.0).unwrap();
        for name in ["devlock.d", "phonefarm-lease.d", "devlock.qmutex", "v2"] {
            fs::create_dir(s.0.join(name)).unwrap();
            assert!(init(&s.0, &s.0.join("devices")).is_err());
            assert!(s.0.join(name).is_dir());
            assert!(!s.0.join("domain.json").exists());
            fs::remove_dir(s.0.join(name)).unwrap();
        }
        fs::write(s.0.join("devlock.queue"), b"waiting\n").unwrap();
        assert!(init(&s.0, &s.0.join("devices")).is_err());
        fs::write(s.0.join("devlock.queue"), b"\n").unwrap();
        let bridge = File::open(s.0.join("phonefarm-bridge.lock")).unwrap();
        bridge.try_lock().unwrap();
        assert!(init(&s.0, &s.0.join("devices")).is_err());
        drop(bridge);
        init(&s.0, &s.0.join("devices")).unwrap();
        fs::write(s.0.join("devlock.queue"), b"late old writer\n").unwrap();
        assert!(inspect(&s.0).is_err());
        assert!(init(&s.0, &s.0.join("devices")).is_err());
    }
}
