# ADR-0036：counters 与生成内容扩展（counter()/counters()/attr()/quotes）

日期：2026-02（P5 批，goal-c293e543）
状态：已接受（落地记录见文末）

## 背景

content 属性现状（property.rs `parse_content` :2531）仅
normal/none/字符串；`attr()/url()/counter()/quotes` 解析期拒绝
（:1033 注释）。counter 体系全仓零命中。但 ::before/::after 基建
已全量落地（C1/ADR-0015）：`PseudoWhich{Before,After}`（tree.rs
:44-80）、`materialize_pseudos`（engine.rs :691）、
`sync_pseudo_text`（engine.rs :776，content 计算值→伪节点文本）。
生成内容管线只差「content 值更丰富」与「计数器求值 pass」。

依据：css-lists-3（计数器）、css-content-3（content/quotes）、
css2 §12（生成内容）。

## 决策

### D1：content 值模型

- `DeclValue::Content` 扩展承载 `Vec<ContentPiece>`（单字符串升为
  序列；normal/none 保持原子语义）：
  - `Str(String)`（既有）；
  - `Counter { name, style }` / `Counters { name, separator, style }`；
  - `Attr(String)`（元素属性名）；
  - `OpenQuote | CloseQuote | NoOpenQuote | NoCloseQuote`。
- 解析：`counter(name)` / `counter(name, style)` /
  `counters(name, sep)` / `counters(name, sep, style)` /
  `attr(ident)` / open-quote 族 ident；`url()` 维持拒绝
  （图片内容=替换元素领域，超范围）；未知函数维持拒绝。
- style 参数：v1 接受 ident，仅 `decimal` 生效（其余忽略按 decimal，
  B 级在案）；默认 decimal。counter-style @规则不做（等上游/后续批）。

### D2：计数器求值 pass（树序、文档序）

- 时机：`materialize_pseudos` 之后、layout 之前（restyle 管线内）；
  前序 DFS（含伪节点按宿主位置参与）。
- 属性（新槽位×2，P1a 后动 property.rs）：
  - `counter-reset: [<custom-ident> <integer>?]#`（initial none；
    ident only = 0；重复 ident 取后者）；
  - `counter-increment: [<custom-ident> <integer>?]#`（ident only
    = 1；默认 increment 规则（无任何 increment 声明的根级
    `counter-increment` 隐含?）——**不做** css-lists-3 §3 的隐含
    list-item 递增（无 list-item counter 概念）。
- 求值模型（css-lists-3 作用域的 v1 近似）：全局表
  `HashMap<String, i64>` 随 DFS 推入/弹出作用域帧——元素声明
  counter-reset 时压入新帧遮蔽同名（子树内可见），离开子树弹出；
  counter-increment 在帧顶累加；`counter(name)` 读当前最内帧值，
  `counters(name, sep)` 读**全部帧**自外向内拼接
  （值.join(separator)，css-lists-3 嵌套编号语义）。
- 消费：求值 pass 将伪节点（及宿主？**仅伪节点**——普通元素
  content 无渲染效果，css2 §12.2）content 中的 Counter/Counters/
  Attr/OpenQuote/CloseQuote 展开为字符串，写入伪节点 text
  （与 sync_pseudo_text 合流为同一 pass）。
- attr()：读 `StyleNode.attrs`（BTreeMap<String,String> 已备）；
  缺失属性 = 空串（css2 规范：无法解析→空字符串）。

### D3：quotes 属性（新槽位×1）

- `quotes: none | auto | [<string> <string>]#`（initial auto）。
- 初始 auto = 引擎内置引号对 `“”‘’`（U+201C/201D/2018/2019，
  拉丁惯例；本地化引号表不做，B 级）。
- OpenQuote/CloseQuote 深度配对：DFS 携带引号深度计数（伪节点
  求值时 +1/−1；<0 时 OpenQuote 补深 0 对——css2 §12.3.1 深度
  语义，NoOpenQuote/NoCloseQuote 不改深度只静默）。

### D4：不做

- `url()` 图片 content（替换元素）；
- counter-style @规则与罗马字/字母样式族；
- ::marker 与 list-item 隐含计数；
- 普通 content 替换（content 在非伪元素上的渲染；规范本就无效）；
- target-counter()/leader()（css-gcpm）。

