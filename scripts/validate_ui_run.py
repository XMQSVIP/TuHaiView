"""Reject incomplete warmup/capture before spending time on repeated measurements."""
import argparse
import json
import sys
from pathlib import Path
from summarize_perf import analyze


def validate_result(run, result, expected_records=0, require_scan=False):
    """Shared gates for immediate validation and later five-run aggregation."""
    errors=list(result['invalid_reasons'])
    metrics=result['metrics']
    header=result.get('header') or {}
    if run.get('pid') and header.get('pid')!=run['pid']:
        errors.append('metadata_log_pid_mismatch')
    if run.get('scenario') and header.get('scenario_name') and header['scenario_name']!=run['scenario']:
        errors.append('metadata_log_scenario_mismatch')
    expected_records=expected_records or run.get('expected_records',0)
    require_scan=require_scan or run.get('require_scan_completion',False)
    count=metrics.get('catalog_displayed_records',{}).get('maximum',0)
    if expected_records and count!=expected_records: errors.append(f'catalog_count_{count}_expected_{expected_records}')
    if require_scan and not metrics.get('scan_elapsed_ms'): errors.append('full_scan_did_not_finish')
    if run.get('scenario')=='open' and expected_records and not metrics.get('first_screen_ms'):
        errors.append('first_screen_did_not_finish')
    if run.get('scenario')=='open' and metrics.get('grid_scroll_offset',{}).get('maximum',0)>0.5:
        errors.append('open_scenario_scrolled_automatically')
    if run.get('requested_seconds',0)>metrics.get('soak_completed_seconds',{}).get('maximum',0):
        errors.append('scenario_ended_before_requested_duration')
    return errors


def validate(metadata, expected_records=0, require_scan=False):
    run=json.loads(metadata.read_text(encoding='utf-8-sig'))
    errors=[]
    if run.get('exit_code')!=0 or run.get('timed_out') or run.get('abort_reason'):
        errors.append('process_did_not_complete')
    if len(run.get('logs',[]))!=1:
        return ['missing_or_multiple_logs']
    csv=Path(run.get('presentmon','missing.csv'))
    if not csv.is_file(): errors.append('missing_presentmon_csv')
    timing=run.get('dwm',{});hz=timing.get('refresh_n',60)/(timing.get('refresh_d') or 1)
    result=analyze(Path(run['logs'][0]),csv if csv.is_file() else None,hz)
    errors.extend(validate_result(run,result,expected_records,require_scan))
    # Preserve the already computed full result before the next graphics run.
    # Five-run aggregation still revalidates raw data; this is progress evidence,
    # not a cache that can silently outlive changed inputs or parser rules.
    result['immediate_validation_errors']=errors
    output=metadata.with_name(metadata.stem.replace('-run','-validated-summary')+'.json')
    output.write_text(json.dumps(result,indent=2),encoding='utf-8')
    return errors


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('metadata',type=Path)
    p.add_argument('--expected-records',type=int,default=0);p.add_argument('--require-scan',action='store_true')
    a=p.parse_args();errors=validate(a.metadata,a.expected_records,a.require_scan)
    print(json.dumps(dict(valid=not errors,errors=errors)))
    sys.exit(bool(errors))
