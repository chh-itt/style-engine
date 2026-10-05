# ADR-0023：F3a 命中测试（hit_test）与序列化（serde feature）

状态：已接受（F3a 批次设计；落地记录随批次收官追加）

## 背景

31 项 gap 的 F3 长尾批。桌面 GUI 语义层定位下，宿主需要「点在哪 → 是哪个
节点」的标准命中查询（交互定位/光标/选择起止）。DisplayList 是绘制真值
（含 clip/scroll/transform 状态流），但逐 op 反走重建命中逻辑 = 每个宿主
重复实现且易与绘制语义漂移；引擎侧统一提供。序列化方面：DisplayList/
ComputedStyle 的调试转储、跨进程传递、测试快照需要 serde；0.x 阶段以
feature 门控避免默认依赖成本。

## 决策

### 1. hit_test = paint 期命中几何表（非 DisplayList 反走查）

- PaintOp 不携带 node_id（绘制基元与节点解耦是既有架构边界）；反走查
  方案要么给 op 加 owner 字段（破坏所有 op 与 sink）、要么启发式匹配
  （漂移）。**弃**。
- 采用：paint_node 递归绘制时已有 mat/clip/scroll 上下文，同步收集
  `HitRect{node_id, mat: [f32; 6], box_rect: [f32; 4]}`（border-box +
  当前复合矩阵 + 祖先 clip 矩形链）入 `Vec<HitRect>`——绘制序即文档
  序，后绘 = 更顶。
- 公共 API：`StyleEngine::hit_test(&self, x: f32, y: f32) ->
  Option<HitTestHit>`；`HitTestHit { node_id: NodeId }`（v1 最小面，
  局部坐标/深度次批）。命中语义：**逆序**遍历表（顶优先），复合矩阵
  逆映射点到节点局部坐标判 border-box 内，且点须在全部祖先 clip 矩形
  内；首个命中即返回。
- 语义对齐（零额外逻辑即正确）：`visibility: hidden` / `display: none`
  子树不在表内（paint 期已跳过）；`pointer-events: none`（A1 语义位）
  收集期排除。
- 收集通道经 PaintCtx 透传（`Option<&mut Vec<HitRect>>`，None=零成本
  跳过）；PaintCtx 是 pub 结构体，新增字段=0.x 记录。

### 2. serde = feature "serde" 门控（绘制面先行）

- Cargo.toml 可选依赖 serde（derive）；`#[cfg_attr(feature = "serde",
  derive(Serialize, Deserialize))]` 门控在 paint 模块（DisplayList +
  PaintOp 全变体）。
- `AlphaColor<Srgb>`（vello peniko 类型，无 serde）→ paint 内新增
  `SerColor([f32; 4])` 包装型承载 serde 往返（cfg 门控，非 feature 时
  不存在）。
- ComputedStyle 值表（DeclValue 全族）次批（F3e）——本轮诚实记录，
  不假装全集。

## 测试

- hit_test 三锁：顶层命中（后绘节点优先）/clip 外不命中/pointer-events:
  none 排除。
- serde 往返锁：feature 门控下 DisplayList serialize→deserialize 相等
  （equality 断言；DisplayList 已是 PartialEq）。

## 否决项

DisplayList 反走查命中（op 无 owner，加字段=全面破坏）；winit/UI 框架
集成层（宿主职责）；ComputedStyle 值表 serde（本轮，F3e 再评）。

## 0.x 破坏性

- `PaintCtx` 新增命中收集通道字段（pub 结构体字段新增）。
- 新增公共类型 `HitRect` / `HitTestHit`（增量，非破坏）。

## 落地记录（F3a 批次收官）

