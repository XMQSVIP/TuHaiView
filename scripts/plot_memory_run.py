"""Render measured per-minute private bytes and resource peaks from one run."""
import argparse
import json
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt


def render(aggregate, destination):
    data = json.loads(aggregate.read_text(encoding='utf-8-sig'))
    if len(data['runs']) != 1:
        raise ValueError('Choose an aggregate containing exactly one independent run')
    run = data['runs'][0]
    result = run['result']
    minutes = result['private_bytes_by_minute']
    x = sorted(map(int, minutes))
    y = [minutes[str(m)]['median'] / 1024**2 for m in x]
    stability = result.get('memory_stability', {})
    fig, axes = plt.subplots(2, 1, figsize=(11, 7.2), height_ratios=[2, 1], constrained_layout=True)
    ax = axes[0]
    ax.plot(x, y, color='#185a87', marker='o', markersize=4, linewidth=1.4, label='Minute median')
    for start in range(2, max(x) + 1, 4):
        ax.axvspan(start - .5, min(start + 1.5, max(x) + .5), color='#eee1c5', alpha=.5)
    ax.set(xlabel='Elapsed minute', ylabel='Process private bytes (MiB)',
           title='30-minute SSD run: fixed mesh-upload reuse candidate')
    ax.grid(axis='y', alpha=.2)
    ax.set_xlim(-.5, max(x) + .5)
    ax.text(.02, .95, f"Steady slope: {stability.get('slope_mib_per_min', float('nan')):.3f} MiB/min\n"
            f"End growth: {stability.get('growth_mib', float('nan')):.2f} MiB\n"
            'Shaded: special-fixture directory (alternates with 50k catalog)',
            transform=ax.transAxes, va='top', fontsize=9, bbox=dict(facecolor='white', alpha=.9, edgecolor='none'))
    labels, values, limits = [], [], []
    for name, label in [('decode_budget_bytes','Decode'),('ready_budget_bytes','Ready pixels'),
                        ('cache_queue_bytes','Cache writes'),('gpu_allocated_bytes','Image textures')]:
        budget = result['managed_budgets'][name]
        labels.append(label)
        values.append(budget['peak_bytes'] / 1024**2)
        limits.append(budget['limit_bytes'] / 1024**2)
    ax = axes[1]
    ax.barh(labels, limits, color='#e6e9ed', height=.65, label='Budget')
    ax.barh(labels, values, color='#357c69', height=.4, label='Measured managed peak')
    for n, (value, limit) in enumerate(zip(values, limits)):
        ax.text(limit + 5, n, f'{value:.1f} / {limit:.0f}', va='center', fontsize=9)
    ax.set(xlabel='MiB (managed resources; excludes driver / native buffer pool)', xlim=(0, 600))
    ax.legend(loc='upper right', fontsize=8)
    status = 'valid single run' if not run['errors'] else 'INVALID run'
    fig.suptitle(f"{status}; not a five-run acceptance matrix\nEXE SHA-256: {data['hashes'][0]}", fontsize=10)
    fig.savefig(destination, dpi=150)
    plt.close(fig)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('aggregate', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    render(args.aggregate, args.destination)
