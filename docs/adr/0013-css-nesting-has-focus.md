# ADR-0013：CSS Nesting（解析期 desugar）+ :has() 相对选择器 + :focus 族引擎管理

状态：已接受（B3 批次落地）
关联：ADR-0004（cssparser/selectors 选型）、ADR-0011（级联起源栈）、ADR-0012（多表/@import/@supports）、docs/INVALIDATION.md（失效模型）

## 背景（Context）

31 项 gap 的 B3 批覆盖三个特性，分别触及解析器架构、选择器引擎与宿主交互 API：

1. **CSS Nesting（css-nesting-1）**：样式规则体内嵌套样式规则与条件组规则，`&` 引用父选择器。引擎选择器/级联核心均不感知嵌套概念。
2. **:has()（selectors-4）**：相对选择器，命中依赖后代/兄弟结构——对增量失效模型是异类（其余选择器命中只依赖节点自身状态/树位置局部信息）。
3. **:focus 族（selectors-4 / UI 事件）**：`:focus`/`:focus-visible`/`:focus-within` 三态由宿主交互驱动，引擎此前只有 `set_state` 低层位接口，焦点链（祖先传播）无管理入口。

## 决策（Decision）

### 1. CSS Nesting = 解析期字符串级 desugar，级联核心零改动

- **`&` ≡ `:is(父有效源)`**（css-nesting-1 §2）：特异性取 `:is` 最特定参数 = 父选择器语义；无 `&` 的嵌套 prelude = 隐式后代 `:is(父) <sel>`；组合器开头（`> .x` 等）同式拼 `:is(父) > .x`。
- **深层嵌套递归展开**：内层父 = 外层 desugar 后的有效源（`:is(:is(#a) .b) .c` 形态），特异性随链累积（锁测试：`(0,2,0)` 的嵌套规则胜过源序更晚的 `(0,1,0)` 平层规则）。
- **Token 级 `&` 识别**：desugar 在 TokenBuf 上逐 token 重建（`Delim('&')` 序列化恰为 `"&"`），字符串/URL 内的 `&`（如 `[a="&"]`）是完整 token 文本不受影响。
- **深度上限 32**（`MAX_NESTING_DEPTH`）：超出规则丢弃 + Dropped 告警。
- **架构**：`QualifiedRuleParser::parse_block` 从 `parse_declaration_block`（纯声明）改为 cssparser `RuleBodyParser` 驱动子解析器（上下文继承 + `nesting_parent = Some(本规则有效源)`）。`RuleBodyItemParser::parse_declarations()` = `nesting_parent.is_some()`——仅嵌套体声明合法，同时激活 cssparser 内建「Ident→声明失败重试限定规则」消歧（`a:hover{}` 走规则不误判声明）。单声明经 `DeclarationBlockParser::parse_value` 直调（input 已过冒号、定界于分号；`take_block` 回收）。
- **嵌套声明源位置分裂（flush-at-item-boundary）**：`QualifiedRuleParser::parse_prelude` / `AtRuleParser::parse_prelude` / `rule_without_block` 入口先 flush 累积声明为隐式 `&` 规则——`.card { color: red; @media (…) { color: lime } }` 中 red 的源位在 media 之前（浏览器语义 lime 胜）；体尾 flush 只兜最后一组。隐式规则选择器取 `enclosing_prelude`（外层规则选择器列表，parse_block 构造子时一次性写入，不被嵌套 prelude 污染）。
- **嵌套条件组提升穿线**：@media/@supports/@container/@layer 在样式规则体内 = 子解析器继承 `nesting_parent`/`enclosing_prelude`，条件与外层 media 经 `MediaQuery::conjoin` AND 合取（B2 管线复用），选择器继续挂父链；@import 子块内出现 = 告警丢弃（B2 既有）。
- **顶层裸声明拒绝**：`DeclarationParser::parse_value` 守卫 `nesting_parent=None → Err`（纵深防御）。已知偏差：样式表顶层的裸声明经 cssparser `StyleSheetParser` 会毒化下一规则（prelude 捕获吞至下一 `{`），预存在行为，记 B 级。

### 2. :has() = selectors 0.40 原生启用 + 表级失效升级

- **匹配**：`SelectorParser::parse_has() → true`（selectors 0.40 自带解析与匹配——matching.rs relative_selector 模块走同一 Element trait 遍历，引擎零自研）。
- **失效**：`Stylesheet.has_relative_selectors`（解析后扫描 `iter_raw_match_order` 含 `Component::Has`——深扫含 :is/:not/:has 内层）；引擎 `any_has_rules()`（主表 + user + 附加表）→ 帧内变更类失效 gate 升级全量 restyle。理由：相对选择器命中依赖后代/兄弟结构，增量子树 restyle 不感知远端变化；正确性优先，全量重样式语义与 @container 快照收敛先例一致。
- **增量 :has 失效**（精准脏传播）= B 级延后（优化非正确性）。

### 3. :focus 族 = 引擎整链管理（set_focus）

- `pub fn set_focus(&mut self, key: Option<K>, focus_visible: bool) -> Result<(), ContractError>`：整体迁移——清旧链（旧焦点节点 FOCUS|FOCUS_VISIBLE + 祖先链 FOCUS_WITHIN）→ 施加新链（FOCUS + focus_visible?FOCUS_VISIBLE + 祖先链 FOCUS_WITHIN）→ `dirty_style = true`。
- **无 early-return**：同节点重设（focus_visible 翻转）也走全链清旧/施新。
- `focus_visible` 启发（键盘 vs 指针焦点）归宿主——引擎无输入设备知识。
- `remove()` 两路径（根清空 / 子树移除含 doomed 检查）清 `focused_node` 锚点（slotmap 键复用防悬垂）。
- `set_state` 保持整体替换低层语义；文档化：焦点族应经 `set_focus` 管理。

## 后果（Consequences）

- **正向**：级联/失效核心零改动吸收 Nesting（desugar 后均为普通规则流）；:has 匹配零自研；焦点族单点 API；嵌套声明源位置分裂与浏览器一致。
- **0.x 破坏性**：`Stylesheet` 新增 `has_relative_selectors` 字段（结构体字面构造点需补）。
- **B 级在案**：顶层裸声明毒化（预存在）；增量 :has 失效延后；嵌套深度 32 上限；@import 子块内丢弃（既有）。
- **锁定**：`crates/style-engine/tests/nesting_has_focus.rs` 26 件；workspace 324 全绿（298 + 26）。
