# CSS 特性注册表（子集边界的权威定义）

本文件是"超出子集"的唯一权威来源：ADR-0004 的容错边界、ADR-0003 的零容忍作用域、v0.1 路标三者共同引用它。用例 manifest 声明的特性必须能对应到这里的条目；注册表变更必须同步更新对应用例的 in-subset 声明。

## T0 — v0.1（conformance 零容忍覆盖）

- 语法层：真实 CSS 文本；选择器子集 = type / class（class 属性为空格分隔 token，引擎归一）/ universal / 伪类（:hover :active :focus :disabled :checked）/ 后代 / 子代 / 分组；简写展开（margin padding border background color font）；CSS 宽关键字（inherit/initial/unset/revert）；custom properties + var()
- @media 子集：width/height、prefers-color-scheme、prefers-reduced-motion（条件值由 Environment 提供）
- 值与颜色：px/em/rem/%/vw/vh；calc() 基础四则；color crate 全谱（hex/rgb/hsl/oklch/color()/light-dark()）
- 级联：三 Origin 双键排序（normal 升序 Default < Stylesheet < Inline；important 升序 Stylesheet < Inline < Default；含 !important 交织用例，以浏览器为基准验证）
- 布局：taffy 0.14 可用面 = flex / grid / block、absolute / relative 定位、min/max/aspect-ratio、gap、overflow 裁剪
- 绘制：背景色、线性/径向渐变（sink 内 stop 加密对齐 sRGB 插值）、圆角、边框、阴影、opacity、图片、圆角矩形 clip
- 文本：单 style run、断行、字体注册与基础 fallback（测量内置，见 ADR-0006）
- 状态与动画：StateFlags、transition（可插值属性子集）

## T1 — v0.2+

:nth-child 系、属性选择器、keyframes 动画、@font-face、媒体查询扩展（pointer/hover 类）、容器查询、bidi 与多 run 混排、flex/grid 子项 z-index（无 position）、transform/filter 触发的 stacking context

## T2 — 暂缓（记录重估条件）

sticky / fixed（依赖滚动语义的完整所有权，滚动偏移已按 ADR-0005 归宿主，重估时补滚动容器模型）、float、table 布局、multi-column（taffy 无对应算法）、打印

## MVP 实现偏差核对（T4/T6 落地后现状）

