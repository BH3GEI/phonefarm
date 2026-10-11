//! Controlled compiler fixtures validate orchestration, not Android code generation.
#[path = "../upopabi.rs"]
mod upopabi;
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static SEQ: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "phonefarm-artifact-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_phonefarm"))
        .arg("gray-artifact")
        .args(args)
        .env_remove("REFBENCH_NDK")
        .env_remove("ANDROID_SDK_ROOT")
        .output()
        .unwrap()
}
fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}
fn pystr(p: &Path) -> String {
    serde_json::to_string(path(p)).unwrap()
}
fn executable(p: &Path, body: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p,format!("#!/usr/bin/env python3\nimport sys,pathlib,time\nif '--version' in sys.argv:\n print('controlled offline compiler fixture v1');sys.exit(0)\n{body}\n")).unwrap();
    #[cfg(unix)]
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
}
fn ndk(temp: &Path, glslc_body: Option<&str>, native_body: Option<&str>) -> PathBuf {
    let n = temp.join("fixture-ndk");
    let bin = n.join("toolchains/llvm/prebuilt/linux-x86_64/bin");
    let fixture = temp.join("valid.spv");
    fs::write(
        &fixture,
        upopabi::tests::fixture(0, 16, false, false, false),
    )
    .unwrap();
    let shader = format!("out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);out.write_bytes(pathlib.Path({}).read_bytes())",pystr(&fixture));
    executable(
        &n.join("shader-tools/linux-x86_64/glslc"),
        glslc_body.unwrap_or(&shader),
    );
    let native = "out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);d=bytearray(64);d[:6]=b'\\x7fELF\\x02\\x01';d[16:20]=b'\\x03\\x00\\xb7\\x00';out.write_bytes(d)";
    executable(
        &bin.join("aarch64-linux-android30-clang++"),
        native_body.unwrap_or(native),
    );
    executable(&bin.join("llvm-nm"),"print('000 T vkNegotiateLoaderLayerInterfaceVersion\\n000 T vkGetInstanceProcAddr\\n000 T vkGetDeviceProcAddr\\n000 T vkEnumerateInstanceLayerProperties\\n000 T vkEnumerateInstanceExtensionProperties')");
    executable(&bin.join("llvm-readelf"),"print('0x1 (NEEDED) Shared library: [liblog.so]\\n0x1 (NEEDED) Shared library: [libc.so]')");
    n
}
fn build(temp: &Path, n: &Path) -> Output {
    build_timeout(temp, n, "10")
}
fn build_timeout(temp: &Path, n: &Path, seconds: &str) -> Output {
    cli(&[
        "build",
        "--source",
        path(&temp.join("candidate.comp")),
        "--out",
        path(&temp.join("artifacts")),
        "--ndk",
        path(n),
        "--timeout-seconds",
        seconds,
    ])
}
fn published(temp: &Path) -> Vec<PathBuf> {
    fs::read_dir(temp.join("artifacts"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("upop-")
        })
        .collect()
}

#[test]
fn inspect_is_a_real_cross_process_spirv_gate() {
    let temp = Temp::new();
    let file = temp.0.join("valid.spv");
    fs::write(&file, upopabi::tests::fixture(0, 16, false, false, false)).unwrap();
    let out = cli(&["inspect", "--spirv", path(&file)]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["scope"],
        "abi_only"
    );
    fs::write(&file, upopabi::tests::fixture(0, 16, false, true, false)).unwrap();
    assert!(!cli(&["inspect", "--spirv", path(&file)]).status.success());
}

