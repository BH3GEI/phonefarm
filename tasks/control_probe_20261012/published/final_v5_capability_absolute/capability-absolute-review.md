# Capability absolute preset: independent initial isolation review

Status: **source/SPEC review only, not CLI/full/Android acceptance**. Reviewed the initial stable B implementation shared after the new capability SPEC. Source hashes are in `capability-absolute-review.json`; B has not supplied a new committed/full-gate identity for this patch. Existing 796/671 and the three real failure records remain their original scope.

C performed no Cargo, shell execution, device, SDK, model/API or source edit. `private-auth` was not inspected, read, indexed or hashed. No inflight USB-recovery tree was read.

## Purpose and preserved predicates

`capability_absolute_v1` is explicitly control-only; it can support configuration readback for one declared sysparam endpoint, sampled 11-second hold, full restore and permanent session closure. It cannot support comparable game arms or performance admission. Separating range/reference comparability from this purpose is reasonable.

The pure resolver uses the same loaded CPU/GPU/skin/battery metadata, units, passive/HAL minimum-derived caps and battery 45C; no cap increase was introduced. It preserves HAL 0–2, exact sensor set, finite normalized values, >=30 seconds / >=6 complete admission points, monotonic uptime and gaps <=6 seconds. Capability diagnostic schema is `phonefarm/thermal_capability_diagnostics@1`; loaded reference freeze and reference evaluation reject that ID. Range/mean bands remain diagnostics; the loaded 3C and +/-2C rules and serialized legacy semantics remain unchanged.

Producer warmup/profile/raw evidence precedes the complete initial control snapshot and restore graph. Dedicated freeze/load/snapshot wrappers recompute the profile and diagnostics from the original raw data and require the fixed purpose marker. Worker samples immediately before apply and after the actual post-apply start at +1/+6/+11; receiver requires exactly four complete readings, correct order/coverage and the explicit hold completion. Lost/invalid thermal output does not yield a successful report.

Existing installation ownership barrier, current serial/boot checks, complete four snapshots, initial/restored byte equality, remote-owned-group quiescence, session tombstone, and host recovery retirement remain in the path. No new auto-retry or release-on-unknown path was introduced by the reviewed patch. A normal source review does not attest Android timing or successful write/restore.

## PF performance isolation found in current source

| Entry | Current rejection boundary |
|---|---|
| confirmation-driver performance prepare/collect | Request parse accepts only `legacy_cold_v1` or `loaded_steady_v1`; collect reconstructs that parsed contract |
| qualification policy | v1 requires absent temperature fields; v2 requires exact loaded ID and valid frozen hashes |
| confirmation observation | policy validation plus `reconstruct_raw` fixed loaded resolver/reference reconstruction rejects a capability diagnostic |
| loaded producer load/snapshot | `load_frozen` expects loaded rules/profile, `loaded_snapshot` requires exact loaded ID |
| worker performance modes | Present capability purpose marker allows only `control_probe` and `reconcile_controls`, before acquiring master |
| direct capability report | Own control-only schema/status, eligible/experiment/game-proof/promotion false; no baseline/plan/statistical samples |

The GOL loaded contract validator also requires exact loaded ID. Direct capability reports fail its observation schema/status gates.

## Concrete GOL legacy-envelope contradiction to fix

`game-loop-loaded-confirmation/src/confirmation.rs::receiver_qualified`, `thermal=None` branch, currently checks @1 observation/sample schema, cold gate and absence of a thermal contract. It does **not** validate qualification/policy version or reject explicitly supplied capability profile fields. Thus a legacy-shaped report with all existing gates but `qualification.policy.temperature_profile_id=capability_absolute_v1` can meet this predicate despite the purpose contradiction.

This is a **static receiver counterexample**, not an actual PF-emitted or device-observed report, and no CLI was run. PF's real observer rejects that policy. Nevertheless the explicit cross-repository purpose isolation requirement should reject the detectable contradiction at the GOL receiver too. Require exact legacy version/absent thermal fields in that branch, or globally reject the capability purpose before either branch; add a held-out report with mixed purpose fields and rehashed outer artifact to verify rejection. Root and A have received the locations.

## Acceptance still required

No independent PF source blocker was found in this narrow initial patch. This does not certify the pending CLI/full gate or real device operation. Required countercases should exercise cap-profile request rejection by performance prepare/collect, policy/observer, GOL freeze/judge; missing/altered purpose marker; hot/HAL-severe/invalid-unit windows; truncated/under-11-second hold; and unknown restore/closure preserving the barrier. Old loaded/cold thresholds and three historical prewrite failures must not be reclassified by the new preset.
