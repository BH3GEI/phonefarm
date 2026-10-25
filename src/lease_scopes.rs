//! Registered lanes within the existing lease root. No alternative device owner.
use super::*;
use crate::device_registry::DeviceBinding;
use crate::lease_domain::Paths;

pub(super) struct Lane {
    pub path: PathBuf,
    pub binding: DeviceBinding,
    pub marker: Value,
}

pub(super) fn fd_path(root: &Path, binding: &DeviceBinding) -> PathBuf {
    id_fd_path(root, &binding.device_id)
}
fn id_fd_path(root: &Path, device_id: &str) -> PathBuf {
    root.join("by-device-id")
        .join(format!("{:x}.lock", Sha256::digest(device_id.as_bytes())))
}

pub(super) fn binding(m: &Value) -> Result<Option<DeviceBinding>, String> {
    match m["format"].as_u64() {
        Some(1) if m.get("scope").is_none() && m.get("device_binding").is_none() => Ok(None),
        Some(2) if m["scope"] == "device" && m.get("outer_instance_id").is_none() => {
            let value = crate::device_registry::parse_binding(&m["device_binding"].to_string())?;
            if m["serial"] != value.serial { return Err("scoped marker serial differs from binding".into()); }
            Ok(Some(value))
        }
        _ => Err("unsupported or inconsistent lease scope".into()),
    }
}

/// Every persisted lane must have an exact recorded identity before another
/// device may proceed. A known owner's liveness is evaluated separately.
pub(super) fn scan(root: &Path, paths: &Paths) -> Result<Vec<Lane>, String> {
    if !paths.lanes_enabled { return Ok(Vec::new()); }
    let dir = paths.lanes_dir.as_ref().ok_or("missing lane namespace")?;
    let mut result = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        if result.len() == 128 { return Err("lane enumeration exceeds registry limit".into()); }
        let path = entry.map_err(|e| e.to_string())?.path();
        let file = path.join("phonefarm-lease.json");
        let meta = fs::symlink_metadata(&file).map_err(|e| format!("unattributed lane {}: {e}", path.display()))?;
        if !meta.is_file() || meta.len() > 65536 { return Err("lane marker is not bounded regular data".into()); }
        let m: Value = serde_json::from_slice(&fs::read(&file).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid lane marker: {e}"))?;
        marker_state(&m)?;
        let binding = binding(&m)?.ok_or("global marker in device namespace")?;
        if paths.domain_id.as_deref() != Some(&binding.domain_id)
            || paths.lane_marker(&binding.device_id)? != path
            || marker_identity(&m)?.is_none()
            || m["token"].as_str().is_none_or(str::is_empty)
            || !owns(&path, m["token"].as_str().unwrap_or(""))
        { return Err(format!("lane identity/path is incomplete or mismatched: {}", path.display())); }
        crate::device_registry::validate_recorded_binding(root, &binding)?;
        result.push(Lane { path, binding, marker: m });
    }
    Ok(result)
}

pub(super) fn global_clear(paths: &Paths) -> Result<(), String> {
    for name in ["devlock.d", NESTED_DIR] {
        match fs::symlink_metadata(paths.queue_dir.join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.to_string()),
            Ok(_) => return Err("global owner/recovery record blocks device scope".into()),
        }
    }
    Ok(())
}

/// Called while the caller owns the queue mutex, or for a readonly snapshot.
pub(super) fn queue_waiting(paths: &Paths) -> Result<bool, String> {
    let path = paths.queue_dir.join("devlock.queue");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
        Ok(m) if !m.is_file() || m.len() > 65536 => Err("global queue is not bounded regular data".into()),
        Ok(_) => Ok(!fs::read_to_string(path).map_err(|e| e.to_string())?.trim().is_empty()),
    }
}