## 测试要点（实施时落锁）

- 解析：counter/counters/attr/open-quote 全形 + junk 拒绝 +
  url() 维持拒绝 + style 参数非 decimal 降级；
- 求值：线性列表编号（1. 2. 3.）、嵌套 counters（"1.1"/"1.2"）、
  counter-reset 作用域遮蔽、负步长 decrement；
- attr()：存在/缺失/非伪元素忽略；
- quotes：auto 初始对、显式 quotes 覆盖、深度嵌套交替、close 超
  open 的钳制；
- 回归：无 counter/content 声明时 restyle 管线行为与现版逐位一致。

## 落地记录

（P5 实施后补记。2026-02，commit 见 IMPLEMENTATION-LOG P5 段。）

### 实施要点

- **属性面**：`CounterReset`/`CounterIncrement`/`Quotes` 三槽位
  （slot 170/171/172，SLOT_COUNT=180，动画描述符后移 173..179）；
  `DeclValue::CounterList(Vec<(String, i64)>)`（reset/increment 共用，
  缺省物化解析期完成）+ `DeclValue::Quotes(QuotesValue)`
  {Auto|None|Pairs(Vec<(String,String)>)（non_exhaustive）}；
  `ContentPiece` 九变体（non_exhaustive）+ `ContentValue::Seq`。
- **求值 pass**：`sync_pseudo_text` 重写为树序 DFS
  （`eval_content_walk` + `eval_pseudo_content` + `apply_pseudo_text`；
  伪节点无样式/无 Seq 走旧单串路径）。作用域=`Vec<HashMap<String,i64>>`
  帧栈；**merge 弹出**（离开节点把本帧计数写回父帧 = css-lists-3
  兄弟继承链）；increment 全栈倒查累加（首见隐式 0 起步写当前帧）；
  counter() 最内帧、counters() 全帧自外向内 join；attr() 读
  originating element（host_of 反查）。
- **quotes**：深度配对；OpenQuote 取 pairs[clamp(d)] 后 d+=1；
  **CloseQuote 深度 0 不产出（css-content-3 钳 0 静默）**——首版
  「先钳 0 再取 pairs[0].1」产出错引号，测试锁暴露后修正；
  NoOpen/NoClose 只动深度。
- **map_style 生成判定**（P5 附带修复）：layout.rs 伪节点
  `Display::None` 判定从 `!matches!(content, Str(_))` 升级为
  「content_pieces() Seq 非空或旧 Str」——单串也承载为 Seq 后
  该判定若不升级会**全量回归伪元素盒生成**（pseudo_elements 14/19
  失败暴露；one-line 修复）。

### 实施偏差（对决策原文）

- 【B】counter()/counters() style 参数解析接受 ident 但恒按 decimal
  求值（罗马字/字母样式族未实现，D1 原案一致）；
- 【B】quotes auto 引号对固定拉丁 U+201C/201D/2018/2019（无本地化）；
- 【A 级升级】counters join 语义按 css-lists-3 嵌套编号（全帧
  自外向内）实现，与 Chromium 一致；
- 【B】负步长 decrement 支持（integer 可为负）已含；隐含 list-item
  不做（D2 原案）。

### 锁测试（+6）

- `decl.rs::counter_props_parse_family`：counter-reset/increment
  空格分隔语法 6 正例+4 拒绝；quotes 3 正例+3 拒绝；content 序列
  6 正例（counter/counters/attr/open-quote/混合）+5 拒绝
  （url()/缺参/逗号分隔/未知函数）。
- `tests/pseudo_elements.rs`：`content_counter_increments_down_tree`
  （树序 1/2/3）、`content_counter_reset_scopes_per_node`（reset
  每节点 1/1/1）、`content_counters_joins_scope_frames`（嵌套
  "1.1"）、`content_attr_reads_host_attribute`（存在产出/缺失空串）、
  `content_quote_pairs_depth_match`（«x» 配对+越配 close 静默）。
  断言经 `Frame.paint` Text op 文本直读（非宽度度量尺）。
