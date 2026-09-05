# 显示路径追加诊断

图片应用关闭后，在独立进程中调用 DXGI `IDXGIOutput::WaitForVBlank`，不创建窗口、设备或交换链，不渲染图片，不修改驱动／电源／刷新率。每轮丢弃前两次等待，再记录 180 次；五轮串行执行。

| 轮次 | 中位 ms | P95 ms | P99 ms |
| --- | ---: | ---: | ---: |
| 1 | 31.113 | 32.069 | 32.368 |
| 2 | 31.159 | 32.042 | 32.101 |
| 3 | 31.031 | 32.044 | 32.175 |
| 4 | 31.202 | 32.068 | 32.389 |
| 5 | 30.995 | 32.045 | 33.170 |

精确原始等待序列见 `dxgi-vblank*.json`，复现工具 `scripts/vblank_probe.py`。接口语义来自 [Microsoft DXGI 文档](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgioutput-waitforvblank)；实际等待时间也受 OS 调度影响，不能当作物理刷新率测量。

只读 [QueryDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-querydisplayconfig) 返回一个活动 HDMI 路径，2560×1440，目标信号约 59.99996 Hz；DWM 报告 60 Hz。详情见 `active-display-path.json`。因此继续采用约 16.667 ms 的刷新周期判定，不能把验收目标改成 32 ms。

06:36 核对目标名称标志：`target_name_flags=2`，即 `friendlyNameForced=1`。Microsoft 将其定义为强制启用目标、没有检测到连接的显示器，此时名称为空；`edidIdsValid=0` 也未提供有效 EDID 身份。见 [官方标志定义](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_target_device_name_flags)。这是 OS 报告的显示状态，不等于确认某个远程软件造成了问题。当前 PresentMon 数据描述该系统目标上的呈现行为，物理显示器的体验验收仍缺少环境，不能标为通过。

这些结果证明约 31～32 ms 的等待可以在不运行图片应用时复现，将调查范围扩展到共同的 DXGI／OS 显示和调度路径；**没有证明某个具体驱动或远程工具是根因**。Windows 安装了多个虚拟显示驱动，当前物理显示链路及远程控制状态仍需独立确认。不得因此把当前应用的显示帧间隔失败改为通过。

产品默认保持 Mailbox。改变默认呈现配置仍须同窗口、同路线的五轮显示及输入对照，不能用更高 CPU 提交帧率或单纯降低 CPU 占用替代实际显示改善。
