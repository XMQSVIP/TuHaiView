"""Plot final product minute medians from validated group reports."""
import argparse
import json
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt


def render(groups, output):
    fig, axes = plt.subplots(1, len(groups), figsize=(7 * len(groups), 5), squeeze=False,
                             sharey=True, constrained_layout=True)
    hashes = set()
    colors = ['#195f8c', '#19715b', '#9e682d', '#755b9b', '#a34d63']
    for ax, path in zip(axes[0], groups):
        data = json.loads(path.read_text(encoding='utf-8-sig'))
        hashes.update(data['hashes'])
        for i, run in enumerate(data['runs']):
            result = run.get('result') or {}
            minutes = result.get('private_bytes_by_minute', {})
            x = sorted(map(int, minutes))
            if not x:
                continue
            y = [minutes[str(m)]['median'] / 1024**2 for m in x]
            stability = result.get('memory_stability', {})
            verdict = 'pass' if not run['errors'] and stability.get('passed') else 'INVALID / FAIL'
            slope = stability.get('slope_mib_per_min', float('nan'))
            ax.plot(x, y, marker='.', linewidth=1.25, alpha=.85, color=colors[i % len(colors)],
                    label=f'Run {i+1}: {slope:.3f} MiB/min; {verdict}')
        for start in range(2, 30, 4):
            ax.axvspan(start - .5, start + 1.5, color='#eee1c5', alpha=.35, zorder=-2)
        disk = 'SSD' if 'ssd' in path.stem.lower() else 'HDD'
        title = 'All five memory runs passed' if data.get('memory_passed_all_five') else 'Incomplete / failing memory group'
        ax.set(title=f'{disk}: {title}', xlabel='Elapsed minute', xlim=(-.5, 29.5))
        ax.grid(axis='y', alpha=.2)
        ax.legend(fontsize=8, loc='upper left')
    axes[0, 0].set_ylabel('Process private bytes: minute median (MiB)')
    identity = next(iter(hashes)) if len(hashes) == 1 else 'MIXED BINARIES'
    fig.suptitle('30-minute product memory matrix\n'
                 'Shaded: special-image directory; other minutes: 50,000-image catalog\n'
                 f'EXE SHA-256: {identity}', fontsize=10)
    fig.savefig(output, dpi=150)
    plt.close(fig)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('groups', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    render(args.groups, args.output)
