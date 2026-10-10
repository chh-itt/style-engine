# ADR-0043: style-engine-tiny 第三绘制 sink（tiny-skia CPU 光栅）

- 状态：已接受（2026-10，P10）
- 关联：SINK-MATRIX.md（三 sink parity 政策、偏差矩阵、tiny 合成不变量）；
  FEATURES.md（tiny 能力面与偏差分级）；CHANGELOG.md [Unreleased] P10 节；
  crates/style-engine-tiny/src/lib.rs 模块文档；
  crates/style-engine-conformance/tests/pixel_tiny.rs。
- 补记说明：本 ADR 与 P10 落地同批追溯补记（先例：ADR-0029/0030），落地
  事实以 CHANGELOG P10 节与 SINK-MATRIX.md 为准。

## 背景

1.0 验收基准（V1-SCOPE）要求绘制语义由机器验证体系锁定。此前两条绘制腿
各有局限：soft 是纯 std 参照实现（非发布定位）；vello 依赖 GPU 适配器——
无头环境不可用，windows CI 因 WARP 设备创建段错误只能环境级跳过
（STYLE_ENGINE_NO_GPU_PROBE=1）。缺一条**纯 CPU、跨平台确定、全平台必跑**
的像素腿，Pixel Channel 无法在所有环境闭合；同时嵌入式/无 GPU 宿主也
需要一个可直接发布的 CPU 绘制后端。

## 决策

- **D1 第三 sink 独立成 crate**：`style-engine-tiny` 以 tiny-skia 0.12
  （resvg 同款光栅器）+ skrifa 0.44 承载全 20 PaintOp。与 soft（真值参照）
  分立而非合并：soft 保持零第三方依赖纪律，tiny 引入 tiny-skia/parley/
  skrifa——二者定位不同（参照实现 vs 可发布 CPU sink），合并会互相
  拖累依赖面。
- **D2 滤镜/模糊/混合基建单一事实源（复用 soft）**：滤镜链
  （soft `filter::apply_effects`/effects_pad）、盒模糊 `blur_alpha_u8`、
  逐像素混合 `blend_pixel` 直接复用 soft 实现（tiny 对 soft 的 path
  依赖），防止双实现漂移。tiny-skia 缺 PlusDarker 混合模式 → 逐像素
  手写路径消费 `soft::blend_pixel`；PlusLighter 映射 tiny
  `BlendMode::Plus`。
- **D3 文本 = parley 排版 + skrifa 轮廓填充**：与 vello sink 共享 parley
  排版驱动（折行在 L2，sink 零折行）；绘制经 YFlipPen 逐字形 Path 填充。
  font_features/variations/装饰线（含 wavy）/文本影全支持。
- **D4 像素腿全平台必跑**：conformance `tests/pixel_tiny.rs` 无 GPU 探测
  逻辑、无 cfg 门控——golden 同源（Chromium 截图）、AA 预算放松与 vello
  腿同式（max_ratio × 5、下限 0.005）、Class 3–5 恒零容忍。
- **D5 与 vello 的语义分歧以 tiny/soft 标准语义为准（登记待议）**：
  conic 渐变 start>360° 相位（`rem_euclid(360°)` + Repeat 采样）与径向
  椭圆 `T(c)∘S(rx/ry)∘T(−c)`——vello 端疑似上游缺陷（SINK-MATRIX 偏差
  矩阵两行，带 golden 证据后与上游对质）；阴影 blur 三遍盒模糊为
  soft/tiny A 级同源语义，vello 环近似维持 B 级。

## 影响

- 三 sink parity 面收口：每条 PaintOp 通路三端均有实现或登记偏差；
  `check_sink_ignores.py` 扫描面扩至三 crate（tiny 源码 0 处静默忽略，
  ALLOWLIST 零条目）。
- tiny-skia 0.12 合成不变量（组遮罩尺寸 == SubPixmap、Pattern 瓦片局部
  锚定、整数 y 边界行 AA 覆盖 0）登记于 SINK-MATRIX「tiny 合成不变量」
  节，tiny 内建 `tests/compositing.rs` 锁定。
- 依赖面：tiny-skia 0.12（BSD-3-Clause，deny.toml 白名单已覆盖）、
  skrifa 0.44（parley 0.11 传递同版本，Cargo.lock 单版本）。
