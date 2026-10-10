# CSS 特性注册表（子集边界的权威定义）

本文件是"超出子集"的唯一权威来源：ADR-0004 的容错边界、ADR-0003 的零容忍作用域、v0.1 路标三者共同引用它。用例 manifest 声明的特性必须能对应到这里的条目；注册表变更必须同步更新对应用例的 in-subset 声明。

## T0 — v0.1（conformance 零容忍覆盖）

- 语法层：真实 CSS 文本；选择器子集 = type / #id / class（class 属性为空格分隔 token，引擎归一）/ universal / 伪类（:hover :active :focus :disabled :checked）/ 结构伪类 :nth-child 系与 :first/:last/:only-child（selectors 兄弟遍历，天然支持）/ :is / :not / 属性选择器六操作符（第五批⑮：宿主经 StyleNode.attrs 供值——[attr] 存在即命中、[attr=v] 值匹配大小写敏感、~=/^=/$=/*= 全数支持，BTreeMap 确定序）/ 后代 / 子代 / 分组；简写展开（margin padding border border-top border-right border-bottom border-left background color font——四向 border 简写=P4 批补齐：<'border-width'>||<'border-style'>||<'border-color'> 任意序，缺省 medium/None/currentcolor）；CSS 宽关键字（inherit/initial/unset/revert）；custom properties + var()
- @media 子集：width/height、prefers-color-scheme、prefers-reduced-motion（条件值由 Environment 提供）；交互媒体特性 pointer/hover/any-pointer/any-hover（第五批⑱：MediaFeature 四变体 + MediaEnv 四字段（pointer: PointerKind{None,Coarse,Fine}、hover、any_pointer、any_hover——宿主每帧推送），any- 变体面向多输入设备独立评估）
- 值与颜色：px/em/rem/%/vw/vh；**rem 基准 = 文档根计算字号**（阶段7 P0 修复：MediaEnv 新增 rem 字段、map_env 按根样式接线，engine/computed/layout/paint 全链路 ResolveCtx 与 calc 延迟结算 DeferredRaw 均改用——修复前全链路恒 16px、:root font-size ≠ 16px 时全部 rem 长度错误；CSS Values 两特殊语义落实：根元素 font-size 内 rem 按初始值 16 解析、根元素其余属性 rem 用解析后的新根字号；多根引擎下 rem 仅由文档根定义，与 ADR-0010 一致；@media/@container 条件内长度仍按初始 16 解析——文档无关解析期换算，规范语义在案）；calc() 基础四则（第五批⑤评估+二期①落地：值解析/嵌套/括号全解，paint/text 消费端 resolve_px 直通无偏差；布局端——**2026-09 上游复评修正**：taffy 0.14 实有公开 `resolve_calc_value` 与官方自定义树示例（examples/custom_tree_owned_unsafe.rs），旧结论「calc 指针传输层被 pub(crate) 阻断（TaffyView 不可达）」不成立（TaffyView 非 taffy 公共类型，旧评估引用有误）——迁移路径在案，维持「结算式直通」因已落地且 perf 实测达标，重估记 B 级清单：映射期捕获含百分比 calc（layout.rs DeferredRaw，thread_local 收集；px 部分照旧折叠供首遍），每帧首遍布局后 settle_calc（engine.rs）以父内容盒（size−border−padding）为基准解析百分比回写固定值并重算，上限 3 遍（百分比基准恒为祖先派生 DAG，逐遍稳定一层，3 层内与浏览器单遍语义一致，更深链路记偏差）；v1 结算槽位 width/height（flex-basis/min-max/margin-padding 维持 0 折算，记录偏差）→ 三期③扩至 15 槽位全落地：width/height/flex-basis/min/max-width/height/margin 四侧/padding 四侧/column-gap/row-gap（CalcAxis 槽位枚举+basis_axis 定基：Height 族与 RowGap 以高为基，flex-basis 动态基=父 flex_direction 主轴，margin/padding 全族按 CSS 2.1 §8.3/§8.4 恒以包含块宽度为基；settle_calc 逐槽位 style.write 回写，负值 padding/gap 钳 0）；顺带修复 min-width/min-height 全体失效 pre-existing bug（解析入 LenAuto 族而 map_style 读 cs.len 恒 None——max 一直走 len_auto 正常，sizing-constraints 因 max 冗余掩盖而假绿；已改 len_auto，min auto→taffy Auto 保 flex automatic minimum size 语义）；conformance calc-slots（8 盒全整数：margin-left/padding 四侧/min 抬升/max 压制/flex-basis 行向/margin-top 宽基验收/gap）；锁定测试 calc_slot_* 六件（flex-basis 双轴/min-max 抬升压制/margin 百分比宽基/padding 结算钳负/gap/两级链跨槽位收敛）；conformance calc-width xfail 转正（Chromium 400px 一致——T0 零容忍首个消除项）；锁定测试 calc_layout_resolution（85=50%×150+10）+ calc_percent_chain_settles（两级链 110→65 两遍收敛））；color crate 全谱（hex/rgb/hsl/oklch/color()/light-dark()）
- 色彩空间合成（第五批㉕双预测探针，用户开工前待办闭环）：wgpu 离屏回读探针 blend_space_srgb_matches_css_default（style-engine-vello，红底+50% 白罩全覆盖单像素）——预测两档：sRGB 混合（Chromium/CSS 默认合成）G=127.5→127/128 vs 线性混合 G=encode(0.5)≈187.5→187/188；实测 G=128 落 sRGB 档=vello 0.10 render_to_texture 在 Rgba8Unorm 目标上以 sRGB 编码值直接合成，与 Chromium/CSS 默认一致、半透明叠加无色彩空间分歧（ADR-0002 旧注「vello 按线性混合」据此修正）；约束记录：结论限定 Rgba8Unorm 目标路径，宽色域/HDR 目标若引入线性合成需重测；探针附 vello 管线纹理 usage 要求（STORAGE_BINDING|TEXTURE_BINDING|COPY_SRC——fine 阶段存储图像直写非光栅化 attachment）与无适配器环境跳过语义；CI 短路语义（阶段5）：windows runner 虚拟适配器（WARP 类）枚举可得但 wgpu 设备创建段错误 0xc0000005（进程内不可捕获）——ci.yml windows 腿设 STYLE_ENGINE_NO_GPU_PROBE=1，探针读到即跳过（本地与 macOS 真适配器必跑）
- 滚动性能基准（第五批㉘）：`examples/scroll_bench.rs`（release 运行，debug 数字无意义）——滚动重帧路径=set_scroll_offset+frame() 全量推进（增量重样式→动画→taffy 布局→DisplayList 重建），测量口径=200 行盒滚动容器（overflow-y: scroll）全量程 60 tick 逐帧计时，纯盒树排除 shaping（㉚ 单独评估）；实测（Windows 本机 release，阶段5 槽位化后）：平均 0.079ms/帧、p95 0.093、最差 0.130——120fps 预算（8.33ms）满足余量 105.4×；线性外推全量重帧预算内可承约 2.1 万行盒规模；滚动场景另入 CI 门禁（perf_gate scroll，阈值 3.0ms，见 docs/PERFORMANCE.md）
- 增量重样式（第五批㉙ 决策=维持全量重帧 → 阶段5 C5 升级落地，依据 ㉘/perf_gate 实测）：**重样式增量、布局与绘制仍全量**——set_declarations 不再整树 dirty_style 而是登记脏根（style_dirty_roots），frame() 择路：全量脏位/结构变更/有 @container 规则（容器快照可能被子树新样式反向影响）→ 全量 restyle()，否则 restyle_subtrees(roots) 仅重算「自身+后代」子树（正确性域：节点样式求值只依赖自身/祖先树数据、继承父样式、祖先容器快照三者；兄弟声明互不影响，:nth-child 按树位）；子树含表格（settle_tables 登记）或多列节点保守退全量（两阶段结算登记一致性）；RestyleGuard（pass 计数 + SecondaryMap done 表）对互为祖先/后代的脏根去重；text 特性下子树逐节点清 min_measures（行高探针缓存失效域与样式失效域同界）；实测（perf_gate incr 场景，1000 盒树逐 tick 轮换叶声明）：27.684ms → 0.489ms（56×），门禁阈值 2.0ms（docs/PERFORMANCE.md）；布局（taffy 全树）与 DisplayList 重建保持每帧全量——实测余量（box_1k 均值 0.460ms/阈值 5.0ms）远超收益/风险比，重估触发条件不变：①树规模实测帧均破预算 ②文本密集树 shaping 占比破预算（㉚ 供数）③动画高频改布局属性——命中即重开增量布局/脏区绘制子票
- shaping 性能基准与探针缓存（第五批㉚）：`examples/text_bench.rs`（release+features text 运行）——50 文本叶全管线帧实测（Windows 本机，阶段5 槽位化后）：稳态平均 0.734ms（p95 1.048/最差 1.850）、空盒基线 0.017ms、单叶摊销 14.3µs、首帧 2.937ms（含字体表首次加载+shaping 预热+1 次 normal 探针）；120fps 预算内稳态可承 ≈581 文本叶；文本场景另入 CI 门禁（perf_gate text_50，阈值 8.0ms，见 docs/PERFORMANCE.md）；normal 行高探针缓存——(主族+字号+字重+italic) 键缓存 ㉔ 探针值，同键组合免探针遍（两遍法退单遍），add_font 清空（字体集变更可改变族选择）；fontique 0.11 无公开度量 API（FontInfo 无 metrics 字段），免探针直读需 skrifa 表解析——升级路径记录 DEPENDENCIES.md（度量查询缺口节）
- 第二 Sink（第五批㉛，契约验证）：`crates/style-engine-soft`——DisplayList 的纯软件绘制后端（零 GPU、零运行时第三方依赖，纯标准库光栅化；测试侧 peniko 仅用于构造输入色值），验证 sink 无关性这一核心架构承诺；合成语义与 ㉕ 探针对齐（sRGB 编码值直接 src-over=vello/Chromium 一致；渐变停点 sRGB 插值=CSS 默认）；v0 覆盖矩阵——FillRect（椭圆圆角逐像素覆盖）/Gradient（linear 角度+radial RadialGeom 椭圆；停点色仅 Absolute、位置 Px/Percent、None 自动均布）/Shadow（平移半透明矩形近似=与 vello MVP 同偏差，inset 与盒求交）/Image（最近邻）/Border（直边带，Dashed/Dotted 近似实线）/PushClip·PopClip（矩形+圆角裁剪栈）/PushOpacity·PopOpacity（有界组 alpha 快照回混，ADR-0008）/PushScroll·PopScroll（平移折叠嵌套累加）；v0 跳过——Transform 层与 Text（需 shaping=与 vello MVP 同注），未识别 op 忽略（非穷举演进契约）；6 项锁定测试含 ㉕ 交叉验证（红底+50% 白罩 → G=128 与 vello 实测同值——双 sink 合成一致性实证）与线性渐变逐像素精确值（t=1/16→16、t=15/16→239）
- 级联：三 Origin 双键排序（normal 升序 Default < Stylesheet < Inline；important 升序 Stylesheet < Inline < Default；含 !important 交织用例，以浏览器为基准验证）
- 布局：taffy 0.14 可用面 = flex / grid / block、absolute / relative 定位、min/max/aspect-ratio、gap、overflow 裁剪
- 表格布局（二期②）：display:table/table-row/table-cell v1——表映射块容器、行映射单行 taffy Grid（列模板由结算期回写）、单元格映射 Grid 项拉伸至列宽；两阶段列宽结算（engine.rs settle_tables，settle_calc 同模式）：首遍布局得表内容宽后按首行单元格声明宽（定宽 px/百分比/auto）+ 单元格水平内缩（padding+border）经 layout::table_column_template 计算像素列模板，回写各行 grid_template_columns 并重排，上限 2 遍（嵌套表外层先行），全等缓存免重排（稳态帧零额外 pass）；浏览器语义对齐（Chromium 153.0.8010.12 实测，conformance table-basic 9 盒零超差）：Length 声明=content-box（列贡献=px+内缩）、百分比声明=border-box（列=p×表内容宽，不追加内缩——单元格百分比宽的已知非对称行为）、auto=均分剩余（三期前 v1；**P7 收敛：auto 列剩余宽按内容 max-content 比例分配（Chromium 同语义），`table_column_template` 加尾参 `content_max: &[f32]`（T-签名破坏，等价旧行为传 `&[]`），引擎侧 content_max_width 子树文本叶测量兜底+根格水平内缩，嵌套盒结构组合近似单叶最大（B 级）；全零回退等分，空列 0 宽；单 auto 列时与规范一致**）；三期④a：colspan 属性 + 行组 + caption 落地——行发现穿透 table-row-group（组=纵向透明块包装，组内行与表直系行混排堆叠）；caption（table-caption→taffy Block）为普通块盒置于行区上方、宽=表内容宽、不参与列发现；单元格图（CSS 2.1 §17.2.11.1 简化版）逐行游标分配显式列位（colspan 取 StyleNode.attrs，钳 1..=1000；列数=各行跨数和最大值），单元格写 grid_column=Line(start)·Span(n)+grid_row 钉第 1 行（洞不吸附后续单元格），列模板全等但单元格图变更时仍回写列位；span-n 声明宽均分给跨内未声明列（Length=px+内缩/Percent=表宽基/auto 跳过；Chromium 按 min/max-content 分配，简单表一致）；锁定测试新增 table_colspan_expands_column_map/table_row_group_stacks_rows/table_caption_sits_above_rows；conformance 新增 table-span 用例：真实 `<table>` 标记（caption+tbody 行组+colspan=3 跨列），Chromium 153 实测 12 盒零超差，Numeric+Pixel 双通道；fixture 解析器扩展支持表格标记（table/caption/thead/tbody/tfoot/tr/td/th），colspan/rowspan 属性原样携带入 StyleNode.attrs，dumper 注入 UA 中和样式（border-spacing/td padding 归零，插于 case.css 之前保持级联）；三期④c rowspan：跨行单元格留在宿主行网格（列位照旧），高度覆写=宿主行+被跨行 definite 高度和→向下溢出覆盖（occupancy 集合让后续行游标跳过被占列，Chromium 同语义——跨行不挤走后行单元格）；行高 definite 时同时钉死行轨道（防跨行高度作为 auto 轨道 min-content 贡献撑大同轨单元格）；conformance 新增 table-rowspan 用例（colspan=3+rowspan=2 交叉，Chromium 13 盒零超差）；三期④d 匿名单元格（CSS 2.1 §17.2.1 第 3 条）：行内非单元格元素按单元格入图（列位分配/宽度拉伸与 td 一致；display:none 与 absolute 出流子件不入图）——HTML 解析器 foster-parenting 使行内非单元格子件对标记不可达（HTML5 解析器只允许 tr>td/th，杂件被移出表格），此特性服务于 DOM API 建树场景（程序化构造树），由锁定测试覆盖、无 Chromium golden 可制；v1 偏差（B 级）：被跨行高度 auto 时不分摊跨行内容（Chromium 会增长行）、rowspan 单元不参与列 auto 尺寸、非首行 colspan 不进列声明、table/row-group 直系杂件不做匿名行合成（标记不可达，DOM API 树按块流堆叠全宽处理）；v1 边界：良构标记（table>row>cell，行组一层）、匿名盒生成未做（table 直系杂件跳过，三期④d 修补）；锁定测试 table_column_template 4 项（含 Chromium 语义混合列 166/300/134）+ table_two_stage_column_settlement（两遍收敛稳态免重排）
- Grid 重复轨道（阶段2①）：repeat() 首参数除固定次数（既有）外接受 auto-fill / auto-fit（TrackSize::RepeatAuto(bool fit, Vec<TrackSize>)，fit=true 即 auto-fit）——计数由布局期按可用空间定，直通 taffy 原生 RepetitionCount::AutoFill/AutoFit（layout.rs grid_component 映射 `style_helpers::repeat("auto-fill"|"auto-fit", …)`）；语义与 Chromium 153 实测一致：auto-fill 计数=可用空间容纳的最大重复数（空轨保留）、auto-fit 折叠空轨后剩余 fr 轨重分自由空间；解析期拒绝嵌套 auto-repeat（repeat 列表内出现 RepeatAuto → 整条丢弃，CSS 规范禁嵌套、Chromium 同判；固定次数嵌套沿用既有解析容错、布局期防御归 auto）；conformance 新增 grid-autofill 用例（双容器一例双验证：auto-fill 400 宽 4 轨各 100 五项回绕第二行 vs auto-fit 2 项折叠 2 空轨 fr 均分各 200，Chromium 153 十盒零超差 Numeric+Pixel 双通道）；锁定测试 grid_repeat_auto_fill_and_fit（解析：minmax/混合混排/嵌套拒绝）+ grid_autofill_repeats_tracks_to_fit/grid_autofit_collapses_empty_tracks_fr_expands（布局，后者同时锁定 auto-fill 对照轨宽与 definite width 子件不拉伸语义）；v1 边界：repeat 的 line_names（网格线命名）未接
- var() 简写（阶段2②）：`margin: var(--m) 20px` 等含 var() 的简写不再整条拒绝——解析期无法做 TRBL 槽位分配（var() 分量数未知），按 CSS 规范推迟到计算值期：解析期对简写长手全集落 N 条挂起声明（DeclSource::PendingShorthand{shorthand, tokens}，每长手一条、同 tokens、important 沿简写整体），级联 winners 按 PropertyId 逐槽竞争天然成立（同块后写长手/更高优先级的长手覆盖对应槽，其余槽仍来自展开——css-cascade 挂起代换值语义）；计算值期 substitute() 代换后 expand_shorthand 展开并取本长手，代换失败（未定义 custom property 且无 fallback）/展开文法失败/长手不在展开集（animation、flex-flow 为子集展开）→ 该长手 IACVT（继承属性取父值否则初始值），与无 var() 声明的 IACVT 语义一致；shorthand_longhands 全集表与 expand_shorthand 各臂由锁测试 shorthand_longhands_match_expand 防漂移（15 简写×代表值展开 ⊆ 全集）；顺带修复捕获尾部缺陷：capture_tokens 在顶层 `!` 处 break 时已消费 `!`，原 finish 处理用 parse_important 必然失败——custom property / var() 长手 / 纯简写带 `!important` 三条路径原先整条丢弃（`--m: 1px !important` 连 custom property 都丢失），现统一 finish_after_capture（post_state 只可能剩 ident important）；锁定测试 var_shorthand_expands_at_computed_time（槽位竞争+两值 TRBL+fallback 代换）/var_shorthand_invalid_substitution_iacvt（代换失败四 margin 归初始 0）/var_shorthand_expands_to_pending_longhands（important 传播+tokens 保留）/shorthand_longhands_match_expand/important_tail_variants_parse（三条 !important 回归锁）+ var_shorthand_padding_reaches_layout（端到端布局）；v1 边界：简写内 var() 与 calc() 混用依赖代换后重解析（既有能力），简写不含 var() 的路径解析期直接展开不变
- 容器查询（阶段2③）：@container 尺寸查询 MVP——解析：@container [name]? { rules }（AtPrelude::Container → 子 StylesheetParser 递归，嵌套 @container/@media 条件扁平 AND；条件=逗号分隔 OR 段、段内特性并置/and 均 AND；特性=(width|inline-size|height|block-size)[: op] len、(orientation:portrait|landscape)、值在前反序形式（300px <= width ≡ width >= 300px，算子翻面）、旧形 min-*/max-* 前缀（→ ≥/≤ 语义改写）；坏条件 report 告警+整规则跳过，与 @media 同形）；数据结构 ContainerFeature{Size{axis,op,value}|Orientation(bool)}/ContainerCondition{name,features} 自带 eval，Rule.container=Option<Vec<_>>（None=无条件）、Stylesheet.has_container_rules 单源派生；级联：ContainerCtx{names,ctype,size}（cascade.rs）——match_rules/cascade_declarations 增 container_ctx 参，规则条件逐段求值不满足即跳过；有名段沿容器栈 rev() 取最近同名容器、无名段取栈顶最近可用容器，无可用容器或 InlineSize 容器遇 Block 轴/orientation 特性（尺寸未知语义）→ 不命中；布局收敛：container-type（normal|size|inline-size）+container-name（none | 空格分隔 custom-ident 列表）长手 + container 简写（<'container-name'> [/ <'container-type'>]?，真简写无条件重置两长手；裸类型关键字 size/inline-size 歧义归 type——CSSWG 裁定）；engine.rs record_container_sizes 帧末记录各容器 content-box 尺寸（taffy size−border−padding），restyle DFS 维护容器栈（自身样式按祖先求值后才入栈——查询容器不含自身），frame 收敛环（has_container_rules 时上限 3 pass：restyle→布局→record，快照变更→dirty_style 重跑，稳态零额外 pass），set_stylesheet 清空快照；锁定测试：container_rule_parse_shapes/container_orientation_and_nested_media/container_invalid_condition_skips_rule/container_longhands_parse/container_shorthand_name_and_type + shorthand_longhands_match_expand（16 简写防漂移）/container_rule_matches_named_and_unnamed/container_inline_size_blocks_block_axis_features/container_no_available_container_no_match/container_query_converges_within_first_frame/container_query_refits_on_viewport_resize/container_inline_size_gates_block_axis_features/container_named_lookup_skips_unnamed/container_snapshot_subtracts_padding；conformance 新增 container-query 用例（inline-size 容器 50% 宽=200px 内容盒，min-width:150px 命中 kid 30×30 红→60×60 蓝，Chromium 153 四盒零超差 Numeric+Pixel 双通道）；v1 边界（B 级偏差）：container-type 无强制 size containment（内容可反向影响容器尺寸，收敛环 3 pass 上限近似规范单帧语义；Chromium containment 强制）、cqh/cqw 容器单位未做、style 查询未做（T2）
- 多列布局（二期③）：column-count/column-width（+columns 简写，任一顺序至少一项，未指定长手重置 auto）v1 平衡多列——应用层结算（taffy 无列算法，保持框架中立）：容器 map_style 映射 taffy Flex Row（multicol_requested 单源判定，引擎登记与映射共用），engine.rs settle_columns 布局期结算（settle_calc/settle_tables 同模式，frame 内两次挂点——文本重排后二次平衡）：n=显式 count（Some(1)→回退单列）或 width 模式 n=max(1,⌊(cw+gap)/(w+gap)⌋)、colw=(cw−gap·(n−1))/n；n 个幻影列叶（Block、定宽、无样式→Frame 不报告）经 set_children 跨父重挂真实子节点，collect/T5c-2 按幻影补偏移与包含块；平衡分配=子节点 margin-box 单元高的**顺序装箱二分**（三期⑤a 重写贪心）：fit(h)=列非空放不下换列、末列溢出失败、空列承接不可断高块、**列首块 margin-top 按 0 计量（断口语义 css-multicol §7）**；fit 单调 → 浮点二分 48 次收窄最小可行列高、终值重装箱定分配——[80,80,80,10] 双列得 [80,80|80,10] max160（贪心 170）；断口截断落地=MulticolState.truncated 备份 taffy margin-top 原值（taffy 0.14 LengthPercentageAuto 无公开字段，零值以 TaffyZero::ZERO 比对）、每帧无条件重算 diff（文本重排 pass 以样式重写 margin → 二次结算需重截；HashMap collect 后写覆盖缺陷已修——列首件必须 entry().or_insert 保树序第一）、回退单列/退出多列恢复全部、restyle 全清（map_style 已复位）；gap 族解析改 parse_len（原 parse_corner_radius 产 Radius 无读者会静默归零，flex/grid gap 同步修复；normal 仍拒收——multicol 语义 normal=1em 由声明缺席表达）；break-inside（avoid|auto，三期⑤a）解析存储——v1 所有块一律不可断装箱（avoid 即默认语义），auto 可分裂未做（B 级）；浏览器对齐（Chromium 153.0.8010.12，conformance multicol-basic 7 盒零超差：列宽 290、列位 0/310、每列 3 件、容器高 90；新增 multicol-balance 用例 10 盒零超差——双 multicol 叠放验二分平衡 160/110 与断口 margin-top 截断，子件 break-inside:avoid 对齐 Chromium 断行模型：无 avoid 时空块会被 Chromium 跨列拆碎平衡高 125≠160）；锁定测试 multicol_balance_two_columns/width_mode_four_columns/revert_single_column/balance_uneven + 三期⑤a multicol_bisection_balances_uneven/multicol_break_margin_top_truncated + columns_shorthand_two_longhands；三期⑤b column-span:all——spanner 切断列流分段独立平衡（engine.rs settle_columns：is_spanner=styles ColumnSpan(Some(true))，plans 段切分，每段独立 fit/二分；spanner 模式容器改 Flex Column+gap 0（每帧重断言——restyle 会以 map_style 重写回 Row+gap），每段一行包装（Flex Row width percent(1) 装 n 幻影列，行 gap=length 列 gap），spanner 为容器直系全宽块；行数/幻影数不符重建、colw 变仅回写幻影宽、容器孩子序签名 seq_sig 变仅重排（spanner 换位复用行）；MulticolState 增 rows/span_mode/seq_sig/assignment 升格 (段,列) 二元组）；截断域修正=仅段内列首（col>0）截 margin-top，段首不截——spanner 边界是强制断行，css-break-3 强断边距保留（Chromium 153 实证 b2 y=360 含 mt20，非截断的 340）；collect 补偿升级=幻影链上溯（child→phantom→row→container 累加 location，沿 taffy_parent 循环至样式节点、深度 16 限），n≤1 回退重挂改树序全部真实子件（含 spanner）；conformance 新增 multicol-span 用例（双 multicol：spanner 前后各 [80,80] 段行并排 + mc2 探针实证段首 margin 保留，11 盒零超差 Numeric+Pixel 双通道）；锁定测试 multicol_span_all_splits_flow/span_all_two_segments_balance/span_segment_start_margin_kept；三期⑤c column-rule（width||style||color 简写+三长手）——解析侧 PropertyId×3：ColumnRuleWidth（thin/medium/thick 物化 1/3/5px、无 none 关键字——none 非法列规宽整条丢弃，Chromium 同语义）、ColumnRuleStyle（复用 BorderStyle 枚举）、ColumnRuleColor（复用 DeclValue::Color）；简写 || 三分量任一顺序至少一项、未指定长手重置初始（medium 3px/style none/currentcolor）；绘制=装饰非盒——engine.rs settle_column_rules 帧末 pass（collect 后、DisplayList 前，layout_by_node 最新鲜）产 ColumnRuleSeg（相对容器 border-box 原点：每行每相邻列对 gap 居中，宽=声明宽 max(0) 缺省 medium 3px、高=列高、色经 resolve_color CurrentColor→元素 color；span 模式逐段行 rel=行+幻影 location、行模式容器直系 rel=幻影 location；style none/hidden 或零宽不画），paint_node 边框后子件前 FillRect 直出（radius 全零）；v1 仅 Solid 实绘，花式（double/dotted/dashed/groove/ridge/inset/outset）解析接受按 Solid 近似=B 级偏差；conformance 新增 multicol-rule 用例（4px 红规 gap20 居中条带 x198..202 y0..160——列规非 DOM 盒，numeric 通道两侧均只含 6 内容盒，条带几何由 Pixel 通道把关，Chromium 153 零超差 Numeric+Pixel 双通道）；锁定测试 multicol_rule_row_mode_gap_centered/multicol_rule_span_mode_per_segment/multicol_rule_style_none_or_zero_width_no_paint + column_rule_shorthand_and_longhands + column_rule_strips_paint_relative_to_container_origin；v1 边界：column-rule-color 不继承（currentcolor 逐元素解析，色差场景才可见）；v1 边界：跨列分裂内容未做（break-inside:avoid 子件与 Chromium 平衡语义一致，auto 块有偏差）、column-span:all 之外的 span 值（如整数列跨）未做、absolute 子件包含块仍为容器
- absolute 包含块跳走（三期②）：taffy 直父锚定的结构性修正——absolute 子件的包含块按 CSS 2.1 §10.1 取最近 positioned（relative/absolute）或 transformed（has_transform；动画可翻转 has_transform，故结算挂点在 apply_animations 之后、compute #1 之前）祖先，无则初始包含块。engine.rs settle_absolute_anchors 帧内结构修正 pass：DFS 求 abs_cb（记录用严格祖先候选——节点自身 positioned 不得自选为 cb，首版自指缺陷）+abs_in 按 cb 分桶，逐节点 diff 期望子表后 taffy set_children 跨父重挂（move 语义顺带归位上帧残留；快速路径=无 absolute 且上帧未重挂则零成本跳过）；taffy_parent 同步维护；collect 对 abs_cb 命中节点改取 cb 视口坐标+l.location（ICB 哨兵→视口原点；style DFS 先序保证 cb 布局先就绪），并在幻影 effp 补偿前短路防双重偏移；v1 契约豁免 table/multicol 子树（absolute 子件包含块仍为容器，见多列条）；auto inset 静态位置=cb 流内近似（显式 inset 全正确，B 级）；fixed 不处理（T2）。Chromium 153 对齐：conformance 新增 absolute-anchor-jump（transformed mid 与叶间夹 static wrap，叶落 (60,20) 而非直父锚定的 (85,20)——x 差 25px 属 Class 3/4 零容忍拦截域），Numeric 合计 19 用例、Pixel 7 用例（该用例整数盒纯色 diff 0），零 xfail【**最后记录点**：此后批次（三期③④⑤）新增用例未再更新通道合计，现值以 CI 为准——全文点名清单见下条补遗】；引擎锁定测试×4（跨 static 包装跳走 / ICB 跳全 static 祖先 / 嵌套 absolute 自为后代 cb / 样式表替换后重挂归位）。随用例记录 taffy 0.14 塌陷偏差：末子 margin-bottom 与父 margin-bottom 塌陷时父盒被下移（塌陷量落在父上方而非父底缘之外，违 CSS 2.1 §8.3.1）——用例规避、偏差在案（B 级，见布局映射条）。
- conformance 通道合计补遗（「最后记录点」= 上条 absolute-anchor-jump 时的 Numeric 19 + Pixel 7；其后三期③④⑤新增用例未更新合计，现值以 CI 为准）——全文点名用例：**最后记录点时点** Numeric 19 = block-flow-basic / box-model / box-model-borderbox / absolute-offset / cjk-kinsoku / flex-row-gap / grid-columns / relative-offset / sizing-constraints / text-wrap-latin / transform-cb / selector-structural / calc-width（原 xfail，二期①转正）/ container-query / grid-autofill / multicol-basic / keyframes-layout / bidi-mixed / absolute-anchor-jump；Pixel 7 = calc-width / multicol-basic / table-pixel / transform-pixel / text-pixel / table-basic / absolute-anchor-jump。**其后新增（各条目自述，合计未随之更新）**：calc-slots（三期③）、table-span / table-rowspan（三期④）、multicol-balance（三期⑤a）、multicol-span（三期⑤b）、multicol-rule（三期⑤c）——其中 table-span / multicol-span / multicol-rule 自述 Numeric+Pixel 双通道，其余未自述通道归属（以 conformance manifest 为准）。
- 绘制：背景全集（F3b：多层列表 cycling、repeat 平铺、position/size/origin/clip、attachment、background 简写）、clip-path 裁剪形状（F3c：inset/circle/ellipse/polygon + geometry-box 参考盒，PushClipPath 多边形裁剪）、border-image 九宫格（F3d：slice/width/outset/repeat，图片与渐变源，替代 Border op）、背景色、线性/径向渐变（sink 内 stop 加密对齐 sRGB 插值；F3d 起携带绝对几何 LinearGeom）、圆角、边框、阴影、opacity、图片（F3d 起携带采样窗口）、圆角矩形 clip
- 文本：单 style run、断行、字体注册与基础 fallback（测量内置，见 ADR-0006）；字体深化（F3d：font-stretch/word-spacing/font-feature-settings/font-variation-settings/font-variant-caps 全通道——测量与三 sink 绘制同源；small-caps 合成）
- 状态与动画：StateFlags、transition-* 全量（P1-5 批，ADR-0032——四长手+简写+reconciliation+采样挂点，可插值属性经 lerp_decl 全子集）
- 行为提示属性（A1，docs/BEHAVIOR-HINT-PROPS.md）：cursor（CSS UI 4 关键字子集 15 值：auto/none/default/pointer/text/wait/progress/help/not-allowed/move/grab/grabbing/crosshair/zoom-in/zoom-out；url() 自定义=T2 解析期拒绝）/ user-select（auto|none|text|all|contain；**不继承**——spec 明确，宿主实现选择时自行取值）/ pointer-events（auto|none）/ caret-color / accent-color（auto|color → Option<ColorValue>，None=auto）——宿主可读零绘制通道：解析入库+级联+继承全语义，不产生任何 PaintOp、不参与布局（锁定测试 behavior_hint_no_paint_ops 断言 ops/几何零变化）；消费端=宿主经 `ComputedStyle::value(PropertyId)`（全集物化含初始值——非继承属性在子级物化为初始 auto；slot 87-91，SLOT_COUNT 94→99，slot_alignment 自动覆盖）；锁定测试 crates/style-engine/tests/behavior_hints.rs 六件
- outline / outline-offset（A2，css-ui-4）：outline-width（<line-width> 复用 BorderWidth 值族）/ outline-style（none|auto|solid|dashed|dotted|double|groove|ridge|inset|outset；**auto=宿主 focus ring 语义位，绘制按 Solid 近似=B 级**；double/groove/ridge/inset/outset 亦按 Solid 近似——BorderStyle 仅 None/Hidden/Solid/Dashed/Dotted 变体）/ outline-color（<color>，初始 currentcolor）/ outline-offset（<length> 可负）；**outline 简写** = <'outline-width'> || <'outline-style'> || <'outline-color'>（任意序、未指定重置初始、**不重置 outline-offset**）；不继承、不占布局（零 taffy 映射）、**ink overflow 不入滚动量程**；绘制=复用 PaintOp::Border 外扩矩形承载（描边带=[border-box+offset, +offset+width]，radius 各分量 +d 圆心不变近似 v1 锁定），绘制序=边框后、列规/内容前；零 sink 改动（soft/vello 自动获得）；锁测试 crates/style-engine/tests/outline.rs 六件（几何锁 (-6,-6,112,112)@d=6）
- 多根引擎与 top-layer（A3，ADR-0010）：`insert(None)` 多次——首个=文档根（单根语义逐位不变），后续=overlay 根（视口原点锚定、fit-content、作者显式 absolute/fixed 定位生效、各为级联根、不被文档 overflow 裁剪）；`set_top_layer(key, on)` 进出弹窗层（幂等；文档根→`ContractError::NotOverlayRoot`）；**有效绘制序 = 文档根 → 非 top overlay（插入序）→ top 层（进层序）**——`sync_root_order` 每帧同步树/taffy 子序（稳态零操作），绘制按根序逐根追加（`paint::append_display_list`）；`StyleTree::is_root` 重定义（:root 匹配全部用户根；`is_super_root` 新增）；remove 语义：overlay 摘除不动文档、文档根移除=全清；锁测试 crates/style-engine/tests/multi_root.rs 六件
- position: fixed / sticky（A4）：Position 全值 {Static,Relative,Absolute,Fixed,Sticky}。fixed=视口锚定（包含块=transformed 祖先或 ICB，**positioned 祖先不算**；taffy 映射 Absolute+settle_absolute_anchors tcb 候选重挂；fixed 为 absolute 后代的 cb）；sticky=in-flow 布局（taffy Relative 且 inset 归 auto——top/right/bottom/left 为粘滞约束语义由宿主按滚动运行时施加，引擎经 scroll_offsets 绘制期平移；ADR-0006 边界）；锁测试 crates/style-engine/tests/position_fixed_sticky.rs 四件
- white-space 全值（A5，css-text-3）：{Normal,NoWrap,Pre,PreWrap,PreLine,BreakSpaces}。语义矩阵：normal=折叠+按宽换行/nowrap=折叠+不换行/pre=保留+不换行/pre-wrap=保留+按宽换行/pre-line=折叠空格但保留换行符+按宽换行/break-spaces=保留+按宽换行（**任意字符断行=v1 常规断点近似=B 级在案**）；T5c-2 重排 pass 换行判定=None|Normal|PreWrap|PreLine|BreakSpaces（白空格族按包含块内容宽自动重测）；锁测试 crates/style-engine/tests/white_space.rs 五件
- min()/max()/clamp()（A6，css-values-4）：CalcNode+Min/Max/Clamp（逗号参数折叠、clamp 恰三参、嵌套数学函数递归；clamp(MIN,VAL,MAX)=max(MIN,min(VAL,MAX))）；与 calc() 同链路（LengthPercentage::Calc 直通：has_percent 延迟结算、长度语境纯数字拒绝）；**map_env 修复**：map_style 解析环境以帧视口覆盖 media 视口（vw/vh 与 calc 视口单位=ICB=帧视口；@media 命中不受影响）；锁测试 crates/style-engine/tests/math_fns.rs 五件
- @media L4 range + 新特性（A7，css-conditional-4/mediaqueries-4）：`Range(RangeCond)` 范围语法（`(width>=400px)`/`(400px<=width<=800px)`/值在前形；轴 width/height/aspect-ratio/resolution；`<=``>=` 含界、`<``>` 开界、双比较 AND）；`Orientation`；aspect-ratio（`<ratio>`=数字或 a/b，含 min-/max- 旧形）；resolution（dpi→/96、dpcm→×2.54/96、dppx|x 直通，含 min-/max- 旧形）；布尔语境 `(hover)`/`(any-hover)`/`(pointer)` 与 `(not feature)`（可嵌套）；**MediaEnv+resolution:dppx（破坏性 0.x，默认 1.0）**；**评估基准统一**：级联 @media 命中与全部解析环境经 map_env（宽高类媒体特性按帧视口评估，宿主 set_environment 供配色/动效/指针/分辨率偏好）；锁测试 crates/style-engine/tests/media_l4.rs 七件
- 逻辑属性 + direction/unicode-bidi（A8，css-logical-1）：30 新槽位——margin/padding/inset ×{inline,block}×{start,end}（8）、border-{inline,block}-{start,end}×{width,style,color}（12）、border-{start,end}-{start,end}-radius（4）、direction/unicode-bidi；槽位重排=物理 0–95 / 逻辑 96–125 / 动画描述符 126–132（SLOT_COUNT 103→133，slot_alignment 锁定）。**级联期映射（规范正确）**：逻辑槽赢家与映射物理槽赢家按 (rank, specificity, order, decl_index) 序键定夺、晚者填物理槽（Candidate 新增 decl_index=同规则块声明序；物理/逻辑同池比先后），逻辑槽不外泄（写位）；ltr: inline-start=left/block-start=top，rtl 仅翻 inline 轴与 radius 四角（**纵向书写模式不支持=B 级在案**）。direction（ltr|rtl，继承）=映射基准：自身声明胜者→继承→ltr；unicode-bidi 六关键字（继承）=文本栈提示（`ComputedStyle::direction()/unicode_bidi()` 可读；引擎文本叶不做 bidi 重排——parley first-strong 现状，见 T2 direction 条=B 级）。简写：margin-inline/block、padding-inline/block、inset-inline/block（1–2 值 start end；值表穷尽校验，三值整条拒绝）、border-inline/block（`||` 任意序展开 6 长手，未指定分量重置初始）。全集物化修复：循环改按 `pid.slot()` 落槽（原以 ALL 下标充当槽位，ALL 序=槽位序时侥幸成立）。锁测试 crates/style-engine/tests/logical_props.rs 十件
- 容器查询与字体相对单位（A9，css-values-4/css-contain-3）：LengthPercentage/CalcUnit +七变体 cqw/cqh/cqi/cqb（容器查询单位；存储=小数同 vw/vh 惯例）与 ch/ex/ic（字体相对单位；存储=em 倍数同 em 惯例），解析双通道（parse_length_percentage Dimension 臂 + calc() Dimension 臂）。**求值=延迟结算管线复用（三期①）**：映射期 has_cq 判定落延迟队列（直变体经 lp_to_calc 归一 CalcNode；首遍以回落基折叠保 taffy 首遍有确定值），settle_calc 结算期沿 taffy 父链现查最近 container-type≠Normal 祖先的本帧布局内容盒为基（size 双轴、inline-size 块轴回落视口高），无容器祖先=small viewport（规范回落）。**ch/ex/ic=真字体度量**：css::fontprobe 零依赖最小 sfnt 探测（head unitsPerEm、cmap format4 gid、hhea+hmtx advance、loca+glyf 头 bbox yMax、name 表族名），add_font 注册即探测，restyle 期按 font-family 首具名族（大小写不敏感）补写 ComputedStyle 私有字段 font_metrics（`ComputedStyle::font_metrics()` 宿主可读）；未注册族=近似缺省 ch/ex=0.5em、ic=1em（B 级在案）；ic 缺字（cmap 无 U+6C34）=1.0em 契约。B 级在案：纵向书写模式不支持 → cqi=cqw、cqb=cqh（水平书写语义）。ResolveCtx+5 字段（cq_w/cq_h/ch/ex/ic_per_em）+`ResolveCtx::base()` 构造器收敛全部 25 处字面量（style-engine-vello 渐变 stop 解析处同）。锁测试 crates/style-engine/tests/relative_units.rs 九件（容器基值≠视口、cqh/cqi/cqb 轴向、calc 混排、无容器回落、ch/ex 真度量≠近似、ic 缺字）
- 级联起源栈与层序（B1，css-cascade-5）：`Origin` 五档阶梯
  Default→UserAgent→User→Author(Stylesheet|Inline 合并桶)（normal 轴），
  important 轴反转（Author→User→UserAgent→Default）；`@layer` 语句形
  `@layer a, b;` / 块形 / 嵌套 / 匿名层 / 点名前缀 `a.b` 全形态解析，
  先现序层树（u32 序数解析期定序），未分层 normal 胜一切分层、important
  轴反转（早层胜晚层、未分层 important 输给分层 important）；
  `revert`/`revert-layer` 赛后回滚（含 custom property 与内联声明）；
  `initial`/`inherit`/`unset` 宽关键字整值物化（var() 代换结果恰为宽
  关键字同语义；`flex: initial` 保持 flex 专属 `0 1 auto` 例外）；
  custom property 终值恰为宽关键字按 css-variables-1 §3 作用于自身
  （guaranteed-invalid，var() fallback 生效——55b5102，var-wide-keywords
  conformance 对拍 Chromium golden 后转正）；
  引擎 `set_user_stylesheet`/`clear_user_stylesheet` 注入 User 起源表。
  锁定：crates/style-engine/tests/cascade_layers.rs 十六件（层序四象限、
  起源互压、revert 三向、custom revert、宽关键字三态、flex 例外）。
