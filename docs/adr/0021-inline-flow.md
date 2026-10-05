# ADR-0021: IFC 行内流 v1（F1）——引擎侧行打包

- 状态：已接受（E5 后 F1 批；round 10）
- 关联：ADR-0019（float 引擎侧结算先例）、第五批⑧（display:inline 契约化——本 ADR 部分取代其「纵向堆叠」现状）

## 背景（D0 盘点）

31 项缺口之「IFC 行内流」。现状：`display:inline/inline-block/…` 在
property.rs :1953-1969 归一 Block/Flex/Grid（第五批⑧契约）——子级纵向
堆叠无行盒；taffy 0.14 无 inline 支持（grep 全源零命中：无 InlineBox、
无 Display::Inline）；文本叶=独立 taffy 块子（tree.node.text +
TextSystem::measure_rich parley 0.11.1 测量，engine frame remeasure
:1424-1539 施用尺寸）；InlineBox 全仓零使用。

## 决策

### D1 架构 = 引擎侧行打包（settle_lines，浮盒先例）

taffy 无原生 IFC → 引擎 `settle_lines` pass（frame 内 settle_floats
之后、文本 remeasure 之前）：块容器子序分类行内参与者 → 贪心行打包 →
Position::Absolute+inset 锚定（ADR-0019 浮盒同式）。否决 parley
InlineBox 单布局合并方案（文本叶折叠进父布局 = 绘制管线大改、跨叶
样式区间重组、span_styles 语义重造——当量超批）。

### D2 Display 语义面（0.x 增量）

`Display` +2 变体：`Inline`（连续性标记：盒透明、扁平化其文本叶后代
参与父运行；inline 内块级后代=v1 边界=运行终止）/`InlineBlock`（原子
行内盒：taffy 布局尺寸参与行打包）。parse："inline"→Inline、
"inline-block"→InlineBlock；inline-flex/inline-grid/inline-table 维持
归一 Flex/Grid（原子化=偏差在案）。layout.rs：Inline/InlineBlock →
taffy Block（零布局变化）。cascade/继承不变（display 非继承）。

### D3 行打包算法（贪心）

逐块容器（豁免 table/multicol）子序分类：文本叶（tree.node.text
Some 非空）/InlineBlock 原子盒/Inline（扁平化）。cursor(x, y,
line_h)：
- 文本叶：`measure_rich(text, cs, spans, Some(content_right − x), env)`
  → (w, h)；置 (x, y)；x += w；line_h = max(line_h, h)；多行叶
  （h > 单行）→ x = content_right（行满，后续参与者下行）。
- 原子盒：(bw, bh) = taffy 布局；x + bw > content_right 且
  x > content_left → 换行（y += line_h、x = content_left、line_h = 0）；
  置 (x, y)；x += bw；line_h = max(line_h, bh)。
- white-space: nowrap → 跳过换行测试（measure avail=None 不折行）。
- 定位 = Position::Absolute + inset（父 padding box 相对）；文本叶
  width=length(w)（测量 max 行宽）+ height=length(h)；幂等 = 每帧
  restyle map_style 重置 + 本 pass 重施（无 taffy Style 缓存——E4
  E0277 教训）。

### D4 行高与对齐（v1 偏差在案）

行高 = max(参与者测量高)（line-height 精确行盒参与 = 偏差）；
vertical-align = TOP 对齐（baseline 基线对齐延后 = B 级偏差）；
叶间强制断行（pre 之换行符在叶内由 parley 处理；跨叶 `<br>` = 边界）。

### D5 否决项

taffy InlineBox（不存在）；parley 单布局合并（D1）；BFC 完整行内
语义（匿名块盒切分等——后续批按需）。

## 锁定测试

tests/css_inline.rs 七件：①文本+inline-block 同行（x 精确）②两原子盒
容器不足换行（y=首盒高）③文本叶续 inline-block 之后同行（x 偏移）
④多行文本推后续参与者下行 ⑤Inline 元素扁平化（span 内文本+后续叶
同行）⑥nowrap 不换行 ⑦无行内参与者零回归。

## 0.x 破坏性

`Display` +2 变体（non_exhaustive 增量）；"inline"/"inline-block" 解析
结果变体名变（Display::Block→Inline/InlineBlock——match 面调整）。

## 落地记录（F1 收口）

实现落位（与 D1-D4 决策的偏差修订）：
1. 参与者分类=文本检查先于 display（带文本节点即叶参与者；display
   inline/inline-block 无文本节点=盒参与者）——D2 的「inline 扁平化
   子孙叶」简化为「inline 组盒」（其子叶保持组内块流，remeasure 继续
   管）：taffy absolute 锚定相对直父，跨节点锚定（把 inline 元素内部
   叶锚到外层容器）不可行；组盒机械=原子盒同式（MaxContent 探针）。
2. 叶原子式参与：声明宽/高叶（第五批⑥声明优先）=按声明宽折行测量、
   盒用声明尺寸（map_style ts0.size + `Dimension::into_option`——
   taffy dimension.rs 仅 Length 变体返 Some，percent/auto→None）。
3. 盒自然尺寸=MaxContent 探针 + 子树文本叶 measure_rich(None) 宽 max：
   taffy 无文本内在尺寸（叶宽只在 remeasure 注入），纯探针得 0 宽
   （inline_group 测试 w=0 实锤）；单叶组/盒精确、多叶组宽=max（非流
   宽和）=D4 追加偏差。
4. 帧间幂等：增量布局下 taffy 样式非每帧全重建 → 上帧 Absolute 覆写
   存续 → prev_participants=mem::take 快照，运行收集的 is_absolute 豁
   免加 !prev_participants.contains。
5. 容器 min_height=打包行高（(y+line_h)−content_top，仅未声明高且未
   声明 min-height 容器）：行内内容出流会使 auto 高容器塌陷（浮盒本
   不贡献父高故 floats 无此步）；taffy 取 max(auto 内容高, min_height)。
6. 行盒溢出续排：Box 换行判定加 x≤content_right（行已溢出（nowrap
   叶）→ 后续盒继续同行，CSS 真行为；nowrap 锁测试驱动）。

排障弧：
- E0599 三连：WhiteSpace::NoWrap（非 Nowrap）、AvailableSpace 无 Auto
  （探针高用 MaxContent）、Dimension=struct 非 Length 变体枚举（读值=
  into_option）。
- E0502 ×3：pl=&Layout 借用 self.taffy 跨 set_style/compute_layout 存
  活 → match 拷标量元组即刻结束借用。
- 锁测试 3 败根因：①容器 min_height 未生效=declared_mh 用
  cs.get().is_some() 恒真（ComputedStyle 槽位初值填满）→
  has_declared_len ②组盒探针 w=0=taffy 无文本内在尺寸 → 叶测宽 max
  ③nowrap 测试文本太短（296<300）→ 文本加倍。
- lib 回归 2 败=语义冲突（非缺陷）：text_leaf_block_stretch（双叶容
  器被 F1 行打包——第五批⑥纵堆叠被取代）→ 测试结构化（#3 套
  wrapper 单叶隔离，单叶拉伸契约保全）；display_inline_contractual
  （块化堆叠契约被 F1 行盒并排取代）→ 重写
  display_inline_line_participation_f1。

锁定：tests/css_inline.rs 七件全绿；全量 **417/0**（基线 410+7）。

0.x 破坏性（登记）：Display +2 变体；"inline"/"inline-block" 解析结果
变体名变；第五批⑧契约（inline 块化堆叠）由 F1 行盒并排取代。
