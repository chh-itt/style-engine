# ADR-0037：绘制 B 级收敛批（dashed/dotted 边框、bg-repeat round/space、wavy、clip-path 命中、圆精度）

日期：2026-02（P4 批，goal-c293e543）
状态：已接受（落地记录见文末）

## 背景

绘制面遗留一批 B 级近似（FEATURES/property.rs 注释在案）：
dashed/dotted 边框近似 solid（property.rs :1816-1818；
engine.rs :4033「v1 仅 solid 实绘」）；background-repeat
round/space 折叠为 repeat（paint.rs `resolve_layer_geom`
:2181 :2250-2251 仅区分 NoRepeat）；text-decoration wavy
近似直线（TextDecoStyleKind 已含 Wavy 变体）；clip-path
命中测试 AABB 近似（paint.rs :1153）；圆折线固定 64 段
（paint.rs :297）。本批逐项收敛至可锁定的真实现。

## 决策

### D1：dashed/dotted 边框——DisplayList 分段层实现

- 实现点：**engine.rs 边框 op 生成层**把 dashed/dotted 边拆分为
  多段矩形/圆点 op（sink 零改动，vello/soft 自动生效；符合
  L3「语义归 DisplayList」哲学）。
- dashed 模式（CSS 未定义，自定惯例 B 级在案）：段长 = 2×边宽、
  间隔 = 1×边宽（首段对齐起点，末段不足不画）；dotted：直径 =
  边宽的圆点串、间距 = 边宽（centered on 边中线）。
- 圆角边框 v1：直边段按上述分段，圆角弧退 solid（B 级在案——
  沿弧虚线分段延后）。
- 段为矩形 fill（水平边/垂直边各自轴对齐），不引入新 PaintOp。

### D2：background-repeat round/space——RepeatAxis 精确化

- `resolve_layer_geom` 返回值从「NoRepeat 与否」扩展为精确轴状态
  `RepeatX/RepeatY { Repeat | NoRepeat | Round | Space }`；
- 绘制处按轴复用 border-image Round/Space 同款整数片算法
  （paint.rs :857-890：Round=整数片等分拉伸、Space=整数片均布，
  n=0 退 stretch）；
- space/round 轴上 background-position 失效（css-backgrounds-3
  §3.5：平铺尺寸由算法决定）。
- property.rs :2202-2204 的「B 级近似 repeat」注释同步移除。

### D3：wavy 装饰线——真波形折线

- TextDecoStyleKind::Wavy 绘制从直线改为正弦近似折线：
  周期 = 2×线宽的固定倍数（自定惯例：周期 6×线宽、振幅 2×线宽，
  B 级在案；每周期 8 段折线）；
- 折线经既有装饰线矩形路径机制输出（若有 polyline 通道用之，
  否则细分矩形段），soft/vello 同生效。

### D4：clip-path 命中测试——折线精确化

- `hit_test` 的 clip-path 分支从 AABB（paint.rs :1153）升级为
  基本形状折线的点包含测试（ray-casting；圆/椭圆采样折线复用
  既有圆折线生成；polygon 直接用顶点）；
- 变换矩阵：命中点逆变换到局部空间后测试（与绘制同一 mat），
  修正变换节点命中未旋盒偏差（paint.rs :49/:1199 注释）；
- inset 圆角形状：圆角区走椭圆角测试（复用 src_inside 同型逻辑）。

### D5：outline auto 维持 Solid（语义位修正）

- outline-style: auto → Solid 的近似维持（paint.rs :1634-1655
  注释已在案；Chromium 的 focus ring 样式属宿主语义），仅把
  「B 级近似」注释升级为设计边界说明（无行为改动）。

### D6：圆折线精度自适应

- 固定 64 段 → 自适应：`seg = clamp(ceil(2πr / 3px), 16, 256)`
  （每段弧长约 3px，与典型屏幕精度匹配；小圆省算力、大圆保
  平滑）。conformance golden 若存在圆快照需重录（Pixel 通道
  0-5 分类评估）。

## 测试要点（实施时落锁）

- 边框：dashed/dotted 段数与首末对齐 ink_bbox 断言；圆角退化
  solid；BorderStyle 序列化往返不变；
- bg-repeat：round 拉伸尺寸、space 均布间隙、n=0 退化、position
  失效；layer 几何锁定测试；
- wavy：波形 ink 覆盖高于直线基线振幅；像素锁定；
- clip-path 命中：圆外/多边形凹角/变换后点命中与未命中；
- 回归：conformance 双通道全绿（圆精度若触发 Pixel 快照变化，
  按分类纪律处理并登记）。

## 落地记录

（P4 实施后补记。）

## 落地记录（P4 实施后补记）

全部六决策落地（commit 见 CHANGELOG P4 条），workspace 559→572（+13：
core lib 229→237（D1×3/D2×2/D6 重写×1/D4 锁×1/四向简写×1）、css_hit_test
3→5（D4 精确命中×2）、outline 6→7（方角重基线+圆角增长锁）、soft
40→42（wavy/double 像素×2））。

