# 图海速览开发者文档

本文档面向参与开发、测试和发布“图海速览”的开发者。

## 项目定位

图海速览是一个面向 Windows 10/11 x64 的 Rust 图片预览与整理工具。它的目标是在包含数万张图片的本地或移动磁盘目录中，仍能保持扫描、滚动、预览和批量文件操作的响应速度。

- 应用名称：图海速览
- 可执行文件：`TuHaiView.exe`
- 当前应用版本：`20260905`
- 支持格式：JPG/JPEG、PNG、WebP、GIF、BMP、TIFF、ICO
- 暂不支持：HEIC、AVIF、RAW；GIF 和 TIFF 仅显示首帧

## 开发环境

开发和发布需要 Windows 10/11、稳定版 Rust MSVC 工具链及 Windows SDK。建议使用 x64 环境。

```powershell
rustup default stable-x86_64-pc-windows-msvc
cargo fmt -- --check
cargo check --message-format=short
cargo test --all-targets -- --test-threads=1
cargo run --release
```

正式构建命令：

```powershell
cargo build --release --locked
```

生成的程序位于 `target\release\TuHaiView.exe`。

JPEG 适配层固定 `turbojpeg = 1.5.1`，通过 `turbojpeg-sys` 从源码静态构建并要求 SIMD。开发机需要 CMake、Ninja、NASM 和 MSVC 工具链；`.cargo/config.toml` 固定 vendor/static 与静态 CRT。运行单 EXE 不需要安装这些构建工具。路径含中文时，本机验证使用 `CARGO_TARGET_DIR=G:\tuhai-perf-build`。

## 性能验收工具

详细记录默认关闭。`TUHAI_PERF=1` 只打开测量；自动轨迹还必须显式指定 `TUHAI_PERF_ROOT` 和 `TUHAI_PERF_SECONDS`。`open` 场景仅打开目录，后台扫描/排序不会自动定位滚动。

- `scripts/prepare_real_fixtures.py`：分类轮流抽样，严格停止于指定数量，记录固定哈希清单。
- `scripts/prepare_comparison_subset.py`：仅从既有测试副本取最多 512 MiB，两盘保持相同文件，保留至少 2 GiB 空间。
- `scripts/run_ui_perf.ps1`：可见窗口、外置 PresentMon 2.5.1、二进制/数据/工具哈希、DWM 刷新率与逐轮报告。
- `scripts/run_acceptance_matrix.ps1`：`short`、`presentation`、`memory-short`、`memory-full`；默认每组五轮，full 每轮 30 分钟。采集失败时终止，修复后重新运行相应组。
- `scripts/run_native_benchmarks.ps1`：独立进程 JPEG 峰值、固定 JPEG 时间对比、缓存压缩、数据库交替五轮和原生监控验证。
- `scripts/summarize_perf.py` / `summarize_runs.py`：检查丢样、结束、落盘凭据、呈现数据及逐轮通过条件。`input_frame_processing_ms` 不包含操作系统事件排队时间。

测试工具的路径和本机固定测试集集中在矩阵脚本，不属于产品运行依赖。真实网络图片不进入发布包。最新进度、限制和结果见 `PERFORMANCE.md` 与 `performance-results/20260906/README.md`；当前不标记“性能收尾完成”。

## 代码结构

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 应用启动、窗口参数、版本常量和窗口图标。 |
| `src/app.rs` | egui 界面、虚拟化图片网格、选择状态、大图预览及对话框。 |
| `src/catalog.rs` | SQLite schema v3、增量扫描、目录监控、数据库同步与渐进元数据写入。 |
| `src/duplicates.rs` | 大小预筛、流式 SHA-256、哈希持久化和重复组默认保留规则。 |
| `src/thumbnails.rs` | 缩略图优先级队列、图片解码、版本任务调度、解码与待上传预算。 |
| `src/file_ops.rs` | Windows STA 文件操作线程：复制、移动、回收站删除及永久删除。 |
| `src/empty_folders.rs` | 后台扫描、复核和删除空文件夹。 |
| `src/sorting.rs` | 只保留最新请求的后台索引排序，避免大记录集排序阻塞界面。 |
| `src/storage.rs` | 解析 exe 同目录的数据目录与缓存目录。 |
| `src/models.rs` | 图片记录、扫描事件和服务间传输模型。 |
| `src/icon_pixels.rs`、`build.rs` | 应用图标及 Windows PE 版本资源。 |

## 性能设计

