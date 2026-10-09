# V1-SCOPE: v1.0 行为验收基准

- 状态：验收基准（1.0 对齐阶段产物，P9）
- 本文回答「v1.0 的 style-engine 行为上承诺什么、不承诺什么」。功能完整性
  以 FEATURES.md 注册表为准；破坏面预告以 BREAKING-POLICY.md 为准；上游
  等待面以 DEPENDENCIES.md 为准；sink 分工以 SINK-MATRIX.md 为准。本文只
  做验收口径，不重复登记细节。

## 1. v1.0 的定义

v1.0 = **框架无关 CSS 语义层的行为可信点**：在桌面 GUI 尺度（非浏览器尺
度）内，级联→计算→结算→显示列表全链的语义正确性达到「可作宿主布局的
唯一真值源」水平，且该正确性由机器验证体系锁定，而非抽检。

明确排除的度量衡：与浏览器逐像素对齐不是目标（CONTEXT.md 范围界定）；
打印/分页/3D/书写模式 fragmentation 等显式排除项见 §4。

## 2. 验收维度与达标证据（P9 后现状）

### L1 样式代数（级联内核）
- 声明解析：183 属性槽（ALL 176 位 + 7 动画描述符）、40 简写（含 font/
  grid/grid-template/list-style 全链展开）、var() 挂起代换、宽关键字、
  calc() 结算式（三期槽位化）、@media/@supports/@container/@layer/
  @keyframes/@font-face/@import/@counter-style/@property 全臂。
- 相对语义：bolder/lighter（css-fonts-4 §2.2.1 表单源）、larger/smaller
  （表步进+1.2 比例）、em/rem/cq/vw/vh 解析与结算期语义。
- 错误恢复：规则表级裸声明容错（顶层+条件组体+嵌套体失败声明恢复）。
- 达标证据：0 个开放 A 级偏差；fuzz_grammar/property_registry/
  cascade_layers/nesting_has_focus 等锁测试全绿。

### L2 布局结算（taffy + 结算层）
- 块/flex/grid/表格（wrapper/inner 双盒）/多列/浮动/绝对定位/IFC 行内、
  calc/表格/多列/浮动/行结算 DAG 化（SettlePassKind 拓扑）、容器查询
  （快照表失效+收敛环）、增量重样式（子树收窄+容器门控收窄）。
- 达标证据：differential 增量≡全量逐位等价锁（含随机两表覆盖）；
  css_margin_collapse 探针锁；perf_gate 四场景余量 ≥2.7×。

### L3 显示列表与双 sink
- 中立 DisplayList（21 PaintOp 变体）+ soft 真值参照 sink（纯 std、
  blend 18/18、filter 全函数、逐像素边框）+ vello GPU 投影 sink（上游
  能力边界内的全部消费）；SINK-MATRIX 政策（parity 分级+禁静默忽略
  门禁）。
- 渐变：停点文法补齐（hint/any-order/css-images-4 双位置 desugar）、双
  sink 共享均布单源、防御分支统一。
- 文本：parley 排版+CJK 词分段、span 富文本（色/字号/族/字重/斜体/行高/
  字距全消费）、text-transform/折行/truncation/::marker 绘制层合成。
- 达标证据：conformance 35 用例（numeric 35 + pixel 15）零 xfail；
  pixel_invariants 7 锁；golden 漂移 CI 门禁。

### 验证体系（制度化）
- 720+ 测试全绿（`cargo test --workspace --all-features --locked`）；
  CI 4 jobs（三平台 gate/msrv 钉 1.90/perf-gate/golden-drift）；
  clippy `-D warnings` 零告警；deny.toml 供应链门禁；FEATURES.md 偏差
  分级台账（A 必须解决/B 视觉豁免/C 近似）为唯一偏差账本。

## 3. v1.0 接受的偏差面

v1.0 不承诺零偏差，承诺「偏差全部在案且分级」：
- **B 级（视觉豁免）**：如阴影近似、outside marker≈inside、span 显式
  normal 行高回退、grid dense 拒绝、系统字体无 OS UI 映射等——逐项见
  FEATURES.md 偏差核对节，每项带机制描述与（多数）重估条件。
- **C 级（近似可保留）**：transform×自身滚动次序、scroll 量程近似等。
- **豁免项（B·豁免/C·豁免）**：经 ADR 决策显式接受并登记（如 marker
  white-space 注入不可覆盖、white-space 声明注入）。
- 新增偏差必须走 PROPERTY-CHECKLIST/FEATURES.md 登记 + ADR 决策，不得
  静默引入。

## 4. 显式排除项（不属 v1.0 验收）

FEATURES.md T2 段为准：@media print/分页、3D 变换、@container style()
查询、sticky 滚动吸附运行时、多列 fragmentation 深化、@page/@scope/
@namespace、@font-face 生命周期（unicode-range fallback/local()/font-
display/size-adjust）、书写模式 fragmentation、git-lfs。各项带重估条件。

## 5. 上游等待面（非本仓控制）

DEPENDENCIES.md 为准：vello filter 本体/plus-lighter、peniko Mix、parley
连字与 kinsoku、taffy 文本内在尺寸、字体度量直读等。这些项在 v1.0 表现
为文档化降级（warn-once/回退近似），不计为验收失败；上游成熟后按
FEATURES.md 重估条件逐项收口。

## 6. v1.0 之后的变更纪律

- 行为变化（含 UA 表、初始值、失效语义）一律走 CHANGELOG「行为变化」
  标注；0.x 阶段 Minor 允许破坏（BREAKING-POLICY），已预告五处 T-契约
  （IFC 重构/结算 DAG 化深化/多重背景 DisplayList/逻辑属性映射/多样式
  表 Epoch）。
- 版本号冻结在 0.1.0 直至维护者显式定版；本文档为定版时的验收凭据。

## 7. 修订

验收口径变化（新增验收维度、调整接受面）须修订本文并经 ADR 关联；偏差
面增减同步 FEATURES.md 台账。
