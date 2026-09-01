# phonefarm Architecture Reference

## System components

- Kernel: a single Rust binary (main / runtime / brain / device / tree / fold /
  cli / telemetry modules)
- Text files: `phonefarm.toml` (config + thresholds + prompts + provider chain)
  and the `tasks/<task>/` data directories
- Design contract: `docs/DESIGN.md` (record contract / six-step loop / task
  isolation / write permissions)

## Directory layout

```
src/                   Rust kernel source
phonefarm.toml         Config: thresholds, prompts, provider chain (keys via env)
docs/DESIGN.md         Design document v1 (core architecture contract)
round.sh               Legacy single-round script (still works; new flows use benchmark directly)
ocr.swift / ocr        OCR text fallback (macOS Vision, auto-compiled on first use)
tasks/<task>/
  lessons.jsonl        Experience base (win/lose counters, atomic rewrite)
  tree.json            Page state-transition graph (recomputed at session end)
  campaign.tsv         Evaluation ledger
  runs/<run-id>/       log.jsonl (ledger) + ctx.log + stepN.jpg/.xml.gz
                       (screenshots/trees are not committed to git)
```

## The six-step execution loop

capture (screenshot + UI tree in parallel) → context assembly (goal + lessons +
last 5 steps + current screen) → model decision (max 4 actions per call) →
three deterministic gates (value-range / prior-offense / blank-click checks) →
execution (coordinate mapping, wait for the screen to settle) → acceptance
(diff the result, judge whether progress was made).

Exits: done → review (normal) | watchdog / budget (stop-loss) | all providers
failed / device failure (abnormal).
After exit: timeline table → experience summary → lessons.jsonl.

## Data contract (log.jsonl record types)

goal / screen / act / diff / note / lesson / ban / hook(verdict|arbit|heal|budget) /
raw / reflect / telemetry / app_event / trace / end

- The model may only write `act` and `note`; anything beyond is treated as a
  format error
- note ≤ 200 chars; lessons ≤ 20 entries; act/diff window of 5 pairs
- `phonefarm schema` prints the complete field contract, generated from code —
  always up to date

## Key mechanisms

- **Probes** (inspect/find/get_state/history): freeze the world and buy time;
  ask for detail from existing observations without re-capturing the screen
- **done pre-check** (#21): the first `done` claim does not end the session; the
  claim plus the current foreground app is echoed back through the alert
  channel, giving the model one chance to self-correct
- **Wobble detection**: repeated tap→back loops or asking the same question
  repeatedly triggers a system warning
- **Telemetry**: a 68-field snapshot per step, written as pure `r=telemetry`
  ledger rows, never enters the model context
- **Multi-device parallel**: `phonefarm parallel` — in-process multi-threading
  with per-task directory isolation; parallel sessions on the same task name
  are refused by default
