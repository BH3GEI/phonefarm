//! Receiver-owned qualification, shared by offline CLI, eval and autoloop.
//! A qualification is permission to interpret an experiment, never an improvement verdict.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    RenderEfficiency,
    RenderCadence,
    DisplaySmoothness,
}

impl Goal {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "render_efficiency" => Ok(Self::RenderEfficiency),
            "render_cadence" => Ok(Self::RenderCadence),
            "display_smoothness" => Ok(Self::DisplaySmoothness),
            _ => Err(format!("未知 qualification.goal: {s}")),
        }
    }
    pub fn metrics(self) -> Vec<String> {
        let mut ms = vec!["frame_p95".into(), "fps_mean".into()];
        if self == Self::RenderEfficiency {
            ms.push("power_w_mean".into());
        }
        ms
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub version: u32,
    pub goal: Goal,
    pub required_metrics: Vec<String>,
    pub required_pairs: u32,
    pub require_quality: bool,
    pub temp_cap_c: Option<f64>,
    pub cool_c: Option<f64>,
    pub baseline_min_runs: usize,
    pub dispersion_max_pct: f64,
    pub monotonic_max_ratio: f64,
    pub minimum_pairs_for_conclusion: u32,
    pub alpha: f64,
}

impl Policy {
    pub fn new(goal: Goal, required_pairs: u32, require_quality: bool) -> Self {
        Self {
            version: 1,
            goal,
            required_metrics: goal.metrics(),
            required_pairs,
            require_quality,
            temp_cap_c: Some(70.0),
            cool_c: Some(40.0),
            baseline_min_runs: 3,
            dispersion_max_pct: 5.0,
            monotonic_max_ratio: 0.90,
            minimum_pairs_for_conclusion: 5,
            alpha: 0.05,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.required_metrics != self.goal.metrics() {
            return Err("资格规则版本/required_metrics 与收端冻结目标不符".into());
        }
        if !(1..=32).contains(&self.required_pairs) {
            return Err("资格规则轮数必须 1..32，对候选裁决另要求至少 5 对".into());
        }
        if self.baseline_min_runs != 3
            || self.dispersion_max_pct != 5.0
            || self.monotonic_max_ratio != 0.90
            || self.minimum_pairs_for_conclusion != 5
        {
            return Err("重复性/轮数阈值不得缩窄收端合同".into());
        }
        if !self.alpha.is_finite() || self.alpha <= 0.0 || self.alpha > 0.05 {
            return Err("收端 alpha 必须 (0,.05]，不接受生成方放宽显著性".into());
        }
        for (name, value) in [("temp_cap_c", self.temp_cap_c), ("cool_c", self.cool_c)] {
            if value.is_some_and(|x| !x.is_finite() || x <= 0.0 || x >= 100.0) {
                return Err(format!("{name} 必须未知或 (0,100) 有限数"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Probe,
    Experiment,
    Conclusion,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub backend: String,
    pub root: Option<bool>,
    pub trace_available: Option<bool>,
    pub measurement_available: Option<bool>,
    pub control_verified: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub version: u32,
    pub stage: Stage,
    #[serde(default = "unknown_origin")]
    pub evidence_origin: String,
    pub policy: Policy,
    pub device: Capabilities,
    #[serde(default)]
    pub workload_verified: Option<bool>,
    #[serde(default)]
    pub cold_ok: Option<bool>,
    #[serde(default)]
    pub restored: Option<bool>,
    #[serde(default)]
    pub quality_verified: Option<bool>,
    #[serde(default)]
    pub baseline: Vec<Value>,
    #[serde(default)]
    pub control: Vec<Value>,
    #[serde(default)]
    pub candidate: Vec<Value>,
}

fn unknown_origin() -> String {
    "unknown".into()
}

impl Evidence {
    pub fn new(stage: Stage, policy: Policy, device: Capabilities) -> Self {
        Self {
            version: 1,
            stage,
            evidence_origin: "live_device".into(),
            policy,
            device,
            workload_verified: None,
            cold_ok: None,
            restored: None,
            quality_verified: None,
            baseline: vec![],
            control: vec![],
            candidate: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub state: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub stage: Stage,
    pub status: String,
    pub probe_allowed: bool,
    pub experiment_allowed: bool,
    pub conclusion_allowed: bool,
    /// Receiver gate only; a statistical win is still required. Defaults false outside conclusion.
    pub eligible: bool,
    pub policy: Policy,
    pub checks: Vec<Check>,
    pub frame_evidence: Vec<Value>,
    pub claim_scope: String,
    pub evidence_origin: String,
    pub control_validation_scope: String,
}

fn check(
    checks: &mut Vec<Check>,
    id: impl Into<String>,
    known: Option<bool>,
    reason: impl Into<String>,
) {
    checks.push(Check {
        id: id.into(),
        state: match known {
            Some(true) => "pass",
            Some(false) => "fail",
            None => "unknown",
        }
        .into(),
        reason: reason.into(),
    });
}

pub fn metric(run: &Value, name: &str) -> Option<f64> {
    run["summary"][name]
        .as_f64()
        .or_else(|| run["env"][name].as_f64())
        .filter(|x| x.is_finite() && *x > 0.0)
}

/// Attach provenance at the producer boundary; a fallback SPF=1 stays unknown.
pub fn frame_evidence(summary: &Value) -> Value {
    let spf = summary["submits_per_frame"].as_u64();
    let acf = spf.and_then(|n| summary["spf_scores"]["acf"][n.to_string()].as_f64());
    let inferred = spf.is_some_and(|n| (1..=6).contains(&n))
        && acf.is_some_and(|x| x.is_finite() && x >= 0.30);
    json!({"source":"raw_ftrace_kgsl", "semantics":"gpu_submit_proxy",
        "confidence":if inferred {"inferred"} else {"unknown"},
        "submits_per_frame":spf, "acf_at_selected_period":acf,
        "display_verified":false})
}

fn runs_checks(checks: &mut Vec<Check>, runs: &[Value], label: &str, policy: &Policy) {
    for (i, run) in runs.iter().enumerate() {
        let prefix = format!("{label}.{}", i + 1);
        for m in &policy.required_metrics {
            check(
                checks,
                format!("{prefix}.{m}"),
                Some(metric(run, m).is_some()),
                "必需指标必须每轮完整且有限正数",
            );
        }
        check(
            checks,
            format!("{prefix}.thermal"),
            policy
                .temp_cap_c
                .map(|cap| metric(run, "soc_temp_max_c").is_some_and(|x| x <= cap)),
            format!(
                "每轮热区必须有效且 <= 冻结 temp_cap_c={:?}",
                policy.temp_cap_c
            ),
        );
        check(
            checks,
            format!("{prefix}.thermal_window"),
            run["env"]["thermal_window_verified"].as_bool(),
            "不能用部分有效热区样本声称整窗未越过温度上限",
        );
        check(
            checks,
            format!("{prefix}.trace_window"),
            run["env"]["trace_window_verified"].as_bool(),
            "采样完成/覆盖必须绑定实际trace采集窗",
        );
        check(
            checks,
            format!("{prefix}.workload_exit"),
            run["workload_exit"].as_i64().map(|x| x == 0),
            "每臂负载退出码必须记录为0，初始自检不能代替逐轮成功",
        );
        let f = &run["frame_evidence"];
        // Only the actual collector supported by this receiver can qualify a rendering claim.
        let source_known = f["source"] == "raw_ftrace_kgsl";
        let frame_ok = source_known
            && policy.goal != Goal::DisplaySmoothness
            && f["semantics"] == "gpu_submit_proxy"
            && f["confidence"] == "inferred"
            && f["submits_per_frame"]
                .as_u64()
                .is_some_and(|n| (1..=6).contains(&n))
            && f["acf_at_selected_period"]
                .as_f64()
                .is_some_and(|x| x.is_finite() && x >= 0.30);
        check(
            checks,
            format!("{prefix}.frame_source"),
            Some(frame_ok),
            "submit proxy 只支持渲染目标；未知分组不支持裁决，display 需直接验证",
        );
        if policy.required_metrics.iter().any(|m| m == "power_w_mean") {
            let p = &run["env"];
            let power_ok = p["power_rail"] == "battery"
                && p["on_battery"] == true
                && p["power_window_verified"] == true;
            check(
                checks,
                format!("{prefix}.power_source"),
                Some(power_ok),
                "功耗需要整窗电池放电证据，均值本身不是供电证明",
            );
        }
    }
}

fn statistically_reachable(policy: &Policy) -> bool {
    if policy.required_pairs < policy.minimum_pairs_for_conclusion {
        return false;
    }
    let n = policy.required_pairs;
    let mut combinations = 1.0;
    for i in 1..=n {
        combinations *= (n + i) as f64 / i as f64;
    }
    2.0 / combinations < policy.alpha / policy.required_metrics.len() as f64
}

/// Pure, deterministic: no devices, clock, random numbers or fallbacks for unknown facts.
pub fn evaluate(e: &Evidence) -> Result<Report, String> {
    if e.version != 1 {
        return Err("资格证据仅认 version=1".into());
    }
    e.policy.validate()?;
    let mut checks = vec![];
    check(
        &mut checks,
        "root",
        e.device.root,
        "优化控制/差异写探测要求已验证 root",
    );
    check(
        &mut checks,
        "trace",
        e.device.trace_available,
        "需要可读的目标 trace 事件；root 不代表该厂商驱动支持",
    );
    let probe_allowed = e.device.root == Some(true)
        && e.device.trace_available == Some(true)
        && e.restored != Some(false);
    if e.stage == Stage::Probe && e.restored.is_some() {
        check(
            &mut checks,
            "probe_restored",
            e.restored,
            "失败探测不能声称还原或获得后续写入资格",
        );
    }
    if e.stage != Stage::Probe {
        check(
            &mut checks,
            "evidence_origin",
            Some(matches!(
                e.evidence_origin.as_str(),
                "live_device" | "recorded_device"
            )),
            "合成/估计/未知来源不能取得真实实验结论资格",
        );
        check(
            &mut checks,
            "statistical_reachability",
            Some(statistically_reachable(&e.policy)),
            "至少五对且精确置换最小p必须小于冻结alpha/K",
        );
        check(
            &mut checks,
            "control",
            e.device.control_verified,
            "存在/可写不等于实际生效",
        );
        check(
            &mut checks,
            "workload",
            e.workload_verified,
            "不能只凭脚本退出成功认定负载执行",
        );
        check(
            &mut checks,
            "cold",
            e.policy.cool_c.and(e.cold_ok),
            "冷机未知或超时不能宣称条件相同",
        );
        check(
            &mut checks,
            "restored",
            e.restored,
            "恢复失败/未知阻断结论并保留回滚依据",
        );
        check(
            &mut checks,
            "baseline_count",
            Some(e.baseline.len() >= e.policy.baseline_min_runs),
            "至少三轮完整基线才能检查重复性与漂移",
        );
        runs_checks(&mut checks, &e.baseline, "baseline", &e.policy);
        for m in &e.policy.required_metrics {
            let xs: Vec<f64> = e.baseline.iter().filter_map(|r| metric(r, m)).collect();
            if xs.len() != e.baseline.len() || xs.len() < e.policy.baseline_min_runs {
                check(
                    &mut checks,
                    format!("baseline.{m}.repeatability"),
                    None,
                    "基线不完整，不能估计重复性",
                );
                continue;
            }
            let (_, _, _, disp) = crate::hwcond::dispersion_pct(&xs);
            let ups = xs.windows(2).filter(|w| w[1] > w[0]).count() as f64 / (xs.len() - 1) as f64;
            let downs =
                xs.windows(2).filter(|w| w[1] < w[0]).count() as f64 / (xs.len() - 1) as f64;
            check(&mut checks,format!("baseline.{m}.repeatability"),Some(disp<e.policy.dispersion_max_pct && ups<e.policy.monotonic_max_ratio && downs<e.policy.monotonic_max_ratio),
                format!("dispersion={disp:.3}%, increasing={ups:.3}, decreasing={downs:.3}; frozen gates <{}%, <{}",e.policy.dispersion_max_pct,e.policy.monotonic_max_ratio));
        }
        if e.policy.require_quality && e.stage == Stage::Conclusion {
            check(
                &mut checks,
                "quality",
                e.quality_verified,
                "改写画面要求独立质量门禁；未验证不能代填通过",
            );
        }
        if e.stage == Stage::Conclusion {
            check(
                &mut checks,
                "arm_counts",
                Some(
                    e.control.len() == e.policy.required_pairs as usize
                        && e.candidate.len() == e.policy.required_pairs as usize,
                ),
                "两臂必须达到冻结轮数，禁止丢失轮次后缩窄合同",
            );
            runs_checks(&mut checks, &e.control, "control", &e.policy);
            runs_checks(&mut checks, &e.candidate, "candidate", &e.policy);
        }
    }
    let all_pass = checks.iter().all(|c| c.state == "pass");
    let failed = checks.iter().any(|c| c.state == "fail");
    let status = if e.device.root == Some(false) && e.device.measurement_available == Some(true) {
        "measure_only"
    } else if failed {
        "blocked"
    } else if all_pass {
        "ready"
    } else {
        "unverified"
    };
    let frames = e
        .baseline
        .iter()
        .chain(&e.control)
        .chain(&e.candidate)
        .map(|r| r["frame_evidence"].clone())
        .collect();
    Ok(Report {
        version: 1,
        stage: e.stage,
        status: status.into(),
        probe_allowed,
        experiment_allowed: all_pass && e.stage != Stage::Probe,
        conclusion_allowed: all_pass && e.stage == Stage::Conclusion,
        eligible: all_pass && e.stage == Stage::Conclusion,
        policy: e.policy.clone(),
        checks,
        frame_evidence: frames,
        evidence_origin: e.evidence_origin.clone(),
        control_validation_scope:
            "probe/apply readback only; continuous control effectiveness not measured".into(),
        claim_scope: if !all_pass || e.stage != Stage::Conclusion {
            "none; evidence does not qualify a conclusion".into()
        } else {
            match e.policy.goal {
                Goal::RenderEfficiency => {
                    "render cadence proxy and battery power; no displayed-frame claim"
                }
                Goal::RenderCadence => {
                    "render cadence proxy only; no power improvement or displayed-frame claim"
                }
                Goal::DisplaySmoothness => "verified displayed-frame cadence",
            }
            .into()
        },
    })
}

/// Only read operations. Call before push/probe/layer mutation, never infer root from model identity.
pub fn inspect_device(serial: &str) -> Capabilities {
    let phone = crate::device::Device::new(
        Some(serial.into()),
        std::env::temp_dir().to_string_lossy().into_owned(),
    );
    let backend = phone.backend_name().to_string();
    let root = crate::hwcond::root_ok(&phone);
    let trace = root && phone.shell("su -c 'test -r /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_submitted/format && test -r /sys/kernel/tracing/events/kgsl/adreno_cmdbatch_retired/format && echo TRACE_AVAILABLE'",8_000).contains("TRACE_AVAILABLE");
    let measurement = if backend == "hdc" {
        crate::perfsrc::version_looks_present(&phone.shell("SP_daemon --version", 8_000))
    } else {
        trace
    };
    Capabilities {
        backend,
        root: Some(root),
        trace_available: Some(trace),
        measurement_available: Some(measurement),
        control_verified: None,
    }
}

pub fn read(path: &Path) -> Result<Evidence, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("资格证据不是合法 JSON: {e}"))
}

pub fn write(path: &Path, report: &Report) -> Result<(), String> {
    std::fs::write(
        path,
        serde_json::to_string_pretty(report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub fn run_qualify(args: &[String]) -> i32 {
    let mut it = args.iter();
    let mut path = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" => path = it.next().cloned(),
            "--json" => {}
            _ => {
                eprintln!("用法: phonefarm qualify --from evidence.json --json");
                return 2;
            }
        }
    }
    let Some(path) = path else {
        eprintln!("缺 --from");
        return 2;
    };
    match read(Path::new(&path)).and_then(|e| evaluate(&e)) {
        Ok(r) => {
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            if r.status == "ready" {
                0
            } else {
                1
            }
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn ready() -> Evidence {
        let mut e: Evidence =
            serde_json::from_str(include_str!("../examples/qualification_ready.json")).unwrap();
        e.evidence_origin = "recorded_device".into();
        for r in e
            .baseline
            .iter_mut()
            .chain(&mut e.control)
            .chain(&mut e.candidate)
        {
            r["frame_evidence"]["source"] = json!("raw_ftrace_kgsl");
        }
        e
    }
    #[test]
    fn synthetic_and_unknown_sources_are_not_real_experiment_eligible() {
        let e: Evidence =
            serde_json::from_str(include_str!("../examples/qualification_ready.json")).unwrap();
        let q = evaluate(&e).unwrap();
        assert!(!q.eligible);
        assert!(q.claim_scope.starts_with("none"));
        let mut e = ready();
        e.evidence_origin = "unknown".into();
        assert!(!evaluate(&e).unwrap().eligible);
        let mut e = ready();
        e.candidate[0]["frame_evidence"]["source"] = json!("static_estimate");
        assert!(!evaluate(&e).unwrap().eligible);
    }
    #[test]
    fn complete_evidence_is_ready_and_replayable() {
        let e = ready();
        let r = evaluate(&e).unwrap();
        assert!(r.conclusion_allowed);
        assert_eq!(
            serde_json::to_vec(&r).unwrap(),
            serde_json::to_vec(&evaluate(&e).unwrap()).unwrap()
        );
    }
    #[test]
    fn unknown_is_never_ready() {
        let mut e = ready();
        e.cold_ok = None;
        assert_eq!(evaluate(&e).unwrap().status, "unverified");
    }
    #[test]
    fn harmony_nonroot_is_measure_only() {
        let mut e = ready();
        e.device.root = Some(false);
        e.device.backend = "hdc".into();
        let r = evaluate(&e).unwrap();
        assert_eq!(r.status, "measure_only");
        assert!(!r.probe_allowed);
        assert!(!r.conclusion_allowed);
    }
    #[test]
    fn root_does_not_prove_driver_capability() {
        let mut e = ready();
        e.device.trace_available = Some(false);
        assert!(!evaluate(&e).unwrap().experiment_allowed);
    }
    #[test]
    fn charging_missing_metric_and_failed_restore_block() {
        for n in 0..3 {
            let mut e = ready();
            match n {
                0 => e.candidate[0]["env"]["power_window_verified"] = json!(false),
                1 => e.candidate[0]["summary"]["frame_p95"] = Value::Null,
                _ => e.restored = Some(false),
            }
            assert!(!evaluate(&e).unwrap().conclusion_allowed);
        }
    }
    #[test]
    fn missing_bound_trace_and_failed_workload_are_not_eligible() {
        for n in 0..3 {
            let mut e = ready();
            match n {
                0 => e.candidate[0]["env"]["trace_window_verified"] = json!(false),
                1 => e.candidate[0]["workload_exit"] = json!(1),
                _ => e.candidate[0]["frame_evidence"]["acf_at_selected_period"] = Value::Null,
            }
            assert!(!evaluate(&e).unwrap().eligible);
        }
    }
    #[test]
    fn proxy_is_not_display_or_known_fallback() {
        let mut e = ready();
        e.policy = Policy::new(Goal::DisplaySmoothness, 5, false);
        assert!(!evaluate(&e).unwrap().conclusion_allowed);
        let f = frame_evidence(&json!({"submits_per_frame":1,"spf_scores":{"acf":{"1":0.0}}}));
        assert_eq!(f["confidence"], "unknown");
    }
    #[test]
    fn baseline_drift_in_either_direction_blocks() {
        for sign in [-1.0, 1.0] {
            let mut e = ready();
            for (i, r) in e.baseline.iter_mut().enumerate() {
                r["summary"]["frame_p95"] = json!(20.0 + sign * i as f64 * 0.01);
            }
            assert!(!evaluate(&e).unwrap().conclusion_allowed);
        }
    }
    #[test]
    fn request_thermal_cap_is_replayed_and_unknown_is_not_ready() {
        let mut e = ready();
        e.policy.temp_cap_c = Some(39.0);
        assert!(!evaluate(&e).unwrap().eligible);
        e.policy.temp_cap_c = None;
        assert!(!evaluate(&e).unwrap().eligible);
        e.policy.temp_cap_c = Some(70.0);
        e.policy.cool_c = None;
        assert!(!evaluate(&e).unwrap().eligible);
    }
    #[test]
    fn strict_alpha_requires_reachable_sample_size() {
        let mut e = ready();
        e.policy.alpha = 0.001;
        assert!(!evaluate(&e).unwrap().eligible);
    }
    #[test]
    fn target_metrics_cannot_be_removed_after_freeze() {
        let mut e = ready();
        e.policy.required_metrics.pop();
        assert!(evaluate(&e).is_err());
    }
}
