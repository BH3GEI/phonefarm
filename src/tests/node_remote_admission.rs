//! Node gateway with real PF fd ownership and a fake Android transport.
#![cfg(unix)]

/// These cases drive an external orchestrator (HGamey harness-ops node service) against
/// the real PF binary. They run only when that checkout is present: `HARNESS_OPS_DIR`,
/// or a sibling `harness-ops/` as in the HGamey monorepo. Otherwise they skip.
fn harness_ops_script(name: &str) -> Option<std::path::PathBuf> {
    let root = std::env::var_os("HARNESS_OPS_DIR").map(std::path::PathBuf::from).unwrap_or_else(||
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../harness-ops"));
    let script = root.join("node/tests").join(name);
    if script.is_file() { return Some(script); }
    eprintln!("skip: {} not found (set HARNESS_OPS_DIR to run)", script.display());
    None
}
#[test]
fn node_remote_uses_pf_for_device_and_global_exclusion() {
    let Some(script) = harness_ops_script("integration_remote_admission.py") else { return };
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
    let Some(script) = harness_ops_script("integration_multi_node_control.py") else { return };
    let output = std::process::Command::new("python3")
        .arg(script).arg(env!("CARGO_BIN_EXE_phonefarm")).output().unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