- 多表/@import/@supports（B2，css-cascade-5 §7/css-conditional-3）：引擎
  `add_stylesheet`/`remove_stylesheet` 句柄化多 author 表（级联序 = 主表 +
  登记序，后表胜平手）；文档全局层树（各表层树附着重映射并入 doc_layers，
  跨表同名层 = 同层、先现序 = 文档序）；`@import` 附着期交错拼接（导入
  规则 = 写在导入点、拼接后全表按文档序重编号 order；`layer(name)`/裸
  `layer` 匿名层前缀、directive media 与规则 media 合取、`supports(...)`
  子句门控、循环 seen 栈 + 深度 32 守卫；导入源 = `set_import_source`
  内存表或宿主 loader 回调——URL→CSS 契约归宿主、引擎零网络；未解析指令
  保留待重拼接，主表重建自源文本重解析使嵌套未决存活）；嵌套 `@media`
  修正为合取 AND（`MediaQuery::conjoin`，旧"内层覆盖"违规范）；
  `@supports` 解析期求值（`(decl)` 文法试探 + `selector()` 试探 + 自定义
  属性恒真 + 裸声明宽容形；块形 + 导入子句两用；false 静默、语法无效
  告警）。锁定：crates/style-engine/tests/imports_supports.rs 二十件
  （拼接序、链式递归、循环守卫、未解析告警、层前缀、匿名层、media 门控、
  supports 门控、selector 特性、and/or 混用无效、多表句柄增删、层树跨表、
  跨表 keyframes、嵌套 media 合取）。
