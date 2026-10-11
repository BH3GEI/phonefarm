//! Real CLI boundary tests: no phone, no model, no secret-bearing environment.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "phonefarm-qualification-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, name: &str, v: &Value) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, serde_json::to_vec(v).unwrap()).unwrap();
        p
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cmd() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
    c.current_dir(std::env::temp_dir());
    c
}
fn fixture() -> Value {
    // Hypothetical canonical collector metadata exercises logic; never device validation.
    let mut e: Value =
        serde_json::from_str(include_str!("../../examples/qualification_ready.json")).unwrap();
    e["evidence_origin"] = json!("recorded_device");
    for arm in ["baseline", "control", "candidate"] {
        for r in e[arm].as_array_mut().unwrap() {
            r["frame_evidence"]["source"] = json!("raw_ftrace_kgsl");
        }
    }
    e
}
fn report(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}
fn request() -> Value {
    json!({"version":2,"kind":"sysparam","candidate_id":"offline-boundary","package":"com.example.game","workload":{"carrier":"game_replay","script":null,"seconds":30},"conditions":{"cool_c":40,"temp_cap_c":60},"protocol":{"rounds":5,"alternating":true,"p_threshold":0.05},"payload":{"params":{}}})
}
#[test]
fn qualify_replays_same_evidence_and_blocks_display_proxy() {
    let t = Scratch::new();
    let p = t.write("evidence.json", &fixture());
    let run = || {
        cmd()
            .args(["qualify", "--from"])
            .arg(&p)
            .arg("--json")
            .output()
            .unwrap()
    };
    let a = run();
    let b = run();
    assert!(a.status.success());
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(report(&a)["eligible"], true);
    let mut e = fixture();
    e["policy"]["goal"] = json!("display_smoothness");
    e["policy"]["required_metrics"] = json!(["frame_p95", "fps_mean"]);
    let p = t.write("display.json", &e);
    let o = cmd().args(["qualify", "--from"]).arg(p).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(report(&o)["eligible"], false);
}
#[test]
fn actual_eval_offline_route_freezes_target_without_optimization_pass() {
    let t = Scratch::new();
    let req = t.write("request.json", &request());
    let e = t.write("evidence.json", &fixture());
    let o = cmd()
        .args(["eval", "--request"])
        .arg(&req)
        .arg("--qualify-from")
        .arg(&e)
        .arg("--json")
        .output()
        .unwrap();
    assert!(o.status.success());
    let r = report(&o);
    assert_eq!(r["qualification_only"], true);
    assert_eq!(r["qualification"]["eligible"], true);
    assert_eq!(r["status"], "ABORT");
    assert_eq!(r["verdict"]["is_pareto_improvement"], false);
    let mut input = fixture();
    input["candidate"][0]["env"]["power_w_mean"] = Value::Null;
    let e = t.write("missing-power.json", &input);
    let o = cmd()
        .args(["eval", "--request"])
        .arg(req)
        .arg("--qualify-from")
        .arg(e)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(report(&o)["qualification"]["eligible"], false);
}
#[cfg(unix)]
fn fake_adb(t: &Scratch) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = t.0.join("adb");
    std::fs::write(&p,"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$QUALIFICATION_ADB_LOG\"\ncase \"$*\" in\n  *get-state*) echo device;;\n  *'su -c id'*) echo 'uid=2000(shell) gid=2000(shell)';;\n  *) exit 0;;\nesac\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}
#[cfg(unix)]
fn assert_only_read_root(log: &Path) {
    let s = std::fs::read_to_string(log).unwrap();
    assert!(s.contains("su -c id"));
    assert!(!s.contains("push"));
    assert!(!s.contains("probe_sysparam"));
    assert!(!s.contains("charge_suspend"));
    assert!(!s.contains("knob_sysparam"));
}
#[cfg(unix)]
#[test]
fn actual_eval_nonroot_stops_before_push_or_writable_probe() {
    let t = Scratch::new();
    let req = t.write("request.json", &request());
    let adb = fake_adb(&t);
    let log = t.0.join("adb.log");
    let o = cmd()
        .args(["eval", "--request"])
        .arg(req)
        .args(["--serial", "fixture-device", "--out"])
        .arg(t.0.join("out"))
        .env("ADB_BIN", adb)
        .env("QUALIFICATION_ADB_LOG", &log)
        .output()
        .unwrap();
    assert!(o.status.success());
    let r = report(&o);
    assert_eq!(r["status"], "ABORT");
    assert_eq!(r["qualification"]["eligible"], false);
    assert_eq!(r["qualification"]["probe_allowed"], false);
    assert_only_read_root(&log);
}
#[cfg(unix)]
#[test]
fn actual_autoloop_nonroot_stops_before_probe_or_model() {
    let t = Scratch::new();
    let adb = fake_adb(&t);
    let log = t.0.join("adb.log");
    let out = t.0.join("out");
    let o = cmd()
        .args(["autoloop", "--serial", "fixture-device", "--out"])
        .arg(&out)
        .env("ADB_BIN", adb)
        .env("QUALIFICATION_ADB_LOG", &log)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_only_read_root(&log);
    let q: Value =
        serde_json::from_slice(&std::fs::read(out.join("qualification.json")).unwrap()).unwrap();
    assert_eq!(q["eligible"], false);
    assert!(!out.join("gen1").exists());
}

#[test]
fn published_synthetic_fixture_cannot_qualify_real_claim() {
    let t = Scratch::new();
    let e: Value =
        serde_json::from_str(include_str!("../../examples/qualification_ready.json")).unwrap();
    let p = t.write("synthetic.json", &e);
    let o = cmd().args(["qualify", "--from"]).arg(p).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let r = report(&o);
    assert_eq!(r["eligible"], false);
    assert_eq!(r["evidence_origin"], "synthetic");
}
#[test]
fn eval_refuses_generator_relaxing_alpha() {
    let t = Scratch::new();
    let mut r = request();
    r["protocol"]["p_threshold"] = json!(1.0);
    let p = t.write("bad-alpha.json", &r);
    let e = t.write("e.json", &fixture());
    let o = cmd()
        .args(["eval", "--request"])
        .arg(p)
        .arg("--qualify-from")
        .arg(e)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn actual_probe_failed_restore_cannot_report_reverted() {
    use std::os::unix::fs::PermissionsExt;
    let t = Scratch::new();
    let p = t.0.join("adb-root-fixture");
    let log = t.0.join("adb.log");
    let counter = t.0.join("snapshot.count");
    let stub = r#"#!/bin/sh
printf '%s\n' "$*" >> "$QUALIFICATION_ADB_LOG"
case "$*" in
 *'su -c id'*) echo 'uid=0(root) gid=0(root)';;
 *TRACE_AVAILABLE*) echo TRACE_AVAILABLE;;
 *device_snapshot.sh*)
   n=0; [ -f "$QUALIFICATION_SNAPSHOT_COUNT" ] && n=$(cat "$QUALIFICATION_SNAPSHOT_COUNT")
   n=$((n+1)); printf '%s\n' "$n" > "$QUALIFICATION_SNAPSHOT_COUNT"
   echo "cpu.policy0.scaling_min=$n";;
 *probe_sysparam.sh*)
   echo cpu.policies=0
   echo 'cpu.policy0.avail_freqs=300000 1000000'
   echo cpu.policy0.scaling_min_freq.cur=300000
   echo cpu.policy0.scaling_min_freq.writable=yes
   echo cpu.policy0.scaling_min_freq.effect=live;;
 *) exit 0;;
esac
"#;
    std::fs::write(&p, stub).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut r = request();
    r["payload"]["probe_only"] = json!(true);
    let req = t.write("probe.json", &r);
    let o = cmd()
        .args(["eval", "--request"])
        .arg(req)
        .args(["--serial", "fixture-device", "--out"])
        .arg(t.0.join("out"))
        .env("ADB_BIN", p)
        .env("QUALIFICATION_ADB_LOG", &log)
        .env("QUALIFICATION_SNAPSHOT_COUNT", counter)
        .output()
        .unwrap();
    assert!(o.status.success());
    let r = report(&o);
    assert_eq!(r["status"], "ABORT");
    assert_eq!(r["conditions_observed"]["restored"], false);
    assert_eq!(r["qualification"]["status"], "blocked");
    assert_eq!(r["qualification"]["probe_allowed"], false);
}
