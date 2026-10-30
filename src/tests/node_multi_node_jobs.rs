//! Routing uses one center identity, node-local workers and the real PF lock protocol.
#![cfg(unix)]
#[test]
fn multi_node_jobs_keep_device_selection_and_local_fd_exclusion() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../harness-ops/node/tests/integration_multi_node_jobs.py");
    let output = std::process::Command::new("python3")
        .arg(script)
        .arg(env!("CARGO_BIN_EXE_phonefarm"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}
