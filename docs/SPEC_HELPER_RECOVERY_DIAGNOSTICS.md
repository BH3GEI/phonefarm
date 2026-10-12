# Owned helper-only recovery diagnostics v1

Base: exact phonefarm518f082c2979d4b1cc4f70bc47474d48f13cf47e.
This addition diagnoses the actual render_readonly transport deadline and
reconcile acquire_master85. It changes neither worker deadlines nor stop/restore
rules. The20s bootstrap cap only bounds the reader after permit; the28s worker
also includes durable handshake and whole-proc group verification. No remote
phase is inferred from empty host stdout/stderr.

`confirmation-driver diagnose-helper --session S --execute` is a new finite
read-only operation. It requires a complete current preinstall manifest and
exact receiver/native source files, matching helper intent/state and no main
install/sample/control intent. It acquires the existing recovery lease for
exact S/target, transfers only the existing dead/reaped local owner, and always
preserves its recovery marker. It never calls complete_recovery, remote worker,
stop, restore, session close, delete, normal lease or automatic retry.

The host freezes its own fixed diagnostic template and forwards it via the
supervised stdin transport, not caller argv/body. Only the state-derived exact
helper directory/token/boot are passed. Before any ledger read the template
checks current boot and exact helper owner; checks them again at completion.
No caller PID/path or general command is accepted. No remote installed tool is
executed. System shell primitives and bounded cat/wc/base64 are used to observe
fixed files only.

Fixed files are worker.lock/owner, render_initial/master.tsv, children.tsv,
launch-render_reader.pending, permit-render_reader, render_reader.log/stderr,
render_exit.txt and quiescence.txt plus journal.tsv. Single metadata record is
<=8KiB; reader stdout<=512KiB, stderr/journal<=128KiB; complete stdout<=2MiB and the supervised transport
has30s deadline with a separate remote20s read-only timeout. A host timeout is not remote-idle proof; both diagnostic timeouts leave the barrier intact.
Proc metadata is selected only by strict token/boot/render_initial
master ledger and the unique registered render_reader child ledger. The full
/proc scan is capped2048 PID directories and256 matched group members; each
stat record<=4096 bytes. Unknown/malformed/truncated/racing/inaccessible ledger
or proc reads are explicit UNKNOWN. Master/child stat/status/cmdline base64 are
bounded. Endpoint snapshots are diagnostics, not a process lifetime proof.

Completion means a complete finite diagnostic byte protocol plus successful exit and
recorded owner bytes matching the frozen token/boot. Boot/owner endpoint checks are
performed by the fixed template; the receiver does not certify process epochs or
remote-idle from these snapshots. Any main-phase marker presence, including a
dangling link or unknown metadata error, refuses this helper-only route. The receiver archives current bytes but every result remains
status UNKNOWN, eligible=false, recovery_completed=false, remote_idle_verified=false,
control_restore_claimed=false, promotion_allowed=false, real_game_optimization_proven=false.
A live/dead/mismatched process or apparently empty group cannot authorize cleanup.
The existing acquire_master/stop_owned rules remain authoritative. A failed or
interrupted diagnostic retains raw transport bytes and the same recovery barrier.
Main-phase sessions require full existing reconciliation and cannot select this
helper-only diagnostic.

Acceptance is actual fixed-template host-shell live/dead/epoch/corrupt/missing/
boot/owner/size/process-cap countercases and actual CLI helper-only rejection /
barrier preservation. Host fixtures are explicitly synthetic, not Android
quiescence or GPU/game proof. Full cargo test and default Clippy must pass on
frozen source before any separately authorized device-owner diagnostic. New
copied tool is signed; existing signed518 tool and actual failed phase remain
unchanged.
