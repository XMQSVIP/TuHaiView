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

## 2026-09-05 实施结果

核心实现已落地，**完整性能验收尚未通过，不标记为正式发布就绪**。

- `8cb13b4`：基线、异步性能日志、取消与 UI 预算。
- `43eaba6`：共享目录快照、索引排序、专用预览调度、字节租约、TurboJPEG、分帧 GPU 上传、压缩缓存、按路径增量校验。原计划阶段 2～4 的接口互相依赖，本次合并为一个可构建的集成提交，没有拆成无法编译的中间状态。
- 随后修复：预取晋升、渐进 JPEG 四分量预算、目录变化合并复杂度、取消时事务回滚、GPU 尚未提交时的资源释放。测试与实验脚本独立提交。

性能常量集中于 `src/performance.rs`。默认关闭详细记录；缓存预算在工具栏选择并保存到 EXE 同目录。当前原生呈现模式为 DX12 Mailbox：本机 FIFO 模式存在约 31 ms 的固定帧间隔，改变队列深度没有改善；Mailbox 明显降低此等待。它仍不能代替 GPU 时间戳或实际呈现事件的测量。

## 固定数据与环境

- CPU：i5-10400；内存约 16 GiB；GPU：Intel UHD 630，显示设置报告约 59 Hz。
- HDD：G 盘 WDC WD10EZEX；SSD：C 盘 Colorful CN600 NVMe。
- `scripts/make_perf_fixtures.py` 生成 50,000 个独立文件（75% JPEG、25% PNG），512×384、64 种确定性图案，每个文件有不同元数据。另含 24/48/100MP JPEG、24MP 渐进 JPEG、CMYK、八种 EXIF 方向、透明 PNG、超长 PNG和损坏 JPEG。
- 测试图为本项目原创合成图，MIT 许可，不能代表真实照片的内容复杂度。10k 子集为前十个子目录；本轮主要跑了完整 50k 集合，未独立完成 10k 全矩阵。
- 完整清单位于测试目录 `manifest.json`，SHA-256 为 `ECF1DFDC5BC6997AC7527E34E57DC6E2BD7931C53B46F21208039408E9DB05C2`。特殊样本摘要见 `performance-results/fixtures.json`。
- 所有下列速度实验使用 Release。文件系统缓存未清空；“冷解码”指绕过应用缩略图缓存，不保证操作系统冷缓存。UI 五次复测保留目录数据库及已有磁盘缩略图，每次重启应用，因此 GPU 缓存从空开始。
- 机器上同时有其他构建任务，部分早期实验也与本项目编译重叠。未修改或停止其他任务。这些结果可复现当前环境表现，但不构成受控硬件下的完整性能证明。

## 已测结果

完整的每轮中位数、P95、P99、最大值、资源峰值保存在 `performance-results/*.json`。只有 5 个样本的指标按 nearest-rank 计算 P95/P99 时均等于最大值，不能当作稳定尾延迟估计。

| 项目 | 结果 | 判定 |
| --- | --- | --- |
| JPG 24MP→256 首轮五次中位数 | image 完整解码 290.47 ms → TurboJPEG 缩放 61.60 ms，减少约 79% | 合成图达到 50% 目标 |
| JPG 48MP→256 首轮五次中位数 | 582.55 ms → 123.26 ms，减少约 79% | 合成图达到目标 |
| HDD 数据库 50k 写入五次 | 5.90、5.41、5.68、5.59、5.60 s；中位数 5.60 s | 相较原单次 7.89 s 未见退化；原基线不足五次 |
| SSD 50k 首批记录，五次中位数 | 252.25 ms | 首批目标达到；不能代替首屏完成时间 |
| HDD 50k 首批记录，五次中位数 | 504.50 ms；首轮 6171.59 ms | HDD 冷页访问明显更慢 |
| SSD 缓存滚动段 P95，五轮 | 15.45、22.84、17.72、15.92、16.38 ms | 并非每轮 ≤16.7 ms，未通过 |
| HDD 缓存滚动段 P95，五轮 | 16.43、19.01、16.53、18.25、25.68 ms | 未通过 |
| UI update P95，五轮范围 | SSD 1.88～2.94 ms，HDD 1.42～2.96 ms | 仅 UI CPU 耗时，不是帧时间 |
| JPEG 缩略图容器体积 | 同批合成样本比 v2 RGBA 少约 85% | 合成图通过；真实照片待补 |
| 最新五轮进程 private bytes 峰值 | SSD 564.0 MiB，HDD 562.8 MiB | 包含驱动/运行库，不等于纹理预算 |

JPEG 对比在同一测试程序中运行旧 image 完整解码与新缩放适配层，交替执行顺序，避免固定顺序偏差。后续 SSD/HDD 对比与八方向逐像素校验亦通过，原始数字见对应 TXT。没有测得单任务原生分配的真实峰值，不能把估算租约当作该指标已经通过。

