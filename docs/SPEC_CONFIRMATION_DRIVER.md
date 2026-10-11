# Independent confirmation sampling driver (draft v1)

Status: source implemented; offline fake acceptance; no device acceptance. Depends on phonefarm
PR19's exact device lease implementation and PR22's raw observation producer.
This branch starts at `516b7dd` and imports only PR19's lease module/SPEC from
`d03af6c`; shader/artifact/native-lifetime branches are not dependencies.

## Purpose and authority

Provide executable Android root/sysparam/KGSL/battery collection, including
restoration. This is not a record converter and does not invoke a model/API.
Phonefarm freezes the sampling contract, allowed control graph, repeatability
thresholds, metrics and noninferiority rules before probing or baseline data.
GOL assigns a candidate and role/run IDs, and registers the exact receiver plan;
it cannot edit the receiver policy or substitute booleans for raw receipts.

Commands:

- `confirmation-driver prepare --request R.json --out SESSION [--execute]`
- `confirmation-driver collect --session SESSION --frozen F.json [--execute]`
- `confirmation-driver reconcile --session SESSION --execute`

Without `--execute`, a real-transport request is validated without any transport
invocation or device writes. Explicit fake transport is a separate simulation
mode. It is always synthetic and never returns real experiment qualification.
The current development phase may run only fake transport; real transport is
source for the next separately authorized hardware session.

## Sessions and frozen bytes

Prepare and collect are distinct sessions. Each owns one lease through all
probes, samples, raw pulls and restoration. Registration happens between them.
No uninterrupted exclusion from initial baseline through final confirmation is
claimed. Collect rechecks serial/boot, frozen bytes, raw admission and the full
initial state; changes or uncertainty block before writing. Other programs,
other hosts and game state may still change during registration; that limitation
is preserved, rather than inferred away by snapshot equality.

The request declares the explicit transport target, package, render thread,
capture duration, lead time, bounded deterministic workload, candidate sysparam
artifact and pair/run IDs. It cannot supply qualification gates. v1 uses receiver
`render_efficiency` rules: required frame_p95/fps_mean/battery power, three
baseline runs, at least five counterbalanced pairs, alpha <= .05, cool <40C,
temp cap70C, existing5% dispersion/.9 drift thresholds. Receiver-owned metric
rules include every required metric and a positive primary improvement target.
Their exact policy/version and bytes are recorded before baseline/probe.

All request, rule, tool/script, workload and candidate bytes are bounded, hashed
and copied into a private immutable execution snapshot. Executions read that
snapshot, not the original caller paths. Hashing then reopening mutable inputs
does not satisfy the contract. Transport executable bytes and identity are
recorded; mutation of a frozen session blocks further operations.

The workload compiler supports a finite native Android tap/swipe/sleep replay
with explicit coordinates and duration. Shell/arbitrary code, app installation,
random monkey input and background gamepad daemons are excluded. The workload
must cover lead+capture+tail and has a bounded total duration. Its exit is
independently recorded. Foreground package/state is checked and recorded; the
driver does not infer that a moving camera is the same scene in every round.

## Actual identity, capability and lease

Require an explicit ADB target; no default serial/root and no device discovery
fallback. Read actual hardware serial/root ID/boot ID before any write; require
known identity and uid=0. Every run repeats the identity check. Reject HDC,
non-KGSL/unknown capability and unknown root before probe or tool installation.

Use PR19's stable host lease root and legacy bridge. Resolve/record both transport
target and physical identity, then acquire explicit lease ownership. The bridge
protects cooperating entrances even when a target has a wireless alias; it does
not prove exclusivity across hosts or direct ADB. Check owner/heartbeat around
each bounded command. Never invoke a nested automatically leased command.

The lease is held during normal error handling and restore. Uncertain commands,
timeout, orphan processes, ownership/heartbeat loss, failed restoration or
missing receipts leave a persistent recovery-required marker on normal return
as well as on crash. Kernel lock release is not device-idle proof. New collection
cannot clear that marker, automatically replay a sample, or silently start over.

## Controlled probe and restoration graph

Do not call the old full `probe_sysparam.sh`: it performs differential writes
without a complete trap/ownership journal. First take a complete fixed-domain
snapshot and known CPU-policy inventory, then construct a finite reversible
graph. Read-only capability collection uses the existing probe key format.
Only requested legal categories are probed, one bounded group at a time.

Supported control categories target the current sysparam whitelist:

