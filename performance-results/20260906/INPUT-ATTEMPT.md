# 输入验证边界

候选 `FAE039A3…`，原始数据位于 `F:\tuhai-validation\mesh-reuse-input`，汇总见 `mesh-reuse-input-attempt.json`。本轮为带人工工具操作的单次 `idle` 诊断，不能加入无干预的静止打开、滚动或内存矩阵。

通过 computer-use 的键盘输入触发 F5，辅助功能树显示扫描进度，随后恢复 50,000 条记录；整个运行的网格偏移保持 0。初始打开与刷新两项 action 到 UI 处理结束分别落在 0.95～6.98 ms 范围。这不包含操作系统排队和显示延迟，也不是五轮输入验收。

点击图片时工具返回 `coordinate input geometry is unavailable`；截图返回 `SetIsBorderRequired ... 0x80004002`。因此没有取得完整鼠标预览、返回定位或实际输入到显示的验证。

键盘打开文件夹选择器后，原生同步对话框等待用户操作约 126 秒。最终以 Escape 取消，产品正常退出，没有选择或修改对话框中显示的其他文件。`input_frame_processing_ms` 的最大值 125,946.20 ms 包含该模态等待；它必须解释为含用户等待的原始调用墙钟时间，不能当作 CPU 处理时间，也不能纳入无对话框的输入反馈统计。原始日志保留此值，未静默删除。

PresentMon 的输入到显示指标没有有效样本，本轮**不通过或证明输入延迟验收**。后续应把操作轨迹与等待用户选择的模态区间分别标记；在可用的桌面输入／采集环境中单独完成五轮切目录、取消、手动排序和预览操作，保留每个输入事件与首个反馈帧的关联。

后续源码已加入 `native_dialog_open`、`native_dialog_wait_ms` 和 `input_frame_wall_ms`；`input_frame_processing_ms` 扣除同一帧内原生模态等待，原始墙钟时间继续保留。自动 open/scroll/soak/trajectory 中出现模态对话框，报告将拒绝其作为有效基准。此测量修正需要新 EXE，不能追溯套用到本轮原始日志；14 项报告测试已通过。

新 EXE `4507D93F…` 的独立键盘诊断已完成，见 [modal-v4-keyboard.json](modal-v4-keyboard.json)。辅助功能树确认“选择文件夹”取得焦点，Space 打开原生对话框，Escape 取消后返回同一目录，网格偏移保持 0。该帧墙钟耗时 22,750.9485 ms，其中原生模态等待 22,749.9214 ms，扣除后的处理耗时 1.0271 ms；三项记录使用同一个 frame_id 531。日志、PresentMon、正常结束和落盘凭据有效。这验证计时修正和取消行为，不加入自动短矩阵，也不补成五轮输入到显示验收。
