# ADR-0039: 相对字体关键字（bolder/lighter 与 larger/smaller）

日期：2026-10-08（P9-1b / P9-1c）
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

---

# 附（P9-1c）：相对字号 larger / smaller

## 规范语义（css-fonts-4 `<<relative-size>>`，原文核实）

> If the parent element has a keyword font size in the absolute size
> keyword mapping table, `larger` **may** compute the font size to the
> next entry in the table, and `smaller` **may** compute the font size
> to the previous entry… Instead of using next and previous items…
> User agents **may** instead use a simple ratio… should be around
> 1.2–1.5.

注意与 §2.2.1 权重表的区别：字号相对关键字是 **may 语气**（UA 策略
空间），无强制图表；两种合法实现并存于真实引擎。

## 决策

### D6 —— 双策略：表步进优先，1.2 比例回退

`relative_font_size(parent_px, larger) -> f32`：父字号（已解析 px）
恰为 `ABSOLUTE_FONT_SIZES_PX` 表值（ε=1e-4）时步进一格（端点钳制：
xx-small 更小/xxx-large 更大不变）；非表值按 1.2 放大/缩小。与
Chromium 行为对齐；比例取规范建议下限 1.2（确定性、可测试）。

### D7 —— 绝对字号表升为公共单源

解析期内联的 px 表（9/10/13/16/18/24/32/48）升为
`pub const ABSOLUTE_FONT_SIZES_PX: [f32; 8]`（名表
`ABSOLUTE_FONT_SIZE_NAMES` 同下标对应），`parse_font_size` 与
`relative_font_size` 共用——消除「表改一处漏一处」漂移面
（`font_size_number_and_keyword_still_absolute` 锁物化值不漂移）。

### D8 —— 物化挂点与字重同型

`compute_node_from_cascade` 步骤 4b（em→px 步骤之后、字重步骤 5 之前）：
`DeclValue::RelativeFontSize(bool)` → 父 `font_size_px()`（根以初始
16px 为基）终结为 `Len(Px)`。计算值恒绝对 px，em/rem 解析、taffy、
Text 通道零感知；继承链每层重复解析。

### D9 —— UA 表同步：small/big 绝对关键字 → smaller/larger

`builtins.rs` 的 `small { font-size: small }` / `big { font-size:
large }` 改为规范语义 `smaller`/`larger`。行为变化：16px（medium）父
下结果不变（small 13/large 18，与旧绝对关键字同值）；**非 medium 表
值父下开始随父缩放**（如 13px 父下 small=10 而非旧 13；非表值父按
1.2 比例）。ADR-0033 登记的该 B 级偏差消除。

## 测试

- `relative_font_size_table_steps_and_ratio_fallback`：表步进/端点
  钳制/1.2 比例三态。
- `larger_smaller_parse_and_materialize_chain`：根 16→18→24→32 链、
  smaller 链 16→13→10、非表值父 100px→120。
- `font_size_number_and_keyword_still_absolute`：绝对关键字物化不
  漂移 + 垃圾值拒绝。
- `ua_small_big_relative_font_size`：UA 语义（16 父下 13/18）+
  author 绝对值覆盖 + 覆盖值非表值时的比例回退。

## 影响

- property.rs：`DeclValue::RelativeFontSize(bool)` +
  `parse_font_size` 三段文法（relative→absolute→length-percentage）+
  `relative_font_size` + 双公共常量。
- computed.rs：步骤 4b 物化。
- builtins.rs：UA small/big 改写 + 头部偏差注释更新。
- 测试 +4（688→692）。
- 登记：FEATURES.md（UA 表条目 + 偏差核对「相对字号」条）、
  CHANGELOG [Unreleased]。
