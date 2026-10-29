//! Real host processes and kernel locks; transport and state roots are isolated.
#![cfg(unix)]
use serde_json::{json, Value};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pf-fd-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("locks")).unwrap();
        let f = Self(root);
        let init = f
            .pf()
            .args(["lease", "domain-init", "--participants-stopped", "--json"])
            .output()
            .unwrap();
        assert!(
            init.status.success(),
            "{}",
            String::from_utf8_lossy(&init.stderr)
        );
        for (revision, id) in ["a", "b"].into_iter().enumerate() {
            let request = f.0.join("register.json");
            fs::write(&request, json!({"expected_revision": revision, "device": {
                "device_id": id, "label": id, "aliases": [format!("fake-{id}"), format!("wifi-{id}")], "enabled": true
            }}).to_string()).unwrap();
            let out = f
                .pf()
                .args(["lease", "devices-set", "--request"])
                .arg(request)
                .arg("--json")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        f
    }
    fn pf(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_phonefarm"));
        c.env("DEVLOCK_DIR", self.0.join("locks"))
            .env("PHONEFARM_LEASE_DIR", self.0.join("devices"))
            .env("PHONEFARM_BIN", env!("CARGO_BIN_EXE_phonefarm"))
            .env_remove("DEVLOCK_LABEL")
            .env_remove("DEVLOCK_INSTANCE_ID")
            .env_remove("PF_JOB_DEVICE_BINDING")
            .env_remove("PF_JOB_ACTOR")
            .env_remove("PF_JOB_ID")
            .env_remove("PF_JOB_LEASE_DIR")
            .env_remove("PF_LOCK_FDS")
            .env_remove("PF_LOCK_SCOPE");
        c
    }
    fn try_device(&self, serial: &str) -> std::process::Output {
        self.pf()
            .args([
                "lease",
                "--serial",
                serial,
                "--timeout-ms",
                "0",
                "--",
                "/usr/bin/true",
            ])
            .output()
            .unwrap()
    }
    fn until(&self, path: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.0.join(path).exists() {
            assert!(
                Instant::now() < deadline,
                "fixture did not reach {path}: {}",
                self.0.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
struct Group(Child);
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

#[test]
fn killing_owner_keeps_child_fd_exclusive_until_the_child_exits() {
    let f = Fixture::new();
    let script = r#"import json,os,sys,time
from pathlib import Path
root=Path(sys.argv[1])
fds=[int(fd) for fd in os.environ.get('PF_LOCK_FDS','').split(',') if fd]
for fd in fds: os.fstat(fd)
(root/'fds.json').write_text(json.dumps(fds))
while not (root/'finish').exists(): time.sleep(.01)
"#;
    let mut owner = Group(
        f.pf()
            .args(["lease", "--serial", "fake-a", "--", "python3", "-c", script])
            .arg(&f.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    f.until("fds.json");
    let fds: Vec<i32> = serde_json::from_slice(&fs::read(f.0.join("fds.json")).unwrap()).unwrap();
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    let while_child_alive = f.try_device("wifi-a");
    let other = f.try_device("fake-b");
    fs::write(f.0.join("finish"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let after_child = loop {
        let result = f.try_device("fake-a");
        if result.status.success() || Instant::now() >= deadline {
            break result;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        fds.len(),
        2,
        "device command must inherit the actual device and farm descriptors"
    );
    assert!(
        !while_child_alive.status.success(),
        "same alias entered while the original child was alive"
    );
    assert!(
        other.status.success(),
        "other device blocked: {}",
        String::from_utf8_lossy(&other.stderr)
    );
    assert!(
        after_child.status.success(),
        "last fd close must release, without marker recovery: {}",
        String::from_utf8_lossy(&after_child.stderr)
    );
}

#[test]
fn old_recovery_records_never_block_a_new_fd_owner() {
    let f = Fixture::new();
    let marker = f.0.join("locks/v2/global/devlock.d");
    fs::create_dir(&marker).unwrap();
    let bytes = json!({"format": 1, "producer": "phonefarm", "serial": "fake-a", "recovery_required": true});
    fs::write(marker.join("phonefarm-lease.json"), bytes.to_string()).unwrap();
    fs::write(marker.join("owner"), "old owner since fixture\n").unwrap();
    let out = f.try_device("fake-a");
    assert!(
        out.status.success(),
        "old recovery record blocked kernel acquisition: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(marker.join("phonefarm-lease.json")).unwrap())
            .unwrap(),
        bytes
    );
}

#[test]
fn adb_and_hdc_daemons_do_not_inherit_task_descriptors() {
    use std::os::unix::fs::PermissionsExt;
    for name in ["adb", "hdc"] {
        let f = Fixture::new();
        let program = f.0.join(format!("fake-{name}"));
        fs::write(&program, r#"#!/usr/bin/env python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['DAEMON_TEST_ROOT'])
if sys.argv[1]=='kill-server':
    if (root/'server.pid').exists(): os.kill(int((root/'server.pid').read_text()),signal.SIGKILL)
    sys.exit(0)
pid=os.fork()
if pid: sys.exit(0)
os.setsid()
signal.alarm(15)
held=[]
for fd in range(3,256):
    try:
        st=os.fstat(fd)
        if any((st.st_dev,st.st_ino)==(p.stat().st_dev,p.stat().st_ino) for p in (root/'devices'/'by-device-id').glob('*.lock')): held.append(fd)
    except OSError: pass
for fd in range(3): os.close(fd)
(root/'server.pid').write_text(str(os.getpid()))
(root/'daemon-fds.json').write_text(json.dumps(held))
time.sleep(15)
"#).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Command::new(&program)
            .arg("kill-server")
            .env("DAEMON_TEST_ROOT", &f.0)
            .status()
            .unwrap();
        let command = format!("test -n \"$PF_LOCK_FDS\" && {name} start-server");
        let output = f
            .pf()
            .env("ADB_BIN", &program)
            .env("HDC_BIN", &program)
            .env("DAEMON_TEST_ROOT", &f.0)
            .args([
                "lease", "--serial", "fake-a", "--", "/bin/sh", "-c", &command,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        f.until("daemon-fds.json");
        let inherited: Vec<i32> =
            serde_json::from_slice(&fs::read(f.0.join("daemon-fds.json")).unwrap()).unwrap();
        let next = f.try_device("fake-a");
        Command::new(&program)
            .arg("kill-server")
            .env("DAEMON_TEST_ROOT", &f.0)
            .status()
            .unwrap();
        assert!(
            inherited.is_empty(),
            "{name} daemon inherited {inherited:?}"
        );
        assert!(
            next.status.success(),
            "{name} server retained the device lock"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn real_adb_first_server_start_does_not_keep_the_device_locked() {
    let f = Fixture::new();
    // A private port: never stop a developer's server or contact a real phone.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    let adb = std::env::var("PHONEFARM_TEST_ADB").unwrap_or_else(|_| "adb".into());
    let stop = || {
        Command::new(&adb)
            .args(["-P", &port, "kill-server"])
            .env("ADB_MDNS_AUTO_CONNECT", "0")
            .output()
            .expect("install adb to run the server inheritance regression")
    };
    stop();
    let output = f
        .pf()
        .env("ADB_BIN", &adb)
        .env("ADB_MDNS_AUTO_CONNECT", "0")
        .env("ANDROID_USER_HOME", f.0.join("android"))
        .args([
            "lease",
            "--serial",
            "fake-a",
            "--",
            "/bin/sh",
            "-c",
            "test -n \"$PF_LOCK_FDS\" && adb -P \"$1\" devices",
            "adb-test",
            &port,
        ])
        .output()
        .unwrap();
    let next = f.try_device("wifi-a");
    stop();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("daemon started successfully"),
        "test must actually start a fresh server: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        next.status.success(),
        "real adb server retained the task lock: {}",
        String::from_utf8_lossy(&next.stderr)
    );
}

#[test]
fn inherited_scope_cannot_widen_and_closed_fds_are_reported() {
    let f = Fixture::new();
    let script = r#"import os,socket,subprocess,sys
pf=os.environ['PHONEFARM_BIN']
fds=tuple(int(fd) for fd in os.environ['PF_LOCK_FDS'].split(','))
for scope in (['--serial','wifi-a'],['--serial','fake-b'],[]):
    p=subprocess.run([pf,'lease',*scope,'--timeout-ms','0','--','/usr/bin/true'],pass_fds=fds,capture_output=True)
    assert (p.returncode==0)==(scope in (['--serial','wifi-a'],[])),(scope,p.stderr)
left,right=socket.socketpair()
p=subprocess.run([pf,'__lease-handoff',str(right.fileno())],pass_fds=(*fds,right.fileno()),capture_output=True)
assert p.returncode!=0 and b'does not cover' in p.stderr,p.stderr
p=subprocess.run([pf,'lease','--serial','fake-a','--','/usr/bin/true'],capture_output=True)
assert p.returncode!=0 and b'was closed' in p.stderr,p.stderr
"#;
    let out = f
        .pf()
        .args(["lease", "--serial", "fake-a", "--", "python3", "-c", script])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(f.try_device("fake-a").status.success());
}

#[test]
fn reset_cleanup_stops_orphaned_adb_client_and_releases_proxy_fds() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let adb = f.0.join("fake-adb");
    fs::write(
        &adb,
        r#"#!/usr/bin/env python3
import os,signal,time
from pathlib import Path
root=Path(os.environ['CLIENT_TEST_ROOT'])
signal.signal(signal.SIGINT,signal.SIG_IGN)
signal.signal(signal.SIGTERM,lambda *_:(root/'term-seen').touch())
signal.alarm(15)
(root/'client-ready').touch()
while True: time.sleep(.01)
"#,
    )
    .unwrap();
    fs::set_permissions(&adb, fs::Permissions::from_mode(0o755)).unwrap();
    let script = r#"import os,subprocess,time
from pathlib import Path
root=Path(os.environ['CLIENT_TEST_ROOT'])
subprocess.Popen(['adb','shell','dumpsys','gfxinfo'],pass_fds=tuple(int(fd) for fd in os.environ['PF_LOCK_FDS'].split(',')))
while not (root/'client-ready').exists():time.sleep(.01)
"#;
    let mut task = Group(
        f.pf()
            .env("ADB_BIN", &adb)
            .env("CLIENT_TEST_ROOT", &f.0)
            .args(["lease", "--serial", "fake-a", "--", "python3", "-c", script])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    f.until("client-ready");
    assert!(task.0.wait().unwrap().success());
    assert!(
        !f.try_device("wifi-a").status.success(),
        "live orphan proxy must retain its lock"
    );
    assert!(f.try_device("fake-b").status.success());
    let node = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../harness-ops/node");
    let cleanup = Command::new("python3").args(["-c",
        "import sys;sys.path.insert(0,sys.argv[1]);from job_runtime import terminate_group;print(terminate_group(int(sys.argv[2]),.1))"])
        .arg(node).arg(task.0.id().to_string()).output().unwrap();
    assert!(
        cleanup.status.success(),
        "{}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    assert!(f.0.join("term-seen").exists());
    let until = Instant::now() + Duration::from_secs(3);
    while !f.try_device("fake-a").status.success() {
        assert!(
            Instant::now() < until,
            "reset group cleanup left the device locked"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
