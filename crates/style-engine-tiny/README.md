# style-engine-tiny

[`style-engine`](https://crates.io/crates/style-engine) DisplayList 的
tiny-skia **纯 CPU 绘制 Sink**（第三 Sink，P10，ADR-0043）。

## 何时选用

- 嵌入式 / 无 GPU 目标，需要可直接发布的 CPU 绘制后端；
- 全平台确定性像素回归：conformance Pixel Channel 的 tiny 腿无 GPU 探测
  逻辑、全平台（含 windows CI）必跑。

## 特性

- tiny-skia 0.12 光栅器（resvg 同款）+ skrifa 0.44 字形轮廓；
  文本排版与 vello sink 同源（parley，折行在 L2）；
- 全 20 `PaintOp`；阴影 / 滤镜 / 文本影 = 真 3× 盒模糊（复用
  [`style-engine-soft`](https://crates.io/crates/style-engine-soft) 的
  `blur_alpha_u8` 等单一事实源，ADR-0043 D2）；
- 混合 18/18：16 标准模式走 tiny-skia 原生管线，`PlusLighter` 映射
  `BlendMode::Plus`，`PlusDarker`（tiny-skia 缺）经 soft
  `blend_pixel` 逐像素；
- font_features / variations / 装饰线（含 wavy）/ 文本影全支持。

## 合成不变量

tiny-skia 0.12 约束（组遮罩尺寸 == SubPixmap、`Pattern` 瓦片局部锚定、
整数 y 边界行 AA 覆盖 0）登记于
[`docs/SINK-MATRIX.md`](../../docs/SINK-MATRIX.md)「tiny 合成不变量」节，
内建 `tests/compositing.rs` 锁定。能力边界与三 sink 偏差对照见同文件
偏差矩阵。
