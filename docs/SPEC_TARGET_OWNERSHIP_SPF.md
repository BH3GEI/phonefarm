# Target queued-command origin and frozen submit grouping

Status: implementation in progress. Offline acceptance pending. Android/GPU,
installed-kernel semantic attestation and real performance acceptance NOT_RUN.
Refs HGamey/.github#19, #14, #18, #11.

Original base ef0e7b5bbfe77eb5fac1c8fcbf80cd469f68fe4b. Isolated checkout
`worktrees/phonefarm-target-ownership-spf`, `codex/target-ownership-spf`, created
with git worktree in the actual phonefarm repository because app cwd is OBSERVER
and no suitable managed checkout was attached. B's start-to-start sampler fix
796ea313dfe832d2de77c63fd8a82462b5e96b88 was independently cherry-picked as
52aac8b. Final dependency base is published 61530eaeabffca0aa36a9cce2a7759ec4c7f3ef5;
rebase skipped the already-upstream timing patch and preserved the exact tree
d78694b57a02ca1e5182a940af986fab4988a143. Original prebase history remains
`codex/target-ownership-spf-prebase`. B's workspace, immutable tools, prereview and older APKs stay unchanged.

## Fixed route and estimand

Explicit `render_attribution_profile_id=kgsl_queued_origin_spf_v1` is supported
only with explicit `goal=render_cadence`. Unknown profile IDs reject. Missing
profile preserves historical comm-only proxy semantics, never retroactively
certifies package attribution. No caller owner boolean, context subset, SPF,
limits, path or post-data route choice. Endpoint debugfs context headers are not
an accepted alternative: they do not prove a full process-owned command stream.

The estimand is **target-TGID original queued-command submit cadence during the
frozen measurement window**, grouped by a baseline-derived period. It is not
display FPS, completed GPU frames, process ownership of a context, GPU active
time, power efficiency, exact scene reset or complete rendered game frames.
The exact matching render comm/TID is diagnostic; selection uses all witnessed
threads in the target TGID, never the submitted event's executor comm/TID.

Primary pinned OnePlus SM8650 and SM8750 source references explain why queued
is synchronous with queue_cmds, whereas submitted can execute on a worker or
inline while serving unrelated pending contexts. Actual queued/lifecycle formats
have NOT yet been read from the current device. Matching format/proc/version
bytes are required; family source is a semantic design reference, not installed
kernel source attestation. That latter limitation remains explicitly false.

## Bounded producer and independent receiver

The receiver owns a fixed readonly collector. It captures one exact package,
UID/version, boot, /proc/version, PID/TGID/start ticks, **all** process tasks
(max256, task_count must equal /proc/status Threads and include process leader),
per-task status/stat/comm, single KGSL device `kgsl-3d0`, all seven event formats,
trace_clock capabilities, event/PID filters, CPU mask and per-CPU loss/buffer
entry stats (max64). Missing, mixed, unknown,
malformed, incomplete, duplicate or changed records reject. A single diagnostic
render comm must exist; its creator/command executor is not inferred from name.

Worker enables queued/submitted, context create/detach/destroy and sched
fork/exit under the existing owned ftrace restoration contract. The registered
route temporarily selects globally comparable **boot** trace clock and freezes
its exact original clock restore value. Clock is included in this route's device
snapshots and restored before after_run/restored snapshots. Unsupported clock,
formats or restoration receipts block. Historical scripts keep their route.

Trace starts before the complete pre-witness, then workload/ENV children start,
then a fixed lead and the measurement window. A fixed 3s tail is registered
separately as tail_end; a complete post-witness also runs while lifecycle tracing
is still continuous. Only after post-witness ends does the worker stop trace.
A mandatory third final witness supplies the stable off-state counts/loss.
Measurement, tail and full trace join bounds are distinct metadata fields;
pre/post intervals must be fully inside the continuous trace span, and final
stats must follow trace-off. This removes pre/post task-inventory gaps.
All three witness scopes/epochs agree, clock is boot, event enables are1,
event/PID filters/triggers are clear, and all CPU loss counts are0. Complete raw
trace line counts must exactly match final per-CPU buffer entries over prefix,
lead, measurement, tail and post-witness. No post/tail/collector time becomes a
metric. Trace count is bounded250000 and raw time/CPU/global order is checked.
The pre-collector runs before the thermal sampler starts, preserving its existing
measurement-window coverage without changing thermal policy/deadlines.

