//! Capture's device adapter inside an already authorized, bounded worker group.
//! Closing local input is not proof that Android removed the input device.
use crate::gamepad::Gamepad;
use crate::plugins::genshin::QuestDevice;
use image::DynamicImage;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::{
    fs::OpenOptionsExt,
    process::{CommandExt, ExitStatusExt},
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: u64 = 32 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);

pub(crate) struct Worker {
    dir: PathBuf,
    adb: String,
    serial: String,
    hid_name: String,
    group: i32,
    state: RefCell<Value>,
    fatal: RefCell<Option<String>>,
    gamepad: RefCell<Option<Gamepad>>,
    sequence: Cell<u64>,
}

fn validate_session(state: &Value) -> Result<(), String> {
    if state["schema"] != "phonefarm/capture-session@1" || state["stage"] != "active" {
        return Err("capture worker requires an active v1 session".into());
    }
    for field in ["serial", "token", "boot_id", "evidence_origin", "hid_name"] {
        if !state[field].as_str().is_some_and(|s| !s.is_empty()) {
            return Err(format!("capture session missing {field}"));
        }
    }
    let name = state["hid_name"].as_str().unwrap();
    if name.chars().any(|c| c.is_control() || c == '"') {
        return Err("capture HID name cannot contain control characters or quotes".into());
    }
    let stayon = &state["stayon"];
    if !stayon.is_null()
        && !stayon.as_str().is_some_and(|s| {
            s == "null" || (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        })
    {
        return Err("invalid saved capture stayon value".into());
    }
    if !state["shutdown"].is_boolean()
        || state.get("stayon").is_none()
        || state["hid_started"] != false
        || state["hid_registered"] != false
        || state.get("write_pending") != Some(&Value::Null)
        || state["transport_uncertain"] != false
    {
        return Err("capture session has pending/unknown input or transport state".into());
    }
    Ok(())
}

fn hid_name_count(devices: &str, name: &str) -> usize {
    let expected = format!("N: Name=\"{name}\"");
    devices
        .lines()
        .filter(|line| line.trim() == expected)
        .count()
}

fn output_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("capture output {}: {e}", path.display()))
}

fn output_within_limit(file: &File) -> Result<(), String> {
    // Reaching the hard child limit can mean a truncated write even if a
    // transport catches EFBIG and exits zero; do not accept that as evidence.
    if file.metadata().map_err(|e| e.to_string())?.len() >= OUTPUT_LIMIT {
        return Err("capture command output limit reached".into());
    }
    Ok(())
}

