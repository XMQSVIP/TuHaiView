"""Compact, hash-bound quick validation report; never substitutes for release acceptance."""
import argparse
import json
from pathlib import Path
from summarize_runs import collect
from summarize_perf import summary


def metric_units(name, value):
    if value and not name.endswith('_ms'):
        return {k:v for k,v in value.items() if k not in {'over_50ms','over_100ms'}}
    return value


def compact(item):
    run=item['metadata'];data=item.get('result') or {};metrics=data.get('metrics',{})
    errors=Path(run['app_errors']).read_text(encoding='utf-8-sig') if run.get('app_errors') else ''
    final=[]
    for line in errors.splitlines():
        if line.startswith('TUHAI_FINALIZE '):
            try:final.append(json.loads(line.split(' ',1)[1]))
            except ValueError:pass
    return dict(metadata_log=run.get('logs'),sha256=run['sha256'],run_id=data.get('header',{}).get('run_id'),
        errors=item['errors'],scenario=run['scenario'],seconds=run.get('requested_seconds'),finalize=final[-1] if final else None,
        metrics={k:metric_units(k,metrics.get(k)) for k in ['startup_first_records_ms','startup_first_thumbnail_ms','startup_first_screen_ms','ui_update_ms','input_frame_processing_ms','eframe_cpu_ms','grid_scroll_offset','render_frame_ms','process_private_bytes','process_working_set_bytes']},
        scroll=data.get('presentmon',{}).get('displayed_by_phase',{}).get('1'),
        all_displayed=data.get('presentmon',{}).get('all_displayed'),
        sample_storage=data.get('sample_storage'),scroll_passed=data.get('scroll_acceptance',{}).get('passed'),cache=data.get('scroll_cache_evidence'),
        budgets=data.get('managed_budgets'),idle_reclamation=data.get('idle_reclamation'),idle_private_bytes=metric_units('process_private_bytes',data.get('metrics_by_phase',{}).get('7',{}).get('process_private_bytes')),
        native_stages={k:v for k,v in data.get('metrics_by_phase',{}).get('1',{}).items() if k.startswith('render_')})


def report(directory):
    groups={}
    for case in ['ssd-synthetic50k','hdd-real50k']:
        for version in ['old','new']:
            for scenario in ['open','scroll']:
                key=f'{case}-{version}-{scenario}';result=collect(directory/key)
                groups[key]=dict(valid_five_runs=result['valid_five_runs'],same_configuration=result['same_configuration'],runs=[compact(r) for r in result['runs']])
        key=f'{case}-new-memory';result=collect(directory/key)
        runs=[compact(r) for r in result['runs']]
        valid=(len(runs)==3 and result['same_binary'] and result['same_configuration'] and result['unique_runs'] and all(not r['errors'] for r in runs))
        groups[key]=dict(valid_three_short_runs=valid,runs=runs,
            budgets_and_reclamation_passed=valid and all(r['idle_reclamation'] and r['idle_reclamation']['passed'] and all(b['passed'] for b in r['budgets'].values()) for r in runs))
    invalid=sorted(str(p) for p in directory.glob('*/invalid-attempts/*-run.json'))
    all_runs=[r for g in groups.values() for r in g['runs']]
    hashes={v:sorted({r['sha256'] for k,g in groups.items() if '-'+v+'-' in k for r in g['runs']}) for v in ['old','new']}
    same_binaries=all(len(v)==1 for v in hashes.values())
    correctness=bool(all_runs) and all(not r['errors'] and all(b['passed'] for b in (r['budgets'] or {}).values()) and (r['scenario']!='scroll' or r['cache'] and r['cache']['fully_resident']) for g in groups.values() for r in g['runs'])
    complete=same_binaries and correctness and all(g.get('valid_five_runs',g.get('valid_three_short_runs',False)) for g in groups.values()) and all(g.get('budgets_and_reclamation_passed',True) for g in groups.values())
    return dict(sha256_by_version=hashes,same_binaries_across_groups=same_binaries,quick_measurements_complete=complete,correctness_and_resources_passed=correctness,status='validation_only',performance_finalization_complete=False,groups=groups,invalid_attempts=invalid,
        full_duration_memory='not run for this candidate; old EXE passes do not transfer',
        limitations=['display target remains authoritative','physical display/input-to-display unverified','system cold cache unverified','clean Windows 10/11 unverified','actual disk-full and native notification overflow unverified'])


