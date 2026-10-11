//! Real CLI subprocess tests. Every serial/bridge/tool is fake; never invoke adb/hdc.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "pf-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.current_dir(&self.dir)
            .env("PHONEFARM_LEASE_DIR", self.dir.join("leases"))
            .env("DEVLOCK_DIR", self.dir.join("legacy"))
            .env("PHONEFARM_LEASE_TIMEOUT_MS", "100")
            .env("ADB_BIN", self.dir.join("fake-adb"));
        c
    }
    fn legacy(&self) -> PathBuf {
        self.dir.join("legacy/devlock.d")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn wait_for(path: &Path) {
    let end = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "{}", path.display());
}
fn worker(f: &Fixture, label: &str, sec: &str, timeout: &str) -> Child {
    // Marker/event script executes on the host only. No phone or model calls.
    let script="import sys,time,pathlib; d=pathlib.Path(sys.argv[1]); l=sys.argv[2]; (d/(l+'.started')).write_text('yes'); p=d/'events'; p.open('a').write(l+' start\\n'); time.sleep(float(sys.argv[3])); p.open('a').write(l+' end\\n')";
    let cwd = f.dir.join(format!("cwd-{label}"));
    std::fs::create_dir_all(&cwd).unwrap();
    f.cmd()
        .current_dir(cwd)
        .args([
            "lease",
            "--serial",
            "fake-device",
            "--timeout-ms",
            timeout,
            "--",
            "python3",
            "-c",
            script,
        ])
        .arg(&f.dir)
        .args([label, sec])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}
