# ADR-0035：`:has()` 失效收窄（style_dirty_roots 增量通道停止全量升级）

日期：2026-02（P6 批，goal-c293e543）
状态：已接受（落地记录见文末）

## 背景

B3 批引入 `:has()` 相对选择器（selectors 0.40 解析+匹配）时，失效
模型保守：任何 `style_dirty_roots` 增量失效遇
`any_has_rules()`（engine.rs `:815-828`，主/user/附加表
`has_relative_selectors` 汇总，stylesheet.rs `:47-50` 表级深扫标志）
即升级**全量重样式**（engine.rs `:1612-1615`）。大文档下持有
`:has()` 规则即触发性能悬崖（与是否实际命中无关）。

css-selectors-4 失效语义：`A:has(B)` 的匹配取决于 host 子树内容，
结构性/样式变更的失效传播 = dirty 节点向 host 的祖先链。

## 决策

### D1：v1 范围——单 compound host 预筛

- 仅对「`:has()` 左侧为**单一 compound、无组合器前缀**」的规则启用
  预筛（`.card:has(img)`、`div:has(> p)`、`li:has(:focus)`）——
  实际用法绝大多数；带前缀组合器的复杂选择器
  （`.a > .b:has(x)`）保持既有全量兜底（保守正确）。
- `HasHostIndex`（sheet 变更重建，时机同 `rebuild_registered_props`）：
  每条合格规则提取快筛键 `(tag: Option<String>, classes: Vec<String>,
  id: Option<String>)`（`:has` 前 compound 的类/id/类型选择器；
  通配 = 无约束键，预筛恒不否决）。

### D2：失效判定算法

`style_dirty_roots` 非空且 `any_has_rules()` 时（替换现 :1612 全量
升级）：

1. 收集全部合格规则的快筛键（无合格规则 → 维持全量，与现状一致）;
2. 对每个 dirty 节点向上遍历祖先：任一祖先**通过**某键的 compound
   级快筛（tag 相同或键无 tag；键类集 ⊆ 祖先类集；id 相同或键无
   id）→ 该键可能命中 → **升级全量**（保守）；
3. 全部 dirty 节点全部祖先被全部键否决 → 不升级，走增量 roots。

否决正确性论证：host 不匹配 compound 快筛 ⇒ 整条规则不可能以该
祖先为 host ⇒ dirty 子树变更不影响该规则命中。快筛只可能**多**
升级（false positive），不可能漏升级（false negative）——结果
保守正确。

### D3：顺带修复

- `any_container_rules()`（engine.rs `:641-647`）漏检 `user_sheet`
  ——补齐与 `any_has_rules()` 同型（任何含 @container 的 user 表
  目前被跳过，收敛环 pass 数判定可错）。
- 全量升级分支保留：无合格键/含前缀组合器 :has 规则/索引未建时
  走原路径（行为兼容位）。

### D4：不做（B 级/后续）

- `@container` 失效收窄到最近容器子树（依赖容器栈反向查询，
  独立批 P6b 候选）。
- 完整 css-selectors-4 失效传播（:has 参数选择器结构分析、
  锚点级精确失效）。
- 带组合器前缀 :has 的预筛（前缀匹配语义复杂，全量兜底）。

## 测试要点（实施时落锁）

- 单 compound :has + 无关子树失效 → 不全量（断言 restyle 范围/
  计数探针或收敛行为等价）；
- 单 compound :has + host 祖先命中 → 全量升级（结果与现状一致）;
- 前缀组合器 :has → 恒全量（兼容位）;
- 通配 host 键（`*:has(img)`）→ 不否决（保守）;
- any_container_rules 含 user_sheet @container → 收敛环 cap=3。

## 落地记录（P6 实施完成）

实施要点（goal-c293e543 round 18，全量 584 绿）：

- **D1 键提取**（selector.rs）：`HasHostKey{tag,classes,id}`（pub(crate)，
  Default=哨兵）+ `list_contains_has`（深扫 `Has`/`Is`/`Where`/`Negation`
  参数内嵌套——防函数式伪类内藏 `:has` 绕过索引造成漏升级）+
  `has_host_key`（`Selector::iter()` 首 sequence 收集 LocalName/ID/Class，
  `Has` 组件置位；`next_sequence().is_some()` = 前缀组合器 → 不合格返
  None；其余组件（属性/伪类/通配/命名空间）不进键=约束弱化，保守正确）。
  tag 匹配与匹配器 `has_local_name` 同为大小写敏感 `==`（无 false
  negative）。
- **D1 索引**（engine.rs）：`has_host_index: Vec<HasHostKey>`；
  `rebuild_has_host_index` 挂全部 5 个 `rebuild_font_faces` 调用点
  （attach/set_stylesheet/user·ua 装载/清除路径）；规则级语义=深扫有
  `:has` 但零合格键或任一选择器不合格 → 追加哨兵键（全空=恒通过）。
- **D2 判定**：`has_invalidation_needs_full`——`any_has_rules()` 为假
  → false（增量畅通）；索引空 → true（兜底）；否则 dirty 根（含自身）
  沿祖先链任一节点命中任一键 → true。frame 增量分支判定从
  `any_container_rules() || any_has_rules()` 改为
  `any_container_rules() || has_invalidation_needs_full()`。
- **D2 补充（结构失效通道）**：`remove`（非根）此前只标 dirty_struct
  不触发样式重算——B3 起 remove+`:has` 组合存在失配缺口（host 后代
  集合变化但命中不刷新）。收口：remove 前取存活父，`any_has_rules()`
  时 push 父进 style_dirty_roots（由快筛判定升级）；无 `:has` 表保持
  v1 语义（零行为变化）。
- **D3**：`any_container_rules` 补 ua_sheet/user_sheet 漏检；
  `any_has_rules` 补 ua_sheet（P5 引入 UA 表后的对称缺口）。
- **测试 +5**：单元 3（`has_host_index_keys_and_sentinel` 五类键矩阵/
  `has_invalidation_needs_full_matrix` 四 case/`container_rules_detected_
  in_user_sheet`）+ 行为锁 2（`has_narrowed_invalidation_unrelated_
  subtree` 无关子树否决走增量双正确+remove 失配捕获/
  `has_prefix_combinator_still_hits` 哨兵路径命中正常）。既有
  `has_invalidation_upgrades_to_full_restyle` 语义收窄后仍绿（断言的
  是命中颜色非路径）。
- 测试教训：`node(id, classes, name)` helper 首参是 id 第三参才是
  tag——类型选择器相对项（`:has(img)`）要求 name=Some("img")；green
  关键字=#008000（0.502 非 1.0）；曾命中后重算无冠军的槽位回落非红
  值（全量 restyle 既有语义），失配锁断言「红不在场」而非 None。