impl Worker {
    /// The entry point must install lease_command::worker_guard before this call.
    pub fn open(dir: &Path) -> Result<Self, String> {
        crate::devlease::Lease::verify_worker_authority()?;
        let mut bytes = Vec::new();
        File::open(dir.join("session.json"))
            .map_err(|e| e.to_string())?
            .take(65537)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 65536 {
            return Err("capture session is too large".into());
        }
        let state: Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("capture session: {e}"))?;
        validate_session(&state)?;
        let serial = state["serial"].as_str().unwrap().to_owned();
        if std::env::var("PHONEFARM_WORKER_SERIAL").ok().as_deref() != Some(serial.as_str())
            || std::env::var("PHONEFARM_WORKER_TOKEN").ok().as_deref() != state["token"].as_str()
        {
            return Err("capture session does not match the authorized lease".into());
        }
        let hid_name = state["hid_name"].as_str().unwrap().to_owned();
        let adb = crate::device::locate_adb().ok_or("capture cannot locate adb")?;
        fs::create_dir_all(dir.join("worker-commands")).map_err(|e| e.to_string())?;
        Ok(Self {
            dir: dir.to_path_buf(),
            adb,
            serial,
            hid_name,
            group: std::process::id() as i32,
            state: RefCell::new(state),
            fatal: RefCell::new(None),
            gamepad: RefCell::new(None),
            sequence: Cell::new(0),
        })
    }

    fn fail<T>(&self, error: impl Into<String>) -> Result<T, String> {
        let error = error.into();
        let mut fatal = self.fatal.borrow_mut();
        let first = fatal.get_or_insert(error);
        Err(first.clone())
    }

    fn remember<T>(&self, result: Result<T, String>) -> Result<T, String> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.fail(error),
        }
    }

    fn save(&self, update: impl FnOnce(&mut Value)) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        update(&mut state);
        self.remember(crate::devlease::durable_replace(
            &self.dir.join("session.json"),
            state.to_string().as_bytes(),
        ))
    }

    pub fn check(&self) -> Result<(), String> {
        if let Some(error) = self.fatal.borrow().as_ref() {
            return Err(error.clone());
        }
        let state = self.state.borrow();
        if state["transport_uncertain"] != false
            || !state["write_pending"].is_null()
            || (state["hid_started"] == true && state["hid_registered"] != true)
        {
            return self.fail("capture has pending or uncertain device activity");
        }
        Ok(())
    }

    fn abort_uncertain(&self, error: &str) -> ! {
        let saved = self.save(|state| state["transport_uncertain"] = json!(true));
        eprintln!("capture transport uncertain: {error}");
        if let Err(e) = saved {
            eprintln!("cannot persist capture uncertainty: {e}");
        }
        // This PGID was checked against the parent's recorded child identity by
        // verify_worker_authority. Never target an ambient/shared process group.
        unsafe {
            if libc::getpgrp() == self.group && std::process::id() as i32 == self.group {
                libc::kill(-self.group, libc::SIGKILL);
            }
            libc::_exit(137);
        }
    }

    /// Internal reads during HID registration may run while hid_register is
    /// pending. Public callers check first; nothing here clears pending state.
    fn raw(&self, args: &[&str], ms: u64) -> Result<Vec<u8>, String> {
        if let Some(error) = self.fatal.borrow().as_ref() {
            return Err(error.clone());
        }
        let result = (|| {
            let sequence = self.sequence.get();
            self.sequence.set(sequence + 1);
            let stem = self
                .dir
                .join("worker-commands")
                .join(format!("{}-{sequence:06}", self.group));
            let stdout_path = stem.with_extension("stdout");
            let stderr_path = stem.with_extension("stderr");
            let stdout = output_file(&stdout_path)?;
            let stderr = output_file(&stderr_path)?;
            let mut record = json!({"argv": args, "timeout_ms": ms, "exit": null});
            crate::devlease::durable_replace(
                &stem.with_extension("json"),
                record.to_string().as_bytes(),
            )?;
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(ms))
                .ok_or("capture command timeout too large")?;
            let mut command = Command::new(&self.adb);
            command
                .args(["-s", &self.serial])
                .args(args)
                .stdin(Stdio::null())
                .stdout(stdout.try_clone().map_err(|e| e.to_string())?)
                .stderr(stderr.try_clone().map_err(|e| e.to_string())?);
            // A per-child file limit bounds raw evidence on disk, including a
            // transport that can write much faster than the polling interval.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: OUTPUT_LIMIT as libc::rlim_t,
                        rlim_max: OUTPUT_LIMIT as libc::rlim_t,
                    };
                    if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command
                .spawn()
                .map_err(|e| format!("capture adb spawn: {e}"))?;
            let status = loop {
                if let Err(e) =
                    output_within_limit(&stdout).and_then(|_| output_within_limit(&stderr))
                {
                    self.abort_uncertain(&e);
                }
                if Instant::now() >= deadline {
                    self.abort_uncertain("adb deadline exceeded");
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => std::thread::sleep(POLL),
                    Err(e) => self.abort_uncertain(&format!("adb wait failed: {e}")),
                }
            };
            if let Err(e) = output_within_limit(&stdout).and_then(|_| output_within_limit(&stderr))
            {
                self.abort_uncertain(&e);
            }
            stdout
                .sync_all()
                .and_then(|_| stderr.sync_all())
                .map_err(|e| e.to_string())?;
            record["exit"] = json!(status.code());
            record["signal"] = json!(status.signal());
            crate::devlease::durable_replace(
                &stem.with_extension("json"),
                record.to_string().as_bytes(),
            )?;
            if !status.success() {
                return Err(format!(
                    "capture adb failed ({status}); raw output: {}",
                    stem.display()
                ));
            }
            let mut bytes = Vec::new();
            File::open(&stdout_path)
                .map_err(|e| e.to_string())?
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() as u64 >= OUTPUT_LIMIT {
                self.abort_uncertain("adb output grew to limit");
            }
            Ok(bytes)
        })();
        self.remember(result)
    }

    fn text(&self, cmd: &str, ms: u64) -> Result<String, String> {
        let bytes = self.raw(&["shell", cmd], ms)?;
        self.remember(
            String::from_utf8(bytes).map_err(|e| format!("capture adb text is not UTF-8: {e}")),
        )
    }

    pub fn png(&self) -> Result<Vec<u8>, String> {
        self.check()?;
        let bytes = self.raw(&["exec-out", "screencap", "-p"], 15_000)?;
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return self.fail("capture screencap did not return PNG");
        }
        Ok(bytes)
    }

    fn with_gamepad<T>(
        &self,
        action: impl FnOnce(&mut Gamepad) -> Result<T, String>,
    ) -> Result<T, String> {
        self.check()?;
        if self.gamepad.borrow().is_none() {
            if self.state.borrow()["hid_started"] == true {
                return self.fail("capture input session was already closed");
            }
            self.save(|state| {
                state["hid_started"] = json!(true);
                state["write_pending"] = json!("hid_register");
            })?;
            let gamepad = self.remember(Gamepad::new_named(
                &self.adb,
                Some(&self.serial),
                &self.hid_name,
            ))?;
            *self.gamepad.borrow_mut() = Some(gamepad);
            let devices = self.text("cat /proc/bus/input/devices", 8_000)?;
            if hid_name_count(&devices, &self.hid_name) != 1 {
                return self
                    .fail("capture HID registration was not uniquely observed; pending retained");
            }
            self.save(|state| {
                state["hid_registered"] = json!(true);
                state["write_pending"] = Value::Null;
            })?;
        }
        let result = action(self.gamepad.borrow_mut().as_mut().unwrap());
        self.remember(result)
    }

    /// Always drop an existing local instance, even after failure. Its remote
    /// absence is verified by the parent; this never creates a new input device.
    pub fn close(&self) -> Result<(), String> {
        let checked = self.check();
        drop(self.gamepad.borrow_mut().take());
        checked
    }
}

