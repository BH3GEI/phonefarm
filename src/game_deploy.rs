//! Finite resource deployment into a new application-owned namespace.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) const REQUEST_SCHEMA: &str = "phonefarm/game-deploy-request@1";
pub(crate) const RECIPE_SCHEMA: &str = "phonefarm/game-resources-recipe@1";
pub(crate) const REPORT_SCHEMA: &str = "phonefarm/game-deployment@1";
pub(crate) const SCOPE: &str = "owned_resource_transaction_settled";
const USAGE: &str = "phonefarm game-deploy capabilities --json\nphonefarm game-deploy --request FILE --serial S --out DIR --json\nphonefarm game-deploy recover --session SOURCE --serial S --out RECOVERY_OUT --json\nAndroid user 0, an already installed matching single APK, and new resource directories only. No APK installation, root, launch, permissions or saved-game replacement.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Resource {
    pub asset_name: String,
    pub path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recipe {
    pub schema: String,
    pub package: String,
    pub version_code: u64,
    pub apk_sha256: String,
    pub files: Vec<Resource>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Asset {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub schema: String,
    pub node_id: String,
    pub game_id: String,
    pub version: String,
    pub serial: String,
    pub mode: String,
    pub recipe: Recipe,
    pub assets: Vec<Asset>,
}

pub(crate) fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
pub(crate) fn valid_resource_path(value: &str) -> bool {
    !value.is_empty()
        && value.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part.len() <= 128
                && !part.starts_with(".phonefarm-")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
fn serial(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.chars().any(char::is_control)
        || value.starts_with("hdc:")
        || value.starts_with("ios:")
    {
        return Err(
            "game-deploy requires an explicit Android serial; other platforms are unsupported"
                .into(),
        );
    }
    Ok(())
}
impl Request {
    pub(crate) fn validate(&self) -> Result<(), String> {
        serial(&self.serial)?;
        if self.schema != REQUEST_SCHEMA
            || !matches!(self.mode.as_str(), "check" | "apply")
            || self.node_id.is_empty()
            || self.node_id.chars().any(char::is_control)
            || !identifier(&self.game_id)
            || !identifier(&self.version)
            || self.recipe.schema != RECIPE_SCHEMA
            || !crate::graylayer::valid_package(&self.recipe.package)
            || self.recipe.version_code == 0
            || !valid_hash(&self.recipe.apk_sha256)
            || !(1..=64).contains(&self.recipe.files.len())
            || !(1..=64).contains(&self.assets.len())
        {
            return Err("invalid game deployment request or recipe".into());
        }
        let mut assets = BTreeMap::new();
        for asset in &self.assets {
            if asset.name.trim().is_empty()
                || asset.name.chars().any(char::is_control)
                || !asset.path.is_absolute()
                || asset
                    .path
                    .components()
                    .any(|p| matches!(p, std::path::Component::ParentDir))
                || !valid_hash(&asset.sha256)
                || assets.insert(asset.name.as_str(), asset).is_some()
            {
                return Err(
                    "deployment assets require unique names, absolute paths and SHA-256".into(),
                );
            }
        }
        let mut paths = BTreeSet::new();
        let mut selected = BTreeSet::new();
        for file in &self.recipe.files {
            if !valid_resource_path(&file.path)
                || !assets.contains_key(file.asset_name.as_str())
                || !paths.insert(file.path.as_str())
            {
                return Err("invalid, duplicate or unresolved deployment resource path".into());
            }
            selected.insert(file.asset_name.as_str());
        }
        for path in &paths {
            if paths
                .iter()
                .any(|other| other.len() > path.len() && other.starts_with(&format!("{path}/")))
            {
                return Err("deployment file paths must not contain another file path".into());
            }
        }
        if selected != assets.keys().copied().collect() {
            return Err("deployment inputs must contain only recipe-selected assets".into());
        }
        Ok(())
    }
    pub(crate) fn asset(&self, name: &str) -> &Asset {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .expect("validated resource")
    }
    pub(crate) fn check_sources(&self) -> Result<(), String> {
        for asset in &self.assets {
            source_file(asset)?;
        }
        Ok(())
    }
}

pub(crate) fn source_file(asset: &Asset) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&asset.path)
        .map_err(|e| format!("source {} unavailable: {e}", asset.name))?;
    let info = file.metadata().map_err(|e| e.to_string())?;
    if !info.is_file() || info.nlink() != 1 || info.len() != asset.size {
        return Err(format!(
            "source {} is not the declared regular file/size",
            asset.name
        ));
    }
    Ok(file)
}

