# 短场景逐轮验收记录

以下仅汇总已经保存的五轮聚合报告；未列出的场景不视为通过。系统文件缓存状态未知。
启动计时从 `main` 开始，首屏指标表示资源就绪；它们均不能代替实际输入到显示延迟。

| 场景 | 独立运行数 | 五轮有效 | 首批记录中位 / 最大 ms | 首屏就绪中位 / 最大 ms |
| --- | ---: | --- | ---: | ---: |
| [real-10k-hdd-open](real-10k-hdd-open.json) | 5 | 是 | 508.19 / 541.75 | 582.22 / 657.07 |
| [real-10k-hdd-scroll](real-10k-hdd-scroll.json) | 5 | 是 | 498.60 / 543.02 | 590.30 / 642.57 |
| [real-50k-hdd-open](real-50k-hdd-open.json) | 5 | 是 | 705.05 / 798.81 | 762.81 / 870.94 |
| [real-50k-hdd-scroll](real-50k-hdd-scroll.json) | 5 | 是 | 715.93 / 738.89 | 771.77 / 806.02 |
| [real-shared-hdd-open](real-shared-hdd-open.json) | 5 | 是 | 451.20 / 472.43 | 555.56 / 710.78 |
| [real-shared-hdd-scroll](real-shared-hdd-scroll.json) | 5 | 是 | 458.82 / 525.50 | 535.69 / 703.64 |
| [real-shared-ssd-open](real-shared-ssd-open.json) | 5 | 是 | 450.59 / 473.66 | 556.88 / 561.93 |
| [real-shared-ssd-scroll](real-shared-ssd-scroll.json) | 5 | 是 | 490.07 / 693.18 | 591.06 / 819.25 |
| [synthetic-10k-hdd-open](synthetic-10k-hdd-open.json) | 5 | 是 | 497.04 / 528.29 | 605.59 / 697.47 |
| [synthetic-10k-hdd-scroll](synthetic-10k-hdd-scroll.json) | 5 | 是 | 452.55 / 713.94 | 550.02 / 845.62 |
| [synthetic-10k-ssd-open](synthetic-10k-ssd-open.json) | 5 | 是 | 470.51 / 518.92 | 596.19 / 626.58 |
| [synthetic-10k-ssd-scroll](synthetic-10k-ssd-scroll.json) | 5 | 是 | 475.62 / 662.47 | 572.84 / 744.88 |
| [synthetic-50k-hdd-open](synthetic-50k-hdd-open.json) | 5 | 是 | 639.21 / 663.28 | 689.31 / 762.05 |
| [synthetic-50k-hdd-scroll](synthetic-50k-hdd-scroll.json) | 5 | 是 | 651.41 / 689.58 | 699.91 / 777.14 |
| [synthetic-50k-ssd-open](synthetic-50k-ssd-open.json) | 5 | 是 | 580.45 / 594.40 | 688.63 / 697.86 |
| [synthetic-50k-ssd-scroll](synthetic-50k-ssd-scroll.json) | 5 | 是 | 570.42 / 648.85 | 643.80 / 730.15 |

## 缓存滚动：每轮实际显示间隔

