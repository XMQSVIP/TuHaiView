# 图海速览

Windows 10/11 x64 上运行的高性能图片浏览与整理工具，面向包含上万张图片的文件夹。

![图海速览界面截图](assets/tuhai-view-screenshot.png)

- 作者：大都督
- 微信：xmqsvip
- 版本号：20260814

## 当前功能

- 递归扫描 JPG/JPEG、PNG、WebP、GIF、BMP、TIFF、ICO；跳过符号链接。
- SQLite 缓存图片索引，扫描结果分批进入界面。
- 虚拟化缩略图网格，后台 2～8 个解码线程，磁盘缩略图缓存和 256MiB GPU 纹理上限。
- Ctrl/Shift 多选、Ctrl+A 全选、Esc 清除、Delete 回收站、Shift+Delete 永久删除。
- 内置沉浸式大图预览：上一张/下一张、滚轮缩放、适应窗口、100%、旋转、拖拽滚动。
- 复制/剪切到应用内选择的目标文件夹；支持覆盖、跳过、自动重命名。
- 右键在资源管理器中定位或使用系统默认程序打开。
- 扫描并复核真正为空的子文件夹后移入回收站或永久删除。
- 监控外部文件变化并自动触发防抖刷新，F5 可手动刷新。

## 构建

需要 Rust MSVC 工具链和 Windows SDK：

```powershell
rustup default stable-x86_64-pc-windows-msvc
cargo run --release
```

发布文件位于 `target\release\TuHaiView.exe`。程序不需要管理员权限。

## 开发文档

项目结构、性能设计、缓存位置、版本更新和发布流程参见 [DEVELOPMENT.md](DEVELOPMENT.md)。
