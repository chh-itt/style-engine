# ADR-0026: border-image 全集与字体深化（F3d）

- 状态：Accepted（实施中）
- 日期：批次 F3d 开工
- 关联：ADR-0024（background 全集——值族与 9-slice 相邻语义）、ADR-0025
  （clip-path）、第五批⑨（背景图宿主像素契约）、第五批⑯（@font-face 静默
  契约——本 ADR 升级为注册表）、C2/ADR-0016（text-transform 分段变换——
  small-caps 合成复用其双位变换模式）

## 背景

F3 定序（IMPLEMENTATION-LOG 在案）：F3d = border-image 全集 + 字体深化
（font-feature-settings / font-variant / @font-face 注册表）。开工在场审计：

- **border-image 全缺席**（`BorderImage|border-image` 零命中）——CSS
  backgrounds-3 的 5 长手 + 简写 + 9-slice 绘制全部未做。既有 Border op
  （paint.rs `PaintOp::Border{x,y,width,height,radius,sides}`）只画
  border-style 条带；背景图宿主像素契约（`add_image` → `ImageRes`）与
  渐变绘制（`PaintOp::Gradient`）已就绪可复用。
- **字体深化**：font-feature-settings / font-variation-settings /
  font-stretch / word-spacing / font-variant-caps 全部零命中缺席。既有
  font-family/size/weight/style + line-height/letter-spacing 已入库并接入
  parley（text.rs `build_layout`）。**parley 0.11.1 能力核查**（注册表源码
  实读）：`StyleProperty` 含 `FontWidth`（=font-stretch，
  `FontWidth::from_percentage`）、`FontVariations`/`FontFeatures`
  （`FontVariation{tag,value:f32}`/`FontFeature{tag,value:u16}`，
  parlance `tag.rs`）、`WordSpacing(f32)`——四项可原生映射；
  small-caps 无直接枚举（需合成路径）。
- **@font-face = 第五批⑯契约静默消费**（stylesheet.rs :1456/:1653/:1917）：
  整块跳过无产出。字体二进制加载保持宿主 `add_font` 契约（零副作用，
  ADR-0001/0006——引擎不取 src url）；但块本身可以结构化解析为注册表供
  宿主查询（族名 → 宿主决定注册哪些字体），真实世界 CSS 不再整块丢弃。

## 决策

### D1: border-image 值族与 PropertyId——ALL 尾追加 5 长手

```rust
pub enum BorderImageSliceComp { Number(f32), Percentage(f32) }      // 1..4 值 + fill
pub struct BorderImageSlice { pub slices: [BorderImageSliceComp; 4], pub fill: bool }
pub enum BorderImageWidthComp { Length(LengthPercentage), Number(f32), Auto }
pub struct BorderImageWidth { pub comps: [BorderImageWidthComp; 4] }
pub enum BorderImageOutsetComp { Length(LengthPercentage), Number(f32) }
pub struct BorderImageOutset { pub comps: [BorderImageOutsetComp; 4] }
pub enum BorderImageRepeatKind { Stretch, Repeat, Round, Space }
pub struct BorderImageRepeatXY { pub x: BorderImageRepeatKind, pub y: BorderImageRepeatKind }
```

- `PropertyId::BorderImageSource`（值复用 `BackgroundImage{None,Url,Gradient}`
  ——url 走背景同款宿主像素契约，渐变直绘）；`PropertyId::BorderImageSlice/
  Width/Outset/Repeat` 各自新 DeclValue。
- **插槽策略：ALL 尾追加**（先例 F3b：背景 6 长手加 ALL 尾 146..151）。
  5 新位 = 152..156；动画描述符 152..158 → 157..163；
  `SLOT_COUNT = 159 → 169`（连同字体深化 5 位——见 D3，一次到位）。
  slot() 描述符分支整体 +10，其余位不动；`slot_alignment` 锁测试自动维持。
- 解析（property.rs）：slice 四值展开（number/percentage 混排）+ `fill`
  任意位宽容（Chromium 行为）；width/outset 四值展开（`/` 分段后同轮次
  展开）；repeat 单值=双轴、双值 x/y。简写 `border-image:
  <'source'> || <'slice'> [/ <'width'>? [/ <'outset'>?]?]? || <'repeat'>`
  ——`||` 组合无序（复用 F3c clip-path 两轮 try_parse 模式 + 斜杠链内层
  仅可跟在 slice 后）。
- number 单位语义**存储原值、绘制期解析**（border-image-width/outset 的
  number × 对应边 border-width；slice 的 number 对光栅源 = 源像素、对渐变
  源 = border image area 尺寸上的像素）——计算期无 border-width 依赖，
  与 css-backgrounds-3 计算值定义一致。

