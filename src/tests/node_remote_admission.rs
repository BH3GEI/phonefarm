//! Node gateway with real PF fd ownership and a fake Android transport.
#![cfg(unix)]
#[test]
fn node_remote_uses_pf_for_device_and_global_exclusion() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness-ops/node/tests/integration_remote_admission.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_phonefarm"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn center_routes_jobs_and_remote_commands_files_and_frames_to_child() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness-ops/node/tests/integration_multi_node_control.py");
    let output = std::process::Command::new("python3")
        .arg(script).arg(env!("CARGO_BIN_EXE_phonefarm")).output().unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
