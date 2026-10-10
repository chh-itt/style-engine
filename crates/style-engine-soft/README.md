# style-engine-soft

[`style-engine`](https://crates.io/crates/style-engine) DisplayList 的
纯软件绘制 Sink（第二 Sink）——**零 GPU、零第三方依赖**（纯 Rust 标准库
光栅化）。

## 何时选用

- CI 级像素断言的**真值参照**：无 GPU、无浮点噪声、逐字节确定可复现；
- 验证 sink 无关性契约：同一 `DisplayList` 在不同后端渲染语义一致；
- 无法引入 GPU 栈的环境。

## 特性

- 全 20 `PaintOp` 逐像素直绘：blend 18/18、filter 全函数管线、逐像素边框；
- 合成语义：sRGB 编码值直接 src-over（与 vello/Chromium 默认一致）、
  渐变停点 sRGB 插值（CSS 默认插值空间）；
- 滤镜/模糊/混合基建（`filter.rs` / `blur_alpha_u8` / `blend_pixel`）
  同时是 [`style-engine-tiny`](https://crates.io/crates/style-engine-tiny)
  的单一事实源（ADR-0043 D2），防止双实现漂移。

## 定位说明

本 crate 是契约验证与真值参照实现，非面向终端用户的绘制库；发布定位见
[`docs/BREAKING-POLICY.md`](../../docs/BREAKING-POLICY.md)，能力矩阵见
[`docs/SINK-MATRIX.md`](../../docs/SINK-MATRIX.md)。API 细节以 crate
文档（`cargo doc`）为准。
