# DisplayList 作为中立契约

本项目不做宿主集成交付物（不写 egui/iced/bevy 适配器），中立性必须由依赖方向保证而不是由口号保证。决定：绘制语义的唯一出口是 DisplayList（绘制指令的有序序列）；核心 crate 对 wgpu/vello 零依赖；crate 的帧计算是纯的（同输入同输出、幂等），状态（StyleTree 与缓存）由 crate 自有，副作用（窗口、事件、时钟、字体与图片字节）全部由宿主推入。进入核心词汇表的第三方类型仅限无 GPU 依赖的 peniko/kurbo/parley 类型（peniko 本身就是 linebender 的共享绘制词汇 crate，这正是它的设计用途）。

## Considered Options

- 内置 wgpu 直渲作为唯一绘制路径：被否，等于把执行器强加给宿主，wgpu 版本碎片化（bevy 等框架锁旧 wgpu）会让版本对不上的宿主无法接入。
- 自定义 Rust 风格 API 代替绘制指令：被否，指令列表是唯一能被任意框架转译的交换格式。

## Consequences

- DisplayList 必须 `#[non_exhaustive]` 且按"可转译、可序列化"设计：它一旦发布，每一项都是永久契约（Hyrum's Law）。
- `PaintCmd::Path` 从第一天就预留（哪怕 v0.1 不实现），避免后期破坏性扩充。
- Frame 语义：v0.1 每帧输出完整 DisplayList，但 (a) Frame 带世代号，宿主对未变更帧零成本跳过；(b) 指令按节点分段连续存放，为将来的增量补丁模式预留缝隙（v2 依性能数据决定是否启用）。
- wgpu/vello 只允许出现在独立的 sink crate 中。
- 没有第二消费者时 DisplayList 会悄悄偏向 vello 的能力边界；Conformance Harness 中的浏览器扮演"最中立的他者"来抵抗这种偏移（见 ADR-0003）。
