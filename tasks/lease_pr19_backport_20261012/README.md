# PR19 lease lifecycle backport: host evidence

Original draft source: `d03af6c`. Backport production/test source: `28a4868`.
This updates the original PR19 branch; it imports the generic lease lifecycle
and no confirmation-driver module. Existing v2 shader, eval and other CLI routes
are unchanged. The direct `libc` dependency supports Unix group checks.

## Verification

- Full cargo test: **533 passed** (518 unit, 3 new host lease CLI, 12 v2 CLI).
- Default Clippy: exit 0, existing warnings retained; not strict clean.
- The actual host wrapper starts a Python leader with an owned `sleep` grandchild;
  leader exit returns 2, preserves the PID/PGID marker and rejects both manual
  recovery and the next wrapper while that group remains live.
- Nine true/malformed/newer-schema barriers reject both admission and manual idle
  recovery without changing marker bytes. Old idle format-1 without extension
  fields remains compatible, with `device_verified=false` operator assertion.
- ENOENT and EACCES spawn errors retire only this owner's unspawned pending
  operation and allow a subsequent lease. Bound/unknown children and recovery
  requirements preserve uncertainty.

Commands (repository root, no device/API):

```sh
PHONEFARM_BACKPORT_FIXTURE_ROOT=/absolute/path/new-host-fixtures \
cargo test --offline --manifest-path src/Cargo.toml
cargo clippy --offline --manifest-path src/Cargo.toml --all-targets
```

`verification_summary.json` binds source, binary, logs and actual host CLI
receipts. Full logs and fixture directories stay local and are not committed.
The first targeted compile failed because the old Cargo manifest lacked a direct
libc dependency; that historical log is preserved separately from final tests.

## Compatibility and limits

Older PID-only wrapper markers do not identify an independent group; they are
UNKNOWN and cannot be cleared by guessing PGID or asserting device idle.
Unknown top-level marker fields/formats require a newer consumer. Typed pending
or recovery barriers cannot be removed by the manual recovery command. This PR
does not supply confirmation-driver reconciliation; the owning driver must
verify its raw restoration/quiescence evidence separately.

The tests execute owned Mac processes and explicit fake serials, not Android or
Harmony. No ADB/HDC/device commands, SDK download, model API or training ran.
Remote process quiescence, real root restoration and performance qualification
remain NOT_RUN. Cooperative host leases cannot protect against commands from
another host or a workload deliberately escaping the recorded process group.
