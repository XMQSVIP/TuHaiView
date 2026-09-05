"""Copy a bounded prefix of the existing local fixture manifest to SSD and HDD.

Never enumerates the original library. Existing files are verified against the
manifest; reruns preserve a deterministic identical comparison set.
"""
import argparse
import hashlib
import json
import shutil
from pathlib import Path


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream,'sha256').hexdigest()


def prepare(fixtures, destinations, limit):
    fixtures=fixtures.resolve(strict=True)
    destinations=[p.resolve() for p in destinations]
    catalog=fixtures/'catalog'
    for target in destinations:
        if target==catalog or target in catalog.parents or catalog in target.parents:
            raise ValueError('Comparison destination must be separate from the 50k catalog')
    if len(set(destinations))!=len(destinations): raise ValueError('Duplicate destination')
    for left in destinations:
        for right in destinations:
            if left!=right and left in right.parents: raise ValueError('Destinations overlap')
    manifest=fixtures/'manifest.jsonl'; selected=[]; total=0
    with manifest.open(encoding='utf-8') as source:
        for line in source:
            record=json.loads(line)
            if total+record['bytes']>limit: break
            relative=Path(record['fixture'])
            path=(fixtures/relative).resolve(strict=True)
            if catalog not in path.parents: raise ValueError('Manifest escapes fixture catalog')
            if digest(path)!=record['sha256']: raise ValueError('Fixture hash changed')
            for destination in destinations:
                destination.mkdir(parents=True,exist_ok=True)
                if shutil.disk_usage(destination).free < 2*1024**3+record['bytes']:
                    raise RuntimeError('Keep at least 2 GiB free')
                output=destination/path.relative_to(catalog)
                if destination not in output.resolve().parents:
                    raise ValueError('Destination contains a link outside the fixture directory')
                output.parent.mkdir(parents=True,exist_ok=True)
                if not output.exists() or digest(output)!=record['sha256']:
                    shutil.copy2(path,output)
                if digest(output)!=record['sha256']: raise ValueError('Copy verification failed')
            selected.append(record); total+=record['bytes']
    result=dict(count=len(selected),bytes=total,source_manifest_sha256=digest(manifest),files=selected)
    for destination in destinations:
        (destination/'comparison-manifest.json').write_text(json.dumps(result,ensure_ascii=False),encoding='utf-8')
    print(json.dumps({k:v for k,v in result.items() if k!='files'}))


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('fixtures',type=Path)
    p.add_argument('destinations',nargs=2,type=Path)
    p.add_argument('--max-mib',type=int,default=512)
    a=p.parse_args()
    if not 0<a.max_mib<=512: p.error('SSD comparison limit must be within 1..512 MiB')
    prepare(a.fixtures,a.destinations,a.max_mib*1024**2)
