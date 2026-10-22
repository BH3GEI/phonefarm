//! Finite frame-audit capture: original property values, owned files and target exit.
use crate::{
    confirmation_transport::{bounded, journal, new_file, quote as q},
    devlease::{durable_replace, Lease},
    graylayer::{self, LAYER_NAME, LAYER_PROPERTIES, SO, STATE},
};
use serde_json::{json, Map, Value};
use std::{cell::RefCell, collections::HashSet, fs, path::{Path, PathBuf}, time::Duration};

const SCHEMA: &str = "phonefarm/gray-capture-session@1";
const SCOPE: &str = "gray_capture_owned_installation_properties_target_stopped";
const CAP: u64 = 8 * 1024 * 1024;

pub(crate) struct Session {
    lease: RefCell<Lease>,
    adb: PathBuf,
    pub dir: PathBuf,
    state: RefCell<Value>,
    // A failed device check stops this attempt. A fresh recovery owner can retry
    // known completed operations; persistent pending/uncertain writes cannot.
    stopped: RefCell<Option<String>>,
    known_dirs: RefCell<HashSet<String>>,
}

impl Session {
    pub fn new(lease: Lease, out: &Path, pkg: &str, origin: &str) -> Result<Self, String> {
        if !graylayer::valid_package(pkg) || !matches!(origin, "device" | "synthetic") {
            return Err("invalid gray capture package or evidence origin".into());
        }
        fs::create_dir_all(out).map_err(|e| e.to_string())?;
        let token = lease.token().to_owned();
        let dir = out.canonicalize().map_err(|e| e.to_string())?.join(format!("gray-{token}"));
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        for parent in dir.ancestors() {
            fs::File::open(parent).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
        }
        let state = json!({"schema":SCHEMA,"serial":lease.serial,"token":token,"package":pkg,
            "stage":"new","boot_id":null,"native_dir":null,"properties":{},"files":[],
            "remote":format!("/data/local/tmp/phonefarm-gray-{token}"),
            "staging_created":false,"installation_owned":false,"state_owned":false,
            "write_pending":null,"transport_uncertain":false,"evidence_origin":origin,
            "created_at":chrono::Utc::now().to_rfc3339()});
        let session = Self { lease:RefCell::new(lease), adb:adb_path()?, dir,
            state:RefCell::new(state), stopped:RefCell::new(None), known_dirs:RefCell::new(HashSet::new()) };
        session.save()?;
        Ok(session)
    }

    pub fn resume(dir: &Path, serial: &str) -> Result<Self, String> {
        let dir = dir.canonicalize().map_err(|e| e.to_string())?;
        let state: Value = serde_json::from_slice(&bounded(&dir.join("session.json"), 1024 * 1024)?).map_err(|e| e.to_string())?;
        validate(&state, &dir, serial)?;
        pending_clear(&state)?;
        let (root, legacy) = crate::lease_storage::resolve().require_ready()?;
        let lease = Lease::acquire_recovery(&root, &legacy, serial, &dir, Duration::from_secs(1))?;
        Ok(Self {lease:RefCell::new(lease), adb:adb_path()?, dir, state:RefCell::new(state),
            stopped:RefCell::new(None), known_dirs:RefCell::new(HashSet::new())})
    }

    fn save(&self) -> Result<(), String> {
        durable_replace(&self.dir.join("session.json"), &serde_json::to_vec_pretty(&*self.state.borrow()).map_err(|e| e.to_string())?)
    }
    fn hold(&self) -> Result<(), String> { self.lease.borrow_mut().hold_for_recovery(&self.dir) }
    fn package(&self) -> String { self.state.borrow()["package"].as_str().unwrap().into() }
    fn remote(&self) -> String { self.state.borrow()["remote"].as_str().unwrap().into() }
    fn ready(&self) -> Result<(), String> {
        pending_clear(&self.state.borrow())?;
        if let Some(error) = &*self.stopped.borrow() { return Err(error.clone()); }
        Ok(())
    }
    fn stop<T>(&self, error: String) -> Result<T, String> {
        if self.stopped.borrow().is_none() { *self.stopped.borrow_mut() = Some(error.clone()); }
        Err(error)
    }

