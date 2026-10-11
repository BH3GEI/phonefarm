//! Receiver-controlled reversible graph. Metadata never becomes proof of effective writes.
use crate::{autoloop, sysparam};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub(crate) struct Graph {
    pub candidate: String,
    pub probes: Vec<(String, String)>,
    pub restore: String,
    pub policies: Vec<String>,
}
fn require(ok: bool, s: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(s.into())
    }
}
fn words(s: &str) -> Vec<String> {
    s.replace(',', " ")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}
fn safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn number(s: &str) -> Result<u64, String> {
    s.parse::<u64>()
        .ok()
        .filter(|n| *n <= 1_000_000_000_000)
        .ok_or("unknown/out-of-range numeric control".into())
}
fn binding(
    id: &str,
    probe: &sysparam::ProbeMap,
) -> Result<(String, String, Vec<String>, String), String> {
    let rd = |k: &str| probe.get(k).unwrap_or("").to_owned();
    if let Some(rest) = id.strip_prefix("cpu.policy") {
        let (pol, field) = rest.split_once('.').ok_or("invalid CPU ID")?;
        require(
            !pol.is_empty()
                && pol.bytes().all(|b| b.is_ascii_digit())
                && words(&rd("cpu.policies")).contains(&pol.into()),
            "unknown CPU policy",
        )?;
        let available = match field {
            "scaling_governor" => "avail_governors",
            "scaling_min_freq" | "scaling_max_freq" => "avail_freqs",
            _ => return Err("unsupported CPU field".into()),
        };
        return Ok((
            "sysfs".into(),
            format!("/sys/devices/system/cpu/cpufreq/policy{pol}/{field}"),
            words(&rd(&format!("cpu.policy{pol}.{available}"))),
            rd(&format!("{id}.cur")),
        ));
    }
    if matches!(id, "gpu.min_pwrlevel" | "gpu.max_pwrlevel") {
        let n = number(&rd("gpu.num_pwrlevels"))?;
        require((2..=256).contains(&n), "unknown GPU powerlevel count")?;
        return Ok((
            "sysfs".into(),
            format!("/sys/class/kgsl/kgsl-3d0/{}", id.trim_start_matches("gpu.")),
            (0..n).map(|v| v.to_string()).collect(),
            rd(&format!("{id}.cur")),
        ));
    }
    if let Some(field) = id.strip_prefix("gpu.devfreq.") {
        require(
            rd("gpu.devfreq_path") == "/sys/class/kgsl/kgsl-3d0/devfreq",
            "dynamic devfreq whitelist/snapshot path is not consistently supported",
        )?;
        let available = match field {
            "governor" => "avail_governors",
            "min_freq" | "max_freq" => "avail_freqs",
            _ => return Err("unsupported devfreq field".into()),
        };
        return Ok((
            "sysfs".into(),
            format!("/sys/class/kgsl/kgsl-3d0/devfreq/{field}"),
            words(&rd(&format!("gpu.devfreq.{available}"))),
            rd(&format!("{id}.cur")),
        ));
    }
    for bus in ["DDR", "LLCC"] {
        if id == format!("bus.{bus}.boost_freq") {
            let mut available = words(&rd(&format!("bus.{bus}.avail_freqs")));
            if available.is_empty() {
                available = vec![
                    rd(&format!("bus.{bus}.hw_min_freq")),
                    rd(&format!("bus.{bus}.hw_max_freq")),
                ];
            }
            return Ok((
                "sysfs".into(),
                format!("/sys/devices/system/cpu/bus_dcvs/{bus}/boost_freq"),
                available,
                rd(&format!("{id}.cur")),
            ));
        }
    }
    for key in ["peak_refresh_rate", "min_refresh_rate", "refresh_rate_mode"] {
        if id == format!("setting.system.{key}") {
            let available = if key == "refresh_rate_mode" {
                let table = words(&rd("setting.system.all_refresh_rate"));
                require(
                    (2..=32).contains(&table.len()) && table.iter().all(|v| number(v).is_ok()),
                    "unknown vendor mode table",
                )?;
                // Index semantics remain unverified until differential readback reports actual active FPS.
                // Probe only indices already present as original or explicit receiver request; never enumerate len(table).
                let wanted = rd("driver.requested_vendor_mode");
                require(
                    number(&wanted)? <= 31,
                    "vendor mode challenge outside receiver bound",
                )?;
                vec![rd("setting.system.refresh_rate_mode.cur"), wanted]
            } else {
                sysparam::fps_list(&rd("display.modes"))
                    .into_iter()
                    .map(|v| v.to_string())
                    .collect()
            };
            return Ok((
                "setting".into(),
                format!("system:{key}"),
                available,
                rd(&format!("{id}.cur")),
            ));
        }
    }
    Err("control outside receiver whitelist categories".into())
}
fn siblings(kind: &str, path: &str) -> Vec<(String, String)> {
    let paths = if path.starts_with("/sys/devices/system/cpu/cpufreq/") {
        let base = path.rsplit_once('/').unwrap().0;
        vec![
            format!("{base}/scaling_governor"),
            format!("{base}/scaling_max_freq"),
            format!("{base}/scaling_min_freq"),
        ]
    } else if path.starts_with("/sys/class/kgsl/kgsl-3d0/devfreq/") {
        vec![
            "/sys/class/kgsl/kgsl-3d0/devfreq/governor".into(),
            "/sys/class/kgsl/kgsl-3d0/devfreq/max_freq".into(),
            "/sys/class/kgsl/kgsl-3d0/devfreq/min_freq".into(),
        ]
    } else if matches!(
        path,
        "/sys/class/kgsl/kgsl-3d0/min_pwrlevel" | "/sys/class/kgsl/kgsl-3d0/max_pwrlevel"
    ) {
        vec![
            "/sys/class/kgsl/kgsl-3d0/max_pwrlevel".into(),
            "/sys/class/kgsl/kgsl-3d0/min_pwrlevel".into(),
        ]
    } else if kind == "setting" {
        vec![
            "system:peak_refresh_rate".into(),
            "system:min_refresh_rate".into(),
            "system:refresh_rate_mode".into(),
        ]
    } else {
        vec![path.into()]
    };
    paths.into_iter().map(|p| (kind.into(), p)).collect()
}
fn bounds(
    id: &str,
    value: &str,
    probe: &sysparam::ProbeMap,
    selected: &BTreeMap<String, String>,
) -> Result<(), String> {
    let current = |k: &str| {
        selected
            .get(k)
            .map(String::as_str)
            .or_else(|| probe.get(&format!("{k}.cur")))
            .unwrap_or("")
    };
    for (low, high, inverse) in [
        (".scaling_min_freq", ".scaling_max_freq", false),
        (".min_freq", ".max_freq", false),
        (".min_pwrlevel", ".max_pwrlevel", true),
    ] {
        if let Some(base) = id.strip_suffix(low) {
            let other = format!("{base}{high}");
            let (a, b) = (number(value)?, number(current(&other))?);
            require(
                if inverse { b <= a } else { a <= b },
                "control lower/upper bounds inverted or unknown",
            )?;
        }
        if let Some(base) = id.strip_suffix(high) {
            let other = format!("{base}{low}");
            let (a, b) = (number(current(&other))?, number(value)?);
            require(
                if inverse { b <= a } else { a <= b },
                "control upper/lower bounds inverted or unknown",
            )?;
        }
    }
    if matches!(
        id,
        "setting.system.min_refresh_rate" | "setting.system.peak_refresh_rate"
    ) {
        let val = |k: &str, default: f64| -> Result<f64, String> {
            let v = if k == id { value } else { current(k) };
            if v == "null" {
                Ok(default)
            } else {
                v.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.)
                    .ok_or("unknown refresh sibling bounds".into())
            }
        };
        require(
            val("setting.system.min_refresh_rate", 0.)?
                <= val("setting.system.peak_refresh_rate", 1e9)?,
            "stored refresh min exceeds peak",
        )?;
    }
    Ok(())
}
pub(crate) fn graph(
    artifact: &Value,
    readonly: &str,
    snapshot: &BTreeMap<String, String>,
) -> Result<Graph, String> {
    require(
        artifact["kind"] == "sysparam",
        "driver v1 supports sysparam only",
    )?;
    let params = artifact["payload"]["params"]
        .as_object()
        .ok_or("params must be strings")?;
    require(
        !params.is_empty() && params.len() <= 32,
        "candidate must contain 1..32 params",
    )?;
    let selected: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .map(|v| (k.clone(), v.into()))
                .ok_or("non-string control value".into())
        })
        .collect::<Result<_, String>>()?;
    let requested_vendor = selected
        .get("setting.system.refresh_rate_mode")
        .cloned()
        .unwrap_or_default();
    let augmented = format!("{readonly}\ndriver.requested_vendor_mode={requested_vendor}\n");
    let probe = sysparam::parse_probe(&augmented);
    let mut restore = String::new();
    let mut seen = BTreeSet::new();
    let mut candidate = String::new();
    let mut probes = vec![];
    for (id, value) in &selected {
        require(
            !sysparam::denied(id) && safe(value),
            "forbidden/unsafe control",
        )?;
        let (kind, path, available, original) = binding(id, &probe)?;
        require(
            available.len() >= 2
                && available.len() <= 512
                && available.iter().all(|s| safe(s))
                && available.contains(value),
            "control value absent from known finite capability table",
        )?;
        require(
            safe(&original) && (original != "null" || kind == "setting"),
            "unknown original selected control",
        )?;
        if kind == "setting" {
            require(
                probe.get(&format!("{id}.read_exit")) == Some("0"),
                "setting read receipt missing",
            )?;
        }
        bounds(id, value, &probe, &selected)?;
        let key =
            autoloop::snapshot_key_of(&kind, &path).ok_or("control has no snapshot coverage")?;
        require(
            snapshot.get(&key) == Some(&original),
            "probe original differs from actual snapshot path",
        )?;
        for (skind, sibling) in siblings(&kind, &path) {
            if seen.insert((skind.clone(), sibling.clone())) {
                let skey = autoloop::snapshot_key_of(&skind, &sibling)
                    .ok_or("sibling has no snapshot coverage")?;
                let old = snapshot.get(&skey).ok_or("missing sibling original")?;
                require(
                    safe(old) && old != "<unreadable>",
                    "unknown/unreadable sibling original",
                )?;
                restore.push_str(&format!("{skind}\t{sibling}\t{old}\n"));
            }
        }
        candidate.push_str(&format!("{id}\t{kind}\t{path}\t{value}\n"));
        let trial = if *value != original && bounds(id, value, &probe, &BTreeMap::new()).is_ok() {
            value.clone()
        } else {
            available
                .into_iter()
                .find(|v| v != &original && bounds(id, v, &probe, &BTreeMap::new()).is_ok())
                .ok_or("no legal differential challenge")?
        };
        // Probe one requested node at a time against original sibling bounds.
        bounds(id, &trial, &probe, &BTreeMap::new())?;
        probes.push((id.clone(), format!("{id}\t{kind}\t{path}\t{trial}\n")));
    }
    let policies = words(probe.get("cpu.policies").unwrap_or(""))
        .into_iter()
        .map(|s| format!("policy{s}"))
        .collect();
    Ok(Graph {
        candidate,
        probes,
        restore,
        policies,
    })
}
pub(crate) fn verify_artifact(artifact: &Value, probe: &str) -> Result<(), String> {
    let params: Vec<(String, String)> = artifact["payload"]["params"]
        .as_object()
        .ok_or("missing params")?
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").into()))
        .collect();
    let wl = sysparam::build_whitelist(&sysparam::parse_probe(probe));
    let (ok, why) = sysparam::validate_candidate(&params, &wl);
    require(ok, &why)?;
    let proposed = "# loop_v1 sysparam plan\n".to_owned()
        + &artifact["payload"]["plan"]
            .as_array()
            .ok_or("missing plan")?
            .iter()
            .map(|p| {
                format!(
                    "{}\t{}\t{}\n",
                    p["kind"].as_str().unwrap_or(""),
                    p["path"].as_str().unwrap_or(""),
                    p["value"].as_str().unwrap_or("")
                )
            })
            .collect::<String>();
    require(
        proposed == sysparam::plan_text(&params, &wl)?,
        "candidate plan differs from actual receiver whitelist",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_remote_entry_intent_and_bootstrap_reject_terminal_session() {
        let dir = std::env::temp_dir().join(format!("phonefarm-revoked-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("session.closed")).unwrap();
        let source = include_str!("../loop_v1/tools/confirmation_worker.sh");
        let intent =
            &source[source.find("intent() {").unwrap()..source.find("read_value() {").unwrap()];
        let master = &source[source.find("acquire_master() {").unwrap()
            ..source.find("release_master() {").unwrap()];
        for (functions, call, code) in [
            (intent, "intent write ignored ignored", 86),
            (master, "acquire_master", 1),
        ] {
            let script = format!(
                "D={}\n{functions}\n{call}\n",
                crate::confirmation_transport::quote(dir.to_str().unwrap())
            );
            let o = std::process::Command::new("/bin/sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert_eq!(
                o.status.code(),
                Some(code),
                "{}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        let o = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(include_str!("../loop_v1/tools/confirmation_bootstrap.sh"))
            .arg("bootstrap")
            .arg(&dir)
            .args(["token", "boot"])
            .arg(dir.join("oldrun"))
            .args(["workload", "must-not-execute"])
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(86));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn actual_worker_bounds_reaches_up_down_and_inverse_without_invalid_intermediate() {
        let source = include_str!("../loop_v1/tools/confirmation_worker.sh");
        let functions = &source
            [source.find("target_value() {").unwrap()..source.find("write_graph() {").unwrap()];
        for (oldlow, oldhigh, low, high) in [
            (100, 300, 600, 1000),
            (600, 1000, 100, 300),
            (0, 5, 1, 3),
            (1, 3, 0, 5),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "phonefarm-bounds-{}-{oldlow}-{oldhigh}-{low}-{high}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let lp = dir.join("low");
            let hp = dir.join("high");
            std::fs::write(&lp, oldlow.to_string()).unwrap();
            std::fs::write(&hp, oldhigh.to_string()).unwrap();
            std::fs::write(
                dir.join("restore.tsv"),
                format!(
                    "sysfs\t{}\t{}\nsysfs\t{}\t{}\n",
                    lp.display(),
                    oldlow,
                    hp.display(),
                    oldhigh
                ),
            )
            .unwrap();
            std::fs::write(
                dir.join("plan.tsv"),
                format!(
                    "testlow\tsysfs\t{}\t{}\ntesthigh\tsysfs\t{}\t{}\n",
                    lp.display(),
                    low,
                    hp.display(),
                    high
                ),
            )
            .unwrap();
            let script=format!("set -eu\nD={}; PLAN=\"$D/plan.tsv\"; LOW={}; HIGH={}\nread_value() {{ cat \"$2\"; }}\nwrite_value() {{ if [ \"$2\" = \"$LOW\" ]; then [ \"$3\" -le \"$(cat \"$HIGH\")\" ] || return 1; else [ \"$3\" -ge \"$(cat \"$LOW\")\" ] || return 1; fi; printf '%s' \"$3\" > \"$2\"; }}\n{functions}\nbounds_group \"$LOW\" \"$HIGH\"\n",crate::confirmation_transport::quote(dir.to_str().unwrap()),crate::confirmation_transport::quote(lp.to_str().unwrap()),crate::confirmation_transport::quote(hp.to_str().unwrap()));
            let o = std::process::Command::new("/bin/sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            assert_eq!(std::fs::read_to_string(&lp).unwrap(), low.to_string());
            assert_eq!(std::fs::read_to_string(&hp).unwrap(), high.to_string());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn known_unset_public_setting_requires_actual_read_hold_and_restore() {
        let raw="display.modes=fps=60,fps=120\nsetting.system.peak_refresh_rate.cur=null\nsetting.system.min_refresh_rate.cur=null\n";
        assert!(sysparam::wl_get(
            &sysparam::build_whitelist(&sysparam::parse_probe(raw)),
            "setting.system.peak_refresh_rate"
        )
        .is_none());
        let controlled=format!("{raw}setting.system.peak_refresh_rate.read_exit=0\nsetting.system.peak_refresh_rate.writable=yes\nsetting.system.peak_refresh_rate.effect=live\nsetting.system.peak_refresh_rate.effect_restored=yes\n");
        let wl = sysparam::build_whitelist(&sysparam::parse_probe(&controlled));
        assert_eq!(
            sysparam::wl_get(&wl, "setting.system.peak_refresh_rate")
                .unwrap()
                .current,
            "null"
        );
        assert!(
            sysparam::validate_candidate(
                &[("setting.system.peak_refresh_rate".into(), "60".into())],
                &wl
            )
            .0
        );
    }
    #[test]
    fn canonical_devfreq_has_every_restoration_sibling() {
        assert_eq!(
            autoloop::snapshot_key_of("sysfs", "/sys/class/kgsl/kgsl-3d0/devfreq/min_freq")
                .as_deref(),
            Some("kgsl.devfreq_min_freq")
        );
        let paths = siblings("sysfs", "/sys/class/kgsl/kgsl-3d0/devfreq/min_freq");
        assert_eq!(paths.len(), 3);
        assert!(paths
            .iter()
            .all(|(k, p)| autoloop::snapshot_key_of(k, p).is_some()));
    }
}