- **hit_test 全链落地**：`HitRect{node_id,x,y,w,h,clips:Vec<[f32;4]>}`/
  `HitCollector{rects,clips}`（Default）/`HitTestHit{node_id}` 三型
  （paint.rs，TextShadowPaint 后）；`PaintCtx.hit: Option<&RefCell<
  HitCollector>>`（None=零成本）；收集点=paint_node radius 解析后
  （pointer-events none 经 `DeclValue::PointerEvents(PointerEventsKind::
  None)` 排除、clip 链快照 clone）；子树 PushClip/PopClip（overflow
  Hidden|Clip|Scroll）链 push/pop 同步；engine `hit_rects: Vec<HitRect>`
  字段（column_rules 后+init）+frame 内 hit_cell 构造/透传/
  `into_inner().rects` 入库；`hit_test(&self,x,y)` 逆序+clip 全含；
  **补充 API `node_id(&self, key: &K) -> Option<NodeId>`**（实现期发现：
  hit_test 返回 NodeId 而宿主无键映射通道则不可用——key_to_node 直读，
  本 ADR 决策 1 的补充，非变更）。
- **测试实值**：tests/css_hit_test.rs 三件绿（topmost_wins=absolute
  后绘兄弟重叠区命中 b、clip 外不命中、pointer-events none 穿透）；
  全量 workspace 434 绿/37 套件 0 失败。
- **范围拆分（如实记录）**：serde（本 ADR 决策 2）**拆独立批 F3a2**——
  PaintOp 全变体（FillRect/Gradient/Shadow/Image/Border/Text/PushClip/
  PopClip/PushOpacity/PopOpacity/PushTransform/…非穷举）类型化镜像需
  ~15 投影型（Gradient/BorderSide/TextSpanPaint/FontFamilyList/ImageRes/
  各 Kind 枚举）≈400 行，工程量超 hit_test 本体；按「每批完整绿」纪律
  独立成批续作（同 ADR、非缩水）。设计预勘：投影走 typed tagged enum
  （serde tag/content），色=[f32;4]、ImageRes.pixels=Vec<u8> 直序列、
  往返锁测试断言 DisplayListDump 相等。
- **0.x 破坏性兑现**：PaintCtx +hit 字段（六测试构造点同步 hit: None）。

## 落地记录补记（F3a2 serde 收官）

- serde 决策 2 全量落地：paint_dump.rs（cfg(feature="serde") 独立模块
  ~690 行）+ Cargo.toml serde 可选依赖（version 1 derive）+ feature
  `serde = ["dep:serde"]` + dev-dep serde_json（仅测试 JSON 通道）。
- 投影面：LPDump{unit,value}（LP 九单位 px/em/rem/%/vw/vh/cqw/cqh/cqi，
  load 未知→None 丢弃）、ColorValueDump（tag=kind content=v：
  CurrentColor/Absolute/LightDark）、GradientDump（Linear{deg}/Radial{
  shape,size,position}/Conic{from_deg,position}+RadialSizeDump Named/
  Explicit）、BorderSideDump、TextSpanDump、TextDecorationDump、
  TextShadowDump、RadialGeomDump、ConicGeomDump、OpDump（tag=op，14
  变体+Image pixels Vec<u8> 直序列+Unknown{name} 兜底）。
- 设计更正（实施期发现）：①`#[non_exhaustive]` 仅对外部 crate 强制
  通配——同 crate match 全变体已知，初版 11 处 `_` 通配臂=unreachable
  pattern 警告全删（收益：新增变体时编译器强制更新镜像）；②serde
  tag 冲突：OpDump::Unknown{op} 字段名撞 tag="op" → 改 name；③
  BorderSideDump/TextDecorationDump 含 String 不可 Copy（E0204）→ 去
  Copy 保 Clone。
- 锁测试：tests/css_serde_dump.rs 1/1（富样式叶渐变+边框+圆角+透明+
  变换+装饰+阴影+overflow → 结构往返 assert_eq + JSON 往返 assert_eq
  +"op":"text" tag 检查）；全量 workspace 435 绿/38 套件 0 失败
  （--features style-engine/text,style-engine/serde）。
