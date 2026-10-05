# ADR-0018：::selection / ::placeholder 伪元素（C4，css-pseudo-4）

状态：已接受（C4 批）

## 背景与定位

`::selection`（选区文本样式）与 `::placeholder`（输入占位文本样式）是 **非盒生成伪元素**：不产生盒子、不进树实体化（区别于 C1/ADR-0015 的 ::before/::after 树实体化模型）。A体B度定位：选区状态与占位文本存在性归宿主（零副作用原则），引擎职责 = 解析 + 匹配 + 级联，暴露**已解析样式通道**供宿主读取。

语义基准（Chrome / css-pseudo-4）：
- `::selection` 继承自 originating element；生效属性子集 = color / background-color / text-decoration(-line/-color/-style) / text-shadow / caret-color（MVP 全量解析计算，子集以文档记录偏差在案——宿主自行取舍）。
- `::placeholder` 同为 origin 继承；Chrome 子集 = color / font 系 / opacity / letter-spacing / line-height / text-transform / background-color / text-decoration。
- 单冒号 `:selection` 不合法（非 CSS2 legacy 集，selectors 0.40 的 `is_css2_pseudo_element` 路由不含二者 → 单冒号形自然拒绝，spec 一致）。

## 决定

### D1：解析面（selector.rs）

`PseudoElement` 枚举 +`Selection` / `Placeholder` 两变体（non_exhaustive 保持）；`ToCss` +`"::selection"` / `"::placeholder"` 两臂；`parse_pseudo_element` +两臂（`name.eq_ignore_ascii_case`）。

### D2：匹配语义（selector.rs）

`match_pseudo_element`：Before/After 保持现形（`node.pseudo == Some(which)`）；**Selection/Placeholder 返回 `node.pseudo == None`**（origin 节点直配；实体化伪节点 pseudo==Some 恒 false）。收益：`match_specificity` / `match_rules` 原样可用（选择器尾伪元素无需截断匹配）。

**主级联防泄漏**：collect_sheet（cascade.rs，私有）双模过滤——`channel = None`（主级联）排除含 Selection/Placeholder 伪元素分量的规则，`Some(w)`（通道级联）仅收对应通道规则；`match_rules`（pub）语义零变更（唯一调用点 = collect_sheet）。主样式永不吸收通道声明（::selection{color:red} 不改变元素正常文本色）。

**D2 补遗（实现期，探针实证）**：`Element::pseudo_element_originating_element` 覆写——C1 实体化伪节点维持默认父链（宿主节点）；origin 直配（::selection/::placeholder 主题复合命中后 PseudoElement 组合器回溯）返回节点自身（前缀复合在 origin 节点自身求值；selectors 0.40 默认实现 `debug_assert!(self.is_pseudo_element())` 于 origin 节点必炸）。**通道契约**：级联 winners+custom_winners 双空（@media 门控外/选择器不命中）→ None 不插入（宿主回退系统缺省）；清除语义二分——全表无规则 → 整 map 清（零成本），单节点未命中 → 仅 remove 本节点（restyle DFS 后段未命中节点不得抹前段命中条目，探针实证）。

### D3：通道级联（cascade.rs）

- 助手 `fn rule_has_channel_pseudo(rule: &Rule, channel: &PseudoElement) -> bool`（`iter_raw_match_order` 扫 `Component::PseudoElement(pe)` 精确比对变体）。
- collect_sheet 加 `channel: Option<PseudoElement>` 参（私有，签名自由）：None=主级联（排除通道规则），Some(w)=通道级联（仅收含 w 伪元素分量的规则）；match_rules（pub）不变。
- 新 pub fn `cascade_channel<'a>(tree, id, sheets: Vec<&'a Stylesheet>, user_sheet: Option<&'a Stylesheet>, env, container_ctx, channel: &PseudoElement) -> CascadeOutput<'a>`——user → author 序逐表 `match_rules_channel` → push_decl/push_custom（与 collect_sheet 同键形）；**无内联声明段**（style 属性无法指向伪元素，spec 一致）；尾部 resolve_revert → retain 非空 → 排序同 cascade_declarations。

### D4：计算复用（computed.rs）

compute_node_in 体（direction 解析起）抽取为 `pub fn compute_node_from_cascade(tree, id, cascaded: CascadeOutput, registered, env, parent) -> ComputedStyle`；原 `pub fn compute_node_in` 变薄壳（**公开签名零改**：内部 cascade_declarations → compute_node_from_cascade）。通道调用 `parent = Some(origin 主 ComputedStyle)`（D2 继承语义）。

### D5：引擎通道（engine.rs / stylesheet.rs）

- `Stylesheet` +`has_selection_rules` / `has_placeholder_rules: bool`（解析期赋值，镜像 `has_pseudo_rules` 的 `sp.rules.iter().any(...)` 扫描）。
- engine +`selection_styles` / `placeholder_styles: HashMap<NodeId, ComputedStyle>`；`pub fn selection_style(&self, key) -> Option<&ComputedStyle>` / `pub fn placeholder_style(&self, key) -> Option<&ComputedStyle>`（镜像 computed_style()）。
- restyle_node：主样式 insert 后，若合并表组任一 `has_selection_rules` → `selection_styles.insert(id, compute_node_from_cascade(cascade_channel(..., Selection), ..., parent=Some(主样式)))`；Placeholder 同构。三表全无对应规则 → 整 map `clear()`（C1 零成本清除模式）。
- 伪节点（materialize 的 ::before/::after）走同一 pass 但 D2 使通道规则对 pseudo==Some 恒不匹配 → 空结果不入 map，自然处理。

### D6：materialize 闸过滤（stylesheet.rs）

`selector_list_has_pseudo`（:2073-2080）**只认 Before/After**——`::selection`-only 表不得触发 `materialize_pseudos`（has_pseudo_rules 保持盒生成语义）。

## 否决项

- **树实体化**：selection/placeholder 无盒，实体化产生假盒破坏布局。
- **MatchingContext 旗标**：mirror 每次调用新建（`TreeNode::new`），无实例载体；且 origin 直配（D2）已语义正确。
- **截断选择器前缀匹配**：selectors 0.40 不外借部分选择器匹配；origin 直配等价且零成本。
- **仅属性子集解析**：全量解析 + 文档记录 Chrome 子集（宿主取舍）；子集白名单过滤反而引入"计算了但不暴露"的语义混乱。

## 0.x 破坏性清单

1. `PseudoElement` +`Selection` / `Placeholder` 变体（non_exhaustive；+Copy；跨 crate 穷举 match 须补臂；库内穷举点 = ToCss / parse_pseudo_element / match_pseudo_element）。
2. `Stylesheet` +`has_selection_rules` / `has_placeholder_rules` 字段。
3. engine +`selection_style` / `placeholder_style` 访问器（纯增量）。

## 测试计划（tests/css_selection.rs，新文件）

锁测约 10 件：①`::selection` 解析接受 + 单冒号 `:selection` 拒绝；②通道隔离——主 color 不被 ::selection{color} 污染、selection_style() 取到通道色；③特异性排序（#id::selection vs .c::selection）；④user/author origin 序；⑤@media 门控（条件外不命中）；⑥继承基 = origin 主样式（通道未声明 font-size → = 主样式 font-size）；⑦::placeholder 独立通道互不污染；⑧has_selection_rules 闸——无规则时通道 map 清空；⑨selection-only 表不触发 materialize_pseudos（has_pseudo_rules = false、无伪节点）；⑩组合前缀（`.btn:hover::selection`）匹配。