All measurement submitted packets require one preceding queued key `(ctx,ts)`
in that capture. Every measurement packet's queued origin must follow the end
of the complete pre-task witness plus one centisecond for /proc/uptime rounding,
before target/foreign classification; an earlier
unknown caller is BLOCKED, never silently classified foreign. All executor TIDs
are retained as diagnostics. Target queued
commands inside measurement must resolve by the frozen tail, else UNKNOWN,
never silently omitted. Lead/tail-only submits are counted as boundary diagnostics
and excluded from the estimand. Queued MARKER/SYNC are explicitly excluded GPU
classes and counted; unsupported submitted classes, zero command ts, unknown
flags, duplicate/replayed submitted or queued keys and sequence wrap reject.

Context create/detach/destroy affecting any observed command ctx rejects over
the whole span; those IDs identify a **capture-local opaque generation**, not a
cross-run object identity or process owner. Any fork/exit touching a witnessed
target task rejects. The same complete task epochs must exist at both endpoints;
unknown/new/reused identities never become target callers. New target contexts
or caller subsets cannot be selected for favorable results. The complete observed
target caller/context sets from all three baselines are frozen for heldout arms.
This includes all supported target commands queued inside measurement whose
submission resolves only in the fixed tail; those tail submits are excluded
from metrics but must still participate in the scope freeze.

Raw clock/loss/lifecycle fields do not prove all-window foreground. Device event
semantics and any unavailable events remain explicit qualification limitations;
no fallback to submitted common_pid, endpoint ctx owner or comm-only selection.

## Freeze and consumer chain

Conditions/run @3 contain the profile and initial raw witness FileRef; thermal
policy remains existing v1/v2. Admission @2 binds a create-only frozen render
contract. All three independent raw baseline windows must produce identical
scope, caller/context sets and grouping period. Existing lag1..6/ACF>=0.30
inference is used; unknown or nonharmonic ambiguous periods reject. Frozen
contract records raw SHA identities and baseline-derived SPF, never caller SPF.

prepare produces the contract; GOL freezes its enclosing admission SHA and
conditions SHA without new numeric thresholds. collect first reconstructs the
contract from all three baseline traces and witnesses, including explicit
synthetic sessions; qualification is never inferred from synthetic data. Raw
receiver reconstructs independently again before heldout measurement. Heldout
inference must equal frozen SPF/caller/context sets; calculation uses frozen
SPF. Any edit/missing witness, raw/derived contradiction or grouping change
rejects. UNKNOWN/interrupted sessions retain existing reconcile/new-session
barriers; no auto-repeat or hidden legacy route fallback.

Observations keep the existing thermal-dependent @1/@2 envelope for GOL,
append command-origin/grouping diagnostics and derived gates, and continue to
set eligible/gameproof/promotion false. Historical missing witnesses remain
unattributed. Hash matching proves integrity, not source authenticity.

## I/O, acceptance and limitations

Each readonly record <=64KiB, whole witness <=2MiB, raw trace <=128MiB; existing
same-fd Reader limits total cached evidence512MiB. Child execution/lease uses
existing bounded transport and remote stop/restore barriers. New clock/event
writes use the same intent journal and restore path, not an independent owner.

Acceptance must exercise actual CLI prepare -> real GOL freeze -> PF collect ->
raw receiver, using explicit controlled fake transport and always synthetic;
no fake source is relabeled live. Countercases cover same-name foreign caller,
worker executors, multiple contexts/callers, PID/TID epoch changes, fork/exit,
ctx reuse/wrap, loss, local clock, unsupported flags, unmatched/duplicate keys,
SPF2 -> 1/3, missing/tampered raw and rehashed frozen-contract contradiction, complete
queued→tail joins with new scope, positional printf identity/flag contradictions,
and lifecycle/witness gaps or origins before the complete pre-task table.
Full local cargo test + default Clippy and independent review required.

This delivery is a bounded command-origin receiver/producer mechanism. Device
format/loss/clock/lifecycle acceptance, installed kernel semantics, display/GPU
completion, foreground/scene reset, quality and real optimization proof are
NOT_RUN/UNKNOWN; no automatic promotion. B is sole future device owner.

