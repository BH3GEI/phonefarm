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

#[test]
fn gate_admission_errors_are_json_without_relabeling_dispatched_capture() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    use std::time::{Duration, Instant};

    let f = Fixture::new("gate-admission");
    let script = f.dir.join("fake-devlock");
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s:%s:%s\n' "$1" "$2" "${DEVLOCK_INSTANCE_ID-}" >> "$GATE_CALLS"
case "$GATE_CASE:$1" in
  unavailable:acquire) echo 'fixture queue unavailable' >&2; exit 2 ;;
  busy:acquire) echo 'fixture occupied'; exit 1 ;;
  inherited_rejected:renew) echo 'fixture identity rejected' >&2; exit 2 ;;
  bad_owner:acquire) printf 'owned\n' > "$GATE_OWNER"; holder=0 ;;
  success:acquire) holder=$PPID ;;
  success:renew|success:release) exit 0 ;;
  *) exit 2 ;;
esac
printf '{"schema":"phonefarm/devlock-owner@1","instance_id":"0123456789abcdef0123456789abcdef","label":"%s","holder_pid":%s,"holder_start":"fixture","host_boot_id":"fixture","owner":"fixture"}\n' "$2" "$holder"
"#,
    )
    .unwrap();
    let transport = f.dir.join("forbidden-transport");
    std::fs::write(
        &transport,
        "#!/bin/sh\nprintf 'called\\n' >> \"$GATE_DEVICE_CALLS\"\nexit 91\n",
    )
    .unwrap();
    for path in [&script, &transport] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let command = |name: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.env_clear()
            .current_dir(&f.dir)
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &f.dir)
            .env("PHONEFARM_STATE_DIR", f.dir.join("state"))
            .env("DEVLOCK_DIR", f.dir.join("legacy"))
            .env("PHONEFARM_LEASE_DIR", f.dir.join("leases"))
            .env("DEVLOCK_BIN", &script)
            .env("ADB_BIN", &transport)
            .env("HDC_BIN", &transport)
            .env("GATE_CALLS", f.dir.join(format!("{name}.calls")))
            .env("GATE_OWNER", f.dir.join("invalid-owner-retained"))
            .env("GATE_DEVICE_CALLS", f.dir.join("device.calls"))
            // No --json: admission is always structured. A permitted command
            // fails capture's argument validation before any device access.
            .args([
                "capture", "--serial", "fake-device", "--frames", "0",
                "--lock-label", "fixture", "--lock-wait", "0s",
            ]);
        c
    };
    let run = |name: &str, mut command: Command| {
        let mut child = command
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let owned = OwnedGroup(child.id() as i32);
        let started = Instant::now();
        let mut expired = false;
        while child.try_wait().unwrap().is_none() {
            if started.elapsed() >= Duration::from_secs(5) {
                expired = true;
                unsafe { libc::kill(-owned.0, libc::SIGKILL); }
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        f.save(name, &output);
        drop(owned);
        assert!(!expired, "{name}: fixture exceeded its five-second bound");
        output
    };
    let calls = |name: &str| {
        std::fs::read_to_string(f.dir.join(format!("{name}.calls"))).unwrap_or_default()
    };
    let instance = "0123456789abcdef0123456789abcdef";
    for (name, mode, code, expected_call) in [
        ("arguments", "success", "invalid_lock_arguments", ""),
        ("storage", "success", "lease_storage_unavailable", ""),
        ("unavailable", "unavailable", "devlock_unavailable", "acquire"),
        ("inherited", "inherited_rejected", "inherited_instance_unverified", "renew"),
        ("busy", "busy", "devlock_wait_expired", "acquire"),
        ("owner", "bad_owner", "devlock_owner_invalid", "acquire"),
    ] {
        let mut c = command(name);
        c.env("GATE_CASE", mode);
        match name {
            "arguments" => { c.arg("--lock-wait"); }
            "storage" => { c.env("DEVLOCK_DIR", "relative-invalid"); }
            "inherited" => { c.env("DEVLOCK_INSTANCE_ID", instance); }
            _ => {}
        }
        let output = run(name, c);
        assert_eq!(output.status.code(), Some(2), "{name}");
        let stdout = std::str::from_utf8(&output.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "{name}: {stdout}");
        let value: Value = serde_json::from_str(stdout).unwrap();
        assert_eq!(value["schema"], "phonefarm/devlock-error@1");
        assert_eq!(value["stage"], "admission");
        assert_eq!(value["code"], code);
        assert_eq!(value["command"], "capture");
        assert_eq!(value["command_started"], false);
        assert_eq!(value["exit_code"], 2);
        assert!(value["error"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(!output.stderr.is_empty(), "{name}: retain human diagnostic");
        let expected = match expected_call {
            "" => String::new(),
            "renew" => format!("renew:fixture:{instance}\n"),
            _ => "acquire:fixture:\n".to_string(),
        };
        assert_eq!(calls(name), expected, "{name}: no retry or release on rejection");
    }
    assert_eq!(std::fs::read(f.dir.join("invalid-owner-retained")).unwrap(), b"owned\n");

    for inherited in [true, false] {
        let name = if inherited { "inherited-dispatch" } else { "child-dispatch" };
        let mut c = command(name);
        c.env("GATE_CASE", "success");
        if inherited {
            c.env("DEVLOCK_INSTANCE_ID", instance);
        }
        let output = run(name, c);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty(), "{name}: producer error is not admission");
        assert!(String::from_utf8_lossy(&output.stderr).contains("--frames 必须 >= 1"));
        let expected = if inherited {
            format!("renew:fixture:{instance}\n")
        } else {
            format!("acquire:fixture:\nrenew:fixture:{instance}\nrelease:fixture:{instance}\n")
        };
        assert_eq!(calls(name), expected);
    }
    assert!(!f.dir.join("device.calls").exists());
    assert!(!f.dir.join("legacy").exists());
    assert!(!f.dir.join("leases").exists());
}
