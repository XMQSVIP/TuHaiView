"""Aggregate five-run groups without hiding individual invalid or failing runs."""
import argparse
import json
from pathlib import Path
from summarize_perf import analyze, summary
from validate_ui_run import validate_result


def collect(directory):
    runs=[]
    for path in sorted(directory.glob('*-run.json')):
        run=json.loads(path.read_text(encoding='utf-8-sig'))
        errors=[]
        if run.get('exit_code')!=0 or run.get('timed_out') or run.get('abort_reason'): errors.append('process_failed')
        if run.get('presentmon_exit')!=0: errors.append('capture_failed_or_missing')
        if not run.get('dataset_manifest'): errors.append('missing_dataset_manifest')
        hz=None
        dwm=run.get('dwm') or {}
        if dwm.get('hresult')=='0x0' and dwm.get('refresh_d') and dwm.get('refresh_n'):
            hz=dwm['refresh_n']/dwm['refresh_d']
        if not hz: errors.append('unknown_refresh_rate')
        logs=run.get('logs',[])
        result=None
        if len(logs)==1 and Path(logs[0]).is_file():
            csv=Path(run.get('presentmon','missing'))
            if not csv.is_file(): errors.append('missing_presentmon_csv')
            result=analyze(Path(logs[0]),csv if csv.is_file() else None,hz or 60)
            errors.extend(validate_result(run,result))
            if run.get('scenario')=='open' and run.get('dataset_manifest'):
                for metric in ['first_records_ms','first_thumbnail_ms','first_screen_ms']:
                    if metric not in result['metrics']: errors.append('missing_'+metric)
            # Failed frame timing is a valid measurement, separate from an invalid capture.
            (directory/(path.stem.replace('-run','-summary')+'.json')).write_text(json.dumps(result,indent=2),encoding='utf-8')
        else: errors.append('missing_or_multiple_logs')
        runs.append(dict(metadata=run,errors=errors,result=result))
    hashes={r['metadata']['sha256'] for r in runs}
    configurations={json.dumps({k:r['metadata'].get(k) for k in
        ['root','scenario','present','repaint_ms','timer_ms','dataset_manifest','display','dwm','allocator_diagnostics','wgpu_environment']},sort_keys=True) for r in runs}
    # Gauge sample counts depend on runtime length; compare the actual values,
    # not those counts or the desktop's maximum resolution.
    viewports={json.dumps({k:{s:r['result']['metrics'].get(k,{}).get(s) for s in ['median','maximum']}
        for k in ['window_width','window_height','pixels_per_point']},sort_keys=True)
        for r in runs if r['result']}
    comparable=len(configurations)==1 and len(viewports)==1
    identities=[r['result'].get('header',{}).get('run_id') or r['metadata'].get('logs',[''])[0]
        for r in runs if r['result'] and r['result'].get('header')]
    unique_runs=len(identities)==len(runs) and len(set(identities))==len(runs)
    counts={k:[] for k in ['first_records_ms','first_thumbnail_ms','first_screen_ms','startup_first_records_ms','startup_first_thumbnail_ms','startup_first_screen_ms','ui_update_ms']}
    displayed=[]
    for r in runs:
        # Keep invalid rounds in runs with their errors; exclude them from the
        # across-run statistics used to assess performance.
        if r['result'] and not r['errors']:
            for k in counts:
                if stat:=r['result']['metrics'].get(k): counts[k].append(stat['median'])
            stat=r['result'].get('presentmon',{}).get('displayed_by_phase',{}).get('1')
            if stat: displayed.append(stat)
    return dict(run_count=len(runs),same_binary=len(hashes)==1,same_configuration=comparable,unique_runs=unique_runs,hashes=sorted(hashes),
        valid_five_runs=len(runs)==5 and len(hashes)==1 and comparable and unique_runs and all(not r['errors'] for r in runs),
        scroll_passed_all_five=len(runs)==5 and len(hashes)==1 and comparable and unique_runs and all(not r['errors'] and r['result'].get('scroll_acceptance',{}).get('passed') is True for r in runs),
        memory_passed_all_five=len(runs)==5 and len(hashes)==1 and comparable and unique_runs and all(not r['errors'] and r['result'].get('memory_stability',{}).get('passed') is True for r in runs),
        displayed_per_run=displayed,median_metrics_across_runs={k:summary(v) for k,v in counts.items()},runs=runs)


if __name__=='__main__':
    p=argparse.ArgumentParser(); p.add_argument('directory',type=Path); p.add_argument('--output',type=Path); a=p.parse_args()
    result=collect(a.directory)
    output=a.output or a.directory/'aggregate.json'
    output.write_text(json.dumps(result,indent=2),encoding='utf-8')
    print(json.dumps({k:v for k,v in result.items() if k!='runs'},indent=2))
