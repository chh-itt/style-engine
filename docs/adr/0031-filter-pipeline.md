# ADR-0031：filter 本体与绘制管线（filter / backdrop-filter 本体化）

日期：2026-02（P2 批，goal-c293e543）
状态：已接受（落地记录见文末，随实施补记）

## 背景

F4 批（ADR-0028 D1）将 `filter` / `backdrop-filter` 以「宽容存在性」入库：
`parse_sc_effect` 任意非 `none` 值 → `DeclValue::Effect(true)`，值被丢弃，
仅支撑 SC 触发与绘制层「有滤镜」判定。这是暂缓决策：绘制层无滤镜效果
（vello 0.10 无 filter 原语、soft 无管线）。

最大化改进计划（goal-c293e543）P2 批将其本体化：解析保留函数表、
绘制层承接真实滤镜效果（soft 原生逐像素管线；vello 边界映射）。

依据：css-filters-1（filter 函数族、`<filter-function-list>` 文法、
函数滤镜运算域）、css-filters-2（backdrop-filter）。

## 决策

### D1：类型——`FilterFn` 函数枚举 + `DeclValue::Filters`

- 新公共枚举 `css::property::FilterFn`（`#[non_exhaustive]`）：
  `Blur(LengthPercentage)`、`Brightness(f32)`、`Contrast(f32)`、
  `Grayscale(f32)`、`Sepia(f32)`、`Saturate(f32)`、`Invert(f32)`、
  `Opacity(f32)`、`HueRotate(f32 /*deg*/)`、
  `DropShadow { dx: LengthPercentage, dy: LengthPercentage, blur: LengthPercentage, color: ColorValue }`。
  （实施修订：长度/颜色承载 `LengthPercentage` / `ColorValue` 而非终结
  f32——em/rem/calc/currentcolor 须活到计算期，与 transform 惯例一致；
  绘制域终结形态另设 `paint::FilterEffect`，见 D3。）
- 数值型函数参数：`<number> | <percentage>`（percentage / 100 归一）；
  规范钳位（grayscale/sepia/invert/opacity ∈ [0,1]、brightness/contrast/saturate ≥ 0，
  超界钳位不拒绝——css-filters-1「clamped, not invalid」；浏览器行为
  sepia(150%) → 1.0 像素锁在案）。
- `DeclValue::Filters(Vec<FilterFn>)` 替换 filter / backdrop-filter 的
  `Effect(bool)` 承载（有序函数表）。`Effect(bool)` 变体保留
  （will-change SC / isolation 仍用存在性语义）。
- `none` → `Filters(vec![])`——**有效声明覆盖语义**而非声明缺席
  （实施修订，级联正确性：`filter: none` 必须胜出并覆盖下位 origin 的
  滤镜表，缺席则下位值穿透）；空表即 none（`has_filter()` =
  `Filters` 非空）。初始值 `Filters(vec![])`。
- backdrop-filter 与 filter 同表同型（css-filters-2 引用同文法）。

### D2：严格文法取代宽容存在性（推翻 ADR-0028 D1 的宽容面）

`parse_sc_effect` 对 filter / backdrop-filter 退役，改专用严格解析：

- 文法：`none | <filter-function>+`（白空格分隔，无逗号）；
  每函数 `name(args)`，参数按上表校验；未知函数/参数非法 → 整条声明
  拒绝（ParseReport `is_clean = false`）。
- `drop-shadow` 参数序：`<length>{2,3} && <color>?`（dx dy [blur] [color]；
  color 缺省 = currentcolor 终结色）。spread 不存在（区别于 box-shadow）。
- 既有测试 `backdrop_filter_parse_presence`（宽容存在性契约）按新契约
  重写：`10px junk(` 由 is_clean 改为整条拒绝。
- A8 批教训适用：修测试期望前先判行为对错——本体化后严格解析即正确行为。

### D3：PaintOp 层对——`PushFilter` / `PopFilter` + 单点 `BackdropFilter`

- 绘制域终结形态 `paint::FilterEffect`（实施补充）：`Blur(f32 px)`、
  数值族 `f32`、`DropShadow { dx, dy, blur: f32, color: AlphaColor<Srgb> }`；
  `resolve_filter_effects(fns, &ComputedStyle, &MediaEnv)` 在发射点 px()/
  resolve_color 终结（同 Shadow 惯例）。
- `PaintOp::PushFilter { filters: Vec<FilterEffect>, x, y, width, height }`
  / `PaintOp::PopFilter`：SC 触发语义不变（`has_filter()` 仍参与 Pos/SC
  判定，paint.rs 谓词改读 `Filters` 非空）；层对包裹该元素子树（含自身
  绘制），bbox 语义与 `PushOpacity` 一致（border-box）。
- **层序（实施修订）**：transform→clip→blend→opacity→filter——层对在
  PushOpacity **之后**，合成序 `opacity(filter(子树))`
  （css-filters-1 §3：filter 作用于元素含 opacity 的整体结果；
  css-compositing-1 blend 再包于外）。收尾严格 LIFO：
  PopFilter→PopOpacity→PopBlend。三重嵌套 LIFO 像素锁在案。
- `PaintOp::BackdropFilter { filters, x, y, width, height }`：单点即时
  op——发射于节点绘制最前（transform 判定之前）；作用于主 canvas 区域
  （border-box + pad 环取样），核心区替换写回。层（PushBlend/PushOpacity
  离屏）内 backdrop 域近似为主 canvas 区域（B 级在案）。

