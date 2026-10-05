# ADR-0012 多样式表文档模型：@import 拼接 + @supports 解析期求值 + 文档全局层树

日期：B2 批次。状态：已实施。

## 背景

B1 后引擎为单 author 表 + 单 user 表模型；@import/@supports 在
`stylesheet.rs` 解析器 else 分支被整条跳过并告警（MVP 行为）。31 项缺口
中"多样式表 + @import + @supports"要求：宿主注入多张 author 表（按登记
序级联）、@import 拉取解析（本地无网络——宿主 loader 供给）、@supports
条件求值、以及 B1 遗留的跨表层序问题（各表 LayerRegistry 序数互不可比，
必须文档全局化）。

## 决策

1. **多样式表 = 主表 + 句柄化附加表**。`StyleEngine` 保留 `sheet`
   （set_stylesheet 语义不变，88 处既有调用零改动）并新增
   `extra_sheets: Vec<(u64, Stylesheet)>`：`add_stylesheet(css) -> u64`
   按登记序附加、`remove_stylesheet(handle)` 移除。级联序 = 主表在前、
   附加表按登记序在后（css-cascade-5：表序即源序，后者胜平手）。
   `cascade_declarations` / `compute_node_in` 改收 `&[&Stylesheet]`。

2. **文档全局层树**。引擎持 `doc_layers: LayerRegistry`；每表附着时
   `remap_layers_to_doc`：把本表 paths 按 intern 并入文档树，再按
   旧 rank → 本表 path → 文档 ordinal 重写全部 `rule.layer_rank`
   （未分层 u32::MAX 不变）。此后全部 author 表的层序键可比，先现序
   = 首次 intern 顺序（引擎按表序附着，文档层先现序稳定）。

3. **@import = 附着期拼接**（css-cascade-5 §3：导入规则视同写在导入点）。
   解析期仅收集 `ImportDirective { url, layer, media, order }`（仅顶层
   合法；块内 @import 警告并丢弃；supports 子句解析期求值，不满足则指令
   静默失效）。引擎附着期 `resolve_imports`：按 order 与规则流交错拼接，
   子表以 `current_layer = 前缀 ∪ 指令层` 解析（`layer(name)` 前缀、
   `layer;` 匿名层附着期固化）、其层树并入本表、keyframes/report 并入、
   规则 media 与指令 media 做 AND 合取。循环守卫 = `seen` URL 栈 +
   深度 32。导入源 = `import_loader: Option<Box<dyn FnMut(&str) -> Option<String>>>`
   （宿主优先）回退内存 `imports: BTreeMap<String, String>`
   （`set_import_source`）。未解析 → Skipped 告警、规则不产出。

4. **@supports = 解析期求值**。引擎能力是构建期静态（属性文法 + 选择器
   文法），故 `parse_supports_condition(input) -> Option<bool>` 在 prelude
   一次求值：`(decl)` 试探 = `PropertyId::from_css_name` + 文法
   `parse_declaration`（自定义属性恒真）；`selector(sel)` 试探 =
   `parse_selector_list`；`not`/`and`/`or`（同级不得混用，与浏览器一致）。
   true → 块规则照常产出（继承 media/container/层上下文）；false → 整块
   静默消费（等价 @media 不命中）；语法无效 → Dropped 告警。

5. **media 合取 = MediaQuery.conjoin**。`MediaQuery` 增
   `conjoin: Vec<MediaQuery>`（各合取元独立求值含自身 negate，外层
   negate 反转全体 = De Morgan 正确）；`eval` 展开 AND；嵌套 @media 由
   旧行为"内层覆盖外层"修正为 `and_media` 合取（嵌套与 @import media
   共用）；`and_media(a, b)`：任一 None 透传，双 Some = a.conjoin.push(b)。

## 破坏性（0.x）

- `compute_node_in`/`cascade_declarations`：`sheet` 参数改为
  `sheets: &[&Stylesheet]`（compute_node 包装 from_ref 兼容）。
- `Stylesheet` 新增 `imports: Vec<ImportDirective>`、`layers: LayerRegistry`
  字段（直构 Stylesheet 的调用方需补字段）。
- `MediaQuery` 新增 `conjoin: Vec<MediaQuery>` 字段。
- 嵌套 @media 语义修正（内层覆盖 → 合取）：依赖旧行为的对拍项随本 ADR
  记录为有意变更。

## 后果

- 级联期零层查表保持（rank 已文档化）；@import 拼接后主表规则流自洽，
  级联器无需感知导入；@supports 无运行时成本。
- 宿主 UA 表与 @import 的层交互：UA 表如经 add_stylesheet 注入会与
  author 共享文档层树——UA 表应走 user_sheet/独立通道（B2 不开放 UA
  add_stylesheet；Origin::UserAgent 档仍留待宿主 UA 内容接入）。
