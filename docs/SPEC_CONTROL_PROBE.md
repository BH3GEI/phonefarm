# Receiver-owned control capability probe

`phonefarm confirmation-driver probe-controls --request R --out NEW [--execute]`
accepts only `phonefarm/control_probe_request@1`, `transport {kind: adb|fake,
program: absolute regular executable, target: explicit serial}`, and `candidate`
(path to one existing sysparam control artifact), optional `temperature_profile_id`
(receiver preset ID; missing defaults to `legacy_cold_v1`). `loaded_steady_v1`
requires the new typed temperature producer/evaluator; unsupported integration
blocks instead of silently falling back. No package, workload, frame
budget, schedule or caller gates are accepted. Missing `--execute` with adb is
VALIDATED_ONLY and creates no session. Explicit fake remains synthetic.

This checks one legal configuration endpoint using the existing full control
sibling graph. It does not start applications, workload, ENV or KGSL capture.
It generates no baseline, confirmation plan or samples. Result eligible,
real_game_optimization_proven and promotion_allowed are always false. Readback
at 1 and 11 seconds proves only the configuration value remained readable as
requested at these boundaries, not actual GPU clocks or sustained performance.

Before remote writes: freeze request, candidate, trusted tool bytes, transport,
readonly node inventory, initial complete snapshot, restore graph, probe plan,
actual serial/boot and receiver rules. Hold the shared stable device lease and
durable recovery barrier. Root and original identity must be verified. The
legacy preset requires accepted SoC zones below 40C; loaded_steady_v1 uses
typed CPU/GPU/skin/battery caps plus a stable baseline-only 30-second warmup
and immutable reference bands. Thermal/fan/trip writes
remain prohibited. All tools execute from frozen files with existing bounded
transport, host-group permit and remote master/child ownership contracts.

Remote `control_probe` records complete snapshots before, immediately after
apply, after the 1+10 second readback hold, and after restoring controls. It
never writes ftrace. Existing ftrace settings are part of the complete snapshot
and must remain equal. Any unrelated state drift is BLOCKED, not silently
attributed to the probe. The recovery graph owns touched plus sibling domains.

Success closes the remote session under its master lock using
`reconcile_controls`, verifies complete restore, all owned groups absent and
matching permanent session-revocation receipt, then retires the host recovery
barrier. Existing `confirmation-driver reconcile --session S --execute` routes
control-only sessions to the same control-only closure path. Failures preserve
the barrier and cannot automatically retry, remove another owner, fabricate
quiescence or revalidate any old sample. A partial closure remains UNKNOWN.
The same token can never write again after its tombstone exists.

Frozen manifests require the exact fixed set plus the one probe plan; missing,
extra, changed or unknown source bytes refuse recovery rather than executing
unfrozen tools. A local adb executable hash is traceability, not remote hardware
attestation. Fake receipts prove only mechanics and never live device facts.

Dependencies: confirmation driver #25, raw observer #22, durable shared device
lease #19. This adds no charging control or unit adapter; power measurement,
true display timing, GPU output/quality, game workload and optimization claims
remain separately blocked until their contracts are actually satisfied.

## Temperature producer integration

`temperature_profile_id` is selected before control writes. The receiver's
versioned module resolves raw supported sensor metadata and trip units; it
never accepts request-defined caps. `thermal_profile_metadata.sh` records the
exact boot, sensor paths/types/domains/raw units and raw trip values. Shell
sensors without trip contracts are explicitly excluded diagnostics; `socd`
is not a Celsius SoC sensor. The resolved profile, raw metadata bytes/hash,
normalized assessments and generated bounded read-only collection script are
frozen before any remote write. Sensor path/type/keys must stay consistent.
Legacy cold retains its original scope. Loaded control probing has temperature
capability scope only; full performance confirmation additionally requires
frozen baseline-only reference bands and typed whole-window raw samples.


The loaded producer binds receiver rules frozen before warmup, actual serial/
boot, raw metadata/HAL/warmup bytes, host warmup start/finish, profile and exact
reference bytes in `thermal-evidence.json`. Sensor type readbacks match the
frozen collection paths. The same generated collection bytes are reconstructed
before resume/reconcile. Virtual fake timing remains explicitly synthetic and
cannot validate a device observation. See [thermal preset contract](SPEC_THERMAL_PROFILE.md).

## Loaded restore ownership timing

For `loaded_steady_v1`, receiver rules/metadata/HAL and the baseline-only warmup
are completed under the same lease before reading the initial complete control
snapshot and building its sibling restore graph. The snapshot owns the actual
post-warmup state; no candidate or remote installation writes have occurred.
This prevents an earlier cold-state vendor CPU bound from being mistaken for
the restore target of a loaded session. Pre-probe and restored snapshots must
still equal that exact frozen snapshot; later unrelated drift remains BLOCKED.
Legacy timing is unchanged.

## Separate absolute capability preset (new observations only)

`capability_absolute_v1` is an explicitly selected, receiver-owned preset accepted
only by `confirmation-driver probe-controls`. It is never a fallback from
`loaded_steady_v1` or `legacy_cold_v1`. Their numerical rules and all historical
failures remain unchanged. Performance prepare/collect/qualification and GOL
confirmation must reject this preset; no baseline, plan, statistical sample or
performance qualification is emitted.

This preset proves the narrower configuration readback/hold/restore capability.
It retains the loaded preset's typed CPU/GPU/skin/battery identity and unit
checks, minimum passive/HAL-severe-derived absolute caps, independent battery
45C cap, HAL Ready=true, no status override, and status 0–2 at every reading.
Before any ownership-bearing remote installation or candidate write it freezes
receiver rules, metadata and a >=30s, >=6 complete-reading window with strict
monotonic uptime and gaps <=6s. All points must satisfy absolute caps/HAL.
Per-sensor range and mean +/-2C diagnostic bands are retained, but range <=3C
and membership in those diagnostic bands are not capability predicates. The
diagnostic artifact has its own schema and cannot be used as a stable reference.

After this admission window the full initial control snapshot/sibling restore
graph is frozen. A fresh host snapshot and the remote frozen thermal tool check
absolute caps/HAL immediately before each write. The remote 11s hold samples
before apply and at post-apply +1/+6/+11s, checks configuration readback at +1
and +11, and emits an explicit thermal hold completion receipt binding actual
apply-hold start/end uptime and four full readings. Receiver reconstruction
requires exactly four readings, gaps <=6s, last reading at least 11s after the
recorded hold start, and the pre-apply reading before that start. Missing,
truncated, invalid-unit, hot/severe/override/unknown, incomplete hold or restore
evidence blocks and keeps the same recovery barrier. Temperature fluctuation
does not prove safe sustained clocks, identical workloads or comparable arms.

The exact frozen tool set includes `thermal-purpose.txt` declaring this preset.
The worker rejects it for every mode except `control_probe` and
`reconcile_controls`, before acquiring write ownership. Consumers recompute
all preset/diagnostic/tool bytes from retained raw data. Any mismatch or missing
purpose marker blocks. Capability success still requires the existing four
complete snapshots, exact initial/restored equality, owned-group quiescence and
permanent remote session closure. Result scope remains
`configuration_readback_only`, all performance/game/proof/promotion flags false.
Explicit fake remains synthetic. No fourth live attempt is authorized by this
host implementation change.
