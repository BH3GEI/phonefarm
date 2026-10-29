//! One kernel lock protocol. Owner JSON is display data, never admission state.
use crate::device_registry::DeviceBinding;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const FDS: &str = "PF_LOCK_FDS";
pub(crate) const SCOPE: &str = "PF_LOCK_SCOPE";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Scope {
    pub key: Option<String>,
    pub serial: Option<String>,
    pub binding: Option<DeviceBinding>,
}
impl Scope {
    pub fn global() -> Self {
        Self {
            key: None,
            serial: None,
            binding: None,
        }
    }
    pub fn device(serial: &str, binding: Option<DeviceBinding>) -> Self {
        Self {
            key: Some(
                binding
                    .as_ref()
                    .map(|b| b.device_id.clone())
                    .unwrap_or_else(|| serial.into()),
            ),
            serial: Some(serial.into()),
            binding,
        }
    }
    fn contains(&self, other: &Self) -> bool {
        self.key.is_none() || self.key == other.key
    }
}

pub(crate) fn device_path(root: &Path, key: &str) -> PathBuf {
    root.join("by-device-id")
        .join(format!("{:x}.lock", Sha256::digest(key.as_bytes())))
}

fn acquire_file(path: &Path, shared: bool, deadline: Option<Instant>) -> Result<File, String> {
    let file = crate::shared_access::lock_file(path, 0o666)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    loop {
        match if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        } {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                if deadline.is_some_and(|until| Instant::now() >= until) {
                    return Err(format!("device lock busy: {}", path.display()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
}

pub(crate) fn inherited() -> Result<Option<(Scope, Vec<RawFd>)>, String> {
    let Some(value) = std::env::var_os(FDS) else {
        return Ok(None);
    };
    let raw = value.to_str().ok_or("lock descriptors are not UTF-8")?;
    let fds: Vec<RawFd> = raw
        .split(',')
        .map(|value| value.parse::<RawFd>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    for fd in &fds {
        if unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0 {
            return Err(format!(
                "inherited lock fd {fd} was closed; pass it through the subprocess launcher"
            ));
        }
    }
    let scope = serde_json::from_str(&std::env::var(SCOPE).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(Some((scope, fds)))
}

pub(crate) struct Guard {
    pub scope: Scope,
    files: Vec<File>,
    pub fds: Vec<RawFd>,
    label: String,
    farm: PathBuf,
}
impl Guard {
    pub fn acquire(
        device_root: &Path,
        farm_root: &Path,
        requested: Scope,
        label: &str,
        timeout: Option<Duration>,
    ) -> Result<Self, String> {
        if let Some((scope, fds)) = inherited()? {
            if !scope.contains(&requested) {
                return Err("task lock does not cover the requested device/global scope; submit a global task for multiple devices".into());
            }
            return Ok(Self {
                scope: if scope.key.is_none() {
                    scope
                } else {
                    requested
                },
                files: Vec::new(),
                fds,
                label: label.into(),
                farm: farm_root.into(),
            });
        }
        crate::shared_access::create_dir_all(farm_root).map_err(|e| e.to_string())?;
        let deadline = timeout.and_then(|duration| Instant::now().checked_add(duration));
        let mut files = Vec::new();
        if let Some(key) = &requested.key {
            let path = device_path(device_root, key);
            crate::shared_access::create_dir_all(path.parent().unwrap())
                .map_err(|e| e.to_string())?;
            files.push(acquire_file(&path, false, deadline)?);
        }
        let farm = farm_root.join("phonefarm-bridge.lock");
        files.push(acquire_file(&farm, requested.key.is_some(), deadline)?);
        let info_path = requested
            .key
            .as_ref()
            .map(|key| device_path(device_root, key))
            .unwrap_or(farm)
            .with_extension("owner.json");
        let info = json!({"scope": requested, "label": label, "pid": std::process::id(),
            "job_id": std::env::var("PF_JOB_ID").ok(), "actor": std::env::var("PF_JOB_ACTOR").ok(),
            "since": chrono::Utc::now().to_rfc3339()});
        if let Err(error) = fs::write(&info_path, info.to_string()) {
            eprintln!("lock owner display {}: {error}", info_path.display());
        }
        let fds: Vec<_> = files.iter().map(AsRawFd::as_raw_fd).collect();
        Ok(Self {
            scope: requested,
            files,
            fds,
            label: label.into(),
            farm: farm_root.into(),
        })
    }
    /// Only a task child inherits ownership; transport daemons never do.
    pub fn command(&self, cmd: &mut std::process::Command) -> Result<(), String> {
        task_transports(cmd, &self.farm)?;
        cmd.env(
            FDS,
            self.fds
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
        .env(
            SCOPE,
            serde_json::to_string(&self.scope).expect("scope serialization"),
        )
        .env("DEVLOCK_LABEL", &self.label);
        if let Some(binding) = &self.scope.binding {
            cmd.env(
                "PF_JOB_DEVICE_BINDING",
                serde_json::to_string(binding).expect("binding serialization"),
            );
        }
        let fds = self.fds.clone();
        unsafe {
            cmd.pre_exec(move || {
                for fd in &fds {
                    let flags = libc::fcntl(*fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        Ok(())
    }
    pub fn owns(&self) -> bool {
        !self.files.is_empty()
    }
}
// Dropping files closes our copies. LOCK_UN would unlock copies still held by children.

/// Raw shell/Python tasks also use the transport boundary. The two symlinks are
/// launchers, not ownership records; they do not participate in acquisition.
fn task_transports(cmd: &mut std::process::Command, farm: &Path) -> Result<(), String> {
    if std::env::var_os("PF_TRANSPORT_PATH").is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let bin = farm.join("transport-bin").join(format!(
        "{:x}",
        Sha256::digest(exe.as_os_str().as_encoded_bytes())
    ));
    fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    for name in ["adb", "hdc"] {
        match std::os::unix::fs::symlink(&exe, bin.join(name)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let adb = crate::device::locate_adb().unwrap_or_else(|| "adb".into());
    let hdc = std::env::var("HDC_BIN").unwrap_or_else(|_| "hdc".into());
    cmd.env("PF_TRANSPORT_PATH", &path)
        .env("PF_TRANSPORT_ADB", &adb)
        .env("PF_TRANSPORT_HDC", hdc)
        .env("ADB_BIN", bin.join("adb"))
        .env("HDC_BIN", bin.join("hdc"));
    let paths = std::iter::once(bin).chain(std::env::split_paths(&path));
    cmd.env(
        "PATH",
        std::env::join_paths(paths).map_err(|e| e.to_string())?,
    );
    Ok(())
}

pub(crate) fn transport_entry() -> Option<i32> {
    let name = std::env::args_os()
        .next()
        .and_then(|path| PathBuf::from(path).file_name().map(|name| name.to_owned()))?;
    let key = match name.to_str()? {
        "adb" => "PF_TRANSPORT_ADB",
        "hdc" => "PF_TRANSPORT_HDC",
        _ => return None,
    };
    let program = std::env::var_os(key)?;
    let mut cmd = transport_command(program);
    cmd.args(std::env::args_os().skip(1)).env(
        "PATH",
        std::env::var_os("PF_TRANSPORT_PATH").unwrap_or_default(),
    );
    // The proxy stays in the task group and holds the lock until the finite client exits.
    Some(crate::devlock::wait_control_child(&mut cmd))
}

/// A separately supervised transport group dies when its parent pipe closes.
/// The actual adb/hdc client cannot inherit either this pipe or the lock fds.
pub(crate) fn transport_lifeline(
    command: &mut std::process::Command,
) -> Result<
    (
        std::os::unix::net::UnixStream,
        std::os::unix::net::UnixStream,
    ),
    String,
> {
    let (parent, child) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
    let fd = child.as_raw_fd();
    command.env("PF_TRANSPORT_LIFELINE", fd.to_string());
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((parent, child))
}

/// A transport server must never retain the task's descriptors. The caller owns
/// the lease while the client runs, and the task group owns client cleanup.
pub(crate) fn transport_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    // The finite proxy retains the task fds; Python closes them for the actual
    // client, so a server fork cannot retain them. Reset kills the whole group.
    const PROXY: &str = r#"import os,signal,subprocess,sys,threading,time
child=None
def forward(signum,frame):
    if child is not None:
        try: os.kill(child.pid,signum)
        except ProcessLookupError: pass
for sig in (signal.SIGINT,signal.SIGTERM): signal.signal(sig,forward)
lifeline=os.environ.pop('PF_TRANSPORT_LIFELINE',None)
if lifeline is not None:
    def watch():
        os.read(int(lifeline),1)
        os.killpg(os.getpgrp(),signal.SIGTERM)
        time.sleep(1)
        os.killpg(os.getpgrp(),signal.SIGKILL)
    threading.Thread(target=watch,daemon=True).start()
env=dict(os.environ)
for name in ('PF_LOCK_FDS','PF_LOCK_SCOPE'): env.pop(name,None)
child=subprocess.Popen(sys.argv[1:],env=env,close_fds=True)
code=child.wait()
sys.exit(code if code>=0 else 128-code)
"#;
    let program = program.as_ref();
    let actual = if std::env::var_os("PF_TRANSPORT_PATH").is_some() {
        if program == "adb" || std::env::var_os("ADB_BIN").as_deref() == Some(program) {
            std::env::var_os("PF_TRANSPORT_ADB")
        } else if program == "hdc" || std::env::var_os("HDC_BIN").as_deref() == Some(program) {
            std::env::var_os("PF_TRANSPORT_HDC")
        } else {
            None
        }
    } else {
        None
    };
    let mut command = std::process::Command::new("python3");
    command
        .args(["-c", PROXY])
        .arg(actual.as_deref().unwrap_or(program));
    if let Some(path) = std::env::var_os("PF_TRANSPORT_PATH") {
        command.env("PATH", path);
    }
    command
}

/// GOL receives these same open-file descriptions, allowing its human-pause path
/// to drop and reacquire the lock without reimplementing acquisition or cooldown.
pub(crate) fn handoff(args: &[String]) -> i32 {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    let result = (|| -> Result<(), String> {
        let fd: RawFd = args
            .first()
            .ok_or("missing handoff socket")?
            .parse()
            .map_err(|e| format!("handoff socket: {e}"))?;
        let mut socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let mut serial = None;
        let mut label = "game_opt_loop";
        let mut timeout = None;
        let mut it = args[1..].iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--serial" => serial = it.next().map(String::as_str),
                "--label" => label = it.next().ok_or("missing label")?,
                "--timeout-ms" => {
                    timeout = Some(Duration::from_millis(
                        it.next()
                            .ok_or("missing timeout")?
                            .parse()
                            .map_err(|e| format!("timeout: {e}"))?,
                    ))
                }
                _ => return Err(format!("unknown handoff argument {arg}")),
            }
        }
        let storage = crate::lease_storage::resolve();
        let (root, farm) = storage.require_ready()?;
        let scope = serial
            .map(|serial| crate::devlease::resolve_scope(&storage, serial))
            .transpose()?
            .unwrap_or_else(Scope::global);
        let guard = Guard::acquire(&root, &farm, scope, label, timeout)?;
        let payload = serde_json::to_vec(&guard.scope).map_err(|e| e.to_string())?;
        crate::lock_handoff::send(&mut socket, &guard.fds, &payload).map_err(|e| e.to_string())?;
        let mut byte = [0];
        socket.read(&mut byte).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            2
        }
    }
}

/// Read-only status probes do not create an inode or trust leftover owner JSON.
pub(crate) fn held(path: &Path, shared: bool) -> Result<bool, String> {
    let file = match File::options().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    match if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    } {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}
