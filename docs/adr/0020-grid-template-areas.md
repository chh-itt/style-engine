# ADR-0020：grid-template-areas 与命名线（E5）

- 状态：已接受（E5 批实现期）
- 关联：ADR-0019（settle 先例与 0.x 政策）、第五批③（grid 轨道接通 taffy）
- 日期：E5 批开工日

## 背景与目标

31 项缺口清单的 E5 项 = grid-template-areas + 命名线 + subgrid。现状
（grep 在场性盘点）：

- 已有：`grid-template-columns/rows`（`GridTemplate { tracks: Vec<TrackSize> }`，
  property.rs :1457；解析 `parse_grid_tracks` :2619，子集=定值轨道+固定次数
  repeat+auto-fill/fit）；`grid-auto-flow`（Row|Column）；`grid-auto-rows/columns`；
  taffy 映射（layout.rs :561-600，`grid_component`/`auto_tracks`）。
- 缺：`grid-template-areas`；模板内 `[line-name]` 括号线名；
  `grid-row-start/end`、`grid-column-start/end` 四长手；`grid-row`/
  `grid-column`/`grid-area` 简写；区域矩形与线名到数字线号的解析。
- taffy 0.14 事实（style/grid.rs verbatim）：
  - `GridPlacement<S>` = Auto | Line(GridLine) | NamedLine(S, i16) | Span(u16)
    | NamedSpan(S, u16)——放置侧有名线形态；
  - `GridTemplateComponent<S>` = 仅 Single | Repeat——**无非重复轨道线名存储、
    无 Subgrid 变体**；`lines_names()` 仅存在于 repeat 的 `line_names`；
  - doc 明言 named tracks "not implemented"（taffy 侧解析不全）。

## 决策

### D1：引擎侧解析（Option B）

CSS 全量解析在本仓（括号线名、areas 字符串、放置名）；引擎侧把**全部命名
引用解为数字线号**，taffy 只收 `GridPlacement::Line(i16)/Span(u16)` 数字
放置。理由：

1. taffy 模板存储不支持非重复轨道线名（侦察③②）；
2. 命名解析权（区域矩形、线名注册表、-start/-end 后缀）本就该引擎持有，
   与 ADR-0019「settle 先例=引擎侧语义」同构；
3. 避免 S 型字符串（CheapCloneStr）进入引擎持有的结构（E4 E0277 教训）。

### D2：值面（property.rs）

- `GridTemplate` + `line_names: Vec<Vec<String>>`：N 轨 N+1 线槽
  （line_names[i] = 第 i 轨之前的线名；line_names[N] = 尾线）；
  `parse_grid_tracks` 交替解析 `[名1 名2]` 括号段。
- `GridAreas { rows: Vec<Vec<String>> }`：`grid-template-areas` 值
  （引号串逐行、空白分词、`.` 为空格）；校验=各行格数一致 + 同名格构成
  精确矩形，违反则整条声明无效（spec）。
- `GridLineSpec { Auto, Number(i16, reversed: bool), Span(u16),
  SpanName(String), Name(String) }`；spec 混合形 `<integer> && <ident>`
  v1 偏差在案。
- 新属性 ×5：`GridTemplateAreas`/`GridRowStart`/`GridRowEnd`/
  `GridColumnStart`/`GridColumnEnd`（slot 134-138；动画描述符顺延
  139-145；SLOT_COUNT 141→146；ALL 134→139——一致性锁自动验收）。
- 简写 ×3：`grid-row`/`grid-column`（start [/ end]）、`grid-area`
  （row-start / col-start / row-end / col-end，缺省补 auto）——走
  shorthand_exists/shorthand_longhands/expand_shorthand 三表+一臂
  （解析期展开；var() PendingShorthand 管线自动兼容；防漂移锁覆盖）。

### D3：解析面（engine.rs `apply_grid_placements`）

- **挂点 = frame 内 seed_image_leaves 之后、compute_layout 之前**：
  放置纯样式派生、零布局依赖 → 无需 post-layout 重排（与 settle_* 的
  「改样式→重跑」不同，此处一遍即可）；每帧幂等 = restyle 的 map_style
  重置 taffy 放置 + 本 pass 重施。
