#!/usr/bin/env python3
"""Offline contract exercise. Generated driver receipts are hypothetical, never device evidence."""
import argparse
import datetime as dt
import hashlib
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--phonefarm", type=pathlib.Path, default=ROOT / "src/target/debug/phonefarm")
parser.add_argument("--game-opt-loop", type=pathlib.Path, default=ROOT.parent / "game-loop-integration/target/debug/game_opt_loop")
parser.add_argument("--out", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "cross_cli")
args = parser.parse_args()
OUT = args.out.resolve()
OUT.mkdir(exist_ok=False)
PF = args.phonefarm.resolve()
GOL = args.game_opt_loop.resolve()
commands = []

def digest(data):
    return hashlib.sha256(data).hexdigest()

def write(name, content):
    data = content.encode() if isinstance(content, str) else json.dumps(content, separators=(",", ":")).encode()
    (OUT / name).write_bytes(data)
    return {"path": name, "sha256": digest(data)}

def invoke(label, argv):
    result = subprocess.run([str(x) for x in argv], capture_output=True, text=True, timeout=30)
    (OUT / (label + ".stdout.json")).write_text(result.stdout)
    (OUT / (label + ".stderr.txt")).write_text(result.stderr)
    commands.append({"label": label, "argv": [str(x) for x in argv], "exit_code": result.returncode})
    if not result.stdout.strip():
        raise RuntimeError(f"{label} failed without report: {result.stderr}")
    return result.returncode, json.loads(result.stdout)

def stamp(value):
    return value.isoformat().replace("+00:00", "Z")

now = dt.datetime.now(dt.timezone.utc)
condition = write("conditions.json", {
    "schema": "phonefarm/confirmation_conditions@1", "device_serial": "hypothetical-contract-device",
    "device_boot_id": "hypothetical-boot", "snapshot_cpu_policies": ["policy0"], "render_comm": "UnityGfxDeviceW", "capture_seconds": 2,
    "frozen_at": stamp(now - dt.timedelta(minutes=11)),
    "policy": {"version": 1, "goal": "render_efficiency", "required_metrics": ["frame_p95", "fps_mean", "power_w_mean"],
               "required_pairs": 5, "require_quality": False, "temp_cap_c": 70., "cool_c": 40.,
               "baseline_min_runs": 3, "dispersion_max_pct": 5., "monotonic_max_ratio": .9,
               "minimum_pairs_for_conclusion": 5, "alpha": .05},
})
baseline = write("baseline.json", {"kind": "sysparam", "payload": {"params": {}, "plan": [], "deny_keywords": []}})
candidate = write("candidate.json", {"kind": "sysparam", "payload": {
    "params": {"bus.DDR.boost_freq": "3200000"}, "plan": [{"kind": "sysfs", "path": "/sys/devices/system/cpu/bus_dcvs/DDR/boost_freq", "value": "3200000"}],
    "deny_keywords": [],
}})
snapshot_keys = ["kgsl.min_pwrlevel", "kgsl.max_pwrlevel", "kgsl.default_pwrlevel", "kgsl.thermal_pwrlevel", "kgsl.force_clk_on", "kgsl.force_rail_on", "kgsl.force_bus_on", "kgsl.force_no_nap", "kgsl.idle_timer", "kgsl.max_gpuclk", "kgsl.pwrscale", "kgsl.bus_split", "kgsl.perfcounter", "kgsl.devfreq_governor", "kgsl.devfreq_min_freq", "kgsl.devfreq_max_freq", "bus.DDR.hw_max_freq", "bus.DDR.hw_min_freq", "bus.LLCC.boost_freq", "bus.LLCC.hw_max_freq", "bus.LLCC.hw_min_freq", "wm.size", "wm.density", "settings.peak_refresh_rate", "settings.min_refresh_rate", "settings.refresh_rate_mode"]
before = "# device_snapshot v1\nftrace.tracing_on=0\nftrace.current_tracer=nop\nftrace.buffer_size_kb=65536\nftrace.set_event=\nbus.DDR.boost_freq=0\n"
before += "".join(key+"=<unreadable>\n" for key in snapshot_keys)
before += "cpu.policy0.governor=schedutil\ncpu.policy0.scaling_max=2000000\ncpu.policy0.scaling_min=300000\n# end\n#adb_exit=0"
probe = "# probe_sysparam v1\ncpu.policies=0\nbus.DDR.boost_freq.cur=0\nbus.DDR.hw_min_freq=200000\nbus.DDR.hw_max_freq=5333000\nbus.DDR.avail_freqs=200000 3200000 5333000\nbus.DDR.boost_freq.writable=yes\nbus.DDR.boost_freq.effect=live\n"
excerpt = (ROOT / "loop_v1/fixtures/trace_slice.txt").read_text().splitlines()[1:]

