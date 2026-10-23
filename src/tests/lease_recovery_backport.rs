//! Actual host processes and CLI; the explicit fake serial never resolves a device.
#![cfg(unix)]
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    dir: PathBuf,
    retain: bool,
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::var_os("PHONEFARM_BACKPORT_FIXTURE_ROOT");
        let retain = root.is_some();
        let dir = root
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(
                "lease-backport-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, retain }
    }
    fn marker(&self) -> PathBuf {
        self.dir.join("legacy/devlock.d/phonefarm-lease.json")
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.current_dir(&self.dir)
            .env("PHONEFARM_LEASE_DIR", self.dir.join("leases"))
            .env("DEVLOCK_DIR", self.dir.join("legacy"))
            .env("ADB_BIN", self.dir.join("NO_DEVICE_TRANSPORT"));
        c.args(["lease", "--serial", "fake-device", "--timeout-ms", "50"]);
        c
    }
    fn recover(&self) -> Output {
        self.cmd()
            .args(["--recover-abandoned", "--confirm-device-idle"])
            .output()
            .unwrap()
    }
    fn status(&self) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
            .current_dir(&self.dir)
            .env("DEVLOCK_DIR", self.dir.join("legacy"))
            .args(["lease", "status", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn save(&self, name: &str, r: &Output) {
        std::fs::write(
            self.dir.join(format!("{name}.json")),
            json!({
                "exit":r.status.code(), "stdout":String::from_utf8_lossy(&r.stdout),
                "stderr":String::from_utf8_lossy(&r.stderr), "device_called":false
            })
            .to_string(),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.retain {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
struct OwnedGroup(i32);
impl Drop for OwnedGroup {
    fn drop(&mut self) {
        // This PGID comes only from our launched deterministic helper, never a caller ledger.
        assert!(self.0 > 0 && self.0 != unsafe { libc::getpgrp() });
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[test]
fn status_without_serial_leaves_missing_roots_missing() {
    let f = Fixture::new("readonly-status");
    let mut status = f.status();
    let storage = status.as_object_mut().unwrap().remove("storage").unwrap();
    assert_eq!(storage["schema_version"], 1);
    assert_eq!(storage["mode"], "custom");
    assert_eq!(storage["readiness"], "ready");
    assert_eq!(storage["devlock_dir"], json!(f.dir.canonicalize().unwrap().join("legacy")));
    assert_eq!(
        status,
        json!({"schema_version":1,"status":"clear","bridge_held":false,
        "entries":[],"error":null})
    );
    assert_eq!(std::fs::read_dir(&f.dir).unwrap().count(), 0);
}

#[test]
fn normal_leader_exit_cannot_release_or_manual_recover_a_live_owned_grandchild() {
    let f = Fixture::new("grandchild");
    let script = "import subprocess,os,pathlib,sys; p=subprocess.Popen(['sleep','30'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); pathlib.Path(sys.argv[1]).write_text(str(p.pid)+' '+str(os.getpgrp()));";
    let r = f
        .cmd()
        .args(["--", "python3", "-c", script])
        .arg(f.dir.join("child.identity"))
        .output()
        .unwrap();
    f.save("wrapper", &r);
    let identity = std::fs::read_to_string(f.dir.join("child.identity")).unwrap();
    let mut ids = identity
        .split_whitespace()
        .map(|s| s.parse::<i32>().unwrap());
    let pid = ids.next().unwrap();
    let group = ids.next().unwrap();
    let _owned = OwnedGroup(group);
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "the actual grandchild must remain live"
    );
    assert_eq!(r.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&r.stderr).contains("process group remains live/unknown"));
    let before = std::fs::read(f.marker()).unwrap();
    let m: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(m["child_pid"], group);
    assert_eq!(m["child_pgid"], group);
    assert_eq!(m["command_pending"], false);
    let denied = f.recover();
    f.save("manual-recover", &denied);
    assert_eq!(denied.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("group live/unknown"));
    let next = f.cmd().args(["--", "/bin/true"]).output().unwrap();
    f.save("next-admission", &next);
    assert_eq!(next.status.code(), Some(2));
    assert_eq!(std::fs::read(f.marker()).unwrap(), before);
    assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
    std::fs::write(
        f.dir.join("verified.json"),
        json!({"leader_exited":true,
        "owned_grandchild_alive":true,"pid":pid,"pgid":group,"marker_retained":true,
        "hardware_verified":false})
        .to_string(),
    )
    .unwrap();
}

fn dead_owner_marker(f: &Fixture) -> Value {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let path = f.marker();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path.parent().unwrap().join("owner"), "owned-test\n").unwrap();
    json!({"format":1,"producer":"phonefarm","pid":pid,"token":"test-token",
        "serial":"fake-device","owner":"owned-test\n"})
}

#[test]
fn true_malformed_and_newer_barriers_block_both_entry_and_manual_idle_without_mutation() {
    for (name, key, value) in [
        ("pending", "command_pending", json!(true)),
        ("recovery", "recovery_required", json!(true)),
        ("stringpending", "command_pending", json!("true")),
        ("stringrecovery", "recovery_required", json!("true")),
        ("nullpending", "command_pending", Value::Null),
        ("pidonly", "child_pid", json!(123)),
        ("pgidonly", "child_pgid", json!(123)),
        ("future", "future_recovery_gate", json!(false)),
        ("version", "format", json!(2)),
    ] {
        let f = Fixture::new(name);
        let mut m = dead_owner_marker(&f);
        m[key] = value;
        let bytes = m.to_string().into_bytes();
        std::fs::write(f.marker(), &bytes).unwrap();
        let next = f.cmd().args(["--", "/bin/true"]).output().unwrap();
        f.save("admission", &next);
        assert_eq!(next.status.code(), Some(2), "{name}");
        assert_eq!(std::fs::read(f.marker()).unwrap(), bytes, "{name}");
        let recovery = f.recover();
        f.save("manual-recover", &recovery);
        assert_eq!(recovery.status.code(), Some(2), "{name}");
        assert_eq!(std::fs::read(f.marker()).unwrap(), bytes, "{name}");
        if name == "future" {
            assert!(
                String::from_utf8_lossy(&recovery.stderr).contains("unknown lease marker field")
            );
        }
    }
}

#[test]
fn old_idle_format_one_without_extensions_remains_manual_assertion_only_compatible() {
    let f = Fixture::new("legacy-idle");
    let m = dead_owner_marker(&f);
    std::fs::write(f.marker(), m.to_string()).unwrap();
    let status = f.status();
    assert_eq!(status["status"], "blocked");
    assert_eq!(status["entries"][0]["state"], "abandoned");
    assert_eq!(status["entries"][0]["label"], Value::Null);
    assert_eq!(std::fs::read_to_string(f.marker()).unwrap(), m.to_string());
    let recovery = f.recover();
    f.save("manual-recover", &recovery);
    assert_eq!(recovery.status.code(), Some(0));
    assert!(!f.marker().exists());
    let record: Value = serde_json::from_str(
        std::fs::read_to_string(f.dir.join("legacy/phonefarm-recovery.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(record["device_verified"], false);
    assert_eq!(record["device_idle_confirmation"], "operator_assertion");
}

#[test]
fn preflight_fix_keeps_its_strong_lease_and_nested_recovery_blocks_transport() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("preflight-fix");
    let fake = f.dir.join("fake-adb");
    std::fs::write(&fake, r#"#!/usr/bin/env python3
import pathlib, sys
root=pathlib.Path(__file__).parent
cmd=' '.join(sys.argv[1:])
with (root/'fake-calls').open('a') as log: log.write(cmd+'\n')
if 'echo 0 > /sys/kernel/fan/fan_enable' in cmd: (root/'fan-fixed').touch()
elif 'fan_enable' in cmd: print('0' if (root/'fan-fixed').exists() else '1')
elif 'set-volume 3 0' in cmd: (root/'volume-fixed').touch()
elif 'media_session volume' in cmd: print('volume is '+('0' if (root/'volume-fixed').exists() else '2'))
elif '/sys/class/thermal' in cmd: print('cpu-0 40000')
"#).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let devlock = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("tools/devlock");
    let command = || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.current_dir(&f.dir).env("DEVLOCK_DIR", f.dir.join("legacy"))
            .env("PHONEFARM_LEASE_DIR", f.dir.join("leases"))
            .env("PHONEFARM_LEASE_TIMEOUT_MS", "50")
            .env("DEVLOCK_BIN", &devlock).env("ADB_BIN", &fake)
            .args(["preflight", "--fix", "--serial", "fake-device", "--json", "--lock-wait", "0s"]);
        c
    };
    let fixed = command().output().unwrap();
    f.save("fix", &fixed);
    assert!(fixed.status.success(), "{}", String::from_utf8_lossy(&fixed.stderr));
    assert!(f.dir.join("fan-fixed").exists());
    assert!(f.dir.join("volume-fixed").exists());
    assert_eq!(f.status()["status"], "clear");
    let calls = std::fs::read(f.dir.join("fake-calls")).unwrap();
    let nested = f.dir.join("legacy/phonefarm-lease.d");
    std::fs::create_dir(&nested).unwrap();
    let marker = nested.join("phonefarm-lease.json");
    std::fs::write(&marker, r#"{"format":1,"recovery_required":true}"#).unwrap();
    let before = std::fs::read(&marker).unwrap();
    let blocked = command().output().unwrap();
    f.save("nested-blocked", &blocked);
    assert!(!blocked.status.success());
    assert_eq!(std::fs::read(f.dir.join("fake-calls")).unwrap(), calls);
    assert_eq!(std::fs::read(marker).unwrap(), before);

    // Invalid storage fails before the default 120-minute queue wait or any probe.
    let start = std::time::Instant::now();
    let invalid = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .env("DEVLOCK_DIR", "relative-invalid")
        .env("DEVLOCK_BIN", &devlock).env("ADB_BIN", &fake)
        .args(["preflight", "--fix", "--serial", "fake-device"]).output().unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(std::fs::read(f.dir.join("fake-calls")).unwrap(), calls);
}
