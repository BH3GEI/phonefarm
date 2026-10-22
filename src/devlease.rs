//! Host leases cover a whole experiment, including restoration. File locks are never unlinked.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Lease {
    _device: File,
    _bridge: File,
    legacy: PathBuf,
    token: String,
    stop: Option<mpsc::Sender<()>>,
    heartbeat: Option<JoinHandle<()>>,
    preserve_marker: bool,
    pub serial: String,
}

fn lock_file(path: &Path, deadline: Instant) -> Result<File, String> {
    let f = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("lease open {}: {e}", path.display()))?;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(e) => return Err(format!("device lease busy/timeout {}: {e}", path.display())),
        }
    }
}

// Durability is required before device writes: never truncate the live ownership/uncertainty marker.
fn durable_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("marker parent missing")?;
    let tmp = parent.join(format!(
        ".lease-write-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    ));
    let mut f = File::options()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())
}
/// 农场设备锁 (`tools/devlock`, 由 main 的 `devlock::gate` 在碰设备的子命令外层拿) 已被本进程树
/// 持有时 (`DEVLOCK_LABEL=L` 且 devlock.d/owner 是 `L since ...`), 租约不能再去抢同一个 devlock.d
/// (否则自己等自己), 改用旁边的 `phonefarm-lease.d` 放 owner/marker/恢复屏障; 外层锁照旧排斥别的参与者。
const NESTED_DIR: &str = "phonefarm-lease.d";
fn outer_devlock_is_ours(legacy_root: &Path) -> bool {
    let Some(label) = std::env::var("DEVLOCK_LABEL").ok().filter(|l| !l.trim().is_empty()) else {
        return false;
    };
    fs::read_to_string(legacy_root.join("devlock.d").join("owner")).is_ok_and(|o| {
        !o.contains("token=") && o.split_whitespace().next() == Some(label.trim())
    })
}
/// 本次租约用的目录: 外层农场锁归本进程树时用 [`NESTED_DIR`], 否则就是 devlock.d。
fn lease_dir(legacy_root: &Path) -> PathBuf {
    if outer_devlock_is_ours(legacy_root) {
        legacy_root.join(NESTED_DIR)
    } else {
        legacy_root.join("devlock.d")
    }
}
/// 恢复类入口: 哪个目录里有 phonefarm marker 就用哪个 (devlock.d 优先)。
fn recovery_dir(legacy_root: &Path) -> PathBuf {
    let main = legacy_root.join("devlock.d");
    if marker(&main).is_none() && marker(&legacy_root.join(NESTED_DIR)).is_some() {
        legacy_root.join(NESTED_DIR)
    } else {
        main
    }
}
fn marker(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path.join("phonefarm-lease.json")).ok()?).ok()
}
/// Missing extension fields support old format-1 owners; present malformed fields never do.
fn marker_state(m: &Value) -> Result<(bool, bool), String> {
    if !m.is_object() || m["format"] != 1 || m["producer"] != "phonefarm" {
        return Err("unknown lease marker format/producer".into());
    }
    // Format-1 is a closed recovery contract, not a bag of optional hints. An older
    // consumer must not clear a newer barrier that it does not understand.
    const KEYS: &[&str] = &[
        "format",
        "producer",
        "pid",
        "token",
        "serial",
        "label",
        "owner",
        "command_pending",
        "pending_operation",
        "child_pid",
        "child_pgid",
        "recovery_required",
        "recovery_context",
        "previous_owner",
    ];
    if let Some(key) = m
        .as_object()
        .unwrap()
        .keys()
        .find(|key| !KEYS.contains(&key.as_str()))
    {
        return Err(format!(
            "unknown lease marker field {key}; upgrade the recovery consumer"
        ));
    }
    let flag = |key: &str| match m.get(key) {
        None => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| format!("invalid lease {key}: expected boolean")),
    };
    let pending = flag("command_pending")?;
    let recovery = flag("recovery_required")?;
    match (m.get("child_pid"), m.get("child_pgid")) {
        (None, None) => {}
        (Some(pid), Some(pgid)) => {
            let valid = |v: &Value| v.as_u64().filter(|n| *n > 0 && *n <= i32::MAX as u64);
            let pid = valid(pid).ok_or("invalid lease child PID")?;
            let pgid = valid(pgid).ok_or("invalid lease child PGID")?;
            if pid != pgid {
                return Err("lease child PID/PGID do not identify the same owned leader".into());
            }
        }
        _ => return Err("incomplete lease child PID/PGID pair; local group unknown".into()),
    }
    Ok((pending, recovery))
}
fn owns(path: &Path, token: &str) -> bool {
    marker(path).is_some_and(|v| {
        v["token"].as_str() == Some(token)
            && v["owner"].as_str().is_some_and(|expected| {
                fs::read_to_string(path.join("owner")).is_ok_and(|actual| actual == expected)
            })
    })
}
fn pid_dead(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // `ps` unavailable/failed is unknown, never permission to remove another lock.
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|o| {
            pid > 0 && o.status.code() == Some(1) && o.stdout.is_empty() && o.stderr.is_empty()
        })
}
fn remove_owned(path: &Path, token: &str) {
    if owns(path, token) {
        let _ = fs::remove_file(path.join("owner"));
        let _ = fs::remove_file(path.join("phonefarm-lease.json"));
        let _ = fs::remove_dir(path); // Never recursively delete other files/owners.
    }
}

