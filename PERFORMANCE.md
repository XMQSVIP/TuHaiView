# 性能改造记录

## 改造前基线

- 机器：Intel i5-10400，约 16 GiB 内存；G 盘 WDC WD10EZEX SATA HDD。
- `cargo test --all-targets -- --test-threads=1`：23 通过，1 个手动基准忽略。
- `cargo test --release batch_upsert_50k_completes_within_budget -- --ignored --nocapture --test-threads=1`：7.89 秒（单次，非五次中位数）。
- 现有代码没有可用的滚动、切图或内存峰值基线；不得用数据库基准代替这些指标。

## 采样

设置环境变量 `TUHAI_PERF=1` 后启动程序。后台将有界采样队列写入 exe 同目录 `data/performance-*.jsonl`；正常运行默认不记录。
资源预算集中在 `src/performance.rs`，不是对进程或 GPU 驱动总内存的承诺。

## 实施记录

- 第一阶段：集中资源预算，加入异步性能记录、进程内存采样和带预览预留量的字节租约。
