//! Host leases cover a whole experiment, including restoration. File locks are never unlinked.
use crate::host_process::{self, Identity, InstanceState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Lease {
    _device: File,
    _bridge: File,
    legacy: PathBuf,
    token: String,
    outer_instance: Option<String>,
    process_identity: Identity,
    stop: Option<mpsc::Sender<()>>,
    heartbeat: Option<JoinHandle<()>>,
    preserve_marker: bool,
    receipt: Option<crate::lease_receipt::JobReceipt>,
    receipt_failed: AtomicBool,
    pub serial: String,
}

fn lock_file(path: &Path, deadline: Instant) -> Result<File, String> {
    let f = crate::shared_access::lock_file(path, 0o666)
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
pub(crate) fn durable_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("marker parent missing")?;
    crate::shared_access::check_existing(path, false).map_err(|e| e.to_string())?;
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
    crate::shared_access::created_file(&tmp, &f, false).map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    File::open(parent)
        .and_then(|d| d.sync_all())
        .map_err(|e| e.to_string())
}
/// The typed outer queue owner may be borrowed only by its current instance's
/// process tree. A label is never an ownership credential.
const NESTED_DIR: &str = "phonefarm-lease.d";
fn recovery_dir(legacy_root: &Path) -> Result<PathBuf, String> {
    let queue = crate::lease_domain::inspect(legacy_root)?.queue_dir;
    let main = queue.join("devlock.d");
    let nested = queue.join(NESTED_DIR);
    let main_marker = marker(&main);
    // Queue identity is not a strong recovery context. Malformed/foreign outer
    // records remain barriers; do not silently route around them.
    let outer_queue = crate::queue_owner::read(legacy_root).ok().flatten().is_some();
    if (main_marker.is_none() || outer_queue) && marker(&nested).is_some() {
        Ok(nested)
    } else {
        Ok(main)
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
        "job_id",
        "lease_id",
        "outer_instance_id",
        "host_boot_id",
        "pid_start",
        "child_start",
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
    if m.get("outer_instance_id").is_some_and(|id| !id.as_str().is_some_and(crate::queue_owner::valid_instance)) {
        return Err("invalid outer queue instance reference".into());
    }
    match (m.get("job_id"), m.get("lease_id")) {
        (None, None) => {}
        (Some(job), Some(lease))
            if job.as_str().is_some_and(crate::lease_receipt::valid_job_id)
                && lease
                    .as_str()
                    .is_some_and(crate::lease_receipt::valid_lease_id) => {}
        _ => return Err("invalid or incomplete lease job association".into()),
    }
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
    let identity = marker_identity(m)?;
    match (identity, m.get("child_pid"), m.get("child_start")) {
        (Some(_), Some(_), Some(start)) if start.as_str().is_some_and(host_process::clean_identity) => {}
        (Some(_), None, None) | (None, _, None) => {}
        _ => return Err("invalid or incomplete lease child process identity".into()),
    }
    Ok((pending, recovery))
}

fn marker_identity(m: &Value) -> Result<Option<Identity>, String> {
    match (m.get("host_boot_id"), m.get("pid_start")) {
        (None, None) => Ok(None),
        (Some(boot), Some(start)) => {
            let host_boot_id = boot
                .as_str()
                .filter(|s| host_process::clean_identity(s))
                .ok_or("invalid lease host boot identity")?;
            let start = start
                .as_str()
                .filter(|s| host_process::clean_identity(s))
                .ok_or("invalid lease owner start identity")?;
            let pid = m["pid"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= i32::MAX as u64)
                .ok_or("invalid lease owner PID")? as u32;
            Ok(Some(Identity {
                pid,
                start: start.into(),
                host_boot_id: host_boot_id.into(),
            }))
        }
        _ => Err("incomplete lease host process identity".into()),
    }
}

fn bind_identity(m: &mut Value, identity: &Identity) {
    m["pid"] = json!(identity.pid);
    m["pid_start"] = json!(identity.start);
    m["host_boot_id"] = json!(identity.host_boot_id);
}

fn inspect_owner(m: &Value) -> Result<Option<InstanceState>, String> {
    marker_identity(m)?
        .as_ref()
        .map(host_process::inspect)
        .transpose()
}

fn owner_dead(m: &Value) -> Result<bool, String> {
    match inspect_owner(m)? {
        Some(InstanceState::Alive) => Ok(false),
        Some(_) => Ok(true),
        None => Ok(pid_dead(
            m["pid"]
                .as_u64()
                .filter(|p| *p > 0 && *p <= i32::MAX as u64)
                .ok_or("invalid lease owner PID")? as u32,
        )),
    }
}

/// Called only after validating marker shape and explicit recovery conditions.
/// A same-boot leader change never proves the old group has disappeared.
fn prior_processes_gone(m: &Value) -> Result<(), String> {
    let identity = marker_identity(m)?;
    match identity.as_ref().map(host_process::inspect).transpose()? {
        Some(InstanceState::PreviousBoot) => return Ok(()),
        Some(InstanceState::Alive) => return Err("lease owner process instance is alive".into()),
        Some(_) => {}
        None if !owner_dead(m)? => return Err("legacy owner is alive/unknown".into()),
        None => {}
    }
    if let Some(child) = m.get("child_pid") {
        let pid = child
            .as_u64()
            .filter(|p| *p > 0 && *p <= i32::MAX as u64)
            .ok_or("invalid recovery child PID")? as u32;
        let alive = if let Some(owner) = identity {
            let child = Identity {
                pid,
                start: m["child_start"]
                    .as_str()
                    .ok_or("missing child start")?
                    .into(),
                host_boot_id: owner.host_boot_id,
            };
            host_process::inspect(&child)? == InstanceState::Alive
        } else {
            !pid_dead(pid)
        };
        if alive {
            return Err("recovery child is alive/unknown".into());
        }
    }
    #[cfg(unix)]
    if let Some(group) = m.get("child_pgid") {
        let group = group
            .as_u64()
            .filter(|p| *p > 0 && *p <= i32::MAX as u64)
            .ok_or("invalid recovery group")? as i32;
        if unsafe { libc::kill(-group, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        {
            return Err("recovery process group is live/unknown".into());
        }
    }
    Ok(())
}

fn remote_recovery_evidence(
    prior: &Value, serial: &str, context: &Path, note: &str,
    participants_stopped: bool, evidence: &Path,
) -> Result<(), String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let context = context.canonicalize().map_err(|e| e.to_string())?;
    if note.trim().is_empty() || note.len() > 4096 || evidence.parent() != Some(context.as_path())
        || !evidence.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.starts_with("recovery-") && s.ends_with(".json"))
        || evidence.symlink_metadata().is_ok()
    {
        return Err("remote recovery requires a note and a new evidence file in the exact session context".into());
    }
    let file = File::options().read(true).custom_flags(libc::O_NOFOLLOW)
        .open(context.join("session.json")).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file()
        || file.metadata().map_err(|e| e.to_string())?.len() > 65536 {
        return Err("remote recovery state is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 65536 { return Err("remote recovery state exceeds its limit".into()); }
    let state: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let session_id = state["session_id"].as_str().filter(|s| !s.is_empty() && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
        .ok_or("invalid remote session identity")?;
    if state["schema"] != "phonefarm/remote-control-state@1" || state["serial"] != serial
        || state["context"] != json!(context) || state["token"].as_str().is_none_or(str::is_empty) {
        return Err("remote recovery schema/serial/context mismatch".into());
    }
    let mut ancestor = prior;
    let mut bound = false;
    for _ in 0..32 {
        if ancestor["token"] == state["token"] && ancestor["serial"] == serial
            && ancestor["recovery_context"] == json!(context)
            && ancestor["pid"] == state["owner"]["pid"]
            && ancestor["pid_start"] == state["owner"]["start"]
            && ancestor["host_boot_id"] == state["owner"]["host_boot_id"] {
            bound = true; break;
        }
        let Some(previous) = ancestor.get("previous_owner") else { break; };
        ancestor = previous;
    }
    if !bound { return Err("remote session does not bind the retained lease ancestry".into()); }
    let pending = prior["command_pending"] == true;
    if pending && (!participants_stopped || prior.get("child_pid").is_some()
        || prior.get("child_pgid").is_some() || prior.get("child_start").is_some()) {
        return Err("unbound remote launch requires --participants-stopped after independently verifying every possible launcher/transport stopped; recorded groups cannot be overridden".into());
    }
    let proof = json!({"schema":"phonefarm/remote-control-recovery@1","session_id":session_id,
        "serial":serial,"context":context,"scope":"remote_control_commands_settled","origin":"operator_assertion",
        "device_verified":false,"game_state_restored":false,"app_state_rollback":false,
        "participants_stopped":participants_stopped,"unbound_pending_retired":pending,"note":note,
        "previous_owner":prior,"pending_request":state["pending_request"],"at":chrono::Utc::now().to_rfc3339()});
    // Preserve the original marker and assertion before transferring or clearing
    // anything. A failed publication leaves the original barrier untouched.
    durable_replace(evidence, &serde_json::to_vec(&proof).map_err(|e| e.to_string())?)
}

/// Verify both actual process instances before the released worker touches a device.
pub(crate) fn verify_worker_processes(m: &Value, parent: u32, child: u32) -> Result<(), String> {
    marker_state(m)?;
    let owner =
        marker_identity(m)?.ok_or("worker requires a host process identity; upgrade its owner")?;
    if owner.pid != parent || host_process::inspect(&owner)? != InstanceState::Alive {
        return Err("worker owner process instance changed or exited".into());
    }
    let actual = host_process::capture(child)?;
    if m["child_pid"] != child
        || m["child_pgid"] != child
        || m["child_start"] != actual.start
        || owner.host_boot_id != actual.host_boot_id
    {
        return Err("worker child process instance mismatch".into());
    }
    Ok(())
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

fn observation_queue_empty(root: &Path) -> Result<(), String> {
    if root.join("devlock.qmutex").symlink_metadata().is_ok() {
        return Err("writer queue is being updated".into());
    }
    match fs::read_to_string(root.join("devlock.queue")) {
        Ok(q) if !q.trim().is_empty() => Err("queued device work takes priority over observation".into()),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot read writer queue: {e}")),
    }
}

fn observation_available(root: &Path) -> Result<(), String> {
    for name in ["devlock.d", NESTED_DIR] {
        match fs::symlink_metadata(root.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            _ => return Err(format!("device domain occupied or unknown: {name}")),
        }
    }
    observation_queue_empty(root)
}

impl Lease {
    pub fn acquire(
        root: &Path,
        legacy_root: &Path,
        serial: &str,
        label: &str,
        timeout: Duration,
    ) -> Result<Self, String> {
        Self::acquire_inner(root, legacy_root, serial, label, timeout, false)
    }

    /// Observation is domain-wide, never borrows a label, and yields to waiting writers.
    pub(crate) fn observe(storage: &crate::lease_storage::Storage, label: &str) -> Result<Self, String> {
        let (root, legacy) = storage.require_ready()?;
        Self::acquire_inner(&root, &legacy, "@observation", label, Duration::ZERO, true)
    }

    fn acquire_inner(
        root: &Path, legacy_root: &Path, serial: &str, label: &str,
        timeout: Duration, observation: bool,
    ) -> Result<Self, String> {
        if serial.trim().is_empty() {
            return Err("lease requires a resolved device serial".into());
        }
        let receipt = crate::lease_receipt::JobReceipt::from_env(serial, label, Value::Null)?;
        let process_identity = host_process::capture(std::process::id())?;
        crate::shared_access::create_dir_all(root).map_err(|e| format!("lease directory: {e}"))?;
        crate::shared_access::create_dir_all(legacy_root).map_err(|e| format!("devlock directory: {e}"))?;
        let deadline = Instant::now() + timeout;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        // Legacy devlock is global. Same order for every participant, no ABBA deadlock.
        let bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        // Resolve after acquiring the stable bridge: a waiting participant may
        // have observed the previous layout before an explicit domain upgrade.
        let paths = crate::lease_domain::inspect(legacy_root)?;
        paths.require_device_root(root)?;
        let queue_root = &paths.queue_dir;
        // Serialize the outer identity check and nested publication with outer
        // release/replacement. This guard is never held during device work.
        let mut queue_guard = if observation { None } else { Some(crate::queue_owner::lock(legacy_root)?) };
        let outer = if observation { None } else { crate::queue_owner::inherited(legacy_root)? };
        if outer.is_none() { drop(queue_guard.take()); }
        let legacy = if observation {
            observation_available(queue_root)?;
            queue_root.join("devlock.d")
        } else if outer.is_some() { queue_root.join(NESTED_DIR) }
        else { queue_root.join("devlock.d") };
        // 另一个目录里残留的租约 (崩溃/恢复屏障) 同样阻断, 不因换了目录而被忽略
        for other in [queue_root.join("devlock.d"), queue_root.join(NESTED_DIR)] {
            if other == legacy {
                continue;
            }
            if let Some(m) = marker(&other) {
                if other == queue_root.join("devlock.d") && outer.as_ref().is_some_and(|owner| {
                    serde_json::to_value(owner).ok().as_ref() == Some(&m)
                }) { continue; }
                if !marker_state(&m).is_ok_and(|(pending, recovery)| !pending && !recovery) {
                    return Err(format!(
                        "lease barrier present in {}; reconcile/recover it first",
                        other.display()
                    ));
                }
            }
        }
        loop {
            match crate::shared_access::create_dir(&legacy) {
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
                                    && owner_dead(&m)?
                                    && owns(&legacy, token)
                                {
                                    return Err(format!("ABANDONED lease {}: parent exited; child/remote workload and device restoration are unverified. After verifying device idle/restored, use lease --recover-abandoned --confirm-device-idle --serial {serial}", legacy.display()));
                                }
                            }
                        }
                    }
                    if outer.is_some() || Instant::now() >= deadline {
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
        let mut info = json!({"format":1,"producer":"phonefarm","pid":std::process::id(),
                          "token":token,"serial":serial,"label":label,"owner":owner});
        bind_identity(&mut info, &process_identity);
        let outer_instance = outer.map(|owner| owner.instance_id);
        if let Some(instance) = &outer_instance { info["outer_instance_id"] = json!(instance); }
        if let Some(r) = &receipt {
            r.bind_marker(&mut info)?;
        }
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
        File::open(queue_root)
            .and_then(|d| d.sync_all())
            .map_err(|e| e.to_string())?;
        drop(queue_guard);
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
        let lease = Self {
            _device: device,
            _bridge: bridge,
            legacy,
            token,
            outer_instance,
            process_identity,
            stop: Some(tx),
            heartbeat: Some(heartbeat),
            preserve_marker: false,
            receipt,
            receipt_failed: AtomicBool::new(false),
            serial: serial.into(),
        };
        lease.publish_receipt(&info, "active", None)?;
        // A writer may have joined between our first queue check and atomic mkdir.
        // Yield before any transport is launched, without leaving a queue entry.
        if observation { observation_queue_empty(queue_root)?; }
        Ok(lease)
    }

    fn publish_receipt(
        &self,
        marker: &Value,
        state: &str,
        uncertainty: Option<&str>,
    ) -> Result<(), String> {
        if let Some(r) = &self.receipt {
            if let Err(e) = r.observe(marker, state, uncertainty) {
                self.receipt_failed.store(true, Ordering::Relaxed);
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn check(&self) -> Result<(), String> {
        if self.receipt_failed.load(Ordering::Relaxed) {
            return Err("job lease receipt publication failed; recovery remains unverified".into());
        }
        let current = marker(&self.legacy).ok_or("lease marker unavailable")?;
        marker_state(&current)?;
        if current["outer_instance_id"].as_str() != self.outer_instance.as_deref() {
            return Err("lease outer instance reference changed".into());
        }
        if marker_identity(&current)?.as_ref() != Some(&self.process_identity)
            || self.process_identity.pid != std::process::id()
            || host_process::inspect(&self.process_identity)? != InstanceState::Alive
        { return Err("lease host process instance changed or is unverified".into()); }
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
        if m["command_pending"] == true || m.get("child_pid").is_some() || m.get("child_pgid").is_some() {
            return Err("previous transport launch/child remains unresolved; refusing to replace its identity".into());
        }
        m["command_pending"] = json!(true);
        m["pending_operation"] = json!(operation);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )?;
        self.publish_receipt(&m, "active", None)
    }
    pub(crate) fn record_child(&self, pid: u32) -> Result<(), String> {
        self.check()?;
        let child = host_process::capture(pid)?;
        if child.host_boot_id != self.process_identity.host_boot_id {
            return Err("child host boot differs from lease owner".into());
        }
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["command_pending"] = json!(false);
        m["child_pid"] = json!(pid);
        m["child_pgid"] = json!(pid);
        m["child_start"] = json!(child.start);
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| format!("record child: {e}"))?;
        self.publish_receipt(&m, "active", None)
    }

    /// Only for a definite spawn Err: no child was established for this pending launch.
    pub(crate) fn cancel_unspawned_command(&mut self, operation: &str) -> Result<(), String> {
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
        self.publish_receipt(&m, "active", None)?;
        self.preserve_marker = false;
        Ok(())
    }

    pub(crate) fn authorize_observation_worker(&self, cmd: &mut std::process::Command) {
        cmd.env("PHONEFARM_OBSERVATION_MARKER", self.legacy.join("phonefarm-lease.json"))
            .env("PHONEFARM_OBSERVATION_TOKEN", &self.token);
    }

    /// A worker may start only after record_child publishes its exact identity.
    /// The deadline includes time spent waiting at the parent's startup gate.
    pub(crate) fn authorize_worker(
        &self,
        cmd: &mut std::process::Command,
        timeout: Duration,
    ) -> Result<(), String> {
        self.check()?;
        let deadline = std::time::SystemTime::now()
            .checked_add(timeout)
            .ok_or("worker deadline is too large")?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let deadline = u64::try_from(deadline).map_err(|_| "worker deadline is too large")?;
        let path = self.legacy.join("phonefarm-lease.json").canonicalize()
            .map_err(|e| format!("worker lease marker: {e}"))?;
        cmd.env("PHONEFARM_WORKER_MARKER", path)
            .env("PHONEFARM_WORKER_TOKEN", &self.token)
            .env("PHONEFARM_WORKER_SERIAL", &self.serial)
            .env("PHONEFARM_WORKER_PARENT_PID", std::process::id().to_string())
            .env("PHONEFARM_WORKER_DEADLINE_MS", deadline.to_string());
        Ok(())
    }

    pub(crate) fn verify_worker_authority() -> Result<Duration, String> {
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let env = |key| std::env::var(key).map_err(|_| format!("missing worker authority: {key}"));
        let path = PathBuf::from(env("PHONEFARM_WORKER_MARKER")?);
        let token = env("PHONEFARM_WORKER_TOKEN")?;
        let serial = env("PHONEFARM_WORKER_SERIAL")?;
        let parent: i32 = env("PHONEFARM_WORKER_PARENT_PID")?.parse()
            .map_err(|_| "invalid worker parent PID")?;
        let deadline_ms: u64 = env("PHONEFARM_WORKER_DEADLINE_MS")?.parse()
            .map_err(|_| "invalid worker deadline")?;
        if !path.is_absolute() || token.is_empty() || serial.is_empty() || parent <= 1 {
            return Err("invalid worker lease authority".into());
        }
        let file = File::options().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
            .map_err(|e| format!("worker lease marker: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > 65536 {
            return Err("worker lease marker is not a bounded regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > 65536 { return Err("worker lease marker is too large".into()); }
        let marker: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid worker lease marker")?;
        marker_state(&marker)?;
        let pid = std::process::id();
        if marker["token"] != token || marker["serial"] != serial
            || marker["pid"] != parent || unsafe { libc::getppid() } != parent
            || marker["child_pid"] != pid || marker["child_pgid"] != pid
            || unsafe { libc::getpgrp() } != pid as i32 || marker["command_pending"] != false
        {
            return Err("worker lease identity mismatch".into());
        }
        verify_worker_processes(&marker, parent as u32, pid)?;
        std::time::UNIX_EPOCH.checked_add(Duration::from_millis(deadline_ms))
            .ok_or("worker deadline is too large")?
            .duration_since(std::time::SystemTime::now())
            .map_err(|_| "worker deadline already elapsed".into())
    }

    pub(crate) fn clear_child(&self) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        if let Some(o) = m.as_object_mut() {
            o.remove("child_pid");
            o.remove("child_pgid");
            o.remove("child_start");
        }
        durable_replace(
            &self.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        self.publish_receipt(&m, "active", None)
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
        .map_err(|e| e.to_string())?;
        self.publish_receipt(&m, "active", None)
    }

    /// Internal caller has separately verified the recorded restore/quiescence evidence.
    pub(crate) fn complete_recovery(
        &mut self,
        scope: &str,
        origin: &str,
        evidence: &[&Path],
    ) -> Result<(), String> {
        self.check()?;
        if scope.trim().is_empty()
            || origin.trim().is_empty()
            || evidence.is_empty()
            || evidence.iter().any(|p| !p.is_absolute() || !p.is_file())
        {
            return Err(
                "recovery completion requires scope, origin, and existing absolute evidence files"
                    .into(),
            );
        }
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        if m["command_pending"] == true || m.get("child_pid").is_some() {
            return Err("recovery completion requires no pending/local child command".into());
        }
        if let Some(r) = &self.receipt {
            if let Err(e) = r.complete(scope, origin, evidence) {
                self.receipt_failed.store(true, Ordering::Relaxed);
                return Err(e);
            }
        }
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
        Self::acquire_recovery_inner(root, legacy_root, serial, context, timeout, None)
    }

    /// Remote control owns no installation rollback. Its local administrator may
    /// attest that the original finite transports stopped. Only an unbound
    /// pending launch additionally accepts an explicit participants-stopped
    /// assertion; a recorded child/group must still be independently absent.
    pub(crate) fn acquire_remote_recovery(
        root: &Path, legacy_root: &Path, serial: &str, context: &Path,
        note: &str, participants_stopped: bool, evidence: &Path,
    ) -> Result<Self, String> {
        Self::acquire_recovery_inner(root, legacy_root, serial, context, Duration::ZERO,
            Some((note, participants_stopped, evidence)))
    }

    fn acquire_recovery_inner(
        root: &Path, legacy_root: &Path, serial: &str, context: &Path,
        timeout: Duration, remote: Option<(&str, bool, &Path)>,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + timeout;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        let bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        crate::lease_domain::inspect(legacy_root)?.require_device_root(root)?;
        let legacy = recovery_dir(legacy_root)?;
        let prior = marker(&legacy).ok_or("missing/corrupt recovery owner")?;
        let (pending, _) = marker_state(&prior)?;
        let recovery_of = if prior.get("job_id").is_some() {
            json!({"job_id":prior["job_id"], "lease_id":prior["lease_id"]})
        } else {
            Value::Null
        };
        let receipt = crate::lease_receipt::JobReceipt::from_env(
            serial,
            "confirmation-reconcile",
            recovery_of,
        )?;
        let prior_token = prior["token"].as_str().ok_or("missing recovery token")?;

        if prior["producer"] != "phonefarm"
            || prior["format"] != 1
            || prior["serial"] != serial
            || (prior["recovery_required"] != true && remote.is_none())
            || prior["recovery_context"]
                != json!(context.canonicalize().map_err(|e| e.to_string())?)
            || !owns(&legacy, prior_token)
        {
            return Err("recovery owner/identity is live, unknown or mismatched".into());
        }
        if pending && remote.is_none() {
            return Err("unbound pending transport launch; future ADB activity unknown, requires manual reconciliation".into());
        }
        prior_processes_gone(&prior)?;
        if let Some((note, participants_stopped, evidence)) = remote {
            // This check deliberately follows prior_processes_gone: no flag can
            // override an unknown/live recorded child or process group.
            remote_recovery_evidence(&prior, serial, context, note, participants_stopped, evidence)?;
        }
        let process_identity = host_process::capture(std::process::id())?;
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
        bind_identity(&mut next, &process_identity);
        next["token"] = json!(token);
        next["owner"] = json!(owner);
        next["label"] = json!("confirmation-reconcile");
        next["previous_owner"] = prior;
        if pending {
            next["command_pending"] = json!(false);
            next.as_object_mut().unwrap().remove("pending_operation");
        }
        if let Some(o) = next.as_object_mut() {
            o.remove("child_pid");
            o.remove("child_pgid");
            o.remove("child_start");
            o.remove("job_id");
            o.remove("lease_id");
        }
        if let Some(r) = &receipt {
            r.bind_marker(&mut next)?;
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
        let lease = Self {
            _device: device,
            _bridge: bridge,
            legacy,
            token,
            outer_instance: next["outer_instance_id"].as_str().map(str::to_owned),
            process_identity,
            stop: Some(tx),
            heartbeat: Some(heartbeat),
            preserve_marker: true,
            receipt,
            receipt_failed: AtomicBool::new(false),
            serial: serial.into(),
        };
        lease.publish_receipt(&next, "active", None)?;
        Ok(lease)
    }

    fn recover_confirmed_idle(
        root: &Path,
        legacy_root: &Path,
        serial: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        crate::shared_access::create_dir_all(root).map_err(|e| e.to_string())?;
        let key = format!("{:x}", Sha256::digest(serial.as_bytes()));
        let _device = lock_file(&root.join(format!("{key}.lock")), deadline)?;
        let _bridge = lock_file(&legacy_root.join("phonefarm-bridge.lock"), deadline)?;
        crate::lease_domain::inspect(legacy_root)?.require_device_root(root)?;
        let legacy = recovery_dir(legacy_root)?;
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

        let token = m["token"].as_str().ok_or("missing owner token")?;
        if m["producer"] != "phonefarm"
            || m["format"] != 1
            || m["serial"] != serial
            || !owns(&legacy, token)
        {
            return Err("owner/serial identity mismatch; cannot recover".into());
        }
        prior_processes_gone(&m)?;
        // Human confirmation is explicit and auditable; it is not a device observation.
        let mut log = crate::shared_access::append_file(&legacy_root.join("phonefarm-recovery.jsonl"))
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
        let (root, legacy) = crate::lease_storage::resolve().require_ready()?;
        let serial = crate::device::resolve_serial(requested)
            .ok_or("cannot resolve exactly one device; pass --serial (lease not acquired)")?;
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
        let m = marker(&self.legacy).unwrap_or(Value::Null);
        let uncertainty = if self.receipt_failed.load(Ordering::Relaxed) {
            Some("receipt_publication_failed")
        } else if marker_state(&m).is_err() {
            Some("marker_unknown")
        } else if m["outer_instance_id"].as_str() != self.outer_instance.as_deref() {
            Some("outer_reference_changed")
        } else if marker_identity(&m).ok().flatten().as_ref() != Some(&self.process_identity)
            || self.process_identity.pid != std::process::id() {
            Some("host_process_identity_changed")
        } else if !owns(&self.legacy, &self.token) {
            Some("ownership_changed")
        } else if m["command_pending"] == true {
            Some("command_pending")
        } else if m.get("child_pid").is_some() {
            Some("child_unverified")
        } else if self.preserve_marker || m["recovery_required"] == true {
            Some("explicit_recovery_barrier")
        } else {
            None
        };
        let release = uncertainty.is_none();
        if let Err(e) = self.publish_receipt(
            &m,
            if release { "released" } else { "retained" },
            uncertainty,
        ) {
            eprintln!("{e}; retaining lease marker");
        }
        if release && !self.receipt_failed.load(Ordering::Relaxed) {
            remove_owned(&self.legacy, &self.token);
            if self.legacy.exists() {
                if let Err(e) = self.publish_receipt(&m, "retained", Some("marker_release_failed"))
                {
                    eprintln!("{e}; lease marker removal incomplete");
                }
            }
        }
        // File fields close after Drop, releasing both kernel locks on every return/unwind.
    }
}

fn host_process_fact(m: &Value, observed: &Result<Option<InstanceState>, String>) -> Value {
    let (status, reason) = match observed {
        Ok(Some(InstanceState::Alive)) => ("alive", "recorded host owner process instance is alive"),
        Ok(Some(InstanceState::PreviousBoot)) => ("previous_boot", "host boot changed; old local processes are gone, device recovery remains unverified"),
        Ok(Some(InstanceState::Exited)) => ("exited", "recorded owner exited; child group and device state are separate obligations"),
        Ok(Some(InstanceState::Zombie)) => ("zombie", "recorded owner exited but is not reaped; child group and device state are separate obligations"),
        Ok(Some(InstanceState::PidReused)) => ("pid_reused", "PID belongs to a different process instance; original group and device state remain unverified"),
        Ok(None) => ("legacy", "old marker has no recorded host process instance; conservative legacy checks apply"),
        Err(error) => ("unknown", error.as_str()),
    };
    json!({"status":status,"pid":m["pid"].as_u64(),"start":m["pid_start"].as_str(),
           "host_boot_id":m["host_boot_id"].as_str(),"reason":reason})
}

/// Observe existing host state only. This is neither a recovery receipt nor write authorization.
fn lease_status(legacy_root: &Path) -> Value {
    let paths = match crate::lease_domain::inspect(legacy_root) {
        Ok(paths) => paths,
        Err(error) => return json!({"schema_version":1,"status":"unknown","bridge_held":null,
            "entries":[],"error":error}),
    };
    let probe_bridge = || -> Result<bool, String> {
        crate::shared_access::check_directory(legacy_root).map_err(|e| e.to_string())?;
        let path = legacy_root.join("phonefarm-bridge.lock");
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("bridge metadata: {e}")),
            Ok(m) if !m.is_file() => return Err("bridge is not a regular file".into()),
            Ok(_) => {}
        }
        crate::shared_access::check_existing(&path, true).map_err(|e| e.to_string())?;
        let file = File::open(path).map_err(|e| format!("bridge read: {e}"))?;
        match file.try_lock() {
            Ok(()) => Ok(false), // Closing this descriptor releases only our brief probe lock.
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(e) => Err(format!("bridge probe: {e}")),
        }
    };
    let bridge = probe_bridge();
    let bridge_held = bridge.as_ref().ok().copied();
    let mut errors = bridge.err().into_iter().collect::<Vec<_>>();
    let mut entries = Vec::new();
    let queue_owner = crate::queue_owner::read(legacy_root);
    for directory in ["devlock.d", NESTED_DIR] {
        let path = paths.queue_dir.join(directory);
        let mut entry = json!({"directory":directory,"serial":null,"label":null,"job_id":null,"lease_id":null,
            "role":null,"instance_id":null,"outer_instance_id":null,"holder_verified":null,
            "state":"unknown","reason":null,"recovery_context":null});
        if directory == "devlock.d" {
            if let Ok(Some(owner)) = &queue_owner {
                entry["role"] = json!("outer_queue");
                entry["instance_id"] = json!(owner.instance_id);
                entry["label"] = json!(owner.label);
                entry["holder_pid"] = json!(owner.holder_pid);
                match crate::queue_owner::check_live(owner) {
                    Ok(()) => {
                        entry["holder_verified"] = json!(true);
                        entry["state"] = json!("active");
                        entry["reason"] = json!("outer queue holder process instance is verified; not a device recovery receipt");
                    }
                    Err(error) => {
                        entry["holder_verified"] = json!(false);
                        entry["reason"] = json!(error);
                        errors.push(format!("{directory}: {}", entry["reason"].as_str().unwrap()));
                    }
                }
                entries.push(entry);
                continue;
            }
        }
        let inspect = || -> Result<Option<(Value, bool, bool)>, String> {
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("directory metadata: {e}")),
                Ok(m) if !m.is_dir() => return Err("lease entry is not a directory".into()),
                Ok(_) => {}
            }
            let marker_path = path.join("phonefarm-lease.json");
            let metadata = fs::symlink_metadata(&marker_path)
                .map_err(|e| format!("lease marker unavailable: {e}"))?;
            if !metadata.is_file() || metadata.len() > 1 << 20 {
                return Err("lease marker is not a bounded regular file".into());
            }
            let bytes =
                fs::read(marker_path).map_err(|e| format!("lease marker unavailable: {e}"))?;
            let m: Value = serde_json::from_slice(&bytes)
                .map_err(|_| "lease marker is not valid JSON".to_string())?;
            let (pending, recovery) = marker_state(&m)?;
            Ok(Some((m, pending, recovery)))
        };
        match inspect() {
            Ok(None) => continue,
            Err(reason) => entry["reason"] = json!(reason),
            Ok(Some((m, pending, recovery))) => {
                entry["role"] = json!("device_lease");
                for key in ["serial", "label", "recovery_context", "job_id", "lease_id"] {
                    entry[key] = m[key].as_str().map_or(Value::Null, |s| json!(s));
                }
                let age = fs::metadata(path.join("owner"))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
                if let Some(age) = age {
                    entry["heartbeat_age_s"] = json!(age.as_secs_f64());
                }
                let pid = m["pid"]
                    .as_u64()
                    .filter(|n| *n > 0 && *n <= u32::MAX as u64);
                let identity = m["serial"].as_str().is_some_and(|s| !s.trim().is_empty())
                    && m["token"].as_str().is_some_and(|token| owns(&path, token));
                let owner_observation = inspect_owner(&m);
                entry["host_process"] = host_process_fact(&m, &owner_observation);
                let owner_gone = match &owner_observation {
                    Ok(Some(InstanceState::Alive)) => false,
                    Ok(Some(_)) => true,
                    Ok(None) => pid.is_some_and(|pid| pid_dead(pid as u32)),
                    Err(_) => false,
                };
                let outer_ref = m["outer_instance_id"].as_str();
                entry["outer_instance_id"] = outer_ref.map_or(Value::Null, |id| json!(id));
                let outer_matches = outer_ref.is_none_or(|id| queue_owner.as_ref().ok().and_then(|v| v.as_ref())
                    .is_some_and(|owner| owner.instance_id == id));
                let (state, reason) = if !identity || pid.is_none() {
                    ("unknown", "lease owner identity is missing or inconsistent")
                } else if pending {
                    (
                        "command_pending",
                        "transport launch is not yet bound to a child receipt",
                    )
                } else if recovery {
                    (
                        "recovery_required",
                        "recorded recovery barrier requires reconciliation",
                    )
                } else if owner_observation.is_err() {
                    ("unknown", "host process instance could not be verified")
                } else if owner_gone {
                    (
                        "abandoned",
                        "recorded host owner is gone or changed; group, remote workload and restoration remain unverified",
                    )
                } else if !outer_matches {
                    ("unknown", "recorded outer instance is absent or does not match; strong owner may still finish restoration")
                } else if bridge_held == Some(true)
                    && age.is_some_and(|a| a <= Duration::from_secs(10))
                {
                    ("active", "bridge is held and owner heartbeat is fresh")
                } else {
                    (
                        "unknown",
                        "owner liveness, bridge ownership or heartbeat is unverified",
                    )
                };
                entry["state"] = json!(state);
                entry["reason"] = json!(reason);
            }
        }
        if entry["state"] == "unknown" {
            errors.push(format!(
                "{directory}: {}",
                entry["reason"].as_str().unwrap()
            ));
        }
        entries.push(entry);
    }
    // A known restoration obligation remains definite even if a separate
    // ordinary queue entry has no strong marker. Keep its unknown entry/error
    // for diagnosis; neither condition grants permission to start device work.
    let status = if entries.iter().any(|e| {
        matches!(
            e["state"].as_str(),
            Some("command_pending" | "recovery_required" | "abandoned")
        )
    }) {
        "blocked"
    } else if !errors.is_empty() {
        "unknown"
    } else if bridge_held == Some(true) || !entries.is_empty() {
        "held"
    } else {
        "clear"
    };
    json!({"schema_version":1,"status":status,"bridge_held":bridge_held,"entries":entries,
        "error":if errors.is_empty(){None}else{Some(errors.join("; "))}})
}

pub(crate) fn storage_status(storage: &crate::lease_storage::Storage) -> Value {
    let mut result = storage.devlock_dir.as_ref().map(|p| lease_status(p)).unwrap_or_else(||
        json!({"schema_version":1,"status":"unknown","bridge_held":null,"entries":[],"error":null}));
    if storage.readiness != "ready" {
        if result["status"] != "blocked" { result["status"] = json!("unknown"); }
        result["error"] = json!(storage.error);
    }
    result["storage"] = json!(storage);
    result
}

pub fn run_lease(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("domain-init") {
        let result = if args.len() == 3 && args[1] == "--participants-stopped" && args[2] == "--json" {
            crate::lease_storage::resolve().upgrade_domain().map_err(|error| ("unavailable", error))
        } else {
            Err(("invalid_request", "phonefarm lease domain-init --participants-stopped --json".into()))
        };
        return match result {
            Ok(storage) => { println!("{}", storage_status(&storage)); 0 }
            Err((code, error)) => {
                println!("{}", json!({"schema":"phonefarm/lease-domain-error@1","code":code,"error":error}));
                2
            }
        };
    }
    if matches!(args.first().map(String::as_str), Some("devices" | "devices-set")) {
        return crate::device_registry::run_cli(args);
    }
    if args.first().map(String::as_str) == Some("init") {
        if args.len() != 2 || args[1] != "--participants-stopped" {
            eprintln!("phonefarm lease init --participants-stopped (stop every old participant first)");
            return 2;
        }
        return match crate::lease_storage::resolve().init() {
            Ok(storage) => { println!("{}", storage_status(&storage)); 0 }
            Err(e) => { eprintln!("lease initialization refused: {e}"); 2 }
        };
    }
    if args.first().map(String::as_str) == Some("status") {
        if args.len() != 2 || args[1] != "--json" {
            eprintln!("phonefarm lease status --json");
            return 2;
        }
        println!("{}", storage_status(&crate::lease_storage::resolve()));
        return 0;
    }
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
    let (root, legacy) = match crate::lease_storage::resolve().require_ready() {
        Ok(paths) => paths,
        Err(e) => { eprintln!("{e}"); return 2; }
    };
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

    fn attach_receipt(scratch: &Scratch, lease: &mut Lease) -> PathBuf {
        let dir = scratch.0.join("receipts");
        fs::create_dir(&dir).unwrap();
        let r = crate::lease_receipt::JobReceipt::new(
            "123456abcdef",
            &dir,
            &lease.serial,
            "test",
            Value::Null,
        )
        .unwrap();
        let mut m = marker(&lease.legacy).unwrap();
        r.bind_marker(&mut m).unwrap();
        durable_replace(
            &lease.legacy.join("phonefarm-lease.json"),
            m.to_string().as_bytes(),
        )
        .unwrap();
        let path = dir.join(format!("{}.json", m["lease_id"].as_str().unwrap()));
        lease.receipt = Some(r);
        lease.publish_receipt(&m, "active", None).unwrap();
        path
    }
    fn read_receipt(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn job_receipt_release_is_not_device_recovery() {
        let scratch = Scratch::new();
        let mut lease = scratch.lease();
        let path = attach_receipt(&scratch, &mut lease);
        let token = lease.token.clone();
        assert_eq!(read_receipt(&path)["state"], "active");
        let observed = lease_status(&scratch.0.join("legacy"));
        assert_eq!(observed["entries"][0]["job_id"], "123456abcdef");
        assert_eq!(observed["entries"][0]["lease_id"], read_receipt(&path)["lease_id"]);
        lease.record_command_pending("host-only").unwrap();
        assert_eq!(read_receipt(&path)["recovery"]["reason"], "command_pending");
        assert!(lease.record_command_pending("replacement").is_err());
        assert_eq!(marker(&lease.legacy).unwrap()["pending_operation"], "host-only");
        lease.cancel_unspawned_command("host-only").unwrap();
        drop(lease);
        let r = read_receipt(&path);
        assert_eq!(r["state"], "released");
        assert_eq!(r["recovery"]["status"], "not_reported");
        assert_eq!(r["recovery_of"], Value::Null);
        assert!(!r.to_string().contains(&token));
        assert!(!scratch.0.join("legacy/devlock.d").exists());
    }

    #[test]
    fn job_receipt_scoped_completion_is_reset_by_new_hold_or_unknown_marker() {
        let scratch = Scratch::new();
        let mut lease = scratch.lease();
        let path = attach_receipt(&scratch, &mut lease);
        let evidence = scratch.0.join("proof.txt");
        fs::write(&evidence, "synthetic helper absence").unwrap();
        lease.hold_for_recovery(&scratch.0).unwrap();
        assert!(lease
            .complete_recovery("helper_retirement_only", "synthetic", &[])
            .is_err());
        lease
            .complete_recovery("helper_retirement_only", "synthetic", &[&evidence])
            .unwrap();
        assert_eq!(
            read_receipt(&path)["recovery"]["evidence_origin"],
            "synthetic"
        );
        lease.hold_for_recovery(&scratch.0).unwrap();
        assert_eq!(read_receipt(&path)["recovery"]["status"], "required");
        assert_eq!(read_receipt(&path)["recovery"]["scope"], Value::Null);
        lease
            .complete_recovery("helper_retirement_only", "synthetic", &[&evidence])
            .unwrap();
        fs::write(lease.legacy.join("phonefarm-lease.json"), "invalid").unwrap();
        drop(lease);
        let r = read_receipt(&path);
        assert_eq!(r["state"], "retained");
        assert_eq!(r["recovery"]["reason"], "marker_unknown");
        assert_eq!(r["recovery"]["status"], "required");
        assert!(scratch.0.join("legacy/devlock.d").exists());
    }

    #[test]
    fn job_receipt_write_failure_blocks_reacquisition_with_or_without_hold() {
        for held in [false, true] {
            let scratch = Scratch::new();
            let mut lease = scratch.lease();
            let path = attach_receipt(&scratch, &mut lease);
            let evidence = scratch.0.join("proof.txt");
            fs::write(&evidence, "synthetic proof").unwrap();
            if held { lease.hold_for_recovery(&scratch.0).unwrap(); }
            fs::rename(path.parent().unwrap(), scratch.0.join("unavailable-receipts")).unwrap();
            if held {
                assert!(lease.complete_recovery("configuration_readback_only", "synthetic", &[&evidence]).is_err());
            } else {
                assert!(lease.clear_child().is_err());
            }
            drop(lease);
            let legacy = scratch.0.join("legacy/devlock.d");
            assert!(marker(&legacy).is_some());
            assert_ne!(lease_status(&scratch.0.join("legacy"))["status"], "clear");
            let error = Lease::acquire(&scratch.0.join("leases"), &scratch.0.join("legacy"),
                "fake-spawn-cancel", "next", Duration::from_millis(100)).err().unwrap();
            assert!(error.contains(if held { "RECOVERY_REQUIRED" } else { "manual/legacy devlock busy" }), "{error}");
        }
    }

    #[test]
    fn status_observes_held_and_recovery_markers_without_changing_them() {
        let scratch = Scratch::new();
        let mut lease = scratch.lease();
        let root = scratch.0.join("legacy");
        let before = fs::read(lease.legacy.join("phonefarm-lease.json")).unwrap();
        let held = lease_status(&root);
        assert_eq!(held["status"], "held");
        assert_eq!(held["bridge_held"], true);
        assert_eq!(held["entries"][0]["state"], "active");
        assert_eq!(
            fs::read(lease.legacy.join("phonefarm-lease.json")).unwrap(),
            before
        );
        assert!(!held.to_string().contains(&lease.token));
        lease.record_command_pending("status-test").unwrap();
        assert_eq!(
            lease_status(&root)["entries"][0]["state"],
            "command_pending"
        );
        lease.cancel_unspawned_command("status-test").unwrap();
        lease.hold_for_recovery(&scratch.0).unwrap();
        drop(lease);
        let recovery = lease_status(&root);
        assert_eq!(recovery["status"], "blocked");
        assert_eq!(recovery["bridge_held"], false);
        assert_eq!(recovery["entries"][0]["state"], "recovery_required");
        let incomplete = root.join(NESTED_DIR);
        fs::create_dir(&incomplete).unwrap();
        let mixed = lease_status(&root);
        assert_eq!(mixed["status"], "blocked");
        assert_eq!(mixed["entries"][1]["state"], "unknown");
        assert!(mixed["error"].as_str().unwrap().contains(NESTED_DIR));
    }

    #[test]
    fn status_never_calls_incomplete_or_corrupt_host_state_clear() {
        let scratch = Scratch::new();
        let root = scratch.0.join("legacy");
        fs::create_dir(&root).unwrap();
        let bridge = lock_file(&root.join("phonefarm-bridge.lock"), Instant::now()).unwrap();
        assert_eq!(lease_status(&root)["status"], "held");
        drop(bridge);
        let legacy = root.join("devlock.d");
        fs::create_dir(&legacy).unwrap();
        assert_eq!(lease_status(&root)["status"], "unknown");
        for content in ["broken JSON", r#"{"format":2,"producer":"phonefarm"}"#] {
            fs::write(legacy.join("phonefarm-lease.json"), content).unwrap();
            assert_eq!(lease_status(&root)["status"], "unknown");
            assert_eq!(
                fs::read_to_string(legacy.join("phonefarm-lease.json")).unwrap(),
                content
            );
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
            ("outer_instance_id", json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
            ("pid_start", json!("different-process")),
            ("host_boot_id", json!("previous-boot")),
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
            ("outer_instance_id", Value::Null),
            ("outer_instance_id", json!("")),
            ("future_recovery_gate", json!(false)),
            ("format", json!(2)),
            ("command_pending", json!("true")),
            ("command_pending", Value::Null),
            ("recovery_required", json!("true")),
            ("recovery_required", json!(0)),
            ("child_pid", json!(123)),
            ("child_pgid", json!(123)),
            ("child_pid", Value::Null),
            ("host_boot_id", Value::Null),
            ("pid_start", json!("")),
            ("child_start", json!("unbound-child")),
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
        assert!(marker_state(&m).is_err());
        m["child_start"] = json!("recorded-start");
        assert!(marker_state(&m).is_ok());
        // Only fully absent identity extensions use the conservative legacy path.
        for key in ["host_boot_id", "pid_start", "child_start"] {
            m.as_object_mut().unwrap().remove(key);
        }
        assert!(marker_state(&m).is_ok());
        m["child_start"] = json!("partial-identity");
        assert!(marker_state(&m).is_err());
        // Only the in-memory test value changed; the owned idle lease can release normally.
        let legacy = lease.legacy.clone();
        drop(lease);
        assert!(!legacy.exists());
    }

    #[test]
    fn previous_boot_transfers_only_the_matching_recovery_context() {
        let scratch = Scratch::new();
        let mut lease = scratch.lease();
        lease.hold_for_recovery(&scratch.0).unwrap();
        let legacy = lease.legacy.clone();
        let identity = lease.process_identity.clone();
        drop(lease);
        let mut prior = marker(&legacy).unwrap();
        prior["host_boot_id"] = json!("previous-boot-fixture");
        prior["child_pid"] = json!(identity.pid);
        prior["child_pgid"] = json!(identity.pid);
        prior["child_start"] = json!(identity.start);
        let path = legacy.join("phonefarm-lease.json");
        let root = scratch.0.join("leases");
        let bridge = scratch.0.join("legacy");
        let timeout = Duration::from_millis(25);
        durable_replace(&path, prior.to_string().as_bytes()).unwrap();
        assert!(
            Lease::recover_confirmed_idle(&root, &bridge, "fake-spawn-cancel", timeout).is_err()
        );
        let wrong = scratch.0.join("wrong-context");
        fs::create_dir(&wrong).unwrap();
        assert!(
            Lease::acquire_recovery(&root, &bridge, "fake-spawn-cancel", &wrong, timeout).is_err()
        );
        prior["command_pending"] = json!(true);
        durable_replace(&path, prior.to_string().as_bytes()).unwrap();
        assert!(
            Lease::acquire_recovery(&root, &bridge, "fake-spawn-cancel", &scratch.0, timeout)
                .is_err()
        );
        prior["command_pending"] = json!(false);
        durable_replace(&path, prior.to_string().as_bytes()).unwrap();
        let recovered =
            Lease::acquire_recovery(&root, &bridge, "fake-spawn-cancel", &scratch.0, timeout)
                .unwrap();
        recovered.check().unwrap();
        let next = marker(&legacy).unwrap();
        assert_eq!(next["previous_owner"], prior);
        assert_eq!(marker_identity(&next).unwrap(), Some(identity));
        assert_eq!(next["recovery_required"], true);
        for key in ["child_pid", "child_pgid", "child_start"] {
            assert!(next.get(key).is_none());
        }
        drop(recovered);
        assert!(legacy.exists()); // A new owner is not evidence of device restoration.
    }

    #[test]
    fn exited_unreaped_child_can_record_its_instance() {
        use std::os::unix::process::CommandExt;
        let scratch = Scratch::new();
        let lease = scratch.lease();
        lease.record_command_pending("short-child").unwrap();
        let mut child = std::process::Command::new("/usr/bin/true")
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let zombie = loop {
            if host_process::process(child.id())
                .ok()
                .flatten()
                .is_some_and(|p| p.zombie)
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let recorded = lease.record_child(child.id());
        let reaped = child.wait(); // Always reap our child before any assertion can panic.
        assert!(zombie, "short child did not become an observable zombie");
        recorded.unwrap();
        assert!(reaped.unwrap().success());
        let m = marker(&lease.legacy).unwrap();
        assert_eq!(m["child_pid"], child.id());
        assert!(m["child_start"]
            .as_str()
            .is_some_and(host_process::clean_identity));
        lease.clear_child().unwrap();
        let legacy = lease.legacy.clone();
        drop(lease);
        assert!(!legacy.exists());
    }

    fn remote_recovery_fixture(scratch: &Scratch) -> (PathBuf, Value) {
        let context = scratch.0.canonicalize().unwrap();
        let mut lease = scratch.lease();
        lease.hold_for_recovery(&context).unwrap();
        lease.record_command_pending("remote-tap").unwrap();
        let legacy = lease.legacy.clone();
        let mut prior = marker(&legacy).unwrap();
        drop(lease);
        // Model the original process instance having exited and its PID reused.
        prior["pid_start"] = json!("retired-remote-instance");
        durable_replace(&legacy.join("phonefarm-lease.json"), prior.to_string().as_bytes()).unwrap();
        let state = json!({"schema":"phonefarm/remote-control-state@1","session_id":"remote-test",
            "serial":prior["serial"],"context":context,"token":prior["token"],"pending_request":"tap.1",
            "owner":{"pid":prior["pid"],"start":prior["pid_start"],"host_boot_id":prior["host_boot_id"]}});
        durable_replace(&context.join("session.json"), state.to_string().as_bytes()).unwrap();
        (context, prior)
    }

    #[test]
    fn remote_pending_recovery_requires_explicit_assertion_and_saves_original_marker() {
        let scratch = Scratch::new();
        let (context, prior) = remote_recovery_fixture(&scratch);
        let root = scratch.0.join("leases"); let legacy = scratch.0.join("legacy");
        let serial = prior["serial"].as_str().unwrap();
        let evidence = context.join("recovery-unit.json");
        assert!(Lease::acquire_recovery(&root, &legacy, serial, &context, Duration::ZERO).is_err());
        assert!(Lease::acquire_remote_recovery(&root, &legacy, serial, &context, "independent verification", false, &evidence).is_err());
        assert!(!evidence.exists());
        assert_eq!(marker(&legacy.join("devlock.d")).unwrap(), prior);
        let mut recovered = Lease::acquire_remote_recovery(&root, &legacy, serial, &context,
            "independently stopped every possible original launcher and device transport", true, &evidence).unwrap();
        let report = read_receipt(&evidence);
        assert_eq!(report["previous_owner"], prior);
        assert_eq!(report["origin"], "operator_assertion");
        assert_eq!(report["device_verified"], false);
        assert_eq!(report["unbound_pending_retired"], true);
        assert_eq!(marker(&legacy.join("devlock.d")).unwrap()["command_pending"], false);
        recovered.complete_recovery("remote_control_commands_settled", "operator_assertion", &[&evidence]).unwrap();
        drop(recovered);
        assert!(!legacy.join("devlock.d").exists());
    }

    #[test]
    fn remote_pending_recovery_cannot_override_schema_or_recorded_group() {
        use std::os::unix::process::CommandExt;
        let scratch = Scratch::new();
        let (context, mut prior) = remote_recovery_fixture(&scratch);
        let root = scratch.0.join("leases"); let legacy = scratch.0.join("legacy");
        let serial = prior["serial"].as_str().unwrap().to_owned();
        let evidence = context.join("recovery-unit.json");
        let original = read_receipt(&context.join("session.json"));
        for (key, invalid) in [("schema", json!("other-session@1")), ("context", json!("/wrong/context"))] {
            let mut state = original.clone(); state[key] = invalid;
            durable_replace(&context.join("session.json"), state.to_string().as_bytes()).unwrap();
            assert!(Lease::acquire_remote_recovery(&root, &legacy, &serial, &context, "verified", true, &evidence).is_err());
            assert!(!evidence.exists());
        }
        durable_replace(&context.join("session.json"), original.to_string().as_bytes()).unwrap();
        let mut child = std::process::Command::new("/bin/sleep").arg("30").process_group(0).spawn().unwrap();
        prior["command_pending"] = json!(false);
        prior["child_pid"] = json!(child.id()); prior["child_pgid"] = json!(child.id());
        // Even a departed/reused recorded leader cannot make its live group safe.
        prior["child_start"] = json!("retired-child-instance");
        durable_replace(&legacy.join("devlock.d/phonefarm-lease.json"), prior.to_string().as_bytes()).unwrap();
        let refused = Lease::acquire_remote_recovery(&root, &legacy, &serial, &context, "verified", true, &evidence).is_err();
        let _ = child.kill(); let _ = child.wait();
        assert!(refused);
        assert!(!evidence.exists());
        assert_eq!(marker(&legacy.join("devlock.d")).unwrap(), prior);
    }
}
