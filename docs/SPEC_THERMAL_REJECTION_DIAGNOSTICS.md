# Captured thermal rejection diagnostics

Base: `afd2db621f822d6419be3bd70c5471ba757817e4`. This is a producer-only
change in `confirmation_temperature::sampling_script`, independent of fixed
tool installation or transport batching. It addresses the real prepare
failure that returned exit82 with only header/UPTIME and no failing TEMP value.

## Invariants

- Read the existing fixed ordered temperature paths exactly once, into the
  existing `values` snapshot. Do not query again for diagnostic values.
- Keep successful stdout byte-for-byte equivalent: header, UPTIME, TEMP records,
  captured HAL block and `#end` retain their existing format and order.
- Keep every numeric grammar, selected count, unit scale, frozen lower/upper
  band, operating cap, sensor type, HAL and purpose predicate unchanged.
  The current device's25 sensors remain25; do not hard-code25 for other profiles.
- Preserve exit82 and fail closed before any candidate write. No invented
  missing temperature, resampled value, widened/reference reset, new baseline
  eligibility or implicit retry. Existing restoration barriers remain owned by
  the driver/transport, which this change does not modify.
- Old frozen scripts are not revalidated by a new implementation. The existing
  exact regenerated-script checks require a new session for these new bytes.

## Bounded stderr protocol

When the existing AWK numeric/band/count gate rejects, emit
`#phonefarm_thermal_rejection v1` to stderr before exiting. The existing
transport already durably saves the command's stderr. No new device file,
side effect, path supplied by a caller, secret or private-auth read is added.

The receipt includes:

1. Expected and actual AWK record counts and fixed-order/index mapping scope.
2. For each expected index (at most128): expected path, declared unit, scale,
   effective frozen lower/upper°C, absolute operating cap°C, and whether the
   bounds came from `reference_band` or `absolute_cap`.
   Numeric diagnostics use17 significant digits of the effective AWK number.
   The gate's existing10-decimal generated literals are unchanged; the receipt
   does not silently round a boundary more aggressively or claim the original
   reference JSON had different precision.
3. The same observed raw record prefix, original record length, truncation and
   replacement flags, normalized°C only for the existing accepted numeric
   grammar, and the specific failed key (`raw_format_invalid`,
   `value_below_low`, `value_above_high`, or `missing_record`).
4. A global `value_count_mismatch` record when counts differ. Extra records are
   counted but are not attributed to a nonexistent sensor.
   Per-index `FAIL` summaries precede the full table so bounded stderr previews
   expose the failure key. Their paths remain expected ordinal identities.

Raw prefixes are at most64 characters, escaped for backslash/tab/CR; other
non-printable/non-ASCII characters are replaced with `?` and flagged. This is
an explicitly bounded diagnostic prefix, not a claim that the entire malformed
file was preserved. Output is ASCII and has a hard64KiB emitter budget. A
truncated diagnostic lacks its normal completion marker. Successful AWK emits
nothing to stderr.

Paths are the frozen expected index paths. A count mismatch can shift ordinal
mapping, so its receipt says `ordinal_only_count_mismatch`; it must not assert
that a particular physical sensor supplied or omitted that record. An invalid
raw token gets no invented normalized temperature. Diagnostics are negative
evidence and never substitute for a valid sample or complete thermal window.

This targeted extension covers the numeric/count/band AWK rejection (exit82).
Existing cat read errors, sensor type exit84 and HAL exit83 retain their own
exit/stdout/stderr behavior; it does not pretend all failure families are now
fully diagnosed. HAL bytes remain captured once by the existing code.

## Validation

Execute the actual generated production script using a host `/bin/sh` and
fixed owned stubs for only cat, cut, timeout and dumpsys. This validates shell
control flow, same-read diagnostics and rejection before a candidate marker;
it is not Android/GPU/device validation. Cover a25-sensor successful byte-exact
stdout/control, empty/missing record, NaN/nonfinite token, count mismatch,
frozen-band excursion, absolute cap, and bounded malformed raw prefix.
Host shell fixtures are isolated from caller environment, serialized, and have
a15-second launcher budget to cover cold macOS script starts. This test-only
budget changes no production sampling, transport or window deadline.

No model API or device calls. Focused tests can be shared with root before the
required complete Cargo/default-Clippy gate on the combined production source.
Refs https://github.com/HGamey/.github/issues/21. The existing public release005
and its old blank-stderr exit82 remain immutable/UNKNOWN; this change neither
edits the dashboard nor retroactively diagnoses that old failure.
