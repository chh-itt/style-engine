# 更新日志（CHANGELOG）

本 crate 不发布 crates.io（契约 C4/C7），版本号仅作 API 演进坐标。格式遵循
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；语义化版本：
**MInor 位 = 特性新增，Patch 位 = 修复，Major 位 = 破坏性变更（未发生）**。

所有已发布提交见 https://github.com/chh-itt/style-engine/commits/main 。

## [Unreleased]

### 1.0 对齐 — 表格 wrapper/inner 拆分与对齐收束

- **表格 wrapper/inner 拆分（fix）**：display:table 节点在 taffy 侧拆为
  wrapper（表盒自身样式，size/max_size=auto，随包含块填充并随内容生长）与
  inner（声明宽保留、Position::Relative、inset 清零）两节点；`<table>` 直下
  非行块级子件提升至 wrapper、置于表盒上方（Chromium 153 实测语义，文档化
  偏差①），裸 table-cell 以幻影行（单行 Grid，按 (table, slot) 缓存）纳入
  行列分配，absolute 后代一律留内表（偏差②）。表盒 rect 消费 inner 布局。
  `table-rowspan`、`table-anon` 金标全键零差异转正（后者 case.css 显式声明
  display，与其他 table-* 用例同约定）。
- **paint 平铺 space/round 精确化（feat）**：background-repeat 补三值文法
  （`<repeat-style> <repeat-style>` 双轴）与 round（按整倍数重标定 tile
  尺寸）/space（首尾贴边、余隙均匀分布）两轴语义；border-image repeat
  round/space 对九宫格边缘做逐边重标定（round/space 交叉取积）+ 端部
  裁剪。新增锁定测试 `tests/repeat_round_space.rs` 九件。
- **text-transform: capitalize 对齐 css-text-4（feat）**：按 UAX#29 词界
  分词（引入 unicode-segmentation 1.x，纯 Rust 零传递依赖）逐词首字符
  大写化，替代旧空白分词；新增锁定测试五件。
- **soft sink P8 补全（feat）**：PaintOp::Border 逐像素重建 vello 语义——
  §5.5 半径缩放、不对称内半径孔洞、角域对角线归属、Dashed（3w/3w 相位自
  run 起点）与 Dotted（径 w 距 2w）、失主边邻角接管；PaintOp::Text 补
  Center/Right/End 对齐（max_advance 源空间偏移、溢出钳回起点，parley
  0.11 语义）；模糊 text-shadow 投影扩至装饰线与多 span 形状。soft 53 项
  测试全绿。
- **vello sink 影模糊环近似（feat）**：vello 0.10 无内置高斯——box-shadow
  以多重同心扩张环、text-shadow 以多重同心偏移环近似，环
  α = 1−(1−a)^(1/N)（N 层复合恰为总 a），text 环数随 blur 自适应（≤0 单
  环=锐利副本）；corner_diagonal_deg 补 .to_degrees() 修复（等宽对角线
  45° 断言回归捕获）。纯函数单测无 GPU 覆盖。

### 阶段7 — A 批：语义补全

- **行为提示属性（A1）**：新增 cursor（CSS UI 4 关键字子集 15 值；url()
  自定义指针=T2）、user-select（auto/none/text/all/contain，**不继承**）、
  pointer-events（auto|none）、caret-color / accent-color（auto|color）——
  宿主可读、零绘制通道：解析/级联/继承全语义入库，不产生任何 PaintOp、
  不参与布局（锁定测试断言 ops 与几何零变化）；宿主消费经
  `ComputedStyle::value`（全集物化含初始值——非继承属性子级物化为初始
  auto）；槽位 87–91（SLOT_COUNT 94→99），slot_alignment 自动覆盖；
  新增锁定测试 `tests/behavior_hints.rs` 六件（文法/初始同型/继承语义/
  零绘制/半透明色形/合法对落槽）。
- **outline / outline-offset（A2）**：新增 outline-width/outline-style/
  outline-color/outline-offset 四长手与 outline 简写（`||` 文法任意序，
  未指定长手重置初始，不重置 outline-offset，css-ui-4）。不继承、不占
  布局（ink overflow，不入滚动量程）；绘制复用 Border 基元以外扩矩形
  承载（描边带=[border-box+offset, +offset+width]，radius 随外扩增长），
  soft/vello sink 零改动自动获得；style auto=宿主 focus ring 语义位、
  花式线型（double/groove/ridge/inset/outset）按 Solid 近似（B 级在案，
  BorderStyle 值族仅 None/Hidden/Solid/Dashed/Dotted）。槽位 92–95
  （SLOT_COUNT 99→103）；简写路由补齐 shorthand_exists+shorthand_longhands
  双表；新增锁定测试 `tests/outline.rs` 六件（op 几何精确锁/布局与量程
  零影响/none 与零宽不发射/auto 近似锁/负 offset/简写任意序）。
- **多根引擎与 top-layer（A3，ADR-0010）**：`StyleTree` arena 根槽改为
  合成超根（永不绑定 key、样式恒空），用户根挂载其下——`insert(None)`
  首个为文档根（单根语义逐位保留），后续为 overlay 根（弹窗/浮层载体，
  `RootExists` 不再发生=契约放宽）；新增 `set_top_layer(key, on)` 与
  `ContractError::NotOverlayRoot`（文档根不可进层）。overlay 根默认
  视口锚定（absolute+原点，作者显式定位不动）、各为级联根；有效绘制
  序 = 文档根 → 非 top overlay（插入序）→ top 层（进层序），frame 经
  `sync_root_order` 同步树序与 taffy 子序（稳态零操作）、绘制按根序
  逐根追加（新增 `paint::append_display_list`）。`:root` 重定义为匹配
  全部用户根（`StyleTree::is_root`）。锁测试 `tests/multi_root.rs`
  六件（overlay 原点锚定/显式定位/top-layer 绘制序进出层/级联根/
  remove 语义/契约）。全量 222 测试绿（含差分逐位与 GPU 像素回归）。
- **position: fixed / sticky（A4）**：`Position` 值族补全 Fixed/Sticky
  两变体并全值解析。fixed：taffy 映射 Absolute，包含块由
  `settle_absolute_anchors` 以 transformed 祖先候选（新 tcb 列）判定
  ——无 transformed 祖先则锚定 ICB 视口、positioned 祖先不算
  （CSS 2.1/3）；fixed 亦为定位元素（其 absolute 后代的 cb）。sticky：
  in-flow 布局（taffy Relative 且 inset 归 auto——top/right/bottom/
  left 是粘滞约束语义、非偏移），`ComputedStyle` 可读；粘滞偏移由
  宿主按其滚动运行时施加（引擎经 `scroll_offsets` 绘制期平移，
  ADR-0006 引擎无运行期状态——架构边界在案 FEATURES.md）。锁测试
  `tests/position_fixed_sticky.rs` 四件（fixed 无视 positioned 祖先/
  transformed 祖先作 cb/fixed overlay 根视口锚定/sticky 解析+in-flow+
  可读）。全量 226 测试绿。
