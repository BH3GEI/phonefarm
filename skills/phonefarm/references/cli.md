# phonefarm CLI Reference

All inspection commands are read-only, burn zero model tokens, and support
`--json`. Run IDs accept prefix matching; ambiguous prefixes list candidates.

## run / benchmark / parallel

```bash
# Single session
phonefarm run --task <T> [--serial S] [--endless] [--budget-calls N] [--app P] [--assert "a,b"] "<goal>"

# Multi-round evaluation (round.sh is folded into this)
phonefarm benchmark --task <T> [--rounds N] [--budget-calls N] [--app P] [--assert "a,b"] [--json] "<goal>"

# Multi-device parallel
phonefarm parallel --job "<task>|<goal>|<serial>|<app>|<assert>" [--job ...] [--budget-calls N]
#   or share one goal/task across several --serial values:
phonefarm parallel --task <T> --budget-calls N --serial S1 --serial S2 "<goal>"
```

## Inspection layer

```bash
phonefarm last [--task T] [--json]          # latest session verdict (entry point)
phonefarm runs [--task T] [--limit N]       # all sessions of a task
phonefarm show <run-id> [--task T]          # session summary: goal/actions/verdicts/file list
phonefarm show <run-id> --step N            # single step: screenshot+elements+full dump+raw XML+telemetry
phonefarm show <run-id> --raw               # raw model replies
phonefarm show <run-id> --hooks             # system verdicts (r=hook)
phonefarm show <run-id> --events            # event stream (app_event: crash/ANR/fd growth/network changes)
phonefarm show <run-id> --crashes           # crash artifacts in depth
phonefarm show <run-id> --anr               # ANR traces
phonefarm show <run-id> --trace             # system trace
phonefarm cat <path> [--grep word] [--tail N] [--head N]  # universal printer: .gz decompress / .jsonl pretty / .jpg dimensions
phonefarm stats <run-id>                    # telemetry summary: fps/CPU/memory/temperature percentiles
phonefarm schema [--type R] [--markdown]    # log.jsonl contract docs
phonefarm tree|lessons|campaign [--task T]  # cross-session artifacts
phonefarm tasks [--json]                    # all tasks with stats
phonefarm config [--key k] [--json]         # effective configuration
phonefarm probe --serial S "read-only cmd"  # read-only direct device channel
phonefarm exec --serial S "cmd" --yes       # arbitrary device command (dangerous)
```

## Drill-down path

`phonefarm last` → `show <run-id> --step N` → `cat .../stepN.xml.gz`