    fn command(&self, operation: &str, args: &[String], timeout: u64) -> Result<crate::lease_command::Output, String> {
        let args: Vec<String> = [vec!["-s".into(), self.lease.borrow().serial.clone()], args.to_vec()].concat();
        journal(&self.dir, json!({"event":"command_intent","operation":operation,"args":args}))?;
        let result = crate::lease_command::run(&self.lease.borrow(), &self.adb, &args,
            &self.dir.join("commands"), operation, Duration::from_millis(timeout));
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                self.state.borrow_mut()["transport_uncertain"] = json!(true);
                let saved = self.save();
                let held = self.hold();
                return self.stop(format!("{operation}: {error}; uncertainty_saved={}; hold_saved={}", saved.is_ok(), held.is_ok()));
            }
        };
        // These raw transport files exist independently of later result JSON.
        journal(&self.dir, json!({"event":"command_result","operation":operation,
            "code":output.code,"stdout_path":output.stdout_path}))?;
        if output.code != 0 {
            return self.stop(format!("{operation}: exit {}: {}", output.code, output.stderr.trim()));
        }
        Ok(output)
    }
    fn read(&self, operation: &str, command: &str) -> Result<String, String> {
        self.ready()?;
        Ok(self.command(operation, &["shell".into(), command.into()], 30_000)?.stdout)
    }
    fn root_read(&self, operation: &str, command: &str) -> Result<String, String> {
        self.read(operation, &format!("su -c {}", q(command)))
    }
    fn write_update(&self, operation: &str, args: &[String], update: impl FnOnce(&mut Value)) -> Result<(), String> {
        self.ready()?;
        self.hold()?;
        let pending = json!({"operation":operation,"args":args});
        self.state.borrow_mut()["write_pending"] = pending.clone();
        self.save()?;
        self.command(operation, args, 120_000)?;
        {
            let mut state = self.state.borrow_mut();
            update(&mut state);
            state["write_pending"] = Value::Null;
        }
        if let Err(error) = self.save() {
            self.state.borrow_mut()["write_pending"] = pending;
            return self.stop(format!("write completion persistence failed: {error}"));
        }
        Ok(())
    }
    fn write(&self, operation: &str, command: &str) -> Result<(), String> {
        self.write_update(operation, &["shell".into(), command.into()], |_| {})
    }
    fn root_write(&self, operation: &str, command: &str) -> Result<(), String> {
        self.write(operation, &format!("su -c {}", q(command)))
    }
    fn root_update(&self, operation: &str, command: &str, update: impl FnOnce(&mut Value)) -> Result<(), String> {
        self.write_update(operation, &["shell".into(), format!("su -c {}", q(command))], update)
    }

    // A missing directory is proved through a successful ancestor listing, not
    // a failed ls/test. Positive directory knowledge only avoids repeated walks;
    // every actual absence decision still uses a fresh successful listing.
    fn entries(&self, dir: &str) -> Result<Option<Vec<String>>, String> {
        if dir != "/" && !self.known_dirs.borrow().contains(dir) {
            let (parent, name) = split_path(dir)?;
            if self.entries(&parent)?.is_none_or(|names| !names.iter().any(|n| n == &name)) {
                return Ok(None);
            }
        }
        let output = self.root_read("directory_listing", &format!("ls -1A -- {}", q(dir)))?;
        self.known_dirs.borrow_mut().insert(dir.into());
        Ok(Some(output.lines().map(str::to_owned).collect()))
    }
    fn present(&self, path: &str) -> Result<bool, String> {
        let (parent, name) = split_path(path)?;
        Ok(self.entries(&parent)?.is_some_and(|names| names.iter().any(|n| n == &name)))
    }
    fn expect_presence(&self, path: &str, expected: bool) -> Result<(), String> {
        if self.present(path)? != expected {
            return self.stop(format!("file presence readback differs: {path}, expected_present={expected}"));
        }
        Ok(())
    }
    fn native_dir(&self) -> Result<String, String> {
        let dump = self.read("native_directory", &format!("dumpsys package {}", q(&self.package())))?;
        let paths: Vec<_> = dump.lines().filter_map(|l| l.trim().strip_prefix("legacyNativeLibraryDir=")).collect();
        if paths.len() != 1 || !graylayer::valid_install_dir(paths[0]) {
            return self.stop("unrecognized current native library directory".into());
        }
        let dir = format!("{}/arm64", paths[0]);
        if self.entries(&dir)?.is_none() { return self.stop("target has no readable arm64 native library directory".into()); }
        Ok(dir)
    }
    fn property(&self, property: &str) -> Result<String, String> {
        let value = self.read("property_readback", &format!("getprop {}", q(property)))?;
        let value = value.strip_suffix('\n').unwrap_or(&value).to_owned();
        if value.contains(['\n', '\r', '\0']) || value.len() > graylayer::PROP_VALUE_MAX {
            return self.stop(format!("invalid property readback: {property}"));
        }
        Ok(value)
    }
    fn set_property(&self, property: &str, value: &str) -> Result<(), String> {
        self.root_write("property_write", &format!("setprop {} {}", q(property), q(value)))?;
        if self.property(property)? != value {
            return self.stop(format!("property readback differs: {property}"));
        }
        Ok(())
    }
    fn stop_target(&self) -> Result<(), String> {
        let package = self.package();
        self.write("stop_target", &format!("am force-stop {}", q(&package)))?;
        let ps = self.root_read("target_readback", "ps -A -o NAME")?;
        let mut names = ps.lines().map(str::trim).filter(|s| !s.is_empty());
        if names.next() != Some("NAME") || names.any(|name| name == package || name.starts_with(&format!("{package}:"))) {
            return self.stop("target process absence not verified".into());
        }
        Ok(())
    }

    pub fn prepare(&self) -> Result<(), String> {
        self.ready()?;
        if self.state.borrow()["stage"] != "new" { return Err("gray capture already prepared".into()); }
        let boot = self.read("boot_identity", "cat /proc/sys/kernel/random/boot_id")?;
        let boot = boot.trim();
        if boot.is_empty() || !boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return self.stop("invalid device boot identity".into());
        }
        let native = self.native_dir()?;
        for path in [STATE.to_string(), format!("{native}/{SO}"), self.remote()] {
            if self.present(&path)? { return self.stop(format!("pre-existing installation/session path; refusing overwrite: {path}")); }
        }
        let mut properties = Map::new();
        for property in LAYER_PROPERTIES {
            properties.insert(property.into(), json!(self.property(property)?));
        }
        if properties["debug.vulkan.layers"] != "" {
            return self.stop("global Vulkan layer is already enabled; reconcile it before capture".into());
        }
        let token = self.state.borrow()["token"].as_str().unwrap().to_owned();
        let mut files = Vec::new();
        for path in output_paths(&self.package()) {
            let (parent, name) = split_path(&path)?;
            let backup = format!("{parent}/.phonefarm-gray-{token}-{name}.bak");
            if self.present(&backup)? { return self.stop(format!("backup already exists: {backup}")); }
            files.push(json!({"path":path,"backup":backup,"original_present":self.present(&path)?,
                "acquired":false,"backup_done":false,"restored":false,"archive":null}));
        }
        let mut state = self.state.borrow_mut();
        state["boot_id"] = json!(boot); state["native_dir"] = json!(native);
        state["properties"] = Value::Object(properties); state["files"] = json!(files);
        state["stage"] = json!("prepared");
        drop(state);
        self.save()
    }

    pub fn install(&self, so: Option<&str>, ts: bool, extras: Option<&str>) -> Result<(), String> {
        self.ready()?;
        if self.state.borrow()["stage"] != "prepared" { return Err("gray capture must be prepared before install".into()); }
        let local = so.map(str::to_owned).unwrap_or_else(graylayer::layer_so_path);
        let local = crate::eval::resolve_path(&local).ok_or("layer SO not found")?;
        let metadata = fs::metadata(&local).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() == 0 { return Err("layer SO must be a nonempty regular file".into()); }
        let extras = intent_extras(extras.unwrap_or(""))?;
        let activity = self.read("resolve_activity", &format!("cmd package resolve-activity --brief {}", q(&self.package())))?;
        let component = activity.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if component.split_once('/').is_none_or(|(pkg, class)| pkg != self.package() || class.is_empty()
            || !class.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'$'))) {
            return Err("could not resolve a launch component for the target package".into());
        }
        self.hold()?;
        self.state.borrow_mut()["stage"] = json!("active");
        if let Err(error) = self.save() { return self.stop(format!("active intent persistence failed: {error}")); }
        self.stop_target()?;
        let files = self.state.borrow()["files"].as_array().unwrap().clone();
        for (index, file) in files.iter().enumerate() {
            let path = file["path"].as_str().unwrap();
            let backup = file["backup"].as_str().unwrap();
            self.expect_presence(path, file["original_present"] == true)?;
            self.expect_presence(backup, false)?;
            if file["original_present"] == true {
                self.root_update("backup_history", &format!("mv -- {} {}", q(path), q(backup)), |state| {
                    state["files"][index]["backup_done"] = json!(true);
                    state["files"][index]["acquired"] = json!(true);
                })?;
                self.expect_presence(path, false)?; self.expect_presence(backup, true)?;
            } else {
                // Preparation-time absence alone does not establish ownership.
                // Acquire only after the target stopped and this fresh check.
                self.state.borrow_mut()["files"][index]["acquired"] = json!(true);
                if let Err(error) = self.save() { return self.stop(format!("output ownership persistence failed: {error}")); }
            }
        }
        let remote = self.remote();
        // adb push runs as shell; a root-owned 0755 directory would reject it.
        self.write_update("create_staging", &["shell".into(), format!("mkdir -- {}", q(&remote))], |s| s["staging_created"] = json!(true))?;
        self.write_update("push_layer", &["push".into(), local.to_string_lossy().into(), format!("{remote}/{SO}")], |_| {})?;
        let installed = format!("{}/{SO}", self.state.borrow()["native_dir"].as_str().unwrap());
        self.root_update("install_layer", &format!("cp -- {} {}", q(&format!("{remote}/{SO}")), q(&installed)), |s| s["installation_owned"] = json!(true))?;
        self.root_write("layer_mode", &format!("chmod 755 {}", q(&installed)))?;
        self.root_write("layer_owner", &format!("chown system:system {}", q(&installed)))?;
        self.root_write("layer_context", &format!("chcon u:object_r:apk_data_file:s0 {}", q(&installed)))?;
        self.expect_presence(&installed, true)?;
        let mode = self.root_read("layer_metadata", &format!("stat -c '%a %u %g' {}", q(&installed)))?;
        let context = self.root_read("layer_context_readback", &format!("ls -Zd -- {}", q(&installed)))?;
        if mode.trim() != "755 1000 1000" || !context.split_whitespace().any(|s| s == "u:object_r:apk_data_file:s0") {
            return self.stop("installed layer mode/owner/context readback differs".into());
        }
        let registration = format!("{}|{}", self.package(), self.state.borrow()["native_dir"].as_str().unwrap());
        self.root_update("register_layer", &format!("printf '%s\\n' {} > {}", q(&registration), q(STATE)), |s| s["state_owned"] = json!(true))?;
        if self.root_read("registration_readback", &format!("cat {}", q(STATE)))?.trim_end_matches('\n') != registration {
            return self.stop("layer registration readback differs".into());
        }
        for property in LAYER_PROPERTIES.into_iter().filter(|p| *p != "debug.vulkan.layers") {
            let value = match property { "debug.knobs.passdump" | "debug.knobs.frameaudit" => "1", "debug.knobs.frameaudit_ts" if ts => "1", _ => "0" };
            self.set_property(property, value)?;
        }
        self.set_property("debug.vulkan.layers", LAYER_NAME)?;
        let extras = extras.iter().map(|arg| q(arg)).collect::<Vec<_>>().join(" ");
        self.write("launch_target", &format!("am start -n {} {extras}", q(component)))
    }

    pub fn narrow(&self) -> Result<(), String> { self.set_property("debug.vulkan.layers", "") }
    pub fn trigger(&self, go: u32) -> Result<(), String> { self.set_property(graylayer::FA_GO_PROP, &go.to_string()) }
    pub fn tap(&self, x: u32, y: u32) -> Result<(), String> { self.write("tap", &format!("input tap {x} {y}")) }

    fn json_output(&self, audit: bool, go: Option<u32>) -> Result<Option<Value>, String> {
        for path in output_paths(&self.package()).into_iter().filter(|p| p.contains("knobs_frameaudit") == audit) {
            if !self.present(&path)? { continue; }
            let text = self.root_read("capture_json", &format!("cat {}", q(&path)))?;
            let value: Value = serde_json::from_str(&text).map_err(|e| format!("invalid capture JSON at {path}: {e}"))?;
            if go.is_none_or(|expected| value["go"].as_u64() == Some(expected.into())) { return Ok(Some(value)); }
        }
        Ok(None)
    }
    pub fn marker(&self) -> Result<Option<Value>, String> { self.json_output(false, None) }
    pub fn audit(&self, go: u32) -> Result<Option<Value>, String> { self.json_output(true, Some(go)) }
    pub fn screenshot(&self, dst: &Path) -> Result<(), String> {
        self.ready()?;
        let output = self.command("screenshot", &["exec-out".into(), "screencap".into(), "-p".into()], 30_000)?;
        let bytes = bounded(&output.stdout_path, CAP)?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return Err("screenshot is not a PNG".into()); }
        new_file(dst, &bytes)
    }

    fn archive(&self, index: usize, path: &str) -> Result<(), String> {
        let prior = self.state.borrow()["files"][index]["archive"].as_str().map(str::to_owned);
        if let Some(prior) = prior {
            let meta = fs::symlink_metadata(&prior).map_err(|e| format!("recorded archive unavailable: {e}"))?;
            if !meta.is_file() || meta.len() > CAP { return Err("recorded archive is not a bounded regular file".into()); }
            bounded(Path::new(&prior), CAP).map_err(|e| format!("recorded archive unreadable: {e}"))?;
            return Ok(());
        }
        let output = self.command("archive_output", &["shell".into(), format!("su -c {}", q(&format!("cat {}", q(path))))], 30_000)?;
        let name = format!("output-{index}-{}-{}.raw", std::process::id(), stamp()?);
        let destination = self.dir.join(name);
        new_file(&destination, &bounded(&output.stdout_path, CAP)?)?;
        self.state.borrow_mut()["files"][index]["archive"] = json!(destination);
        self.save()
    }
    fn restore_file(&self, index: usize) -> Result<(), String> {
        let file = self.state.borrow()["files"][index].clone();
        if file["acquired"] != true { return Ok(()); }
        let path = file["path"].as_str().unwrap(); let backup = file["backup"].as_str().unwrap();
        let original = file["original_present"] == true;
        if file["restored"] == true {
            self.expect_presence(path, original)?; self.expect_presence(backup, false)?;
            return Ok(());
        }
        self.expect_presence(backup, original)?;
        if self.present(path)? {
            self.archive(index, path)?;
            self.root_write("remove_capture_output", &format!("rm -f -- {}", q(path)))?;
            self.expect_presence(path, false)?;
        }
        if original {
            self.root_update("restore_history", &format!("mv -- {} {}", q(backup), q(path)), |s| s["files"][index]["restored"] = json!(true))?;
        } else {
            self.state.borrow_mut()["files"][index]["restored"] = json!(true); self.save()?;
        }
        self.expect_presence(path, original)?; self.expect_presence(backup, false)
    }
    fn restore(&self) -> Result<Value, String> {
        validate(&self.state.borrow(), &self.dir, &self.lease.borrow().serial)?;
        pending_clear(&self.state.borrow())?;
        if self.state.borrow()["stage"] != "active" {
            return Ok(json!({"scope":"no_device_mutations","no_device_mutations":true,"target_stopped":null,"property_values_restored":null}));
        }
        self.ready()?;
        let boot = self.read("recovery_boot", "cat /proc/sys/kernel/random/boot_id")?;
        if self.state.borrow()["boot_id"].as_str() != Some(boot.trim()) { return self.stop("device boot changed; original values cannot be applied automatically".into()); }
        let expected_native = self.state.borrow()["native_dir"].as_str().ok_or("missing native directory")?.to_owned();
        if expected_native != self.native_dir()? { return self.stop("target installation directory changed; recovery refused".into()); }
        let state_owned = self.state.borrow()["state_owned"] == true;
        if self.present(STATE)? {
            if !state_owned { return self.stop("unowned layer registration appeared; recovery refused".into()); }
            self.check_registration()?;
        }
        self.stop_target()?;
        self.narrow()?;
        let mut local_errors = Vec::new();
        let count = self.state.borrow()["files"].as_array().unwrap().len();
        for index in 0..count {
            if let Err(error) = self.restore_file(index) {
                // Artifact storage errors need not prevent other known-safe
                // cleanup. Device/identity uncertainty always stops immediately.
                self.ready()?;
                local_errors.push(error);
            }
        }
        let installed = format!("{}/{SO}", self.state.borrow()["native_dir"].as_str().unwrap());
        if self.state.borrow()["installation_owned"] == true {
            self.root_write("remove_layer", &format!("rm -f -- {}", q(&installed)))?;
            self.expect_presence(&installed, false)?;
        }
        if state_owned && self.present(STATE)? {
            self.check_registration()?;
            self.root_write("remove_registration", &format!("rm -f -- {}", q(STATE)))?;
        }
        self.expect_presence(STATE, false)?;
        let staging_created = self.state.borrow()["staging_created"] == true;
        if staging_created && self.present(&self.remote())? {
            self.root_write("remove_staging_layer", &format!("rm -f -- {}", q(&format!("{}/{SO}", self.remote()))))?;
            let entries = self.entries(&self.remote())?.ok_or("staging directory disappeared during recovery")?;
            if !entries.is_empty() { return self.stop("unexpected staging files; directory preserved".into()); }
            self.root_write("remove_staging_directory", &format!("rmdir -- {}", q(&self.remote())))?;
        }
        self.expect_presence(&self.remote(), false)?;
        let originals = self.state.borrow()["properties"].clone();
        for property in LAYER_PROPERTIES { self.set_property(property, originals[property].as_str().ok_or("missing original property")?)?; }
        if !local_errors.is_empty() { return Err(local_errors.join("; ")); }
        Ok(json!({"scope":SCOPE,"target_stopped":true,
            "app_check":"force-stop succeeded; exact package/package:child names absent from ps NAME",
            "property_values_restored":true,"owned_installation_absent":true,
            "acquired_historical_files_restored":true,"owned_staging_absent":true,"game_state_restored":false}))
    }

    fn check_registration(&self) -> Result<(), String> {
        let registration = self.root_read("recovery_registration", &format!("cat {}", q(STATE)))?;
        let expected = format!("{}|{}", self.package(), self.state.borrow()["native_dir"].as_str().unwrap());
        if registration.trim_end_matches('\n') != expected {
            return self.stop("layer registration no longer matches this session".into());
        }
        Ok(())
    }

    pub fn finish(&self, error: Option<&str>) -> Result<Value, String> {
        let checks = self.restore();
        let scope = checks.as_ref().ok().and_then(|v| v["scope"].as_str()).unwrap_or(SCOPE).to_owned();
        let report_path = self.dir.join(format!("recovery-{}-{}.json", std::process::id(), stamp()?));
        let mut value = json!({"schema":"phonefarm/gray-capture-recovery@1","status":if checks.is_ok(){"verified"}else{"required"},
            "scope":scope,"serial":self.lease.borrow().serial,"package":self.package(),"session":self.dir,
            "checks":checks.as_ref().ok(),"error":checks.as_ref().err(),"experiment_error":error,
            "evidence_origin":self.state.borrow()["evidence_origin"],"game_state_restored":false,
            "report_path":report_path,"at":chrono::Utc::now().to_rfc3339()});
        if checks.is_err() { self.hold()?; }
        new_file(&report_path, &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)?;
        if checks.is_ok() {
            self.lease.borrow_mut().complete_recovery(&scope, self.state.borrow()["evidence_origin"].as_str().unwrap(), &[&report_path])?;
            value["status"] = json!("reported_complete");
        }
        Ok(value)
    }
}

