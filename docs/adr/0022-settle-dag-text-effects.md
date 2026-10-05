# ADR-0022: F2——结算 DAG 化 + ellipsis/line-clamp + text-decoration/text-shadow

- 状态：已接受（F1 后 F2 批；round 11）
- 关联：ADR-0019（浮盒）、ADR-0021（行内流）、三期①（延迟结算管线）

## 背景（盘点）

31 项缺口之「结算 DAG 化、ellipsis+line-clamp、text-decoration+text-shadow」。
盘点（grep 全 src）：ellipsis/text-overflow/line-clamp/text-shadow/
text-decoration **零命中=四件全真缺口**。settle 家族=固定硬编码序
engine.rs :1411-1424（calc→tables→columns→floats→lines）+ 七个
settle fn；PaintOp=#[non_exhaustive]（paint.rs :23，Shadow :59 模糊=
sink 多环近似先例）；Text op 发射 :934-950（op_text=tree text 克隆）。

## 决策

### D1 结算 DAG 化

`SettlePassKind` 枚举 {Calc, Tables, Columns, Floats, Lines}：
- `deps() -> &'static [SettlePassKind]`（声明依赖：Tables/Columns→Calc、
  Floats→Columns、Lines→Floats）+ `schedule()`（拓扑序+去重）。
- `should_run(&Engine) -> bool`：各 pass 早退谓词显式化（tables 空/
  无浮盒/无行内运行…）。
- frame 的 :1411-1424 硬编码序改为执行 `SettlePassKind::schedule()`
  （逐 pass `should_run` 门 + `run`）。
- 锁测试：①schedule() 拓扑序=依赖全在前（断言序）②空输入 pass 被
  should_run 跳过。
- 诚实边界：pass 内部脏区跳过=增量布局（F3）范围；本批 DAG=声明化
  调度+门控+拓扑正确性锁定。

### D2 text-overflow: ellipsis

- 解析 clip|ellipsis（<string>=偏差在案）+ PropertyId 槽 + 访问器。
- 应用（文本 remeasure 循环内，测量后）：叶父（包含块）overflow
  hidden ∧ text-overflow=ellipsis ∧ 叶 nowrap → 二分最长前缀使
  width(prefix)+"…"宽 ≤ avail → `text_overrides: HashMap<NodeId,
  String>`（PaintCtx 新字段，对称 wrap_widths 先例）→ paint Text op
  文本取 override；盒宽=min(测量宽, avail)。
- overflow visible 不省略（spec：仅裁剪时）；多行溢出=D3 管。
- 锁：三件（截断带…/不溢出原样/visible 不截）。

### D3 -webkit-line-clamp

- 解析 <int>|none + 访问器（现代 line-clamp 别名后补；-webkit-box
  display 要求不强制=偏差在案）。
- 应用（remeasure 内）：父 overflow hidden ∧ clamp N≥1 ∧ 叶测高 >
  N*行高 → 二分最长前缀使 measure(prefix+"…").h ≤ N*行高 →
  text_overrides + 盒高=N*行高。
- 锁：两件（钳 2 行带…/N=0 none 回归）。

### D4 text-decoration

- 解析简写 text-decoration（|| line || style || color || thickness）
  + 长手四件：text-decoration-line（underline|overline|line-through，
  空格分隔多值）、-style（solid|double|dotted|dashed|wavy）、-color、
  -thickness（auto|from-font|<length>）。槽 149-152，SLOT_COUNT 153
  （**0.x 破坏性**）。
- PaintOp::Text += `decorations: Vec<TextDecoration{line: u8,
  style, color, thickness_px}>`；paint_node 按叶 cs 发射。
- sink：线位=基线+ascent 偏移（underline≈baseline+desc*0.6 处）/
  overline（ascent 上方）/line-through（中轴）；厚度=声明 px 或
  font_size/12 近似（auto）；solid+double 软 sink 实绘；
  dotted/dashed（分段 rect）/wavy（折线）=B 级在案（sink 升级路径）；
  span 级装饰（区间内部分装饰）=v1 叶级在案。
- 锁：解析四件（简写全参数/line 多值/缺省 color=currentColor/thickness
  auto）+ 绘制一件（op 携带 resolved decorations）。

### D5 text-shadow

- 解析 none | <shadow>#（<offset-x> <offset-y> <blur-length>? <color>?，
  缺 color=currentColor、缺 blur=0；双长度必须在 color 前 per spec）。
  槽 153，SLOT_COUNT 154（**0.x 破坏性**）+ 访问器
  Vec<TextShadow{dx,dy,blur,color}>。
