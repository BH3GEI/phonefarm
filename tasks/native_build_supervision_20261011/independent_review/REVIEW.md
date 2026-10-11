# Independent native build supervision review

Read-only review of `phonefarm-build-supervision`; no modifications to its files or target. Controlled tools and receipts are in this directory. No real SDK/device/model/GPU calls.

## Publication blocker reproduced

`src/grayartifact.rs:363-373` prints an inherited-mode UNKNOWN diagnostic with `eprintln!` **before** sending SIGKILL to the controlled group. A valid outer supervisor can give the launcher a full, undrained stderr pipe. The diagnostic blocks, preventing timeout termination.

Actual test: a controlled glslc --version sleeps 30 seconds; inherited-group launcher leads an isolated group with explicit fake NDK; tool limit is 1 second. The stderr pipe was prefilled with 65536 bytes. At 3 seconds the launcher and group were still alive. The independent outer process then killed the group and reaped the launcher with SIGKILL. Exact args/binary SHA/results are in blocked-stderr-result.json; stdout remained empty. No successful artifact was accepted.

Suggested fix: inherited failure termination cannot depend on synchronous diagnostic output. Kill the owned group immediately and let the outer supervisor record the signal/failure, or emit only a genuinely nonblocking best-effort diagnostic. Add the full-pipe actual CLI watchdog regression. A timeout receipt from the outer supervisor is sufficient; JSON cannot be guaranteed after intentionally terminating the launcher.

## Other reviewed boundaries

Explicit inherited mode is accepted only for build with explicit NDK and current group leader. Helper calls inherit the group; standalone helper groups remain default. Descriptor freezes supervision and state; verify always recomputes requires_outer_quiescence and returns outer_quiescence_verified=false; default field omission preserves historical descriptor identity. Group absence is deliberately left to the outer supervisor. In-root artifact integrity and source association retain previous limited scope.

Actual additional negative CLI checks: inspect/adapt-storage reject supervision argument, missing explicit NDK rejects before staging, and a launcher sharing reviewer group rejects before tool execution. Full outputs are in mode-negative-results.json.

Current independent conclusion: failure-log blocking must be fixed before publishing this bounded termination interface. Static review found no separate supervision/qualification upgrade bypass. Real Android build, actual GOL producer integration, intentionally detached descendants and GPU semantics remain NOT_RUN/out of scope.

## Fix recheck — publication blocker resolved

The owner removed all logging/I/O before group termination: terminate_build_group directly sends SIGKILL. The actual latest CLI binary SHA is `db1721a7779741367f974b27d57e398b1392cd08fb4c16972e95015a5fe65005`. Re-running the original full 65536-byte undrained stderr-pipe counterexample with the same one-second hanging version probe exited by SIGKILL in **1.027 seconds** without an outer kill. The controlled group was positively absent after reaping. Pipe bytes remained exactly the prefill, confirming there was no attempted pre-kill log write. `fixed-stderr-result.json` preserves the new receipt; `blocked-stderr-result.json` preserves the original failure.

Source inspection confirms the same immediate kill helper handles inherited timeout/overflow/uncertain wait; the added actual CLI regression uses a full unread pipe, bounded outer watchdog and explicit SIGKILL assertion. No remaining publication blocker found in the reviewed supervision interface. This is offline process-control acceptance only; real NDK/GOL wrapper construction and device/GPU/source-authenticity acceptance remain unverified.