fn adb_path() -> Result<PathBuf, String> { crate::device::locate_adb().map(PathBuf::from).ok_or("ADB not available".into()) }
fn stamp() -> Result<i64, String> { chrono::Utc::now().timestamp_nanos_opt().ok_or("timestamp unavailable".into()) }
fn split_path(path: &str) -> Result<(String, String), String> {
    if !graylayer::valid_install_dir(path) { return Err("invalid absolute remote path".into()); }
    let (parent, name) = path.rsplit_once('/').ok_or("remote path has no parent")?;
    Ok((if parent.is_empty() { "/" } else { parent }.into(), name.into()))
}
fn output_paths(package: &str) -> Vec<String> {
    ["knobs_layer_out", "knobs_frameaudit"].into_iter().flat_map(|name| [
        format!("/storage/emulated/0/Android/data/{package}/files/{name}.json"),
        format!("/data/data/{package}/{name}.json"),
        format!("/data/local/tmp/{name}.{package}.json"),
    ]).collect()
}
fn pending_clear(state: &Value) -> Result<(), String> {
    if state["transport_uncertain"] != false || state.get("write_pending") != Some(&Value::Null) {
        Err("gray capture has an unresolved command/write; independent reconciliation is required before further device operations".into())
    } else { Ok(()) }
}

