# Feature 门禁策略

> 状态：Accepted（2026-10）。31 项缺口实施期新增可选面的统一裁决规则。

## 现状

核心 crate 三个 feature：`layout`（默认开）、`text`（默认开）、`serde`
（默认关，DisplayList/PaintOp 类型化投影）；`--no-default-features` 下仅
CSS 解析/级联/绘制编译。CI 矩阵 = no-default / layout / text /
all-features + `cargo hack --feature-powerset`（serde 落地后幂集含 serde
组合）。

## 原则

1. **CSS 核心语义不设 gate**：级联、选择器、值文法、文本排版语义是本体——
   @layer、:has()、nesting、@property、@supports、min()/clamp()、逻辑属性、伪元素、
   ellipsis 一律进核心（开了 gate 的语义等于把碎片化从渲染层搬回样式层，与
   ADR-0006 内置文本测量的同一逻辑）。
2. **gate 只为依赖与编译成本而设**：新第三方依赖、大体积数据、平台绑定才允许。
3. **gate 成对审计**：新增 gate 同批更新 hack 幂集与全组合（2^n）零警告检查。
4. **sink 实现的能力不进核心 gate**：filter/blend/backdrop 管线在
   style-engine-vello / style-engine-soft；核心只承载解析与 PaintOp 词汇。

## 决策表（31 项逐一归位）

| 特性簇 | 归属 | gate |
|---|---|---|
| cursor/user-select/pointer-events/caret-color/accent-color | 核心（槽位+访问器，不参与绘制） | 无 |
| outline / conic-gradient / object-fit / 背景全集 / 多重背景 / border-image / clip-path / mix-blend-mode / 3D | 核心（解析+PaintOp） | 无 |
| @layer / :has() / nesting / @property / @supports / @import / 多样式表 | 核心 | 无 |
| sticky / fixed / 多根 / float / grid-areas / DAG 化 / 增量布局 / hit_test | layout | 无（沿用） |
| IFC 行内流 | layout + text 联动 | 无（沿用） |
| white-space 变体 / text-decoration / text-transform / ellipsis / line-clamp / text-shadow / hyphens / span 级属性 / 字体深化 | text（no-default 下解析仍可用，消费随 text） | 无（沿用） |
| DisplayList serde | 核心 | **新 gate `serde`**（默认关；T2 重估条件已触发） |
| fuzz / proptest / 差分基座 | dev-dependencies（proptest 仅 dev，不进公共面，C4） | 无 |
| hyphens: auto 词典 | 重估时定（引入词典依赖则设 `hyphenation` gate 默认关） | 预留 |

## 约定

- 命名小写单数，与 `layout`/`text` 同风格；gate 间不互斥（幂集全编译通过）；
- 新 gate 落地时同步本文决策表与 lib.rs「依赖策略（C4）」段。
