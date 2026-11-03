//! Read-only application state at task start. Observation errors are data, not admission.
use crate::device::{parse_aa_dump, parse_bm_bundles, Device};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

fn read(phone: &Device, command: &str, deadline: Instant) -> Result<String, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(format!("{command}: observation deadline exceeded"));
    }
    // Device::shell already bounds the transport client and detaches adb/hdc servers.
    let text = phone.shell(
        &format!("{command}\nprintf '\\n__PF_APP_STATUS__%s\\n' \"$?\""),
        remaining.as_millis() as u64,
    );
    let Some((body, status)) = text.rsplit_once("__PF_APP_STATUS__") else {
        return Err(format!(
            "{command}: no completed response (transport unavailable or timed out)"
        ));
    };
    if status.trim() != "0" {
        return Err(format!(
            "{command}: exit {}: {}",
            status.trim(),
            body.trim()
        ));
    }
    Ok(body.trim().to_string())
}

fn running(phone: &Device, harmony: bool, deadline: Instant) -> Result<Vec<String>, String> {
    let processes = read(phone, "ps -A -o NAME", deadline)?;
    if !processes.lines().any(|line| line.trim() == "NAME") {
        return Err("ps: process list header unavailable".into());
    }
    let names: BTreeSet<_> = processes
        .lines()
        .skip(1)
        .map(|line| line.trim().split(':').next().unwrap_or("").to_string())
        .collect();
    if !harmony {
        return Ok(read(phone, "pm list packages -3", deadline)?
            .lines()
            .filter_map(|line| line.strip_prefix("package:"))
            .filter(|name| names.contains(*name))
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect());
    }
    let bundles = parse_bm_bundles(&read(phone, "bm dump -a", deadline)?);
    let mut result = vec![];
    for bundle in bundles.into_iter().filter(|name| names.contains(name)) {
        let text = read(phone, &format!("bm dump -n '{}'", bundle), deadline)?;
        let start = text
            .find('{')
            .ok_or_else(|| format!("bm: no application info for {bundle}"))?;
        let value: Value =
            serde_json::from_str(&text[start..]).map_err(|e| format!("bm {bundle}: {e}"))?;
        // OpenHarmony ApplicationInfo reports the system-app bit; package prefixes do not.
        match value["applicationInfo"]["isSystemApp"].as_bool() {
            Some(false) => result.push(bundle),
            Some(true) => (),
            None => return Err(format!("bm {bundle}: isSystemApp unavailable")),
        }
    }
    Ok(result)
}

pub(crate) fn snapshot(serial: &str) -> Result<Value, String> {
    let tmp = std::env::temp_dir().join(format!(
        "pf-app-snapshot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&tmp).map_err(|e| e.to_string())?;
    let result = crate::observation::run(
        "app-snapshot",
        &[
            "--serial".into(),
            serial.into(),
            "--tmp".into(),
            tmp.to_string_lossy().into(),
        ],
        &crate::lease_storage::resolve(),
        Duration::from_secs(5),
    )
    .map_err(|error| error.reason)
    .and_then(|output| serde_json::from_slice(&output.stdout).map_err(|e| e.to_string()));
    // The supervisor has reaped the observation group, including timed-out clients.
    let _ = std::fs::remove_dir_all(tmp);
    Ok(result.unwrap_or_else(|error| {
        json!({"serial": serial, "foreground": null,
        "running_third_party": null, "errors": [error]})
    }))
}

pub(crate) fn collect(serial: &str, tmp: &str) -> Result<Value, String> {
    let phone = Device::new(Some(serial.into()), tmp.into());
    let deadline = Instant::now() + Duration::from_secs(4);
    let harmony = serial.starts_with("hdc:");
    let mut errors = vec![];
    let foreground = read(
        &phone,
        if harmony {
            "aa dump -a"
        } else {
            "dumpsys window"
        },
        deadline,
    )
    .and_then(|text| {
        let value = if harmony {
            let (name, _) = parse_aa_dump(&text);
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        } else {
            crate::fleet::parse_foreground(&text)
        };
        value.ok_or_else(|| "foreground application unavailable".into())
    })
    .map_err(|e| errors.push(e))
    .ok();
    let running = running(&phone, harmony, deadline)
        .map_err(|e| errors.push(e))
        .ok();
    Ok(
        json!({"serial": serial, "foreground": foreground, "running_third_party": running, "errors": errors}),
    )
}