Primary filter semantics reference:
[Linux event tracing](https://docs.kernel.org/trace/events.html) explains event
and PID filtering. The registered EMPTY sentinel is for empty set_event_pid and
set_event_notrace_pid bytes (v6.6 trace_events.c uses trace_pid_show, whose empty
list emits no records); it is not inferred from unrelated set_ftrace_pid's
`no pid` display. Any nonempty PID list rejects. Installed read acceptance is
still NOT_RUN.

The finite print-wire reader binds every required ctx/ts/caller/lifecycle key to
its exact positional REC field expression, not token presence. Conversion/argument
counts agree and command flag expressions have the unique complete registered
bit/name table. Swapped ctx/ts/parent/child fields, duplicate bits or changed
flag meanings reject even when token strings and outer hashes still look valid.
Matching wire structure does not attest installed-kernel behavior.

Android mksh's stdin route is `sh -s -- PACKAGE COMM`; `sh - PACKAGE COMM`
can instead treat PACKAGE as a script path and is rejected in the initial read
before installation/control. A real readonly invocation demonstrated that
argument difference; failure bytes and the corrected planned invocation are
kept separately. Worker file-path invocations keep `sh SCRIPT PACKAGE COMM`.
Post-witness trace events remain counted, but cannot resolve a measurement
command later than the separately frozen 3s tail_end.

### Bounded text producer after actual runtime timeout

The original producer used two external commands (`wc`/`cat`) per proc task
record. A real 190-task inventory hit its unchanged remote20s deadline after
88,470 bytes before formats and the terminal marker; that partial witness stays
UNKNOWN. The revised registered reader uses `LC_ALL=C` shell builtin `read` and
`printf` for newline-terminated proc/trace text, with the same64KiB per-record,
2MiB transport/output,256-task and20s execution limits. Missing file, empty
mandatory record, unterminated final line or size overflow aborts without its
END/whole-witness `#end`. It never drops tasks or extends the deadline. Proc
cmdline retains its separate bounded NUL-to-LF conversion. PID filter values
keep the old trailing-LF removal and EMPTY-sentinel meaning. Host fixtures prove
byte preservation and negative bounds, not Android runtime acceptance. A new
source-pinned device inventory is still required before claiming usable live
command-origin attribution.

### Native bulk reader after second actual timeout

The builtin-line revision also failed the unchanged actual20s limit (17,971bytes,
no#end). Neither shell revision is accepted as a usable device producer. A fixed
read-only C helper now receives a bounded NUL-framed request stream from the
registered shell, snapshots each regular proc/trace file with bulk read of at
most64KiB+1, and writes the same v2 witness bytes through one buffered process.
Request stream/output each2MiB, requests<=2048, shell complete tasks<=256 and
CPUs<=64, whole remote command20s remain fixed. Missing/short framing/end,
unknown operations, empty mandatory record, partial LF, embedded text NUL,
read/open/close/output errors or overflow fail without successful completion.
Proc cmdline has the existing NUL-to-LF/trailing-LF conversion; PID filters have
the existing EMPTY and trailing-LF semantics. No shell command is evaluated by
the helper and it performs no file writes.

Fixed source is built with the already-installed official r28c arm64 API30
compiler into an isolated artifact: `source.c`, `reader.elf`, `build.json`, `compiler.stdout`, `compiler.stderr`.
Source bytes/schema/target/flags/source and ELF hashes are checked by the host;
ELF64 little-endian AArch64 executable identity is mandatory. No binaries go
into Git, no caller qualification boolean is accepted, and a matching hash is
not independent source authentication or installed-kernel semantic proof.
Standalone B-owned inventory and actual event-format parse must run before
final whole-gate acceptance. The actual fork label `pid=` binds only to the
registered REC->parent_pid; mixed/swap/duplicate mapping rejects. Prepared
manifest contains all three baseline witness suffixes including final.raw.

Driver integration uses the existing single lease, unique remote owner, durable
recovery barrier, source/artifact snapshots and remote byte check for a bounded
helper-only installation before initial native witness. Full receiver contract
freezes that same helper before any probe/control sampling. This is a guarded
phase of the existing session, not a second owner/session or an automatic retry.
Legacy missing-profile route needs no native artifact. A requested native route
with absent or invalid artifact blocks, never falls back to either shell reader.


### Guarded native preinstall and recovery (execution contract v1)

An explicit queued-origin request must provide `render_reader_artifact`, the
local directory locator of the five-file fixed-source artifact. Missing or
invalid artifacts reject before transport. The receiver reads bounded regular
files from a no-follow directory fd, verifies the fixed compiled-in source,
receipt schema/target/flags, ELF64LE AArch64 PIE identity, source/ELF/compiler-log
hashes and false proof flags. These bytes are copied into the session and frozen;
matching hashes bind local provenance, not independent build authentication.

After readonly identity/control-restore inventory and before any main worker
installation, the host persists state and a complete preinstall input manifest,
then holds the existing lease for recovery. A unique `-reader` directory uses
the same session token and boot ID. Only fixed worker/bootstrap/collector/ELF/
arguments are installed there with byte readback and executable mode. The
`render_readonly` worker uses the existing durable master/child permit and
PID/start/PGID ledger. Its child has a remote20s deadline. This mode and its
failure trap never restore/write sysfs, settings or ftrace.

The initial witness is accepted only after successful child exit and parsing.
`reconcile_render` retires attributable helper children, revokes the helper
session and releases its master. A separate fixed closure checker verifies
both recorded master groups and child groups absent, exact token/boot and the
revocation receipt; only then can it remove this owned helper directory. The
host saves immutable closure and removal receipts before it persists the
main-install intent. Main installation/control/capture cannot precede these
receipts. Main worker witnesses consume the same frozen ELF as a third argument.

Interrupted or failed preinstall is explicitly reconciled under the same lease
barrier. Helper-only recovery is available only with the preinstall manifest,
known state/tools, and no main-install intent, full input manifest or later
control/capture/sample intent. Partial/unknown installation, PID reuse, live or
unknown master/group, malformed ledger, boot/owner changes or ambiguous cleanup
retain UNKNOWN and the barrier. It never invents a restore success. Once the
main phase starts, only the existing full control/ftrace reconciliation may
close the session, and it must also verify the helper closure. Helper shutdown
is not a substitute for main-worker quiescence or full snapshot restoration.
Normal/failed/interrupt cross-phase host cases exercise these exact callbacks;
synthetic transport remains synthetic and cannot certify the physical device.


The successful standalone v5 readonly record was independently parsed in full:
184 tasks and all seven actual print mappings were structurally accepted.
Its HOME/paused state, selected local clock and disabled events are not an arm
measurement; no origin/SPF/display/GPU/performance qualification is derived.
Comment-only trigger listings may contain blank lines: blank lines are ignored,
at least one nonempty comment is required, and any noncomment record rejects.
This is byte grammar acceptance, not clearing or rewriting device filters.

Preinstall's closed/removal files are immutable. An interrupt after remote
revocation but before durable complete removal receipt cannot be guessed safe;
that ambiguous boundary retains the barrier. If complete removal receipt is
already durable, helper-only recovery additionally checks the current helper
path absent. After main intent/full input manifest, helper-only recovery is
forbidden even if the helper itself is absent. Normal collection and full
reconciliation verify frozen helper closure and current absence as part of the
main execution closure. Prepared names include the native provenance files,
preinstall/intent/closure receipts and all three baseline final raw witnesses.

### Rejected baseline admission and observation diagnostics

Queued-origin heldout measurements depend on the baseline-derived grouping.
They must reconstruct and validate the admission before interpreting the heldout
trace; invalid or synthetic admission never supplies a usable grouping.

Historical comm-proxy observations have no such grouping dependency. After the
conditions, frozen plan and complete arm raw bytes pass structural checks, the
receiver retains their known legacy/loaded observation schema, purpose, raw
thermal binding and mechanical diagnostics even when baseline admission later
rejects. Rejection remains `BLOCKED`, `valid_observation=false`,
`baseline_admission_verified=false` and `eligible=false`; it cannot promote a
synthetic session or bypass any baseline, chronology, purpose or thermal rule.
Malformed/mixed conditions and capability-only profiles still reject before
these diagnostics. Do not relabel a known loaded arm as legacy merely because
its separate baseline admission is ineligible.
