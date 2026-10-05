# ADR-0016: C2 文本栈三属性——text-transform 自实现 + word-break/overflow-wrap parley 原生映射

日期：C2 批次实施前
状态：已定案

## 背景

31 项缺口清单 C2 = `text-transform` + `overflow-wrap`/`word-break`。引擎文本栈 = parley 0.11.1（fontique 零系统字体枚举，ADR-0001/0006）；折行由 parley `break_all_lines(max_advance)` 承担（engine 测量与 vello sink 各自构建 parley Layout，语义一致性由双端同参数保证）。

## 调研结论（parley 0.11.1 / parlance 0.1.0 源码实证）

- parlance-0.1.0/src/text.rs：`WordBreak` = Normal/BreakAll/KeepAll；`OverflowWrap` = Normal/Anywhere/BreakWord——与 CSS 属性值一一对应。
- parley-0.11.1：`StyleProperty::WordBreak(..)` 与 `StyleProperty::OverflowWrap(..)` 均为可推样式（resolve/mod.rs :166/:167）；断行器原生消费两者（line_break.rs :684）；analysis/mod.rs :58-72 按 WordBreak 强度选 icu_segmenter LineBreakWordOption（BreakAll/KeepAll 原生段器）。

## 决策

1. **word-break/overflow-wrap = parley 原生映射，零自研断行**。引擎 `TextSystem::build_layout` 与 vello sink 的 ranged builder 均在非缺省时 push_default 两样式；`PaintOp::Text` 新增 `word_break`/`overflow_wrap` 平铺字段供 sink 重建同参排版（测量与绘制断行一致性契约与 white-space 通道同构）。
2. **text-transform = 引擎自实现**（parley 无此能力）。值族 `TextTransformKind` = None/Uppercase/Lowercase/Capitalize/FullWidth/FullSizeKana；前五实作（full-width = latin/标点→U+FF01-FF5E、空格→U+3000），**full-size-kana = T2**（解析接受、渲染不变换，FEATURES 偏差条在案）。
3. **变换注入点 = 度量与绘制的共同上游**：`measure_rich`/`measure_min_content` 入口（`measure` 委托 measure_rich）+ `paint.rs` PaintOp::Text 构造点（op.text 携带变换后文本 → sink 零改动）。树内 `node.text` 恒存原文（唯一真源），变换幂等消费。
4. **span 级语义 = 分段变换**：按 span 边界切段，每段用管辖样式（span 覆盖样式或基样式）的 text_transform；变换函数逐字符执行并记录 old→new 字节映射（ß→SS 等扩缩安全），span 偏移经映射重写。capitalize 词界 ≈ 非字母数字分隔符后首个字母（spec 为 UAX#29 词界，偏差在案）。
5. **继承**：text-transform 继承；overflow-wrap/word-break 不继承（spec）。
6. **属性注册**：`PropertyId` 新增 `TextTransform`(127)/`OverflowWrap`(128)/`WordBreak`(129)，动画描述符槽平移至 130-136，SLOT_COUNT = 137；`overflow-wrap` 的 legacy 别名 `word-wrap` 在 from_css_name 补表路由。

## 0.x 破坏性

- `StyleNode` 不变；`ComputedStyle` 新增 `text_transform()`/`overflow_wrap()`/`word_break()` 访问器。
- `PropertyId` 新增 3 变体；SLOT_COUNT 134 → 137（槽位存储 Vec 长度随动）。
- `PaintOp::Text` 新增 `word_break`/`overflow_wrap` 字段（DisplayList 消费者需补全构造）。

## 备选与取舍

- 自研断行器（拒绝）：icu_segmenter 的 BreakAll/KeepAll 语义 parley 已原生正确；自研必然重蹈 unicode 细节。
- ZWSP 注入 shim（拒绝）：污染 run 结构与 bidi，且 min-content 语义无法正确表达 anywhere/break-word 差异。
- 仅在 sink 变换（拒绝）：测量与绘制必须消费同一变换文本，双注入点共享同一实现为最小正确面。
