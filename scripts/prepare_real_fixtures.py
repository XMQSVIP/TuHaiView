"""Bounded, resumable local fixture copy. Never modify the source tree."""
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import shutil
import time
from PIL import Image

SUPPORTED = {'.jpg', '.jpeg', '.png', '.webp', '.gif', '.bmp', '.tif', '.tiff', '.ico'}

def files(root):
    for folder, dirs, names in os.walk(root, followlinks=False):
        dirs[:] = sorted(d for d in dirs if not (Path(folder) / d).is_symlink())
        for name in sorted(names):
            path = Path(folder) / name
            if path.suffix.lower() in SUPPORTED and not path.is_symlink():
                yield path

def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--destination', type=Path, required=True)
    parser.add_argument('--count', type=int, default=50_000)
    args = parser.parse_args()
    source = args.source.resolve(strict=True)
    destination = args.destination.resolve()
    if source == destination or source in destination.parents or destination in source.parents:
        raise SystemExit('Source and fixture tree must be separate')
    destination.mkdir(parents=True, exist_ok=True)
    categories = sorted(p for p in source.iterdir() if p.is_dir() and not p.is_symlink())
    streams = [iter(files(p)) for p in categories]
    # Root files are another category; never recurse through the root a second time.
    streams.append(iter(sorted(p for p in source.iterdir() if p.is_file() and p.suffix.lower() in SUPPORTED)))
    start = time.monotonic()
    copied = total_bytes = errors = 0
    formats = {}
    manifest = destination / 'manifest.jsonl'
    with manifest.open('w', encoding='utf-8') as output:
        while streams and copied < args.count:
            remaining = []
            for stream in streams:
                if copied >= args.count:
                    break
                try:
                    path = next(stream)
                except StopIteration:
                    continue
                remaining.append(stream)
                relative = path.relative_to(source)
                target = destination / 'catalog' / f'part-{copied // 10_000 + 1:02}' / relative
                stat = path.stat()
                if shutil.disk_usage(destination).free < stat.st_size + 2 * 1024**3:
                    raise RuntimeError('Keep at least 2 GiB free in fixture volume')
                target.parent.mkdir(parents=True, exist_ok=True)
                if not target.exists() or target.stat().st_size != stat.st_size or target.stat().st_mtime_ns != stat.st_mtime_ns:
                    shutil.copy2(path, target)
                after = path.stat()
                if (stat.st_size, stat.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                    raise RuntimeError('Source changed during fixture copy')
                digest = hashlib.sha256()
                with target.open('rb') as data:
                    for block in iter(lambda: data.read(1024 * 1024), b''):
                        digest.update(block)
                row = dict(index=copied, source=str(relative), fixture=str(target.relative_to(destination)), bytes=stat.st_size, modified_ns=stat.st_mtime_ns, sha256=digest.hexdigest())
                try:
                    with Image.open(target) as image:
                        row.update(format=image.format, width=image.width, height=image.height, mode=image.mode)
                except Exception as error:
                    row['header_error'] = type(error).__name__
                    errors += 1
                formats[row.get('format', 'invalid')] = formats.get(row.get('format', 'invalid'), 0) + 1
                output.write(json.dumps(row, ensure_ascii=False) + '\n')
                total_bytes += stat.st_size
                copied += 1
                if copied % 1000 == 0:
                    output.flush()
                    print(json.dumps(dict(copied=copied, gib=round(total_bytes/1024**3, 3), seconds=round(time.monotonic()-start))), flush=True)
            streams = remaining
    summary = dict(source=str(source), destination=str(destination), count=copied, bytes=total_bytes, formats=formats, header_errors=errors, complete=copied == args.count, manifest_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(), seconds=time.monotonic()-start, sampling='round-robin top-level categories; stop at count; no full-library inventory')
    (destination/'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(summary, ensure_ascii=False), flush=True)

if __name__ == '__main__':
    main()
