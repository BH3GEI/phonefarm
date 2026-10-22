//! One host storage domain. Resolving paths never initializes or migrates it.
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const LEGACY: &str = "/private/tmp/claude-501";
const RECORD: &str = "phonefarm/lease-storage@1";
const RETIRED: &str = "phonefarm/lease-domain-retired@1";
const RETIRED_OWNER: &[u8] =
    b"phonefarm-retired: persistent storage since explicit initialization\n";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Storage {
    schema_version: u32,
    pub mode: &'static str,
    pub state_dir: Option<PathBuf>,
    pub devlock_dir: Option<PathBuf>,
    pub device_lease_dir: Option<PathBuf>,
    pub readiness: &'static str,
    pub error: Option<String>,
    #[serde(skip)]
    legacy: PathBuf,
}

// Resolve existing ancestors too: macOS /var and /private/var must bind one target.
fn absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "lease storage path must be absolute: {}",
            path.display()
        ));
    }
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    existing
                        .file_name()
                        .ok_or("storage path has no existing ancestor")?,
                );
                existing = existing.parent().ok_or("storage path parent missing")?;
            }
            Err(e) => return Err(format!("storage path {}: {e}", existing.display())),
        }
    }
    let mut result = existing.canonicalize().map_err(|e| e.to_string())?;
    for component in missing.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn record(path: &Path) -> Result<Option<Value>, String> {
    let m = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(m) => m,
    };
    if !m.is_file() || m.len() > 65536 {
        return Err(format!(
            "not a bounded regular storage record: {}",
            path.display()
        ));
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| format!("invalid storage record {}: {e}", path.display()))
}

// Only our exact retired fence may be re-entered. Unknown contents remain a barrier.
fn retired_matches(dir: &Path, target: &Path) -> Result<bool, String> {
    if !exists(dir) {
        return Ok(false);
    }
    if !fs::symlink_metadata(dir)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("old domain retired entry is not a directory".into());
    }
    let marker = record(&dir.join("phonefarm-lease.json"))?;
    if !marker.is_some_and(|v| v["schema"] == RETIRED && v["devlock_dir"] == json!(target)) {
        return Ok(false);
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        match entry.file_name().to_str() {
            Some("phonefarm-lease.json") => {}
            Some("owner")
                if entry.file_type().map_err(|e| e.to_string())?.is_file()
                    && fs::read(entry.path()).map_err(|e| e.to_string())? == RETIRED_OWNER => {}
            _ => return Err("old retired domain contains an unknown owner or extra entry".into()),
        }
    }
    Ok(true)
}

