# CSS 特性注册表（子集边界的权威定义）

本文件是"超出子集"的唯一权威来源：ADR-0004 的容错边界、ADR-0003 的零容忍作用域、v0.1 路标三者共同引用它。用例 manifest 声明的特性必须能对应到这里的条目；注册表变更必须同步更新对应用例的 in-subset 声明。

## T0 — v0.1（conformance 零容忍覆盖）

- 语法层：真实 CSS 文本；选择器子集 = type / class / universal / 伪类（:hover :active :focus :disabled :checked）/ 后代 / 子代 / 分组；简写展开（margin padding border background color font）；CSS 宽关键字（inherit/initial/unset/revert）；custom properties + var()
- @media 子集：width/height、prefers-color-scheme、prefers-reduced-motion（条件值由 Environment 提供）
- 值与颜色：px/em/rem/%/vw/vh；calc() 基础四则；color crate 全谱（hex/rgb/hsl/oklch/color()/light-dark()）
- 级联：三 Origin 双键排序（normal 升序 Default < Stylesheet < Inline；important 升序 Stylesheet < Inline < Default；含 !important 交织用例，以浏览器为基准验证）
- 布局：taffy 0.14 可用面 = flex / grid / block、absolute / relative 定位、min/max/aspect-ratio、gap、overflow 裁剪
- 绘制：背景色、线性/径向渐变（sink 内 stop 加密对齐 sRGB 插值）、圆角、边框、阴影、opacity、图片、圆角矩形 clip
- 文本：单 style run、断行、字体注册与基础 fallback（测量内置，见 ADR-0006）
- 状态与动画：StateFlags、transition（可插值属性子集）

## T1 — v0.2+

:nth-child 系、属性选择器、keyframes 动画、@font-face、媒体查询扩展（pointer/hover 类）、容器查询、bidi 与多 run 混排、z-index / stacking contexts

## T2 — 暂缓（记录重估条件）

sticky / fixed（依赖滚动语义的完整所有权，滚动偏移已按 ADR-0005 归宿主，重估时补滚动容器模型）、float、table 布局、multi-column（taffy 无对应算法）、打印

## MVP 实现偏差核对（T4/T6 落地后现状）

- 阴影：无模糊半径、无 inset（vello 0.10 无内置高斯模糊）；`PaintOp::Shadow` 以偏移半透明矩形近似。
- 边框：四边独立（`PaintOp::Border` 携带每边 `BorderSide{width,style,color}`，none/0 宽边由 sink 忽略）；solid 与 dashed/dotted 同走「角弧+直线」中心线描边（圆角弧三次贝塞尔近似），角弧按顺时针归属（TL→top、TR→right、BR→bottom、BL→left）；简化偏差：多色相邻边的角部覆盖取后画方，不做对角线混合，dashed 角部接合为近似。
- 渐变：radial = 正圆、半径取对角线/2（farthest-corner 近似）；stop 缺省位置按 CSS 语义均匀补位；插值色空间 sRGB。
- 文本：`PaintOp::Text` 经 sink `VelloTextSystem`（parley 0.11 排版 + DrawGlyphs）落字形（零副作用——系统字体禁用、字体字节由宿主双推 engine 测量/sink 绘制；无界宽度单行）；文本测量亦内置（text.rs），未推送测量的文本叶自动测量。
- opacity / 背景图片：属性未实现（BackgroundImage 仅 Gradient/None；Opacity 未映射）。
- z-index：已解析物化，绘制仍按树序；stacking context 属 T1 票据。
- 布局映射：display:inline 缺席（统一块化）；calc 含百分比时百分比基按 0 扁平化；vw/vh 已按视口解析。
- 滚动：`PushScroll/PopScroll` 折叠为坐标平移；滚动语义归宿主（ADR-0005）。
