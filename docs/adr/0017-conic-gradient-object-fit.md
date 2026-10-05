# ADR-0017：conic-gradient 与 object-fit/object-position（css-images-3 补全）

状态：已接受（C3 批）
关联：ADR-0014（注册表）、第五批⑨（url() 背景图）、T4c（径向几何）

## 背景

31 项清单 C3 = conic-gradient + object-fit。现状（盘点实证）：

- 渐变基建在位：`Gradient { kind, stops }`、`GradientKind::Linear(Angle) | Radial(RadialSpec)`
  （non_exhaustive，`_` 降级臂契约）；`PaintOp::Gradient { x, y, width, height, radius,
  gradient, radial: Option<RadialGeom> }`；vello 经 peniko 0.6.1 映射（radial 臂坐标约定 =
  `Point::new(cx + state.offset.x, cy + state.offset.y)`）；soft 逐像素渐变（linear/radial）。
- **peniko 0.6.1 `GradientKind::Sweep(SweepGradientPosition)` 原生存在**：
  `{ center: Point, start_angle: f32, end_angle: f32 }`，弧度、正 X 轴起、Y-down 顺时针
  ——与 CSS conic 顺时针同向；center 内建，无需 brush transform 注入。
- object-fit 全库零命中。图像通道现状 = `background-image: url()` → 注册表
  （`add_image(reference, w, h, rgba)`）→ `PaintOp::Image`（全盒拉伸，无 size/repeat）。
  引擎无替换元素模型；叶节点固有尺寸机制 = `set_leaf_intrinsic(k, min_w, min_h, max_w, max_h)`。
- `background-size` 尚不存在（F 批「背景全集」范围）——本 ADR 不得预占其语义。

## 决策

### D1 conic-gradient 解析：`GradientKind::Conic(ConicSpec)` 第三变体

- `ConicSpec { from: Angle, position: (LengthPercentage, LengthPercentage) }`
  （默认 from 0deg、position 50% 50%）。
- 语法 `conic-gradient([from <angle>]? [at <position>]? ,? <stop-list>)`；
  复用 `parse_angle_deg`、`parse_position_component`、`parse_gradient_stops`；
  前导段有内容时 expect_comma（radial 同款容错）。
- CSS 角度 0 = 12 点方向顺时针 → **peniko 弧度 = (from_deg − 90°)·π/180**
  （peniko 0 = 3 点 = CSS 90°；顺时针同向直接映射）。

### D2 conic 几何与绘制：`ConicGeom` + `PaintOp::Gradient` +conic 字段

- `paint.rs pub struct ConicGeom { cx: f32, cy: f32, start: f32 }`
  （绝对 px 圆心 + 起始角弧度，peniko 语义对齐；RadialGeom 同构风格）。
- `PaintOp::Gradient` +`conic: Option<ConicGeom>`（线性 None；**0.x 破坏性**——
  穷举构造/解构须补字段）。
- `resolve_conic(spec, x, y, w, h, style, env)`：position→px 复用 radial 位置解析数学；
  PaintOp 构造处按 kind 分派（radial/conic 互斥 Some）。
- **vello**：`peniko_gradient` +Conic 臂 →
  `GradientKind::Sweep(SweepGradientPosition::new(
      Point::new(f64::from(cx) + state.offset.x, f64::from(cy) + state.offset.y),
      start, start + TAU))`——坐标约定镜像 radial 臂（盒坐标 + state.offset）。
- **soft**：destructure +conic；逐像素 t = 归一扫角
  `((atan2(fy−cy, fx−cx) − start) mod 2π) / 2π`（其余停点插值机制复用）。
- 现存 `_` 降级臂保留（面向未来变体）；本次 Conic 臂为实装取代降级路径。

### D3 object-fit/object-position 属性

- `PropertyId::ObjectFit`（slot 130）、`ObjectPosition`（slot 131）；
  SLOT_COUNT 137→**139**；动画描述符 132-138；ALL.len()=132。
- `ObjectFitKind { Fill(默认), Contain, Cover, None, ScaleDown }`（non_exhaustive+Default）；
  object-position = `(LengthPercentage, LengthPercentage)`（默认 50% 50%）。
  均不继承。
- `word-wrap` 式别名：无。

### D4 替换内容通道：诚实模型，不动背景语义

- `StyleNode` +`image: Option<String>`（**0.x 破坏性**；None 默认零行为变化）——
  引擎的替换元素 MVP 通道；与 `background-image: url()` 完全解耦
  （后者仍走背景臂、background-size 留给 F 批，不冲突）。
- 固有尺寸自动注入：布局前，节点带 image 且注册表命中且用户未手动
  `set_leaf_intrinsic` → 按 ((w,h),(w,h)) 注入自然尺寸（min=max=自然）；
  未注册引用 → 告警跳过（零副作用契约与背景臂一致）。
- 绘制：paint 走访新增「元素图像」段（背景之后）：查引用 → **fit 数学**：
  - fill：dest = 内容盒；
  - contain：s = min(bw/sw, bh/sh)；cover：s = max(...)；none：s = 1（自然 px）；
    scale-down：s = min(1, min(bw/sw, bh/sh))（none/contain 的较小者）；
  - object-position 偏移：`dx = (bw − fw)·posx`、`dy = (bh − fh)·posy`
    （溢出时为负=按百分比反向对齐，CSS 语义一致）；
  - dest 超出内容盒（cover/none 溢出）或盒有圆角 → `PushClip(内容盒+radius)`
    …`PopClip` 包裹（背景臂同款）；
  - `PaintOp::Image` = 拟合后 dest 矩形（source_w/h/pixels 自足）——
    **sink 零改动**（soft/vello 按 op 矩形绘制）。
- 落盒基准 = 内容盒（css replaced content area）；区块流下叶盒尺寸沿用既有
  叶语义（容器驱动），固有钳制经 D4 注入自然生效（flex/grid/absolute 同机制）。

### D5 测试契约

- tests/css_images.rs：conic 解析锁（默认/ from / at / 组合）；PaintOp::Gradient
  conic 几何（start=−π/2、圆心 px）；soft 象限像素锁（0deg 中心 conic 四象限色）；
  object-fit 五模式 dest 矩形（contain 信箱/cover 溢出+PushClip/none 自然/
  scale-down/fill）；object-position 偏移；image 叶 intrinsic 注入（盒=自然尺寸）。

## 备选与否决

- conic 经 soft 多边形扇面近似（vello 无 Sweep 假设下）——否决：peniko 0.6.1
  Sweep 原生在位，近似引入可见色带且双 sink 不一致。
- object-fit 作用于 background-image url() 通道——否决：语义错误（object-fit 属
  替换内容），且与将来 background-size 冲突。
- 替换内容经 content: url()——否决：C1 已定 content MVP=none/normal/字符串，
  扩展 content 值域属另一批；StyleNode.image 通道更直白且可独立锁定。
- object-position 暂缓——否决：fit 已实现时增量≈30 行且是 css-images-3
  replaced 定位的另一半；缺它 cover/contain 无法表达非居中对齐。

## 0.x 破坏性清单

1. `GradientKind` +`Conic` 变体（non_exhaustive，外部 `_` 臂不受影响）。
2. `PaintOp::Gradient` +`conic: Option<ConicGeom>` 字段。
3. `StyleNode` +`image: Option<String>` 字段。
4. `PropertyId`/`DeclValue` +2 变体（ObjectFit/ObjectPosition）、SLOT_COUNT 139。
5. `ComputedStyle` +`object_fit()`/`object_position()` 访问器。
