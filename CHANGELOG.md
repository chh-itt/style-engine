# 更新日志（CHANGELOG）

本 crate 不发布 crates.io（契约 C4/C7），版本号仅作 API 演进坐标。格式遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；语义化版本：
**MInor 位 = 特性新增，Patch 位 = 修复，Major 位 = 破坏性变更（未发生）**。

所有已发布提交见 https://github.com/chh-itt/style-engine/commits/main 。

## [Unreleased]

### 阶段3 — API 冻结（C3）

- **#[non_exhaustive] 全量公共枚举**（style-engine 40 项：PropertyId/DeclValue/PaintOp/
  Display/Position/Overflow/TrackSize/GradientKind/ContainerFeature/MediaFeature/
  LengthPercentage/CalcNode/ColorValue/… 与先期已标的 PaintOp/ContractError；soft sink
  无公共枚举）。变体集=演进面：宿主 `match` 必须带通配臂，新增变体不构成破坏性变更。
  参考宿主已按此修配：vello sink（TextAlign→Start / FamilyName→sans-serif /
  GradientKind→缺省径向几何）、soft sink（GradientKind→缺省径向几何）。
- **#![deny(missing_docs)]**（style-engine + style-engine-soft）——公共项文档强制；
  存量 0 缺失，新增 pub 项无文档即编译失败。
- 公共枚举冻结策略记录于 `crates/style-engine/src/lib.rs` 模块头：枚举全量
  non_exhaustive；结构体按宿主构造面决策（StyleNode 等宿主可构造类型保持穷举）。

### 阶段2 — 特性扩展（0.1.0 之上，全部为增量）

- **容器查询（阶段2③，5b30a4d）**：@container 尺寸查询 MVP——条件解析（命名段/逗号
  OR/and 并置/值在前反序翻面/旧形 min-* max-*）、ContainerCtx 级联过滤（有名段沿容器栈
  rev() 查找、无名段取栈顶、InlineSize 容器门控 Block 轴与 orientation）、
  container-type/container-name/container 简写、帧内尺寸快照收敛环（上限 3 pass，稳态
  零额外 pass）。conformance 新增 container-query 用例（Chromium 153 四盒零超差
  Numeric+Pixel 双通道）；锁定测试 13 项。B 级偏差：无强制 size containment（v1 边界
  与重估条件见 FEATURES.md T2 段）。
- **var() 简写（阶段2②，0813900）**：含 var() 的简写不再整条拒绝——解析期落
  PendingShorthand 挂起声明，计算值期代换后展开逐槽竞争（css-cascade 挂起代换值语义）；
  代换失败/展开失败/长手不在展开集 → IACVT。顺带修复 !important 尾部捕获缺陷三条路径
  （custom property / var() 长手 / 纯简写）——finish_after_capture 统一处理。
- **Grid 重复轨道（阶段2①，229b426）**：repeat() 接受 auto-fill / auto-fit
  （TrackSize::RepeatAuto），布局期按可用空间定计数，语义对齐 Chromium 153；嵌套
  auto-repeat 解析期拒绝（CSS 规范禁嵌套）。
- **T2 排除项文档化（8a7118c）**：FEATURES.md T2 段重写为 16 项结构化清单（sticky/fixed、
  float、打印、3D 变换、滤镜效果本体、:has()、CSS 嵌套、容器单位与 style 查询、多列
  fragmentation、多动画组、direction、text-decoration、@import/@supports、字体描述符、
  背景图 repeat/size/position、git-lfs），每项记录当前行为与重估条件。

### 阶段1 — 正确性（三期⑥收束，61519ae）

- 滚动量程语义（ADR-0007）：Frame.scrollable 每轴上报内容并集超出 padding box 的幅度，
  transform 后代按祖先链复合仿射四角 AABB 并入、仅正向溢出计入（三期⑥ 61519ae）。
- absolute 包含块跳走（三期② 6515e5d）：包含块取最近 positioned/transformed 祖先
  （settle_absolute_anchors 帧内结构修正 pass，跨父重挂）。
- calc 槽位结算（三期③ 2b3a1e3）；表格深化（三期④a-d：colspan/行组/caption、rowspan
  跨行高度、匿名单元格）；多列深化（三期⑤a-c：顺序装箱二分平衡、break-inside、
  column-span:all、column-rule 三长手+简写）。
- conformance 零容忍扩展：absolute-anchor-jump / calc-slots / table-span / table-rowspan /
  multicol-* / transform-cb 等用例（Chromium 153 golden，Numeric+Pixel 双通道）。

## [0.1.0] — MVP（T0 收敛，二期⑤）

- **L1 样式代数**：真实 CSS 文本解析（cssparser 0.38）、选择器匹配（selectors 0.40，
  结构伪类+组合器）、三 Origin 级联、继承、custom properties（var() 长手）、
  @media 条件子集、@keyframes 动画（第五批⑰）。
- **L2 布局**（feature = "layout"）：taffy 0.14（flex/grid/block、absolute/relative、
  min/max/aspect-ratio、gap、box-sizing、margin collapse）；表格布局 v1（display:table
  族，两阶段列宽结算）；多列布局 v1（column-count/width 平衡）；文本栈（parley 0.11
  内置测量，UAX #14 断行，bidi first-strong，line-height/letter-spacing/text-align 消费）。
- **L3 绘制**：中立 DisplayList（PaintOp 24 变体：FillRect/Gradient/Shadow/Image/Border/
  Clip/Opacity/Scroll/Transform/Text/ColumnRule）；层叠三带（ADR-0008）；transform 2D
  （origin 消费，ADR-0009）。
- **双 Sink 架构**：style-engine-vello（GPU 参考实现）+ style-engine-soft（零依赖纯软件
  光栅化，像素确定性参照，第五批㉛）——sink 无关性契约实证。
- **零副作用契约**：时钟/窗口/字体/图片字节全由宿主推入；错误双轨（CSS 内容错误=
  ParseReport 容错，宿主违约=ContractError）；conformance Numeric 16 + Pixel 6 用例
  零 xfail（Chromium 153 golden）。

[Unreleased]: https://github.com/chh-itt/style-engine/compare/8a7118c...HEAD
[0.1.0]: https://github.com/chh-itt/style-engine
