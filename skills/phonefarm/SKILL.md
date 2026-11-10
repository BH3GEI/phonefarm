---
name: phonefarm
description: Use phonefarm for parallel mobile / OpenHarmony device automation, testing and collection — device discovery, deterministic scripts, CTS/XTS batches, screenshots, UI trees, telemetry, frame capture, bounded scrolling collection, optional VLM UI traversal, local CLI/MCP tools, and Rust implementation work. Android (adb), OpenHarmony (hdc) and iOS (WDA) support varies by command and device. Optional graphics-performance tools (eval, gray layer, loop_v1) serve upper-layer research clients.
license: MIT
metadata:
  source-repo: github.com/BH3GEI/phonefarm
---

# phonefarm

phonefarm is a per-host device layer: it discovers devices, operates them, collects artifacts and runs tests, with a host-local device lock. Cross-host scheduling and board/device allocation belong to the caller (for example a test harness driving a farm of OpenHarmony boards; see `docs/D600_HARNESS.md`). Choose the smallest existing command that serves the requested task, using the current checkout's help and the target's reported capabilities. Android/ADB, OpenHarmony/HDC and iOS/WDA support is command-specific; a backend name does not promise every operation, metric or restoration path.

Agents, test harnesses and people call it directly. Upper-layer clients (test harnesses, research loops such as HGamey/game_opt_loop) are optional; this skill is usable for a single device operation or local result inspection on its own.

## Scope, cost and target

- Stay within the user's authorized devices, actions and budget. Existing authorization need not be requested again; skill availability or credentials do not authorize additional changes.
- Deterministic commands can avoid model calls while still installing, launching, writing settings, consuming device time or changing application state. “Zero tokens” is a cost property, not unconditional permission or a safety guarantee. `decide --local` differs from a configured model sidecar; VLM traversal and other model-backed paths require their own provider/budget configuration.
- Select the intended `--serial` for device work. Do not let a missing target silently turn a single-device task into farm-wide keep-alive or parallel work. Enumerate only as needed; device listing does not establish readiness or exclusive ownership.
- Local PF commands act only on devices attached to this host. A multi-host service in front of phonefarm is a separate client with its own identity and credentials; do not translate a missing remote capability into raw shell/ADB/HDC access.

## Ownership and recovery

Use the command's existing lease/transport path in the same configured lock domain. A queue label, shared task name or copied environment value is not lease ownership; inheritance requires the actual open lock fds (`pass_fds` for Python subprocesses). Do not add a second weak lock, change the storage domain, or bypass a live device lock with a raw transport command.

`phonefarm lease status --json` inspects host-side lease evidence. Interpret it with the operation's own status/report, not as proof that all application state was restored. Host process exit, a successful kill, TTL expiry or an empty queue alone does not prove device recovery. Reset stops remaining task processes and releases node reservations; restore device state through adb or the operation’s recovery command, preserving original context and receipts.

`screen` now obtains a short strong observation lease for each new frame. Cached-frame reads do not capture or hold a device lease. Busy, blocked or unknown ownership can prevent a fresh frame; the service must not fall back to raw screencap. The worker and finite transport proxies hold the fd until exit; no persistent recovery barrier is created. Screenshots also disturb CPU/GPU work, so do not treat live viewing as measurement-neutral.

Optional research paths — persistent gray-control sessions and (in builds with the `game` feature) game deployment/initialization/scene operations — have their own recorded lifecycle and recovery scope. A remote-control session closing settles its commands while deliberately retaining application changes; it is not a game restore. Likewise a successful recovery job can remain queue-blocked pending explicit release of its recovery chain. Follow the status and contract for the operation actually used.

## Selective command entrypoints

These examples describe shapes, not a sequence to run for every task. Use files and IDs from the current task; inspect help before choosing version-specific options.

```bash
# Local result inspection; no device operation.
./phonefarm last
./phonefarm show RUN_ID --step N
./phonefarm stats RUN_ID
./phonefarm cat PATH_TO_RESULT
./phonefarm schema
./phonefarm frame-audit analyze knobs_frameaudit.json --json audit.json

# Host capability/ownership reads.
./phonefarm lease status --json
./phonefarm screen capabilities --json

# Authorized device work with an explicit serial and supplied input.
./phonefarm script --serial "$SERIAL" --task "$TASK" --app "$PACKAGE" script.json
./phonefarm test-batch --profile profile.json --environment environment.json \
  --serial "$SERIAL" --out results/
./phonefarm eval --request eval_request.json --serial "$SERIAL" --out eval-out/ --json

# Observation capture into a new directory, or a viewer using per-frame leases.
./phonefarm screen capture --serial "$SERIAL" --out "$NEW_DIR" --json
./phonefarm screen --serial "$SERIAL" --interval-ms 1000
```

