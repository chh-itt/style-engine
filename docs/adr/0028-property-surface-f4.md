# ADR-0028: F4 属性面补全 —— backdrop-filter 存在性 + hyphens 属性层

日期: 2026（F4 批）
状态: 已接受

## 背景

31 项缺口总盘点（goal dc428d02 收口轮）发现两项点名特性在库内零命中：

1. **backdrop-filter**：目标清单第 29 项「clip-path+mix-blend-mode+backdrop-filter」——clip-path（F3c）、mix-blend-mode（第五批㉒）均已落地，backdrop-filter 缺席。
2. **hyphens**：目标清单第 24 项「ellipsis+line-clamp、text-decoration+text-shadow、hyphens」——其余四件 F2 全落，hyphens 缺席。

## 决策

### D1 backdrop-filter —— filter 同型存在性语义位（先例：第四批④ filter / 第五批㉒ mix-blend-mode）

- **解析**：复用 `parse_sc_effect`（property.rs）——值 ≠ none 即 `DeclValue::Effect(true)`，`none` 缺席；函数块经 `parse_nested_block(skip_block_content)` 显式吞咽满足 parse_entirely 耗尽契约。零新解析代码。
- **SC 触发**：css-filters-2 规定非 none 的 backdrop-filter 创建 stacking context——paint.rs SC 谓词并入 `has_backdrop_filter()`（与 filter/clip-path/will-change/isolation/mix-blend 并列，非定位触发者进 Pos 带键 0）。
- **效果本体 = T2**（同 filter/mix-blend-mode 先例）：背景滤镜需要对已绘制内容的回采 + vello 0.10 无滤镜/图像效果管线。重估条件与 filter 条相同（vello 滤镜管线可用）。
- 不新增 `PaintOp`、不动 serde 通道。

### D2 hyphens —— 属性层落地 + 上游断词边界在案（先例：A8 direction/unicode-bidi）

- **解析**：`HyphensKind { #[default] Manual, None, Auto }`（css-text-3 §5.4，initial=manual），`keyword` helper 三关键字解析。
- **继承**：hyphens 为继承属性 → `inherits()` 白名单补 `P::Hyphens`。
- **消费边界（诚实标注，B 级在案）**：三值 v1 行为一致——
  - 显式断点（U+00AD 软连字符 / U+2010）由 parley 的 UAX 14 分段决定（软连字符现代 UCD 类 BA=其后可断，≈manual 的默认语义）；
  - `auto` 无连字词典（parley 0.11 无 hyphenation 支持，上游）；
  - `none` 无法抑制上游已注入的断点（分段机会由 parley 决定）。
  - 重估条件：parley 暴露连字/断点覆盖 API，或引擎侧软连字预处理管线。
- 槽位：ALL 尾追加 Hyphens、BackdropFilter；`slot_alignment` 不变量
  （ALL 成员槽位=ALL 序连续）要求动画描述符槽整体后移 ×2——终局=
  Hyphens=162、BackdropFilter=163、Animation* 164..170，`SLOT_COUNT
  169→171`。

### D3 兼容性

- **0.x 破坏性（加性）**：`PropertyId` +2 变体（`Hyphens`/`BackdropFilter`）、`DeclValue` +1 变体（`Hyphens(HyphensKind)`）、`ComputedStyle` +2 访问器（`hyphens()`/`has_backdrop_filter()`）、`SLOT_COUNT` 169→171、`HyphensKind` 新公开枚举。`#[non_exhaustive]` 枚举消费者不受影响。
- 与既有 `DeclValue::Effect(bool)` 完全同型复用——不引入新值族。

## 后果

- 31 项目标清单点名特性全部在场（或经用户裁决明确排除：subgrid）。
- 锁定测试：decl.rs `hyphens_parse_modes`（三关键字+非法值拒绝+initial manual）、`backdrop_filter_parse_presence`（none→缺席/值→存在）；engine.rs SC 触发表扩 backdrop-filter 行（触发组与控制组带序断言复用 filter_clip_path_trigger_stacking_context_order 骨架）。
- FEATURES.md 同步三处：T0 F4 条目、T2 filter 条扩 backdrop-filter 效果本体、MVP 偏差文本条 hyphens 边界；过时 T2 text-decoration 条（:397，F2 已落地）同步为「已落地」。