def run(index, identifier, role, phase, start, pair=None, origin="recorded_device", factor=1.):
    base = 1045979.1 + index * 60
    shift = lambda m: f"{base + (float(m[0])-1045979.1)*factor:.6f}"
    trace = f"#loop_v1_meta capture_start_uptime={base} capture_end_uptime={base+2.46} duration_req=2\n"
    trace += re.sub(r"10459\d{2}\.\d+", shift, "\n".join(excerpt)) + "\n"
    current = -1000000 if role == "baseline" else -900000 - index * 1000
    env = f"#sample_env v1 dur=5 interval=1\n#battery_status=Discharging\n#battery_status_after=Discharging\n#fan_state=unknown\n# hypothetical-run={identifier}\n"
    env += "".join(f"ENV {base-1+n} 3850000 {current} NA 0 0 cpu-1-0 39000\n" for n in range(5))
    env += "#sample_env done\n"
    raw = {}
    active = before if role == "baseline" else before.replace("bus.DDR.boost_freq=0", "bus.DDR.boost_freq=3200000")
    for key, value in {"trace": trace, "env": env, "workload_exit": "0\n", "sampler_exit": "0\n",
                       "capture_exit": "0\n", "restore_exit": "0\n", "root_id": "uid=0(root) gid=0(root)\n",
                       "boot_id": "hypothetical-boot\n", "probe": probe, "snapshot_before": before,
                       "snapshot_at_run": active, "snapshot_after_run": active, "snapshot_restored": before}.items():
        raw[key] = write(f"{identifier}-{key}.txt", value)
    raw["cold"] = write(f"{identifier}-cold.json", {"checked_at": stamp(start-dt.timedelta(seconds=1)), "zone": "cpu-1-0", "c": 39., "waited_s": 0})
    document = {"schema": "phonefarm/run_one_bundle@1", "evidence_origin": origin, "phase": phase, "run_id": identifier,
                "pair_id": pair, "sample_role": role, "device_serial": "hypothetical-contract-device", "started_at": stamp(start),
                "finished_at": stamp(start+dt.timedelta(seconds=10)), "conditions": condition,
                "artifact": baseline if role == "baseline" else candidate, "raw": raw}
    return write(identifier+".run.json", document), document

admission_runs = [run(n, f"explore{n}", "baseline", "admission", now-dt.timedelta(minutes=10-n)) for n in range(1, 4)]
admission = write("admission.json", {"schema": "phonefarm/confirmation_baseline_admission@1", "conditions": condition, "baseline_artifact": baseline,
                                      "runs": [item[0] for item in admission_runs]})
plan = {"baseline_artifact_sha256": baseline["sha256"], "candidate_artifact_sha256": candidate["sha256"], "conditions_sha256": condition["sha256"],
        "baseline_admission_sha256": admission["sha256"], "exploration_run_ids": [f"explore{n}" for n in range(1, 4)],
        "exploration_artifact_sha256": [r[1]["raw"][key]["sha256"] for r in admission_runs for key in ("trace", "env")],
        "pairs": 5, "schedule": [{"pair_id": f"p{n}", "a_run_id": f"a{n}", "b_run_id": f"b{n}", "baseline_first": n%2==1} for n in range(1, 6)],
        "family_alpha": .05, "paired_t_assumptions_acknowledged": True,
        "metrics": [{"name": "fps_mean", "direction": "higher", "scale": "percent", "minimum_improvement": 1., "maximum_regression": 1.},
                    {"name": "frame_p95_ms", "direction": "lower", "scale": "percent", "minimum_improvement": None, "maximum_regression": 51.248178375505404},
                    {"name": "power_watt", "direction": "lower", "scale": "percent", "minimum_improvement": None, "maximum_regression": 1.}]}
