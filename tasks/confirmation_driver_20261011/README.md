# Independent confirmation driver: offline evidence

This change provides executable `confirmation-driver prepare / collect /
reconcile`, with actual Android root/KGSL/sysparam command source. This session
ran **zero actual ADB/HDC/device commands, SDK downloads, model calls or training**.
Fake transport is explicit, remains synthetic, and GOL must return BLOCKED.

The historical complete archive uses production source `f526b31`; its
source/test/tool fingerprints and cross-CLI fixture path remain in
`verification_summary.json`. Current followup source `22641c3` and its fresh
full test/tool fingerprints are recorded separately in `lease_schema_followup.json`.
Actual prepare/collect/registration argv, environment overrides, exit status and
stdout/stderr are retained in each fixture's `host-cli-*.exit.json` receipts.
The original workload/candidate files are deliberately changed after prepare
to prove execution uses frozen bytes. Use a new fixture to repeat prepare; use
saved raw bundles for independent observation replay.

The final GOL executable is a private snapshot of the binary built from
`5149a3335c957984c5005ceafe25c99b74322c21`, SHA-256
`589667fa4675a2bc73c36eebe239cdeaa637c52604a357bd79b260d7647890d3`.
Historical `final_v3` evidence used the older `134e0df` binary, SHA `a152ba…`;
its complete raw fixture and verification remain local. `final_v4_default_tmp`
failed a qualification CLI assertion because of shared capability-probe output;
that failure and the original root/nonroot false-positive are preserved. They
are historical observations, not final-source validation.

Dependencies: PR22 `516b7dd` raw producer (branch base), and only PR19 `d03af6c`
lease module/SPEC imported then extended for persistent recovery ownership.
The lease also includes definite-spawn-error cancellation and strict malformed
marker handling. No shader/native artifact changes are included. Final integration must keep both
full PR19 routing and this updated lease module; use the driver version when
resolving the imported `devlease.rs` add/add conflict.

## Reproduce without a device

Build GOL separately and provide its actual CLI path:

```sh
PHONEFARM_DRIVER_GOL_BIN=/absolute/path/game_opt_loop \
PHONEFARM_DRIVER_TEST_ARTIFACT_DIR=/absolute/path/new-artifacts \
cargo test --offline --manifest-path src/Cargo.toml
```

The full control-category CLI test prepares 3 raw baselines, probes 13 nodes
(CPU governor/min/max, canonical KGSL devfreq governor/min/max, KGSL powerlevel
pair, DDR/LLCC, public refresh-rate pair and vendor mode), registers via actual
GOL freeze, executes 5 counterbalanced pairs, judges all 10 saved reports, then
rejects replay. Tool bytes, input mutation, complete frozen manifests, four
snapshots, cold receipts and separate sampling/workload/restore exits are checked.
Fixture roots are canonicalized so macOS `/var` and `/private/var` aliases cannot
create different artifact inventory keys for the two producer entry points.

Find the retained fixture containing `session/samples.json`, then reconstruct
each emitted bundle through the actual PR22 CLI and run GOL judge again:

```sh
python3 tasks/confirmation_driver_20261011/verify_local_evidence.py \
  --phonefarm /absolute/path/phonefarm \
  --gol /absolute/path/game_opt_loop \
  --fixture /absolute/path/confirmation-driver-PID-N \
  --out /absolute/path/new-verification-directory
```

Every reconstructed observation must match the driver JSON, remain BLOCKED,
synthetic, experiment/eligible=false, and the saved GOL report must read 10
report hashes while keeping `claim_scope=none`, proof/promotion=false.

## Final results

Final source: **568 passed** (535 unit, 10 driver CLI, 14 observation CLI,
9 qualification CLI). The complete driver group took 926.27 seconds on this
host. Default Clippy exited 0 with 53 binary / 58 test warnings (46 duplicates);
this is not a strict-clean claim. The independent CLI replay matched all ten
saved reports; GOL read all ten hashes and returned BLOCKED, with proof and
promotion false.

## Failure and ownership evidence

The CLI suite saves fake `argv.jsonl` (actual host invocations/stdin), each
session's `journal.jsonl`, command stdout/stderr and a persistent
`legacy/devlock.d/phonefarm-lease.json` when uncertainty remains. Cases cover:

- Unknown root before writes; failed probe, cooling, workload, ENV completion,
  restored snapshot, active snapshot or discharge evidence stop collection.
- Same device across different working directories is mutually exclusive;
  heartbeat renews, a known completed child releases, unbound pending commands
  block explicit reconciliation and legacy idle assertions.
- Parent exit with a live process group, boot change, uncertain terminal
  closure and failed old samples cannot produce success/VALID receipts.
- Reconcile after a crashed game can restore positively owned state, closes
  the remote session permanently, and a delayed old probe is rejected. Missing
  closure receipts preserve the barrier and cannot be repaired by replay.

Host unit tests execute bounded transports, FIFO rejection, frozen stdin and
atomic permit publication; extracted shell tests check reachable min/max order
and terminal entry/intent/bootstrap guards. Extracted production ledger/parser
tests hold an actual local process group and reject partial records or failed
reads before recovery. Strict host marker checks reject incomplete PID/PGID pairs
and wrongly typed booleans in every release/recovery path. Capability preflight
uses an atomic per-call private directory; allocation failure remains unknown,
and concurrent root/nonroot fake requests cannot share command output.
The fake executable simulates the
worker protocol; it **does not execute the full Android worker**. Root restoration,
remote `setsid`/process identity, procfs permissions, supported hardware nodes,
KGSL timing and battery measurement need a separately authorized hardware run.

Full raw evidence remains local in this task folder. Only this README,
reproduction helper and compact verification summary are committed; large logs,
executable snapshots and raw captures are excluded.

## Lease schema followup (2026-10-12)

Source `22641c3` additionally refuses unfamiliar format-1 top-level fields and
new marker versions in every marker consumer/Drop. The unit counterexample
checks the exact schema error and unchanged marker bytes across active checks,
admission, manual recovery, recovery ownership transfer and Drop. The original
PR19 now includes the complete generic lease lifecycle backport (`28a4868`,
evidence `4e171f7`, 533 tests); it imports no driver module. Both branches retain
the same strict recovery fields and independent child-group lifecycle.

Fresh full source gate: **568 passed** (535 unit, 10 driver CLI, 14 observation CLI,
9 qualification CLI); driver group 634.13 seconds. Default Clippy exited 0,
53 binary / 58 test warnings (46 duplicates), not strict clean. PF binary SHA-256
`c2cba01d5e82397620210f4e15d62d8d32d9c3351f179b9432bb038d7fbc8bb6`;
the actual GOL executable remained the `5149a3` / `589667fa…` build above.
The current full test used default temporary fixtures, which are cleaned by
the suite; it does not relabel the retained historical `final_v5` independent
ten-bundle archive. Both runs remain synthetic/BLOCKED with false proof and
promotion, zero real device/API execution and no Android worker acceptance.