impl QuestDevice for Worker {
    fn check(&self) -> Result<(), String> {
        Worker::check(self)
    }

    fn shell(&self, cmd: &str, ms: u64) -> String {
        self.check()
            .and_then(|_| self.text(cmd, ms))
            .unwrap_or_default()
    }

    fn shell_write(&self, cmd: &str, ms: u64) -> Result<(), String> {
        self.check()?;
        self.save(|state| state["write_pending"] = json!(cmd))?;
        self.raw(&["shell", cmd], ms)?;
        self.save(|state| state["write_pending"] = Value::Null)
    }

    fn screen_image(&self) -> Option<DynamicImage> {
        let result = self.png().and_then(|bytes| {
            image::load_from_memory(&bytes).map_err(|e| format!("capture PNG decode: {e}"))
        });
        self.remember(result).ok()
    }

    fn gamepad_press(&self, btn: &str, duration_ms: u64) -> Result<(), String> {
        self.with_gamepad(|gamepad| gamepad.press(btn, duration_ms))
    }

    fn gamepad_stick(&self, stick: &str, x: f32, y: f32, duration_ms: u64) -> Result<(), String> {
        self.with_gamepad(|gamepad| {
            gamepad.set_stick(stick, x, y)?;
            if duration_ms > 0 {
                std::thread::sleep(Duration::from_millis(duration_ms));
                gamepad.set_stick(stick, 0.0, 0.0)?;
            }
            Ok(())
        })
    }

    fn gamepad_wander(&self, duration_ms: u64) -> Result<(), String> {
        self.with_gamepad(|gamepad| gamepad.wander(duration_ms))
    }

    fn gamepad_reset(&self) -> Result<(), String> {
        self.check()?;
        let result = match self.gamepad.borrow_mut().as_mut() {
            Some(gamepad) => gamepad.reset(),
            None => Ok(()),
        };
        self.remember(result)
    }

    fn gamepad_close(&self) -> Result<(), String> {
        self.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_session_rejects_pending_or_reused_input() {
        let valid = json!({"schema":"phonefarm/capture-session@1", "serial":"fake", "token":"token",
            "boot_id":"boot", "stayon":"7", "shutdown":false, "evidence_origin":"synthetic_fixture",
            "stage":"active", "hid_name":"capture-fixture", "hid_started":false, "hid_registered":false,
            "write_pending":null, "transport_uncertain":false});
        assert!(validate_session(&valid).is_ok());
        for (key, value) in [
            ("write_pending", json!("hid_register")),
            ("transport_uncertain", json!(true)),
            ("hid_started", json!(true)),
            ("stage", json!("verified")),
            ("stayon", json!("unknown")),
        ] {
            let mut bad = valid.clone();
            bad[key] = value;
            assert!(validate_session(&bad).is_err(), "{key}");
        }
    }

    #[test]
    fn input_registration_requires_one_exact_name() {
        assert_eq!(
            hid_name_count("N: Name=\"capture-fixture\"\n", "capture-fixture"),
            1
        );
        assert_eq!(
            hid_name_count("N: Name=\"capture-fixture-extra\"\n", "capture-fixture"),
            0
        );
        assert_eq!(
            hid_name_count(
                "N: Name=\"capture-fixture\"\nN: Name=\"capture-fixture\"\n",
                "capture-fixture"
            ),
            2
        );
    }
}
