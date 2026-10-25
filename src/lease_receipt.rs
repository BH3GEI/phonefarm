//! Job association and scoped caller-reported recovery, never whole-device clearance.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) fn valid_job_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn valid_lease_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) struct JobReceipt {
    path: PathBuf,
    value: Mutex<Value>,
}

impl JobReceipt {
    pub(crate) fn from_env(
        serial: &str,
        label: &str,
        recovery_of: Value,
    ) -> Result<Option<Self>, String> {
        match (
            std::env::var_os("PF_JOB_ID"),
            std::env::var_os("PF_JOB_LEASE_DIR"),
        ) {
            (None, None) => Ok(None),
            (Some(id), Some(dir)) => Self::new(
                id.to_str().ok_or("PF_JOB_ID must be UTF-8")?,
                &PathBuf::from(dir),
                serial,
                label,
                recovery_of,
            )
            .map(Some),
            _ => Err("PF_JOB_ID and PF_JOB_LEASE_DIR must be provided together".into()),
        }
    }

    pub(crate) fn new(
        job_id: &str,
        dir: &Path,
        serial: &str,
        label: &str,
        recovery_of: Value,
    ) -> Result<Self, String> {
        if !valid_job_id(job_id) {
            return Err("PF_JOB_ID must contain exactly 12 lowercase hexadecimal characters".into());
        }
        if !dir.is_absolute()
            || !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
        {
            return Err("PF_JOB_LEASE_DIR must be an existing absolute directory".into());
        }
        // A public correlation ID, generated separately from the ownership token.
        let id = format!(
            "lease-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        );
        Ok(Self {
            path: dir.join(format!("{id}.json")),
            value: Mutex::new(json!({"schema":"phonefarm/job-lease@1", "job_id":job_id,
                "lease_id":id, "serial":serial, "label":label, "state":"active",
                "recovery_of":recovery_of, "recovery":{"status":"not_reported", "reason":null,
                    "context":null, "scope":null, "evidence_origin":null, "evidence":[]},
                "updated_at":chrono::Utc::now().to_rfc3339()})),
        })
    }

    pub(crate) fn bind_marker(&self, marker: &mut Value) -> Result<(), String> {
        let v = self
            .value
            .lock()
            .map_err(|_| "job receipt mutex poisoned")?;
        marker["job_id"] = v["job_id"].clone();
        marker["lease_id"] = v["lease_id"].clone();
        Ok(())
    }

    pub(crate) fn observe(
        &self,
        marker: &Value,
        state: &str,
        uncertainty: Option<&str>,
    ) -> Result<(), String> {
        let reason = uncertainty.or_else(|| {
            if marker["command_pending"] == true {
                Some("command_pending")
            } else if marker["recovery_required"] == true {
                Some("explicit_recovery_barrier")
            } else if marker.get("child_pid").is_some() {
                Some("child_unverified")
            } else {
                None
            }
        });
        self.update(|v| {
            v["state"] = json!(state);
            if let Some(binding) = marker.get("device_binding") {
                v["device_binding"] = binding.clone();
                v["lease_scope"] = json!("device");
            }
            v["recovery"]["context"] = marker["recovery_context"].clone();
            if let Some(reason) = reason {
                v["recovery"] = json!({"status":"required", "reason":reason,
                    "context":marker["recovery_context"], "scope":null, "evidence_origin":null, "evidence":[]});
            } else if v["recovery"]["status"] != "reported_complete" {
                v["recovery"]["status"] = json!("not_reported");
                v["recovery"]["reason"] = Value::Null;
            }
        })
    }

    pub(crate) fn complete(
        &self,
        scope: &str,
        origin: &str,
        evidence: &[&Path],
    ) -> Result<(), String> {
        self.update(|v| {
            v["recovery"]["status"] = json!("reported_complete");
            v["recovery"]["reason"] = Value::Null;
            v["recovery"]["scope"] = json!(scope);
            v["recovery"]["evidence_origin"] = json!(origin);
            v["recovery"]["evidence"] = json!(evidence);
        })
    }

    fn update(&self, update: impl FnOnce(&mut Value)) -> Result<(), String> {
        let mut current = self
            .value
            .lock()
            .map_err(|_| "job receipt mutex poisoned")?;
        let mut value = current.clone();
        update(&mut value);
        value["updated_at"] = json!(chrono::Utc::now().to_rfc3339());
        crate::devlease::durable_replace(&self.path, value.to_string().as_bytes())
            .map_err(|e| format!("job lease receipt {}: {e}", self.path.display()))?;
        *current = value;
        Ok(())
    }
}
