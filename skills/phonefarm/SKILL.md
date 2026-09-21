---
name: phonefarm
description: Drive the phonefarm device automation and measurement infrastructure (Rust core, adb + hdc backends). Use when operating Android emulators or OpenHarmony devices through the phonefarm binary — VLM-driven UI traversal (run/benchmark/parallel/quest), zero-token deterministic scripts and replay (script), conformance-test batches and result extraction (test-batch/cts-fetch), on-device model latency benchmarking (bench), frame capture (capture), device farm keep-alive (keepalive/devices/probe), the hypothesis-experiment-evidence loop (hyp/pred/caps/tools/experiment/export), inspecting results via the read-only CLI (last/runs/show/cat/stats/schema), the MCP tool service (serve), or working on the Rust codebase. Covers both Android (adb) and OpenHarmony (hdc) backends, telemetry, and the house rules for spending model tokens and pushing changes.
license: MIT
metadata:
  version: 3.0
  source-repo: github.com/BH3GEI/phonefarm
---

# phonefarm

phonefarm is a **device automation and measurement infrastructure**: one Rust
kernel, two device backends (Android `adb` / OpenHarmony `hdc`), carrying
several independent upper paths on one shared foundation of device abstraction,
record contract, telemetry, and evidence grading.

A vision-language model drives **one** of those paths. It is not what the
project is. `script`, `test-batch`, `cts-fetch`, `bench`, `capture` and
`keepalive` spend zero tokens and work with no model at all.

Architecture: decision and execution are separated. Where a model is used at
all, it only does screen understanding and action planning; state capture,
validation, safety interception, post-run review, and performance monitoring
are all enforced by deterministic Rust code in the host process.

This skill is self-contained: everything an agent needs to install, build,
run, and inspect phonefarm is in this file and the `references/` folder.

## Capability map

| Path | Commands | Tokens |
| :--- | :--- | :--- |
| Device & farm ops | `devices` `keepalive` `probe` `exec` | none |
| VLM UI traversal | `run` `benchmark` `parallel` `quest` `plugins` | **burns tokens** |
| Deterministic script & replay | `script` | none |
| Conformance tests (CTS/XTS) | `test-batch` `cts-fetch` | none |
| On-device model benchmark | `bench` | none |
| Frame capture | `capture` | none |
| Performance optimization loop | `loop_v1/` toolchain (not a subcommand) | none |
| Hypothesis-experiment-evidence | `hyp` `pred` `caps` `tools` `experiment` `export` `eval` | some |
| Read-only inspection | `last` `runs` `show` `status` `stats` `cat` `tasks` `tree` `lessons` `campaign` `schema` `config` | none |
| MCP tool service | `serve` | none |

## When to use this skill

- Running any of the paths above against a device
- Inspecting results (`last` / `runs` / `show` / `cat` / `stats`) or reading the
  ledger schema (`schema`)
- Modifying the Rust kernel (`runtime` / `device` / `cli` / `telemetry` / `cts`)
  or adding new capabilities

## Setup from scratch

Prerequisites:

- Rust toolchain (`cargo`) for building the kernel
- Android: `adb` — auto-detected in this order: `ADB_BIN` env var →
  `platform-tools/` in the repo root → `PATH` → common system SDK locations
- OpenHarmony: `hdc` available on `PATH` (physical device, remote host OK — see
  "Devices" below)
- Optional: `swiftc` (macOS) for the OCR fallback helper (`ocr.swift`), compiled
  automatically on first use; if compilation fails the OCR channel simply stays
  off and the main loop is unaffected
- A GLM API key (Zhipu coding plan) for model decisions

Steps:

```bash
# 1. Clone and enter the repo
git clone git@github.com:BH3GEI/phonefarm.git && cd phonefarm

# 2. Configure secrets
cp secrets.env.example secrets.env   # fill in GLM_KEY
# The program auto-loads ./secrets.env on startup. If a required key is
# missing it prints setup instructions and exits safely. Never invent a key
# for it, and never commit secrets.env (already git-ignored).

# 3. Build
cd src && cargo build --release && cp target/release/phonefarm .. && cd ..

# 4. Verify the environment (read-only, costs nothing)
./phonefarm devices        # list connected adb and hdc devices
./phonefarm last           # should print latest run info or an empty-state hint
```

## Devices

- **Android emulator**: AVD named `agentphone`, must be started beforehand.
- **OpenHarmony physical phone**: reachable over `hdc`; on the intel-mac host
  (via ssh) — sync the repo there and rebuild before running.
- Switch targets with `--serial`, e.g. `--serial emulator-5554` or
  `--serial hdc:<connect key>`.
- Farm-level keep-alive (`docs/SPEC_KEEPALIVE.md`): `phonefarm keepalive`
  wakes + unlocks every connected device and enforces the never-sleep
  policy (idempotent, re-runnable); `--status` is the read-only report,
  `--watch [sec]` (default 300s) is the resident watchdog that
  re-enumerates devices every cycle. Session-level lifecycle in
  `quest.rs` is separate (it relocks on exit to save battery) — do not
  run both against the same device at the same time.

## Essential commands