- 容器面：读 GridTracks(cols/rows) + GridAreas →
  ①线名注册表：名 → 有序线号集（repeat(固定次数) 内线名按迭代展开；
  RepeatAuto 内线名静态不可数 → 解名失败 = Auto，B 级偏差在案）；
  ②区域矩形：名 → (row_start, row_end, col_start, col_end) 0 基格。
- 子放置解析（四长手 → 数字）：
  - `Number(n)` → `Line(n)`（负数 = 自端计数，taffy GridLine 原生）；
  - `Span(k)` → `Span(k)`（k ≥ 1，解析期钳）；
  - `Name(s)` → 区域边线（start=cs+1、end=ce+2）或线名第 n 次
    （含 `-start`/`-end` 后缀名）或未知名 → `Auto`（spec：不存在的名
    视作 auto）；
  - `SpanName(s)` → start 先解，end = start 后第 k 次名线（不足 = 末线
    +1 钳；隐式线计次）。
- 区域隐式轨道：taffy `grid_auto_*` 原生（模板不必注入）。

### D4：否决项

- taffy `NamedLine`/`NamedSpan` 放置形态：模板侧无名存储 + S 型字符串
  入引擎结构 + 解析权应留引擎（D1）。
- subgrid：taffy 0.14 `GridTemplateComponent` 无 Subgrid 变体 = 引擎级
  限制（非本仓可解），诚实边界在案，待 taffy 支持后重估。
- `grid-template` 三合一简写：v1 不做（rows+areas+columns 复合文法），
  后续批评估。

## 0.x 破坏性

- `PropertyId` +5 变体（SLOT_COUNT 141→146；动画描述符重编号）。
- `DeclValue` +`GridAreas`/+`GridLine` 变体。
- `GridTemplate` +`line_names` pub 字段（字面量构造破坏）。

## 锁测试（tests/css_grid_areas.rs）

①区域矩形放置（2×2 区域 → 子格位与跨度）；②线名 `[a] 100px [b]` +
`grid-column: a / b`；③整数线号；④span 整数；⑤span 名；⑥未知名 →
Auto 回退；⑦非矩形区域 → 声明无效回退；⑧区域隐式第三列
（2 列模板 + 3 列区域 → auto 列轨）。

## 落地记录（E5 收口）

- 实现全绿：值面+简写面+引擎面三层；workspace **410** 测试/0 失败（新增
  tests/css_grid_areas.rs 八锁：2×2 区域矩形放置/线名 a..b/整数线号/
  span 整数/span 名线/未知名 Auto/非矩形区域无效回退/区域隐式列）。
- 落地修订（相对本 ADR 原文）：
  1. **grid-area 缺省段镜像**（spec css-grid §7.4/7.5）：单值 `a` →
     四段全 a（col-start 镜像 row-start ident、col-end 镜像
     col-start ident）——原「缺省补 auto」会破区域主用例（end 定
     start 自动 → taffy start=end−1 落错格 x=100），已按 spec 实现。
  2. **cssparser 词法教训**：`[ … ]` 非 Delim('[')，是
     SquareBracketBlock 块 token（**unit 变体**，E0532 实证——块内容
     不在 token 里）→ bracket_open 命中块 token +
     parse_nested_block(bracket_names) 收名（非 ident=声明无效；
     try_parse commit 后 last_token=块，取块合法）。
  3. **auto_tracks 升级**：grid-auto-* 与模板共用轨道列表解析
     （GridTracks → track_sizing 全列表映射；原 LenAuto 单长度子集
     形态令 50px 声明落空 → 隐式轨 auto 拉伸 w=400 偏差，已修）。
  4. 诚实边界确认：repeat 内线名=解析失败声明无效（v1 括号仅模板
     顶层）；`span <int> <ident>` 混合形不支持（SpanName 隐含 k=1）；
     subgrid=taffy 0.14 无 Subgrid 变体，诚实边界维持。
- 0.x 破坏性落地：PropertyId/DeclValue +5 变体（slot 134–138，动画
  描述符 139–145，SLOT_COUNT 146）、GridTemplate +line_names
  （slot_alignment 一致性锁自动验收）。
