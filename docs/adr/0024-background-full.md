# ADR-0024: background 全集——多层背景（F3b）

- 状态：已接受
- 日期：2025 年（F3b 批）
- 关联：ADR-0002（属性解析分层）、第五批⑨（宿主注册图）、第五批⑩（阴影）、ADR-0023（serde）

## 背景

31 项清单 F3 收尾批。现有 background 面为单层：`BackgroundColor`（纯色）+
`BackgroundImage`（none | url() | 渐变，值族 :1818，parse :3175，paint 单层
消费 :660-736）。decl.rs :449 明示 background 简写不支持。CSS Backgrounds 3
全集（多层、repeat、position、size、clip、origin、attachment、简写）缺位。

## 决策

### 1. 数据模型：长手列表 + cycling 层对齐

每长手持有**逗号分层列表**（`Vec<T>`），层对齐遵循 css-backgrounds-3 §3：
**短列表循环补齐至 max 层数**（`background-image: a, b, c` + `background-
repeat: no-repeat` → no-repeat 应用到全部三层）。

新值型（property.rs）：

- `pub struct RepeatXY { pub x: RepeatAxis, pub y: RepeatAxis }`；
  `pub enum RepeatAxis { Repeat, Space, Round, NoRepeat }`。
  `repeat` 单关键字 = 双轴 Repeat；`repeat-x` = {Repeat, NoRepeat}；
  `repeat-y` = {NoRepeat, Repeat}；双值首 = x、次 = y。
- `pub enum Attachment { Scroll, Fixed, Local }`。
- `pub struct Position2D { pub x: PositionComp, pub y: PositionComp }`；
  `pub struct PositionComp { pub base: LengthPercentage, pub offset:
  Option<LengthPercentage> }`——bg-position 全语法（1-4 值：关键字
  left/center/right/top/bottom、百分比、长度、边+偏移四值）解析期归一：
  关键字→等价基（left/top=0，center=50%，right/bottom=100%），三/四值
  偏移入 offset。
- `pub enum BackgroundBox { BorderBox, PaddingBox, ContentBox }`（origin）；
  clip 在此之上多 `Text`（css-backgrounds-4，B 级）。
- `pub enum BgSize { Auto, Cover, Contain, Explicit { w: LPorAuto, h:
  LPorAuto } }`；`pub enum LPorAuto { LP(LengthPercentage), Auto }`。

`DeclValue::BackgroundImage` **单值 → Vec\<BackgroundImage\>（0.x 破坏性）**；
新增 DeclValue 变体：BackgroundRepeat/BackgroundAttachment/BackgroundPosition/
BackgroundOrigin/BackgroundClip/BackgroundSize（各 Vec）。背景色仍单项
（非分层）。新 6 槽 153-158，SLOT_COUNT 153→159；from_css_name 自动迭代，
slot_alignment 锁测试防漂移。

初始值：image None；repeat Repeat×2；attachment Scroll；position
0% 0%；size Auto；origin **PaddingBox**；clip **BorderBox**；color 透明
（现状不变）。

### 2. 绘制语义（paint.rs 重写 :660-736 分支）

- **层序**：首层最上 → **逆序发射**（末层先画）。
- **定位区** = origin box（border/padding/content box 几何）；**绘制区** =
  clip box；渐变几何解析于定位区。
- **size**：url 图 = 内在尺寸（宿主注册 w/h）；gradient = 定位区；
  cover/contain = 覆盖/容纳数学；显式 w/h 解析 LP 于定位区（auto 维持
  该轴内在比例）。
- **position**：`offset_px = P% · (area − image) + base_len + offset`
  （百分比语义 = 图 P% 对齐区 P%；right/bottom 负号）。
- **repeat**：Repeat 平铺发射；NoRepeat 单幅；repeat-x/y 单轴平铺；
  **space/round → repeat（B 级近似，偏差在案）**。
- **attachment**：fixed = 视口锚定（层绘制原点取视口非元素盒）；local ≈
  scroll（B 级，引擎无滚动容器实义）。
- **clip: text**：解析收容、绘制降级 border-box clip + tracing warn
  （B 级偏差在案——文字遮罩填充需 vello 字形路径蒙版，另批）。

### 3. background 简写（decl.rs，全语法）

css-backgrounds-3 §3：`<bg-layer># , <bg-final-layer>`；bg-layer =
`<bg-image> || <bg-position> [ / <bg-size> ]? || <repeat-style> ||
<attachment> || <box>{1,2}`；终层另允许 `<color>`；一个 box = origin 与
clip 同值，两个 = 前 origin 后 clip；`||` 任意序各组件至多一次；未指定
组件 = 初始值；简写**重置全部九长手再赋值**。decl.rs 注册
shorthand_exists + shorthand_longhands（9 长手）+ 漂移锁。

### 4. 诚实边界（0.x）

