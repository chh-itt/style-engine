# vello + parley 渲染栈

L3 的 GPU 执行器选 vello（底座同为 wgpu，路径/AA/文本绘制免费，上限高）；文本选 parley（与 vello 同属 linebender 生态，glyph run 直通 vello，换行使用 ICU 分段、与浏览器行为对齐，blitz 已验证该组合）。vello_cpu 明确不碰（预计大改）；CPU 后端的"口子"就是 DisplayList 本身——未来的 CPU 后端只是又一个 Sink，因此现在不为它写任何代码或 trait。

## Considered Options

- 自写窄管线（圆角矩形 + 渐变 + glyph atlas，约数千行）：被否，SVG 图标无处不在，纯盒类基元上限太低。
- cosmic-text：被否，与 taffy/vello/parley 的生态一致性优先；parley 的换行分段与浏览器对齐对 Conformance Harness 有直接收益。

## Consequences

- wgpu 版本跟随 vello 锁定（vello 0.10 依赖 wgpu `^29.0.3`），不单独追新，避免应用内编译两份 wgpu；升级节奏以 vello 的 wgpu 升级为节拍器。
- vello 处于 0.x，sink crate 独立发布并锁定版本，vello/wgpu 的漂移不得波及核心 crate。
- 合成色彩空间分歧的架构级决定：合成采用 vello 原生线性混合（不做 sink 层 sRGB 校正——vello 无逐基元伽马合成开关，离屏预合成精度/性能/复杂度三输，且线性是 CSS 阵营的演进方向）；harness 以双预测接受结构性豁免（可解析的简单叠层同时计算线性与 sRGB 两种期望值，命中任一即通过，见 ADR-0003）；渐变插值空间差通过 sink 内 stop 加密（按 CSS 默认 sRGB 插值预采样中间 stop）修复到 8-bit 精度内，不降级 Class 4 零容忍。gamma 逐像素保真永不进核心，只可能是未来某个 sink 的能力。