### D2: 绘制——border-image 取代 border-style，9-slice 几何在 paint 层

- **css-backgrounds-3「in place of border-style」**：source ≠ none 且切片
  有效时，`PaintOp::Border` 不再发射，改发 9 区域基元（4 角 + 4 边 +
  中心[仅 fill]）。绘制序与 Border op 同位（背景之上、内容之下）。
- 区域几何：slice 四线切源 → width（缺省=对应边 border-width，auto=切片
  尺寸等比）分配目标域（border-box 外扩 outset——ink overflow，不入
  scrollable overflow / 命中），round/space → B 级近似 repeat/stretch
  （与 F3b 背景 space/round 同型边界，FEATURES 在案）。
- **源切片通用化两件 additive 改造**：
  1. `PaintOp::Image` 增 `src_x/src_y/src_w/src_h: f32`（源像素坐标子域，
     背景路径发全图 0,0,W,H）——vello：仿射把子域映到目标域 + 区域裁剪；
     soft：最近邻采样偏移。0.x additive（non_exhaustive 变体字段，构造
     面仅 crate 内 + 镜像）。
  2. `PaintOp::Gradient` 增 `linear: Option<LinearGeom{start,end:[f32;2]}>`
     ——**CSS 渐变线在 paint 层按全 border image area 一次解析为绝对
     px**（先例：radial/conic 已 paint 层解析绝对几何），9 区域共享同一
     绝对几何 = 渐变切片精确（各区域显示渐变的对应子域）。两 sink linear
     臂优先消费绝对几何、缺省回退盒推导（既有直接构造 Gradient op 的
     测试保持有效）。常规背景 linear 同时改发绝对几何（一致性）。
- 渐变源 repeat 平铺 = B 级近似 stretch（渐变切片平铺的真实语义需要
  渐变域重参数化，v1 不做，在案）。

### D3: 字体深化——5 属性，parley 原生映射 + small-caps 合成

- `font-stretch`：normal | <percentage [50,200]> | ultra-condensed(50%) ..
  ultra-expanded(200%) 九关键字 → `DeclValue::FontStretch(f32)`（存储归一
  百分比 50..=200，normal=100）→ parley `FontWidth::from_percentage`。
  继承。
- `word-spacing`：normal | <length-percentage>（% 基 = 元素 font-size，
  css-text-3 inline base size）→ 复用 `DeclValue::LenAuto(Option<LP>)`
  （normal → None → 0；可负）→ `StyleProperty::WordSpacing(px)`。继承。
- `font-feature-settings`：normal | <feature-tag-value>#（tag 必须为
  <string>，spec 严格式；value = on/off/<integer 0-65535>，缺省 on=1）→
  `DeclValue::FontFeatures(Vec<([u8;4], u16)>)` → parley
  `FontFeatures::List`。继承。
- `font-variation-settings`：normal | [ <string> <number> ]# →
  `DeclValue::FontVariations(Vec<([u8;4], f32)>)` → parley
  `FontVariations::List`。继承。
- `font-variant-caps`：normal | small-caps | all-small-caps | petite-caps |
  all-petite-caps | unicase | titling-caps → 枚举。**合成路径**（复用
  text-transform 双位变换模式——text.rs measure 与 paint.rs 绘制两处）：
  small-caps 族四值 → 文本预变换（小写→大写）+ 合成区间追加
  FontSize×0.8 的 ranged 覆盖（span 模型现成：TextSpanPaint.font_size /
  StyleProperty::FontSize ranged push）；titling-caps → 'titl'、unicase →
  'unic' 特性并入 FontFeatures 推送。合成区间按 UTF-8 字符粒度（大写
  字母范围 A-Z/a-z→A-Z，非字母不动）。**与 text-transform 交互序：先
  text-transform、后 small-caps 合成**（CSS 规范序；合成看变换后文本）。
  B 级在案：不探测字体 smcp 表（fontique 无 per-family 特性协商钩子），
  真小型大写字体也走合成——度量近似、字形形式为全大写缩尺而非真 smcp。
- PropertyId +5（FontStretch/WordSpacing/FontFeatures/FontVariations/
  FontVariantCaps）ALL 尾 157..161；全部继承。

### D4: @font-face 注册表——静默契约升级为结构化注册表

- `FontFaceRule { family: String, sources: Vec<FontFaceSource>,
  style: Option<FontStyle>, weight: Option<f32>, stretch: Option<f32>,
  display: Option<FontDisplay>, unicode_ranges: Vec<(u32, u32)>,
  features: Vec<([u8;4], u16)>, variations: Vec<([u8;4], f32)> }`；
  `FontFaceSource { kind: Url(String) | Local(String), format: Vec<String> }`。