space/round 近似 repeat；local ≈ scroll；clip:text 绘制降级；无
`background-blend-mode`（css-compositing，另批）；url 未注册图跳过
（现状语义保留）。

## 测试切片

解析锁（分层逗号/循环补齐源值/简写全组件/双 box 语义/position 四值）+
计算锁（cycling 对齐/初始值回退）+ 绘制锁（多层逆序序/平铺 op 计数/
cover contain 显式尺寸/origin 定位区位移/fixed 视口锚定）。

## 落地记录

F3b 全量落地（本批，全工作区绿）：

- **切片①②（值族+解析）**：`property.rs` 新值类型 9 个（`RepeatAxis`/
  `RepeatXY`/`Attachment`/`PositionComp{base, offset}`/`Position2D`/
  `BackgroundBox`/`BackgroundClip`/`LPorAuto`/`BgSize`/`BackgroundLayer`）；
  PropertyId +6 变体（BackgroundImage/Repeat/Attachment/Position/Size/
  Origin/Clip——Image 于枚举内插、其余 ALL 尾部追加 ：503）；SLOT_COUNT
  153→**159**；`slot()` 重排=六背景槽 146–151（=ALL 位序）、动画描述符
  让位 152–158（slot_alignment 锁同步）。解析器：`parse_background_image`
  Vec 化（逗号层循环）、`parse_repeat_xy`（repeat-x/y + 双轴）、
  `parse_background_position`（PosTok ≤4，right/bottom 偏移解析期
  `neg_lp` 取反）、`parse_background_size`（`LPorAuto{1,2}`+cover/
  contain）、origin/clip（文本特例 clip: text）；分发六臂 + `from_css_name`
  经 ALL 自动遍历。教训：`u32::MAX as i32` 回绕——正整数上限必须
  `.min(i32::MAX) as u32`。
- **切片③（计算）**：`computed.rs` `initial()` 六新分支 + BackgroundImage
  初始 `vec![None]`；`ComputedStyle::background_layers()` 层对齐视图
  （层数 = max(七列表, 1)，i%len cycling，缺省声明 = 初始值参与）。
- **切片④（绘制）**：`paint.rs` 重写发射——色 FillRect 恒发
  （border-box+元素圆角）；`background_layers().iter().rev()`（首层最上）；
  fixed=视口锚定矩形；clip PushClip（clip=BorderBox 用元素圆角、否则
  [0.0;8]；clip: text 降级 border-box+警告）；tile 双循环 ceil 对齐
  （space/round→重复=B 级）；`resolve_layer_geom`（cover/contain 精确
  数学、显式 LP、显式宽+auto 高保纵横比）；渐变逐 tile 重解
  radial/conic 半径 [0;8]；Image 固有尺寸平铺；未注册 url 警告跳过。
  语义精化（非回归）：旧实现 size:auto 拉伸至盒（≈100% 100%）→
  固有尺寸平铺；旧实现无 clip 包裹→每层 Push/Pop。
- **切片⑤（简写）**：`decl.rs` `shorthand_exists`/`shorthand_longhands`
  双表 + `expand_shorthand` background 臂——层内组件无序贪心
  （image→repeat→attachment→box{≤2: 首双赋 origin+clip、次覆盖
  clip}→color→position[+斜杠 size]，position 最后试=LP 读数最宽）；
  非末层 color/空层/重复组件=整条 Err 拒绝；缺省部件回初始值
  （transparent 规范形 = `AlphaColor::new([0,0,0,0])`）。八长手单分量
  解析函数 `pub(crate)` 化复用（`parse_attachment_one`/`parse_bg_size_one`
  新抽）。
- **切片⑥（锁定）**：decl.rs 四测（`background_shorthand_full_decode`
  八声明序全组件/`background_shorthand_multi_layer` 三层缺省回初始/
  `background_box_single_sets_both` 单双 box/`background_shorthand_rejects`
  三拒绝）+ computed.rs `background_layers_cycling_alignment`（双向
  cycling）+ paint.rs `background_layers_paint_first_layer_on_top`
  （七 op 反层序锁）。三既有 paint 快照重基线（`find_map` 于 op 序列
  找 Gradient/Image——两层 Option 解包教训：模式内 `Some(g)`）；
  `background_image_op` 重基线为 tile 计数 1250 锁。
- **验证**：lib 181/0（--features text）；全工作区 38 套件 440 测试
  0 失败（含差分逐位与 GPU 像素回归）；--features serde,text 加跑绿；
  clippy 零新增（存量在 F2 及更早区域，归 F3e 基建批清理）。
- **0.x 破坏性**（BREAKING-POLICY 在案）：SLOT_COUNT 153→159；
  `DeclValue::BackgroundImage` 单值 → `Vec<BackgroundImage>`；
  PropertyId +6 变体。
