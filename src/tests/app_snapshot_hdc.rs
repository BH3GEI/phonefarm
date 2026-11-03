//! Reproduce the HUAWEI transport cost: every hdc shell invocation costs 1.5 s.
#![cfg(unix)]
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command, time::Instant};

struct Fixture(PathBuf);
impl Fixture {
    fn new(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!("pf-app-hdc-{}-{mode}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let hdc = root.join("hdc");
        fs::write(
            &hdc,
            r#"#!/usr/bin/env python3
import json,os,sys,time
args=sys.argv[1:]
if args==['list','targets']: sys.exit(0)
assert args[:4]==['-p','-t','harmony-test','shell'],args
with open(os.environ['HDC_CALLS'],'a') as log: log.write(json.dumps(args)+'\n')
time.sleep(1.5)  # Measured transport latency, not synchronization or a retry.
os.execv('/bin/sh',['sh','-c',args[4]])
"#,
        )
        .unwrap();
        fs::set_permissions(&hdc, fs::Permissions::from_mode(0o755)).unwrap();
        let device = root.join("device-command");
        fs::write(&device, r#"#!/usr/bin/env python3
import json,os,pathlib,signal,sys
name=pathlib.Path(sys.argv[0]).name
args=[name,*sys.argv[1:]]
mode=os.environ['APP_MODE']
with open(os.environ['DEVICE_CALLS'],'a') as log: log.write(json.dumps(args)+'\n')
if name=='aa':
    assert args==['aa','dump','-a'],args
    if mode=='fail-aa': sys.exit(3)
    print('bundle name [com.example.game]\nmain name [Main]\nstate #FOREGROUND')
elif name=='ps':
    assert args==['ps','-A','-o','NAME'],args
    if mode=='hung':
        pathlib.Path(os.environ['HUNG_PID']).write_text(str(os.getpid()))
        signal.alarm(20)
        signal.pause()
    print('NAME\n  com.example.game\ncom.example.game:renderer\ncom.example.megacity\ncom.ohos.sceneboard')
elif args==['bm','dump','-a']:
    print('OK!\ncom.example.game\ncom.example.megacity\ncom.example.stopped\ncom.ohos.sceneboard')
else:
    assert args[:3]==['bm','dump','-n'],args
    bundle=args[3]
    assert bundle in ('com.example.game','com.example.megacity','com.ohos.sceneboard'),args
    if mode=='fail-bm' and bundle=='com.example.megacity': sys.exit(9)
    print(bundle+':\n'+json.dumps({'applicationInfo':{'isSystemApp':bundle=='com.ohos.sceneboard'}}))
"#).unwrap();
        fs::set_permissions(&device, fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["aa", "ps", "bm"] {
            std::os::unix::fs::symlink(&device, root.join(name)).unwrap();
        }
        Self(root)
    }

    fn run(&self, mode: &str) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_phonefarm"))
            .args(["app-snapshot", "--serial", "hdc:harmony-test"])
            .env("HDC_BIN", self.0.join("hdc"))
            .env(
                "PATH",
                format!("{}:{}", self.0.display(), std::env::var("PATH").unwrap()),
            )
            .env("TMPDIR", &self.0)
            .env("HDC_CALLS", self.0.join("hdc-calls"))
            .env("DEVICE_CALLS", self.0.join("device-calls"))
            .env("HUNG_PID", self.0.join("hung-pid"))
            .env("APP_MODE", mode)
            .env("DEVLOCK_DIR", self.0.join("locks"))
            .env("PHONEFARM_LEASE_DIR", self.0.join("leases"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn calls(&self, name: &str) -> Vec<Value> {
        fs::read_to_string(self.0.join(name))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn slow_hdc_samples_all_apps_in_one_shell_round_trip() {
    let fixture = Fixture::new("ok");
    let value = fixture.run("ok");
    assert_eq!(value["foreground"], "com.example.game", "{value}");
    assert_eq!(
        value["running_third_party"],
        json!(["com.example.game", "com.example.megacity"]),
        "{value}"
    );
    assert_eq!(value["errors"], json!([]), "{value}");
    assert_eq!(
        fixture.calls("hdc-calls").len(),
        1,
        "including per-bundle detail reads"
    );
    assert_eq!(
        fixture.calls("device-calls"),
        vec![
            json!(["aa", "dump", "-a"]),
            json!(["ps", "-A", "-o", "NAME"]),
            json!(["bm", "dump", "-a"]),
            json!(["bm", "dump", "-n", "com.example.game"]),
            json!(["bm", "dump", "-n", "com.example.megacity"]),
            json!(["bm", "dump", "-n", "com.ohos.sceneboard"])
        ]
    );
    assert!(!fixture.0.join("locks").exists());
}

#[test]
fn failed_commands_in_the_batch_remain_unavailable_instead_of_empty() {
    for mode in ["fail-aa", "fail-bm"] {
        let fixture = Fixture::new(mode);
        let value = fixture.run(mode);
        if mode == "fail-aa" {
            assert!(value["foreground"].is_null(), "{value}");
            assert_eq!(
                value["running_third_party"],
                json!(["com.example.game", "com.example.megacity"]),
                "{value}"
            );
            assert!(value["errors"].to_string().contains("exit 3"), "{value}");
        } else {
            assert_eq!(value["foreground"], "com.example.game", "{value}");
            assert!(value["running_third_party"].is_null(), "{value}");
            assert!(value["errors"].to_string().contains("exit 9"), "{value}");
        }
        assert_eq!(fixture.calls("hdc-calls").len(), 1);
    }
}

#[test]
fn a_stuck_batched_device_command_is_still_bounded_and_reaped() {
    let fixture = Fixture::new("hung");
    let started = Instant::now();
    let value = fixture.run("hung");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(7),
        "{value}"
    );
    assert!(!value["errors"].as_array().unwrap().is_empty(), "{value}");
    assert!(value["running_third_party"].is_null(), "{value}");
    let pid = fs::read_to_string(fixture.0.join("hung-pid")).unwrap();
    let ps = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&ps.stdout);
    assert!(
        state.trim().is_empty() || state.contains('Z'),
        "device command survived timeout: {state}"
    );
    assert!(!fixture.0.join("locks").exists());
}
