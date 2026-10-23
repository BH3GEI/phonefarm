//! Identity for the existing outer queue owner. No device commands or long-lived locks.
use crate::host_process::{boot_id, clean_identity, process};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const SCHEMA: &str = "phonefarm/devlock-owner@1";
const MARKER: &str = "phonefarm-lease.json";
const LIMIT: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct QueueOwner {
    pub schema: String,
    pub instance_id: String,
    pub label: String,
    pub holder_pid: u32,
    pub holder_start: String,
    pub host_boot_id: String,
    pub owner: String,
}

pub(crate) fn valid_instance(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 128
        && !label.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn checked_dir(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => Err(format!("not a regular directory: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
fn root_path(root: &Path) -> Result<(), String> {
    if !root.is_absolute() {
        return Err("queue root must be absolute".into());
    }
    checked_dir(root).map(|_| ())
}
fn bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err("owner file is not bounded regular data".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("owner file exceeds limit".into());
    }
    Ok(bytes)
}

/// No writes and no liveness assumption. A known strong marker belongs to devlease.
pub(crate) fn read(root: &Path) -> Result<Option<QueueOwner>, String> {
    root_path(root)?;
    let dir = root.join("devlock.d");
    if !checked_dir(&dir)? {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&bounded(&dir.join(MARKER))?)
        .map_err(|e| format!("invalid queue/lease marker JSON: {e}"))?;
    if value["format"] == 1 && value["producer"] == "phonefarm" {
        return Ok(None);
    }
    let owner: QueueOwner =
        serde_json::from_value(value).map_err(|e| format!("unrecognized queue owner: {e}"))?;
    if owner.schema != SCHEMA
        || !valid_instance(&owner.instance_id)
        || !valid_label(&owner.label)
        || owner.holder_pid <= 1
        || owner.holder_pid > i32::MAX as u32
        || !clean_identity(&owner.holder_start)
        || !clean_identity(&owner.host_boot_id)
        || !owner.owner.starts_with(&format!("{} since ", owner.label))
        || !owner
            .owner
            .ends_with(&format!(" instance={}\n", owner.instance_id))
        || owner.owner[..owner.owner.len() - 1].contains(['\r', '\n'])
    {
        return Err("invalid queue owner identity".into());
    }
    if bounded(&dir.join("owner"))? != owner.owner.as_bytes() {
        return Err("queue marker and owner text differ".into());
    }
    Ok(Some(owner))
}

fn is_live(owner: &QueueOwner) -> Result<bool, String> {
    if boot_id()? != owner.host_boot_id {
        return Ok(false);
    }
    Ok(process(owner.holder_pid)?.is_some_and(|p| p.start == owner.holder_start && !p.zombie))
}
pub(crate) fn check_live(owner: &QueueOwner) -> Result<(), String> {
    if is_live(owner)? {
        Ok(())
    } else {
        Err("queue holder exited or process/boot instance changed".into())
    }
}
fn descendant(holder: u32, mut caller: u32) -> Result<bool, String> {
    for _ in 0..128 {
        if caller == holder {
            return Ok(true);
        }
        if caller <= 1 {
            return Ok(false);
        }
        let Some(info) = process(caller)? else {
            return Ok(false);
        };
        if info.zombie || info.parent == caller {
            return Ok(false);
        }
        caller = info.parent;
    }
    Err("host process ancestry exceeds limit".into())
}
fn authorize(owner: &QueueOwner, instance: &str, label: &str) -> Result<(), String> {
    if !valid_instance(instance) || owner.instance_id != instance || owner.label != label {
        return Err("queue instance or label does not match; no ownership was borrowed".into());
    }
    check_live(owner)?;
    if !descendant(owner.holder_pid, std::process::id())? {
        return Err("caller is outside the queue holder process tree".into());
    }
    check_live(owner)
}
pub(crate) fn inherited(root: &Path) -> Result<Option<QueueOwner>, String> {
    let instance = match std::env::var("DEVLOCK_INSTANCE_ID") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(_) => return Err("DEVLOCK_INSTANCE_ID is not UTF-8".into()),
    };
    let label = std::env::var("DEVLOCK_LABEL")
        .map_err(|_| "inherited queue instance requires DEVLOCK_LABEL")?;
    let owner = read(root)?.ok_or("inherited queue owner is absent or is a strong lease")?;
    authorize(&owner, &instance, &label)?;
    Ok(Some(owner))
}

pub(crate) fn lock(root: &Path) -> Result<File, String> {
    root_path(root)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join("queue-owner.lock"))
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
fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
fn exact_files(dir: &Path, expected: &[&str]) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| expected.contains(&name))
        {
            return Err("queue directory contains unrecognized files; preserved".into());
        }
    }
    Ok(())
}
fn same(root: &Path, expected: &QueueOwner) -> Result<(), String> {
    if read(root)?.as_ref() == Some(expected) {
        Ok(())
    } else {
        Err("queue instance changed; metadata preserved".into())
    }
}
fn bridge(root: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join("phonefarm-bridge.lock"))
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("bridge is not a regular file".into());
    }
    // Strong acquisition takes bridge before the short metadata mutex. Never
    // wait in the reverse order: an active/unknown bridge preserves the barrier.
    file.try_lock()
        .map_err(|e| format!("bridge held or unknown: {e}"))?;
    Ok(file)
}
fn create(root: &Path, label: &str, holder: u32) -> Result<QueueOwner, String> {
    if !valid_label(label) || holder <= 1 || holder > i32::MAX as u32 {
        return Err("invalid queue label or holder PID".into());
    }
    if std::env::var_os("DEVLOCK_INSTANCE_ID").is_some() {
        return Err("inherited instance cannot create a replacement queue owner".into());
    }
    let _lock = lock(root)?;
    let dir = root.join("devlock.d");
    if !checked_dir(&dir)? {
        return Err("queue directory must first be acquired by devlock".into());
    }
    exact_files(&dir, &[])?;
    let _bridge = bridge(root)?;
    if exists(&root.join("phonefarm-lease.d"))? {
        return Err("nested lease entry requires reconciliation".into());
    }
    let info = process(holder)?
        .filter(|p| !p.zombie)
        .ok_or("queue holder is not live")?;
    if !descendant(holder, std::process::id())? {
        return Err("queue holder is not an actual caller ancestor".into());
    }
    let mut random = [0u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut random))
        .map_err(|e| e.to_string())?;
    let instance_id: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let owner = QueueOwner {
        schema: SCHEMA.into(),
        owner: format!(
            "{label} since {} instance={instance_id}\n",
            chrono::Local::now().format("%F %T")
        ),
        instance_id,
        label: label.into(),
        holder_pid: holder,
        holder_start: info.start,
        host_boot_id: boot_id()?,
    };
    check_live(&owner)?;
    crate::devlease::durable_replace(
        &dir.join(MARKER),
        &serde_json::to_vec(&owner).map_err(|e| e.to_string())?,
    )?;
    crate::devlease::durable_replace(&dir.join("owner"), owner.owner.as_bytes())?;
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    same(root, &owner)?;
    Ok(owner)
}
fn retire(root: &Path, owner: &QueueOwner) -> Result<(), String> {
    let dir = root.join("devlock.d");
    exact_files(&dir, &[MARKER, "owner"])?;
    same(root, owner)?;
    fs::remove_file(dir.join("owner")).map_err(|e| e.to_string())?;
    fs::remove_file(dir.join(MARKER)).map_err(|e| e.to_string())?;
    fs::remove_dir(dir).map_err(|e| e.to_string())?;
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}
fn renew_or_release(
    root: &Path,
    instance: &str,
    label: &str,
    release: bool,
) -> Result<Value, String> {
    let _lock = lock(root)?;
    let owner = read(root)?.ok_or("queue owner is absent or is a strong lease")?;
    authorize(&owner, instance, label)?;
    same(root, &owner)?;
    if release {
        retire(root, &owner)?;
    } else {
        crate::devlease::durable_replace(&root.join("devlock.d/owner"), owner.owner.as_bytes())?;
        same(root, &owner)?;
    }
    Ok(
        json!({"schema":"phonefarm/devlock-operation@1","operation":if release {"release"} else {"renew"},
        "instance_id":instance,"status":"complete","nested_preserved":true}),
    )
}
fn reconcile(root: &Path, instance: &str) -> Result<Value, String> {
    if !valid_instance(instance) {
        return Err("invalid queue instance".into());
    }
    let _lock = lock(root)?;
    let owner = read(root)?.ok_or("no typed queue owner to reconcile")?;
    if owner.instance_id != instance {
        return Err("queue reconciliation instance mismatch".into());
    }
    if is_live(&owner)? {
        return Err("queue holder is still live; reconciliation refused".into());
    }
    if exists(&root.join("phonefarm-lease.d"))? {
        return Err("nested lease entry must be recovered first".into());
    }
    let _bridge = bridge(root)?;
    same(root, &owner)?;
    if exists(&root.join("phonefarm-lease.d"))? {
        return Err("nested lease appeared; reconciliation refused".into());
    }
    let archive = root.join("devlock-reconciliations");
    if !checked_dir(&archive)? {
        fs::create_dir(&archive).map_err(|e| e.to_string())?;
    }
    let report = json!({"schema":"phonefarm/devlock-reconciliation@1","instance_id":instance,"owner":owner,
        "scope":"outer_queue_metadata_retired_after_explicit_idle_assertion","participants_stopped_asserted":true,
        "device_idle_asserted":true,"device_recovery_verified":false,"acknowledged_by_pid":std::process::id(),
        "at":chrono::Utc::now().to_rfc3339()});
    let path = archive.join(format!("{instance}.json"));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let old: Value = serde_json::from_slice(&bounded(&path)?).map_err(|e| e.to_string())?;
            if old["schema"] != report["schema"] || old["owner"] != report["owner"] {
                return Err("existing queue retirement audit differs; preserved".into());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    File::open(&archive)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    retire(root, &owner)?;
    Ok(
        json!({"schema":"phonefarm/devlock-reconciliation@1","status":"retired","instance_id":instance,
        "scope":report["scope"],"device_recovery_verified":false,"audit_path":path}),
    )
}

pub(crate) fn run(args: &[String]) -> i32 {
    let result = (|| -> Result<Value, String> {
        let operation = args.first().ok_or("queue helper requires an operation")?;
        let mut root: Option<PathBuf> = None;
        let mut label = None;
        let mut instance = None;
        let mut holder = None;
        let (mut stopped, mut idle) = (false, false);
        let mut it = args[1..].iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--participants-stopped" => stopped = true,
                "--confirm-device-idle" => idle = true,
                "--root" | "--label" | "--instance" | "--holder-pid" => {
                    let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                    match arg.as_str() {
                        "--root" => root = Some(PathBuf::from(value)),
                        "--label" => label = Some(value.clone()),
                        "--instance" => instance = Some(value.clone()),
                        _ => holder = Some(value.parse::<u32>().map_err(|_| "invalid holder PID")?),
                    }
                }
                _ => return Err(format!("unknown queue helper option {arg}")),
            }
        }
        let root = root.ok_or("queue helper requires --root")?;
        root_path(&root)?;
        match operation.as_str() {
            "create" => serde_json::to_value(create(
                &root,
                label.as_deref().ok_or("create requires --label")?,
                holder.ok_or("create requires --holder-pid")?,
            )?)
            .map_err(|e| e.to_string()),
            "check" => {
                if let Some(label) = label {
                    let instance = instance
                        .or_else(|| std::env::var("DEVLOCK_INSTANCE_ID").ok())
                        .ok_or("check requires a queue instance")?;
                    let owner = read(&root)?.ok_or("no typed queue owner")?;
                    authorize(&owner, &instance, &label)?;
                    serde_json::to_value(owner).map_err(|e| e.to_string())
                } else {
                    if instance.is_some() {
                        return Err("instance check also requires --label".into());
                    }
                    let owner = read(&root)?;
                    let live = owner.as_ref().map(check_live);
                    Ok(
                        json!({"schema":"phonefarm/devlock-check@1","status":match &live {
                        Some(Ok(())) => "held", Some(Err(_)) => "unknown", None if exists(&root.join("devlock.d"))? => "protected", None => "absent" },
                        "owner":owner,"error":live.and_then(Result::err)}),
                    )
                }
            }
            "renew" | "release" => {
                let instance = instance
                    .or_else(|| std::env::var("DEVLOCK_INSTANCE_ID").ok())
                    .ok_or("renew/release requires DEVLOCK_INSTANCE_ID")?;
                renew_or_release(
                    &root,
                    &instance,
                    label.as_deref().ok_or("renew/release requires --label")?,
                    operation == "release",
                )
            }
            "reconcile" if stopped && idle => reconcile(
                &root,
                instance.as_deref().ok_or("reconcile requires --instance")?,
            ),
            "reconcile" => {
                Err("reconcile requires --participants-stopped and --confirm-device-idle".into())
            }
            _ => Err("unknown queue helper operation".into()),
        }
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({"schema":"phonefarm/devlock-error@1","error":error})
            );
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_instance_and_inheritance_are_not_labels() {
        let pid = std::process::id();
        let info = process(pid).unwrap().unwrap();
        let mut owner = QueueOwner {
            schema: SCHEMA.into(),
            instance_id: "0123456789abcdef0123456789abcdef".into(),
            label: "test".into(),
            holder_pid: pid,
            holder_start: info.start,
            host_boot_id: boot_id().unwrap(),
            owner: String::new(),
        };
        assert!(authorize(&owner, &owner.instance_id, "test").is_ok());
        assert!(authorize(&owner, "ffffffffffffffffffffffffffffffff", "test").is_err());
        assert!(authorize(&owner, &owner.instance_id, "other").is_err());
        owner.holder_start.push('0');
        assert!(check_live(&owner).is_err());
        assert!(!valid_instance("../../owner"));
    }
}
