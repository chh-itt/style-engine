# ADR-0014：@property 自定义属性注册（css-properties-values-api）

状态：已接受（B4 批次）
关联：ADR-0011（级联起源栈/custom 候选）、ADR-0012（多表合并）、ADR-0013（解析架构）、docs/INVALIDATION.md

## 背景（Context）

var()/custom property 管线（B1 级联候选 + 字符串级代换）已就绪，但自定义属性无类型系统：无语法门、无注册初值、继承行为一律可继承。@property（css-properties-values-api）为自定义属性引入注册机制：syntax 文法、inherits、initial-value 三描述符 + 计算期语法校验。

## 决策（Decision）

### 1. 解析与注册面

- `@property <name> { <descriptors> }`：name 必须为 `--` 前缀自定义属性名（否则整规则无效 Dropped）。描述符：`syntax`（带引号字符串）、`inherits`（true/false，缺省 false）、`initial-value`（token 流）。未知描述符忽略；描述符值经 `;` 定界捕获。
- 产物 `PropertyRule { name, syntax: PropertySyntax, inherits: bool, initial_value: Option<TokenBuf> }`，`Stylesheet.property_rules: Vec<PropertyRule>` 收集（0.x 破坏性字段）。
- **语法文法**：`syntax: "*"` = Universal（不校验）；`Named { ty, multi }` 支持类型族 `<length>|<number>|<percentage>|<length-percentage>|<color>|<image>|<url>|<integer>|<angle>|<time>|<resolution>|<transform-function>|<custom-ident>|<string>` 与多值组合子 `+`（空白分隔）/`#`（逗号分隔）单层组合。不支持 `||`/`&&`/嵌套括号组合（解析失败 = 规则无效）——MVP 面记录。
- **注册有效性**：非 universal 且缺 `initial-value` → 规则无效（Dropped 告警）；initial-value 与 syntax 不匹配 → 规则无效。仅在样式表顶层合法（嵌套语境/条件组内 = Dropped；@supports/@layer 包裹允许——非条件过滤语境）。
- **类型试探**（syntax_matches）：number/integer = cssparser expect_number/expect_integer；length/angle/time/resolution/percentage = token 级（Dimension 单位集/Percentage）；color = 复用 `property.rs parse_color` 真文法；string = QuotedString；custom-ident = Ident 且非 CSS 宽关键字；url/image/transform-function = 函数名集合判定。

### 2. 注册表合并

- 引擎文档级 `registered_props: BTreeMap<String, RegisteredProperty>`——sheet 变更点（rebuild_sheets/attach_sheet/set_user_stylesheet/add/remove_stylesheet）统一重建。合并序 = user_sheet 先、主表、附加表登记序后（author 覆 user——与起源优先级同构）；同名后表覆盖（spec：@property 全部层叠前按文档序处理）。
- compute_node_in 新增 `registered: &BTreeMap<String, RegisteredProperty>` 参数（0.x 签名变更，user_sheet 先例）。

### 3. 计算语义（computed.rs）

- **继承门**：`inherits: false` 的注册名不进继承通道（parent.custom 跳过）→ 子代取 initial-value 或 guaranteed-invalid。
- **语法门**：CustomResolver.get 终值化后，注册名非 universal → `syntax_matches` 试探；失败 → None = guaranteed-invalid（var() 引用处走 fallback 链）。继承值上游已校验，不重复。
- **initial 填充**：final_custom 定稿后，注册名若声明/继承双缺 → 填 initial-value 字符串化 token（无 initial（仅 universal 可达）→ 缺席 = guaranteed-invalid）。
- 未注册自定义属性行为完全不变（可继承、不校验）。

## 后果（Consequences）

- 正向：类型化 custom property 三描述符全语义；var() 消费端零改动（校验在注册属性的计算端）；注册表单点合并。
- 0.x 破坏性：`Stylesheet` +`property_rules`；`compute_node_in`/`compute_node` +`registered` 参数。
- MVP 面在案：syntax 组合子仅 `+`/`#` 单层（`||`/`&&`/嵌套 → 注册无效）；<image>/<url>/<transform-function> 为函数名集合近似；@property 条件组内拒绝（spec 一致）。
- 锁定：`crates/style-engine/tests/property_registry.rs`；workspace 全绿。
