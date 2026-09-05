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
| `src/thumbnails.rs` | 缩略图优先级队列、图片解码、磁盘缓存 v2 和内存/GPU LRU。 |
| `src/file_ops.rs` | Windows STA 文件操作线程：复制、移动、回收站删除及永久删除。 |
| `src/empty_folders.rs` | 后台扫描、复核和删除空文件夹。 |
| `src/sorting.rs` | latest-wins 后台排序，避免大记录集排序阻塞界面。 |
| `src/storage.rs` | 解析 exe 同目录的数据目录与缓存目录。 |
| `src/models.rs` | 图片记录、扫描事件和服务间传输模型。 |
| `src/icon_pixels.rs`、`build.rs` | 应用图标及 Windows PE 版本资源。 |

## 性能设计

- 图片网格使用虚拟滚动，只创建视口及其附近的卡片控件；图片记录可在内存中常驻，目标规模为单根目录 5 万张以内。去重显示只缓存记录下标，不复制完整图片记录。
- 首次扫描仅采集路径、大小与修改时间；尺寸在缩略图解码或缓存命中后渐进写回，避免扫描阶段打开全部原图。
- SQLite 使用 WAL、`synchronous=NORMAL` 和批量事务。未变化文件复用已有索引，完整扫描成功后才删除过期记录。
- 查重先排除大小唯一的文件，只对候选文件流式计算 SHA-256；哈希随文件大小和修改时间缓存，未变化文件再次查重不读取原图。
- 查重完成后可启用“重复副本只显示一张”；它只过滤主网格、全选及预览导航，不修改索引或磁盘文件，目录变化时自动失效。
- 缩略图任务分为 `Preview`、`Visible`、`Prefetch` 三个有界队列；当前大图和视口任务优先，快速滚动时旧预取可被丢弃。
- 每个扫描根目录都有 generation。切换目录后旧任务在解码和写缓存前后都会失效，避免旧结果污染新目录。
- 缩略图磁盘缓存限制为 1 GiB，超过阈值后后台清理至约 800 MiB；内存/GPU 纹理按访问时间淘汰，预算为 256 MiB。
- 扫描、缩略图、监控和文件操作通过有界 channel 与唤醒回调通知 UI；空闲时不采用固定间隔持续重绘。

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
