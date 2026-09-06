"""Verify a copied catalog survives a real OS access-denied interval."""
import argparse
import json
import sqlite3
from pathlib import Path
from perf_log import read_samples


def verify(database, protected, log, stage):
    # URI read-only prevents verification from creating or changing the index.
    with sqlite3.connect(database.resolve().as_uri() + '?mode=ro', uri=True) as connection:
        rows = connection.execute('SELECT path,size FROM images').fetchall()
    if len(rows) != 2:
        raise ValueError(f'{stage}: expected both indexed images, got {len(rows)}')
    matches = [row for row in rows if Path(row[0]) == protected]
    if len(matches) != 1:
        raise ValueError('Protected image was removed from the index')
    metrics = {}
    header = None
    with log.open(encoding='utf-8-sig') as source:
        for row in read_samples(source):
            if row.get('kind') == 'run_header':
                header = row
            elif row.get('name') in ('scan_visited_files', 'scan_elapsed_ms', 'log_flush', 'log_dropped', 'soak_completed_seconds'):
                metrics.setdefault(row['name'], []).append(row['value'])
    certificate = json.loads(log.with_suffix('.complete.json').read_text(encoding='utf-8-sig'))
    if not header or certificate.get('run_id') != header['run_id'] or not certificate.get('sync_completed') or certificate.get('bytes') != log.stat().st_size:
        raise ValueError('No durable run completion')
    if not metrics.get('scan_elapsed_ms') or max(metrics.get('log_dropped', [1])) != 0:
        raise ValueError('Incomplete scan or missing samples')
    visits = max(metrics.get('scan_visited_files', [0]))
    expected = 1 if stage == 'denied' else 2
    if visits != expected:
        raise ValueError(f'{stage}: expected {expected} accessible files, visited {visits}')
    if stage != 'denied' and matches[0][1] != protected.stat().st_size:
        raise ValueError('Recovered file version was not updated')
    return dict(stage=stage,retained_records=len(rows),visited_files=visits,protected_indexed_size=matches[0][1],
                log=str(log),passed=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('database', type=Path)
    parser.add_argument('protected', type=Path)
    parser.add_argument('log', type=Path)
    parser.add_argument('stage', choices=['readable','denied','restored'])
    args = parser.parse_args()
    print(json.dumps(verify(args.database,args.protected,args.log,args.stage)))
