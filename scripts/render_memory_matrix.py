"""Render completed five-run memory groups, preserving every per-run verdict."""
import argparse
import json
from pathlib import Path


def render(files):
    lines = ['# 最终 EXE 的 30 分钟内存矩阵', '',
             '只汇总提供的实际结果；缺轮、无效采集和任一失败均不能记为整组通过。', '',
             '| 组 / 轮次 | 时长足够 / 样本有效 | 稳态斜率 MiB/min | 末段增长 MiB | 末段回收 | 资源预算 | 内存判定 |',
             '| --- | --- | ---: | ---: | --- | --- | --- |']
    hashes = set()
    for path in files:
        group = json.loads(path.read_text(encoding='utf-8-sig'))
        hashes.update(group.get('hashes', []))
        for n, run in enumerate(group['runs'], 1):
            result = run.get('result') or {}
            stable = result.get('memory_stability', {})
            full = stable.get('full_30_minutes') and stable.get('steady_window_sample_coverage')
            budgets = result.get('managed_budgets', {})
            budget_ok = len(budgets) == 4 and all(v['passed'] for v in budgets.values())
            def number(key):
                value = stable.get(key)
                return f'{value:.4f}' if isinstance(value, (int, float)) else '缺少'
            def verdict(ok):
                return '通过' if ok else '未通过／未验证'
            valid = not run['errors'] and result.get('log_valid')
            lines.append(f'| {path.stem} / {n} | {bool(full)} / {bool(valid)} | '
                         f'{number("slope_mib_per_min")} | {number("growth_mib")} | '
                         f'{verdict(result.get("idle_reclamation", {}).get("passed"))} | '
                         f'{verdict(budget_ok)} | {verdict(valid and stable.get("passed"))} |')
        lines += ['', f'{path.stem}：实际 {group["run_count"]} 轮；五轮有效：{group["valid_five_runs"]}；'
                  f'五轮全部内存通过：{group["memory_passed_all_five"]}。', '']
    lines += ['## 判定边界', '',
              '稳态使用分钟 5～28 的 private bytes 中位数，斜率 ≤1 MiB/min；末三分钟中位数相对稳态前三分钟中位数增长 ≤32 MiB。',
              '每个稳态分钟至少 50 个样本，完成 1800 秒；末段九项待处理／回收计数归零，四项管理预算全部通过，且日志和 PresentMon 有效。',
              '这是本轮工程验收阈值，不证明绝无泄漏。内存通过不能覆盖显示间隔、实际输入或 Windows 平台兼容性。', '',
              '## 二进制 SHA-256', '']
    lines.extend(f'- `{value}`' for value in sorted(hashes))
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('groups', type=Path, nargs='+')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(render(args.groups), encoding='utf-8')