#[test]
fn fixture_build_archives_snapshot_checks_reuse_and_tampering() {
    let temp = Temp::new();
    fs::write(
        temp.0.join("candidate.comp"),
        "// exact original source snapshot\n",
    )
    .unwrap();
    let n = ndk(&temp.0, None, None);
    let out = build(&temp.0, &n);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let dir = PathBuf::from(v["artifact"].as_str().unwrap());
    assert_eq!(v["device_validation"], "not_run");
    assert_eq!(
        fs::read(dir.join("candidate.comp")).unwrap(),
        fs::read(temp.0.join("candidate.comp")).unwrap()
    );
    let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        m["descriptor"]["abi"]["local_size"],
        serde_json::json!([16, 16, 1])
    );
    assert!(cli(&["verify", "--artifact", path(&dir)]).status.success());
    let again = build(&temp.0, &n);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&again.stdout).unwrap()["reused"],
        true
    );
    assert!(m["descriptor"]["inputs"]["dispatch_ownership.h"].is_string());
    let ownership = fs::read(dir.join("dispatch_ownership.h")).unwrap();
    fs::write(dir.join("dispatch_ownership.h"), b"changed ownership").unwrap();
    assert!(!cli(&["verify", "--artifact", path(&dir)]).status.success());
    fs::write(dir.join("dispatch_ownership.h"), ownership).unwrap();
    assert!(cli(&["verify", "--artifact", path(&dir)]).status.success());
    fs::write(dir.join("candidate.spv"), b"tampered").unwrap();
    assert!(!cli(&["verify", "--artifact", path(&dir)]).status.success());
    let reuse = build(&temp.0, &n);
    assert!(!reuse.status.success());
    assert!(String::from_utf8_lossy(&reuse.stderr).contains("hash mismatch"));
}

