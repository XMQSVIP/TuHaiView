"""Summarize a completed serial native benchmark with binary provenance."""
import argparse
import json
import math
import re
import statistics
from pathlib import Path


def distribution(values):
    values = sorted(values)
    if not values or not all(math.isfinite(v) for v in values):
        raise ValueError('Missing or non-finite benchmark values')
    return dict(n=len(values), median=statistics.median(values),
                p95=values[math.ceil(len(values) * .95) - 1],
                p99=values[math.ceil(len(values) * .99) - 1], maximum=values[-1])


def collect(directory):
    provenance = json.loads((directory / 'provenance.json').read_text(encoding='utf-8-sig'))
    if not provenance.get('completed') or not provenance.get('product_sha256'):
        raise ValueError('Missing completed run or product binary provenance')

    def output(name):
        text = (directory / (name + '.txt')).read_text(encoding='utf-8-sig')
        if 'test result: ok. 1 passed; 0 failed; 0 ignored;' not in text:
            raise ValueError(f'{name}: missing single-test success')
        return text

    def events(name, prefix):
        output(name)
        rows = []
        for line in (directory / (name + '-errors.txt')).read_text(encoding='utf-8-sig').splitlines():
            if line.startswith(prefix + ' '):
                rows.append(json.loads(line[len(prefix) + 1:]))
        return rows

    report = dict(provenance=provenance)
    for kind in ['baseline', 'candidate']:
        values = []
        for run in range(1, 6):
            match = re.search(r'finished in ([\d.]+)s', output(f'db-{kind}-{run}'))
            if not match:
                raise ValueError('Database duration missing')
            values.append(float(match[1]) * 1000)
        report['db_' + kind] = dict(runs_ms=values, summary=distribution(values))
    ratio = report['db_candidate']['summary']['median'] / report['db_baseline']['summary']['median'] - 1
    report.update(db_regression_fraction=ratio, db_passed=ratio <= .1)

    jpeg = events('jpeg', 'PERF')
    report['jpeg'] = {}
    for file in sorted({row['file'] for row in jpeg}):
        groups = {}
        for fast in [False, True]:
            rows = [row for row in jpeg if row['file'] == file and row['fast'] == fast]
            if len(rows) != 5 or {row['run'] for row in rows} != set(range(5)):
                raise ValueError(f'{file}: expected five independent decode samples')
            groups[str(fast)] = dict(runs_ms=[r['ms'] for r in rows], summary=distribution([r['ms'] for r in rows]))
        reduction = 1 - groups['True']['summary']['median'] / groups['False']['summary']['median']
        groups.update(reduction=reduction, passed=reduction >= .5)
        report['jpeg'][file] = groups
    if len(report['jpeg']) != 2:
        raise ValueError('Missing 24/48 MP JPEG comparison')
    for fast in [0, 1]:
        rows = [events(f'peak-{fast}-{run}', 'NATIVE_PEAK') for run in range(1, 6)]
        if any(len(row) != 1 for row in rows):
            raise ValueError('Missing process peak sample')
        flat = [row[0] for row in rows]
        if any(r['fast'] != bool(fast) for r in flat):
            raise ValueError('Process peak decode path mismatch')
        report[f'native_peak_{fast}'] = dict(runs=flat, working_set_bytes=distribution([r['peak_working_set'] for r in flat]))
    cache = events('real-cache', 'REAL_CACHE')
    if len(cache) != 1 or cache[0]['count'] != 1000:
        raise ValueError('Missing fixed real JPEG cache sample')
    report['real_cache'] = dict(**cache[0], passed=cache[0]['reduction'] >= .8)
    watcher = []
    for run in range(1, 6):
        output(f'watcher-{run}')
        text = (directory / f'watcher-{run}-errors.txt').read_text(encoding='utf-8-sig')
        match = re.search(r'NATIVE_WATCH add_ms=([\d.]+) visited=(\d+)', text)
        if not match:
            raise ValueError('Missing native watcher measurement')
        watcher.append(dict(ms=float(match[1]), visited=int(match[2])))
    report['watcher'] = dict(runs=watcher, summary=distribution([r['ms'] for r in watcher]),
                             passed=all(r['visited'] == 1 and r['ms'] <= 2000 for r in watcher),
                             boundary='native event to catalog publication, not display latency')
    output('gpu')
    report['gpu_readback_passed'] = True
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('directory', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    report = collect(args.directory)
    (args.output or args.directory / 'summary.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    print(json.dumps({k: v for k, v in report.items() if k.endswith('passed') or k == 'db_regression_fraction'}))
