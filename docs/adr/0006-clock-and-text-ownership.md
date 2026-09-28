# 时钟与文本测量的所有权

两个曾处于"未决"状态的宿主输入就此定案。Clock：宿主通过 frame(now) 传入单调绝对时间，crate 无内部时钟——绝对时间（而非 dt）保住 frame() 的幂等不变量（同输入同输出），dt 累积在暂停/恢复/跳帧下漂移，且 transition 语义（起始时刻 + 时长）本就以绝对时间表达最自然。文本测量：内置，parley 从 L3 提升为 L2 核心依赖（纯 CPU，无 GPU 依赖，不违反 ADR-0001），taffy 叶子测量闭包由引擎用 parley + 节点 ComputedStyle 的字体属性完成；宿主只喂字体字节（register_font）。非文本叶子（自绘控件等）通过可选的 LeafMeasure 钩子提供固有尺寸；feature `text` 关闭时该钩子变为必选。

## Considered Options

- 宿主实现 TextMeasure trait：被否——文本是"美观"的最大变量，外包等于把碎片化从渲染层搬到文本层，且浏览器基准的 conformance 无从谈起（宿主钩子越少，行为越统一）。
- crate 内部时钟 / dt 增量：被否，破坏幂等与可测试性。

## Consequences

- parley 与 vello 版本必须同批升级（DEPENDENCIES.md 的整批升级策略因此是强制的）。
- DisplayList 的 GlyphRun 直接携带 TextRun 引用，vello sink 零转换；文本行为在 Numeric/Pixel 两通道与浏览器基准之间保持自洽。
- 未注册任何字体的冷启动下，文本叶子测量为零尺寸并告警，不 panic。