| 场景 / 轮次 | 样本数 | 中位 ms | P95 ms | P99 ms | 最大 ms | >50 / >100 ms 次数 | 判定 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| real-10k-hdd-scroll / 1 | 1315 | 31.108 | 31.985 | 32.126 | 32.202 | 0 / 0 | 未通过 |
| real-10k-hdd-scroll / 2 | 1316 | 31.114 | 32.033 | 32.144 | 32.473 | 0 / 0 | 未通过 |
| real-10k-hdd-scroll / 3 | 1307 | 31.105 | 32.002 | 32.128 | 32.630 | 0 / 0 | 未通过 |
| real-10k-hdd-scroll / 4 | 1310 | 31.113 | 31.999 | 32.123 | 32.627 | 0 / 0 | 未通过 |
| real-10k-hdd-scroll / 5 | 1313 | 31.096 | 32.025 | 32.160 | 32.530 | 0 / 0 | 未通过 |
| real-50k-hdd-scroll / 1 | 1276 | 31.104 | 31.993 | 32.147 | 32.713 | 0 / 0 | 未通过 |
| real-50k-hdd-scroll / 2 | 1290 | 31.126 | 31.946 | 32.145 | 32.275 | 0 / 0 | 未通过 |
| real-50k-hdd-scroll / 3 | 1279 | 31.121 | 31.991 | 32.161 | 32.280 | 0 / 0 | 未通过 |
| real-50k-hdd-scroll / 4 | 1282 | 31.123 | 31.986 | 32.139 | 32.449 | 0 / 0 | 未通过 |
| real-50k-hdd-scroll / 5 | 1282 | 31.107 | 31.970 | 32.137 | 32.246 | 0 / 0 | 未通过 |
| real-shared-hdd-scroll / 1 | 1369 | 31.026 | 32.054 | 32.136 | 62.487 | 3 / 0 | 未通过 |
| real-shared-hdd-scroll / 2 | 1368 | 31.033 | 32.059 | 32.135 | 61.773 | 1 / 0 | 未通过 |
| real-shared-hdd-scroll / 3 | 1375 | 31.030 | 32.047 | 32.112 | 32.292 | 0 / 0 | 未通过 |
| real-shared-hdd-scroll / 4 | 1369 | 30.993 | 32.037 | 32.100 | 62.047 | 2 / 0 | 未通过 |
| real-shared-hdd-scroll / 5 | 1315 | 31.083 | 32.003 | 32.185 | 62.146 | 2 / 0 | 未通过 |
| real-shared-ssd-scroll / 1 | 1319 | 31.106 | 31.992 | 32.118 | 32.559 | 0 / 0 | 未通过 |
| real-shared-ssd-scroll / 2 | 1347 | 31.014 | 32.051 | 32.144 | 63.127 | 3 / 0 | 未通过 |
| real-shared-ssd-scroll / 3 | 1312 | 31.064 | 32.041 | 32.312 | 92.417 | 11 / 0 | 未通过 |
| real-shared-ssd-scroll / 4 | 1318 | 31.096 | 32.010 | 32.167 | 62.114 | 4 / 0 | 未通过 |
| real-shared-ssd-scroll / 5 | 1314 | 31.094 | 32.000 | 32.138 | 62.511 | 1 / 0 | 未通过 |
| synthetic-10k-hdd-scroll / 1 | 1316 | 31.115 | 32.033 | 32.157 | 61.854 | 1 / 0 | 未通过 |
| synthetic-10k-hdd-scroll / 2 | 1318 | 31.101 | 31.995 | 32.135 | 32.279 | 0 / 0 | 未通过 |
| synthetic-10k-hdd-scroll / 3 | 1323 | 31.103 | 31.972 | 32.144 | 61.618 | 1 / 0 | 未通过 |
| synthetic-10k-hdd-scroll / 4 | 1322 | 31.123 | 32.012 | 32.126 | 32.519 | 0 / 0 | 未通过 |
| synthetic-10k-hdd-scroll / 5 | 1323 | 31.114 | 32.003 | 32.130 | 32.535 | 0 / 0 | 未通过 |
| synthetic-10k-ssd-scroll / 1 | 1311 | 31.055 | 31.932 | 32.147 | 62.651 | 8 / 0 | 未通过 |
| synthetic-10k-ssd-scroll / 2 | 1313 | 30.998 | 31.948 | 32.144 | 32.357 | 0 / 0 | 未通过 |
| synthetic-10k-ssd-scroll / 3 | 1321 | 31.093 | 31.977 | 32.131 | 61.684 | 1 / 0 | 未通过 |
| synthetic-10k-ssd-scroll / 4 | 1319 | 31.092 | 32.013 | 32.148 | 61.747 | 1 / 0 | 未通过 |
| synthetic-10k-ssd-scroll / 5 | 1322 | 31.070 | 31.984 | 32.185 | 32.565 | 0 / 0 | 未通过 |
| synthetic-50k-hdd-scroll / 1 | 1314 | 31.145 | 32.010 | 32.134 | 32.228 | 0 / 0 | 未通过 |
| synthetic-50k-hdd-scroll / 2 | 1322 | 31.112 | 32.025 | 32.130 | 32.450 | 0 / 0 | 未通过 |
| synthetic-50k-hdd-scroll / 3 | 1317 | 31.123 | 32.018 | 32.138 | 62.280 | 1 / 0 | 未通过 |
| synthetic-50k-hdd-scroll / 4 | 1320 | 31.106 | 32.011 | 32.155 | 61.050 | 1 / 0 | 未通过 |
| synthetic-50k-hdd-scroll / 5 | 1308 | 31.114 | 32.035 | 32.154 | 32.365 | 0 / 0 | 未通过 |
| synthetic-50k-ssd-scroll / 1 | 1310 | 31.127 | 32.017 | 32.151 | 62.684 | 2 / 0 | 未通过 |
| synthetic-50k-ssd-scroll / 2 | 1311 | 31.080 | 32.013 | 32.146 | 62.406 | 2 / 0 | 未通过 |
| synthetic-50k-ssd-scroll / 3 | 1314 | 31.113 | 32.001 | 32.156 | 61.040 | 1 / 0 | 未通过 |
| synthetic-50k-ssd-scroll / 4 | 1309 | 31.115 | 32.009 | 32.132 | 47.346 | 0 / 0 | 未通过 |
| synthetic-50k-ssd-scroll / 5 | 1311 | 31.108 | 32.033 | 32.178 | 62.670 | 2 / 0 | 未通过 |

阈值使用各运行保存的刷新率：P95 ≤ T＋0.5 ms，P99 ≤ 2T＋0.5 ms，并要求滚动段可见纹理全部命中。
完整的逐轮资源预算、GPU 时间、窗口参数和本地原始文件位置保存在链接的 JSON 中。

## 本报告中的二进制 SHA-256

- `4507D93F1BD9C484065FA3710E1C89C59B7E7E45A7631ED4526A44597C1225EA`

这些短场景不覆盖 30 分钟内存矩阵、真实输入延迟、系统冷缓存、隔离卷写满或干净 Windows 10/11 兼容性。
存在失败或未验证项时，产品继续作为验证版交付。