#[test]
fn compiler_failure_missing_tools_wrong_abi_and_snapshot_mutation_publish_nothing() {
    for mode in [
        "missing",
        "compiler_error",
        "storage_abi",
        "mutate_snapshot",
        "tamper_metadata",
    ] {
        let temp = Temp::new();
        fs::write(temp.0.join("candidate.comp"), "// source\n").unwrap();
        let n = match mode {
            "missing" => temp.0.join("not-installed-ndk"),
            "compiler_error" => ndk(&temp.0,Some("print('GLSL rejected',file=sys.stderr);sys.exit(9)"),None),
            "storage_abi" => { let n = ndk(&temp.0,None,None); fs::write(temp.0.join("valid.spv"),upopabi::tests::fixture(0,16,false,true,false)).unwrap(); n },
            "mutate_snapshot" => {
                let body = format!("src=pathlib.Path(sys.argv[sys.argv.index('-o')-1]);src.write_text('changed');out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);out.write_bytes(pathlib.Path({}).read_bytes())",pystr(&temp.0.join("valid.spv")));
                ndk(&temp.0,Some(&body),None)
            },
            "tamper_metadata" => ndk(&temp.0,None,Some("out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);(out.parent/'artifact_metadata.h').write_text('wrong');d=bytearray(64);d[:6]=b'\\x7fELF\\x02\\x01';d[16:20]=b'\\x03\\x00\\xb7\\x00';out.write_bytes(d)")),
            _=>unreachable!(),
        };
        let out = build(&temp.0, &n);
        assert!(!out.status.success(), "mode {mode}");
        assert!(published(&temp.0).is_empty(), "mode {mode}");
        let stage = fs::read_dir(temp.0.join("artifacts"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(
            stage.join("failure.json").exists(),
            "mode {mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stage.join("upop.comp").exists(),
            "must never compile legacy candidate"
        );
    }
}

#[test]
fn compiler_hang_and_spam_are_bounded_and_fail_closed() {
    for body in ["time.sleep(30)", "while True: print('x'*65536,flush=True)",
        "import subprocess;subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)']);sys.exit(0)",
        "out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);out.write_bytes(bytes(17*1024*1024))"] {
        let temp = Temp::new();
        fs::write(temp.0.join("candidate.comp"), "// source\n").unwrap();
        let n = ndk(&temp.0, Some(body), None);
        let start = Instant::now();
        let out = build_timeout(&temp.0, &n,"2");
        assert!(!out.status.success());
        assert!(start.elapsed().as_secs() < 10);
        assert!(published(&temp.0).is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("limit")
                || String::from_utf8_lossy(&out.stderr).contains("timeout")
        );
    }
}

#[test]
fn version_probe_hang_is_bounded_too() {
    let temp = Temp::new();
    fs::write(temp.0.join("candidate.comp"), "// source\n").unwrap();
    let n = ndk(&temp.0, None, None);
    fs::write(
        n.join("shader-tools/linux-x86_64/glslc"),
        "#!/usr/bin/env python3\nimport time\ntime.sleep(30)\n",
    )
    .unwrap();
    let start = Instant::now();
    let out = build_timeout(&temp.0, &n, "2");
    assert!(!out.status.success());
    assert!(start.elapsed().as_secs() < 10);
    assert!(String::from_utf8_lossy(&out.stderr).contains("timeout"));
    assert!(published(&temp.0).is_empty());
}

#[test]
fn compiler_reads_snapshot_even_if_original_source_changes() {
    let temp = Temp::new();
    let original = temp.0.join("candidate.comp");
    fs::write(&original, "// original\n").unwrap();
    let body = format!("src=pathlib.Path(sys.argv[sys.argv.index('-o')-1]);out=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);\nif src.name=='candidate.comp':\n assert src.read_text()=='// original\\n';pathlib.Path({}).write_text('// changed outside snapshot\\n')\nout.write_bytes(pathlib.Path({}).read_bytes())",pystr(&original),pystr(&temp.0.join("valid.spv")));
    let n = ndk(&temp.0, Some(&body), None);
    let out = build(&temp.0, &n);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = PathBuf::from(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["artifact"]
            .as_str()
            .unwrap(),
    );
    assert_eq!(
        fs::read_to_string(dir.join("candidate.comp")).unwrap(),
        "// original\n"
    );
    assert_ne!(fs::read_to_string(&original).unwrap(), "// original\n");
    fs::write(dir.join("untracked"), "extra").unwrap();
    assert!(!cli(&["verify", "--artifact", path(&dir)]).status.success());
    fs::remove_file(dir.join("untracked")).unwrap();
    #[cfg(unix)]
    {
        let snapshot = dir.join("candidate.comp");
        let bytes = fs::read(&snapshot).unwrap();
        fs::write(&original, bytes).unwrap();
        fs::remove_file(&snapshot).unwrap();
        std::os::unix::fs::symlink(&original, &snapshot).unwrap();
        assert!(!cli(&["verify", "--artifact", path(&dir)]).status.success());
    }
}

#[test]
fn oversize_source_is_rejected_before_tools_or_staging() {
    let temp = Temp::new();
    fs::write(
        temp.0.join("candidate.comp"),
        vec![b' '; 4 * 1024 * 1024 + 1],
    )
    .unwrap();
    let out = build(&temp.0, &temp.0.join("none"));
    assert!(!out.status.success());
    assert!(!temp.0.join("artifacts").exists());
}

#[test]
fn compiler_background_writer_with_closed_pipes_is_rejected() {
    let temp = Temp::new();
    fs::write(temp.0.join("candidate.comp"), "// source\n").unwrap();
    let body = format!("import subprocess\nout=pathlib.Path(sys.argv[sys.argv.index('-o')+1]);out.write_bytes(pathlib.Path({}).read_bytes())\nsubprocess.Popen([sys.executable,'-c',\"import os,time,pathlib;os.close(1);os.close(2);time.sleep(30);pathlib.Path(\"+repr(str(out.parent/'late-write'))+\").write_text('late')\"])\n",pystr(&temp.0.join("valid.spv")));
    let n = ndk(&temp.0, Some(&body), None);
    let out = build_timeout(&temp.0, &n, "3");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("background descendants"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(published(&temp.0).is_empty());
    let stage = fs::read_dir(temp.0.join("artifacts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(!stage.join("late-write").exists());
}
