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

fn marker(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path.join("phonefarm-lease.json")).ok()?).ok()
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
        let legacy = legacy_root.join("devlock.d");
        loop {
            match fs::create_dir(&legacy) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(m) = marker(&legacy) {
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
        if let Err(e) = fs::write(legacy.join("phonefarm-lease.json"), info.to_string()) {
            let _ = fs::remove_dir(&legacy);
            return Err(format!("devlock marker: {e}"));
        }
        if let Err(e) = fs::write(legacy.join("owner"), &owner) {
            let _ = fs::remove_file(legacy.join("phonefarm-lease.json"));
            let _ = fs::remove_file(legacy.join("owner"));
            let _ = fs::remove_dir(&legacy);
            return Err(format!("devlock heartbeat: {e}"));
        }
        let (tx, rx) = mpsc::channel();
        let hb_path = legacy.clone();
        let hb_token = token.clone();
        let heartbeat = std::thread::spawn(move || {
            while rx.recv_timeout(Duration::from_secs(5)) == Err(mpsc::RecvTimeoutError::Timeout) {
                if !owns(&hb_path, &hb_token) {
                    break;
                }
                if let Err(e) = fs::write(hb_path.join("owner"), &owner) {
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
            serial: serial.into(),
        })
    }

    pub fn check(&self) -> Result<(), String> {
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

    fn record_child(&self, pid: u32) -> Result<(), String> {
        self.check()?;
        let mut m = marker(&self.legacy).ok_or("lease marker unavailable")?;
        m["child_pid"] = json!(pid);
        fs::write(self.legacy.join("phonefarm-lease.json"), m.to_string())
            .map_err(|e| format!("record child: {e}"))
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
        let legacy = legacy_root.join("devlock.d");
        let m = marker(&legacy)
            .ok_or("no verifiable phonefarm owner; cannot recover manual/unknown lock")?;
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
        remove_owned(&self.legacy, &self.token);
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
    let _lease = match Lease::acquire(
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
    let mut child = match std::process::Command::new(command)
        .args(&args[split + 2..])
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
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