```bash
# Single traversal session (Android)
./phonefarm run --task news-traversal --endless --budget-calls 90 --app com.ss.android.article.news "<goal text>"

# Multi-round evaluation
./phonefarm benchmark --task news-traversal --rounds 10 --budget-calls 90 --app com.ss.android.article.news --json "<goal text>"

# OpenHarmony session
./phonefarm run --serial hdc:<connect key> --task oh-settings-smoke --budget-calls 30 "<goal>"

# Multi-device parallel (one independent session per device, stdout lines
# prefixed with [device]; any failure makes the overall exit code non-zero)
./phonefarm parallel --job "taskA|goalA|emulator-5554|com.pkg" --job "taskB|goalB|hdc:<key>" --budget-calls 60
# The same task name on multiple devices is refused (lessons/tree state would
# collide) — use distinct task names.

# Deterministic script execution or historical run replay (zero model tokens, full telemetry)
./phonefarm script --task game-bench --app com.pkg --repeat 10 script.json
./phonefarm script --task replay <run-id>

# Conformance test batches (zero tokens; Android and OpenHarmony both supported)
./phonefarm test-batch --profile P.json --environment E.json --serial <S> --out results/
./phonefarm test-batch --module android.content.cts/androidx.test.runner.AndroidJUnitRunner
./phonefarm test-batch --module oh:com.example.demo/entry_test/OpenHarmonyTestRunner --serial hdc:<key>
./phonefarm test-batch --dir /path/to/apks --install-cmd 'pm install -r {apk}'
# Produces summary.json (per-case verdict + full assertion stack) and JUnit XML.
# Failing cases carry the complete stack — no need to shell into the device for logs.

# Pull results another tool already left on the device, and scan them for assertions
./phonefarm cts-fetch --remote /data/local/tmp/cts_result --serial hdc:<key> --out fetched/

# On-device model latency ruler (needs root) and frame capture
./phonefarm bench --serial <S> --model m.tflite --runs 3 --json   # exit 0/1/2 = PASS/FAIL/ERROR
./phonefarm bench --serial <S> --unlock                           # roll back a stuck freq lock
./phonefarm capture --serial <S> --out dir --frames 200 --json

# Inspecting results (read-only, offline, zero model tokens)
./phonefarm last                                  # latest session verdict
./phonefarm show <run-id> --step N                # drill into one step
./phonefarm show <run-id> --raw|--hooks|--events  # model raw reply / system verdicts / event stream
./phonefarm stats <run-id>                        # telemetry summary
./phonefarm cat <path>                            # universal printer (.gz auto-decompress, .jsonl pretty, image info)
./phonefarm schema                                # log.jsonl field contract (generated from code, always current)
```

The full CLI surface is in `references/cli.md`.

## House rules (must follow)

- **Only the VLM path burns tokens (real money)**: `run`, `benchmark`,
  `parallel`, and the `[evolution]` quota. Quote the cost and get the user's
  consent before long or physical-device sessions. Everything else —
  `script`, `test-batch`, `cts-fetch`, `bench`, `capture`, `keepalive`, all
  read-only inspection, unit tests and builds — is free and safe to run.
- Keys come from `./secrets.env` automatically. Never commit it; never
  fabricate keys.
- Data only lives under `tasks/<task>/`: `log.jsonl` is append-only,
  `lessons.jsonl` is written atomically; screenshots and raw XML trees are
  never committed to git.
- After code changes run `cd src && cargo test` and keep everything green.
- Bug fixes: new defects are numbered sequentially (check the latest number in
  the codebase/docs first); fixes must be general, not special-cased; verify
  with a regression assertion session afterwards.
- **Pushing to git belongs to the user.** Never push unless the user
  explicitly asks. Deploy with atomic `mv` replacement.
- Report in plain language, no jargon. Do not write battle scores into the
  README.
- New capabilities: write a SPEC first, implement second. The architecture
  contract lives in `docs/DESIGN.md`.

## Workflow hints

- Read results top-down: `phonefarm last` → `show <run-id> --step N` →
  `cat .../stepN.xml.gz`. Run IDs accept prefix matching (ambiguous prefixes
  list candidates).
- In the VLM path each step has six phases: capture → context assembly → model
  decision → three deterministic gates → execution → acceptance diff. See
  `references/architecture.md`.
- All inspection commands are offline local parsing and consume no API quota.
- Telemetry is shared across every path, so a script run, a CTS run and a VLM
  run are directly comparable — same fields, same units.
- Verdicts are never guessed. Anything the harness could not account for is
  labelled as such (`NOT_RUN` / `ENV_BLOCKED` / left empty), never silently
  zeroed and never counted as a pass.
- MCP hosts (e.g. octos): `phonefarm serve [--root <dir>]` exposes 21
  newline-delimited JSON-RPC tools (`phonefarm_*`): 17 read-only + 4 that touch
  the device. `run` / `benchmark` / `script` are always detached — poll with
  `phonefarm_status`; `cat` is jailed to the tasks root; raw-shell probe/exec
  are not exposed. Spec: `docs/SPEC_MCP_SERVE.md`.
- For conformance-test work specifically, the `phonefarm-cts` skill has the
  full contract (verdict enum, reconciliation rules, both wire protocols).

## Detailed references

- `references/architecture.md` — architecture, directory layout, data contract, six-step loop
- `references/cli.md` — every CLI subcommand with flags
- `references/telemetry.md` — the ten telemetry layers and collection mechanics

In the repo checkout (not bundled with this skill):

- `loop_v1/README.md` — the performance optimization loop. Read it **before**
  attempting any frame-timing work: `SurfaceFlinger --latency`, `gfxinfo` and
  the Perfetto GPU producer have all been ruled out on real hardware there, and
  raw ftrace kgsl is what is actually used. Its three disciplines are binding:
  the decision rule is frozen to disk before candidate data is seen; every
  sysfs write is restored on exit and the device snapshot must match line for
  line; parsing is pure functions so re-running on the same trace is
  byte-identical.
- `docs/MOBILE_GPU_OPT_ROUTES.md` — candidate optimization routes, each tagged
  with how it would be verified inside that loop