- stylesheet.rs：@font-face 块解析为描述符集合（family 必须、src 必须，
  缺失=整块丢弃+tracing 警告）；`Stylesheet.font_faces: Vec<FontFaceRule>`
  新公共字段；引擎 `pub fn font_faces(&self) -> &[FontFaceRule]` 宿主查询
  ——宿主据此决定 add_font 注册哪些族名字体（二进制加载契约不变）。
- **不再静默**：解析产出注册表 + 块级告警仅用于丢弃情形（描述符不识别
  仍宽容跳过——前向兼容惯例）。真实世界 CSS（携带 @font-face）零告警。

## 破坏性（0.x）

- `SLOT_COUNT` 159 → 169；动画描述符槽移位 ×10（ComputedStyle 槽位
  物理布局变更）。
- `PaintOp::Image` +4 字段、`PaintOp::Gradient` +1 字段（non_exhaustive
  变体的 crate 内构造面 + serde 镜像同步）。
- `Stylesheet` 新公共字段 `font_faces`（构造面 crate 内）。
- 新增类型/变体均为新增面。

## 备选与不取理由

- border-image 区域发射为「每区域独立 Image/Gradient op」而非单一
  `PaintOp::BorderImage`：显示列表保持中性词汇（sink 不理解 9-slice 语义，
  几何与平铺在 paint 层终结——ADR-0001 中立性契约；软/硬双 sink 零逻辑
  重复）。
- linear 渐变绝对几何化（而非仅 border-image 场景内联处理）：radial/conic
  已是 paint 层绝对几何先例，linear 盒推导是历史遗留——补齐后渐变切片、
  任意盒一致性同源；sink 回退分支保留兼容。
- small-caps 真 smcp 协商：fontique 0.11 无公开 per-family 特性协商 API，
  引入 skrifa 表解析超出本批（DEPENDENCIES 升级路径同款判断）；合成是
  Chromium 缺字体时的同型行为，B 级在案。
- font-variant 完整简写（east-asian/numeric/ligature 复合值族）：切
  font-variant-caps 单轴（本批范围由 31 项清单 F3d 定义为 font-feature-
  settings/font-variant/@font-face；caps 轴是唯一有可见绘制语义的子集，
  其余轴（numeric/ligature/east-asian）解析面大、无 sink 消费点，归 T2
  文档化——不做无效解析面）。

## 后果

- 落地后：border-image 全语义（source/slice/width/outset/repeat/简写/
  9-slice 绘制/渐变与像素双源）；字体属性覆盖 font-family/size/weight/
  style/stretch/word-spacing/feature/variation/variant-caps 全接入 parley；
  @font-face 可查询注册表。
- 锁定测试面：解析（值族+简写）、计算（初始/继承）、绘制几何（9 区域
  坐标/取代 Border op/outset 外扩/repeat 平铺计数）、sink（vello 子域
  变换断言 + soft 像素差分）、serde 往返、字体（stretch/word-spacing
  测量变化、features 推送、small-caps 合成区间）、@font-face 注册表。
- B 级边界登记（FEATURES）：round/space→repeat/stretch、渐变 repeat→
  stretch、small-caps 合成、unicode-range 仅记录不驱动 fallback 选择。

## 落地记录（F3d 实施完毕；全量 477 测试绿）

