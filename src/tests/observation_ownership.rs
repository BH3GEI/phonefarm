//! Real supervisor/lease processes with fake transports; never discovers a real device.
#![cfg(unix)]
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pf-observation-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir(root.join("locks")).unwrap();
        script(&root.join("bin/hdc"), "exit 0");
        script(
            &root.join("bin/adb"),
            r#"
if [ "$1" = devices ]; then
  : > "$FIXTURE/started"
  while [ -f "$FIXTURE/hold" ]; do /bin/sleep 0.02; done
  printf 'List of devices attached\n'
  [ ! -f "$FIXTURE/slow-shell" ] || printf 'fake-observer device model:Fixture\n'
  exit 0
fi
if [ -f "$FIXTURE/slow-shell" ]; then exec /bin/sleep 30; fi
exit 0
"#,
        );
        Self(root)
    }
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        cmd.env("DEVLOCK_DIR", self.0.join("locks"))
            .env("PHONEFARM_LEASE_DIR", self.0.join("leases"))
            .env("ADB_BIN", self.0.join("bin/adb"))
            .env("PATH", self.0.join("bin"))
            .env("FIXTURE", &self.0)
            .env_remove("DEVLOCK_LABEL")
            .env_remove("PF_JOB_ID")
            .env_remove("PF_JOB_LEASE_DIR");
        cmd
    }
    fn fleet(&self) -> std::process::Output {
        self.cmd().args(["fleet", "--json"]).output().unwrap()
    }
    fn status(&self) -> Value {
        serde_json::from_slice(
            &self
                .cmd()
                .args(["lease", "status", "--json"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
    }
    fn marker(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.0.join("locks/devlock.d/phonefarm-lease.json")).unwrap(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "fixture did not progress");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn sampling_window_excludes_strong_and_legacy_writers_then_releases() {
    let f = Fixture::new();
    fs::write(f.0.join("hold"), "").unwrap();
    let sampler = f
        .cmd()
        .args(["fleet", "--json"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    until(|| f.0.join("started").exists());
    assert_eq!(f.status()["status"], "held");
    let competing = f
        .cmd()
        .args([
            "lease",
            "--serial",
            "fake-writer",
            "--timeout-ms",
            "0",
            "--",
            "/usr/bin/true",
        ])
        .output()
        .unwrap();
    assert!(!competing.status.success());
    if Command::new("zsh").arg("-c").arg("exit 0").status().is_ok() {
        let legacy = Command::new("zsh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/devlock"))
            .args(["acquire", "writer"])
            .env("DEVLOCK_DIR", f.0.join("locks"))
            .output()
            .unwrap();
        assert_eq!(legacy.status.code(), Some(1));
    }
    fs::remove_file(f.0.join("hold")).unwrap();
    let output = sampler.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["observation"], "sampled");
    assert_eq!(result["devices"], serde_json::json!([]));
    assert_eq!(f.status()["status"], "clear");
}

#[test]
fn queued_writers_unknown_records_and_same_label_never_grant_observation() {
    let f = Fixture::new();
    for entry in [
        "devlock.queue",
        "devlock.qmutex",
        "phonefarm-lease.d",
        "devlock.d",
    ] {
        let path = f.0.join("locks").join(entry);
        if entry == "devlock.queue" {
            fs::write(&path, "writer\n").unwrap();
        } else {
            fs::create_dir(&path).unwrap();
        }
        if entry == "devlock.d" {
            fs::write(path.join("owner"), "same since fixture\n").unwrap();
        }
        let output = f
            .cmd()
            .env("DEVLOCK_LABEL", "same")
            .args([
                "fleet",
                "--json",
                "--lock-label",
                "same",
                "--probe-while-locked",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3));
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["observation"], "deferred");
        assert!(result["devices"].is_null());
        assert!(!f.0.join("started").exists());
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    let direct = f
        .cmd()
        .args(["__observe-worker", "fleet", "--json"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(direct.status.code(), Some(90));
    assert!(!f.0.join("started").exists());
}

#[test]
fn transport_timeout_retains_uncertain_lease_instead_of_fresh_empty_sample() {
    let f = Fixture::new();
    fs::write(f.0.join("slow-shell"), "").unwrap();
    let output = f
        .cmd()
        .args(["fleet", "--json", "--timeout-ms", "30"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["observation"], "error");
    assert!(result["devices"].is_null());
    assert!(f.marker()["child_pgid"].as_u64().is_some());
    assert_ne!(f.status()["status"], "clear");
    assert_eq!(
        serde_json::from_slice::<Value>(&f.fleet().stdout).unwrap()["observation"],
        "deferred"
    );
}

#[test]
fn killed_supervisor_leaves_worker_identity_and_blocks_new_sampling() {
    let f = Fixture::new();
    fs::write(f.0.join("hold"), "").unwrap();
    let mut supervisor = f
        .cmd()
        .args(["fleet", "--json"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    until(|| f.0.join("started").exists());
    let group = f.marker()["child_pgid"].as_i64().unwrap() as i32;
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    assert_ne!(f.status()["status"], "clear");
    assert_eq!(
        serde_json::from_slice::<Value>(&f.fleet().stdout).unwrap()["observation"],
        "deferred"
    );
    until(|| unsafe { libc::kill(-group, 0) } < 0);
    assert!(
        f.0.join("hold").exists(),
        "parent death must stop even a stuck transport"
    );
    assert_ne!(
        f.status()["status"],
        "clear",
        "worker exit cannot acknowledge remote idle/restoration"
    );
}