| Category | Recovery group / gate |
| --- | --- |
| CPU policy governor/min/max | All three governor/max/min values; restore governor first, then frequency bounds; reject unknown original/available values or incoherent bounds |
| KGSL min/max powerlevel | Both max/min levels; reject unknown levels and inverted bounds |
| KGSL devfreq governor/min/max | Governor/max/min together; probe and snapshot must resolve the same actual path. Fixed canonical path supported; a dynamic path not represented consistently by the existing whitelist/snapshot is explicitly rejected before write |
| DDR/LLCC boost | Original boost plus readable frozen bounds/availability |
| Public peak/min refresh settings | Both recorded refresh settings, exact readback and declared available modes; null is a delete on restore, not the string "null" |
| Existing vendor refresh mode | Recorded original mode, bounded mode-index table and actual active-display readback; no guess that mode index is FPS |

Thermal/trip/cooling/fan/bcl/throttling are always rejected. Unknown/unreadable
originals or sibling domains cannot be written. Whitelist status is derived from
actual exit/readback/differential hold+restore receipts, never caller metadata.
After each probe group, restore it and compare the entire snapshot. A candidate
must pass existing validate_candidate/plan_text under the reconstructed whitelist.
No category is considered implemented merely because a DDR fixture passed.

Before every device write, append and sync the intent, exact frozen operation,
physical/boot identity, session/lease token, affected graph and original recovery
values. The remote worker keeps its own owned durable journal before internal
writes. Host journal ownership is not permission to alter a different device.
Every temporary device path is session-scoped; no shared global knob state file.
Probe and sample restoration never deletes another owner or unknown directory.

## One sample

1. Validate immutable session, lease, actual identity and complete original state.
2. Wait for a known SoC cpu/cpullc/gpuss temperature below the receiver cool cap;
   save raw thermal reads, real exit and timestamp. Unknown/timeout blocks.
3. Record started_at and before snapshot. Journal and apply the exact selected
   plan; both sampling-edge snapshots must show expected touched/sibling values.
4. Start owned deterministic workload and whole ENV sampler, then capture raw
   KGSL ftrace after the frozen lead. All children are bounded, recorded and
   awaited. Capture saves actual uptime metadata; ENV saves complete protocol and
   samples. Record each real workload/sampler/capture exit separately.
5. Quiesce only positively identified owned children, restore ftrace, and take
   snapshot_after_run **while the selected control remains applied**. Only then
   restore the selected control graph and take snapshot_restored and restore exit.
   Full fixed/CPU domains must match, including untouched keys. Failed/unknown
   ownership or restore leaves recovery required.
6. Pull raw receipts, bind exact bytes and invoke PR22 pure raw validation. No
   old summary or run_once exit can substitute for missing files. Stop on first
   failed real sample; never auto-repeat a heldout run under the same ID.

The complete ENV window belongs to exactly one sample and fits its host elapsed
time. Capture and ENV uptimes are sequential/nonoverlapping and share boot ID.
Event trace_clock position is not inferred from matching duration. Battery
power requires whole-window discharge evidence; no automatic charging or fan
control is inferred from the request.

## Admission, registration and confirmation output

Prepare collects at least three separately cooled/restored raw baseline runs,
rechecks receiver repeatability and writes PR22's
`phonefarm/confirmation_baseline_admission@1`, conditions and exact empty baseline
sysparam artifact. The candidate, workload, original state, tool hashes and probe
are also bound into the immutable session. It emits a receiver-owned GOL
`game_opt_loop/confirmation_plan@1` with admission SHA and raw exploration ID/hash
exclusions; the plan contains the exact accepted role schedule. No confirmation
data is collected before GOL's real freeze registration.

Collect consumes that frozen registration and rechecks its exact typed SHA and
plan equality. It follows counterbalanced AB/BA schedule order, stopping on any
failure. It writes one PR22 `confirmation_bundle@1` per arm, reconstructs
`confirmation_observation@1`, and archives GOL PR29's samples file with exact
report/raw/condition/artifact/admission bindings and measurement windows.

Single reports always remain experiment/eligible=false; proof and promotion
flags stay false. Source is live_device only for real ADB acquisition; PR22's
offline reconstruction labels it recorded_device. Fake transport retains
synthetic source and BLOCKED/UNQUALIFIED outcomes, including at GOL. Simulation
may exercise complete orchestration without treating its files as device proof.

## Explicit reconciliation

Reconcile is a distinct operator action, not resume/retry. It must acquire the
same kernel/bridge exclusion while retaining the persistent barrier, validate
the saved session token, actual serial and boot, and refuse a live/unknown old
owner or unrelated state. Saved owned PID/start identities are rechecked before
stopping processes; unknown ownership is a manual recovery gap. Recovery runs
only the frozen original-value plan and verifies a fresh complete snapshot.
It does not require the game to remain foreground: a crashed game must not
prevent restoring positively owned state. Sampling still requires foreground.
Only successful observed quiescence and recorded-state equality can clear the
driver's marker. It cannot mark old samples valid or resume an interrupted
confirmation schedule. Legacy `--confirm-device-idle` is merely an operator
assertion; it cannot manufacture driver restore receipts or sample qualification.

## Offline acceptance

