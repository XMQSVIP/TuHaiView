"""Summarize complete JSONL samples, safely including a currently running log.
Usage: python scripts/summarize_perf.py path/to/performance.jsonl [--output file]
Frame intervals include idle gaps; use the trajectory phase to select active work.
"""
import argparse, collections, json, math, statistics
from pathlib import Path
p=argparse.ArgumentParser(); p.add_argument("log",type=Path); p.add_argument("--output",type=Path); a=p.parse_args()
d=collections.defaultdict(list); phases=collections.defaultdict(list); memory=[]; phase=None
for line in a.log.open(encoding="utf-8"):
    try: sample=json.loads(line)
    except json.JSONDecodeError: continue
    name=sample["name"]; value=sample["value"]; d[name].append(value)
    if name=="trajectory_phase": phase=int(value)
    if name=="frame_interval_ms" and phase is not None: phases[phase].append(value)
    if name=="process_private_bytes": memory.append((sample["time_ms"],value))
def summary(values):
    v=sorted(values)
    return dict(n=len(v),median=statistics.median(v),p95=v[max(0,math.ceil(len(v)*.95)-1)],p99=v[max(0,math.ceil(len(v)*.99)-1)],maximum=max(v))
out=dict(log=str(a.log),metrics={k:summary(v) for k,v in d.items()},frame_intervals_by_phase={str(k):summary(v) for k,v in phases.items()})
if memory:
    minutes=collections.defaultdict(list); start=memory[0][0]
    for t,v in memory: minutes[int((t-start)/60000)].append(v)
    out["private_bytes_by_minute"]={str(k):summary(v) for k,v in minutes.items()}
text=json.dumps(out,indent=2)
if a.output: a.output.write_text(text,encoding="utf-8")
else: print(text)
