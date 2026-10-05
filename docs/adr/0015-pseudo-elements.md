# ADR-0015：伪元素 ::before/::after 生成内容（树实体化 + selectors 原生匹配）

- 状态：已采纳（C1）
- 关联：ADR-0013（Nesting/:has 失效模型）、ADR-0010（多根/overlay）、docs/DEPENDENCIES.md（selectors 0.40 配对）

## 背景

目标清单要求伪元素 `::before`/`::after` + `content`。selectors 0.40 的
`PseudoElement` 关联类型与 `Combinator::PseudoElement` 匹配机制已内建，
引擎侧 `PseudoElement` 枚举（Before/After）早已存在但从未接线：`SelectorParser`
未覆写 `parse_pseudo_element`（默认实现 = 拒绝），`#t::before` 此前按无效
选择器整规则丢弃；`TreeNode::match_pseudo_element` 恒 false。

## 决策

### 1. 树实体化（tree materialization），非按需虚拟节点

伪元素作为**真实树节点**挂到 origin 之下（`::before` = 首子、`::after` =
末子），由引擎 `materialize_pseudos`（frame 内、`sync_root_order` 之后、
`rebuild_taffy` 之前）统一建/清。理由：

- selectors 0.40 默认 `pseudo_element_originating_element() = parent_element()`
  （tree.rs :64-72）——右复合 `[::before]` 命中伪节点后经
  `Combinator::PseudoElement` 回到 origin 匹配左复合（`#t::before` 的
  `#t` 在 origin 上求值）。伪节点**无需身份拷贝**（name/id/classes/attrs
  全空），特异性/组合器/属性选择器/状态伪类全部走既有路径，零特判。
- 继承自然成立（伪节点树父 = origin，compute 走既有 parent 链）。
- 布局/绘制无新通路：伪叶经既有 tree→taffy 镜像与文本测量管线
  （`node.text` 载体）。

### 2. 存在策略：有伪规则 → 全宿主节点实体化

任一表含伪元素规则（`Stylesheet.has_pseudo_rules`，解析期扫描，同
`has_relative_selectors` 模式）→ 对每个宿主节点实体化 before/after 两个
裸节点；无 → 清除全部（零成本快路径）。`content: none|normal` → taffy
`Display::None`（box 不生成、不布局、不绘制），由 `ComputedStyle.pseudo`
标记驱动 `map_style`（content 属性按 spec 仅作用于伪元素，宿主节点恒
初始 Normal 不受影响）。性能优化（按 origin 级联命中懒实体化）记 B 级
待复核。

### 3. 结构伪类隔离

伪节点不参与宿主结构语义：`TreeNode::first_element_child` /
`prev_sibling_element` / `next_sibling_element` / `is_empty` 跳过伪节点
——`#t:empty`、`:first-child`、`nth-child`、兄弟组合器对宿主子节点的
计数与伪元素实体化前后**逐位不变**（spec：伪元素不影响 :empty 与结构
伪类索引）。伪节点自身不响应结构伪类与状态位（无身份无状态）。

### 4. 宿主镜像合并

宿主 `set_children` 镜像通道只含宿主键：引擎侧合并为
`[::before] + 宿主序 + [::after]`；伪节点键由引擎注册表
`pseudo_ids: BTreeMap<(NodeId, u8), NodeId>` 持有。宿主移除节点时伪节点
随子树 doomed 一起消亡（children 遍历含伪节点），注册表按 doomed origin
清理（防 slotmap 键复用悬垂，同 B3 焦点锚点教训）。

### 5. content 属性 MVP 边界

`content` = `ContentValue { None, Normal, Str(String) }`；`attr()` /
`url()` / `counter()` / `quotes` / 计数器 = T2（解析期告警拒绝，不静默
吞）。content **不继承**，初始 `Normal`。伪元素文本经 compute 后的
`sync_pseudo_text`（restyle 后、compute_layout 前）写入 `node.text` 并
按 remeasure 同式登记测量（同帧布局正确，折行由既有 T5c-2 pass 收敛）。

### 6. 解析面

`SelectorParser::parse_pseudo_element` 接受 `before`/`after`
（selectors 0.40 的 `is_css2_pseudo_element` 单冒号 legacy 路由使
`:before`/`::before` 双形自动通）；`first-line`/`first-letter` 拒绝
（目标范围仅 ::before/::after）。

## 0.x 破坏性

- `StyleNode` 新增 `pseudo` 字段（宿主构造 StyleNode 需 `..Default::default()`）。
- `ComputedStyle` 新增 `pseudo` 字段。
- `Stylesheet` 新增 `has_pseudo_rules` 字段。
- `PropertyId::Content` 追加 + `SLOT_COUNT` +1（动画槽位整体后移）。

## 后果

- 伪元素级联/继承/布局/绘制全部复用既有管线；匹配层改动集中在
  selector.rs 三方法 + 解析钩子。
- 快路径（无伪规则）零成本；慢路径（有伪规则）树规模 ×(1+2/N)，
  N=平均宿主子数，B 级在案。
- ::selection/::placeholder（C4）将复用同一实体化骨架（伪节点变体扩展）。