Execute the real host driver against an explicit fake executable transport that
records actual argv/stdin/exits and simulates the same scoped worker protocol.
Verify commands exist for all legitimate control categories, snapshot/probe/
sampling/restoration order, exact frozen byte execution, seven independent
failure paths, distinct raw windows/roles, interruption and non-retry barrier,
cross-process lease exclusion/heartbeat, bounded parent and lingering-child
failure, explicit reconciliation and zero unexpected external calls.

Run PR22 on emitted bundles and actual GOL freeze/judge on emitted plans/samples;
synthetic outcomes must remain blocked with proof/promotion false. Real-device
eligibility, scene repeatability, actual root recovery, sustained frequency,
display timing and game optimization remain pending hardware acceptance.

## Execution and trust details

`paired_t_assumptions_acknowledged=true` is required explicitly in the request.
The receiver fixes primary `frame_p95_ms` improvement at 1%, and noninferiority
at 1% for frame_p95_ms/fps_mean/power_watt, family alpha .05. Collect reconstructs
that exact receiver plan and checks the sampling contract; caller-edited manifest
hashes cannot loosen these constants. Current receiver tool bytes must match.

The host executor opens stdin once, hashes those bytes against its frozen input
map, then sends an owned unlinked fd containing exactly those bytes. macOS does
not permit exec of `/dev/fd/N`; tool execution therefore uses a fresh private
create_new inode, mode0500, populated from the verified owned executable bytes.
No original caller executable/input path is reopened for execution. The private
executable remains until process completion for shebang interpreters. Same-uid
malicious writes are outside this trusted-local-execution boundary. Tool hashes
are not proof that a user-selected program named/kind `adb` is authentic or that
it cannot forge remote stdout. Device receipts and source remain declarations,
not remote acquisition attestation; genuine hardware acceptance is still pending.

A durable host command-pending marker precedes spawn. A bounded wrapper waits for
a permit; it cannot invoke transport before the child PID/group receipt is
atomically persisted. An unbound pending launch blocks reconciliation/manual
idle assertions. A bound live/unknown group blocks recovery. Killing local
transport never produces a remote-idle receipt.

Each remote main worker runs under setsid and a persistent exclusive worker.lock
holding session/boot/PID/start ticks/run. Other entries cannot execute controls.
Reconcile rejects a live/reused/unknown master or surviving old group. It may
replace a dead master's lock only after positive group absence and matching
session/boot identity. Every remote workload/sampler has a durable pending launch
before setsid and waits for a permit until its PID/start/group ledger is synced.
Unmatched pending launches are UNKNOWN/manual recovery gaps, never quiescence.
Every present child ledger/pending record must be a regular nonsymlink file,
nonempty, bounded, newline-terminated and have the exact frozen field grammar.
Children require known unique tags, positive PID/start ticks and their exact
receiver-owned script path; pending requires one matching tag/script row. Empty,
partial, malformed or duplicated records are UNKNOWN before any group stop or
restore. EOF without LF must never be treated as an empty process set. Master
ownership likewise requires a complete single record. A detected broken ledger
preserves the host barrier; no restore/clear receipt is manufactured.
Validation freezes the expected row count (at most two children or one pending
record); every subsequent shell read loop must consume that count exactly.
A reader error or premature EOF is UNKNOWN, even when a preceding grammar check
succeeded. These host fault-injection checks do not establish Android shell or
actual storage-error behavior.
Normal errors retain the master lock and host barrier. Remote frozen tool/graph
bytes are reread across registration and before each sample. Snapshot_after_run
remains while control is applied; snapshot_restored is recorded afterward.

Atomic marker/owner replacement uses create_new temporary files, file sync,
rename and directory sync. Journals sync before writes. Remote durable intents
use sync and ownership/boot checks. Old samples are never repaired into valid
observations by reconcile; a failed/interrupted session cannot collect again.

Fake has an explicit synthetic virtual host timeline solely for mechanical
bounded-duration/window validation. The real transport branch always records
actual host timestamps and elapsed commands; it has no simulation exception.
Fake reports, GOL samples and all proof/promotion flags remain blocked/false.

Successful reconciliation permanently revokes that remote session while holding
its exclusive master lock. A persistent `session.closed` tombstone is created
and synced before the closure receipt/master release. Normal worker entry,
master acquisition, write intents, installation and child bootstrap all reject
a tombstoned session; delayed old probe/sample commands cannot acquire after
recovery. A tombstone with a missing/failed receipt remains closed and uncertain:
the host retains its barrier and cannot replay reconciliation into old VALID
samples. It never deletes/reuses the remote session/tombstone. This prevents
late commands already submitted to transport from restarting writes after
recovery has proved the presently visible owned processes absent.

