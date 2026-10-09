# ADR-0039: 相对字体关键字（bolder / lighter）

日期：2026-10-08（P9-1b）
状态：已接受

## 背景

`font-weight: bolder|lighter` 是 css-fonts-4 §2.2.1 的相对关键字：计算值
取决于**父元素的计算权重**，而非任何绝对值。此前引擎的
`parse_font_weight` 只接受 `normal|bold|<number 1-1000>`，bolder/lighter
整条 IACVT（无效声明丢弃→继承父值），语义完全缺席；UA 表以
`b, strong { font-weight: bold }` 记录了该偏差（ADR-0033 登记在案）。

权威语义（csswg-drafts css-fonts-4 Overview.bs §2.2.1 图表，已原文核实）：

| 父权重 w | bolder | lighter |
|---|---|---|
| w < 100 | 400 | 不变 |
| 100 ≤ w < 350 | 400 | 100 |
| 350 ≤ w < 550 | 700 | 100 |
| 550 ≤ w < 750 | 900 | 400 |
| 750 ≤ w < 900 | 900 | 700 |
| 900 ≤ w | 不变 | 700 |

## 决策

### D1 —— 文法层：新 DeclValue 变体承载相对性

`parse_font_weight` 增 `bolder|lighter` 两 ident 臂，产出
`DeclValue::RelativeFontWeight(bool)`（bolder=true）。解析期不查父——
父值在解析期不可得（级联序），相对性必须存活到级联物化期。

### D2 —— 物化挂点：compute_node_from_cascade 步骤 5

级联物化期（字号 em→px 步骤之后）发现 `RelativeFontWeight` 时，取
`parent.font_weight()`（根节点无父以初始 400 为基）经核心函数终结为
`DeclValue::Number`。要点：

- 计算值恒为绝对权重——`ComputedStyle::font_weight()` 访问器、过渡层、
  lerp、PaintOp::Text 的 font_weight 通道**零改动、零感知**；
- 继承链每层重复解析：子代继承的是父**已解析的绝对值**，再查表——
  与规范「相对关键字按父计算值再映射」一致，无限递归不可能。

### D3 —— 核心单源：`relative_font_weight(inherited, bolder) -> f32`

上表唯一实现（边界含 <  与 ≤ 的逐行区分），doc 注明图表出处；
`relative_font_weight_table_edges` 以图表逐行边界 22 断言锁定。

### D4 —— UA 表同步：bold → bolder

`builtins.rs` 的 `b, strong` 声明由 `bold` 改为 `bolder`。这是行为变化：
400 父下两者等值（绝大多数场景无感），但 `h1`（700）内的 `b` 由
bold=700（错，应不变）变为 bolder→900（对），`font-weight: 100` 父下
由 700 变为 400（对）。ADR-0033 登记的该 B 级偏差就此消除。
UA 表是数据不是行为的中立契约不变——表内容仍由宿主显式装载。

### D5 —— 拒绝垃圾值语义不变

`font-weight: 500 bolder` 等混合仍 IACVT；数字仍钳 1-1000；
`font_weight_number_still_absolute_and_rejects_garbage` 锁定。

## 否决的替代方案

- **解析期物化为绝对值**：父值解析期不可得，且 stylesheet 是可复用
  数据（同表挂不同父根结果不同）——相对性必须进计算期。否决。
- **计算值保留 RelativeFontWeight 变体、消费期再解析**：所有消费者
  （过渡/lerp/Text 通道/sink push_default）都要感知新变体，契约面
  扩大无收益。计算值=绝对权重是规范语义本身。否决。
- **UA 表维持 bold**（保守不动）：bolder 机器已就位后维持偏差没有
  论证，且 h1>b 是真实场景。否决。

## 影响

- property.rs：`DeclValue::RelativeFontWeight(bool)` 变体 +
  `parse_font_weight` 两臂 + `pub fn relative_font_weight`。
- computed.rs：compute_node_from_cascade 步骤 5 物化。
- builtins.rs：UA 表 b/strong=bolder。
- 测试 +4（685→688）：relative_font_weight_table_edges /
  bolder_lighter_parse_and_materialize_chain /
  font_weight_number_still_absolute_and_rejects_garbage /
  ua_bolder_semantics_and_author_override。
- 登记：FEATURES.md（UA 表条目 + 偏差核对新增「相对字重」条）、
  CHANGELOG [Unreleased]。

## 后续

font-size 的 `larger|smaller`（css-fonts-4 §2.2.2 相对字号）同机制
（RelativeFontSize 变体 + 级联物化 + 核心单源 + UA small/big 改写），
P9-1c 独立实施。
