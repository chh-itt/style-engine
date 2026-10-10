# style-engine-vello

[`style-engine`](https://crates.io/crates/style-engine) DisplayList 的
Vello (wgpu) **GPU 绘制 Sink**。

## 何时选用

- 目标平台有可用 GPU 适配器，需要 GPU 批渲染吞吐与 vello 渲染质量；
- 文本经 parley（与引擎测量同源折行），DisplayList 全 20 `PaintOp` 消费。

## 用法

```rust
use style_engine::StyleEngine;
use style_engine_vello::render;

let mut engine: StyleEngine<u64> = StyleEngine::new();
engine.set_stylesheet("div { width: 64px; height: 64px; background-color: #f00 }");
engine.insert(None, 1, Default::default())?;

let frame = engine.frame((64.0, 64.0), 1.0, 0.0);
let scene = render(&frame.paint); // vello::Scene → 宿主 wgpu 管线消费
```

增量路径：`render_ops` / `render_ops_with_text`（后者经 `VelloTextSystem`
落字形——`render`/`render_ops` 为无文本变体，遇 `Text` op 跳过）。
离屏像素回读：`render_offscreen`（无可用适配器时返回 `Ok(None)`，
跳过语义由调用方决定）。

## 能力边界（B 级在案）

偏差分级与三 sink 对照见 [`docs/SINK-MATRIX.md`](../../docs/SINK-MATRIX.md)
偏差矩阵：

- 非 opacity 滤镜链 / `BackdropFilter`：warn-once 降级（上游 vello
  filter 本体 / backdrop 原语未落地）；
- `PlusLighter` / `PlusDarker`：回退 `Mix::Normal`（上游 peniko Mix 缺
  两模式）；
- box-shadow / text-shadow 模糊：多重同心环近似（非真高斯）；
- conic 渐变 start>360° 相位与径向椭圆 rx/ry：待上游对质。

## 同族 crate

- [`style-engine-soft`](https://crates.io/crates/style-engine-soft)：
  零依赖纯 CPU 参照（确定性真值）；
- [`style-engine-tiny`](https://crates.io/crates/style-engine-tiny)：
  tiny-skia 纯 CPU 发布 sink（嵌入式 / 无 GPU 目标）。