### 实施要点

- **D1**：mit_borders（paint.rs，Border 发射点与 outline 点共两个调用点）——
  visible（任一 side style≠None && width>0）否则早退；无 fancy（Dashed/Dotted）
  → 原 Border op 单发；圆角框（radius 非零）含 fancy → 整框退 Solid 单 op；
  直角框 → 四边带拆段 mit_side_band（Dashed 段 2t 步进 3t 末段不足不画；
  Dotted 圆点直径 t 中心距 2t FillRect 近似圆）。outline 通道复用后
  dashed/dotted outline 自动拆段。
- **D2**：LayerGeom.tile_x/tile_y:bool → axis_x/axis_y:TileAxis{None,Repeat,
  Space,Round}；	ile_axis_positions(axis,origin,size,win,win_len,area_origin,
  area_len)——None 单片；Repeat/窗口对齐步进（边缘半片保留）；Space n=⌊area/
  size⌋，n≤1 单片（position 生效），否则 gap=(area−size·n)/(n−1) 首片锚定
  area 起点（position 失效）；Round n=round(area/size).max(1) ts=area/n 锚定
  area 起点、网格恰铺满定位区不出界。消费循环 tiles_y×tiles_x 双轴，per-tile
  渐变几何用 tile 尺寸（tw/th 替代 dw/dh）。
- **D3**：soft 	ext_device_polys 装饰段按 TextDecoStyleKind 分派全部折线
  承载（Solid band/Double 两半厚带/Dashed 2t-3t 段/Dotted 16 段圆环/Wavy
  波带闭环：周期 6t、振幅 2t、每周期 8 段、上缘去程下缘回程）；vello 同参
  BezPath 折线（fill_deco_wavy/fill_deco_rect 模块级 fn，避免闭包双借
  &mut scene）。
- **D4**：命中基建——HitClip 枚举（Rect{rect,radius,inv}/Path{points,
  nonzero,inv}）替代 Vec<[f32;4]> AABB；HitRect 增 clips+mat（收集时活跃
  仿射）；HitCollector.mat 复合/恢复（PushTransform/PopTransform 发射点；
  **手写 Default——derive 会把 mat 初始化为全零而非恒等**，教训在案）；
  invert_affine/apply_affine/rounded_rect_contains/poly_contains/
  hit_clip_contains（paint.rs）；engine hit_test：点先经 HitRect.mat 逆变换
  到局部系测盒，clips 逐个经自身 inv 判定。
- **D5**：outline auto 注释升级（宿主 focus ring 语义位，引擎不注入 UA
  默认 outline）。
- **D6**：CLIP_POLY_SEGMENTS=64 → CLIP_POLY_SEGMENTS_MIN=16/MAX=256/
  CLIP_POLY_CHORD_TARGET=3.0，ellipse_points 自适应（r=20→42 段、r=2→16、
  r=200→256）。

### 实施偏差

- 【B】直角框 outline 外扩保持直角（原实现 radius 恒 +d 会把方角盒
  outline 外扩成圆角）——outline 测试重基线（方角 radius 全 0；新增
  outline_rounded_grows_radius 锁圆角盒 radius=源+d 增长语义）。
- 【B】Dotted 边框/装饰圆点为 FillRect 方形近似（soft 光栅圆折线仅装饰线
  通道实现）；D1 边框圆点直径/间距语义精确，形状近似。
- 【B】Wavy 在 vello 走折线直线段（每周期 8 段），非真贝塞尔曲线——soft
  同构保证双 sink 像素一致。
- 【A】测试暴露 **border-top/right/bottom/left 四向简写缺失**——本批补齐
  （order_shorthand_parts：<'border-width'>||<'border-style'>||
  <'border-color'> 任意序贪心，缺省 medium/None/currentcolor；shorthand
  注册三处）。属四向简写家族缺口收口，非 P4 决策原文，记录在案。
- serde 锁 clip_path_dump_round_trip 段数 64→63（r=30 自适应
  ceil(2π·30/3)=63）。

### 教训

- emit_borders 首版漏 visible 检查 → 全无边框节点也发 Border op → 21 个
  op 序测试错位（先补 visible 早退）。
- HitCollector derive Default 的 mat 全零陷阱（mul_affine 全零阵=全零）。
- RefCell 同语句 borrow()+borrow_mut() panic——先 borrow_mut 再用局部。
- 装饰线像素测试：字形墨迹主导 ink_bbox，双带/gap 判定需在基线附近窗口
  按列扫描墨迹段数（decoration_double_is_two_bands：t=6、窗口基线±、
  亮度阈值 216 容纳半覆盖行）。
- DisplayList 测试构造用 default()+ops.push（struct 字面量缺 generation
  字段）。
- rustfmt --check 传文件仍对 workspace 跑的误解已澄清（cargo fmt -- --check
  才会全 workspace；rustfmt 逐文件安全）。