- `catalog_runtime.rs` 的单一后台所有者负责扫描、监控、SQL、元数据合并与共享快照。扫描分片可取消，UI 不等待 join；快照通常最多每 100 ms 发布一次。
- `CatalogSnapshot` 共享 `Arc<ImageRecord>`，提供路径、ID 查找表；排序返回下标顺序与反向位置表，并校验请求序号和数据版本。旧快照与大型排序表在后台释放。
- 当前预览使用专用工作线程；普通解码 SSD 4 / HDD 2 个，受核心数限制，无法识别时按 HDD。可见项优先于预取，按文件版本、用途与尺寸去重。
- 中间缓冲估算 512 MiB（预览保留 128 MiB），待上传像素 96 MiB，缓存写队列 32 MiB，纹理像素 256 MiB。RAII 租约随缓冲生命周期释放；原生解码器内部只能协作取消。
- JPEG 使用独立 `turbojpeg` 适配层进行缩放解码；不启用旧 image 集成。其他格式一次完整解码，再用面积采样缩小。资源不足有独立状态，不标记为损坏。
- `gpu_images.rs` 使用 egui 注册的原生 wgpu 纹理，按行分帧上传，完整上传后才替换预览。所有上传共用 4 MiB / 2 ms 提交预算，普通图每帧最多 8 张。CPU 提交时间与 GPU 完成时间不同。
- 目录监控保持整个根目录会话，合并路径，700 ms 防抖、2 s 最长等待；溢出或丢失事件回退完整校验。遍历失败保留索引。
- `thumbnail_cache.rs` 是压缩缓存和独立 SQLite 清单的后台所有者；访问时间合并更新，淘汰按 LRU 分批清至 80%。缓存清理有独立 epoch，旧写入不会重新落盘。
- 去重扫描共享记录，后台按大小筛选、流式 SHA-256；删除前复核移至文件操作线程，保留原有确认流程。文件操作报告的源与目标路径进入相同的增量校验流程。
- 默认不记录性能；设置 `TUHAI_PERF=1` 后输出有界异步 JSONL。复现实验与限制见 [PERFORMANCE.md](PERFORMANCE.md)。

### 原生构建

安装 CMake、Ninja、NASM 并加入 PATH。`.cargo/config.toml` 固定源码静态构建、Ninja 和 MSVC 静态 CRT；debug 下 turbojpeg-sys 使用非 debug CRT，避免混合 CRT。NASM 缺失时 SIMD 构建直接失败，不静默降速。

中文工作区的 Visual Studio/NASM 自定义步骤曾在本机挂起；使用 Ninja 和 ASCII 输出路径：

```powershell
$env:CARGO_TARGET_DIR='G:\tuhai-build'
cargo build --release --locked
```

Windows 10/11 的系统 DLL 仍是运行前提，无需附带 JPEG 或 VC 运行库 DLL。

## 本地数据布局

程序会将可变数据保存在 exe 所在目录的 `data` 文件夹，而不是固定写入 C 盘：

```text
TuHaiView.exe
data/
  catalog.sqlite3
  cache/
    thumbnails/
```

应用内可一键清理数据库和缩略图缓存。运行程序前手动删除 `data` 也可以重建索引，但会丢失缓存并导致下次打开重新扫描。

## 文件操作与安全

- 常规删除默认进入 Windows 回收站；永久删除必须经过不可恢复确认。
- 批量复制、移动前会检查同名冲突，支持覆盖、跳过和自动重命名；文件名比较遵循 Windows 的大小写不敏感语义。
- 空文件夹扫描把隐藏文件和系统文件都视为内容；根目录不会被列入候选项。确认删除前会再次确认该目录仍为空。
- Windows Shell 文件操作运行在专用 STA 线程，避免直接阻塞 UI 线程。

## 版本更新清单

每次发布版本时，请同步更新以下位置：

1. `src/main.rs` 的 `APP_VERSION` 与 `APP_WINDOW_TITLE`。
2. `build.rs` 的 `ProductVersion` 与 `FileVersion`。
3. `README.md` 的版本号。
4. 本文档的“当前应用版本”（如版本发生变化）。

更新后重新执行 `cargo build --release`，以使 exe 的窗口标题和 Windows 文件属性保持一致。

## 发布流程

1. 在干净工作区执行测试和发布构建：

   ```powershell
   cargo fmt -- --check
   cargo test --all-targets -- --test-threads=1
   cargo build --release
   ```

2. 将 `target\release\TuHaiView.exe` 打包为 zip，例如：

   ```powershell
   Compress-Archive -Path target\release\TuHaiView.exe -DestinationPath TuHaiView-20260905-win-x64.zip
   Get-FileHash TuHaiView-20260905-win-x64.zip -Algorithm SHA256
   ```

3. 在 GitHub Releases 创建标签，例如 `v20260905`，上传 zip 和 SHA-256 值，并说明支持的平台、版本内容和已知限制。

4. 将源码、`Cargo.lock`、README 和开发文档提交并推送。不要提交构建输出、运行数据或缓存。

## Git 约定

仓库的 `.gitignore` 已排除 `/target/`、`/data/`、SQLite 数据库、WAL 文件和日志。提交前建议检查：

```powershell
git status --short
git diff --check
```

只提交源码、资源、文档和可复现构建所需的 `Cargo.lock`；不要把用户图片、缓存数据库或本地发布包提交到仓库。
# 追加的内存与显示诊断（2026-09-06）

