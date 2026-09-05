# 性能验收：验证版，尚未完成

当前产品源码固定为 `a504605`，默认 features。EXE SHA-256：

`4507D93F1BD9C484065FA3710E1C89C59B7E7E45A7631ED4526A44597C1225EA`

本文只汇总这个候选的证据。历史测量完整保存在 [PERFORMANCE-HISTORY.md](PERFORMANCE-HISTORY.md)；不同 EXE 的通过记录不互相转记。当前仍有显示性能失败和环境缺项，不能标记“性能收尾完成”。

## 已完成的实现

- 视口不变时复用调度状态，共享图片记录，同帧新增任务批量提交；后台重绘通知合并。
- 目录快照与相应排序结果成对发布，按稳定 ID 恢复选择与预览。打开文件夹停在顶部，后台扫描和排序不触发自动滚动。
- 旧快照、排序表和过期像素在后台释放；纹理分批注销，提交完成回调后归还资源租约。增加使用中、待回收及释放统计。
- egui 原生顶点／索引上传使用 StagingBelt 复用映射缓冲，32 帧 GPU 逐像素对照包含多网格和放弃提交的情形。普通图片的解码、质量、上传预算和默认 Mailbox 保持原值。
- 性能采样与自动轨迹分离，普通启动没有自动操作。日志关联运行、场景、帧、请求和单调时钟；正常结束后记录落盘凭据。原生对话框等待与输入处理分别计时。

主要实现提交为 `81dc153`、`c0bf841`、`433cebc`、`79e3515`、`a504605`；测量与样本提交从 `1d0e71c` 开始。目录 schema v3 和缓存兼容保持不变。

## 当前 EXE 的实测状态

| 项目 | 已取得的证据 | 状态与边界 |
| --- | --- | --- |
| Release 功能回归 | 40 通过、0 失败；5 项手动测试另行执行 | 本机通过，不代表全部平台／故障矩阵 |
| 图片 GPU 分片上传、取消 | 读回像素一致，取消后租约回收 | 本机 DX12 通过 |
| 原生网格上传复用 | 32 帧新旧路径 GPU 逐像素一致，含大多网格与放弃编码器 | 本机 DX12 通过 |
| 24MP / 48MP JPEG 解码 | 五轮中位数 264.01→59.67 / 555.55→117.51 ms，减少 77.40% / 78.85% | 固定样本达到 ≥50% 目标 |
| 48MP 单任务进程峰值 | 完整／缩放路径工作集中位数 168.04 / 26.00 MiB | 各五次独立进程，系统缓存未知 |
| 1,000 张真实 JPEG 缓存 | 240,966,912→15,328,052 字节，减少 93.64% | 达到 ≥80% 目标；透明图另做无损像素回归 |
| 数据库 50k 写入 | 同一 HDD 路径交替五轮，基线 3.07 s、当前 2.97 s | −3.26%，达到退化 ≤10% 目标 |
| 原生监控小批变化 | 五轮新增发布 745.62～756.86 ms，每轮只访问 1 个文件 | 后台发布通过；不是到屏幕显示的测量 |
| 实际临时读取权限失败 | 拒绝访问时保留索引，恢复后更新；测试访问规则恢复 | 独立副本、本机通过 |
| 原生文件夹对话框取消 | 原始帧 22,750.95 ms，模态等待 22,749.92 ms，处理 1.03 ms；返回后偏移 0 | 计时修正通过；非五轮输入到显示验收 |
| 呈现设置对照 | Mailbox／8 ms 测试节流／FIFO 各五轮有效，P95 约 32 ms | 15 轮均未达到 60 Hz P95；维持 Mailbox |
| 最终 EXE 30 分钟矩阵 | SSD/HDD 各五轮，已于 06:55 启动串行执行 | 进行中，未记为通过 |
| 最终 EXE 完整短矩阵 | 合成与真实数据的 8 个磁盘／数据组合 | 待重跑；旧 EXE 的 80 轮只作历史对照 |
| 单 EXE 静态依赖 | 17,414,656 字节，x64 GUI，只发现已核对系统 DLL | 静态检查通过；干净 Windows 10/11 启动未验证 |

逐轮数据：[原生基准](performance-results/20260906/native-v4-summary.json)、[呈现对照](performance-results/20260906/PRESENTATION-V4.md)、[权限回归](performance-results/20260906/permission-v4.json)、[输入验证](performance-results/20260906/INPUT-ATTEMPT.md)、[依赖检查](performance-results/20260906/portable-v4.json)。原始输出在对应 JSON 的本地路径；真实网络图片不进入仓库或发布包。

