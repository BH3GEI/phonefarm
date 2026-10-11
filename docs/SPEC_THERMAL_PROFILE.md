# Receiver thermal profiles

This replaces the assumption that every loaded SoC must cool below 40°C. It
does not change vendor thermal protection, trips, fans or clock throttling.
No candidate performance has been measured under this contract. Select the
receiver preset before metadata/warmup, freeze the resolved profile and raw
producer identities before baseline/candidate data, and retain old policy IDs.

`temperature_profile_id` selects `legacy_cold_v1` (default) or
`loaded_steady_v1`; unknown IDs refuse. Callers cannot supply numeric limits.
The pure module accepts typed sensor metadata, not device/model heuristics.
Metadata records path, actual type, declared temperature unit, domain and raw
trip points. CPU accepts cpu-/cpullc types, GPU gpuss/gpu types, battery the
battery type, and skin explicit skin/shell types. Domain/type mismatches refuse.
Unclassified non-temperature sensors, including socd, remain excluded diagnostic
metadata; their values are never normalized to zero or mixed into SoC maxima.
Units and type classification require receiver producer evidence. A declared
unit is not independently attested by this pure function.

Legacy selects CPU/GPU only, admission <40°C and operation <=70°C. It does not
retroactively impose battery, skin or Android HAL evidence on old results.
Loaded selects CPU/GPU/skin with valid passive trips and battery independently:
CPU/GPU ceiling = min(lowest passive trip, matched HAL severe threshold) minus
5°C; skin uses the same minimum minus 3°C; battery receiver ceiling =45°C.
Each selected CPU/GPU/skin sensor requires both a valid passive trip and the
receiver's typed `hal_severe` trip copied from the actual domain HAL receipt.
HAL Celsius values must be explicitly converted to that sensor's declared unit;
the producer freezes the mapping and original receipt. A missing/mismatched HAL
threshold refuses. These are configuration-derived
receiver limits, not declarations of physically safe junction temperatures.
Every loaded domain needs at least one sensor. A skin diagnostic without trips
may be explicitly excluded; any classified CPU/GPU sensor without a valid
passive threshold refuses even if a sibling supplies one. Missing the whole
domain refuses. Selected unknown
units, incoherent trips, unknown/changed sensor identity or malformed readings
refuse. Metadata, exclusions and all numerical receiver choices are frozen.

Loaded readings require Android thermal status 0–2; unknown or >=3 blocks.
Profiles are validated by recomputing their registered preset from retained
metadata, so editing serialized limits does not create an accepted policy.

Admission for loaded operation requires a warmup window >=30 seconds with >=6
complete samples, increasing uptime, gaps <=6 seconds and per-sensor range
<=3°C. All warmup readings must meet typed limits. Freeze mean +/-2°C reference
bands from that window, clipped to the operating ceiling. The reference retains
the original warmup and profile SHA; consumers recompute it exactly. Candidate
data cannot construct or widen the reference. Host freezing and producer/boot
identity must additionally bind the reference; these pure functions do not
establish that the supplied warmup preceded a candidate.

Operating-window checks need >=2 complete readings with strictly increasing
uptime, gaps <=6s, operating ceilings and the same frozen reference. The 30s
warmup requirement is not imposed on short capture/control windows. Readings
must contain exactly the selected sensor paths. Whole-window capture coverage,
DUR/IVL/n completeness, actual source, boot/clock identity and remote ownership
are independently required by the producer/observation validator. This module
assesses only supplied temperature readings, never performance eligibility.

Bounded data: <=128 metadata sensors, <=16 trips each, <=512 readings/window;
all values finite and normalized temperatures in [-50,150]°C. A point check is
not stable admission, continuous thermal safety, or proof of identical load.
Installing/downloading a game is not a rendering baseline. Versioned typed ENV,
new profile/receiver policy hashes and full restoration receipts must be consumed
before a loaded profile can qualify an experiment. Probe-only results retain
eligible/proof/promotion=false. Legacy ENV is not silently upgraded.

## Control capability purpose

The pure resolver additionally recognizes `capability_absolute_v1` for the
control-only producer. Its selected sensors, units, cap derivation, HAL and
window integrity rules equal the typed loaded preset. Its >=30s/≥6 diagnostic
admission does not enforce the loaded <=3C range or mean +/-2C reference band.
`freeze_capability_diagnostics` emits
`phonefarm/thermal_capability_diagnostics@1`; `freeze_reference_band` and
`evaluate_against_reference` explicitly reject this ID. Diagnostic ranges/bands
are not stable-reference evidence. Endpoint allowlists must independently
reject it for performance admission and observations. All loaded/cold serialized
profiles, numerical rules and reference contracts retain their existing meaning.