enum Command {
    Run {
        request: PathBuf,
        serial: String,
        out: PathBuf,
    },
    Recover {
        session: PathBuf,
        serial: String,
        out: PathBuf,
    },
    Capabilities,
    Help,
}
fn parse(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Help);
    }
    let recover = args.first().is_some_and(|s| s == "recover");
    let capabilities = args.first().is_some_and(|s| s == "capabilities");
    let (mut request, mut session, mut target, mut out) = (None, None, None, None);
    let mut help = false;
    let mut it = args.iter().skip(usize::from(recover || capabilities));
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => help = true,
            "--json" => {}
            "--request" | "--session" | "--serial" | "--out" if !capabilities => {
                let value = it
                    .next()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--request" => request = Some(PathBuf::from(value)),
                    "--session" => session = Some(PathBuf::from(value)),
                    "--serial" => target = Some(value.clone()),
                    _ => out = Some(PathBuf::from(value)),
                }
            }
            _ => return Err(format!("unknown game-deploy option: {arg}")),
        }
    }
    if help {
        return Ok(Command::Help);
    }
    if capabilities {
        return Ok(Command::Capabilities);
    }
    let target = target.ok_or("game-deploy requires --serial")?;
    serial(&target)?;
    let out = out.ok_or("game-deploy requires a separate --out directory")?;
    if recover && request.is_none() {
        Ok(Command::Recover {
            session: session.ok_or("recover requires --session")?,
            serial: target,
            out,
        })
    } else if !recover && session.is_none() {
        Ok(Command::Run {
            request: request.ok_or("game-deploy requires --request")?,
            serial: target,
            out,
        })
    } else {
        Err("request and recovery context must not be combined".into())
    }
}