impl Storage {
    fn from_paths(
        state: Option<PathBuf>,
        custom: Option<PathBuf>,
        devices: Option<PathBuf>,
        legacy: PathBuf,
    ) -> Self {
        let mut s = Self {
            schema_version: 1,
            mode: if custom.is_some() {
                "custom"
            } else {
                "default"
            },
            state_dir: None,
            devlock_dir: None,
            device_lease_dir: None,
            readiness: "unavailable",
            error: None,
            legacy,
        };
        let paths = (|| {
            s.state_dir = state.as_deref().map(absolute).transpose()?;
            s.devlock_dir = Some(match custom {
                Some(ref p) => absolute(p)?,
                None => s
                    .state_dir
                    .as_ref()
                    .ok_or("PHONEFARM_STATE_DIR or HOME is required for persistent lease storage")?
                    .join("locks"),
            });
            s.device_lease_dir = Some(match devices {
                Some(ref p) => absolute(p)?,
                None => s.devlock_dir.as_ref().unwrap().join("devices"),
            });
            for p in [&s.devlock_dir, &s.device_lease_dir].into_iter().flatten() {
                if exists(p) && !p.is_dir() {
                    return Err(format!("storage path is not a directory: {}", p.display()));
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(e) = paths {
            s.error = Some(e);
            return s;
        }
        s.assess();
        s
    }

    fn assess(&mut self) {
        let root = self.devlock_dir.as_ref().unwrap();
        let check = (|| {
            if self.mode == "custom" {
                if record(&root.join("devlock.d/phonefarm-lease.json"))?
                    .is_some_and(|v| v["schema"] == RETIRED)
                {
                    return Err(
                        "custom DEVLOCK_DIR is a retired domain; use its persistent target".into(),
                    );
                }
                return Ok(true);
            }
            if !self.legacy.parent().is_some_and(Path::is_dir) {
                return Err("old default parent is unavailable; configure an explicit persistent DEVLOCK_DIR for a custom deployment".into());
            }
            let registered = record(&root.join("storage.json"))?;
            let retired = retired_matches(&self.legacy.join("devlock.d"), root)?;
            Ok(registered.is_some_and(|v| {
                v["schema"] == RECORD
                    && v["devlock_dir"] == json!(root)
                    && v["legacy_devlock_dir"] == json!(self.legacy)
            }) && retired)
        })();
        match check {
            Ok(true) => {
                self.readiness = "ready";
                self.error = None;
            }
            Ok(false) => {
                self.readiness = "migration_required";
                self.error=Some(format!(
                "lease storage is not initialized or the old domain fence is missing/mismatched; stop all participants, then run phonefarm lease init --participants-stopped (old domain {})", self.legacy.display()));
            }
            Err(e) => {
                self.readiness = "unavailable";
                self.error = Some(e);
            }
        }
    }

    pub(crate) fn require_ready(&self) -> Result<(PathBuf, PathBuf), String> {
        if self.readiness != "ready" {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "lease storage unavailable".into()));
        }
        Ok((
            self.device_lease_dir.clone().unwrap(),
            self.devlock_dir.clone().unwrap(),
        ))
    }

    pub(crate) fn custom(path: &Path) -> Self {
        Self::from_paths(None, Some(path.to_path_buf()), None, PathBuf::from(LEGACY))
    }

    pub(crate) fn init(&self) -> Result<Self, String> {
        if self.mode != "default" {
            return Err("explicit DEVLOCK_DIR uses custom storage; default initialization is not applicable".into());
        }
        // A partial resolution must fail before even creating the old bridge.
        if self.readiness == "unavailable" {
            return Err(self
                .error
                .clone()
                .unwrap_or_else(|| "lease storage paths unavailable".into()));
        }
        let root = self
            .devlock_dir
            .as_ref()
            .ok_or("lease storage root missing")?;
        let devices = self
            .device_lease_dir
            .as_ref()
            .ok_or("device lease storage path missing")?;
        let legacy_parent = self.legacy.parent().ok_or("old domain has no parent")?;
        if !legacy_parent.is_dir() {
            return Err(format!("old default parent {} is unavailable; do not create privileged ancestors implicitly; configure an explicit persistent DEVLOCK_DIR for a custom deployment", legacy_parent.display()));
        }
        ensure_dir(&self.legacy)?;
        // Keep this inode and lock for the whole transition; never copy/unlink locks.
        let bridge = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.legacy.join("phonefarm-bridge.lock"))
            .map_err(|e| e.to_string())?;
        bridge
            .try_lock()
            .map_err(|e| format!("old domain bridge is held/unavailable: {e}"))?;
        if exists(&self.legacy.join("phonefarm-lease.d"))
            || exists(&self.legacy.join("devlock.qmutex"))
        {
            return Err(
                "old domain has a nested lease or queue mutex; explicit reconciliation is required"
                    .into(),
            );
        }
        let queue = self.legacy.join("devlock.queue");
        if exists(&queue) {
            let m = fs::symlink_metadata(&queue).map_err(|e| e.to_string())?;
            if !m.is_file()
                || m.len() > 65536
                || !fs::read_to_string(&queue)
                    .map_err(|e| e.to_string())?
                    .trim()
                    .is_empty()
            {
                return Err("old domain queue is nonempty or unreadable; stop/resolve its participants first".into());
            }
        }
        let retired_dir = self.legacy.join("devlock.d");
        let fence = retired_matches(&retired_dir, root)?;
        if exists(&retired_dir) && !fence {
            return Err("old domain owner/barrier is present or retired to another target; it cannot be moved or cleared by init".into());
        }
        if let Some(v) = record(&root.join("storage.json"))? {
            if v["schema"] != RECORD
                || v["devlock_dir"] != json!(root)
                || v["legacy_devlock_dir"] != json!(self.legacy)
            {
                return Err(
                    "persistent storage is already bound to a different/unknown domain".into(),
                );
            }
        }
        // Reference bounded-name history in place; unrelated files and large logs stay untouched.
        let history: Vec<_> = [
            "devlock.log",
            "devlock.queue",
            "devlock.seen",
            "phonefarm-recovery.jsonl",
        ]
        .iter()
        .map(|n| self.legacy.join(n))
        .filter(|p| exists(p))
        .collect();
        ensure_dir(root)?;
        ensure_dir(devices)?;
        if !fence {
            fs::create_dir(&retired_dir)
                .map_err(|e| format!("old domain changed during initialization: {e}"))?;
            crate::devlease::durable_replace(
                &retired_dir.join("phonefarm-lease.json"),
                json!({"schema":RETIRED,"devlock_dir":root})
                    .to_string()
                    .as_bytes(),
            )?;
            crate::devlease::durable_replace(&retired_dir.join("owner"), RETIRED_OWNER)?;
            File::open(&self.legacy)
                .and_then(|d| d.sync_all())
                .map_err(|e| e.to_string())?;
        }
        crate::devlease::durable_replace(
            &root.join("storage.json"),
            json!({"schema":RECORD,"devlock_dir":root,"legacy_devlock_dir":self.legacy,
                "legacy_metadata":history,"initialized_at":chrono::Utc::now().to_rfc3339()})
            .to_string()
            .as_bytes(),
        )?;
        let mut ready = self.clone();
        ready.assess();
        ready.require_ready()?;
        Ok(ready)
    }
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().ok_or("storage directory has no parent")?;
    ensure_dir(parent)?;
    fs::create_dir(path)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() {
                Ok(())
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("create persistent storage {}: {e}", path.display()))?;
    File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())
}