- CSS Nesting + :has() + :focus 族（B3，css-nesting-1 / selectors-4）：解析期
  desugar（`&` ≡ `:is(父)` 特异性=父选择器、无 `&` 隐式后代、深层递归展开、
  token 级 `&` 识别、深度 32）——级联核心零改动；规则体 RuleBodyParser 驱动
  （嵌套体声明合法化 + 声明/规则消歧）；**嵌套声明源位置分裂**
  （flush-at-item-boundary，浏览器一致）；嵌套条件组提升穿线（条件 AND
  合取 + 选择器挂父链）；:has() 原生匹配（子/后裔/相邻兄弟/not 反转/嵌套
  内组合），`has_relative_selectors` 表级标记 → 变更类失效升级全量重样式
  （增量失效暂缓；**P6 收窄落地**——宿主键快筛判定全量升级必要性，
  见失效收窄条）；`set_focus(key, focus_visible)` 焦点族整链管理
  （:focus/:focus-visible/:focus-within，祖先传播、迁移清链、移除清锚，
  focus_visible 启发归宿主）。锁定：crates/style-engine/tests/nesting_has_focus.rs
  二十六件（隐式后代、复合 &、状态伪类、组合器开头、深层、:is 特异性、
  media/supports 隐式声明、穿线、坏选择器告警、:has 六形态+失效升级、
  焦点七态）。
- **:has 失效收窄（P6，ADR-0035）**：`any_has_rules()` 命中不再一律全量
  重样式——**宿主键快筛收窄**：①`HasHostKey{tag,classes,id}`
  （pub(crate)，Default=哨兵恒通过）+ `list_contains_has`（深扫
  Is/Where/Negation 参数内嵌套防绕过）+ `has_host_key`（首 sequence
  组件收集；前缀组合器/通配/属性伪类混入→约束弱化或返 None，只多
  升级不漏升级）；②`has_host_index` 随表重建（挂全部 rebuild_font_faces
  调用点=表变更全路径），规则级零合格键/任一不合格 → 哨兵；③
  `has_invalidation_needs_full`——dirty 根沿祖先链命中任一键 → 全量
  升级，全否决 → restyle_subtrees 增量；④**remove+`:has` 结构失效
  收口**（存活父 push style_dirty_roots，此前 remove 非根只标
  dirty_struct——B3 起失配缺口）；⑤`any_container_rules` 补
  user/ua 表漏检、`any_has_rules` 补 ua_sheet。锁定：engine.rs 单元
  三件（键矩阵/判定矩阵/user 表 @container 检出）+
  tests/nesting_has_focus.rs 增两件（无关子树否决走增量+remove 失配
  捕获、前缀组合器哨兵命中）——二十八件。
- @property 注册（B4，css-properties-values-api）：三描述符 syntax/
  inherits/initial-value——syntax 文法 MVP = 14 类型族（length/
  percentage/length-percentage/number/integer/color/image/url/angle/
  time/resolution/transform-function/custom-ident/string）+ `+`/`#`
  单层多值组合子（spec 合法的 `|` 单组合与 ident 关键字语法 v1 亦拒绝——超出
  spec 必要拒绝面，重估条件=组合子文法补全；`||`/`&&`/嵌套语法→注册无效）；
  注册有效性 = 非
  universal 缺 initial 或 initial 不匹配→规则丢弃告警；universal
  （`*` 缺省）不校验、声明值原样在场。注册表 registered_props
  （合并序 user→主表→附加表，同名后者胜，sheet 变更点统一重建）；
  计算三闸 = 继承门（inherits:false 不进继承通道）→ 语法门（终值
  不匹配→unset→initial-value——Chrome 一致：var(--x) 解析到 initial
  而非触发 fallback，fallback 仅用于缺席名）→ initial 填充（声明/
  继承双缺→initial；无 initial 的 universal=缺席=guaranteed-invalid）；
  仅样式表顶层合法（嵌套/条件组/语句形丢弃告警）。0.x 破坏性：
  Stylesheet +property_rules 字段、compute_node_in +registered 参数。
  锁定：crates/style-engine/tests/property_registry.rs 十八件
  （initial 填充、声明直通、门败回 initial、门败不触发 fallback、
  未注册 fallback、继承 false/true、未注册回归、多表覆盖、universal
  在场、注册无效×2、嵌套/条件组/语句形丢弃×3、`<length>+`/`<color>#`
  多值、custom-ident 门）。
- 伪元素 ::before/::after + content（C1，css-content-3 / css-pseudo-4
  MVP；**P5 升级 <content-list> 见下条**）：ADR-0015 树实体化——伪节点 =
  引擎 materialize_pseudos 实体化的
  真实树子节点（::before 首子 / ::after 末子，裸 StyleNode 无身份）；
  selectors 0.40 原生匹配（parse_pseudo_element hook + originating_element
  回 origin 左复合，单冒号 CSS2 形 :before/:after 同路由）；
  has_pseudo_rules 解析期深扫判据——全表无伪规则零成本清除全部、有则
  全树 ensure+归位（宿主子序过滤伪节点）；content = none/normal/字符串
  单串 MVP；content
  仅伪元素语义——none/normal → Display::None 无盒（宿主恒 Normal）；
  文本经 sync_pseudo_text 同帧测量布局（继承 origin 字体/字号）；结构
  伪类与 :empty 不受伪节点影响；set_children 镜像自动合并伪键、remove
  两路径清 pseudo_ids 注册表。0.x 破坏性：StyleNode +pseudo 字段、
  ComputedStyle +pseudo 字段与 content()/pseudo() 访问器、Stylesheet
  +has_pseudo_rules、PropertyId +Content（SLOT_COUNT 134）。锁定：
  crates/style-engine/tests/pseudo_elements.rs 十四件（before/after 造盒、
  none/normal 无盒、下推宿主子、单冒号形、first-child/:empty 排除伪节点、
  hover 门控、字号继承、content 级联覆盖、换表清除、多表、class origin）。
- content 序列生成内容：counter()/counters()/attr()/quotes（P5，css-lists-3
  / css-content-3 / ADR-0036）：content 升级 `<content-list>`——
  `ContentValue::Seq(Vec<ContentPiece>)`（九变体 non_exhaustive：Str/
  Counter{name,style}/Counters{name,separator,style}/Attr/OpenQuote/
  CloseQuote/NoOpenQuote/NoCloseQuote；单串也承载 Seq；url() 维持拒绝）；
  新属性 counter-reset/counter-increment（`[ <custom-ident> <integer>? ]+`
  空格分隔、缺省 0/1、重复 ident 后者胜）与 quotes（none|auto|
  `[ <string> <string> ]+`，auto=拉丁四引号内置对，继承）；求值 =
  sync_pseudo_text 树序 DFS（reset 压帧遮蔽、increment 全栈累加、
  merge 弹出=兄弟继承、counter() 最内帧、counters() 全帧自外向内 join、
  attr() 读 originating element、open/close-quote 深度配对——close
  深度 0 静默）。counter style 参数经 @counter-style 登记表渲染（1.0 对齐
  counter_format 切片，见下条）。偏差【B】：url()
  图片内容不做（隐含 list-item 计数已由 P9-3 落地，见列表闭环条）。
  锁定：decl.rs
  counter_props_parse_family（15 正例+12 拒绝）+ pseudo_elements
  五件（树序 1/2/3、reset 1/1/1、join "1.1"、attr 存在/缺失、quotes
  配对/越配静默）。
- @counter-style 规则与计数器格式化（1.0 对齐，css-counter-styles-3）：
  解析+登记（counter_style 切片）——system 七形（cyclic/numeric/
  alphabetic/symbolic/additive/fixed <integer>?/extends <name>）+
  symbols/additive-symbols/negative/prefix/suffix/range（auto 或
  `[ <integer>|infinite ]{2}#`）/pad/fallback 九描述符；宽容语义与
  @font-face 对齐但未知描述符=告警（刻意不对称）；登记语境不设限（条
  件组/嵌套照常）；登记表合并序 ua→user→主表→附加表（附加表胜、可覆
  盖内置；名称区分大小写、内置 ASCII 不区分）。格式化（counter_format
  切片，engine eval_pseudo_content 消费）——css-counter-styles-3 §2 全
  算法：extends 逐字段合并→system 最低符号数门（不满足→decimal）→
  range 门（auto 语义随 system：cyclic/numeric/fixed=−∞..+∞、
  alphabetic/symbolic=1..+∞、additive=0..+∞）→未知/越界→fallback（链
  成环→decimal）→六种 system 核心（cyclic 取模/fixed 窗/symbolic 重
  复/alphabetic 双射/numeric 按位/additive 贪心+零权组）→pad（§3.6 补
  齐差值减负号簇数）→负号包裹→60 码点上限→decimal；counter()/counters()
  输出不带 prefix/suffix（§5）。内置子集：decimal/decimal-leading-zero
  （extends decimal+pad 2）/lower·upper-roman（additive 十三权+range
  1 3999，据 §6.1 Bikeshed 源锁定——0/负值/4000+ 走 decimal）/
  lower·upper-alpha·latin/lower-greek/disc·circle·square。偏差【B】：
  extends 合并以「字段≠默认值」判已设置（显式重述默认值不可区分）；
  disc/circle/square 用字面字形（§6.3 允许 UA 变体）；fallback:none
  宽容登记→空输出（严格按规范应回落 decimal）。偏差【C】：表示超 60
  码点→fallback（自定上限防病态展开）。锁定：counter_format 二十二
  单测 + pseudo_elements 五件引擎级（upper-roman/自定义注册/range 回
  退链/counters join/登记表覆盖内置）。
- UA 起源样式表（P5，css-cascade-5 / ADR-0033）：`ua_sheet` 挂点 +
  `set_ua_stylesheet`/`clear_ua_stylesheet`（镜像 user 表五步链）；级联
  收集序 Default→UA→User→Author（::selection/::placeholder 通道同挂）；
  @property/@font-face 合并序 UA 先于 user；important 反转链锁：
  UA-important(6) > Author-important(4)、Default-important 最强(7)；
  `style_engine::builtins::DEFAULT_UA_SHEET`（HTML 语义最小表：块级
  清单/h1–h6 字号+bold+margin/p 等 margin/i 族 italic/u·s 装饰线/
  center/pre monospace+white-space:pre）；默认不装载（中立契约——表是
  数据不是行为，宿主显式装载）。P9-1b（ADR-0039）：b/strong UA 声明由
  bold 改为 bolder（css-fonts-4 §2.2.1 相对字重——400 父下两者等值，
  h1 内 b 等非 400 父场景 bolder 语义才正确）；P9-1c（ADR-0039 附）：
  small/big UA 声明由绝对字号改为 smaller/larger（css-fonts-4
  `<<relative-size>>`——16px/medium 父下与旧绝对关键字同值 13/18，
  其余父值按表步进或 1.2 比例随父缩放）；P9-3（ADR-0041）：li 发
  display:list-item、ul/ol 列表标记族（disc/circle/square/decimal 嵌套
  对齐 Chromium，见列表闭环条）。偏差【B】残余：hr 3D/表 UA 细节。
  锁定：engine.rs ua_origin_ladder_and_important_inversion +
  ua_builtin_sheet_applies_and_clears + ua_bolder_semantics_and_author_override +
  ua_small_big_relative_font_size。
- 文本变换与断行控制（C2，css-text-3 / ADR-0016）：text-transform
  none/uppercase/lowercase/capitalize/full-width（full-size-kana 接受
  no-op=T2）——自实现分段变换：span 边界切段、逐字符 old→new 字节映射
  （扩缩字符安全）、测量（measure_two_pass）与绘制（op.text 变换后文本
  +span 偏移重映射）双注入、全表 none 快路径直通；word-break
  normal/break-all/keep-all + overflow-wrap normal/break-word/anywhere
  （`word-wrap` 别名）= parley 原生断行映射，测量/vello 绘制同参
  （PaintOp::Text +word_break/+overflow_wrap 通道）。继承：transform 是、
  wrap 对否（spec）。锁定：text_transform 单元 5+tests/css_text.rs 11
  （uppercase/capitalize 折行几何、继承等高、full-size-kana 惰性、
  full-width 加宽、break-all 词内断、break-word 溢出断、anywhere 等高、
  别名等高、keep-all CJK 禁断、变换+断行叠加）。偏差：capitalize 词界
  ≈alphanumeric（spec=UAX#29）、soft 渲染不消费断行参数（v1 无折行）、
  anywhere min-content 差异未锁（折行几何等高已锁）。
