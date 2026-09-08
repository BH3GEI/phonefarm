---
name: phonefarm-cts
description: Drive the phonefarm CTS automation test harness for A2OH (Android-on-OpenHarmony bridge) environments. Use when running CTS instrumentation batches via `phonefarm test-batch` — starting/stopping batch runs, querying batch status, exporting JUnit/summary reports, resuming interrupted batches, or interpreting the PASS / ASSERTION_FAIL / ENV_BLOCKED / TIMEOUT / NOT_RUN verdicts. Aligns with the A2OH team's CTS_FAST_TESTING.md six-step flow and consumes the same environment.json / per-API profile files.
license: MIT
metadata:
  version: 1.0
  source-repo: github.com/BH3GEI/phonefarm
---

# phonefarm-cts

Batch CTS execution for the A2OH bridge: `phonefarm test-batch` is the挂机
(long-running unattended) executor that sits **on top of** the colleague's
single-API quick loop (CTS_FAST_TESTING.md). The colleague's tool answers
"did my adapter change break this API slice?"; this harness answers
"run these N modules overnight, survive deadlocks, and give me per-case
evidence bundles plus a standard report in the morning".

Both speak the same contract:

- **Granularity**: `Class#method` slices via `-e class com.x.C#m`, never
  blind whole-suite runs.
- **Verdicts** (literal strings, do not paraphrase): `PASS`,
  `ASSERTION_FAIL`, `ENV_BLOCKED`, `TIMEOUT`, `NOT_RUN`. Skipped or
  zero-case runs NEVER count as PASS.
- **Recovery discipline**: on disconnect — stop device writes, leave a
  recovery record, re-verify `boot_id` after reconnect, never reuse old
  PIDs. A batch is not "complete" while recovery is `PENDING`.
- **Config**: consumes the colleague's `environment.json` and per-API
  profile JSON directly.

## When to use this skill

- Launching a CTS batch from an APK directory, explicit module list, or the
  colleague's per-API profile
- Checking progress / exporting reports of a running or finished batch
- Resuming an interrupted batch (`--resume`) or retrying flaky cases
  (`--retry`)
- Deciding whether a batch outcome is claimable (recovery VERIFIED, no
  ENV_BLOCKED/TIMEOUT) vs. diagnostic-only

## Commands

```bash
# 1. Batch from colleague's per-API profile (Class#method slices)
phonefarm test-batch --profile .work/cts-fast/profiles/getPackageInfo.json \
  --environment .work/cts-fast/environment.json --serial hdc:<key>

# 2. Whole APK directory (differential deploy: sha256 cache on device skips
#    unchanged pushes/installs — the 50s→2s overhead lever)
phonefarm test-batch --dir ./cts-apks/ --serial hdc:<key> \
  --include 'android\.(content|graphics)\..*' --exclude '.*\.perftest\..*' \
  --retry 1 --heal-script ./scripts/a2oh_bridge_restart.sh

# 3. Single module, ad-hoc
phonefarm test-batch --module android.content.cts/androidx.test.runner.AndroidJUnitRunner

# 4. Resume an interrupted batch (same --out dir)
phonefarm test-batch --dir ./cts-apks/ --resume --out cts-batch-20260908-101500
```

Flags:

| Flag | Meaning |
|---|---|
| `--profile` | Colleague's per-API JSON: package/runner/cases(Class#method list) |
| `--environment` | Colleague's environment.json (serial/runtime/abi/cts version) |
| `--dir` | Directory of test APKs; auto install + runner discovery |
| `--module` | Explicit `pkg/runner`, repeatable |
| `--include/--exclude` | Regex on module name `pkg/runner` |
| `--resume` | Skip modules marked done in `batch_state.json` |
| `--retry` | Per-case retries for ASSERTION_FAIL/TIMEOUT slices |
| `--timeout-ms` | Total watchdog per instrument run (default 600000) |
| `--idle-timeout-ms` | Silent-pipe watchdog (default 90000) |
| `--heal-script` | External A2OH bridge restart script, fired on crash/disconnect |
| `--install-cmd` | Device-side install template with `{apk}` placeholder (required for hdc/A2OH; the bridge owns its install entry, we don't invent it) |
| `--out` | Output directory (default `cts-batch-<ts>`) |

## Output contract

```
<out>/
  summary.json                 # batch totals, per-verdict counts, recovery VERIFIED|PENDING
  junit_<module>.xml           # standard JUnit per module (failures≠errors≠skipped)
  batch_state.json             # resume bookkeeping
  recovery_pending.json        # present ONLY if a recovery is still owed
  <module>_stdout.log          # full raw runner output per module
  artifacts/<module>/<case>/   # per failed case Crash Bundle:
    stdout.log                 #   case-window runner output
    hilog_slice.log            #   device log sliced to case window ±5s
    telemetry.json             #   before/after mem/FD/thread deltas
    meta.json                  #   verdict, timing, window anchors
```

## Status query & report export

Batches print progress to stdout; for detached operation run with your
platform's backgrounding and poll the out dir:

- `cat <out>/batch_state.json` — per-module done/failed
- `cat <out>/summary.json` — final verdicts (only trust it when
  `recovery == "VERIFIED"`)
- JUnit XML files import directly into CI test reporting

## Claim discipline (matches CTS_FAST_TESTING.md §4)

- Runner errors, missing cases, zero-case runs, and skips must NOT be
  reported as passes — the harness already maps these to
  ENV_BLOCKED / NOT_RUN; don't paper over them in summaries.
- If `recovery == "PENDING"`, say the batch is INCOMPLETE, not failed and
  not passed.
- A green batch on selected slices is evidence for those APIs only — never
  claim "CTS certified" from slice runs.