pub(crate) fn resolve() -> Storage {
    let state = std::env::var_os("PHONEFARM_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state/phonefarm"))
        });
    Storage::from_paths(
        state,
        std::env::var_os("DEVLOCK_DIR").map(PathBuf::from),
        std::env::var_os("PHONEFARM_LEASE_DIR").map(PathBuf::from),
        PathBuf::from(LEGACY),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "lease-storage-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
        fn storage(&self) -> Storage {
            Storage::from_paths(
                Some(self.0.join("state")),
                None,
                None,
                self.0.join("old-domain"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn paths_and_status_are_read_only_and_custom_overrides_are_explicit() {
        let f = Fixture::new();
        let s = f.storage();
        assert_eq!(s.readiness, "migration_required");
        assert_eq!(crate::devlease::storage_status(&s)["status"], "unknown");
        assert!(!f.0.join("state").exists());
        assert!(!f.0.join("old-domain").exists());
        assert!(s.require_ready().is_err());
        let custom = Storage::from_paths(
            None,
            Some(f.0.join("custom")),
            Some(f.0.join("devices")),
            f.0.join("old-domain"),
        );
        assert_eq!(custom.mode, "custom");
        assert_eq!(custom.readiness, "ready");
        assert_eq!(
            custom.require_ready().unwrap(),
            (f.0.join("devices"), f.0.join("custom"))
        );
        assert!(!f.0.join("custom").exists());
        assert_eq!(
            Storage::from_paths(None, Some("relative".into()), None, f.0.join("old-domain"))
                .readiness,
            "unavailable"
        );
        assert_eq!(
            Storage::from_paths(
                Some(f.0.join("state")),
                None,
                None,
                f.0.join("missing-parent/old-domain")
            )
            .readiness,
            "unavailable"
        );
        assert!(!f.0.join("missing-parent").exists());
        let invalid_devices = Storage::from_paths(
            Some(f.0.join("state")),
            None,
            Some("relative-devices".into()),
            f.0.join("old-domain"),
        );
        assert!(invalid_devices.init().unwrap_err().contains("absolute"));
        assert!(!f.0.join("old-domain").exists());
        assert!(!f.0.join("state").exists());
    }

    #[test]
    fn initialization_rejects_old_owners_queue_mutex_and_bridge_then_binds_one_target() {
        let f = Fixture::new();
        let s = f.storage();
        let old = &s.legacy;
        fs::create_dir(old).unwrap();
        fs::write(old.join("unrelated.txt"), b"leave this alone").unwrap();
        fs::write(old.join("devlock.log"), b"old history\n").unwrap();
        fs::write(old.join("devlock.queue"), b"waiting\n").unwrap();
        assert!(s.init().unwrap_err().contains("queue"));
        fs::write(old.join("devlock.queue"), b"\n").unwrap();
        fs::create_dir(old.join("devlock.qmutex")).unwrap();
        assert!(s.init().unwrap_err().contains("mutex"));
        fs::remove_dir(old.join("devlock.qmutex")).unwrap();
        fs::create_dir(old.join("devlock.d")).unwrap();
        fs::write(old.join("devlock.d/owner"), "other participant").unwrap();
        assert!(s.init().unwrap_err().contains("owner/barrier"));
        fs::remove_file(old.join("devlock.d/owner")).unwrap();
        fs::remove_dir(old.join("devlock.d")).unwrap();
        let bridge = File::open(old.join("phonefarm-bridge.lock")).unwrap();
        bridge.try_lock().unwrap();
        assert!(s.init().unwrap_err().contains("bridge"));
        drop(bridge);
        let ready = s.init().unwrap();
        assert_eq!(ready.readiness, "ready");
        assert_eq!(f.storage().readiness, "ready");
        assert_eq!(
            fs::read(old.join("unrelated.txt")).unwrap(),
            b"leave this alone"
        );
        assert_eq!(fs::read(old.join("devlock.log")).unwrap(), b"old history\n");
        assert_eq!(Storage::custom(old).readiness, "unavailable");
        fs::write(old.join("devlock.d/unexpected"), b"do not ignore").unwrap();
        assert!(s.init().unwrap_err().contains("extra entry"));
        assert_eq!(f.storage().readiness, "unavailable");
        fs::remove_file(old.join("devlock.d/unexpected")).unwrap();
        fs::write(old.join("devlock.d/owner"), b"another owner").unwrap();
        assert!(s.init().unwrap_err().contains("unknown owner"));
        fs::write(old.join("devlock.d/owner"), RETIRED_OWNER).unwrap();
        let different = Storage::from_paths(Some(f.0.join("other-state")), None, None, old.clone());
        assert!(different.init().unwrap_err().contains("another target"));
        assert!(!f.0.join("other-state").exists());
    }

    #[test]
    fn missing_temporary_fence_requires_reinit_while_persistent_recovery_is_preserved() {
        let f = Fixture::new();
        let s = f.storage().init().unwrap();
        let (devices, locks) = s.require_ready().unwrap();
        let mut lease = crate::devlease::Lease::acquire(
            &devices,
            &locks,
            "fake-storage-device",
            "host-only",
            Duration::from_millis(50),
        )
        .unwrap();
        lease.hold_for_recovery(&f.0).unwrap();
        drop(lease);
        let marker = locks.join("devlock.d/phonefarm-lease.json");
        let before = fs::read(&marker).unwrap();
        assert_eq!(crate::devlease::storage_status(&s)["status"], "blocked");
        fs::remove_dir_all(s.legacy.join("devlock.d")).unwrap();
        let after_cleanup = f.storage();
        assert_eq!(after_cleanup.readiness, "migration_required");
        assert!(after_cleanup.require_ready().is_err());
        assert_eq!(
            crate::devlease::storage_status(&after_cleanup)["status"],
            "blocked"
        );
        let repaired = after_cleanup.init().unwrap();
        assert_eq!(repaired.readiness, "ready");
        assert_eq!(fs::read(&marker).unwrap(), before);
        assert!(crate::devlease::Lease::acquire(
            &devices,
            &locks,
            "fake-storage-device",
            "next",
            Duration::from_millis(50)
        )
        .is_err());
        assert_eq!(repaired.init().unwrap().readiness, "ready");
        assert_eq!(fs::read(&marker).unwrap(), before);
    }
}
