//! Execute the actual heldout driver through explicit synthetic transport. No device/API calls.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
static CLI_RECEIPT: AtomicU64 = AtomicU64::new(0);
fn sha(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
struct F {
    dir: PathBuf,
}
impl F {
    fn new(all: bool) -> Self {
        let base = std::env::var_os("PHONEFARM_DRIVER_TEST_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!(
            "confirmation-driver-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // The driver canonicalizes its session. Keep independent producer inputs
        // on the same root (/var and /private/var are aliases on macOS).
        let dir = std::fs::canonicalize(dir).unwrap();
        std::fs::write(
            dir.join("fake-adb"),
            include_str!("support/fake_confirmation_adb.py"),
        )
        .unwrap();
        std::fs::write(
            dir.join("trace_fixture.txt"),
            include_str!("../../loop_v1/fixtures/trace_slice.txt"),
        )
        .unwrap();
        let mut params = vec![(
            "bus.DDR.boost_freq",
            "300",
            "sysfs",
            "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq",
        )];
        if all {
            params.extend([
                (
                    "bus.LLCC.boost_freq",
                    "300",
                    "sysfs",
                    "/sys/devices/system/cpu/bus_dcvs/LLCC/boost_freq",
                ),
                (
                    "cpu.policy0.scaling_min_freq",
                    "300",
                    "sysfs",
                    "/sys/devices/system/cpu/cpufreq/policy0/scaling_min_freq",
                ),
                (
                    "cpu.policy0.scaling_max_freq",
                    "600",
                    "sysfs",
                    "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq",
                ),
                (
                    "cpu.policy0.scaling_governor",
                    "performance",
                    "sysfs",
                    "/sys/devices/system/cpu/cpufreq/policy0/scaling_governor",
                ),
                (
                    "gpu.devfreq.min_freq",
                    "300",
                    "sysfs",
                    "/sys/class/kgsl/kgsl-3d0/devfreq/min_freq",
                ),
                (
                    "gpu.devfreq.max_freq",
                    "600",
                    "sysfs",
                    "/sys/class/kgsl/kgsl-3d0/devfreq/max_freq",
                ),
                (
                    "gpu.devfreq.governor",
                    "performance",
                    "sysfs",
                    "/sys/class/kgsl/kgsl-3d0/devfreq/governor",
                ),
                (
                    "gpu.min_pwrlevel",
                    "3",
                    "sysfs",
                    "/sys/class/kgsl/kgsl-3d0/min_pwrlevel",
                ),
                (
                    "gpu.max_pwrlevel",
                    "1",
                    "sysfs",
                    "/sys/class/kgsl/kgsl-3d0/max_pwrlevel",
                ),
                (
                    "setting.system.peak_refresh_rate",
                    "60",
                    "setting",
                    "system:peak_refresh_rate",
                ),
                (
                    "setting.system.min_refresh_rate",
                    "60",
                    "setting",
                    "system:min_refresh_rate",
                ),
                (
                    "setting.system.refresh_rate_mode",
                    "1",
                    "setting",
                    "system:refresh_rate_mode",
                ),
            ]);
        }
        params.sort_by_key(|(id, _, _, _)| {
            (
                !(id.ends_with("max_freq")
                    || id.ends_with("max_pwrlevel")
                    || id.contains("peak_refresh")),
                id.to_string(),
            )
        });
        let mut values = json!({});
        let mut plan = vec![];
        for (id, val, kind, path) in params {
            values[id] = json!(val);
            plan.push(json!({"kind":kind,"path":path,"value":val}));
        }
        let f = Self { dir };
        f.write(
            "candidate.json",
            &json!({"kind":"sysparam","payload":{"params":values,"plan":plan,"deny_keywords":[]}}),
        );
        f.write("workload.json",&json!({"schema":"phonefarm/deterministic_touch_workload@1","actions":[{"type":"tap","x":100,"y":100},{"type":"swipe","x1":100,"y1":100,"x2":500,"y2":100,"duration_ms":10},{"type":"sleep","seconds":6}]}));
        f.write("request.json",&json!({"schema":"phonefarm/confirmation_driver_request@1","transport":{"kind":"fake","program":f.dir.join("fake-adb"),"target":"fake-target"},"package":"com.example.game","render_comm":"UnityGfxDeviceW","capture_seconds":2,"lead_seconds":1,"workload":f.dir.join("workload.json"),"candidate":f.dir.join("candidate.json"),"paired_t_assumptions_acknowledged":true,"schedule":(0..5).map(|i|json!({"pair_id":format!("p{i}"),"a_run_id":format!("a{i}"),"b_run_id":format!("b{i}"),"baseline_first":i%2==0})).collect::<Vec<_>>()}));
        f
    }
    fn write(&self, n: &str, v: &Value) {
        std::fs::write(self.dir.join(n), serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }
    fn read(&self, n: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.dir.join(n)).unwrap()).unwrap()
    }
    fn command(&self, args: &[&str], fail: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.arg("confirmation-driver")
            .args(args)
            .current_dir(&self.dir)
            .env("PHONEFARM_FAKE_DEVICE_DIR", &self.dir)
            .env("PHONEFARM_LEASE_DIR", self.dir.join("lease"))
            .env("DEVLOCK_DIR", self.dir.join("legacy"))
            .env("PHONEFARM_LEASE_TIMEOUT_MS", "100")
            .env("PHONEFARM_FAKE_FAIL", fail);
        c
    }
    fn call(&self, args: &[&str], fail: &str) -> (i32, Value) {
        let mut command = self.command(args, fail);
        let o = command.output().unwrap();
        self.receipt(args[0], &command, &o);
        let v = serde_json::from_slice(&o.stdout).unwrap_or_else(|_| {
            panic!(
                "{} {}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        });
        (o.status.code().unwrap_or(-1), v)
    }
    fn receipt(&self, label: &str, command: &Command, output: &std::process::Output) {
        let prefix = format!(
            "host-cli-{}-{label}",
            CLI_RECEIPT.fetch_add(1, Ordering::Relaxed)
        );
        std::fs::write(self.dir.join(format!("{prefix}.stdout")), &output.stdout).unwrap();
        std::fs::write(self.dir.join(format!("{prefix}.stderr")), &output.stderr).unwrap();
        let argv = std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|v| v.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let environment = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|s| s.to_string_lossy().into_owned()),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        self.write(&format!("{prefix}.exit.json"), &json!({"argv":argv,
            "cwd":command.get_current_dir(),"environment_overrides":environment,"exit":output.status.code()}));
    }
    fn prepare(&self, fail: &str) -> (i32, Value) {
        self.call(
            &["prepare", "--request", "request.json", "--out", "session"],
            fail,
        )
    }
    fn freeze(&self) {
        // Exact typed order is supplied by actual GOL when env points to it; portable fallback mirrors its contract.
        if let Some(bin) = std::env::var_os("PHONEFARM_DRIVER_GOL_BIN") {
            let mut command = Command::new(bin);
            command
                .args(["confirm", "freeze", "--plan"])
                .arg(self.dir.join("session/plan.json"))
                .arg("--out")
                .arg(self.dir.join("frozen.json"));
            let o = command.output().unwrap();
            self.receipt("gol-freeze", &command, &o);
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            return;
        }
        let p = self.read("session/plan.json");
        let registered = chrono::Utc::now().to_rfc3339();
        let names = [
            "baseline_artifact_sha256",
            "candidate_artifact_sha256",
            "conditions_sha256",
            "baseline_admission_sha256",
            "exploration_run_ids",
            "exploration_artifact_sha256",
            "pairs",
            "schedule",
            "family_alpha",
            "paired_t_assumptions_acknowledged",
            "metrics",
        ];
        let plantext = format!(
            "{{{}}}",
            names
                .iter()
                .map(|n| format!("{}:{}", serde_json::to_string(n).unwrap(), p[*n]))
                .collect::<Vec<_>>()
                .join(",")
        );
        // Nested structs also have a fixed field order.
        let schedule = p["schedule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{{\"pair_id\":{},\"a_run_id\":{},\"b_run_id\":{},\"baseline_first\":{}}}",
                    s["pair_id"], s["a_run_id"], s["b_run_id"], s["baseline_first"]
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let metrics=p["metrics"].as_array().unwrap().iter().map(|m|format!("{{\"name\":{},\"direction\":{},\"scale\":{},\"minimum_improvement\":{},\"maximum_regression\":{}}}",m["name"],m["direction"],m["scale"],m["minimum_improvement"],m["maximum_regression"])).collect::<Vec<_>>().join(",");
        let plantext = plantext
            .replace(&p["schedule"].to_string(), &format!("[{schedule}]"))
            .replace(&p["metrics"].to_string(), &format!("[{metrics}]"));
        let hash = sha(format!(
            "[\"game_opt_loop/confirmation_plan@1\",{},{}]",
            serde_json::to_string(&registered).unwrap(),
            plantext
        )
        .as_bytes());
        self.write("frozen.json",&json!({"schema":"game_opt_loop/confirmation_plan@1","registered_at":registered,"plan":p,"sha256":hash}));
    }
    fn collect(&self) -> (i32, Value) {
        self.call(
            &["collect", "--session", "session", "--frozen", "frozen.json"],
            "",
        )
    }
    fn delayed_old_worker(&self) -> std::process::Output {
        let log = std::fs::read_to_string(self.dir.join("argv.jsonl")).unwrap();
        let old: Value = log
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| {
                v["command"].as_str().unwrap().contains("/worker.sh ")
                    && v["command"].as_str().unwrap().contains("'probe'")
            })
            .unwrap();
        Command::new("python3")
            .arg(self.dir.join("fake-adb"))
            .args(
                old["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap()),
            )
            .env("PHONEFARM_FAKE_DEVICE_DIR", &self.dir)
            .env("PHONEFARM_FAKE_FAIL", "")
            .output()
            .unwrap()
    }
}
impl Drop for F {
    fn drop(&mut self) {
        if std::env::var_os("PHONEFARM_DRIVER_TEST_ARTIFACT_DIR").is_none() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}
#[test]
fn complete_actual_driver_all_control_categories_retains_synthetic_and_cannot_replay() {
    let f = F::new(true);
    let (code, v) = f.prepare("");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "SIMULATED_BLOCKED");
    assert!(!f.dir.join("legacy/devlock.d").exists());
    let cmd = std::fs::read_to_string(f.dir.join("argv.jsonl")).unwrap();
    assert!(cmd.contains("setsid sh"));
    assert_eq!(
        std::fs::read_to_string(f.dir.join("session/probe.txt"))
            .unwrap()
            .matches("effect_restored=yes")
            .count(),
        13
    );
    for i in 0..3 {
        let r = f.read(&format!("session/admission_{i}-run.json"));
        assert_eq!(r["evidence_origin"], "synthetic");
        let validation = f.read(&format!("session/admission_{i}-validation.json"));
        assert_eq!(validation["valid"], false);
        assert!(validation["gates"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| *v == true));
    }
    // Changing original caller files after prepare cannot alter the frozen workload/candidate execution.
    f.write("candidate.json", &json!({"mutated":true}));
    f.write("workload.json", &json!({"mutated":true}));
    f.freeze();
    // A self-consistent but incomplete hash map must not bypass frozen remote-tool coverage.
    let original_inputs = std::fs::read(f.dir.join("session/inputs.manifest.json")).unwrap();
    let original_prepared = std::fs::read(f.dir.join("session/prepared.manifest.json")).unwrap();
    let original_argv = std::fs::read(f.dir.join("argv.jsonl")).unwrap();
    let mut inputs = f.read("session/inputs.manifest.json");
    inputs["hashes"]
        .as_object_mut()
        .unwrap()
        .remove("worker.sh");
    f.write("session/inputs.manifest.json", &inputs);
    let mut prepared = f.read("session/prepared.manifest.json");
    prepared["hashes"]["inputs.manifest.json"] = json!(sha(&std::fs::read(
        f.dir.join("session/inputs.manifest.json")
    )
    .unwrap()));
    f.write("session/prepared.manifest.json", &prepared);
    let (code, v) = f.collect();
    assert_eq!(code, 1, "{v}");
    assert_eq!(
        std::fs::read(f.dir.join("argv.jsonl")).unwrap(),
        original_argv
    );
    std::fs::write(f.dir.join("session/inputs.manifest.json"), &original_inputs).unwrap();
    let mut prepared: Value = serde_json::from_slice(&original_prepared).unwrap();
    prepared["hashes"]
        .as_object_mut()
        .unwrap()
        .remove("admission_0-trace.txt");
    f.write("session/prepared.manifest.json", &prepared);
    let (code, v) = f.collect();
    assert_eq!(code, 1, "{v}");
    assert_eq!(
        std::fs::read(f.dir.join("argv.jsonl")).unwrap(),
        original_argv
    );
    std::fs::write(
        f.dir.join("session/prepared.manifest.json"),
        original_prepared,
    )
    .unwrap();
    let (code, v) = f.collect();
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["status"], "SIMULATED_BLOCKED");
    let s = f.read("session/samples.json");
    assert_eq!(s["source_kind"], "synthetic_fixture");
    assert_eq!(s["pairs"].as_array().unwrap().len(), 5);
    for p in s["pairs"].as_array().unwrap() {
        for side in ["a", "b"] {
            let r = f.read(&format!(
                "session/{}",
                p[format!("{side}_report")].as_str().unwrap()
            ));
            assert_eq!(r["status"], "BLOCKED");
            assert_eq!(r["qualification"]["eligible"], false);
            assert_eq!(r["real_game_optimization_proven"], false);
            assert_eq!(r["promotion_allowed"], false);
            let id = p[format!("{side}_run_id")].as_str().unwrap();
            let o = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
                .args(["confirmation-observation", "--bundle"])
                .arg(f.dir.join(format!("session/{id}-bundle.json")))
                .arg("--out")
                .arg(f.dir.join(format!("{id}-reconstructed.json")))
                .output()
                .unwrap();
            assert!(
                !o.status.success(),
                "synthetic raw producer cannot become qualified"
            );
            assert_eq!(f.read(&format!("{id}-reconstructed.json")), r);
        }
    }
    if let Some(bin) = std::env::var_os("PHONEFARM_DRIVER_GOL_BIN") {
        let o = Command::new(bin)
            .args(["confirm", "judge", "--frozen"])
            .arg(f.dir.join("frozen.json"))
            .arg("--samples")
            .arg(f.dir.join("session/samples.json"))
            .arg("--out")
            .arg(f.dir.join("gol-report.json"))
            .output()
            .unwrap();
        std::fs::write(f.dir.join("gol-judge.stdout"), &o.stdout).unwrap();
        std::fs::write(f.dir.join("gol-judge.stderr"), &o.stderr).unwrap();
        f.write("gol-judge.exit.json", &json!({"exit":o.status.code()}));
        assert!(!o.status.success(), "synthetic confirmation must not PASS");
        let r = f.read("gol-report.json");
        assert_eq!(r["verdict"], "BLOCKED");
        assert_eq!(r["claim_scope"], "none");
        assert_eq!(r["real_game_optimization_proven"], false);
        assert_eq!(r["promotion_allowed"], false);
        assert!(r["report_hashes"].as_object().unwrap().len() == 10, "{r}");
    }
    let (code, v) = f.collect();
    assert_eq!(code, 1);
    assert!(v["error"].as_str().unwrap().contains("already started"));
}
#[test]
fn real_validation_without_execute_invokes_no_transport() {
    let f = F::new(false);
    let mut r = f.read("request.json");
    r["transport"]["kind"] = json!("adb");
    f.write("request.json", &r);
    let (c, v) = f.prepare("");
    assert_eq!(c, 0, "{v}");
    assert_eq!(v["status"], "VALIDATED_ONLY");
    assert!(!f.dir.join("argv.jsonl").exists());
    assert!(!f.dir.join("session").exists());
}
#[test]
fn unknown_root_blocks_before_any_write() {
    let f = F::new(false);
    let (c, v) = f.prepare("root");
    assert_eq!(c, 1, "{v}");
    let s = std::fs::read_to_string(f.dir.join("argv.jsonl")).unwrap();
    assert!(!s.contains("mkdir"));
    assert!(!f.dir.join("legacy/devlock.d").exists());
}
#[test]
fn every_failed_raw_gate_stops_and_preserves_uncertainty() {
    for fail in [
        "probe", "cold", "workload", "env", "restore", "active", "power",
    ] {
        let f = F::new(false);
        let (c, v) = f.prepare(fail);
        assert_eq!(c, 1, "{fail}: {v}");
        let m = f.read("legacy/devlock.d/phonefarm-lease.json");
        assert_eq!(m["recovery_required"], true, "{fail}");
        assert!(!f.dir.join("session/prepared.manifest.json").exists());
        let (c, _) = f.call(
            &["prepare", "--request", "request.json", "--out", "another"],
            "",
        );
        assert_eq!(c, 1);
    }
}
#[test]
fn explicit_reconcile_never_validates_failed_old_sample() {
    let f = F::new(false);
    assert_eq!(f.prepare("workload").0, 1);
    // A crashed game must not prevent restoring positively owned control state.
    let (c, v) = f.call(
        &["reconcile", "--session", "session", "--execute"],
        "foreground",
    );
    assert_eq!(c, 0, "{v}");
    assert_eq!(v["old_samples_revalidated"], false);
    assert!(!f.dir.join("legacy/devlock.d").exists());
    assert!(!f.dir.join("session/prepared.manifest.json").exists());
    let state = std::fs::read(f.dir.join("fake-state.json")).unwrap();
    assert_eq!(f.delayed_old_worker().status.code(), Some(86));
    assert_eq!(std::fs::read(f.dir.join("fake-state.json")).unwrap(), state);
}
#[test]
fn uncertain_session_closure_keeps_barrier_and_delayed_command_revoked() {
    let f = F::new(false);
    assert_eq!(f.prepare("probe").0, 1);
    let (c, v) = f.call(
        &["reconcile", "--session", "session", "--execute"],
        "closure_unknown",
    );
    assert_eq!(c, 1, "{v}");
    assert_eq!(
        f.read("legacy/devlock.d/phonefarm-lease.json")["recovery_required"],
        true
    );
    assert_eq!(f.delayed_old_worker().status.code(), Some(86));
    // A missing closure receipt cannot be filled by replaying recovery on a tombstoned session.
    assert_eq!(
        f.call(&["reconcile", "--session", "session", "--execute"], "")
            .0,
        1
    );
    assert_eq!(
        f.read("legacy/devlock.d/phonefarm-lease.json")["recovery_required"],
        true
    );
}
#[test]
fn unbound_pending_transport_cannot_be_reconciled_or_asserted_idle() {
    let f = F::new(false);
    assert_eq!(f.prepare("probe").0, 1);
    let original_argv = std::fs::read(f.dir.join("argv.jsonl")).unwrap();
    let mut m = f.read("legacy/devlock.d/phonefarm-lease.json");
    m["command_pending"] = json!(true);
    m["pending_operation"] = json!("unbound-before-spawn");
    f.write("legacy/devlock.d/phonefarm-lease.json", &m);
    let (c, v) = f.call(&["reconcile", "--session", "session", "--execute"], "");
    assert_eq!(c, 1, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unbound pending"));
    assert_eq!(
        std::fs::read(f.dir.join("argv.jsonl")).unwrap(),
        original_argv
    );
    let o = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .args([
            "lease",
            "--serial",
            "fake-target",
            "--recover-abandoned",
            "--confirm-device-idle",
            "--timeout-ms",
            "100",
        ])
        .env("PHONEFARM_LEASE_DIR", f.dir.join("lease"))
        .env("DEVLOCK_DIR", f.dir.join("legacy"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(
        f.read("legacy/devlock.d/phonefarm-lease.json")["recovery_required"],
        true
    );
}
#[test]
fn local_parent_exit_with_lingering_closed_pipe_child_is_not_success() {
    let f = F::new(false);
    let (c, v) = f.prepare("closed_pipes_child");
    assert_eq!(c, 1);
    assert!(
        v["error"].as_str().unwrap().contains("process group"),
        "{v}"
    );
}
#[test]
fn boot_changed_reconcile_preserves_barrier() {
    let f = F::new(false);
    assert_eq!(f.prepare("probe").0, 1);
    let (c, v) = f.call(&["reconcile", "--session", "session", "--execute"], "boot");
    assert_eq!(c, 1, "{v}");
    assert_eq!(
        f.read("legacy/devlock.d/phonefarm-lease.json")["recovery_required"],
        true
    );
}
#[test]
fn per_device_lease_excludes_other_cwd_renews_and_releases_completed_command() {
    use std::time::{Duration, Instant};
    let f = F::new(false);
    let started = f.dir.join("lease-started");
    let done = f.dir.join("lease-done");
    let script = "echo start > \"$1\"; while [ ! -f \"$2\" ]; do sleep .05; done";
    let mut a = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .args([
            "lease",
            "--serial",
            "fake-target",
            "--timeout-ms",
            "100",
            "--",
            "/bin/sh",
            "-c",
            script,
            "lease-test",
        ])
        .arg(&started)
        .arg(&done)
        .current_dir(&f.dir)
        .env("PHONEFARM_LEASE_DIR", f.dir.join("lease"))
        .env("DEVLOCK_DIR", f.dir.join("legacy"))
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(a.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    let owner = f.dir.join("legacy/devlock.d/owner");
    let first = std::fs::metadata(&owner).unwrap().modified().unwrap();
    let until = Instant::now() + Duration::from_secs(7);
    while std::fs::metadata(&owner).unwrap().modified().unwrap() == first {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(25));
    }
    let competing = || {
        Command::new(env!("CARGO_BIN_EXE_phonefarm"))
            .args([
                "lease",
                "--serial",
                "fake-target",
                "--timeout-ms",
                "100",
                "--",
                "/bin/sh",
                "-c",
                "exit 0",
            ])
            .current_dir(f.dir.parent().unwrap())
            .env("PHONEFARM_LEASE_DIR", f.dir.join("lease"))
            .env("DEVLOCK_DIR", f.dir.join("legacy"))
            .output()
            .unwrap()
    };
    assert!(!competing().status.success());
    std::fs::write(&done, "done").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = a.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!f.dir.join("legacy/devlock.d").exists());
    assert!(competing().status.success());
    assert!(!f.dir.join("legacy/devlock.d").exists());
}
