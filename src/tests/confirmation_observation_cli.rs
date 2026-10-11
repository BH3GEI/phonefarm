//! Hypothetical driver metadata tests the file/receiver contract, not physical device proof.
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn hash(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let base = std::env::var_os("PHONEFARM_CONFIRMATION_TEST_ARTIFACT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(
            "confirmation-contract-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn text(&self, name: &str, s: &str) -> Value {
        std::fs::write(self.0.join(name), s).unwrap();
        json!({"path":name,"sha256":hash(s.as_bytes())})
    }
    fn json(&self, name: &str, v: &Value) -> Value {
        self.text(name, &serde_json::to_string(v).unwrap())
    }
    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.0.join(name)).unwrap()).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var_os("PHONEFARM_CONFIRMATION_TEST_ARTIFACT_DIR").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
#[derive(Serialize)]
struct Plan {
    baseline_artifact_sha256: String,
    candidate_artifact_sha256: String,
    conditions_sha256: String,
    baseline_admission_sha256: String,
    exploration_run_ids: Vec<String>,
    exploration_artifact_sha256: Vec<String>,
    pairs: usize,
    schedule: Vec<Schedule>,
    family_alpha: f64,
    paired_t_assumptions_acknowledged: bool,
    metrics: Vec<Metric>,
}
#[derive(Serialize)]
struct Schedule {
    pair_id: String,
    a_run_id: String,
    b_run_id: String,
    baseline_first: bool,
}
#[derive(Serialize)]
struct Metric {
    name: String,
    direction: String,
    scale: String,
    minimum_improvement: Option<f64>,
    maximum_regression: f64,
}
struct Fixture {
    s: Scratch,
    conditions: Value,
    baseline: Value,
    candidate: Value,
    admission: Value,
    bundle: Value,
}
const BEFORE:&str=concat!("# device_snapshot v1\nftrace.tracing_on=0\nftrace.current_tracer=nop\nftrace.buffer_size_kb=65536\nftrace.set_event=\n",
    "kgsl.min_pwrlevel=<unreadable>\nkgsl.max_pwrlevel=<unreadable>\nkgsl.default_pwrlevel=<unreadable>\nkgsl.thermal_pwrlevel=<unreadable>\n",
    "kgsl.force_clk_on=<unreadable>\nkgsl.force_rail_on=<unreadable>\nkgsl.force_bus_on=<unreadable>\nkgsl.force_no_nap=<unreadable>\n",
    "kgsl.idle_timer=<unreadable>\nkgsl.max_gpuclk=<unreadable>\nkgsl.pwrscale=<unreadable>\nkgsl.bus_split=<unreadable>\nkgsl.perfcounter=<unreadable>\n",
    "kgsl.devfreq_governor=<unreadable>\nkgsl.devfreq_min_freq=<unreadable>\nkgsl.devfreq_max_freq=<unreadable>\n",
    "bus.DDR.boost_freq=0\nbus.DDR.hw_max_freq=5333000\nbus.DDR.hw_min_freq=200000\nbus.LLCC.boost_freq=<unreadable>\nbus.LLCC.hw_max_freq=<unreadable>\nbus.LLCC.hw_min_freq=<unreadable>\n",
    "wm.size=<unreadable>\nwm.density=<unreadable>\nsettings.peak_refresh_rate=<unreadable>\nsettings.min_refresh_rate=<unreadable>\nsettings.refresh_rate_mode=<unreadable>\n",
    "cpu.policy0.governor=schedutil\ncpu.policy0.scaling_max=2000000\ncpu.policy0.scaling_min=300000\n# end\n#adb_exit=0");