write("plan.json", plan)
code, frozen = invoke("freeze", [GOL, "confirm", "freeze", "--plan", OUT/"plan.json", "--out", OUT/"frozen.json"])
assert code == 0
frozen_ref = {"path": "frozen.json", "sha256": digest((OUT/"frozen.json").read_bytes())}
registered = dt.datetime.fromisoformat(frozen["registered_at"])
reports = {}
sample_pairs = []
for pair in plan["schedule"]:
    n = int(pair["pair_id"][1:])
    order = [("a", "baseline"), ("b", "candidate")] if pair["baseline_first"] else [("b", "candidate"), ("a", "baseline")]
    for j, (arm, role) in enumerate(order):
        index = 5 + (n-1)*2 + j
        ident = f"{arm}{n}"
        r, _ = run(index, ident, role, "confirmation", registered+dt.timedelta(minutes=index), pair["pair_id"], factor=1. if role=="baseline" else .95+n*.001)
        write(ident+".bundle.json", {"schema": "phonefarm/confirmation_bundle@1", "run": r, "baseline_admission": admission, "frozen_plan": frozen_ref})
        code, report = invoke(ident, [PF, "confirmation-observation", "--bundle", OUT/(ident+".bundle.json"), "--out", OUT/(ident+".report.json")])
        assert code == 0 and report["status"] == "VALID_OBSERVATION", report
        assert report["qualification"]["eligible"] is False and report["qualification"]["stage"] == "experiment"
        assert not report["real_game_optimization_proven"] and not report["promotion_allowed"]
        reports[ident] = report
    sample_pairs.append({"pair_id":pair["pair_id"],"a_run_id":pair["a_run_id"],"b_run_id":pair["b_run_id"],
                         "a_report":pair["a_run_id"]+".report.json","b_report":pair["b_run_id"]+".report.json",
                         "a_report_sha256":digest((OUT/(pair["a_run_id"]+".report.json")).read_bytes()),
                         "b_report_sha256":digest((OUT/(pair["b_run_id"]+".report.json")).read_bytes())})
samples = {"frozen_plan_sha256": frozen["sha256"], "candidate_artifact_sha256": candidate["sha256"], "conditions_sha256": condition["sha256"],
           "source_kind": "recorded_receiver_reports", "pairs":sample_pairs}
write("samples.json", samples)
code, result = invoke("judge", [GOL, "confirm", "judge", "--frozen", OUT/"frozen.json", "--samples", OUT/"samples.json", "--out", OUT/"judge.json"])
assert result["verdict"] == "PASS" and code == 0, result
assert not result["real_game_optimization_proven"] and not result["promotion_allowed"]
assert len(result["report_hashes"]) == 10 and len(result["metrics"]) == 3

# Hash-consistent report tampering must still fail the consumer's single-observation gate.
tampered = json.loads(json.dumps(reports["b1"]))
tampered["sample_qualification"]["gates"]["restore_verified"] = False
write("b1.tampered.report.json", tampered)
bad = json.loads(json.dumps(samples))
bad["pairs"][0]["b_report"] = "b1.tampered.report.json"
bad["pairs"][0]["b_report_sha256"] = digest((OUT/"b1.tampered.report.json").read_bytes())
write("samples-tampered.json", bad)
code, rejected = invoke("judge-tampered", [GOL, "confirm", "judge", "--frozen", OUT/"frozen.json", "--samples", OUT/"samples-tampered.json", "--out", OUT/"judge-tampered.json"])
assert code != 0 and rejected["verdict"] == "BLOCKED"
assert not rejected["promotion_allowed"] and not rejected["real_game_optimization_proven"]

# Explicit synthetic source cannot get even single-run observation validity.
r, _ = run(30, "synthetic", "baseline", "confirmation", registered+dt.timedelta(minutes=30), "p1", origin="synthetic")
document = json.loads((OUT / "synthetic.run.json").read_text()); document["run_id"] = "a1"
r = write("synthetic.run.json", document)
write("synthetic.bundle.json", {"schema":"phonefarm/confirmation_bundle@1","run":r,"baseline_admission":admission,"frozen_plan":frozen_ref})
code, rejected = invoke("synthetic", [PF,"confirmation-observation","--bundle",OUT/"synthetic.bundle.json","--out",OUT/"synthetic.report.json"])
assert code != 0 and rejected["status"] == "BLOCKED" and not rejected["sample_qualification"]["valid_observation"]
write("summary.json", {"scope":"hypothetical driver declarations and transformed archived raw excerpt; zero device/API calls; no acquisition attestation",
                       "gol_binary_sha256":digest(GOL.read_bytes()),"phonefarm_binary_sha256":digest(PF.read_bytes()),
                       "commands":commands,"observations_valid":10,"judge":result["verdict"],"tampered": "BLOCKED","synthetic":"BLOCKED",
                       "nontrivial_float_margin":frozen["plan"]["metrics"][1]["maximum_regression"],
                       "real_game_optimization_proven":False,"promotion_allowed":False})
print(json.dumps({"output":str(OUT),"observations_valid":10,"judge":result["verdict"],"tampered":"BLOCKED","proof_flags":False}))