fn validate(state: &Value, dir: &Path, serial: &str) -> Result<(), String> {
    let token = state["token"].as_str().ok_or("missing gray session token")?;
    let package = state["package"].as_str().ok_or("missing gray session package")?;
    if state["schema"] != SCHEMA || state["serial"] != serial || !graylayer::valid_package(package)
        || token.is_empty() || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || dir.file_name().and_then(|s| s.to_str()) != Some(format!("gray-{token}").as_str())
        || state["remote"] != format!("/data/local/tmp/phonefarm-gray-{token}")
        || !matches!(state["stage"].as_str(), Some("new" | "prepared" | "active"))
        || !matches!(state["evidence_origin"].as_str(), Some("device" | "synthetic"))
        || state.get("write_pending").is_none()
        || !state["transport_uncertain"].is_boolean()
        || ["staging_created", "installation_owned", "state_owned"].iter().any(|key| !state[*key].is_boolean()) {
        return Err("invalid or mismatched gray session identity".into());
    }
    if state["stage"] != "active" && ["staging_created", "installation_owned", "state_owned"].iter().any(|key| state[*key] == true) {
        return Err("nonactive gray session contains device mutations".into());
    }
    if state["stage"] == "new" && (!state["boot_id"].is_null() || !state["native_dir"].is_null()
        || state["files"].as_array().is_none_or(|files| !files.is_empty())
        || state["properties"].as_object().is_none_or(|properties| !properties.is_empty())) {
        return Err("unprepared gray session contains partial baseline state".into());
    }
    if state["stage"] != "new" {
        if state["boot_id"].as_str().is_none_or(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'))
            || state["native_dir"].as_str().is_none_or(|s| !graylayer::valid_install_dir(s))
            || state["properties"].as_object().is_none_or(|m| m.len() != LAYER_PROPERTIES.len())
            || LAYER_PROPERTIES.iter().any(|p| state["properties"][*p].as_str().is_none_or(|s| s.len() > graylayer::PROP_VALUE_MAX || s.contains(['\n', '\r', '\0'])))
            || state["properties"]["debug.vulkan.layers"] != "" {
            return Err("invalid gray session original values".into());
        }
        let files = state["files"].as_array().ok_or("missing gray session files")?;
        let paths = output_paths(package);
        if files.len() != paths.len() { return Err("invalid gray session file inventory".into()); }
        for (file, path) in files.iter().zip(paths) {
            let (parent, name) = split_path(&path)?;
            if file["path"] != path || file["backup"] != format!("{parent}/.phonefarm-gray-{token}-{name}.bak")
                || ["original_present", "acquired", "backup_done", "restored"].iter().any(|key| !file[*key].is_boolean())
                || (file["backup_done"] == true && (file["acquired"] != true || file["original_present"] != true))
                || (file["restored"] == true && file["acquired"] != true)
                || (file["acquired"] == true && file["original_present"] == true && file["backup_done"] != true)
                || (state["stage"] == "prepared" && (file["acquired"] == true || file["backup_done"] == true || file["restored"] == true)) {
                return Err("invalid gray session file ownership".into());
            }
            if !file["archive"].is_null() && file["archive"].as_str().is_none_or(|p| Path::new(p).parent() != Some(dir)) {
                return Err("invalid gray session output archive".into());
            }
        }
    }
    Ok(())
}

// Intent extras are argv, never a shell fragment. Quotes/backslashes only group
// literal values; substitutions and shell operators have no execution meaning.
fn intent_extras(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new(); let mut word = String::new();
    let mut quote = None; let mut escape = false; let mut started = false;
    for ch in text.chars() {
        if ch == '\0' || ch == '\n' || ch == '\r' { return Err("control character in intent extras".into()); }
        if escape { word.push(ch); escape = false; started = true; continue; }
        if ch == '\\' && quote != Some('\'') { escape = true; started = true; continue; }
        if quote == Some(ch) { quote = None; continue; }
        if quote.is_none() && matches!(ch, '\'' | '"') { quote = Some(ch); started = true; continue; }
        if quote.is_none() && ch.is_whitespace() {
            if started { words.push(std::mem::take(&mut word)); started = false; }
        } else { word.push(ch); started = true; }
    }
    if quote.is_some() || escape { return Err("unfinished quoting in intent extras".into()); }
    if started { words.push(word); }
    let mut cursor = 0;
    while cursor < words.len() {
        let flag = words[cursor].as_str();
        let count = match flag { "--esn" => 2, "--es" | "--ez" | "--ei" | "--el" | "--ef" | "--eu" | "--ecn" => 3,
            _ => return Err(format!("unsupported intent extra {flag}; use typed scalar --es/--ez/--ei/--el/--ef/--eu/--ecn/--esn")) };
        if cursor + count > words.len() || words[cursor + 1].is_empty() { return Err("incomplete intent extra".into()); }
        if count == 3 {
            let value = &words[cursor + 2];
            let valid = match flag { "--ez" => matches!(value.as_str(), "true" | "false"), "--ei" => value.parse::<i32>().is_ok(),
                "--el" => value.parse::<i64>().is_ok(), "--ef" => value.parse::<f32>().is_ok_and(f32::is_finite), _ => true };
            if !valid { return Err(format!("invalid value for intent extra {flag}")); }
        }
        cursor += count;
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intent_extras_are_literal_typed_arguments() {
        let words = intent_extras("--es scene 'city static' --es literal '$(id);x' --ei count 3 --es empty \"\"").unwrap();
        assert_eq!(words, ["--es", "scene", "city static", "--es", "literal", "$(id);x", "--ei", "count", "3", "--es", "empty", ""]);
        for bad in ["--es x 'unterminated", "--ei n nope", "--ez flag yes", "-n other/Activity", "--es only-key"] {
            assert!(intent_extras(bad).is_err());
        }
    }
    #[test]
    fn pending_write_or_uncertain_transport_is_not_a_recoverable_readback_failure() {
        assert!(pending_clear(&json!({"write_pending":null,"transport_uncertain":false})).is_ok());
        assert!(pending_clear(&json!({"write_pending":{"operation":"setprop"},"transport_uncertain":false})).is_err());
        assert!(pending_clear(&json!({"write_pending":null,"transport_uncertain":true})).is_err());
        assert!(pending_clear(&json!({})).is_err());
    }
}