/// Parse values before choosing the ordinary queue; recovery owns its retained lease.
pub(crate) fn needs_queue(args: &[String]) -> Result<bool, String> {
    let mut all = vec!["game-deploy".into()];
    all.extend_from_slice(args);
    crate::devlock::strip_lock_args(&mut all)?;
    Ok(matches!(parse(&all[1..])?, Command::Run { .. }))
}
pub(crate) fn error_value(error: &str) -> Value {
    json!({"schema":REPORT_SCHEMA,"phase":"failed","error":error,"session_path":null,
        "observed_at":chrono::Utc::now().to_rfc3339(),"recovery":{"status":"not_reported"}})
}
pub(crate) fn report(request: &Request) -> Value {
    json!({"schema":REPORT_SCHEMA,"node_id":request.node_id,"game_id":request.game_id,
        "version":request.version,"serial":request.serial,"mode":request.mode,"phase":"preflight_failed",
        "observed_at":chrono::Utc::now().to_rfc3339(),"installed":{"status":"unknown"},
        "resources_deployed":{"status":"unknown"},"application_access":{"status":"unknown"},
        "login":{"status":"unknown"},"scene_ready":{"status":"unknown"},"checks":{},
        "error":null,"session_path":null,"recovery":{"status":"not_reported","scope":SCOPE}})
}
pub(crate) fn save_report(out: &Path, report: &Value) -> Result<(), String> {
    crate::devlease::durable_replace(
        &out.join("deployment.json"),
        &serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?,
    )
}
fn output(out: &Path) -> Result<PathBuf, String> {
    crate::shared_access::create_dir_all(out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;
    for name in ["deployment.json", "game-deploy"] {
        match std::fs::symlink_metadata(out.join(name)) {
            Ok(_) => {
                return Err(
                    "deployment output already exists; use a new job/output directory".into(),
                )
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(out)
}
fn execute(command: Command) -> Result<(Value, i32), String> {
    use crate::game_deploy_session::Session;
    match command {
        Command::Capabilities => Ok((
            json!({"schema":"phonefarm/game-deploy-capabilities@1","resources_v1":true}),
            0,
        )),
        Command::Help => {
            println!("{USAGE}");
            Ok((Value::Null, 0))
        }
        Command::Run {
            request,
            serial,
            out,
        } => {
            let request: Request =
                serde_json::from_slice(&crate::confirmation_transport::bounded(&request, 65536)?)
                    .map_err(|e| format!("invalid deployment JSON: {e}"))?;
            request.validate()?;
            if request.serial != serial {
                return Err("request serial differs from explicit target".into());
            }
            let out = output(&out)?;
            let result = request
                .check_sources()
                .and_then(|_| crate::devlease::Lease::target(Some(&serial), "game-deploy"));
            let lease = match result {
                Ok(lease) => lease,
                Err(error) => {
                    let mut value = report(&request);
                    value["error"] = json!(error);
                    save_report(&out, &value)?;
                    return Ok((value, 2));
                }
            };
            match Session::new(lease, request.clone(), &out) {
                Ok(session) => session.execute(),
                Err(error) => {
                    let mut value = report(&request);
                    value["error"] = json!(error);
                    save_report(&out, &value)?;
                    Ok((value, 2))
                }
            }
        }
        Command::Recover {
            session,
            serial,
            out,
        } => {
            let out = output(&out)?;
            Session::recover(&session, &serial, &out)
        }
    }
}
pub(crate) fn run(args: &[String]) -> i32 {
    match parse(args).and_then(execute) {
        Ok((value, code)) => {
            if !value.is_null() {
                println!("{value}");
            }
            code
        }
        Err(error) => {
            println!("{}", error_value(&error));
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recipe_and_cli_keep_paths_and_queue_semantics_explicit() {
        for path in ["base/demo00.pk4", "state/save.game"] {
            assert!(valid_resource_path(path));
        }
        for path in [
            "/base",
            "../base",
            "a//b",
            "a/../b",
            ".phonefarm-owner",
            "base/.phonefarm-x",
        ] {
            assert!(!valid_resource_path(path));
        }
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!needs_queue(&args(&["capabilities", "--json"])).unwrap());
        assert!(!needs_queue(&args(&[
            "recover",
            "--session",
            "old",
            "--serial",
            "S",
            "--out",
            "new"
        ]))
        .unwrap());
        assert!(needs_queue(&args(&[
            "--request",
            "--help",
            "--serial",
            "S",
            "--out",
            "new"
        ]))
        .unwrap());
        assert!(needs_queue(&args(&[
            "--request",
            "x",
            "--serial",
            "hdc:S",
            "--out",
            "new"
        ]))
        .is_err());
        let mut value = json!({"schema":REQUEST_SCHEMA,"node_id":"node","game_id":"doom","version":"v1", "serial":"S","mode":"apply",
            "recipe":{"schema":RECIPE_SCHEMA,"package":"com.example.game","version_code":1,"apk_sha256":"a".repeat(64),
                "files":[{"asset_name":"data","path":"base"},{"asset_name":"data","path":"base/file"}]},
            "assets":[{"name":"data","path":"/cache/blob","size":1,"sha256":"b".repeat(64)}]});
        assert!(serde_json::from_value::<Request>(value.clone())
            .unwrap()
            .validate()
            .is_err());
        value["recipe"]["files"][0]["path"] = json!("other");
        serde_json::from_value::<Request>(value)
            .unwrap()
            .validate()
            .unwrap();
    }
}