impl Lease {
    pub fn acquire(
        root: &Path,
        legacy_root: &Path,
        serial: &str,
        label: &str,
        timeout: Duration,
    ) -> Result<Self, String> {
        if serial.trim().is_empty() {
            return Err("lease requires a resolved device serial".into());
        }
        fs::create_dir_all(root).map_err(|e| format!("lease directory: {e}"))?;
        fs::create_dir_all(legacy_root).map_err(|e| format!("devlock directory: {e}"))?;
        let deadline = Instant::now() + timeout;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        // Legacy devlock is global. Same order for every participant, no ABBA deadlock.
        let bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        let legacy = lease_dir(legacy_root);
        // 另一个目录里残留的租约 (崩溃/恢复屏障) 同样阻断, 不因换了目录而被忽略
        for other in [legacy_root.join("devlock.d"), legacy_root.join(NESTED_DIR)] {
            if other == legacy {
                continue;
            }
            if let Some(m) = marker(&other) {
                if !marker_state(&m).is_ok_and(|(pending, recovery)| !pending && !recovery) {
                    return Err(format!("lease barrier present in {}; reconcile/recover it first", other.display()));
                }
            }
        }
        loop {
            match fs::create_dir(&legacy) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(m) = marker(&legacy) {
                        let (pending, recovery) = marker_state(&m)?;
                        if pending {
                            return Err("unbound pending transport launch cannot be cleared by idle assertion".into());
                        }
                        if recovery {
                            return Err(format!("RECOVERY_REQUIRED: explicit confirmation-driver reconcile required ({})", legacy.display()));
                        }
                        if m["format"] == 1 && m["producer"] == "phonefarm" {
                            if let (Some(pid), Some(token)) =
                                (m["pid"].as_u64(), m["token"].as_str())
                            {
                                if pid > 0
                                    && pid <= u32::MAX as u64
                                    && pid_dead(pid as u32)
                                    && owns(&legacy, token)
                                {
                                    return Err(format!("ABANDONED lease {}: parent exited; child/remote workload and device restoration are unverified. After verifying device idle/restored, use lease --recover-abandoned --confirm-device-idle --serial {serial}", legacy.display()));
                                }
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(format!("manual/legacy devlock busy: {}. Release the outer manual devlock; ownership cannot be inferred from its label", legacy.display()));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(format!("devlock acquire: {e}")),
            }
        }
        let token = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let owner = format!(
            "phonefarm:{label}:{serial} since {} token={token}\n",
            chrono::Utc::now().to_rfc3339()
        );
        let info = json!({"format":1,"producer":"phonefarm","pid":std::process::id(),
                          "token":token,"serial":serial,"label":label,"owner":owner});
        if let Err(e) = durable_replace(
            &legacy.join("phonefarm-lease.json"),
            info.to_string().as_bytes(),
        ) {
            let _ = fs::remove_dir(&legacy);
            return Err(format!("devlock marker: {e}"));
        }
        if let Err(e) = durable_replace(&legacy.join("owner"), owner.as_bytes()) {
            let _ = fs::remove_file(legacy.join("phonefarm-lease.json"));
            let _ = fs::remove_file(legacy.join("owner"));
            let _ = fs::remove_dir(&legacy);
            return Err(format!("devlock heartbeat: {e}"));
        }
        File::open(legacy_root)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let hb_path = legacy.clone();
        let hb_token = token.clone();
        let heartbeat = std::thread::spawn(move || {
            while rx.recv_timeout(Duration::from_secs(5)) == Err(mpsc::RecvTimeoutError::Timeout) {
                if !owns(&hb_path, &hb_token) {
                    break;
                }
                if let Err(e) = durable_replace(&hb_path.join("owner"), owner.as_bytes()) {
                    eprintln!("lease heartbeat failed: {e}");
                    break;
                }
            }
        });
        Ok(Self {
            _device: device,
            _bridge: bridge,
            legacy,
            token,
            stop: Some(tx),
            heartbeat: Some(heartbeat),
            preserve_marker: false,
            serial: serial.into(),
        })
    }

    pub fn check(&self) -> Result<(), String> {
        marker_state(&marker(&self.legacy).ok_or("lease marker unavailable")?)?;
        if !owns(&self.legacy, &self.token) {
            return Err("lease owner changed; experiment contaminated/unknown".into());
        }
        let age = fs::metadata(self.legacy.join("owner"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
        if age.is_none_or(|d| d > Duration::from_secs(10)) {
            return Err("lease heartbeat stale/unknown; experiment is invalid".into());
        }
        Ok(())
    }

    pub(crate) fn record_command_pending(&self, operation: &str) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["command_pending"] = json!(true);
        m["pending_operation"] = json!(operation);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
    }
    pub(crate) fn record_child(&self, pid: u32) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["command_pending"] = json!(false);
        m["child_pid"] = json!(pid);
        m["child_pgid"] = json!(pid);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| format!("record child: {e}"))
    }

    /// Only for a definite spawn Err: no child was established for this pending launch.
    fn cancel_unspawned_command(&mut self, operation: &str) -> Result<(), String> {
        // A failed fsync may follow a published rename; retain the barrier on any error.
        let previously_preserved = self.preserve_marker;
        self.preserve_marker = true;
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        if previously_preserved
            || m["recovery_required"] == true
            || m["command_pending"] != true
            || m["pending_operation"].as_str() != Some(operation)
            || m.get("child_pid").is_some()
            || m.get("child_pgid").is_some()
        {
            return Err("cannot cancel an unbound or mismatched pending launch".into());
        }
        m["command_pending"] = json!(false);
        if let Some(o) = m.as_object_mut() {
            o.remove("pending_operation");
        }
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| format!("cancel unspawned command: {e}"))?;
        self.preserve_marker = false;
        Ok(())
    }