- **white-space 全值（A5）**：`WhiteSpace` 值族补全 PreWrap/PreLine/
  BreakSpaces（pre 语义不变）；pre-wrap/break-spaces 不再容错映射
  Pre（保留+换行是真实语义）。语义矩阵：normal=折叠+按宽换行/
  nowrap=折叠+不换行/pre=保留+不换行/**pre-wrap=保留+按宽换行*/
  **pre-line=折叠空格但保留换行符+按宽换行**/**break-spaces=保留+按宽
  换行**（任意字符断行=v1 常规断点近似，B 级在案 FEATURES.md）。
  T5c-2 重排 pass 换行判定扩为
  None|Normal|PreWrap|PreLine|BreakSpaces（白空格族按包含块宽自动
  重测换行）。锁测试 `tests/white_space.rs` 五件（pre-wrap 换行且
  保留空白/pre-line 保留 \n 且折叠空格/break-spaces 换行/pre+nowrap
  不换行回归/normal 折叠换行基线；显式注册族名 "DejaVu Sans"——
  未注册泛族测量为 0 尺寸契约）。全量 231 测试绿。
- **min()/max()/clamp()（A6）**：`CalcNode` 扩展 Min/Max/Clamp 三变体
  （value.rs）——解析（逗号参数二元折叠、clamp 恰三参、嵌套数学函数
  经 parse_calc_value 递归）、求值（clamp(MIN,VAL,MAX)=max(MIN,min(
  VAL,MAX))）、百分比延迟结算（has_percent 直通）与纯数字拒绝（长度
  语境须量纲值）全链路与 calc() 一致；`parse_length_percentage` 路由
  min/max/clamp 函数 token → `LengthPercentage::Calc`。**修复映射期
  视口单位解析基准（既有缺口）**：新增 `map_env()`——map_style 的
  解析环境以帧视口覆盖 media 视口字段（CSS 语义：vw/vh 与 calc 视口
  单位 = 初始包含块 = 帧视口；@media 条件命中仍在级联期按宿主推送的
  media 判定，不受影响），九处 map_style 调用点统一。锁测试
  `tests/math_fns.rs` 五件（min/max 择向/clamp 三段夹取/calc 嵌套与
  vw/畸形拒绝）。全量 236 测试绿。
- **@media L4 range + 新特性（A7）**：`MediaFeature` 扩展 12 变体
  （stylesheet.rs）——`Range(RangeCond)`（L4 范围语法
  `(width >= 400px)`/`(400px <= width <= 800px)`/值在前形，轴
  width/height/aspect-ratio/resolution，含界 `<=`/`>=`、开界 `<`/`>`、
  双比较 AND）、`Orientation`（landscape/portrait）、
  AspectRatio/MinAspectRatio/MaxAspectRatio（`<ratio>`=数字或 `a/b`）、
  Resolution/MinResolution/MaxResolution（dpi/96、dpcm×2.54/96、
  dppx|x 直通 → dppx）、布尔语境 HoverBool/AnyHoverBool/PointerBool
  （`(hover)`/`(pointer)`）与 `Not`（`(not feature)`，可嵌套）；
  **MediaEnv 破坏性扩展**：+`resolution: f32`（dppx，默认 1.0——
  0.x 契约，结构字面量宿主需补字段）。**评估基准统一（既有缺口
  收口）**：级联 @media 命中与全部解析环境统一经 `map_env()`——
  宽高类媒体特性按帧视口评估（CSS 语义：媒体查询=视口），宿主
  `set_environment` 继续供配色/动效/指针/分辨率偏好；视口外字段
  不受影响。锁测试 `tests/media_l4.rs` 七件（含界/开界/双比较/值在
  前/纵横比/分辨率换算/orientation/布尔与 not/旧形与 and 链回归）。
  全量 243 测试绿。
- **逻辑属性 + direction/unicode-bidi（A8，css-logical-1）**：新增 30
  槽位——margin/padding/inset ×{inline,block}×{start,end}（8）、
  border-{inline,block}-{start,end}×{width,style,color}（12）、
  border-{start,end}-{start,end}-radius（4）、direction 与
  unicode-bidi；槽位布局依 slot_alignment 契约重排为
  物理 0–95 / 逻辑 96–125 / 动画描述符 126–132（SLOT_COUNT 103→133）。
  **级联期规范正确映射**：逻辑槽赢家与映射物理槽赢家按级联序键
  (origin.rank, specificity, order, **decl_index**) 定夺、晚者填物理槽
  （同规则块内按声明序——Candidate 新增 decl_index 声明级键，物理/逻辑
  同池比先后），逻辑槽自身不外泄（写位）；ltr：inline-start=left、
  block-start=top；rtl 仅翻 inline 轴与 radius 四角（纵向书写模式不
  支持，B 级在案）。direction（ltr|rtl，继承）为映射基准——自身声明
  胜者优先、继承次之、缺省 ltr；unicode-bidi 六关键字（继承）为文本栈
  提示（`ComputedStyle::direction()/unicode_bidi()` 可读，引擎文本叶
  不做 bidi 重排=B 级在案）。简写：margin-inline/block、
  padding-inline/block、inset-inline/block（1–2 值 start end，值表
  穷尽校验）、border-inline/block（`||` 文法展开 6 长手，未指定分量
  重置初始）。**全集物化修复**：物化循环改按 `pid.slot()` 落槽（此前
  以 ALL 下标充当槽位，ALL 序=槽位序时侥幸成立——新布局下既暴露也
  修复该隐患）。锁测试 `tests/logical_props.rs` 十件（ltr/rtl 映射、
  块轴不受 rtl 影响、同块声明序与跨规则源序定夺、双轴简写、border
  逻辑集、direction 继承与缺省、逻辑槽不外泄）。全量 253 测试绿。
- **容器查询与字体相对单位（A9，css-values-4 / css-contain-3）**：
  `LengthPercentage`/`CalcUnit` 新增 cqw/cqh/cqi/cqb（容器查询单位）与
  ch/ex/ic（字体相对单位）七变体（**0.x 破坏性：下游对 LengthPercentage
  的穷尽 match 需补臂**；存储约定 cq 按小数、ch/ex/ic 按 em 倍数，
  解析期经 parse_length_percentage 与 calc() Dimension 臂双通道入库）。
  **求值复用延迟结算管线**：cq 单位映射期判 has_cq 落延迟队列（直变体经
  lp_to_calc 归一 CalcNode；首遍以回落基值折叠保证 taffy 首遍有确定值），
  settle_calc 结算期沿 taffy 父链现查最近 container-type≠Normal 祖先的
  本帧布局内容盒为基（size 双轴、inline-size 块轴回落视口高）；无容器
  祖先=small viewport（规范回落）。**ch/ex/ic 走真字体度量**：新增
  `css::fontprobe` 最小 sfnt 探测（head/cmap format4/hhea+hmtx/loca+
  glyf 头 bbox/name 表族名，零依赖），add_font 注册即探测；restyle 期
  按 font-family 首个具名族（大小写不敏感）补写 ComputedStyle 新私有
  字段 font_metrics（`ComputedStyle::font_metrics()` 宿主可读）；未注册
  族回落近似缺省 ch/ex=0.5em、ic=1em，ic 缺字（cmap 无 U+6C34）=1.0em。
  B 级在案：纵向书写模式不支持 → cqi=cqw、cqb=cqh（水平书写语义）；
  @container style() 查询仍不支持（另条）。ResolveCtx 增五字段
  （cq_w/cq_h/ch/ex/ic_per_em）并以 `ResolveCtx::base()` 构造器收敛
  全部 25 处字面量。锁测试 `tests/relative_units.rs` 九件（容器基值≠
  视口、cqh/cqi/cqb 轴向、calc 混排、无容器回落、ch/ex 真度量≠近似、
  ic 缺字契约）。全量 262 测试绿。
- **B1 级联起源栈 + @layer + revert 族（css-cascade-5）**：`Origin` 扩为
  `Default/UserAgent/User/Stylesheet/Inline` 五档（UA/User 真实起源就位；
  UA 表宿主后续接入，语义已完备）；beats 序键扩为 (origin-importance,
  layer_key, specificity, order, decl_index)——Inline 并入 Author 桶（桶内
  以 u32::MAX 特异性区分，行为与独立档等价，见 ADR-0011 实施修订）；
  `@layer` 全形态解析（语句形 `@layer a, b;` / 块形 / 嵌套 / 匿名层 /
  点名前缀的先现序层树，层序 u32 序数解析期定序，未分层 normal 轴胜一切
  分层、important 轴反转）；`revert`/`revert-layer` 赛后回滚（custom
  property 同机制）；`initial`/`inherit`/`unset` CSS 宽关键字整值物化
  （var() 代换结果为宽关键字同语义；`flex: initial` 保持 flex 专属
  `0 1 auto` 例外）；引擎新增 `set_user_stylesheet`/`clear_user_stylesheet`
  （User 起源注入，全树失效）。**0.x 破坏性**：`Origin` 新增 `UserAgent`/
  `User` 变体；`Rule` 新增 `layer_rank` 字段；`cascade_declarations` 与
  `compute_node_in` 各新增 `user_sheet` 参数；`CascadeOutput` winners 改为
  全候选列表（`cascade_winner`/`cascade_winner_custom` 取冠军）。
  全量 278 测试绿（新增 tests/cascade_layers.rs 16 件）。
- **B2 多样式表 + @import + @supports（css-cascade-5 §7/css-conditional-3）**：
  引擎多 author 表——`add_stylesheet(source) -> u64` 句柄化附加表
  （`remove_stylesheet` 移除；级联序 = 主表后按登记序、后表胜平手；源文本
  留存供重建）；文档全局层树——`doc_layers` 附着重置、各表 `@layer` 经
  `remap_layers_to_doc` 并入（表内 rank→文档序数重映射，跨表同名层 = 同
  层、先现序 = 文档序）；`@import` 附着期拼接——`resolve_imports` 按
  directive.order 与规则流交错（导入规则视同写在导入点；拼接后全表按
  文档序重编号 order），子表经 `parse_stylesheet_in_layer(path)` 以层前缀
  解析（`layer(name)` 指定层 / 裸 `layer` 匿名层固化唯一路径），directive
  media 与规则 media 合取（`MediaQuery::conjoin` AND 链——修正嵌套
  `@media` 旧"内层覆盖"为规范合取），`supports(...)` 子句解析期求值
  （false 指令整体静默失效）；循环守卫（seen 栈）+ 深度上限 32；导入源 =
  内存 `set_import_source(url, css)` 或宿主 `set_import_loader`（`Send +
  Sync` 回调，loader 优先；URL→CSS 契约归宿主，引擎零网络）；未解析导入
  指令保留待重拼接（嵌套未决指令经整表重解析存活——rebuild_sheets 从源
  文本重解析主表）；`@supports` 块形解析期求值（not/and/or 同级不混用、
  `(decl)` 文法试探经 parse_declaration、`selector(...)` 经
  parse_selector_list 试探、自定义属性恒真、裸声明宽容形
  `supports(decl)`；false 块静默丢弃、语法无效 Dropped 告警）；`url()`
  函数形与裸串两形均收。**0.x 破坏性**：`cascade_declarations`/
  `compute_node_in` 的 author 表参数改为按值 `Vec<&Stylesheet>`（输出只借
  表内容，调用方行内临时构造即可）；`Stylesheet` 新增 `imports`/`layers`
  字段；`MediaQuery` 新增 `conjoin` 字段。
  全量 298 测试绿（新增 tests/imports_supports.rs 20 件）。

- **B3 CSS Nesting + :has() + :focus 族（css-nesting-1 / selectors-4）**：
  解析期 desugar 架构（级联核心零改动）——`&` ≡ `:is(父)`（特异性 = 父
  选择器）、无 `&` 隐式后代、深层递归展开、token 级 `&` 识别（字符串内
  `&` 无害）、深度 32 守卫；规则体经 `RuleBodyParser` 驱动（嵌套体声明
  合法化 + cssparser「声明失败重试规则」消歧激活）；**嵌套声明源位置
  分裂**（flush-at-item-boundary——`.card{color:red;@media(…){color:lime}}`
  的 lime 胜，与浏览器一致）；嵌套条件组提升穿线（条件 AND 合取 + 选择器
  挂父链）；顶层裸声明拒绝守卫（顶层毒化为预存在偏差，B 级在案）。
  :has() = selectors 0.40 原生解析+匹配启用（`parse_has` hook，引擎零
  自研）；`Stylesheet.has_relative_selectors` 解析期深扫导出（含 :is/:not
  内层）+ 引擎 `any_has_rules()` 变更类失效升级全量重样式（增量失效 B 级
  延后）。:focus 族 = `set_focus(key: Option<K>, focus_visible: bool)`
  整链迁移（FOCUS/FOCUS_VISIBLE/FOCUS_WITHIN；祖先链传播；同节点
  focus_visible 翻转全链重走；remove 两路径清锚防 slotmap 键复用悬垂；
  focus_visible 启发归宿主）。**0.x 破坏性**：`Stylesheet` 新增
  `has_relative_selectors` 字段。
  全量 324 测试绿（新增 tests/nesting_has_focus.rs 26 件）。
- **B4 @property 注册（css-properties-values-api）**：解析器三描述符
  syntax/inherits/initial-value——syntax 文法 MVP 子集 = 14 类型族
  （length/percentage/length-percentage/number/integer/color/image/url/
  angle/time/resolution/transform-function/custom-ident/string）+
  `+`/`#` 单层多值组合子（`||`/`&&`/嵌套语法 → 注册无效丢弃）；注册
  有效性 = 非 universal 缺 initial 或 initial 不匹配 syntax → 规则
  丢弃告警；universal（`*`，缺省）不校验且声明值原样在场。注册表 =
  引擎 `registered_props`（BTreeMap），sheet 变更点统一重建，合并序
  user → 主表 → 附加表（author 覆 user，同名后者胜）。计算语义三闸：
  继承门（inherits:false 不进继承通道）→ 语法门（终值不匹配 → unset →
  initial-value——Chrome 一致：var(--x) 解析到 initial 而非触发
  fallback，fallback 仅用于缺席名）→ initial 填充（声明/继承双缺 →
  initial-value；无 initial 的 universal = 缺席 = guaranteed-invalid）。
  @property 仅样式表顶层合法（嵌套/条件组/语句形丢弃告警）。
  **0.x 破坏性**：`Stylesheet` 新增 `property_rules` 字段、
  `compute_node_in` 新增 `registered` 参数。
  全量 342 测试绿（新增 tests/property_registry.rs 18 件）。

- **C1 伪元素 ::before/::after + content（css-content-3 / css-pseudo-4
  MVP）**：ADR-0015 树实体化架构——伪节点 = 引擎 `materialize_pseudos`
  实体化的真实树子节点（::before 首子 / ::after 末子，裸 StyleNode 无
  身份）；选择器匹配走 selectors 0.40 原生通道（`parse_pseudo_element`
  hook + `originating_element` 回 origin 左复合，MatchingMode::Normal
  直配无模式门）。实体化 pass 在 frame 内 sync_root_order 后、taffy 重建
  前：`Stylesheet.has_pseudo_rules` 解析期深扫判据（主/user/附加三表
  any），全表无伪规则 → 零成本清除全部；有 → 全树宿主节点确保
  ::before/::after 存在并归位（宿主子序过滤伪节点 → [b]+host+[a]）。
  content 属性：`PropertyId::Content` slot 126（SLOT_COUNT 133→134，动画
  描述符槽 127-133 平移）；`ContentValue` = None/Normal/Str 单串 MVP
  （attr()/url()/counter()/quotes = T2 解析期拒绝告警）；content 仅伪
  元素语义——none/normal → map_style `Display::None` 无盒（宿主节点恒
  Normal 不受影响）。`sync_pseudo_text`（restyle 后、布局前）：content
  计算值 → tree.node.text + measure + taffy set_style 同帧布局，none/
  normal 撤测量。结构伪类与 :empty 不受伪节点实体化影响（selector.rs
  结构遍历排除伪节点）；宿主镜像通道兼容——`set_children` 自动合并
  [before]+宿主序+[after]，remove 两路径清 `pseudo_ids` 注册表防
  slotmap 键复用悬垂。**0.x 破坏性**：`StyleNode` 新增 `pseudo`
  字段、`ComputedStyle` 新增 `pseudo` 字段与 `content()`/`pseudo()`
  访问器、`Stylesheet` 新增 `has_pseudo_rules` 字段、`PropertyId` 新增
  `Content` 变体（SLOT_COUNT 134）。
  全量 356 测试绿（新增 tests/pseudo_elements.rs 14 件）。
- **C2 text-transform / overflow-wrap / word-break（css-text-3 / ADR-0016）**：
  解析：`PropertyId` +`TextTransform`/`OverflowWrap`/`WordBreak`（slot
  127/128/129；SLOT_COUNT 134→**137**，动画描述符移 130-136；ALL.len()=130）；
  值枚举 `TextTransformKind`（None/Uppercase/Lowercase/Capitalize/FullWidth/
  FullSizeKana）/`OverflowWrapKind`（Normal/BreakWord/Anywhere）/
  `WordBreakKind`（Normal/BreakAll/KeepAll）皆 non_exhaustive+Default；
  `word-wrap` 别名路由 overflow-wrap。实现：text-transform 自实现分段变换
  （新模块 text_transform.rs——needs_transform 快路径直通、span 边界切段
  连续覆盖、逐字符 old→new 字节映射（ß→SS 扩缩安全）、Capitalize 词界跨段
  重置；span 级=各段随覆盖 span 或基样式；继承：transform 是、wrap 对否
  ——spec 一致）；word-break/overflow-wrap = parley 0.11.1 原生映射
  （build_layout push_default 非缺省才推）。绘制一致性：测量
  （measure_two_pass）与 vello sink（draw_text）同参——`PaintOp::Text`
  +`word_break`/`overflow_wrap` 字段通道；op.text 携带变换后文本+span
  偏移重映射（sink 对 transform 零改动）。full-size-kana 接受 no-op（T2
  偏差在案）；Capitalize 词界≈alphanumeric 近似（spec=UAX#29，偏差在案）；
  soft 渲染 v1 无折行不消费断行参数（偏差在案）。**0.x 破坏性**：
  `PaintOp::Text` +2 字段（穷举构造须补）、`ComputedStyle`
  +`text_transform()`/`overflow_wrap()`/`word_break()`、`PropertyId`/
  `DeclValue` +3 变体、SLOT_COUNT 137。全量 **372** 测试绿（text_transform
  单元 5+tests/css_text.rs 11）；顺带清 19 处测试告警债（multi_root 6/
  logical_props 11/behavior_hints 2：unused Frame→`let _ =`、unused mut）。
- **C3 conic-gradient + object-fit/object-position（css-images-3 / ADR-0017）**：
  conic 解析：`GradientKind` +`Conic(ConicSpec)`（non_exhaustive 第三变体；
  `ConicSpec { from: Angle, position: (LP, LP) }`，缺省 0deg/center）——语法
  `conic-gradient([from <angle>]? [at <position>]? ,? <stops>)` 复用
  parse_angle_deg/parse_position_component/parse_gradient_stops。几何：
  paint.rs `ConicGeom { cx, cy, start }`（绝对 px 圆心 + 起始角弧度、正 X 轴
  起顺时针）+ `PaintOp::Gradient` +`conic: Option<ConicGeom>` +
  `resolve_conic`（CSS 0deg=12 点 → (deg−90°)·π/180 平移）；vello 经
  peniko 0.6.1 **Sweep 原生映射**（`GradientKind::Sweep(
  SweepGradientPosition::new(center, start, start+2π))`，坐标约定镜像
  radial 臂=盒坐标+state.offset；`_` 未来变体降级臂保留）；soft 逐像素
  扫角（atan2 归一 rem_euclid(2π)，Y-down 顺时针=CSS 同向），四象限硬停点
  像素锁。object-fit/object-position：`PropertyId` +2（slot 130/131，
  SLOT_COUNT 137→**139**，动画描述符 132-138；ALL.len()=132）；值族
  `ObjectFitKind`（Fill/Contain/Cover/None/ScaleDown，non_exhaustive+Default）
  +object-position (LP, LP)（left/center/right/top/bottom/`<l_p>`，分量文法
  同 radial `at`）。诚实替换内容通道（与背景 url() 语义完全解耦；
  background-size 留 F 批不冲突）：`StyleNode` +`image: Option<String>`
  → 引擎 `seed_image_leaves`（compute_layout 前种子；CSS 10.3.4 简化——
  双边声明=盒取声明、单边=另一边按源宽高比、全 auto=自然尺寸（块级替换
  不拉伸）；absolute 叶仅注入固有区间走 T5d shrink 通道；宿主
  set_leaf_intrinsic 手动优先，引擎不接管）→ paint 元素图像段（fit 数学
  fill/contain/cover/none/scale-down + object-position 偏移
  offset=pct·(盒−拟合)（负基合法=溢出反向对齐）；溢出或圆角
  PushClip(内容盒+radius) 包裹；`PaintOp::Image`=拟合后矩形，
  **sink 零改动**）；未注册引用告警跳过（零副作用契约）。**0.x 破坏性**：
  `GradientKind` +`Conic` 变体、`PaintOp::Gradient` +`conic` 字段（穷举
  构造/解构须补）、`StyleNode` +`image` 字段、`PropertyId`/`DeclValue` +2
  变体（SLOT_COUNT 139）、`ComputedStyle` +`object_fit()`/`object_position()`
  访问器。全量 **386** 测试绿（新增 tests/css_images.rs 13 件 +
  soft conic 象限像素 1 件）。
- **C4 ::selection / ::placeholder（css-pseudo-4 / ADR-0018）**：
  非盒生成伪元素双通道——`PseudoElement` +`Selection`/`Placeholder`
  变体（non_exhaustive；+Copy；单冒号 `:selection` 非 CSS2 legacy 集
  自然拒绝）。匹配=**origin 直配**：`match_pseudo_element` 对两变体返回
  `pseudo == None`（实体化伪节点恒不命中）；`Element::
  pseudo_element_originating_element` 覆写——C1 伪节点维持默认父链，
  origin 直配时前缀复合在节点自身求值（originating element = 节点自身；
  selectors 0.40 默认实现 debug_assert 于 origin 节点炸，探针实证）。
  级联：collect_sheet 双模过滤（`channel=None` 主级联排除通道规则防
  样式泄漏；`Some(w)` 仅收对应通道规则；match_rules pub 语义零变更）+
  `cascade_channel`（user→author 序、无内联段——style 属性无法指向
  伪元素；尾 resolve_revert→retain→排序同 cascade_declarations）。计算：
  compute_node_in 主体抽取 `compute_node_from_cascade`（公开签名零改）；
  通道继承基 = origin 主样式（css-pseudo-4 继承语义）+ 字体度量同主
  路径。引擎：`Stylesheet` +`has_selection_rules`/`has_placeholder_rules`
  （解析期判据）；engine +`selection_styles`/`placeholder_styles` 两 map
  + `selection_style(key)`/`placeholder_style(key)` 访问器（#[must_use]，
  镜像 computed_style）；restyle_node 主样式 insert 前 channel pass
  （cs 移入前取继承基）；**契约 = 通道 Some 仅当 ≥1 规则命中**
  （winners+custom_winners 双空 → None 宿主回退系统缺省；清除语义
  二分：全表无规则 → 整 map 清零成本 / 单节点未命中 → 仅 remove 本
  节点——restyle DFS 后段未命中不得抹前段命中条目，探针实证）。
  实体化闸：`selector_list_has_pseudo` 收紧为仅盒生成 Before/After
  （::selection-only 表不触发 materialize_pseudos）。Chrome 生效属性
  子集（宿主取舍；引擎全量解析计算）：selection=color/background-color/
  text-decoration 系/text-shadow/caret-color；placeholder=color/font 系/
  opacity/letter-spacing/line-height/text-transform/background-color。
  **0.x 破坏性**：`PseudoElement` +2 变体（+Copy；跨 crate 穷举 match
  须补臂）、`Stylesheet` +2 字段、engine +2 访问器（纯增量）。
  全量 **397** 测试绿（新增 tests/css_selection.rs 10 件 + selector.rs
  origin 直配探针转正 1 件）。
- **E4 float / clear（css-position-3 / ADR-0019）**：浮动结算引擎侧
  实现（taffy 0.14.0 无原生 float——`settle_floats` 两相结算，先例
  settle_tables/settle_columns）。解析面：`PropertyId` +`Float`/`Clear`
  两槽（slot 132/133，动画描述符顺延 134-140，SLOT_COUNT 139→**141**，
  ALL 132→134——`slot_alignment` 一致性锁自动验收）；值族
  `FloatKind`（None 缺省/Left/Right）+`ClearKind`（None 缺省/Left/
  Right/Both）（non_exhaustive+Default，ObjectFitKind 同形）；
  `DeclValue` +2 变体；`parse_float`（none|left|right）/`parse_clear`
  （none|left|right|both）+dispatch；`ComputedStyle` +`float()`/
  `clear()` 访问器（缺省 None）+initial 两臂。结算面：engine
  `fn settle_floats(&mut self, viewport: (f32, f32))`（frame 收敛环内
  settle_columns 之后、文本 remeasure 之前——折行宽依赖结算值）：
  ①DFS 收集（文档序，`.into_iter().rev()` 修 LIFO 倒序；豁免
  table/multicol/定位盒）；②每帧还原=**map_style 重放 pristine**
  （不缓存 taffy::Style 值——其含 !Send/!Sync 的 CheapCloneStr
  （\*const ()），炸 assert_send_sync；参数借用拆语句防 E0502）；
  ③放置=struct Place{id,is_left,x,y,w,h,idx} 同侧栈 (带顶,前缘,底缘)
  水平适配贪心（同带继续直至容器缘溢出才下移，CSS §9.5.1）+
  own_clear 钳位；④浮盒覆写=Position::Absolute+inset（padding box
  相对）；兄弟环绕=出流补偿 post_y=pristine_y−前序浮盒高和，顶缘落
  带内（post_y∈[y,y+h)）同侧宽和 → margin.left/right 增量（clear 盒
  跳过横向——钳位后全宽）；⑤clear 钳位=margin.top += 相关向最后浮
  盒底 − post_y。幂等=float_touched 触点集。诚实边界（0.x）：兄弟级
  环绕带宽常量化+嵌套子树局部+BFC 边界按需迭代（FEATURES 偏差在
  案）。**0.x 破坏性**：PropertyId/DeclValue +2 变体（SLOT_COUNT
  141）、ComputedStyle +float()/clear()。全量 **402** 测试绿（新增
  tests/css_float.rs 5 件）。
- **E5 grid-template-areas 与命名线（css-grid-1 / ADR-0020）**：grid
  放置全链引擎侧实现（taffy 0.14 模板存储无非重复行名、子网格无
  Subgrid 变体 → 引擎端解析，ADR-0020 D1 选项 B）。解析面：
  `PropertyId` +`GridTemplateAreas`/`GridRowStart`/`GridRowEnd`/
  `GridColumnStart`/`GridColumnEnd` 五槽（slot 134–138，动画描述符
  顺延 139–145，SLOT_COUNT 141→**146**，ALL 134——`slot_alignment`
  一致性锁自动验收）；值族 `GridAreas{rows}`（引号串行逐行空白分词、
  `.`=空、行宽一致+逐名格数==行跨×列跨矩形校验，min/max 边界法漏
  对角格故必须计数）+`GridLineSpec`（Auto/Number(i16 非零)/Span(≥1)/
  SpanName/Name，non_exhaustive）+`GridTemplate` +`line_names:
  Vec<Vec<String>>`（N 轨 → N+1 槽；cssparser 把 `[ … ]` 词法化为
  SquareBracketBlock 块 token（unit 变体）→ `bracket_open` 命中 +
  `parse_nested_block(bracket_names)` 收名，非 ident = 声明无效）；
  简写 grid-row/grid-column（start [/ end]，end 缺省=auto、start 为
  <custom-ident> 时镜像同 ident）+grid-area（行起/列起/行止/列止，
  缺省段按 spec §7.4/7.5 镜像 ident），shorthand_exists/
  shorthand_longhands/expand_shorthand 三表同步+防漂移锁 3 代表值行；
  `auto_tracks` 扩展 GridTracks 全列表映射（grid-auto-* 由单长度
  子集升级为轨道列表，`track_sizing` 直转）。结算面：engine
  `fn apply_grid_placements`（纯样式派生：容器线名注册表
  line_names[i]=线 i+1+区域矩形 spans min/max → 子四长手
  `Self::resolve_axis` → taffy `grid_row`/`grid_column` 数字放置；
  挂点=seed_image_leaves 之后、compute_layout 之前——无布局依赖无
  额外重排；每帧幂等=map_style 重置+重施；豁免 table/multicol/
  定位盒）；解析序=区域边线（起=界+1/止=界+2）→ 全名线（起
  first/止 last）→ strip `-start`/`-end` 裸名 → 未知名=Auto；
  SpanName=start 线后第 1 次名线（不足=末候选+1 钳，隐式线按 spec
  计入）；两侧正线号 start≥end → end=start+1 钳。诚实边界（0.x）：
  repeat 内线名=解析失败声明无效、`span <int> <ident>` 混合形不
  支持、subgrid（taffy 0.14 无 Subgrid 变体）。**0.x 破坏性**：
  PropertyId/DeclValue +5 变体（SLOT_COUNT 146）、GridTemplate
  +line_names 字段。全量 **410** 测试绿（新增
  tests/css_grid_areas.rs 8 件）。
- **F1 IFC 行内流 v1（css-display-3 / ADR-0021）**：块容器直子行内参与者
  （文本叶 / inline-block 原子盒 / inline 组盒）贪心行打包 → taffy
  Absolute+inset 锚定（ADR-0019 浮盒先例）——`settle_lines` pass（挂
  点=settle_floats 后、文本 remeasure 前；参与者经
  inline_run_participants 豁免全宽重测；帧间幂等=prev_participants
  快照防 is_absolute 误跳）。语义面：Display +Inline（连续性标记/组
  盒）+InlineBlock（原子盒）变体（parse 拆臂；inline-flex/grid/table
  仍归一告警；layout 映射 taffy Block 零布局变）。行打包：文本叶
  measure_rich(剩余宽) 接排、折行叶占满本行推后续下行、声明宽/高叶=
  原子式参与（声明宽折行测量）、原子/组盒自然尺寸=taffy MaxContent
  探针 + 子树文本叶 nowrap 测宽 max（taffy 无文本内在尺寸）、行盒溢
  出续排（nowrap 叶后续盒不换行，CSS 真行为）、容器
  min_height=打包行高（行内内容贡献行盒高、auto 高塌陷防护）。v1 偏
  差（ADR-0021 D4 在案）：行高=max(参与者测量高)、vertical-align=TOP
  对齐（baseline 延后）、inline 组内子叶纵向堆叠（单叶 span 精确、
  多叶组宽=max 非流宽和）、跨叶强制断行（br）不支持、仅 Block 容器
  直子运行、inline-flex/grid/table 原子化。**0.x 破坏性**：Display
  +2 变体（"inline"/"inline-block" 解析结果变体名变）。全量 **417**
  测试绿（新增 tests/css_inline.rs 7 件；第五批⑧契约测试按 F1 语义
  重写为 display_inline_line_participation_f1）。
- **F2 结算 DAG 与文本效果五件（ADR-0022）**：①**结算 DAG**——帧内结算链
  硬编码 → `SettlePassKind{Calc,Tables,Columns,Floats,Lines}` 显式依赖图
  （Tables/Columns→Calc、Floats→Columns、Lines→Floats）+ 拓扑 `schedule()`
  （debug_assert 环防护）+ `settle_should_run` 空输入门（Calc=calc_deferred
  空/Tables=tables 空/Columns=multicols 空）+ `settle_run_pass` 分派——
  pass 顺序/豁免语义逐位保留，后续结算 pass 可插 DAG（声明式扩展点）。
  ②**text-overflow: ellipsis（css-overflow-3）**：槽 139；叶级截断经
  `text_overrides: HashMap<NodeId,String>` 绘制期覆盖（布局不受影响——
  浏览器同为 ink 裁切语义）+ `make_ellipsis_text` 二分字符前缀
  （width(prefix)+"…" ≤ 可用宽；溢出前缀→空串）+ `apply_text_truncation`
  两相（相 1 不可变扫描收集候选、相 2 分派——measure_rich 需 `&mut
  self.text` 借用约束）；PaintCtx 通道 + op_text 覆盖 + span 字节区间
  随前缀过滤/钳位；overflow: visible 不截断（spec）。③**-webkit-line-clamp
  （display:-webkit-box 习惯用法）**：槽 140（none|正整数，`min(i32::MAX)`
  防 as-cast 回绕）；截断预算=行数×行高（resolved_line_height_px 缺省
  1.2×font_size）；全文折行测量 ≤ 预算 → 不截断；"-webkit-box" 显示值
  接受并按块布局（偏差在案）。④**text-decoration 四长手+简写**：槽
  141–144（line 位集 1=underline 2=overline 4=line-through / style
  Solid|Double|Dotted|Dashed|Wavy / color（缺省 currentColor）/
  thickness Auto|FromFont|Length）；简写『line* | style | color |
  thickness』任意序贪心（line 多关键字累积 OR）；绘制=Text 基元
  `decorations`（厚度声明 LP 解析 px，auto/from-font≈font_size/12——
  B 级近似在案），sink 实绘线位 underline=baseline+descent×0.5/
  overline=baseline−ascent×0.9/line-through=baseline−ascent×0.5（字体
  优先装饰位=B 级），style solid/double 实绘、dotted/dashed/wavy v1
  以实线矩形近似（B 级在案）；span 级装饰=边界（v1 叶级）。⑤**
  text-shadow（css-backgrounds-3）**：槽 145（SLOT_COUNT 152→153）；
  `none | [<color>? <dx> <dy> <blur>? <color>?]#`（颜色前后均可置，
  缺省 currentColor/blur 0）；**继承**（inherits allowlist 登记）；绘制
  =Text 基元 `shadows` 影字先绘（transform 平移重发、spans 置空=全字
  影色），blur=B 级（vello 逐 run 模糊/软栅格字形模糊未启，锐利影
  落地）。锁测试：`tests/css_text_overflow.rs` 六件（ellipsis 截断/
  放得下不截/visible 不截/clamp 两行/clamp none 全文/clamp 放得下不截）
  + `tests/css_text_decoration.rs` 六件（简写解析携带/缺省空装饰/
  line 多关键字位集/影单+色/影多+blur/none 空）。**0.x 破坏性**：
  PropertyId/DeclValue +6 变体（SLOT_COUNT 152→153）、`PaintOp::Text`
  += `decorations`/`shadows` 双字段。全量 **431** 测试绿（36 套件
  0 失败，含差分逐位与 GPU 像素回归）。
- **hit_test 命中测试（F3a，ADR-0023）**：paint 期命中几何表——
  paint_node 递归收集 `HitRect{node_id, x, y, w, h, clips}`（border-box
  视口坐标+活跃 clip 链快照；子树 PushClip 登记 / PopClip 弹出同步），
  `PaintCtx.hit` 通道透传（RefCell 收集器，None=零成本跳过）；公共 API
  `StyleEngine::hit_test(x, y) -> Option<HitTestHit>`（绘制序逆序=顶
  优先，祖先 clip 链全含判定）与 `StyleEngine::node_id(key)`（用户键 →
  NodeId 宿主解释通道）。语义天然正确：`visibility: hidden` /
  `display: none` 子树不入表（paint 期已跳过）；`pointer-events: none`
  收集期排除（A1 语义位消费）。诚实边界（0.x）：变换节点=未旋盒
  （op 坐标为视口系、PushTransform 由 sink 终结——旋转命中为近似）。
  锁测试 `tests/css_hit_test.rs` 三件（顶层命中=后绘优先 / overflow
  裁剪外不命中 / pointer-events: none 穿透）。serde（同 ADR 后半）
  拆 F3a2 批——PaintOp 全变体类型化镜像工程量独立成批保完整绿。
  **0.x 破坏性**：`PaintCtx` +`hit` 字段。全量 **434** 测试绿（37
  套件 0 失败，含差分逐位与 GPU 像素回归）。
- **DisplayList serde 投影（F3a2，ADR-0023 决策 2）**：`paint_dump`
  模块（feature = "serde" 门控，serde 1 可选依赖）——PaintOp 全 14
  变体 typed tagged-enum 镜像（serde tag="op"；色=[f32;4] sRGBA 分量、
  ImageRes 像素=Vec\<u8\> 直序列、Border/Text/Gradient 全字段无损）；
  `DisplayList::to_dump()` 与 `DisplayListDump::to_display_list()`
  往返。枚举值 canonical 名承载（TextAlign/BorderStyle/WordBreak/
  OverflowWrap/TextDecoStyle/FamilyName/GradientKind/RadialShape/
  RadialSize）；non_exhaustive 值族同 crate 全变体枚举（新增变体=
  编译器强制更新镜像），未知单位/枚举名重建=缺省回退、未知 op=跳过
  （诚实边界，不 panic）。锁测试 `tests/css_serde_dump.rs`：富样式叶
  （渐变+边框+圆角+透明+变换+装饰+阴影+overflow）→ to_dump →
  to_display_list 结构往返相等 + serde_json JSON 通道往返相等
  （serde_json 仅 dev-dep）。全量 **435** 测试绿（38 套件 0 失败，
  含差分逐位与 GPU 像素回归；serde 特性加跑）。
- **背景全集（F3b，ADR-0024）**：css-backgrounds-3 §/§9 语义层——
  ①**六长手值族与解析**：background-repeat（RepeatXY 双轴
  Repeat/Space/Round/NoRepeat；repeat-x/y 简值展开双轴）、
  background-attachment（Scroll/Fixed/Local）、background-position
  （Position2D=两轴 PositionComp{base: LP, offset: Option<LP>}——
  edge 关键字/百分比/长度全值 + 三值四值偏移文法，right/bottom 偏移
  解析期取反=B 级简化）、background-size（`<bg-size>{1,2}` 斜杠对：
  Auto/Cover/Contain/Explicit{LP,LP}）、background-origin/clip
  （BackgroundBox BorderBox/PaddingBox/ContentBox；clip 额外 Text——
  text=B 级降级 border-box+解析警告）。②**多层列表**：全部七长手
  Vec 化（逗号层列表），`ComputedStyle::background_layers()` 层对齐
  视图——层数 = max(各长手层数, 1)、短列表按 i%len cycling 补齐、
  缺省长手以初始值参与（css-backgrounds-3 §3 语义）；background-image
  值族 Url/Gradient/None。③**background 简写**：八长手展开（层内
  组件无序贪心；单 `<box>`=origin 与 clip 双赋、双 `<box>` 首 origin
  次 clip；color 仅末层合法否则整条拒绝；position/size 斜杠对；空层/
  重复组件拒绝；缺省部件回初始值——transparent 规范形）。④**绘制
  精化**：背景色 FillRect 恒发（border-box+元素圆角）；层反序发射=
  首层最上（css 层序语义）；fixed 附件=视口锚定矩形；origin/clip
  语义=PushClip 裁剪盒（clip=BorderBox 用元素圆角、否则方角；每层
  Push/Pop 包裹）；平铺=tile 双循环 ceil 对齐（space/round→重复=B 级
  在案）；size cover/contain 精确数学、explicit LP、explicit 宽+auto
  高保纵横比；渐变逐 tile 重解几何（radial/conic 半径 [0;8]）；Image
  固有尺寸+repeat 平铺（未注册 url=警告跳过）。**0.x 破坏性**：
  PropertyId +6 变体（SLOT_COUNT 153→**159**，动画描述符让位
  152–158）、`DeclValue::BackgroundImage` 单值 → `Vec<BackgroundImage>`。
  锁测试：decl.rs 四件（全组件解码/多层缺省回初始/box 单双赋/三拒绝
  文法）+ computed.rs cycling 双向对齐 + paint.rs 多层反序发射序 +
  既有 css_images 十三件全绿；三 paint 快照重基线（PushClip 包裹/
  半径 [0;8]/平铺语义=精化在案）。全量 **440** 测试绿（38 套件
  0 失败，含差分逐位与 GPU 像素回归；serde 特性加跑）。
- **clip-path 裁剪形状（F3c，ADR-0025）**：css-masking-1 §5.1 /
  css-shapes-1 §3 语义层——①**值族与解析**：`ClipShape{None, Inset{
  insets, radius:Option<双集>, reference}, Circle{radius, at, reference},
  Ellipse{rx, ry, at, reference}, Polygon{nonzero, points, reference},
  Other}` + `ClipRadius{Length(LP), ClosestSide, FarthestSide,
  ClosestCorner, FarthestCorner}`；文法 `<basic-shape> || <geometry-box>`
  次序不限，geometry-box（border/padding/content/margin-box——margin 降
  border=B 级）单独出现 = inset(0) 基准该盒、reference 平铺进四形状；
  `url()`/`path()` 宽容收容 Other（cssparser UnquotedUrl 顶层 token 与
  Function("url") 两形态都接住）+ tracing 警告（SVG clipPath 资源=T2）；
  `polygon()` fill-rule 缺省 nonzero、坐标对 <3 整条拒绝（Chromium 同
  判）；circle 百分比半径拒绝（轴向歧义，css-shapes-1 §3.2.1）、负半径
  拒绝；inset round 双集（`5px / 10px`）精确解析。②**计算**：复用既有
  `PropertyId::ClipPath`（第四批④ slot 71 原位升级，Effect 组剥离——
  **零 slot 破坏，SLOT_COUNT 保持 159**，动画描述符 152–158 不动）；初始
  none、非继承（css-masking-1）；`has_clip_path()` = 形状≠none（SC 触发
  语义保持，`inset(0)` 亦触发）。③**绘制**：新
  `PaintOp::PushClipPath{ points: Vec<[f32;2]>, nonzero: bool }`（视口
  坐标；PopClip 复用）——inset 走既有 `PushClip` 矩形（round radius 精确
  承载）；circle/ellipse 折算 **64 段折线**（面积误差 0.14%=B 级在案）；
  polygon 顶点直传 + fill-rule 随 op 传递；裁剪作用于该元素及子树
  （overflow 裁剪后、背景前，LIFO 双 PopClip）；命中=AABB 链近似
  （B 级）；量程不变（Chromium 同语义）。④**sink 与 serde**：vello
  BezPath 折线 push_layer（NonZero/EvenOdd 挑选）、soft 射线法
  point-in-polygon（nonzero winding / even-odd 双规则）、paint_dump
  serde 镜像同步（OpDump::PushClipPath + nonzero）。锁测试：decl.rs 五件
  （inset 全形态/circle-ellipse 含拒绝/polygon 文法/geometry-box 双序/
  宽容与拒绝）+ paint.rs 六件（inset op 几何/64 段端点/polygon 点序与
  fill-rule/参考盒位移/url-none 零 op/overflow 复合序）+ soft 两件
  （多边形像素内含/{5/2} 五芒星序 nonzero-evenodd 像素差分）+
  css_serde_dump 往返一件 + computed 初始与非继承一件；效果触发 SC 测试
  重基线（inset(0) 实发 PushClip+PopClip 对 = +2 op）。**0.x 破坏性**：
  `PaintOp` +`PushClipPath` 变体（non_exhaustive，宿主 match 需新臂）。
  全量 **456** 测试绿（工作区 serde,text 全跑 0 失败）。
- **border-image 全集与字体深化（F3d，ADR-0026）**：css-backgrounds-3 §6
  / css-fonts-4 语义层——①**值族与解析**：五长手 border-image-
  source（复用 BackgroundImage{None,Url,Gradient}）/slice（1–4 值+fill
  任意位，负值拒绝）/width（LP|number|auto 三态）/outset（LP|number，
  百分比拒绝）/repeat（stretch|repeat|round|space ×双轴）+ **简写**
  `<source> || <slice> [/ <width>? [/ <outset>?]?]? || <repeat>`（词法
  不相交贪心单遍，shorthand_exists/longhands/expand 三表齐）；PropertyId
  +10（五 border-image 槽 152–156 + font 族五槽——font-stretch/
  word-spacing/font-features/font-variations/font-variant-caps），
  SLOT_COUNT 159→**169**。②**计算**：border-image 五长手非继承、font
  族继承（css-fonts-4）；ComputedStyle 访问器 ×10；font-stretch 百分比
  50–200% 钳制、九关键字（css-fonts-4 映射表）；word-spacing 归一
  LenAuto。③**绘制九宫格**：source≠none **替代 Border op**——四角恒
  拉伸→四边 tile（双轴独立）→fill 中心；切片溢出缩放与带宽缩放（基准=
  边框盒）；outset 仅 ink-overflow；Repeat 末片 PushClip 截断、Round 等
  分、Space 首尾贴边（改进近似=B 级在案）。④**op 精化**：PaintOp::Image
  +src 采样窗口四字段（vello 裁剪层+非均匀仿射、soft 源窗口采样）；
  PaintOp::Gradient +LinearGeom 绝对几何（vello new_linear、soft 线段投
  影）。⑤**@font-face 登记表**：FontFaceRule 全描述符（family/src
  （url|local+format）/style/weight 区间/stretch/display/unicode-range
  '?' nibble 通配/features/variations）；登记语境不设限（条件组/嵌套均
  登记）；缺 family/src=warn 丢弃；未知描述符宽容；
  `engine.font_faces()` 访问器（user→主表→附加表序、同族后规则胜）；
  字体字节仍宿主 add_font（契约不变）。⑥**字体深化接线**：PaintOp::Text
  +4（stretch/word-spacing/features/variations 基样式级）；vello
  FontWidth/WordSpacing/FontFeatures/FontVariations push_default（parley
  原生）、soft 伪拉伸+空格词距；effective_font_features（caps 派生
  titl/unic 并入，显式 settings 同 tag 优先）；small-caps 合成
  （uppercase+0.8 缩放 span；先 text-transform 后合成）。**修复两既有
  缺陷**：@media/@container 漏并子表 keyframes；parse_font_stretch 百分
  比漏 ×100（锁测试抓出）。锁测试五面（九宫格/tile 截断/渐变九区/
  @font-face 登记/small-caps 测量组合/serde 往返）。**0.x 破坏性**：
  SLOT_COUNT 169、PaintOp::Image/Gradient/Text 新字段、
  AtPrelude::Skip→FontFace、Stylesheet+font_faces。全量 **477** 测试绿
  （462→477，serde,text 全跑）。
- **基础设施批（F3e，ADR-0027）**：①**人读调试工具链**——新增 `debug`
  模块 `display_list_dump`（Push/Pop 作用域缩进树）+ `ComputedStyle::
  debug_dump`（全 169 槽逐属性）+ `Frame::boxes_dump`/`layout_tree_dump`
  （0.x 加性公共 API；纯 core 格式化，零新依赖）；锁测试断言缩进结构。
  ②**clip-path circle 百分比半径修正（落地修正）**——cssparser
  `parse_nested_block` 对闭包未消费 token 整块改判 Err，circle(50% at …)
  因 % 半径被拒致整条声明丢弃（F3c 误读 spec）；按 css-shapes-1 §3.2.1
  放开 % 半径（基准 √(w²+h²)/√2、ellipse 逐轴宽/高；负半径仍整条拒绝），
  conformance 新增 `tests/pixel_invariants.rs` 七锁（渐变单调/背景层序/
  circle 遮角/border-image 环/hard box-shadow 墨迹/opacity 混合/确定性
  渲染）全绿。③**fuzz 扩面三靶**——@font-face 注册表不变量、绘制声明
  文法不 panic、hit_test⊆边框盒；④**clippy 清零**——78→0（`-D warnings`
  全 workspace：type_complexity 别名化 8 处（ImportLoader 为 0.x 加性
  pub 别名）、ptr_arg/parens/while-let 等机械修 19、too_many_arguments
  带因 `#[allow]` 7）。全量 **490** 测试绿（477→490）。
- **属性面补全（F4，ADR-0028，31 项收口）**：①**backdrop-filter**——
  filter 同型存在性位（`parse_sc_effect` 复用：值 ≠ none → `Effect(true)`、
  none 缺席、宽容吞咽）+ css-filters-2 SC 触发（`has_backdrop_filter()` 并
  入 paint 谓词，非定位触发者 Pos 带键 0；触发表锁扩行）；效果本体=T2。
  ②**hyphens**——`HyphensKind{Manual#[default],None,Auto}` 属性层（initial
  =manual、继承、`ComputedStyle::hyphens()`）；断词效果边界在案（上游
  parley UAX 14 分段）。0.x 破坏性（加性）：SLOT_COUNT 169→171、
  PropertyId +2/DeclValue +1 变体、新公开枚举 `HyphensKind` + 访问器
  `hyphens()`/`has_backdrop_filter()`；动画描述符槽整体后移 ×2。锁：
  `hyphens_parse_modes`/`backdrop_filter_parse_presence` + engine SC 触发
  表扩行。全量 **492** 测试绿（490→492）。
- **rem 基准修复（P0 批，含文档同步）**：`MediaEnv` 新增 `rem: f32`
  （默认 16.0，0.x 加性）——此前全部 `ResolveCtx` 构建位点 rem 硬编码
  16.0，`:root{font-size:20px}` 下 `width:2rem` 解析为 32px（应 40px，
  A 级偏差 FEATURES.md rem 条目移除）。接线全链路：`map_env()` 统一
  填充 `env.rem = rem_base()`（新辅助：文档根 font-size，经 root_key→
  key_to_node→styles 查询，缺根回落 16.0——对齐 ADR-0010：仅文档根
  定义 rem 基准，首帧/重样式前无根样式时回落初始值）；restyle_node
  环境线程化（env 自顶向下携带，文档根 font-size 计算后原位更新
  env.rem 供子树级联/测量/绘制同源消费）；style_channels 加 env 参数、
  computed（行高/字距/词距解析与 font-size 步骤）/layout（map_ctx 与
  defer_calc 延迟结算快照）/paint（5 处）全改读 env.rem。语义
  （CSS Values 4）：非根元素 rem=文档根计算字号；文档根自身
  font-size 声明内 rem=初始 16px；根的其他属性 rem=新根字号；
  @media/@container 条件内长度仍 16px（解析期无布局上下文，在案）。
  修复映射期根判定：is_doc_root 经 root_key 精确判定（合成超根架构
  下 parent_id.is_none() 只匹配超根、不匹配用户文档根）。新增锁定
  测试 rem_follows_root_font_size（:root 20px+子 2rem=40px）、
  rem_on_root_font_size_uses_initial_value（:root font-size:2rem →
  计算字号 32px）。随批文档同步：FEATURES.md rem 条目（A 级移除）
  与 T2 fixed/sticky 条目更新（A4 已落地、残余=宿主滚动吸附）、
  DEPENDENCIES.md taffy calc 结论复评修正（resolve_calc_value 公开
  trait 方法、接入路径存在，维持结算式直通系工程性选择记 B 级重估）
  + vello 0.11.0 发布注（锁 wgpu 30，升级阶梯候选）、
  SETTLEMENT-PIPELINE.md 行号基准更新（engine.rs 8547 行，符号名
  锚定约定）。全量 **494** 测试绿（492→494）。
- **渐变停点位置修复（P1-0 批，双 sink 对齐）**：vello sink
  `distribute_stops` 此前以 basis=0 解析显式停点——Percent 全塌 0、
  px 未按渐变线长归一（`50px`/`100%` 均按 0 处理）。修复：签名加
  `line_len: f32`，px→v/line_len 夹 [0,1]、%（存储即分数）夹 [0,1]、
  其余单位（em/rem/cq）退化为 NaN→自动均布（B 级豁免在案）；NaN
  填充后按 css-images-3 §4.5.2 逆序停点单调夹取（后停<前停抬至前停；
  解析器不做夹取、用值期语义归 sink）。各几何臂接线：linear=渐变线
  长（geom 与回落公式两臂）、radial=ry.max(0.5)（与画刷实际用半径
  一致）、conic=1.0（sweep 停点=角分数全周）。soft sink
  `stop_positions` 补同款单调夹取（双 sink 一致性契约）。新增锁定
  测试 gradient_stop_positions_normalize_and_monotonic（200px 线：
  0%/50px/缺省/100%→0.0/0.25/0.625/1.0；逆序 60%+40%→[0.6,0.6]）。
- **mix-blend-mode / isolation 原生混合层（P1-2 批，css-compositing-1/2）**：
  两属性从「仅 SC 触发位」升级为真实混合层——新公共枚举 `BlendMode`
  （16 标准模式 + plus-lighter/plus-darker 全 18 值）、`DeclValue::BlendMode`
  入库（替换旧 `Effect(bool)` 存在位）、`PaintOp::PushBlend{mode,x,y,width,
  height}/PopBlend` 层对（bbox=border-box 同 PushOpacity；混合层须最外——
  先 PushOpacity push、后 PopOpacity pop，合成序=blend(背后画布,
  opacity(子树))）。`isolation: isolate` ≡ `PushBlend{Normal}` 隔离组
  边界（子树内混合不越界，修旧「isolate 内混合穿透祖先画布」缺口）。
  vello sink：peniko `Mix` 16 标准模式一一映射（plus 族不在 Mix 枚举退
  Normal=B 级，上游缺口记 DEPENDENCIES）；soft sink：全 18 种原生像素
  合成（快照底+清区累积+pop 按模式全式合成 Co=αs(1−αb)Cs+αs·αb·B+
  (1−αs)αbCb；非可分离按 W3C Lum/Sat 定义；plus 族按预乘加法惯例——
  Normal 臂漏全式中项的 bug 即由像素锁测试捕获后修复）。测试：SC 层对
  发射/发射序（blend 包 opacity）、解析语义重基线（isolation/mix-blend
  各 +2 op）、soft 像素锁 ×10（可分离/加法/非可分离/嵌套 opacity）、
  serde 往返（kebab-case 模式名）。顺修既有耦合：paint_dump 三个 serde 往返
  测试补 `#[cfg(feature = "serde")]` 门（默认 feature 集下 `cargo test
  -p style-engine --lib` 因此编译不过，属 P1-2 之前已存在的问题）。文档：
  FEATURES T2 条目改写（filter/
  backdrop-filter 保持暂缓）+ T0 混合层新条 + DEPENDENCIES vello Mix
  缺口节。全量 **499** 测试绿（494→499）。
- **repeating-*-gradient 解析与双 sink 平铺（P1-3 批，css-images-3）**：
  `repeating-linear/radial/conic-gradient()` 三族全收——分派器按函数名
  strip `repeating-` 前缀复用三解析器（内层文法逐一相同），
  `css::Gradient.repeating: bool` 随 BackgroundImage→PaintOp::Gradient
  流入双 sink。周期=首末停点跨距，停点模式沿渐变轴无限平铺；显式逆序
  抬升后首末重合 → 周期 0 → 透明黑（source-over 无操作）；全缺省停点
  退化为非 repeating。vello 终结：画刷几何收缩为「一个周期」
  （linear 单位向量缩段 / radial `new_two_point` 两圆承载 r0 首停相位 /
  sweep 起终角弧段）+ stops 平移归一 + `Extend::Repeat`——径向必须
  new_two_point（r0=0 平移 stops 会丢相位 first）；soft 终结：采样
  `u = first + (t − first).rem_euclid(period)` 回停点序列插值（t 不
  夹取，CSS 无限平铺）。`GradientDump.repeating` serde 往返
  （`#[serde(default)]` 旧 dump 兼容）。锁定：解析三族 flag/kind、
  标记流入 DisplayList、serde 往返+旧 dump 回落、vello 段收缩与周期 0
  纯 fn 两件、soft 像素锁两件（周期 20px 手算 (249,0,6)/(185,0,70)/
  (121,0,134)、x 与 x+20 同色；周期 0 画布不变）。全量 **506** 测试绿
  （499→506）。
- **margin collapsing 重估与 padding 长手修复（P1-1 批）**：
  ①**重估结论（css2.1 §8.3.1 / css-box-4）**：块流父子-兄弟 margin 折叠
  在结算管线全路径正确——父首子顶塌穿（父无 padding/border/clear 时
  mt 出父外）、兄弟正负取 max(|max正|+|min负|)、空块自塌穿、浮动子
  不参与折叠、结算 pass（tables/columns/floats/lines）不扰动塌缩；
  taffy 原生 `CollapsibleMarginSet` 自 0.10 起承载块流折叠语义，引擎
  块流直通 taffy 无二次折叠，浮动由引擎 `settle_floats` 手管（taffy
  Style 不映射 float，历史结论维持）。此前「margin 折叠缺口」疑虑
  解除：浮动/清除/折叠三面均有像素级锁定。新增
  `tests/css_margin_collapse.rs` 七件锁（塌穿/空块/负 margin/浮动/
  结算穿越/简写+padding 阻断折叠/calc 与逻辑 padding）。
  ②**A 级修复：padding 物理与逻辑八长手静默归零**——解析层误路由
  `parse_corner_radius` 产出 `DeclValue::Radius`，而计算（`len()`）、
  布局（padding Rect→`lp_defer(CalcAxis::Padding*)`）、绘制
  （`.len(PaddingTop)`）全部只认 `DeclValue::Len` → padding-top/left
  等长手写法整条链失效（仅简写 `padding:`/`padding-inline:` 正常）；
  既有测试全走简写未暴露，calc 长手钳位测试腿为虚掩（丢弃→0 与钳位
  →0 同值）。修复：`property.rs` 解析派发 `PaddingTop/Right/Bottom/
  Left` 与 `PaddingInline/Block 四向` 改路由 `parse_len`（radius 四角
  不变）；`wide_inherit_forces_non_inherited_property` 断言由固化
  Radius 族改回 Len 族。锁定：长手生效+折叠共存、calc(0%+10px)
  正腿、padding-inline-start 逻辑向、inherit 逐字复制。
  全量 **513** 测试绿（506→513；cascade_layers 修正为断言重写不计新增，
  新增 7 件折叠锁）。
- **soft sink 真 blur：box-shadow / text-shadow 遮罩盒模糊（P1-4 批）**：
  soft sink「blur 忽略」偏差升级为真模糊管线——①形状 alpha 遮罩：
  Shadow op 在纯平移矩阵下构造设备空间 u8 遮罩（outset=外扩 spread 的
  圆角矩形，圆角随 spread 增缩并钳半宽防退化椭圆；inset=盒内减平移
  扩展矩形，合成期钳回盒内）；②3×可分离盒模糊（σ=blur/2，盒宽
  w=⌊√(4σ²+1)⌉ 取奇——三遍合计方差 ≈σ²；u32 窗口累加取整
  `(sum+len/2)/len`，全程整数运算+固定遍历序，逐位确定、像素测试可
  锁）；③遮罩 pad=⌈3σ⌉（高斯 99.7% 能量界）∩画布；④着色合成
  alpha=color.a×mask/255 逐像素 src-over（clips 之外、inset 盒外跳
  过）。旋转/缩放矩阵回退平移矩形近似（B 级在案）。text-shadow
  blur>0 = 字形折线遮罩（fill_polygons 同款扫描线 16 级覆盖、写入取
  max 重叠不叠加）+ 同款盒模糊 + 着色，装饰线不投影（Chromium 同语
  义）；blur=0 保持平移重发原路径（既有像素输出逐位不变）。Text op
  折线构建重构为 `text_device_polys`（每字符一组轮廓保持逐字符填充
  粒度、装饰线组附色——draw_text 消费端字节等价）。新增锁定测试五
  件：盒影软化+单调衰减+pad 外零+双渲染逐字节一致、spread/圆角形状、
  inset 钳盒、text-shadow 遮罩可见+确定性、blur=0 与平移重发逐字节
  一致、旋转矩阵回退。全量 **518** 测试绿（513→518；soft 30/30）。
- **transition-* 全量：CSS Transitions 落地（P1-5 批，ADR-0032）**：
  ①属性面 5 新槽位（SLOT_COUNT 176）——transition-property（None/All/
  Ident 列表，custom-ident 合法保留）、duration/delay（`<time>#`，
  duration 拒负、delay 允负快进）、timing-function（复用 TimingFn 共
  文法）、behavior（Normal/AllowDiscrete）+ 简写 `<single-transition>#`
  （顺序自由、首 time=duration 次=delay、三 time/负 duration 报错整条
  忽略）；②reconciliation（restyle 提交点、styles.insert 前）：新值==to
  保持运行（容器收敛环保护）> 新级联==生效值取消 > combined≤0 取消/
  不启动 > 重定向（from=当前插值中间值）> 可插值探针（离散对需
  allow-discrete）> 动画覆盖槽抑制启动（偏差在案）；描述符槽自身不可
  过渡；③采样挂点 frame()（restyle 后、动画前——animation 层高于
  transition 层）；延迟段写 from、进度≥1 写 to 移除、离散 50% 翻转；
  过渡表空=稳态零写入；④**引擎修正**：TimingFn::Steps 语义分支写反
  （jump-end 应为 ⌊p·n⌋/n，原走 ceil）对调修复；⑤**动画结束恢复底层
  值**：apply_animations 结束且无填充时原「不覆写」≠「恢复 underlying」
  （styles 表残留最后动画采样值）——新增 anim_underlying 副本机制
  （首见快照关键帧槽位当前值、外部重算自动刷新、结束写回并移除、
  forwards/both 终值固定即清、节点移除三处卫生同步）；⑥锁定测试
  19 件（解析 5 + 驱动 7 + none/all + steps 采样点 + 离散双语义 +
  动画覆盖 + 稳态幂等 + 文本 color 过渡）。全量 **545** 测试绿
  （518→545）。
- **filter / backdrop-filter 本体化（P2 批，ADR-0031）**：F4 批宽容存在性
  （值丢弃 → Effect(bool)）升级为全函数族本体——①解析：`FilterFn` 十函数
  枚举（Blur/Brightness/Contrast/Grayscale/Sepia/Saturate/Invert/Opacity/
  HueRotate/DropShadow；长度/颜色活到计算期：`LengthPercentage`/`ColorValue`
  承载，绘制域 `paint::FilterEffect` 终结），`parse_filter_value_list`
  严格文法取代 `parse_sc_effect`（白空格分隔无逗号、未知函数/参数非法
  整条拒绝、`url()` 拒绝=T2、钳位不拒绝 grayscale/sepia/invert/opacity∈[0,1]），
  `none` → `Filters(vec![])` 有效声明覆盖语义（级联胜出，非缺席）；
  ②绘制：`PushFilter/PopFilter` 层对（bbox 同 PushOpacity）+
  `BackdropFilter` 单点即时 op（节点最前发射、主画布区域替换），
  层序 transform→clip→blend→opacity→filter（合成序 opacity(filter(子树))，
  css-filters-1 §3），收尾严格 LIFO；③soft 原生逐像素管线（P1-4 盒模糊
  基建扩展）：颜色矩阵族（css-filters-1 §4 sRGB 表）/仿射/blur(σ=r/2)/
  drop-shadow（遮罩模糊平移着色先影后源）/opacity 全函数集，FilterLayer
  快照-清空-回合成，非预乘直排 src-over（filter 后全透明 → 保留快照）；
  ④vello：纯 opacity 链 alpha 连乘直映，其余 warn-once 恒等层降级
  （B 级在案，等待 vello filter 原语）；⑤serde：`FilterEffectDump`
  （tag="fn" kebab-case）+ OpDump 三变体往返。锁定测试 +6：严格解析
  （18 正例 7 拒绝例）、backdrop 严格契约重写、三重嵌套 LIFO、SC op 增量
  （filter+2 层对/backdrop+1/will-change+0）、soft 端到端像素三件
  （invert 层/backdrop 区域替换/透明保留快照）。全量 **551** 测试绿
  （545→551）。
- **vertical-align 基线对齐 / IFC v2（P3 批，ADR-0034）**：F1 行模型
  TOP 硬编码升级为 vertical-align 全值族——①属性面新槽位
  `vertical-align`（SLOT_COUNT 177，九变体 `VerticalAlignKind`，
  八关键字 + `<length-percentage>`（% 存小数），严格文法尾 token
  拒绝；不继承，初始 baseline）；②测量层加法式
  `measure_with_baseline -> (宽, 高, 首行基线)`（measure_two_pass
  三元组化，首行首 run `metrics().ascent.round()`；旧三签名不变）；
  ③行结算两阶段化：TOP 装箱收集 `PendingLinePart`（基线距/字号/
  字体度量）→ 行尾 `flush_inline_line` 统一结算——L=max(基线距)、
  逐参与者求 dy 回填 inset.top、行盒扩展 max(参与者盒底)、bottom
  二遍对齐行底；Box 基线探针 `box_first_text_baseline`（子树首文本
  叶，无文本=盒高）；④字体度量扩展：fontprobe hhea asc/desc
  （`ascent_per_em`/`descent_per_em`，abs 归一，缺失回退 0.8/0.2）
  注入 FontMetrics——text-top/text-bottom strut 与 middle x-height
  消费面；⑤偏移语义：length（px/em/rem/% 行高/视口）、sub/super
  ±0.34em（兜底 B 级）、middle=x-height/2 对齐、top=装箱即位、
  bottom=行底、baseline 不偏移（v1 逐位一致回归锁；混字号默认
  基线下沉 B 级在案）、calc() 偏移=0（B 级）。锁定测试 +8：解析
  全值族 12 例 + 拒绝 4 例、measure_with_baseline 线性/空文本、
  行为锁 6 项（无声明 vs baseline 逐位一致、length 精确 −10px、
  super/sub ±5.44px + 行盒扩展、middle 方向序、盒基线=子文本基线、
  top/bottom 对齐）。全量 **559** 测试绿（551→559）。
- **绘制 B 级收敛批（P4 批，ADR-0037）**：六项绘制近似收敛——①
  **dashed/dotted 边框拆段**（ADR-0037 D1）：`emit_borders` 直角框按边
  拆 FillRect 序列（Dashed 段 2t 步进 3t 首对齐末段不足不画；Dotted
  圆点直径 t 中心距 2t，方形近似 B 级），圆角框含花式线型整框退
  Solid（弧上虚线 B 级），outline 通道复用自动受益；②**bg-repeat
  round/space 轴精确化**（D2）：`TileAxis{None,Repeat,Space,Round}` 替代
  bool——Space 均布 gap 首片锚定定位区起点（position 失效）、Round
  整数片拉伸 ts=area/n 网格恰铺满、n≤1 退化单片（position 生效），
  per-tile 渐变几何随 tile 尺寸；③**wavy 文本装饰真波形**（D3）：
  soft/vello 同参折线闭环（周期 6t、振幅 2t、每周期 8 段、带厚沿波
  平移），Double 双半厚带/Dashed 2t-3t 段/Dotted 圆环折线全家族落地；
  ④**clip-path 命中精确化**（D4）：`HitClip`（Rect 圆角/Path 折线
  nonzero-eo）+ HitRect 携带活跃仿射 mat 与 clip 逆阵——命中点先经
  mat 逆变换测盒、clip 逐个经自身 inv 判定（圆外/凹角/变换后点
  均精确）；⑤outline auto 语义注释升级（D5）；⑥**clip-path 圆/
  椭圆段数自适应**（D6）：clamp(ceil(2πr/3),16,256) 替代固定 64
  （小圆减 48 段、大圆升平滑，serde 锁随动 64→63）。**附带补齐
  border-top/right/bottom/left 四向简写**（<'border-width'>||
  <'border-style'>||<'border-color'> 任意序，缺省 medium/None/
  currentcolor）——测试暴露的简写家族缺口收口。锁定测试 +13：
  边框拆段×4（dashed 8 段/dotted 13 点/圆角退化/outline dashed 受益）、
  repeat 几何×3（space gap/round 拉伸/直测五轴）、命中×2（clip 圆外
  穿透/transform 旋转盒）、装饰像素×2（wavy 纵跨/double 双带列扫）、
  自适应段数×1、四向简写×1。全量 **572** 测试绿（559→572）。
- **UA 起源样式表 + counters/quotes 生成内容（P5 批，ADR-0033/0036）**：
  ①**UA 表挂点**（ADR-0033）：`ua_sheet` 字段 + `set_ua_stylesheet`/
  `clear_ua_stylesheet`（镜像 user 表五步链）；级联收集序
  Default→**UA**→User→Author（cascade_declarations/cascade_channel 尾参
  ua_sheet，::selection/::placeholder 同挂）；@property/@font-face 合并序
  UA 先于 user；`pub mod builtins::DEFAULT_UA_SHEET`（HTML 语义最小表：
  块级清单/h1–h6 字号边距/bold/italic/装饰线/monospace/pre——b/strong
  bold 物化、small/big 绝对关键字 B 级在案）；默认不装载（中立契约）；
  important 反转链锁实测：UA-important(6) > Author-important(4)（UA
  无障碍语义，css-cascade-5 反转）。②**生成内容扩展**（ADR-0036）：
  content 升级 `<content-list>`（`ContentPiece` 九变体 Seq 承载：
  counter()/counters()/attr()/open-quote 族；url() 维持拒绝）；新槽位
  counter-reset/counter-increment/quotes（SLOT_COUNT 180；`[ <ident>
  <integer>? ]+` 空格分隔、重复 ident 后者胜；quotes auto=拉丁四引号
  内置对）；`sync_pseudo_text` 重写树序 DFS 求值 pass——计数器帧栈
  （reset 压帧遮蔽、increment 全栈累加、**merge 弹出=兄弟继承**）、
  counter() 最内帧/counters() 全帧 join、attr() 读 originating element、
  open/close-quote 深度配对（close 深度 0 静默）；map_style 伪节点
  Display 判定随 Seq 形态升级（回归保护）。锁定测试 +8：UA 级联序与
  important 反转、内置表装载/清除、counter 解析 15 例、树序计数
  1/2/3、reset 作用域 1/1/1、嵌套 join "1.1"、attr 存在/缺失、quotes
  配对/越配静默。全量 **580** 测试绿（572→580）。
- **:has 失效收窄（P6 批，ADR-0035）**：含 `:has()` 规则的表此前把整个
  增量通道全量化（any_has_rules → 每帧 restyle()）；升级为**宿主键快筛
  收窄**——①`HasHostKey{tag,classes,id}`（Default=哨兵恒通过）+
  `list_contains_has` 深扫 Is/Where/Negation 参数内嵌套（防 `:is(:has)`
  绕过索引漏升级）+ `has_host_key`（首 sequence 组件收集，前缀组合器
  不合格返 None）；②`has_host_index` 随表重建（挂全部 5 个
  rebuild_font_faces 调用点），规则级零合格键/任一不合格 → 哨兵；
  ③`has_invalidation_needs_full`——dirty 根沿祖先链命中任一键 → 全量
  升级，全否决 → 增量 subtree 通道（快筛只多升级不漏升级）；④**结构
  失效收口**：remove（非根）+`:has` 此前只标 dirty_struct 不重算（B3
  起失配缺口）——存活父 push style_dirty_roots；⑤`any_container_rules`
  补 user_sheet/ua_sheet 漏检、`any_has_rules` 补 ua_sheet（P5 对称
  缺口）。锁定测试 +5：键提取五类矩阵/判定四 case/user 表 @container
  检出/无关子树否决走增量+remove 失配捕获/前缀组合器哨兵命中。全量
  **584** 测试绿（580→584）。
- **span 富文本 soft 落地 + 表格 max-content 列 + 多动画组（P7 批）**：
  ①**soft 端 TextSpanPaint 全消费**（vello 端 T5c 已有，soft 端此前丢弃
  spans——两端收口）：`text_device_polys`/`draw_text` 加基色+spans 参数，
  逐字符按字节偏移归属 span（后 span 胜同 vello rev-find）、span 感知
  颜色/字号/族（族切换重解析 SoftFont、字号≠基重算 scale），字形着色
  逐组 fill_polygons；影字路径传真实 spans（形状感知）、blur>0 影子
  取基样式形状（B 级在案）；装饰线仍基样式单行近似（B 级）。锁定测试
  +2（双色分段/字号混合高度）。②**表格 auto 列 max-content 比例分配**
  （v1「等分剩余」偏差退役）：`table_column_template` 加尾参
  `content_max: &[f32]`——**破坏（T-签名）**：调用方需追传内容宽度切片
  （等价旧行为传 `&[]`）；auto 列按内容 max-content 比例分剩余宽
  （m>0 且总量>0），全零回退等分，空列 0 宽（Chromium 一致）；引擎侧
  `content_max_width` 子树文本叶测量兜底（taffy MaxContent 探针对文本
  =0，同 settle_lines 范式）+根格水平内缩；嵌套盒结构组合近似单叶最大
  （B 级在案）。锁定测试 +4（单测比例/零回退/空列零宽+引擎级比例与
  等宽对照）。③**多动画组**（css-animations-1 `<single-animation>#`）：
  七描述符列表化（`AnimationNameList/AnimationTimeList/
  AnimationIterationList/AnimationTimingList/AnimationDirectionList/
  AnimationFillModeList`，**T-扩展**加法式——旧单值变体保留兼容，
  解析器恒产列表）；描述符按 `i % len` 循环补齐（组数=name 列表长）；
  `apply_animations` 组循环逐组采样（后组胜同槽覆写、结束无填充组
  逐槽恢复底层值、underlying 快照节点级共享锚定级联值、全组结束清
  副本）；`animation` 简写升级 `<single-animation>#` 多组（组间逗号、
  组内缺省回 CSS 初始、空组整条拒绝）；animation_covered_slots 组
  并集。锁定测试 +3（解析九正五拒/简写两组缺省物化/双组并行
  both-无fill 三时刻/循环补齐两组同 duration）。全量 **594** 测试绿
  （584→594）。


### 阶段6 — 实施期 Phase 0：治理文档与测试基座（前置）

- **vello 像素回归（前置⑤）**：Pixel 通道新增 GPU sink 腿——`style-engine-vello`
  新公共 API `render_offscreen`（DisplayList+VelloTextSystem → 紧密 RGBA8；
  scale≠1 经 `Scene::append` 注入缩放），conformance 新增 `tests/pixel_vello.rs`
  自动遍历 [pixel] 用例对 Chromium golden（预算=max_ratio×5 下限 0.005、
  allowed∪{1,2}，Class 3–5 恒零容忍；跳过语义同 GPU 探针惯例）——绘制特性
  批次落地前先建立 GPU 侧像素级保护；pollster 升入 vello crate
  [dependencies]，conformance 对 vello sink 仅 dev-dependency（wgpu 不进主
  依赖树）。
- **proptest 差分基座（前置⑥）**：`crates/style-engine/tests/differential.rs`
  ——不变量「增量引擎链 ≡ 全量重建」：同一随机操作序列（插入/移除/子序
  重排/类/状态/文本/声明），hot 引擎逐 op 推进并每步 `frame()`（增量路径
  全缓存跨帧存续），cold 引擎每步从零重放单帧出结果，布局盒/DisplayList/
  滚动量程三面逐位相等；双样式表跑两轮（无 @container 命中
  `restyle_subtrees` 增量路径 / 有 @container 退全量+容器快照收敛）；附带
  锁定 frame() 幂等不变量（generation 除外）。proptest 仅 dev-dep
  （C4 纪律）。
- **proptest fuzz 基座（前置⑦，Windows 无 libFuzzer → 结构化生成器）**：
  `crates/style-engine/tests/fuzz_grammar.rs` 五靶点——①属性文法 ②简写
  展开（引擎 roundtrip）③var() 代换（环检测/fallback 递归）④@container/
  @media 条件 ⑤capture_tokens 序列化回环（序列化→再解析→再序列化收敛）；
  语义锁：TRBL N 值展开（margin 族存 LenAuto、padding 族存 Len）、
  var 环 → fallback 胜出（css-variables-1 §3.5）、32 级 fallback 深链终止。
- **ComputedStyle 宿主读通道（缺口补全）**：`values`/`custom` 此前全私有，
  `computed_style()` 返回值对外不透明（宿主可读契约缺口）——新增公共
  访问器 `ComputedStyle::value(PropertyId) -> Option<&DeclValue>` 与
  `custom_value(&str) -> Option<&str>`（加法演进，T-扩展）。
- **治理文档七份**：`README.md`（定位声明/快速上手/文档索引）、
  `docs/BREAKING-POLICY.md`（T-扩展/T-签名/T-契约 三级 + 五处预告契约推翻 +
  兼容层生命周期）、`docs/FEATURE-GATES.md`（核心语义不设 gate + 31 项归位
  表）、`docs/SETTLEMENT-PIPELINE.md`（frame() 16 挂点 I/O 契约 + DAG 化
  准入）、`docs/INVALIDATION.md`（失效源→失效标传播图 + B1/B2/B4 接入
  预检）、`docs/PROPERTY-CHECKLIST.md`（属性七件套流程）、
  `docs/BEHAVIOR-HINT-PROPS.md`（宿主可读非绘制属性模板）、
  `docs/IMPLEMENTATION-LOG.md`（实施期权威日志）。

### 阶段5 — 性能预算与 CI 门控（C5）

- **增量重样式（㉙ 升级落地）**：`set_declarations` 不再整树置脏，改为登记脏根
  （`style_dirty_roots`），`frame()` 无容器规则时仅重算「自身+后代」子树
  （`restyle_subtrees`：脏根过滤→表格/多列子树保守退全量→祖先链重建容器栈→
  子树 restyle）；`RestyleGuard`（pass 计数）对互为祖先/后代的脏根去重。
  正确性域：节点样式求值只依赖自身/祖先树数据、继承父样式、祖先容器快照；
  有 @container 规则在场退全量。实测 1000 盒树逐 tick 增量：27.684ms →
  0.489ms（**56×**）；布局与 DisplayList 重建保持每帧全量（余量充足）。
- **样式存储槽位化**：`ComputedStyle.values` 从 `BTreeMap<PropertyId,DeclValue>`
  改为 `Vec<Option<DeclValue>>`（`PropertyId::slot()` 下标；`SLOT_COUNT=94` =
  ALL 87 变体 + 7 动画描述符；锁定测试 `slot_alignment` 保证 slot() ↔ ALL
  对齐，新增变体须同步）。物化路径 log-n 走查+逐项分配 → 下标写入+整块克隆：
  compute pass 16µs/节点 → 9.9µs/节点（−38%）。
- **父样式借用化**：`restyle_node` 父样式由深拷贝（`.cloned()`）改为借用——
  全量重样式 −15~20%。
- **性能预算门禁（C5）**：`examples/perf_gate.rs` 四场景 release 阈值断言——
  box_1k ≤5.0ms（实测 0.460）/ incr ≤2.0ms（0.489）/ text_50 ≤8.0ms（0.867）/
  scroll ≤3.0ms（0.086），120fps 帧预算 8.33ms 内、阈值与实测余量 ≥2.7×；
  debug 下断言自动跳过。CI 新增 `perf-gate` job（ubuntu release 运行），
  本地 `run.ps1` 新增 perf 段（`-NoPerf` 可跳过）。阈值推导、优化归因史与
  增量重样式设计记录于 **docs/PERFORMANCE.md**（新建）。
- **基准数字刷新（FEATURES.md ㉘㉚）**：滚动 200 行 0.079ms/帧（余量 105.4×，
  可承 ≈2.1 万行盒）；文本 50 叶稳态 0.734ms（单叶摊销 14.3µs，可承 ≈581 叶）。
- **MSRV 上调 1.85 → 1.90**：依赖地板漂移（ordered-float 5.5.0 需 1.90、
  smol_str 0.3.6 需 1.89、vello/parley/fontique 链需 1.88、wgpu-types 29.0.4
  需 1.87），按「MSRV 以依赖最高者为准」政策随行；msrv job 钉定
  `1.90.0` 验证（docs/DEPENDENCIES.md 记录完整地板链）。连带：MSRV 解锁
  clippy let-chains 建议（1.88 稳定），34 处嵌套 `if` 折叠。
- **CI 全绿修复**：本仓库 CI 自首次提交起从未绿过（无人核查）——三病根修复：
  ubuntu 补 `pkg-config/libfontconfig1-dev`（fontique 系统字体发现 build
  script 依赖）；windows 腿设 `STYLE_ENGINE_NO_GPU_PROBE=1` 短路 vello GPU
  探针（CI 虚拟适配器 wgpu 设备创建段错误 0xc0000005，进程内不可捕获；本地与
  macOS 真适配器必跑，FEATURES.md ㉕ 补 CI 短路语义）；`cargo hack
  --feature-powerset` 去 `--locked`（`--no-dev-deps` 运行期剥离 dev-deps 改变
  依赖图与 `--locked` 冲突）。

### 阶段4 — 依赖治理（C4）

- **公有词汇表 re-export**：lib.rs 根新增 `pub use peniko::color::{AlphaColor, Srgb};`
  ——公有面唯一第三方类型（PaintOp/ColorValue 色值签名）现在由本 crate 直出，
  宿主消费 DisplayList/ComputedStyle **无需自行依赖 peniko**（版本锚定，杜绝
  双份 peniko）；`pub use smallvec;` 同语义此前已有。
- **依赖策略文档化（C4）**：lib.rs 新增「依赖策略（C4）」段（词汇表公有 / 重依赖
  隔离 / 基础设施不进核心）；`docs/DEPENDENCIES.md` 新增「阶段4 依赖治理审计」
  节并修正两处滞后声明（kurbo 不出现在公有面；vello sink 公有 API 不暴露
  wgpu 类型，无需 `pub use wgpu`）。
- **feature 门禁审计**：`--no-default-features` / `+layout` / `+text` /
  `--all-features` 四组合编译零警告；修复 layout-only 死代码盲区
  （`abs_avail_width` 补 `#[cfg(feature = "text")]`）。
- **供应链门禁 cargo-deny**：新增 `deny.toml`（licenses 硬门 + RustSec
  advisories 漏洞硬失败 + yanked=warn + 多版本 bans=warn）与 CI `cargo deny
  check` 步骤（ubuntu；本地 run.ps1 未装则跳过）。处置两项：`RUSTSEC-2026-0192`
  （ttf-parser unmaintained，仅 Linux demo 传递链，ignore + 重估条件）、
  yoke-derive 0.8.3 yank（cargo update 升 0.8.4）；licenses allow 含 CC0-1.0
  （hexf-parse，color 链 hex 解析）。重复版本审计（16 组）记录于 DEPENDENCIES.md。

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
- **#[must_use] 提示**（API 冻结·下）：`Frame`/`LayoutEntry`/`ComputedStyle`/
  `DisplayList` 类型级 + `StyleEngine::computed_style` 访问器——结果被丢弃即编译警告。
- **线程承诺静态断言**：`StyleEngine`/`Frame`/`ComputedStyle`/`DisplayList`/
  `ParseReport`/`ContractError` 均 `Send + Sync`（测试锁定）。落地面两处：
  taffy 0.14 `CompactLength`（nan-boxing `*const ()`）非 `Send`/`Sync`（上游
  未提供 impl）——多列断口 margin 备份改显式三态镜像（`MarginTopBackup`），
  `TaffyTree` 经 `SendSyncTaffy` 包装（引擎不构造 taffy calc 值，该指针恒为
  位模式载荷，SAFETY 注释论证）；相应地 crate 级 `forbid(unsafe_code)` 放宽为
  `deny` + 唯一豁免点（`SendSyncTaffy` 两个 unsafe impl，精确 `#[allow]`），
  其余任何 unsafe 仍直接编译失败。
- **`ParseWarning` 增 `severity: ParseSeverity`**（`#[non_exhaustive]` 枚举：
  `Dropped`=无效内容按规范丢弃 / `Skipped`=认识但跳过）；`ParseReport::push`
  签名加 severity 参数（crate 内私有面）；全部 7 处上报点定级完成。
- **诊断 API（C6）**：`StyleEngine::computed_style(key) -> Option<&ComputedStyle>`
  ——宿主可在帧外检查任意节点的级联求解结果。
- **可观测性**：帧级 `tracing` span（`target: style_engine::engine`，`pass` 字段
  记录收敛轮次）；`ParseReport::push` 同步发 `warn!`（`target: style_engine::css`，
  含 line/column/severity）。tracing 已是 workspace 依赖，零新增。
- **`ContractError` source 链契约**：全 5 变体均为根因，`Error::source()` 恒
  `None`（测试锁定）。
- **crate 级 rustdoc**：`lib.rs` 新增「快速上手」doc-test（解析→插树→帧→命中查询
  全链路）与「API 冻结策略（C3）」段。
- **`DisplayList` serde 决策**：v1 不提供序列化实现（不引入 serde 依赖；宿主可
  基于 `PaintOp` 中立枚举自写转换）；重估条件=跨进程合成/录制回放需求，届时优先
  独立 feature gate。记录于 `lib.rs` 模块头与 `docs/FEATURES.md` T2 第 17 项。

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

[Unreleased]: https://github.com/chh-itt/style-engine/compare/d955894...HEAD
[0.1.0]: https://github.com/chh-itt/style-engine