fn shader_request() -> Value {
    json!({"version":2,"kind":"shader","candidate_id":"c","serial":"fake-device","package":"refbench",
    "workload":{"carrier":"refbench","script":"sr_pipeline","seconds":null,"frames":370,"intensity":3},
    "conditions":{"cool_c":42,"require_root":true,"require_discharging":true,"charging_suspend":{"enabled":true,"node":null},"inject":false},
    "protocol":{"rounds":4,"alternating":true,"p_threshold":0.05},
    "payload":{"track":"sr","shader_path":"candidate.glsl","spirv_path":"absent.spv","budget_ms":3,"quality_baseline_db":30}})
}
#[test]
fn two_real_processes_do_not_interleave_across_cwds() {
    let f = Fixture::new("exclusive");
    let mut a = worker(&f, "A", "0.3", "1000");
    wait_for(&f.dir.join("A.started"));
    let mut b = worker(&f, "B", "0.01", "1000");
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(f.dir.join("events")).unwrap(),
        "A start\nA end\nB start\nB end\n"
    );
    assert!(!f.legacy().exists());
}
#[test]
fn timeout_preserves_other_owner_and_failure_releases() {
    let f = Fixture::new("timeout");
    let mut a = worker(&f, "A", "0.3", "1000");
    wait_for(&f.dir.join("A.started"));
    let owner = std::fs::read(f.legacy().join("phonefarm-lease.json")).unwrap();
    let mut b = worker(&f, "B", "0", "20");
    assert_eq!(b.wait().unwrap().code(), Some(2));
    assert_eq!(
        std::fs::read(f.legacy().join("phonefarm-lease.json")).unwrap(),
        owner
    );
    assert!(a.wait().unwrap().success());
    let r = f
        .cmd()
        .args([
            "lease",
            "--serial",
            "fake-device",
            "--timeout-ms",
            "20",
            "--",
            "/this-command-does-not-exist",
        ])
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(!f.legacy().exists());
    let mut c = worker(&f, "C", "0", "50");
    assert!(c.wait().unwrap().success());
}
#[test]
fn crash_releases_kernel_but_abandoned_owner_blocks_live_child_and_unknown_device() {
    let f = Fixture::new("crash");
    let mut a = worker(&f, "A", "1.0", "1000");
    wait_for(&f.dir.join("A.started"));
    let marker = f.legacy().join("phonefarm-lease.json");
    let deadline = Instant::now() + Duration::from_secs(2);
    let owner = loop {
        let v: Value = serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        if v["child_pid"].is_u64() {
            break v;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(owner["pid"], a.id());
    a.kill().unwrap();
    a.wait().unwrap();
    // Underlying host kernel exclusion is released, while the semantic abandoned barrier stays.
    let bridge = std::fs::File::options()
        .read(true)
        .write(true)
        .open(f.dir.join("legacy/phonefarm-bridge.lock"))
        .unwrap();
    bridge.try_lock().unwrap();
    drop(bridge);
    let mut b = worker(&f, "B", "0", "20");
    assert_eq!(b.wait().unwrap().code(), Some(2));
    assert!(!f.dir.join("B.started").exists());
    assert!(f.legacy().exists());
    let recover = || {
        f.cmd()
            .args([
                "lease",
                "--serial",
                "fake-device",
                "--timeout-ms",
                "20",
                "--recover-abandoned",
                "--confirm-device-idle",
            ])
            .output()
            .unwrap()
    };
    let early = recover();
    assert_eq!(early.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&early.stderr).contains("child alive/unknown"));
    let child = owner["child_pid"].as_u64().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let p = Command::new("ps")
            .args(["-p", &child, "-o", "pid="])
            .output()
            .unwrap();
        if p.status.code() == Some(1) && p.stdout.is_empty() && p.stderr.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut c = worker(&f, "C", "0", "20");
    assert_eq!(c.wait().unwrap().code(), Some(2));
    let unconfirmed = f
        .cmd()
        .args(["lease", "--serial", "fake-device", "--recover-abandoned"])
        .output()
        .unwrap();
    assert_eq!(unconfirmed.status.code(), Some(2));
    assert!(f.legacy().exists());
    assert!(recover().status.success());
    assert!(!f.legacy().exists());
    let log: Value = serde_json::from_str(
        std::fs::read_to_string(f.dir.join("legacy/phonefarm-recovery.jsonl"))
            .unwrap()
            .trim(),
    )
    .unwrap();
    assert_eq!(log["device_verified"], false);
    let mut d = worker(&f, "D", "0", "100");
    assert!(d.wait().unwrap().success());
}
#[test]
fn unknown_manual_lock_is_never_deleted_even_when_old() {
    let f = Fixture::new("manual");
    std::fs::create_dir_all(f.legacy()).unwrap();
    std::fs::write(f.legacy().join("owner"), "someone since 2020-01-01\n").unwrap();
    std::fs::File::open(f.legacy().join("owner"))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    let mut b = worker(&f, "B", "0", "20");
    assert_eq!(b.wait().unwrap().code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(f.legacy().join("owner")).unwrap(),
        "someone since 2020-01-01\n"
    );
}
#[test]
fn actual_eval_route_writes_consumed_plan_and_honest_abort_without_phone() {
    let f = Fixture::new("v2");
    let req = shader_request();
    let path = f.dir.join("request.json");
    std::fs::write(&path, req.to_string()).unwrap();
    // Missing local SPIR-V aborts before any device/root/lease operation.
    let r = f
        .cmd()
        .args(["eval", "--request"])
        .arg(&path)
        .arg("--out")
        .arg(f.dir.join("out"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(report["status"], "ABORT");
    assert_eq!(report["conditions_observed"]["root_verified"], Value::Null);
    assert_eq!(report["resolved_config"]["carrier"], "refbench");
    assert_eq!(report["resolved_config"]["frames"], 370);
    assert_eq!(report["conditions_requested"], req["conditions"]);
    assert_eq!(report["qualification"]["eligible"], false);
    assert_eq!(report["measurement_scope"], "refbench_measurement_only");
    let adapted = std::fs::read_dir(f.dir.join("out"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("_eval_v1_request_")
        })
        .unwrap();
    let plan: Value = serde_json::from_slice(&std::fs::read(adapted).unwrap()).unwrap();
    assert_eq!(
        plan["v2_context"]["resolved_config"]["charging_suspend"]["enabled"],
        true
    );
    assert!(!f.legacy().exists());
}
#[test]
fn unsupported_injection_returns_specific_abort_before_artifact_or_device() {
    let f = Fixture::new("unsupported");
    let mut req = shader_request();
    req["conditions"]["inject"] = json!(true);
    let path = f.dir.join("request.json");
    std::fs::write(&path, req.to_string()).unwrap();
    let r = f
        .cmd()
        .args(["eval", "--request"])
        .arg(path)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(report["status"], "ABORT");
    assert!(report["verdict"]["reason"]
        .as_str()
        .unwrap()
        .contains("inject=true unsupported"));
    assert!(!f.legacy().exists());
}

#[test]
fn heartbeat_renews_and_live_or_replaced_legacy_owner_is_not_reclaimed() {
    let f = Fixture::new("heartbeat");
    let mut a = worker(&f, "A", "5.4", "1000");
    wait_for(&f.dir.join("A.started"));
    let initial = std::fs::metadata(f.legacy().join("owner"))
        .unwrap()
        .modified()
        .unwrap();
    let end = Instant::now() + Duration::from_millis(5250);
    while std::fs::metadata(f.legacy().join("owner"))
        .unwrap()
        .modified()
        .unwrap()
        == initial
        && Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        std::fs::metadata(f.legacy().join("owner"))
            .unwrap()
            .modified()
            .unwrap()
            > initial
    );
    // Foreign owner with our old marker must survive normal Drop and subsequent dead-PID recovery.
    std::fs::write(f.legacy().join("owner"), "new-manual-owner\n").unwrap();
    assert_eq!(a.wait().unwrap().code(), Some(2));
    let mut b = worker(&f, "B", "0", "20");
    assert_eq!(b.wait().unwrap().code(), Some(2));
    assert_eq!(
        std::fs::read_to_string(f.legacy().join("owner")).unwrap(),
        "new-manual-owner\n"
    );
}
#[test]
fn eval_sysparam_entry_respects_existing_manual_lease_before_device_access() {
    let f = Fixture::new("sysparam-held");
    std::fs::create_dir_all(f.legacy()).unwrap();
    std::fs::write(f.legacy().join("owner"), "external-task\n").unwrap();
    let mut req = shader_request();
    req["kind"] = json!("sysparam");
    req["payload"] = json!({"params":{}});
    req["workload"] = json!({"carrier":"game_replay","script":null,"seconds":30});
    let path = f.dir.join("request.json");
    std::fs::write(&path, req.to_string()).unwrap();
    let r = f
        .cmd()
        .args(["eval", "--request"])
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&r.stderr).contains("manual/legacy devlock busy"));
    assert_eq!(
        std::fs::read_to_string(f.legacy().join("owner")).unwrap(),
        "external-task\n"
    );
}

