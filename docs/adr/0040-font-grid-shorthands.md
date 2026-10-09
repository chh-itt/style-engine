# ADR-0040：font / grid / grid-template 简写展开

- 状态：已接受（2026-10-09，P9-2）
- 关联：ADR-0024（background 层化简写，同机制）、ADR-0033（UA 表与字体偏差台账）、
  ADR-0039（相对字体关键字——本 ADR 的 font 臂直接复用其文法产物）、
  FEATURES.md T0/T1 简写条目、decl.rs `shorthand_exists`/`shorthand_longhands`/
  `expand_shorthand` 三表
- 背景：MVP 简写清单 36 项不含 `font`/`grid`/`grid-template`（decl.rs 注释与
  FEATURES.md 在案）。三者的所有长手均已存在且解析完备（font：Style/VariantCaps/
  Weight/Stretch/Size/LineHeight/Family/Features/Variations；grid：模板三 +
  auto 两 + flow + 放置四），唯一缺的是简写→长手的解析期展开。P9「1.0 对齐」
  将其列为简写收口项（分析与差距报告 ④修复清单）。

## D1：解析期展开，复用既有三表机制

`font`/`grid`/`grid-template` 走与 margin/border/background 完全相同的机制：
`shorthand_exists` 增名 → `shorthand_longhands` 增长手全集（var() 挂起按长手
逐槽落 PendingShorthand）→ `expand_shorthand` 增臂。`shorthand_longhands_match_expand`
锁测试的代表值表扩容（+10）保证两表不漂移。var() 计算值期代换（computed.rs
PendingShorthand → 重解析 → expand_shorthand）零新增代码自动走通——engine 级
测试以 `font: var(--f)` / `grid: var(--t)` 端到端锁定。

## D2：font 文法（css-fonts-4 §3.7）

```
[ <'font-style'> || <font-variant-css2> || <'font-weight'> || <font-width-css3> ]?
<'font-size'> [ / <'line-height'> ]? <'font-family'>
```

- 前导 `||` 组：任意序、各分量至多一次；实现按 token 轮转 try_parse 四个分量
  解析器（save/reset 回滚）。`normal` 是 style/variant/width 三个文法的合法
  值——多次出现均接受且幂等置初始（规范允许每个分量各匹配一次 normal）。
- 字宽分量 `<font-width-css3>` 仅九关键字（ultra-condensed…ultra-expanded），
  **百分比不接受**（百分比是 font-stretch 长手扩展文法，简写文法不含）。
- size 分量完整复用 parse_font_size：含 larger/smaller（P9-1c）、绝对关键字、
  长度百分比。weight 分量完整复用 parse_font_weight：含 bolder/lighter（P9-1b）。
  相对关键字在简写中同样存活到级联物化期，与长手语义一致。
- `font-variant-css2` = normal | small-caps → FontVariantCaps（small-caps 族
  合成既有通路）。
- family 必需（css-fonts-4：简写必含 size 与 family）；缺 family 或尾随垃圾
  → 整条无效（尾随垃圾注意：`12px serif bogus` 合法——`<family-name>` 可为
  多 custom-ident 序列；锁测试用 `12px serif 5px`）。
- reset 集：引擎 font 长手全集 9 项（Style/VariantCaps/Weight/Stretch/Size/
  LineHeight/Family/Features/Variations），未指定分量发初始值。css-fonts-4
  的完整 reset 集含 size-adjust/kerning/palette 等引擎未拥有的属性——按「有
  才重置」原则收敛到引擎属性面（与 margin 简写不重置 margin-trim 同理）。

## D3：系统 UI 字体关键字 = 全长手初始展开（B·豁免）

`caption|icon|menu|message-box|small-caption|status-bar` 文法上整条合法，但
「OS UI 字体映射」超出引擎可达面（无宿主字体桥接、fontique 无该查询）。实现：
全长手初始展开（font-family=sans-serif 缺省族、size=medium）。结果=声明合法
且渲染确定，但非平台 UI 外观。**B·豁免**：重估条件=宿主字体桥接 API（届时
由宿主注入 UI 字体映射，引擎保持中立）。否决「整条拒绝」：文法合法声明被拒
会让严格遵循规范的样式表出现解析失败，比确定性的缺省渲染更糟。

## D4：grid 三形与 grid-template（css-grid-1 §7.3/§7.6）

- `grid` = `<'grid-template'> | <'grid-template-rows'> / [ auto-flow && dense? ]
  <'grid-auto-rows'>? | [ auto-flow && dense? ] <'grid-auto-columns'>? /
  <'grid-template-columns'>`；`grid-template` = none | 轨道形 | areas 形。
- 判别：`auto-flow` 关键字开头 = auto-flow 列形；否则先 try_parse areas 形
  （线名段 `[..]` 与轨道形同型、QuotedString 起始判别不可靠——try_parse 失败
  自复位后回退轨道形）；轨道形读至 `/`，`/` 后为 auto-flow = 行形。
- areas 形：行轨缺省 Auto；行前/行后线名段合并归 GridTemplate 线名槽
  （N 轨 N+1 槽）；areas 行矩形性与逐名格数校验复用 property.rs
  `validate_area_rows`（与 grid-template-areas 长手单源）；areas 形的
  grid-template-areas 产物照常驱动 grid-area 命名放置（既有通路）。
- reset 集：`grid` 10 长手（TemplateRows/Cols/Areas + AutoRows/AutoCols +
  AutoFlow + 放置四）；`grid-template` 仅模板三（§7.3 语义）。`none` 关键字
  两简写均按各自 reset 全集物化初始。
- 共享助手：`parse_track_list_until_slash`（显式轨道表，`/` 停）、
  `parse_template_areas_form`、`parse_font_stretch_kw`（纯关键字形），
  property.rs pub(crate)，长手与简写共用（文法单源，避免第三份轨道解析）。

## D5：dense 拒绝（B 级在案）

`GridAutoFlowKind` 仅 Row|Column，自动放置为稀疏算法（不回填空洞）。css-grid-1
的 dense 要求回填式放置算法（引擎级布局工作，非文法工作）。决策：简写与
grid-auto-flow 长手**一致拒绝 dense**（整条 IACVT + report 告警）。理由：接受
而忽略会静默产出与作者意图不同的布局（回填 vs 不回填），比显式拒绝更糟；
与既有 grid-auto-flow 长手行为一致（不存在「简写接受、长手拒绝」的文法裂缝）。
重估条件：引擎实现 dense 自动放置（届时 GridAutoFlowKind 增 Dense 变体 +
apply_grid_placements 回填算法 + 简写/长手同步放开）。

## 否决替代

1. **calc() 内/计算值期展开**：简写展开必须发生在解析期（级联按长手槽竞争、
   PendingShorthand 机制都依赖解析期展开）；无新信息。
2. **font 系统关键字映射到固定近似字体**（如 caption→Arial）：平台特异性硬
   编码违背框架中立（引擎不感知平台），且 CSS 要求该映射来自 OS——维持初始
   展开并文档化（D3）。
3. **grid 简写接受 dense 但按稀疏处理**：见 D5（静默错排劣于显式拒绝）。

## 影响

- 解析面：三简写 +10 代表值锁；var() 挂起端到端 5 件 engine 测试。
- 行为变化：`font: italic small-caps bold condensed 16px/1.5 serif`、
  `grid: auto-flow / 200px` 等声明由整条 IACVT 变为完整展开；UA/内置样式表
  不含这三简写（无 UA 行为变化）。
- 文档：FEATURES.md 偏差核对节新条目；dense 拒与系统字体 B 级登记。
