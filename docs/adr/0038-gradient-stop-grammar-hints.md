# ADR-0038: 渐变停点文法补齐与均布单源（P9-1a）

日期: 2026-10（P9「1.0 行为对齐」批第 1 项）

## 状态

已接受（落地见 CHANGELOG「1.0 对齐 — 渐变停点文法补齐与双 sink 均布统一」）。

## 背景

深度差距分析（P9 立项依据）列出渐变管线四项缺口：

1. **文法缺口**：css-images-3 色彩提示（`red, 50%, blue`——`<color>` 与
   `<length-percentage>` 混排，位置可在色前）整条 IACVT；css-images-4
   双位置停点（`red 10% 90%`）不支持。
2. **soft 均布缺陷**：`stop_positions` 前向填充把中段无位停点塌缩到
   前一停位（red,yellow,blue → 0/0/1，红带消失、整条黄→蓝）；vello
   `distribute_stops` 是正确的 NaN 段均布——同语义两套实现且其一有错。
   golden 用例全用显式位置，缺陷被遮蔽。
3. **防御分支相反**：停点色非 Absolute 时 soft 视作全透明黑、vello 视作
   不透明黑。引擎契约下（绘制发射前 `resolve_color` 已把每个停点色终结
   为 Absolute，仅 border-image 与背景 tile 两发射点）该分支不可达，但
   「不可达路径两 sink 行为相反」违反 parity 政策第 1 条的一致性基线。
4. **serde 缺口**：`GradientDump` 不承载提示（新字段引入前不存在）。

## 决策

- **D1 文法（引擎侧 desugar）**：`parse_gradient_stops` 重写为任意序
  收集（每项 lp≤2 + color≤1）：(Some lp, 0 color)=无位停点、
  (Some, 1)=停点、(Some, 2)=双位置 desugar 为同色两停点、(None, 1)=
  色彩提示（`GradientHint{after_stop: stops.len(), position}`）、
  (None, ≥2)/(Some, ≥3)=语法错；首停点前提示（stops 空）、尾随提示、
  停点 <2 整条拒绝。双位置在解析期 desugar——下游（过渡 lerp、serde、
  双 sink）无需知晓双位置概念。
- **D2 提示载体**：`Gradient` 增 `hints: Vec<GradientHint>`；
  `PaintOp::Gradient` 与 `GradientDump`/`GradientHintDump`（serde
  `#[serde(default)]` 向后兼容旧 dump）同链透传。过渡不可及渐变
  （lerp_decl 无该臂），提示不涉 lerp。
- **D3 均布单源**：核心 `css::property::distribute_stop_positions(&[Option<f32>])`
  ——首 NaN→0、末 NaN→1、NaN 段按 span=j−i+1 邻点间均布、显式逆序
  按 css-images-3 §4.5.2 抬升。单位归一（Px/线长、Percent、其余=None）
  留在 sink——line_len 是 sink 绘制期几何。vello 删本地重复实现，
  soft 删有错的旧实现，双 sink 同源。
- **D4 提示展开=sink 侧合成停点**：核心 `apply_gradient_hints(&[(f32,
  AlphaColor<Srgb>)], &[(usize, f32)])` 把提示展开为「位置=提示点
  （clamp 到前后停位区间内保单调）、色=前后停点色中点」的合成停点。
  该形与 css-images-3 提示语义**精确等价**（提示即中点色线性内插控制点），
  非 B 级近似。索引 0 / ≥len 的提示（解析期已拒绝，防御）忽略。
- **D5 无上下文单位**：em/rem/cq 提示位置 sink 侧无字体/容器上下文，
  与 em/rem/cq 停点同约定整体丢弃（提示不展开=该段线性回退），
  B·豁免在案（SINK-MATRIX 渐变行）。Px 提示按线长归一、Percent 直取。
- **D6 防御分支统一**：非 Absolute 停点色两 sink 统一为**不透明黑**
  （取 vello 现行为；透明黑会让「漏终结」静默消失，不透明黑在视觉上
  显著、利于暴露契约破坏）。
- **D7 采样表重构（soft）**：`stop_at`（每像素重算位置+线性扫停点）
  退役，改为一次 `build_stop_table`（停点+提示展开）+ 像素闭包
  `sample_table` 查表插值；repeating 周期取表首末（提示为内部点，
  不改首末，周期语义与停点周期一致）。

## 后果

- 正向：css-images-3 停点文法完整（提示/任意序）+ css-images-4 双位置；
  soft 渐变像素正确性修复（多停点无位场景）；双 sink 渐变管线单一
  事实源（均布/提示均在核心，sink 只做单位归一）；parity 矩阵新增
  渐变三行（均布/提示/防御分支）全一致。
- 负向/边界：`Gradient`/`GradientDump`/两 sink 测试构造点需同步新字段
  （编译器强制）；em/rem/cq 提示丢弃维持 B·豁免（重估条件=fontique
  字体度量经 fontprobe 进 sink 或引擎侧预折叠）。
- 否决替代：解析期把提示直接物化为停点（丢「提示」结构信息，serde
  往返不可逆，且双位置 desugar 与提示物化叠加会放大停点数）；引擎侧
  预折叠提示为停点（同上，且需绘制期 line_len 前置到级联期——不存在）。

## 登记

- FEATURES.md T4「渐变」条（P9-1a 停点文法补齐段）。
- SINK-MATRIX.md 偏差矩阵渐变三行。
- 锁定测试：crates/style-engine/tests/css_images.rs（hint 解析/任意序/
  双位置/四拒绝/DisplayList 透传/核心均布/提示中点色）；soft
  linear_gradient_unpositioned_stops_distribute_evenly（均布修复像素锁）、
  linear_gradient_color_hint_bends_interpolation（提示弯曲插值像素锁）、
  gradient_non_absolute_stop_color_is_opaque_black（防御分支锁）；vello
  gradient_hint_expands_stop_table（展开表+em 提示丢弃）。