    pub(crate) fn clear_child(&self) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        if let Some(o) = m.as_object_mut() {
            o.remove("child_pid");
            o.remove("child_pgid");
        }
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())
    }

    /// Must precede the first remote write. Failure/drop leaves the barrier intact.
    pub(crate) fn hold_for_recovery(&mut self, context: &Path) -> Result<(), String> {
        self.preserve_marker = true;
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["recovery_required"] = json!(true);
        m["recovery_context"] = json!(context.canonicalize().map_err(|e| e.to_string())?);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())
    }

    /// Internal caller has separately verified the recorded restore/quiescence evidence.
    pub(crate) fn complete_recovery(&mut self) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["recovery_required"] = json!(false);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        self.preserve_marker = false;
        Ok(())
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Exclusive reconciliation ownership; never silently removes an old marker.
    pub(crate) fn acquire_recovery(
        root: &Path,
        legacy_root: &Path,
        serial: &str,
        context: &Path,
        timeout: Duration,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + timeout;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        let bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        let legacy = recovery_dir(legacy_root);
        let prior = marker(&legacy).ok_or("missing/corrupt recovery owner")?;
        let (pending, _) = marker_state(&prior)?;
        let prior_token = prior["token"].as_str().ok_or("missing recovery token")?;
        let pid = prior["pid"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= u32::MAX as u64)
            .ok_or("invalid recovery PID")?;
        if prior["producer"] != "phonefarm"
            || prior["format"] != 1
            || prior["serial"] != serial
            || prior["recovery_required"] != true
            || prior["recovery_context"]
                != json!(context.canonicalize().map_err(|e| e.to_string())?)
            || !owns(&legacy, prior_token)
            || !pid_dead(pid as u32)
        {
            return Err("recovery owner/identity is live, unknown or mismatched".into());
        }
        if pending {
            return Err("unbound pending transport launch; future ADB activity unknown, requires manual reconciliation".into());
        }
        if let Some(child) = prior.get("child_pid") {
            let child = child
                .as_u64()
                .filter(|p| *p > 0 && *p <= u32::MAX as u64)
                .ok_or("invalid recovery child")?;
            if !pid_dead(child as u32) {
                return Err("recovery child is live/unknown".into());
            }
        }
        #[cfg(unix)]
        if let Some(group) = prior.get("child_pgid") {
            let group = group
                .as_u64()
                .filter(|p| *p > 0 && *p <= i32::MAX as u64)
                .ok_or("invalid recovery group")?;
            if unsafe { libc::kill(-(group as i32), 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                return Err("recovery process group is live/unknown".into());
            }
        }
        // Transfer only after all old local ownership checks; persistent uncertainty remains.
        let token = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        let owner = format!("phonefarm:confirmation-reconcile:{serial} token={token}\n");
        let mut next = prior.clone();
        next["pid"] = json!(std::process::id());
        next["token"] = json!(token);
        next["owner"] = json!(owner);
        next["previous_owner"] = prior;
        if let Some(o) = next.as_object_mut() {
            o.remove("child_pid");
            o.remove("child_pgid");
        }
        durable_replace(
            &legacy.join("phonefarm-lease.json"),
            next.to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        durable_replace(&legacy.join("owner"), owner.as_bytes()).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        let hb_path = legacy.clone();
        let hb_token = token.clone();
        let heartbeat = std::thread::spawn(move || {
            while rx.recv_timeout(Duration::from_secs(5)) == Err(mpsc::RecvTimeoutError::Timeout) {
                if !owns(&hb_path, &hb_token)
                    || durable_replace(&hb_path.join("owner"), owner.as_bytes()).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            _device: device,
            _bridge: bridge,
            legacy,
            token,
            stop: Some(tx),
            heartbeat: Some(heartbeat),
            preserve_marker: true,
            serial: serial.into(),
        })
    }

    fn recover_confirmed_idle(
        root: &Path,
        legacy_root: &Path,
        serial: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let _device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        let _bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        let legacy = recovery_dir(legacy_root);
        let m = marker(&legacy)
            .ok_or("no verifiable phonefarm owner; cannot recover manual/unknown lock")?;
        let (pending, recovery) = marker_state(&m)?;
        if pending {
            return Err(
                "unbound pending transport launch cannot be cleared by idle assertion".into(),
            );
        }
        if recovery {
            return Err("confirmation driver barrier requires observed explicit reconcile; operator idle assertion cannot clear it".into());
        }
        let pid = m["pid"]
            .as_u64()
            .filter(|p| *p > 0 && *p <= u32::MAX as u64)
            .ok_or("invalid owner PID")?;
        let token = m["token"].as_str().ok_or("missing owner token")?;
        if m["producer"] != "phonefarm"
            || m["format"] != 1
            || m["serial"] != serial
            || !owns(&legacy, token)
        {
            return Err("owner/serial identity mismatch; cannot recover".into());
        }
        if !pid_dead(pid as u32) {
            return Err("owner alive/unknown; cannot recover".into());
        }
        if let Some(child) = m.get("child_pid") {
            let child = child
                .as_u64()
                .filter(|p| *p > 0 && *p <= u32::MAX as u64)
                .ok_or("invalid child PID")?;
            if !pid_dead(child as u32) {
                return Err("child alive/unknown; cannot recover".into());
            }
        }
        #[cfg(unix)]
        if let Some(group) = m.get("child_pgid") {
            let group = group
                .as_u64()
                .filter(|p| *p > 0 && *p <= i32::MAX as u64)
                .ok_or("invalid recovery group")?;
            if unsafe { libc::kill(-(group as i32), 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                return Err("local child group live/unknown; cannot clear idle assertion".into());
            }
        }
        // Human confirmation is explicit and auditable; it is not a device observation.
        let mut log = File::options()
            .create(true)
            .append(true)
            .open(legacy_root.join("phonefarm-recovery.jsonl"))
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        writeln!(log,"{}",json!({"serial":serial,"previous_owner":m,"acknowledged_by_pid":std::process::id(),
            "device_idle_confirmation":"operator_assertion","device_verified":false,"at":chrono::Utc::now().to_rfc3339()})).map_err(|e|e.to_string())?;
        remove_owned(&legacy, token);
        if legacy.exists() {
            return Err("recovery could not clear verified directory".into());
        }
        Ok(())
    }

    pub fn target(requested: Option<&str>, label: &str) -> Result<Self, String> {
        let serial = crate::device::resolve_serial(requested)
            .ok_or("cannot resolve exactly one device; pass --serial (lease not acquired)")?;
        let root = std::env::var_os("PHONEFARM_LEASE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("phonefarm-device-leases"));
        let legacy = std::env::var_os("DEVLOCK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/private/tmp/claude-501"));
        let ms = std::env::var("PHONEFARM_LEASE_TIMEOUT_MS")
            .ok()
            .map(|v| v.parse::<u64>())
            .transpose()
            .map_err(|_| "bad PHONEFARM_LEASE_TIMEOUT_MS")?
            .unwrap_or(600_000);
        Self::acquire(&root, &legacy, &serial, label, Duration::from_millis(ms))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.heartbeat.take() {
            let _ = h.join();
        }
        let unknown_command = marker(&self.legacy).is_none_or(|m| {
            !marker_state(&m).is_ok_and(|(pending, recovery)| {
                !pending && !recovery && m.get("child_pid").is_none()
            })
        });
        if !self.preserve_marker && !unknown_command {
            remove_owned(&self.legacy, &self.token);
        }
        // File fields close after Drop, releasing both kernel locks on every return/unwind.
    }
}

pub fn run_lease(args: &[String]) -> i32 {
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut recover = false;
    let mut confirm = false;
    let mut serial = None;
    let mut timeout = None;
    let mut it = args[..split].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--serial" => serial = it.next().cloned(),
            "--recover-abandoned" => recover = true,
            "--confirm-device-idle" => confirm = true,
            "--timeout-ms" => match it.next().and_then(|s| s.parse::<u64>().ok()) {
                Some(ms) => timeout = Some(ms),
                None => {
                    eprintln!("--timeout-ms requires a nonnegative integer");
                    return 2;
                }
            },
            _ => {
                eprintln!("unknown lease flag {a}");
                return 2;
            }
        }
    }
    let Some(serial) = serial.filter(|s| !s.trim().is_empty()) else {
        eprintln!("lease requires explicit --serial");
        return 2;
    };
    let root = std::env::var_os("PHONEFARM_LEASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("phonefarm-device-leases"));
    let legacy = std::env::var_os("DEVLOCK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/private/tmp/claude-501"));
    if recover {
        if !confirm || split != args.len() {
            eprintln!("recovery requires --confirm-device-idle after independently verifying remote workloads stopped and device restored; no command allowed");
            return 2;
        }
        return match Lease::recover_confirmed_idle(
            &root,
            &legacy,
            &serial,
            Duration::from_millis(timeout.unwrap_or(600_000)),
        ) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                2
            }
        };
    }
    if confirm {
        eprintln!("--confirm-device-idle is only for --recover-abandoned");
        return 2;
    }
    let Some(command) = args.get(split + 1) else {
        eprintln!("phonefarm lease --serial S [--timeout-ms N] -- COMMAND ...");
        return 2;
    };
    let mut _lease = match Lease::acquire(
        &root,
        &legacy,
        &serial,
        "lease",
        Duration::from_millis(timeout.unwrap_or(600_000)),
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if let Err(e) = _lease.record_command_pending("lease-wrapper") {
        eprintln!("lease pending marker: {e}");
        return 2;
    }
    let mut cmd = std::process::Command::new(command);
    cmd.args(&args[split + 2..]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Rust reported no child was established; retire only our exact pending launch.
            // If the durable cancellation fails, Drop preserves the uncertainty barrier.
            if let Err(cleanup) = _lease.cancel_unspawned_command("lease-wrapper") {
                eprintln!("lease spawn cancellation: {cleanup}");
            }
            eprintln!("lease command: {e}");
            return 2;
        }
    };
    if let Err(e) = _lease.record_child(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        eprintln!("{e}");
        return 2;
    }
    match child.wait() {
        Ok(status) => {
            if let Err(e) = _lease.check() {
                eprintln!("{e}");
                return 2;
            }
            #[cfg(unix)]
            if (unsafe { libc::kill(-(child.id() as i32), 0) }) >= 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                eprintln!("lease child exited but process group remains live/unknown");
                return 2;
            }
            if let Err(e) = _lease.clear_child() {
                eprintln!("lease completion: {e}");
                return 2;
            }
            status.code().unwrap_or(2)
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("lease child: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "phonefarm-spawn-cancel-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn lease(&self) -> Lease {
            Lease::acquire(
                &self.0.join("leases"),
                &self.0.join("legacy"),
                "fake-spawn-cancel",
                "test",
                Duration::from_millis(100),
            )
            .unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // This test owns the create-only directory; no global or device state is used.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unspawned_pending_cancellation_releases_but_unresolved_drop_preserves() {
        let scratch = Scratch::new();
        let mut lease = scratch.lease();
        lease.record_command_pending("lease-wrapper").unwrap();
        lease.cancel_unspawned_command("lease-wrapper").unwrap();
        assert_eq!(marker(&lease.legacy).unwrap()["command_pending"], false);
        drop(lease);
        assert!(!scratch.0.join("legacy/devlock.d").exists());
        let unresolved = scratch.lease();
        unresolved.record_command_pending("lease-wrapper").unwrap();
        drop(unresolved);
        assert!(scratch.0.join("legacy/devlock.d").exists());
    }

    #[test]
    fn cancellation_rejects_mismatched_pending_child_or_owner_without_mutating() {
        for (key, value) in [
            ("pending_operation", json!("another-operation")),
            ("command_pending", json!(false)),
            ("child_pid", json!(123)),
            ("child_pid", Value::Null),
            ("child_pgid", json!(123)),
            ("recovery_required", json!(true)),
            ("owner", json!("replacement-owner")),
        ] {
            let scratch = Scratch::new();
            let mut lease = scratch.lease();
            lease.record_command_pending("lease-wrapper").unwrap();
            let mut m = marker(&lease.legacy).unwrap();
            m[key] = value;
            durable_replace(
                &lease.legacy.join("phonefarm-lease.json"),
                m.to_string().as_bytes(),
            )
            .unwrap();
            let before = fs::read(lease.legacy.join("phonefarm-lease.json")).unwrap();
            assert!(
                lease.cancel_unspawned_command("lease-wrapper").is_err(),
                "{key}"
            );
            assert_eq!(
                fs::read(lease.legacy.join("phonefarm-lease.json")).unwrap(),
                before
            );
            let legacy = lease.legacy.clone();
            drop(lease);
            assert_eq!(
                fs::read(legacy.join("phonefarm-lease.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn corrupt_marker_extensions_block_every_release_and_transfer() {
        let invalid = [
            ("future_recovery_gate", json!(false)),
            ("format", json!(2)),
            ("command_pending", json!("true")),
            ("command_pending", Value::Null),
            ("recovery_required", json!("true")),
            ("recovery_required", json!(0)),
            ("child_pid", json!(123)),
            ("child_pgid", json!(123)),
            ("child_pid", Value::Null),
        ];
        for (key, value) in invalid {
            let scratch = Scratch::new();
            let lease = scratch.lease();
            let serial = lease.serial.clone();
            let legacy = lease.legacy.clone();
            let mut m = marker(&legacy).unwrap();
            m[key] = value;
            let state_error = marker_state(&m).unwrap_err();
            durable_replace(
                &legacy.join("phonefarm-lease.json"),
                m.to_string().as_bytes(),
            )
            .unwrap();
            let before = fs::read(legacy.join("phonefarm-lease.json")).unwrap();
            assert_eq!(lease.check().unwrap_err(), state_error, "{key}");
            drop(lease);
            assert_eq!(
                fs::read(legacy.join("phonefarm-lease.json")).unwrap(),
                before
            );
            let root = scratch.0.join("leases");
            let bridge = scratch.0.join("legacy");
            assert_eq!(
                Lease::acquire(&root, &bridge, &serial, "test", Duration::from_millis(25))
                    .err()
                    .unwrap(),
                state_error
            );
            assert_eq!(
                Lease::recover_confirmed_idle(&root, &bridge, &serial, Duration::from_millis(25))
                    .unwrap_err(),
                state_error
            );
            assert_eq!(
                Lease::acquire_recovery(
                    &root,
                    &bridge,
                    &serial,
                    &scratch.0,
                    Duration::from_millis(25)
                )
                .err()
                .unwrap(),
                state_error
            );
            assert_eq!(
                fs::read(legacy.join("phonefarm-lease.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn marker_child_identity_requires_complete_equal_positive_signed_pair() {
        let scratch = Scratch::new();
        let lease = scratch.lease();
        let mut m = marker(&lease.legacy).unwrap();
        // Older format-1 markers without extension fields remain accepted.
        assert_eq!(marker_state(&m).unwrap(), (false, false));
        for (pid, group) in [
            (0, 0),
            (123, 124),
            (i32::MAX as u64 + 1, i32::MAX as u64 + 1),
        ] {
            m["child_pid"] = json!(pid);
            m["child_pgid"] = json!(group);
            assert!(marker_state(&m).is_err());
        }
        m["child_pid"] = json!(123);
        m["child_pgid"] = json!(123);
        assert!(marker_state(&m).is_ok());
        // Only the in-memory test value changed; the owned idle lease can release normally.
        let legacy = lease.legacy.clone();
        drop(lease);
        assert!(!legacy.exists());
    }
}
