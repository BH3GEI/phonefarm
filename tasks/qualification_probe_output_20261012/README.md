# Qualification private output regression — 2026-10-12

Backport of the independently reviewed capability output fix from `5390db7` to the original qualification draft PR18; includes the later caller TMPDIR failure negative test from `767b3eb`. Original `e411436` acceptance remains in the validation document.

Each `inspect_device` reserves an atomic, distinct private scratch directory (Unix 0700), uses it for root/trace/measurement, and cleans only known output names plus an empty directory. Reservation failure returns unknown capability. Fake CLI helpers isolate their lease/devlock roots. No threshold, evidence rule, driver module or real-device operation added.

Actual full offline gate: 521 unit + 9 real-binary CLI tests. Concurrent fake root and nonroot output intentionally overlaps; nonroot cannot borrow the root result. Caller TMPDIR regular-file failure cannot invoke the bridge or authorize a probe. Default all-targets clippy completes with existing repository warnings and no changed Rust file diagnostics. Raw logs and byte digests retained.

These are synthetic host regression mechanisms; they do not authenticate a physical device or prove a game improvement. Device/model/API calls: 0.
