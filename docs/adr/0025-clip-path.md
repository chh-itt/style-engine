# ADR-0025: clip-path 裁剪形状（F3c）

状态: 已接受（本批实施）
日期: 2026-02（F3 批次）
关联: ADR-0001（DisplayList 中立性）、ADR-0008（层叠/裁剪）、ADR-0023（hit_test）、ADR-0024（背景全集先例：值族→计算→绘制→sink 四切片法）

## 背景

`clip-path` 现状（第四批④）：仅作为 stacking context 触发语义位——
`DeclValue::Effect(bool)` 存在性语义，任意值宽容接受（函数块经
skip_block_content 吞咽），不解析形状、不产生任何裁剪。31 项清单 F3c
要求：基本形状解析 + 绘制裁剪落地。

## 决策

### 1. 值族与解析（L1）

新值类型（property.rs）：

```rust
pub enum ClipShape {
    None,
    Inset { insets: [LP; 4], radius: Option<[LP; 4]> },   // inset( t r b l [round r]? )
    Circle { radius: ClipRadius, at: Position2D },        // circle( r? at p? )
    Ellipse { rx: ClipRadius, ry: ClipRadius, at: Position2D },
    Polygon { nonzero: bool, points: Vec<(LP, LP)> },     // polygon([rule,]? x y, ...)
    Other,                                                // url()/path()/未来=宽容吞咽
}
pub enum ClipRadius { Length(LP), ClosestSide, FarthestSide, ClosestCorner, FarthestCorner }
```

- 文法（css-masking-1 §5.1 / css-shapes-1 §3）：`<basic-shape> ||
  <geometry-box>`，次序不限；`none`。geometry-box ∈ border-box（默认）/
  padding-box / content-box / margin-box（**margin-box 降级 border-box**
  =B 级在案）；geometry-box 单独出现 = 矩形裁剪该参考盒。
- 值族规则：非 None/Other 的 clip-path **不继承**（css-masking-1 初始
  none）。
- `url()` / `path()`：解析宽容吞咽（与第四批④同型）→ `ClipShape::Other`
  + tracing 警告，语义 none（诚实边界：SVG clipPath 资源解析与 path()
  语法是独立工程，入 T2）。
- `inset()` 的 `round <'border-radius'>`：解析并精确消费（裁剪矩形带
  radius [f32;8]，复用 PushClip 既有能力——非近似）。
- `circle()/ellipse()` 的 `<length-percentage>` 半径：百分比轴向解
  （x 轴=参考盒宽、y 轴=高，css-shapes-1 §3.2.1）。
- `polygon()`：可选 nonzero|evenodd（默认 nonzero）+ 逗号分隔坐标对
  （每对 = x y，length-percentage 全值）；坐标对数 < 3 = 无效整条丢弃
  （Chromium 同判）。

### 2. 计算（L2）

- PropertyId +1 变体 `ClipPath`（ALL 尾部追加；动画描述符顺延 k+1；
  **SLOT_COUNT 159→160**，slot_alignment 锁同步——0.x 破坏性）。
- `ComputedStyle::clip_path() -> ClipShape`（初始 None）。

### 3. 绘制（L3）

- **新 PaintOp 变体** `PushClipPath { points: Vec<[f32; 2]> }`（视口
  坐标；PopClip 复用既有变体）。既有 `PushClip { radius: [f32; 8] }`
  矩形语义不动——圆/椭圆在 paint 期折算为 **64 段折线**（参数化
  cx+r·cos(θk)）：64 段多边形面积比 = (n/2π)·sin(2π/n) ≈ 0.99858，
  面积误差 0.14%、边缘弦高 ≈ r·(1−cos(π/64)) ≈ 0.12%·r，视觉不可辨
  （**B 级在案**）。理由：不为圆/椭圆扩 op 形状枚举，sink 增量面最小
  （一个新变体 × 两 sink + dump 镜像），既有 PushClip 全链路零改动。
- 参考盒解析：geometry-box → 背景批次既有 `background_box_rect`
  同语义（border/padding/content-box 三盒；margin-box→border-box）。
- `inset` 圆角矩形走既有 `PushClip{radius}`；circle/ellipse/polygon 走
  `PushClipPath`。裁剪作用于**该元素及其子树**（PushClip/PopClip 包裹
  子树绘制，与 overflow 裁剪同位同序——overflow 裁剪之后、背景之前；
  ADR-0008 层序）。
