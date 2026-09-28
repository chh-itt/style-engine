# 依赖清单与版本策略

原则（2026-09 定）：选兼容的最新版本；一个 workspace 内每个 crate 只出现一个版本（One-Version Rule）；升级节奏由 vello 的 wgpu 锁定充当节拍器。版本核实日期：2026-09-11（数据源 crates.io API）。

## 决定性依赖

| Crate | 版本 | 层 | 角色 | 备注 |
|---|---|---|---|---|
| cssparser | 0.38.0 | L1 | CSS 词法/语法解析，错误恢复 | 2026-09-07 发布，MPL-2.0 |
| selectors | 0.40.0 | L1 | 选择器解析/匹配/specificity | 需实现 SelectorImpl，MPL-2.0 |
| color | 0.3.3 | L1 | CSS Color 4 颜色模型（oklch/color-mix）| linebender 出品 |
| taffy | 0.14.0 | L2 | 布局引擎 | 2026-08-24 发布 |
| slotmap | 1.1.1 | 核心 | StyleTree 与 secondary map 存储 | |
| bitflags | 2.13.2 | 核心 | StateFlags | |
| smallvec | 1.x | L1 | 声明/子选择器内联存储 | 由 cargo update 定 patch |
| peniko | 0.6.1 | 核心(词汇表) | 画笔/颜色/图片类型 | 无 GPU 依赖，vello 同款，见 ADR-0001 |
| kurbo | (peniko 传递) | 核心(词汇表) | DisplayList 几何类型 | |
| parley | 0.11.1 | L2 核心 | 文本 shaping/测量/行布局（测量内置，见 ADR-0006） | 传递引入 fontique |
| vello | 0.10.0 | sink | GPU 绘制执行器 | 2026-08-14 发布 |
| wgpu | **29.x** | sink | GPU 底座 | ⚠️ 见下"版本配对" |
| winit | =0.31.0-beta.3 | demo/harness | 窗口（仅示例与冒烟测试） | beta，精确锁版本 |
| image | 0.25.10 | 资源 | 图片解码（宿主喂字节，核心不做 IO） | |
| tracing | 0.1.x | 全部 | 警告/诊断通道 | ParseReport 同时进 tracing |
| bytemuck | 1.x | sink | vello 传递依赖（^1.25） | |

## 版本配对：wgpu 必须跟 vello 走（29，不是 30）

vello 0.10.0 的 wgpu 依赖为 `^29.0.3`（optional feature `wgpu`）。wgpu 29 与 30 是 semver 不兼容的大版本：若我们直接声明 wgpu 30.0.1，应用会同时编译两份 wgpu（或无法与 vello 统一）。因此：

- sink 与宿主统一使用 vello 传递的 wgpu 29.x，sink crate 显式声明同版本并 `pub use wgpu;`。
- 这符合"选**兼容的**最新版本"原则：29.x 就是当前兼容的最新。等 vello 升级支持 wgpu 30（linebender 节奏通常数周内跟上）后整体升级。
- winit 0.31-beta 与 wgpu 的对接走 raw-window-handle，不受此影响。

## 版本策略

- 决定性依赖（上表前 12 行）在 workspace 根 Cargo.toml 用 `[workspace.dependencies]` 统一声明，成员 crate 一律引用 workspace 版本。
- winit beta 是唯一精确锁（`=0.31.0-beta.3`）的依赖，且只出现在 demo/harness；0.31 stable 发布后立即替换。
- vello/parley/taffy 均 0.x：允许破坏性升级，但必须一次升级整条 linebender 链（vello+peniko+parley 同批），不允许混代。
- MSRV 以依赖最高者为准（当前这一代 linebender/wgpu 通常要求 Rust 1.85+，edition 2024），CI 用 cargo-hack 验证后在 README 定值。
- 不承诺 no_std；L1（值与级联）保持 no_std+alloc 可达性，作为将来选项保留。