- conic-gradient + object-fit/object-position（C3，css-images-3 / ADR-0017）：
  conic = `GradientKind::Conic(ConicSpec { from, position })`（缺省
  0deg/center；语法 `conic-gradient([from <angle>]? [at <position>]? ,?
  <stops>)` 复用角度/位置/停点三助手）——paint 层 `ConicGeom { cx, cy,
  start }`（绝对 px 圆心+起始角弧度；CSS 0deg=12 点→(deg−90°)·π/180 正 X
  轴起顺时针），vello 经 peniko 0.6.1 Sweep 原生映射（SweepGradientPosition
  center 内建、Y-down 顺时针=CSS 同向），soft 逐像素 atan2 扫角归一——
  四象限硬停点像素锁。object-fit fill/contain/cover/none/scale-down +
  object-position（left/center/right/top/bottom/<length-percentage>；slot
  130/131，SLOT_COUNT 139）：诚实替换内容通道——`StyleNode.image` 引用
  （与背景 url() 语义解耦、background-size 留 F 批不冲突）→ 引擎
  seed_image_leaves（compute_layout 前种子：双边声明=盒取声明/单边=另一边
  按源宽高比缩放/全 auto=自然尺寸（块级替换不拉伸）；absolute 叶仅注入
  固有区间走 T5d shrink 通道；宿主 set_leaf_intrinsic 手动优先）→ paint
  元素图像段（fit 数学+object-position 偏移 offset=pct·(盒−拟合)（负基
  合法=溢出反向对齐）；溢出/圆角 PushClip(内容盒+radius) 包裹；
  PaintOp::Image=拟合后矩形，sink 零改动）；未注册引用告警跳过（零副
  作用契约）。0.x 破坏性：GradientKind +Conic、PaintOp::Gradient +conic
  字段、StyleNode +image、PropertyId/DeclValue +2（SLOT_COUNT 139）、
  ComputedStyle +object_fit()/object_position()。锁定：
  crates/style-engine/tests/css_images.rs 十三件（conic 默认几何 −π/2+
  盒心/from 90deg→0+at 25%→(15,10)/停点直通；fit 五模式 dest 矩形、cover
  PushClip、position 关键字与 px 偏移；image 叶自然 40×20/声明宽 80px→
  80×40；未注册零 op）+ soft conic 象限像素一件（45°/135°/225°/315°
  红蓝交替）。
- repeating-*-gradient（P1-3，css-images-3）：`repeating-linear/radial/conic-gradient()`
  三族全收——分派器按函数名 strip `repeating-` 前缀后复用三解析器
  （内层文法逐一相同），`css::Gradient.repeating: bool` 标记随
  BackgroundImage→PaintOp::Gradient（内嵌 gradient 副本）流入三 sink。
  语义=停点模式沿渐变轴无限平铺（周期=首末停点跨距；显式 Px 停点经
  线长归一、逆序按 §4.5.2 抬升后首末重合 → 周期 0 → 透明黑=source-over
  无操作；全缺省停点周期=全长退化为非 repeating）。sink 终结：vello
  画刷几何收缩为「一个周期」（linear=单位向量缩段/radial=
  RadialGradientPosition::new_two_point 两圆承载 r0 首停相位/sweep=
  起终角弧段）+ stops 平移归一 [0,1] + Extend::Repeat（r0=0 平移 stops
  会丢相位 first——径向必须 new_two_point）；soft 采样 t 不夹取改
  u=first+(t−first).rem_euclid(period) 回停点序列插值（垂直条带方向
  不重复，与 CSS 一致）。em/rem/cq 相对停点三 sink 同约定均布（既有
  偏差延续）。0.x 破坏性：css::Gradient +repeating、GradientDump
  +repeating（#[serde(default)] 旧 dump 兼容）。锁定：css_images.rs
  repeating 三族 flag+kind 一件；paint.rs repeating 标记流入
  DisplayList 一件 + serde 往返/旧 dump 回落一件；vello
  repeating_peniko 段收缩+周期 0 两件；soft 像素锁两件（40px 盒周期
  20px：x=0/5/10 手算 (249,0,6)/(185,0,70)/(121,0,134)、x 与 x+20
  同色；周期 0 画布不变）。
- mix-blend-mode / isolation 实混合层（P1-2，css-compositing-1/2）：mix-blend-mode 全 18 值（css-compositing-1 16 标准模式含 normal + plus-lighter/plus-darker）+ isolation（isolate/auto）从「仅 SC 触发位」升级真实混合层——公共枚举 `BlendMode`（`DeclValue::BlendMode` 替换旧 `Effect(bool)` 存在位）；`PaintOp::PushBlend{mode,x,y,width,height}/PopBlend` 层对（bbox=border-box 同 PushOpacity）；混合层须最外——PushOpacity 先 push、PopOpacity 后 pop，合成序=blend(背后画布, opacity(子树))；`isolation: isolate` ≡ `PushBlend{Normal}` 隔离组边界（子树内混合不越界，修复旧「isolate 内混合穿透祖先画布」缺口）。sink 覆盖：soft sink 全 18 值原生像素合成（可分离模式全式 Co=αs(1−αb)Cs+αs·αb·B+(1−αs)αbCb、非可分离按 W3C Lum/Sat、plus 族按预乘加法）；vello sink 16/18——peniko `Mix` 枚举 16 标准模式一一映射，plus-lighter/plus-darker 不在 peniko Mix 枚举（上游缺口，见 DEPENDENCIES.md）退 Mix::Normal=B 级。测试：SC 层对发射/发射序（blend 包 opacity）、解析语义重基线（isolation/mix-blend 各 +2 op）、soft 像素锁 ×10（可分离/加法/非可分离/嵌套 opacity）、BlendMode serde 往返（kebab-case 模式名）。
- filter / backdrop-filter 本体化（P2，css-filters-1/2）：F4 存在性语义
  升级全函数族本体——①类型：`css::property::FilterFn`（#[non_exhaustive]
  十函数：Blur/Brightness/Contrast/Grayscale/Sepia/Saturate/Invert/Opacity/
  HueRotate/DropShadow；长度/颜色活到计算期 `LengthPercentage`/`ColorValue`）
  + `DeclValue::Filters(Vec<FilterFn>)`（替换 Effect(bool) 承载，
  Effect 变体保留给 will-change/isolation/mix-blend-mode）；`none` →
  `Filters(vec![])` **有效覆盖声明**（级联胜出，非缺席）。②解析：
  `parse_filter_value_list` 严格文法（`none | <filter-function>+` 白空格
  分隔无逗号、percentage /100 归一、钳位不拒绝 grayscale/sepia/invert/
  opacity∈[0,1]（浏览器行为 sepia(150%)→1.0）、brightness/contrast/
  saturate≥0、缺省实参（brightness()=1、hue-rotate()=0、blur()=0）、
  drop-shadow 两序 `<length>{2,3} && <color>?`、未知函数/参数非法/`url()`
  整条拒绝）。③绘制：`paint::FilterEffect` 绘制域终结形态 +
  `PushFilter{filters,x,y,width,height}/PopFilter` 层对（bbox=border-box）
  + `BackdropFilter` 单点即时 op（节点最前发射、主画布区域替换写回、
  pad 环仅取样）；层序 transform→clip→blend→opacity→filter（合成序
  opacity(filter(子树))，css-filters-1 §3）收尾严格 LIFO（三重嵌套锁）；
  SC 谓词 has_filter()/has_backdrop_filter() 改读 Filters 非空。
  ④soft 原生逐像素全函数管线：颜色矩阵族按 css-filters-1 §4 sRGB 表
  3×4 乘、brightness/contrast 仿射、blur σ=r/2 盒模糊（P1-4 同源）、
  drop-shadow 遮罩→模糊→平移→着色→影先源后、opacity alpha 缩放；
  FilterLayer 快照-清空-回合成、非预乘直排 src-over（filter 后全透明
  保留快照）。⑤vello：纯 opacity 链 alpha 连乘直映，其余 warn-once
  恒等层降级（栈平衡保持；B 级在案）。⑥serde：`FilterEffectDump`
  （tag="fn" kebab-case）+ OpDump push_filter/pop_filter/backdrop_filter
  往返。测试 +6（551=545→551）：filter_parse_strict（18 正例 7 拒绝例）、
  backdrop_filter_parse_strict 重写、filter_layer_triple_nesting_lifo、
  SC op 增量（filter+2/backdrop+1/will-change+0）+层对位置、soft 端到端
  像素三件（invert 层/backdrop 区域替换/透明保留快照）、
  filter_dump_round_trip。
- margin collapsing 重估 + padding 长手修复（P1-1，css2.1 §8.3.1）：块流
  折叠语义全路径正确并首次锁定——父首子顶塌穿（父无 padding/border 时
  mt 出父外）、兄弟间距=max(正和)+min(负)（负 margin 入 max）、空块自塌
  穿、浮动子不参与折叠、结算 pass（tables/columns/floats/lines）不扰动
  塌缩；taffy 原生 CollapsibleMarginSet 承载块流折叠（引擎块流直通无二
  次折叠），浮动仍由 settle_floats 手管（taffy 无 float 映射，历史结论
  维持）。新增 tests/css_margin_collapse.rs 七件锁。**A 级修复**：padding
  物理与逻辑八长手（padding-top/right/bottom/left、padding-inline/block
  四向）解析误路由 parse_corner_radius 产出 Radius 值族，而计算/布局/
  绘制读取方全只认 Len → 长手写法整链静默归零（简写不受影响故既有测试
  未暴露）——解析派发改 parse_len（Radius 四角不变），
  cascade_layers.rs 固化断言改回 Len 族；锁定长手生效+折叠共存、
  calc 正腿、padding-inline-start、inherit 逐字复制。遗留（既有在案）：
  末子 margin-bottom 与父塌陷时父盒被下移（见三期②条 B 级注）。
- ::selection / ::placeholder（C4，css-pseudo-4 / ADR-0018）：非盒生成
  伪元素双通道——`PseudoElement` +`Selection`/`Placeholder`（单冒号形
  拒绝），匹配=origin 直配（pseudo==None 即命中；实体化伪节点恒不
  命中；originating element=节点自身，前缀复合在 origin 求值），主级联
  防泄漏（collect_sheet 过滤，通道规则不进主样式）+ `cascade_channel`
  通道级联（无内联段）+ `compute_node_from_cascade` 复用（继承基=
  origin 主样式，css-pseudo-4）；engine `selection_style(key)`/
  `placeholder_style(key)` 通道访问器，契约=有规则命中才 Some（@media
  外/选择器不命中 → None 宿主回退系统缺省；全表无规则 → 整 map 清
  零成本）；selection-only 表不触发 materialize_pseudos；Chrome 生效
  属性子集（selection=色/背景/文本装饰系/text-shadow/caret-color；
  placeholder=色/font 系/opacity/letter-spacing/line-height/
  text-transform/background-color）全量解析计算、宿主取舍；选区在场/
  占位文本存在性归宿主（零副作用）。锁定：tests/css_selection.rs 十件
  （解析/单冒号拒绝/主级联隔离/特异性/author 表序/@media 门/origin
  继承基/双通道独立/无规则缺席/实体化闸宿主盒三态）+ selector.rs
  origin 直配探针一件。
- float / clear（E4，css-position-3 / ADR-0019）：浮动结算引擎侧实现
  （taffy 0.14 无原生 float）——`settle_floats` 两相（每帧 map_style
  重放还原→pristine 布局→放置/覆写→终布局；挂点=settle_columns 后、
  文本 remeasure 前）：浮盒出流（taffy Position::Absolute+inset 锚定）+
  同父浮栈水平适配贪心放置（同带继续直至容器缘溢出才下移；own_clear
  钳位）+兄弟环绕（出流补偿 post_y；顶缘落带内=同侧带宽和 margin
  增量；clear 盒全宽）+clear 钳位（margin-top 增量至相关向最后浮盒
  底）；`FloatKind`（none/left/right）+`ClearKind`（none/left/right/
  both）slot 132/133（SLOT_COUNT 141）；诚实边界=兄弟级环绕带宽
  常量化、嵌套子树局部结算、BFC 隔离边界按需迭代（偏差在案）。
  锁定：tests/css_float.rs 五件（左浮出流+环绕/clear 钳位/右浮右缘
  对齐/同带堆叠/none 回归）。
- grid-template-areas 与命名线（E5，css-grid-1 / ADR-0020）：grid
  放置全链（taffy 0.14 模板存储缺非重复行名）——`apply_grid_placements`
  纯样式派生（容器线名注册表 line_names[i]=线 i+1 + 区域矩形 → 子
  四长手解析 → taffy grid_row/grid_column 数字放置；挂点=
  seed_image_leaves 后、compute_layout 前无额外重排；每帧幂等）；
  解析面=GridTemplateAreas/GridRowStart/GridRowEnd/GridColumnStart/
  GridColumnEnd 五槽（SLOT_COUNT 146）+`GridAreas`（矩形校验：行宽
  一致+逐名格数==行跨×列跨——min/max 边界法漏对角格）+
  `GridLineSpec`+`GridTemplate.line_names`（cssparser `[ … ]`=
  SquareBracketBlock → parse_nested_block 收名）；简写 grid-row/
  grid-column/grid-area（缺省段镜像 ident per spec §7.4/7.5）；
  解析序=区域边线→全名线→-start/-end 后缀裸名→未知名 Auto；隐式
  轨=taffy grid-auto-* 原生（auto_tracks 升级全列表映射）。诚实
  边界=repeat 内线名解析失败声明无效、`span <int> <ident>` 混合形、
  subgrid（taffy 无 Subgrid 变体）。锁定：tests/css_grid_areas.rs
  八件（2×2 区域矩形放置/线名 a..b/整数线号/span 整数/span 名线/
  未知名 Auto/非矩形区域无效回退/区域隐式列）。
- IFC 行内流 v1（F1，css-display-3 / ADR-0021）：块容器直子行内参与者
  贪心行打包——文本叶（tree.node.text）/inline-block 原子盒/inline 组
  盒 → `settle_lines`（settle_floats 后、文本 remeasure 前；Absolute+
  inset 锚定=浮盒先例；inline_run_participants 豁免 remeasure 全宽重
  测；prev_participants 快照帧间幂等）。Display +Inline（连续性标记/
  组盒）+InlineBlock（原子）变体；inline-flex/grid/table 仍归一告警。
  行打包=叶 measure_rich(剩余宽) 接排+折行叶占满本行、声明宽/高叶原
  子式参与、盒自然尺寸=MaxContent 探针+子树叶 nowrap 测宽 max（taffy
  无文本内在尺寸）、行盒溢出续排（CSS 真行为）、容器 min_height=打包
  行高（auto 高塌陷防护）。诚实边界（0.x）：行高=max(测量高)、
  vertical-align=TOP（baseline 延后→**P3 已落地全值族**，见下条）、
  组内子叶纵向堆叠（多叶组宽=max）、跨叶 br 不支持、仅 Block 容器
  直子。锁定：tests/css_inline.rs 七件（叶+盒同行 advance 接排/双盒
  换行+容器高/叶续盒/折行推下行/组盒参与/nowrap 溢出续排/无参与者
  块流回归）。