pub(super) fn probe_lock(path: &Path, shared: bool) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
        Ok(m) if !m.is_file() => return Err("lease lock is not a regular file".into()),
        Ok(_) => (),
    }
    crate::shared_access::check_existing(path, true).map_err(|e| e.to_string())?;
    let file = File::open(path).map_err(|e| e.to_string())?;
    match if shared { file.try_lock_shared() } else { file.try_lock() } {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

pub(super) fn entry(lane: &Lane, device_root: &Path) -> Value {
    let m = &lane.marker;
    let observed = inspect_owner(m);
    let age = fs::metadata(lane.path.join("owner")).and_then(|m| m.modified()).ok()
        .and_then(|t| std::time::SystemTime::now().duration_since(t).ok());
    let held = probe_lock(&fd_path(device_root, &lane.binding), false);
    let (pending, recovery) = marker_state(m).expect("scan validated marker");
    let (state, reason) = if pending { ("command_pending", "transport launch remains unbound") }
        else if recovery { ("recovery_required", "recorded device recovery obligation") }
        else if observed.as_ref().is_ok_and(|v| v.is_some_and(|state| state != InstanceState::Alive)) {
            ("abandoned", "recorded process is gone or changed; restoration remains unverified")
        } else if observed.as_ref().is_ok_and(|v| matches!(v, Some(InstanceState::Alive)))
            && held.as_ref().is_ok_and(|held| *held) && age.is_some_and(|a| a <= Duration::from_secs(10)) {
            ("active", "this device lock is held and its owner instance/heartbeat is verified")
        } else { ("unknown", "this device owner/lock/heartbeat could not be verified") };
    json!({"directory":lane.path,"marker_path":lane.path.join("phonefarm-lease.json"),
        "scope":"device","device_binding":lane.binding,"serial":m["serial"],"label":m["label"],
        "job_id":m["job_id"],"lease_id":m["lease_id"],"recovery_context":m["recovery_context"],
        "role":"device_lease","state":state,"reason":reason,"host_process":host_process_fact(m,&observed),
        "device_lock_held":held.ok(),"heartbeat_age_s":age.map(|a|a.as_secs_f64())})
}

pub(super) fn status(entries: &[Value], errors: &[String], held: bool) -> &'static str {
    if entries.iter().any(|e| matches!(e["state"].as_str(),
        Some("command_pending" | "recovery_required" | "abandoned"))) { "blocked" }
    else if !errors.is_empty() || entries.iter().any(|e| e["state"] == "unknown") { "unknown" }
    else if held || !entries.is_empty() { "held" } else { "clear" }
}

pub(super) fn snapshot(root: &Path, paths: &Paths, entries: &mut Vec<Value>,
    errors: &mut Vec<String>, bridge_held: Option<bool>) -> Value {
    let exclusive = probe_lock(&root.join("phonefarm-bridge.lock"), true);
    if let Err(error) = &exclusive { errors.push(error.clone()); }
    let global_status = status(entries, errors, exclusive.as_ref().is_ok_and(|held| *held));
    let global_error = if errors.is_empty() { None } else { Some(errors.join("; ")) };
    let registry = crate::device_registry::snapshot(root);
    let lanes = scan(root, paths);
    let waiting = queue_waiting(paths);
    let mut unknown = !errors.is_empty();
    for result in [registry.as_ref().map(|_| ()), lanes.as_ref().map(|_| ()), waiting.as_ref().map(|_| ())] {
        if let Err(error) = result { errors.push(error.clone()); unknown = true; }
    }
    match fs::symlink_metadata(paths.queue_dir.join("devlock.qmutex")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => { errors.push(e.to_string()); unknown = true; }
        Ok(_) => { errors.push("global FIFO is being changed or retains an unknown mutex".into()); unknown = true; }
    }
    let mut devices = Vec::new();
    if let (Ok((_, registered)), Ok(lanes)) = (&registry, &lanes) {
        if bridge_held == Some(true) && exclusive == Ok(false) && lanes.is_empty() {
            errors.push("shared bridge owner has no attributable device record".into());
            unknown = true;
        }
        for device in registered {
            let lane = lanes.iter().find(|lane| lane.binding.device_id == device.device_id);
            let (state, error) = if let Some(lane) = lane {
                let item = entry(lane, paths.device_lease_dir.as_ref().expect("layout 2 device root"));
                let state = status(std::slice::from_ref(&item), &[], false);
                let error = item["reason"].as_str().map(str::to_owned);
                entries.push(item);
                (state, error)
            } else {
                match probe_lock(&id_fd_path(paths.device_lease_dir.as_ref().expect("layout 2 device root"), &device.device_id), false) {
                    Ok(false) => ("clear", None),
                    result => {
                        let error = result.err().unwrap_or_else(|| "device lock is held without a published owner".into());
                        entries.push(json!({"scope":"device","device_id":device.device_id,
                            "state":"unknown","reason":error}));
                        ("unknown", Some(error))
                    }
                }
            };
            devices.push(json!({"device_id":device.device_id,"status":state,"error":error}));
        }
        if lanes.iter().any(|lane| !registered.iter().any(|d| d.device_id == lane.binding.device_id)) {
            errors.push("recorded device lane is absent from the current registry".into());
            unknown = true;
        }
    }
    json!({"schema":"phonefarm/device-scopes@1","domain_id":paths.domain_id,
        "registry_revision":registry.as_ref().map(|(revision,_)|*revision).ok(),
        "global":{"status":global_status,"error":global_error},
        "queue_waiting":waiting.unwrap_or(true),"unknown":unknown,"devices":devices})
}
