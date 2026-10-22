# Offline confirmation observation contract evidence

This change depends on phonefarm PR18 (`codex/experiment-qualification`) and the
single-observation consumer in game_opt_loop PR29. Retarget this branch to `main`
after PR18 lands; do not treat the stacked diff as an independent replacement.

## Implemented scope

`phonefarm confirmation-observation --bundle B.json --out R.json` reconstructs
one recorded root/sysparam/KGSL/battery sample and its separate baseline admission
from hash-bound raw files. Missing receipts or unknown/synthetic sources block.
It does not collect devices, grant an improvement verdict, or run a live heldout
driver. Every single-run report has `qualification.stage=experiment`,
`qualification.eligible=false`, `real_game_optimization_proven=false`, and
`promotion_allowed=false`.

Raw declarations and SHA bindings are not trusted acquisition attestation. The
positive contract fixtures deliberately declare hypothetical recorded-device
receipts so both executables can exercise their interface; they are not physical
measurements. The explicit synthetic-source counterexample remains BLOCKED.

## Checks completed on 2026-10-11

| Check | Result |
| --- | --- |
| `cargo test` in `src` | 542 passed: 521 unit, 14 new observation CLI, 7 qualification CLI |
| `cargo clippy --all-targets` | Exit 0; existing warnings remain, not strict-clean |
| Actual GOL `confirm freeze` -> 10 actual phonefarm CLI reports -> actual GOL `confirm judge` | 10 VALID_OBSERVATION; conditional judge PASS; both proof/promotion flags false |
| Hash-consistent report mutation setting restore gate false | GOL BLOCKED, exit 1 |
| Explicit synthetic source | phonefarm BLOCKED, exit 1 |
| Nontrivial float margin `51.248178375505404` | Typed frozen fingerprint roundtrip interoperates |
| Reviewer replay of shared-long-ENV baseline counterexample | BLOCKED after duration/whole-window independence fix |

The CLI regression tests also cover missing context, individual receipt failures,
same-boot uptime reversal, incomplete fixed/CPU snapshot domains, reused raw
evidence, unstable baseline power, oversize/shared ENV windows, malformed frozen
plans, immutable outputs, symbolic links/FIFOs, and zero external device/model
calls through PATH sentinels.

## Reproduce the cross-repository check

Build both binaries first. Use game_opt_loop PR29 revision `920357e` or a later
revision containing its measurement-window consumer. The checked integration
binary was built at `ca4c699`.

```sh
cd src
cargo test --offline
cargo build --offline
cd ..
python3 tasks/confirmation_observation_20261011/cross_repository_cli.py \
  --phonefarm src/target/debug/phonefarm \
  --game-opt-loop /path/to/game_opt_loop/target/debug/game_opt_loop \
  --out /tmp/phonefarm-confirmation-contract
```

The output directory must not already exist. The script archives exact bundles,
raw files, report bytes, command exits, binary SHA-256 values, and `summary.json`.
All generated device/host context is hypothetical. Only an archived raw trace
excerpt supplies the trace format, with timestamps shifted for the contract
exercise. No ADB/HDC/device/model/API invocation is performed.

The run's complete local evidence is retained in `cross_cli/`, with fresh test
logs in `cargo-test.log` and `clippy.log`. Raw fixtures and logs are intentionally
local; the committed script reconstructs the full interface exercise. The small
`verification.json` records the checked outcomes and binary fingerprints.

## Remaining acceptance work

A live heldout driver must collect all cold/root/boot, capture/sampler/workload
exit, restore, role, identity and timing receipts inside an exclusive device
lease. Existing legacy run directories lack that context and cannot be upgraded
by pasting boolean gates or an aggregate conclusion. Real hardware acquisition,
event-clock positioning, sustained control through the entire window, display
timing, shader/quality paths, and real-game improvement remain unverified.
