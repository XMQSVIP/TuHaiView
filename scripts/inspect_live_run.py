"""Read a bounded tail for progress only; never issue an acceptance verdict.

This intentionally avoids scanning completed multi-GiB logs while a graphics
benchmark is running. Durable certificates and full reports remain authoritative.
"""
import argparse
import json
import shutil
from pathlib import Path
from perf_log import expand


def inspect(directory):
    metadata = sorted(directory.glob('*/*-run.json'))
    validated = sorted(directory.glob('*/*-validated-summary.json'), key=lambda p: p.stat().st_mtime)
    active = sorted(directory.glob('*/performance-*.jsonl'), key=lambda p: p.stat().st_mtime)
    report = {'completed_process_runs': len(metadata), 'c_free_gib': round(shutil.disk_usage('C:/').free / 1024**3, 3)}
    report['validated_runs'] = len(validated)
    report['validated_non_warmup_runs'] = sum('warmup' not in p.parent.name for p in validated)
    report['aggregated_groups'] = len(list(directory.glob('*/aggregate.json')))
    if validated:
        result = json.loads(validated[-1].read_text(encoding='utf-8-sig'))
        report['latest_validated'] = dict(path=str(validated[-1]),
            errors=result.get('immediate_validation_errors'), memory=result.get('memory_stability'),
            reclamation=result.get('idle_reclamation', {}).get('passed'))
    if not active:
        report['active_log'] = None
        return report
    path = active[-1]
    size = path.stat().st_size
    with path.open('rb') as source:
        source.seek(max(0, size - 2 * 1024 * 1024))
        rows = source.read().splitlines()
    latest = {}
    timestamp = None
    phase = None
    for line in rows:
        try:
            records=list(expand(json.loads(line)))
        except (ValueError, UnicodeDecodeError):
            continue
        for item in records:
            timestamp = item.get('monotonic_us', timestamp)
            phase = item.get('scenario', phase)
            if 'name' in item and 'value' in item:
                latest[item['name']] = item['value']
    wanted = ['process_private_bytes', 'catalog_displayed_records', 'decode_budget_bytes',
              'ready_budget_bytes', 'cache_queue_bytes', 'gpu_allocated_bytes',
              'cpu_retired_count', 'image_inflight_count', 'gpu_retired_bytes', 'log_dropped']
    report.update(active_log=str(path), elapsed_seconds=round(timestamp / 1e6, 1) if timestamp else None,
                  phase=phase, tail_values={k: latest[k] for k in wanted if k in latest},
                  limitation='Bounded tail only; not complete peaks, trends, or a pass verdict.')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    print(json.dumps(inspect(args.directory), ensure_ascii=False))