Host failure cleanup uses SIGKILL followed by at most one second of `try_wait`
polling; it never calls blocking `wait` after a deadline. Failure to reap/prove
group absence retains the marker; successful local kill still does not prove
remote idleness. Offline tests exercise these host mechanisms and extracted
remote terminal guards, not Android process/root semantics.

Legacy eval/autoloop capability preflights also reserve a distinct temporary
output directory per invocation: root/trace results must not borrow another
concurrent device probe's `_cmd.out`. Failure to reserve a directory is unknown
capability and cannot authorize writes. Fake CLI tests isolate both lease roots
and explicitly verify that a nonroot probe cannot consume a concurrent root
probe's output; this is host result isolation, not acquisition attestation.


## Versioned loaded temperature sampling

Optional `temperature_profile_id` defaults to legacy_cold_v1. Choosing
loaded_steady_v1 freezes receiver rules and sensor/HAL metadata before a
baseline-only warmup of at least 30 seconds. It emits conditions/run schema @2,
Policy version 2 and an immutable thermal contract, without changing historical
@1 40C/70C qualification. Limits never come from the candidate or caller.

One pre-arm typed receipt and a separate owned thermal sampler cover each
entire ENV/capture window. The third process group is governed by the same
persistent pending/PID/start/group/permit ledger as workload and ENV; unknown
ownership prevents restoring controls. The thermal sampler begins before ENV,
waits for a complete first reading, and samples until ENV duration + 3 seconds;
its terminal duration/interval/clock/count marker and exit0 are required. Read
failures, severe/unknown HAL state, missing sensor identity, warmup or reference
drift block the experiment. No thermal/fan/trip controls are written.

Typed thermal admission/window gates replace the v1 cold gate only for @2;
all other raw/control/restore/power/trace/independent-window requirements remain.
The output observation/sample-qualification schemas are @2. See
[SPEC_THERMAL_PROFILE](SPEC_THERMAL_PROFILE.md) and
[SPEC_LOADED_OBSERVATION](SPEC_LOADED_OBSERVATION.md) for wire and reconstruction.
These gates validate reported measurements, not remote hardware attestation,
true display frames or sustained requested GPU frequency.


## Explicit frame-only rendering goal

Request `goal` is optional and defaults to render_efficiency (three metrics,
verified battery window required). Explicit render_cadence freezes a new
receiver policy and exactly two rules: frame_p95_ms lower by at least 1%, and
fps_mean no more than 1% regression, alpha .05 and at least five AB/BA pairs.
Power raw artifacts/status/current/voltage remain diagnostic; unverified power
is null and no energy improvement is claimed. GPU submit cadence is a proxy,
not display smoothness. display_smoothness is rejected. Unknown/charging power
never silently switches the requested goal; a caller must explicitly request
and freeze a separate render_cadence experiment before its measurements.

Loaded initial control ownership is acquired after the receiver-only warmup:
freeze temperature rules and metadata, collect the baseline-only stable warmup,
then read the complete initial snapshot and construct the sibling restore graph.
All steps hold the same lease and precede installation or control writes. No
post-write snapshot can replace the frozen restore target. Later full snapshot
equality, conditions freeze, raw chronology and restoration gates stay strict.

Every loaded prepare probe revalidates a fresh typed snapshot against the frozen
profile and warmup reference immediately before invoking its worker. The worker
also runs the exact frozen `thermal_sample.sh` before each probe apply, including
ordinary prepare probes, and at the readback hold boundaries. Failed or unknown
checks precede control writes or trigger owned cleanup; earlier warmup and later
baseline rejection do not substitute for this per-write admission. Raw probe
thermal receipts remain capability evidence, not performance samples.

Loaded prepared manifests require each `pre-probe_N-thermal.raw.tsv` and
`pre-probe_N-thermal.json` receipt explicitly, in addition to the original probe
and admission artifacts. Resuming collect rejects missing or extra prepared
keys; a caller cannot drop these newly required control-admission receipts.

Thermal collection schedules sample starts using device uptime deadlines. Warmup
targets five seconds between starts; the query duration is subtracted from the
remaining wait rather than followed by another five-second sleep. The frozen
30-second, at-least-six-reading, <=6-second-gap and <=3C-span rules remain
unchanged. A slow query, unknown/backward clock or missed six-second deadline
blocks admission; cooling drift is independently rejected.

The same owned worker clock helper schedules loaded probe checks and the typed
window sampler. A probe waits through eleven seconds after apply before its
final readback; checks retain their frozen temperature/reference requirements.
Window collection targets one second between starts, may overrun that nominal
target while a query is active, and still rejects a gap beyond six seconds. It
never emits fabricated timestamps or catches up by reusing a sample. Host tests
with an actual delayed sample body validate scheduling only, not Android clocks,
thermal HAL latency, device quiescence or control restoration.

A failed thermal point retains its partial raw stdout with the original nonzero
exit code; it cannot emit a completion marker or become an admitted reading.