- vertical-align 基线对齐（P3，ADR-0034）：属性面新槽位
  `vertical-align`（`VerticalAlignKind`：baseline|sub|super|text-top|
  text-bottom|middle|top|bottom|`<length-percentage>`；不继承、初始
  baseline；严格文法尾 token 拒绝）；测量层 `measure_with_baseline`
  返回 (宽, 高, 首行基线=首行首 run `ascent.round()`，加法式——
  measure/measure_rich/measure_min_content 签名不变）；行结算两阶段
  （TOP 装箱收集 → flush_inline_line 统一结算：L=max(基线距)、逐
  参与者 dy 回填 inset.top、行盒扩展、bottom 二遍）；Box 参与者基线
  =子树首文本叶（`box_first_text_baseline`，无文本=盒高）；字体度量
  扩展 hhea asc/desc（FontMetrics `ascent_per_em`/`descent_per_em`）。
  偏差【B】：baseline（初始值）不产生偏移（v1 逐位一致保护，混字号
  默认基线下沉后续）；sub/super=±0.34em 兜底常量（UA 决定值，Chromium
  校准未做）；middle x-height=fontprobe ex_per_em（未注册 0.5em）；
  calc() 承载偏移=0；% 基准=装箱行高；仅 settle_lines 行内参与者
  消费（flex/grid baseline 对齐等上游；span 级不做）。锁定：
  decl.rs::vertical_align_parse_family（12 正 4 拒）、
  text.rs::measure_with_baseline_first_run_ascent、engine.rs 行为锁
  六件（no_decl_bitwise_v1/length_offset/super_sub_shifts/
  middle_between_super_and_baseline/box_baseline_from_text/
  top_bottom_alignment）。
- 结算 DAG 与文本效果五件（F2，ADR-0022）：①帧内结算链硬编码 →
  `SettlePassKind{Calc,Tables,Columns,Floats,Lines}` 显式依赖图+拓扑
  `schedule()`（debug_assert 环防护）+ `settle_should_run` 空输入门——
  后续结算 pass 声明式插 DAG（顺序/豁免语义逐位保留）。②text-overflow:
  ellipsis（css-overflow-3，槽 139）：叶级绘制期截断——`text_overrides`
  覆盖（布局不变，ink 裁切语义同浏览器）+ `make_ellipsis_text` 二分
  字符前缀（width(prefix)+"…"≤可用宽，前缀即溢出→空串）+
  `apply_text_truncation` 两相（不可变扫描→分派，measure_rich 借用
  约束）；PaintCtx 通道+span 字节区间随前缀过滤/钳位；overflow: visible
  不截断（spec）。③-webkit-line-clamp（槽 140，none|正整数
  `min(i32::MAX)` 防 as-cast 回绕）：截断预算=行数×行高（行高缺省
  1.2×font_size）、全文折行测量≤预算不截断；display:-webkit-box 接受
  按块布局（偏差在案）。④text-decoration 四长手+简写（槽 141–144）：
  line 位集（1=underline 2=overline 4=line-through）/style
  Solid|Double|Dotted|Dashed|Wavy/color（缺省 currentColor）/thickness
  Auto|FromFont|Length，简写『line*|style|color|thickness』任意序贪心
  （line 多关键字累积 OR）；绘制=Text.decorations（厚度 LP 解析 px），
  sink 线位 underline=baseline+descent×0.5、overline=baseline−ascent×
  0.9、line-through=baseline−ascent×0.5；**style 全家族精确绘制
  （P4 D3，ADR-0037）**：Solid 整带/Double 两半厚带（cy±0.75t 各
  0.5t）/Dashed 段 2t 步进 3t/Dotted 圆环折线（直径 t 中心距 2t）/
  Wavy 真波形带（周期 6t、振幅 2t、每周期 8 段、带厚沿波平移，
  soft/vello 同参折线闭环），全部折线承载 mat 设备化，span 级装饰=
  边界（v1 叶级）。⑤
  text-shadow（css-backgrounds-3，槽 145，SLOT_COUNT 153；**继承**）：
  `none | [<color>? <dx> <dy> <blur>? <color>?]#`（颜色前后均可置，
  缺省 currentColor/blur 0）；绘制=Text.shadows 影字先绘——soft sink
  （P1-4 批）：blur>0=字形折线遮罩+3×盒模糊真模糊（装饰线不投影，
  Chromium 同语义）、blur=0=transform 平移重发（spans 置空=全字影
  色）；tiny（P10）同 soft 真盒模糊（复用 `blur_alpha_u8`）；vello=
   平移重发 + 多重同心偏移环近似模糊（环数随 blur 自适应、环
   α=1−(1−a)^(1/N) 复合守恒；B 级在案）。诚实边界
  （0.x）：装饰厚度 auto/from-font≈font_size/12、字体优先装饰位未接
  （线位=B 级近似）、vello 侧 text-shadow=环近似模糊（非真盒模糊）。锁定：tests/css_text_overflow.rs 六件（ellipsis 截断/放得下
  不截/visible 不截/clamp 两行/clamp none 全文/clamp 放得下不截）+
  tests/css_text_decoration.rs 六件（简写解析携带/缺省空装饰/line
  位集/影单+色/影多+blur/none 空）。
- hit_test 命中测试（F3a，ADR-0023）：paint 期命中几何表——paint_node
  递归收集 `HitRect{node_id,x,y,w,h,clips,mat}`（border-box 视口坐标+
  活跃 clip 链快照；**P4 D4，ADR-0037：clips 升级 `HitClip` 枚举——
  Rect{rect,radius,inv}/Path{points,nonzero,inv} 精确几何+各 clip 自身
  逆阵；mat=收集时活跃仿射**），`PaintCtx.hit`
  通道透传（RefCell 收集器，None=零成本）；`StyleEngine::hit_test(x,y)
  -> Option<HitTestHit>`（绘制序逆序=顶优先；**命中点先经 HitRect.mat
  逆变换到局部系测盒、clips 逐个经自身 inv 判定——圆外/多边形凹角/
  变换后点精确，AABB 近似退役**）+
  `StyleEngine::node_id(key)`（用户键→NodeId 宿主解释通道）。
  visibility: hidden/display:none 天然不入表；pointer-events: none
  收集期排除。锁定：tests/css_hit_test.rs 五件
  （顶层命中=后绘优先/overflow 裁剪外不命中/pointer-events: none
  穿透/clip-path 圆外穿透/transform 旋转盒命中）。serde（同 ADR 后半）拆 F3a2 批。
- DisplayList serde 投影（F3a2，ADR-0023 决策 2）：paint_dump 模块
  （feature="serde"，serde 可选依赖）——PaintOp 全 14 变体 typed
  tagged-enum 镜像（serde tag="op"；色=[f32;4]、ImageRes 像素
  Vec<u8> 直序列）；`to_dump()`/`to_display_list()` 往返；枚举
  canonical 名承载（non_exhaustive 同 crate 全变体枚举——新增变体
  编译器强制更新镜像）；未知 op/单位/枚举名降级跳过（不 panic）。
  锁定：tests/css_serde_dump.rs（结构往返+JSON 通道相等，serde_json
  仅 dev-dep）。
- 背景全集（F3b，ADR-0024，css-backgrounds-3）：①六长手值族——
  background-repeat（RepeatXY 双轴 Repeat/Space/Round/NoRepeat，
  repeat-x/y 简值展开）、background-attachment（Scroll/Fixed/Local）、
  background-position（两轴 PositionComp{base, offset}，edge 关键字/
  百分比/长度 + 三值四值偏移文法，right/bottom 偏移解析期取反=B 级）、
  background-size（斜杠对 Auto/Cover/Contain/Explicit）、
  background-origin/clip（BorderBox/PaddingBox/ContentBox；clip 另有
  Text——降级 border-box+警告=B 级）。②多层列表：七长手全 Vec 化，
  `ComputedStyle::background_layers()` 层对齐视图（层数=max(各列表,1)、
  i%len cycling、缺省=初始值参与）；background-image: Url/Gradient/None。
  ③background 简写：八长手展开（层内无序贪心；单 box=origin/clip 双
  赋、双 box 首 origin 次 clip；color 仅末层否则整条拒绝；缺省部件回
  初始值）。④绘制：色 FillRect 恒发（border-box+元素圆角）；层反序
  发射=首层最上；fixed=视口锚定；origin/clip=PushClip 裁剪盒（每层
  包裹）；tile 双循环平铺——**P4 D2（ADR-0037）：TileAxis{None,Repeat,Space,Round} 精确化取代 bool**（space 均布 gap 首片锚定定位区、round 整数片拉伸 ts=area/n 恰铺满、n≤1 退化单片 position 生效）；cover/contain 精确
  数学、explicit 宽+auto 高保纵横比；渐变逐 tile 重解几何；Image 固有
  尺寸+repeat 平铺（未注册 url=警告跳过）。0.x 破坏性：PropertyId +6
  变体（SLOT_COUNT 153→159，动画描述符让位 152–158）、
  `DeclValue::BackgroundImage` 单值→Vec。锁定：decl.rs 四件（全组件
  解码/多层缺省/box 双赋/三拒绝）+ computed.rs cycling 双向 +
  paint.rs 多层反序发射序 + tests/css_images.rs 十三件。
- clip-path 裁剪形状（F3c，ADR-0025，css-masking-1 §5.1 / css-shapes-1
  §3）：①值族——`ClipShape{None, Inset{insets, radius, reference},
  Circle{radius, at, reference}, Ellipse{rx, ry, at, reference},
  Polygon{nonzero, points, reference}, Other}` + `ClipRadius{Length/
  ClosestSide/FarthestSide/ClosestCorner/FarthestCorner}`；`<basic-shape>
  || <geometry-box>` 次序不限、geometry-box 单独出现=inset(0) 基准该盒、
  reference 平铺进形状变体；margin-box 降 border-box=B 级；url()/path()
  宽容→Other（UnquotedUrl 顶层 token 与 Function 两形态都接住）+tracing
  警告（SVG clipPath=T2）；polygon fill-rule 缺省 nonzero、<3 坐标对整条
  拒绝；负半径拒绝；circle 百分比半径合法（F3e 修正：css-shapes-1 §3.2.1 基准
  √(w²+h²)/√2，paint.rs 同步解析——原"轴歧义拒绝"系误读，遗留 token 曾致整条
  声明丢弃）。②计算：复用
  第四批④ `PropertyId::ClipPath`（slot 71 原位升级，Effect 组剥离——
  零 slot 破坏、SLOT_COUNT 保持 159）；初始 none、非继承；
  `has_clip_path()`=形状≠none。③绘制：新 `PaintOp::PushClipPath{points,
  nonzero}`——inset 走既有 PushClip 矩形（round radius 精确承载）；
  circle/ellipse 段数自适应 clamp(ceil(2πr/3),16,256)（P4 D6：弦长目标 3px，小圆减段大圆平滑；旧固定 64 退役）；polygon 顶点直传+
  fill-rule 随 op；裁剪作用元素+子树（overflow 后、背景前，LIFO 双
  PopClip）；命中=AABB 链近似（B 级）；量程不变。④sink：vello BezPath
  push_layer（NonZero/EvenOdd）、soft 射线法 point-in-polygon（winding/
  even-odd 双规则）、paint_dump 镜像同步。锁定：decl.rs 五件 + paint.rs
  六件 + soft 像素两件（{5/2} 五芒星序 nonzero/evenodd 差分）+
  css_serde_dump 往返 + computed 初始非继承；效果触发 SC 测试重基线
  （inset(0) 实发 PushClip+PopClip 对）。0.x：PaintOp 新变体
  （non_exhaustive 宿主 match 需新臂）。
- border-image 全集与字体深化（F3d，ADR-0026，css-backgrounds-3 §6 /
  css-fonts-4）：①**border-image 值族与解析**：五长手——source（复用
  BackgroundImage{None,Url,Gradient}）、slice（1–4 值 + fill 任意位、
  负值拒绝）、width（<LP>|<number>|auto 三态，负值拒绝）、outset
  （<LP>|<number>，百分比拒绝）、repeat（stretch/repeat/round/space ×双轴）
  + **border-image 简写**（`<source> || <slice> [/ <width>? [/ <outset>?]?]?
  || <repeat>`，词法不相交贪心单遍）。②**计算**：slot 152–161（
  SLOT_COUNT 159→**169**）+ font 族五槽 162 区（stretch/word-spacing/
  features/variations/variant-caps）；border-image 五长手非继承、font 族
  继承（css-fonts-4）；ComputedStyle 访问器 ×10。③**绘制九宫格**：source
  ≠none 时**替代 Border op**——四角恒拉伸 → 四边 tile（repeat 双轴独立）
  → fill 中心；切片溢出缩放 f=(w/(sl+sr)).min(h/(st+sb))、带宽 fw 同型
  （基准=边框盒）；outset 仅 ink-overflow（不入滚动量程/命中）；
  Repeat 顺排末片 PushClip 截断、Round 等分拉伸、Space 首尾贴边 gap 均
  摊（优于 CSS 精确重排=B 级在案）。④**图像/渐变通道精化**：
  PaintOp::Image +src 采样窗口四字段（vello sub_rect 裁剪层 + 非均匀仿
  射、soft 源窗口采样）、PaintOp::Gradient +linear 绝对几何 LinearGeom
  （0deg=12 点顺时针 d=(sinθ,−cosθ)；径向/锥形已有盒几何，线性对齐）。
  ⑤**@font-face 登记表**：FontFaceRule 全描述符（font-family/src
  （url()|local()+format()）/font-style（oblique 角）/font-weight（区间）/
  font-stretch/font-display/unicode-range（'?' nibble 通配、0x10FFFF 钳
  制）/font-feature-settings/font-variation-settings/ascent-override/
  descent-override/line-gap-override（normal|<percentage>，/100 存储））；
  登记语境不设限
  （顶层/条件组/嵌套均登记，@media 内 @font-face 真实世界常见）；缺
  family/src=规则无效 warn 丢弃、未知描述符宽容跳过、已知描述符值非法=
  该描述符忽略规则存活；`engine.font_faces()` 访问器（user→主表→附加表
  合并序，同族后规则胜）；字体字节仍宿主 add_font 推送（契约不变，src
  url 文本=注册表键）。⑥**字体深化接线**：PaintOp::Text +4 字段
  （font_stretch/word_spacing/font_features/font_variations，基样式级）；
  vello push_default FontWidth(from_percentage)/WordSpacing/
  FontFeatures/FontVariations（parley 0.11 原生）；soft 伪拉伸（轮廓与
  步进同比 x 缩放）+空格词距（软栅格无 GSUB/gvar=记录偏差）；
  effective_font_features（显式 settings ∪ font-variant-caps 派生
  titl/unic，同 tag 显式优先）；**small-caps 合成**（lowercase→uppercase
  + 0.8 缩放 span，petite 回退 small、先 text-transform 后合成=CSS 顺序，
  Chromium 一致语义）。**顺手修复两既有缺陷**：@media/@container 臂漏并
  子表 keyframes（条件组内 @keyframes 静默丢弃）；parse_font_stretch 百
  分比漏 ×100（cssparser unit_value=分数，125% 误判越界整条丢弃——锁测
  试抓出）。0.x 破坏性：SLOT_COUNT 159→169、PaintOp::Image/Gradient/Text
  新字段、AtPrelude::Skip→AtPrelude::FontFace 更名、Stylesheet+font_faces。
  锁定：decl/paint/soft/dump/registry 五面（九宫格区域与 src 窗口精确
  锁、repeat tile 数与 clip、渐变九区共享绝对几何线、@font-face 全描述
  符/无效丢弃/条件组登记/合并序、small-caps 测量=大写×0.8 组合、serde
  往返含四新字段）。全量测试绿（当时 **477**=462→477，serde,text 全跑；现值 ≥512，以 CI 为准）。
- 人读调试工具链（F3e，ADR-0027）：纯 core 格式化零新依赖——`debug::
  display_list_dump`（Push/Pop 作用域缩进树，元素自身 bg/text 印在自身
  clip push 之前）、`ComputedStyle::debug_dump`（全 ALL 槽逐属性 +
  [metrics]/[pseudo]）、`Frame::boxes_dump`/`StyleEngine::layout_tree_dump`
  （0.x 加性公共 API）；锁测试 `css_debug_toolchain.rs` 断言缩进结构。
  配套：conformance `pixel_invariants.rs` 七锁（渐变单调/背景层序/
  circle 遮角/border-image 环/box-shadow 墨迹/opacity 混合/确定性）+
  fuzz 三靶（@font-face 注册表/绘制声明/hit_test⊆border-box）+
  clippy workspace `-D warnings` 清零（78→0）；circle 百分比半径修正
  （√(w²+h²)/√2 基准，见 ADR-0025 落地修正）。全量测试绿（当时 **490**；现值 ≥512，以 CI 为准）。
- 属性面补全 F4（ADR-0028，31 项收口）：①**backdrop-filter**——filter 同
  型存在性语义位（parse_sc_effect 复用：值 ≠ none → `Effect(true)`、none
  缺席、函数块宽容吞咽）；css-filters-2 非 none 即触发 stacking context，
  paint SC 谓词并入 `has_backdrop_filter()`（非定位触发者进 Pos 带键 0，
  触发表锁扩 backdrop-filter 行）；效果本体后于 P2 本体化（ADR-0031，
  strict 文法+PushFilter/BackdropFilter ops+soft 全函数管线，见下）。
  ②**hyphens**——`HyphensKind{#[default]
  Manual,None,Auto}` 属性层（css-text-3 §5.4 initial=manual、继承属性入
  inherits 白名单）；断词效果三值 v1 一致=上游边界（显式断点 U+00AD/
  U+2010 由 parley UAX 14 分段、auto 无词典、none 无法抑制上游断点——
  重估条件=parley 连字/断点覆盖 API，见 T2 hyphens 条）。槽位：ALL 尾
  追加 Hyphens=162/BackdropFilter=163、动画描述符整体后移 ×2 至
  164..171（0.x 破坏性=SLOT_COUNT 169→171 + PropertyId/DeclValue 变体
  加性）。锁：decl.rs `hyphens_parse_modes`（三关键字+非法拒绝+initial
  manual）、`backdrop_filter_parse_strict`（P2 重写：none/函数正例+
  宽容存在性拒绝）+ engine SC 触发表扩行。全量测试绿（当时 **492**=490→492；现值 ≥551，以 CI 为准）。
- T0 零容忍收敛（二期⑤）：conformance 全量绿且零 xfail——Numeric 16 用例（仅存 xfail 项 calc-width 已于二期①转正）+ Pixel 3 用例（二期④通道转正）；9 manifest 显式 xfail=false、其余 serde 默认 false，零 xfail 资产存续，T0 范围内 Class 3–5 零违规；含文本/变换用例暂不入 Pixel 通道属 ⑦ 能力缺口（非 xfail，⑦ 转正后并入）

## T1 — v0.2+

- @keyframes 动画（第五批⑰已落地）：解析——`@keyframes name { from/to/百分比 [,分组] { decls } }`（-webkit-keyframes 别名；坏帧选择器整帧容错丢弃+告警）；描述符——animation-name/duration/delay/iteration-count(infinite)/timing-function(linear/ease 系三次贝塞尔二分求值/steps(n[,start|end]))/direction(normal/reverse/alternate/alternate-reverse)/fill-mode(none/forwards/backwards/both) + animation 简写（**P7 升级 `<single-animation>#` 多组**：组间逗号分段、组内 `<time>` 首现=duration 次现=delay、关键字先行消歧、首个非关键字 ident=名、组内缺省部件回 CSS 初始、空组整条拒绝；单组用法逐位兼容）；**描述符列表化（P7，T-扩展）**——七描述符恒产列表变体（AnimationNameList/AnimationTimeList/AnimationIterationList/AnimationTimingList/AnimationDirectionList/AnimationFillModeList；旧单值变体保留兼容读取端）；采样——engine.frame 每帧 apply_animations（级联后、布局前覆写）：**多组循环（P7）**组数=name 列表长、各描述符按 i%len 循环补齐、逐组独立采样后组胜同槽覆写、结束无填充组逐槽恢复底层值、underlying 快照节点级共享（首组捕获级联值）、全组结束清副本；fill 语义（未开始 backwards/both、结束 forwards/both，否则回底层值）、方向折叠、缓动按关键帧段施加、插值=lerp_decl（颜色 pick_scheme 终结后 sRGBA 直排混合、长度同变体线性跨单位离散、transform 同名函数逐参、圆角/origin 双组件、不可插值离散取段进度 0.5 界）；**结束恢复底层值（P1-5 补齐）**——已结束且无 forwards/both 填充时写回 anim_underlying 底层值副本（原「不覆写」残留最后采样值；首见快照、外部重算自动刷新、节点移除三处卫生同步）；steps 语义修正（P1-5：jump-end=⌊p·n⌋/n，原分支写反走 ceil）；锁定测试 keyframes_parse + keyframes_animation_sampling（0/0.25/0.5/1 插值、fill both 端点保持、无 fill 回底层）+ P7 四件（animation_descriptor_lists_parse / animation 简写两组缺省物化 / animation_multi_groups_parallel 双组并行三时刻 / animation_group_descriptor_cycling 循环补齐）
- CSS Transitions transition-* 全量（P1-5 批已落地，ADR-0032）：①属性面——transition-property（None/All/Ident 列表；custom-ident 合法保留=未知名不产生过渡不报错）、duration（拒负）/delay（允负快进）、timing-function（复用 TimingFn）、behavior（Normal/AllowDiscrete）+ 简写 `<single-transition>#`（顺序自由、首 time=duration 次=delay、三 time/负 duration 报错整条忽略）；②reconciliation（restyle 提交点 styles.insert 前，css-transitions-2 §3 精化序）：新值==活动过渡 to → 保持运行（容器收敛环保护）→ 新级联==生效值 → 取消 → combined≤0 → 取消/不启动 → 重定向（from=当前插值中间值）→ 可插值探针（离散对需 allow-discrete）→ 动画覆盖槽抑制启动（偏差在案：动画层高于过渡层）；描述符槽自身不可过渡；首帧无 before-change 不过渡；③采样挂点 frame()（restyle 后、apply_animations 前——animation 层高于 transition 层同槽覆写）：延迟段写 from、进度≥1 写 to 移除、离散 50% 翻转；start_time=变更提交帧时刻；过渡表空=稳态零写入（frame() 逐位一致）；锁定测试 tests/transition.rs 19 件（解析 5+驱动 7+none/all+steps 采样点+离散双语义+动画覆盖+稳态幂等+文本 color 过渡）
- bidi 与多 run 混排（第五批⑲已落地评估）：parley 0.11 内建 Unicode bidi 算法（analysis 以 base_level=None 调 resolve）——引擎文本栈零成本继承混排重排（first-strong 基向、嵌段层级、簇视觉序经 line items 直出）；锁定测试 bidi_mixed_direction_first_strong（混排串 LTR+RTL 双 run 划分、纯 RTL 串全 run 奇数层级、advance 不塌缩，DejaVu 覆盖希伯来字形）；残余偏差：CSS direction 属性无显式基向管道（parley 0.11 硬编码 first-strong，上游暴露 builder 级基向后接入）
- text-align（消费已落地——第五批⑳：折行后 parley align，justify 实际消费，测量不变宽）；filter 触发的 stacking context 已落地（第四批④），效果本体已落地（P2，ADR-0031：严格文法+PushFilter/PopFilter 层对+BackdropFilter op+soft 全函数管线+vello opacity 直映，见偏差核对 filter 条）；clip-path 已升实裁剪（F3c，见 T0 段条目）；容器查询已落地（阶段2③，见 T0 段条目）
- T1 用例覆盖（二期⑥）：conformance 新增 2 用例入 Numeric 通道（合计 18，零 xfail）——①keyframes-layout（动画布局不变性：.a/.b 挂 animation 简写+@keyframes（背景色/透明度插值），golden 与静态布局逐位一致，实证 apply_animations 每帧采样不扰动布局；transform 动画刻意不用——getBoundingClientRect 含变换、像素截图时机非确定，动画终值像素验收待 ⑦）；②bidi-mixed（混排一致性：相对容器 600×200 内 3 个 absolute 文本叶——拉丁+希伯来混排串/纯 RTL 串/声明宽 140px 折行串，宽 203.1/42.4/140、高 19/19/38 与 Chromium 153 golden 零超差，parley first-strong 基向两侧一致）；随用例修复两缺陷：restyle 的 absolute 宽例外改查本地 cs（原 is_absolute 查 self.styles 而本节点 cs 到函数尾才 insert 恒 None——auto 宽绝对文本叶测量宽丢失，taffy 绝对布局对 auto 叶无测量回退得 0）与 T5d 换行宽声明优先（原一律用 shrink 夹紧宽，声明 140px 被无界测量 233px 盖过漏折行；percent 基准暂取夹紧宽 v1 近似）；引擎锁定测试 absolute_text_leaf_sizes（混排/RTL 测量宽正负号回归+声明宽折行高）
- 第二软 Sink 转正：Text + Transform（二期⑦）——style-engine-soft 从 FillRect/Gradient/Shadow/Image/Border/Clip/Opacity/Scroll 八算子扩至全算子：①Transform 逆映射光栅化（DisplayList 级组合矩阵 cur=[f32;6]+栈消费 PushTransform（cur=M∘cur）/PushScroll（平移折叠）/Pop*；dest 包围盒=变换后角点，逐像素中心逆仿射回源空间做矩形/圆角/clip 判定，color_at 源坐标采样——旋转 90/180° 轴对齐整数边缘无 AA，与 Chromium 逐位一致，transform-pixel 实测 diff 0/120000）；②Text 最小 TrueType 光栅化（soft::ttf 纯 std：sfnt/head/hhea/maxp/hmtx/loca/glyf 简单+复合字形（平移+双轴缩放，点匹配与 2×2 按单位阵近似记录偏差）、cmap format 4 BMP、二次曲线 16 段折线化（2048 upm 偏差≲0.06px@16px）、4×4 超采样 16 级 AA；FontBank 族名→字节注入（零副作用原则，宿主推字体），render_with_fonts 接管 Text op（基线=y+(行高−(asc+desc))/2+asc，normal 行高=round(asc)+round(desc) 与引擎㉔/Chromium 同式）；v1 边界：无合成粗斜体/kerning/spans/换行/align 消费）；③Pixel 通道转正新增 3 用例（合计 6）——transform-pixel（rotate(180)×2 非对称渐变盒+非对称 border-top 宽盒，预算 0.001 实测逐位一致）、text-pixel（DejaVu 16/20px 单行短词，预算 0.01 实测 0.00585）、table-basic 并入（预算 0.005 实测 0.002875）；④随用例修复引擎缺陷：resolve_transform_affine 漏传盒偏移——op 坐标为视口系而 origin 按 (w/2,h/2) 解析，offset 盒绕错中心旋转、子树整体错位/消失（既有单测盒在 (0,0) 故盲），paint.rs paint_node 改传 (x,y)、origin=(x,y)+盒内百分比基点，锁定测试 transform_origin_includes_box_offset（盒 (10,20,100,50) rotate(180) → e=120/f=90）

## T2 — 暂缓（显式排除项清单；每项记录当前行为与重估条件）

> T2 定位：**有意不进 MVP 的特性**（区别于已落地项的 B/C 级偏差）。每项记录：当前行为（引擎真实状态，勿臆测）→ 重估条件（何时/依赖什么才值得做）。清单随特性落地增删。

- sticky / fixed 定位：已于 A4 落地定位语义（css-position-3 / ADR-0006）——
  fixed=视口锚定（taffy Absolute + settle_absolute_anchors tcb 候选重挂）、
  sticky=in-flow（taffy Relative、inset 归 auto），解析与布局均在场，
  见 T0 段 position: fixed / sticky 条。残余暂缓 = sticky 粘滞约束的滚动
  运行时吸附：top/right/bottom/left 粘滞量由宿主按滚动偏移施加（引擎经
  scroll_offsets 绘制期平移，不自动吸附，ADR-0005 偏移归宿主边界不变）。
  重估条件：宿主侧需要引擎内建吸附时（滚动容器注册 + 偏移驱动的重布局钩子）。
- float 浮动：已于 E4 落地（css-position-3 / ADR-0019）——解析+
  settle_floats 引擎侧结算，见 T0 段 float/clear 条；子树局部/带宽
  常量化等偏差见该条诚实边界。
- 打印 / @media print / 分页：媒体查询求值按视口（screen 语义），无 page box 与 fragmentation。重估条件：分页布局需求出现（page 模型+跨页断行是独立工程量级）。
- 3D 变换（matrix3d/translate3d/rotate3d/scale3d/perspective 等，ADR-0009）：解析期 warn 拒绝、声明丢弃=none——vello 0.11（P9-8 升级后同 0.10）纯 2D 仿射管线（[f32;6]）。重估条件：sink 升级 3D 或换渲染后端。
- filter / backdrop-filter 效果本体：**已于 P2 落地（ADR-0031）**——
  十函数枚举 `FilterFn` 严格解析（钳位不拒绝、`url()`/未知函数整条拒绝、
  `none` 为有效覆盖声明）、`PushFilter/PopFilter` 层对 + `BackdropFilter`
  即时 op（层序 opacity(filter(子树))）；soft sink 原生逐像素全函数管线
  （颜色矩阵族 sRGB 表/blur σ=r/2/drop-shadow 影先源后），vello 纯 opacity
  链直映、其余 warn-once 恒等层降级（见 T0 段混合层后滤镜条）。残余 B 级：
  vello 像素效果等待 vello filter/fragment brush 原语；层内 backdrop 域
  近似主画布区域。
- @container style() 查询：未支持——依赖容器自定义属性计算值快照与样式重算反馈回路。（其伴生条目 cqw/cqh/cqi/cqb 单位已于 A9 落地：延迟结算+容器基值结算期现查+small viewport 回落，见 T0 A9 条。）重估条件：容器 custom property 快照管线 + 依赖失效模型。
- @layer 跨样式表：**主体已落地（B2）**——文档全局层树（各表层树附着
  期经 remap_layers_to_doc 重映射并入 doc_layers，跨表同名层 = 同层、
  先现序 = 文档序，LayerRegistry::merge 承接表间并树）。残余 = UA 样式
  表内容仍空缺（Origin::UserAgent 档语义完备；`add_stylesheet` 仅
  author 起源——UA 表注入 API 的重估条件 = UA 默认样式需求出现）。
- 条件组规则内的 @layer：@media 块内 @layer 无条件登记层树（条件求值
  过滤规则命中而非层登记）；B2 复核完成：@supports 块形同约定（子解析
  继承层上下文，块内层登记无条件）。
- revert 与内联声明：内联与 author 表同桶（Author important/normal），
  revert 语义按"严格更低 origin-importance 档"回滚，不会回滚到
  简写展开前的分量（展开发生在解析期）。
- var() 代换简写结果恰为宽关键字：按"各长手各得该语义"物化——conformance
  对拍已完成（var-wide-keywords 用例对 Chromium 153 golden 逐盒 0.5px
  一致后转正，当前 35 用例零 xfail）。
- 多列 fragmentation 深化：break-inside:auto 内容跨列分裂、column-span 整数列跨。当前 avoid/整列装箱语义（B 级在案）。重估条件：fragmentainer 模型（内容分裂是通用断行工程，table/多列共享）。
- 书写模式 fragmentation（writing-mode / vertical-rl 等，css-writing-modes-4）：
  未支持——`writing-mode` 属性未入文法（无槽位，声明按未知名拒绝+告警）；
  方向语义仅 `direction`/`unicode-bidi`（T0 A8）与逻辑→物理映射
  （css-writing-modes-4 映射表为逻辑属性基准），横排 IFC 单书写模式。
  重估条件：宿主需要纵向排版——writing-mode 槽位+正交流（正交轴 IFC）
  是独立工程量级，牵动 taffy 主轴映射、文本栈与 fragmentation 模型
  （与多列 fragmentation 深化共享 fragmentainer 前提）。
- 多动画组：**已落地（P7 批，见 T1 段 @keyframes 条）**——animation 简写
  `<single-animation>#` 多组、七描述符列表化（T-扩展）、组数=name 列表长
  描述符按 i%len 循环补齐、逐组采样后组胜同槽覆写、结束组逐槽恢复底层。
- direction / unicode-bidi 显式基向：**属性层已落地（A8：解析/级联/继承/计算值经 `ComputedStyle::direction()/unicode_bidi()` 可读）**——文本栈基向仍硬编码 first-strong（parley 0.11 上游限制，属性值暂不改变折行方向）。重估条件：parley 暴露 builder 级基向 API（届时以计算 direction 接管基向）。
- text-decoration（underline/overline/line-through + 装饰色/线型）：**已落地
  （F2 D4，ADR-0022——见 T0 段 text-decoration 四长手+简写条）**，本条仅
  存档；残余 B 级（线位近似/blur）见该条。
- hyphens 断词效果本体：**属性层已落地（F4，ADR-0028——HyphensKind 解析/
  继承/计算值可读）**；断词效果三值 v1 行为一致（显式断点由 parley UAX 14
  分段≈manual 默认语义、auto 无词典、none 无法抑制上游断点）。重估条件：
  parley 暴露连字/断点覆盖 API，或引擎侧软连字预处理管线。
- 其余未支持 at-rule（@namespace/@page 等）：解析跳过+report 告警（既有）。@import/@supports 已于 B2 落地（见 T0 B2 条：宿主取 URL 契约 + 静态能力求值）。重估条件：按宿主需求。
- 字体描述符深化：**@font-face 登记表已落地（F3d，ADR-0026——见 T0 段
  条目⑤）**——全描述符解析入 `engine.font_faces()`（unicode-range/
  font-display/font-stretch/features/variations/ascent·descent·
  line-gap-override 均记录）；**残余**：unicode-range 记录但不驱动字体
  fallback 选择（需子集化匹配管线）、ascent/descent/line-gap-override
  与 src format 匹配度校验未消费、local() 仅记录（FontBank 无本地字体
  探测）；font-display 与描述符级 features/variations、format() 提示
  亦仅登记（无宿主消费语义定义）；size-adjust 与 src tech() 未解析
  （宽容跳过，B）。重估条件：多字重/子集化字体需求或宿主接注册表驱动
  的字体选择。
- 背景图片 repeat / size / position：**已落地（F3b，ADR-0024——见 T0 段背景全集条）**；残余 B 级：space/round 平铺按重复近似、background-attachment: local 滚动联动=宿主滚动运行时消费（ADR-0006 边界）、clip: text 降级 border-box。
- border-image 与字体深化（F3d，ADR-0026）**已落地（见 T0 段条目）**；残余 B 级：round/space 切片平铺为改进近似（Round 等分拉伸/Space 首尾贴边 gap 均摊，非 CSS 精确「最近整数 tile 重排」）、圆角与九宫格边不相交裁剪（圆角不切 9 片）、渐变源 repeat→stretch；small-caps 合成=uppercase+0.8 缩放（petite 回退 small，真 smcp 字形未探测——fontique 无协商钩子）、font-variant 其余轴（numeric/ligature/east-asian）未消费；span 级 stretch/word-spacing/features/variations 仅基样式生效（行高/字距已由 P9-5 收口，见 span 级深化条）。
- git-lfs（第五批㉗暂缓，记录重估条件）：字体基准资产（NotoSansSC.ttf/DejaVuSans.ttf 等 demo+conformance 双侧共享）暂以普通 git 对象入库；重估条件=仓库二进制总量显著增长（如新增多字重字体族/图片基准资产）或克隆体积成为协作痛点——届时迁 LFS 需同步改 CI checkout（lfs: true）与 dumper 路径无差（file 语义不变）
### 已落地存档（原 T2 暂缓项兑现后移档备查）

- DisplayList serde 序列化（阶段3 决策）【已兑现（F3a2，paint 模块 serde feature gate）】：原决策 = `DisplayList`/`PaintOp` v1 不提供 serde derive——不引入默认 serde 依赖（C4 依赖纪律），`PaintOp` 保持中立公共枚举（`#[non_exhaustive]`）；重估条件=出现跨进程合成、录制回放或可视化调试工具链需求时**优先独立 feature gate**。兑现形态即该方案：`paint_dump` 模块（feature="serde" 可选依赖）提供 PaintOp 类型化 dump 镜像与 `to_dump()`/`to_display_list()` 往返（未知 op/单位/枚举名降级跳过），serde_json 仅 dev-dep；锁定 tests/css_serde_dump.rs（见 T0 段 DisplayList serde 投影条）。

## MVP 实现偏差核对（T4/T6 落地后现状；分级冻结见各条【】标注）

> 分级（第五批冻结）：**A 级** = 影响布局/绘制正确性，必须解决（标注待办票号）；**B 级** = 视觉细节，豁免但需文档；**C 级** = 近似/性能取舍，可保留。

- 阴影【已落地（第五批⑩；模糊/内阴影=sink 近似，像素校准重估条件=阴影专项 pixel 用例入 conformance 预算）】：inset 关键字支持（前置/尾随两形，重复 inset 整条容错丢弃）；spread（第四长度）入 op；绘制序=外阴影先于背景、内阴影于背景之上边框之下（CSS 序）；`PaintOp::Shadow` 携带 blur/spread/inset。渲染（vello 0.11 同 0.10 无内置高斯模糊）：模糊=多重同心圆环近似（N=6，单环 alpha=1−(1−a)^(1/N) 使 N 层复合恰为 a——同心叠涂复合公式精确、边缘自然衰减）；内阴影=盒裁剪层内反转填充（EvenOdd：盒路径−影框路径）+ 影框逐环外扩近似模糊；spread 外扩/内缩影框，圆角随扩张同步增长。锁定测试 box_shadow_inset_and_spread（解析）+ shadow_inset_and_spread_op（op 载荷）。soft sink 真 blur（P1-4 批）：Shadow op 纯平移矩阵下走形状 alpha 遮罩（outset=外扩 spread 圆角矩形、圆角随 spread 增缩钳半宽；inset=盒内减平移扩展矩形、合成期钳回盒内）+3×可分离盒模糊（σ=blur/2、盒宽 ⌊√(4σ²+1)⌉ 奇数、u32 窗口取整逐位确定）+pad=⌈3σ⌉∩画布+着色 src-over 合成；旋转/缩放矩阵回退平移矩形近似（B 级在案）；vello 侧维持多重圆环近似（上文的 sink 近似边界仅对 vello 成立）。
- 椭圆圆角【已落地（第五批⑪）】：border-radius 斜杠文法 `<lp>{1,4} [ '/' <lp>{1,4} ]?`（横/纵分组各按 1-4 展开 tl tr br bl）与长手 `<lp>{1,2}`（第二值=纵向半径，缺省=横向=圆形角）；数据链=DeclValue::Radius(横, 纵) → resolve_radius [f32; 8]（序 tl.x tl.y tr.x tr.y br.x br.y bl.x bl.y）→ FillRect/Gradient/Shadow/Border/PushClip 五类 op 全部携带；sink 以 kappa（4/3·tan(π/8)）cubic 逼近四分之一椭圆构建 BezPath（kurbo RoundedRect 退役），并按 CSS 重叠规则等比缩放（任一边上相邻两角半径和超过边长时全组乘 f）与负半径截断；边框条角部暂以横向半径作圆形角近似（完整椭圆边框条后置）；锁定测试 border_radius_slash_elliptical（解析）+ elliptical_radius_pairs_resolved（op 载荷），像素目验重估条件=椭圆角专项 pixel 用例入 conformance 预算
- 边框【方角对角线二分=已修复（第四批⑤）；不等宽圆角弧起点=B·豁免（重估条件=不等宽圆角专项 pixel 用例入 conformance 预算）】：四边独立（`PaintOp::Border` 携带每边 `BorderSide{width,style,color}`，none/0 宽边由 sink 忽略）；solid 与 dashed/dotted 同走「角弧+直线」中心线描边（圆角弧三次贝塞尔近似），角弧按顺时针归属（TL→top、TR→right、BR→bottom、BL→left）；方角（radius≈0）角部 = 对角线二分（第四批⑤：外角→内角对角线把角部方块分给相邻两边、单边存在整块归该边、同色一次填充——消除旧「全边长直线交叉」的半透明双重着色与「后画方」角色偏差，四色快照目验通过）；**P4 D1（ADR-0037）：直角框 dashed/dotted 按边拆 FillRect 序列**（Dashed 段 2t 步进 3t 首对齐末段不足不画、Dotted 圆点直径 t 中心距 2t 方形近似 B 级；圆角框含花式线型整框退 Solid——弧上虚线 B 级；outline 通道复用自动受益）；残余偏差：不等宽圆角的弧起点不随邻边带宽调整（角部可能有细缝/重叠）。
- 渐变【rx≠ry 画刷缩放=B·豁免｜几何已校准（第五批⑫：radial_ellipse_geometry_calibrated 全组公式锁定，像素校验重估条件=radial 专项 pixel 用例入 conformance 预算）；线性画刷原点=已修复（第四批⑤）】：radial 语义完整（T4c：`circle|ellipse` + `closest/farthest-side|corner` / 显式半径 + `at <position>`，paint 层按盒子解析为绝对 center/r；ellipse rx≠ry 由 sink 画刷 x 向缩放近似；farthest-corner 公式第四批⑤复核=css-images-3 一致——circle=最远角距离、ellipse=fx·√2/fy·√2（fx/fy=圆心到最远边距离），偏心 circle farthest-corner 用例锁公式；第五批⑫新增全组校准测试——两形状×四关键字在居中/偏心两中心下的 rx/ry 与绝对锚点全数断言=spec 一致）；stop 位置=解析不夹取、用值期归 sink 归一（P1-0 修：显式 px/% 停点按渐变线长折算 vello 0..1 offset——px/线长、%夹取 [0,1]；css-images-3 §4.5.2 逆序停点单调夹取三 sink 同款；其余单位 em/rem/cq 退化为自动均布=B·豁免）；**P9-1a 停点文法补齐（ADR-0038）**：色彩提示 `red, 50%, blue`（任意序位置 `25% red`；css-images-3）与 css-images-4 双位置 `red 10% 90%`（=同色两停点 desugar）入文法，`Gradient.hints`（`GradientHint{after_stop, position}`）全链透传（ComputedStyle→PaintOp→serde dump/load 双向），sink 侧经核心共享 `apply_gradient_hints` 展开为「位置=提示点、色=前后停点中点」的合成停点（=css-images-3 提示语义的精确等价形）；无上下文单位提示（em/rem/cq）sink 侧整体丢弃=线性回退（B·豁免，与 em 停点同约定）；提示位置经 §4.5.2 邻域 clamp 保证单调；解析拒绝=首停点前提示/尾随提示/双位置缺色/单停点；停点缺省均布上移核心单源 `distribute_stop_positions`（首 0 末 1、缺位段邻点间均布、逆序抬升）——修复 soft 旧前向填充把中段无位停点塌缩到前一停位的偏差（red,yellow,blue 曾渲染为黄→蓝，红带消失），vello 同步删本地重复实现；停点非 Absolute 色防御分支两 sink 统一为不透明黑（旧 soft=透明黑、vello=不透明黑相反；引擎契约=发射前 `resolve_color` 已终结全部停点色，该分支为防御路径）；锁定测试 css_images.rs 十件（hint 解析/任意序/双位置/四拒绝/DisplayList 透传/核心均布语义/提示中点色）+ soft 均布修复像素锁 + soft 提示弯曲插值像素锁 + soft 非 Absolute 防御锁 + vello 提示展开表测试；插值色空间 sRGB；线性渐变画刷中心第四批⑤修复补入盒原点（此前漏加 (x,y)，非原点盒采样区错位——与 radial 含原点不对称暴露）。
- 相对字重【P9-1b（ADR-0039）已落地】：`font-weight: bolder|lighter`
  入文法（`DeclValue::RelativeFontWeight(bool)`），级联物化期（compute_node_from_cascade
  步骤 5）按父计算权重经核心单源 `relative_font_weight`（css-fonts-4
  §2.2.1 图表：w<100→bolder 400/lighter 不变；100≤w<350→400/100；
  350≤w<550→700/100；550≤w<750→900/400；750≤w<900→900/700；900≤w→
  不变/700）终结为 `Number`——计算值恒为绝对权重，消费者/过渡层/
  lerp 零感知；根节点无父以初始 400 为基；继承链每层重复解析（子代
  继承的是父已解析的绝对值）。UA 表 b/strong=bold 同步改 bolder（400
  父下等值、非 400 父下语义正确）。锁定：computed.rs
  relative_font_weight_table_edges（图表逐行边界）+
  bolder_lighter_parse_and_materialize_chain（根/700→900 链/lighter
  链）+ font_weight_number_still_absolute_and_rejects_garbage +
  engine.rs ua_bolder_semantics_and_author_override。
- 相对字号【P9-1c（ADR-0039 附）已落地】：`font-size: larger|smaller`
  入文法（`DeclValue::RelativeFontSize(bool)`），级联物化期
  （compute_node_from_cascade 步骤 4b，em→px 之后、字重之前）按父
  计算字号经核心单源 `relative_font_size` 终结为 `Len(Px)`——父字号
  恰为绝对字号表值时步进一格（端点钳制 xx-small/xxx-large 不变）、
  非表值按 1.2 比例缩放（css-fonts-4 `<<relative-size>>` 的 may 语气
  允许两实现，表步进优先与 Chromium 对齐）；根节点无父以初始 16px
  为基；继承链每层重复解析。绝对字号 px 表（9/10/13/16/18/24/32/48）
  升公共单源 `ABSOLUTE_FONT_SIZES_PX` + `ABSOLUTE_FONT_SIZE_NAMES`
  （parse_font_size 与步进共用，物化漂移面消除）。UA 表 small/big=
  绝对关键字同步改 smaller/larger（16px medium 父下同值 13/18，
  其余父值随父缩放）。锁定：computed.rs
  relative_font_size_table_steps_and_ratio_fallback +
  larger_smaller_parse_and_materialize_chain +
  font_size_number_and_keyword_still_absolute +
  engine.rs ua_small_big_relative_font_size。
- font / grid / grid-template 简写【P9-2（ADR-0040）已落地】：三简写解析期
  展开（shorthand_exists/shorthand_longhands/expand_shorthand 三表同步；
  var() 挂起路径自动走通——PendingShorthand 代换后统一经 expand_shorthand）。
  `font`（css-fonts-4 §3.7）：`[<'font-style'> || <font-variant-css2> ||
  <'font-weight'> || <font-width-css3>]? <'font-size'> [/ <'line-height'>]?
  <'font-family'>`——前导 || 组任意序各至多一次（`normal` 三处文法均接受，
  幂等置初始）；字宽分量仅九关键字（百分比仅长手）；size 接受相对关键字
  （larger/smaller，P9-1c）、weight 接受 bolder/lighter（P9-1b）；缺 family
  或尾随垃圾整条无效。reset 集=引擎 font 长手全集 9 项（Style/VariantCaps/
  Weight/Stretch/Size/LineHeight/Family/Features/Variations）。系统 UI 字体
  关键字（caption/icon/menu/message-box/small-caption/status-bar）=全长手
  初始展开（**B·豁免**：无 OS UI 字体映射；重估条件：宿主字体桥接）。
  `grid`（css-grid-1 §7.6）三形：轨道形 `rows / cols`、areas 形
  `[线名? string 轨? 线名?]+ [/ 列表]?`（行轨缺省 Auto、前后线名归槽、
  矩形性校验复用 validate_area_rows 单源）、auto-flow 形两向（rows /
  auto-flow <auto-rows>? 与 auto-flow <auto-cols>? / cols）；reset 集 10
  长手（模板三 + auto 两 + flow + 放置四）。`grid-template`（§7.3）独立
  简写=none | 轨道形 | areas 形，reset 仅模板三长手。**dense 关键字拒绝**
  （简写与 grid-auto-flow 长手一致，GridAutoFlowKind 无 Dense、稀疏自动
  放置算法，接受而忽略将静默错排=B 级在案）。锁定：decl.rs
  font_shorthand_full_components_and_resets +
  font_shorthand_system_keyword_resets_all +
  grid_shorthand_track_and_auto_flow_forms +
  grid_shorthand_areas_form_and_none_reset +
  shorthand_longhands_match_expand（10 新代表值）+ tests/css_grid_areas.rs
  grid_shorthand_via_var_suspension / grid_shorthand_track_form_direct /
  grid_shorthand_auto_flow_rows_form + tests/css_text.rs
  font_shorthand_via_var_suspension / font_shorthand_direct_and_resets。
- 列表闭环【P9-3（ADR-0041）已落地】：值面三物理槽 ListStyleType/
  ListStylePosition/ListStyleImage（slot 173/174/175，SLOT_COUNT 183）+
  Display::ListItem + `list-style` 简写第 40 项（none 二义消解
  css-lists-3 §3.6：none 归未设分量、`none disc url(b)` 语法错误）；
  ::marker 伪节点（key=(host,2)、首子位、::before 之前，§3.1）无条件
  创建（any_list_item keep-alive 门控），作者 `li::marker{color/font-size}`
  经 originating_element 既有通路生效；**标记渲染=绘制层合成**（D3
  redesign——IFC 不合并宿主文本与子伪盒，marker taffy 恒隐藏，paint 层于
  宿主首行内容左缘合成 Text op、宿主文本 x+=marker 前进宽、
  leaf_wrap_width 让位；inside 语义天然成立，**outside≈inside=B 级豁免**，
  重估条件=IFC 重构）；内容算法 §3.2 序（作者 content>image>type>none，
  eval_marker_text）；隐式 list-item 计数器（§4.6，display:list-item 自动
  +1，counter-reset: list-item 可用）；list-style-type 未知名使用期回退
  decimal（css-counter-styles-3 §2），Str 字面形无 affixes；
  marker_text=表示+prefix+suffix（counter_format 单源，P8 层复用）。
  list-style-image=向 marker 注入 background-image+1em 方声明近似
  （**B 级**：首帧无图/尺寸非固有；image 在场抑制文本标记）。
  UA 表：li{display:list-item}、ul disc/ul ul circle/ul ul ul square/
  ol decimal（Chromium 对齐）；不注入 padding-inline-start:40px（范围
  界定）；white-space:pre 声明注入 marker（UA 义务，作者不可覆盖=
  **B·豁免**）。运行收集器收窄（D7）：ListItem/Table 系带文本子节点
  终止行内运行（块堆叠修复，CSS 2.1 §9.2.1）；Block+文本匿名行内叶
  参与契约不变。锁定：tests/css_lists.rs 9 件（ol 数字序/ul disc+嵌套
  circle/none 抑制/Str 字面/作者 marker 色/display:list-item div+
  counter-reset/inside 首行几何/两帧幂等≡differential/非列表项抑制）+
  decl.rs cases 表 7 代表值。
- 容器查询失效收窄【P9-4 已落地】：增量门由「有 @container 规则即全量
  restyle」收窄为「有规则**且** `container_sizes` 快照表非空」——快照表每帧
  全量重建，表空=上帧无 container-type 元素=容器规则必然不命中（无资格
  容器 → 查询 unknown → 规则不适用）→ 增量子树重算安全；新容器出现经
  record_container_sizes→changed→收敛环全量 pass 以新鲜快照重匹配（与
  容器尺寸变化同一收敛通道，无新增失效标）；restyle() 结算缓存清空面收窄
  评估后 **defer F2**（增量布局的缓存版，见 INVALIDATION.md §5）。锁定：
  tests/css_container_invalidation.rs 4 件（无容器增量路径/容器后出现收敛/
  容器移除失配/容器尺寸变化同帧重匹配）+ differential 两表随机覆盖。
- span 级行高/字距【P9-5（ADR-0042）已落地】：span 覆盖样式经 parley
  ranged push 进测量与三 sink——行高仅 span 计算值 ≠ normal 时推 ranged
  Absolute（parley 0.11 LineHeight 无 Normal 变体；显式 normal 回退基默认
  =B·豁免，重估条件=parley 提供 normal 语义）；字距恒推 ranged（显式 0
  覆盖继承非零基值=精确语义）。PaintOp::Text 的 TextSpanPaint 增加
  letter_spacing/line_height 终结值（serde dump 同步、旧 dump 缺字段兼
  容）；vello 绘制与测量同规则推 StyleProperty（折行一致）；soft
  text_device_polys 逐字符字距取 span 覆盖（span 行高不消费——soft 单行
  渲染无行盒语义，B·豁免）。锁定：tests/css_text.rs 3 件（span 字距加宽
  测量+绘制终结值、span 0 覆盖继承基值、span 行高主导行盒+normal 回退
  锁）。
- 文本【line-height/letter-spacing 消费=已修复（第四批①）；text-align=已消费（第五批⑳：PaintOp::Text 携带声明值、sink 折行后 `Layout::align`——start/end/center/left/right/justify 全语义消费，对齐宽=max_advance 与 CSS 内容盒语义一致、justify 末行起始对齐、测量不变宽故盒几何零变化；像素目验归 Pixel 批）；span 级行高/字距=已落地（P9-5，见 span 级深化条）；自定义禁则=B·豁免（上游）】：`PaintOp::Text` 经 sink `VelloTextSystem`（parley 0.11 排版 + DrawGlyphs）落字形（零副作用——系统字体禁用、字体字节由宿主双推 engine 测量/sink 绘制）；文本测量亦内置（text.rs），未推送测量的文本叶自动测量。富文本 spans（T5c-1）：`StyleNode.spans` 字节区间声明以节点基样式为 parent 复用 `compute_node` 级联求解，`PaintOp::Text.spans` 携带绘制期终结样式，sink 按 run 文本区间选色/推样式；测量按字节区间吸收 span 度量。换行（T5c-2）：white-space: normal 的自动测量文本叶在 pass1 布局后按包含块内容宽重测量并按需二次布局（包含块内容宽 = 父 border-box − 父左右 padding − 已生效 border，border-style 为 none 时宽归零、初始 medium 不计入；shrink-to-fit 父宽受无界文本影响的场景仍为近似）；宿主 `set_leaf_measure` 不参与自动重测。span 区间契约：`insert` 校验 UTF-8 字节边界/有序/不越界（无文本节点的 span 一律非法），违规返回 `ContractError::InvalidSpan`。未注册字体对应的泛族（缺省 sans-serif）测量为 0 尺寸——宿主应显式指定已注册族名或注册匹配泛族的字体。绘制侧 `PaintOp::Text.max_advance` 与测量共用同一约束保证折行一致（sink 用 `positioned_glyphs`，已含 run 偏移与基线）。**soft 端 span 消费已收口（P7）**：`text_device_polys`/`draw_text` 携带基色+spans，逐字符按字节偏移归属 span（后 span 胜同 vello rev-find），span 感知颜色/字号/族（族切换重解析、字号≠基重算 scale）；影字平移路径传真实 spans、blur>0 影子取基样式形状（B 级）；装饰线仍基样式单行近似（B 级）。属性→通路盘点（第四批）：line-height 与 letter-spacing 此前已解析入库但**无任何消费者**（无效声明）——已接入测量与绘制双通路（ComputedStyle 解析为 px，PaintOp::Text 携带 `line_height/letter_spacing`，sink 与测量同源推 StyleProperty，保证折行一致；span 级行高/字距已由 P9-5 经 ranged push 落地）；letter-spacing 的 initial 类型与解析产物不一致（Len(Px(0)) vs LenAuto(None)）已修同型；text-align 解析入库无消费者（start-only），列入 T1。离屏目验通路：`cargo run -p style-engine-demo --example snapshot`（vello `render_to_texture` → 纹理回读 → PNG；存储纹理路径要求 Rgba8Unorm，快照色彩较窗口路径偏亮属已知伪影）。字体资产：demo 内嵌 DejaVu Sans Regular/Bold（许可证见 `crates/style-engine-demo/assets/fonts/LICENSE-DejaVu.txt`）与 CJK 第二波 Noto Sans SC 可变字体（默认实例 Regular，OFL 见 `crates/style-engine-demo/assets/fonts/LICENSE-NotoSansSC.txt`）。CJK 行断行走 UAX #14 类规则（快照目验通过：混排行在表意文字边界折行；icu_segmenter 2.x 行分段器有意不加载 CJ 词典——设计取舍，调研见 DEPENDENCIES）。parley 已开 `complex-scripts`（上游 #621）：词分段获得 CJ 词典（`No segmentation model` 警告消除，≈+2MiB baked 数据），SEA 文字获词典级行断；禁则处理（kinsoku）仍不可达——parley `LineBreakOverrideFn` 仅 ASCII（上游限制，详见 DEPENDENCIES）。
- opacity / 背景图片【opacity=已实现（勘误：旧文「Opacity 未映射」系文档失同步，与下条 z-index、L12 绘制清单及快照目验矛盾）；背景图片=已落地（第五批⑨；repeat/size/position/多层=F3b 语义精化取代 MVP 拉伸）】：opacity 经 `PaintOp::PushOpacity{alpha}/PopOpacity` 层对落地；背景图契约：background-image: url() 引用（解析已支持 UnquotedUrl 与 url() 函数两形）→ 宿主 `add_image(reference, w, h, rgba)` 注册表解析（零副作用——引擎不取 URL、不解码位图格式；rgba=宿主预解码 ARGB8 直排，peniko Blob 同型 `Arc<dyn AsRef<[u8]>>` 承载）；未注册引用=tracing 告警跳过（无 op）；**渲染（F3b）=固有尺寸+repeat 平铺（tile 双循环 ceil 对齐；space/round→重复=B 级）、size cover/contain/显式 LP 消费、多层反序发射（首层最上）、origin/clip PushClip 裁剪盒、圆角非零时 clip=BorderBox 用元素圆角**；`PaintOp::Image{source_w, source_h, pixels: ImageRes}` 自足携带（DisplayList 中立载体）；锁定测试 background_image_op（首 tile 几何+tile 计数 1250+未注册跳过）（vello `push_layer` alpha，SC 触发 opacity<1 已并入带序判定，见下条）。
- z-index / 层叠【flex/grid 子项显式 z-index（无 position）=已落地（第五批㉑：显式 z ≠ auto 的 flex/grid item 与定位元素同等参与 ADR-0008 三带——负 z 进 Neg、其余进 Pos 带排序，锁定测试 flex_grid_item_z_index_orders；CSS 语义 flex/grid item 的 z-index ≠ auto 还创建 stacking context，按三带即可表达）；SC 触发全集=已落地（第四批②④ + 第五批㉒：transform / filter / clip-path / will-change 含触发属性 / isolation: isolate / mix-blend-mode ≠ normal——clip-path 已升实裁剪（F3c，见 T0 段条），其余仅触发、无效果实现）】（ADR-0008，CSS 2.1 Appendix E 简化三带）：SC 触发 = positioned 且数字 z、opacity < 1（clamp [0,1]）；带序 Neg（负 z 升序、等值树序）→ Flow（in-flow 树序）→ Pos（auto/0 树序在前、正 z 升序在后，非定位 SC 触发者键 0 树序）；SC 子树经 paint 递归天然原子。opacity 经 `PaintOp::PushOpacity{alpha}/PopOpacity` 层对（sink 用 vello `push_layer` alpha，快照目验通过）；`z-index: auto` 物化为 `ZIndex(None)`（与缺席等价、不触发 SC），数字为 `ZIndex(Some)`。残余偏差：flex/grid 子项的 z-index（无 position）不生效；transform 触发者已落地（第四批②，Pos 带键 0——层序测试覆盖树序控制组 + 快照目验）；filter 触发者已落地（第四批④：`DeclValue::Effect(bool)` 存在性语义位——`filter: none` 缺席、任意值宽容存在，函数块经 skip_block_content 递归吞咽满足 parse_nested_block 的 parse_entirely 耗尽契约；带序并入 SC 判定、不产生任何 PaintOp——测试断言 op 数与控制组一致；效果实现显式不在范围）；clip-path 已升实裁剪（F3c，ADR-0025——ClipShape 全形状解析 + PushClipPath 绘制裁剪，SC 触发=形状≠none，inset(0) 实发 PushClip/PopClip 对、效果触发测试已重基线）；will-change/isolation/mix-blend-mode 触发者已落地（第五批㉒ SC 触发全集：will-change 列表含 transform/filter/opacity/mix-blend-mode/clip-path/isolation/perspective 才置位（宽容接受任意 ident、纯语义位无提示优化）、isolation 仅 isolate 置位、mix-blend-mode 16 标准模式 + plus-lighter/darker 非 normal 置位——isolation/mix-blend-mode 已升实混合层（P1-2，见 T0 段混合层条；will-change 纯语义位不变），带序测试 + 解析语义测试×2 覆盖）。
- transform【cb 跳走=已落地（三期②，见 absolute 包含块跳走条——数值哨兵 transform-cb 的「引擎 taffy 直锚同点 (50,20)」说明随之作废：直父锚定场景行为不变，跨 static 层场景改锚 cb）；transform-origin=已落地（第五批⑬）；同节点 transform×自滚动=C·豁免】（第四批②，ADR-0009 双时机契约）：L1 解析 transform 函数列表（translate/scale/rotate/skew/matrix 及 2D 变体，单参回退；3D 函数族 warn 拒绝、声明丢弃=none；函数间逗号宽容、终止符不消费交回声明循环）；布局盒永不被变换污染（taffy 不可见，Frame/scrollable 全为非变换几何）。L2：has_transform 谓词进 absolute 可用宽 cb walk（transformed 祖先=cb，收缩夹紧红转绿 360→200）+ 数值哨兵 transform-cb 进 Numeric Channel（恒等变换零投影噪声）+ has_transform 进三期② cb 判定（absolute 跳走谓词=positioned‖transformed，与可用宽 walk 同一语义）。L3：PushTransform/PopTransform 层对（paint 期终结仿射 A=T(origin)·M·T(−origin)，origin 按 transform-origin 解析消费——第五批⑬：2D 二维子集 1~2 组件（length-percentage 或 left/center/right/top/bottom 关键字）、关键字轴归类顺序宽容（top left ≡ left top）、单值=横向在前且 top/bottom 单值横向缺省 center、第三组件（z 轴）随终止符宽容吞下不解析、百分比基=自身 border-box、无效值声明丢弃；原「固定 50% 50% v0 不解析」已替代；仿射断言测试含默认/显式 center/0 0/关键字/单值/px 双值×8 组）+ 非定位触发者进 Pos 带键 0；层栈序 transform→clip→filter→opacity（transform 组合的像素级组合已由二期⑦软 Sink 逆映射光栅化转正+transform-pixel 用例验收）。sink：vello 0.10 Scene 无变换栈 API——自维护 xforms 栈（A·B 中 B 先行）+ per-call **偏移共轭** eff(v)=offset+T·(v−offset)（形状坐标构造期已手叠偏移）、字形 run 变换=translate(offset+T·原点)∘T，恒等栈顶逐位退化回原行为；同节点 transform×自身滚动的次序近似（CSS 语义滚动在变换内，此处为外）记录在案、验收不覆盖。命中测试与滚动补偿=宿主契约（ADR-0009 §5：宿主自重建仿射求逆映射，引擎不提供助手）。
- @font-face / 字体资源【登记表已落地（F3d，ADR-0026，取代第五批⑯「静
  默跳过」契约）：全描述符解析入 `engine.font_faces()`（合并序 user→
  主表→附加表、同族后规则胜）；引擎保持 metadata-only——不做字体匹配
  决策、无 unicode-range 分片 fallback、font-display 无加载生命周期、
  度量 override 不进度量管线（残余细目见上文字体描述符深化条）；匹配
  与字节加载=宿主职责（add_font 推送、src url 文本=注册表键）；
  font-family 按字体内部家族名匹配（fontique 注册，generic 族
  sans-serif/monospace 等不在无系统环境下回退——用例须显式命名族），
  与登记表族名不交叉；golden 侧 dumper 经 data:-URL @font-face +
  document.fonts.ready、引擎侧 run_case 以同字节 add_font——双源同字形】
- 文本叶尺寸语义【已契约+已落地（第五批⑥）】（第四批快照目验发现，旧语义=盒尺寸被文本实测覆写、显式 width/height 不生效——demo tilt 卡曾现形 90×24 即该缺陷）：契约=①声明 width/height 优先于文本测量（CSS 显式尺寸胜出）；②无声明时交 taffy auto——块流文本叶拉伸到容器内容宽（与浏览器匿名块盒一致），flex/grid 子项取内容宽（固有尺寸经 LeafMeasure/min-content 供给），min/max 宽仍由 taffy 夹紧；③absolute 无声明宽 = shrink-to-fit 夹紧 clamp(min_content, cb 内容宽, max_content)（CSS 10.3.7，第三 pass 保留——restyle 期落显式测量宽保 compute #1 可用）；④测量高兜底 height:auto=内容高、声明高优先；⑤折行约束（wrap_widths/max_advance=容器内容宽）与盒宽解耦——盒宽不再夹住折行。测试 text_leaf_block_stretch_and_declared_width（拉伸 220/声明 120/内容高非零）+ absolute_text_shrinks_to_fit 断言更新（host 声明宽保留后夹紧宽=300、measures=该约束下最宽行）。
- 滚动容器【量程语义=已文档（第五批⑭；绝对定位后代并入为近似=C·豁免）】（ADR-0007）：overflow ∈ {auto（解析归一为 scroll）, scroll} 两轴独立识别；偏移归宿主（`set_scroll_offset`，引擎不夹紧——越界/回弹/阻尼语义=宿主按 `Frame.scrollable` 自行实现），量程经 `Frame.scrollable`（key → 各轴 (max_x, max_y)，px，每帧随布局重算）上报。量程定义=每轴 max(0, 内容并集在该轴超出 padding box 的幅度)：内容并集 = 滚动容器 padding box ∪ 全部流内后代 border box（按 relative 偏移后实际位置；文本叶按实测盒；三期⑥：带 transform 的后代（含祖先链复合仿射——各变换均以未变换视口系为基，复合序 `mul_affine(acc, own)` 与绘制流 cur∘M 嵌套一致，终结复用 ADR-0009 的 `resolve_transform_affine`）按变换后四角 AABB 并入、仅正向溢出（LTR 左/上越界不扩量程，Chromium 同语义；锁定测试 scroll_range_includes_transformed_child / scroll_range_nested_transform_composes / scroll_range_rotate_aabb_positive_side_only / scroll_range_transform_up_extends_nothing））；绝对定位后代以当前布局位置并入为近似（abspos 盒锚定最近 positioned 祖先而非滚动容器流，滚动后不重投影、量程不随之重算——精确 abspos 量程=C·豁免，宿主如需可自并集）；hidden/clip 仅裁剪、不上报量程；滚动条 overlay 式、宿主所有、不占布局空间；sticky 契约记录（后布局位移 pass）、实现停泊。绘制 PushClip → PushScroll{dx,dy} → PopScroll → PopClip，偏移变更不触发重排（量程计算锁定于 scrollable 上报测试 (0,190) 用例）。
- 布局映射【display:inline=IFC 行内流已落地（F1 / ADR-0021，取代第五批⑧契约化决策）：inline→Display::Inline（连续性标记/组盒）、inline-block→Display::InlineBlock（原子盒）参与块容器行打包（settle_lines，见 T0 F1 条；vertical-align 全值族已落地 P3/ADR-0034——TOP 硬编码退役、baseline 不偏移回归锁在案，见 T0 段 vertical-align 条）；inline-flex→Flex、inline-grid→Grid、inline-table→Table 仍归一并发 tracing 告警（`style_engine::css` target）而非报告失败；inline 内容=文本叶承载（T5c 通路）；第五批⑧纵向堆叠语义由 F1 行盒并排取代（契约测试重写 display_inline_line_participation_f1）；calc 百分比扁平化=A·待办⑤（独立票）→**已消除（三期③「calc 15 槽位结算」实质消除）**；margin auto 居中、box-sizing、shrink-to-fit、grid 轨道消费、margin collapse=已落地——第五批⑦评估证实 taffy 0.14 block 算法内置纵向折叠（兄弟取 max/负 margin 求和/父子穿透 strut/浮动与 clearance 机械齐全），引擎用内建 `taffy::TaffyTree` 直接获得、零额外代码；flex/grid 上下文不折叠与 CSS 一致；已知偏差（三期②随 absolute-anchor-jump 用例记录）：末子 margin-bottom 与父 margin-bottom 塌陷时 taffy 把塌陷量落在父盒上方（父.y 增大）而非父底缘之外（违 CSS 2.1 §8.3.1），用例规避中、B 级在案（重估条件=taffy 上游修复或引擎侧塌陷后处理）；测试 margin_collapse_block_siblings 块流 30（max）/flex 50（相加）双向锁定】：display:inline 缺席（统一块化）；calc 含百分比：旧「百分比基按 0 扁平化」语义已消除（A·待办⑤ 由三期③「calc 15 槽位结算」实质消除——settle_calc 以父内容盒（size−border−padding）为基解析 calc 百分比回写，width/height/flex-basis/min/max-width/height/margin 四侧/padding 四侧/column-gap/row-gap 共 15 槽位全落地，见 T0 段 calc 结算条；旧「taffy 0.14 calc 类型擦除指针 + 宿主回调求值、接入需指针所有权约定与 unsafe 面」评估已被 2026-09 上游复评修正：taffy 0.14 公开 `resolve_calc_value`）；15 槽位外的 calc 百分比长尾仍走延迟结算管线；vw/vh 已按视口解析。box-sizing（Numeric Channel box-model 用例驱动接入）：CSS 默认 content-box，taffy `BoxSizing` 直通换算（border-box 显式路径有对照用例）；边框计入布局——border 映射进 taffy border rect（style none → 0，与 used-width 语义一致），此前「边框仅绘制不占位」属语义偏差、由通道首战修正。shrink-to-fit（T5d 第三 pass 已落地）：absolute 叶按包含块可用宽夹紧——width = clamp(min_content, avail, max_content)，cb = 最近 positioned/transformed 祖先（三期②锚定跳走对齐；无 → 视口宽；近似：可用宽未扣自身 margin/静态位置），夹紧对象为内容宽（先扣自身水平 padding 与有效 border，CSS 10.3.7 约束式）；文本 min-content = `break_all_lines(Some(0.0))` 的最宽不可断原子（restyle 期随自动测量产出 `min_measures`，max-content 即无界测量）；非文本叶经 `set_leaf_intrinsic(key, min_w, min_h, max_w, max_h)` 提供固有区间，definite 首选尺寸仍走 `set_leaf_measure`（两者尺寸语义 = content-box，与 CSS width 一致）；块级流 width:auto 拉伸语义不变。合成视口根：taffy 根之上另有 ICB 节点，树根自身 margin 得以生效；taffy `location` 为父相对坐标，collect 沿树累计祖先偏移输出视口绝对坐标；margin 初始值为 0（CSS），显式 auto 才触发定宽块级盒居中。
- 滚动：`PushScroll/PopScroll` 折叠为坐标平移；滚动语义归宿主（ADR-0005）。
- Conformance（ADR-0003 双通道骨架已落地，`crates/style-engine-conformance`）：Numeric Channel——case（case.html +
- CSS Nesting 裸声明容错【P9-4 已落地=浏览器恢复语义（B3 后续）】：样式表顶层与条件组体（@media/@supports/@container/@layer 块）规则表级裸声明（`color: red;`）解析期剥除 + 告警，后续规则存活（旧行为=prelude 视图吞至下一 `{`，后续规则连带丢失）；嵌套体失败声明（非法值/未知名）同样剥除恢复，下一嵌套规则存活（cssparser「Ident→声明，失败重试限定规则」消歧并入 prelude 视图——容错探测在 prelude 视图内完成）；css-nesting 合法嵌套声明语义不变（parse_value→pending_decls）。锁测试：stylesheet.rs 内联 top_level_* 五件 + bare_declaration_inside_media_recovers_next_rule / bare_declaration_inside_condition_groups_recover / invalid_nested_declaration_recovers_next_nested_rule。已知限制：缺 `;` 的裸声明仍吞至下一 `{`（cssparser prelude 视图边界，在案）。
- :has() 失效粒度【快筛收窄=已落地（P6/ADR-0035）：宿主键快筛判定全量升级必要性——全否决走增量 subtree 通道、命中任一键升级全量（只多升级不漏升级）；remove+`:has` 结构失效已收口（存活父入脏根）；前缀组合器/复杂形态=哨兵路径保守全量】；全帧级精准脏传播（单帧内多次变更合并）=暂缓：重估条件：大文档性能画像出现（十万节点级文档 + 高频 DOM 变更场景）。 case.css + manifest.toml）单源，自动遍历 cases/（引擎盒 vs Chromium getBoundingClientRect golden 逐分量 0.5px），Playwright Chromium 生成 golden（含浏览器版本元数据）；基准版本化（第五批㉖）——golden meta.schema=1 钉格式/对比语义版本（dump_rects.py SCHEMA_VERSION 常量，13 用例全量重生成入档，diff 验证仅 schema 行新增零 rect 漂移），runner 校验不匹配即超差失败并提示重生成，重生成协议=browser_version（浏览器升级）或 schema（格式/语义变更）任一变化 → 全量重生成；现有 13 用例（block-flow-basic / box-model / box-model-borderbox / absolute-offset / cjk-kinsoku / flex-row-gap / grid-columns / relative-offset / sizing-constraints / text-wrap-latin / transform-cb / selector-structural / calc-width〔xfail〕），box-model 两用例首战即修正 box-sizing 与边框占位两处语义、grid-columns 用例暴露「grid 轨道解析入库但零消费」缺口后修复（见布局映射条），通道持续发挥缺口侦测作用；第五批㉓扩容——selector-structural（结构伪类 :first-child/:nth-child(odd)/:nth-child(3)/:last-child 驱动宽度差 + 同特异性源序级联，引擎与 Chromium 逐盒一致=⑮收敛验证）、calc-width（xfail 资产钉死含百分比 calc 布局 0 折算偏差，Chromium 400 vs 引擎 10，转正条件=自定义 LayoutPartialTree）；xfail 复核与转正（㉓复核→㉔落地）——㉔ normal 行高度量 Chromium 对齐（两遍法：探针取主 run RunMetrics，normal=round(asc)+round(desc) 不含 lineGap/leading；DejaVu 16px=12+4=16 对齐 Chromium 逐字体整数化，parley 浮点 normal=16.25，显式行高单遍不受影响）落地后 cjk-kinsoku 与 text-wrap-latin 双资产转正（96=96、64=64，此前残余=行高度量 93/96 与 65/64 + 文本叶不拉伸已由⑥契约消除），xfail 集合仅剩 calc-width；golden 重生成 `tools/dump_rects.py`，本地门禁 `run.ps1`（与 `.github/workflows/ci.yml` gate job 同步：fmt/clippy/test --all-features/hack 幂集，msrv job 钉 1.90）；文本用例字体注入（第四批⑥）：manifest.fonts 项 = "族名=相对仓库根路径"，dumper 以 data: URL @font-face 注入并等 `document.fonts.ready`，引擎 run_case 同字节 add_font——两侧字形度量同源；自定义禁则不可达属上游限制（parley `LineBreakOverrideFn` 仅 ASCII，见 DEPENDENCIES）；标准禁则断行选择两侧一致（UAX#14 已覆盖）；StateFlags/transition 用例：numeric 通道为静态对比、无状态注入与时间轴，暂不适配（记录）。Pixel Channel——PNG 连通域差异 + Error Class 0-5 启发式分类（几何/颜色/基元缺失零容忍）+ per-case manifest `[pixel]` 预算；二期④转正——manifest `[pixel]` 段声明即入通道（max_ratio 差异占比上限 + allowed 白名单，Class 3–5 恒零容忍），dumper 对声明用例追加整视口截图 golden/pixel.png（设备像素=视口×scale），引擎侧 render_case_png 与 Numeric 共享 build_case_engine——同 case 同一次布局驱动双通道（Numeric 盒与 Pixel 像素一致性即通道间同步验证），frame.paint 经软 Sink style-engine-soft 光栅化（白底=Chromium 默认画布），tests/pixel.rs 自动遍历预算校验；二期④启用 calc-width/multicol-basic/table-pixel 三用例（全整数矩形；table-pixel=table-basic 无文本变体——软 Sink v0 跳过 Text/Transform，含文本/变换用例不得声明 `[pixel]`，⑦ 转正后并入）；文本类用例（字形栅格化）：行高语义已由㉔对齐、具备接入条件，栅格化噪声仍需 per-case 预算校准后启用；**vello 像素回归（实施期 Phase 0 前置⑤）**——Pixel 通道新增 vello sink 腿（conformance tests/pixel_vello.rs）：同一 [pixel] 用例经 style-engine-vello::render_offscreen 离屏渲染 vs Chromium golden（GPU 侧首次获得像素级回归保护，先于一切绘制特性建立——像素差异可归因引擎几何 vs vello 光栅化）；预算=manifest max_ratio×5（下限 0.005）、allowed∪{1,2}（AA 边缘/文本栅格化=两套光栅化器合法分歧），Class 3–5 恒零容忍不变；跳过语义=STYLE_ENGINE_NO_GPU_PROBE=1（CI windows WARP 段错误环境级短路，同 ㉕ 探针惯例）或无适配器（Ok(None)），真适配器在场的渲染/回读失败为 Err 上浮测试失败（错误双轨）；`render_offscreen(list, text, w, h, scale, base_color)` 为 vello sink 公共 API（DisplayList+VelloTextSystem→紧密 RGBA8 行主序；scale≠1 经 `Scene::append(&sub, Some(Affine::scale))` 注入缩放——vello 0.10 Scene 无场景级 push/pop_transform），wgpu 纹理 usage=RENDER_ATTACHMENT|STORAGE_BINDING|TEXTURE_BINDING|COPY_SRC；pollster 0.4 升入 vello crate [dependencies]（库内同步阻塞执行器）；conformance 对 style-engine-vello 仅 dev-dependency（wgpu 不进主依赖树，C4 纪律）
- soft 字体格式覆盖【CFF/OTTO 不支持=B·豁免（未登记缺口补登，1.0 发布评审）】：`SoftFont::parse`（soft ttf.rs:101-107）仅收 sfntVersion==0x0001_0000 与 "true"——CFF/OTTO 返回 None，参照 sink 对该类字体无字形；vello/tiny 走 fontique/skrifa 通路不受限。conformance golden 资产全为 TTF，numeric/pixel 通道全绿掩盖此分歧。已登记 SINK-MATRIX「字体格式覆盖（sfnt）」行。重估条件：soft 补 CFF/OTTO 轮廓解析或加载即 warn-once 告警。
- @property syntax 组合子面【`|` 单组合与 ident 关键字被拒=B·豁免（未登记缺口补登）】：css-properties-values-api 合法语法 `big|small`（ident 关键字）与 `<length>|<percentage>`（单竖线）在 parse_syntax（css/property_rule.rs:134-167）返回 None→整条 @property 注册无效；已支持面=14 类型族 + `+`/`#` 单层多值（见「@property 注册」条）。重估条件：组合子文法补全（`|`/ident 字面量）。
- @container 条件 not/or【解析期拒绝=B·豁免（未登记缺口补登）】：@container 条件内 `not(...)` 与 `or` 组合未实现——解析期报错、整条规则丢弃+告警（css/stylesheet.rs `parse_container_conditions`）；已支持面=逗号分隔 OR 段、段内并置/and（见「容器查询」条）。重估条件：not/or 文法接入（嵌套条件求值器）。
- 字体度量探针 TTC【集合字体不支持=C·豁免（未登记缺口补登）】：`probe_metrics`（css/fontprobe.rs:47-49）对 ttc 集合返回 None——TTC 字体的 normal 行高探针走缺省值。重估条件：出现 TTC 基准资产或探针消费方需要时补 sfnt 头索引。