- `cargo build --release --locked --features heap-diagnostics` 可计量 Rust 活跃分配；诊断时另外传 `-AllocatorDiagnostics`，每五秒记录一次，并记录 wgpu 活跃/预留分配。计量不包含 C 库、驱动和系统堆。
- `mimalloc` 是默认关闭的对照实验 feature，没有作为产品优化采用。对照结果见 `performance-results/20260906/*probe.json`。
- `run_ui_perf.ps1` 将新版本日志存到输出目录；旧 EXE 仍从自身 data 目录收集。只在基准期间调用 `SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)` 重置显示和系统空闲计时，不修改电源方案。没有显示样本的运行无效。
- `-TimerMs 1` 是进程内计时器分辨率诊断，退出时配对恢复；正常启动不会启用。参考 [Windows API](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod)。
- `summarize_memory_probe.py` 用流式读取生成分钟汇总和相同四分钟周期差值，排除首轮和末段空闲。它不会把诊断结果转换为完整验收通过。
- `validate_ui_run.py` 在每次采集后验证日志、目标进程显示样本以及可选的记录数量/全扫描完成条件。矩阵先完成索引，再预热路线，防止 90 秒中止在 2.7 万条的扫描被当作已缓存 5 万张基线。`open` 场景还检查滚动偏移始终为零。
- `first_records_ms` 等指标从打开目录开始；`startup_first_records_ms` 等指标从 `main` 初始化单调时钟开始，包含窗口和 GPU 初始化。它仍不包括进入 `main` 前的系统加载耗时，真实输入到显示另行采集。
- 新增真实后台缓存回归覆盖旧 v2 迁移、清理中写入、完成清理后旧任务延迟回写、清理后新写入、缓存配置从 2 GiB 调回 1 GiB。使用独立临时目录。
- `examples/dx12_memory_probe.rs` 是单独的最小渲染复现程序，不进入产品 EXE。它只绘制固定英文控件、记录进程内存和 wgpu 分配，不读图库。构建：`cargo build --release --locked --example dx12_memory_probe`；设置 `TUHAI_PROBE_LOG` 指定日志，`TUHAI_PROBE_SECONDS=180` 指定时间。可用 `TUHAI_PROBE_EXTRA_POLL=1` 对照应用的额外 poll、`TUHAI_PROBE_REPAINT_MS=16` 对照节流、`TUHAI_PROBE_FIFO=1` 对照 FIFO。不得与正式 UI 性能运行同时构建或运行。
- 最小渲染探针通过 `scripts/run_renderer_probe.ps1` 串行运行；默认增加 30 秒结束空闲期。`-NoWidgets` 只提交空白画面，`-Warp` 仅在此诊断程序使用软件 DX12 适配器。构建加 `--features wgpu/counters` 才会启用原生对象计数；未启用时的零值不能解释为没有原生对象。脚本与汇总均标为诊断，不作为产品验收。
- 逐轮元数据保存期望记录数、是否要求完整扫描与请求时长；即时检查和五轮汇总共用相同约束。内存验收要求至少 1800 秒、稳态每分钟至少 50 个内存样本；无效轮次不进入跨轮性能统计。
- egui-wgpu 0.31.1 使用本地固定补丁复用原生网格上传缓冲，来源、许可证及单独 GPU 回归命令见 `vendor/egui-wgpu/TUHAI-PATCH.md`。修改后同时运行产品分片图片上传回归和该网格逐像素回归。StagingBelt 块尺寸不是原生缓冲池总预算，不能将图片纹理租约当作进程上限。
- 正式图形采集脚本拒绝与已运行的产品或最小渲染探针重叠；长内存场景也要求出现完整 50,000 条记录，防止只有空目录或部分索引的循环获得通过。
- 人工输入诊断中，同步原生文件夹对话框的等待时间可能落在 `input_frame_processing_ms` 和 `ui_update_ms` 的墙钟区间内。保留原始数据并单独标注模态区间；不能把它解释为 CPU 处理时间，也不能混入无干预的基准矩阵。实际输入到显示缺样仍标为未验证。
- 新增 `native_dialog_open`／`native_dialog_wait_ms` 标记该模态区间，`input_frame_wall_ms` 保留包含等待的原始时间，随后构建的 `input_frame_processing_ms` 扣除同一帧内等待。旧日志不会自动获得修正值。自动基准出现原生模态区间直接判无效；这些仍是墙钟区间，不是线程 CPU 采样。
- `display_paths.py` 只读查询当前活动显示路径和目标信号频率；`vblank_probe.py` 在独立进程中测量 DXGI 垂直空白等待，不创建渲染窗口，不改配置。后者必须在正式图形测试之外运行，也不等价于物理输入到光子的仪器测量。
- `run_permission_regression.ps1` 只在新建 UUID 测试目录操作合成 JPEG 副本，短暂添加仅该目录的读取拒绝规则，并在 `finally` 恢复原 DACL；OS 未实际拒绝访问时不得判通过。先索引、拒绝访问仍保留索引、恢复后更新版本分别由 `verify_permission_stage.py` 验证。它不使用原图片目录进行文件或权限修改。