#[test]
fn invalid_pid_marker_and_live_owner_cannot_be_recovered() {
    for pid in [0, std::process::id()] {
        let f = Fixture::new("pid-unknown");
        std::fs::create_dir_all(f.legacy()).unwrap();
        let m = json!({"format":1,"producer":"phonefarm","pid":pid,"token":"t","serial":"fake-device","owner":"same\n"});
        std::fs::write(f.legacy().join("phonefarm-lease.json"), m.to_string()).unwrap();
        std::fs::write(f.legacy().join("owner"), "same\n").unwrap();
        let r = f
            .cmd()
            .args([
                "lease",
                "--serial",
                "fake-device",
                "--recover-abandoned",
                "--confirm-device-idle",
            ])
            .output()
            .unwrap();
        assert_eq!(r.status.code(), Some(2));
        assert!(f.legacy().exists());
    }
}

#[test]
fn gpuop_adapter_and_perf_cannot_write_through_a_manual_lock() {
    let f = Fixture::new("gpu-held");
    std::fs::create_dir_all(f.legacy()).unwrap();
    std::fs::write(f.legacy().join("owner"), "someone-else\n").unwrap();
    let words: [u32; 16] = [
        0x07230203,
        0x00010300,
        0,
        10,
        0,
        (5 << 16) | 15,
        5,
        1,
        0x6e69616d,
        0,
        (6 << 16) | 16,
        1,
        17,
        1,
        1,
        1,
    ];
    let spv = f.dir.join("local.spv");
    std::fs::write(
        &spv,
        words
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut req = shader_request();
    req["payload"]["spirv_path"] = json!(spv);
    let path = f.dir.join("request.json");
    std::fs::write(&path, req.to_string()).unwrap();
    let r = f
        .cmd()
        .args(["eval", "--request"])
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&r.stdout).unwrap();
    assert_eq!(report["status"], "ABORT");
    assert!(String::from_utf8_lossy(&r.stderr).contains("manual/legacy devlock busy"));
    assert_eq!(report["conditions_observed"]["root_verified"], Value::Null);
    let r = f
        .cmd()
        .args(["perf", "--serial", "fake-device", "--suspend-charging"])
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&r.stderr).contains("manual/legacy devlock busy"));
    assert_eq!(
        std::fs::read_to_string(f.legacy().join("owner")).unwrap(),
        "someone-else\n"
    );
}
