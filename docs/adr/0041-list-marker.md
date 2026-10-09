# ADR-0041：列表闭环——display:list-item、::marker 与 list-style 全链

- 状态：已接受（2026-10-09，P9-3）
- 关联：ADR-0033/0036（UA 表与 content 伪节点机制）、ADR-0038（counter-style
  全链——本 ADR 复用其格式化层）、css-lists-3 ED（2026-04-28）、
  css-counter-styles-3、FEATURES.md 偏差核对节、SINK-MATRIX.md
- 背景：MVP 明确排除列表（T2：li 不发 list-item、无 ::marker、无 list-style）。
  counter-style 格式化层（P8，marker_text/format_counter_with）已就绪，本批
  补齐列表语义闭环：值面、伪节点、标记渲染、隐式计数器、UA 表。

## D1：值面——三物理槽 + Display::ListItem + 第 40 简写

- `PropertyId` 增 `ListStyleType/ListStylePosition/ListStyleImage`（slot
  173/174/175，ALL 尾追加，动画描述符整体后移 176..182，SLOT_COUNT 180→183；
  `from_css_name` 的 OnceLock 二分索引由 ALL 自动纳入）。三属性均继承；初始
  disc / outside / none（css-lists-3 §3.3–§3.5）。
- `DeclValue` 增 `ListStyleType(Option<ListStyleTypeValue>)`
  （`ListStyleTypeValue::Name(String)` 名字解析期不校验注册表——@counter-style
  可后置定义，未知名使用期回退 decimal（css-counter-styles-3 §2）/ `Str(String)`
  字面形无 prefix/suffix）、`ListStylePosition(Inside|Outside)`、
  `ListStyleImage(Option<BackgroundImage>)`（复用背景图像值文法单源）。
- `Display` 增 `ListItem`（parse "list-item"；map_style → taffy Block——列表项
  是块级盒，marker 与隐式计数由引擎管线承担）。
- `list-style` 简写（第 40 项）：三长手 `||`；**none 二义消解**按 css-lists-3
  §3.6——none 依序应用于未设置的分量（`none disc`→image=none+type=disc；
  `none`→双双 none；`none disc url(b)`→语法错误）。实现上 none 词面在分量
  try_parse 前拦截计数，循环后消解。锁测试 7 代表值。

## D2：::marker 伪节点基础设施

- `PseudoWhich::Marker`（tree.rs）；伪节点 key=(host,2)，children 序
  [marker, before, 宿主子, after]（css-lists-3 §3.1：marker 是首子盒、位于
  ::before 之前）。selector.rs 四处同步（parse/ToCss/match——作者
  `li::marker { color }` 经 originating_element 既有通路命中）。
- **无条件创建**（与 ::before/::after 同型）：materialize_pseudos 的
  any_pseudo 快路径增 `any_list_item` 条件（styles 表有 ListItem 或 UA 表
  装载）——UA 表 `li{display:list-item}` 不含伪元素规则，marker 仍须存活。
  每帧稳态幂等（differential 增量≡全量保持）。
- 非列表项宿主的 ::marker 内容计算为 none（§3.1）——由 D4 内容算法的宿主
  检查承担，节点存在但抑制成盒。

## D3：标记渲染 = 绘制层合成（redesign，非 taffy 盒）

初版把 marker 做成独立 taffy 文本盒参与行打包——实测失败：引擎 IFC 不合并
宿主自身文本与子伪节点（ADR-0034 基线：伪节点盒下推宿主子），marker 独立
盒占据独立行、与宿主文本同 x 重叠。redesign 定案：

- marker 伪节点 taffy **恒隐藏**（map_style Display::None），不参与布局与
  行打包；文本/测量照常经 sync_pseudo_text→apply_pseudo_text 入树节点与
  measures（复用既有通路）。
- paint 层在宿主首行内容左缘**合成** marker Text op（样式全取 marker 自身
  computed style——作者 `li::marker{color/font-size}` 生效）；宿主文本 op
  x += marker 前进宽；leaf_wrap_width 同步减前进宽（换行宽度让位）。
  marker 伪节点自身在 paint_node 跳过（防零尺寸布局条目双重发射）。
- **inside 语义**由此天然成立（marker 文本在首行内容左缘、内容接排）；
  **outside**（悬挂于行盒外、inline-start 侧负缩进）按 inside 渲染
  ——css-lists-3 §3.5 自认 outside 布局 handwavey，**B 级豁免**；重估条件
  = IFC 重构（BREAKING-POLICY T-契约「IFC 推翻 inline 归一」一并解决）。
- SINK 面：marker 以普通 Text op 抵达 sink，soft/vello 零改动
  （SINK-MATRIX 无新行）。