切片①值族/解析：PropertyId +10（BorderImage 五槽 152–156 + font 族五槽
157–161；Animation 描述符区 162–168 顺移），SLOT_COUNT 159→169；
DeclValue/值族 `BorderImageSliceComp/Slice{slices:[_;4],fill}/
BorderImageWidthComp/Width/BorderImageOutsetComp/Outset/
BorderImageRepeatKind{Stretch,Repeat,Round,Space}/BorderImageRepeatXY/
FontVariantCapsKind` 七族（PartialEq+non_exhaustive；LP 在场族去 Copy，
默认值 `array::from_fn`）；E4 css_name +10；E7 解析器块
（parse_bi_slice/width/outset/repeat + parse_font_stretch（九关键字+
百分比钳 50..=200）/parse_font_features/parse_font_variations/
parse_font_variant_caps + `lp_reject_negative` + `trbl_expand<T:Clone>`）；
E8 dispatch +11 臂。切片②绘制：`paint_border_image`（paint_node 中
`if !paint_border_image(...) && sides active { Border }`——source≠none
**替代** Border op）；`BiPaint{Image,Gradient}`（渐变 stops 终结绝对色，
linear/radial/conic 按**整盒**解析）；slice TRBL%（top/bottom×sh、
left/right×sw）+number（raster=源 px/gradient=面积 px）；溢出缩放
`f=(sw/(sl+sr)).min(sh/(st+sb))`、带宽 `fw` 同型（基准=边框盒）；
widths Length→px/Number→×border-width/Auto→raster=slice、gradient=
border-width；outset 仅 paint 域平移（ink overflow，不入滚动量程/命中）；
`emit_region`（Image 携 src 窗口/Gradient 携区域+整盒几何）+
`emit_tiled_edge`（Stretch 单片拉伸/Repeat 顺排末片 PushClip 截断/Round
n=max(1,round(len/tile)) 等分/Space n=floor(len/tile) 首尾贴边 gap 均摊
——优于本 ADR D2 原定 B 级方案，实施期改进）；发射序=四角→四边→fill。
切片③sink+serde：vello sub_rect 裁剪层+非均匀仿射（src_w.max(0.001)
防除零）、`Gradient::new_linear` 绝对端点；soft 源窗口采样（
`(src_x+(fx−ix)/iw·src_w)/source_w`）+线段投影 t；paint_dump 镜像
`LinearGeomDump`+Image 四窗口字段。切片④@font-face：FontFaceRule 全描
述符（`parse_font_face_block/descriptor/parse_urange_token/
skip_until_semicolon`——urange '?' nibble 通配 a 腿填 0/b 腿填 F、
0x10FFFF 双端钳制）；AtPrelude::Skip→**FontFace 更名**（宿主 match 需
改臂=0.x）；Stylesheet+font_faces（user→主表→附加表合并序，同族后规则
胜=消费方语义）；登记语境不设限（media/supports/container/layer/嵌套均
extend）；engine.rs `rebuild_font_faces`（三触发点）+`font_faces()`
访问器；字体字节仍宿主 add_font（契约不变）。切片⑤字体深化：
computed `resolved_word_spacing_px`+`effective_font_features`（显式
settings ∪ caps 派生 titl/unic，同 tag 显式优先）；text_transform
`synth_caps`（SYNTH_CAPS_SCALE=0.8；Small=lower→upper+缩放/upper 不动、
AllSmall=upper 原样缩放；ß→SS；并邻仅限同覆盖 span——跨 span 不并供绘
制侧按 span 取样式）；measure_two_pass transform→synth 串联（CSS 顺序：
先 text-transform 后 caps 合成；uppercase+small-caps=全大写不缩放，
Chromium 一致），probe 缓存键 +caps u8；build_layout +caps_scaled 参
（span 后 push FontSize×0.8，后推覆盖前推）；PaintOp::Text +4
（stretch/word-spacing/features/variations 基样式级——span 级=行高/字
距先例同型 B 级）；vello push_default FontWidth(from_percentage)/
WordSpacing/FontFeatures::List(Cow::Owned)/FontVariations；soft 伪拉伸
（轮廓与步进同比 x×fw）+空格词距（GSUB/gvar 无消费点=记录偏差）。

**验证与锁定**：decl/paint/soft/dump/registry 五面锁测试全绿——九宫格
区域 dest+src 窗口精确锁、repeat tile 数与 PushClip、渐变九区共享整盒
绝对几何线、@font-face 全描述符/无效丢弃/条件组登记/合并序/刷新、
small-caps 测量=A@0.8+B@1.0 组合（all-small=全串 0.8×）、serde 往返含
全部新字段。全工作区 `cargo test --workspace --features serde,text`
**477 通过 0 失败 0 rustc 警告**（462→477）。

**实施期修复的两既有缺陷**：①@media/@container 臂漏并子表 keyframes
（条件组内 @keyframes 静默丢弃）——切片④合并点修复+锁测试
font_face_and_keyframes_inside_media_registered；②parse_font_stretch
百分比漏 ×100（cssparser `Percentage.unit_value`=分数，125% 误判越界
整条丢弃）——small-caps/深化锁测试抓出后修复。**实施期发现（工具链事
实）**：cssparser `Token::Percentage.unit_value` 为 f32 分数、
`Token::Number.int_value` 可空、无引号 `url(#c)` 顶层为 UnquotedUrl——
三事实均已入锁测试。

**破坏性（0.x）终单**：SLOT_COUNT 159→169（SettlePassKind/槽位直查宿主
需重排）；`PaintOp::Image`+4 字段、`PaintOp::Gradient`+1、
`PaintOp::Text`+4（non_exhaustive，解构需新字段）；`AtPrelude::Skip`→
`AtPrelude::FontFace`；`Stylesheet`+`font_faces` 字段；`DeclValue`+10
变体。`PaintOp::PushClipPath` 为本周期新增未发布变体=零破坏。

**B 级边界终单**（FEATURES 同步登记）：round/space 切片平铺改进近似、
圆角不切 9 片、渐变源 repeat→stretch；small-caps 合成（petite 回退
small，无 smcp 探测——fontique 无协商钩子）；unicode-range 记录不驱动
fallback；软栅格无 GSUB/gvar；span 级四属性仅基样式生效。
