//! Bounded, sequential transport commands under an existing experiment lease.
//! Local process completion never certifies that activity on the device stopped.
use crate::devlease::Lease;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::{
    fs::OpenOptionsExt,
    process::{CommandExt, ExitStatusExt},
};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(20);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

// A single newline is the permit. EOF, a failed read, or any other line refuses
// execution. Positional arguments keep the program and argv out of shell syntax.
const GATE: &str =
    "IFS= read -r permit || exit 91; [ -z \"$permit\" ] || exit 92; exec \"$@\" </dev/null";
const WORKER_GATE: &str =
    "IFS= read -r permit || exit 91; [ -z \"$permit\" ] || exit 92; exec \"$@\"";

#[derive(Debug)]
pub(crate) struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn group_gone(pid: u32) -> bool {
    (unsafe { libc::kill(-(pid as i32), 0) == -1 })
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn stop_and_reap(child: &mut Child) -> bool {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut reaped = false;
    loop {
        if !reaped {
            match child.try_wait() {
                Ok(Some(_)) => reaped = true,
                Err(_) => return false,
                Ok(None) => {}
            }
        }
        if reaped && group_gone(child.id()) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

fn output_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("command output {}: {e}", path.display()))
}

fn within_limit(file: &File) -> Result<(), String> {
    let size = file
        .metadata()
        .map_err(|e| format!("command output metadata: {e}"))?
        .len();
    if size > OUTPUT_LIMIT {
        return Err("command output limit exceeded".into());
    }
    Ok(())
}

fn read_output(mut file: File) -> Result<String, String> {
    within_limit(&file)?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err("command output grew beyond limit".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The caller retains the recovery hold and serializes commands using this lease.
/// `dir` keeps the raw stdout/stderr files; no executable snapshot or hash is made.
pub(crate) fn run(
    lease: &Lease,
    program: &Path,
    args: &[String],
    dir: &Path,
    operation: &str,
    timeout: Duration,
) -> Result<Output, String> {
    supervise(lease, program, args, dir, operation, timeout, false)
}

/// Same-executable workers must call worker_guard before doing any device work.
/// Their inherited stdin is only a parent-liveness pipe, never application input.
pub(crate) fn run_worker(
    lease: &Lease,
    program: &Path,
    args: &[String],
    dir: &Path,
    operation: &str,
    timeout: Duration,
) -> Result<Output, String> {
    supervise(lease, program, args, dir, operation, timeout, true)
}

fn supervise(
    lease: &Lease,
    program: &Path,
    args: &[String],
    dir: &Path,
    operation: &str,
    timeout: Duration,
    worker: bool,
) -> Result<Output, String> {
    lease.check()?;
    fs::create_dir_all(dir).map_err(|e| format!("command output directory: {e}"))?;
    let stem = format!(
        "{}-{:06}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let out = output_file(&dir.join(format!("{stem}.stdout")))?;
    let err = output_file(&dir.join(format!("{stem}.stderr")))?;
    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            if worker { WORKER_GATE } else { GATE },
            "phonefarm-command-gate",
        ])
        .arg(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(out.try_clone().map_err(|e| e.to_string())?)
        .stderr(err.try_clone().map_err(|e| e.to_string())?)
        .process_group(0);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("command timeout is too large")?;
    if worker {
        lease.authorize_worker(&mut command, timeout)?;
    }
    lease.record_command_pending(operation)?;
    let mut child = command.spawn().map_err(|e| {
        format!("{operation}: command could not spawn; pending lease retained: {e}")
    })?;
    let start = lease
        .record_child(child.id())
        .and_then(|_| lease.check())
        .and_then(|_| {
            if Instant::now() >= deadline {
                return Err("command deadline exceeded before gate".into());
            }
            child
                .stdin
                .as_mut()
                .ok_or("command gate pipe unavailable".to_string())?
                .write_all(b"\n")
                .map_err(|e| format!("command gate: {e}"))
        });
    // Worker stdin remains open until the child exits. Ordinary commands retain
    // the old EOF/null-input behavior immediately after the startup permit.
    if !worker {
        drop(child.stdin.take());
    }
    if let Err(e) = start {
        // A failed marker publication may have reached disk. Never clear this
        // uncertain launch even when the unopened gate has now been killed.
        let gone = stop_and_reap(&mut child);
        return Err(format!(
            "{operation}: startup failed: {e}; local_group_gone={gone}; lease retained"
        ));
    }

    let result: Result<ExitStatus, String> = loop {
        if let Err(e) = lease
            .check()
            .and_then(|_| within_limit(&out))
            .and_then(|_| within_limit(&err))
        {
            break Err(e);
        }
        if Instant::now() >= deadline {
            break Err("command deadline exceeded".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let grace = Instant::now() + Duration::from_millis(100);
                while !group_gone(child.id()) && Instant::now() < grace {
                    std::thread::sleep(POLL);
                }
                if !group_gone(child.id()) {
                    break Err("command exited but its process group remains live/unknown".into());
                }
                break Ok(status);
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => break Err(format!("command wait failed: {e}")),
        }
    };
    let gone = result.is_ok() || stop_and_reap(&mut child);
    if gone {
        lease.clear_child().map_err(|e| {
            format!("{operation}: local group gone but child receipt not cleared: {e}")
        })?;
    }
    let status = result.map_err(|e| {
        format!("{operation}: {e}; local_group_gone={gone}; remote activity not verified stopped")
    })?;
    Ok(Output {
        code: status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        stdout: read_output(out)?,
        stderr: read_output(err)?,
    })
}

/// Called once by the internal worker entry point, before opening transports.
/// A dead or suspended parent cannot leave the verified worker group unbounded.
pub(crate) fn worker_guard() -> Result<(), String> {
    let remaining = Lease::verify_worker_authority()?;
    let group = std::process::id() as i32;
    let stop = move || unsafe {
        libc::kill(-group, libc::SIGKILL);
        libc::_exit(137);
    };
    std::thread::Builder::new()
        .name("lease-worker-lifeline".into())
        .spawn(move || {
            let mut byte = [0];
            let _ = std::io::stdin().read(&mut byte);
            stop();
        })
        .map_err(|e| format!("worker lifeline guard: {e}"))?;
    std::thread::Builder::new()
        .name("lease-worker-deadline".into())
        .spawn(move || {
            std::thread::sleep(remaining);
            stop();
        })
        .map_err(|e| format!("worker deadline guard: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pf-command-test-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn lease(&self) -> Lease {
            Lease::acquire(
                &self.0.join("devices"),
                &self.0.join("legacy"),
                "fake-command",
                "test",
                Duration::from_millis(100),
            )
            .unwrap()
        }
        fn marker(&self) -> Value {
            serde_json::from_slice(
                &fs::read(self.0.join("legacy/devlock.d/phonefarm-lease.json")).unwrap(),
            )
            .unwrap()
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn gate_refuses_eof_and_invalid_permission_then_command_preserves_output() {
        let scratch = Scratch::new();
        let sentinel = scratch.0.join("executed");
        for (gate, permission) in [GATE, WORKER_GATE].into_iter().flat_map(|gate| {
            [b"".as_slice(), b"invalid\n".as_slice()]
                .into_iter()
                .map(move |permission| (gate, permission))
        }) {
            let mut child = Command::new("/bin/sh")
                .args(["-c", gate, "test-gate", "/usr/bin/touch"])
                .arg(&sentinel)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(permission).unwrap();
            assert!(!child.wait().unwrap().success());
            assert!(!sentinel.exists());
        }
        let lease = scratch.lease();
        let result = run(
            &lease,
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "printf output; printf diagnostic >&2; exit 7".into(),
            ],
            &scratch.0.join("commands"),
            "fake",
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            (result.code, result.stdout.as_str(), result.stderr.as_str()),
            (7, "output", "diagnostic")
        );
        assert!(scratch.marker().get("child_pid").is_none());
        assert_eq!(scratch.marker()["command_pending"], false);
    }

    #[test]
    fn worker_keeps_liveness_pipe_until_timeout() {
        let scratch = Scratch::new();
        let lease = scratch.lease();
        let error = run_worker(
            &lease,
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "test -n \"$PHONEFARM_WORKER_SERIAL\" || exit 3; IFS= read -r unused; exit 4"
                    .into(),
            ],
            &scratch.0.join("commands"),
            "worker-pipe",
            Duration::from_millis(250),
        )
        .unwrap_err();
        assert!(error.contains("deadline exceeded"), "{error}");
        assert!(scratch.marker().get("child_pid").is_none(), "{error}");
    }

    #[test]
    fn timeout_and_remaining_group_cannot_complete_recovery() {
        let scratch = Scratch::new();
        let context = scratch.0.join("recovery.json");
        fs::write(&context, "{}").unwrap();
        let mut lease = scratch.lease();
        lease.hold_for_recovery(&context).unwrap();
        let error = run(
            &lease,
            Path::new("/bin/sleep"),
            &["5".into()],
            &scratch.0.join("commands"),
            "timeout",
            Duration::from_millis(250),
        )
        .unwrap_err();
        assert!(error.contains("deadline exceeded"), "{error}");
        assert!(scratch.marker().get("child_pid").is_none(), "{error}");
        assert_eq!(scratch.marker()["recovery_required"], true);
        let error = run(
            &lease,
            Path::new("/bin/sh"),
            &["-c".into(), "/bin/sleep 5 & exit 0".into()],
            &scratch.0.join("commands"),
            "descendant",
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.contains("process group remains"), "{error}");
        assert_eq!(scratch.marker()["recovery_required"], true);
        // Some hosts retain an orphan zombie briefly. Judge the cleanup result
        // recorded at return, not a second racy lookup of a process now exiting.
        assert_eq!(
            scratch.marker().get("child_pid").is_none(),
            error.contains("local_group_gone=true")
        );
    }
}