`trajectory_phase` 每 60 秒循环：0、1 为往返滚动；2 为每秒 2 次切图；3 为每秒 10 次切图；4 为快速滚动；5 为滚动并每秒切换排序。缓存滚动段取 phase 1；系统缓存状态与命中情况由日志保留。该轨迹以可重复操作为目的，不等价于人工输入响应测试。

## 长时间与正确性验证

最终 Release 功能回归：**35 通过，0 失败，3 个手动测试默认忽略**。手动运行的 GPU 读回/取消与 JPEG/特殊格式/EXIF 测试均通过；数据库手动基准已执行五次。最终自动测试输出见 `performance-results/final-regressions.txt`。

早期构建在 HDD 上连续运行约 30 分钟，记录到 1772 次秒级进程内存样本。纹理、待上传和解码租约始终没有超过预算。private bytes 峰值约 863.2 MiB，分钟中位数从约 835 MiB 缓慢升至约 860 MiB；**这不足以判定无持续增长**。该构建在日志退出前最后一批尚未 flush，因此没有 `soak_completed_seconds`；不能以缺少完成标记的这次实验宣称最终版 soak 通过。之后复用元数据快照查找表显著减少内存占用，但最终版本仍需重新完成受控 30 分钟运行。

已覆盖的自动回归：原有 schema v2/v3 迁移、文件操作、查重验证和格式规则；按单路径更新（断言只访问 1 个文件）、重命名、删除、快速取消与重新扫描；旧文件版本元数据拒绝；元数据快照共享查找表；旧筛选排序结果拒绝；结果队列满时取消释放租约；缓存损坏、旧 v2 读取、透明度、跨批 LRU 清至 80%、过期写入与不可写目标；EXIF 八方向像素对应；原生 GPU 16 MiB 图像至少四帧上传、读回完全一致、上传中途取消。

实际渲染输出通过 eframe 截图读回检查，缩略图与预览可显示；系统截图插件在本机遇到 `SetIsBorderRequired / E_NOINTERFACE`，辅助功能树仍可读取。未以缺失的桌面截图冒充验证结果。正式便携包依赖检查未发现 libjpeg-turbo 或 VC 运行库 DLL，仅保留 Windows 系统 DLL；未在干净 Windows 10/11 虚拟机上运行。

## 复现命令

```powershell
$env:CARGO_TARGET_DIR='G:\tuhai-perf-build'
cargo test --all-targets -- --test-threads=1
cargo build --release --locked

python scripts/make_perf_fixtures.py G:/tuhai-fixtures-new --count 50000
$env:TUHAI_FIXTURES='G:\tuhai-fixtures-new'
cargo test --release jpeg_scaled_comparison_and_special_formats -- --ignored --nocapture --test-threads=1
cargo test --release gpu_sliced_upload_readback_and_cancel -- --ignored --nocapture --test-threads=1

# 使用独立 EXE 目录，避免影响日常索引；脚本不清除系统缓存。
./scripts/run_ui_perf.ps1 -Executable G:/qa/TuHaiView.exe -Root G:/tuhai-fixtures-new/catalog -Runs 5 -Seconds 60
python scripts/summarize_perf.py G:/qa/data/performance-xxxx.jsonl --output report.json
```

`TUHAI_PERF_CAPTURE=1` 可在启用轨迹时导出应用自己的渲染帧；`TUHAI_PERF_PRESENT=vsync/immediate` 和 `TUHAI_PERF_LATENCY=1` 仅用于呈现诊断，普通使用无需配置。轨迹启动必须同时指定 `TUHAI_PERF=1`、`TUHAI_PERF_ROOT`、`TUHAI_PERF_SECONDS`。

## 发布前仍须完成

1. 在无并发构建的 SSD/HDD 上重跑固定轨迹，继续定位 GPU/呈现尾延迟，使滚动 P95/P99 全部满足目标；补充实际 GPU 时间戳或 PresentMon/ETW 证据。
2. 对最终二进制完成 30 分钟 soak，区分分配器高水位、wgpu 延迟回收和泄漏；当前较低内存峰值不能代替长期稳定性。
3. 补充可再分发的真实 20～100MP 照片、16-bit PNG/TIFF；扩展目录移动、权限短暂丢失、实际监控溢出、磁盘真实写满及清理中并发写入的端到端矩阵。
4. 补足 10k 数据集、冷系统文件缓存、窗口切目录/关闭与完成事件同时到达的输入延迟记录，以及干净 Windows 10/11 的单 EXE 测试。

参考接口：[TurboJPEG 缩放](https://docs.rs/turbojpeg/latest/turbojpeg/struct.Decompressor.html)、[静态源码构建](https://docs.rs/crate/turbojpeg-sys/latest)、[notify 丢失事件语义](https://docs.rs/notify/8.2.0/notify/struct.Event.html#method.need_rescan)。
