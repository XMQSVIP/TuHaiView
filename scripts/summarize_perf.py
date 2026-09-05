"""Summarize correlated app samples and optional PresentMon 2.5 CSV. No validity by omission."""
import argparse, collections, csv, json, math, statistics
from pathlib import Path

def summary(values):
    v=sorted(values)
    if not v: return None
    return dict(n=len(v),median=statistics.median(v),p95=v[math.ceil(len(v)*.95)-1],p99=v[math.ceil(len(v)*.99)-1],maximum=max(v),over_50ms=sum(x>50 for x in v),over_100ms=sum(x>100 for x in v))

def analyze(log, presentmon=None, refresh_hz=60):
    d=collections.defaultdict(list); phases=collections.defaultdict(list); per_phase=collections.defaultdict(lambda: collections.defaultdict(list)); memory=[]; frames=[]; legacy_phase=None; header=None; malformed=0
    with log.open(encoding="utf-8-sig") as source:
        for line in source:
            try: sample=json.loads(line)
            except json.JSONDecodeError: malformed+=1; continue
            if sample.get("kind")=="run_header": header=sample; continue
            name=sample.get("name"); value=sample.get("value")
            if name is None or not isinstance(value,(int,float)): continue
            d[name].append(value)
            per_phase[sample.get('scenario',8)][name].append(value)
            if name=="trajectory_phase": legacy_phase=int(value)
            if name=="frame_interval_ms":
                phase=sample.get("scenario",legacy_phase)
                if phase is not None: phases[phase].append(value)
                if "qpc" in sample: frames.append((sample["qpc"],sample.get("scenario",8)))
            if name=="process_private_bytes": memory.append((sample.get("monotonic_us",sample.get("time_ms",0)*1000)/1000,value))
    reasons=[]
    if not header: reasons.append("legacy_log_without_correlated_frames")
    if not d["soak_completed_seconds"]: reasons.append("missing_scenario_completion")
    if not d["log_flush"]: reasons.append("missing_log_flush")
    if not d["log_dropped"] or max(d["log_dropped"],default=0)>0: reasons.append("missing_or_dropped_samples")
    if malformed: reasons.append("malformed_log_lines")
    if header and header.get('schema',0)>=3:
        try:
            certificate=json.loads(log.with_suffix('.complete.json').read_text(encoding='utf-8-sig'))
            if not certificate.get('sync_completed') or certificate.get('run_id')!=header.get('run_id') or certificate.get('bytes')!=log.stat().st_size:
                reasons.append('invalid_flush_certificate')
        except (OSError,ValueError): reasons.append('missing_flush_certificate')
    if max(d['window_minimized'],default=0)>0: reasons.append('window_was_minimized')
    if d['native_dialog_open'] and header and header.get('scenario_name') in ('open','scroll','soak','trajectory'):
        reasons.append('native_modal_interrupted_automated_run')
    if header and header.get('schema',0)>=3:
        for key in ['window_minimized','window_width','window_height','pixels_per_point']:
            if not d[key]: reasons.append('missing_'+key)
    out=dict(log=str(log),header=header,metrics={k:summary(v) for k,v in d.items() if v},frame_intervals_by_phase={str(k):summary(v) for k,v in phases.items()},invalid_reasons=reasons)
    out['metrics_by_phase']={str(p):{k:summary(v) for k,v in metrics.items()} for p,metrics in per_phase.items()}
    out['native_dialog'] = dict(count=len(d['native_dialog_open']), wait_ms=summary(d['native_dialog_wait_ms']),
        interpretation='User/modal wait is preserved separately; input processing excludes this wait only when input_frame_wall_ms is present.')
    budgets={'decode_budget_bytes':512,'ready_budget_bytes':96,'cache_queue_bytes':32,'gpu_allocated_bytes':256}
    out['managed_budgets']={k:dict(limit_bytes=mib*1024**2,peak_bytes=max(d[k],default=None),passed=bool(d[k]) and max(d[k])<=mib*1024**2) for k,mib in budgets.items()}
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
            coverage=all(len(minutes.get(m,[]))>=50 for m in range(5,29))
            full_duration=max(d['soak_completed_seconds'],default=0)>=1800
            out["memory_stability"]=dict(slope_mib_per_min=slope,growth_mib=growth,threshold_passed=slope<=1 and growth<=32,
                full_30_minutes=full_duration,steady_window_sample_coverage=coverage,passed=False)
    if presentmon:
        import bisect
        gpu=[]; latency=[]; input_latency=[]; present_wait=[]; gpu_wait=[]; displayed=collections.defaultdict(list); all_shown=[]; dropped=0; previous={}; transitions=0; present_modes=collections.Counter(); tearing=0
        frames.sort(); times=[f[0] for f in frames]
        with presentmon.open(encoding="utf-8-sig",newline="") as data:
            for row in csv.DictReader(data):
                try:
                    if header and header.get('pid') and str(header['pid']) != row.get('ProcessID'):
                        continue
                    qpc=int(row.get("CPUStartQPC",0)); index=bisect.bisect_right(times,qpc)-1
                    phase=frames[index][1] if index>=0 else -1
                    present_modes[row.get('PresentMode','unknown')]+=1
                    tearing+=row.get('AllowsTearing')=='1'
                    for keys,target in [(('MsGPUBusy',),gpu),(('MsUntilDisplayed','DisplayLatency'),latency),(('MsAllInputToPhotonLatency',),input_latency),(('MsInPresentAPI',),present_wait),(('MsGPUWait',),gpu_wait)]:
                        for key in keys:
                            try: target.append(float(row[key])); break
                            except (ValueError,KeyError): pass
                    raw=row.get("DisplayedTime",row.get("MsBetweenDisplayChange","NA"))
                    if raw in ("NA","",None): dropped+=1; continue
                    value=float(raw)
                    if value<=0: dropped+=1; continue
                    chain=(row.get('ProcessID'),row.get('SwapChainAddress'))
                    prior=previous.get(chain)
                    # Legacy duration belongs to the previous displayed frame; v2 belongs
                    # to this frame. Exclude intervals crossing scenario boundaries in both.
                    if prior is not None and prior[0]==phase and phase>=0:
                        interval=prior[1] if 'DisplayedTime' in row else value
                        if interval>0: displayed[phase].append(interval)
                    elif prior is not None: transitions+=1
                    previous[chain]=(phase,value)
                    if value>0: all_shown.append(value)
                except (ValueError,KeyError): continue
        out["presentmon"]=dict(path=str(presentmon),displayed_by_phase={str(k):summary(v) for k,v in displayed.items()},all_displayed=summary(all_shown),not_displayed=dropped,excluded_phase_transitions=transitions,present_modes=dict(present_modes),allows_tearing_rows=tearing,gpu_busy_ms=summary(gpu),display_latency_ms=summary(latency))
        if len(all_shown) < 30:
            reasons.append('insufficient_target_display_samples')
        out['presentmon'].update(input_to_display_ms=summary(input_latency),present_api_ms=summary(present_wait),gpu_wait_ms=summary(gpu_wait))
        scroll=summary(displayed[1]); period=1000/refresh_hz
        if header and header.get("scenario_name") in ("scroll", "trajectory", "soak") and (not scroll or scroll["n"]<300): reasons.append("insufficient_correlated_displayed_scroll_frames")
        applicable=bool(header and header.get('scenario_name') in ('scroll','trajectory','soak'))
        misses=per_phase[1].get('visible_texture_missing',[])
        out['scroll_cache_evidence']=dict(visible_missing=summary(misses),fully_resident=bool(misses) and max(misses)==0)
        out["scroll_acceptance"]=dict(applicable=applicable,refresh_hz=refresh_hz,p95_limit_ms=period+.5,p99_limit_ms=2*period+.5,passed=bool(scroll and scroll["n"]>=300 and scroll["p95"]<=period+.5 and scroll["p99"]<=2*period+.5 and out['scroll_cache_evidence']['fully_resident'] and not reasons) if applicable else None)
    else: out["display_acceptance"]="unverified_without_presentmon"
    out["log_valid"]=not reasons
    if 'memory_stability' in out:
        idle=per_phase[7]
        reclaim_keys=['image_queued_count','image_inflight_count','image_ready_count','decode_budget_bytes','ready_budget_bytes','cache_queue_bytes','gpu_retired_bytes','cpu_retired_count','deferred_pixel_bytes']
        idle_last={k:idle[k][-1] if idle.get(k) else None for k in reclaim_keys}
        out['idle_reclamation']=dict(last=idle_last,passed=all(v==0 for v in idle_last.values()))
        out['memory_stability']['passed']=out['memory_stability']['full_30_minutes'] and out['memory_stability']['steady_window_sample_coverage'] and out['memory_stability']['threshold_passed'] and out['idle_reclamation']['passed'] and all(v['passed'] for v in out['managed_budgets'].values()) and presentmon is not None and not reasons
    return out

if __name__ == "__main__":
    p=argparse.ArgumentParser();p.add_argument("log",type=Path);p.add_argument("--output",type=Path);p.add_argument("--presentmon",type=Path);p.add_argument("--refresh-hz",type=float,default=60);a=p.parse_args()
    text=json.dumps(analyze(a.log,a.presentmon,a.refresh_hz),indent=2)
    if a.output:a.output.write_text(text,encoding="utf-8")
    else:print(text)
