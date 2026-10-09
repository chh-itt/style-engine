//! 样式引擎：宿主推送同步协议 + 帧驱动（restyle → taffy layout）。
//!
//! 引擎零副作用：样式表/树镜像/状态/环境/叶测量全部由宿主推送
//! （ADR-0005/0006）。`frame()` 单调推进 sync→style→layout，返回
//! 生成号戳记的布局帧（T4 起叠加 DisplayList）。

use crate::computed::{ComputedStyle, compute_node_from_cascade, compute_node_in};
use crate::css::decl::parse_inline_declarations;
use crate::css::property::{DeclValue, PropertyId, TimingFn};
use crate::css::stylesheet::{MediaEnv, Stylesheet};
use crate::error::ParseReport;
use crate::layout::map_style;
use crate::tree::{NodeId, StyleNode, StyleTree};
use std::collections::HashMap;
use std::hash::Hash;

/// B2：宿主 @import 加载器（url → CSS 文本；Send+Sync 引擎契约）。
pub type ImportLoader = Box<dyn Fn(&str) -> Option<String> + Send + Sync + 'static>;

/// B4 absolute 重挂扫描栈项：(节点, 样式父, 最近 cb 候选（None=尚无）,
/// 豁免子树, 豁免标记)。
type AbsCbStackItem = (NodeId, Option<NodeId>, Option<NodeId>, Option<NodeId>, bool);

/// E5 grid 语境线名表：模板顶层线名槽 → 线名列表（1 基线号 = 序+1）。
type GridLineMap = std::collections::BTreeMap<String, Vec<i16>>;
/// E5 grid 语境区域表：区域名 → (row0, col0, row1, col1) 0 基格界。
type GridAreaMap = std::collections::BTreeMap<String, (usize, usize, usize, usize)>;

/// C2 文本截断候选：(节点, 容器可用宽, 原文, 计算样式, span 表,
/// 行数上限 None=ellipsis)。
type TruncCandidate = (
    NodeId,
    f32,
    String,
    ComputedStyle,
    Vec<(u32, u32, ComputedStyle)>,
    Option<f32>,
);

/// 布局帧条目（border-box；坐标相对视口、滚动前）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[must_use = "布局结果被丢弃则该节点无法绘制"]
pub struct LayoutEntry<K: Copy> {
    /// 宿主节点键。
    pub key: K,
    /// 盒左缘 x（视口坐标，px，滚动前）。
    pub x: f32,
    /// 盒顶缘 y（视口坐标，px，滚动前）。
    pub y: f32,
    /// 盒宽 px（border-box）。
    pub width: f32,
    /// 盒高 px（border-box）。
    pub height: f32,
}

/// 一帧的布局与绘制结果。
#[derive(Debug, Clone)]
#[must_use = "帧结果（布局+DisplayList）被丢弃则该帧无法绘制"]
pub struct Frame<K: Copy> {
    /// 单调递增帧号。
    pub generation: u64,
    /// 布局盒（树序，含根）。
    pub boxes: Vec<LayoutEntry<K>>,
    /// 本帧绘制清单（树序基元）。
    pub paint: crate::paint::DisplayList,
    /// 滚动容器量程（ADR-0007）：key → 各轴最大滚动量（px，非滚动轴为 0）。
    /// 引擎不夹紧偏移——量程供宿主 clamp 用（零副作用、幂等）。
    pub scrollable: HashMap<K, (f32, f32)>,
}

impl<K: Copy + PartialEq> Frame<K> {
    /// 按 key 查找布局盒。
    pub fn find(&self, key: K) -> Option<&LayoutEntry<K>> {
        self.boxes.iter().find(|b| b.key == key)
    }

    /// 布局几何人读视图（F3e，ADR-0027 D1）：本帧全部布局盒按树序一行
    /// 一个（`key x y w h`）。与 [`StyleEngine::layout_tree_dump`](crate::StyleEngine::layout_tree_dump)
    /// 的结构树视图分离——Frame 有几何无树、引擎有树无帧几何，宿主按需
    /// 取用（键格式化要求 `K: Debug`）。
    pub fn boxes_dump(&self) -> String
    where
        K: std::fmt::Debug,
    {
        let mut out = format!("boxes={}\n", self.boxes.len());
        for b in &self.boxes {
            out.push_str(&format!(
                "key={:?} x={:.2} y={:.2} w={:.2} h={:.2}\n",
                b.key, b.x, b.y, b.width, b.height
            ));
        }
        out
    }
}

/// 非文本叶固有尺寸区间（T5d）：((min_w, min_h), (max_w, max_h))。
pub(crate) type IntrinsicSize = ((f32, f32), (f32, f32));

/// ③multi-column 稳态缓存：幻影列 taffy 节点与当前分配（settle_columns
/// 复用；n/colw/分配全等帧零额外布局 pass）。assignment[i] = 子节点 i
/// 所在列（树序）；usize::MAX = 重建后待分配。
#[derive(Default)]
struct MulticolState {
    phantoms: Vec<taffy::NodeId>,
    /// 三期⑤b：spanner 分段行包装（Flex Row 装 n 幻影列）——
    /// column-span:all 模式下每段一行；行模式为空。
    rows: Vec<taffy::NodeId>,
    /// 三期⑤b：spanner 分段模式（容器 Flex Column + 行包装）。
    span_mode: bool,
    /// 三期⑤b：容器孩子序签名（Seg(段序) | Spanner→usize::MAX）——
    /// 行数不变而 spanner 换位时仅重排容器孩子。
    seq_sig: Vec<usize>,
    n: usize,
    colw: f32,
    /// 每真实子件 (段序, 列序)；(usize::MAX, _) = 重建后待分配
    /// （spanner 不参与平衡，恒 MAX）。
    assignment: Vec<(usize, usize)>,
    /// 三期⑤a：断口 margin-top 截断的原值备份（taffy 层）——
    /// 不再列首 / 回退块流时恢复；restyle 时清空（map_style 全量重写
    /// 已把 margin 复位为样式真值）。
    truncated: HashMap<NodeId, MarginTopBackup>,
}

/// 断口 margin-top 备份的 Send/Sync 镜像：taffy 0.14 `LengthPercentageAuto`
/// 为 nan-boxing（内部 `*const ()`），非 `Send`/`Sync`，直接入 `HashMap`
/// 会拖垮 `StyleEngine` 的线程承诺（阶段3 静态断言）。改用显式三态镜像
/// 记录，恢复时重建；margin 不产生 calc 值（CSS 层已解析），其余 tag 不
/// 可达（按 Auto 兜底）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum MarginTopBackup {
    Length(f32),
    Percent(f32),
    Auto,
}

impl MarginTopBackup {
    fn capture(v: taffy::prelude::LengthPercentageAuto) -> Self {
        use taffy::style::CompactLength;
        let raw = v.into_raw();
        match raw.tag() {
            CompactLength::LENGTH_TAG => Self::Length(raw.value()),
            CompactLength::PERCENT_TAG => Self::Percent(raw.value()),
            _ => Self::Auto,
        }
    }

    fn restore(self) -> taffy::prelude::LengthPercentageAuto {
        match self {
            Self::Length(v) => taffy::prelude::LengthPercentageAuto::length(v),
            Self::Percent(v) => taffy::prelude::LengthPercentageAuto::percent(v),
            Self::Auto => taffy::prelude::LengthPercentageAuto::auto(),
        }
    }
}

/// taffy 0.14 `TaffyTree` 的 Send/Sync 包装：`TaffyTree` 类型面非
/// `Send`/`Sync`——内部 `taffy::Style` 携带 nan-boxing 的 `CompactLength`
/// （`*const ()`）。该指针仅在 taffy `calc` 特性下承载 calc 句柄；引擎
/// 从不构造 taffy calc 值（CSS `calc()` 在解析/计算层解析为纯 f32），
/// 所有 `CompactLength` 均为 NaN-boxed 位模式载荷（等同 f32），跨线程
/// 转移安全。上游已知限制：taffy 未为 CompactLength 提供 Send/Sync impl。
struct SendSyncTaffy(taffy::TaffyTree);

/// 增量重样式去重守卫（阶段5）：同一 restyle 调用内按 pass 计数标记
/// 已完成节点——脏根互为祖先/后代时，先走的子树覆盖后走的根，避免
/// 重复求值。全量路径用空表（全节点未标记）语义等价于无守卫。
#[derive(Default)]
struct RestyleGuard {
    done: slotmap::SecondaryMap<NodeId, u32>,
    pass: u32,
}

impl RestyleGuard {
    fn skip(&self, id: NodeId) -> bool {
        self.done.get(id) == Some(&self.pass)
    }
    fn mark(&mut self, id: NodeId) {
        self.done.insert(id, self.pass);
    }
}

impl std::ops::Deref for SendSyncTaffy {
    type Target = taffy::TaffyTree;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SendSyncTaffy {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

// SAFETY: 见类型文档——引擎不构造 taffy calc 值，`CompactLength` 的
// `*const ()` 恒为 NaN-boxed 位模式（非真实指针）；`TaffyTree` 其余
// 字段均为普通数据。
#[allow(unsafe_code)]
unsafe impl Send for SendSyncTaffy {}
#[allow(unsafe_code)]
unsafe impl Sync for SendSyncTaffy {}

/// F2（ADR-0022 D1）：结算 pass 种类——依赖声明 + 拓扑调度（替换 frame
/// 硬编码序 calc→tables→columns→floats→lines）。pass 内脏区跳过（增量
/// 布局）=F3 范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlePassKind {
    /// 延迟 calc 求值（三期①）——几何结算源头。
    Calc,
    /// 表格列模板结算。
    Tables,
    /// 多列结算。
    Columns,
    /// 浮动结算（E4）。
    Floats,
    /// IFC 行打包结算（F1）。
    Lines,
}

impl SettlePassKind {
    /// 声明依赖（被依赖者须先运行）。
    fn deps(self) -> &'static [SettlePassKind] {
        match self {
            SettlePassKind::Calc => &[],
            SettlePassKind::Tables => &[SettlePassKind::Calc],
            SettlePassKind::Columns => &[SettlePassKind::Calc],
            SettlePassKind::Floats => &[SettlePassKind::Columns],
            SettlePassKind::Lines => &[SettlePassKind::Floats],
        }
    }

    /// 拓扑调度序（稳定插入序；依赖为静态无环表——环即 bug，
    /// debug_assert 防回归）。
    fn schedule() -> Vec<SettlePassKind> {
        let all = [
            SettlePassKind::Calc,
            SettlePassKind::Tables,
            SettlePassKind::Columns,
            SettlePassKind::Floats,
            SettlePassKind::Lines,
        ];
        let mut order: Vec<SettlePassKind> = Vec::with_capacity(all.len());
        let mut remaining: Vec<SettlePassKind> = all.to_vec();
        while !remaining.is_empty() {
            let mut progressed = false;
            let mut i = 0;
            while i < remaining.len() {
                if remaining[i].deps().iter().all(|d| order.contains(d)) {
                    let p = remaining.remove(i);
                    order.push(p);
                    progressed = true;
                } else {
                    i += 1;
                }
            }
            debug_assert!(progressed, "settle pass dependency cycle");
            if !progressed {
                break;
            }
        }
        order
    }
}

/// G1（ADR-0032）：单槽活动过渡——reconciliation 启动/重定向，采样挂点
/// 逐帧推进。`from`/`to` 为过渡端点值（from 可能是重定向时刻的插值中间
/// 值）；时间量与 `self.now` 同为秒（f32，与 animation 采样一致）。
#[derive(Debug, Clone)]
struct ActiveTransition {
    /// 目标 PropertyId（写回与列表配对用；槽位由 pid.slot() 派生）。
    pid: PropertyId,
    /// 过渡起点值（启动=before-change 值；重定向=当前插值中间值）。
    from: DeclValue,
    /// 过渡终点值（restyled 后的级联值）。
    to: DeclValue,
    /// 启动/重定向时刻（帧时间，秒）。
    start_time: f32,
    /// 过渡时长（秒；≥0）。
    duration: f32,
    /// 延迟（秒；可为负=快进）。
    delay: f32,
    /// 缓动（复用 animation 文法，ADR-0032 D1）。
    easing: TimingFn,
}

/// P7-②：单个动画组的运行参数视图（anim_groups 从描述符列表
/// 循环补齐物化）。
#[derive(Clone, Debug)]
struct AnimGroupSpec {
    name: Option<String>,
    duration: f32,
    delay: f32,
    iterations: f32,
    timing: crate::css::property::TimingFn,
    direction: crate::css::property::AnimDirection,
    fill: crate::css::property::AnimFillMode,
}

/// F1（P3，ADR-0034 D2）：行内参与者 TOP 装箱暂存——settle_lines 收集、
/// flush_inline_line 消费（vertical-align 统一结算偏移+行盒扩展）。
#[cfg(feature = "text")]
struct PendingLinePart {
    tid: taffy::NodeId,
    va: crate::css::property::VerticalAlignKind,
    /// 参与者基线距（TOP 装箱内从盒顶到其基线；Box 无文本=盒高）。
    pb: f32,
    /// 参与者盒高（decl 覆盖后）。
    h: f32,
    /// TOP 装箱 inset.top 值（= 装箱行顶 − 容器 pad_top）。
    top: f32,
    /// 参与者字号 px（sub/super em 常量、middle x-height 换算）。
    font_size: f32,
    /// 参与者字体度量（middle x-height、text-top/bottom asc/desc）。
    fm: crate::css::value::FontMetrics,
}

/// P7-②：组预处理产物——（组参数，槽位→关键帧轨道）。轨道值借用
/// 样式表（'sheet 生命周期内只读）。
type PreparedAnimGroups<'a> = Vec<(
    &'a AnimGroupSpec,
    std::collections::BTreeMap<
        crate::css::property::PropertyId,
        Vec<(f32, &'a crate::css::property::DeclValue)>,
    >,
)>;

/// G1（ADR-0032）：不可过渡的描述符槽——animation-* 与 transition-*
/// 描述符自身（文档未定义其动画性；transition 描述符自指无意义）。
fn is_unanimatable_descriptor(pid: PropertyId) -> bool {
    use PropertyId as P;
    matches!(
        pid,
        P::AnimationName
            | P::AnimationDuration
            | P::AnimationDelay
            | P::AnimationIterationCount
            | P::AnimationTimingFunction
            | P::AnimationDirection
            | P::AnimationFillMode
            | P::TransitionProperty
            | P::TransitionDuration
            | P::TransitionTimingFunction
            | P::TransitionDelay
            | P::TransitionBehavior
    )
}

/// G1（ADR-0032）：过渡在时刻 now 的当前值（重定向 from 端取值）——
/// 延迟段=from；进行中=缓动求值 lerp_decl（离散对 50% 翻转）；完成=to。
fn transition_sample(t: &ActiveTransition, now: f32, dark: bool) -> DeclValue {
    use crate::css::property::lerp_decl;
    let local = now - t.start_time - t.delay;
    if local <= 0.0 || t.duration <= 0.0 {
        return t.from.clone();
    }
    let p = (local / t.duration).min(1.0);
    let eased = t.easing.sample(p);
    lerp_decl(&t.from, &t.to, eased, dark).unwrap_or_else(|| {
        if eased < 0.5 {
            t.from.clone()
        } else {
            t.to.clone()
        }
    })
}

/// 样式引擎实例。K 为宿主节点键（Copy + Eq + Hash）。
pub struct StyleEngine<K: Copy + Eq + Hash + 'static> {
    tree: StyleTree,
    sheet: Stylesheet,
    /// B1：用户起源样式表（css-cascade-5 User 层；set_user_stylesheet 注入）。
    user_sheet: Option<Stylesheet>,
    /// P5（ADR-0033）：UA 起源样式表（UserAgent 层挂点；默认 None——
    /// 缺省呈现是宿主策略，引擎只提供机制；内置表见 `builtins` 模块）。
    ua_sheet: Option<Stylesheet>,
    /// B3：当前焦点链锚点（set_focus 管理 FOCUS/FOCUS_VISIBLE/FOCUS_WITHIN
    /// 三态迁移；None = 无焦点）。
    focused_node: Option<NodeId>,
    /// B4：文档级 @property 注册表（name → 规则；sheet 变更点统一重建）。
    registered_props: std::collections::BTreeMap<String, crate::css::property_rule::PropertyRule>,
    /// F3d（ADR-0026 D4）：文档级 @font-face 登记表（合并序 = user → 主表
    /// → 附加表；同族后规则胜；sheet 变更点统一重建）。字体字节仍由宿主
    /// add_font 推送——登记表仅描述映射与筛选元数据。
    font_faces: Vec<crate::css::stylesheet::FontFaceRule>,
    /// E：文档级 @counter-style 登记表（与 @font-face 同变更点重建；合并
    /// 序 = ua → user → 主表 → 附加表，同名后规则胜——查询按此序取末条）。
    /// css-counter-styles-3 §3：登记可整体覆盖内置样式。
    counter_styles: Vec<crate::css::stylesheet::counter_style::CounterStyleRule>,
    /// C1（ADR-0015）：伪元素注册表 (origin NodeId, which) → 伪节点 NodeId
    ///（引擎 materialize_pseudos 持有；宿主镜像通道不含伪键）。
    pseudo_ids: std::collections::BTreeMap<(NodeId, u8), NodeId>,
    /// B2：主表源文本留存（rebuild_sheets 重解析用——嵌套 @import 的
    /// 未决指令只在整表重解析时存活）。
    primary_source: String,
    /// B2：附加 author 表（登记序级联，句柄化移除；源文本留存供层树
    /// 重建与导入重拼接）。
    extra_sheets: Vec<(u64, String, Stylesheet)>,
    next_sheet_id: u64,
    /// B2：文档全局层树（跨表层序唯一基准；附着序 = 先现序）。
    doc_layers: crate::css::stylesheet::LayerRegistry,
    /// B2：内存导入源（@import url → CSS 文本）。
    imports: std::collections::BTreeMap<String, String>,
    /// B2：宿主导入加载器（优先于内存源；Send+Sync 引擎契约）。
    import_loader: Option<ImportLoader>,
    media: MediaEnv,
    key_to_node: HashMap<K, NodeId>,
    node_to_key: HashMap<NodeId, K>,
    root_key: Option<K>,
    /// ADR-0010 多根：overlay 根（插入序）。首个 `insert(None)` 为文档根
    ///（`root_key`），后续 `insert(None)` 依次入列——弹窗/浮层载体。
    overlay_roots: Vec<K>,
    /// ADR-0010：top-layer 名单（进层序），绘制于全部普通根之后。
    top_layer: Vec<K>,
    /// ADR-0010：上次 sync_root_order 已应用的根序（稳态帧零操作）。
    root_order_applied: Vec<K>,
    /// 宿主推送的叶测量（文本叶，T5 前由宿主提供）。
    measures: HashMap<NodeId, (f32, f32)>,
    /// 节点滚动偏移（绘制层用；布局不消费）。
    scroll_offsets: HashMap<NodeId, (f32, f32)>,
    epoch: u64,
    generation: u64,
    dirty_struct: bool,
    dirty_style: bool,
    /// 增量重样式（阶段5）：set_declarations 脏根（子树局部重算）。
    /// 全量失效标（dirty_style）优先；容器规则在场时增量退全量。
    style_dirty_roots: Vec<NodeId>,
    /// P6（ADR-0035 D1）：`:has()` 单 compound host 快筛索引（键 = `:has`
    /// 前 compound 的类型/类/id；表变更点重建，同 rebuild_document_registries 时机）。
    /// 空 = 未建或无合格规则（判定回全量兜底）；含哨兵键（全空）= 恒升级。
    has_host_index: Vec<crate::selector::HasHostKey>,
    viewport: (f32, f32),
    scale: f32,
    now: f64,
    // taffy 镜像
    taffy: SendSyncTaffy,
    taffy_root: Option<taffy::NodeId>,
    taffy_node: HashMap<NodeId, taffy::NodeId>,
    /// ①calc 直通：延迟结算条目（restyle 重建，settle_calc 消费）。
    calc_deferred: Vec<crate::layout::DeferredCalc>,
    /// taffy 父链（结算基准 = 父内容尺寸；build_taffy_subtree 填充）。
    taffy_parent: HashMap<taffy::NodeId, taffy::NodeId>,
    /// 三期② absolute 锚定跳走：node → 重挂包含块（Some(cb)=cb≠直父；
    /// None=ICB）。settle_absolute_anchors 每帧重建，collect 消费。
    abs_cb: HashMap<NodeId, Option<NodeId>>,
    /// 三期②：taffy 结构偏离样式镜像的活跃标（重挂过 absolute 即置位；
    /// 全部归位后的下一帧清零——稳态零 absolute 页面跳过整段结构对比）。
    abs_structured: bool,
    /// ②table：display:table 节点注册表（restyle 收集，settle_tables 结算）。
    tables: Vec<NodeId>,
    /// ②table：上次结算列宽缓存（px；全等免重排——稳态帧零额外布局 pass）。
    table_cols: HashMap<NodeId, Vec<f32>>,
    /// 三期④：上次结算的单元格列位签名（(cell, 列起点0基, 列跨, 行跨)；
    /// 全等免重写）。
    table_cells: HashMap<NodeId, Vec<(NodeId, usize, usize, u32)>>,
    /// 表格 wrapper 重构：display:table 节点 → 内表 taffy 节点。CSS 2.2
    /// 表格盒模型要求块级子件提升到表盒之外（匿名块包裹），taffy 无法在
    /// 单节点内同时表达「表盒自身框」与「提升件堆叠」——故表元素的 taffy
    /// 节点降级为 wrapper（Block、填满、零 margin/padding/border），新建
    /// 内表节点承载真实表盒样式；行/组/标题等表格内部件挂内表，不当块级
    /// 子件挂 wrapper。settle_table_fixup 填充；rebuild_taffy 清空；
    /// collect 消费内表矩形。
    taffy_table_inner: HashMap<NodeId, taffy::NodeId>,
    /// 表格 wrapper 重构：裸单元格幻影行缓存——display:table-cell 直属表盒
    /// 时 settle_tables 建单格行 Grid（幻影节点，同 multicol 幻影列模式）
    /// 承接单元格；键（表节点, 行槽位）。rebuild_taffy 清空。
    table_row_anon: HashMap<(NodeId, usize), taffy::NodeId>,
    /// ③multi-column：多列容器注册表（restyle 收集，settle_columns 结算）。
    multicols: Vec<NodeId>,
    /// ③multi-column：稳态缓存（幻影列节点 + 当前分配；全等免重排）。
    multicol_state: HashMap<NodeId, MulticolState>,
    /// 三期⑤c：列规条带（相对容器 border-box 原点）——settle_column_rules
    /// 逐帧全量重建（几何随列平衡/文本换行漂移，无稳态签名可复用）。
    column_rules: HashMap<NodeId, Vec<crate::paint::ColumnRuleSeg>>,
    /// 命中几何表（F3a，ADR-0023）：最近一帧 paint 期收集；hit_test 逆序查询。
    hit_rects: Vec<crate::paint::HitRect>,
    /// 阶段2③：容器内容盒尺寸快照（上一布局 pass 的 record_container_sizes
    /// 记录；restyle 期 @container 求值消费；缺席 = unknown → 特性不命中）。
    container_sizes: HashMap<NodeId, [f32; 2]>,
    styles: HashMap<NodeId, ComputedStyle>,
    /// G1（ADR-0032）：活动过渡表——restyle 提交点 reconciliation 启动/
    /// 重定向/取消，frame() 采样挂点逐帧写入过渡中间值并清理过期条目。
    /// 空表 = 稳态零写入（无活动过渡时 frame() 与既有行为逐位一致）。
    transitions: HashMap<NodeId, Vec<ActiveTransition>>,
    /// G1（ADR-0032）：动画运行槽位的底层值副本——动画结束（fill:none）
    /// 时恢复 underlying（css-animations-1：无填充结束后回落底层值，不得
    /// 残留最后动画采样值）。条目 = (槽, 底层值, 上帧写入值)；上帧写入值
    /// 用于检测外部重算（restyle 重建 cs 后自动刷新快照）。
    anim_underlying: HashMap<NodeId, Vec<(PropertyId, DeclValue, DeclValue)>>,
    /// C4（ADR-0018）：::selection 通道样式（origin 直配；宿主读取）。
    selection_styles: HashMap<NodeId, ComputedStyle>,
    /// C4（ADR-0018）：::placeholder 通道样式（origin 直配；宿主读取）。
    placeholder_styles: HashMap<NodeId, ComputedStyle>,
    /// E4（ADR-0019）：浮动覆写触点集（还原清单——还原=map_style 重放
    /// pristine 样式，不缓存 taffy::Style 值（含 !Send/!Sync 的
    /// CheapCloneStr，会炸 assert_send_sync）。
    float_touched: std::collections::HashSet<NodeId>,
    /// F1（ADR-0021）：行内运行参与者集——settle_lines 按运行上下文宽度
    /// 测置的文本叶/盒；文本 remeasure 全宽重测豁免（否则破坏打包结果）。
    inline_run_participants: std::collections::HashSet<NodeId>,
    /// F2（ADR-0022 D2）：文本截断 override（ellipsis/line-clamp）——
    /// apply_text_truncation 生成，paint Text op 文本替换消费。
    text_overrides: std::collections::HashMap<NodeId, String>,
    /// A9：注册字体度量（add_font 时探测：族名表 → ch/ex/ic 每 em 值；
    /// 未注册族回落近似缺省 ch/ex=0.5em、ic=1em，B 级在案）。
    font_metrics: Vec<(Vec<String>, crate::css::value::FontMetrics)>,
    /// span 级样式（T5c）：NodeId → (字节起点, 字节终点, 覆盖后的 ComputedStyle)。
    span_styles: HashMap<NodeId, Vec<(u32, u32, ComputedStyle)>>,
    /// 文本叶父指针（T5c-2）：换行重测量需读包含块宽度。
    parents: HashMap<NodeId, NodeId>,
    /// 内置自动测量的文本叶集合（T5c-2）：仅这些节点参与换行重测量。
    #[cfg(feature = "text")]
    auto_text: std::collections::HashSet<NodeId>,
    /// 文本叶测量所用换行约束（T5c-2）：绘制与测量折行一致；缺席 = 无界。
    wrap_widths: HashMap<NodeId, Option<f32>>,
    /// 背景图注册表（第五批⑨）：url() 引用 → 宿主预解码 RGBA。
    images: HashMap<String, crate::paint::ImageRes>,
    /// 文本叶最小内容尺寸（T5d，shrink-to-fit 下限；restyle 期随自动测量产出）。
    #[cfg(feature = "text")]
    min_measures: HashMap<NodeId, (f32, f32)>,
    /// 宿主推送的非文本叶固有尺寸区间（T5d）：(min, max) 各轴。
    intrinsics: HashMap<NodeId, IntrinsicSize>,
    /// 内置文本栈（feature = "text"；字体字节由宿主推送）。
    #[cfg(feature = "text")]
    text: crate::text::TextSystem,
}

impl<K: Copy + Eq + Hash + 'static> Default for StyleEngine<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash + 'static> StyleEngine<K> {
    /// 创建空引擎（树镜像/样式表/环境全空，等待宿主推送）。
    pub fn new() -> Self {
        Self {
            tree: StyleTree::new(),
            sheet: Stylesheet::default(),
            user_sheet: None,
            ua_sheet: None,
            focused_node: None,
            registered_props: std::collections::BTreeMap::new(),
            font_faces: Vec::new(),
            counter_styles: Vec::new(),
            pseudo_ids: std::collections::BTreeMap::new(),
            primary_source: String::new(),
            extra_sheets: Vec::new(),
            next_sheet_id: 0,
            doc_layers: crate::css::stylesheet::LayerRegistry::default(),
            imports: std::collections::BTreeMap::new(),
            import_loader: None,
            media: MediaEnv::default(),
            key_to_node: HashMap::new(),
            node_to_key: HashMap::new(),
            root_key: None,
            overlay_roots: Vec::new(),
            top_layer: Vec::new(),
            root_order_applied: Vec::new(),
            measures: HashMap::new(),
            scroll_offsets: HashMap::new(),
            epoch: 0,
            generation: 0,
            dirty_struct: true,
            dirty_style: true,
            style_dirty_roots: Vec::new(),
            has_host_index: Vec::new(),
            viewport: (0.0, 0.0),
            scale: 1.0,
            now: 0.0,
            taffy: SendSyncTaffy(taffy::TaffyTree::new()),
            #[cfg(feature = "text")]
            text: crate::text::TextSystem::new(),
            taffy_root: None,
            taffy_node: HashMap::new(),
            calc_deferred: Vec::new(),
            taffy_parent: HashMap::new(),
            abs_cb: HashMap::new(),
            abs_structured: false,
            tables: Vec::new(),
            table_cols: HashMap::new(),
            table_cells: HashMap::new(),
            taffy_table_inner: HashMap::new(),
            table_row_anon: HashMap::new(),
            multicols: Vec::new(),
            multicol_state: HashMap::new(),
            column_rules: HashMap::new(),
            hit_rects: Vec::new(),
            container_sizes: HashMap::new(),
            styles: HashMap::new(),
            transitions: HashMap::new(),
            anim_underlying: HashMap::new(),
            selection_styles: HashMap::new(),
            placeholder_styles: HashMap::new(),
            float_touched: std::collections::HashSet::new(),
            inline_run_participants: std::collections::HashSet::new(),
            text_overrides: std::collections::HashMap::new(),
            font_metrics: Vec::new(),
            span_styles: HashMap::new(),
            parents: HashMap::new(),
            #[cfg(feature = "text")]
            auto_text: std::collections::HashSet::new(),
            wrap_widths: HashMap::new(),
            images: HashMap::new(),
            #[cfg(feature = "text")]
            min_measures: HashMap::new(),
            intrinsics: HashMap::new(),
        }
    }

    // ---------- 推送协议 ----------

    /// 全量替换样式表（容错：坏规则跳过并记录）。B2：替换主表并全量重建
    /// 文档层树与导入拼接（附加表保留、按登记序重附着——层序数跨表不可比，
    /// 以源文本重解析获得干净表内序数再映射文档序）。
    pub fn set_stylesheet(&mut self, source: &str) -> ParseReport {
        let sheet = crate::css::stylesheet::parse_stylesheet(source);
        self.primary_source = source.to_string();
        self.sheet = sheet;
        self.rebuild_sheets();
        // 阶段2③：新规则集的容器集合可变，旧尺寸快照可能误导新规则求值。
        self.container_sizes.clear();
        self.epoch += 1;
        self.dirty_style = true;
        self.sheet.report.clone()
    }

    /// B2：附加 author 样式表（返回句柄供 remove_stylesheet；级联序 =
    /// 主表之后按登记序，后表胜平手）。@import 在附着期经导入源拼接。
    pub fn add_stylesheet(&mut self, source: &str) -> u64 {
        self.next_sheet_id += 1;
        let handle = self.next_sheet_id;
        let sheet = crate::css::stylesheet::parse_stylesheet(source);
        self.extra_sheets.push((handle, source.to_string(), sheet));
        self.rebuild_sheets();
        self.container_sizes.clear();
        self.epoch += 1;
        self.dirty_style = true;
        handle
    }

    /// B2：移除附加样式表（句柄无效返回 false）。
    pub fn remove_stylesheet(&mut self, handle: u64) -> bool {
        let before = self.extra_sheets.len();
        self.extra_sheets.retain(|(id, _, _)| *id != handle);
        let removed = self.extra_sheets.len() != before;
        if removed {
            self.rebuild_sheets();
            self.container_sizes.clear();
            self.epoch += 1;
            self.dirty_style = true;
        }
        removed
    }

    /// B2：内存导入源（@import url → CSS 文本；未命中再试宿主 loader）。
    /// 变更后重建已附着表（未解析的导入可补齐）。
    pub fn set_import_source(&mut self, url: &str, css: &str) {
        self.imports.insert(url.to_string(), css.to_string());
        self.rebuild_sheets();
        self.dirty_style = true;
    }

    /// B2：宿主导入加载器（优先于内存源；None = 移除）。
    pub fn set_import_loader(&mut self, loader: Option<ImportLoader>) {
        self.import_loader = loader;
        self.rebuild_sheets();
        self.dirty_style = true;
    }

    /// B2：导入解析器（宿主 loader 优先，回退内存源）。
    fn import_resolver(&self) -> impl FnMut(&str) -> Option<String> + '_ {
        move |url: &str| {
            if let Some(loader) = &self.import_loader
                && let Some(css) = loader(url)
            {
                return Some(css);
            }
            self.imports.get(url).cloned()
        }
    }

    /// B2：附着单表（@import 拼接 + 层树并入文档树重映射 rank）。
    fn attach_sheet(&mut self, sheet: &mut Stylesheet) {
        {
            let mut seen = Vec::new();
            let mut resolver = self.import_resolver();
            crate::css::stylesheet::resolve_imports(sheet, &mut resolver, &[], 0, &mut seen);
        }
        sheet.remap_layers_to_doc(&mut self.doc_layers);
    }

    /// B2：文档级全量重建（主表 → 附加表按登记序）：重解析附加表（干净
    /// 表内层序数）→ 附着（导入拼接 + 文档层树重映射）。
    fn rebuild_sheets(&mut self) {
        self.doc_layers = crate::css::stylesheet::LayerRegistry::default();
        // B2 修订：主表从源文本重解析（而非复用既有解析产物）——嵌套
        // @import 的未决指令（子表待源）只在整表重解析时存活；最终一次
        // rebuild（全部导入源就位后）完成完整拼接。
        let mut primary = crate::css::stylesheet::parse_stylesheet(&self.primary_source);
        self.attach_sheet(&mut primary);
        self.sheet = primary;
        for i in 0..self.extra_sheets.len() {
            let source = self.extra_sheets[i].1.clone();
            let mut sheet = crate::css::stylesheet::parse_stylesheet(&source);
            self.attach_sheet(&mut sheet);
            self.extra_sheets[i].2 = sheet;
        }
        // B4：注册表随全量重建刷新（add/remove/import 路由皆经此）。
        self.rebuild_registered_props();
        // F3d：@font-face 登记表同点刷新。
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
    }

    /// B2：author 表组快照（主表在前、附加表按登记序 = 文档序）。
    fn author_sheets(&self) -> Vec<&Stylesheet> {
        std::iter::once(&self.sheet)
            .chain(self.extra_sheets.iter().map(|(_, _, s)| s))
            .collect()
    }

    /// B2：全表集合是否含 @container 规则（收敛环 pass 判据）。P6 D3
    /// （ADR-0035）：补齐 user_sheet 与 ua_sheet 漏检（原仅主表+附加表——
    /// 含 @container 的 user 表此前被跳过，收敛环 pass 数判定可错）。
    fn any_container_rules(&self) -> bool {
        self.sheet.has_container_rules
            || self
                .ua_sheet
                .as_ref()
                .is_some_and(|s| s.has_container_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_container_rules)
            || self
                .extra_sheets
                .iter()
                .any(|(_, _, s)| s.has_container_rules)
    }

    /// B4：重建文档级 @property 注册表（sheet 变更点统一调用）。合并序 =
    /// ua_sheet → user_sheet → 主表 → 附加表登记序（author 覆 user 覆 UA，
    /// 与起源优先级同构；P5 ADR-0033）；同名后规则覆盖（spec：@property
    /// 全部层叠前按文档序处理）。
    fn rebuild_registered_props(&mut self) {
        self.registered_props.clear();
        if let Some(ua) = &self.ua_sheet {
            for r in &ua.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
        if let Some(u) = &self.user_sheet {
            for r in &u.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
        for r in &self.sheet.property_rules {
            self.registered_props.insert(r.name.clone(), r.clone());
        }
        for (_, _, s) in &self.extra_sheets {
            for r in &s.property_rules {
                self.registered_props.insert(r.name.clone(), r.clone());
            }
        }
    }

    /// F3d（ADR-0026 D4）+ E：文档级登记表统一重建（@font-face +
    /// @counter-style，同变更点）。合并序 = ua_sheet → user_sheet → 主表
    /// → 附加表登记序（P5 ADR-0033 起源序）；@font-face 同族后规则胜
    /// （css-fonts-4）；@counter-style 同名后规则胜（css-counter-styles-3，
    /// 且可整体覆盖内置样式）。字体字节仍由宿主 add_font 推送——登记表
    /// 仅描述映射与筛选元数据。
    fn rebuild_document_registries(&mut self) {
        self.font_faces.clear();
        if let Some(ua) = &self.ua_sheet {
            self.font_faces.extend(ua.font_faces.iter().cloned());
        }
        if let Some(u) = &self.user_sheet {
            self.font_faces.extend(u.font_faces.iter().cloned());
        }
        self.font_faces
            .extend(self.sheet.font_faces.iter().cloned());
        for (_, _, s) in &self.extra_sheets {
            self.font_faces.extend(s.font_faces.iter().cloned());
        }
        self.counter_styles.clear();
        if let Some(ua) = &self.ua_sheet {
            self.counter_styles
                .extend(ua.counter_styles.iter().cloned());
        }
        if let Some(u) = &self.user_sheet {
            self.counter_styles.extend(u.counter_styles.iter().cloned());
        }
        self.counter_styles
            .extend(self.sheet.counter_styles.iter().cloned());
        for (_, _, s) in &self.extra_sheets {
            self.counter_styles.extend(s.counter_styles.iter().cloned());
        }
    }

    /// C1（ADR-0015）：伪元素实体化 pass（frame 内 sync_root_order 之后、
    /// rebuild_taffy 之前）。全表无伪元素规则 → 清除全部（零成本快路径）；
    /// 有 → 为每个宿主节点确保 ::before 首子 / ::after 末子存在并归位。
    /// 伪节点裸 StyleNode（无身份——selectors 经 originating_element 回
    /// origin 匹配左复合）；文本由 sync_pseudo_text 按 content 计算值供给。
    /// P9-3（css-lists-3 §3.1）：每个宿主另确保 ::marker 伪节点（首子、
    /// ::before 之前；空 marker 由 sync_pseudo_text 抑制成盒）——无条件
    /// 创建使 list-item 宿主首帧即有样式（无 materialize 期滞后）；marker
    /// 的 list-style-image 注入按上一帧宿主样式（首帧无图像=B·豁免，
    /// ADR-0041）。any_pseudo 快路径例外：UA/作者表无伪元素规则但存在
    /// list-item 时 marker 仍需存活——快路径条件加 any_list_item 逃逸。
    fn materialize_pseudos(&mut self) {
        let any_pseudo = self.sheet.has_pseudo_rules
            || self.user_sheet.as_ref().is_some_and(|s| s.has_pseudo_rules)
            || self.ua_sheet.as_ref().is_some_and(|s| s.has_pseudo_rules)
            || self.extra_sheets.iter().any(|(_, _, s)| s.has_pseudo_rules);
        // P9-3：上一帧存在 list-item 宿主（或 UA 表可产生）→ marker 通道
        // 必须 keep-alive（即便无伪元素规则）。
        let any_list_item = self
            .styles
            .values()
            .any(|cs| cs.display() == crate::css::property::Display::ListItem)
            || self.ua_sheet.is_some();
        if !any_pseudo && !any_list_item {
            if self.pseudo_ids.is_empty() {
                return;
            }
            let ids: Vec<NodeId> = self.pseudo_ids.values().copied().collect();
            for pid in ids {
                self.tree.remove(pid);
                // G1（ADR-0032）：伪节点死亡卫生清理（与树移除同域）
                self.transitions.remove(&pid);
                self.anim_underlying.remove(&pid);
            }
            self.pseudo_ids.clear();
            self.dirty_struct = true;
            return;
        }
        // DFS 全树（超根起，覆盖文档根与 overlay 根）。
        let mut stack = vec![self.tree.root()];
        let mut changed = false;
        while let Some(cur) = stack.pop() {
            if !self.tree.is_pseudo(cur) {
                let host = cur;
                // P9-3：::marker 首子（css-lists-3 §3.1：marker 位于
                // ::before 之前）。
                let m = match self.pseudo_ids.get(&(host, 2)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::Marker),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 2), nid);
                        changed = true;
                        nid
                    }
                };
                // P9-3：list-style-image 注入（上一帧宿主样式；B·豁免：
                // 首帧 styles 空不注入，ADR-0041）。每帧重算保持幂等——
                // 图像消失时清注入。white-space: pre 恒注入（css-lists-3
                // §3.1 UA 义务：suffix 尾空格不被折叠；注入于声明层级，
                // 作者 ::marker white-space 不可覆盖=B·豁免）。
                let host_image = self
                    .styles
                    .get(&host)
                    .and_then(|hcs| hcs.list_style_image());
                let mut marker_decls: Vec<crate::css::Declaration> =
                    vec![crate::css::Declaration {
                        id: crate::css::PropertyId::WhiteSpace,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::WhiteSpace(
                                crate::css::property::WhiteSpace::Pre,
                            ),
                        ),
                    }];
                if let Some(img) = &host_image {
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::BackgroundImage,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::BackgroundImage(vec![img.clone()]),
                        ),
                    });
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::Width,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::Len(
                                crate::css::value::LengthPercentage::Em(1.0),
                            ),
                        ),
                    });
                    marker_decls.push(crate::css::Declaration {
                        id: crate::css::PropertyId::Height,
                        important: false,
                        value: crate::css::decl::DeclSource::Parsed(
                            crate::css::property::DeclValue::Len(
                                crate::css::value::LengthPercentage::Em(1.0),
                            ),
                        ),
                    });
                }
                let mnode = self.tree.node_mut(m);
                if mnode.declarations.decls != marker_decls {
                    mnode.declarations = crate::css::decl::DeclarationBlock {
                        decls: marker_decls,
                        ..Default::default()
                    };
                    changed = true;
                }
                // ::before 首子
                let b = match self.pseudo_ids.get(&(host, 0)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::Before),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 0), nid);
                        changed = true;
                        nid
                    }
                };
                // ::after 末子
                let a = match self.pseudo_ids.get(&(host, 1)).copied() {
                    Some(e) => e,
                    None => {
                        let nid = self.tree.insert_child(
                            host,
                            crate::tree::StyleNode {
                                pseudo: Some(crate::tree::PseudoWhich::After),
                                ..Default::default()
                            },
                        );
                        self.pseudo_ids.insert((host, 1), nid);
                        changed = true;
                        nid
                    }
                };
                // 归位：marker + before + 宿主子序 + after（伪节点不参与宿主结构序）
                let host_kids: Vec<NodeId> = self
                    .tree
                    .children(host)
                    .iter()
                    .copied()
                    .filter(|c| *c != b && *c != a && *c != m && !self.tree.is_pseudo(*c))
                    .collect();
                let mut want = Vec::with_capacity(host_kids.len() + 3);
                want.push(m);
                want.push(b);
                want.extend(host_kids);
                want.push(a);
                if self.tree.children(host) != want.as_slice() {
                    self.tree.set_children(host, &want);
                    changed = true;
                }
            }
            stack.extend(self.tree.children(cur).iter().copied());
        }
        if changed {
            self.dirty_struct = true;
        }
    }

    /// C1（ADR-0015）：伪元素文本供给（frame 内 restyle 之后、布局之前）。
    /// 按 content 计算值写 tree.node.text 并登记测量（remeasure pass 同式；
    /// 折行由 T5c-2 收敛）。content none/normal → text 清空 + 撤测量
    ///（盒经 map_style display:none 移除）。
    /// P5（ADR-0036 D2）：升级为树序 DFS 求值——counter 作用域帧栈
    ///（reset 压帧 / increment 帧顶或隐式 0 累加 / counter() 最内帧、
    /// counters() 全帧 join）+ 引号深度（quotes 对表）+ attr()（伪元素
    /// 取 originating element 属性）。树序保证 = materialize_pseudos 归位。
    #[cfg(feature = "text")]
    fn sync_pseudo_text(&mut self) {
        if self.pseudo_ids.is_empty() {
            return;
        }
        let host_of: std::collections::HashMap<NodeId, NodeId> =
            self.pseudo_ids.iter().map(|((h, _), p)| (*p, *h)).collect();
        let mut scopes: Vec<std::collections::HashMap<String, i64>> = Vec::new();
        let mut quote_depth: i64 = 0;
        self.eval_content_walk(self.tree.root(), &mut scopes, &mut quote_depth, &host_of);
    }

    /// P5（ADR-0036 D2）：content 求值 DFS（树序）。普通节点应用
    /// counter-reset/increment（压帧/累加，离开弹出）；伪节点按 content
    /// 序列求值写文本；子树递归含伪节点（归位序 = before → 宿主子 → after）。
    #[cfg(feature = "text")]
    fn eval_content_walk(
        &mut self,
        nid: NodeId,
        scopes: &mut Vec<std::collections::HashMap<String, i64>>,
        quote_depth: &mut i64,
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) {
        let is_pseudo = self.tree.is_pseudo(nid);
        if !is_pseudo {
            // 计数器声明应用（css-lists-3 语义）：
            // reset → 新帧（遮蔽外层同名；重复 ident 后者胜 = map insert）。
            let mut frame: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            if let Some(cs) = self.styles.get(&nid) {
                for (name, val) in cs.counter_reset() {
                    frame.insert(name.clone(), *val);
                }
            }
            scopes.push(frame);
            // increment → 写当前帧；值沿全栈倒查（祖先/前兄弟经 merge 上浮
            // 的值 = css-lists-3 counter 继承链），无实例 → 隐式 0 起始。
            if let Some(cs) = self.styles.get(&nid) {
                for (name, step) in cs.counter_increment() {
                    let n = scopes.len();
                    let hit = scopes.iter().rev().find_map(|f| f.get(name).copied());
                    match hit {
                        Some(v) => {
                            scopes[n - 1].insert(name.clone(), v + *step);
                        }
                        None => {
                            scopes[n - 1].insert(name.clone(), *step);
                        }
                    }
                }
                // P9-3（css-lists-3 §4.6）：display:list-item 自动累加隐式
                // list-item 计数器（step=1）；显式 counter-increment 含
                // list-item 时以其为准（不重复累加）。
                if cs.display() == crate::css::property::Display::ListItem
                    && !cs.counter_increment().iter().any(|(n, _)| n == "list-item")
                {
                    let n = scopes.len();
                    let hit = scopes
                        .iter()
                        .rev()
                        .find_map(|f| f.get("list-item").copied());
                    match hit {
                        Some(v) => {
                            scopes[n - 1].insert("list-item".to_string(), v + 1);
                        }
                        None => {
                            scopes[n - 1].insert("list-item".to_string(), 1);
                        }
                    }
                }
            }
        } else if self.tree.node(nid).pseudo == Some(crate::tree::PseudoWhich::Marker) {
            // P9-3（css-lists-3 §3.1/§3.2）：marker 文本由宿主 list-style
            // 机器合成（§3.2 内容算法）。文本/测量照常供给（绘制层合成
            // 消费）；taffy 侧 map_style 恒隐藏——空 marker 天然无产物，
            // 无需显式抑制 pass。
            let (text, _hide, new_depth) =
                self.eval_marker_text(nid, *quote_depth, scopes, host_of);
            *quote_depth = new_depth;
            self.apply_pseudo_text(nid, text);
        } else {
            // 伪节点：content 求值（计数器栈/引号深度/attrs 消费）。
            let (text, new_depth) = self.eval_pseudo_content(nid, *quote_depth, scopes, host_of);
            *quote_depth = new_depth;
            self.apply_pseudo_text(nid, text);
        }
        let kids: Vec<NodeId> = self.tree.children(nid).to_vec();
        for kid in kids {
            self.eval_content_walk(kid, scopes, quote_depth, host_of);
        }
        if !is_pseudo {
            // merge 弹出（css-lists-3 兄弟继承）：离开节点时把本帧计数
            // 写回父帧——后续兄弟据此继承增量；reset 帧遮蔽仅在位期间
            // 生效（兄弟以自己的 reset 值重开）。
            if let Some(f) = scopes.pop()
                && let Some(parent) = scopes.last_mut()
            {
                for (k, v) in f {
                    parent.insert(k, v);
                }
            }
        }
    }

    /// P5（ADR-0036 D1/D2/D3）：伪节点 content 序列求值为文本。
    /// 返回 (文本, 新引号深度)；None = 不生成（none/normal/无样式）。
    #[cfg(feature = "text")]
    fn eval_pseudo_content(
        &self,
        pid: NodeId,
        depth: i64,
        scopes: &[std::collections::HashMap<String, i64>],
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) -> (Option<String>, i64) {
        let Some(cs) = self.styles.get(&pid) else {
            return (None, depth);
        };
        let mut d = depth;
        let mut out = String::new();
        let Some(pieces) = cs.content_pieces() else {
            // 单串/none/normal：沿用旧路径。
            return (
                match cs.content() {
                    crate::css::property::ContentValue::Str(s) => Some(s),
                    _ => None,
                },
                d,
            );
        };
        // attr() 属性源：伪节点 → originating element。
        let attr_node = host_of.get(&pid).copied().unwrap_or(pid);
        // counter() 取值：最内作用域帧优先（css-lists-3）；无实例 → 0。
        let counter_lookup = |name: &str| -> i64 {
            scopes
                .iter()
                .rev()
                .find_map(|f| f.get(name).copied())
                .unwrap_or(0)
        };
        for piece in pieces {
            match piece {
                crate::css::property::ContentPiece::Str(s) => out.push_str(s),
                crate::css::property::ContentPiece::Counter { name, style } => {
                    let v = counter_lookup(name);
                    out.push_str(
                        &crate::css::stylesheet::counter_format::format_counter_with(
                            style,
                            v,
                            &self.counter_styles,
                        ),
                    );
                }
                crate::css::property::ContentPiece::Counters {
                    name,
                    separator,
                    style,
                } => {
                    // counters()：全作用域帧自外向内逐帧按样式格式化后
                    // join；无实例 → 空串。
                    let vals: Vec<String> = scopes
                        .iter()
                        .filter_map(|f| {
                            f.get(name).map(|v| {
                                crate::css::stylesheet::counter_format::format_counter_with(
                                    style,
                                    *v,
                                    &self.counter_styles,
                                )
                            })
                        })
                        .collect();
                    out.push_str(&vals.join(separator));
                }
                crate::css::property::ContentPiece::Attr(name) => {
                    if let Some(v) = self.tree.node(attr_node).attrs.get(name) {
                        out.push_str(v);
                    }
                }
                crate::css::property::ContentPiece::OpenQuote => {
                    let pairs = cs.quotes_pairs();
                    if !pairs.is_empty() {
                        let idx = (d.max(0) as usize).min(pairs.len() - 1);
                        out.push_str(&pairs[idx].0);
                    }
                    d += 1;
                }
                crate::css::property::ContentPiece::CloseQuote => {
                    // css-content-3：深度 0 处 close-quote 不产出（钳 0 静默）。
                    if d > 0 {
                        d -= 1;
                        let pairs = cs.quotes_pairs();
                        if !pairs.is_empty() {
                            let idx = (d.max(0) as usize).min(pairs.len() - 1);
                            out.push_str(&pairs[idx].1);
                        }
                    }
                }
                crate::css::property::ContentPiece::NoOpenQuote => d += 1,
                crate::css::property::ContentPiece::NoCloseQuote => {
                    d -= 1;
                    if d < 0 {
                        d = 0;
                    }
                }
            }
        }
        (Some(out), d)
    }

    /// C1 旧路径搬运：把求值文本写回伪节点（text/measure/taffy 高度）。
    #[cfg(feature = "text")]
    fn apply_pseudo_text(&mut self, pid: NodeId, text: Option<String>) {
        if self.tree.node(pid).text == text {
            return;
        }
        self.tree.node_mut(pid).text = text.clone();
        match text {
            Some(t) => {
                let cs = match self.styles.get(&pid) {
                    Some(c) => c.clone(),
                    None => return,
                };
                let (w, h) = self.text.measure(&t, &cs, &self.map_env());
                self.measures.insert(pid, (w, h));
                self.auto_text.insert(pid);
                if let Some(&tid) = self.taffy_node.get(&pid) {
                    let mut ts = map_style(&cs, &self.map_env());
                    ts.size = taffy::prelude::Size {
                        width: taffy::prelude::Dimension::auto(),
                        height: taffy::prelude::Dimension::length(h),
                    };
                    let _ = self.taffy.set_style(tid, ts);
                }
            }
            None => {
                self.measures.remove(&pid);
                self.auto_text.remove(&pid);
            }
        }
    }

    /// P9-3（css-lists-3 §3.2）：::marker 内容算法（按首个真条件求值）。
    /// 返回 (文本, hide, 新引号深度)；hide=true → 引擎显式抑制成盒
    ///（suppress_marker 置 taffy Display::None，每帧重放=稳态幂等）。
    /// ① 宿主非 list-item → 抑制（§3.1：非 list-item 的 ::marker content
    /// 计算为 none）；② 作者 content ≠ normal → 按 content 求值（同
    /// ::before）；③ list-style-image 有效 → 匿名替换元素盒（materialize
    /// 注入 1em 声明，文本 None 不 hide）；④ list-style-type：none →
    /// 抑制；string → 字面；counter-style 名 → list-item 计数表示 +
    /// prefix + suffix（未知名回退 decimal，css-counter-styles-3 §2）。
    #[cfg(feature = "text")]
    fn eval_marker_text(
        &self,
        pid: NodeId,
        depth: i64,
        scopes: &[std::collections::HashMap<String, i64>],
        host_of: &std::collections::HashMap<NodeId, NodeId>,
    ) -> (Option<String>, bool, i64) {
        let Some(host) = host_of.get(&pid).copied() else {
            return (None, true, depth);
        };
        let Some(hcs) = self.styles.get(&host) else {
            return (None, true, depth);
        };
        if hcs.display() != crate::css::property::Display::ListItem {
            return (None, true, depth);
        }
        let Some(cs) = self.styles.get(&pid) else {
            return (None, true, depth);
        };
        // ② 作者 content 优先（§3.2 条件 1）。
        let author_content = cs.content_pieces().is_some_and(|p| !p.is_empty())
            || matches!(cs.content(), crate::css::property::ContentValue::Str(_));
        if author_content {
            let (text, d) = self.eval_pseudo_content(pid, depth, scopes, host_of);
            let hide = text.is_none();
            return (text, hide, d);
        }
        // ③ list-style-image（§3.2 条件 2）。
        if cs.list_style_image().is_some() {
            return (None, false, depth);
        }
        // ④ list-style-type（§3.2 条件 3）。
        match cs.list_style_type() {
            None => (None, true, depth),
            Some(crate::css::property::ListStyleTypeValue::Str(s)) => (Some(s), false, depth),
            Some(crate::css::property::ListStyleTypeValue::Name(name)) => {
                // list-item 计数值：宿主帧（marker 为宿主子节点，帧已含
                // 隐式/显式增量）；无实例 → 0。
                let v = scopes
                    .iter()
                    .rev()
                    .find_map(|f| f.get("list-item").copied())
                    .unwrap_or(0);
                let text = crate::css::stylesheet::counter_format::marker_text(
                    &name,
                    v,
                    &self.counter_styles,
                );
                (Some(text), false, depth)
            }
        }
    }

    /// P9-3：空 marker 显式 taffy 抑制。map_style 对 Marker 伪节点不做
    /// content 空判隐藏（可见性由本函数与文本供给管）；rebuild_taffy 每
    /// 帧重播种 → 本抑制每帧重放（稳态幂等）。
    #[cfg(feature = "text")]
    #[allow(dead_code)]
    fn suppress_marker(&mut self, pid: NodeId) {
        let Some(cs) = self.styles.get(&pid).cloned() else {
            return;
        };
        if let Some(&tid) = self.taffy_node.get(&pid) {
            let mut ts = map_style(&cs, &self.map_env());
            ts.display = taffy::prelude::Display::None;
            let _ = self.taffy.set_style(tid, ts);
        }
    }

    /// B3：全表集合是否含 `:has()` 相对选择器规则（变更类失效升级全量
    /// 重样式判据——相对选择器命中依赖后代/兄弟结构，增量子树 restyle
    /// 不感知远端变化）。user_sheet 变更本身即全量重样式，但后续增量
    /// 变更需此判据感知。P6 补 ua_sheet 漏检（自定义 UA 表可含 `:has`）。
    fn any_has_rules(&self) -> bool {
        self.sheet.has_relative_selectors
            || self
                .ua_sheet
                .as_ref()
                .is_some_and(|s| s.has_relative_selectors)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_relative_selectors)
            || self
                .extra_sheets
                .iter()
                .any(|(_, _, s)| s.has_relative_selectors)
    }

    /// P6（ADR-0035 D1）：重建 `:has()` host 快筛索引（表变更点调用，同
    /// rebuild_document_registries 时机——attach/set_stylesheet/user·ua 表装载路径）。
    /// 逐表逐规则深扫 `:has`（含 `:is()`/`:where()`/`:not()` 参数内嵌套）：
    /// 无 → 不进索引；有 → 逐选择器提键（合格）或记不合格；任一选择器
    /// 不合格（前缀组合器/`:has` 不在顶层最右 compound）→ 追加无约束
    /// 哨兵键（该规则任何 host 路径都可能命中，恒升级全量，保守正确）。
    fn rebuild_has_host_index(&mut self) {
        self.has_host_index.clear();
        let sheets = std::iter::once(&self.sheet)
            .chain(self.ua_sheet.as_ref())
            .chain(self.user_sheet.as_ref())
            .chain(self.extra_sheets.iter().map(|(_, _, s)| s));
        for sheet in sheets {
            for rule in &sheet.rules {
                if !crate::selector::list_contains_has(&rule.selectors) {
                    continue;
                }
                let mut qualified = false;
                let mut unqualified = false;
                for sel in rule.selectors.slice() {
                    match crate::selector::has_host_key(sel) {
                        Some(key) => {
                            self.has_host_index.push(key);
                            qualified = true;
                        }
                        None => unqualified = true,
                    }
                }
                if unqualified || !qualified {
                    // 不合格选择器在场，或深扫有 `:has` 但零合格键（例如
                    // `:has` 仅存在于函数式伪类参数内）——全量兜底哨兵。
                    self.has_host_index
                        .push(crate::selector::HasHostKey::default());
                }
            }
        }
    }

    /// P7-①（表格 auto 列）：单元格内容 max-content 宽度——子树文本
    /// 叶 nowrap 测量取最大（同 settle_lines 盒探针的文本兜底范式）
    /// 加根格水平内缩（padding+border）。嵌套盒结构组合（多块纵向
    /// 叠加、行内横向并排）v1 近似为单叶最大——偏差【B】登记 ADR。
    fn content_max_width(&mut self, nid: NodeId) -> f32 {
        let mut w = 0.0f32;
        let mut sub: Vec<NodeId> = vec![nid];
        while let Some(s) = sub.pop() {
            if let Some(text) = self.tree.node(s).text.clone()
                && !text.is_empty()
                && let Some(cs) = self.styles.get(&s).cloned()
            {
                let owned = self.span_styles.get(&s).cloned().unwrap_or_default();
                let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                    owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                let (lw, _lh) =
                    self.text
                        .measure_rich(&text, &cs, &span_refs, None, &self.map_env());
                if lw > w {
                    w = lw;
                }
            }
            for g in self.tree.children(s).iter().copied() {
                sub.push(g);
            }
        }
        let mut inset = 0.0f32;
        if let Some(cs) = self.styles.get(&nid) {
            let env = self.map_env();
            let rctx = crate::css::value::ResolveCtx {
                em: cs.font_size_px(),
                rem: env.rem,
                viewport_w: env.viewport_w,
                viewport_h: env.viewport_h,
                ..crate::css::value::ResolveCtx::base(
                    cs.font_size_px(),
                    16.0,
                    env.viewport_w,
                    env.viewport_h,
                )
            };
            inset = [
                (crate::css::property::PropertyId::PaddingLeft, None),
                (crate::css::property::PropertyId::PaddingRight, None),
                (
                    crate::css::property::PropertyId::BorderLeftWidth,
                    Some(crate::css::property::PropertyId::BorderLeftStyle),
                ),
                (
                    crate::css::property::PropertyId::BorderRightWidth,
                    Some(crate::css::property::PropertyId::BorderRightStyle),
                ),
            ]
            .iter()
            .filter_map(|(pid, style_pid)| used_h_inset(cs, *pid, *style_pid, &rctx))
            .sum();
        }
        w + inset
    }

    /// P6（ADR-0035 D2）：`style_dirty_roots` 增量失效是否须升级全量。
    /// 任一 dirty 节点（含自身）沿祖先链通过任一快筛键 → 可能命中 →
    /// 升级；全部祖先被全部键否决 → 走增量。索引空（无合格规则/未建）
    /// → 全量兜底（与既有 any_has_rules 行为兼容）。快筛只可能多升级
    /// （false positive），不可能漏升级——保守正确。
    fn has_invalidation_needs_full(&self) -> bool {
        if !self.any_has_rules() {
            return false;
        }
        if self.has_host_index.is_empty() {
            return true;
        }
        for &root in &self.style_dirty_roots {
            let mut cur = Some(root);
            while let Some(nid) = cur {
                let node = self.tree.node(nid);
                if self.has_host_index.iter().any(|k| {
                    k.matches_node(node.name.as_deref(), node.id.as_deref(), &node.classes)
                }) {
                    return true;
                }
                cur = self.tree.parent(nid);
            }
        }
        false
    }

    /// 更新媒体环境（视口外因素：配色/动效偏好）。
    pub fn set_environment(&mut self, env: MediaEnv) {
        if self.media != env {
            self.media = env;
            self.dirty_style = true;
        }
    }

    /// B3：焦点族管理（:focus / :focus-visible / :focus-within）。整体迁移
    /// 语义：清旧焦点链（焦点节点 FOCUS|FOCUS_VISIBLE + 祖先链
    /// FOCUS_WITHIN）→ 施加新链（焦点节点 FOCUS（focus_visible 时加
    /// FOCUS_VISIBLE）+ 祖先链 FOCUS_WITHIN）。focus_visible 的启发判定
    /// （键盘 vs 指针）归宿主——引擎无输入设备知识。
    pub fn set_focus(
        &mut self,
        key: Option<K>,
        focus_visible: bool,
    ) -> Result<(), crate::error::ContractError> {
        let new_id = match &key {
            Some(k) => Some(
                *self
                    .key_to_node
                    .get(k)
                    .ok_or(crate::error::ContractError::UnknownNode)?,
            ),
            None => None,
        };
        // 无 early-return：同节点重设（focus_visible 标记翻转）也要走
        // 全链清旧/施新——否则 :focus-visible 切换不生效。
        // 清旧链：焦点节点三态 + 祖先链 FOCUS_WITHIN。
        if let Some(old) = self.focused_node {
            let node = self.tree.node_mut(old);
            node.state
                .remove(crate::tree::NodeState::FOCUS | crate::tree::NodeState::FOCUS_VISIBLE);
            let mut cur = self.tree.parent(old);
            while let Some(p) = cur {
                self.tree
                    .node_mut(p)
                    .state
                    .remove(crate::tree::NodeState::FOCUS_WITHIN);
                cur = self.tree.parent(p);
            }
        }
        // 施加新链。
        if let Some(new) = new_id {
            {
                let node = self.tree.node_mut(new);
                node.state.insert(crate::tree::NodeState::FOCUS);
                if focus_visible {
                    node.state.insert(crate::tree::NodeState::FOCUS_VISIBLE);
                }
            }
            let mut cur = self.tree.parent(new);
            while let Some(p) = cur {
                self.tree
                    .node_mut(p)
                    .state
                    .insert(crate::tree::NodeState::FOCUS_WITHIN);
                cur = self.tree.parent(p);
            }
        }
        self.focused_node = new_id;
        self.dirty_style = true;
        Ok(())
    }

    /// 插入节点。`parent=None` 声明根（至多一次）。
    pub fn insert(
        &mut self,
        parent: Option<K>,
        key: K,
        node: StyleNode,
    ) -> Result<(), crate::error::ContractError> {
        if self.key_to_node.contains_key(&key) {
            return Err(crate::error::ContractError::DuplicateNode);
        }
        // class 语义归一（CSS class 属性为空格分隔 token）
        let mut node = node;
        node.classes = normalize_classes(&node.classes);
        // span 区间契约校验（T5c）：UTF-8 字节边界内、有序、不越界；
        // 无文本节点的 span 一律非法（区间无处着落）。
        for span in &node.spans {
            let (start, end) = (span.range.0 as usize, span.range.1 as usize);
            let ok = node.text.as_ref().is_some_and(|t| {
                start <= end
                    && end <= t.len()
                    && t.is_char_boundary(start)
                    && t.is_char_boundary(end)
            });
            if !ok {
                return Err(crate::error::ContractError::InvalidSpan);
            }
        }
        match parent {
            None => {
                // ADR-0010 多根：用户根 = 合成超根之子。首个 = 文档根
                //（单根语义逐位保留），后续 = overlay 根（弹窗/浮层）。
                let id = self.tree.insert_child(self.tree.root(), node);
                self.key_to_node.insert(key, id);
                self.node_to_key.insert(id, key);
                if self.root_key.is_none() {
                    self.root_key = Some(key);
                } else {
                    self.overlay_roots.push(key);
                }
                self.dirty_struct = true;
                Ok(())
            }
            Some(pk) => {
                let Some(&pid) = self.key_to_node.get(&pk) else {
                    return Err(crate::error::ContractError::UnknownNode);
                };
                let id = self.tree.insert_child(pid, node);
                self.key_to_node.insert(key, id);
                self.node_to_key.insert(id, key);
                self.dirty_struct = true;
                Ok(())
            }
        }
    }

    /// 移除节点及其子树（根移除 = 清空引擎树）。
    pub fn remove(&mut self, key: K) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        if Some(key) == self.root_key {
            // 清空重来
            self.tree = StyleTree::new();
            self.key_to_node.clear();
            self.node_to_key.clear();
            self.measures.clear();
            self.span_styles.clear();
            self.parents.clear();
            self.wrap_widths.clear();
            #[cfg(feature = "text")]
            self.min_measures.clear();
            self.intrinsics.clear();
            #[cfg(feature = "text")]
            self.auto_text.clear();
            self.scroll_offsets.clear();
            self.root_key = None;
            self.overlay_roots.clear();
            self.top_layer.clear();
            self.root_order_applied.clear();
            self.taffy_root = None;
            self.taffy_node.clear();
            self.styles.clear();
            self.transitions.clear();
            self.anim_underlying.clear();
            self.focused_node = None;
            self.pseudo_ids.clear();
            self.dirty_struct = true;
            self.dirty_style = true;
            return Ok(());
        }
        // 收集子树 keys 一并解除映射
        let mut stack = vec![id];
        let mut doomed = Vec::new();
        while let Some(cur) = stack.pop() {
            doomed.push(cur);
            stack.extend(self.tree.children(cur).iter().copied());
        }
        // B3：焦点链锚点落在本子树 → 清除（防 NodeId 槽位复用悬垂）。
        if let Some(f) = self.focused_node
            && doomed.contains(&f)
        {
            self.focused_node = None;
        }
        // C1：伪元素注册表按 doomed origin 清理（伪节点为 origin 子节点，
        // 已随子树 doomed 消亡；防 NodeId 槽位复用悬垂）。
        self.pseudo_ids
            .retain(|(origin, _), _| !doomed.contains(origin));
        for d in doomed {
            if let Some(k) = self.node_to_key.remove(&d) {
                self.key_to_node.remove(&k);
            }
            self.measures.remove(&d);
            self.span_styles.remove(&d);
            self.parents.remove(&d);
            self.wrap_widths.remove(&d);
            #[cfg(feature = "text")]
            self.min_measures.remove(&d);
            self.intrinsics.remove(&d);
            #[cfg(feature = "text")]
            self.auto_text.remove(&d);
            self.scroll_offsets.remove(&d);
            self.styles.remove(&d);
            // G1（ADR-0032）：节点移除同步清理过渡条目（NodeId 槽位复用
            // 防悬垂——与 styles.remove 同一正确性域）。
            self.transitions.remove(&d);
            self.anim_underlying.remove(&d);
            self.taffy_node.remove(&d);
        }
        // P6（ADR-0035）：结构变更改变 host 的后代集合，直接影响
        // `:has()` 命中域——存活父进增量通道（快筛判定升级与否）。无
        // `:has` 表时保持 v1 语义（remove 不触发样式重算，零行为变化）；
        // B3 起 remove+`:has` 组合的失配缺口在此收口。
        let survival_parent = self.tree.parent(id);
        self.tree.remove(id);
        self.overlay_roots.retain(|&k| k != key);
        self.top_layer.retain(|&k| k != key);
        self.dirty_struct = true;
        if self.any_has_rules()
            && let Some(p) = survival_parent
        {
            self.style_dirty_roots.push(p);
        }
        Ok(())
    }

    /// ADR-0010：把 overlay 根移入/移出 top-layer（弹窗层）。有效绘制序 =
    /// 文档根 → 非 top overlay（插入序）→ top 层根（进层序）。文档根不可
    /// 进层（报 [`ContractError::NotOverlayRoot`]）；未知 key 报
    /// [`ContractError::UnknownNode`]。重复移入幂等。
    pub fn set_top_layer(&mut self, key: K, on: bool) -> Result<(), crate::error::ContractError> {
        if Some(key) == self.root_key {
            return Err(crate::error::ContractError::NotOverlayRoot);
        }
        if !self.key_to_node.contains_key(&key) {
            return Err(crate::error::ContractError::UnknownNode);
        }
        if on {
            if !self.top_layer.contains(&key) {
                self.overlay_roots.retain(|&k| k != key);
                self.top_layer.push(key);
            }
        } else if self.top_layer.contains(&key) {
            self.top_layer.retain(|&k| k != key);
            self.overlay_roots.push(key);
        }
        Ok(())
    }

    /// 重设子节点顺序（所有 key 须已是该父节点的子节点）。
    pub fn set_children(
        &mut self,
        parent: K,
        children: &[K],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&pid) = self.key_to_node.get(&parent) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let mut ids = Vec::with_capacity(children.len());
        for k in children {
            let Some(&cid) = self.key_to_node.get(k) else {
                return Err(crate::error::ContractError::UnknownNode);
            };
            if self.tree.parent(cid) != Some(pid) {
                return Err(crate::error::ContractError::UnknownNode);
            }
            ids.push(cid);
        }
        // C1（ADR-0015）：宿主镜像不含伪键——合并 [::before] + 宿主序 +
        // [::after]（伪节点由 materialize_pseudos 持有）。
        let mut merged = Vec::with_capacity(ids.len() + 2);
        if let Some(&b) = self.pseudo_ids.get(&(pid, 0)) {
            merged.push(b);
        }
        merged.extend(ids);
        if let Some(&a) = self.pseudo_ids.get(&(pid, 1)) {
            merged.push(a);
        }
        self.tree.set_children(pid, &merged);
        self.dirty_struct = true;
        Ok(())
    }

    /// 更新节点 class 列表。
    pub fn set_classes(
        &mut self,
        key: K,
        classes: &[String],
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).classes = normalize_classes(classes);
        self.dirty_style = true;
        Ok(())
    }

    /// 更新节点交互状态位。
    pub fn set_state(
        &mut self,
        key: K,
        state: crate::tree::NodeState,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).state = state;
        self.dirty_style = true;
        Ok(())
    }

    /// 更新节点文本（叶内容）。
    pub fn set_text(
        &mut self,
        key: K,
        text: Option<String>,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.tree.node_mut(id).text = text;
        self.dirty_style = true;
        Ok(())
    }

    /// 更新内联声明（style 属性文本；容错解析，报告随返回值给出）。
    pub fn set_declarations(
        &mut self,
        key: K,
        style_text: &str,
    ) -> Result<ParseReport, crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        let (block, report) = parse_inline_declarations(style_text);
        // 流参与翻转失效（差分锁定）：内联块是整体替换——新旧任一侧
        // 声明了 position/display/float，本节点在父流程中的参与方式
        // （出流锚定、行打包成员、流位槽、margin 塌穿）都可能翻转，
        // 兄弟的布局结果随之改变但兄弟样式未变。把父节点一并记入脏根
        // （restyle 父子树 → 兄弟 set_style → taffy 布局缓存失效），
        // 仅结构性声明才付父子树代价，常规声明保持节点子树局部性。
        let flow_structural = |b: &crate::css::decl::DeclarationBlock| {
            b.decls.iter().any(|d| {
                matches!(
                    d.id,
                    crate::css::property::PropertyId::Position
                        | crate::css::property::PropertyId::Display
                        | crate::css::property::PropertyId::Float
                )
            })
        };
        let parent_dirty =
            flow_structural(&block) || flow_structural(&self.tree.node(id).declarations);
        self.tree.node_mut(id).declarations = block;
        // 增量重样式（阶段5）：内联声明默认只影响自身与后代的样式求值
        //（选择器命中只依赖自身/祖先的树数据与继承链），记脏根子树局部
        // 重算；frame 依容器规则在场与否择路。唯一例外即上方流参与翻转
        // ——结构性声明额外脏化父节点。
        if parent_dirty && let Some(pid) = self.tree.parent(id) {
            self.style_dirty_roots.push(pid);
        }
        self.style_dirty_roots.push(id);
        Ok(report)
    }

    /// 推送叶测量（文本叶尺寸；T5 后由内置 parley 测量接管）。
    pub fn set_leaf_measure(
        &mut self,
        key: K,
        width: f32,
        height: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.measures.insert(id, (width, height));
        // 宿主测量接管后不再参与自动换行重测量（T5c-2）
        #[cfg(feature = "text")]
        self.auto_text.remove(&id);
        self.dirty_style = true;
        Ok(())
    }

    /// 推送非文本叶固有尺寸区间（T5d，shrink-to-fit 第三 pass 消费）：
    /// (min_w, min_h) / (max_w, max_h) 各轴独立。definite 首选尺寸仍走
    /// [`StyleEngine::set_leaf_measure`]；absolute 叶按包含块宽夹紧到区间内。
    pub fn set_leaf_intrinsic(
        &mut self,
        key: K,
        min_w: f32,
        min_h: f32,
        max_w: f32,
        max_h: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.intrinsics.insert(id, ((min_w, min_h), (max_w, max_h)));
        Ok(())
    }

    /// 节点是否 position: absolute（T5d：不按父流宽换行，走 shrink-to-fit）。
    fn is_absolute(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Absolute
            ))
        )
    }

    /// 节点是否定位元素（position != static，absolute 叶的包含块判定）。
    /// A4：fixed 亦为定位元素（其 absolute 后代的包含块）。
    fn is_positioned(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Relative
                    | crate::css::property::Position::Absolute
                    | crate::css::property::Position::Fixed
            ))
        )
    }

    /// 节点是否 fixed 定位（A4：包含块=transformed 祖先或 ICB 视口；
    /// positioned 祖先不构成 fixed 的包含块）。
    fn is_fixed(&self, id: NodeId) -> bool {
        matches!(
            self.styles
                .get(&id)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::Position)),
            Some(crate::css::property::DeclValue::Position(
                crate::css::property::Position::Fixed
            ))
        )
    }

    /// rem 基准：文档根计算字号（ADR-0010 多根语义下仅文档根定义 rem）。
    /// styles 未含根（全量 restyle 清空后求值中 / 首帧前）回落 16.0——
    /// 恰为 CSS Values 对「根元素 font-size 中 rem 按初始值解析」的规定。
    fn rem_base(&self) -> f32 {
        self.root_key
            .and_then(|k| self.key_to_node.get(&k))
            .and_then(|id| self.styles.get(id))
            .map(|cs| cs.font_size_px())
            .unwrap_or(16.0)
    }

    /// 映射期环境（CSS 语义：vw/vh 与 calc 视口单位 = 初始包含块 = 帧视口；
    /// map_style 不评估媒体条件——@media 命中在级联期按宿主推送的 media
    /// 判定，故此处以帧视口覆盖 media 视口字段，A6 min/max/clamp 与
    /// calc 同路径受益）。
    fn map_env(&self) -> MediaEnv {
        let mut env = self.media;
        env.viewport_w = self.viewport.0;
        env.viewport_h = self.viewport.1;
        // P0 修复：rem 基准接线文档根计算字号（此前恒 16.0，
        // :root font-size ≠ 16px 时全部 rem 长度错误）。
        env.rem = self.rem_base();
        env
    }
    /// transform ≠ none 的元素成为 absolute/fixed 后代的包含块
    /// （ADR-0009 双时机之 L2：restyle 期谓词，cb walk 消费）。
    fn is_transform_cb(&self, id: NodeId) -> bool {
        self.styles.get(&id).is_some_and(|cs| cs.has_transform())
    }

    /// absolute 叶的可用宽（T5d）：最近 positioned 或 transformed 祖先的内容宽
    /// （border-box − padding − 已生效 border，`used_h_inset`）；均无 → 视口宽。
    // 仅 text 测量路径消费（shrink-to-fit pass）；layout-only 编译下保持
    // 零死代码（依赖治理 feature 门禁审计，阶段4）。
    #[cfg(feature = "text")]
    fn abs_avail_width(&self, id: NodeId) -> Option<f32> {
        let mut cb = self.parents.get(&id).copied();
        let mut cb_id: Option<NodeId> = None;
        while let Some(p) = cb {
            if self.is_positioned(p) || self.is_transform_cb(p) {
                cb_id = Some(p);
                break;
            }
            cb = self.parents.get(&p).copied();
        }
        match cb_id {
            Some(p) => {
                let tid = *self.taffy_node.get(&p)?;
                let pline = self.taffy.layout(tid).ok()?;
                let cbcs = self.styles.get(&p)?;
                let rctx = crate::css::value::ResolveCtx {
                    em: cbcs.font_size_px(),
                    rem: self.map_env().rem,
                    viewport_w: self.map_env().viewport_w,
                    viewport_h: self.map_env().viewport_h,
                    ..crate::css::value::ResolveCtx::base(
                        cbcs.font_size_px(),
                        16.0,
                        self.map_env().viewport_w,
                        self.map_env().viewport_h,
                    )
                };
                let inset: f32 = [
                    (crate::css::property::PropertyId::PaddingLeft, None),
                    (crate::css::property::PropertyId::PaddingRight, None),
                    (
                        crate::css::property::PropertyId::BorderLeftWidth,
                        Some(crate::css::property::PropertyId::BorderLeftStyle),
                    ),
                    (
                        crate::css::property::PropertyId::BorderRightWidth,
                        Some(crate::css::property::PropertyId::BorderRightStyle),
                    ),
                ]
                .iter()
                .filter_map(|(pid, style_pid)| used_h_inset(cbcs, *pid, *style_pid, &rctx))
                .sum();
                Some((pline.size.width - inset).max(0.0))
            }
            None => Some(self.viewport.0),
        }
    }

    /// 三期② absolute 锚定跳走（A 级缺口收口）：CSS 中 absolute 子件的包含
    /// 块 = 最近 positioned 或 transform≠none 祖先（均无 → 初始包含块 ICB），
    /// 而 taffy 0.14 只按直父 padding box 锚定绝对子件——直父与 cb 之间存在
    /// static 包装层时 inset 百分比基准、静态位置与 auto 边距全部错位。
    /// 本 pass 在样式最终就绪（含 @keyframes 覆写——动画可翻转 has_transform）
    /// 后、布局前执行：用 set_children 移动语义把 absolute 子件重挂到 cb
    /// 节点（cb==直父的常规情形零变化；跳走后 taffy 的 inset 基准/静态位置
    /// 随 cb 正确）。table/multicol 子树维持 v1 契约（settle_columns：absolute
    /// 子件包含块仍为容器）不参与。collect() 按 abs_cb 推导视口坐标；
    /// 稳态帧 desired 与当前子列表全等免 set_children，无 absolute 且结构
    /// 已归位时整段跳过。
    fn settle_absolute_anchors(&mut self) {
        let Some(viewport) = self.taffy_root else {
            return;
        };
        // 1) DFS 求重挂映射：abs_cb[node] = Some(cb)（cb≠直父）| None（ICB）；
        //    abs_in[cb]（None=ICB）= 该 cb 名下需重挂的 absolute 子件。
        let mut abs_cb: HashMap<NodeId, Option<NodeId>> = HashMap::new();
        let mut abs_in: HashMap<Option<NodeId>, Vec<NodeId>> = HashMap::new();
        // 栈项：(节点, 样式父, 最近 cb 候选（None=尚无）, 豁免子树)
        let mut stack: Vec<AbsCbStackItem> = vec![(self.tree.root(), None, None, None, false)];
        while let Some((id, parent, cb, tcb, exempt)) = stack.pop() {
            let cb_next = if self.is_positioned(id) || self.is_transform_cb(id) {
                Some(id)
            } else {
                cb
            };
            // A4：fixed 的包含块候选只认 transformed 祖先（positioned 但
            // 未 transform 的祖先对 fixed 仍是「透明」的——CSS 2.1/3）。
            let tcb_next = if self.is_transform_cb(id) {
                Some(id)
            } else {
                tcb
            };
            // v1 契约豁免：table/multicol 容器及其子树——子件列表由
            // settle_tables/settle_columns 管理，此处不触碰。
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            if !exempt_next && parent.is_some() {
                if self.is_absolute(id) && cb != parent {
                    // 注意用继承的 cb（严格祖先候选），不得用 cb_next——
                    // absolute 节点自身 positioned 会把自己选成自己的 cb。
                    abs_cb.insert(id, cb);
                    abs_in.entry(cb).or_default().push(id);
                } else if self.is_fixed(id) && tcb != parent {
                    // A4 fixed：重挂到 transformed 祖先（None=ICB 视口）。
                    abs_cb.insert(id, tcb);
                    abs_in.entry(tcb).or_default().push(id);
                }
            }
            for c in self.tree.children(id) {
                stack.push((*c, Some(id), cb_next, tcb_next, exempt_next));
            }
        }
        self.abs_cb = abs_cb;
        if self.abs_cb.is_empty() && !self.abs_structured {
            return;
        }
        // 2) 结构归位：逐非豁免节点比较 desired taffy 子列表（样式子件剔除
        // 重挂走的 + 重挂进来的）与当前列表，有差才 set_children（move 语义
        // 顺带从旧父摘除——上帧重挂残留随之归位）。
        let mut moved = false;
        let mut stack: Vec<(NodeId, bool)> = vec![(self.tree.root(), false)];
        while let Some((id, exempt)) = stack.pop() {
            let exempt_next = exempt || self.tables.contains(&id) || self.multicols.contains(&id);
            for c in self.tree.children(id) {
                stack.push((*c, exempt_next));
            }
            if exempt_next {
                continue;
            }
            let Some(&tid) = self.taffy_node.get(&id) else {
                continue;
            };
            let mut desired: Vec<taffy::NodeId> = self
                .tree
                .children(id)
                .iter()
                .filter(|c| !self.abs_cb.contains_key(*c))
                .filter_map(|c| self.taffy_node.get(c).copied())
                .collect();
            if let Some(list) = abs_in.get(&Some(id)) {
                desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
            }
            if self.taffy.children(tid).unwrap_or_default() != desired {
                let _ = self.taffy.set_children(tid, &desired);
                moved = true;
            }
            // taffy_parent 同步（collect 幻影补偿共用该表）：重挂子件指 cb，
            // 恢复直父锚定的子件回写直父，防陈旧项误触发补偿。
            for c in self.tree.children(id) {
                let Some(&ctid) = self.taffy_node.get(c) else {
                    continue;
                };
                let expected = match self.abs_cb.get(c) {
                    Some(Some(cb)) => self.taffy_node.get(cb).copied(),
                    Some(None) => Some(viewport),
                    None => Some(tid),
                };
                if let Some(e) = expected
                    && self.taffy_parent.get(&ctid) != Some(&e)
                {
                    self.taffy_parent.insert(ctid, e);
                }
            }
        }
        // 3) ICB：无 positioned/transformed 祖先的 absolute 直接挂合成视口根
        //（无 border/padding → location 相对视口原点；inset 百分比基准 =
        // 视口尺寸，与 CSS ICB 语义一致）。
        let mut vp_desired: Vec<taffy::NodeId> = self
            .taffy_node
            .get(&self.tree.root())
            .copied()
            .into_iter()
            .collect();
        if let Some(list) = abs_in.get(&None) {
            vp_desired.extend(list.iter().filter_map(|n| self.taffy_node.get(n).copied()));
        }
        if self.taffy.children(viewport).unwrap_or_default() != vp_desired {
            let _ = self.taffy.set_children(viewport, &vp_desired);
            moved = true;
        }
        self.abs_structured = moved || !self.abs_cb.is_empty();
    }

    /// 推送节点滚动偏移（绘制消费；T4 生效）。
    pub fn set_scroll_offset(
        &mut self,
        key: K,
        x: f32,
        y: f32,
    ) -> Result<(), crate::error::ContractError> {
        let Some(&id) = self.key_to_node.get(&key) else {
            return Err(crate::error::ContractError::UnknownNode);
        };
        self.scroll_offsets.insert(id, (x, y));
        Ok(())
    }

    /// 当前样式表纪元（单调递增）。
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// C4（ADR-0018）：::selection / ::placeholder 通道样式解析——origin
    /// 直配（match_pseudo_element：pseudo == None 即命中），继承基 = origin
    /// 主样式（css-pseudo-4）；级联经 cascade_channel（主级联防泄漏由
    /// collect_sheet 过滤承担）。通道样式不进 taffy/不参与布局——纯宿主
    /// 读取通道；合并表组全无对应规则时整 map 清除（零成本模式）。
    fn style_channels(
        &mut self,
        id: NodeId,
        main: &ComputedStyle,
        cctx: &mut [crate::cascade::ContainerCtx],
        env: &MediaEnv,
    ) {
        if self.tree.node(id).pseudo.is_some() {
            return; // 通道规则对实体化伪节点恒不命中（D2），不产通道样式
        }
        let author = self.author_sheets();
        let has_sel = author.iter().any(|s| s.has_selection_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_selection_rules);
        let has_ph = author.iter().any(|s| s.has_placeholder_rules)
            || self
                .user_sheet
                .as_ref()
                .is_some_and(|s| s.has_placeholder_rules);
        // 级联与计算 confined 在不可变阶段（author/CascadeOutput 对 self
        // 的共享借用随块终结），其后进入可变阶段写 map（NLL 借用纪律）。
        let sel = if has_sel {
            let cascaded = crate::cascade::cascade_channel(
                &self.tree,
                id,
                author.clone(),
                self.user_sheet.as_ref(),
                env,
                cctx,
                crate::selector::PseudoElement::Selection,
                self.ua_sheet.as_ref(),
            );
            // 通道契约：无任何规则命中（winners 双空）→ None（宿主回退
            // 系统缺省；@media 门控外 / 选择器不命中皆此形）。
            if cascaded.winners.is_empty() && cascaded.custom_winners.is_empty() {
                None
            } else {
                let mut style = compute_node_from_cascade(
                    &self.tree,
                    id,
                    cascaded,
                    &self.registered_props,
                    env,
                    Some(main),
                );
                let fm = self.metrics_for(&style);
                style.set_font_metrics(fm);
                Some(style)
            }
        } else {
            None
        };
        let ph = if has_ph {
            let cascaded = crate::cascade::cascade_channel(
                &self.tree,
                id,
                author.clone(),
                self.user_sheet.as_ref(),
                env,
                cctx,
                crate::selector::PseudoElement::Placeholder,
                self.ua_sheet.as_ref(),
            );
            if cascaded.winners.is_empty() && cascaded.custom_winners.is_empty() {
                None
            } else {
                let mut style = compute_node_from_cascade(
                    &self.tree,
                    id,
                    cascaded,
                    &self.registered_props,
                    env,
                    Some(main),
                );
                let fm = self.metrics_for(&style);
                style.set_font_metrics(fm);
                Some(style)
            }
        } else {
            None
        };
        // 清除语义二分：has_sel=false（全表无规则）→ 整 map 清（零成本）；
        // 单节点空级联（@media 外 / 选择器不命中）→ 仅移除本节点陈旧
        // 条目——不得整 map clear（restyle DFS 后段未命中节点会抹掉
        // 前段命中条目，探针实证）。
        if !has_sel {
            self.selection_styles.clear();
        } else if let Some(style) = sel {
            self.selection_styles.insert(id, style);
        } else {
            self.selection_styles.remove(&id);
        }
        if !has_ph {
            self.placeholder_styles.clear();
        } else if let Some(style) = ph {
            self.placeholder_styles.insert(id, style);
        } else {
            self.placeholder_styles.remove(&id);
        }
    }

    /// 诊断 API（C6 可观测性）：读取节点最近一次 restyle 的计算样式。
    /// 帧前调用返回上一帧样式；未知 key 返回 None——诊断路径不上浮
    /// [`ContractError`](crate::error::ContractError)。
    #[must_use]
    pub fn computed_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.styles.get(&id)
    }

    /// C4（ADR-0018）：节点 ::selection 通道样式（origin 直配 + 主样式
    /// 继承基；选区在场与否归宿主——引擎零副作用）。表组无对应规则 /
    /// 未知 key = None。
    #[must_use]
    pub fn selection_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.selection_styles.get(&id)
    }

    /// C4（ADR-0018）：节点 ::placeholder 通道样式（语义同
    /// [`Self::selection_style`]；占位文本存在性归宿主）。
    #[must_use]
    pub fn placeholder_style(&self, key: K) -> Option<&ComputedStyle> {
        let id = *self.key_to_node.get(&key)?;
        self.placeholder_styles.get(&id)
    }

    // ---------- 帧驱动 ----------

    /// 推进一帧：结构同步 → 重算样式 → taffy 布局 → 收集布局盒。
    pub fn frame(&mut self, viewport: (f32, f32), scale: f32, now: f64) -> Frame<K> {
        // C6 可观测性：帧级 span（`style_engine::engine` target；无订阅者时
        // 惰性构建零成本）。pass 字段随收敛环逐 pass 记录。
        let frame_span = tracing::info_span!(target: "style_engine::engine", "frame", pass = tracing::field::Empty);
        let _frame_guard = frame_span.enter();
        self.viewport = viewport;
        self.scale = scale;
        self.now = now;
        // ADR-0010：根序同步先于 taffy 重建——重建镜像树序（天然含多根）；
        // 结构未脏时走 taffy set_children 局部重排（稳态零操作）。
        self.sync_root_order();
        // C1（ADR-0015）：伪元素实体化先于 taffy 重建（新增伪节点进结构
        // 镜像；sheet 变更后 has_pseudo_rules 翻转在此收敛）。
        self.materialize_pseudos();
        if self.dirty_struct {
            self.rebuild_taffy();
        }
        // 阶段2③ 收敛环：@container 尺寸快照来自上一 pass 布局，规则命中
        // 可改变布局 → 有变即重算样式再布局，定点收敛（上限 3 pass；无
        // @container 规则单 pass，与拆分前逐位等价）。
        let cap = if self.any_container_rules() { 3 } else { 1 };
        for pass in 0..cap {
            frame_span.record("pass", pass);
            if self.dirty_style {
                self.restyle();
            } else if !self.style_dirty_roots.is_empty() {
                // 增量重样式（阶段5）：无容器规则 → 脏根子树局部重算；
                // 有容器规则 → 保守全量（容器快照收敛环自会处理）。P6
                // （ADR-0035 D2）：`:has` 在场不再无脑全量——脏根祖先链
                // 过 host 快筛，全否决才走增量（保守正确，见
                // has_invalidation_needs_full）。P9-4：容器收窄——快照表
                // 空时（上帧无 container-type 元素）容器规则不可能命中，
                // 走增量；新容器经 record_container_sizes→changed→全量
                // pass 收敛（下一 pass 带新鲜快照，与容器尺寸变化同路）。
                let container_full = self.any_container_rules() && !self.container_sizes.is_empty();
                if container_full || self.has_invalidation_needs_full() {
                    self.restyle();
                } else {
                    let roots = std::mem::take(&mut self.style_dirty_roots);
                    self.restyle_subtrees(roots);
                }
            }
            // C1（ADR-0015）：伪元素文本按 content 计算值供给（restyle 后、
            // 布局前；同帧布局正确）。
            #[cfg(feature = "text")]
            self.sync_pseudo_text();
            // G1（ADR-0032）：transition 采样——级联后、动画前（CSS 层叠：
            // animation 层高于 transition 层，同槽动画值随后覆写过渡值）。
            // 过渡表空 → 零写入（稳态铁律：无活动过渡 frame() 行为与既有
            // 实现逐位一致；布局每帧全量重算，过渡值变更无需独立失效标）。
            self.sample_transitions();
            // 第五批⑰：@keyframes 动画采样——每帧级联后、布局前覆写（布局与
            // 绘制消费动画值；布局每帧全量重算，动画值变更无需独立失效标）
            self.apply_animations();
            // 三期② absolute 锚定跳走：样式最终就绪（动画可翻转 has_transform
            // → cb 集合逐帧变化）后、布局前，把 absolute 子件重挂到 CSS 包含块
            //（taffy 0.14 只按直父 padding box 锚定绝对子件）。
            self.settle_absolute_anchors();
            // 表格 wrapper 重构：wrapper/inner 结构就绪须先于首次
            // compute_layout（wrapper 填满 + 内表声明宽同帧生效）；表子树
            // 结构由本函数与 settle_tables 独占（结构同步对表豁免，v1 契约）。
            self.settle_table_fixup();
            // ADR-0010：超根全视口化 + overlay 根默认视口锚定（显式定位不动）
            self.apply_root_anchor_styles();
            // C3（ADR-0017 D4）：替换内容叶固有尺寸种子——须在首次
            // compute_layout 前（盒=自然/声明/单边比例；静态量幂等）。
            self.seed_image_leaves();
            // ④E5（ADR-0020）：grid 放置解析——纯样式派生（容器线名/
            // 区域矩形 → 子放置数字线号），无布局依赖 → compute_layout
            // 之前一遍即可（无额外重排）。
            self.apply_grid_placements();
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
                // ③F2（ADR-0022 D1）：结算 pass DAG 调度（拓扑序+运行门，
                // 替换硬编码序）。依赖声明=Tables/Columns→Calc、Floats→
                // Columns、Lines→Floats（文本折行宽依赖全部结算值——下游
                // 一致先于 remeasure）；pass 内脏区跳过=增量布局（F3）范围。
                for pass in SettlePassKind::schedule() {
                    if self.settle_should_run(pass) {
                        self.settle_run_pass(pass, viewport);
                    }
                }
            }
            // T5c-2：文本叶换行——pass1 布局给出包含块宽后，white-space: normal 的
            // 自动测量文本叶按 max_advance 重测量并按需二次布局。包含块内容宽 =
            // 父 border-box − 父左右 padding − 已生效 border（style none 时宽归零）；
            // shrink-to-fit 父宽受无界文本影响的场景为残余偏差。
            #[cfg(feature = "text")]
            if let Some(root) = self.taffy_root {
                let mut remeasure: Vec<(NodeId, f32)> = Vec::new();
                for &id in self.measures.keys() {
                    if !self.auto_text.contains(&id) {
                        continue;
                    }
                    // F1（ADR-0021）：行内运行参与者已由 settle_lines 按运行
                    // 上下文宽度测置；全宽重测会破坏打包结果。
                    if self.inline_run_participants.contains(&id) {
                        continue;
                    }
                    // absolute 叶不按父流宽换行——shrink-to-fit 由第三 pass 夹紧（T5d）
                    if self.is_absolute(id) {
                        continue;
                    }
                    let Some(cs) = self.styles.get(&id) else {
                        continue;
                    };
                    let wraps = matches!(
                        cs.get(crate::css::property::PropertyId::WhiteSpace),
                        None | Some(crate::css::property::DeclValue::WhiteSpace(
                            // A5：pre-wrap/pre-line/break-spaces 亦按包含块宽换行
                            crate::css::property::WhiteSpace::Normal
                                | crate::css::property::WhiteSpace::PreWrap
                                | crate::css::property::WhiteSpace::PreLine
                                | crate::css::property::WhiteSpace::BreakSpaces
                        ))
                    );
                    if !wraps {
                        continue;
                    }
                    if let Some(avail) = self.leaf_wrap_width(id) {
                        remeasure.push((id, avail));
                    }
                }
                let mut reflow = false;
                for (id, avail) in remeasure {
                    let Some(text) = self.tree.node(id).text.clone() else {
                        continue;
                    };
                    let Some(cs) = self.styles.get(&id).cloned() else {
                        continue;
                    };
                    let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                    let (w, h) = self.text.measure_rich(
                        &text,
                        &cs,
                        &span_refs,
                        Some(avail),
                        &self.map_env(),
                    );
                    if let Some(old) = self.measures.get(&id) {
                        reflow |=
                            (old.0 - w).abs() > f32::EPSILON || (old.1 - h).abs() > f32::EPSILON;
                    }
                    self.measures.insert(id, (w, h));
                    self.wrap_widths.insert(id, Some(avail));
                    if let Some(&tid) = self.taffy_node.get(&id) {
                        let mut ts = map_style(&cs, &self.map_env());
                        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                        for raw in crate::layout::take_calc_deferred() {
                            self.calc_deferred
                                .push(crate::layout::DeferredCalc { node: tid, raw });
                        }
                        // 第五批⑥：同 restyle_node 契约——声明宽优先，否则
                        // taffy auto（块流拉伸到容器内容宽）；测量高兜底。
                        ts.size = taffy::prelude::Size {
                            width: if has_declared_len(&cs, crate::css::property::PropertyId::Width)
                            {
                                ts.size.width
                            } else {
                                taffy::prelude::Dimension::auto()
                            },
                            height: if has_declared_len(
                                &cs,
                                crate::css::property::PropertyId::Height,
                            ) {
                                ts.size.height
                            } else {
                                taffy::prelude::Dimension::length(h)
                            },
                        };
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
                // 第三 pass（T5d，shrink-to-fit，CSS 10.3.7）：absolute 叶按包含块
                // 可用宽夹紧——width = clamp(min_content, avail, max_content)。
                // cb = 最近 positioned 祖先（无 → 视口宽）；近似：可用宽未扣自身
                // margin/静态位置。文本叶 max_content 取 pass1 无界测量（wrap pass
                // 已跳过 absolute 叶，measures 仍是无界值）；LeafMeasure 叶用
                // set_leaf_intrinsic 区间。
                let mut shrink: Vec<(NodeId, f32)> = Vec::new();
                for &id in self.measures.keys() {
                    if !self.is_absolute(id) || !self.tree.children(id).is_empty() {
                        continue;
                    }
                    let (min_w, max_w) = if self.auto_text.contains(&id) {
                        let Some(min) = self.min_measures.get(&id) else {
                            continue;
                        };
                        let Some(m) = self.measures.get(&id) else {
                            continue;
                        };
                        (min.0, m.0.max(min.0))
                    } else if let Some(((mnw, _), (mxw, _))) = self.intrinsics.get(&id) {
                        (*mnw, *mxw)
                    } else {
                        continue;
                    };
                    let Some(raw_avail) = self.abs_avail_width(id) else {
                        continue;
                    };
                    // CSS 10.3.7：夹紧对象是内容宽——content-box 语义下先扣自身
                    // 水平 padding+border（border 仅 style 非 none 计入）
                    let avail = match self.styles.get(&id) {
                        Some(cs) => {
                            let rctx = crate::css::value::ResolveCtx {
                                em: cs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    cs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            };
                            let inset: f32 = [
                                (crate::css::property::PropertyId::PaddingLeft, None),
                                (crate::css::property::PropertyId::PaddingRight, None),
                                (
                                    crate::css::property::PropertyId::BorderLeftWidth,
                                    Some(crate::css::property::PropertyId::BorderLeftStyle),
                                ),
                                (
                                    crate::css::property::PropertyId::BorderRightWidth,
                                    Some(crate::css::property::PropertyId::BorderRightStyle),
                                ),
                            ]
                            .iter()
                            .filter_map(|(pid, style_pid)| {
                                used_h_inset(cs, *pid, *style_pid, &rctx)
                            })
                            .sum();
                            (raw_avail - inset).max(0.0)
                        }
                        None => raw_avail,
                    };
                    let width = avail.min(max_w).max(min_w);
                    shrink.push((id, width));
                }
                for (id, width) in shrink {
                    if self.auto_text.contains(&id) {
                        let text = self.tree.node(id).text.clone().unwrap_or_default();
                        let Some(cs) = self.styles.get(&id).cloned() else {
                            continue;
                        };
                        // 换行宽：声明宽优先（CSS 10.3.7 声明宽即内容可用宽，
                        // 文本按声明宽折行——曾一律用 shrink 夹紧宽，声明 140
                        // 被无界测量 233 盖过而漏折行，bidi-mixed key4 高超差
                        // 根因）。percent 基准取夹紧宽（v1 近似）。
                        let rctx = crate::css::value::ResolveCtx {
                            em: cs.font_size_px(),
                            rem: self.map_env().rem,
                            viewport_w: self.map_env().viewport_w,
                            viewport_h: self.map_env().viewport_h,
                            ..crate::css::value::ResolveCtx::base(
                                cs.font_size_px(),
                                16.0,
                                self.map_env().viewport_w,
                                self.map_env().viewport_h,
                            )
                        };
                        let wrap = match cs.get(crate::css::property::PropertyId::Width) {
                            Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => {
                                lp.resolve(&rctx, width)
                            }
                            _ => None,
                        }
                        .map(|d| d.max(0.0))
                        .unwrap_or(width);
                        let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                        let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                            owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                        let (w, h) = self.text.measure_rich(
                            &text,
                            &cs,
                            &span_refs,
                            Some(wrap),
                            &self.map_env(),
                        );
                        if let Some(old) = self.measures.get(&id) {
                            reflow |= (old.0 - w).abs() > f32::EPSILON
                                || (old.1 - h).abs() > f32::EPSILON;
                        }
                        self.measures.insert(id, (w, h));
                        self.wrap_widths.insert(id, Some(wrap));
                    } else {
                        let h = self.measures.get(&id).map(|m| m.1).unwrap_or(0.0);
                        if let Some(old) = self.measures.get(&id) {
                            reflow |= (old.0 - width).abs() > f32::EPSILON;
                        }
                        self.measures.insert(id, (width, h));
                    }
                    if let Some(&tid) = self.taffy_node.get(&id)
                        && let Some(cs) = self.styles.get(&id)
                    {
                        let mut ts = map_style(cs, &self.map_env());
                        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点。
                        for raw in crate::layout::take_calc_deferred() {
                            self.calc_deferred
                                .push(crate::layout::DeferredCalc { node: tid, raw });
                        }
                        // 第五批⑥：absolute shrink-to-fit（CSS 10.3.7）仅适
                        // 用 width:auto——声明宽优先保留（含 min/max 夹紧由
                        // taffy 消费）；测量高兜底 height:auto。
                        ts.size = taffy::prelude::Size {
                            width: if has_declared_len(cs, crate::css::property::PropertyId::Width)
                            {
                                ts.size.width
                            } else {
                                taffy::prelude::Dimension::length(width)
                            },
                            height: if has_declared_len(
                                cs,
                                crate::css::property::PropertyId::Height,
                            ) {
                                ts.size.height
                            } else {
                                taffy::prelude::Dimension::length(
                                    self.measures.get(&id).map(|m| m.1).unwrap_or(0.0),
                                )
                            },
                        };
                        let _ = self.taffy.set_style(tid, ts);
                    }
                }
                if reflow {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
            }
            // ③multi-column：文本重排后列内高度可能变化——二次平衡（稳态
            // 零成本：n/colw/分配全等即返回）。
            self.settle_columns(viewport);
            // 阶段2③：记录容器内容盒快照；有变且未达上限 → 下一 pass 以
            // 新快照重算样式（文本换行约束随包含块宽逐 pass 自动重测，
            // 无需失效 measures——宿主推送测量缓存照常复用）。
            let changed = self.record_container_sizes();
            if !changed || pass + 1 >= cap {
                break;
            }
            self.dirty_style = true;
        }
        self.generation += 1;
        let mut boxes = Vec::with_capacity(self.tree.len());
        let mut layout_by_node: HashMap<NodeId, (f32, f32, f32, f32)> =
            HashMap::with_capacity(self.tree.len());
        if self.root_key.is_some() {
            self.collect(self.tree.root(), 0.0, 0.0, &mut boxes, &mut layout_by_node);
        }
        // 三期⑤c：列规几何结算（collect 后 layout_by_node 最新鲜；
        // DisplayList 构建前完成，PaintCtx 直引同一张表）。
        self.settle_column_rules(&layout_by_node);
        // ADR-0007：滚动容器可滚动外延（宿主推进偏移的量程）。识别 = overflow
        // ∈ {auto, scroll}（两轴独立）；外延 = padding box 与全部后代 border box
        // 并集相对 padding box 原点的最大超出。三期⑥：后代带 transform 时
        // （含祖先链复合，与 ADR-0009 绘制期同一终结 resolve_transform_affine），
        // 贡献 = 变换后 AABB，仅正向溢出并入（LTR 左/上不扩量程，Chromium 同）。
        // 近似：绝对定位后代一并并入，随定位模型细化；偏移归宿主
        // （set_scroll_offset），引擎不夹紧。
        let mut scrollable: HashMap<K, (f32, f32)> = HashMap::new();
        if self.root_key.is_some() {
            let border_px = |cs: &ComputedStyle,
                             w_id: crate::css::property::PropertyId,
                             s_id: crate::css::property::PropertyId|
             -> f32 {
                let painted = !matches!(
                    cs.get(s_id),
                    Some(crate::css::property::DeclValue::BorderStyle(
                        crate::css::property::BorderStyle::None
                    ))
                ) && cs.get(s_id).is_some();
                if !painted {
                    return 0.0;
                }
                match cs.get(w_id) {
                    Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp
                        .resolve(
                            &crate::css::value::ResolveCtx {
                                em: cs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    cs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            },
                            0.0,
                        )
                        .unwrap_or(0.0),
                    _ => 0.0,
                }
            };
            for (&key, &id) in &self.key_to_node {
                let Some(cs) = self.styles.get(&id) else {
                    continue;
                };
                let scrollable_axis = |pid: crate::css::property::PropertyId| {
                    // 解析层已把 auto 归一为 Scroll（引擎内语义等价，见 parse_overflow）
                    matches!(
                        cs.get(pid),
                        Some(crate::css::property::DeclValue::Overflow(
                            crate::css::property::Overflow::Scroll
                        ))
                    )
                };
                let ox = scrollable_axis(crate::css::property::PropertyId::OverflowX);
                let oy = scrollable_axis(crate::css::property::PropertyId::OverflowY);
                if !ox && !oy {
                    continue;
                }
                let Some(&(x, y, w, h)) = layout_by_node.get(&id) else {
                    continue;
                };
                use crate::css::property::PropertyId;
                let bl = border_px(cs, PropertyId::BorderLeftWidth, PropertyId::BorderLeftStyle);
                let bt = border_px(cs, PropertyId::BorderTopWidth, PropertyId::BorderTopStyle);
                let br = border_px(
                    cs,
                    PropertyId::BorderRightWidth,
                    PropertyId::BorderRightStyle,
                );
                let bb = border_px(
                    cs,
                    PropertyId::BorderBottomWidth,
                    PropertyId::BorderBottomStyle,
                );
                let (px0, py0) = (x + bl, y + bt);
                let (pw, ph) = ((w - bl - br).max(0.0), (h - bt - bb).max(0.0));
                let mut ex = px0 + pw;
                let mut ey = py0 + ph;
                let mut stack: Vec<(NodeId, Option<[f32; 6]>)> =
                    self.tree.children(id).iter().map(|&c| (c, None)).collect();
                while let Some((c, acc)) = stack.pop() {
                    // acc = 祖先链复合仿射（None = 尚未遇变换，恒等）；
                    // 各仿射均以未变换视口系为基（ADR-0009 布局盒不变），先
                    // 后代 own、再祖先 acc —— 与绘制流 cur∘M 嵌套完全一致。
                    let mut eff = acc;
                    if let Some(ccs) = self.styles.get(&c)
                        && ccs.has_transform()
                        && let Some(&(lx, ly, lw, lh)) = layout_by_node.get(&c)
                    {
                        let own = crate::paint::resolve_transform_affine(
                            ccs,
                            lx,
                            ly,
                            lw,
                            lh,
                            &self.map_env(),
                        );
                        eff = Some(match acc {
                            Some(a) => crate::paint::mul_affine(&a, &own),
                            None => own,
                        });
                    }
                    if let Some(&(cx, cy, cw, ch)) = layout_by_node.get(&c) {
                        if let Some(a) = eff {
                            // 变换后四角 AABB；仅 max 并入（左/上不扩量程）。
                            let mut maxx = f32::NEG_INFINITY;
                            let mut maxy = f32::NEG_INFINITY;
                            for (px, py) in
                                [(cx, cy), (cx + cw, cy), (cx, cy + ch), (cx + cw, cy + ch)]
                            {
                                maxx = maxx.max(a[0] * px + a[2] * py + a[4]);
                                maxy = maxy.max(a[1] * px + a[3] * py + a[5]);
                            }
                            ex = ex.max(maxx);
                            ey = ey.max(maxy);
                        } else {
                            ex = ex.max(cx + cw);
                            ey = ey.max(cy + ch);
                        }
                    }
                    stack.extend(self.tree.children(c).iter().map(|&g| (g, eff)));
                }
                let max_x = if ox { (ex - px0 - pw).max(0.0) } else { 0.0 };
                let max_y = if oy { (ey - py0 - ph).max(0.0) } else { 0.0 };
                if max_x > 0.0 || max_y > 0.0 {
                    scrollable.insert(key, (max_x, max_y));
                }
            }
        }
        // ⑤F2（ADR-0022 D2）：文本截断结算（ellipsis；line-clamp=D3）——
        // 测量宽就绪后生成 text_overrides（绘制替换）。
        #[cfg(feature = "text")]
        self.apply_text_truncation();
        // F3a（ADR-0023）：命中几何收集（paint 期透传，帧末入库供
        // hit_test 逆序查询）。
        let hit_cell = std::cell::RefCell::new(crate::paint::HitCollector::default());
        let mut paint = crate::paint::DisplayList {
            generation: self.generation,
            ..Default::default()
        };
        // ADR-0010：按有效根序逐根追加绘制基元（超根无样式条目，不可作
        // paint 走查起点；用户根的样式/布局条目齐备——单根视觉输出与
        // 旧「自 tree.root() 走查」逐位一致，超根本身不产生基元）。
        // P9-3（css-lists-3 §3.1/§3.5）：list-item 宿主 → 可见文本 marker
        // 绘制层合成表（host → (marker 节点, 前进宽 px)）。marker 伪节点
        // taffy 隐藏（map_style 恒 Display::None），由 paint 层在宿主首行
        // 内容左缘合成文本 op（inside 语义；outside≈inside B·豁免
        // ADR-0041）；advance = marker 测量宽（含 white-space:pre 尾空格）。
        // 非 text 特性：measures 恒空 → 表恒空（marker 不绘制）。
        let mut markers: HashMap<NodeId, (NodeId, f32)> = HashMap::new();
        for (&(host, slot), &m) in &self.pseudo_ids {
            if slot != 2 {
                continue;
            }
            if let Some(adv) = self.marker_advance_of(host) {
                markers.insert(host, (m, adv));
            }
        }
        {
            let ctx = crate::paint::PaintCtx {
                tree: &self.tree,
                styles: &self.styles,
                layout: &layout_by_node,
                scroll: &self.scroll_offsets,
                env: &self.map_env(),
                spans: &self.span_styles,
                wrap_widths: &self.wrap_widths,
                text_overrides: &self.text_overrides,
                hit: Some(&hit_cell),
                images: &self.images,
                column_rules: &self.column_rules,
                markers: &markers,
            };
            let mut root_ids: Vec<NodeId> = self
                .root_order_applied
                .iter()
                .filter_map(|k| self.key_to_node.get(k).copied())
                .collect();
            if root_ids.is_empty() {
                root_ids.push(self.tree.root());
            }
            for id in root_ids {
                crate::paint::append_display_list(&ctx, id, &mut paint);
            }
        }
        self.hit_rects = hit_cell.into_inner().rects;
        Frame {
            generation: self.generation,
            boxes,
            paint,
            scrollable,
        }
    }

    /// 命中测试（F3a，ADR-0023）：点 → 最顶可命中节点（绘制序逆序；
    /// 祖先 clip 链全含判定；visibility/display:none 与 pointer-events:
    /// none 天然排除）。无帧数据或未命中 → None。
    /// P4 D4（ADR-0037）精确化：查询点先经命中单元登记时活跃仿射的逆
    /// 变换到节点局部系再测盒（transform 节点命中随变换走）；clip 链
    /// 改精确形状（矩形含逐角圆角 / clip-path 折线，各自逆矩阵映射）。
    pub fn hit_test(&self, x: f32, y: f32) -> Option<crate::paint::HitTestHit> {
        for r in self.hit_rects.iter().rev() {
            // 变换节点：视口点 → 局部点（登记时 mat 的逆；奇异→恒等回退）。
            let (lx, ly) = if r.mat == [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
                (x, y)
            } else {
                match crate::paint::invert_affine(&r.mat) {
                    Some(inv) => crate::paint::apply_affine(&inv, x, y),
                    None => (x, y),
                }
            };
            if lx < r.x || ly < r.y || lx > r.x + r.w || ly > r.y + r.h {
                continue;
            }
            if r.clips
                .iter()
                .any(|c| !crate::paint::hit_clip_contains(c, x, y))
            {
                continue;
            }
            return Some(crate::paint::HitTestHit { node_id: r.node_id });
        }
        None
    }

    /// 用户键 → 节点 id（F3a，ADR-0023：hit_test 返回 NodeId 的宿主解释
    /// 通道；未注册键 → None）。
    pub fn node_id(&self, key: &K) -> Option<NodeId> {
        self.key_to_node.get(key).copied()
    }

    /// 样式树结构人读视图（F3e，ADR-0027 D1）：显示序（root_order_applied
    /// = paint 同序；空则文档根）逐节点一行，深度缩进；标签 = 元素名/`#id`
    /// /`.class`/宿主键/伪元素标记/文本截断。几何视图见
    /// [`Frame::boxes_dump`]——本方法只有树，无帧几何（Frame 才有盒）。
    /// 纯投影零新状态，不参与 restyle/paint 任何路径。
    pub fn layout_tree_dump(&self) -> String
    where
        K: std::fmt::Debug,
    {
        let mut rev: HashMap<NodeId, K> = self.key_to_node.iter().map(|(k, &n)| (n, *k)).collect();
        let mut out = String::new();
        let mut roots: Vec<NodeId> = self
            .root_order_applied
            .iter()
            .filter_map(|k| self.key_to_node.get(k).copied())
            .collect();
        if roots.is_empty() {
            roots.push(self.tree.root());
        }
        for r in roots {
            dump_tree_node(&self.tree, &mut rev, r, 0, &mut out);
        }
        out
    }

    /// 阶段2③：记录容器内容盒尺寸快照（container-type ≠ normal 的节点；
    /// 内容盒 = taffy border box − 解析后 padding/border，负值夹 0）。返回
    /// 快照是否相对上帧变化（首帧从无到有亦算变 → 驱动一次收敛 pass）。
    /// 无 @container 规则零成本跳过（快照保持空表）。
    fn record_container_sizes(&mut self) -> bool {
        if !self.any_container_rules() {
            return false;
        }
        let mut next: HashMap<NodeId, [f32; 2]> = HashMap::new();
        for (id, cs) in &self.styles {
            if cs.container_type() == crate::css::property::ContainerType::Normal {
                continue;
            }
            let Some(&tid) = self.taffy_node.get(id) else {
                continue;
            };
            let Ok(l) = self.taffy.layout(tid) else {
                continue;
            };
            let inset_x = l.border.left + l.border.right + l.padding.left + l.padding.right;
            let inset_y = l.border.top + l.border.bottom + l.padding.top + l.padding.bottom;
            next.insert(
                *id,
                [
                    (l.size.width - inset_x).max(0.0),
                    (l.size.height - inset_y).max(0.0),
                ],
            );
        }
        let changed = next != self.container_sizes;
        self.container_sizes = next;
        changed
    }

    /// ADR-0010：超根子序 = 有效绘制序（文档根 → 非 top overlay（插入序）
    /// → top 层根（进层序））。树子序与 taffy 子序同步重排，此后所有既有
    /// 走查（绘制/命中/量程/文本/级联）零改动地遵序。序未变时零操作
    ///（稳态帧无 taffy 结构失效）。
    fn sync_root_order(&mut self) {
        let doc = match self.root_key {
            Some(k) => k,
            None => {
                self.root_order_applied.clear();
                return;
            }
        };
        let mut order = Vec::with_capacity(1 + self.overlay_roots.len() + self.top_layer.len());
        order.push(doc);
        order.extend(self.overlay_roots.iter().copied());
        order.extend(self.top_layer.iter().copied());
        if self.root_order_applied == order {
            return;
        }
        let ids: Vec<NodeId> = order
            .iter()
            .filter_map(|k| self.key_to_node.get(k).copied())
            .collect();
        let dirty = self.dirty_struct;
        if !dirty {
            let tids: Vec<taffy::NodeId> = ids
                .iter()
                .filter_map(|id| self.taffy_node.get(id).copied())
                .collect();
            if let Some(super_tid) = self.taffy_node.get(&self.tree.root()).copied() {
                let _ = self.taffy.set_children(super_tid, &tids);
            }
        }
        self.tree.set_children(self.tree.root(), &ids);
        self.root_order_applied = order;
    }

    /// ADR-0010：超根全视口化（block、宽高 100%——overlay 的绝对定位锚定
    /// 盒 = 视口）与 overlay 根默认视口锚定（映射样式仍为 relative/static
    /// 时强制 absolute + top/left 0；作者显式 absolute/fixed 不动）。样式
    /// pass 会以默认样式覆写超根 → 每帧幂等重贴（计算布局前）。
    fn apply_root_anchor_styles(&mut self) {
        use taffy::prelude::{
            Dimension, Display, LengthPercentageAuto as LPA, Position as TPos, Rect, Size,
            Style as TStyle,
        };
        if self.root_key.is_none() {
            return;
        }
        let mut overrides: Vec<(taffy::NodeId, TStyle)> = Vec::new();
        if let Some(&super_tid) = self.taffy_node.get(&self.tree.root()) {
            overrides.push((
                super_tid,
                TStyle {
                    display: Display::Block,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
            ));
        }
        let media = self.map_env();
        for k in self.overlay_roots.iter().chain(self.top_layer.iter()) {
            let Some(&id) = self.key_to_node.get(k) else {
                continue;
            };
            let Some(cs) = self.styles.get(&id) else {
                continue;
            };
            let Some(tid) = self.taffy_node.get(&id).copied() else {
                continue;
            };
            let mut ts = crate::layout::map_style(cs, &media);
            if ts.position != TPos::Relative {
                continue; // 作者显式定位（absolute/fixed）：不动
            }
            ts.position = TPos::Absolute;
            ts.inset = Rect {
                top: LPA::length(0.0),
                right: LPA::auto(),
                bottom: LPA::auto(),
                left: LPA::length(0.0),
            };
            overrides.push((tid, ts));
        }
        for (tid, style) in overrides {
            let _ = self.taffy.set_style(tid, style);
        }
    }

    fn rebuild_taffy(&mut self) {
        self.taffy = SendSyncTaffy(taffy::TaffyTree::new());
        self.taffy_node.clear();
        self.taffy_root = None;
        self.calc_deferred.clear();
        self.taffy_parent.clear();
        self.abs_cb.clear();
        self.abs_structured = false;
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        // wrapper 重构映射一并作废（内表 taffy 节点随整树销毁）。
        self.taffy_table_inner.clear();
        self.table_row_anon.clear();
        self.multicols.clear();
        self.multicol_state.clear();
        if self.root_key.is_some() {
            let root = self.tree.root();
            let tid = self.build_taffy_subtree(root, None);
            // 合成视口根（ICB）：树根成为其子，树根自身的 margin 得以生效
            //（taffy 不应用根节点 margin；CSS 中根盒 margin 相对初始包含块生效）。
            let viewport = self
                .taffy
                .new_leaf(taffy::prelude::Style {
                    display: taffy::prelude::Display::Block,
                    ..Default::default()
                })
                .expect("viewport root");
            self.taffy
                .set_children(viewport, &[tid])
                .expect("viewport root children");
            // ①calc 直通：树根的结算基准 = 视口根内容尺寸。
            self.taffy_parent.insert(tid, viewport);
            self.taffy_root = Some(viewport);
        }
        self.dirty_struct = false;
        self.dirty_style = true; // 新树需重贴样式
    }

    fn build_taffy_subtree(
        &mut self,
        id: NodeId,
        parent_tid: Option<taffy::NodeId>,
    ) -> taffy::NodeId {
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        let mut child_ids: Vec<taffy::NodeId> = Vec::new();
        let tid = if children.is_empty() {
            self.taffy
                .new_leaf(taffy::prelude::Style::default())
                .expect("taffy leaf creation")
        } else {
            child_ids = children
                .iter()
                .map(|c| self.build_taffy_subtree(*c, None))
                .collect();
            self.taffy
                .new_with_children(taffy::prelude::Style::default(), &child_ids)
                .expect("taffy node creation")
        };
        self.taffy_node.insert(id, tid);
        // ①calc 直通：父链（后序回填——子 tid 已知，自身 tid 现创建）。
        if let Some(p) = parent_tid {
            self.taffy_parent.insert(tid, p);
        }
        for ct in &child_ids {
            self.taffy_parent.insert(*ct, tid);
        }
        tid
    }

    /// G1（ADR-0032）：restyle 提交点 reconciliation——新计算值 vs
    /// before-change 值（styles 表）逐槽对账，启动/重定向/取消过渡
    /// （css-transitions-2 §3 三则）。判定序（规格精化，在案偏差）：
    /// ①新值==生效值（含过渡采样中间值）→ 取消该槽活动过渡；
    /// ②combined duration（duration+delay）≤0 → 取消 + 不启动（值即刻
    /// 跳变为新级联值）；
    /// ③活动过渡存在且新值==to → 保持运行（不重启时钟——无关 restyle
    /// 不复位动画进度，偏离任务规格"值相同→取消"字面）；
    /// ④其余 → 启动/重定向：from=当前插值中间值（活动过渡）否则生效值，
    /// start=当前帧时间；
    /// ⑤可插值对（lerp_decl 分类探针）才可 normal 过渡；离散对需
    /// transition-behavior:allow-discrete 才启动，否则立即跳变；
    /// ⑥（偏差）目标槽被活动动画覆盖（animation 命中且 duration>0）时
    /// 不启动新过渡——动画层高于过渡层，启动后首帧即被覆写，徒留隐形
    /// 计时器；已运行过渡不受影响（动画结束后采样值重现，ADR-0032 边界）。
    /// transition-* 与 animation-* 描述符槽自身不可过渡（文档未定义其
    /// 动画性，且自指无意义）。
    fn reconcile_transitions(&mut self, id: NodeId, cs: &ComputedStyle) {
        use crate::css::property::{
            DeclValue, PropertyId as P, TimingFn, TransitionBehavior, TransitionTarget, lerp_decl,
        };

        // 快路径：无活动过渡且新声明无正时长 → 无启动面（全局稳态下每
        // 节点仅一次 map 探测 + 小列表扫描）。
        let active_empty = self.transitions.get(&id).is_none_or(Vec::is_empty);
        let durations_zero = match cs.get(P::TransitionDuration) {
            Some(DeclValue::TransitionTime(l)) => l.0.iter().all(|&d| d <= 0.0),
            _ => true,
        };
        if active_empty && durations_zero {
            return;
        }
        // before-change 值；首次 restyle（无旧值）不产生过渡。
        let Some(old) = self.styles.get(&id) else {
            return;
        };
        // 目标属性列表（after-change 的 transition-property）
        let Some(DeclValue::TransitionProperty(props)) = cs.get(P::TransitionProperty) else {
            return;
        };
        if props.0.is_empty() {
            return;
        }
        // 候选槽展开：None 跳过；All = 全部可过渡槽（ALL 表剔除描述符，
        // 配对下标=槽自身位序）；Ident = 属性名路由（未知名=不产生过渡）。
        let mut candidates: Vec<(usize, P)> = Vec::new();
        for (k, target) in props.0.iter().enumerate() {
            match target {
                TransitionTarget::None => {}
                TransitionTarget::All => {
                    for (i, &pid) in P::ALL.iter().enumerate() {
                        if !is_unanimatable_descriptor(pid) {
                            candidates.push((i, pid));
                        }
                    }
                }
                TransitionTarget::Ident(name) => {
                    if let Some(pid) = P::from_css_name(name)
                        && !is_unanimatable_descriptor(pid)
                    {
                        candidates.push((k, pid));
                    }
                }
            }
        }
        if candidates.is_empty() {
            return;
        }
        let durations: &[f32] = match cs.get(P::TransitionDuration) {
            Some(DeclValue::TransitionTime(l)) => &l.0,
            _ => &[],
        };
        let timings: &[TimingFn] = match cs.get(P::TransitionTimingFunction) {
            Some(DeclValue::TransitionTiming(l)) => &l.0,
            _ => &[],
        };
        let delays: &[f32] = match cs.get(P::TransitionDelay) {
            Some(DeclValue::TransitionTime(l)) => &l.0,
            _ => &[],
        };
        let behavior = match cs.get(P::TransitionBehavior) {
            Some(DeclValue::TransitionBehavior(b)) => *b,
            _ => TransitionBehavior::Normal,
        };
        let now = self.now as f32;
        let dark = self.media.dark;
        let anim_covered = self.animation_covered_slots(cs);
        let has_active = !active_empty;
        let mut actions: Vec<(P, Option<ActiveTransition>)> = Vec::new();
        for (k, pid) in candidates {
            let (Some(ov), Some(nv)) = (old.get(pid), cs.get(pid)) else {
                continue;
            };
            let active_t = if has_active {
                self.transitions
                    .get(&id)
                    .and_then(|l| l.iter().find(|t| t.pid == pid))
            } else {
                None
            };
            let dur = if durations.is_empty() {
                0.0
            } else {
                durations[k % durations.len()]
            };
            let delay = if delays.is_empty() {
                0.0
            } else {
                delays[k % delays.len()]
            };
            let tim = if timings.is_empty() {
                TimingFn::Ease
            } else {
                timings[k % timings.len()]
            };
            match active_t {
                Some(t) => {
                    // ③新值 == to → 保持运行（不重启时钟）。放在最前：容器
                    // 规则收敛环 pass2 重算级联不变（ov==nv 采样表象）时，
                    // 不得取消 pass1 刚启动的过渡（css-transitions §3 终值
                    // 对账语义；偏差①的精化边界，ADR-0032）。
                    if nv == &t.to {
                        continue;
                    }
                    // ①新级联 == 当前生效值（== 采样中间值）→ 取消
                    //（视觉等效：Chromium 走零跨度过渡，同效）。
                    if ov == nv {
                        actions.push((pid, None));
                        continue;
                    }
                    // ②combined duration ≤ 0（新声明）→ 取消
                    if dur + delay <= 0.0 {
                        actions.push((pid, None));
                        continue;
                    }
                    // ④重定向：from = 当前时刻插值值
                    let from = transition_sample(t, now, dark);
                    actions.push((
                        pid,
                        Some(ActiveTransition {
                            pid,
                            from,
                            to: nv.clone(),
                            start_time: now,
                            duration: dur,
                            delay,
                            easing: tim,
                        }),
                    ));
                }
                None => {
                    // ①值相同 → 无动作（稳态快路径核心判定）
                    if ov == nv {
                        continue;
                    }
                    // ②combined duration ≤ 0 → 不启动（值即刻跳变）
                    if dur + delay <= 0.0 {
                        continue;
                    }
                    // ⑤可插值性探针（0.5 中点）：可插值对才可 normal 过渡；
                    // 离散对需 allow-discrete 才启动，否则立即跳变。
                    let interpolable = lerp_decl(ov, nv, 0.5, dark).is_some();
                    if !interpolable && behavior != TransitionBehavior::AllowDiscrete {
                        continue;
                    }
                    // ⑥活动动画覆盖槽不启动新过渡（偏差⑥在案）
                    if anim_covered
                        .as_ref()
                        .is_some_and(|s| s.contains(&pid.slot()))
                    {
                        continue;
                    }
                    actions.push((
                        pid,
                        Some(ActiveTransition {
                            pid,
                            from: ov.clone(),
                            to: nv.clone(),
                            start_time: now,
                            duration: dur,
                            delay,
                            easing: tim,
                        }),
                    ));
                }
            }
        }
        if actions.is_empty() {
            return;
        }
        if !has_active && actions.iter().all(|(_, a)| a.is_none()) {
            return; // 仅取消但无活动条目 → 无需动表（稳态不建空键）
        }
        let entry = self.transitions.entry(id).or_default();
        for (pid, action) in actions {
            match action {
                None => entry.retain(|t| t.pid != pid),
                Some(t) => match entry.iter_mut().find(|e| e.pid == pid) {
                    Some(e) => *e = t,
                    None => entry.push(t),
                },
            }
        }
        if entry.is_empty() {
            self.transitions.remove(&id);
        }
    }

    /// G1（ADR-0032）：该节点活动动画覆盖的槽位集（animation-name 命中
    /// @keyframes 且 duration>0 时，关键帧声明的 Parsed 槽位集合）——
    /// reconciliation 抑制这些槽的新过渡启动（偏差⑥）。
    fn animation_covered_slots(
        &self,
        cs: &ComputedStyle,
    ) -> Option<std::collections::BTreeSet<usize>> {
        let mut slots = std::collections::BTreeSet::new();
        // P7-②：多组并集——任一组命中的槽位都抑制新过渡启动。
        for g in Self::anim_groups(cs) {
            let Some(name) = g.name else { continue };
            if g.duration <= 0.0 {
                continue;
            }
            let Some(rule) = self
                .extra_sheets
                .iter()
                .rev()
                .find_map(|(_, _, s)| s.keyframes.iter().find(|r| r.name == name))
                .or_else(|| self.sheet.keyframes.iter().find(|r| r.name == name))
            else {
                continue;
            };
            for f in &rule.frames {
                for d in &f.declarations.decls {
                    slots.insert(d.id.slot());
                }
            }
        }
        if slots.is_empty() { None } else { Some(slots) }
    }

    /// G1（ADR-0032）：transition 采样挂点——对每个活动过渡按 now 求插值
    /// 并写回对应槽位：延迟段保持 from；进度 ≥1 写 to 并移除条目（终值
    /// = to 保持）；进行中按缓动求值 lerp_decl，不可插值对（allow-discrete
    /// 启动的离散过渡）按离散规则在 50% 翻转。表空早退（稳态零写入）。
    fn sample_transitions(&mut self) {
        use crate::css::property::lerp_decl;
        if self.transitions.is_empty() {
            return;
        }
        let dark = self.media.dark;
        let now = self.now as f32;
        let ids: Vec<NodeId> = self.transitions.keys().copied().collect();
        for id in ids {
            let Some(list) = self.transitions.get_mut(&id) else {
                continue;
            };
            let Some(cs) = self.styles.get_mut(&id) else {
                // 样式表孤儿（树移除竞态）：条目作废
                list.clear();
                continue;
            };
            list.retain_mut(|t| {
                let local = now - t.start_time - t.delay;
                if local < 0.0 {
                    // 延迟段：保持 from（负延迟=快进，直接落进行中段）
                    cs.set_value(t.pid, t.from.clone());
                    return true;
                }
                if t.duration <= 0.0 {
                    // 0s 过渡：终值即刻生效
                    cs.set_value(t.pid, t.to.clone());
                    return false;
                }
                let p = local / t.duration;
                if p >= 1.0 {
                    cs.set_value(t.pid, t.to.clone());
                    return false;
                }
                let eased = t.easing.sample(p);
                let v = lerp_decl(&t.from, &t.to, eased, dark).unwrap_or_else(|| {
                    // 离散对（allow-discrete 过渡）：50% 翻转
                    if eased < 0.5 {
                        t.from.clone()
                    } else {
                        t.to.clone()
                    }
                });
                cs.set_value(t.pid, v);
                true
            });
            if list.is_empty() {
                self.transitions.remove(&id);
            }
        }
    }

    /// P7-②：动画组视图——从计算样式提取动画组列表（CSS 多动画：
    /// 组数 = name 列表长度，各描述符列表按 `i % len` 循环补齐，
    /// 缺省值兜底：duration/delay=0s、iterations=1、ease/normal/none）。
    /// 兼容旧单值变体（initial_value 遗留）；name 全空 → 无组。
    fn anim_groups(cs: &ComputedStyle) -> Vec<AnimGroupSpec> {
        use crate::css::property::{AnimDirection, AnimFillMode, DeclValue, PropertyId, TimingFn};
        let names: Vec<Option<String>> = match cs.get(PropertyId::AnimationName) {
            Some(DeclValue::AnimationNameList(v)) => v.iter().cloned().collect(),
            Some(DeclValue::AnimationName(n)) => vec![n.clone()],
            _ => return Vec::new(),
        };
        let n = names.len();
        if n == 0 || names.iter().all(|o| o.is_none()) {
            return Vec::new();
        }
        let times: Vec<f32> = match cs.get(PropertyId::AnimationDuration) {
            Some(DeclValue::AnimationTimeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTime(s)) => vec![*s],
            _ => Vec::new(),
        };
        let delays: Vec<f32> = match cs.get(PropertyId::AnimationDelay) {
            Some(DeclValue::AnimationTimeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTime(s)) => vec![*s],
            _ => Vec::new(),
        };
        let iters: Vec<f32> = match cs.get(PropertyId::AnimationIterationCount) {
            Some(DeclValue::AnimationIterationList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationIteration(x)) => vec![*x],
            _ => Vec::new(),
        };
        let timings: Vec<TimingFn> = match cs.get(PropertyId::AnimationTimingFunction) {
            Some(DeclValue::AnimationTimingList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationTiming(t)) => vec![*t],
            _ => Vec::new(),
        };
        let dirs: Vec<AnimDirection> = match cs.get(PropertyId::AnimationDirection) {
            Some(DeclValue::AnimationDirectionList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationDirection(d)) => vec![*d],
            _ => Vec::new(),
        };
        let fills: Vec<AnimFillMode> = match cs.get(PropertyId::AnimationFillMode) {
            Some(DeclValue::AnimationFillModeList(v)) => v.iter().copied().collect(),
            Some(DeclValue::AnimationFillMode(f)) => vec![*f],
            _ => Vec::new(),
        };
        let pick =
            |v: &[f32], dflt: f32, i: usize| v.get(i % v.len().max(1)).copied().unwrap_or(dflt);
        names
            .into_iter()
            .enumerate()
            .map(|(i, name)| AnimGroupSpec {
                name,
                duration: pick(&times, 0.0, i),
                delay: pick(&delays, 0.0, i),
                iterations: pick(&iters, 1.0, i),
                timing: timings
                    .get(i % timings.len().max(1))
                    .copied()
                    .unwrap_or(TimingFn::Ease),
                direction: dirs
                    .get(i % dirs.len().max(1))
                    .copied()
                    .unwrap_or(AnimDirection::Normal),
                fill: fills
                    .get(i % fills.len().max(1))
                    .copied()
                    .unwrap_or(AnimFillMode::None),
            })
            .collect()
    }

    /// @keyframes 动画采样（第五批⑰）：对声明了 animation-name 且命中
    /// @keyframes 的节点，按 now（宿主帧推进，秒）采样关键帧轨道并覆写
    /// 计算样式。动画层高于作者级联（CSS：动画覆盖普通声明，仅
    /// !important 更高——分层为残余偏差）；缓动按关键帧段施加（CSS 时序
    /// 函数语义）；不可插值对按离散规则（段进度<0.5 取前帧）。
    /// P7-②：多动画组——逐组独立采样（组数 = name 列表长度，描述符
    /// 循环补齐）；后组胜同槽覆写；underlying 快照节点级共享（首组
    /// 捕获级联值），结束无填充组逐槽恢复底层值。
    fn apply_animations(&mut self) {
        use crate::css::property::{AnimDirection, AnimFillMode, DeclValue, PropertyId};
        use std::collections::BTreeMap;
        let dark = self.media.dark;
        let now = self.now as f32;
        // B2：跨表 @keyframes 查找（后表同名覆盖前表——文档级最后声明胜；
        // 字段级不相交借用：sheet/extras 不可变 + styles 可变）
        let sheet = &self.sheet;
        let extras = &self.extra_sheets;
        let styles = &mut self.styles;
        let anim_underlying = &mut self.anim_underlying;
        for (anim_id, cs) in styles.iter_mut() {
            let groups = Self::anim_groups(cs);
            if groups.is_empty() {
                continue;
            }
            // P7-② 组预处理：逐组查找 @keyframes 并收集轨道；槽位并集
            // 供 underlying 快照对齐。
            let mut prepared: PreparedAnimGroups = Vec::new();
            let mut slot_union: std::collections::BTreeSet<PropertyId> =
                std::collections::BTreeSet::new();
            for g in &groups {
                let Some(name) = &g.name else { continue };
                let Some(rule) = extras
                    .iter()
                    .rev()
                    .find_map(|(_, _, s)| s.keyframes.iter().find(|r| r.name == *name))
                    .or_else(|| sheet.keyframes.iter().find(|r| r.name == *name))
                else {
                    continue;
                };
                if g.duration <= 0.0 || rule.frames.is_empty() {
                    continue;
                }
                // 轨道收集（Parsed 声明；var() 载体不入轨——MVP 偏差）。
                let mut tracks: BTreeMap<PropertyId, Vec<(f32, &DeclValue)>> = BTreeMap::new();
                for f in &rule.frames {
                    for d in &f.declarations.decls {
                        if let crate::css::decl::DeclSource::Parsed(v) = &d.value {
                            tracks.entry(d.id).or_default().push((f.offset, v));
                            slot_union.insert(d.id);
                        }
                    }
                }
                prepared.push((g, tracks));
            }
            if prepared.is_empty() {
                continue;
            }
            // G1（ADR-0032）：底层值副本管理（P7-② 节点级共享）——首见
            // 动画快照各组槽位并集的当前值（覆写前 = 级联/底层值）；已有
            // 条目则槽值 ≠ 上帧写入值 = 外部重算（restyle 重建 cs）→ 刷新
            // 快照，使「恢复 underlying」语义始终锚定最新级联值；槽集随
            // 并集对齐（换动画名自愈）。
            {
                let entry = anim_underlying.entry(*anim_id).or_default();
                if entry.is_empty() {
                    for pid in &slot_union {
                        if let Some(v) = cs.value(*pid) {
                            entry.push((*pid, v.clone(), v.clone()));
                        }
                    }
                } else {
                    entry.retain(|(pid, _, _)| slot_union.contains(pid));
                    for pid in &slot_union {
                        if !entry.iter().any(|(p, _, _)| p == pid)
                            && let Some(v) = cs.value(*pid)
                        {
                            entry.push((*pid, v.clone(), v.clone()));
                        }
                    }
                    for (pid, under, last) in entry.iter_mut() {
                        if let Some(cur) = cs.value(*pid)
                            && cur != last
                        {
                            *under = cur.clone();
                            *last = cur.clone();
                        }
                    }
                }
            }
            // P7-② 逐组采样：后组胜同槽覆写；结束无填充组恢复其槽位
            // 底层值（后续组仍可覆写同槽——CSS 复合序）。
            for (g, tracks) in &prepared {
                let local = now - g.delay;
                // 采样点：未开始（backwards/both → 0）/进行中/已结束
                // （forwards/both → 1）；其余阶段用底层值（不覆写）
                let total = g.duration * g.iterations.max(0.0);
                let p_eff: Option<f32> = if local < 0.0 {
                    matches!(g.fill, AnimFillMode::Backwards | AnimFillMode::Both).then_some(0.0)
                } else if g.iterations.is_infinite() || local < total {
                    let raw = local / g.duration;
                    let cycle_index = raw.floor();
                    let seg = raw - cycle_index;
                    // 方向折叠（缓动在段内施加）
                    let folded = match g.direction {
                        AnimDirection::Normal => seg,
                        AnimDirection::Reverse => 1.0 - seg,
                        AnimDirection::Alternate => {
                            if (cycle_index as i64) % 2 == 0 {
                                seg
                            } else {
                                1.0 - seg
                            }
                        }
                        AnimDirection::AlternateReverse => {
                            if (cycle_index as i64) % 2 == 0 {
                                1.0 - seg
                            } else {
                                seg
                            }
                        }
                    };
                    Some(folded)
                } else {
                    matches!(g.fill, AnimFillMode::Forwards | AnimFillMode::Both).then_some(1.0)
                };
                let Some(p_eff) = p_eff else {
                    // G1：已结束且无 forwards/both 填充 → 恢复该组槽位底层值
                    //（css-animations-1：结束后回落 underlying，不残留最后
                    // 动画采样值；多组下后续组仍可覆写同槽）。
                    if let Some(entry) = anim_underlying.get(anim_id) {
                        for pid in tracks.keys() {
                            if let Some((_, under, _)) = entry.iter().find(|(p, _, _)| p == pid) {
                                cs.set_value(*pid, under.clone());
                            }
                        }
                    }
                    continue;
                };
                for (pid, track) in tracks.iter() {
                    let mut track = track.clone();
                    track
                        .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    // p < 首帧 offset 或 > 末帧 offset → 合成帧取 underlying
                    //（CSS：缺 0%/100% 关键帧时以底层值补帧）→ 不覆写
                    if p_eff < track[0].0 || p_eff > track[track.len() - 1].0 {
                        continue;
                    }
                    let sampled = track
                        .iter()
                        .find(|(o, _)| *o == p_eff)
                        .map(|(_, v)| (*v).clone())
                        .or_else(|| {
                            // 区间括位插值；不可插值 → 离散（段进度<0.5 取前帧）
                            let mut seg_pair: Option<(&f32, &DeclValue, &f32, &DeclValue)> = None;
                            for w in track.windows(2) {
                                if w[0].0 <= p_eff && p_eff <= w[1].0 {
                                    seg_pair = Some((&w[0].0, w[0].1, &w[1].0, w[1].1));
                                    break;
                                }
                            }
                            let (o0, v0, o1, v1) = seg_pair?;
                            let local_t = if o1 > o0 {
                                (p_eff - o0) / (o1 - o0)
                            } else {
                                0.0
                            };
                            let eased = g.timing.sample(local_t);
                            crate::css::property::lerp_decl(v0, v1, eased, dark)
                                .or_else(|| Some(if eased < 0.5 { v0.clone() } else { v1.clone() }))
                        });
                    if let Some(v) = sampled {
                        cs.set_value(*pid, v);
                    }
                }
            }
            // P7-②：全部组结束（local ≥ total）——forwards 组终值已固定
            // 于槽位、无填充组已恢复底层值 → 副本无后续用途，清除。
            let all_over = prepared.iter().all(|(g, _)| {
                let local = now - g.delay;
                let total = g.duration * g.iterations.max(0.0);
                local >= total
            });
            if all_over {
                anim_underlying.remove(anim_id);
            }
            // G1：记录本帧写入值（外部重算检测基准——下帧槽值 ≠ 此值即
            // 视为 restyle 重算，刷新底层值快照）。
            if let Some(entry) = anim_underlying.get_mut(anim_id) {
                for (pid, _, last) in entry.iter_mut() {
                    if let Some(v) = cs.value(*pid) {
                        *last = v.clone();
                    }
                }
            }
        }
    }

    /// ①calc 直通：百分比 calc 结算循环（设计决策见 layout.rs DeferredRaw
    /// 注）。首遍布局后，以父节点已布局内容尺寸为基准解析延迟 calc，
    /// 回写固定值并重算；循环至无变更（上限 3 遍——百分比基准恒为祖先
    /// 派生（DAG），逐遍稳定一层；3 层内链路与浏览器单遍语义一致，
    /// 更深链路记偏差待重估）。
    /// A9：延迟 cq 条目的容器查询基值——沿 taffy 父链上溯找最近
    /// container-type≠Normal 祖先，取其本帧布局内容盒（settle 在
    /// compute_layout 之后=本帧新值）；InlineSize 容器块轴回落视口高
    ///（small viewport 语义）；无容器祖先=视口（规范回落）。
    fn cq_basis(&self, node: taffy::NodeId, viewport: (f32, f32)) -> (f32, f32) {
        let rev: HashMap<taffy::NodeId, NodeId> =
            self.taffy_node.iter().map(|(k, v)| (*v, *k)).collect();
        let mut cur = self.taffy_parent.get(&node).copied();
        while let Some(tid) = cur {
            if let Some(&eid) = rev.get(&tid)
                && let Some(cs) = self.styles.get(&eid)
            {
                let ct = cs.container_type();
                if ct != crate::css::property::ContainerType::Normal
                    && let Ok(pl) = self.taffy.layout(tid)
                {
                    let w = pl.size.width
                        - pl.border.left
                        - pl.border.right
                        - pl.padding.left
                        - pl.padding.right;
                    let h = pl.size.height
                        - pl.border.top
                        - pl.border.bottom
                        - pl.padding.top
                        - pl.padding.bottom;
                    let hh = if ct == crate::css::property::ContainerType::Size {
                        h
                    } else {
                        viewport.1
                    };
                    return (w.max(0.0), hh.max(0.0));
                }
            }
            cur = self.taffy_parent.get(&tid).copied();
        }
        viewport
    }

    /// A9：节点字体相对单位度量——font-family 首个具名族匹配注册字体
    ///（add_font 时探测；大小写不敏感），未命中=近似缺省。
    fn metrics_for(&self, cs: &ComputedStyle) -> crate::css::value::FontMetrics {
        for fam in cs.font_family().0.iter() {
            if let crate::css::property::FamilyName::Named(n) = fam {
                for (names, m) in &self.font_metrics {
                    if names.iter().any(|f| f.eq_ignore_ascii_case(n)) {
                        return *m;
                    }
                }
            }
        }
        crate::css::value::FontMetrics::default()
    }

    /// P9-3（css-lists-3 §3.1）：list-item 宿主的可见文本 marker 前进宽。
    /// 条件 = 宿主 display:list-item + marker 节点存在 + 文本非空 + 测量
    /// 就绪（sync_pseudo_text 供 measures；非 text 特性恒空 → None）。
    /// 返回 None = marker 抑制或图像盒（无文本合成）。
    fn marker_advance_of(&self, host: NodeId) -> Option<f32> {
        if self.styles.get(&host)?.display() != crate::css::property::Display::ListItem {
            return None;
        }
        let m = *self.pseudo_ids.get(&(host, 2))?;
        let text = self.tree.node(m).text.as_ref()?;
        if text.is_empty() {
            return None;
        }
        self.measures.get(&m).map(|(w, _)| *w)
    }

    /// 叶换行约束宽=包含块内容宽（父 border-box − padding − 有效 border；
    /// multicol 重挂叶=幻影列宽无内缩）。T5c-2 remeasure 与 F2 截断结算
    /// 共用（ADR-0022 D2）。P9-3：父为带可见文本 marker 的 list-item →
    /// 减 marker 前进宽（inside 语义：首行文本自 marker 右缘起排；后续
    /// 行同宽收缩=B·豁免近似，ADR-0041）。
    #[cfg(feature = "text")]
    fn leaf_wrap_width(&self, id: NodeId) -> Option<f32> {
        let parent_id = *self.parents.get(&id)?;
        let ptid = *self.taffy_node.get(&parent_id)?;
        let ctid = *self.taffy_node.get(&id)?;
        // ③multi-column：子节点重挂幻影列后，换行包含块 = 幻影列
        //（无 padding/border，内缩为 0）。
        let rehomed = self.taffy_parent.get(&ctid).is_some_and(|&p| p != ptid);
        if rehomed {
            let effp = *self.taffy_parent.get(&ctid)?;
            let pl = self.taffy.layout(effp).ok()?;
            return Some(pl.size.width.max(0.0));
        }
        let marker_adv = self.marker_advance_of(parent_id).unwrap_or(0.0);
        let pl = self.taffy.layout(ptid).ok()?;
        let pcs = self.styles.get(&parent_id)?;
        let rctx = crate::css::value::ResolveCtx {
            em: pcs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: self.map_env().viewport_w,
            viewport_h: self.map_env().viewport_h,
            ..crate::css::value::ResolveCtx::base(
                pcs.font_size_px(),
                16.0,
                self.map_env().viewport_w,
                self.map_env().viewport_h,
            )
        };
        let inset: f32 = [
            (crate::css::property::PropertyId::PaddingLeft, None),
            (crate::css::property::PropertyId::PaddingRight, None),
            (
                crate::css::property::PropertyId::BorderLeftWidth,
                Some(crate::css::property::PropertyId::BorderLeftStyle),
            ),
            (
                crate::css::property::PropertyId::BorderRightWidth,
                Some(crate::css::property::PropertyId::BorderRightStyle),
            ),
        ]
        .iter()
        .filter_map(|(pid, style_pid)| used_h_inset(pcs, *pid, *style_pid, &rctx))
        .sum();
        Some((pl.size.width - inset - marker_adv).max(0.0))
    }

    /// F2（ADR-0022 D2）：ellipsis 截断文本——二分最长字符前缀使
    /// width(prefix)+"…" ≤ avail；"…" 本身超宽 → 空串（超窄容器）。
    /// 字节安全=chars().take（span 字节区间仍为原文本前缀，绘制端
    /// map_start/map_end 截断语义不变）。
    #[cfg(feature = "text")]
    fn make_ellipsis_text(
        &mut self,
        text: &str,
        cs: &ComputedStyle,
        span_refs: &[(u32, u32, &ComputedStyle)],
        avail: f32,
    ) -> Option<String> {
        const ELLIPSIS: &str = "\u{2026}";
        let env = self.map_env();
        let (ew, _eh) = self.text.measure_rich(ELLIPSIS, cs, span_refs, None, &env);
        if ew > avail {
            return Some(String::new());
        }
        let budget = avail - ew;
        let total_chars = text.chars().count();
        let mut lo = 0usize;
        let mut hi = total_chars;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let prefix: String = text.chars().take(mid).collect();
            let (pw, _ph) = self.text.measure_rich(&prefix, cs, span_refs, None, &env);
            if pw <= budget {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let mut out: String = text.chars().take(lo).collect();
        out.push_str(ELLIPSIS);
        Some(out)
    }

    /// F2（ADR-0022 D3）：line-clamp 截断文本——二分最长字符前缀使
    /// measure(prefix+"…", avail).1 ≤ max_h=N*行高（折行测量）；全文本
    /// 不超行 → None（不截）。空解=仅"…"。
    #[cfg(feature = "text")]
    fn make_clamp_text(
        &mut self,
        text: &str,
        cs: &ComputedStyle,
        span_refs: &[(u32, u32, &ComputedStyle)],
        avail: f32,
        max_h: f32,
    ) -> Option<String> {
        const ELLIPSIS: &str = "\u{2026}";
        let with_ell = |s: &str| {
            let mut owned = String::with_capacity(s.len() + 3);
            owned.push_str(s);
            owned.push_str(ELLIPSIS);
            owned
        };
        let env = self.map_env();
        let (_w, fh) = self
            .text
            .measure_rich(&with_ell(text), cs, span_refs, Some(avail), &env);
        if fh <= max_h {
            return None;
        }
        let total_chars = text.chars().count();
        let mut lo = 0usize;
        let mut hi = total_chars;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let prefix: String = text.chars().take(mid).collect();
            let full = with_ell(&prefix);
            let (_w2, h2) = self
                .text
                .measure_rich(&full, cs, span_refs, Some(avail), &env);
            if h2 <= max_h {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(with_ell(&text.chars().take(lo).collect::<String>()))
    }

    /// F2（ADR-0022 D2）：text-overflow:ellipsis 截断结算——测量宽就绪
    /// 后逐 auto_text 叶检查：父（包含块）overflow 非 visible ∧ 父
    /// text-overflow=ellipsis ∧ 叶 nowrap ∧ 测量宽>包含块内容宽 →
    /// make_ellipsis_text 生成 text_overrides（绘制替换；盒几何不变）。
    /// overflow visible 不截（spec：仅裁剪语境生效）；多行溢出=line-clamp
    /// （D3）范围。每帧全量重算（幂等）。
    #[cfg(feature = "text")]
    fn apply_text_truncation(&mut self) {
        self.text_overrides.clear();
        // 两段化：phase1 不可变扫描收候选（measure_rich 需 &mut self.text，
        // 不能持 self.measures 迭代借用跨调用——remeasure 先例同构）。
        // mode=None=ellipsis（avail 宽预算）、Some(max_h)=line-clamp（行高
        // 预算）。
        let mut cands: Vec<TruncCandidate> = Vec::new();
        for (&id, &(mw, mh)) in self.measures.iter() {
            // 注：不设 auto_text 门——measures 本就只含文本叶测量；折行叶
            //（white-space normal）走 remeasure 路径、nowrap 叶走 restyle
            // 登记，两者都须可截断（D2 nowrap 语义由 want_ellipsis 门治）。
            let Some(&parent_id) = self.parents.get(&id) else {
                continue;
            };
            let Some(pcs) = self.styles.get(&parent_id) else {
                continue;
            };
            let clipped = matches!(
                pcs.overflow_x(),
                crate::css::property::Overflow::Hidden
                    | crate::css::property::Overflow::Clip
                    | crate::css::property::Overflow::Scroll
            );
            if !clipped {
                continue;
            }
            let Some(cs) = self.styles.get(&id).cloned() else {
                continue;
            };
            let Some(avail) = self.leaf_wrap_width(id) else {
                continue;
            };
            // D2 ellipsis 条件：叶 nowrap ∧ 测量宽>包含块内容宽。
            let leaf_nowrap = matches!(
                cs.white_space(),
                crate::css::property::WhiteSpace::NoWrap | crate::css::property::WhiteSpace::Pre
            );
            let want_ellipsis = pcs.text_overflow()
                == crate::css::property::TextOverflowKind::Ellipsis
                && leaf_nowrap
                && mw > avail;
            // D3 line-clamp 条件：clamp N≥1 ∧ 测量高>N*行高（折行叶，
            // 无 white-space 前置——单行 nowrap 叶 1 行永不超 N≥1）。
            let clamp_n = pcs.webkit_line_clamp();
            let env = self.map_env();
            // 行高缺省（normal）≈1.2em 近似（clamp 预算基准）。
            let lh = cs
                .resolved_line_height_px(&env)
                .unwrap_or_else(|| 1.2 * cs.font_size_px());
            let want_clamp = !want_ellipsis && clamp_n > 0 && mh > clamp_n as f32 * lh;
            if !want_ellipsis && !want_clamp {
                continue;
            }
            let Some(text) = self.tree.node(id).text.clone() else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
            let mode = if want_ellipsis {
                None
            } else {
                Some(clamp_n as f32 * lh)
            };
            cands.push((id, avail, text, cs, owned, mode));
        }
        for (id, avail, text, cs, owned, mode) in cands {
            let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
            let t = match mode {
                None => self.make_ellipsis_text(&text, &cs, &span_refs, avail),
                Some(max_h) => self.make_clamp_text(&text, &cs, &span_refs, avail, max_h),
            };
            if let Some(t) = t {
                self.text_overrides.insert(id, t);
            }
        }
    }

    /// F2（ADR-0022 D1）：pass 运行门——空输入跳过（廉价谓词；无输入
    /// 索引的 pass（浮盒/行内）默认 true，内部早退治理）。
    fn settle_should_run(&self, pass: SettlePassKind) -> bool {
        match pass {
            SettlePassKind::Calc => !self.calc_deferred.is_empty(),
            SettlePassKind::Tables => !self.tables.is_empty(),
            SettlePassKind::Columns => !self.multicols.is_empty(),
            SettlePassKind::Floats | SettlePassKind::Lines => true,
        }
    }

    /// F2（ADR-0022 D1）：pass 分发（原 frame 硬编码序的函数体不动）。
    fn settle_run_pass(&mut self, pass: SettlePassKind, viewport: (f32, f32)) {
        match pass {
            SettlePassKind::Calc => self.settle_calc(viewport),
            SettlePassKind::Tables => self.settle_tables(viewport),
            SettlePassKind::Columns => self.settle_columns(viewport),
            SettlePassKind::Floats => self.settle_floats(viewport),
            SettlePassKind::Lines => {
                #[cfg(feature = "text")]
                self.settle_lines(viewport);
                #[cfg(not(feature = "text"))]
                {
                    let _ = viewport;
                }
            }
        }
    }

    fn settle_calc(&mut self, viewport: (f32, f32)) {
        use crate::css::value::ResolveCtx;
        const MAX_SETTLE_PASSES: usize = 3;
        for _ in 0..MAX_SETTLE_PASSES {
            let mut updates: Vec<(taffy::NodeId, crate::layout::CalcAxis, f32)> = Vec::new();
            for d in &self.calc_deferred {
                let Some(&parent) = self.taffy_parent.get(&d.node) else {
                    continue;
                };
                let Ok(pl) = self.taffy.layout(parent) else {
                    continue;
                };
                let cw = pl.size.width
                    - pl.border.left
                    - pl.border.right
                    - pl.padding.left
                    - pl.padding.right;
                let ch = pl.size.height
                    - pl.border.top
                    - pl.border.bottom
                    - pl.padding.top
                    - pl.padding.bottom;
                // 三期③槽位扩展：flex-basis 百分比基=父容器主轴内容尺寸
                // （flex-direction 行向=宽、列向=高）；margin/padding 全族
                // 按 CSS 2.1 §8.3/§8.4 恒以包含块宽度为基（含 top/bottom）。
                let basis = match d.raw.axis {
                    crate::layout::CalcAxis::FlexBasis => {
                        let dir = self
                            .taffy
                            .style(parent)
                            .map(|s| s.flex_direction)
                            .unwrap_or(taffy::prelude::FlexDirection::Row);
                        let vertical = matches!(
                            dir,
                            taffy::prelude::FlexDirection::Column
                                | taffy::prelude::FlexDirection::ColumnReverse
                        );
                        if vertical { ch } else { cw }
                    }
                    axis => match axis.basis_axis() {
                        Some(crate::layout::CalcAxis::Width) => cw,
                        _ => ch,
                    },
                };
                // A9：容器查询基值结算期现查（本帧新布局）；字体度量取快照
                let cq = self.cq_basis(d.node, viewport);
                let ctx = ResolveCtx {
                    em: d.raw.em,
                    rem: d.raw.rem,
                    viewport_w: d.raw.vw,
                    viewport_h: d.raw.vh,
                    cq_w: cq.0,
                    cq_h: cq.1,
                    ch_per_em: d.raw.ch_per_em,
                    ex_per_em: d.raw.ex_per_em,
                    ic_per_em: d.raw.ic_per_em,
                };
                let Some(px) = d.raw.expr.resolve(&ctx, basis) else {
                    continue;
                };
                let Some(style) = self.taffy.style(d.node).ok() else {
                    continue;
                };
                let mut target = style.clone();
                d.raw.axis.write(&mut target, px);
                if target != *style {
                    updates.push((d.node, d.raw.axis, px));
                }
            }
            if updates.is_empty() {
                return;
            }
            // 重复条目幂等：同值 set_style 二次应用无副作用（首次应用后
            // changed 检查即拦截），无需去重。
            for (node, slot, px) in updates {
                let Ok(mut style) = self.taffy.style(node).cloned() else {
                    continue;
                };
                slot.write(&mut style, px);
                let _ = self.taffy.set_style(node, style);
            }
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
        }
    }

    /// 三期④：节点 display 快查（缺样式视为 None）。
    fn display_of(&self, id: NodeId) -> Option<crate::css::property::Display> {
        self.styles.get(&id).map(|cs| cs.display())
    }

    /// ②table：列模板结算——首遍布局给出表内容宽后，按首行单元格声明宽
    /// （定宽 px / 百分比 / auto）计算列模板回写各 table-row 的单行 Grid；
    /// 全等缓存则免重排（稳态帧零额外布局 pass）。嵌套表外层先行：变更
    /// 表格 wrapper 重构（CSS 2.2 §17.4 匿名盒结构侧）：display:table 的
    /// taffy 节点降级为 wrapper（Block、auto 尺寸、零 margin/padding/border、
    /// overflow 复位），新建内表节点承载表盒自身全部样式（map_style 全量；
    /// position 强制 Relative + inset 清零——inner 需充当其 absolute 后代的
    /// taffy 包含块）；表盒不当块级子件（block/flex/grid/inline-table 等）
    /// 提升到 wrapper（置于表盒上方，DOM 序——table-anon 金标实测：
    /// div.c 先于表盒、宽 = 包含块宽），表格内部件（行/行组/单元格/标题）
    /// 挂内表。collect 读内表矩形；settle_tables 以内表为结算基准。每帧
    /// 重应用（restyle 会把原样式写回 wrapper tid）。幂等：内表节点按
    /// taffy_table_inner 在场复用。表子树整体豁免于结构同步（settle_
    /// absolute_anchors v1 契约），表结构由本函数与 settle_tables 独占。
    /// 已知边界：absolute 子件随内表（inner 的 Relative 语境承接 taffy
    /// 锚定，语义 ≈ Chromium 锚表盒 padding box）；提升件若 DOM 序晚于
    /// 表格内部件，本实现仍置表盒上方（Chromium 同场景插序未建模）。
    fn settle_table_fixup(&mut self) {
        if self.tables.is_empty() {
            return;
        }
        let tables = self.tables.clone();
        for table in tables {
            let Some(&wtid) = self.taffy_node.get(&table) else {
                continue;
            };
            let pristine = match self.styles.get(&table) {
                Some(cs) => crate::layout::map_style(cs, &self.map_env()),
                None => continue,
            };
            // 内表 = 表盒自身样式；position 强制 Relative（abs 后代包含块），
            // inset 清零（偏移语义归 wrapper）。
            let mut inner_style = pristine.clone();
            inner_style.position = taffy::prelude::Position::Relative;
            inner_style.inset = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            let itid = match self.taffy_table_inner.get(&table).copied() {
                Some(itid) => itid,
                None => {
                    let itid = self
                        .taffy
                        .new_leaf(inner_style.clone())
                        .expect("table inner leaf");
                    self.taffy_table_inner.insert(table, itid);
                    itid
                }
            };
            let _ = self.taffy.set_style(itid, inner_style);
            self.taffy_parent.insert(itid, wtid);
            // 子件分类（样式树序）：表格内部件/none/absolute → 内表；
            // 其余（不当块级）→ wrapper 提升。
            let mut hoisted: Vec<taffy::NodeId> = Vec::new();
            let mut proper: Vec<taffy::NodeId> = Vec::new();
            for &c in self.tree.children(table) {
                let Some(&ctid) = self.taffy_node.get(&c) else {
                    continue;
                };
                let disp = self.display_of(c);
                let internal = matches!(
                    disp,
                    Some(
                        crate::css::property::Display::TableRow
                            | crate::css::property::Display::TableRowGroup
                            | crate::css::property::Display::TableCell
                            | crate::css::property::Display::TableCaption
                            | crate::css::property::Display::None
                    )
                ) || disp.is_none();
                let is_abs = self
                    .styles
                    .get(&c)
                    .is_some_and(|ccs| ccs.position() == crate::css::property::Position::Absolute);
                if internal || is_abs {
                    proper.push(ctid);
                    self.taffy_parent.insert(ctid, itid);
                } else {
                    hoisted.push(ctid);
                    self.taffy_parent.insert(ctid, wtid);
                }
            }
            // wrapper：填满包含块的 Block；表元素声明的尺寸/边距/内边距/
            // 边框/overflow 全部移交内表，wrapper 只承担提升件堆叠与
            // 定位语境（position/inset 保留自 pristine）。
            let mut ws = pristine;
            ws.display = taffy::prelude::Display::Block;
            // 注意 Dimension 的零 = 定值 0px，wrapper 尺寸须 auto（内容推导）。
            ws.size = taffy::prelude::Size {
                width: taffy::prelude::Dimension::auto(),
                height: taffy::prelude::Dimension::auto(),
            };
            ws.min_size = taffy::prelude::Size {
                width: taffy::prelude::TaffyZero::ZERO,
                height: taffy::prelude::TaffyZero::ZERO,
            };
            // max_size 的零同样是 0px 定值钳（默认应为 auto）——不能写 ZERO。
            ws.max_size = taffy::prelude::Size {
                width: taffy::prelude::LengthPercentageAuto::auto(),
                height: taffy::prelude::LengthPercentageAuto::auto(),
            };
            ws.margin = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.padding = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.border = taffy::prelude::Rect {
                left: taffy::prelude::TaffyZero::ZERO,
                right: taffy::prelude::TaffyZero::ZERO,
                top: taffy::prelude::TaffyZero::ZERO,
                bottom: taffy::prelude::TaffyZero::ZERO,
            };
            ws.overflow = Default::default();
            ws.aspect_ratio = None;
            let _ = self.taffy.set_style(wtid, ws);
            // 子表结构：wrapper = [提升件…, 内表]；内表 = 表格内部件。
            let mut wrapper_children = hoisted;
            wrapper_children.push(itid);
            if self.taffy.children(wtid).unwrap_or_default() != wrapper_children {
                let _ = self.taffy.set_children(wtid, &wrapper_children);
            }
            if self.taffy.children(itid).unwrap_or_default() != proper {
                let _ = self.taffy.set_children(itid, &proper);
            }
        }
    }

    /// 触发一次重排后二次迭代（上限 2 遍，内层表宽度取结算后值）。
    /// 三期④：行发现穿透行组；单元格图（CSS 2.1 §17.2.11.1 简化版）按
    /// colspan 属性分配显式列位；span-n 声明宽度均分给跨内未声明列。
    fn settle_tables(&mut self, viewport: (f32, f32)) {
        if self.tables.is_empty() {
            return;
        }
        for _ in 0..2 {
            let mut changed = false;
            let tables = self.tables.clone();
            for table in tables {
                let Some(&ttid) = self.taffy_node.get(&table) else {
                    continue;
                };
                // wrapper 重构：结算基准 = 内表（表盒自身框；wrapper 仅承载
                // 提升件堆叠，宽 = 包含块全宽）。
                let itid = self.taffy_table_inner.get(&table).copied().unwrap_or(ttid);
                let Ok(il) = self.taffy.layout(itid) else {
                    continue;
                };
                // 表内容宽 = 边框盒 − border − padding（百分比列基准）。
                let tw = (il.size.width
                    - il.border.left
                    - il.border.right
                    - il.padding.left
                    - il.padding.right)
                    .max(0.0);
                // 三期④：行发现穿透行组（row-group/header/footer 组为透明
                // 包装）；caption 与杂件不入行图。wrapper 重构匿名盒修补：
                // 裸单元格（display:table-cell 直属表盒，CSS 2.1 §17.2.1）
                // 建幻影单格行 Grid 承接——同 multicol 幻影列模式（无样式
                // 节点、Frame 不报告，槽位键复用）。内表 taffy 子表同步重排
                //（幻影行插在裸单元格原位；表子树结构同步豁免，该结构由
                // settle_table_fixup 与本函数独占）。
                struct TableRowSpec {
                    /// 真实行样式节点（幻影行为 None——无样式节点）。
                    node: Option<NodeId>,
                    /// 行 taffy 节点（真实行 tid 或幻影行 tid）。
                    tid: taffy::NodeId,
                    /// 参与单元格图的本行单元格（真实行 = 全部样式子件，
                    /// none/abs 由下方入图过滤剔除；幻影行 = 单个裸单元格）。
                    cells: Vec<NodeId>,
                }
                let mut rows: Vec<TableRowSpec> = Vec::new();
                let mut inner_children: Vec<taffy::NodeId> = Vec::new();
                for &c in self.tree.children(table) {
                    let Some(&ctid) = self.taffy_node.get(&c) else {
                        continue;
                    };
                    let is_abs = self.styles.get(&c).is_some_and(|cs| {
                        cs.position() == crate::css::property::Position::Absolute
                    });
                    // abs 后代一律内表（与 settle_table_fixup 的 is_abs 分支
                    // 对齐——abs 包含块语境 ≈ 表格 padding box，文档化偏差②）。
                    if is_abs {
                        inner_children.push(ctid);
                        continue;
                    }
                    match self.display_of(c) {
                        Some(crate::css::property::Display::TableRow) => {
                            rows.push(TableRowSpec {
                                node: Some(c),
                                tid: ctid,
                                cells: self.tree.children(c).to_vec(),
                            });
                            inner_children.push(ctid);
                        }
                        Some(crate::css::property::Display::TableRowGroup) => {
                            inner_children.push(ctid);
                            for &r in self.tree.children(c) {
                                if self.display_of(r)
                                    == Some(crate::css::property::Display::TableRow)
                                    && let Some(&rtid) = self.taffy_node.get(&r)
                                {
                                    rows.push(TableRowSpec {
                                        node: Some(r),
                                        tid: rtid,
                                        cells: self.tree.children(r).to_vec(),
                                    });
                                }
                            }
                        }
                        Some(crate::css::property::Display::TableCell) => {
                            let slot = rows.len();
                            let ptid = match self.table_row_anon.get(&(table, slot)).copied() {
                                Some(p) => p,
                                None => {
                                    let p = self
                                        .taffy
                                        .new_leaf(taffy::prelude::Style::default())
                                        .expect("table anon row leaf");
                                    self.table_row_anon.insert((table, slot), p);
                                    p
                                }
                            };
                            // 幻影行 = 单行 Grid（列模板由下方结算覆写）；
                            // 单元格经 set_children 移动挂入（taffy move 语义，
                            // 稳态重设同列表无副作用）。
                            let ps = taffy::prelude::Style {
                                display: taffy::prelude::Display::Grid,
                                ..taffy::prelude::Style::default()
                            };
                            let _ = self.taffy.set_style(ptid, ps);
                            let _ = self.taffy.set_children(ptid, &[ctid]);
                            self.taffy_parent.insert(ptid, itid);
                            self.taffy_parent.insert(ctid, ptid);
                            rows.push(TableRowSpec {
                                node: None,
                                tid: ptid,
                                cells: vec![c],
                            });
                            inner_children.push(ptid);
                        }
                        // 表格内部件兜底（caption / display:none / 无样式
                        // 节点——与 fixup internal 分支对齐）；其余块级子件
                        // 已被 fixup 提升至 wrapper，不得拉回内表。
                        Some(crate::css::property::Display::TableCaption)
                        | Some(crate::css::property::Display::None) => inner_children.push(ctid),
                        None => inner_children.push(ctid),
                        _ => {}
                    }
                }
                if self.taffy.children(itid).unwrap_or_default() != inner_children {
                    let _ = self.taffy.set_children(itid, &inner_children);
                }
                // 三期④：单元格图（CSS 2.1 §17.2.11.1 简化版）——逐行游标
                // 分配列位，colspan/rowspan 取属性（StyleNode.attrs，缺省 1），
                // 列数 = 各行跨数和的最大值；④d 行内非单元格元素按匿名单元
                // 格入图（见下方过滤注释）。
                // ④c rowspan：occupancy 集合记录被跨单元格占据的 (行,列)，
                // 后续行游标先跳过占据位（Chromium 语义——跨行单元不挤走
                // 后行单元格，列照常向后开辟）。
                let mut placements: Vec<(NodeId, usize, usize, u32)> = Vec::new();
                let mut occupied: std::collections::BTreeSet<(usize, usize)> = Default::default();
                let mut n_cols = 0usize;
                let mut first_row_len = 0usize;
                for (ri, spec) in rows.iter().enumerate() {
                    let mut cursor = 0usize;
                    for &c in &spec.cells {
                        // ④d 匿名盒（CSS 2.1 §17.2.1 第 3 条简化）：行内非
                        // 单元格元素（框架常见直写 <div>）→ 匿名单元格——
                        // 直接按单元格入图（列位/宽度拉伸与 td 一致，匿名
                        // 包装盒省略、元素自身承担单元格几何）。display:none
                        // 不入图；absolute 出流不作为单元格。
                        if self.display_of(c) == Some(crate::css::property::Display::None) {
                            continue;
                        }
                        if self.styles.get(&c).is_some_and(|cs| {
                            cs.position() == crate::css::property::Position::Absolute
                        }) {
                            continue;
                        }
                        let attrs = &self.tree.node(c).attrs;
                        let span = attrs
                            .get("colspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000) as usize;
                        let rspan = attrs
                            .get("rowspan")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        while occupied.contains(&(ri, cursor)) {
                            cursor += 1;
                        }
                        placements.push((c, cursor, span, rspan));
                        for rr in ri..ri + rspan as usize {
                            for cc in cursor..cursor + span {
                                occupied.insert((rr, cc));
                            }
                        }
                        cursor += span;
                    }
                    n_cols = n_cols.max(cursor);
                    if ri == 0 {
                        first_row_len = placements.len();
                    }
                }
                // 列声明仍取首行单元格；span-n 声明把宽度分给跨内未声明列
                // （等分近似——Chromium 按 min/max-content 分配，简单表一致）。
                // Length 声明 = px+内缩（content-box），Percent = 表宽基不追加。
                let mut declared: Vec<Option<(taffy::prelude::Dimension, f32)>> =
                    vec![None; n_cols];
                let mut claims: Vec<(usize, usize, taffy::prelude::Dimension, f32)> = Vec::new();
                for (cell, start, span, _) in placements[..first_row_len].iter() {
                    let Some(cs) = self.styles.get(cell) else {
                        continue;
                    };
                    let d = map_style(cs, &self.map_env()).size.width;
                    let rctx = crate::css::value::ResolveCtx {
                        em: cs.font_size_px(),
                        rem: self.map_env().rem,
                        viewport_w: self.map_env().viewport_w,
                        viewport_h: self.map_env().viewport_h,
                        ..crate::css::value::ResolveCtx::base(
                            cs.font_size_px(),
                            16.0,
                            self.map_env().viewport_w,
                            self.map_env().viewport_h,
                        )
                    };
                    let inset: f32 = [
                        (crate::css::property::PropertyId::PaddingLeft, None),
                        (crate::css::property::PropertyId::PaddingRight, None),
                        (
                            crate::css::property::PropertyId::BorderLeftWidth,
                            Some(crate::css::property::PropertyId::BorderLeftStyle),
                        ),
                        (
                            crate::css::property::PropertyId::BorderRightWidth,
                            Some(crate::css::property::PropertyId::BorderRightStyle),
                        ),
                    ]
                    .iter()
                    .filter_map(|(pid, style_pid)| used_h_inset(cs, *pid, *style_pid, &rctx))
                    .sum();
                    if *span == 1 {
                        declared[*start] = Some((d, inset));
                    } else {
                        claims.push((*start, *span, d, inset));
                    }
                }
                for (start, span, d, inset) in &claims {
                    let spanned = *start..(*start + *span);
                    let autos: Vec<usize> =
                        spanned.clone().filter(|i| declared[*i].is_none()).collect();
                    if autos.is_empty() {
                        continue;
                    }
                    let target = if let Some(px) = d.into_option() {
                        px + inset
                    } else if d.is_auto() {
                        continue;
                    } else {
                        d.value() * tw
                    };
                    let fixed: f32 = spanned
                        .filter_map(|i| declared[i])
                        .map(|(dd, ii)| match dd.into_option() {
                            Some(p) => p + ii,
                            None if dd.is_auto() => 0.0,
                            None => dd.value() * tw,
                        })
                        .sum();
                    let share = ((target - fixed) / autos.len() as f32).max(0.0);
                    for i in autos {
                        declared[i] = Some((taffy::prelude::Dimension::length(share), 0.0));
                    }
                }
                let declared: Vec<(taffy::prelude::Dimension, f32)> = declared
                    .into_iter()
                    .map(|d| d.unwrap_or((taffy::prelude::Dimension::auto(), 0.0)))
                    .collect();
                // P7-①：auto 列内容 max-content 测量——全行 span=1 且
                // 未声明宽的单元格（声明列内容不参与分配）。
                let mut content_max = vec![0.0f32; n_cols];
                for (cell, start, span, _) in &placements {
                    if *span != 1 || *start >= n_cols {
                        continue;
                    }
                    if self.styles.get(cell).is_some_and(|cs| {
                        map_style(cs, &self.map_env())
                            .size
                            .width
                            .into_option()
                            .is_some()
                    }) {
                        continue;
                    }
                    let w = self.content_max_width(*cell);
                    if w > content_max[*start] {
                        content_max[*start] = w;
                    }
                }
                let cols =
                    crate::layout::table_column_template(tw, &declared, n_cols, &content_max);
                let cols_unchanged = self
                    .table_cols
                    .get(&table)
                    .is_some_and(|prev| *prev == cols);
                let cells_unchanged = self.table_cells.get(&table) == Some(&placements);
                if cols_unchanged && cells_unchanged {
                    continue;
                }
                if !cols_unchanged {
                    let template: Vec<_> = cols
                        .iter()
                        .map(|&px| {
                            taffy::style::GridTemplateComponent::Single(
                                taffy::style_helpers::length(px),
                            )
                        })
                        .collect();
                    for spec in &rows {
                        let Ok(mut rs) = self.taffy.style(spec.tid).cloned() else {
                            continue;
                        };
                        rs.grid_template_columns = template.clone();
                        // ④c：行高 definite 时同时钉死行轨道——否则跨行单元
                        // 的高度覆写作为 auto 轨道的 min-content 贡献会把整
                        // 条轨道（及同轨其他单元格）撑高；Chromium 语义是跨
                        // 行内容不改显式行高、只向下溢出。幻影行无样式节点
                        //（node=None）→ 高度恒 auto。
                        if let Some(row_h) = spec
                            .node
                            .and_then(|rn| self.styles.get(&rn))
                            .and_then(|cs| map_style(cs, &self.map_env()).size.height.into_option())
                        {
                            rs.grid_template_rows =
                                vec![taffy::style::GridTemplateComponent::Single(
                                    taffy::style_helpers::length(row_h),
                                )];
                        }
                        let _ = self.taffy.set_style(spec.tid, rs);
                    }
                    self.table_cols.insert(table, cols);
                }
                // 三期④：单元格显式列位（1 基网格线起点 + 跨数），行位钉在
                // 第 1 行——洞（rowspan/杂件）不再吸附后续单元格。列模板全等
                // 但单元格图变化（如 colspan 属性变更）时仍需回写列位。
                // ④c rowspan：跨行单元留在宿主行网格内（列位照旧），高度
                // 覆写 = 宿主行 + 下方被跨行的 definite 高度和 → 向下溢出
                // 覆盖后续行（行高 definite 时与 Chromium 一致；被跨行 auto
                // 或宿主行 auto 时放弃覆写、按内容高——v1 记偏差，Chromium
                // 会把跨行内容分摊进被跨行高度）。行跨超出末行按 CSS 截断。
                if !cells_unchanged {
                    for (cell, start, span, rspan) in &placements {
                        let Some(&ctid) = self.taffy_node.get(cell) else {
                            continue;
                        };
                        let Ok(mut cst) = self.taffy.style(ctid).cloned() else {
                            continue;
                        };
                        cst.grid_column = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(((*start + 1) as i16).into()),
                            end: taffy::style::GridPlacement::Span(*span as u16),
                        };
                        cst.grid_row = taffy::geometry::Line {
                            start: taffy::style::GridPlacement::Line(1i16.into()),
                            end: taffy::style::GridPlacement::Span(1),
                        };
                        if *rspan > 1 {
                            // 宿主行 = 收纳该单元格的行规格（真实行含其样式
                            // 子件；裸单元格行即幻影行自身）。
                            let ri = rows
                                .iter()
                                .position(|spec| spec.cells.contains(cell))
                                .unwrap_or(0);
                            let end = (ri + *rspan as usize).min(rows.len());
                            let heights: Vec<Option<f32>> = rows[ri..end]
                                .iter()
                                .map(|spec| {
                                    spec.node
                                        .and_then(|rn| self.styles.get(&rn))
                                        .and_then(|cs| {
                                            map_style(cs, &self.map_env()).size.height.into_option()
                                        })
                                })
                                .collect();
                            if heights.iter().all(|h| h.is_some()) {
                                let total: f32 = heights.iter().map(|h| h.unwrap_or(0.0)).sum();
                                if total > 0.0 {
                                    cst.size.height = taffy::prelude::Dimension::length(total);
                                }
                            }
                        }
                        let _ = self.taffy.set_style(ctid, cst);
                    }
                    self.table_cells.insert(table, placements.clone());
                }
                changed = true;
            }
            if !changed {
                return;
            }
            if let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
        }
    }

    /// ③multi-column 结算（settle_calc/settle_tables 同模式的布局期结算）：
    /// pass1 布局给出容器内容宽后解析列数（count 显式；width 模式
    /// n = max(1, ⌊(内容宽+gap)/(理想宽+gap)⌋)）与列宽 colw=(cw−gap·(n−1))/n；
    /// 创建/复用 taffy 幻影列节点（无样式节点、Frame 不报告）并 set_children
    /// 跨父重挂真实子节点（taffy 0.14 支持移动语义）；按子节点 margin-box
    /// 单元高做平衡分配（CSS column-fill:balance 近似——理想高 = 总量/列数
    /// 的贪心填充，空列可承接不可断高子件）；文本重排后二次调用以最终高度
    /// 复衡。稳态（n/colw/分配全等）零额外布局 pass。v1 边界：断口 margin-top
    /// 不截断、column-span/rule 不实现、absolute 子件包含块仍为容器。
    fn settle_columns(&mut self, viewport: (f32, f32)) {
        if self.multicols.is_empty() {
            return;
        }
        let containers = self.multicols.clone();
        for mc in containers {
            let Some(&ctid) = self.taffy_node.get(&mc) else {
                continue;
            };
            let Ok(cl) = self.taffy.layout(ctid) else {
                continue;
            };
            // 容器内容宽 = 边框盒 − border − padding（列宽与 gap 基准）。
            let cw = (cl.size.width
                - cl.border.left
                - cl.border.right
                - cl.padding.left
                - cl.padding.right)
                .max(0.0);
            let Some(cs) = self.styles.get(&mc).cloned() else {
                continue;
            };
            let rctx = crate::css::value::ResolveCtx {
                em: cs.font_size_px(),
                rem: self.map_env().rem,
                viewport_w: self.map_env().viewport_w,
                viewport_h: self.map_env().viewport_h,
                ..crate::css::value::ResolveCtx::base(
                    cs.font_size_px(),
                    16.0,
                    self.map_env().viewport_w,
                    self.map_env().viewport_h,
                )
            };
            // gap：声明值优先；缺席 → multicol 语义 normal = 1em。
            let gap = match cs
                .get(crate::css::property::PropertyId::ColumnGap)
                .or_else(|| cs.get(crate::css::property::PropertyId::Gap))
            {
                Some(crate::css::property::DeclValue::Len(lp)) => {
                    lp.resolve(&rctx, 0.0).unwrap_or(0.0)
                }
                _ => cs.font_size_px(),
            };
            let count = match cs.get(crate::css::property::PropertyId::ColumnCount) {
                Some(crate::css::property::DeclValue::ColumnCount(c)) => *c,
                _ => None,
            };
            let width_px = match cs.get(crate::css::property::PropertyId::ColumnWidth) {
                Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => lp.resolve(&rctx, 0.0),
                _ => None,
            };
            let n: usize = match (count, width_px) {
                (Some(c), _) => (c as usize).max(1),
                (None, Some(w)) if w > 0.0 => (((cw + gap) / (w + gap)).floor() as usize).max(1),
                _ => 1,
            };
            if n <= 1 {
                // width 模式回退单列：容器还原块流（map_style 按多列请求
                // 映射 Flex，此处覆写）；如曾有幻影结构则子节点归位容器。
                let mut changed = false;
                if let Some(prev) = self.multicol_state.remove(&mc) {
                    // 三期⑤a：回退块流前恢复全部断口 margin-top 截断
                    // （幻影拆除，子节点回到容器块流）；⑤b 拆净段行。
                    for (&id, &orig) in &prev.truncated {
                        if let Some(&tid) = self.taffy_node.get(&id)
                            && let Ok(mut ts) = self.taffy.style(tid).cloned()
                        {
                            ts.margin.top = orig.restore();
                            let _ = self.taffy.set_style(tid, ts);
                        }
                    }
                    for r in &prev.rows {
                        let _ = self.taffy.set_children(*r, &[]);
                    }
                    for p in &prev.phantoms {
                        let _ = self.taffy.set_children(*p, &[]);
                    }
                    // 树序重挂全部真实子件（含 spanner——行序不含它）。
                    let ids: Vec<taffy::NodeId> = self
                        .tree
                        .children(mc)
                        .iter()
                        .filter_map(|c| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(ctid, &ids);
                    for t in &ids {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    changed = true;
                }
                if let Ok(ts) = self.taffy.style(ctid).cloned()
                    && ts.display == taffy::prelude::Display::Flex
                {
                    let mut ms = crate::layout::map_style(&cs, &self.map_env());
                    ms.display = taffy::prelude::Display::Block;
                    let _ = self.taffy.set_style(ctid, ms);
                    changed = true;
                }
                if changed && let Some(root) = self.taffy_root {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
                continue;
            }
            let colw = ((cw - gap * (n as f32 - 1.0)) / n as f32).max(0.0);
            let children: Vec<NodeId> = self.tree.children(mc).to_vec();
            // 三期⑤b：column-span:all 子件切断列流——spanner 前后各成段，
            // 每段独立二分平衡。spanner 模式容器 = Flex Column + 零 gap，
            // 每非空段一行包装（Flex Row 装 n 幻影列），spanner 为容器直
            // 系全宽块；段内列首、spanner 后新段首列的列首均按断口截断
            // margin-top（spanner 强制断行 = 截断边）。
            let is_spanner = |c: &NodeId| -> bool {
                matches!(
                    self.styles.get(c),
                    Some(ccs) if matches!(
                        ccs.get(crate::css::property::PropertyId::ColumnSpan),
                        Some(crate::css::property::DeclValue::ColumnSpan(Some(true)))
                    )
                )
            };
            let mut plans: Vec<Vec<NodeId>> = vec![Vec::new()];
            for &c in &children {
                if is_spanner(&c) {
                    plans.push(Vec::new());
                } else {
                    let i = plans.len() - 1;
                    plans[i].push(c);
                }
            }
            let seq_sig: Vec<usize> = {
                let mut sig = Vec::new();
                let mut seen = vec![false; plans.len()];
                for &c in &children {
                    if is_spanner(&c) {
                        sig.push(usize::MAX);
                    } else {
                        let i = plans
                            .iter()
                            .position(|p| p.contains(&c))
                            .unwrap_or(plans.len() - 1);
                        if !seen[i] {
                            sig.push(i);
                            seen[i] = true;
                        }
                    }
                }
                sig
            };
            let mut st = self.multicol_state.remove(&mc).unwrap_or_default();
            let mut structure_changed = false;
            let has_span = seq_sig.contains(&usize::MAX);
            if st.span_mode != has_span {
                // 模式切换：拆净旧行/幻影（真实子件随重挂归位）。
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                st.span_mode = has_span;
                structure_changed = true;
            }
            if has_span {
                // 容器样式每帧重断言——restyle 会以 map_style 重写回
                // Row + gap（多列请求的默认映射）。
                let mut ms = crate::layout::map_style(&cs, &self.map_env());
                ms.display = taffy::prelude::Display::Flex;
                ms.flex_direction = taffy::prelude::FlexDirection::Column;
                ms.gap = taffy::prelude::Size {
                    width: taffy::prelude::LengthPercentage::length(0.0),
                    height: taffy::prelude::LengthPercentage::length(0.0),
                };
                let _ = self.taffy.set_style(ctid, ms);
            } else if structure_changed {
                // 切回行模式：容器还原 map_style 的 Flex Row + gap。
                let ts = crate::layout::map_style(&cs, &self.map_env());
                let _ = self.taffy.set_style(ctid, ts);
            }
            // 行与幻影：数量不符 → 重建节点并临时均分（单元高与分配无关，
            // 仅供测量）；仅列宽变化 → 原节点回写宽度。
            let need_rows = if has_span { plans.len() } else { 0 };
            let need_phantoms = if has_span { plans.len() * n } else { n };
            if st.rows.len() != need_rows || st.phantoms.len() != need_phantoms {
                for r in &st.rows {
                    let _ = self.taffy.set_children(*r, &[]);
                }
                for p in &st.phantoms {
                    let _ = self.taffy.set_children(*p, &[]);
                }
                st.rows.clear();
                st.phantoms.clear();
                if has_span {
                    for plan in &plans {
                        let row = self
                            .taffy
                            .new_leaf(taffy::prelude::Style {
                                display: taffy::prelude::Display::Flex,
                                flex_direction: taffy::prelude::FlexDirection::Row,
                                size: taffy::prelude::Size {
                                    width: taffy::prelude::Dimension::percent(1.0),
                                    height: taffy::prelude::Dimension::auto(),
                                },
                                gap: taffy::prelude::Size {
                                    width: taffy::prelude::LengthPercentage::length(gap),
                                    height: taffy::prelude::LengthPercentage::length(0.0),
                                },
                                ..Default::default()
                            })
                            .expect("multicol segment row");
                        let phantoms: Vec<taffy::NodeId> = (0..n)
                            .map(|_| {
                                self.taffy
                                    .new_leaf(taffy::prelude::Style {
                                        display: taffy::prelude::Display::Block,
                                        size: taffy::prelude::Size {
                                            width: taffy::prelude::Dimension::length(colw),
                                            height: taffy::prelude::Dimension::auto(),
                                        },
                                        ..Default::default()
                                    })
                                    .expect("multicol phantom column")
                            })
                            .collect();
                        let _ = self.taffy.set_children(row, &phantoms);
                        for &p in &phantoms {
                            self.taffy_parent.insert(p, row);
                        }
                        // 临时均分（测量基座；分配随二分结果重挂）。
                        for (j, &p) in phantoms.iter().enumerate() {
                            let lo = plan.len() * j / n;
                            let hi = plan.len() * (j + 1) / n;
                            let ids: Vec<taffy::NodeId> = plan[lo..hi]
                                .iter()
                                .filter_map(|c| self.taffy_node.get(c).copied())
                                .collect();
                            let _ = self.taffy.set_children(p, &ids);
                            for c in plan[lo..hi].iter().filter_map(|c| self.taffy_node.get(c)) {
                                self.taffy_parent.insert(*c, p);
                            }
                        }
                        st.rows.push(row);
                        st.phantoms.extend(phantoms);
                    }
                    // 容器子序 = 段行 + spanner（树序）。
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c))
                                && !seen[i]
                            {
                                seen[i] = true;
                                v.push(st.rows[i]);
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                    st.seq_sig = seq_sig;
                } else {
                    let phantoms: Vec<taffy::NodeId> = (0..n)
                        .map(|_| {
                            self.taffy
                                .new_leaf(taffy::prelude::Style {
                                    display: taffy::prelude::Display::Block,
                                    size: taffy::prelude::Size {
                                        width: taffy::prelude::Dimension::length(colw),
                                        height: taffy::prelude::Dimension::auto(),
                                    },
                                    ..Default::default()
                                })
                                .expect("multicol phantom column")
                        })
                        .collect();
                    let _ = self.taffy.set_children(ctid, &phantoms);
                    for (i, &p) in phantoms.iter().enumerate() {
                        self.taffy_parent.insert(p, ctid);
                        let lo = children.len() * i / n;
                        let hi = children.len() * (i + 1) / n;
                        let ids: Vec<taffy::NodeId> = children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c).copied())
                            .collect();
                        let _ = self.taffy.set_children(p, &ids);
                        for c in children[lo..hi]
                            .iter()
                            .filter_map(|c| self.taffy_node.get(c))
                        {
                            self.taffy_parent.insert(*c, p);
                        }
                    }
                    st.phantoms = phantoms;
                    st.seq_sig = vec![0];
                }
                st.assignment = vec![(usize::MAX, usize::MAX); children.len()];
                structure_changed = true;
            } else if st.colw != colw {
                for &p in &st.phantoms {
                    if let Ok(mut ps) = self.taffy.style(p).cloned() {
                        ps.size.width = taffy::prelude::Dimension::length(colw);
                        let _ = self.taffy.set_style(p, ps);
                    }
                }
                structure_changed = true;
            } else if st.seq_sig != seq_sig {
                // 行数不变而 spanner 换位：仅重排容器孩子（复用行）。
                if has_span {
                    let seq_nodes: Vec<taffy::NodeId> = {
                        let mut v = Vec::new();
                        let mut seen = vec![false; plans.len()];
                        for &c in &children {
                            if is_spanner(&c) {
                                if let Some(&t) = self.taffy_node.get(&c) {
                                    v.push(t);
                                }
                            } else if let Some(i) = plans.iter().position(|p| p.contains(&c))
                                && !seen[i]
                            {
                                seen[i] = true;
                                v.push(st.rows[i]);
                            }
                        }
                        v
                    };
                    let _ = self.taffy.set_children(ctid, &seq_nodes);
                    for t in &seq_nodes {
                        self.taffy_parent.insert(*t, ctid);
                    }
                }
                st.seq_sig = seq_sig;
                structure_changed = true;
            }
            if structure_changed && let Some(root) = self.taffy_root {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
            // 平衡分配（三期⑤a 二分 + ⑤b 分段）：每段独立试高 h 的顺序装
            // 箱 fit——列首块 margin-top 截断（断口语义，css-multicol §7）、
            // 非空列放不下换列、末列溢出失败、空列承接不可断高块；fit 单调
            // → 浮点二分 48 次收窄最小可行列高，终值重装箱定分配。
            let idx_of: HashMap<NodeId, usize> =
                children.iter().enumerate().map(|(i, c)| (*c, i)).collect();
            let mut assignment: Vec<(usize, usize)> =
                vec![(usize::MAX, usize::MAX); children.len()];
            for (si, plan) in plans.iter().enumerate() {
                if plan.is_empty() {
                    continue;
                }
                let mut units: Vec<(f32, f32, f32)> = Vec::with_capacity(plan.len());
                for c in plan {
                    let h = self
                        .taffy_node
                        .get(c)
                        .and_then(|t| self.taffy.layout(*t).ok())
                        .map(|l| l.size.height)
                        .unwrap_or(0.0);
                    let (mt, mb) = match self.styles.get(c) {
                        Some(ccs) => {
                            let rctx = crate::css::value::ResolveCtx {
                                em: ccs.font_size_px(),
                                rem: self.map_env().rem,
                                viewport_w: self.map_env().viewport_w,
                                viewport_h: self.map_env().viewport_h,
                                ..crate::css::value::ResolveCtx::base(
                                    ccs.font_size_px(),
                                    16.0,
                                    self.map_env().viewport_w,
                                    self.map_env().viewport_h,
                                )
                            };
                            let m = ccs.margin();
                            let res = |lp: Option<&crate::css::value::LengthPercentage>| {
                                lp.and_then(|v| v.resolve(&rctx, 0.0)).unwrap_or(0.0)
                            };
                            (res(m[0]), res(m[3]))
                        }
                        None => (0.0, 0.0),
                    };
                    units.push((mt, h, mb));
                }
                let total: f32 = units.iter().map(|u| u.0 + u.1 + u.2).sum();
                let fit = |h: f32| -> Option<Vec<usize>> {
                    let mut assign = Vec::with_capacity(plan.len());
                    let (mut col, mut acc) = (0usize, 0.0f32);
                    for &(mt, uh, mb) in &units {
                        let mut eff = mt + uh + mb;
                        // 列非空（acc>0）且未到末列时才允许溢出换列；空列可
                        // 承接不可断高子件（首件无条件入列）。
                        if acc > 0.0 && acc + eff > h + f32::EPSILON {
                            if col == n - 1 {
                                return None;
                            }
                            col += 1;
                            acc = 0.0;
                            // 断口 margin-top 截断：换列后首件不带 mt 计量。
                            eff = uh + mb;
                        }
                        assign.push(col);
                        acc += eff;
                    }
                    Some(assign)
                };
                let (mut lo, mut hi) = (0.0f32, total);
                for _ in 0..48 {
                    let mid = (lo + hi) * 0.5;
                    if fit(mid).is_some() {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                let cols = fit(hi + 1.0e-3).or_else(|| fit(total)).unwrap_or_default();
                for (c, col) in plan.iter().zip(cols) {
                    if let Some(&i) = idx_of.get(c) {
                        assignment[i] = (si, col);
                    }
                }
            }
            // 断口 margin-top 截断（三期⑤a + ⑤b 段内列）：段内非首列的
            // 列首件写 margin-top=0（taffy 层），不再是列首的恢复原值。
            // 段首（含 spanner 后新段首列）不截——spanner 边界是强制断行，
            // css-break-3 规定强断边距保留（Chromium 153 实证 y 保留 mt）。
            // 计量恒用样式原值（ccs.margin()），分配跨帧稳定幂等；文本重
            // 排 pass 会以样式重写 margin → 每次调用无条件重算截断 diff。
            let mut trunc_changed = false;
            // 段内列首件（列 0 不算）；entry().or_insert() 保树序第一件——
            // HashMap::collect 是后写覆盖（曾拿到列尾件）。
            let mut col_first: std::collections::HashMap<(usize, usize), NodeId> =
                std::collections::HashMap::new();
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a != (usize::MAX, usize::MAX) && a.1 > 0 {
                    col_first.entry(*a).or_insert(*c);
                }
            }
            for id in st.truncated.keys().copied().collect::<Vec<_>>() {
                // 仅当该件仍是某段非首列的列首才保留截断；挪回列 0 或换
                // 位都恢复。
                let keep = children.iter().zip(assignment.iter()).any(|(c, a)| {
                    c == &id
                        && *a != (usize::MAX, usize::MAX)
                        && a.1 > 0
                        && col_first.get(a).copied() == Some(id)
                });
                if keep {
                    continue;
                }
                let orig = st.truncated.remove(&id);
                if let (Some(&tid), Some(orig)) = (self.taffy_node.get(&id), orig)
                    && let Ok(mut ts) = self.taffy.style(tid).cloned()
                {
                    ts.margin.top = orig.restore();
                    let _ = self.taffy.set_style(tid, ts);
                }
                trunc_changed = true;
            }
            for (c, a) in children.iter().zip(assignment.iter()) {
                if *a == (usize::MAX, usize::MAX) || a.1 == 0 {
                    continue;
                }
                let isfirst = col_first.get(a).copied() == Some(*c);
                if !isfirst {
                    continue;
                }
                let Some(&tid) = self.taffy_node.get(c) else {
                    continue;
                };
                let Ok(mut ts) = self.taffy.style(tid).cloned() else {
                    continue;
                };
                // taffy 0.14 LengthPercentageAuto 无公开字段——零值用
                // TaffyZero::ZERO 常量比对。已为零（含重排 pass 后重截）
                // 只登记占位；被重排 pass 重写回样式值的重新截断。
                let zero =
                    <taffy::prelude::LengthPercentageAuto as taffy::prelude::TaffyZero>::ZERO;
                if ts.margin.top == zero {
                    st.truncated
                        .entry(*c)
                        .or_insert(MarginTopBackup::capture(zero));
                    continue;
                }
                st.truncated
                    .insert(*c, MarginTopBackup::capture(ts.margin.top));
                ts.margin.top = zero;
                let _ = self.taffy.set_style(tid, ts);
                trunc_changed = true;
            }
            if trunc_changed || st.assignment != assignment {
                for (i, &p) in st.phantoms.iter().enumerate() {
                    let si = if has_span { i / n } else { 0usize };
                    let ids: Vec<taffy::NodeId> = children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c).copied())
                        .collect();
                    let _ = self.taffy.set_children(p, &ids);
                    for c in children
                        .iter()
                        .zip(assignment.iter())
                        .filter(|(_, a)| **a == (si, i % n))
                        .filter_map(|(c, _)| self.taffy_node.get(c))
                    {
                        self.taffy_parent.insert(*c, p);
                    }
                }
                if let Some(root) = self.taffy_root {
                    let _ = self.taffy.compute_layout(
                        root,
                        taffy::prelude::Size {
                            width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                            height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                        },
                    );
                }
            }
            st.n = n;
            st.colw = colw;
            st.assignment = assignment;
            self.multicol_state.insert(mc, st);
        }
    }

    /// 三期⑤c：列规几何结算——每行每相邻列间一条竖直条带（视口系，
    /// 相对容器 border-box 原点；paint 层加容器原点）。语义：style ∈
    /// BorderStyle（none/hidden 不画）；v1 仅 solid 实绘，dashed/dotted
    /// 近似 solid（B 级偏差，与 border 策略一致）；width 缺席 = medium
    /// 3px，显式负值钳 0（0 宽不画）；color 走 currentcolor 终结。
    /// 行模式条带 y/高 = 幻影列盒（Flex 拉伸全等 = 容器内容高）；span
    /// 模式逐段独立。空列照画（CSS 未设内容存在性条件；用例规避）。
    /// F1（ADR-0021）：IFC 行内流 v1——行打包结算（浮盒先例）。块容器
    /// 直子分类行内参与者（文本叶 / inline-block 原子盒 / inline 组盒）
    /// → 贪心行打包 → Position::Absolute+inset 锚定（父 padding box
    /// 相对）。挂点=settle_floats 之后、文本 remeasure 之前（参与者经
    /// inline_run_participants 豁免全宽重测）；每帧幂等=map_style 重置
    /// +重施（无 taffy Style 缓存——E4 E0277 教训）。
    /// v1 偏差（ADR-0021 D4）：行高=max(参与者测量高)、vertical-align=
    /// TOP 对齐、inline 组内子叶纵向堆叠（单叶 span 精确）、跨叶强制
    /// 断行（br）不支持、原子/组盒自然宽=taffy MaxContent 探针、仅
    /// Block 容器直子运行（inline-flex/grid/table 原子化=偏差在案）。
    #[cfg(feature = "text")]
    fn settle_lines(&mut self, viewport: (f32, f32)) {
        use crate::css::property::{DeclValue, Display, PropertyId, WhiteSpace};
        /// 行内参与者：文本叶（容器直子）或盒（inline-block 原子 / inline
        /// 组——其子叶保持组内块流，remeasure 继续管）。
        #[derive(Debug, Clone, Copy)]
        enum InlinePart {
            Leaf { id: NodeId },
            Box { id: NodeId },
        }
        self.inline_run_participants.clear();
        // frame-2 幂等：上帧参与者（settle_lines 覆写为 Absolute）在本帧
        // 运行收集中不得被 is_absolute 豁免误跳——快照后再收集。
        let prev_participants = std::mem::take(&mut self.inline_run_participants);
        // ① DFS 收集运行（Block 容器直子；连续行内参与者 ≥2 或含盒）。
        let mut runs: Vec<(NodeId, Vec<InlinePart>)> = Vec::new();
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
            let is_block = self.styles.get(&id).is_some_and(|cs| {
                matches!(
                    cs.get(PropertyId::Display),
                    Some(DeclValue::Display(Display::Block))
                )
            });
            if !is_block {
                continue;
            }
            let mut parts: Vec<InlinePart> = Vec::new();
            for c in self.tree.children(id).iter().copied() {
                if self.tables.contains(&c)
                    || (self.is_absolute(c) && !prev_participants.contains(&c))
                    || self.is_fixed(c)
                {
                    if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                    {
                        runs.push((id, std::mem::take(&mut parts)));
                    } else {
                        parts.clear();
                    }
                    continue;
                }
                let disp = self
                    .styles
                    .get(&c)
                    .and_then(|cs| match cs.get(PropertyId::Display) {
                        Some(DeclValue::Display(d)) => Some(*d),
                        _ => None,
                    });
                let has_text = self
                    .tree
                    .node(c)
                    .text
                    .as_deref()
                    .is_some_and(|t| !t.is_empty());
                // P9-3（css-lists-3 §3，ADR-0041）：带文本子节点的行内参与
                // 仅限行内级与 Block（Block+文本=引擎树模型的匿名行内近似，
                // F1 契约）；ListItem/Table*/TableRowGroup 等无歧义块级盒的
                // 文本属自身盒（CSS 2.1 §9.2.1），终止运行而非叶参与——
                // 否则相邻文本 li 会被打包进同一行（块堆叠被破坏）。
                if matches!(
                    disp,
                    Some(Display::ListItem)
                        | Some(Display::Table)
                        | Some(Display::TableRow)
                        | Some(Display::TableRowGroup)
                        | Some(Display::TableCaption)
                ) {
                    if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                    {
                        runs.push((id, std::mem::take(&mut parts)));
                    } else {
                        parts.clear();
                    }
                    continue;
                }
                if has_text {
                    parts.push(InlinePart::Leaf { id: c });
                    continue;
                }
                match disp {
                    Some(Display::InlineBlock) | Some(Display::Inline) => {
                        parts.push(InlinePart::Box { id: c });
                    }
                    // display:none 兄弟不终止行内流（spec：不生成盒）。
                    Some(Display::None) => {}
                    _ => {
                        if parts.len() >= 2
                            || parts.iter().any(|p| matches!(p, InlinePart::Box { .. }))
                        {
                            runs.push((id, std::mem::take(&mut parts)));
                        } else {
                            parts.clear();
                        }
                    }
                }
            }
            if parts.len() >= 2 || parts.iter().any(|p| matches!(p, InlinePart::Box { .. })) {
                runs.push((id, parts));
            }
        }
        if runs.is_empty() {
            return;
        }
        // ② 逐运行打包+置样式。
        let mut changed = false;
        for (pid, parts) in &runs {
            let Some(&ptid) = self.taffy_node.get(pid) else {
                continue;
            };
            // pl 是 &Layout（借用 self.taffy）——立即拷出标量并结束借用，
            // 否则 set_style/compute_layout 的可变借用冲突（E0502）。
            let (pad_top, pad_left, content_left, content_right, content_top) =
                match self.taffy.layout(ptid) {
                    Ok(pl) => (
                        pl.location.y + pl.border.top,
                        pl.location.x + pl.border.left,
                        pl.location.x + pl.border.left + pl.padding.left,
                        pl.location.x + pl.size.width - pl.border.right - pl.padding.right,
                        pl.location.y + pl.border.top + pl.padding.top,
                    ),
                    Err(_) => continue,
                };
            let mut y = content_top;
            let mut x = content_left;
            let mut line_h = 0.0f32;
            // P3（ADR-0034 D2）：strut = 容器字体度量（text-top/bottom 的
            // 对齐目标）；pending = 本行 TOP 装箱暂存，行结束 flush。
            let (strut_fm, strut_size) = match self.styles.get(pid) {
                Some(c) => (*c.font_metrics(), c.font_size_px()),
                None => (Default::default(), 16.0),
            };
            let mut pending: Vec<PendingLinePart> = Vec::new();
            for part in parts {
                match *part {
                    InlinePart::Leaf { id } => {
                        let Some(text) = self.tree.node(id).text.clone() else {
                            continue;
                        };
                        let Some(cs) = self.styles.get(&id).cloned() else {
                            continue;
                        };
                        let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
                        let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                            owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
                        // nowrap/pre = 不折行（剩余宽约束解除）。
                        let nowrap = matches!(
                            cs.get(PropertyId::WhiteSpace),
                            Some(DeclValue::WhiteSpace(WhiteSpace::NoWrap | WhiteSpace::Pre))
                        );
                        let remaining = (content_right - x).max(0.0);
                        let avail = if nowrap { None } else { Some(remaining) };
                        // 声明宽/高叶=原子式参与（第五批⑥声明优先 + F1 行内
                        // 参与合成）：按声明宽折行测量高、盒用声明尺寸。
                        let ts0 = map_style(&cs, &self.map_env());
                        let decl_w = if has_declared_len(&cs, PropertyId::Width) {
                            ts0.size.width.into_option()
                        } else {
                            None
                        };
                        let decl_h = if has_declared_len(&cs, PropertyId::Height) {
                            ts0.size.height.into_option()
                        } else {
                            None
                        };
                        let (w, h, pb) = match decl_w {
                            Some(dw) => {
                                let m = self.text.measure_with_baseline(
                                    &text,
                                    &cs,
                                    &span_refs,
                                    Some(dw),
                                    &self.map_env(),
                                );
                                (dw, m.1, m.2)
                            }
                            None => self.text.measure_with_baseline(
                                &text,
                                &cs,
                                &span_refs,
                                avail,
                                &self.map_env(),
                            ),
                        };
                        let h = decl_h.unwrap_or(h);
                        let Some(&tid) = self.taffy_node.get(&id) else {
                            continue;
                        };
                        let Ok(mut st) = self.taffy.style(tid).cloned() else {
                            continue;
                        };
                        st.position = taffy::prelude::Position::Absolute;
                        st.inset = taffy::geometry::Rect {
                            top: taffy::style_helpers::length(y - pad_top),
                            bottom: taffy::style_helpers::auto(),
                            left: taffy::style_helpers::length(x - pad_left),
                            right: taffy::style_helpers::auto(),
                        };
                        st.size = taffy::prelude::Size {
                            width: taffy::style_helpers::length(w),
                            height: taffy::style_helpers::length(h),
                        };
                        let _ = self.taffy.set_style(tid, st);
                        self.inline_run_participants.insert(id);
                        changed = true;
                        // P3：TOP 装箱信息入行暂存（flush 统一算偏移）。
                        pending.push(PendingLinePart {
                            tid,
                            va: cs.vertical_align(),
                            pb,
                            h,
                            top: y - pad_top,
                            font_size: cs.font_size_px(),
                            fm: *cs.font_metrics(),
                        });
                        // 满行判定：折行发生（宽触及剩余）→ 占满本行，
                        // 后续参与者下行。
                        if avail.is_some() && w > 0.0 && w >= remaining - 0.5 {
                            x = content_right;
                        } else {
                            x += w;
                        }
                        line_h = line_h.max(h);
                    }
                    InlinePart::Box { id } => {
                        let Some(&tid) = self.taffy_node.get(&id) else {
                            continue;
                        };
                        // 自然尺寸探针：MaxContent 可用宽下的布局尺寸
                        //（块布局 auto 宽=拉伸 → MaxContent = 收缩适配）。
                        let _ = self.taffy.compute_layout(
                            tid,
                            taffy::prelude::Size {
                                width: taffy::prelude::AvailableSpace::MaxContent,
                                height: taffy::prelude::AvailableSpace::MaxContent,
                            },
                        );
                        let (bw, bh) = match self.taffy.layout(tid) {
                            Ok(l) => (l.size.width, l.size.height),
                            Err(_) => continue,
                        };
                        // MaxContent 探针对文本叶子树=0 宽（taffy 无文本内在
                        // 尺寸——叶宽只在 remeasure 注入）→ 盒自然宽取
                        // max(探针, 子树文本叶 nowrap 测量宽)。单叶组/盒精确；
                        // 多叶组 max（非流宽和）=v1 B 级偏差在案 ADR-0021。
                        let mut bw = bw;
                        let mut sub: Vec<NodeId> = vec![id];
                        while let Some(s) = sub.pop() {
                            if let Some(text) = self.tree.node(s).text.clone()
                                && !text.is_empty()
                                && let Some(cs) = self.styles.get(&s).cloned()
                            {
                                let owned = self.span_styles.get(&s).cloned().unwrap_or_default();
                                let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                                    owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                                let (lw, _lh) = self.text.measure_rich(
                                    &text,
                                    &cs,
                                    &span_refs,
                                    None,
                                    &self.map_env(),
                                );
                                if lw > bw {
                                    bw = lw;
                                }
                            }
                            for g in self.tree.children(s).iter().rev() {
                                sub.push(*g);
                            }
                        }
                        // 换行判定（行首盒不换；行已溢出（nowrap 叶等）→
                        // 后续盒继续同行——CSS 行盒溢出续排真行为）。
                        if x > content_left && x <= content_right && x + bw > content_right {
                            // P3：行结束——先 flush 上一行（偏移回填+行盒
                            // 扩展）再下行。
                            let final_h =
                                self.flush_inline_line(&mut pending, strut_fm, strut_size, line_h);
                            y += final_h;
                            x = content_left;
                            line_h = 0.0;
                        }
                        let declared_w = self
                            .styles
                            .get(&id)
                            .is_some_and(|cs| has_declared_len(cs, PropertyId::Width));
                        let declared_h = self
                            .styles
                            .get(&id)
                            .is_some_and(|cs| has_declared_len(cs, PropertyId::Height));
                        let Ok(mut st) = self.taffy.style(tid).cloned() else {
                            continue;
                        };
                        st.position = taffy::prelude::Position::Absolute;
                        st.inset = taffy::geometry::Rect {
                            top: taffy::style_helpers::length(y - pad_top),
                            bottom: taffy::style_helpers::auto(),
                            left: taffy::style_helpers::length(x - pad_left),
                            right: taffy::style_helpers::auto(),
                        };
                        if !declared_w {
                            st.size.width = taffy::style_helpers::length(bw);
                        }
                        if !declared_h {
                            st.size.height = taffy::style_helpers::length(bh);
                        }
                        let _ = self.taffy.set_style(tid, st);
                        self.inline_run_participants.insert(id);
                        changed = true;
                        // P3：Box 基线探针（子树首文本叶=探针 y+叶基线；
                        // 无文本=底边）+ TOP 装箱信息入行暂存。
                        let pb = self.box_first_text_baseline(id, bh);
                        pending.push(PendingLinePart {
                            tid,
                            va: self
                                .styles
                                .get(&id)
                                .map(|c| c.vertical_align())
                                .unwrap_or(crate::css::property::VerticalAlignKind::Baseline),
                            pb,
                            h: bh,
                            top: y - pad_top,
                            font_size: self
                                .styles
                                .get(&id)
                                .map(|c| c.font_size_px())
                                .unwrap_or(16.0),
                            fm: self
                                .styles
                                .get(&id)
                                .map(|c| *c.font_metrics())
                                .unwrap_or_default(),
                        });
                        x += bw;
                        line_h = line_h.max(bh);
                    }
                }
            }
            // P3：run 尾 flush 残余行（偏移回填+行盒扩展，容器高度
            // 保持段按扩展后行高计）。
            line_h = self.flush_inline_line(&mut pending, strut_fm, strut_size, line_h);
            // 容器高度保持：行内内容出流（Absolute）会使 auto 高容器塌陷
            //（spec：行内内容贡献行盒高；浮盒本就不贡献父高故 floats 无此
            // 步）。min_height=打包行底（内容盒高）；taffy 取 max(auto 内容
            // 高, min_height)——多 run 容器与残余块级子高天然合成。
            let declared_ch = self
                .styles
                .get(pid)
                .is_some_and(|cs| has_declared_len(cs, PropertyId::Height));
            let declared_mh = self
                .styles
                .get(pid)
                .is_some_and(|cs| has_declared_len(cs, PropertyId::MinHeight));
            if !declared_ch
                && !declared_mh
                && let Ok(mut pst) = self.taffy.style(ptid).cloned()
            {
                let lines_h = (y + line_h) - content_top;
                if lines_h > 0.0 {
                    pst.min_size.height = taffy::style_helpers::length(lines_h);
                    let _ = self.taffy.set_style(ptid, pst);
                }
            }
        }
        // ③ 终布局：锚定生效（浮盒同式；文本 remeasure 读运行终宽）。
        if changed && let Some(root) = self.taffy_root {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
        }
    }

    /// Box 参与者基线探针（P3，ADR-0034 D2）：子树**首个文本叶**的
    /// 探针布局 y 偏移 + 其首行基线；无文本叶 = 盒底边（v1 近似）。
    /// 前置：对 tid 已跑 MaxContent 探针布局（location 树就绪）。
    /// location.y 沿路径累加（taffy 子节点 location 相对父 content box；
    /// border/padding 差异近似=B 级在案 ADR-0034）。
    #[cfg(feature = "text")]
    fn box_first_text_baseline(&mut self, root: NodeId, fallback: f32) -> f32 {
        let mut stack: Vec<(NodeId, f32)> = vec![(root, 0.0)];
        while let Some((nid, dy)) = stack.pop() {
            let has_text = self
                .tree
                .node(nid)
                .text
                .as_deref()
                .is_some_and(|t| !t.is_empty());
            if has_text {
                if let Some(cs) = self.styles.get(&nid).cloned() {
                    let owned = self.span_styles.get(&nid).cloned().unwrap_or_default();
                    let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                        owned.iter().map(|(a, b, sc)| (*a, *b, sc)).collect();
                    let env = self.map_env();
                    let (_, _, pb) = self.text.measure_with_baseline(
                        self.tree.node(nid).text.as_deref().unwrap_or(""),
                        &cs,
                        &span_refs,
                        None,
                        &env,
                    );
                    return dy + pb;
                }
                return dy + fallback;
            }
            if let Some(&tid) = self.taffy_node.get(&nid) {
                let child_y = match self.taffy.layout(tid) {
                    Ok(l) => l.location.y,
                    Err(_) => 0.0,
                };
                for g in self.tree.children(nid).iter().rev() {
                    stack.push((*g, dy + child_y));
                }
            }
        }
        fallback
    }

    /// 行两阶段结算 flush（P3，ADR-0034 D2）：TOP 装箱收集完成后按
    /// vertical-align 统一结算纵向偏移——行基线 L = max(基线距)；非
    /// bottom 值先算 dy 并回填 inset.top（dy 相对装箱行顶）；行盒底 =
    /// max(原行高, max(dy+h)) 扩展（descender 下沉扩展语义）；bottom
    /// 值对齐扩展后行底（二遍）。返回最终行高（≥ 入参 line_h）。
    /// 偏差（ADR-0034 在案）：va=baseline 不产生偏移（v1 TOP 装箱逐位
    /// 一致——混字号默认基线下沉属 B 级后续）；calc() 承载偏移按 0。
    #[cfg(feature = "text")]
    fn flush_inline_line(
        &mut self,
        pending: &mut Vec<PendingLinePart>,
        strut_fm: crate::css::value::FontMetrics,
        strut_size: f32,
        line_h: f32,
    ) -> f32 {
        use crate::css::property::VerticalAlignKind as Va;
        use crate::css::value::LengthPercentage as Lp;
        if pending.is_empty() {
            return line_h;
        }
        let baseline = pending.iter().map(|p| p.pb).fold(0.0f32, f32::max);
        let strut_asc = strut_fm.ascent_per_em * strut_size;
        let strut_desc = strut_fm.descent_per_em * strut_size;
        let mut max_bottom = 0.0f32;
        let mut bottom_parts: Vec<(usize, f32)> = Vec::new();
        for (i, p) in pending.iter().enumerate() {
            let dy = match &p.va {
                Va::Baseline | Va::Top => 0.0,
                Va::Bottom => {
                    bottom_parts.push((i, p.h));
                    0.0
                }
                Va::Sub => baseline + 0.34 * p.font_size - p.pb,
                Va::Super => baseline - 0.34 * p.font_size - p.pb,
                Va::Length(lp) => {
                    // % 基准 = 装箱行高；px 直接；em/rem/视口/容器按节点
                    // 与全局基准换算；calc() = 0（B 级在案）。
                    let va_px = match lp {
                        Lp::Px(v) => *v,
                        Lp::Em(v) => *v * p.font_size,
                        Lp::Rem(v) => *v * self.rem_base(),
                        Lp::Percent(v) => *v * line_h,
                        Lp::Vw(v) => *v * self.map_env().viewport_w,
                        Lp::Vh(v) => *v * self.map_env().viewport_h,
                        _ => 0.0,
                    };
                    baseline - va_px - p.pb
                }
                Va::Middle => baseline - 0.5 * p.fm.ex_per_em * p.font_size - 0.5 * p.h,
                Va::TextTop => baseline - strut_asc - p.pb + p.fm.ascent_per_em * p.font_size,
                Va::TextBottom => baseline + strut_desc - p.pb - p.fm.descent_per_em * p.font_size,
            };
            max_bottom = max_bottom.max(dy + p.h);
            if dy != 0.0
                && let Ok(mut st) = self.taffy.style(p.tid).cloned()
            {
                st.inset.top = taffy::style_helpers::length(p.top + dy);
                let _ = self.taffy.set_style(p.tid, st);
            }
        }
        let final_h = line_h.max(max_bottom);
        for &(i, h) in &bottom_parts {
            let p = &pending[i];
            let dy = final_h - h;
            if dy != 0.0
                && let Ok(mut st) = self.taffy.style(p.tid).cloned()
            {
                st.inset.top = taffy::style_helpers::length(p.top + dy);
                let _ = self.taffy.set_style(p.tid, st);
            }
        }
        pending.clear();
        final_h
    }

    /// E4（ADR-0019）：浮动结算——出流/堆叠/环绕/clear 钳位（CSS 2 §9.5）。
    /// 诚实边界（0.x）：兄弟级环绕（顶缘落在浮盒带内=带内同侧浮盒宽和）
    /// +同父浮栈贪心放置+clear 钳位（margin-top 增量）；table/multicol/
    /// 定位子树豁免（v1 浮动不进表格/定位上下文）。挂点=settle_columns
    /// 之后、文本 remeasure 之前（折行宽依赖结算值）。幂等=每帧先还原
    /// 上帧覆写（map_style 重放 pristine）→ pristine 布局→几何计算→覆写→终布局
    /// （浮盒在场=两次布局，同 settle_tables 多遍先例）。
    fn settle_floats(&mut self, viewport: (f32, f32)) {
        let Some(root) = self.taffy_root else {
            return;
        };
        // 0) 还原上帧覆写（重放式幂等——原样式缓存；taffy 重建后新 tid
        //    上缓存的原样式仍=该样式树节点的 pristine 形，还原安全）。
        let mut restored = false;
        for id in std::mem::take(&mut self.float_touched) {
            if let (Some(&tid), Some(cs)) = (self.taffy_node.get(&id), self.styles.get(&id)) {
                // 还原=从 ComputedStyle 重放 pristine 样式（map_style 纯
                // 函数；不缓存 taffy::Style 值——Send/Sync 与陈旧缓存双防）。
                // 拆语句：参数不可变借用止于 map_style 返回（E0502）。
                let pristine = crate::layout::map_style(cs, &self.map_env());
                let _ = self.taffy.set_style(tid, pristine);
                restored = true;
            }
        }
        // 1) DFS 收集（文档序）：按父分组浮盒 + clear 盒。豁免 table/
        //    multicol 子树（settle_tables/columns 自治）与定位盒
        //    （CSS：float 对 absolute/fixed 无效）。
        let mut floats_by_parent: HashMap<NodeId, Vec<(NodeId, crate::css::property::FloatKind)>> =
            HashMap::new();
        let mut clears: Vec<(NodeId, crate::css::property::ClearKind)> = Vec::new();
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            if let Some(cs) = self.styles.get(&id) {
                let f = cs.float();
                let cl = cs.clear();
                if f != crate::css::property::FloatKind::None {
                    if let Some(p) = self.tree.parent(id) {
                        floats_by_parent.entry(p).or_default().push((id, f));
                    }
                } else if cl != crate::css::property::ClearKind::None {
                    clears.push((id, cl));
                }
            }
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
        }
        if floats_by_parent.is_empty() && clears.is_empty() {
            if restored {
                let _ = self.taffy.compute_layout(
                    root,
                    taffy::prelude::Size {
                        width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                        height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                    },
                );
            }
            return;
        }
        // 2) pristine 布局：还原后（或本帧首现浮盒）重排一遍——浮盒仍
        //    在流内，首遍几何即流内位置（未还原时 :1389 布局已是 pristine）。
        if restored {
            let _ = self.taffy.compute_layout(
                root,
                taffy::prelude::Size {
                    width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                    height: taffy::prelude::AvailableSpace::Definite(viewport.1),
                },
            );
        }
        // 3) 逐父：3a 浮盒放置（文档序贪心带放置）→ 3b 单遍 children
        //    扫描（浮盒覆写 + 兄弟环绕 + clear 钳位）。
        for (pid, floats) in &floats_by_parent {
            let Some(&ptid) = self.taffy_node.get(pid) else {
                continue;
            };
            let Ok(pl) = self.taffy.layout(ptid) else {
                continue;
            };
            // 父盒几何（视口坐标）——precompute 局部量（pl 借用不得跨
            // set_style 可变借用，E0502）。
            let content_left = pl.location.x + pl.border.left + pl.padding.left;
            let content_right = pl.location.x + pl.size.width - pl.border.right - pl.padding.right;
            let content_top = pl.location.y + pl.border.top + pl.padding.top;
            let pad_right_x = pl.location.x + pl.size.width - pl.border.right;
            /// 单个浮盒放置记录（本 pass 局部）。
            struct Place {
                id: NodeId,
                is_left: bool,
                x: f32,
                y: f32,
                w: f32,
                h: f32,
                idx: usize,
            }
            // 3a) 放置：同侧栈 (带顶, 前缘, 底缘) 水平适配贪心（同带继续
            //     直至容器缘溢出才下移，CSS §9.5.1）；浮盒自身 clear 钳位
            //     查已放置记录。
            let mut placements: Vec<Place> = Vec::new();
            let mut left_stack: Option<(f32, f32, f32)> = None;
            let mut right_stack: Option<(f32, f32, f32)> = None;
            for (idx, (fid, kind)) in floats.iter().enumerate() {
                let Some(&ftid) = self.taffy_node.get(fid) else {
                    continue;
                };
                let Ok(fl) = self.taffy.layout(ftid) else {
                    continue;
                };
                let (fw, fh) = (fl.size.width, fl.size.height);
                if fw <= 0.0 || fh <= 0.0 {
                    continue;
                }
                let own_clear = self
                    .styles
                    .get(fid)
                    .map(|cs| cs.clear())
                    .unwrap_or(crate::css::property::ClearKind::None);
                let clamp_y = |ps: &[Place], k: crate::css::property::ClearKind| -> f32 {
                    match k {
                        crate::css::property::ClearKind::None => 0.0,
                        crate::css::property::ClearKind::Left => ps
                            .iter()
                            .filter(|p| p.is_left)
                            .map(|p| p.y + p.h)
                            .fold(0.0, f32::max),
                        crate::css::property::ClearKind::Right => ps
                            .iter()
                            .filter(|p| !p.is_left)
                            .map(|p| p.y + p.h)
                            .fold(0.0, f32::max),
                        crate::css::property::ClearKind::Both => {
                            ps.iter().map(|p| p.y + p.h).fold(0.0, f32::max)
                        }
                    }
                };
                match kind {
                    crate::css::property::FloatKind::Left => {
                        let (band_top, next_x, max_bottom) =
                            left_stack.unwrap_or((content_top, content_left, content_top));
                        let (x, mut y) = if next_x + fw <= content_right {
                            (next_x, band_top)
                        } else {
                            (content_left, max_bottom)
                        };
                        y = y.max(content_top + clamp_y(&placements, own_clear));
                        left_stack = Some((y, x + fw, (y + fh).max(max_bottom)));
                        placements.push(Place {
                            id: *fid,
                            is_left: true,
                            x,
                            y,
                            w: fw,
                            h: fh,
                            idx,
                        });
                    }
                    crate::css::property::FloatKind::Right => {
                        let (band_top, next_right, max_bottom) =
                            right_stack.unwrap_or((content_top, content_right, content_top));
                        let (x, mut y) = if next_right - fw >= content_left {
                            (next_right - fw, band_top)
                        } else {
                            (content_right - fw, max_bottom)
                        };
                        y = y.max(content_top + clamp_y(&placements, own_clear));
                        right_stack = Some((y, x, (y + fh).max(max_bottom)));
                        placements.push(Place {
                            id: *fid,
                            is_left: false,
                            x,
                            y,
                            w: fw,
                            h: fh,
                            idx,
                        });
                    }
                    crate::css::property::FloatKind::None => {}
                }
            }
            if placements.is_empty() {
                continue;
            }
            let float_ids: std::collections::HashSet<NodeId> =
                floats.iter().map(|(f, _)| *f).collect();
            let children: Vec<NodeId> = self.tree.children(*pid).to_vec();
            // 3b) 单遍 children：浮盒覆写（Absolute+inset）+ 兄弟处理。
            for (idx, sid) in children.iter().enumerate() {
                if let Some(p) = placements.iter().find(|q| q.id == *sid) {
                    // 浮盒：出流 + inset 锚定（taffy absolute 基准=直父
                    // padding box；x/y 视口坐标 → 减 border 得相对量）。
                    let (is_left, fx, fy, fw) = (p.is_left, p.x, p.y, p.w);
                    let Some(&ftid) = self.taffy_node.get(sid) else {
                        continue;
                    };
                    let Ok(mut st) = self.taffy.style(ftid).cloned() else {
                        continue;
                    };
                    st.position = taffy::prelude::Position::Absolute;
                    st.inset = taffy::prelude::Rect {
                        top: taffy::style_helpers::length(fy - content_top),
                        bottom: taffy::style_helpers::auto(),
                        left: if is_left {
                            taffy::style_helpers::length(fx - content_left)
                        } else {
                            taffy::style_helpers::auto()
                        },
                        right: if is_left {
                            taffy::style_helpers::auto()
                        } else {
                            taffy::style_helpers::length(pad_right_x - (fx + fw))
                        },
                    };
                    let _ = self.taffy.set_style(ftid, st);
                    self.float_touched.insert(*sid);
                    continue;
                }
                if float_ids.contains(sid)
                    || self.tables.contains(sid)
                    || self.multicols.contains(sid)
                    || self.is_absolute(*sid)
                    || self.is_fixed(*sid)
                {
                    continue;
                }
                let Some(&stid) = self.taffy_node.get(sid) else {
                    continue;
                };
                let Ok(sl) = self.taffy.layout(stid) else {
                    continue;
                };
                // 布局读值 precompute（sl 借用不得跨 set_style，E0502）。
                let pristine_y = sl.location.y;
                let sml = sl.margin.left;
                let smr = sl.margin.right;
                let smt = sl.margin.top;
                // 出流补偿：本文档序之前的同父浮盒高和（原在流内推挤）。
                let removed: f32 = placements.iter().filter(|p| p.idx < idx).map(|p| p.h).sum();
                let post_y = pristine_y - removed;
                let sib_clear = self
                    .styles
                    .get(sid)
                    .map(|cs| cs.clear())
                    .unwrap_or(crate::css::property::ClearKind::None);
                // 横向环绕：顶缘落在浮盒带内（post_y ∈ [y, y+h)）的同侧
                // 浮盒宽和；clear 盒跳过（钳位后全宽，CSS 语义）。
                if sib_clear == crate::css::property::ClearKind::None {
                    let left_w: f32 = placements
                        .iter()
                        .filter(|p| p.is_left && post_y >= p.y && post_y < p.y + p.h)
                        .map(|p| p.w)
                        .sum();
                    let right_w: f32 = placements
                        .iter()
                        .filter(|p| !p.is_left && post_y >= p.y && post_y < p.y + p.h)
                        .map(|p| p.w)
                        .sum();
                    if left_w > 0.0 || right_w > 0.0 {
                        let Ok(mut sst) = self.taffy.style(stid).cloned() else {
                            continue;
                        };
                        if left_w > 0.0 {
                            sst.margin.left = taffy::style_helpers::length(sml + left_w);
                        }
                        if right_w > 0.0 {
                            sst.margin.right = taffy::style_helpers::length(smr + right_w);
                        }
                        let _ = self.taffy.set_style(stid, sst);
                        self.float_touched.insert(*sid);
                    }
                }
                // clear 钳位：margin-top 增量 = 相关向最后浮盒底 − post_y。
                let need = match sib_clear {
                    crate::css::property::ClearKind::None => 0.0,
                    crate::css::property::ClearKind::Left => placements
                        .iter()
                        .filter(|p| p.is_left)
                        .map(|p| p.y + p.h)
                        .fold(0.0, f32::max),
                    crate::css::property::ClearKind::Right => placements
                        .iter()
                        .filter(|p| !p.is_left)
                        .map(|p| p.y + p.h)
                        .fold(0.0, f32::max),
                    crate::css::property::ClearKind::Both => {
                        placements.iter().map(|p| p.y + p.h).fold(0.0, f32::max)
                    }
                };
                if need > post_y {
                    let Ok(mut cst) = self.taffy.style(stid).cloned() else {
                        continue;
                    };
                    cst.margin.top = taffy::style_helpers::length(smt + (need - post_y));
                    let _ = self.taffy.set_style(stid, cst);
                    self.float_touched.insert(*sid);
                }
            }
        }
        // 4) 终布局：覆写生效（浮盒 Absolute 出流+兄弟 margin 环绕+clear
        //    下移）；文本 remeasure（frame 内 settle_floats 之后）读终宽。
        let _ = self.taffy.compute_layout(
            root,
            taffy::prelude::Size {
                width: taffy::prelude::AvailableSpace::Definite(viewport.0),
                height: taffy::prelude::AvailableSpace::Definite(viewport.1),
            },
        );
    }
    fn settle_column_rules(&mut self, layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>) {
        self.column_rules.clear();
        for &mc in &self.multicols {
            let Some(segs) = self.column_rule_segs(mc, layout_by_node) else {
                continue;
            };
            if !segs.is_empty() {
                self.column_rules.insert(mc, segs);
            }
        }
    }

    /// 单容器的列规条带（只读；None = 不画/无几何）。
    fn column_rule_segs(
        &self,
        mc: NodeId,
        layout_by_node: &HashMap<NodeId, (f32, f32, f32, f32)>,
    ) -> Option<Vec<crate::paint::ColumnRuleSeg>> {
        use crate::css::property::{DeclValue, PropertyId};
        let cs = self.styles.get(&mc)?;
        let rule_style = match cs.get(PropertyId::ColumnRuleStyle) {
            Some(DeclValue::ColumnRuleStyle(s)) => *s,
            _ => crate::css::property::BorderStyle::None,
        };
        if matches!(rule_style, crate::css::property::BorderStyle::None) {
            return None;
        }
        let st = self.multicol_state.get(&mc)?;
        if st.n < 2 || st.phantoms.is_empty() {
            return None;
        }
        let rctx = crate::css::value::ResolveCtx {
            em: cs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: self.map_env().viewport_w,
            viewport_h: self.map_env().viewport_h,
            ..crate::css::value::ResolveCtx::base(
                cs.font_size_px(),
                16.0,
                self.map_env().viewport_w,
                self.map_env().viewport_h,
            )
        };
        let rule_w = match cs.get(PropertyId::ColumnRuleWidth) {
            Some(DeclValue::ColumnRuleWidth(Some(lp))) => {
                lp.resolve(&rctx, 0.0).unwrap_or(3.0).max(0.0)
            }
            _ => 3.0, // medium
        };
        if rule_w <= 0.0 {
            return None;
        }
        let rule_color = match cs.get(PropertyId::ColumnRuleColor) {
            Some(DeclValue::Color(cv)) => crate::paint::resolve_color(cv, cs, &self.map_env()),
            _ => crate::paint::resolve_color(
                &crate::css::value::ColorValue::CurrentColor,
                cs,
                &self.map_env(),
            ),
        };
        let (bx, by) = layout_by_node.get(&mc).map(|&(x, y, _, _)| (x, y))?;
        let n = st.n;
        // 行集合：span 模式逐段（rows[i] 行 + 段内幻影切片）；行模式单行
        // （容器直系幻影）。幻影 location 相对父 border-box 原点（行模式
        // 父 = 容器；span 模式再上行一层 row.location）。
        let row_specs: Vec<(Option<taffy::NodeId>, std::ops::Range<usize>)> = if st.span_mode {
            st.rows
                .iter()
                .enumerate()
                .map(|(i, r)| (Some(*r), i * n..(i + 1) * n))
                .collect()
        } else {
            vec![(None, 0..st.phantoms.len())]
        };
        let mut segs = Vec::new();
        for (row, range) in row_specs {
            let row_y = match row {
                Some(r) => self.taffy.layout(r).map(|l| l.location.y).unwrap_or(0.0),
                None => 0.0,
            };
            // (x, y, w, h) 各幻影列（重建帧缺布局的段跳过，下帧自然补齐）。
            let phs: Vec<(f32, f32, f32, f32)> = st.phantoms[range]
                .iter()
                .filter_map(|p| self.taffy.layout(*p).ok())
                .map(|l| (l.location.x, l.location.y, l.size.width, l.size.height))
                .collect();
            if phs.len() < 2 {
                continue;
            }
            let rel_y = row_y + phs[0].1;
            let row_h = phs[0].3;
            for i in 1..phs.len() {
                let (ax, _, aw, _) = phs[i - 1];
                let (cx, _, _, _) = phs[i];
                // 条带中心 = 前列右缘 + 半 gap；宽度 = rule_w。
                let gap = cx - (ax + aw);
                let center = ax + aw + gap * 0.5;
                segs.push(crate::paint::ColumnRuleSeg {
                    x: bx + center - rule_w * 0.5,
                    y: by + rel_y,
                    width: rule_w,
                    height: row_h,
                    color: rule_color,
                });
            }
        }
        Some(segs)
    }

    fn restyle(&mut self) {
        // G1（ADR-0032）：styles 表不清空——保留为 before-change style
        // （class/state/text 变更走 dirty_style 全量路径，若清空则旧值
        // 丢失、过渡无从对账）。restyle_node 父先子后逐节点覆盖；孤儿
        // 条目由 remove()/materialize_pseudos 清理；根重算先行 → 后代
        // 读 rem_base 等仍取新根值（与清空语义一致）。
        self.span_styles.clear();
        self.parents.clear();
        self.wrap_widths.clear();
        self.calc_deferred.clear();
        self.tables.clear();
        self.table_cols.clear();
        self.table_cells.clear();
        self.multicols.clear();
        self.multicol_state.clear();
        #[cfg(feature = "text")]
        self.min_measures.clear();
        let root = self.tree.root();
        // 阶段2③：容器栈自根向叶构建（祖先 → 后代），@container 求值
        // 自栈顶向外查找；无名段命中最近容器。
        let mut cctx: Vec<crate::cascade::ContainerCtx> = Vec::new();
        // 增量脏根一并吸收（全量重算覆盖一切局部失效）。
        self.style_dirty_roots.clear();
        let mut guard = RestyleGuard::default();
        self.restyle_node(root, None, &mut cctx, &mut guard);
        self.dirty_style = false;
    }

    /// 增量重样式（阶段5）：对脏根子树（根 + 全部后代）局部重算样式，
    /// 全树其余节点的 styles/taffy 镜像保持现值。正确性域：节点样式求值
    /// 只依赖①自身/祖先的树数据（类、状态、声明块——兄弟声明互不影响，
    /// :nth-child 按树位不按样式）②继承的父样式（子树重算即重取）
    /// ③祖先容器快照（有容器规则时不走本路径，退全量收敛环）。
    /// 表格/多列子树退全量：幻影列与列模板为跨节点累积状态，v1 增量
    /// 路径不触碰（残余偏差记 FEATURES.md ㉙）。
    fn restyle_subtrees(&mut self, roots: Vec<NodeId>) {
        // 失效根过滤：树中已不存在的根（remove 后残留）跳过。
        let roots: Vec<NodeId> = roots
            .into_iter()
            .filter(|&r| r == self.tree.root() || self.tree.parent(r).is_some())
            .collect();
        if roots.is_empty() {
            return;
        }
        // 枚举各脏根子树节点。
        let mut subtree: Vec<NodeId> = Vec::new();
        for &r in &roots {
            let mut stack = vec![r];
            while let Some(id) = stack.pop() {
                subtree.push(id);
                stack.extend(self.tree.children(id).iter().copied());
            }
        }
        // 表格/多列兜底：子树触及登记表 → 保守全量（此时还未改动任何
        // 登记，全量重算自我清空，无双登记风险）。
        for &id in &subtree {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                self.restyle();
                return;
            }
        }
        // 派生缓存同步：全量路径清 min_measures，增量路径按子树清除，
        // 使重样式节点文本按新样式（如继承字号）重测最小内容宽。
        #[cfg(feature = "text")]
        for &id in &subtree {
            self.min_measures.remove(&id);
        }
        // 容器栈重建：自根向各脏根爬祖先链，按 styles 现值压容器上下文
        //（祖先样式未失效，即全量路径同一栈形状）。
        let mut cctx: Vec<crate::cascade::ContainerCtx> = Vec::new();
        // 去重守卫：脏根互为祖先/后代时先走覆盖后走。
        let mut guard = RestyleGuard::default();
        for r in roots {
            // 祖先容器链（根侧在先）：restyle_node 自根向叶压栈的同形状。
            let mut chain: Vec<NodeId> = Vec::new();
            let mut cur = r;
            while let Some(&p) = self.parents.get(&cur) {
                chain.push(p);
                cur = p;
            }
            chain.reverse();
            cctx.clear();
            for a in chain {
                if let Some(cs) = self.styles.get(&a)
                    && cs.container_type() != crate::css::property::ContainerType::Normal
                {
                    cctx.push(crate::cascade::ContainerCtx {
                        names: cs.container_names().to_vec(),
                        ctype: cs.container_type(),
                        size: self.container_sizes.get(&a).copied(),
                    });
                }
            }
            self.restyle_node(r, self.parents.get(&r).copied(), &mut cctx, &mut guard);
        }
    }

    /// B1：设置用户起源样式表（css-cascade-5 User 层：介于 UA 与 author
    /// 之间；层树独立于 author 表）。样式变化触发全树重样式。
    pub fn set_user_stylesheet(&mut self, css: &str) {
        self.user_sheet = Some(crate::css::stylesheet::parse_stylesheet(css));
        // B4：user 表可携带 @property（合并序最前 = author 覆 user）。
        self.rebuild_registered_props();
        // F3d：user 表可携带 @font-face（合并序最前）。
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// B1：清除用户起源样式表。
    pub fn clear_user_stylesheet(&mut self) {
        self.user_sheet = None;
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// P5（ADR-0033 D1）：设置 UA 起源样式表（css-cascade-5 UserAgent 层：
    /// Default < UA < User < Author；important 反转自动生效）。合并序：
    /// @property/@font-face UA 表注册先于 user 表（起源序）。默认不装载
    /// ——缺省呈现是宿主策略（中立契约）；HTML 缺省表见
    /// `style_engine::builtins::DEFAULT_UA_SHEET`。样式变化触发全树重样式。
    pub fn set_ua_stylesheet(&mut self, css: &str) {
        self.ua_sheet = Some(crate::css::stylesheet::parse_stylesheet(css));
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// P5（ADR-0033 D1）：清除 UA 起源样式表。
    pub fn clear_ua_stylesheet(&mut self) {
        self.ua_sheet = None;
        self.rebuild_registered_props();
        self.rebuild_document_registries();
        // P6（ADR-0035 D1）：:has host 快筛索引同点重建。
        self.rebuild_has_host_index();
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// 宿主推入字体数据（feature = "text"）；字体变化影响文本测量，
    /// 触发全树样式/布局失效。
    #[cfg(feature = "text")]
    pub fn add_font(&mut self, data: Vec<u8>) {
        // A9：注册即探测 ch/ex/ic 度量与族名（失败=不落表→近似缺省）
        if let Some(p) = crate::css::fontprobe::probe_metrics(&data) {
            self.font_metrics.push((
                p.names,
                crate::css::value::FontMetrics {
                    ch_per_em: p.metrics.ch_per_em,
                    ex_per_em: p.metrics.ex_per_em,
                    ic_per_em: p.metrics.ic_per_em,
                    ascent_per_em: p.metrics.ascent_per_em,
                    descent_per_em: p.metrics.descent_per_em,
                },
            ));
        }
        self.text.add_font(data);
        self.dirty_style = true;
        self.dirty_struct = true;
    }

    /// F3d（ADR-0026 D4）：@font-face 登记表只读视图（合并序 = user → 主表
    /// → 附加表；同族后规则胜）。宿主据此映射 local()/url() 键到 add_font
    /// 推送的字节、按 unicode-range/style/weight/stretch 筛选匹配；引擎仅
    /// 供给元数据，不做字体匹配决策（匹配与回退属宿主/后续阶段契约）。
    pub fn font_faces(&self) -> &[crate::css::stylesheet::FontFaceRule] {
        &self.font_faces
    }

    /// C3（ADR-0017 D4）：替换内容叶测量（CSS 10.3.4 简化契约）：
    /// 双边声明=盒取声明值；单边声明=另一边按源宽高比缩放；全 auto=自然
    /// 尺寸（块级替换元素不拉伸）。% 宽以 `avail`（容器可用宽近似）为基。
    /// 返回 None = 非图像叶 / 引用未注册 / 宿主已手动 set_leaf_intrinsic
    ///（此时引擎不接管尺寸；absolute 叶的手动区间经 T5d shrink 通道消费）。
    fn image_leaf_measure(&mut self, id: &NodeId, avail: f32) -> Option<(f32, f32)> {
        let reference = self.tree.node(*id).image.as_ref()?.clone();
        let img = self.images.get(&reference)?;
        // 宿主手动 set_leaf_intrinsic 优先（零侵入契约）。
        if self.intrinsics.contains_key(id) {
            return None;
        }
        let (nw, nh) = (img.width as f32, img.height as f32);
        if nw <= 0.0 || nh <= 0.0 {
            return None;
        }
        let cs = self.styles.get(id)?;
        let env = self.map_env();
        let rc = crate::css::value::ResolveCtx {
            em: cs.font_size_px(),
            rem: self.map_env().rem,
            viewport_w: env.viewport_w,
            viewport_h: env.viewport_h,
            ..crate::css::value::ResolveCtx::base(
                cs.font_size_px(),
                16.0,
                env.viewport_w,
                env.viewport_h,
            )
        };
        let declared = |pid: crate::css::property::PropertyId| -> Option<f32> {
            // width/height 声明可落 Len(Lp) 或 LenAuto(Some(Lp)) 两形。
            match cs.get(pid) {
                Some(crate::css::property::DeclValue::Len(lp)) => lp.resolve(&rc, avail),
                Some(crate::css::property::DeclValue::LenAuto(Some(lp))) => lp.resolve(&rc, avail),
                _ => None,
            }
        };
        let dw = declared(crate::css::property::PropertyId::Width);
        let dh = declared(crate::css::property::PropertyId::Height);
        let (mw, mh) = match (dw, dh) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, nh * (w / nw)),
            (None, Some(h)) => (nw * (h / nh), h),
            (None, None) => (nw, nh),
        };
        // 固有区间注入（absolute 叶经 T5d shrink 通道消费）。
        self.intrinsics.insert(*id, ((nw, nh), (nw, nh)));
        Some((mw, mh.max(0.0)))
    }

    /// C3（ADR-0017 D4）：替换内容叶布局前种子——measures + taffy 尺寸回写。
    /// 静态量（样式不变则幂等跳过，无需 reflow 收敛环）；absolute 叶仅注入
    /// 固有区间，不写 taffy 尺寸（位置/夹紧由 T5d 第三 pass 处理）。
    /// E5（ADR-0020）：grid 放置解析——纯样式派生（容器模板线名+区域
    /// 矩形 → 子放置数字线号），挂点 = seed_image_leaves 之后、
    /// compute_layout 之前（无布局依赖 → 无额外重排；每帧幂等 =
    /// restyle 的 map_style 重置放置 + 本 pass 重施）。
    /// 解析序（<custom-ident>）：区域名 → 全名线 → strip `-start`/`-end`
    /// 裸名线 → 未知名 = Auto（spec：不存在的名视作 auto）。
    /// v1 边界：线名仅支持模板顶层（repeat 内括号 = 解析失败声明无效）。
    fn apply_grid_placements(&mut self) {
        let mut stack: Vec<NodeId> = vec![self.tree.root()];
        while let Some(id) = stack.pop() {
            if self.tables.contains(&id) || self.multicols.contains(&id) {
                continue;
            }
            if self.is_absolute(id) || self.is_fixed(id) {
                continue;
            }
            let is_grid = self.styles.get(&id).is_some_and(|cs| {
                matches!(
                    cs.get(crate::css::property::PropertyId::Display),
                    Some(crate::css::property::DeclValue::Display(
                        crate::css::property::Display::Grid
                    ))
                )
            });
            for c in self.tree.children(id).iter().rev() {
                stack.push(*c);
            }
            if !is_grid {
                continue;
            }
            let (col_lines, row_lines, areas) = self.grid_context(&id);
            let children: Vec<NodeId> = self.tree.children(id).to_vec();
            for sid in children {
                if self.tables.contains(&sid) || self.is_absolute(sid) || self.is_fixed(sid) {
                    continue;
                }
                let Some(&stid) = self.taffy_node.get(&sid) else {
                    continue;
                };
                let Some(cs) = self.styles.get(&sid) else {
                    continue;
                };
                let col_start = cs.get(crate::css::property::PropertyId::GridColumnStart);
                let col_end = cs.get(crate::css::property::PropertyId::GridColumnEnd);
                let row_start = cs.get(crate::css::property::PropertyId::GridRowStart);
                let row_end = cs.get(crate::css::property::PropertyId::GridRowEnd);
                let any = [col_start, col_end, row_start, row_end]
                    .iter()
                    .any(|v| matches!(v, Some(crate::css::property::DeclValue::GridLine(_))));
                if !any {
                    continue;
                }
                let (gs, ge) = Self::resolve_axis(&col_lines, &areas, col_start, col_end, true);
                let (rs, re) = Self::resolve_axis(&row_lines, &areas, row_start, row_end, false);
                let Ok(mut st) = self.taffy.style(stid).cloned() else {
                    continue;
                };
                st.grid_column = taffy::geometry::Line { start: gs, end: ge };
                st.grid_row = taffy::geometry::Line { start: rs, end: re };
                let _ = self.taffy.set_style(stid, st);
            }
        }
    }

    /// E5：容器语境——列/行线名注册表（1 基线号；模板顶层线名槽，
    /// line_names[i] = 第 i+1 号线）+ 区域矩形表（0 基格界）。
    fn grid_context(&self, gid: &NodeId) -> (GridLineMap, GridLineMap, GridAreaMap) {
        let mut col_lines: GridLineMap = std::collections::BTreeMap::new();
        let mut row_lines: GridLineMap = std::collections::BTreeMap::new();
        let mut areas: GridAreaMap = std::collections::BTreeMap::new();
        let Some(cs) = self.styles.get(gid) else {
            return (col_lines, row_lines, areas);
        };
        let reg = |t: Option<&crate::css::property::DeclValue>, out: &mut GridLineMap| {
            if let Some(crate::css::property::DeclValue::GridTracks(t)) = t {
                for (i, names) in t.line_names.iter().enumerate() {
                    for n in names {
                        out.entry(n.clone()).or_default().push(i as i16 + 1);
                    }
                }
            }
        };
        reg(
            cs.get(crate::css::property::PropertyId::GridTemplateColumns),
            &mut col_lines,
        );
        reg(
            cs.get(crate::css::property::PropertyId::GridTemplateRows),
            &mut row_lines,
        );
        if let Some(crate::css::property::DeclValue::GridAreas(a)) =
            cs.get(crate::css::property::PropertyId::GridTemplateAreas)
        {
            let mut spans: std::collections::BTreeMap<&str, (usize, usize, usize, usize)> =
                std::collections::BTreeMap::new();
            for (r, row) in a.rows.iter().enumerate() {
                for (c, name) in row.iter().enumerate() {
                    if name == "." {
                        continue;
                    }
                    let e = spans.entry(name.as_str()).or_insert((r, r, c, c));
                    e.0 = e.0.min(r);
                    e.1 = e.1.max(r);
                    e.2 = e.2.min(c);
                    e.3 = e.3.max(c);
                }
            }
            for (k, v) in spans {
                areas.insert(k.to_string(), v);
            }
        }
        (col_lines, row_lines, areas)
    }

    /// E5：单轴放置对解析（start/end 长手 → taffy 数字放置对）。
    /// SpanName 终界 = start 线后第 k 次名线（候选 = 全名+strip 后缀裸名，
    /// 排序去重；不足 = 末候选+1 钳——隐式线按 spec 计入）；两侧正线号
    /// start ≥ end → end = start+1 钳（跨度至少 1）。
    fn resolve_axis(
        lines: &GridLineMap,
        areas: &GridAreaMap,
        start_spec: Option<&crate::css::property::DeclValue>,
        end_spec: Option<&crate::css::property::DeclValue>,
        is_col: bool,
    ) -> (taffy::style::GridPlacement, taffy::style::GridPlacement) {
        use crate::css::property::GridLineSpec;
        use taffy::style::GridPlacement;
        // 内部放置形（数值先行——taffy GridLine→i16 无 Into 反向转换，
        // 钳位/比较全程 i16 域，末端 conv 一次性转 GridPlacement）。
        enum P {
            Num(i16),
            Span(u16),
            Auto,
        }
        let to_p = |g: &GridLineSpec,
                    lines: &GridLineMap,
                    areas: &GridAreaMap,
                    is_col: bool,
                    is_start: bool|
         -> P {
            match g {
                GridLineSpec::Number(n) => P::Num(*n),
                GridLineSpec::Span(k) => P::Span(*k),
                GridLineSpec::Name(s) => {
                    let v = if is_start {
                        Self::resolve_named_start(s, lines, areas, is_col)
                    } else {
                        Self::resolve_named_end(s, lines, areas, is_col)
                    };
                    v.map(P::Num).unwrap_or(P::Auto)
                }
                GridLineSpec::SpanName(_) | GridLineSpec::Auto => P::Auto,
            }
        };
        let start_p = match start_spec {
            Some(crate::css::property::DeclValue::GridLine(g)) => {
                to_p(g, lines, areas, is_col, true)
            }
            _ => P::Auto,
        };
        let start_num = match &start_p {
            P::Num(v) if *v >= 1 => Some(*v),
            _ => None,
        };
        // SpanName 终界：start 线后第 1 次名线（spec `span <ident>` 隐含
        // k=1；候选 = 全名+strip 后缀裸名，排序去重；不足 = 末候选+1 钳
        // ——隐式线按 spec 计入）。混合形 `span <int> <ident>` v1 偏差在案。
        let end_p = match end_spec {
            Some(crate::css::property::DeclValue::GridLine(GridLineSpec::SpanName(s))) => {
                let from = start_num.unwrap_or(1);
                let base = s
                    .strip_suffix("-start")
                    .or_else(|| s.strip_suffix("-end"))
                    .unwrap_or(s.as_str());
                let mut cands: Vec<i16> = Vec::new();
                if let Some(v) = lines.get(s.as_str()) {
                    cands.extend_from_slice(v);
                }
                if base != s.as_str()
                    && let Some(v) = lines.get(base)
                {
                    cands.extend_from_slice(v);
                }
                cands.sort_unstable();
                cands.dedup();
                match cands.iter().find(|&&l| l > from) {
                    Some(&l) => P::Num(l),
                    None => {
                        let last = cands.last().copied().unwrap_or(from);
                        P::Num(last.max(from) + 1)
                    }
                }
            }
            Some(crate::css::property::DeclValue::GridLine(g)) => {
                to_p(g, lines, areas, is_col, false)
            }
            _ => P::Auto,
        };
        // 两侧正线号 start ≥ end → end = start+1 钳（跨度至少 1）；
        // 借用止于闭包调用（scrutinee 借用不得跨 move）。
        let clamp_pair = |a: &P, b: &P| -> Option<i16> {
            match (a, b) {
                (P::Num(x), P::Num(y)) if *x >= 1 && *y >= 1 && x >= y => Some(x + 1),
                _ => None,
            }
        };
        let end_p = if let Some(nx) = clamp_pair(&start_p, &end_p) {
            P::Num(nx)
        } else {
            end_p
        };
        let conv = |p: P| -> GridPlacement {
            match p {
                P::Num(v) => GridPlacement::Line(v.into()),
                P::Span(k) => GridPlacement::Span(k),
                P::Auto => GridPlacement::Auto,
            }
        };
        (conv(start_p), conv(end_p))
    }

    /// E5：命名起点解析（区域起边 → 全名线首现 → strip 后缀裸名首现）。
    fn resolve_named_start(
        s: &str,
        lines: &GridLineMap,
        areas: &GridAreaMap,
        is_col: bool,
    ) -> Option<i16> {
        if let Some(&(r0, _r1, c0, _c1)) = areas.get(s) {
            return Some(if is_col { c0 as i16 + 1 } else { r0 as i16 + 1 });
        }
        if let Some(v) = lines.get(s) {
            return v.first().copied();
        }
        let base = s
            .strip_suffix("-start")
            .or_else(|| s.strip_suffix("-end"))
            .unwrap_or(s);
        if base != s
            && let Some(v) = lines.get(base)
        {
            return v.first().copied();
        }
        None
    }

    /// E5：命名终点解析（区域止边 = 界+2 → 全名线末现 → strip 后缀裸名末现）。
    fn resolve_named_end(
        s: &str,
        lines: &GridLineMap,
        areas: &GridAreaMap,
        is_col: bool,
    ) -> Option<i16> {
        if let Some(&(_r0, r1, _c0, c1)) = areas.get(s) {
            return Some(if is_col { c1 as i16 + 2 } else { r1 as i16 + 2 });
        }
        if let Some(v) = lines.get(s) {
            return v.last().copied();
        }
        let base = s
            .strip_suffix("-start")
            .or_else(|| s.strip_suffix("-end"))
            .unwrap_or(s);
        if base != s
            && let Some(v) = lines.get(base)
        {
            return v.last().copied();
        }
        None
    }

    fn seed_image_leaves(&mut self) {
        let ids: Vec<NodeId> = self
            .taffy_node
            .keys()
            .copied()
            .filter(|&id| self.tree.children(id).is_empty() && self.tree.node(id).image.is_some())
            .collect();
        for id in ids {
            let absolute = self.is_absolute(id);
            let avail = self.map_env().viewport_w;
            let Some((mw, mh)) = self.image_leaf_measure(&id, avail) else {
                continue;
            };
            if let Some(old) = self.measures.get(&id)
                && (old.0 - mw).abs() <= f32::EPSILON
                && (old.1 - mh).abs() <= f32::EPSILON
            {
                continue;
            }
            self.measures.insert(id, (mw, mh));
            if absolute {
                continue;
            }
            if let Some(&tid) = self.taffy_node.get(&id) {
                let Some(cs) = self.styles.get(&id).cloned() else {
                    continue;
                };
                let mut ts = map_style(&cs, &self.map_env());
                // 声明边保留 map_style 结果，其余边落测量值（自然/比例）。
                ts.size = taffy::prelude::Size {
                    width: if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                        ts.size.width
                    } else {
                        taffy::prelude::Dimension::length(mw)
                    },
                    height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                        ts.size.height
                    } else {
                        taffy::prelude::Dimension::length(mh)
                    },
                };
                let _ = self.taffy.set_style(tid, ts);
            }
        }
    }

    /// 注册背景图（第五批⑨）：background-image: url(ref) 引用 → 宿主
    /// 预解码 RGBA（零副作用——引擎不取 URL、不解码位图格式）；注册于
    /// 样式表设置前后皆可，未注册引用绘制期告警跳过；仅影响绘制
    /// （DisplayList 每帧重建，无需脏标）。
    pub fn add_image(&mut self, reference: &str, width: u32, height: u32, rgba: Vec<u8>) {
        debug_assert_eq!(
            width as usize * height as usize * 4,
            rgba.len(),
            "image rgba buffer must be width*height*4"
        );
        let buf: std::sync::Arc<dyn std::convert::AsRef<[u8]> + Send + Sync> =
            std::sync::Arc::new(rgba);
        self.images.insert(
            reference.to_string(),
            crate::paint::ImageRes {
                width,
                height,
                rgba: buf,
            },
        );
    }

    fn restyle_node(
        &mut self,
        id: NodeId,
        parent_id: Option<NodeId>,
        cctx: &mut Vec<crate::cascade::ContainerCtx>,
        guard: &mut RestyleGuard,
    ) {
        // 增量去重：本轮已重样式（被更早脏根的子树覆盖）直接跳过。
        if guard.skip(id) {
            return;
        }
        if let Some(p) = parent_id {
            self.parents.insert(id, p);
        }
        // 性能（阶段5）：父样式借引用传入（compute_node_in 仅需共享借用），
        // 不再整份克隆 ComputedStyle（全集物化 BTreeMap ~110 项/节点）。
        let parent_style = parent_id.and_then(|p| self.styles.get(&p));
        let author = self.author_sheets();
        // P0 rem 修复：本节点求值环境一次性构建（此前各通道各自
        // self.map_env() 且 rem 恒 16）。文档根（root_key 对应节点——
        // 与 rem_base 同源；用户根挂合成根之下，parent_id 判不出）的
        // font-size 内 rem 按 CSS Values 以初始值解析；求值完成后
        // env.rem 即新根字号，供本节点 span/map_style/度量与通道。
        let is_doc_root = self
            .root_key
            .and_then(|k| self.key_to_node.get(&k))
            .copied()
            == Some(id);
        let mut env = self.map_env();
        if is_doc_root {
            env.rem = 16.0;
        }
        let mut cs = compute_node_in(
            &self.tree,
            id,
            author,
            self.user_sheet.as_ref(),
            &self.registered_props,
            &env,
            parent_style,
            cctx,
            self.ua_sheet.as_ref(),
        );
        // A9：字体相对单位度量（ch/ex/ic）按注册族名补写（未注册=近似缺省）
        let fm = self.metrics_for(&cs);
        cs.set_font_metrics(fm);
        if is_doc_root {
            // 根字号 = rem 基准（本 pass 新值即时生效；后代经 map_env→
            // rem_base 读表，根自身余下通道直接携带）。
            env.rem = cs.font_size_px();
        }
        // 阶段2③：自身是容器 → 入栈（后代 @container 求值用；自身样式已
        // 按祖先快照求值完毕——查询容器不含自身）。快照缺席 = 尺寸 unknown
        //（首帧/收敛中：特性不命中，B 级偏差——未强制 size containment）。
        let is_container = cs.container_type() != crate::css::property::ContainerType::Normal;
        if is_container {
            cctx.push(crate::cascade::ContainerCtx {
                names: cs.container_names().to_vec(),
                ctype: cs.container_type(),
                size: self.container_sizes.get(&id).copied(),
            });
        }
        // ②table：display:table 节点登记（settle_tables 按表内容宽结算列模板）。
        if cs.display() == crate::css::property::Display::Table {
            self.tables.push(id);
        }
        // ③multi-column：多列容器登记（settle_columns 布局期建幻影列并分配）。
        if crate::layout::multicol_requested(&cs) {
            self.multicols.push(id);
        }
        // T5c：span 级联求解——以节点基样式为 parent，复用 compute_node（空临时树）
        let spans = self.tree.node(id).spans.clone();
        let text_len = self
            .tree
            .node(id)
            .text
            .as_ref()
            .map(|t| t.len() as u32)
            .unwrap_or(0);
        if spans.is_empty() {
            self.span_styles.remove(&id);
        } else {
            let mut resolved = Vec::with_capacity(spans.len());
            for sp in &spans {
                let mut tmp = crate::tree::StyleTree::new();
                let tmp_id = tmp.insert_child(
                    tmp.root(),
                    crate::tree::StyleNode {
                        declarations: sp.declarations.clone(),
                        ..Default::default()
                    },
                );
                let author = self.author_sheets();
                let mut scs = compute_node_in(
                    &tmp,
                    tmp_id,
                    author,
                    self.user_sheet.as_ref(),
                    &self.registered_props,
                    &env,
                    Some(&cs),
                    cctx,
                    self.ua_sheet.as_ref(),
                );
                let sfm = self.metrics_for(&scs);
                scs.set_font_metrics(sfm);
                resolved.push((sp.range.0.min(text_len), sp.range.1.min(text_len), scs));
            }
            self.span_styles.insert(id, resolved);
        }
        // T5：未推送测量的文本叶 → 内置 parley 测量（无字体时宽高 0，不落表）
        #[cfg(feature = "text")]
        if !self.measures.contains_key(&id)
            && self
                .tree
                .node(id)
                .text
                .as_ref()
                .is_some_and(|t| !t.is_empty())
        {
            let text = self.tree.node(id).text.clone().unwrap_or_default();
            let owned = self.span_styles.get(&id).cloned().unwrap_or_default();
            let span_refs: Vec<(u32, u32, &ComputedStyle)> =
                owned.iter().map(|(a, b, s)| (*a, *b, s)).collect();
            let (w, h) = self.text.measure_rich(&text, &cs, &span_refs, None, &env);
            if w > 0.0 || h > 0.0 {
                self.measures.insert(id, (w, h));
                let min = self.text.measure_min_content(&text, &cs, &span_refs, &env);
                self.min_measures.insert(id, min);
                self.auto_text.insert(id);
            }
        }
        let mut ts = map_style(&cs, &env);
        // ①calc 直通：捕获本节点延迟 calc 并挂接 taffy 节点（结算基准 =
        // 父内容尺寸；无 taffy 节点则弃置——下次 restyle 重新捕获）。
        if let Some(&tid) = self.taffy_node.get(&id) {
            for raw in crate::layout::take_calc_deferred() {
                self.calc_deferred
                    .push(crate::layout::DeferredCalc { node: tid, raw });
            }
        }
        if let Some((w, h)) = self.measures.get(&id) {
            // 文本叶盒宽语义（第五批⑥契约）：声明宽度优先（CSS 显式 width
            // 胜出测量，旧实现被测量值覆写为缺陷）；无声明时交 taffy auto——
            // 块流拉伸到容器内容宽（与浏览器匿名块盒一致），flex/grid 子项
            // 取内容宽，min/max 宽仍由 taffy 夹紧。absolute 例外：restyle 先
            // 落显式测量宽（第三 pass 再夹紧——auto 时 taffy 绝对定位布局
            // 不回退测量值，compute #1 即需可用）。测量高兜底 height:auto=
            // 内容高；声明高优先。
            let width_dim = if has_declared_len(&cs, crate::css::property::PropertyId::Width) {
                ts.size.width
            } else if matches!(
                cs.get(crate::css::property::PropertyId::Position),
                Some(crate::css::property::DeclValue::Position(
                    crate::css::property::Position::Absolute
                ))
            ) {
                // absolute 例外查本地 cs 而非 is_absolute(id)——后者读
                // self.styles，而本节点 cs 到函数尾才 insert，查表恒 None
                // → auto 宽绝对文本叶丢测量宽，taffy 绝对布局对 auto 叶
                // 无测量回退得 0（bidi-mixed key2/key3 宽超差根因）。
                taffy::prelude::Dimension::length(*w)
            } else {
                taffy::prelude::Dimension::auto()
            };
            ts.size = taffy::prelude::Size {
                width: width_dim,
                height: if has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                    ts.size.height
                } else {
                    taffy::prelude::Dimension::length(*h)
                },
            };
        }
        // ②table：单元格宽度交列模板（映射后覆写）——首行声明宽已折入模板，
        // 单元格一律拉伸至列宽（CSS 单元格 % 基准为表宽而非列宽；v1 契约：
        // 模板唯一权威，单元格自身 width 声明不直接生效）。
        // wrapper 重构：裸单元格（display:table-cell 直属表盒）同样交列
        // 模板——单元格由幻影单格行承接，行内单元格判定在样式父链上补
        // 「父 = 表盒 且 自身 = 单元格」分支。
        if parent_style
            .as_ref()
            .is_some_and(|p| p.display() == crate::css::property::Display::TableRow)
            || (parent_style
                .as_ref()
                .is_some_and(|p| p.display() == crate::css::property::Display::Table)
                && cs.display() == crate::css::property::Display::TableCell)
        {
            ts.size.width = taffy::prelude::Dimension::auto();
        }
        // 容器强制包含（container-type 的规范前提，css-contain-3 size/inline-size
        // containment；container-query 案例暴露的 B 级缺口收敛）：taffy 无 contain
        // 模型，按轴语境最小仿真「内容贡献视空」：
        // - flex 主轴 auto → flex_basis 固定 0 + automatic minimum size 抑制
        //   （min 0）——basis auto 的内容测量被断开；作者显式尺寸/basis 声明
        //   不受影响（containment 只压内容推导，不压显式声明）。
        // - 纵轴 auto（块流高/绝对定位高）→ 固定高 0（块高本就内容推导，
        //   等价空内容；padding/border 照常外扩，box-sizing 语义不变）。
        // 显式 width 的 inline-size 容器（taffy 按声明取值）与块流宽（fill
        // 本就内容无关）天然合规，无需干预；grid 轨道与 flex 交叉轴
        // align 非 stretch 的内容贡献断链留待后续（B 级，FEATURES 记偏差）。
        if is_container {
            let ctype = cs.container_type();
            let inline_contained = ctype != crate::css::property::ContainerType::Normal;
            let size_contained = ctype == crate::css::property::ContainerType::Size;
            let parent_flex_dir = parent_style
                .as_ref()
                .filter(|p| p.display() == crate::css::property::Display::Flex)
                .map(|p| p.flex_direction());
            let main_axis_is_row = !matches!(
                parent_flex_dir,
                Some(crate::css::property::FlexDirection::Column)
                    | Some(crate::css::property::FlexDirection::ColumnReverse)
            );
            if inline_contained
                && !has_declared_len(&cs, crate::css::property::PropertyId::Width)
                && ts.flex_basis.is_auto()
                && main_axis_is_row
                && parent_flex_dir.is_some()
            {
                ts.flex_basis = taffy::prelude::Dimension::length(0.0);
                ts.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
            }
            if size_contained && !has_declared_len(&cs, crate::css::property::PropertyId::Height) {
                if parent_flex_dir.is_some() && !main_axis_is_row {
                    // flex 列主轴：高 = 内容推导（basis auto）→ 断开
                    if ts.flex_basis.is_auto() {
                        ts.flex_basis = taffy::prelude::Dimension::length(0.0);
                        ts.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
                    }
                } else {
                    ts.size.height = taffy::prelude::Dimension::length(0.0);
                }
            }
        }
        if let Some(&tid) = self.taffy_node.get(&id) {
            let _ = self.taffy.set_style(tid, ts);
        }
        // C4（ADR-0018）：::selection / ::placeholder 通道（须在 cs 移入
        // styles 前取 &cs 作继承基）。
        self.style_channels(id, &cs, cctx, &env);
        // G1（ADR-0032）：transition reconciliation——提交前对新旧计算值
        // 对账，启动/重定向/取消过渡（from 端 = 旧 styles 表的生效值，
        // 含过渡采样中间值 = before-change 语义）。
        self.reconcile_transitions(id, &cs);
        self.styles.insert(id, cs);
        guard.mark(id);
        let children: Vec<NodeId> = self.tree.children(id).to_vec();
        for c in children {
            self.restyle_node(c, Some(id), cctx, guard);
        }
        if is_container {
            cctx.pop();
        }
    }

    /// 深度优先收集布局盒。taffy 的 `location` 是父相对坐标，故携带祖先累计偏移
    /// 合成视口绝对坐标（合成视口根引入后，树根不再是 taffy 根）。
    fn collect(
        &self,
        id: NodeId,
        ox: f32,
        oy: f32,
        out: &mut Vec<LayoutEntry<K>>,
        layout_by_node: &mut HashMap<NodeId, (f32, f32, f32, f32)>,
    ) {
        let mut x = ox;
        let mut y = oy;
        if let Some(&tid) = self.taffy_node.get(&id)
            && let Ok(l) = self.taffy.layout(tid)
        {
            // 三期②：重挂 absolute 的坐标 = cb border box 原点 + 自身
            // location（taffy 绝对锚定 = cb padding box，location 已含
            // cb border 偏移）；ICB → 视口原点。cb 为样式树祖先，本 DFS
            // 先序保证 layout_by_node[cb] 已就绪（嵌套 absolute 同序）。
            if let Some(cb) = self.abs_cb.get(&id) {
                let (bx, by) = match cb {
                    Some(cb_id) => layout_by_node
                        .get(cb_id)
                        .map(|&(bx, by, _, _)| (bx, by))
                        .unwrap_or((ox, oy)),
                    None => (0.0, 0.0),
                };
                x = bx + l.location.x;
                y = by + l.location.y;
            } else {
                x = l.location.x + ox;
                y = l.location.y + oy;
            }
            // 表格 wrapper 重构：表元素矩形 = wrapper 原点 + 内表 location
            //（wrapper 无 border/padding；子件递归基准仍是 wrapper 原点，
            // inner/幻影行偏移由下方 effp 补偿自动叠加——内表子件的
            // taffy_parent 指向 inner，链式累加到 wrapper 原点）。
            let box_rect = match self
                .taffy_table_inner
                .get(&id)
                .and_then(|&itid| self.taffy.layout(itid).ok())
            {
                Some(il) => (
                    x + il.location.x,
                    y + il.location.y,
                    il.size.width,
                    il.size.height,
                ),
                None => (x, y, l.size.width, l.size.height),
            };
            layout_by_node.insert(id, box_rect);
            if let Some(&key) = self.node_to_key.get(&id) {
                out.push(LayoutEntry {
                    key,
                    x: box_rect.0,
                    y: box_rect.1,
                    width: box_rect.2,
                    height: box_rect.3,
                });
            }
        }
        for c in self.tree.children(id) {
            // 三期②：重挂 absolute 的坐标独立于样式父链——递归基准归零，
            // 由节点自身 cb 分支推导；并跳过幻影 effp 补偿（防双重偏移）。
            if self.abs_cb.contains_key(c) {
                self.collect(*c, 0.0, 0.0, out, layout_by_node);
                continue;
            }
            // ③multi-column / ⑤b：子节点 taffy 父可能是合成层（幻影列 →
            // 段行 → 容器）——沿 taffy_parent 链上溯到样式节点本身，累加
            // 全部合成层偏移（列相对行、行相对容器的 location 之和）。
            let (mut cx, mut cy) = (x, y);
            if let Some(&tid_c) = self.taffy_node.get(c)
                && let Some(mut effp) = self.taffy_parent.get(&tid_c).copied()
            {
                let mut depth = 0usize;
                while self.taffy_node.get(&id) != Some(&effp) && depth < 16 {
                    if let Ok(pl) = self.taffy.layout(effp) {
                        cx += pl.location.x;
                        cy += pl.location.y;
                    }
                    match self.taffy_parent.get(&effp) {
                        Some(&pp) => effp = pp,
                        None => break,
                    }
                    depth += 1;
                }
            }
            self.collect(*c, cx, cy, out, layout_by_node);
        }
    }
}

/// 样式树结构递归投影（F3e，ADR-0027 D1）：`layout_tree_dump` 的行生产器。
/// 标签 = 元素名（匿名 `anon`）/`#id`/`.class` 连串/`key=K`（宿主映射时）
/// /`[::before|::after]`（伪实体）/`text="…"`（截断 24 字节）。深度缩进
/// 两空格一级。
fn dump_tree_node<K: Copy + std::fmt::Debug>(
    tree: &StyleTree,
    keys: &mut HashMap<NodeId, K>,
    id: NodeId,
    depth: usize,
    out: &mut String,
) {
    let node = tree.node(id);
    out.push_str(&"  ".repeat(depth));
    out.push('<');
    out.push_str(node.name.as_deref().unwrap_or("anon"));
    if let Some(i) = &node.id {
        out.push_str(&format!(" #{i}"));
    }
    for c in &node.classes {
        out.push_str(&format!(".{c}"));
    }
    out.push('>');
    if let Some(k) = keys.remove(&id) {
        out.push_str(&format!(" key={k:?}"));
    }
    if let Some(p) = node.pseudo {
        out.push_str(&format!(" [pseudo={p:?}]"));
    }
    if let Some(t) = &node.text {
        let head: String = t.chars().take(24).collect();
        out.push_str(&format!(" text={head:?}"));
    }
    out.push('\n');
    for &c in tree.children(id) {
        dump_tree_node(tree, keys, c, depth + 1, out);
    }
}

/// class 语义归一（CSS class 属性为空格分隔 token）：把每个条目按 ASCII
/// 空白拆开并丢弃空项——宿主传 `"row alt"` 与 `["row", "alt"]` 等价。
fn normalize_classes(input: &[String]) -> smallvec::SmallVec<[String; 4]> {
    input
        .iter()
        .flat_map(|c| c.split_ascii_whitespace())
        .map(String::from)
        .collect()
}

/// 两阶段包含块内容宽的水平内缩项（T5c-2 收口）：padding 恒计；border 仅在
/// style 非 none 时计入（CSS used width：style none 时边框宽归零，初始
/// medium 不参与）。width 侧兼容 Len 与 BorderWidth 两种物化。
fn used_h_inset(
    cs: &ComputedStyle,
    width_id: crate::css::property::PropertyId,
    style_id: Option<crate::css::property::PropertyId>,
    rctx: &crate::css::value::ResolveCtx,
) -> Option<f32> {
    if let Some(sid) = style_id {
        let none = match cs.get(sid) {
            Some(crate::css::property::DeclValue::BorderStyle(s)) => {
                matches!(s, crate::css::property::BorderStyle::None)
            }
            _ => true,
        };
        if none {
            return None;
        }
    }
    match cs.get(width_id) {
        Some(crate::css::property::DeclValue::Len(lp)) => lp.resolve(rctx, 0.0),
        Some(crate::css::property::DeclValue::BorderWidth(Some(lp))) => lp.resolve(rctx, 0.0),
        _ => None,
    }
}

/// 声明长度检查（第五批⑥文本叶契约）：width/height 是否被显式声明
/// （`LenAuto(Some)`；auto=None 不算声明）。声明值优先于文本测量。
fn has_declared_len(cs: &ComputedStyle, pid: crate::css::property::PropertyId) -> bool {
    matches!(
        cs.get(pid),
        Some(crate::css::property::DeclValue::LenAuto(Some(_)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::NodeState;

    /// API 冻结（阶段3）：公共类型线程安全承诺（C3）——静态断言防回归。
    /// StyleEngine/Frame 须可跨线程移动（宿主在渲染线程消费 DisplayList）。
    #[test]
    fn public_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StyleEngine<Key>>();
        assert_send_sync::<Frame<Key>>();
        assert_send_sync::<ComputedStyle>();
        assert_send_sync::<crate::paint::DisplayList>();
        assert_send_sync::<ParseReport>();
        assert_send_sync::<crate::error::ContractError>();
    }

    #[test]
    fn rich_text_spans_reach_paint() {
        use crate::tree::TextSpan;
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.set_stylesheet("").is_clean());
        let span_decls =
            crate::css::decl::parse_inline_declarations("color: #ff0000; font-weight: 700").0;
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("span".into()),
                        text: Some("hello link world".into()),
                        spans: std::iter::once(TextSpan {
                            range: (6, 10),
                            declarations: span_decls,
                        })
                        .collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let (text, spans) = frame
            .paint
            .ops
            .iter()
            .find_map(|op| match op {
                crate::paint::PaintOp::Text { text, spans, .. } => Some((text.as_str(), spans)),
                _ => None,
            })
            .expect("text op");
        assert_eq!(text, "hello link world");
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!((s.start, s.end), (6, 10));
        assert_eq!(s.color.components, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.font_weight, 700.0);
    }

    #[test]
    fn margin_and_block_flow() {
        // 树根 margin 生效（合成视口根）+ 无 margin 定宽块级子盒靠 inline-start（不被 auto-margin 居中）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.card { width: 300px; height: 160px; margin: 40px; padding: 16px; } div.kid { width: 100px; height: 20px; }"
            )
            .is_clean());
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("card".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("kid".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert_eq!(frame.boxes[0].x, 40.0);
        assert_eq!(frame.boxes[0].y, 40.0);
        // 子盒：padding 内 inline-start，而非 auto-margin 居中
        assert_eq!(frame.boxes[1].x, 56.0); // 40 + 16
    }

    /// 真实字体（demo 资产，同仓自由许可）：让测量/换行通路在测试中真实生效。
    const TEST_FONT: &[u8] = include_bytes!("../../style-engine-demo/assets/fonts/DejaVuSans.ttf");
    const TEST_FONT_CJK: &[u8] =
        include_bytes!("../../style-engine-demo/assets/fonts/NotoSansSC.ttf");

    #[test]
    fn wrap_avail_subtracts_padding_and_used_border() {
        // T5c-2 收口：包含块内容宽 = width − padding − 已生效 border
        //（border-style 为 none 时宽归零，初始 medium 不计入）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                // 泛族 sans-serif 在零副作用集合中无解析（ADR-0006），测试显式指到注册字体。
                // 父级 auto 宽（content-box 下 auto 与 box-sizing 无关）：
                // wrap avail = 根 border-box − 自身 padding − 有效 border。
                "div { font-family: \"DejaVu Sans\"; } div.p { padding: 10px; } div.b { padding: 10px; border: 5px solid black; }"
            )
            .is_clean());
        let node = |classes: &str, text: bool| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: if text {
                Some("the quick brown fox jumps over the lazy dog".into())
            } else {
                None
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("", false)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("p", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(2)), Key(3), node("", true)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("b", false))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(4)), Key(5), node("", true)).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let id5 = *engine.key_to_node.get(&Key(5)).unwrap();
        assert_eq!(engine.wrap_widths.get(&id3), Some(&Some(780.0)));
        assert_eq!(engine.wrap_widths.get(&id5), Some(&Some(770.0)));
    }

    #[test]
    fn keyframes_animation_sampling() {
        // 第五批⑰：动画采样——线性插值、fill both 端点保持、无 fill 回
        // 底层值；缓动按段施加（linear 下数值即线性）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear both; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        let width_at = |engine: &mut StyleEngine<Key>, t: f64| -> f32 {
            let _ = engine.frame((400.0, 100.0), 1.0, t);
            match engine
                .styles
                .get(&idn)
                .unwrap()
                .get(crate::css::property::PropertyId::Width)
            {
                Some(crate::css::property::DeclValue::LenAuto(Some(
                    crate::css::value::LengthPercentage::Px(v),
                ))) => *v,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(width_at(&mut engine, 0.0), 100.0);
        assert_eq!(width_at(&mut engine, 0.5), 150.0);
        assert_eq!(width_at(&mut engine, 0.25), 125.0);
        assert_eq!(width_at(&mut engine, 1.0), 200.0);
        // fill both：结束后停在终点
        assert_eq!(width_at(&mut engine, 2.0), 200.0);
        // 无 fill：结束后回底层值
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             div { width: 50px; animation: grow 1s linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(width_at(&mut engine, 2.0), 50.0);
    }

    #[test]
    fn animation_multi_groups_parallel() {
        // P7-②：多动画组并行采样——组 0 both（结束后驻留终值）、组 1 无
        // fill（结束后回底层值）；同节点两组各自推进互不干扰。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             @keyframes fade { from { opacity: 1 } to { opacity: 0 } } \
             div { width: 50px; opacity: 1; animation: grow 1s linear both, fade 2s linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        use crate::css::property::{DeclValue, PropertyId};
        use crate::css::value::LengthPercentage;
        let probe = |engine: &mut StyleEngine<Key>, t: f64| -> (f32, f32) {
            let _ = engine.frame((400.0, 100.0), 1.0, t);
            let cs = engine.styles.get(&idn).unwrap();
            let width = match cs.get(PropertyId::Width) {
                Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) => *v,
                other => panic!("width: {other:?}"),
            };
            let opacity = match cs.get(PropertyId::Opacity) {
                Some(DeclValue::Number(v)) => *v,
                other => panic!("opacity: {other:?}"),
            };
            (width, opacity)
        };
        // t=0.5：组 0 进度 0.5（width 150）、组 1 进度 0.25（opacity 0.75）。
        assert_eq!(probe(&mut engine, 0.5), (150.0, 0.75));
        // t=1.5：组 0 结束驻留终值（both）、组 1 进度 0.75（opacity 0.25）。
        assert_eq!(probe(&mut engine, 1.5), (200.0, 0.25));
        // t=2.5：全部结束——组 0 终值驻留、组 1 回底层值。
        assert_eq!(probe(&mut engine, 2.5), (200.0, 1.0));
    }

    #[test]
    fn animation_group_descriptor_cycling() {
        // P7-②：描述符循环补齐——name 两组共用单值 duration/timing
        //（CSS：各描述符列表按 i%len 循环）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "@keyframes grow { from { width: 100px } to { width: 200px } } \
             @keyframes fade { from { opacity: 1 } to { opacity: 0 } } \
             div { width: 50px; animation-name: grow, fade; \
                   animation-duration: 1s; animation-timing-function: linear; }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        let idn = *engine.key_to_node.get(&Key(1)).unwrap();
        use crate::css::property::{DeclValue, PropertyId};
        use crate::css::value::LengthPercentage;
        let _ = engine.frame((400.0, 100.0), 1.0, 0.5);
        let cs = engine.styles.get(&idn).unwrap();
        let width = match cs.get(PropertyId::Width) {
            Some(DeclValue::LenAuto(Some(LengthPercentage::Px(v)))) => *v,
            other => panic!("width: {other:?}"),
        };
        let opacity = match cs.get(PropertyId::Opacity) {
            Some(DeclValue::Number(v)) => *v,
            other => panic!("opacity: {other:?}"),
        };
        // 两组同 1s 线性：t=0.5 各自进度 0.5。
        assert_eq!(width, 150.0);
        assert!((opacity - 0.5).abs() < 1e-4, "opacity: {opacity}");
    }

    #[test]
    fn calc_layout_resolution() {
        // 二期①calc 直通：taffy 原生指针传输层公共接入点被 pub(crate) 内部
        // 阻断——引擎侧结算式直通（layout.rs DeferredRaw / settle_calc）：
        // px 部分首遍折叠，百分比部分以父内容尺寸为基准结算后回写固定值。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: calc(100px + 50px); height: 20px } \
             p { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let d = frame.find(Key(1)).unwrap();
        assert_eq!(d.width, 150.0, "纯长度 calc 应在布局期正确解析");
        let p = frame.find(Key(2)).unwrap();
        assert_eq!(p.width, 85.0, "含百分比 calc 结算后 = 50%×150+10");
    }

    #[test]
    fn calc_percent_chain_settles() {
        // 二期①calc 直通：两级百分比链——p1 = 50%×200+10 = 110（1 遍结算），
        // p2 = 50%×110+10 = 65（p1 稳定后方可结算，需第 2 遍）；收敛上限
        // 3 遍覆盖 3 层内链路。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "div { width: 200px; height: 20px } \
             p1 { width: calc(50% + 10px); height: 20px } \
             p2 { width: calc(50% + 10px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("div")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("p1")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("p2")).is_ok());
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        let p1 = frame.find(Key(2)).unwrap();
        assert_eq!(p1.width, 110.0, "一级链 1 遍结算");
        let p2 = frame.find(Key(3)).unwrap();
        assert_eq!(p2.width, 65.0, "二级链经第 2 遍结算收敛");
    }

    #[test]
    fn calc_slot_flex_basis_settles_both_axes() {
        // 三期③槽位扩展：flex-basis 含百分比 calc 延迟结算——基=父容器
        // 主轴内容尺寸（行向=宽、列向=高，settle 期按父 flex_direction 判定）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             row { display: flex; flex-direction: row; width: 400px; height: 20px } \
             rit { flex-basis: calc(25% + 50px); flex-grow: 0; height: 20px } \
             col { display: flex; flex-direction: column; width: 20px; height: 600px } \
             cit { flex-basis: calc(50% - 100px); flex-grow: 0; width: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("rit")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("col")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(5), mk("cit")).is_ok());
        let frame = engine.frame((400.0, 800.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            150.0,
            "行向基=容器内容宽：25%×400+50"
        );
        assert_eq!(
            frame.find(Key(5)).unwrap().height,
            200.0,
            "列向基=容器内容高：50%×600−100"
        );
    }

    #[test]
    fn calc_slot_min_max_settles() {
        // 三期③槽位扩展：min/max-width 含百分比 calc 延迟结算（基=父内容
        // 宽）——min 抬升过窄子盒、max 压制过宽子盒。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 20px } \
             mn { width: 10px; min-width: calc(50% - 30px); height: 20px } \
             mx { width: 1000px; max-width: calc(50% + 50px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("mn")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("mx")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            170.0,
            "min-width = 50%×400−30 抬升"
        );
        assert_eq!(
            frame.find(Key(3)).unwrap().width,
            250.0,
            "max-width = 50%×400+50 压制"
        );
    }

    #[test]
    fn calc_slot_margin_percent_base_is_width() {
        // 三期③槽位扩展：margin 含百分比 calc 延迟结算——CSS 2.1 §8.3
        // 百分比 margin（含 top/bottom）恒以包含块宽度为基而非高度：
        // margin-top 10%×400+5 = 45（若误用高基 300×10%=30+5=35）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "pw { width: 400px; height: 300px } \
             it { width: 100px; height: 20px; margin-left: calc(50% - 50px); \
                  margin-top: calc(10% + 5px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("pw")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        let frame = engine.frame((400.0, 400.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        assert_eq!(it.x, 150.0, "margin-left = 50%×400−50");
        assert_eq!(
            it.y, 45.0,
            "margin-top 以宽度为基 = 10%×400+5（首子 margin 与父塌陷后父子同位）"
        );
    }

    #[test]
    fn calc_slot_padding_settles_and_clamps() {
        // 三期③槽位扩展：padding 含百分比 calc 延迟结算（基=包含块内容宽，
        // 含 padding-top——CSS 2.1 §8.4）；负值钳 0（padding 负值非法）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "wrap { width: 400px } \
             it { width: 100px; height: 20px; padding: calc(10% + 5px) } \
             cc { width: 80px; height: 10px } \
             nz { width: 50px; height: 10px; padding-left: calc(0% - 10px) }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("it")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("nz")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        let it = frame.find(Key(2)).unwrap();
        let cc = frame.find(Key(3)).unwrap();
        assert_eq!(it.width, 190.0, "border-box = 100 + 两侧 padding 45");
        assert_eq!(cc.x, 45.0, "内容随 padding-left 内缩（宽基 10%×400+5）");
        assert_eq!(cc.y, 45.0, "padding-top 同以宽度为基");
        let nz = frame.find(Key(4)).unwrap();
        assert_eq!(nz.width, 50.0, "padding-left 负值钳 0（不缩宽度）");
    }

    #[test]
    fn calc_slot_gap_settles() {
        // 三期③槽位扩展：column-gap 含百分比 calc 延迟结算（列隙基=容器
        // 内容宽 20%×400+10 = 90 → 第二项 x=190）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "fx { display: flex; width: 400px; height: 20px; column-gap: calc(20% + 10px) } \
             ia { width: 100px; height: 20px } \
             ib { width: 100px; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("fx")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("ia")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("ib")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 190.0, "gap = 90 结算后推进");
    }

    #[test]
    fn calc_slot_two_level_chain_converges() {
        // 三期③：跨槽位两级链——mid.width 结算后 inner.min-width 以其为基
        // （需第 2 遍，3 遍上限内收敛）：mid = 50%×400+10 = 210；
        // inner min-width = 50%×210−5 = 100。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mid { width: calc(50% + 10px); height: 20px } \
             inner { width: 10px; min-width: calc(50% - 5px); height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mid")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("inner")).is_ok());
        let frame = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(1)).unwrap().width, 210.0);
        assert_eq!(
            frame.find(Key(2)).unwrap().width,
            100.0,
            "min-width 以已结算父宽为基（跨槽位第 2 遍）"
        );
    }

    #[test]
    fn table_two_stage_column_settlement() {
        // 二期②：table→块容器、row→单行 Grid（共享列模板）、cell→Grid 项；
        // settle_tables 以表内容宽结算列模板（150 定宽 + 50% + auto 均分），
        // 第二遍重排后各单元格宽度/列位与 CSS 表模型一致。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             ca { display: table-cell; width: 150px } \
             cb { display: table-cell; width: 50% } \
             cc { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        for (parent, k, name) in [
            (Key(2), Key(4), "ca"),
            (Key(2), Key(5), "cb"),
            (Key(2), Key(6), "cc"),
            (Key(3), Key(7), "ca"),
            (Key(3), Key(8), "cb"),
            (Key(3), Key(9), "cc"),
        ] {
            assert!(engine.insert(Some(parent), k, mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let row1 = frame.find(Key(2)).unwrap();
        assert_eq!(row1.width, 600.0, "行宽=表内容宽");
        let w = |k: Key| frame.find(k).unwrap().width;
        let x = |k: Key| frame.find(k).unwrap().x;
        assert_eq!(w(Key(4)), 150.0, "定宽列");
        assert_eq!(w(Key(5)), 300.0, "50% 列 = 表内容宽×0.5");
        assert_eq!(w(Key(6)), 150.0, "auto 列均分剩余");
        assert_eq!(w(Key(7)), 150.0, "第二行共享模板");
        assert_eq!(w(Key(8)), 300.0);
        assert_eq!(w(Key(9)), 150.0);
        assert_eq!(x(Key(4)), 0.0);
        assert_eq!(x(Key(5)), 150.0);
        assert_eq!(x(Key(6)), 450.0, "列位随模板推进");
        // 稳态帧：模板缓存全等 → 不再触发额外重排（幂等）。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().width, 300.0);
    }

    #[test]
    fn table_colspan_expands_column_map() {
        // 三期④a：colspan 属性（StyleNode.attrs）→ 单元格图显式列位；
        // span-n 声明宽均分给跨内未声明列（Chromium 简单表一致）。
        // 列模板 [100,100,100,100]：ca 定宽 100 占列 1，cb 宽 300
        // colspan=3 占列 2–4；第二行四格共享模板并显式列位推进。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row } \
             ca { display: table-cell; width: 100px; height: 30px } \
             cb { display: table-cell; width: 300px; height: 30px } \
             cd { display: table-cell; height: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, span: &str| StyleNode {
            name: Some(name.to_string()),
            attrs: [("colspan".to_string(), span.to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("ca")).is_ok());
        assert!(
            engine
                .insert(Some(Key(2)), Key(5), mk_span("cb", "3"))
                .is_ok()
        );
        for k in 6..=9 {
            assert!(engine.insert(Some(Key(3)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let (w, x, y) = (
            |k: Key| frame.find(k).unwrap().width,
            |k: Key| frame.find(k).unwrap().x,
            |k: Key| frame.find(k).unwrap().y,
        );
        assert_eq!(w(Key(4)), 100.0, "定宽列 1");
        assert_eq!(x(Key(5)), 100.0, "跨列单元起于列 2");
        assert_eq!(w(Key(5)), 300.0, "跨 3 列 = 100×3");
        for (i, k) in (6..=9).enumerate() {
            assert_eq!(w(Key(k)), 100.0, "第二行共享模板");
            assert_eq!(x(Key(k)), i as f32 * 100.0, "显式列位推进");
            assert_eq!(y(Key(k)), 30.0, "第二行位于首行下方");
        }
        // 稳态幂等：列位签名全等不重写。
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let frame3 = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame3.find(Key(5)).unwrap().x, 100.0);
    }

    #[test]
    fn table_auto_columns_proportional_to_content() {
        // P7-①：auto 列剩余宽按单元格内容 max-content 比例分配——长
        // 内容列显著宽于短内容列（v1 均分时两列各 300）；等内容两列
        // 退回等宽。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        let report = engine.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             td { display: table-cell; height: 30px; white-space: nowrap; \
                  font-family: \"DejaVu Sans\" }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str, text: &str| StyleNode {
            name: Some(name.to_string()),
            text: Some(text.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab", "")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row", "")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("td", "ab")).is_ok());
        assert!(
            engine
                .insert(Some(Key(2)), Key(4), mk("td", "wide content here"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let w3 = frame.find(Key(3)).unwrap().width;
        let w4 = frame.find(Key(4)).unwrap().width;
        assert!(
            (w3 + w4 - 600.0).abs() < 0.5,
            "两 auto 列应铺满表宽: {w3}+{w4}"
        );
        assert!(w4 > w3 * 3.0, "内容比例分配: {w3} vs {w4}");

        // 等内容两列 → 等宽（测量接入后均分语义保持）。
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.add_font(TEST_FONT.to_vec());
        let report2 = engine2.set_stylesheet(
            "tab { display: table; width: 600px } \
             row { display: table-row } \
             td { display: table-cell; height: 30px; white-space: nowrap; \
                  font-family: \"DejaVu Sans\" }",
        );
        assert!(report2.is_clean(), "{report2:?}");
        assert!(engine2.insert(None, Key(1), mk("tab", "")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(2), mk("row", "")).is_ok());
        assert!(engine2.insert(Some(Key(2)), Key(3), mk("td", "ab")).is_ok());
        assert!(engine2.insert(Some(Key(2)), Key(4), mk("td", "ab")).is_ok());
        let frame2 = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame2.find(Key(3)).unwrap().width;
        let b = frame2.find(Key(4)).unwrap().width;
        assert!(
            (a - b).abs() < 0.5 && (a + b - 600.0).abs() < 0.5,
            "等内容两列等宽: {a} vs {b}"
        );
    }

    #[test]
    fn table_row_group_stacks_rows() {
        // 三期④a：行组（table-row-group）=纵向透明块包装，行发现穿透；
        // 组内行与表直系行混排（直系行在组后接续堆叠）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             grp { display: table-row-group } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("grp")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(6), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(7), mk("cell")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(8), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 行组透明：组内两行 y=0/30，直系行接续 y=50。
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(5)).y, 60.0, "直系行接在行组之后（组内 30+30）");
        // 列模板跨行共享（首行单元格声明穿透行组生效）。
        for k in [Key(6), Key(7), Key(8)] {
            assert_eq!(b(k).width, 100.0);
            assert_eq!(b(k).x, 0.0);
        }
        assert_eq!(b(Key(1)).height, 80.0, "表高 = 30+30+20");
    }

    #[test]
    fn table_caption_sits_above_rows() {
        // 三期④a：caption=普通块盒置于行区上方，宽=表内容宽，不参与列发现。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             cap { display: table-caption; height: 20px } \
             row { display: table-row; height: 30px } \
             cell { display: table-cell; width: 100px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("cap")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(3)), Key(4), mk("cell")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0, "caption 在最上");
        assert_eq!(b(Key(2)).height, 20.0);
        assert_eq!(b(Key(2)).width, 400.0, "caption 宽=表内容宽");
        assert_eq!(b(Key(3)).y, 20.0, "行区在 caption 之下");
        assert_eq!(b(Key(4)).width, 100.0, "caption 不参与列发现");
        assert_eq!(b(Key(1)).height, 50.0);
    }

    #[test]
    fn table_rowspan_spans_rows_in_home_grid() {
        // 三期④c：rowspan=2 单元格留在宿主行网格（列位照旧），高度覆写 =
        // 被跨行 definite 高度和（30+20=50）向下溢出覆盖；后行游标跳过被
        // 占列（第二行 col 2–4 被占，单格落列 1）；第三行四格补齐列 1–4。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 400px } \
             row { display: table-row; height: 30px } \
             row2 { display: table-row; height: 20px } \
             row3 { display: table-row; height: 20px } \
             ca { display: table-cell; width: 100px } \
             cb { display: table-cell; width: 300px } \
             cc { display: table-cell; width: 100px } \
             cd { display: table-cell }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let mk_span = |name: &str, attrs: &[(&str, &str)]| StyleNode {
            name: Some(name.to_string()),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("row3")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("ca")).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(2)),
                    Key(6),
                    mk_span("cb", &[("colspan", "3"), ("rowspan", "2")])
                )
                .is_ok()
        );
        assert!(engine.insert(Some(Key(3)), Key(7), mk("cc")).is_ok());
        for k in 8..=11 {
            assert!(engine.insert(Some(Key(4)), Key(k), mk("cd")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 跨行单元：宿主行内起列 2，高 = 30+20。
        assert_eq!(b(Key(6)).x, 100.0);
        assert_eq!(b(Key(6)).y, 0.0, "无 caption，首行在表顶");
        assert_eq!(b(Key(6)).width, 300.0);
        assert_eq!(b(Key(6)).height, 50.0, "rowspan 覆写 = 30+20");
        // 第二行：col 2–4 被占，唯一格落列 1。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).y, 30.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(7)).height, 20.0);
        // 第三行四格补齐列 1–4。
        for (i, k) in (8..=11).enumerate() {
            assert_eq!(b(Key(k)).x, i as f32 * 100.0);
            assert_eq!(b(Key(k)).y, 50.0);
            assert_eq!(b(Key(k)).width, 100.0);
        }
        assert_eq!(b(Key(1)).height, 70.0, "表高 = 30+20+20");
    }

    #[test]
    fn table_row_non_cell_becomes_anonymous_cell() {
        // 三期④d：行内非单元格元素（display:block div）→ 匿名单元格
        // （CSS 2.1 §17.2.1 第 3 条）：入单元格图、列位分配、宽度拉伸；
        // display:none 与 absolute 子件不入图。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "tab { display: table; width: 300px } \
             row { display: table-row; height: 20px } \
             row2 { display: table-row; height: 25px } \
             ca { display: table-cell; width: 100px } \
             cb { display: block; width: 200px } \
             cc { display: block } \
             hidden { display: none }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("tab")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("row")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("ca")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("cb")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(5), mk("hidden")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("row2")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(7), mk("cc")).is_ok());
        assert!(engine.insert(Some(Key(6)), Key(8), mk("ca")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 首行：td 列 1，div 匿名单元格列 2（列模板 [100,200]）。
        assert_eq!(b(Key(3)).x, 0.0);
        assert_eq!(b(Key(3)).width, 100.0);
        assert_eq!(b(Key(4)).x, 100.0, "div 匿名单元格起列 2");
        assert_eq!(b(Key(4)).width, 200.0);
        // 第二行：div 落列 1（auto 拉伸 100），td 落列 2 拉伸 200。
        assert_eq!(b(Key(7)).x, 0.0);
        assert_eq!(b(Key(7)).width, 100.0);
        assert_eq!(b(Key(8)).x, 100.0);
        assert_eq!(b(Key(8)).width, 200.0, "声明 100 拉伸到列宽 200");
        assert_eq!(b(Key(1)).height, 45.0, "表高 = 20+25");
    }

    #[test]
    fn multicol_balance_two_columns() {
        // 二期③：column-count:2 → settle_columns 建幻影列、按理想高贪心
        // 平衡分配；子节点坐标经幻影补偿后与真实列位一致（列宽=(400−20)/2）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc = frame.find(Key(1)).unwrap();
        assert_eq!(mc.width, 400.0);
        assert_eq!(mc.height, 90.0, "容器高=最高幻影列（3×30）");
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "第 1 列");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 190.0, "列宽 =（400−20）/2");
        }
        for (i, k) in (5..=7).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 210.0, "第 2 列 = 190+gap20");
            assert_eq!(bx.y, (i as f32) * 30.0);
        }
    }

    #[test]
    fn multicol_width_mode_four_columns() {
        // 二期③：column-width 模式 n=⌊(cw+gap)/(w+gap)⌋=4、colw=100，
        // 8 项理想高贪心 → 每列 2 项、列距 0。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 100px; column-gap: 0px; width: 400px } \
             it { height: 25px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=9 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (col, k) in [
            (0, 2u32),
            (0, 3),
            (1, 4),
            (1, 5),
            (2, 6),
            (2, 7),
            (3, 8),
            (3, 9),
        ] {
            let bx = b(Key(k));
            assert_eq!(bx.x, (col as f32) * 100.0, "第 {col} 列");
            assert_eq!(bx.width, 100.0);
        }
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 25.0);
        assert_eq!(b(Key(8)).y, 0.0);
        assert_eq!(b(Key(9)).y, 25.0);
    }

    #[test]
    fn multicol_revert_single_column() {
        // 二期③：column-width 过宽 → n=1 回退单列块流（首帧即还原，
        // 不残留 Flex 行布局；声明宽保留）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-width: 1000px; width: 400px } \
             it { height: 30px; width: 200px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=4 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        for (i, k) in (2..=4).enumerate() {
            let bx = b(Key(k));
            assert_eq!(bx.x, 0.0, "回退块流：纵向堆叠");
            assert_eq!(bx.y, (i as f32) * 30.0);
            assert_eq!(bx.width, 200.0, "声明宽保留");
        }
        assert_eq!(frame.find(Key(1)).unwrap().height, 90.0);
    }

    #[test]
    fn multicol_balance_uneven() {
        // 二期③：理想高贪心（ideal=total/n=90）：高 80/10/10/80 →
        // [80,10] | [10,80]，不可断子件整件入列、尾列 min 守护。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h2"), (4, "h2"), (5, "h1")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "第 1 列 [80,10]");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "第 2 列 [10,80]");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 10.0);
        assert_eq!(b(Key(2)).width, 200.0, "列宽 = 400/2");
    }

    #[test]
    fn multicol_bisection_balances_uneven() {
        // 三期⑤a：二分平衡（试高顺序装箱，fit 单调 → 浮点二分最小可行
        // 列高）。[80,80,80,10] 双列：贪心 ideal=125 给 [80|80,80,10]
        // max170；二分得 [80,80|80,10] max160（Chromium 平衡语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             h1 { height: 80px } h2 { height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for (k, name) in [(2u32, "h1"), (3, "h1"), (4, "h1"), (5, "h2")] {
            assert!(engine.insert(Some(Key(1)), Key(k), mk(name)).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80,80]、列 2 [80,10]；容器高 160（贪心 170）。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0, "列 1 两件 80");
        assert_eq!(b(Key(4)).x, 200.0);
        assert_eq!(b(Key(4)).y, 0.0, "列 2 首件 80");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 80.0, "列 2 尾件 10");
        assert_eq!(b(Key(1)).height, 160.0, "二分平衡列高（贪心 170）");
    }

    #[test]
    fn multicol_break_margin_top_truncated() {
        // 三期⑤a：断口 margin-top 截断（css-multicol §7）——列首件
        // margin-top 不生效。b2(mt20,h30) 平衡进列 2 顶（二分含截断建模：
        // 列 1=[80] 列 2=[30,80] max110 < 无截断建模的 130）；列中件
        // margin 保留（b1 下方若有 margin 仍占位）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } \
             b2 { height: 30px; margin-top: 20px } \
             b3 { height: 80px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("b2")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b3")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 列 1 [80]；列 2 [b2(截断 mt→y0), b3(y30)]；容器高 110。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0, "列首 margin-top 截断（不截则 y=20）");
        assert_eq!(b(Key(4)).y, 30.0);
        assert_eq!(b(Key(1)).height, 110.0);
    }

    #[test]
    fn multicol_span_all_splits_flow() {
        // 三期⑤b：column-span:all 切断列流——a/b 平衡进 spanner 前段行
        // （双列并排），spanner 全宽横贯，c/d 收进后段行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             blk { height: 100px } sp { column-span: all; height: 50px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("blk")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("blk")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行：a(0,0,200,100) b(200,0,200,100)；spanner 全宽 y=100；
        // 后段行：c(0,150,200,100) d(200,150,200,100)；容器高 250。
        assert_eq!(b(Key(2)).x, 0.0);
        assert_eq!(b(Key(3)).x, 200.0);
        assert_eq!(b(Key(3)).y, 0.0);
        assert_eq!(b(Key(4)).x, 0.0, "spanner 全宽横贯");
        assert_eq!(b(Key(4)).y, 100.0, "spanner 前段行（高 100）之下");
        assert_eq!(b(Key(4)).width, 400.0);
        assert_eq!(b(Key(4)).height, 50.0);
        assert_eq!(b(Key(5)).x, 0.0);
        assert_eq!(b(Key(5)).y, 150.0, "spanner 之下新段行");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 150.0);
        assert_eq!(b(Key(1)).height, 250.0);
    }

    #[test]
    fn multicol_span_all_two_segments_balance() {
        // 三期⑤b：spanner 分段各段独立二分平衡——a(80) | sp(10) |
        // b(80) c(80) d(10)：前段行高 80；后段 3 件双列二分
        // [80,80|80,10] 行高 90（贪心 170）；容器高 80+10+90=180。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b80 { height: 80px } b10 { height: 10px } \
             sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(6), mk("b10")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        // 前段行高 80；spanner y=80 h=10；后段行二分 [80,80|80,10] 高 90
        //（c y=90 col1，d y=90 col2，e y=170 col2）；容器高 180。
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(2)).x, 0.0, "单件段占列 1");
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(3)).height, 10.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 90.0, "后段行列 1 首件");
        assert_eq!(b(Key(5)).x, 200.0);
        assert_eq!(b(Key(5)).y, 90.0, "后段行列 2 首件");
        assert_eq!(b(Key(6)).x, 200.0);
        assert_eq!(b(Key(6)).y, 170.0, "后段行列 2 尾件");
        assert_eq!(b(Key(1)).height, 180.0, "段内二分行高 90（贪心 170）");
    }

    #[test]
    fn multicol_span_segment_start_margin_kept() {
        // 三期⑤b：spanner 边界 = 强制断行——后段首件 margin-top 保留
        // （css-break-3 强断边距保留；Chromium 153 实证 b2 y=360 含 mt20）。
        // 段内列首仍截断（⑤a 语义），spanner 后新列除外。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px } \
             b1 { height: 80px } sp { column-span: all; height: 50px } \
             b2 { height: 80px; margin-top: 20px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b1")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b2")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k: Key| frame.find(k).unwrap();
        assert_eq!(b(Key(2)).y, 0.0);
        assert_eq!(b(Key(3)).y, 80.0);
        assert_eq!(b(Key(4)).x, 0.0);
        assert_eq!(b(Key(4)).y, 150.0, "段首 margin-top 保留（截断则 130）");
        assert_eq!(b(Key(1)).height, 230.0);
    }

    #[test]
    fn multicol_rule_row_mode_gap_centered() {
        // 三期⑤c：行模式列规——条带落在列间 gap 正中（center = 前列右缘 +
        // 半 gap），y/高 = 幻影列盒（容器内容高）；宽 4px 居中 ⇒ x = 198。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 20px; width: 400px; \
              column-rule: 4px solid red } \
             it { height: 30px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        for k in 2..=7 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
        }
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("行模式列规存在");
        assert_eq!(segs.len(), 1, "相邻列对数 = n−1");
        let s = &segs[0];
        // 列位 0..190 | 210..400：center = 190+10 = 200；x = 200−2。
        assert_eq!(s.x, 198.0);
        assert_eq!(s.y, 0.0);
        assert_eq!(s.width, 4.0);
        assert_eq!(s.height, 90.0, "条带高 = 列盒高（3×30）");
        let c = s.color.components;
        assert!(c[0] > 0.9 && c[3] == 1.0, "red currentcolor 终结（{c:?}）");
    }

    #[test]
    fn multicol_rule_span_mode_per_segment() {
        // 三期⑤c：span 模式列规逐段绘制——前段行（y 0..80）与后段行
        // （y 90..170）各一条；spanner 横贯处不画（列间断点消失）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let report = engine.set_stylesheet(
            "mc { column-count: 2; column-gap: 0px; width: 400px; \
              column-rule: 2px solid blue } \
             b80 { height: 80px } sp { column-span: all; height: 10px }",
        );
        assert!(report.is_clean(), "{report:?}");
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("b80")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), mk("sp")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), mk("b80")).is_ok());
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let mc_id = engine.key_to_node[&Key(1)];
        let segs = engine.column_rules.get(&mc_id).expect("span 模式逐段列规");
        assert_eq!(segs.len(), 2, "每段一行一条");
        let mut ys: Vec<f32> = segs.iter().map(|s| s.y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(ys, vec![0.0, 90.0], "前段行 y=0；后段行 y=90（80+10）");
        for s in segs {
            assert_eq!(s.x, 199.0, "gap0 居中：x = 200−1");
            assert_eq!(s.width, 2.0);
            assert_eq!(s.height, 80.0);
        }
    }

    #[test]
    fn multicol_rule_style_none_or_zero_width_no_paint() {
        // 三期⑤c：style:none / 宽 0 / 缺省（style 初始 none）→ 无条带。
        let mk = |name: &str| StyleNode {
            name: Some(name.to_string()),
            ..Default::default()
        };
        let run = |sheet: &str| {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine.set_stylesheet(sheet).is_clean());
            assert!(engine.insert(None, Key(1), mk("mc")).is_ok());
            for k in 2..=5 {
                assert!(engine.insert(Some(Key(1)), Key(k), mk("it")).is_ok());
            }
            let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
            let mc_id = engine.key_to_node[&Key(1)];
            !engine.column_rules.contains_key(&mc_id)
        };
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: none } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule: 0 solid red } \
             it { height: 30px }"
        ));
        assert!(run(
            "mc { column-count: 2; width: 400px; column-rule-color: red } \
             it { height: 30px }",
        ));
    }

    #[test]
    fn z_index_auto_not_materialized_as_number() {
        // z-index: auto → ZIndex(None)（不再物化为 Number(0)）；
        // 数字 → ZIndex(Some(n))，paint effective_z 仅认 Some。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { position: relative; z-index: auto; } div.n { position: relative; z-index: 5; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("n")).is_ok());
        let _ = engine.frame((400.0, 100.0), 1.0, 0.0);
        let ida = *engine.key_to_node.get(&Key(1)).unwrap();
        let idn = *engine.key_to_node.get(&Key(2)).unwrap();
        assert_eq!(
            engine
                .styles
                .get(&ida)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(None))
        );
        assert_eq!(
            engine
                .styles
                .get(&idn)
                .and_then(|cs| cs.get(crate::css::property::PropertyId::ZIndex)),
            Some(&crate::css::property::DeclValue::ZIndex(Some(5.0)))
        );
    }

    #[test]
    fn insert_rejects_invalid_span_ranges() {
        // span 区间契约：UTF-8 边界内、有序、不越界；无文本节点的 span 非法。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let span = |start: u32, end: u32| crate::tree::TextSpan {
            range: (start, end),
            declarations: crate::css::decl::parse_inline_declarations("color: #ff0000").0,
        };
        let mk = |spans: std::iter::Once<crate::tree::TextSpan>| StyleNode {
            name: Some("div".into()),
            text: Some("héllo".into()),
            spans: spans.collect(),
            ..Default::default()
        };
        // h(0) é(1..3) l(3) l(4) o(5) → len 6，边界 {0,1,3,4,5,6}
        assert!(
            engine
                .insert(None, Key(1), mk(std::iter::once(span(1, 3))))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), mk(std::iter::once(span(2, 4))))
                .is_err()
        ); // 2 非字符边界
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), mk(std::iter::once(span(0, 10))))
                .is_err()
        ); // 越界
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), mk(std::iter::once(span(4, 2))))
                .is_err()
        ); // 倒置
        let textless = StyleNode {
            name: Some("div".into()),
            spans: std::iter::once(span(0, 1)).collect(),
            ..Default::default()
        };
        assert!(engine.insert(Some(Key(1)), Key(5), textless).is_err()); // 无文本
    }

    #[test]
    fn scroll_bounds_reported_and_hidden_skipped() {
        // ADR-0007：overflow∈{auto,scroll} 上报量程（padding box + 后代并集）；
        // hidden 仅裁剪、不上报。content-box 默认：padbox = 220×120（含 padding），
        // 内容底 310 → 量程 310 − 120 = 190。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.list { width: 200px; height: 100px; overflow-y: scroll; padding: 10px; } div.hid { width: 200px; height: 100px; overflow-y: hidden; padding: 10px; } div.item { width: 200px; height: 150px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), node("item")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), node("item")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), node("hid")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(6), node("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(2)), Some(&(0.0, 190.0)));
        assert!(!frame.scrollable.contains_key(&Key(5)));
    }

    #[test]
    fn scroll_offset_translates_paint() {
        // 宿主偏移 → PushScroll/PopScroll 平移层（不改布局盒）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.list { width: 200px; height: 100px; overflow-y: scroll; }")
                .is_clean()
        );
        let node = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("list".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node()).is_ok());
        assert!(engine.set_scroll_offset(Key(1), 0.0, 40.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.paint.ops.iter().any(|op| matches!(
            op,
            crate::paint::PaintOp::PushScroll { dx, dy } if *dx == 0.0 && *dy == 40.0
        )));
        assert!(
            frame
                .paint
                .ops
                .iter()
                .any(|op| matches!(op, crate::paint::PaintOp::PopScroll))
        );
    }

    #[test]
    fn scroll_range_includes_transformed_child() {
        // 三期⑥：transform ≠ none 的后代以变换后 AABB 贡献正向溢出。
        // 容器 200×100，子件 200×50 translate(0,100px) → AABB y 100..150
        // → 量程 max_y = 50（原盒 0..50 不够、平移后盒才到 150）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.item { width: 200px; height: 50px; transform: translate(0, 100px); }"
                )
                .is_clean()
        );
        let list = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("list".to_string()).collect(),
            ..Default::default()
        };
        let item = || StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("item".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), list()).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), item()).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(1)), Some(&(0.0, 50.0)));
    }

    #[test]
    fn scroll_range_nested_transform_composes() {
        // 三期⑥：祖先链复合（包裹层 translate(0,40) × 内层 translate(0,40)
        // → 内层有效 AABB y 80..180 → max_y 80；只算 own 会得 40）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.wrap { transform: translate(0, 40px); } \
                     div.inner { width: 200px; height: 100px; transform: translate(0, 40px); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("wrap")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("inner")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.scrollable.get(&Key(1)), Some(&(0.0, 80.0)));
    }

    #[test]
    fn scroll_range_rotate_aabb_positive_side_only() {
        // 三期⑥：rotate(45deg) 100×100 于 100×100 容器 → 变换后 AABB
        // 半延 = 50·(|cos45|+|sin45|) ≈ 70.71，正侧越界 ≈ 20.71；负侧
        // （左/上 -20.71）不扩 LTR 量程。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 100px; height: 100px; overflow: scroll; } \
                     div.item { width: 100px; height: 100px; transform: rotate(45deg); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let (mx, my) = frame.scrollable.get(&Key(1)).copied().unwrap();
        let want = 50.0_f32 * (45f32.to_radians().sin() + 45f32.to_radians().cos()) - 50.0;
        assert!((mx - want).abs() < 1e-3, "max_x {mx} want {want}");
        assert!((my - want).abs() < 1e-3, "max_y {my} want {want}");
    }

    #[test]
    fn scroll_range_transform_up_extends_nothing() {
        // 三期⑥：负向平移（AABB 越过左/上边界）不扩 LTR 正向量程；
        // 全部越出后仅 padding box 自身贡献 → 不可滚。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.list { width: 200px; height: 100px; overflow-y: scroll; } \
                     div.item { width: 200px; height: 50px; transform: translate(0, -100px); }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("list")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("item")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(!frame.scrollable.contains_key(&Key(1)));
    }

    #[test]
    fn var_shorthand_padding_reaches_layout() {
        // 阶段2②：var() 简写端到端——解析期挂起、计算值期代换+展开，
        // padding 驱动子件偏移。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.box { --p: 30px; padding: var(--p); width: 200px; height: 100px; } \
                     div.kid { width: 50px; height: 50px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("box")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("kid")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(2)).unwrap();
        assert_eq!((kid.x, kid.y), (30.0, 30.0));
    }

    #[test]
    fn grid_autofill_repeats_tracks_to_fit() {
        // 阶段2①：auto-fill 重复计数 = 可用空间容纳的最大次数（空轨保留）。
        // 400 宽 + repeat(auto-fill, 100px) → 4 轨；5 个 80×80 自动回绕第二行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.g { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fill, 100px); } \
                     div.i { width: 80px; height: 80px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), mk("g")).is_ok());
        for k in 2..=6u32 {
            assert!(engine.insert(Some(Key(1)), Key(k), mk("i")).is_ok());
        }
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = |k| frame.find(Key(k)).unwrap();
        assert_eq!((b(2).x, b(2).y), (0.0, 0.0));
        assert_eq!((b(3).x, b(3).y), (100.0, 0.0));
        assert_eq!((b(4).x, b(4).y), (200.0, 0.0));
        assert_eq!((b(5).x, b(5).y), (300.0, 0.0));
        assert_eq!((b(6).x, b(6).y), (0.0, 80.0), "第 5 项回绕第二行");
    }

    #[test]
    fn grid_autofit_collapses_empty_tracks_fr_expands() {
        // 阶段2①：auto-fit 空轨折叠后剩余轨（fr）重分自由空间。
        // 对照 400 宽 2 项 minmax(100px, 1fr)：auto-fill=4 轨各 100；
        // auto-fit=折叠 2 空轨 → 2 轨各 200。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.g { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)); } \
                     div.f { display: grid; width: 400px; \
                     grid-template-columns: repeat(auto-fit, minmax(100px, 1fr)); } \
                     div.i { height: 80px; }"
                )
                .is_clean()
        );
        let mk = |cls: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(cls.to_string()).collect(),
            ..Default::default()
        };
        // 单根约束：w 包裹两个对照容器（g=auto-fill / f=auto-fit）。
        assert!(engine.insert(None, Key(1), mk("w")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), mk("g")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(3), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(2)), Key(4), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(5), mk("f")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(6), mk("i")).is_ok());
        assert!(engine.insert(Some(Key(5)), Key(7), mk("i")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        // auto-fill：4 轨各 100，两项 x=0 / x=100。
        let b3 = frame.find(Key(3)).unwrap();
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!((b3.x, b3.width), (0.0, 100.0));
        assert_eq!((b4.x, b4.width), (100.0, 100.0));
        // auto-fit：2 空轨折叠，剩余 2 轨 fr 均分 → 各 200。
        let b6 = frame.find(Key(6)).unwrap();
        let b7 = frame.find(Key(7)).unwrap();
        assert_eq!((b6.x, b6.width), (0.0, 200.0));
        assert_eq!((b7.x, b7.width), (200.0, 200.0));
        assert_eq!((b6.y, b7.y), (80.0, 80.0), "auto-fit 容器在 g 容器下方");
    }

    #[test]
    fn class_attribute_is_space_separated_tokens() {
        // class 属性按空格拆 token："row alt" 同时命中 .row 与 .alt。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.row { width: 200px; height: 40px; } div.alt { background-color: #164e63; }"
                )
                .is_clean()
        );
        let node = StyleNode {
            name: Some("div".into()),
            classes: std::iter::once("row alt".to_string()).collect(),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(1)).unwrap();
        assert_eq!((b.x, b.y, b.width, b.height), (0.0, 0.0, 200.0, 40.0));
    }

    #[test]
    fn min_content_is_widest_atom() {
        // T5d：min-content = 0 宽强制断行后的最宽行（最宽不可断原子）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(
            engine
                .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("hello world foo".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id = *engine.key_to_node.get(&Key(1)).unwrap();
        let cs = engine.styles.get(&id).cloned().unwrap();
        let (min_w, _) = engine.min_measures.get(&id).copied().unwrap();
        let (max_w, _) = engine.measures.get(&id).copied().unwrap();
        assert!(max_w > min_w && min_w > 0.0);
        // 三个原子中最宽者（"world" 的 w 宽于 "hello"/"foo"，逐个对照）
        let mut widest = 0.0f32;
        for word in ["hello", "world", "foo"] {
            let (ww, _) = engine.text.measure(word, &cs, &MediaEnv::default());
            widest = widest.max(ww);
        }
        assert!((min_w - widest).abs() < 0.5);
    }

    #[test]
    fn absolute_text_shrinks_to_fit() {
        // T5d 第三 pass：absolute 文本叶 = clamp(min_content, cb 内容宽, max_content)。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.host { position: relative; width: 300px; height: 200px; } div.tip { position: absolute; top: 10px; left: 20px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, text: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if parent.is_some() {
                        std::iter::once("tip".to_string()).collect()
                    } else {
                        std::iter::once("host".to_string()).collect()
                    },
                    text: Some(text.into()),
                    ..Default::default()
                },
            )
        };
        // 长文本：被 300−0(insets) 夹紧 → 宽 < 无界宽
        assert!(mk(&mut engine, Key(1), None, "host").is_ok());
        assert!(
            mk(
                &mut engine,
                Key(2),
                Some(Key(1)),
                "the quick brown fox jumps over the lazy dog again and again"
            )
            .is_ok()
        );
        // 短文本：max-content ≤ 可用宽 → 保持固有宽（不拉伸）
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "short").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let id3 = *engine.key_to_node.get(&Key(3)).unwrap();
        let (w2, _) = engine.measures.get(&id2).copied().unwrap();
        let (w3, _) = engine.measures.get(&id3).copied().unwrap();
        assert!(w2 <= 300.0 && w2 > 0.0);
        // 短文本未拉伸到可用宽（300），仍是自身 max-content
        let short_max = {
            let cs = engine.styles.get(&id3).cloned().unwrap();
            let (mw, _) = engine.text.measure("short", &cs, &MediaEnv::default());
            mw
        };
        assert!((w3 - short_max).abs() < 0.5);
        // wrap_widths 同步（绘制折行一致）——第五批⑥后宿主声明宽（300）
        // 不再被文本测量覆写，absolute 夹紧宽 = cb 内容宽 300（折行约束），
        // 而 measures.0 = 该约束下的最宽行（283.97 类值）
        assert_eq!(engine.wrap_widths.get(&id2), Some(&Some(300.0)));
    }

    #[test]
    fn leaf_intrinsic_clamps_absolute() {
        // T5d：LeafMeasure 叶固有区间在 absolute 语境下夹紧宽度。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.host { position: relative; width: 300px; height: 200px; } div.wide-host { position: relative; width: 1000px; height: 200px; } div.chip { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "host").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "chip").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "wide-host").is_ok());
        assert!(mk(&mut engine, Key(5), Some(Key(4)), "chip").is_ok());
        for k in [Key(3), Key(5)] {
            assert!(engine.set_leaf_measure(k, 500.0, 20.0).is_ok());
            assert!(
                engine
                    .set_leaf_intrinsic(k, 80.0, 20.0, 500.0, 20.0)
                    .is_ok()
            );
        }
        let frame = engine.frame((1200.0, 600.0), 1.0, 0.0);
        // 300 宽宿主：500 被夹到 300；1000 宽宿主：保持 500
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.width, 300.0);
        let b5 = frame.find(Key(5)).unwrap();
        assert_eq!(b5.width, 500.0);
    }

    #[test]
    fn scroll_offset_set_is_idempotent() {
        // ADR-0007 宿主集成边界（滚轮夹紧在宿主）：同值重设不改变任何
        // 状态与输出——偏移表幂等、布局盒不动（滚动只是绘制期平移）、
        // 量程不变。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.list { position: relative; width: 240px; height: 80px; overflow-y: scroll; } div.row { height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("list")).is_ok());
        for k in [Key(3), Key(4), Key(5)] {
            assert!(engine.insert(Some(Key(2)), k, node("row")).is_ok());
        }
        let _ = engine.frame((400.0, 300.0), 1.0, 0.0);
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        let first = *engine.scroll_offsets.get(&id2).unwrap();
        assert!(engine.set_scroll_offset(Key(2), 0.0, 20.0).is_ok());
        assert_eq!(engine.scroll_offsets.get(&id2), Some(&first));
        let f1 = engine.frame((400.0, 300.0), 1.0, 0.0);
        let s1 = f1.scrollable.get(&Key(2)).copied();
        let b1 = f1.find(Key(3)).unwrap();
        let f2 = engine.frame((400.0, 300.0), 1.0, 0.0);
        assert_eq!(f2.scrollable.get(&Key(2)), s1.as_ref());
        let b2 = f2.find(Key(3)).unwrap();
        assert_eq!(
            (b1.x, b1.y, b1.width, b1.height),
            (b2.x, b2.y, b2.width, b2.height)
        );
    }

    #[test]
    fn cjk_min_content_is_single_char() {
        // T5d × CJK：CJK 的 min-content = 单字宽（UAX #14 表意文字逐字可断），
        // 与 Latin 的「最宽词」语义相对；max-content 仍为无界整行。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT_CJK.to_vec());
        assert!(
            engine
                .set_stylesheet(
                    "div { font-family: \"Noto Sans SC\"; font-size: 16px; white-space: normal; }"
                )
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("样式引擎渲染检查".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let min = engine
            .min_measures
            .get(&id1)
            .copied()
            .expect("CJK 文本叶应有 min-content");
        let cs = engine.styles.get(&id1).cloned().unwrap();
        let (single, _) = engine.text.measure("样", &cs, &MediaEnv::default());
        assert!(
            (min.0 - single).abs() < 0.6,
            "min={} 单字宽={}",
            min.0,
            single
        );
        let (max, _) = engine
            .text
            .measure("样式引擎渲染检查", &cs, &MediaEnv::default());
        assert!(max > single + 1.0);
    }

    #[test]
    fn absolute_without_positioned_ancestor_uses_initial_cb() {
        // CSS 10.3.7：无 positioned 祖先 → 包含块 = 初始包含块（视口）。
        // LeafMeasure 2000 宽叶在 800 视口被夹到 800（≥ min 80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.leaf { position: absolute; top: 0; left: 0; }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: if class.is_empty() {
                        Default::default()
                    } else {
                        std::iter::once(class.to_string()).collect()
                    },
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(2), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(2), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!(b2.width, 800.0);
    }

    #[test]
    fn line_height_resolves_into_layout() {
        // 盘点修复回归：line-height 此前已解析入库但无任何消费者（无效声明）。
        // 绝对行高单行 = 盒高（parley Absolute 行盒语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.host { width: 300px; } div.t { font-family: \"DejaVu Sans\"; font-size: 16px; line-height: 40px; }"
            )
            .is_clean());
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: text.map(String::from),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("host", None)).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("t", Some("Hello World")))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert!(
            (b2.height - 40.0).abs() < 0.6,
            "line-height: 40px 单行盒高应≈40，实际 {}",
            b2.height
        );
    }

    #[test]
    fn letter_spacing_widens_measures() {
        // 盘点修复回归：letter-spacing 此前完全无消费者（连访问器都没有）。
        // "Hello World Test" 16 字符 ≥2 字符间隙 → 字距 4px 至少拉开 2 间隙。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet("div { font-family: \"DejaVu Sans\"; font-size: 16px; } div.s { letter-spacing: 4px; }")
            .is_clean());
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: if classes.is_empty() {
                Default::default()
            } else {
                std::iter::once(classes.to_string()).collect()
            },
            text: Some("Hello World Test".into()),
            ..Default::default()
        };
        assert!(engine.insert(None, Key(1), node("")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("s")).is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let id1 = *engine.key_to_node.get(&Key(1)).unwrap();
        let id2 = *engine.key_to_node.get(&Key(2)).unwrap();
        let cs1 = engine.styles.get(&id1).cloned().unwrap();
        let cs2 = engine.styles.get(&id2).cloned().unwrap();
        let env = MediaEnv::default();
        let (w0, _) = engine.text.measure("Hello World Test", &cs1, &env);
        let (w4, _) = engine.text.measure("Hello World Test", &cs2, &env);
        assert!(
            w4 - w0 > 8.0,
            "letter-spacing: 4px 应显著加宽测量（Δ={}）",
            w4 - w0
        );
        // min-content 同步生效：最宽原子被拉开
        let (m0, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs1, &[], &env);
        let (m4, _) = engine
            .text
            .measure_min_content("Hello World Test", &cs2, &[], &env);
        assert!(m4 > m0 + 4.0, "字距应作用于 min-content（Δ={}）", m4 - m0);
    }

    #[test]
    fn transform_parse_2d_and_reject_3d() {
        // ADR-0009 v1：2D 函数列表解析；3D 函数解析拒绝（warn 路径 → 非 clean，
        // 声明丢弃 → 空表 = none）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        let rep = engine.set_stylesheet(
            "div.a { transform: translate(10px, 20%) rotate(45deg) scale(2) skew(10deg); } div.b { transform: matrix(1, 0, 0, 1, 5, 6); } div.d { transform: none; }",
        );
        assert!(rep.is_clean(), "warnings={:?}", rep.warnings);
        let mut engine3d: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine3d
                .set_stylesheet("div.c { transform: translate3d(1px, 2px, 3px); }")
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        use crate::css::property::TransformFn as TF;
        use crate::css::value::LengthPercentage as LP;
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        let a = cs(&engine, Key(1));
        assert_eq!(
            a.transform(),
            &[
                TF::Translate(LP::Px(10.0), LP::Percent(0.2)),
                TF::Rotate(45.0),
                TF::Scale(2.0, 2.0),
                TF::Skew(10.0, 0.0),
            ]
        );
        assert!(a.has_transform());
        let b = cs(&engine, Key(2));
        assert_eq!(b.transform(), &[TF::Matrix(1.0, 0.0, 0.0, 1.0, 5.0, 6.0)]);
        let d = cs(&engine, Key(4));
        assert!(d.transform().is_empty());
    }

    #[test]
    fn transform_ancestor_is_absolute_cb_for_avail() {
        // ADR-0009 L2 双时机：transform ≠ none 祖先 → absolute 后代包含块
        // （修正前 cb walk 跳过 static+transformed 的 mid → avail = outer 内容宽
        // 360 → 叶宽 360；修正后夹到 mid 内容宽 200）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { position: relative; width: 400px; padding: 20px; } div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } div.leaf { position: absolute; top: 0; left: 0; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        assert!(engine.set_leaf_measure(Key(3), 2000.0, 20.0).is_ok());
        assert!(
            engine
                .set_leaf_intrinsic(Key(3), 80.0, 20.0, 2000.0, 20.0)
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.width), (50.0, 200.0)); // 外 padding 20 + margin 30；identity 变换不改布局
        let b3 = frame.find(Key(3)).unwrap();
        // avail = mid 内容宽 200（transformed 祖先成为 cb），夹紧 80..2000
        assert_eq!(b3.width, 200.0);
        // taffy 锚定直父（mid）：x = mid.x，y = outer padding
        assert_eq!((b3.x, b3.y), (50.0, 20.0));
    }

    #[test]
    fn rem_follows_root_font_size() {
        // P0 rem 修复：rem = 文档根计算字号（修复前全链路恒 16px）。
        // :root font-size: 20px → 子 width: 2rem = 40px（旧 bug：32px）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    ":root { font-size: 20px; } \
                     div.outer { width: 400px; } \
                     div.leaf { width: 2rem; height: 10px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!(
            b2.width, 40.0,
            "2rem 应 = 2 × 根字号 20px = 40px（修复前恒 16 → 32px）"
        );
    }

    #[test]
    fn rem_on_root_font_size_uses_initial_value() {
        // CSS Values：根元素 font-size 内的 rem 按初始值 16 解析
        // （2rem = 32px，不递归引用自身）；根元素其余属性（margin-left:
        // 1rem）的 rem 用解析后的新根字号 32px（框 x = 32）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    ":root { font-size: 2rem; margin-left: 1rem; width: 100px; height: 20px; }"
                )
                .is_clean()
        );
        let mut engine = engine;
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let _frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let cs = engine.computed_style(Key(1)).unwrap();
        assert_eq!(
            cs.font_size_px(),
            32.0,
            "根 font-size: 2rem 应按初始值 16 解析为 32px"
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b1 = frame.find(Key(1)).unwrap();
        assert_eq!(
            b1.x, 32.0,
            "根 margin-left: 1rem 应用新根字号 32px（非初始 16）"
        );
    }

    #[test]
    fn absolute_anchor_jumps_over_static_wrapper() {
        // 三期②锚定跳走：cb（transformed mid）与 absolute 叶之间存在 static
        // 包装层时，taffy 直父锚定会把 inset 基准错挂在 wrap 上（旧：
        // 75+30=105）；跳走后 inset 相对 cb padding box（新：50+30=80）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 20.0)); // 外 padding 20 + margin 30
        // wrap 只给水平 margin：taffy 0.14 末子 margin-bottom 塌陷会把父级
        // 顶下 15px（CSS 2.1 §8.3.1 应落在父底缘之外），本测聚焦锚定跳走，
        // 不掺入该塌陷行为（已知偏差记录于 FEATURES.md）。
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(b3.x, 75.0); // wrap 流内位置 = mid 内容原点 + margin 25（旧锚定基准）
        let b4 = frame.find(Key(4)).unwrap();
        // cb = mid：x = mid border box 原点 50 + inset 30；y = 20 + inset 20
        assert_eq!(
            (b4.x, b4.y),
            (80.0, 40.0),
            "absolute 应跳过 static 包装层锚定到 cb"
        );
    }

    #[test]
    fn absolute_icb_anchor_skips_all_static_ancestors() {
        // 三期②ICB：无 positioned/transformed 祖先 → 包含块 = 初始包含块，
        // inset 相对视口原点（旧：直父 wrap 流内位置 50+10/25+10）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 40px; margin-left: 10px; } \
                 div.wrap { margin-top: 25px; } \
                 div.leaf { position: absolute; top: 10px; left: 10px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!(
            (b3.x, b3.y),
            (10.0, 10.0),
            "无 positioned/transformed 祖先应锚定 ICB（视口原点 + inset）"
        );
    }

    #[test]
    fn absolute_anchor_reparents_nested_absolute_cb() {
        // 三期②嵌套：absolute 节点自身成为后代 absolute 的 cb（abs1 为
        // positioned）。abs1 锚 ICB（50,40）；abs2 跳过 static wrap 锚定
        // abs1 → (55,45)（旧：wrap 流内 70,60 + inset = 75,65）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.outer { width: 300px; padding: 10px; } \
                 div.abs1 { position: absolute; top: 40px; left: 50px; width: 100px; height: 50px; } \
                 div.wrap { margin-left: 20px; margin-top: 20px; } \
                 div.abs2 { position: absolute; top: 5px; left: 5px; }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "abs1").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "abs2").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(Key(2)).unwrap();
        assert_eq!((b2.x, b2.y), (50.0, 40.0), "abs1 无 positioned 祖先 → ICB");
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!(
            (b4.x, b4.y),
            (55.0, 45.0),
            "abs2 应锚定 absolute 祖先 abs1（嵌套 cb 链）"
        );
    }

    #[test]
    fn absolute_anchor_restores_after_style_change() {
        // 三期②恢复路径：样式表替换抹去 transformed/positioned 祖先后，
        // cb 集合变化 → 重挂残留归位，叶改锚 ICB（80,40 → 30,20）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { position: relative; width: 400px; padding: 20px; } \
                 div.mid { transform: translate(0px, 0px); width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "outer").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "mid").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(2)), "wrap").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(3)), "leaf").is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4 = frame.find(Key(4)).unwrap();
        assert_eq!((b4.x, b4.y), (80.0, 40.0));
        // 替换样式表：去掉 transform 与 relative → cb 链瓦解 → ICB
        assert!(
            engine
                .set_stylesheet(
                    "div.outer { width: 400px; padding: 20px; } \
                 div.mid { width: 200px; margin-left: 30px; } \
                 div.wrap { margin-left: 25px; margin-top: 15px; } \
                 div.leaf { position: absolute; top: 20px; left: 30px; }"
                )
                .is_clean()
        );
        let frame2 = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b4b = frame2.find(Key(4)).unwrap();
        assert_eq!(
            (b4b.x, b4b.y),
            (30.0, 20.0),
            "cb 消失后应重锚 ICB（重挂残留归位）"
        );
    }

    #[test]
    fn transform_creates_stacking_context_order() {
        // ADR-0008/0009：非定位 transform ≠ none → SC，进 Pos 带键 0——
        // 树序在前的 B（transform）画在树序在后的 A 之上；无 transform 控制组相反。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |transformed: bool| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            let tb = if transformed {
                " transform: translate(0px, 0px);"
            } else {
                ""
            };
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{tb} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 无 transform：Flow 带树序 → 蓝(B) 先画
        let plain = build(false);
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        // transform ≠ none：B 进 Pos 带 → 后画（覆盖 A）
        let sc = build(true);
        let (blue, red) = (find(&sc, [0.0, 0.0, 1.0]), find(&sc, [1.0, 0.0, 0.0]));
        assert!(
            blue > red,
            "transform SC 应后画（blue={blue:?} red={red:?}）"
        );
    }

    #[test]
    fn filter_clip_path_sc_effect_parse() {
        // 第四批④：filter/clip-path 仅存在性语义位（Effect）——none 缺席、
        // 任意值存在（多函数列表含内）；不做滤镜/裁剪效果实现
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.a { filter: blur(2px); } div.b { clip-path: inset(50%); } div.c { filter: none; } div.d { filter: blur(1px) saturate(2); }"
            )
            .is_clean());
        let mk = |engine: &mut StyleEngine<Key>, key: Key, parent: Option<Key>, class: &str| {
            engine.insert(
                parent,
                key,
                StyleNode {
                    name: Some("div".into()),
                    classes: std::iter::once(class.to_string()).collect(),
                    ..Default::default()
                },
            )
        };
        assert!(mk(&mut engine, Key(1), None, "a").is_ok());
        assert!(mk(&mut engine, Key(2), Some(Key(1)), "b").is_ok());
        assert!(mk(&mut engine, Key(3), Some(Key(1)), "c").is_ok());
        assert!(mk(&mut engine, Key(4), Some(Key(1)), "d").is_ok());
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        let cs = |e: &StyleEngine<Key>, k: Key| {
            e.styles
                .get(e.key_to_node.get(&k).unwrap())
                .cloned()
                .unwrap()
        };
        assert!(cs(&engine, Key(1)).has_filter());
        assert!(!cs(&engine, Key(1)).has_clip_path());
        assert!(cs(&engine, Key(2)).has_clip_path());
        assert!(!cs(&engine, Key(3)).has_filter(), "none → 缺席");
        assert!(cs(&engine, Key(4)).has_filter(), "多函数列表 → 存在");
    }

    #[test]
    fn filter_clip_path_trigger_stacking_context_order() {
        // 第四批④：非定位 filter/clip-path ≠ none → SC（Pos 带键 0），后画
        // 覆盖树序控制组；效果触发组（filter/will-change/backdrop-filter）
        // 不产生效果 PaintOp。F3c（ADR-0025）：clip-path 升级为真实裁剪
        // 语义——inset(0) 恰产生 PushClip+PopClip 对（包住自身与子树）。
        // P1-2：isolation/mix-blend-mode 升级为真实混合层——各自产生
        // PushBlend/PopBlend 层对（isolation=Normal 模式，mix-blend=具体模式）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        let plain = build("");
        let (blue, red) = (find(&plain, [0.0, 0.0, 1.0]), find(&plain, [1.0, 0.0, 0.0]));
        assert!(blue < red, "控制组应树序绘制（blue={blue:?} red={red:?}）");
        for (label, trigger, extra) in [
            // P2（ADR-0031）：filter 本体化 → PushFilter/PopFilter 层对
            //（blur(0px) 非空链仍触发 SC）；
            ("filter", " filter: blur(0px);", 2usize),
            ("will-change", " will-change: transform;", 0),
            // backdrop-filter → BackdropFilter op（无配对 pop）
            ("backdrop-filter", " backdrop-filter: blur(2px);", 1),
        ] {
            let ops = build(trigger);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} SC 应后画（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), plain.len() + extra, "{label} PaintOp 增量");
        }
        // filter 层对位置：PushFilter 包住子树 fill（blue），PopFilter 收尾。
        let ops = build(" filter: blur(0px);");
        let blue = find(&ops, [0.0, 0.0, 1.0]).unwrap();
        let pf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::PushFilter { .. }))
            .expect("PushFilter 应发射");
        let popf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::PopFilter))
            .expect("PopFilter 应发射");
        assert!(
            pf < blue && blue < popf,
            "层对包住子树: pf={pf} blue={blue} popf={popf}"
        );
        // backdrop op 发射在节点内容之前（主画布即时作用）。
        let ops = build(" backdrop-filter: blur(2px);");
        let blue = find(&ops, [0.0, 0.0, 1.0]).unwrap();
        let bf = ops
            .iter()
            .position(|o| matches!(o, crate::paint::PaintOp::BackdropFilter { .. }))
            .expect("BackdropFilter 应发射");
        assert!(bf < blue, "backdrop op 先于内容: bf={bf} blue={blue}");
        // P1-2：isolation（Normal 隔离组）/mix-blend-mode（具体模式）层对。
        for (label, trigger, expected) in [
            (
                "isolation",
                " isolation: isolate;",
                crate::css::property::BlendMode::Normal,
            ),
            (
                "mix-blend-mode",
                " mix-blend-mode: multiply;",
                crate::css::property::BlendMode::Multiply,
            ),
        ] {
            let ops = build(trigger);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} SC 应后画（blue={blue:?} red={red:?}）");
            assert_eq!(ops.len(), plain.len() + 2, "{label} 应产生层对");
            assert_eq!(
                ops.iter()
                    .filter(|op| matches!(op, crate::paint::PaintOp::PushBlend { .. }))
                    .count(),
                1,
                "{label} 应恰一个 PushBlend"
            );
            assert!(
                ops.iter().any(|op| matches!(
                    op,
                    crate::paint::PaintOp::PushBlend { mode, .. } if *mode == expected
                )),
                "{label} PushBlend 模式应为 {expected:?}"
            );
        }
        // F3c：clip-path = SC 后画 + 恰一对 PushClip/PopClip（无其它效果 op）。
        let clip_ops = build(" clip-path: inset(0);");
        let (blue, red) = (
            find(&clip_ops, [0.0, 0.0, 1.0]),
            find(&clip_ops, [1.0, 0.0, 0.0]),
        );
        assert!(
            blue > red,
            "clip-path SC 应后画（blue={blue:?} red={red:?}）"
        );
        assert_eq!(
            clip_ops.len(),
            plain.len() + 2,
            "clip-path 应恰产生 PushClip+PopClip 对"
        );
        assert!(
            clip_ops
                .iter()
                .any(|op| matches!(op, crate::paint::PaintOp::PushClip { .. }))
        );
    }

    #[test]
    fn blend_layer_pair_wraps_subtree_and_opacity() {
        // P1-2：mix-blend-mode 升级为真实混合层——PushBlend{mode}/PopBlend
        // 包住自身与子树；与 opacity 同用时混合层必须最外
        //（合成序 = blend(背后画布, opacity(子树))，css-compositing-1）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 40px; height: 40px; background-color: #ff0000; } \
                 div.c { width: 20px; height: 20px; mix-blend-mode: multiply; opacity: 0.5; background-color: #0000ff; }",
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("c")).is_ok());
        let ops = engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec();
        let find = |tag: &str| {
            ops.iter()
                .position(|op| match op {
                    crate::paint::PaintOp::PushBlend { mode, .. }
                        if tag == "push_blend"
                            && *mode == crate::css::property::BlendMode::Multiply =>
                    {
                        true
                    }
                    crate::paint::PaintOp::PopBlend if tag == "pop_blend" => true,
                    crate::paint::PaintOp::PushOpacity { .. } if tag == "push_opacity" => true,
                    crate::paint::PaintOp::PopOpacity if tag == "pop_opacity" => true,
                    crate::paint::PaintOp::FillRect { color, .. } if tag == "fill" => {
                        color.components[2] == 1.0 && color.components[0] == 0.0
                    }
                    _ => false,
                })
                .unwrap_or_else(|| panic!("{tag} 未找到于 {ops:?}"))
        };
        let (b, po, f, oo, pb) = (
            find("push_blend"),
            find("push_opacity"),
            find("fill"),
            find("pop_opacity"),
            find("pop_blend"),
        );
        assert!(
            b < po && po < f && f < oo && oo < pb,
            "序应为 blend→opacity→fill→popO→popB（b={b} po={po} f={f} oo={oo} pb={pb}）"
        );
    }

    #[test]
    fn clip_path_computed_initial_and_non_inherited() {
        // F3c（ADR-0025）：初始值 none；clip-path 非继承（css-masking-1）——
        // 父形状不落入子节点
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet(
                    "div.p { width: 40px; height: 40px; clip-path: circle(10px); } \
                 div.c { width: 40px; height: 40px; }"
                )
                .is_clean()
        );
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("c")).is_ok());
        let _ = engine.frame((200.0, 200.0), 1.0, 0.0);
        let parent = engine.computed_style(Key(1)).expect("父快照");
        let child = engine.computed_style(Key(2)).expect("子快照");
        assert!(parent.has_clip_path(), "父触发");
        assert!(
            matches!(child.clip_path(), crate::css::property::ClipShape::None),
            "子节点非继承 → 缺省 none"
        );
        assert!(!child.has_clip_path());
    }

    #[test]
    fn sc_trigger_full_set_parse_semantics() {
        // 第五批㉒：SC 触发全集解析语义——will-change 按列表成员判定
        // （含触发属性才置位）、isolation/mix-blend-mode 按值判定；
        // 观测量 = 带序（触发 → 蓝(树序后)盖红；未触发 → 红盖蓝树序）
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let build = |trigger: &str| -> Vec<crate::paint::PaintOp> {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(engine
                .set_stylesheet(&format!(
                    "div.a {{ width: 40px; height: 40px; background-color: #ff0000; }} div.b {{ width: 40px; height: 40px; background-color: #0000ff;{trigger} }}"
                ))
                .is_clean());
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
            assert!(engine.insert(Some(Key(1)), Key(3), node("a")).is_ok());
            engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.to_vec()
        };
        let find = |ops: &[crate::paint::PaintOp], rgb: [f32; 3]| {
            ops.iter().position(|op| {
                matches!(op, crate::paint::PaintOp::FillRect { color, .. }
                    if color.components[0] == rgb[0]
                        && color.components[1] == rgb[1]
                        && color.components[2] == rgb[2])
            })
        };
        // 触发（b 应后画：blue > red）。will-change 仍不产生效果 PaintOp；
        // isolation/mix-blend-mode 自 P1-2 起产生 PushBlend/PopBlend 层对
        for (css, label, extra_ops) in [
            (" will-change: transform;", "will-change 触发属性", 0),
            (" will-change: transform, color;", "will-change 混合列表", 0),
            (" will-change: color, opacity;", "will-change 尾部触发", 0),
            (" isolation: isolate;", "isolation", 2),
            (" mix-blend-mode: multiply;", "mix-blend-mode", 2),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(blue > red, "{label} 应触发 SC（blue={blue:?} red={red:?}）");
            assert_eq!(
                ops.len(),
                build("").len() + extra_ops,
                "{label} 额外 PaintOp 数不符"
            );
        }
        // 不触发（树序：blue < red）
        for (css, label) in [
            (" will-change: color;", "will-change color"),
            (" will-change: auto;", "will-change auto"),
            (" isolation: auto;", "isolation auto"),
            (" mix-blend-mode: normal;", "mix-blend-mode normal"),
        ] {
            let ops = build(css);
            let (blue, red) = (find(&ops, [0.0, 0.0, 1.0]), find(&ops, [1.0, 0.0, 0.0]));
            assert!(
                blue < red,
                "{label} 不应触发 SC（blue={blue:?} red={red:?}）"
            );
        }
    }

    #[test]
    fn transform_origin_parse_and_affine() {
        // 第五批⑬：transform-origin 解析（关键字/LP/单值缺省 center/关键字
        // 轴归类顺序宽容）+ paint 期 origin 环绕消费（A = T(o)·M·T(−o)）。
        // rotate(90deg) 盒 100×50：R = [0,1,−1,0,0,0]；
        // e = ox − (a·ox + c·oy)、f = oy − (b·ox + d·oy)。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let affine =
            |css: &str| -> [f32; 6] {
                let mut engine: StyleEngine<Key> = StyleEngine::new();
                assert!(engine
                .set_stylesheet(&format!(
                    "div.b {{ width: 100px; height: 50px; transform: rotate(90deg); {css} }}"
                ))
                .is_clean());
                assert!(engine.insert(None, Key(1), node("")).is_ok());
                assert!(engine.insert(Some(Key(1)), Key(2), node("b")).is_ok());
                for op in engine.frame((800.0, 600.0), 1.0, 0.0).paint.ops.iter() {
                    if let crate::paint::PaintOp::PushTransform { affine } = op {
                        return *affine;
                    }
                }
                panic!("PushTransform 缺失");
            };
        let near = |got: [f32; 6], want: [f32; 6]| {
            for (g, w) in got.iter().zip(want.iter()) {
                assert!((g - w).abs() < 1e-4, "got {got:?} want {want:?}");
            }
        };
        // 默认 = 50% 50%（o=(50,25)）：e = 50 − (0·50 + (−1)·25) = 75、
        // f = 25 − (1·50 + 0·25) = −25
        near(affine(""), [0.0, 1.0, -1.0, 0.0, 75.0, -25.0]);
        near(
            affine(" transform-origin: center;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        near(
            affine(" transform-origin: 50% 50%;"),
            [0.0, 1.0, -1.0, 0.0, 75.0, -25.0],
        );
        // 0 0 → 纯旋转
        near(
            affine(" transform-origin: 0 0;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        near(
            affine(" transform-origin: left top;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 顺序宽容：top left ≡ left top
        near(
            affine(" transform-origin: top left;"),
            [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
        );
        // 单值 top → (50%, 0%)：e = 50 − (0·50 + (−1)·0) = 50、f = 0 − (1·50) = −50
        near(
            affine(" transform-origin: top;"),
            [0.0, 1.0, -1.0, 0.0, 50.0, -50.0],
        );
        // 10px 20px：e = 10 − (0·10 + (−1)·20) = 30、f = 20 − (1·10 + 0·20) = 10
        near(
            affine(" transform-origin: 10px 20px;"),
            [0.0, 1.0, -1.0, 0.0, 30.0, 10.0],
        );
        // 无效值：声明丢弃（warn → 非 clean）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            !engine
                .set_stylesheet("div.b { transform-origin: red; }")
                .is_clean()
        );
    }

    #[test]
    fn text_align_reaches_paint_op() {
        // 第五批⑳：text-align 实际消费——测量不变宽（盒宽/折行与对齐无关），
        // PaintOp::Text 携带声明值供 sink 折行后 align；默认 Start。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let align_of = |css: &str| -> crate::css::property::TextAlign {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            assert!(
                engine
                    .set_stylesheet(&format!("div.b {{ width: 120px;{css} }}"))
                    .is_clean()
            );
            assert!(engine.insert(None, Key(1), node("")).is_ok());
            let mut leaf = node("b");
            leaf.text = Some("wrap me".into());
            assert!(engine.insert(Some(Key(1)), Key(2), leaf).is_ok());
            engine
                .frame((800.0, 600.0), 1.0, 0.0)
                .paint
                .ops
                .iter()
                .find_map(|op| match op {
                    crate::paint::PaintOp::Text { text_align, .. } => Some(*text_align),
                    _ => None,
                })
                .expect("text op 缺失")
        };
        assert!(matches!(
            align_of(""),
            crate::css::property::TextAlign::Start
        ));
        assert!(matches!(
            align_of(" text-align: center;"),
            crate::css::property::TextAlign::Center
        ));
        assert!(matches!(
            align_of(" text-align: right;"),
            crate::css::property::TextAlign::Right
        ));
        assert!(matches!(
            align_of(" text-align: justify;"),
            crate::css::property::TextAlign::Justify
        ));
    }

    #[test]
    fn text_leaf_block_stretch_and_declared_width() {
        // 第五批⑥文本叶语义契约：无声明宽度的文本叶在块流中拉伸到容器
        // 内容宽（与浏览器匿名块盒一致；旧实现=测量自然宽）；声明宽度
        // 优先于测量；高度=测量内容高（有字形非零）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 220px; font-family: \"DejaVu Sans\"; font-size: 16px; } div.fix { width: 120px; font-family: \"DejaVu Sans\"; font-size: 16px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        let mut stretch = node("auto");
        stretch.text = Some("shrink wrap candidate".into());
        assert!(engine.insert(Some(Key(1)), Key(2), stretch).is_ok());
        let mut fixed = node("fix");
        fixed.text = Some("shrink wrap candidate".into());
        // F1（ADR-0021）：声明宽叶独占容器（wrapper 隔离）——单叶不构成
        // 运行（判据=≥2 参与者或含盒）→ 第五批⑥块流拉伸/声明宽契约保全。
        assert!(engine.insert(Some(Key(1)), Key(4), node("wrap")).is_ok());
        assert!(engine.insert(Some(Key(4)), Key(3), fixed).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let st = frame.find(Key(2)).unwrap();
        // 自然宽 < 220：叶盒应 = 220（拉伸），而非测量自然宽
        assert!((st.width - 220.0).abs() < 0.5, "leaf width {}", st.width);
        let fx = frame.find(Key(3)).unwrap();
        // 声明宽度优先：120 而非测量值
        assert!((fx.width - 120.0).abs() < 0.5, "fixed width {}", fx.width);
        assert!(st.height > 0.0, "内容高应非零");
    }

    #[test]
    fn margin_collapse_block_siblings() {
        // 第五批⑦评估：taffy 0.14 block 算法内置纵向 margin collapsing
        // （CollapsibleMarginSet/ strut）——相邻兄弟纵 margin 取 max
        // （bottom 20 vs top 30 → 间距 30，非相加 50）；flex 子项不折叠
        // （CSS 语义，flex 上下文无折叠）。
        let node = |classes: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine
            .set_stylesheet(
                "div.p { width: 200px; } div.a { height: 40px; margin-bottom: 20px; background-color: #ff0000; } div.b { height: 40px; margin-top: 30px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let gap = b.y - (a.y + a.height);
        assert!((gap - 30.0).abs() < 0.5, "折叠间距应为 30，实测 {gap}");
        // flex 上下文不折叠（CSS：flex 子项 margin 永不相邻，间距相加）
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        assert!(engine2
            .set_stylesheet(
                "div.p { display: flex; flex-direction: column; width: 200px; } div.a { height: 40px; margin-bottom: 20px; } div.b { height: 40px; margin-top: 30px; }"
            )
            .is_clean());
        assert!(engine2.insert(None, Key(1), node("p")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(2), node("a")).is_ok());
        assert!(engine2.insert(Some(Key(1)), Key(3), node("b")).is_ok());
        let frame2 = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let fa = frame2.find(Key(2)).unwrap();
        let fb = frame2.find(Key(3)).unwrap();
        let gap2 = fb.y - (fa.y + fa.height);
        assert!((gap2 - 50.0).abs() < 0.5, "flex 间距应相加 50，实测 {gap2}");
    }

    #[test]
    fn display_inline_line_participation_f1() {
        // F1（ADR-0021）取代第五批⑧契约：display:inline/inline-block 参与
        // IFC 行打包（同行并排、收缩适配），不再纵向块化堆叠；解析仍接受
        //（is_clean 保持 true，归一化告警仅余 inline-flex/grid/table）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; background-color: #ff0000; } div.b { display: inline-block; height: 40px; font-family: \"DejaVu Sans\"; font-size: 16px; background-color: #0000ff; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        // F1 行盒并排：双叶参与者同行（y 相等），b.x = a 宽（advance 接排）。
        assert_eq!(a.y, 0.0);
        assert_eq!(b.y, 0.0, "F1：同行并排（取代第五批⑧块化堆叠契约）");
        assert!((b.x - a.width).abs() < 0.5, "b.x=a 宽（接排）");
        assert!(a.width > 0.0 && b.width > 0.0);
    }

    #[test]
    fn vertical_align_no_decl_bitwise_v1() {
        // P3（ADR-0034 D2 回归锁）：无 vertical-align 声明与显式 baseline
        // 的行结算输出逐位一致（baseline 不产生偏移——v1 TOP 装箱保护）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let run = |sheet_css: &str| {
            let mut engine: StyleEngine<Key> = StyleEngine::new();
            engine.add_font(TEST_FONT.to_vec());
            assert!(engine.set_stylesheet(sheet_css).is_clean());
            assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
            assert!(
                engine
                    .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                    .is_ok()
            );
            assert!(
                engine
                    .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                    .is_ok()
            );
            engine.frame((800.0, 600.0), 1.0, 0.0)
        };
        let f1 = run(
            "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; }",
        );
        let f2 = run(
            "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: baseline; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: baseline; }",
        );
        for k in [Key(1), Key(2), Key(3)] {
            let r1 = f1.find(k).unwrap();
            let r2 = f2.find(k).unwrap();
            assert_eq!(r1.x, r2.x, "k={k:?} x 逐位一致");
            assert_eq!(r1.y, r2.y, "k={k:?} y 逐位一致");
            assert_eq!(r1.width, r2.width);
            assert_eq!(r1.height, r2.height);
        }
    }

    #[test]
    fn vertical_align_length_offset() {
        // P3（ADR-0034 D2）：va:<length> 上浮负偏移——b（40px 无文本盒，
        // va:10px）dy = L−10−pb = 40−10−40 = −10 → b.y=−10；a（baseline）
        // 不动。行基线 L=max(pb)=40（b 无文本=盒高）。
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: text.map(|t| t.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 40px; vertical-align: 10px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", Some(""))).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", Some("aaa")))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(1)), Key(3), node("b", None)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        assert_eq!(a.y, 0.0, "baseline 叶不动");
        assert!(
            (b.y - (-10.0)).abs() < 0.5,
            "va:10px 盒 dy=L−10−pb=−10（b.y={}）",
            b.y
        );
    }

    #[test]
    fn vertical_align_super_sub_shifts() {
        // P3（ADR-0034 D2）：sub/super=±0.34em 常量（0.34×16=5.44px）。
        // 三 inline 叶同行：b(super) 上浮 −5.44、c(sub) 下沉 +5.44；
        // 行盒扩展 final_h = max(19, 5.44+19) = 24.44。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 400px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: super; } div.c { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: sub; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("c", "ccc"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert_eq!(a.y, 0.0, "baseline 叶不动");
        assert!(
            (b.y - (-5.44)).abs() < 0.6,
            "super 上浮 5.44px（b.y={}）",
            b.y
        );
        assert!((c.y - 5.44).abs() < 0.6, "sub 下沉 5.44px（c.y={}）", c.y);
        // 容器高度保持段按扩展后行高（> 单行 19）
        let p = frame.find(Key(1)).unwrap();
        assert!(p.height >= 24.0, "行盒扩展进容器高（p.h={}）", p.height);
    }

    #[test]
    fn vertical_align_middle_between_super_and_baseline() {
        // P3（ADR-0034 D2）：middle = x-height/2 对齐——介于 super 与
        // baseline 之间（dy_super = −5.44 < dy_middle = −0.5·xh·fs − h/2
        // + L − pb … 方向性锁定）。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 400px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: super; } div.c { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; vertical-align: middle; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(4), node("c", "ccc"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert!(b.y < a.y, "super 高于 baseline（{} < {}）", b.y, a.y);
        assert!(b.y < c.y, "super 高于 middle（{} < {}）", b.y, c.y);
    }

    #[test]
    fn vertical_align_box_baseline_from_text() {
        // P3（ADR-0034 D2）：Box 参与者基线=子树首文本叶基线（非盒高）。
        // b（inline-block h:60 有文本 16px）：pb≈15=L → a/b 同 y=0。
        // 若误用盒高（pb=60）则 L=60、a.y=45——本断言区分两种实现。
        let node = |classes: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: Some(text.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 60px; font-family: \"DejaVu Sans\"; font-size: 16px; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", "")).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", "aaa"))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), node("b", "bbb"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        assert_eq!(a.y, 0.0, "a 基线=L（盒基线取文本）");
        assert_eq!(b.y, 0.0, "b 基线=子文本基线（非盒高）");
    }

    #[test]
    fn vertical_align_top_bottom_alignment() {
        // P3（ADR-0034 D2）：top=行顶（TOP 装箱即位 dy=0）；bottom=行底
        //（扩展后二遍 dy=final_h−h）。b(h60,top)、c(h40,bottom)、
        // a(baseline 叶)：final_h=max(19,60,40)=60 → c.y=20。
        let node = |classes: &str, text: Option<&str>| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(classes.to_string()).collect(),
            text: text.map(|t| t.to_string()),
            ..Default::default()
        };
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "div.p { width: 300px; } div.a { display: inline; font-family: \"DejaVu Sans\"; font-size: 16px; } div.b { display: inline-block; height: 60px; vertical-align: top; } div.c { display: inline-block; height: 40px; vertical-align: bottom; }"
            )
            .is_clean());
        assert!(engine.insert(None, Key(1), node("p", Some(""))).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), node("a", Some("aaa")))
                .is_ok()
        );
        assert!(engine.insert(Some(Key(1)), Key(3), node("b", None)).is_ok());
        assert!(engine.insert(Some(Key(1)), Key(4), node("c", None)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let a = frame.find(Key(2)).unwrap();
        let b = frame.find(Key(3)).unwrap();
        let c = frame.find(Key(4)).unwrap();
        assert_eq!(a.y, 0.0);
        assert_eq!(b.y, 0.0, "top=行顶（装箱即位）");
        assert!(
            (c.y - 20.0).abs() < 0.5,
            "bottom=行底 60−40=20（c.y={}）",
            c.y
        );
    }

    #[test]
    fn settle_pass_schedule_topological_order() {
        // F2（ADR-0022 D1）：调度序=依赖全在前（Tables/Columns→Calc、
        // Floats→Columns、Lines→Floats）。
        let order = SettlePassKind::schedule();
        assert_eq!(order.len(), 5);
        for (i, p) in order.iter().enumerate() {
            for d in p.deps() {
                let di = order.iter().position(|x| x == d).unwrap();
                assert!(di < i, "依赖 {d:?} 须先于 {p:?}");
            }
        }
    }

    #[test]
    fn settle_should_run_gates_empty_inputs() {
        // F2（ADR-0022 D1）：空输入 pass 被运行门跳过（廉价谓词面）。
        let engine: StyleEngine<Key> = StyleEngine::new();
        assert!(!engine.settle_should_run(SettlePassKind::Calc));
        assert!(!engine.settle_should_run(SettlePassKind::Tables));
        assert!(!engine.settle_should_run(SettlePassKind::Columns));
        // 浮盒/行内无廉价输入索引——默认 true（内部早退治理）。
        assert!(engine.settle_should_run(SettlePassKind::Floats));
        assert!(engine.settle_should_run(SettlePassKind::Lines));
    }

    #[test]
    fn wrap_two_phase_frame_layout() {
        // T5c-2：两阶段帧通路（无字体时 remeasure 集为空，验证不回归）
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .set_stylesheet("div.p { width: 200px; padding: 10px; }")
                .is_clean()
        );
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        classes: std::iter::once("p".into()).collect(),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        text: Some("text".into()),
                        ..Default::default()
                    },
                )
                .is_ok()
        );
        let frame = engine.frame((400.0, 100.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct Key(u32);

    fn text_node(text: &str) -> StyleNode {
        StyleNode {
            name: Some("span".into()),
            text: Some(text.into()),
            ..Default::default()
        }
    }

    #[test]
    fn contract_errors() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        // ADR-0010：第二个 insert(None) = overlay 根（RootExists 不再发生）
        assert!(engine.insert(None, Key(2), StyleNode::default()).is_ok());
        // top-layer 契约：文档根不可进层；未知 key 报 UnknownNode；overlay 可进
        assert_eq!(
            engine.set_top_layer(Key(1), true),
            Err(crate::error::ContractError::NotOverlayRoot)
        );
        assert_eq!(
            engine.set_top_layer(Key(9), true),
            Err(crate::error::ContractError::UnknownNode)
        );
        assert!(engine.set_top_layer(Key(2), true).is_ok());
        assert_eq!(
            engine.insert(Some(Key(9)), Key(4), StyleNode::default()),
            Err(crate::error::ContractError::UnknownNode)
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert_eq!(
            engine.insert(Some(Key(1)), Key(3), StyleNode::default()),
            Err(crate::error::ContractError::DuplicateNode)
        );
        assert_eq!(
            engine.set_text(Key(7), None),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn flex_row_layout() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 3);
        let b2 = frame.find(Key(2)).unwrap();
        let b3 = frame.find(Key(3)).unwrap();
        assert_eq!((b2.x, b2.width, b2.height), (0.0, 100.0, 50.0));
        assert_eq!((b3.x, b3.width), (100.0, 100.0));
        // 根为 flex 容器：高度收缩为内容 50px
        let root = frame.find(Key(1)).unwrap();
        assert_eq!(root.height, 50.0);
    }

    #[test]
    fn restyle_picks_up_new_stylesheet() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(engine.set_stylesheet("div { width: 100px }").is_clean());
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 100.0);
        let report = engine.set_stylesheet("div { width: 300px }");
        assert!(report.is_clean());
        assert_eq!(engine.epoch(), 2);
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 300.0);
    }

    fn cn(name: &str, classes: &[&str]) -> StyleNode {
        StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn container_query_converges_within_first_frame() {
        // 阶段2③：首帧快照缺席 → 特性不命中；记录后环内第二 pass 命中，
        // 单次 frame() 调用即收敛（无需宿主再推一帧）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 400px; height: 300px } \
                 .kid { width: 100px; height: 50px } \
                 @container (min-width: 300px) { .kid { width: 200px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 200.0);
        // 快照 = 容器内容盒（border box 无 padding → 400×300）。
        let cid = engine.key_to_node[&Key(1)];
        assert_eq!(engine.container_sizes.get(&cid), Some(&[400.0, 300.0]));
    }

    #[test]
    fn container_query_refits_on_viewport_resize() {
        // 容器 50% 视口宽：缩小越阈后同一帧内收敛回基础值（陈旧快照不外泄）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 50%; height: 300px } \
                 .kid { width: 100px; height: 50px } \
                 @container (min-width: 350px) { .kid { width: 200px } }"
                )
                .is_clean()
        );
        assert_eq!(
            engine
                .frame((600.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            100.0
        );
        // 600→1000：陈旧快照 300 不命中 → 记录 500 → 环内第二 pass 命中。
        assert_eq!(
            engine
                .frame((1000.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            200.0
        );
        // 1000→400：反向翻转同样单帧收敛。
        assert_eq!(
            engine
                .frame((400.0, 600.0), 1.0, 0.0)
                .find(Key(2))
                .unwrap()
                .width,
            100.0
        );
    }

    #[test]
    fn container_inline_size_gates_block_axis_features() {
        // inline-size 容器：块轴特性（min-height）永不命中；行轴照常。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: inline-size; width: 400px; height: 100px } \
                 .kid { width: 30px; height: 30px } \
                 @container (min-height: 50px) { .kid { width: 90px } } \
                 @container (min-width: 300px) { .kid { height: 60px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(2)).unwrap();
        assert_eq!(kid.width, 30.0); // 块轴被门控
        assert_eq!(kid.height, 60.0); // 行轴命中
    }

    #[test]
    fn container_named_lookup_skips_unnamed() {
        // 命名段自最近向外查（跳过无名 inner），无名段取最近容器——两者互不串。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["panel"]))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(2)), Key(3), cn("div", &["inner"]))
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(3)), Key(4), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "div { container-type: size; width: 500px; height: 300px } \
                 .panel { container-name: panel; width: 400px; height: 250px } \
                 .inner { width: 100px; height: 100px } \
                 .kid { width: 20px; height: 20px } \
                 @container panel (min-width: 300px) { .kid { width: 80px } } \
                 @container (min-width: 300px) { .kid { height: 80px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let kid = frame.find(Key(4)).unwrap();
        assert_eq!(kid.width, 80.0); // panel 400×250 命中（跳过无名 inner）
        assert_eq!(kid.height, 20.0); // 无名段取最近 inner 100 → 不命中
    }

    #[test]
    fn container_snapshot_subtracts_padding() {
        // 快照 = 内容盒：width:400（content-box）+ padding:20 → 内容 400×200
        //（border box 440×240）。≥420 只在误用 border box 时命中；≥390 照常。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), cn("div", &[])).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), cn("div", &["kid"]))
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(
                    Key(1),
                    "container-type: size; width: 400px; height: 200px; padding: 20px"
                )
                .is_ok()
        );
        // 基础值放样式表：inline 声明会压过 @container 规则（级联优先级）。
        assert!(
            engine
                .set_stylesheet(
                    ".kid { width: 50px; height: 50px } \
                 @container (min-width: 420px) { .kid { width: 90px } } \
                 @container (min-width: 390px) { .kid { width: 70px } }"
                )
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().width, 70.0);
        let cid = engine.key_to_node[&Key(1)];
        assert_eq!(engine.container_sizes.get(&cid), Some(&[400.0, 200.0]));
    }

    #[test]
    fn structural_rebuild_after_remove() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(engine.remove(Key(2)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.boxes.len(), 2);
        assert!(frame.find(Key(2)).is_none());
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert!(engine.remove(Key(2)).is_err());
    }

    #[test]
    fn set_children_reorders() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .insert(Some(Key(1)), Key(3), StyleNode::default())
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: row")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(2), "width: 100px; height: 50px")
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(3), "width: 100px; height: 50px")
                .is_ok()
        );
        // 交换顺序
        assert!(engine.set_children(Key(1), &[Key(3), Key(2)]).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(3)).unwrap().x, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().x, 100.0);
        // 非子节点 → 契约错误
        assert_eq!(
            engine.set_children(Key(2), &[Key(1)]),
            Err(crate::error::ContractError::UnknownNode)
        );
    }

    #[test]
    fn leaf_measure_sizes_text() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), text_node("hello"))
                .is_ok()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        // 未推送测量：叶 0 高
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 0.0);
        assert!(engine.set_leaf_measure(Key(2), 100.0, 20.0).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b = frame.find(Key(2)).unwrap();
        // 第五批⑥契约：host 测量只供固有尺寸——列 flex 交叉轴 stretch 拉
        // 伸到容器内容宽（浏览器 flex 项一致），主轴高 = 内容高 20
        assert_eq!((b.width, b.height), (800.0, 20.0));
    }

    #[test]
    fn pseudo_state_restyling() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("button".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .set_stylesheet(
                    "button { width: 50px; height: 20px } button:hover { height: 40px }"
                )
                .is_clean()
        );
        assert!(
            engine
                .set_declarations(Key(1), "display: flex; flex-direction: column")
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 20.0);
        assert!(engine.set_state(Key(2), NodeState::HOVER).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert_eq!(frame.find(Key(2)).unwrap().height, 40.0);
    }

    #[test]
    fn clear_via_root_removal() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(Some(Key(1)), Key(2), StyleNode::default())
                .is_ok()
        );
        assert!(engine.remove(Key(1)).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        assert!(frame.boxes.is_empty());
        // 根槽位已释放，可重新声明
        assert!(engine.insert(None, Key(5), StyleNode::default()).is_ok());
    }

    #[test]
    fn absolute_text_leaf_sizes() {
        // ⑥ bidi-mixed 回归：auto 宽绝对文本叶取测量宽（restyle 的 absolute
        // 例外须查本地 cs 的 position——self.styles 此刻尚未 insert，查表恒
        // None 会把测量宽丢成 auto，taffy 绝对布局无测量回退得 0）；声明宽
        // 叶按声明宽折行（T5d 换行宽声明优先，非 shrink 夹紧宽）；混排与
        // 纯 RTL 测量宽均为正且与 Chromium golden 同容差。
        let mut engine: StyleEngine<u32> = StyleEngine::new();
        engine.add_font(TEST_FONT.to_vec());
        assert!(engine
            .set_stylesheet(
                "body { margin: 0 } .host { position: relative; width: 600px; height: 200px } \
                 .mix { position: absolute; top: 0px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .rtl { position: absolute; top: 30px; left: 0px; font-family: \"DejaVu Sans\"; font-size: 16px } \
                 .wrap { position: absolute; top: 60px; left: 0px; width: 140px; font-family: \"DejaVu Sans\"; font-size: 16px }",
            )
            .is_clean());
        let mk = |class: &str, text: &str| StyleNode {
            name: Some("div".into()),
            classes: std::iter::once(class.to_string()).collect(),
            text: Some(text.into()),
            ..Default::default()
        };
        assert!(engine.insert(None, 1, mk("host", "")).is_ok());
        assert!(
            engine
                .insert(Some(1), 2, mk("mix", "Hello שלום world עברית 42"))
                .is_ok()
        );
        assert!(engine.insert(Some(1), 3, mk("rtl", "עברית")).is_ok());
        assert!(
            engine
                .insert(Some(1), 4, mk("wrap", "Hello שלום world עברית 42 tail"))
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let b2 = frame.find(2).unwrap();
        assert_eq!((b2.x, b2.y), (0.0, 0.0));
        // 测量宽经 taffy 舍入取整（与 conformance 0.5px 容差同源）
        assert!((b2.width - 203.10938).abs() < 0.5, "w={}", b2.width);
        assert_eq!(b2.height, 19.0);
        let b3 = frame.find(3).unwrap();
        assert!((b3.width - 42.390625).abs() < 0.5, "w={}", b3.width);
        assert_eq!((b3.y, b3.height), (30.0, 19.0));
        let b4 = frame.find(4).unwrap();
        assert_eq!((b4.x, b4.y, b4.width), (0.0, 60.0, 140.0));
        assert_eq!(b4.height, 38.0);
    }

    #[test]
    fn ua_origin_ladder_and_important_inversion() {
        // P5（ADR-0033 D2/D3）：UA 层插在 Default 与 User 之间；important
        // 轴反转 = UA-important 最强（无障碍语义）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(
            engine
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("div".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        engine.set_ua_stylesheet("div { color: red; }");
        assert!(engine.set_stylesheet("div { color: blue; }").is_clean());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        use crate::css::property::DeclValue;
        use crate::css::value::ColorValue;
        let cs = engine.computed_style(Key(1)).unwrap();
        let author_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[2] > 0.5 && c.components[0] < 0.5
        );
        assert!(author_wins, "Author 应胜 UA（普通层序 UA < User < Author）");
        // UA-important 反转：胜过 Author 普通声明。
        engine.set_ua_stylesheet("div { color: red !important; }");
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let cs = engine.computed_style(Key(1)).unwrap();
        let ua_important_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[0] > 0.5 && c.components[2] < 0.5
        );
        assert!(ua_important_wins, "UA-important 应最强（important 轴反转）");
        // important 轴反转链锁：Author-important(4) 不改写 UA-important(6)
        //（css-cascade-5 origin+importance 反转——UA 无障碍语义压过作者）。
        assert!(
            engine
                .set_stylesheet("div { color: blue !important; }")
                .is_clean()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let cs = engine.computed_style(Key(1)).unwrap();
        let ua_still_wins = matches!(
            cs.value(crate::css::property::PropertyId::Color),
            Some(DeclValue::Color(ColorValue::Absolute(c)))
            if c.components[0] > 0.5 && c.components[2] < 0.5
        );
        assert!(ua_still_wins, "UA-important 应压过 Author-important");
    }

    #[test]
    fn ua_builtin_sheet_applies_and_clears() {
        // P5（ADR-0033 D3）：DEFAULT_UA_SHEET 解析零错误；装载后 h1 字号
        // 2em×16=32px；clear 后回初始 16px；未装载引擎不受影响。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(1)).unwrap().font_size_px(),
            16.0,
            "未装载 UA 表：初始字号"
        );
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        // div 不在 UA 表内 → 不受影响；h1 生效。
        assert!(
            engine
                .insert(
                    None,
                    Key(2),
                    StyleNode {
                        name: Some("h1".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(2)).unwrap().font_size_px(),
            32.0,
            "h1 UA 表字号 2em = 32px"
        );
        engine.clear_ua_stylesheet();
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(
            engine.computed_style(Key(2)).unwrap().font_size_px(),
            16.0,
            "clear 后回初始"
        );
    }

    #[test]
    fn ua_bolder_semantics_and_author_override() {
        // P9-1b：UA 表 b/strong → bolder（css-fonts-4 §2.2.1 相对字重）。
        // div(400) > strong > b 链：400 → 700 → 900；h1(700) 内 b → 900；
        // author 绝对权重压过 UA origin bolder（css-cascade origin 序）。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("strong".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(2)),
                    Key(3),
                    StyleNode {
                        name: Some("b".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(engine.computed_style(Key(2)).unwrap().font_weight(), 700.0);
        assert_eq!(
            engine.computed_style(Key(3)).unwrap().font_weight(),
            900.0,
            "strong(700) 内 b bolder → 900"
        );

        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        engine2.set_stylesheet("b { font-weight: 500 }");
        assert!(
            engine2
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("h1".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine2
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("b".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        assert_eq!(engine2.computed_style(Key(1)).unwrap().font_weight(), 700.0);
        assert_eq!(
            engine2.computed_style(Key(2)).unwrap().font_weight(),
            500.0,
            "author 500 压 UA bolder"
        );
    }

    #[test]
    fn ua_small_big_relative_font_size() {
        // P9-1c：UA 表 small/big → smaller/larger（css-fonts-4
        // <<relative-size>>）。body(16=medium) > small → 13（small）；
        // 16 下 big → 18（large）。author 绝对字号压过 UA origin。
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        assert!(engine.insert(None, Key(1), StyleNode::default()).is_ok());
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("small".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine
                .insert(
                    Some(Key(1)),
                    Key(3),
                    StyleNode {
                        name: Some("big".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let fs = |k: Key| engine.computed_style(k).unwrap().font_size_px();
        assert!(
            (fs(Key(2)) - 13.0).abs() < 1e-4,
            "16px 下 smaller → small(13)"
        );
        assert!(
            (fs(Key(3)) - 18.0).abs() < 1e-4,
            "16px 下 larger → large(18)"
        );

        // author 覆盖：big { font-size: 20px }（非表值）→ 其子 small
        // smaller = 20/1.2 ≈ 16.67（比例回退）。
        let mut engine2: StyleEngine<Key> = StyleEngine::new();
        engine2.set_ua_stylesheet(crate::builtins::DEFAULT_UA_SHEET);
        engine2.set_stylesheet("big { font-size: 20px }");
        assert!(
            engine2
                .insert(
                    None,
                    Key(1),
                    StyleNode {
                        name: Some("big".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        assert!(
            engine2
                .insert(
                    Some(Key(1)),
                    Key(2),
                    StyleNode {
                        name: Some("small".into()),
                        ..Default::default()
                    }
                )
                .is_ok()
        );
        let frame = engine2.frame((800.0, 600.0), 1.0, 0.0);
        let _ = frame;
        let fs2 = |k: Key| engine2.computed_style(k).unwrap().font_size_px();
        assert!(
            (fs2(Key(1)) - 20.0).abs() < 1e-4,
            "author 20px 压 UA larger"
        );
        assert!((fs2(Key(2)) - 20.0 / 1.2).abs() < 1e-4, "20/1.2 比例回退");
    }

    /// P6（ADR-0035 D1）：host 快筛键提取矩阵——类/类型/通配/前缀组合器
    /// 哨兵/id 键，且表装载路径重建索引。
    #[test]
    fn has_host_index_keys_and_sentinel() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine
            .set_stylesheet(
                ".card:has(img) { color: red } div:has(> p) { color: blue } \
                 *:has(q) { color: gray } .a > .b:has(x) { color: black } \
                 #lead:has(strong) { color: green }",
            )
            .is_clean();
        // 5 条规则全部深扫含 :has → 全进索引。
        assert_eq!(engine.has_host_index.len(), 5, "五条 :has 规则全建键");
        // .card 键：类约束、无 tag/id。
        let card = engine
            .has_host_index
            .iter()
            .find(|k| k.classes == ["card".to_string()])
            .expect(".card 键在场");
        assert!(card.tag.is_none() && card.id.is_none());
        // div 键：类型约束。
        let div = engine
            .has_host_index
            .iter()
            .find(|k| k.tag.as_deref() == Some("div"))
            .expect("div 键在场");
        assert!(div.classes.is_empty() && div.id.is_none());
        // #lead 键：id 约束。
        let lead = engine
            .has_host_index
            .iter()
            .find(|k| k.id.as_deref() == Some("lead"))
            .expect("#lead 键在场");
        assert!(lead.tag.is_none() && lead.classes.is_empty());
        // * 通配键与 .a > .b 前缀哨兵键同型（全空 = 恒不否决）。
        let empty = engine
            .has_host_index
            .iter()
            .filter(|k| k.tag.is_none() && k.id.is_none() && k.classes.is_empty())
            .count();
        assert_eq!(empty, 2, "通配键 + 前缀组合器哨兵键");
        // 换表重建：无 :has 表 → 索引清空。
        engine.set_stylesheet(".card { color: red }").is_clean();
        assert!(engine.has_host_index.is_empty(), "非 :has 表零键");
    }

    /// P6（ADR-0035 D2）：增量失效快筛判定矩阵——无关子树否决（走增量）、
    /// host 子树命中（升级全量）、前缀组合器哨兵恒升级、无 :has 表恒增量。
    #[test]
    fn has_invalidation_needs_full_matrix() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        engine
            .set_stylesheet(".card:has(img) { color: red }")
            .is_clean();
        let mk = |name: &str, classes: &[&str]| StyleNode {
            name: Some(name.into()),
            classes: classes.iter().map(|c| c.to_string()).collect(),
            ..Default::default()
        };
        engine.insert(None, Key(1), mk("root", &[])).unwrap();
        engine
            .insert(Some(Key(1)), Key(2), mk("div", &["card"]))
            .unwrap();
        engine.insert(Some(Key(2)), Key(3), mk("img", &[])).unwrap();
        engine
            .insert(Some(Key(1)), Key(4), mk("aside", &[]))
            .unwrap();
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case A：脏根在 host 子树之外（aside）——祖先链 root 无 .card 约束
        // 键命中 → 否决，走增量。
        engine.set_declarations(Key(4), "color: blue").unwrap();
        assert!(
            !engine.has_invalidation_needs_full(),
            "无关子树失效被快筛否决"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case B：脏根在 host 子树内（img）——祖先链 img→.card 命中 → 升级。
        engine.set_declarations(Key(3), "color: blue").unwrap();
        assert!(
            engine.has_invalidation_needs_full(),
            "host 祖先链命中升级全量"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case C：前缀组合器规则 → 哨兵键恒升级（保守兜底）。
        engine
            .set_stylesheet(".a > .b:has(x) { color: red }")
            .is_clean();
        engine.set_declarations(Key(4), "color: green").unwrap();
        assert!(
            engine.has_invalidation_needs_full(),
            "前缀组合器 :has 恒全量"
        );
        let _ = engine.frame((800.0, 600.0), 1.0, 0.0);
        // case D：无 :has 表 → 判定恒否（增量通道畅通）。
        engine.set_stylesheet(".a > .b { color: red }").is_clean();
        engine.set_declarations(Key(4), "color: green").unwrap();
        assert!(!engine.has_invalidation_needs_full(), "无 :has 零升级");
    }

    /// P6（ADR-0035 D3）：any_container_rules 补齐 user_sheet/ua_sheet
    /// 漏检——user 表含 @container 时收敛环 pass 判定（cap=3）正确。
    #[test]
    fn container_rules_detected_in_user_sheet() {
        let mut engine: StyleEngine<Key> = StyleEngine::new();
        assert!(!engine.any_container_rules());
        engine.set_user_stylesheet("@container (min-width: 100px) { .a { color: red } }");
        assert!(engine.any_container_rules(), "user 表 @container 不再漏检");
        engine.clear_user_stylesheet();
        assert!(!engine.any_container_rules(), "clear 后复位");
    }
}