## D4：标记内容算法与隐式 list-item 计数器

- 隐式计数器（css-lists-3 §4.6）：`display:list-item` 元素在
  eval_content_walk 的非伪节点分支自动 +1（显式 counter-increment 含
  list-item 时不叠加）；counter-reset: list-item N 照常可用。
- `eval_marker_text` 按 §3.2 首个真条件：作者 content（content-list 非空）
  → 走既有 eval_pseudo_content；list-style-image 有效 → 无文本（盒由 D6
  注入声明承担）；list-style-type none → 抑制；`Str` 字面；`Name(n)` →
  list-item 计数值 + `marker_text(n, v, registry)`（表示+prefix+suffix，
  counter_format.rs 新 pub fn——未知名回退 decimal affixes，none 返回空）。

## D5：UA 表（Chromium 对齐 + 有意收敛）

`li { display: list-item }`；`ul{list-style-type:disc}`、`ul ul{circle}`、
`ul ul ul{square}`、`ol{list-style-type:decimal}`。**不注入**
`padding-inline-start:40px`（Chromium 有）：会扰动既有布局基准与 golden，
缩进属宿主排版决策——范围界定在案（FEATURES.md UA 表条目）。UA ::marker
义务规则（unicode-bidi:isolate 等）以**声明注入**实现其中可达部分：
white-space:pre 恒注入 marker 伪节点（保 suffix 尾空格；声明层级注入 →
作者不可覆盖，**B·豁免**，重估条件=伪元素 UA 规则通路）。

## D6：list-style-image = 注入声明近似（B 级）

materialize_markers 时按宿主**上一帧** list_style_image 向 marker 节点注入
`background-image` + `width/height:1em` 声明（默认 object size 1em 方，
css-lists-3 §3.3）。偏差：首帧无图（styles 表尚未计算）；尺寸非内容固有；
em 以宿主字号解析。**B 级**：重估条件=伪节点替换元素通路（匿名行内替换
元素 + U+0020 文本 §3.2②）。image 在场时文本标记不发射（与 §3.2 条件序
一致）。

## D7：运行收集器收窄——无歧义块级终止行内运行

行内运行收集（settle_lines）原「带文本子节点一律叶参与」使相邻文本
li 被打包进同一行（块堆叠被破坏——li 装载 UA display:list-item 后暴露）。
收窄：computed display 为 **ListItem/Table/TableRow/TableRowGroup/
TableCaption** 的带文本子节点**终止**运行（CSS 2.1 §9.2.1：块级盒文本属
自身盒）；`Block+文本` 仍按 F1 契约作匿名行内叶参与（css_inline 契约与
无声明文本运行依赖此近似——其彻底修正属 IFC 重构 T-契约，不在本批）。
TableCaption/TableRowGroup 虽 map_style 同 Block，同样无歧义块级，一并收窄。

## 否决替代

1. **marker 独立 taffy 盒**（初版）：IFC 不合并宿主文本与子盒（ADR-0034
   基线），独立盒占独立行——实测重叠/断行双重缺陷，见 D3 redesign。
2. **::marker 做 full box 通路**（伪元素内容+定位）：outside 悬挂需要
   inline-start 负缩进与行盒协作，taffy 无该原语；等 IFC 重构后重估。
3. **UA 表注入 ::marker 规则**（真选择器）：引擎 UA 表按元素选择器匹配，
   伪元素规则通路不存在；以声明注入近似（D5），通路落地后迁移。
4. **list-style-image 走 ::marker content**：content 值文法不含 image
   上下文（尺寸/替换基线语义不同）；注入声明更贴近盒语义。

## 影响

- 值面：+3 物理槽（SLOT_COUNT 183）、Display::ListItem、第 40 简写、
  三 parse 三访问器；反序列化面无变化（DeclValue 不出 serde 边界）。
- 行为变化：li 由普通块变列表项（marker + 隐式计数）；`list-style` 系
  声明由 IACVT 变为生效；相邻块级 li 堆叠（D7 修复）。
- 兼容：无 UA 表时 li=Block（近似旧行为，无 marker）；装 UA 表即获得
  完整列表语义。
- 测试：css_lists.rs 9 件（数字序/嵌套族/none 抑制/字面/作者色/
  display:list-item+counter-reset/inside 首行几何/两帧幂等/非列表项抑制）
  + decl 锁 7 代表值；golden 无扰动（既有用例不含列表）。
- 文档：FEATURES.md 偏差核对节「列表闭环」条目（B 级残余：outside≈inside、
  white-space 注入不可覆盖、image 首帧无图/1em 近似）；SINK-MATRIX 无新增
  （marker=引擎侧 Text op 合成，sink 无感）。
