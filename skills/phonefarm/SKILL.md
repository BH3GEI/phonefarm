---
name: phonefarm
description: Drive the phonefarm mobile-device automation harness (Rust core + vision-language model). Use when operating Android emulators or OpenHarmony devices through the phonefarm binary — running or benchmarking automated UI-traversal sessions (run/benchmark/parallel), inspecting run results via the read-only CLI (last/runs/show/cat/stats/schema/probe), or working on the Rust codebase (src/runtime.rs, src/device.rs, src/cli.rs, src/telemetry.rs). Covers both Android (adb) and OpenHarmony (hdc) backends, telemetry collection, lesson/tree state, and the house rules for spending model tokens and pushing changes.
license: MIT
metadata:
  version: 2.0
  source-repo: github.com/BH3GEI/phonefarm
---

# phonefarm

phonefarm is an agentic mobile-device automation, testing, and telemetry harness.
A Rust kernel drives devices (Android emulators / OpenHarmony physical phones)
while a vision-language model looks at the screen and decides the next action.
It performs deep automated traversal of mobile apps and produces fully
replayable run records.

Architecture: the decision loop and the execution loop are separated. The model
only does screen understanding and action planning; state capture, validation,
safety interception, post-run review, and performance monitoring are all
enforced by deterministic Rust code in the host process.

This skill is self-contained: everything an agent needs to install, build,
run, and inspect phonefarm is in this file and the `references/` folder.

## When to use this skill

- Running a single session (`run`), a multi-round evaluation (`benchmark`), or
  multi-device parallel runs (`parallel`)
- Inspecting results (`last` / `runs` / `show` / `cat` / `stats`) or reading the
  ledger schema (`schema`)
- Modifying the Rust kernel (`runtime` / `device` / `cli` / `telemetry`) or
  adding new capabilities

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

- **Running sessions burns GLM tokens (real money).** Quote the cost and get
  the user's consent before long or physical-device sessions. Unit tests,
  offline CLI queries, and builds are always free to run.
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
- Each step has six phases: capture → context assembly → model decision →
  three deterministic gates → execution → acceptance diff. See
  `references/architecture.md`.
- All inspection commands are offline local parsing and consume no API quota.
- MCP hosts (e.g. octos): `phonefarm serve [--root <dir>]` exposes the same
  CLI surface as newline-delimited JSON-RPC tools (`phonefarm_*`). run and
  benchmark are always detached; `cat` is jailed to the tasks root; raw-shell
  probe/exec are not exposed. Spec: `docs/SPEC_MCP_SERVE.md`.

## Detailed references

- `references/architecture.md` — architecture, directory layout, data contract, six-step loop
- `references/cli.md` — every CLI subcommand with flags
- `references/telemetry.md` — the ten telemetry layers and collection mechanics