def markdown(result):
    lines=['# 快速验证版五轮对照与短循环','', '本报告绑定逐轮 EXE 哈希。三分钟循环只检验短期资源回收，不能转记长期内存通过。','',
        '| 场景 | 有效轮数 | 首批记录中位 ms | 首屏资源中位 ms | 滚动 P95 范围 ms | >50 / >100 ms |',
        '| --- | ---: | ---: | ---: | --- | --- |']
    for key,group in result['groups'].items():
        runs=[r for r in group['runs'] if not r['errors']]
        def median(name):
            values=[r['metrics'][name]['median'] for r in runs if r['metrics'][name]]
            return f"{summary(values)['median']:.2f}" if values else '—'
        scroll=[r['scroll'] for r in runs if r['scroll']]
        p95=f"{min(s['p95'] for s in scroll):.3f}–{max(s['p95'] for s in scroll):.3f}" if scroll else '—'
        stalls=f"{sum(s['over_50ms'] for s in scroll)} / {sum(s['over_100ms'] for s in scroll)}" if scroll else '—'
        lines.append(f"| {key} | {len(runs)} | {median('startup_first_records_ms')} | {median('startup_first_screen_ms')} | {p95} | {stalls} |")
    lines+=['','逐轮中位数、P95、P99、最大值、哈希、完成状态及无效尝试见同名 JSON。','',
        '保留 DX12/Mailbox、现有资源预算及打开目录停在顶部。约 32 ms 的显示周期没有达标；不归因到某个驱动，也不放宽 60 Hz 门槛。']
    lines+=['','## 二进制与有效性','',
        f"快速采集完整：{result['quick_measurements_complete']}；完整性能验收：{result['performance_finalization_complete']}。",'']
    for version,hashes in result['sha256_by_version'].items():
        lines.append(f"- {version}：`{', '.join(hashes)}`")
    for key,group in result['groups'].items():
        lines+=['',f'## {key}','',
            '显示列只含场景 1（缓存滚动），启动/预热停顿另见 final-stall-diagnosis.json；打开场景没有滚动统计。', '',
            '| 轮次 / run_id | 首批 / 首屏 ms | 显示中位 / P95 / P99 / 最大 ms | >50 / >100 ms | UI update P95 ms | 收尾 ms |',
            '| --- | --- | --- | --- | ---: | ---: |']
        for index,run in enumerate(group['runs'],1):
            def metric(name):
                value=run['metrics'].get(name)
                return f"{value['median']:.2f}" if value else '—'
            s=run['scroll'];u=run['metrics'].get('ui_update_ms');f=run['finalize']
            display=' / '.join(f"{s[n]:.3f}" for n in ['median','p95','p99','maximum']) if s else '—'
            stalls=f"{s['over_50ms']} / {s['over_100ms']}" if s else '—'
            ui=f"{u['p95']:.3f}" if u else '—'
            finish=f"{f['elapsed_ms']:.2f}" if f else '旧版无此字段'
            lines.append(f"| {index} / {run['run_id']} | {metric('startup_first_records_ms')} / {metric('startup_first_screen_ms')} | {display} | {stalls} | {ui} | {finish} |")
        if key.endswith('-memory'):
            lines+=['','| 轮次 | Private bytes 全程峰值 MiB | 结束空闲中位 MiB | 管理预算 / 回收 |',
                '| --- | ---: | ---: | --- |']
            for index,run in enumerate(group['runs'],1):
                peak=run['metrics']['process_private_bytes']['maximum']/1048576
                idle=run['idle_private_bytes']['median']/1048576
                passed=all(b['passed'] for b in run['budgets'].values()) and run['idle_reclamation']['passed']
                lines.append(f'| {index} | {peak:.2f} | {idle:.2f} | {passed} |')
    lines+=['','UI update 是应用处理墙钟时间，不能代替真实输入到显示延迟。三分钟 private bytes 不能证明长期无增长。']
    return '\n'.join(lines)+'\n'


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('directory',type=Path);p.add_argument('--output',type=Path,required=True)
    args=p.parse_args();result=report(args.directory)
    args.output.write_text(json.dumps(result,indent=2),encoding='utf-8')
    args.output.with_suffix('.md').write_text(markdown(result),encoding='utf-8')
    print(json.dumps({k:{a:b for a,b in v.items() if a!='runs'} for k,v in result['groups'].items()}))
