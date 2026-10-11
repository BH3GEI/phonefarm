# Capability purpose isolation: committed delta review

Reviewed B committed source `19bfed5a2907c1770cf62656fbf9ded4d733a658` after the earlier initial SPEC/source review.
This is a narrow read-only delta, not a PF CLI/full or Android acceptance.

- Performance collect explicitly rejects capability profile before prepared loading, lease acquisition or capture.
- Actual worker-purpose guard extraction covers sample/probe/reconcile rejection and only control_probe/reconcile_controls acceptance.
- Qualification tests reject capability ID under both explicit loaded and legacy policies; production policy was not widened.
- Dedicated capability diagnostic schema/purpose, typed absolute caps/HAL/admission cadence, 11-second completion and full restoration ownership remain as previously reviewed. Loaded reference/span thresholds and old failures were not relabeled.

No new source blocker in this delta. C ran no PF Cargo/device/API/shell body. B's new full gate remains pending at review time. GOL legacy None contradiction is separately repaired in `0dd011e`, independent 364/strict gate, not PF or root integrated acceptance.