旧上传候选 `FAE039A3…` 的单次 30 分钟 SSD 内存结果通过（斜率 0.1005 MiB／分钟，末段增长 3.94 MiB），只能作为修复线索，不能计入当前 EXE 的十轮验收。上传问题的独立对照与限制见 [MESH-UPLOAD.md](performance-results/20260906/MESH-UPLOAD.md)。

## 测量与环境

机器为 Windows 10 Pro 22H2、i5-10400、16 GiB、Intel UHD 630。C 为 Colorful CN600 NVMe；F/G 属于同一块 WDC WD10EZEX HDD，不能称为两块机械盘。测试期间不重叠构建、原生基准和图形运行，C 盘保留至少 2 GiB。系统文件缓存状态均为未知，进程重启不称为系统冷缓存。

真实集有 50,000 张、23,399,709,977 字节（49,055 JPEG、945 PNG），来自原图库多个顶层分类的有界轮流抽样，达到上限即停止。1 万张为固定子集；跨盘相同真实图共 1,333 张、536,573,718 字节。该样本不代表百万张原库的分布；没有进行原库全扫描、查重或压力测试。格式、尺寸、哈希和来源路径保存在本地清单。特殊集含超大图、渐进／CMYK JPEG、八种 EXIF、透明图、损坏图、16 位 PNG/TIFF。

PresentMon 固定为 2.5.1，按运行 PID 和 QPC 关联场景；每轮保存工具／二进制／清单哈希、磁盘、窗口、DPI、刷新率、呈现参数、缓存状态及完成凭据。首批记录、首批缩略图、首屏资源就绪、预览首次显示和目标分辨率分别统计；启动计时从 `main` 开始，不包含进入 `main` 前的系统加载。资源就绪不等于物理显示。

活动显示目标报告约 60 Hz，Windows 同时报告 `friendlyNameForced=1`、没有检测到连接显示器且 EDID 无效。独立 DXGI 垂直空白等待也约 31～32 ms；这不能证明某个驱动是根因，或证明真实屏幕刷新率。继续按 P95 ≤ T＋0.5 ms、P99 ≤ 2T＋0.5 ms 判定，不放宽目标。[显示路径证据](performance-results/20260906/DISPLAY-PATH.md) 保存详细边界。

## 资源规则与回归范围

资源预算维持解码估算 512 MiB（预览保留 128）、待上传像素 96 MiB、缓存写队列 32 MiB、图片纹理 256 MiB；普通上传每帧最多 8 张，全部上传共享 4 MiB／2 ms 限制。它们只约束应用管理的对应资源，不是进程或驱动内存硬上限。快照字节为结构估算；StagingBelt 的 256 KiB 是单块大小，不是整个池的容量上限。进程 private bytes 和资源计数需另外验收。

现有自动回归覆盖过期版本／排序、记录共享、任务去重／晋升、取消与结果饱和、后台最终析构、缓存损坏／旧格式迁移／清理 epoch／失败写入、局部新增／修改／删除／重命名／目录移动、逻辑通知溢出及 2 秒最长等待、EXIF／透明度、查重及文件操作校验。自动断言与实际环境故障分开记录，未把模拟不可写目录称为磁盘写满，未把逻辑队列溢出称为原生通知丢失压力测试。

仍缺：物理显示器和有效真实输入到显示测量、系统冷缓存重启窗口、隔离小容量卷实际写满、原生通知溢出压力测试、干净 Windows 10/11 单 EXE 启动。最终 EXE 的全部内存／短矩阵完成后也不能自动补齐这些条件。

## 复现

```powershell
$env:CARGO_TARGET_DIR='G:\tuhai-perf-build'
cargo build --release --locked
cargo test --release --locked --all-targets -- --test-threads=1
python scripts/test_perf_summary.py

./scripts/run_acceptance_matrix.ps1 -Group presentation -HddExecutable F:\qa\TuHaiView.exe -SsdExecutable C:\qa\TuHaiView.exe -OutputDirectory F:\qa-results\presentation
./scripts/run_acceptance_matrix.ps1 -Group memory-full -HddExecutable F:\qa\TuHaiView.exe -SsdExecutable C:\qa\TuHaiView.exe -OutputDirectory F:\qa-results\memory
./scripts/run_acceptance_matrix.ps1 -Group short -HddExecutable F:\qa\TuHaiView.exe -SsdExecutable C:\qa\TuHaiView.exe -OutputDirectory F:\qa-results\short
```

矩阵脚本使用已固定的本机测试集；其他机器须先准备清单匹配的数据。各命令串行运行。设置 `TUHAI_PERF=1` 才启用详细日志，自动轨迹还须明确指定根目录、场景和时长。详细用法见 [DEVELOPMENT.md](DEVELOPMENT.md)。
