"""Small independent DX12 reproduction; reports evidence, never product acceptance."""
import argparse
import collections
import hashlib
import json
import statistics
from pathlib import Path


def analyze(path):
    header=None
    completion=None
    minutes=collections.defaultdict(lambda:collections.defaultdict(list))
    idle=collections.defaultdict(list)
    minimized=False
    samples=0
    with path.open(encoding='utf-8') as source:
        for line in source:
            row=json.loads(line)
            if row.get('kind')=='probe_header':
                header=row
                continue
            if 'completed_seconds' in row:
                completion=row
                continue
            if 'seconds' not in row: continue
            samples+=1
            minimized |= row.get('window_minimized',False)
            destination=idle if row.get('idle') else minutes[int(row['seconds']//60)]
            for key,value in row.items():
                if isinstance(value,(int,float)) and not isinstance(value,bool):
                    destination[key].append(value)
    medians={str(m):{k:statistics.median(v) for k,v in values.items()} for m,values in minutes.items()}
    valid=bool(header and completion and completion.get('completed') and not minimized
        and samples>=0.8*(header['seconds']+header['idle_seconds']))
    result=dict(log=str(path),sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
        header=header,completion=completion,valid_diagnostic=valid,window_minimized=minimized,
        median_by_minute=medians,idle_medians={k:statistics.median(v) for k,v in idle.items()},
        interpretation='diagnostic_only; does not prove absence of leaks or meet product acceptance')
    stable=[(int(m),v['private_bytes']/1024**2) for m,v in medians.items()
        if int(m)>=1 and (int(m)+1)*60<=header['seconds'] and 'private_bytes' in v] if header else []
    if len(stable)>=2:
        xs,ys=zip(*stable)
        xm,ym=statistics.mean(xs),statistics.mean(ys)
        result['private_slope_mib_per_min']=sum((x-xm)*(y-ym) for x,y in stable)/sum((x-xm)**2 for x in xs)
        result['private_growth_mib']=ys[-1]-ys[0]
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('log',type=Path);p.add_argument('--output',type=Path)
    a=p.parse_args();result=analyze(a.log);text=json.dumps(result,indent=2)
    if a.output: a.output.write_text(text,encoding='utf-8')
    print(text)
    raise SystemExit(not result['valid_diagnostic'])