- 命中测试（ADR-0023）：HitRect.clips 为矩形链快照——PushClipPath 以
  其点集 AABB 参与链（**多边形精确命中=AABB 近似，B 级在案**）。
- 滚动量程（ADR-0007）：clip-path 为绘制期裁剪，不影响布局盒与量程
  （Chromium 同语义）。

### 4. sink 与 serde

- style-engine-vello：`PushClipPath` → BezPath 折线 push_layer（复用
  clip 层机制）。
- style-engine-soft：clip 栈增加多边形分支——逐像素 point-in-polygon
  （射线法，evenodd/nonzero 计数规则；soft 现有 clip 判定为函数栈，
  增量一个变体）。
- paint_dump 镜像（ADR-0023 决策 2 契约）：`ClipPathDump` 变体同步——
  non_exhaustive 同 crate 全变体枚举，编译器强制更新。

### 5. 破坏性与边界（0.x）

- **0.x 破坏性**：PropertyId +1（SLOT_COUNT 159→160）；
  `PaintOp` +`PushClipPath` 变体（non_exhaustive，宿主 match 需新臂）。
- B 级在案：圆/椭圆 64 段折线；margin-box 降级；多边形命中 AABB；
  url()/path() 无效果；transition/动画插值不做（离散值段进度 0.5，
  lerp_decl 既有行为天然覆盖）。
- clip-path 与 overflow 裁剪、border-radius 裁剪、transform 的复合：
  全走既有 PushClip/PopClip 栈语义（嵌套天然成立）；clip-path 与
  `clip: text`（背景批次）互不相干（不同属性）。

## 测试切片

①解析锁（四形全值/geometry-box 次序不限/margin-box 降级/url+path 宽容/
polygon 坐标对+fill-rule/坐标对<3 拒绝）；②计算锁（initial none/不继承/
slot_alignment 自动覆盖）；③绘制锁（inset→PushClip radius/
circle→PushClipPath 64 点端点锁/polygon 点序视口锁/参考盒位移/与
overflow 复合双 clip）；④sink 锁（dump 往返含 PushClipPath/soft
point-in-polygon 像素断言）；⑤全工作区绿。

## 落地记录

实施于 F3c 批次（切片①解析 → ②计算 → ③绘制 → ④sink/serde → ⑤锁测
试+全绿）。**两处决策修订**（实施期发现，均使方案更优）：

1. **PropertyId 复用（Option B）——§2「PropertyId +1 变体（SLOT_COUNT
   159→160）」作废**。编译期发现第四批④早已登记 `PropertyId::ClipPath`
   （ALL :443 / slot 71 / Effect 组存在性语义位 / css_name），本 ADR 起草
   时未勘察到位。原位升级：Effect 组剥离（initial 归 ClipShape::None、
   dispatch `P::ClipPath => parse_clip_path`、`has_clip_path()` 改形状
   判定），**SLOT_COUNT 保持 159，动画描述符 152–158 不动，零 slot
   破坏**——比原决策多省 1 slot 与一次全 slot 对齐破坏。
2. **PushClipPath 签名增 `nonzero: bool`**——§3 决策时 fill-rule 仅存于
   Polygon 值族，op 层不携带；实施④时认定 fill-rule 语义不能在 op 层
   丢失（soft 射线法与 vello push_layer 都需要），签名定为
   `PushClipPath { points: Vec<[f32; 2]>, nonzero: bool }`（本会话新增
   未发布 = 零破坏）。ClipShape 的 reference 平铺进四形状变体（非独立
   字段），`<basic-shape> || <geometry-box>` 组合天然表达。

其余与决策一致，实施要点：

