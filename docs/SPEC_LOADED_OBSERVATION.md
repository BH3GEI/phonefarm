# Loaded temperature observation contract

This extends confirmation to a versioned loaded temperature policy. Existing
conditions/run/observation version 1 and their cold receipts keep their original
meaning. Unknown versions and mixed contracts refuse; this does not promote
history, a control probe, installation, or host fixtures to device evidence.

## Frozen inputs

`confirmation_conditions@2` retains the version-1 identity, capture, CPU-domain
coverage and sampling contract fields, and requires `thermal_contract: FileRef`.
Policy version 2 has `temp_cap_c=null`, `cool_c=null`, and exactly the registered
`temperature_profile_id=loaded_steady_v1`, canonical `temperature_profile_sha256`
and exact reference-file `thermal_reference_sha256`. No caller numeric ceiling
or legacy cold flag can replace the raw producer contract. The pair/metric,
dispersion, drift and statistical rules remain receiver-owned.

The referenced `phonefarm/frozen_thermal_evidence@1` contains actual_serial,
boot_id, rules_frozen_at, warmup_started_at, warmup_finished_at, metadata_raw,
hal_raw, warmup_raw, profile, reference, profile_sha256, reference_sha256 and
clock_scope. Each raw/JSON artifact is a bounded relative regular FileRef with
exact SHA-256. Reference SHA identifies the exact reference-file bytes; profile
SHA is the pure thermal module's canonical serialized fingerprint. Consumers
resolve metadata and HAL again, parse the original warmup and recompute both
profile and reference. Stored JSON or caller booleans are not substitutes.

Host chronology must satisfy rules <= warmup start < warmup finish < conditions
freeze < arm start. Retained host warmup duration must cover the device warmup
span. The original warmup and frozen reference are deliberately shared by arms;
pre-arm readings and complete sampled windows are unique per arm.

The GOL Plan appends optional `temperature_contract` after metrics, with ordered
fields profile_id, receiver_policy_version, profile_sha256, reference_sha256 and
thermal_contract_sha256. None is skipped so old typed Plan fingerprints remain
byte-identical. Loaded plans require version 2 and exactly the binding computed
from conditions; legacy plans require None. The observation repeats the binding
under confirmation_binding; the GOL consumer checks it against frozen inputs.

## One arm

`run_one_bundle@2` adds raw thermal_admission, thermal_window and
thermal_sampler_exit FileRefs. Legacy cold is absent. The admission is one
complete typed snapshot, taken after the warmup and within 60 seconds before
ENV begins, before the complete thermal window, under the same frozen limits
and reference. Version 1 rejects loaded fields and still requires cold.

The window contains the frozen producer's complete raw snapshot blocks and one
terminal receipt with duration_s, interval_s, start_uptime_s, end_uptime_s and
integer samples. Only interval 1 is supported; requested duration is ENV DUR+3.
Count equals parsed snapshots (2..512), clock increases with gaps 0.5..6 seconds;
HAL call latency means ceil(DUR/1) is not an honest sample-count assumption.
Every selected sensor and HAL status is checked, with no missing/extra reading,
against the registered preset and pre-frozen reference. The observed sample
span is at least DUR-1; terminal start is no later than the first snapshot by
at most 1 second, terminal end no earlier than the last by at most 6 seconds,
and terminal span at most DUR+9 and no longer than the retained host arm.

First thermal uptime <= first ENV uptime and capture start; last thermal uptime
>= last ENV uptime and capture end. ENV must independently be complete and
cover capture. Sampler exit must be zero. These are observed sampled windows,
not a claim of continuous thermal safety between readings. Producer script,
live type readback, boot identity, child ownership and stop/restore mechanisms
remain independently frozen and required by the driver.

## Reconstructed output and limits

Outputs are `confirmation_observation@2` and
`confirmation_sample_qualification@2`. Qualification version/policy are 2;
derived thermal_profile_verified and thermal_window_verified replace
cold_verified. Root, control, trace, workload, restore and baseline-admission
gates remain required. No derived thermal gate is copied from input JSON.
Pre-arm snapshot, thermal window, trace and ENV hashes join exploration exclusion
and independent-arm raw uniqueness. Plain exit bytes `0\n` can repeat and are
bound/hash-checked without requiring unique contents.

Version-2 baseline qualification derives thermal_admission_verified from the
validated raw baselines and keeps cold_ok=None. It remains stage=experiment,
eligible=false. This does not change the power unit adapter: ambiguous current
units, charging, missing discharge coverage or absent frame metrics still block.
Native codegen, a rendering screenshot and thermal admission are not GPU-layout,
quality, displayed-frame, game optimization, promotion or source attestation.
Device acquisition and declared host chronology are checked for consistency,
not cryptographically attested by hashes.
