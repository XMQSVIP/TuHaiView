"""Summarize correlated app samples and optional PresentMon 2.5 CSV. No validity by omission."""
import argparse, collections, csv, json, math, statistics
from pathlib import Path

def summary(values):
    v=sorted(values)
    if not v: return None
    return dict(n=len(v),median=statistics.median(v),p95=v[math.ceil(len(v)*.95)-1],p99=v[math.ceil(len(v)*.99)-1],maximum=max(v),over_50ms=sum(x>50 for x in v),over_100ms=sum(x>100 for x in v))

def analyze(log, presentmon=None, refresh_hz=60):
    d=collections.defaultdict(list); phases=collections.defaultdict(list); memory=[]; frames=[]; legacy_phase=None; header=None; malformed=0
    with log.open(encoding="utf-8-sig") as source:
        for line in source:
            try: sample=json.loads(line)
            except json.JSONDecodeError: malformed+=1; continue
            if sample.get("kind")=="run_header": header=sample; continue
            name=sample.get("name"); value=sample.get("value")
            if name is None or not isinstance(value,(int,float)): continue
            d[name].append(value)
            if name=="trajectory_phase": legacy_phase=int(value)
            if name=="frame_interval_ms":
                phase=sample.get("scenario",legacy_phase)
                if phase is not None: phases[phase].append(value)
                if "qpc" in sample: frames.append((sample["qpc"],sample.get("scenario",8)))
            if name=="process_private_bytes": memory.append((sample.get("monotonic_us",sample["time_ms"]*1000)/1000,value))
    reasons=[]
    if not header: reasons.append("legacy_log_without_correlated_frames")
    if not d["soak_completed_seconds"]: reasons.append("missing_scenario_completion")
    if not d["log_flush"]: reasons.append("missing_log_flush")
    if not d["log_dropped"] or max(d["log_dropped"],default=0)>0: reasons.append("missing_or_dropped_samples")
    if malformed: reasons.append("malformed_log_lines")
    out=dict(log=str(log),header=header,metrics={k:summary(v) for k,v in d.items() if v},frame_intervals_by_phase={str(k):summary(v) for k,v in phases.items()},invalid_reasons=reasons)
    if memory:
        minutes=collections.defaultdict(list); start=memory[0][0]
        for t,v in memory: minutes[int((t-start)/60000)].append(v)
        medians={m:statistics.median(v) for m,v in minutes.items()}
        out["private_bytes_by_minute"]={str(k):summary(v) for k,v in minutes.items()}
        stable=[(m,v/1024**2) for m,v in medians.items() if 5<=m<=28]
        if len(stable)>=20:
            xs,ys=zip(*stable); xm=statistics.mean(xs); ym=statistics.mean(ys)
            slope=sum((x-xm)*(y-ym) for x,y in stable)/sum((x-xm)**2 for x in xs)
            growth=statistics.median(ys[-3:])-statistics.median(ys[:3])
            out["memory_stability"]=dict(slope_mib_per_min=slope,growth_mib=growth,passed=slope<=1 and growth<=32 and not reasons)
    if presentmon:
        import bisect
        gpu=[]; latency=[]; displayed=collections.defaultdict(list); all_shown=[]; dropped=0
        frames.sort(); times=[f[0] for f in frames]
        with presentmon.open(encoding="utf-8-sig",newline="") as data:
            for row in csv.DictReader(data):
                try:
                    qpc=int(row.get("CPUStartQPC",0)); index=bisect.bisect_right(times,qpc)-1
                    phase=frames[index][1] if index>=0 else -1
                    raw=row.get("DisplayedTime",row.get("MsBetweenDisplayChange","NA"))
                    if raw in ("NA","",None): dropped+=1; continue
                    value=float(raw)
                    if value>0: displayed[phase].append(value); all_shown.append(value)
                    for key,target in [("MsGPUBusy",gpu),("MsUntilDisplayed",latency)]:
                        try: target.append(float(row[key]))
                        except (ValueError,KeyError): pass
                except (ValueError,KeyError): continue
        out["presentmon"]=dict(path=str(presentmon),displayed_by_phase={str(k):summary(v) for k,v in displayed.items()},all_displayed=summary(all_shown),not_displayed=dropped,gpu_busy_ms=summary(gpu),display_latency_ms=summary(latency))
        scroll=summary(displayed[1]); period=1000/refresh_hz
        if header and header.get("scenario_name") in ("scroll", "trajectory", "soak") and (not scroll or scroll["n"]<300): reasons.append("insufficient_correlated_displayed_scroll_frames")
        out["scroll_acceptance"]=dict(refresh_hz=refresh_hz,p95_limit_ms=period+.5,p99_limit_ms=2*period+.5,passed=bool(scroll and scroll["n"]>=300 and scroll["p95"]<=period+.5 and scroll["p99"]<=2*period+.5 and not reasons))
    else: out["display_acceptance"]="unverified_without_presentmon"
    out["log_valid"]=not reasons
    return out

if __name__ == "__main__":
    p=argparse.ArgumentParser();p.add_argument("log",type=Path);p.add_argument("--output",type=Path);p.add_argument("--presentmon",type=Path);p.add_argument("--refresh-hz",type=float,default=60);a=p.parse_args()
    text=json.dumps(analyze(a.log,a.presentmon,a.refresh_hz),indent=2)
    if a.output:a.output.write_text(text,encoding="utf-8")
    else:print(text)
