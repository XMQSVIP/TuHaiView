# 性能改造记录

## 改造前基线

- 机器：Intel i5-10400，约 16 GiB 内存；G 盘 WDC WD10EZEX SATA HDD。
- `cargo test --all-targets -- --test-threads=1`：23 通过，1 个手动基准忽略。
- `cargo test --release batch_upsert_50k_completes_within_budget -- --ignored --nocapture --test-threads=1`：7.89 秒（单次，非五次中位数）。
- 现有代码没有可用的滚动、切图或内存峰值基线；不得用数据库基准代替这些指标。

## 采样

设置环境变量 `TUHAI_PERF=1` 后启动程序。后台将有界采样队列写入 exe 同目录 `data/performance-*.jsonl`；可用 `TUHAI_PERF_LOG_DIR` 将大型诊断日志放到独立测试盘。产品索引和设置仍在 EXE 同目录，正常运行默认不记录。
资源预算集中在 `src/performance.rs`，不是对进程或 GPU 驱动总内存的承诺。

## 实施记录

- 第一阶段：集中资源预算，加入异步性能记录、进程内存采样和带预览预留量的字节租约。

## 2026-09-06 性能收尾验证版

本轮实现和原始记录见 [验证记录](performance-results/20260906/README.md)；候选 EXE SHA-256 为 `B77E34E3083AEB5F4C419EFE2A613C0A28070B56687A2078BA8DC4531BCA480D`。以下新结果不覆盖后文的历史基线。

| 项目 | 本轮结果 | 结论范围 |
| --- | --- | --- |
| Release 功能回归 | 39 通过，0 失败，5 个手动测试默认忽略 | 不包含未执行的平台/满盘矩阵 |
| 24MP / 48MP JPEG 缩略图 | 五轮中位数 58.70 / 114.23 ms，较完整解码减少 77.52% / 78.47% | 固定合成样本，系统文件缓存未知 |
| 48MP 单任务进程峰值 | 完整路径 168.05 MiB，缩放路径 26.02 MiB（各五轮中位数） | Windows 进程峰值工作集，并非分配器精确归因 |
| 真实 JPEG 缓存 | 固定 1,000 张从 240,966,912 降至 15,328,052 字节，减少 93.64% | 透明图另外做无损像素回归 |
| 数据库 50k 写入 | 同一 F 盘路径交替五轮：基线 2.85 s，候选 2.93 s，退化 2.81% | 通过 ≤10% 条件；不能直接与旧单次 7.89 s 比较 |
| Windows 原生监控新增文件 | 五轮 703.9～763.3 ms，每轮访问 1 个文件 | 包含原生通知与目录服务发布；不是 UI 显示延迟 |
| GPU 上传/取消 | 分片上传、透明像素读回、取消后预算归零测试通过 | 本机 DX12 适配器 |

固定本地真实集为 50,000 张、约 21.79 GiB，抽样在达到上限后停止，原库未修改。相同跨盘集为 1,333 张、536,573,718 字节，F/G 是同一 HDD；小图集在 C/F 对比，不称为两块机械盘。16 位 PNG/TIFF 已补充固定样本与哈希。

测量脚本已拆分 `open`、`scroll`、`soak`，普通启动不启用轨迹。滚动预热起点改为首屏就绪且校验结束；每帧记录可见纹理缺失数。场景之间的显示间隔不混计；仅有正常退出码、缺少 CSV 的采集会立即停止矩阵并标无效。schema 3 的完成凭据在日志 flush/sync 后生成。

**目前仍是验证版。** 早先有效诊断（SHA `222D58A2…`）在 DWM 60 Hz 环境的显示 P95 约 32.04 ms，未满足 17.17 ms；空目录、FIFO、Mailbox 节流诊断没有证明更换默认设置有益。随后曾出现 PresentMon 退出 0 但无 CSV。03:01 的追加诊断已恢复有效 CSV：脚本在测试期间周期请求显示器和系统保持唤醒，不修改永久电源设置。本机原电源方案 10 分钟后关闭显示；恢复与唤醒请求时间一致，但尚不以此证明全部缺样根因。恢复后空目录与固定滚动路线仍约 31 ms，默认仍为 Mailbox。没有最终交付 EXE 的五轮显示验收通过记录。

长期诊断发现未解决的内存增长：B77 候选在约 28 分 58 秒因 C 盘空间保护停止，分钟 5～28 的 private bytes 斜率约 **7.59 MiB/分钟**、末段增长约 **152.30 MiB**，超过工程阈值。该轮没有完成凭据、缺少 PresentMon 且部分时间与构建重叠，属于失败线索，不能作为正式 30 分钟结果。随后两个完整 10 分钟探针使用 Rust 活跃分配统计与 wgpu 分配器报告；旧目录对象数和活跃字节会回落，但相同四分钟周期的 private bytes 仍增长。可选 mimalloc 对照没有达到足以采用的证据，产品默认分配器保持不变。

空目录持续绘制也复现增长，三分钟中位数约 501.57 → 508.08 → 512.76 MiB，Rust 活跃数据约 40 MiB、wgpu 预留 320 MiB 基本稳定。去除底层调试标签的短诊断未消除增长；表面错误日志未出现异常。以上将调查范围缩到共同绘制及原生分配路径，但没有证明某个驱动或库泄漏。测试脚本现在保留应用标准输出、表面错误、分配器配置，并区分有效显示样本与仅 CPU 更新样本。

发布仍需：完成五轮短场景和三种呈现设置对比、真实输入延迟、解决内存增长并完成 SSD/HDD 各五轮 30 分钟最终二进制验收、冷系统缓存窗口、隔离测试卷真实写满和暂时权限故障、干净 Windows 10/11 单 EXE 验证。脚本已提供，缺项不会自动判通过。

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
