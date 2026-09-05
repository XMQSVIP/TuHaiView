"""Bounded-memory diagnostic summary; never substitutes for the acceptance matrix."""
import argparse
import collections
import hashlib
import json
import statistics
from pathlib import Path

METRICS = {
    'process_private_bytes', 'process_working_set_bytes', 'rust_live_bytes',
    'rust_live_allocations', 'wgpu_allocator_reserved_bytes',
    'wgpu_allocator_allocated_bytes', 'wgpu_allocator_allocations',
    'gpu_allocated_bytes', 'egui_data_entries', 'egui_text_layouts',
    'cpu_retired_count', 'image_inflight_count', 'image_ready_count',
}


def analyze(path):
    minutes = collections.defaultdict(lambda: collections.defaultdict(list))
    header = None
    completed = False
    malformed = 0
    dropped = None
    duration = 0
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for line in source:
            digest.update(line)
            try:
                sample = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                malformed += 1
                continue
            if sample.get('kind') == 'run_header':
                header = sample
            name = sample.get('name')
            duration = max(duration, sample.get('monotonic_us', 0) / 1e6)
            if name == 'soak_completed_seconds':
                completed = True
            if name == 'log_dropped':
                dropped = sample['value']
            if name in METRICS:
                minutes[int(sample['monotonic_us'] // 60000000)][name].append(sample['value'])
    medians = {m: {k: statistics.median(v) for k, v in metrics.items()}
               for m, metrics in minutes.items()}
    # Same workload recurs every four minutes in the alternating-root soak.
    # Exclude the partial final minute and the first cold GPU/cache cycle.
    deltas = []
    active_seconds = duration - (30 if header and header.get('scenario_name') == 'soak' else 0)
    for minute in sorted(medians):
        previous = minute - 4
        if previous < 4 or minute + 1 > active_seconds / 60:
            continue
        before = medians.get(previous, {}).get('process_private_bytes')
        after = medians[minute].get('process_private_bytes')
        if before is not None and after is not None:
            deltas.append(dict(start_minute=previous, end_minute=minute,
                               private_growth_mib=(after - before) / 1024**2))
    certificate = None
    try:
        certificate = json.loads(path.with_suffix('.complete.json').read_text(encoding='utf-8-sig'))
    except (OSError, ValueError):
        pass
    durable = bool(certificate and header and certificate.get('sync_completed')
                   and certificate.get('run_id') == header.get('run_id')
                   and certificate.get('bytes') == path.stat().st_size)
    return dict(log=str(path), sha256=digest.hexdigest(), header=header,
                duration_seconds=duration, completed=completed,
                durable_log=durable, malformed=malformed, dropped=dropped,
                median_by_minute=medians, comparable_cycle_deltas=deltas,
                acceptance='diagnostic_only; display and five-run matrix not evaluated')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('log', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    text = json.dumps(analyze(args.log), indent=2)
    if args.output:
        args.output.write_text(text, encoding='utf-8')
    else:
        print(text)
