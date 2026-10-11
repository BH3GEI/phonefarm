# Quality + qualification + v2 conditions/lease integration

2026-10-11. Local assembly only; no main merge, device operation or model/API call.

## Component relationship

| Component | Source | Integration commit |
|---|---|---|
| Paired sequence quality | Independent quality branch | `67dbf62` |
| Experiment/conclusion qualification, PR #18 | Qualification branch | `3a52568` |
| Shader v2 conditions + automatic lease, PR #19 | `bb53299` | `2424cd0` |
| Actual GOL fan/Inject contract correction, PR #19 | `db0b6a8` | `638ff01` |

The eval conflict was resolved by retaining the complete `--qualify-from` early return
before shader plan execution and device resolution/lease acquisition. The sequence-quality,
qualify and lease CLI routes remain registered. Qualification metric provenance, power window,
workload exit, root-before-write, cold/baseline/control and restoration gates remain in place.

The PR #18 subprocess tests now isolate both automatic and legacy lock directories. One added
integration regression uses the actual serialized GOL shader envelope, an unsupported live
hook and an existing foreign lock. Offline replay returns an ABORT report without ADB/HDC,
lease creation or artifact application; the same live request rejects unsupported conditions
before any lease/device operation.

## Verification

`cargo test --manifest-path src/Cargo.toml`: **561 passed**, zero failures:
531 unit + 8 qualification CLI + 11 sequence-quality CLI + 11 v2/lease CLI tests.
Tests include actual subprocess mutual exclusion across working directories, controlled-error
release, SIGKILL parent with live child, abandoned blocking and unverified manual-lock preservation.

Six additionally saved CLI boundary cases were consumed by the actual GOL public report parser
and `Report::kept()` / `is_measurement_valid()`: all remained blocked. Cases cover native SR with
missing artifact, unsupported layer/fan, synthetic ineligible offline evidence, shader offline
replay before support/lease gates, and eval blocked by a genuine abandoned host wrapper.
The typed request fixture was generated using GOL's public `Request::Serialize`, including
the mandatory fan string and object-form Inject. Fake adb/hdc sentinels recorded **zero calls**.
`sequence-quality --help` reached its own CLI parser. `git diff --check` passed.

Detailed logs, requests, reports, Rust GOL contract driver, repeatable boundary runner and
consumer outcomes are in `tasks/system_handoff_20261011/integration_phone_next` of the primary
phonefarm checkout. They are local artifacts, not device or performance evidence.

## Limits

Native injection here means refbench's SR postfx consumer only; Vulkan-layer shader injection,
headless/game/frame_gen injection and non-unknown fan control remain explicit ABORTs. Unknown
fan observations remain null. No report in this boundary check constitutes a game optimization
PASS; an offline eligible logic replay still has status ABORT and qualification_only=true.
Whole-window power, real display/present, quality acquisition identity and true game deployment
remain unverified. No component PR or combined worktree was merged into main by this task.
