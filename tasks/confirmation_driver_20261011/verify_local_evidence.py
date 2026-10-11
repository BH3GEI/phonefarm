#!/usr/bin/env python3
"""Reconstruct every synthetic driver bundle via actual CLIs. Never invokes transport/device/API."""
import argparse
import hashlib
import json
import pathlib
import subprocess


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(argv, prefix):
    proc = subprocess.run([str(v) for v in argv], capture_output=True, timeout=30, check=False)
    prefix.with_suffix('.stdout').write_bytes(proc.stdout)
    prefix.with_suffix('.stderr').write_bytes(proc.stderr)
    prefix.with_suffix('.exit.json').write_text(json.dumps({'argv': [str(v) for v in argv], 'exit': proc.returncode})+'\n')
    return proc.returncode


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--phonefarm', type=pathlib.Path, required=True)
    parser.add_argument('--gol', type=pathlib.Path, required=True)
    parser.add_argument('--fixture', type=pathlib.Path, required=True)
    parser.add_argument('--out', type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    session = args.fixture/'session'
    samples = json.loads((session/'samples.json').read_text())
    assert samples['source_kind'] == 'synthetic_fixture'
    reports = []
    for pair in samples['pairs']:
        for side in ('a', 'b'):
            run_id = pair[side+'_run_id']
            output = args.out/(run_id+'.json')
            code = run([args.phonefarm, 'confirmation-observation', '--bundle', session/(run_id+'-bundle.json'), '--out', output], args.out/(run_id+'-cli'))
            report = json.loads(output.read_text())
            assert code != 0 and report['status'] == 'BLOCKED'
            assert report['qualification']['eligible'] is False
            assert report['qualification']['evidence_origin'] == 'synthetic'
            assert report['real_game_optimization_proven'] is False and report['promotion_allowed'] is False
            # Offline reconstruction must be byte-equivalent JSON to the driver producer.
            assert report == json.loads((session/pair[side+'_report']).read_text())
            reports.append({'run_id': run_id, 'exit': code, 'status': report['status'], 'report_sha256': sha(session/pair[side+'_report'])})
    judge = args.out/'gol-report.json'
    code = run([args.gol, 'confirm', 'judge', '--frozen', args.fixture/'frozen.json', '--samples', session/'samples.json', '--out', judge], args.out/'gol-judge')
    report = json.loads(judge.read_text())
    assert code != 0 and report['verdict'] == 'BLOCKED'
    assert len(report['report_hashes']) == 10 and report['claim_scope'] == 'none'
    assert report['real_game_optimization_proven'] is False and report['promotion_allowed'] is False
    summary = {'schema': 'phonefarm/confirmation_driver_offline_verification@1', 'evidence_origin': 'synthetic', 'device_executed': False, 'phonefarm_sha256': sha(args.phonefarm), 'gol_sha256': sha(args.gol), 'baseline_runs': 3, 'heldout_reports': reports, 'gol_exit': code, 'gol_verdict': report['verdict'], 'real_game_optimization_proven': False, 'promotion_allowed': False, 'scope': 'host orchestration and raw/consumer interoperability only; Android shell/root/GPU not run'}
    (args.out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(json.dumps(summary))


if __name__ == '__main__':
    main()
