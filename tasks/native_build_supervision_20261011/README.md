# Offline native build supervision acceptance

The actual phonefarm CLI was exercised under isolated launcher process groups
with explicitly controlled fake NDK scripts. These scripts emit mechanical SPIR-V
and ELF fixtures; they do not perform Android code generation or GPU execution.

Final full `cargo test` passed **535 tests** (513 unit, 15 builder process,
1 native host, 6 adapter process). `cargo clippy --all-targets` exited 0 with
existing warnings; none point to the new supervision code or tests. Final logs
are `fixed-cargo-test.log` and `fixed-clippy.log`. Source commit, exact source
bytes and executable SHA are recorded in `verification.json`.

Independent review reproduced a blocked termination path: logging before SIGKILL
waited forever on a full unread stderr pipe. The fix kills the controlled group
before any diagnostic I/O. The identical actual CLI counterexample then exited
by SIGKILL in **1.027 seconds** with a one-second tool timeout, no outer kill,
and positive group absence. Both failure and fixed receipts remain under
`independent_review/`; the full suite includes this unread-pipe regression.
Earlier pre-fix suite logs are retained as historical evidence only.

Contract checks cover shared launcher/helper groups, explicit NDK and isolation
guards, version/compiler hangs, stream/output-file caps, and a closed-pipe child
which survives a successful launcher. The outer supervisor rejects that child
case. Inherited artifacts remain provisional with `requires_outer_quiescence=true`
and `outer_quiescence_verified=false`, including after artifact verification.

Real NDK and GOL wrapper construction remain NOT_RUN; every device/GPU/game proof,
deployability and promotion flag remains false. The candidate-prepare driver
keeps present-SDK construction blocked until separate integration acceptance.
