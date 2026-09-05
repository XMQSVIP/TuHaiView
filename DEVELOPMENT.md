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
cargo build --release
```

生成的程序位于 `target\release\TuHaiView.exe`。

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
