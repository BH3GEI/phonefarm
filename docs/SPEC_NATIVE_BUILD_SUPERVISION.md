# Native build supervision interface

Refs [system #14](https://github.com/HGamey/.github/issues/14). Stacked on
[phonefarm #21](https://github.com/HGamey/phonefarm/pull/21). This changes only the
offline builder's process contract; no SDK installation, device or GPU execution.

## Problem and explicit mode

The existing builder places each NDK helper in a new process group. A wrapper
which controls the phonefarm launcher group therefore cannot stop those groups
by killing only the launcher. Preserve this standalone behavior and add:

```
phonefarm gray-artifact build ... --ndk EXPLICIT_DIR --process-supervision inherited-group
```

The inherited mode requires a Unix host, an explicit NDK path, and phonefarm to
be the leader of its current process group (`getpgrp() == getpid()`). An external
supervisor must create that fresh isolated group and retain its identity. Reject
unsupported values, use on other commands, or a launcher sharing the caller's
group before any tool execution. Do not infer this mode from environment values.

In this mode every version probe and compiler/inspection helper inherits the
launcher's group. Helpers must be trusted tools which do not deliberately detach.
This is process supervision, not a sandbox or a dependency authenticity claim.

## Failure and final retirement

Retain existing per-tool wall, stream and output-file caps. A helper timeout,
overflow or uncertain wait terminates the entire inherited group, including the
phonefarm process, so the wrapper must retain its InFlight journal as UNKNOWN
or failed according to its actual receipts. A signal exit never implies a
restored or valid artifact. If group termination cannot be established, hold the
outer recovery barrier; do not retry automatically.

Termination must precede any diagnostic I/O. An outer supervisor may leave a
full stderr pipe unread; synchronous logging must not delay killing the owned
group. The outer exit receipt records this failure even when no JSON can be
emitted by the terminated launcher.

A successful helper exit with closed pipes does not prove that other inherited
descendants stopped. The launcher remains in the group, so it cannot certify
group absence itself. Its output is explicitly provisional. The wrapper must:

1. Reap the actual launcher and obtain its real exit status under its budget.
2. Positively prove that the controlled group is absent; a live or unknown group
   must be terminated and cannot produce an accepted preparation result.
3. Only after absence, re-read and verify artifact bytes and all frozen expected
   identities. Preserve the outer journal, group/exit receipts and final hashes.

Outer-supervisor crashes, intentionally detached helpers, NDK dynamic libraries
and sysroot authenticity remain outside this interface. The wrapper must retain
interrupted state as UNKNOWN; this interface does not promise immediate cleanup
after the supervisor itself dies.

## Artifact declarations

Freeze the supervision mode in the descriptor, hence in the artifact identity.
Old descriptors omit the default standalone mode and remain readable without an
identity rewrite. Inherited artifacts use state
`built_offline_outer_quiescence_required`; build and verify always return
`requires_outer_quiescence=true` and `outer_quiescence_verified=false`.
Verification checks state/mode agreement and hashes, but cannot accept an outer
quiescence declaration or promote this state. The outer preparation journal
owns that additional evidence. All manifests retain `deployable=false` and
`device_validation=not_run` regardless of tool success or group retirement.

## Offline acceptance

Use actual phonefarm child processes with explicitly controlled fake NDK tools:
record each tool's group, verify all share the isolated launcher group, and
verify provisional build/verification state. Exercise invalid/non-isolated mode,
missing explicit tools, version probe and compile hangs, output overflow, and
successful helpers leaving closed-pipe background descendants. The outer test
supervisor must reject the latter and stop the group; no false final success.
Exercise a full, undrained stderr pipe during a timed-out version probe and
require bounded group termination without an outer watchdog kill.
Also retain default standalone, adaptation, identity and tamper regressions.

Run the full Rust tests and clippy. Fake compiler outputs are mechanism fixtures,
not Android code generation or GPU evidence. A real NDK build and the actual
game_opt_loop wrapper integration require later separate acceptance.