- PaintOp::Text += `shadows: Vec<TextShadow>`；sink=影字先绘（偏移+
  颜色）再正字；blur=多环近似复用 box-shadow sink 机械（paint.rs :59
  先例同源）。
- 锁：解析三件（单影全参/多影逗号/缺省回填）+ 绘制一件。

## 否决项

parley 层截断（parley 无 ellipsis 原语；引擎侧前缀二分=测量机械复用）；
现代 line-clamp 简写（解析面后补）；真实高斯 text-shadow（sink 多环
近似=box-shadow 同源先例，B 级在案）。

## 0.x 破坏性

PropertyId/DeclValue +6 变体（text-overflow/text-decoration 系 4/
text-shadow；SLOT_COUNT 146→154）；PaintOp::Text +2 字段
（decorations/shadows）。

## 落地记录（F2 批次收官）

实现与本文档设计的偏差勘误与实况：

- **槽位实值**：text-overflow=139、-webkit-line-clamp=140、
  text-decoration line/style/color/thickness=141–144、text-shadow=145；
  动画描述符顺移 146–152；**SLOT_COUNT 146→153**（本文档前文"→154"
  为规划期估计，实值 153=ALL 144+7 描述符，勘误在案）。
- **D1**：`SettlePassKind{Calc,Tables,Columns,Floats,Lines}`+`deps()`/
  `schedule()`（debug_assert 环防护）+`settle_should_run`（Calc=
  !calc_deferred.is_empty()、Tables=!tables.is_empty()、Columns=
  !multicols.is_empty()、Floats/Lines=恒真内部早退）+`settle_run_pass`
  分派；frame 硬编码链替换为调度循环，pass 顺序/豁免逐位保留。
  锁定 tests：settle_pass_schedule_topological_order /
  settle_should_run_gates_empty_inputs。
- **D2**：`text_overrides: HashMap<NodeId,String>` 绘制期覆盖；
  `leaf_wrap_width` 自 remeasure 循环提取复用（multicol 幻影列宽分支
  同源）；`make_ellipsis_text` 二分字符前缀（chars().take 字节安全、
  全溢出→空串）；`apply_text_truncation` **两相**（相 1 不可变扫描、
  相 2 分派——measure_rich 需 `&mut self.text` 的借用约束所致）；
  PaintCtx 通道+op_text 覆盖+`trunc_prefix = len.saturating_sub(3)`
  （"…"=3 UTF-8 字节）span 过滤/钳位；overflow: visible 不截断。
- **D3**：`min(i32::MAX)` 防 `u32::MAX as i32` 回绕（探针
  `[e2a] clamp_n=4294967295` 实锤）；预算=行数×行高
  （resolved_line_height_px None→1.2×font_size）；auto_text 门移除
  （仅 nowrap 叶阻塞 clamp——折叠叶经 remeasure/restyle 注册）。
- **D4**：组件解析器 ValResult 化 + try_parse 闭包显式返回标注
  （代码库惯例 ~40 先例，E0282×2 实锤）；decl.rs 简写=shorthand_exists
  +shorthand_longhands 双表+expand 任意序贪心；绘制线位 underline=
  baseline+descent×0.5、overline=baseline−ascent×0.9、line-through=
  baseline−ascent×0.5（vello=行内 positioned_glyphs 聚合+run.advance()
  +run.metrics()；soft=pen 循环后总 advance+矩形 fill_polygons）；
  solid/double 实绘、dotted/dashed/wavy v1 实线矩形近似（B 级）；
  span 级装饰=边界（v1 叶级）。
- **D5**：`TextShadowSpec{dx,dy,blur:Option,color:Option<ColorValue>}`；
  解析 `none` 短路+`<color>?` 前置可省+dx dy+blur?+color? 后置回填
  （css-backgrounds-3 && 组合）；**继承**（computed.rs `inherits`
  allowlist +P::TextShadow）；影字=主 draw_text/draw_text 前平移重发
  （vello：Affine 平移并入+spans 置空表；soft：x+dx/y+dy 重发），
  blur=B 级锐利影（vello scene.rs :316 fill 第五参实为 brush_transform
  非 blur——规划期"多环近似"未启用，锐利影先落地在案）。
- **测试实值**：tests/css_text_overflow.rs 六件 + tests/
  css_text_decoration.rs 六件；全量 workspace **431 绿/36 套件 0 失败**
  （含差分逐位与 GPU 像素回归）；soft op_text 测试助手与 vello 解构
  同步补齐新字段。
