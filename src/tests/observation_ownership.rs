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
        if std::thread::panicking() {
            eprintln!("failed observation fixture retained: {}", self.0.display());
        } else {
            let _ = fs::remove_dir_all(&self.0);
        }
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

#[test]
fn bound_screen_samples_another_device_without_widening_its_worker() {
    let f = Fixture::new();
    let json_call = |args: &[&str]| -> Value {
        let output = f.cmd().args(args).output().unwrap();
        assert!(output.status.success(), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice(&output.stdout).unwrap()
    };
    assert_eq!(json_call(&["screen", "capabilities", "--json"])["device_binding_v1"], true);
    json_call(&["lease", "domain-init", "--participants-stopped", "--json"]);
    for (revision, id, aliases) in [(0, "A", vec!["screen-A"]), (1, "B", vec!["screen-B", "screen-B-wifi"])] {
        let request = f.0.join("register.json");
        fs::write(&request, serde_json::json!({"expected_revision":revision,
            "device":{"device_id":id,"label":id,"aliases":aliases,"enabled":true}}).to_string()).unwrap();
        json_call(&["lease", "devices-set", "--request", request.to_str().unwrap(), "--json"]);
    }
    let a = json_call(&["lease", "resolve", "--serial", "screen-A", "--json"]);
    let b = json_call(&["lease", "resolve", "--serial", "screen-B", "--json"]);
    let alias = json_call(&["lease", "resolve", "--serial", "screen-B-wifi", "--json"]);
    let png = f.0.join("screen.png");
    image::RgbImage::from_fn(128, 128, |x, y| image::Rgb([
        (x * 31 + y * 17) as u8, (x * 13 ^ y * 43) as u8, (x * y + y * 7) as u8,
    ])).save(&png).unwrap();
    assert!(fs::metadata(&png).unwrap().len() > 1000);
    script(&f.0.join("bin/adb"), r#"
[ "$1" = '-s' ] || exit 91
serial=$2; shift 2
printf '%s %s\n' "$serial" "$*" >> "$FIXTURE/calls"
case "$*" in
  'shell wm size') printf 'Physical size: 128x128\n' ;;
  'exec-out screencap -p')
    if [ "$serial" = screen-B ]; then
      : > "$FIXTURE/started-B"
      ticks=0
      while [ -f "$FIXTURE/hold-B" ] && [ "$ticks" -lt 500 ]; do
        /bin/sleep 0.02; ticks=$((ticks + 1))
      done
      [ ! -f "$FIXTURE/hold-B" ] || exit 92
    fi
    /bin/cat "$FIXTURE/screen.png" ;;
  *) exit 93 ;;
esac
"#);
    let capture = |serial: &str, binding: Option<&Value>, name: &str| {
        let mut cmd = f.cmd();
        cmd.args(["screen", "capture", "--serial", serial, "--out"])
            .arg(f.0.join(name)).arg("--json");
        if let Some(binding) = binding { cmd.arg("--device-binding").arg(binding.to_string()); }
        cmd
    };
    // The only concurrent child is owned here and bounded by the observation worker.
    struct Owned(Option<std::process::Child>);
    impl Drop for Owned {
        fn drop(&mut self) { if let Some(child) = &mut self.0 { let _ = child.kill(); let _ = child.wait(); } }
    }
    fs::write(f.0.join("hold-B"), "").unwrap();
    let mut held = Owned(Some(capture("screen-B", Some(&b), "B").stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()));
    until(|| f.0.join("started-B").exists());
    let status = f.status();
    let entry = status["entries"].as_array().unwrap().iter().find(|entry| entry["serial"] == "screen-B").unwrap();
    let marker: Value = serde_json::from_slice(&fs::read(entry["marker_path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(marker["device_binding"], b);
    assert_eq!(marker["serial"], "screen-B");
    assert_eq!(marker["pid"], held.0.as_ref().unwrap().id());
    assert_eq!(marker["child_pid"], marker["child_pgid"]);
    let worker = marker["child_pid"].as_i64().unwrap() as i32;
    let output = capture("screen-A", Some(&a), "A").output().unwrap();
    assert!(output.status.success(), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["device_binding"], a);
    assert_eq!(result["lease_scope"], "device");
    assert_eq!(result["serial"], "screen-A");
    assert!(f.0.join("A/capture.jpg").is_file());
    let before = fs::read(f.0.join("calls")).unwrap();
    let alias_output = capture("screen-B-wifi", Some(&alias), "alias").output().unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&alias_output.stdout).unwrap()["state"], "deferred");
    assert_eq!(serde_json::from_slice::<Value>(&f.fleet().stdout).unwrap()["observation"], "deferred");
    assert_eq!(fs::read(f.0.join("calls")).unwrap(), before, "refused observations must not reach ADB");
    fs::remove_file(f.0.join("hold-B")).unwrap();
    let output = held.0.take().unwrap().wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap()["device_binding"], b);
    assert!(unsafe { libc::kill(-worker, 0) } < 0, "owned worker group must have exited");
    assert_eq!(f.status()["status"], "clear");
    let output = capture("screen-A", None, "legacy").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let legacy: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(legacy.get("device_binding").is_none() && legacy.get("lease_scope").is_none());
    assert_eq!(f.status()["status"], "clear");
    println!("scoped screen A completed while B held; alias/fleet made no transport; worker PGID {worker} gone; legacy global capture released");
}