- **解析**：`<basic-shape> || <geometry-box>` 两轮 try_parse 次序不限；
  geometry-box 单独出现 = inset(0) 基准该盒；margin-box 降 border-box
  （tracing 警告，B 级在案——不进 ParseReport）；url()/path() 宽容收容
  `ClipShape::Other`。词法事实在案：**cssparser 将无引号 url(#c) 词法化
  为顶层 `Token::UnquotedUrl` 而非 Function("url")——两形态都必须接住**
  （首版漏 UnquotedUrl 致宽容路径整条拒绝，锁测试抓出后补臂）。
  circle 百分比半径/负半径、polygon <3 坐标对、fill-rule 后缺逗号均整条
  拒绝。
- **计算**：初始 none、非继承；`clip_path()` 访问器（slot 71）；
  `has_clip_path()` = 形状≠None——第四批④ SC 触发语义逐位保持。
- **绘制**：inset → 既有 `PushClip` 矩形（round radius [f32;8] 精确
  承载，非近似）；circle/ellipse → 64 段折线 PushClipPath（起点 0°、
  逆时针、首点右极点；面积误差 0.14%）；polygon → 顶点直传（% 基准
  参考盒）；零半径圆/椭圆 → 退化 3 点；url()/path()/none → 零裁剪 op。
  发射位 = overflow 裁剪之后、背景之前（元素+子树包夹，LIFO 双
  PopClip）。命中 = 点集 AABB 入 HitRect.clips 链（B 级在案）。
- **sink**：vello BezPath move_to/line_to/close_path + push_layer
  （Fill::NonZero/EvenOdd 按 nonzero 挑选）；soft clip 栈泛化
  `Clip{Rect, Poly}` + 射线法 point-in-polygon（nonzero winding 计数 /
  even-odd 穿越计数）；paint_dump OpDump::PushClipPath 镜像 + serde
  往返锁。
- **测试切片落地**：decl.rs 五件（inset 全形态含斜杠双集 / circle-ellipse
  含 circle(50%)/circle(-5px) 拒绝 / polygon 文法与两拒绝 /
  geometry-box 双序+margin-box / url-path-none-拒绝）+ paint.rs 六件
  （inset op 几何 / 64 段端点+半径逐点 / polygon 点序与 fill-rule /
  参考盒位移（padding 偏移 content-box vs border-box）/
  url-none 零 op / overflow 复合恰 4 op 序）+ soft 两件（三角像素内含 /
  {5/2} 五芒星序 nonzero 中心填充 vs evenodd 中心镂空像素差分）+
  css_serde_dump 往返一件 + computed 初始与非继承一件。
- **既有测试重基线**：engine.rs `filter_clip_path_trigger_stacking_
  context_order` 重写——第四批④断言「clip-path 不产生额外 PaintOp」
  已过时，inset(0) 现实发 PushClip+PopClip 对（+2 op），clip-path 组
  单列断言、四效果组保留 op 数等值断言。
- **诚实边界（B 级在案）**：margin-box 降级；url()/path() 无效果
  （SVG clipPath 资源解析与 path() 语法=T2）；circle/ellipse 64 段
  折线；多边形命中 AABB；inset 斜杠双集解析支持但绘制期仅消费单值
  投影（垂直半径与水平半径独立圆角矩形=既有 PushClip [f32;8] 能力，
  双集差异场景由 sink 精化）；transition/动画插值不做（离散段进度
  0.5，lerp_decl 既有行为天然覆盖）。
- **全绿**：工作区 `cargo test --features serde,text` **456** 测试
  0 失败（lib 193 含 +12 新件、soft 20 含 +2 像素件、dump 往返、
  差分逐位与 GPU 像素回归）。0.x 破坏性仅 PaintOp 新变体
  （non_exhaustive）。

## 落地修正（F3e，2024：circle 百分比半径反转）

- **决策反转**：本 ADR 原判定「circle 百分比半径 = 轴歧义 → 拒绝」。
  F3e 像素不变量切片（soft Sink RGBA 语义锁 `clip_path_circle_masks_
  corners`）暴露该判定同时引入缺陷：半径 try_parse 失败后 `unwrap_or
  (ClosestSide)` 回落，但 "50% at …" token 留存块内，cssparser
  `parse_nested_block`→`parse_entirely` 对未消费输入**整块改判 Err**
  → **整条声明丢弃**（而非按本 ADR 设想的形状内回落）。
- **复核 spec**：css-shapes-1 §3.2.1 对 circle 百分比半径有明确基准
  `sqrt(w²+h²)/sqrt(2)`（非歧义）——原判据系误读，予以反转：解析
  放开 %（`parse_clip_radius(p, true)`），绘制期按 √(w²+h²)/√2 基准
  解析（paint.rs circle Length 臂）。
- **负半径维持拒绝**且现为 spec 语义（"negative values are invalid"
  = 整条拒绝；机制同为回落 token 留存→parse_entirely Err）。
- 锁测试 `clip_path_parse_circle_ellipse` 重基线：circle(50% at 50% 50%)
  → `ClipRadius::Length(Percent(0.5))`；circle(-5px) 拒绝保持。