Other paths include `capture` for measurement capture, `collect` for bounded scrolling, `cts-fetch` for existing test results, `bench` for on-device model measurements, and optional VLM `run` / `benchmark` / `parallel` / `quest`. Their prerequisites and effects differ; do not infer a root requirement, rollback guarantee or platform support from another path. `keepalive` can wake/unlock and change sleep policy; use its explicit target/status options for the requested scope.

For an eval client, use the accepted request schema and the resulting report's qualification/recovery fields. A client's request schema (for example game_opt_loop's `contracts/eval_{request,report}.schema.json`) does not make every request kind executable on every target. For gray-control and game deployment, read the corresponding lifecycle contract before attempting a new operation or recovery.

## Evidence and result use

Read existing results before repeating device work: `last`, `show`, `stats`, or the specific report named by the request. Preserve run IDs, serials, source paths, receipts and unknown/null fields. A missing metric or incomplete recovery is not zero or PASS.

Shared telemetry fields and units aid comparison, but do not establish comparable workloads, time windows, clocks, sampling methods or device conditions. A diagnostic screenshot, scene launch or imported historical result is not automatically a qualified performance/quality result. Keep the report's stated scope and limitations.

For MCP, `phonefarm serve [--root DIR]` exposes the tool list of the installed binary. Inspect those tool schemas instead of assuming an old fixed tool count or that all tools are read-only. Detached operations need their returned IDs for status queries; a client timeout is not a reason to launch the same device action again.

## Setup and implementation work

Use the existing approved checkout/binary when available. Installation, rebuilding, model configuration and device setup are separate actions, not prerequisites to reading local results. Provider credentials are needed only for paths that use that provider; do not read or copy secrets merely to run a deterministic command. Never fabricate or commit credentials.

When a build is needed, follow repository `AGENTS.md`. In this checkout the Rust package is under `src/`; Apple Silicon release binaries require ad-hoc signing after the configured stripping/build step. Follow applicable repository instructions for focused checks, artifacts, commits and publication rather than imposing a new blanket test or push policy from this skill.

## References

Load only the reference needed for the task:

Local docs live under `docs/` in this repository. Links into HGamey/docs cover the optional research paths; use current local contracts/help for executable behavior.

- [Architecture](references/architecture.md) — core modules, records and the VLM step loop.
- [CLI reference](references/cli.md) — command background and examples. Some entries predate the current ownership model, including the old screen lock description; current help and the ownership contracts below take precedence.
- [Telemetry](references/telemetry.md) — collection mechanisms; verify availability and qualification on the actual target.
- [D600 / board-farm integration](https://github.com/BH3GEI/phonefarm/blob/main/docs/D600_HARNESS.md), [devices](https://github.com/BH3GEI/phonefarm/blob/main/docs/DEVICES.md).
- [Design](https://github.com/BH3GEI/phonefarm/blob/main/docs/DESIGN.md), [MCP](https://github.com/BH3GEI/phonefarm/blob/main/docs/SPEC_MCP_SERVE.md), [keep-alive](https://github.com/BH3GEI/phonefarm/blob/main/docs/SPEC_KEEPALIVE.md).
- [Observation ownership](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/OBSERVATION-OWNERSHIP.md), [gray-control lifecycle](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-CONTROL-SESSION.md), [gray-control quality](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GRAY-CONTROL-QUALITY.md).
- [Game initialization](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GAME-INITIALIZATION.md), [game scenes](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/GAME-SCENE.md), [job recovery chains](https://github.com/HGamey/docs/blob/main/plans/2026-10-22-agent-platform/SESSION-RECOVERY-CHAIN.md).
- [loop_v1](https://github.com/BH3GEI/phonefarm/blob/main/loop_v1/README.md) and [GPU optimization routes](https://github.com/BH3GEI/phonefarm/blob/main/docs/MOBILE_GPU_OPT_ROUTES.md) — useful measurement recipes. Their observations about unavailable SurfaceFlinger/gfxinfo/Perfetto paths apply to the recorded devices and versions, not all future targets. Preserve the chosen recipe's preregistered decision rule and restoration checks when using it.

For CTS-specific work, the separate `phonefarm-cts` skill has the verdict and reconciliation contract. None of these references requires routing every task through one experiment loop.