fn run(
    s: &Scratch,
    index: u32,
    id: &str,
    role: &str,
    phase: &str,
    conditions: &Value,
    artifact: &Value,
) -> Value {
    let trace = include_str!("../../loop_v1/fixtures/trace_slice.txt");
    // Recorded raw excerpt, clock-shifted to distinct hypothetical replay windows. Never attested.
    let trace = regex::Regex::new(r"10459\d{2}\.\d+")
        .unwrap()
        .replace_all(trace, |c: &regex::Captures| {
            format!("{:.6}", c[0].parse::<f64>().unwrap() + index as f64 * 60.)
        })
        .to_string();
    let trace = trace.lines().skip(1).collect::<Vec<_>>().join("\n");
    let trace = format!(
        "#loop_v1_meta capture_start_uptime={} capture_end_uptime={} duration_req=2\n{trace}\n",
        1045979.1 + index as f64 * 60.,
        1045981.56 + index as f64 * 60.
    );
    let mut env=format!("#sample_env v1 dur=5 interval=1\n#battery_status=Discharging\n#battery_status_after=Discharging\n#fan_state=unknown\n# fixture-run={id}\n");
    for n in 0..5 {
        env.push_str(&format!(
            "ENV {} 3850000 -1000000 NA 0 0 cpu-1-0 39000\n",
            1045978. + index as f64 * 60. + n as f64
        ));
    }
    env.push_str("#sample_env done\n");
    let prefix = format!("{id}-");
    let mut raw = json!({});
    for (key,content) in [("trace",trace.as_str()),("env",env.as_str()),("workload_exit","0\n"),("sampler_exit","0\n"),
        ("capture_exit","0\n"),("restore_exit","0\n"),("root_id","uid=0(root) gid=0(root)\n"),
        ("boot_id","fixture-boot\n"),("probe","# probe_sysparam v1\ncpu.policies=0\nbus.DDR.boost_freq.cur=0\nbus.DDR.hw_min_freq=200000\nbus.DDR.hw_max_freq=5333000\nbus.DDR.avail_freqs=200000 3200000 5333000\nbus.DDR.boost_freq.writable=yes\nbus.DDR.boost_freq.effect=live\n"),
        ("snapshot_before",BEFORE),("snapshot_restored",BEFORE)] {
        raw[key]=s.text(&format!("{prefix}{key}.txt"),content);
    }
    let active = if role == "candidate" {
        BEFORE.replace("bus.DDR.boost_freq=0", "bus.DDR.boost_freq=3200000")
    } else {
        BEFORE.into()
    };
    raw["snapshot_at_run"] = s.text(&format!("{prefix}snapshot_at_run.txt"), &active);
    raw["snapshot_after_run"] = s.text(&format!("{prefix}snapshot_after_run.txt"), &active);
    raw["cold"]=s.json(&format!("{prefix}cold.json"),&json!({"checked_at":format!("2026-10-11T00:{index:02}:00Z"),"zone":"cpu-1-0","c":39.,"waited_s":0}));
    json!({"schema":"phonefarm/run_one_bundle@1","evidence_origin":"recorded_device","phase":phase,
        "run_id":id,"pair_id":if phase=="confirmation"{json!("p1")}else{Value::Null},"sample_role":role,
        "device_serial":"fixture-device","started_at":format!("2026-10-11T00:{index:02}:01Z"),
        "finished_at":format!("2026-10-11T00:{index:02}:11Z"),"conditions":conditions,"artifact":artifact,"raw":raw})
}
impl Fixture {
    fn new() -> Self {
        let s = Scratch::new();
        let conditions=s.json("conditions.json",&json!({"schema":"phonefarm/confirmation_conditions@1","device_serial":"fixture-device",
            "device_boot_id":"fixture-boot","snapshot_cpu_policies":["policy0"],"render_comm":"UnityGfxDeviceW","capture_seconds":2,"frozen_at":"2026-10-11T00:00:00Z",
            "policy":{"version":1,"goal":"render_efficiency","required_metrics":["frame_p95","fps_mean","power_w_mean"],
                "required_pairs":5,"require_quality":false,"temp_cap_c":70.,"cool_c":40.,"baseline_min_runs":3,
                "dispersion_max_pct":5.,"monotonic_max_ratio":0.90,"minimum_pairs_for_conclusion":5,"alpha":0.05}}));
        let baseline = s.json(
            "baseline.json",
            &json!({"kind":"sysparam","payload":{"params":{},"plan":[],"deny_keywords":[]}}),
        );
        let candidate=s.json("candidate.json",&json!({"kind":"sysparam","payload":{"params":{"bus.DDR.boost_freq":"3200000"},
            "plan":[{"kind":"sysfs","path":"/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq","value":"3200000"}],"deny_keywords":[]}}));
        let mut runs = vec![];
        for n in 1..=3 {
            let id = format!("explore{n}");
            runs.push(s.json(
                &format!("{id}.json"),
                &run(&s, n, &id, "baseline", "admission", &conditions, &baseline),
            ));
        }
        let admission=s.json("admission.json",&json!({"schema":"phonefarm/confirmation_baseline_admission@1","conditions":conditions,"baseline_artifact":baseline,"runs":runs}));
        let current = run(
            &s,
            5,
            "a1",
            "baseline",
            "confirmation",
            &conditions,
            &baseline,
        );
        let current = s.json("run.json", &current);
        let mut result = Self {
            s,
            conditions,
            baseline,
            candidate,
            admission,
            bundle: json!({"schema":"phonefarm/confirmation_bundle@1","run":current,"baseline_admission":null,"frozen_plan":null}),
        };
        result.freeze();
        result
    }
    fn freeze(&mut self) {
        self.freeze_at("2026-10-11T00:04:00Z");
    }
    fn freeze_at(&mut self, registered: &str) {
        let mut excluded = vec![];
        for n in 1..=3 {
            let r = self.s.read(&format!("explore{n}.json"));
            for k in ["trace", "env"] {
                excluded.push(r["raw"][k]["sha256"].as_str().unwrap().into());
            }
        }
        let p = Plan {
            baseline_artifact_sha256: self.baseline["sha256"].as_str().unwrap().into(),
            candidate_artifact_sha256: self.candidate["sha256"].as_str().unwrap().into(),
            conditions_sha256: self.conditions["sha256"].as_str().unwrap().into(),
            baseline_admission_sha256: self.admission["sha256"].as_str().unwrap().into(),
            exploration_run_ids: (1..=3).map(|n| format!("explore{n}")).collect(),
            exploration_artifact_sha256: excluded,
            pairs: 5,
            schedule: (1..=5)
                .map(|n| Schedule {
                    pair_id: format!("p{n}"),
                    a_run_id: format!("a{n}"),
                    b_run_id: format!("b{n}"),
                    baseline_first: n % 2 == 1,
                })
                .collect(),
            family_alpha: 0.05,
            paired_t_assumptions_acknowledged: true,
            metrics: vec![
                Metric {
                    name: "fps_mean".into(),
                    direction: "higher".into(),
                    scale: "percent".into(),
                    minimum_improvement: Some(1.),
                    maximum_regression: 1.,
                },
                Metric {
                    name: "frame_p95_ms".into(),
                    direction: "lower".into(),
                    scale: "percent".into(),
                    minimum_improvement: None,
                    maximum_regression: 51.24817837550540_4, // default serde float parser previously changed this by one ULP
                },
                Metric {
                    name: "power_watt".into(),
                    direction: "lower".into(),
                    scale: "percent".into(),
                    minimum_improvement: None,
                    maximum_regression: 1.,
                },
            ],
        };
        let fingerprint = hash(
            &serde_json::to_vec(&("game_opt_loop/confirmation_plan@1", registered, &p)).unwrap(),
        );
        let frozen = json!({"schema":"game_opt_loop/confirmation_plan@1","registered_at":registered,"plan":serde_json::to_value(&p).unwrap(),"sha256":fingerprint});
        self.bundle["frozen_plan"] = self.s.json("frozen.json", &frozen);
        self.bundle["baseline_admission"] = self.admission.clone();
        self.s.json("bundle.json", &self.bundle);
    }
    fn update_run(&mut self, r: &Value) {
        self.bundle["run"] = self.s.json("run.json", r);
        self.s.json("bundle.json", &self.bundle);
    }
    fn raw(&mut self, key: &str, s: &str) {
        let mut r = self.s.read("run.json");
        r["raw"][key] = self.s.text(&format!("changed-{key}.txt"), s);
        self.update_run(&r);
    }
    fn output(&self, name: &str) -> (i32, Value) {
        let log = self.s.0.join("external.log");
        let o = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
            .args(["confirmation-observation", "--bundle"])
            .arg(self.s.0.join("bundle.json"))
            .arg("--out")
            .arg(self.s.0.join(format!("{name}.json")))
            .env("ADB_BIN", self.s.0.join("adb"))
            .env("QUALIFICATION_ADB_LOG", &log)
            .output()
            .unwrap();
        assert!(!log.exists(), "offline producer invoked adb");
        let v = serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: stdout={} stderr={}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        });
        (o.status.code().unwrap(), v)
    }
}
fn blocked(code: i32, r: &Value) {
    assert_eq!(code, 1, "{r:#}");
    assert_eq!(r["status"], "BLOCKED");
    assert_eq!(r["qualification"]["stage"], "experiment");
    assert_eq!(r["qualification"]["eligible"], false);
    assert_eq!(r["sample_qualification"]["valid_observation"], false);
}
#[test]
fn raw_single_observation_recomputes_metrics_and_remains_ineligible() {
    let f = Fixture::new();
    let (code, r) = f.output("valid");
    assert_eq!(code, 0, "{r:#}");
    assert_eq!(r["status"], "VALID_OBSERVATION");
    assert_eq!(r["qualification"]["eligible"], false);
    assert_eq!(r["qualification"]["stage"], "experiment");
    assert_eq!(
        r["baseline_admission_qualification"]["experiment_allowed"],
        true
    );
    assert_eq!(r["sample_qualification"]["valid_observation"], true);
    assert!(r["sample_qualification"]["gates"]
        .as_object()
        .unwrap()
        .values()
        .all(|g| g == true));
    assert!(r["metrics"]["fps_mean"].as_f64().unwrap() > 0.);
    assert_eq!(r["metrics"]["power_watt"], 3.85);
    assert_eq!(
        r["metric_provenance"]["fps_mean"]["semantics"],
        "gpu_submit_proxy"
    );
    assert_eq!(
        r["metric_provenance"]["fps_mean"]["display_verified"],
        false
    );
    assert_eq!(
        r["metric_provenance"]["power_watt"]["window_verified"],
        true
    );
    assert_eq!(r["confirmation_binding"]["aggregation_level"], "single_run");
    assert_eq!(r["promotion_allowed"], false);
    assert_eq!(r["real_game_optimization_proven"], false);
    let (_, again) = f.output("valid-again");
    assert_eq!(r, again);
}
#[test]
fn candidate_uses_real_gol_plan_entry_shape_and_readback() {
    let mut f = Fixture::new();
    let r = run(
        &f.s,
        6,
        "b1",
        "candidate",
        "confirmation",
        &f.conditions,
        &f.candidate,
    );
    f.update_run(&r);
    let (code, r) = f.output("candidate-valid");
    assert_eq!(code, 0, "{r:#}");
    assert_eq!(r["confirmation_binding"]["sample_role"], "candidate");
    f.raw("snapshot_after_run", BEFORE);
    let (code, r) = f.output("candidate-control-fail");
    blocked(code, &r);
    assert_eq!(
        r["sample_qualification"]["gates"]["control_verified"],
        false
    );
}
#[test]
fn every_raw_gate_blocks_independently_and_synthetic_never_valid() {
    for (key, value, gate) in [
        (
            "root_id",
            "uid=2000(shell) gid=2000(shell)",
            "root_verified",
        ),
        ("workload_exit", "7", "workload_succeeded"),
        ("sampler_exit", "1", "trace_window_verified"),
        ("capture_exit", "1", "trace_window_verified"),
        ("restore_exit", "1", "restore_verified"),
        (
            "snapshot_restored",
            &BEFORE.replace("tracing_on=0", "tracing_on=1"),
            "restore_verified",
        ),
    ] {
        let mut f = Fixture::new();
        f.raw(key, value);
        let (code, r) = f.output(key);
        blocked(code, &r);
        assert_eq!(r["sample_qualification"]["gates"][gate], false, "{r:#}");
    }
    let mut f = Fixture::new();
    let mut m = f.s.read("run.json");
    m["evidence_origin"] = json!("synthetic");
    f.update_run(&m);
    let (code, r) = f.output("synthetic");
    blocked(code, &r);
    assert_eq!(r["qualification"]["evidence_origin"], "synthetic");
    let mut f = Fixture::new();
    f.raw(
        "cold",
        r#"{"checked_at":"2026-10-11T00:05:00Z","zone":"cpu-1-0","c":41.0,"waited_s":0}"#,
    );
    let (code, r) = f.output("hot");
    blocked(code, &r);
    assert_eq!(r["sample_qualification"]["gates"]["cold_verified"], false);
}
#[test]
fn missing_driver_context_and_handwritten_gates_are_rejected() {
    for key in ["sampler_exit", "root_id", "cold", "snapshot_restored"] {
        let mut f = Fixture::new();
        let mut m = f.s.read("run.json");
        m["raw"].as_object_mut().unwrap().remove(key);
        f.update_run(&m);
        let (code, r) = f.output(key);
        blocked(code, &r);
    }
    let mut f = Fixture::new();
    let mut m = f.s.read("run.json");
    m["gates"] = json!({"root_verified":true});
    f.update_run(&m);
    let (code, r) = f.output("fake-gates");
    blocked(code, &r);
    assert!(r["issues"].to_string().contains("unknown field"));
}
#[test]
fn frozen_binding_raw_reuse_and_aggregate_admission_fail_closed() {
    let mut f = Fixture::new();
    let mut m = f.s.read("run.json");
    m["run_id"] = json!("explore1");
    f.update_run(&m);
    let (code, r) = f.output("wrong-role");
    blocked(code, &r);
    let mut f = Fixture::new();
    let mut m = f.s.read("run.json");
    m["raw"]["trace"] = f.s.read("explore1.json")["raw"]["trace"].clone();
    f.update_run(&m);
    let (code, r) = f.output("raw-reuse");
    blocked(code, &r);
    let mut f = Fixture::new();
    f.admission = f.s.json(
        "admission.json",
        &json!({"version":1,"stage":"conclusion","eligible":true}),
    );
    f.freeze();
    let (code, r) = f.output("aggregate");
    blocked(code, &r);
    assert_eq!(
        r["sample_qualification"]["gates"]["baseline_admission_verified"],
        false
    );
    let mut f = Fixture::new();
    let mut frozen = f.s.read("frozen.json");
    frozen["plan"]["family_alpha"] = json!(1.);
    f.bundle["frozen_plan"] = f.s.json("frozen.json", &frozen);
    f.s.json("bundle.json", &f.bundle);
    let (code, r) = f.output("changed-frozen");
    blocked(code, &r);
}
#[test]
fn trace_truncation_incomplete_power_and_snapshot_truncation_cannot_pass() {
    let mut f = Fixture::new();
    let m = f.s.read("run.json");
    let text =
        std::fs::read_to_string(f.s.0.join(m["raw"]["trace"]["path"].as_str().unwrap())).unwrap();
    f.raw(
        "trace",
        &text.lines().take(100).collect::<Vec<_>>().join("\n"),
    );
    let (code, r) = f.output("short-trace");
    blocked(code, &r);
    let mut f = Fixture::new();
    let m = f.s.read("run.json");
    let text =
        std::fs::read_to_string(f.s.0.join(m["raw"]["env"]["path"].as_str().unwrap())).unwrap();
    f.raw("env", &text.replace("#sample_env done", ""));
    let (code, r) = f.output("incomplete-env");
    blocked(code, &r);
    assert_eq!(
        r["metric_provenance"]["power_watt"]["window_verified"],
        false
    );
    let mut f = Fixture::new();
    f.raw("snapshot_restored", "bus.DDR.boost_freq=0\n#adb_exit=0");
    let (code, r) = f.output("short-snapshot");
    blocked(code, &r);
}
#[test]
fn same_boot_uptime_cannot_go_back_after_baseline_admission() {
    let mut f = Fixture::new();
    let mut m = run(
        &f.s,
        2,
        "a1-old-uptime",
        "baseline",
        "confirmation",
        &f.conditions,
        &f.baseline,
    );
    // Host clock alone looks post-registration; saved capture/ENV uptime precedes admission3.
    m["run_id"] = json!("a1");
    m["started_at"] = json!("2026-10-11T00:05:01Z");
    m["finished_at"] = json!("2026-10-11T00:05:11Z");
    m["raw"]["cold"] = f.s.json(
        "later-cold.json",
        &json!({"checked_at":"2026-10-11T00:05:00Z","zone":"cpu-1-0","c":39.,"waited_s":0}),
    );
    let trace_path = f.s.0.join(m["raw"]["trace"]["path"].as_str().unwrap());
    let mut trace = std::fs::read_to_string(trace_path).unwrap();
    trace.push_str("# hypothetical distinct receipt\n");
    m["raw"]["trace"] = f.s.text("backwards-trace.txt", &trace);
    f.update_run(&m);
    let (code, r) = f.output("uptime-backwards");
    blocked(code, &r);
    assert!(
        r["issues"]
            .to_string()
            .contains("confirmation capture uptime"),
        "{r:#}"
    );
}
#[test]
fn admission_recomputes_repetition_and_rejects_unstable_raw_baselines() {
    let mut f = Fixture::new();
    let mut run = f.s.read("explore2.json");
    let old =
        std::fs::read_to_string(f.s.0.join(run["raw"]["env"]["path"].as_str().unwrap())).unwrap();
    run["raw"]["env"] = f.s.text(
        "unstable-baseline-env.txt",
        &old.replace("-1000000", "-1500000"),
    );
    let changed = f.s.json("explore2.json", &run);
    let mut a = f.s.read("admission.json");
    a["runs"][1] = changed;
    f.admission = f.s.json("admission.json", &a);
    f.freeze();
    let (code, r) = f.output("unstable-baseline");
    blocked(code, &r);
    assert_eq!(
        r["sample_qualification"]["gates"]["baseline_admission_verified"],
        false
    );
    assert!(r["baseline_admission_qualification"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == "baseline.power_w_mean.repeatability" && c["state"] == "fail"));
}
#[test]
fn even_identical_snapshots_cannot_omit_untouched_fixed_or_declared_cpu_domains() {
    for omitted in [
        "kgsl.force_clk_on=<unreadable>\n",
        "cpu.policy0.governor=schedutil\n",
    ] {
        let mut f = Fixture::new();
        let mut m = f.s.read("run.json");
        for key in [
            "snapshot_before",
            "snapshot_at_run",
            "snapshot_after_run",
            "snapshot_restored",
        ] {
            m["raw"][key] =
                f.s.text(&format!("missing-{key}.txt"), &BEFORE.replace(omitted, ""));
        }
        f.update_run(&m);
        let (code, r) = f.output("missing-domain");
        blocked(code, &r);
        assert_eq!(
            r["sample_qualification"]["gates"]["restore_verified"],
            false
        );
        assert_eq!(
            r["sample_qualification"]["gates"]["control_verified"],
            false
        );
    }
}
fn long_env(id: &str) -> String {
    let mut env=format!("#sample_env v1 dur=220 interval=1\n#battery_status=Discharging\n#battery_status_after=Discharging\n# hypothetical-run={id}\n");
    for n in 0..220 {
        env.push_str(&format!(
            "ENV {} 3850000 -1000000 NA 0 0 cpu-1-0 39000\n",
            1045978 + n
        ));
    }
    env.push_str("#sample_env done\n");
    env
}
#[test]
fn complete_telemetry_cannot_exceed_the_recorded_run_duration() {
    let mut f = Fixture::new();
    let base = 1046278;
    let env = long_env("current").replace("10459", "10462");
    // Use an independently complete 220-second window containing the current 2-second capture.
    let mut env = env
        .lines()
        .filter(|l| !l.starts_with("ENV "))
        .collect::<Vec<_>>()
        .join("\n");
    env = env.replace("#sample_env done", "");
    for n in 0..220 {
        env.push_str(&format!(
            "\nENV {} 3850000 -1000000 NA 0 0 cpu-1-0 39000",
            base + n
        ));
    }
    env.push_str("\n#sample_env done\n");
    f.raw("env", &env);
    let (code, r) = f.output("env-too-long");
    blocked(code, &r);
    assert_eq!(
        r["sample_qualification"]["gates"]["trace_window_verified"],
        false
    );
    assert_eq!(
        r["metric_provenance"]["power_watt"]["window_verified"],
        false
    );
}
#[test]
fn different_hashes_of_shared_long_telemetry_cannot_admit_independent_baselines() {
    let mut f = Fixture::new();
    let mut admission = f.s.read("admission.json");
    for n in 1..=3 {
        let mut m = f.s.read(&format!("explore{n}.json"));
        let start = 1 + (n - 1) * 9;
        m["started_at"] = json!(format!("2026-10-11T00:{start:02}:01Z"));
        m["finished_at"] = json!(format!("2026-10-11T00:{:02}:01Z", start + 8));
        m["raw"]["cold"]=f.s.json(&format!("explore{n}-cold.json"),&json!({"checked_at":format!("2026-10-11T00:{start:02}:00Z"),"zone":"cpu-1-0","c":39.,"waited_s":0}));
        m["raw"]["env"] = f.s.text(
            &format!("explore{n}-long-env.txt"),
            &long_env(&format!("explore{n}")),
        );
        admission["runs"][n - 1] = f.s.json(&format!("explore{n}.json"), &m);
    }
    f.admission = f.s.json("admission.json", &admission);
    f.freeze_at("2026-10-11T00:40:00Z");
    let mut m = f.s.read("run.json");
    m["started_at"] = json!("2026-10-11T00:45:01Z");
    m["finished_at"] = json!("2026-10-11T00:45:11Z");
    m["raw"]["cold"] = f.s.json(
        "later-cold.json",
        &json!({"checked_at":"2026-10-11T00:45:00Z","zone":"cpu-1-0","c":39.,"waited_s":0}),
    );
    f.update_run(&m);
    let (code, r) = f.output("shared-env");
    blocked(code, &r);
    assert_eq!(
        r["sample_qualification"]["gates"]["baseline_admission_verified"],
        false
    );
    assert!(
        r["issues"]
            .to_string()
            .contains("independent host/capture/ENV windows"),
        "{r:#}"
    );
}
#[test]
fn wrong_bytes_symlinks_and_existing_output_are_rejected() {
    let f = Fixture::new();
    std::fs::write(f.s.0.join("a1-workload_exit.txt"), "0\nchanged").unwrap();
    let (code, r) = f.output("changed-bytes");
    blocked(code, &r);
    assert!(r["issues"].to_string().contains("changed evidence"));
    #[cfg(unix)]
    {
        let f = Fixture::new();
        std::fs::remove_file(f.s.0.join("a1-root_id.txt")).unwrap();
        std::os::unix::fs::symlink(
            f.s.0.join("explore1-root_id.txt"),
            f.s.0.join("a1-root_id.txt"),
        )
        .unwrap();
        let (code, r) = f.output("symlink");
        blocked(code, &r);
        assert!(r["issues"].to_string().contains("symlink"));
    }
    let f = Fixture::new();
    f.output("immutable");
    let p = f.s.0.join("immutable.json");
    let original = std::fs::read(&p).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .args(["confirmation-observation", "--bundle"])
        .arg(f.s.0.join("bundle.json"))
        .arg("--out")
        .arg(&p)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(std::fs::read(p).unwrap(), original);
}
#[cfg(unix)]
#[test]
fn named_pipe_evidence_is_blocked_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let mut f = Fixture::new();
    let pipe = f.s.0.join("receipt-fifo");
    let cpath = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let mut run = f.s.read("run.json");
    run["raw"]["root_id"]["path"] = json!("receipt-fifo");
    f.update_run(&run);
    let mut child = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .args(["confirmation-observation", "--bundle"])
        .arg(f.s.0.join("bundle.json"))
        .arg("--out")
        .arg(f.s.0.join("fifo-report.json"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("offline reader blocked on FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let r: Value = serde_json::from_slice(&output.stdout).unwrap();
    blocked(output.status.code().unwrap(), &r);
    assert!(r["issues"].to_string().contains("regular file"));
}
#[cfg(unix)]
#[test]
fn no_device_or_model_invocation_even_for_valid_observation() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let sentinel = f.s.0.join("adb");
    std::fs::write(
        &sentinel,
        "#!/bin/sh\necho invoked >> \"$QUALIFICATION_ADB_LOG\"\nexit 88\n",
    )
    .unwrap();
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o755)).unwrap();
    for tool in ["hdc", "claude", "codex"] {
        std::fs::copy(&sentinel, f.s.0.join(tool)).unwrap();
    }
    let log = f.s.0.join("external.log");
    let o = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .args(["confirmation-observation", "--bundle"])
        .arg(f.s.0.join("bundle.json"))
        .arg("--out")
        .arg(f.s.0.join("offline.json"))
        .env("ADB_BIN", &sentinel)
        .env("PATH", &f.s.0)
        .env("QUALIFICATION_ADB_LOG", &log)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    assert!(!log.exists());
}