- 阴影：无模糊半径、无 inset（vello 0.10 无内置高斯模糊）；`PaintOp::Shadow` 以偏移半透明矩形近似。
- 边框：四边独立（`PaintOp::Border` 携带每边 `BorderSide{width,style,color}`，none/0 宽边由 sink 忽略）；solid 与 dashed/dotted 同走「角弧+直线」中心线描边（圆角弧三次贝塞尔近似），角弧按顺时针归属（TL→top、TR→right、BR→bottom、BL→left）；简化偏差：多色相邻边的角部覆盖取后画方，不做对角线混合，dashed 角部接合为近似。
- 渐变：radial 语义完整（T4c：`circle|ellipse` + `closest/farthest-side|corner` / 显式半径 + `at <position>`，paint 层按盒子解析为绝对 center/r；ellipse rx≠ry 由 sink 画刷 x 向缩放近似）；stop 缺省位置按 CSS 语义均匀补位；插值色空间 sRGB。
- 文本：`PaintOp::Text` 经 sink `VelloTextSystem`（parley 0.11 排版 + DrawGlyphs）落字形（零副作用——系统字体禁用、字体字节由宿主双推 engine 测量/sink 绘制）；文本测量亦内置（text.rs），未推送测量的文本叶自动测量。富文本 spans（T5c-1）：`StyleNode.spans` 字节区间声明以节点基样式为 parent 复用 `compute_node` 级联求解，`PaintOp::Text.spans` 携带绘制期终结样式，sink 按 run 文本区间选色/推样式；测量按字节区间吸收 span 度量。换行（T5c-2）：white-space: normal 的自动测量文本叶在 pass1 布局后按包含块内容宽重测量并按需二次布局（包含块内容宽 = 父 border-box − 父左右 padding − 已生效 border，border-style 为 none 时宽归零、初始 medium 不计入；shrink-to-fit 父宽受无界文本影响的场景仍为近似）；宿主 `set_leaf_measure` 不参与自动重测。span 区间契约：`insert` 校验 UTF-8 字节边界/有序/不越界（无文本节点的 span 一律非法），违规返回 `ContractError::InvalidSpan`。未注册字体对应的泛族（缺省 sans-serif）测量为 0 尺寸——宿主应显式指定已注册族名或注册匹配泛族的字体。绘制侧 `PaintOp::Text.max_advance` 与测量共用同一约束保证折行一致（sink 用 `positioned_glyphs`，已含 run 偏移与基线）。离屏目验通路：`cargo run -p style-engine-demo --example snapshot`（vello `render_to_texture` → 纹理回读 → PNG；存储纹理路径要求 Rgba8Unorm，快照色彩较窗口路径偏亮属已知伪影）。字体资产：demo 内嵌 DejaVu Sans Regular/Bold（许可证见 `crates/style-engine-demo/assets/fonts/LICENSE-DejaVu.txt`）。
- opacity / 背景图片：属性未实现（BackgroundImage 仅 Gradient/None；Opacity 未映射）。
- z-index / 层叠（ADR-0008，CSS 2.1 Appendix E 简化三带）：SC 触发 = positioned 且数字 z、opacity < 1（clamp [0,1]）；带序 Neg（负 z 升序、等值树序）→ Flow（in-flow 树序）→ Pos（auto/0 树序在前、正 z 升序在后，非定位 SC 触发者键 0 树序）；SC 子树经 paint 递归天然原子。opacity 经 `PaintOp::PushOpacity{alpha}/PopOpacity` 层对（sink 用 vello `push_layer` alpha，快照目验通过）；`z-index: auto` 物化为 `ZIndex(None)`（与缺席等价、不触发 SC），数字为 `ZIndex(Some)`。残余偏差：flex/grid 子项的 z-index（无 position）不生效，transform/filter 触发者待对应属性落地。
- 滚动容器（ADR-0007）：overflow ∈ {auto（解析归一为 scroll）, scroll} 两轴独立识别；偏移归宿主（`set_scroll_offset`，引擎不夹紧），量程经 `Frame.scrollable`（key → 各轴最大滚动量，px）上报——padding box 与全部后代 border box 并集相对 padding box 的最大超出（绝对定位后代并入为近似）；hidden/clip 仅裁剪、不上报量程；滚动条 overlay 式、宿主所有、不占布局空间；sticky 契约记录（后布局位移 pass）、实现停泊。绘制 PushClip → PushScroll{dx,dy} → PopScroll → PopClip，偏移变更不触发重排。
- 布局映射：display:inline 缺席（统一块化）；calc 含百分比时百分比基按 0 扁平化——taffy 0.14 的 calc 为类型擦除指针 + 宿主回调求值（`CompactLength::calc(*const ())`、`traits.rs calc(val, basis)`，布局中以 parent_size 为基调用），接入需指针所有权约定与 unsafe 面，待 calc 正式特性票落地后以直通替代扁平化；vw/vh 已按视口解析。shrink-to-fit：块级流 width:auto 拉伸（正确）；absolute width:auto 经 taffy 绝对算法 + 我们的测量闭包近似（max-content 由无界测量给出，min-content 下限未实现）；parley 0.11 无 min/max content API——min-content 候选设计 = `break_all_lines(Some(0.0))` 的最宽行（0 宽行为待验证），落地路径为两阶段换行框架的第三 pass。合成视口根：taffy 根之上另有 ICB 节点，树根自身 margin 得以生效；taffy `location` 为父相对坐标，collect 沿树累计祖先偏移输出视口绝对坐标；margin 初始值为 0（CSS），显式 auto 才触发定宽块级盒居中。
- 滚动：`PushScroll/PopScroll` 折叠为坐标平移；滚动语义归宿主（ADR-0005）。
