# 图海速览

Windows 10/11 x64 上运行的高性能图片浏览与整理工具，面向包含上万张图片的文件夹。

![图海速览界面截图](assets/tuhai-view-screenshot.png)

- 微信公众号：大王没有玉玺
- 版本号：20260905
- 当前开发交付：2026-09-06 快速验证版 v5，完整性能验收尚未完成，见 [实测报告](PERFORMANCE.md)。

## 关注与赞赏

| 微信公众号：大王没有玉玺 | 微信赞赏码 |
|:---:|:---:|
| <img src="assets/wechat-public-account.jpg" alt="微信公众号：大王没有玉玺" width="240"> | <img src="assets/wechat-donation-code.jpg" alt="微信赞赏码" width="240"> |

## 当前功能

- 递归扫描 JPG/JPEG、PNG、WebP、GIF、BMP、TIFF、ICO；跳过符号链接。
- SQLite 缓存图片索引，扫描结果分批进入界面。
- 虚拟化缩略图网格，预览专用线程、SSD 4 / HDD 2 个普通解码线程，以及按字节计量的资源预算。
- Ctrl/Shift 多选、Ctrl+A 全选、Esc 清除、Delete 回收站、Shift+Delete 永久删除。
- 内置沉浸式大图预览：上一张/下一张、滚轮缩放、适应窗口、预览 100%、旋转、拖拽滚动。
- 复制/剪切到应用内选择的目标文件夹；支持覆盖、跳过、自动重命名。
- 右键在资源管理器中定位或使用系统默认程序打开。
- 扫描并复核真正为空的子文件夹后移入回收站或永久删除。
- 使用文件大小与 SHA-256 查找内容完全相同的图片，可每组保留一张、删除整个重复组，或在主界面隐藏多余副本而不删除文件。
- 按路径合并目录变化，局部校验；F5 执行完整校验。

## 构建

需要 Rust MSVC 工具链、Windows SDK、CMake、Ninja 和 NASM（必须启用 JPEG SIMD）。依赖已锁定，libjpeg-turbo 与 C 运行库静态链接：

```powershell
rustup default stable-x86_64-pc-windows-msvc
# 中文工作区建议使用 ASCII 构建输出目录
$env:CARGO_TARGET_DIR='G:\tuhai-build'
cargo run --release --locked
```

上述命令的发布文件位于 `G:\tuhai-build\release\TuHaiView.exe`；未设置输出目录时为 `target\release\TuHaiView.exe`。程序不需要管理员权限。

## 开发文档

项目结构、性能设计、缓存位置、版本更新和发布流程参见 [DEVELOPMENT.md](DEVELOPMENT.md)。

## 性能与数据

预览先显示已有缩略图，再按窗口像素加载 1024 / 2048 / 4096 档；最高最长边 4096，小图不放大解码。“预览 100%”对应预览像素，界面同时标明源图和预览分辨率。

缩略图磁盘缓存默认 1 GiB，可选 1 / 2 / 4 / 8 / 16 GiB。JPEG 来源使用质量 85 的 JPEG，其他图片使用无损 WebP。旧 RGBA 缓存命中后后台迁移。数据仍保存在 EXE 同目录的 `data` 中。

实现说明、实测结果及尚未通过的验收项目见 [PERFORMANCE.md](PERFORMANCE.md)。预算约束应用管理的缓冲区，不能视为进程总内存硬上限。