### D4：soft sink——原生逐像素管线（全函数集真实现）

- 离屏 RGBA（预乘）渲染子树 → 函数链逐像素/整幅处理 → 合成回主画布。
- 运算域：函数滤镜在 **sRGB** 域（css-filters-1 §3：函数滤镜色域为
  sRGB；linearRGB 仅 SVG `url()` 滤镜缺省，v1 不做 url()）。
- 颜色矩阵族（grayscale / sepia / saturate / invert / hue-rotate）：
  css-filters-1 §4 固定 feColorMatrix 矩阵表（sRGB 列）逐像素 3×4 乘。
  brightness / contrast 为仿射映射（`c*amount`、`amount*(c−0.5)+0.5`）。
- `blur`：P1-4 盒模糊基建 4 通道扩展（预乘 RGB + alpha 同核三遍盒模糊，
  u32 窗口取整逐位确定），σ=blur/2。
- `drop-shadow`：离屏 alpha 遮罩 → 盒模糊 → 平移 (dx,dy) → 着色 →
  先影后源 src-over（影与源同离屏合成）。
- `opacity(n)`：c.a 缩放（预乘域 RGB 同乘）。
- 链序保持（函数表有序；invert→brightness ≠ brightness→invert，
  像素锁断言非交换性）。

### D5：vello sink——边界映射（B 级在案）

- 纯 opacity **链**（全表仅 `opacity(n)` 函数）→ alpha 连乘直映 vello
  层 alpha（实施修订：链乘超集于单函数直映）。
- 其余函数/列表：维持 SC 触发与层对 op 序，但**无像素效果**——
  vello 0.10 / peniko 无 filter 原语；warn-once 后降级为恒等层
  （保证 PopFilter 栈平衡/层对称不变式），B 级在案
  （docs/DEPENDENCIES.md 升级阶梯注记：等待 vello filter/fragment brush
  原语成熟）。
- `BackdropFilter`：vello 侧无效果（warn-once，B 级）。

### D6：序列化与 Dump

- `FilterFn` 随 DeclValue serde 往返（serde feature 自动派生）。
- `paint_dump`：`OpDump::PushFilter { fns } / PopFilter / BackdropFilter { fns }`
  对应扩展（serde 门内）。

## 边界与已知偏差

- vello：filter 像素效果缺失（除 opacity 直映）——B 级，DEPENDENCIES 在案。
- soft：`url()` SVG 滤镜引用不支持（解析拒绝，T2）；层内 backdrop 域
  近似主 canvas（B 级）。
- `drop-shadow` 双影叠加与真实高斯细微差异（盒模糊近似，P1-4 同源）。
- sub/super 偏移、hue-rotate 矩阵近似元（W3C 表为 sRGB 线性近似）——
  与 Chromium 对齐度由 conformance/像素锁逐步校准。

## 否决项

- 保留 Effect(bool) 承载 filter（存在性不足以绘制，且丢失函数表）。
- 解析期把多函数折叠为等价单函数（破坏链序非交换性）。
- vello 侧 CPU 回读模拟滤镜（违背零拷贝 GPU 管线定位，成本失控）。
- linearRGB 运算域（Chromium 函数滤镜实为 sRGB；SVG url() 才涉 linear）。

## 落地记录

P2 批实施完成（goal-c293e543 round 12–13）：

- **解析**（`css/property.rs`）：`parse_sc_effect` 退役删除，`parse_filter_value_list`
  严格文法 + 辅助（`parse_filter_fn_args` / `parse_filter_amount` /
  `parse_filter_opt_length` / `parse_filter_opt_angle` /
  `parse_filter_drop_shadow` 两序）；`url()` 经 cssparser Token::Url →
  空表 → 整条拒绝（T2 语义自然达成，无专门分支）。
- **计算**（`computed.rs`）：初始值 / `has_filter()` / `has_backdrop_filter()`
  / `filter_chain()` / `backdrop_filter_chain()`。
- **绘制**（`paint.rs`）：`FilterEffect` / `resolve_filter_effects` /
  三 PaintOp 变体 / 发射点（backdrop 最前；filter 层对在 opacity 后）。
- **soft**（`style-engine-soft`）：`filter.rs` 管线（P1-4 基建扩展）+
  `FilterLayer` 快照-清空-回合成 + `BackdropFilter` 核心区替换；
  非预乘直排 src-over（filter 全透明 → 保留快照）。
- **vello**：opacity 链直映 + warn-once 恒等层降级（零新依赖）。
- **serde**（`paint_dump.rs`）：`FilterEffectDump`（tag="fn" kebab-case）+
  OpDump 三变体；往返锁 tests/css_serde_dump.rs::filter_dump_round_trip。
- **测试**：filter_parse_strict（18 正例 + 7 拒绝例）、
  backdrop_filter_parse_strict、filter_layer_triple_nesting_lifo、
  SC 谓词扩充（层对位置/backdrop 位置/增量 2-0-1）、soft 端到端
  像素三测（invert 层 / backdrop 替换 / 透明保留快照）。
  workspace --all-features 551 测试全绿（545→551，+6）；clippy lib 零警告。
- **教训**：sepia(150%) 期望 1.5 系测试错（clamp 语义=浏览器行为）；
  像素测试索引 (y·w+x)·4 两次算错（引擎无罪，测试修正）。
