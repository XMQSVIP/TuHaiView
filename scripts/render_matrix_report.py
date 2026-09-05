"""Render saved, validated five-run aggregates without inferring missing passes."""
import argparse
import json
from pathlib import Path


def render(files):
    rows=[]
    for path in sorted(files):
        data=json.loads(path.read_text(encoding='utf-8-sig'))
        if 'valid_five_runs' in data: rows.append((path,data))
    lines=['# 短场景逐轮验收记录','',
        '以下仅汇总已经保存的五轮聚合报告；未列出的场景不视为通过。系统文件缓存状态未知。',
        '启动计时从 `main` 开始，首屏指标表示资源就绪；它们均不能代替实际输入到显示延迟。','',
        '| 场景 | 独立运行数 | 五轮有效 | 首批记录中位 / 最大 ms | 首屏就绪中位 / 最大 ms |',
        '| --- | ---: | --- | ---: | ---: |']
    hashes=set()
    for path,data in rows:
        hashes.update(data.get('hashes',[]))
        def metric(name):
            value=data.get('median_metrics_across_runs',{}).get(name)
            return f"{value['median']:.2f} / {value['maximum']:.2f}" if value else '—'
        lines.append(f"| [{path.stem}]({path.name}) | {data['run_count']} | {'是' if data['valid_five_runs'] else '否'} | {metric('startup_first_records_ms')} | {metric('startup_first_screen_ms')} |")
    lines += ['', '## 缓存滚动：每轮实际显示间隔', '',
        '| 场景 / 轮次 | 样本数 | 中位 ms | P95 ms | P99 ms | 最大 ms | >50 / >100 ms 次数 | 判定 |',
        '| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |']
    for path,data in rows:
        for n,run in enumerate(data.get('runs',[]),1):
            result=run.get('result') or {}
            acceptance=result.get('scroll_acceptance',{})
            if not acceptance.get('applicable'): continue
            value=result.get('presentmon',{}).get('displayed_by_phase',{}).get('1')
            if not value:
                lines.append(f'| {path.stem} / {n} | 0 | — | — | — | — | — | 无效：缺少显示样本 |')
                continue
            status='无效' if run.get('errors') else ('通过' if acceptance.get('passed') else '未通过')
            lines.append(f"| {path.stem} / {n} | {value['n']} | {value['median']:.3f} | {value['p95']:.3f} | {value['p99']:.3f} | {value['maximum']:.3f} | {value['over_50ms']} / {value['over_100ms']} | {status} |")
    lines += ['', '阈值使用各运行保存的刷新率：P95 ≤ T＋0.5 ms，P99 ≤ 2T＋0.5 ms，并要求滚动段可见纹理全部命中。',
        '完整的逐轮资源预算、GPU 时间、窗口参数和本地原始文件位置保存在链接的 JSON 中。','',
        '## 本报告中的二进制 SHA-256','']
    lines.extend(f'- `{value}`' for value in sorted(hashes))
    lines += ['', '这些短场景不覆盖 30 分钟内存矩阵、真实输入延迟、系统冷缓存、隔离卷写满或干净 Windows 10/11 兼容性。',
        '存在失败或未验证项时，产品继续作为验证版交付。','']
    return '\n'.join(lines)


if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('directory',type=Path);parser.add_argument('--output',type=Path)
    args=parser.parse_args();text=render(args.directory.glob('*.json'))
    (args.output or args.directory/'SHORT-MATRIX.md').write_text(text,encoding='utf-8')
