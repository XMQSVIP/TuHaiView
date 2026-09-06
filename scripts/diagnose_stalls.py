"""QPC-local evidence around display stalls. Temporal overlap is not causation."""
import argparse
import bisect
import csv
import json
from pathlib import Path
from summarize_perf import summary
from perf_log import read_samples


def displayed_stalls(rows, pid, frequency):
    stalls=[]
    for row in rows:
        if str(pid)!=str(row.get('ProcessID')):
            continue
        modern='DisplayedTime' in row
        try:
            duration=float(row['DisplayedTime'] if modern else row['MsBetweenDisplayChange'])
        except (KeyError,ValueError,TypeError):
            continue
        if not duration>50:
            continue
        try:
            start=int(row['CPUStartQPC'])
            latency=float(row['DisplayLatency'] if modern else row['MsUntilDisplayed'])
            origin=start if modern else int(row['TimeInQPC'])
            display=origin+latency*frequency/1000
            left=display if modern else display-duration*frequency/1000
            right=display+duration*frequency/1000 if modern else display
            stalls.append(dict(cpu_start_qpc=start,display_start_qpc=left,display_end_qpc=right,interval_ms=duration))
        except (KeyError,ValueError,TypeError):
            stalls.append(dict(interval_ms=duration,correlation_error='missing_or_invalid_QPC_or_display_latency'))
    return stalls


def diagnose(metadata):
    run=json.loads(metadata.read_text(encoding='utf-8-sig'))
    frequency=run.get('dwm',{}).get('qpc_frequency')
    if not frequency:
        raise ValueError('QPC frequency missing; cannot infer milliseconds from ticks')
    with Path(run['presentmon']).open(encoding='utf-8-sig',newline='') as source:
        stalls=displayed_stalls(csv.DictReader(source),run['pid'],frequency)
    # Cover queued work before the interval as well; keep this explicitly diagnostic.
    windows=sorted((s['display_start_qpc']-frequency*.25,s['display_end_qpc']+frequency*.05,i) for i,s in enumerate(stalls) if 'correlation_error' not in s)
    by_frame={};render_metrics={};frames=[];stage_names=[]
    relevant={'ui_update_ms','ui_events_ms','upload_submit_ms','upload_bytes','gpu_poll_ms','gpu_reclaim_ms','gpu_reclaim_count','texture_unregister_ms','cpu_retired_count','cpu_retired_estimated_bytes','gpu_retired_bytes','texture_retired_count','eframe_cpu_ms'}
    with Path(run['logs'][0]).open(encoding='utf-8-sig') as source:
        for sample in read_samples(source):
            if sample.get('kind')=='run_header':
                stage_names=sample.get('render_stage_names',[]);continue
            name=sample.get('name');qpc=sample.get('qpc',0)
            if name=='frame_interval_ms':frames.append((qpc,sample.get('scenario',8)))
            render=sample.get('render')
            if render:
                for stage,value in zip(stage_names,render['stages_ms']):render_metrics.setdefault(stage,[]).append(value)
            if not sample.get('frame_known',False):continue
            # Previous-frame CPU arrives one frame later: associate by stable ID.
            fid=sample.get('frame_id',0)
            if render or name=='frame_interval_ms' or name in relevant:
                frame=by_frame.setdefault(fid,dict(frame_id=fid,scenario=sample.get('scenario',8),samples={}))
                if name=='frame_interval_ms':frame['qpc']=qpc
                if render:frame['render']=render
                elif name in relevant:frame['samples'][name]=sample.get('value')
    ordered=sorted((f for f in by_frame.values() if 'qpc' in f),key=lambda f:f['qpc'])
    times=[f['qpc'] for f in ordered]
    frames.sort();phase_times=[f[0] for f in frames]
    for left,right,i in windows:
        stall=stalls[i]
        index=bisect.bisect_right(phase_times,stall['cpu_start_qpc'])-1
        stall['scenario']=frames[index][1] if index>=0 else None
        selected=ordered[bisect.bisect_left(times,left):bisect.bisect_right(times,right)]
        stall['nearby_frames']=selected
        stall['largest_native_phase']=max(((name,value) for frame in selected for name,value in zip(stage_names,frame.get('render',{}).get('stages_ms',[]))),key=lambda x:x[1],default=None)
    return dict(sha256=run['sha256'],metadata=str(metadata),stage_names=stage_names,
        native_phase_metrics={k:summary(v) for k,v in render_metrics.items()},stalls=stalls,
        interpretation='QPC neighborhood: 250ms before to 50ms after each display interval; correlation only, not proof of causation. Legacy logs have no fixed frame/render context.')


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('metadata',type=Path);parser.add_argument('--output',type=Path)
    args=parser.parse_args();result=diagnose(args.metadata)
    output=args.output or args.metadata.with_name(args.metadata.stem+'-stalls.json')
    output.write_text(json.dumps(result,indent=2),encoding='utf-8')
    print(json.dumps(dict(output=str(output),stalls=len(result['stalls']),native_phase_metrics=result['native_phase_metrics'])))
